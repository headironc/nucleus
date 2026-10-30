//! Reconcile ended C1 work and delete expired records without contacting suppliers.

use chrono::{DateTime, TimeDelta, Utc};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_postgres::Client;
use tracing::Instrument;

use crate::{
    database::transaction::{Completion, Known, Outcome, commit, confirm},
    identity::{
        authentication::{
            Error, Failure, Inner, Service, Stage, Status, TRANSACTION_BUDGET,
            database::{self, challenge},
            transaction,
        },
        challenge::Id,
        message, operation,
    },
};

/// A supervised background task owns the cursor and each complete retention attempt.
pub struct Retention {
    service: Service,
    cursor: Option<operation::Id>,
    session_cursor: Option<operation::Id>,
}

impl Retention {
    pub(super) fn new(service: Service) -> Self {
        Self {
            service,
            cursor: None,
            session_cursor: None,
        }
    }

    /// Examine at most 128 candidates per family. Await in-flight recovery before stopping.
    pub async fn run(&mut self) -> Result<(), Error> {
        self.service.0.registry.ready()?;
        crate::identity::authentication::session::retention::run(
            &self.service,
            &mut self.session_cursor,
        )
        .await?;
        self.service.0.registry.ready()?;
        let candidates = match self.candidates().await {
            Ok(candidates) => candidates,
            Err(error @ Error::Closed) => return Err(error),
            Err(error) if error.unreliable() => {
                self.service.0.registry.close();
                return Err(error);
            }
            Err(error) => {
                tracing::error!(stage = "retention_scan", kind = "unavailable", error = ?error);
                return Ok(());
            }
        };
        let complete = candidates.len() < 128;
        for operation_id in candidates {
            self.service.0.registry.ready()?;
            // Even a skipped identity advances the cursor so later candidates get a turn.
            self.cursor = Some(operation_id);
            let mut permit = match self.service.0.registry.try_acquire(operation_id).await {
                Ok(permit) => permit,
                Err(crate::runtime::work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let service = &self.service.0;
            let result = async {
                let completion = scopeguard::guard((), |_| {
                    service.registry.close();
                    tracing::error!(stage = "retention_completion", kind = "unavailable", error = ?Error::Worker);
                });
                let result = service.reclaim(operation_id).await;
                scopeguard::ScopeGuard::into_inner(completion);
                if let Err(error) = result {
                    let unreliable = error.unreliable();
                    if unreliable {
                        service.registry.close();
                    }
                    let kind = match error.failure() {
                        Failure::Unknown => "unknown",
                        _ => "unavailable",
                    };
                    tracing::error!(stage = "retention_completion", kind, error = ?error);
                    if unreliable {
                        return Err(error);
                    }
                }
                Ok(())
            }.instrument(tracing::info_span!("retention", %operation_id)).await;
            permit.finish();
            result?;
            self.service.0.registry.ready()?;
        }
        if complete {
            self.cursor = None;
        }
        Ok(())
    }

    async fn candidates(&self) -> Result<Vec<operation::Id>, Error> {
        let lease = self
            .service
            .0
            .registry
            .lease_until_closed::<Error>(self.service.0.context.connections())
            .await?;
        let error = match timeout(TRANSACTION_BUDGET, async {
            let now = self.service.0.now(lease.client()).await?;
            challenge::fetch_candidates(lease.client(), self.cursor, now).await
        })
        .await
        {
            Ok(Ok(candidates)) => {
                lease.release();
                return Ok(candidates);
            }
            Ok(Err(error)) => error,
            Err(error) => Error::from(error),
        };
        Err(error.combine(lease.discard().await))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    challenge_id: Id,
    challenge_status: Status,
    message: Option<(crate::identity::message::Id, message::Status)>,
}

enum Attempt {
    Reading,
    Absent,
    Unchanged(Snapshot),
    Updating { before: Snapshot, after: Snapshot },
    Deleting(Snapshot),
}

impl Inner {
    async fn reclaim(&self, operation_id: operation::Id) -> Result<(), Error> {
        let lease = self.context.connections().lease().await?;
        let mut attempt = Attempt::Reading;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let cause = match timeout_at(
            deadline,
            self.write_retention(lease.client(), operation_id, &mut attempt),
        )
        .await
        {
            Ok(Ok(outcome)) => {
                let completion =
                    commit(outcome, deadline, lease.client().batch_execute("COMMIT")).await;
                let (completion, cleanup) = completion.recycle(lease).await;
                match completion {
                    Completion::Known(Known {
                        value: (),
                        end: Ok(()),
                    }) => {
                        return match cleanup {
                            Ok(()) => Ok(()),
                            Err(cause) => Err(Error::RetentionCompleted {
                                cause: Box::new(cause.into()),
                            }),
                        };
                    }
                    Completion::Known(Known {
                        value: (),
                        end: Err(cause),
                    }) => {
                        return Err(Error::RetentionCompleted {
                            cause: Box::new(Error::from(cause).combine(cleanup)),
                        });
                    }
                    Completion::Pending { error, .. } => Error::from(error).combine(cleanup),
                }
            }
            Ok(Err(error)) => error.combine(lease.discard().await),
            Err(error) => Error::from(error).combine(lease.discard().await),
        };
        match self.recover_retention(operation_id, &attempt).await {
            Ok((
                Known {
                    value: true,
                    end: Ok(()),
                },
                Ok(()),
            )) if !cause.unreliable() => Ok(()),
            Ok((
                Known {
                    value: complete,
                    end: commit,
                },
                termination,
            )) => {
                let cause = Box::new(Error::RetentionReadback {
                    cause: Box::new(cause),
                    commit,
                    termination,
                });
                if complete {
                    Err(Error::RetentionCompleted { cause })
                } else if matches!(attempt, Attempt::Reading | Attempt::Unchanged(_)) {
                    Err(*cause)
                } else {
                    Err(Error::NotCommitted { cause })
                }
            }
            Err(recovery) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(recovery),
            }),
        }
    }

    async fn write_retention(
        &self,
        client: &Client,
        operation_id: operation::Id,
        attempt: &mut Attempt,
    ) -> Result<Outcome<()>, Error> {
        transaction::begin(&self.context, client, operation_id).await?;
        if let Some(challenge) = challenge::find_by_operation(client, operation_id).await? {
            let mobile = challenge.mobile(&self.keys)?;
            transaction::object(client, b"identity/mobile", mobile.as_bytes()).await?;
            transaction::object(
                client,
                b"message/source",
                challenge.source().to_string().as_bytes(),
            )
            .await?;
            transaction::object(client, b"message/deployment", b"permit").await?;
            challenge.lock(client).await?;
        }
        let now = self.now(client).await?;
        let option = challenge::verify(client, &self.keys, operation_id, now).await?;
        let Some(challenge) = option else {
            *attempt = Attempt::Absent;
            return Ok(Outcome::Known(()));
        };
        database::message::lock(client, challenge.id()).await?;
        let mut option = database::message::find_by_challenge(client, challenge.id()).await?;
        if let Some(message) = &option {
            message.validate(&self.keys.message, now)?;
        }
        let before = Snapshot {
            challenge_id: challenge.id(),
            challenge_status: challenge.status(),
            message: option
                .as_ref()
                .map(|message| (message.id(), message.status())),
        };
        *attempt = Attempt::Unchanged(before);
        if challenge.status() == Status::Calling
            || (matches!(challenge.status(), Status::Retryable { .. })
                && now >= challenge.expires_at())
        {
            if option.is_some() {
                return Err(inconsistent());
            }
            let after = Snapshot {
                challenge_status: Status::Inconclusive { finished_at: now },
                ..before
            };
            *attempt = Attempt::Updating { before, after };
            challenge.conclude(client, now).await?;
        } else if let Some(message) = &mut option
            && message.status() == message::Status::Sending
        {
            let conclusion = message.orphaned();
            let status = conclusion.at(now);
            let after = Snapshot {
                message: Some((message.id(), status)),
                ..before
            };
            *attempt = Attempt::Updating { before, after };
            message.finish(client, conclusion, now).await?;
        } else if !retained(
            challenge.expires_at(),
            challenge.status().finished_at(),
            now,
        ) && !option.as_ref().is_some_and(|message| {
            retained(message.expires_at(), message.status().finished_at(), now)
        }) {
            *attempt = Attempt::Deleting(before);
            if let Some(message) = option {
                if message.retained(client).await? {
                    *attempt = Attempt::Unchanged(before);
                    return Ok(Outcome::Known(()));
                }
                message.delete(client).await?;
            }
            challenge.delete(client).await?;
        }
        Ok(if matches!(attempt, Attempt::Unchanged(_)) {
            Outcome::Known(())
        } else {
            Outcome::Pending(())
        })
    }

    async fn recover_retention(
        &self,
        operation_id: operation::Id,
        attempt: &Attempt,
    ) -> Result<(Known<bool>, Result<(), platform::database::Error>), Error> {
        let session = self.context.connections().recover().await?;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let result = timeout_at(deadline, async {
            let client = session.client();
            transaction::begin(&self.context, client, operation_id).await?;
            let now = self.now(client).await?;
            let option = challenge::verify(client, &self.keys, operation_id, now).await?;
            let complete = match (attempt, option) {
                (Attempt::Absent, _) => true,
                (Attempt::Reading, _) => false,
                (Attempt::Deleting(before), option)
                    if option
                        .as_ref()
                        .is_none_or(|challenge| challenge.id() != before.challenge_id) =>
                {
                    // The operation UUID may now identify another request. Confirm that the
                    // exact original group is gone without touching its replacement.
                    let option = before.message.map(|(id, _)| id);
                    if !challenge::deleted(client, before.challenge_id, option).await? {
                        return Err(inconsistent());
                    }
                    true
                }
                (_, Some(challenge)) => {
                    let option =
                        database::message::find_by_challenge(client, challenge.id()).await?;
                    if let Some(message) = &option {
                        message.validate(&self.keys.message, now)?;
                    }
                    let snapshot = Snapshot {
                        challenge_id: challenge.id(),
                        challenge_status: challenge.status(),
                        message: option
                            .as_ref()
                            .map(|message| (message.id(), message.status())),
                    };
                    match attempt {
                        Attempt::Updating { after, .. } if snapshot == *after => true,
                        Attempt::Unchanged(before)
                        | Attempt::Deleting(before)
                        | Attempt::Updating { before, .. }
                            if snapshot == *before =>
                        {
                            false
                        }
                        _ => return Err(inconsistent()),
                    }
                }
                _ => return Err(inconsistent()),
            };
            Ok::<_, Error>(complete)
        })
        .await;
        let result = match result {
            Ok(Ok(complete)) => {
                Ok(confirm(complete, deadline, session.client().batch_execute("COMMIT")).await)
            }
            Ok(Err(error)) => Err(error),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(result) => Ok(result.terminate(session).await),
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }
}

fn inconsistent() -> Error {
    Error::Facts {
        stage: Stage::Retention,
        rule: "the original record identities and stages must identify the retention outcome",
    }
}

fn retained(expires_at: DateTime<Utc>, option: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    let Some(finished_at) = option else {
        return true;
    };
    now.signed_duration_since(expires_at.max(finished_at)) < TimeDelta::hours(24)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::poll_fn, sync::Arc, time::Duration};

    use chrono::{DateTime, TimeDelta, Utc};
    use platform::database::{self, Connections, credentials::Password};
    use rustls::RootCertStore;
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::{Client, Row, error::SqlState};
    use tracing::Instrument;
    use tracing_test::traced_test;
    use uuid::Uuid;

    use crate::{
        access::role::tests::start,
        database::{
            Barrier,
            transaction::{Completion, EndError, Known, commit},
        },
        identity::{
            authentication::{
                self, Error, Input, Service, Stage, Status,
                database::message::Message,
                dispatch::tests::{client, proxied},
                retention::Attempt,
                verification::tests::{Database, Supplier, keys, server},
            },
            message::{self, Conclusion},
            operation,
        },
        runtime::Context,
    };

    #[derive(Clone, Copy)]
    enum History {
        Calling,
        Retryable,
        Rejected,
        Passed,
        Unclaimed,
        Claimed,
        Sent,
    }

    fn input(number: u128) -> Input {
        Input::new(
            Uuid::from_u128(number)
                .to_string()
                .parse()
                .expect("operation"),
            "13800000000".parse().expect("mobile"),
            format!("fictional-token-{number}").parse().expect("token"),
            "192.0.2.1".parse().expect("source"),
        )
    }

    async fn seed(database: &Database, number: u128, now: DateTime<Utc>, history: History) {
        let session = database
            .connections(&database.user)
            .recover()
            .await
            .expect("runtime writer");
        let client = session.client();
        client
            .batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED")
            .await
            .expect("begin");
        let input = input(number);
        let before = authentication::database::challenge::insert(
            client,
            &keys(),
            &input,
            authentication::database::challenge::Subject::Anonymous,
            now,
        )
        .await
        .expect("original facts");
        let status = match history {
            History::Calling => Status::Calling,
            History::Retryable => Status::Retryable { finished_at: now },
            History::Rejected => Status::Rejected { finished_at: now },
            _ => Status::Passed {
                solved_at: now,
                finished_at: now,
            },
        };
        if !matches!(history, History::Calling) {
            let after = crate::identity::authentication::Receipt { status, ..before };
            authentication::database::challenge::update(client, &input, before, after)
                .await
                .expect("conclusion");
        }
        if matches!(
            history,
            History::Unclaimed | History::Claimed | History::Sent
        ) {
            let ordinal = client
                .query_one("SELECT nextval('identity.ordinals')", &[])
                .await
                .expect("ordinal")
                .get(0);
            let code = message::Code::generate().expect("fictional code");
            let mut message = Message::new(
                &keys().message,
                before.id(),
                ordinal,
                now,
                &message::Limits::default(),
                &code,
                authentication::database::message::Binding::Authentication,
            )
            .expect("message facts");
            message.insert(client).await.expect("paired permit");
            if !matches!(history, History::Unclaimed) {
                message.claim(client, now).await.expect("claim");
            }
            if matches!(history, History::Sent) {
                message
                    .finish(client, Conclusion::Sent, now)
                    .await
                    .expect("sent");
            }
        }
        client.batch_execute("COMMIT").await.expect("commit");
        session.terminate().await.expect("writer ended");
    }

    async fn stop(context: crate::runtime::Context, service: Service) {
        service.drain().await.expect("drain");
        context.stop().await.expect("runtime ended");
    }

    async fn counts(client: &Client) -> (i64, i64) {
        let row = client.query_one("SELECT (SELECT count(*) FROM identity.challenges), (SELECT count(*) FROM identity.messages)", &[]).await.expect("counts");
        (row.get(0), row.get(1))
    }

    async fn delete(client: &Client, operation_id: operation::Id) {
        for sql in [
            "DELETE FROM identity.checks WHERE id IN (SELECT id FROM identity.messages WHERE challenge_id IN (SELECT id FROM identity.challenges WHERE operation_id=$1))",
            "DELETE FROM identity.claims WHERE id IN (SELECT id FROM identity.messages WHERE challenge_id IN (SELECT id FROM identity.challenges WHERE operation_id=$1))",
            "DELETE FROM identity.messages WHERE challenge_id IN (SELECT id FROM identity.challenges WHERE operation_id=$1)",
            "DELETE FROM identity.challenges WHERE operation_id=$1",
        ] {
            client
                .execute(sql, &[&operation_id])
                .await
                .expect("protected group deletion");
        }
    }

    #[test]
    fn retention_stopped_while_waiting_for_the_pool_preserves_held_slots() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(server).await;
                let supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let first = context.connections().acquire().await.expect("first slot");
                let second = context.connections().acquire().await.expect("second slot");
                let mut pids = Vec::new();
                for session in [&first, &second] {
                    pids.push(
                        session
                            .client()
                            .query_one("SELECT pg_backend_pid()", &[])
                            .await
                            .expect("held backend")
                            .get::<_, i32>(0),
                    );
                }
                let mut retention = services.authentication().retention();
                let mut discovery = Box::pin(tokio::task::unconstrained(retention.run()));
                // Discovery reaches the exhausted pool before observing transaction time.
                poll_fn(|context| {
                    assert!(discovery.as_mut().poll(context).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                // Let R1 finish its empty scan, then reserve its slot before C1 can lease it.
                drop(first);
                let mut returned = Box::pin(context.connections().acquire());
                let first = timeout(
                    Duration::from_secs(2),
                    poll_fn(|context| {
                        assert!(discovery.as_mut().poll(context).is_pending());
                        returned.as_mut().poll(context)
                    }),
                )
                .await
                .expect("R1 scan finished")
                .expect("R1 slot returned");
                poll_fn(|context| {
                    assert!(discovery.as_mut().poll(context).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                services.close();
                let result = timeout(Duration::from_millis(500), discovery)
                    .await
                    .expect("queued discovery stops without the pool wait timeout");
                assert!(matches!(result, Err(Error::Closed)));
                for (session, pid) in [&first, &second].into_iter().zip(&pids) {
                    assert_eq!(
                        session
                            .client()
                            .query_one("SELECT pg_backend_pid()", &[])
                            .await
                            .expect("held slot remains usable")
                            .get::<_, i32>(0),
                        *pid
                    );
                }
                drop((first, second));
                timeout(Duration::from_secs(1), async {
                    let first = context
                        .connections()
                        .lease()
                        .await
                        .expect("first slot returned");
                    let second = context
                        .connections()
                        .lease()
                        .await
                        .expect("second slot returned");
                    let mut reused = Vec::new();
                    for lease in [&first, &second] {
                        reused.push(
                            lease
                                .client()
                                .query_one("SELECT pg_backend_pid()", &[])
                                .await
                                .expect("returned slot remains usable")
                                .get::<_, i32>(0),
                        );
                    }
                    reused.sort_unstable();
                    pids.sort_unstable();
                    assert_eq!(
                        reused, pids,
                        "both original connections survive cancellation"
                    );
                    first.release();
                    second.release();
                })
                .await
                .expect("cancellation leaks neither online slot");
                services.drain().await.expect("work drained");
                context.stop().await.expect("runtime stopped");
                supplier.stop().await;
            })
            .await
            .expect("bounded stop during queued discovery");
        });
    }

    #[test]
    fn deletes_expired_groups_and_preserves_orphan_conclusions_after_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let old = database.now().await - TimeDelta::hours(25);
            for (number, history) in [(1, History::Rejected), (2, History::Sent), (3, History::Calling),
                (4, History::Retryable), (5, History::Unclaimed), (6, History::Claimed)] {
                seed(&database, number, old, history).await;
            }
            seed(&database, 7, database.now().await, History::Passed).await;
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            service.retention().run().await.expect("one round");
            let reader = database.connections(&database.user).recover().await.expect("reader");
            assert_eq!(counts(reader.client()).await, (5, 2));
            let statuses: Vec<String> = reader.client().query("SELECT status FROM identity.messages ORDER BY ordinal", &[]).await.expect("messages").iter().map(|row| row.get(0)).collect();
            assert_eq!(statuses, ["failed", "unknown"]);
            let concluded: i64 = reader.client().query_one("SELECT count(*) FROM identity.challenges WHERE status='inconclusive' AND finished_at > clock_timestamp() - interval '1 minute'", &[]).await.expect("new conclusions").get(0);
            assert_eq!(concluded, 2);
            service.retention().run().await.expect("new conclusions remain retained");
            assert_eq!(counts(reader.client()).await, (5, 2));
            let ordinal: i64 = reader.client().query_one("SELECT nextval('identity.ordinals')", &[]).await.expect("continuing ordinal").get(0);
            assert_eq!(ordinal, 4);
            stop(context, service).await;
            let (context, service) = database.start(supplier.client()).await;
            for number in [3, 4] {
                assert!(matches!(service.verify(input(number)).await.expect("retained conclusion").status(), Status::Inconclusive { .. }));
            }
            for (number, expected) in [(5, "failed"), (6, "unknown")] {
                let receipt = service.send(input(number)).await.expect("retained message");
                assert_eq!(receipt.status().as_ref(), expected);
            }
            service.ready().await.expect("retention does not close admission");
            reader.terminate().await.expect("reader ended");
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn completed_noop_survives_cleanup_without_a_second_readback() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for retained in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                if retained { seed(&database, 1, database.now().await, History::Sent).await; }
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy)).await.expect("runtime");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("checked barrier"), keys(),
                    crate::startup::Settings::new(4, message::Limits::default(),
                        crate::identity::authentication::verification::tests::session_settings(),
                        crate::registration::Limits::default(),
                        crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                    challenge.client(), client(&supplier),
                ).await.expect("service");
                let reader = database.connections(&database.user).recover().await.expect("independent observer");
                let snapshot = async || {
                    let row = reader.client().query_one("SELECT (SELECT jsonb_agg(challenge)::text FROM identity.challenges challenge), (SELECT jsonb_agg(message)::text FROM identity.messages message), (SELECT jsonb_agg(checks)::text FROM identity.checks checks), (SELECT jsonb_agg(claim)::text FROM identity.claims claim)", &[]).await.expect("complete group snapshot");
                    [0,1,2,3].map(|column| row.get::<_,Option<String>>(column))
                };
                let before = snapshot().await;
                if retained { assert!(before.iter().all(Option::is_some)); }
                else { assert!(before.iter().all(Option::is_none)); }
                tokio::select! {
                    _ = challenge.request() => panic!("no Siteverify during no-op cleanup"),
                    _ = supplier.request() => panic!("no PNVS during no-op cleanup"),
                    () = async {
                        proxy.arm(Fault::StalledReadCommit);
                        let worker = service.clone();
                        let task = tokio::spawn(async move { worker.0.reclaim(input(1).operation_id()).await });
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("first complete no-op read ended its transaction");
                        proxy.arm(Fault::RejectedBegin); proxy.release();
                        let result = timeout(Duration::from_secs(3), task).await.expect("bounded no-op cleanup").expect("task");
                        assert_eq!(snapshot().await, before, "independent original group before another cleanup");
                        let Err(Error::RetentionCompleted { cause }) = result else { panic!("complete no-op remains known: {result:?}") };
                        let Error::Combined { cause, termination } = *cause else { panic!("transaction and driver failures retained") };
                        assert!(matches!(*cause, Error::Query(ref cause) if cause.0.is_closed()));
                        assert!(matches!(termination, platform::database::Error::Postgres(_)));
                        stop(context, service).await;
                    } => {}
                }
                assert!(timeout(Duration::from_millis(30), challenge.request()).await.is_err());
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                reader.terminate().await.expect("reader ended");
                challenge.stop().await; supplier.stop().await; proxy.stop().await;
            }
        });
    }

    #[test]
    fn runtime_sql_protects_retained_records_pairing_and_isolation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            seed(&database, 1, database.now().await, History::Claimed).await;
            seed(&database, 2, database.now().await - TimeDelta::hours(25), History::Sent).await;
            let writer = database.connections(&database.user).recover().await.expect("runtime writer");
            let client = writer.client();
            for table in ["challenges", "messages", "checks", "claims"] {
                assert!(client.batch_execute(&format!("DELETE FROM identity.{table}")).await.is_err());
            }
            for sql in [
                "SELECT setval('identity.ordinals', 1)",
                "DELETE FROM identity.claims WHERE id IN (SELECT id FROM identity.messages WHERE ordinal=2)",
            ] { assert!(client.batch_execute(sql).await.is_err(), "direct SQL must preserve responsibilities"); }
            for isolation in ["REPEATABLE READ", "SERIALIZABLE", "READ UNCOMMITTED"] {
                for sql in [
                    "DELETE FROM identity.challenges WHERE false", "DELETE FROM identity.messages WHERE false",
                    "DELETE FROM identity.checks WHERE false", "DELETE FROM identity.claims WHERE false",
                    "INSERT INTO identity.challenges (id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at) SELECT id, operation_id, purpose, hash, key_id, mobile, source, calls, created_at, expires_at FROM identity.challenges WHERE false",
                    "INSERT INTO identity.messages (id, challenge_id, ordinal, worker_id, created_at, expires_at) SELECT id, challenge_id, ordinal, worker_id, created_at, expires_at FROM identity.messages WHERE false",
                    "INSERT INTO identity.checks (id, hash, key_id, expires_at) SELECT id, hash, key_id, expires_at FROM identity.checks WHERE false",
                    "INSERT INTO identity.claims (id, worker_id, created_at) SELECT id, worker_id, created_at FROM identity.claims WHERE false",
                    "UPDATE identity.challenges SET calls=calls WHERE false", "UPDATE identity.messages SET status=status WHERE false",
                ] {
                    client.batch_execute(&format!("BEGIN ISOLATION LEVEL {isolation}")).await.expect("begin unsupported isolation");
                    assert_eq!(client.batch_execute(sql).await.expect_err("isolation guard").code(), Some(&SqlState::INVALID_TRANSACTION_STATE));
                    client.batch_execute("ROLLBACK").await.expect("rollback");
                }
            }
            client.batch_execute("BEGIN").await.expect("begin partial deletion");
            for sql in [
                "DELETE FROM identity.checks WHERE id IN (SELECT id FROM identity.messages WHERE ordinal=2)",
                "DELETE FROM identity.claims WHERE id IN (SELECT id FROM identity.messages WHERE ordinal=2)",
                "DELETE FROM identity.messages WHERE ordinal=2",
            ] {
                client.batch_execute(sql).await.expect("defer group validation until commit");
            }
            assert_eq!(client.batch_execute("COMMIT").await.expect_err("message deletion requires its challenge").code(), Some(&SqlState::CHECK_VIOLATION));
            assert_eq!(counts(client).await, (2, 2));
            let paired: bool = client.query_one(
                "SELECT (SELECT count(*) FROM identity.checks) = 2 AND (SELECT count(*) FROM identity.claims) = 2", &[],
            ).await.expect("entire partial deletion rolled back").get(0);
            assert!(paired);
            client.batch_execute("BEGIN").await.expect("begin");
            delete(client, input(2).operation_id()).await;
            client.batch_execute("COMMIT").await.expect("legal group deletion");
            assert_eq!(counts(client).await, (1, 1));
            // A complete expired Challenge has no permanent UUID tombstone.
            seed(&database, 4, database.now().await - TimeDelta::hours(25), History::Rejected).await;
            client.batch_execute("DELETE FROM identity.challenges WHERE operation_id='00000000-0000-0000-0000-000000000004'").await.expect("eligible standalone deletion");
            assert_eq!(counts(client).await, (1, 1));
            writer.terminate().await.expect("writer ended");
        });
    }

    #[test]
    fn retention_recovers_lost_late_and_rolled_back_commits_and_drains_owned_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for fault in [
                fixture::database_proxy::Fault::LostCommit,
                fixture::database_proxy::Fault::DelayedCommit,
                fixture::database_proxy::Fault::RolledBack,
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                seed(
                    &database,
                    1,
                    database.now().await - TimeDelta::hours(25),
                    History::Sent,
                )
                .await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy CA");
                let connections = Connections::new(
                    database::Settings::new(
                        "127.0.0.1".into(),
                        proxy.port(),
                        database.name.clone(),
                        database.user.clone(),
                        2,
                        database::settings::Budgets::default(),
                    )
                    .expect("settings"),
                    Password::new(fixture::PASSWORD.into()).expect("password"),
                    roots,
                )
                .expect("connections");
                let supplier = Supplier::new().await;
                let (context, service) = database.start_on(supplier.client(), connections).await;
                let reader = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("retention observer");
                let original = reader
                    .client()
                    .query_one(
                        "SELECT challenge.id AS challenge_id, message.id AS message_id
                         FROM identity.challenges challenge
                         JOIN identity.messages message ON message.challenge_id = challenge.id
                         WHERE challenge.operation_id = $1",
                        &[&input(1).operation_id()],
                    )
                    .await
                    .expect("original group identities");
                let challenge_id: crate::identity::challenge::Id = original.get("challenge_id");
                let message_id: message::Id = original.get("message_id");
                // Capture every persisted field independently of the recovery decoder.
                let group = async || {
                    let row = timeout(
                        Duration::from_secs(5),
                        reader.client().query_one(
                            "SELECT
                                (SELECT to_jsonb(challenge)::text FROM identity.challenges challenge WHERE id = $1),
                                (SELECT to_jsonb(message)::text FROM identity.messages message WHERE id = $2),
                                (SELECT to_jsonb(checks)::text FROM identity.checks checks WHERE id = $2),
                                (SELECT to_jsonb(claim)::text FROM identity.claims claim WHERE id = $2)",
                            &[&challenge_id, &message_id],
                        ),
                    )
                    .await
                    .expect("bounded group readback")
                    .expect("original group facts");
                    [0, 1, 2, 3].map(|column| row.get::<_, Option<String>>(column))
                };
                let original = group().await;
                assert!(original.iter().all(Option::is_some));
                let delayed = matches!(fault, fixture::database_proxy::Fault::DelayedCommit);
                let rolled_back = matches!(fault, fixture::database_proxy::Fault::RolledBack);
                proxy.arm(fault);
                let mut retention = service.retention();
                let mut task = tokio::spawn(async move { retention.run().await });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("retention COMMIT intercepted");
                if delayed {
                    // Independent SHA-256 vector: E("challenge/operation", input(1)'s
                    // UUID bytes) starts with 6e22b238 b856773d. PostgreSQL exposes the
                    // bigint lock's high/low halves as classid/objid, with objsubid=1.
                    let lock = (1_847_767_608_u32, 3_092_674_365_u32);
                    timeout(Duration::from_secs(5), async {
                        loop {
                            let waiting: bool = reader
                                .client()
                                .query_one(
                                    "SELECT EXISTS (
                                        SELECT 1 FROM pg_catalog.pg_locks
                                        WHERE locktype = 'advisory' AND NOT granted
                                          AND mode = 'ExclusiveLock'
                                          AND classid = $1 AND objid = $2 AND objsubid = 1
                                          AND database = (
                                              SELECT oid FROM pg_catalog.pg_database
                                              WHERE datname = current_database()
                                          )
                                          AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                                    )",
                                    &[&lock.0, &lock.1],
                                )
                                .await
                                .expect("retention recovery lock observation")
                                .get(0);
                            if waiting {
                                break;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("retention recovery waits for the original operation lock");
                    assert_eq!(group().await, original);
                    assert!(timeout(Duration::from_millis(30), &mut task).await.is_err());
                    // Only the borrowed waiter is cancelled; its owner retains the worker.
                    service.close();
                    assert!(timeout(Duration::from_millis(30), &mut task).await.is_err());
                    assert_eq!(group().await, original);
                    proxy.release();
                    let error = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("drain budget")
                        .expect("retention task")
                        .expect_err("closed admission ends the round after recovery");
                    assert!(matches!(error, Error::Closed));
                } else {
                    proxy.release();
                    timeout(Duration::from_secs(5), task)
                        .await
                        .expect("recovery budget")
                        .expect("scanner task")
                        .expect("round");
                }
                // Inspect before another round can delete a rolled-back original group.
                assert_eq!(
                    group().await,
                    if rolled_back { original } else { [None, None, None, None] }
                );
                if rolled_back {
                    service
                        .retention()
                        .run()
                        .await
                        .expect("next bounded attempt");
                    assert_eq!(group().await, [None, None, None, None]);
                }
                reader.terminate().await.expect("reader ended");
                stop(context, service).await;
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn orphan_conclusions_survive_commit_faults_without_resending_or_early_deletion() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (history, expected) in [(History::Unclaimed, "failed"), (History::Claimed, "unknown")] {
                for fault in [Fault::LostCommit, Fault::DelayedCommit, Fault::RolledBack] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    seed(&database, 1, database.now().await - TimeDelta::hours(25), history).await;
                    let mut challenge = Supplier::new().await;
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let context = Context::new(proxied(&database, &proxy)).await.expect("runtime");
                    let service = Service::start(
                        Barrier::begin(context.clone()).await.expect("checked barrier"),
                        keys(),
                        crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                        challenge.client(),
                        client(&supplier),
                    ).await.expect("service");
                    let reader = database.connections(&database.user).recover().await.expect("observer");
                    let message_id: message::Id = reader.client().query_one(
                        "SELECT message.id FROM identity.messages message
                         JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                         WHERE challenge.operation_id = $1",
                        &[&input(1).operation_id()],
                    ).await.expect("original message").get(0);
                    let facts = async || {
                        timeout(Duration::from_secs(5), reader.client().query_one(
                            "SELECT message.status, message.finished_at,
                                    (to_jsonb(message) - 'status' - 'finished_at')::text AS message_facts,
                                    to_jsonb(challenge)::text AS challenge_facts,
                                    to_jsonb(checks)::text AS check_facts,
                                    to_jsonb(claim)::text AS claim_facts
                             FROM identity.messages message
                             JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                             JOIN identity.checks checks ON checks.id = message.id
                             LEFT JOIN identity.claims claim ON claim.id = message.id
                             WHERE message.id = $1",
                            &[&message_id],
                        )).await.expect("bounded fact readback").expect("original record group")
                    };
                    let original = facts().await;
                    assert_eq!(original.get::<_, &str>("status"), "sending");
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(original.get::<_, Option<String>>("claim_facts").is_some(), matches!(history, History::Claimed));
                    let assert_original_facts = |actual: &Row| {
                        for column in ["message_facts", "challenge_facts", "check_facts"] {
                            assert_eq!(actual.get::<_, String>(column), original.get::<_, String>(column), "{column}");
                        }
                        assert_eq!(actual.get::<_, Option<String>>("claim_facts"), original.get::<_, Option<String>>("claim_facts"));
                    };
                    let delayed = matches!(fault, Fault::DelayedCommit);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    // Watch the real transport through recovery, later scans and receipt replay.
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("orphan reconciliation must not send an SMS"),
                        () = async {
                            let mut started_at = database.now().await;
                            proxy.arm(fault);
                            let mut retention = service.retention();
                            let task = tokio::spawn(async move { retention.run().await });
                            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("orphan conclusion COMMIT intercepted");
                            if delayed {
                                // Independent SHA-256 vector: E("challenge/operation", input(1)'s
                                // UUID bytes) starts with 6e22b238 b856773d. PostgreSQL exposes the
                                // bigint lock's high/low halves as classid/objid, with objsubid=1.
                                let lock = (1_847_767_608_u32, 3_092_674_365_u32);
                                timeout(Duration::from_secs(5), async {
                                    loop {
                                        let waiting: bool = reader.client().query_one(
                                            "SELECT EXISTS (
                                                SELECT 1 FROM pg_catalog.pg_locks
                                                WHERE locktype = 'advisory' AND NOT granted
                                                  AND mode = 'ExclusiveLock'
                                                  AND classid = $1 AND objid = $2 AND objsubid = 1
                                                  AND database = (
                                                      SELECT oid FROM pg_catalog.pg_database
                                                      WHERE datname = current_database()
                                                  )
                                                  AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                                            )", &[&lock.0, &lock.1],
                                        ).await.expect("orphan recovery lock observation").get(0);
                                        if waiting { break; }
                                        tokio::task::yield_now().await;
                                    }
                                }).await.expect("orphan recovery waits for the original operation lock");
                                let pending = facts().await;
                                assert_original_facts(&pending);
                                assert_eq!(pending.get::<_, &str>("status"), "sending");
                                assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                                assert!(!task.is_finished());
                            }
                            proxy.release();
                            timeout(Duration::from_secs(5), task).await.expect("bounded recovery").expect("retention task").expect("round");
                            // A later scan or request must not repair the facts before this read.
                            let recovered = facts().await;
                            assert_original_facts(&recovered);
                            if rolled_back {
                                assert_eq!(recovered.get::<_, &str>("status"), "sending");
                                assert_eq!(recovered.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                                started_at = database.now().await;
                                timeout(Duration::from_secs(5), service.retention().run()).await.expect("bounded next round").expect("retry next round");
                            }
                            let concluded = facts().await;
                            assert_original_facts(&concluded);
                            assert_eq!(concluded.get::<_, &str>("status"), expected);
                            let finished_at: DateTime<Utc> = concluded.get("finished_at");
                            assert!(finished_at >= started_at && finished_at <= database.now().await);
                            // The 25-hour-old permit must now be retained from its new conclusion.
                            timeout(Duration::from_secs(5), service.retention().run()).await.expect("bounded later scan").expect("retain new conclusion");
                            let retained = facts().await;
                            assert_original_facts(&retained);
                            assert_eq!(retained.get::<_, &str>("status"), expected);
                            assert_eq!(retained.get::<_, DateTime<Utc>>("finished_at"), finished_at);
                            let receipt = timeout(Duration::from_secs(5), service.send(input(1))).await.expect("bounded replay").expect("original conclusion");
                            assert_eq!(receipt.id(), message_id);
                            assert_eq!(receipt.status().as_ref(), expected);
                            assert_eq!(receipt.status().finished_at(), Some(finished_at));
                            service.ready().await.expect("recovered service stays ready");
                            stop(context, service).await;
                        } => {},
                    }
                    assert!(timeout(Duration::from_millis(30), challenge.request()).await.is_err());
                    reader.terminate().await.expect("observer ended");
                    challenge.stop().await;
                    proxy.stop().await;
                }
            }
        });
    }

    #[traced_test]
    #[test]
    fn retention_readback_keeps_completed_deletion_and_its_cleanup_diagnostic() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            seed(&database, 1, database.now().await - TimeDelta::hours(25), History::Sent).await;
            let mut challenge = Supplier::new().await;
            let mut supplier = fixture::supplier::Supplier::start().await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let context = Context::new(proxied(&database, &proxy)).await.expect("runtime");
            let service = Service::start(
                Barrier::begin(context.clone()).await.expect("checked barrier"), keys(),
                crate::startup::Settings::new(4, message::Limits::default(),
                    crate::identity::authentication::verification::tests::session_settings(),
                    crate::registration::Limits::default(),
                    crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                challenge.client(), client(&supplier),
            ).await.expect("service");
            let reader = database.connections(&database.user).recover().await.expect("observer");
            let row = reader.client().query_one(
                "SELECT challenge.id, message.id FROM identity.challenges challenge
                 JOIN identity.messages message ON message.challenge_id=challenge.id
                 WHERE challenge.operation_id=$1", &[&input(1).operation_id()],
            ).await.expect("original identities");
            let challenge_id: crate::identity::challenge::Id = row.get(0);
            let message_id: message::Id = row.get(1);
            let group = async || {
                let row = reader.client().query_one(
                    "SELECT (SELECT to_jsonb(challenge)::text FROM identity.challenges challenge WHERE id=$1),
                            (SELECT to_jsonb(message)::text FROM identity.messages message WHERE id=$2),
                            (SELECT to_jsonb(checks)::text FROM identity.checks checks WHERE id=$2),
                            (SELECT to_jsonb(claim)::text FROM identity.claims claim WHERE id=$2)",
                    &[&challenge_id, &message_id],
                ).await.expect("independent original group");
                [0,1,2,3].map(|column| row.get::<_,Option<String>>(column))
            };
            let original = group().await;
            assert!(original.iter().all(Option::is_some));
            tokio::select! {
                biased;
                _ = challenge.request() => panic!("retention must not verify a token"),
                _ = supplier.request() => panic!("retention must not send a message"),
                () = async {
                    proxy.arm(fixture::database_proxy::Fault::DelayedCommit);
                    let mut retention = service.retention();
                    let task = tokio::spawn(async move { retention.run().await }.in_current_span());
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("deletion commit held");
                    // Independently derived SHA-256 high/low halves for input(1)'s C1 lock.
                    let pids = timeout(Duration::from_secs(5), async {
                        loop {
                            if let Some(row) = reader.client().query_opt(
                                "SELECT waiting.pid, held.pid FROM pg_catalog.pg_locks waiting
                                 JOIN pg_catalog.pg_locks held ON held.locktype=waiting.locktype
                                   AND held.database=waiting.database AND held.classid=waiting.classid
                                   AND held.objid=waiting.objid AND held.objsubid=waiting.objsubid
                                 WHERE waiting.locktype='advisory' AND NOT waiting.granted
                                   AND waiting.mode='ExclusiveLock' AND held.granted
                                   AND held.mode='ExclusiveLock' AND waiting.classid=1847767608
                                   AND waiting.objid=3092674365 AND waiting.objsubid=1
                                   AND waiting.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database())
                                   AND held.pid=ANY(pg_catalog.pg_blocking_pids(waiting.pid))", &[],
                            ).await.expect("exact original operation lock") { break vec![row.get::<_,i32>(0),row.get::<_,i32>(1)]; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("recovery waits for original deletion");
                    assert_eq!(group().await, original);
                    proxy.arm(fixture::database_proxy::Fault::LostReadCommit);
                    proxy.release();
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("readback commit response lost");
                    // No subsequent scan or replay may supply the observed deletion.
                    assert_eq!(group().await, [None,None,None,None]);
                    proxy.release();
                    timeout(Duration::from_secs(5), task).await.expect("bounded recovery").expect("retention task").expect("ordinary cleanup fault keeps scanner usable");
                    timeout(Duration::from_secs(5), async {
                        loop {
                            if reader.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&pids]).await.expect("actual backend end").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("both actual backends ended");
                    service.retention().run().await.expect("later empty scan");
                    assert_eq!(group().await, [None,None,None,None]);
                    stop(context, service).await;
                } => {},
            }
            reader.terminate().await.expect("reader ended");
            challenge.stop().await;
            supplier.stop().await;
            proxy.stop().await;
        });
        logs_assert(|lines| {
            let failures: Vec<_> = lines
                .iter()
                .filter(|line| line.contains("retention_completion"))
                .collect();
            if failures.len() != 1 {
                return Err(format!("one final cleanup diagnostic: {failures:?}"));
            }
            let line = failures[0];
            if !line.contains("00000000-0000-0000-0000-000000000001")
                || !line.contains(r#"kind="unavailable""#)
                || line.contains("Unknown")
            {
                return Err(format!(
                    "known deleted group must not become unknown: {line}"
                ));
            }
            for cause in [
                "RetentionCompleted { cause: RetentionReadback {",
                "cause: Combined { cause: Query(connection closed)",
                "commit: Err(Commit(connection closed))",
                "termination: Err(database driver connection failed)",
            ] {
                if !line.contains(cause) {
                    return Err(format!(
                        "missing known result or cleanup reason {cause}: {line}"
                    ));
                }
            }
            Ok(())
        });
    }

    #[test]
    fn retention_readback_distinguishes_an_orphan_conclusion_rollback_and_incomplete_read() {
        use fixture::database_proxy::{Fault, Proxy};

        #[derive(Debug, Clone, Copy)]
        enum Case {
            Orphan,
            Rollback,
            Incomplete,
        }

        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("driver observation runtime").block_on(async {
            for case in [Case::Orphan, Case::Rollback, Case::Incomplete] {
                let database = Database::initialized(Arc::clone(&server)).await;
                seed(&database, 1, database.now().await-TimeDelta::hours(25),
                    if matches!(case,Case::Orphan) { History::Claimed } else { History::Sent }).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database,&proxy)).await.expect("runtime");
                let service = Service::start(Barrier::begin(context.clone()).await.expect("barrier"), keys(),
                    crate::startup::Settings::new(4,message::Limits::default(),
                        crate::identity::authentication::verification::tests::session_settings(),
                        crate::registration::Limits::default(),
                        crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                    challenge.client(),client(&supplier)).await.expect("service");
                let reader=database.connections(&database.user).recover().await.expect("observer");
                let admin=database.connections("postgres").recover().await.expect("isolated backend control");
                let row=reader.client().query_one("SELECT challenge.id,message.id FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id WHERE challenge.operation_id=$1",&[&input(1).operation_id()]).await.expect("original identities");
                let challenge_id:crate::identity::challenge::Id=row.get(0);
                let message_id:message::Id=row.get(1);
                let group=async || {
                    let row=reader.client().query_one(
                        "SELECT (SELECT to_jsonb(challenge)::text FROM identity.challenges challenge WHERE id=$1),
                                (SELECT to_jsonb(message)::text FROM identity.messages message WHERE id=$2),
                                (SELECT to_jsonb(checks)::text FROM identity.checks checks WHERE id=$2),
                                (SELECT to_jsonb(claim)::text FROM identity.claims claim WHERE id=$2)",
                        &[&challenge_id,&message_id]).await.expect("independent original group");
                    [0,1,2,3].map(|column|row.get::<_,Option<String>>(column))
                };
                let original=group().await;
                let message_facts:String=reader.client().query_one("SELECT (to_jsonb(message)-'status'-'finished_at')::text FROM identity.messages message WHERE id=$1",&[&message_id]).await.expect("immutable message facts").get(0);
                let started_at=database.now().await;
                tokio::select! {
                    biased;
                    ()=async { loop { assert!(timeout(Duration::from_secs(4),challenge.request()).await.is_err(),"no Siteverify during {case:?}"); } }=>{},
                    _=supplier.request()=>panic!("no PNVS during {case:?}"),
                    ()=async {
                        proxy.arm(Fault::DelayedCommit);
                        let worker=service.clone();
                        let task=tokio::spawn(async move { worker.0.reclaim(input(1).operation_id()).await });
                        timeout(Duration::from_secs(5),proxy.intercepted()).await.expect("original write COMMIT held");
                        let (recovery_pid,original_pid)=timeout(Duration::from_secs(5),async { loop {
                            if let Some(row)=reader.client().query_opt(
                                "SELECT waiting.pid,held.pid FROM pg_catalog.pg_locks waiting
                                 JOIN pg_catalog.pg_locks held ON held.locktype=waiting.locktype
                                   AND held.database=waiting.database AND held.classid=waiting.classid
                                   AND held.objid=waiting.objid AND held.objsubid=waiting.objsubid
                                 WHERE waiting.locktype='advisory' AND NOT waiting.granted
                                   AND waiting.mode='ExclusiveLock' AND held.granted AND held.mode='ExclusiveLock'
                                   AND waiting.classid=1847767608 AND waiting.objid=3092674365 AND waiting.objsubid=1
                                   AND waiting.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database())
                                   AND held.pid=ANY(pg_catalog.pg_blocking_pids(waiting.pid))",&[]).await.expect("exact operation and blocking backend") {
                                break (row.get::<_,i32>(0),row.get::<_,i32>(1));
                            }
                            tokio::task::yield_now().await;
                        }}).await.expect("recovery waits for original C1 operation");
                        assert_eq!(group().await,original);
                        proxy.arm(match case { Case::Orphan=>Fault::ReadCommitTermination, Case::Rollback=>Fault::StalledReadCommit, Case::Incomplete=>Fault::StalledQuery });
                        if matches!(case,Case::Rollback) {
                            assert!(admin.client().query_one("SELECT pg_terminate_backend($1)",&[&original_pid]).await.expect("roll back precisely the held isolated transaction").get::<_,bool>(0));
                        }
                        proxy.release();
                        timeout(Duration::from_secs(5),proxy.intercepted()).await.expect("recovery fault intercepted");
                        let recovered=group().await;
                        let result=timeout(Duration::from_secs(12),task).await.expect("single recovery deadline").expect("retention worker");
                        let error=match (case,result) {
                            (Case::Orphan,Err(Error::RetentionCompleted{cause}))=>*cause,
                            (Case::Rollback,Err(Error::NotCommitted{cause}))=>*cause,
                            (Case::Incomplete,Err(Error::Unknown{cause,recovery}))=>{
                                assert!(matches!(*cause,Error::Query(_) | Error::Combined{..}));
                                let recovery=match *recovery { Error::Combined{cause,..}=>*cause,error=>error };
                                assert!(matches!(recovery,Error::Runtime(crate::runtime::Error::Timeout(_))),"actual necessary clock sampling timeout: {recovery:?}");
                                assert_eq!(recovered,[None,None,None,None]);
                                recovery
                            }
                            (_,other)=>panic!("wrong original retention result for {case:?}: {other:?}"),
                        };
                        if !matches!(case,Case::Incomplete) {
                            let Error::RetentionReadback{cause,commit,termination}=error else { panic!("all original and cleanup causes retained: {error:?}"); };
                            let cause=match *cause { Error::Combined{cause,..}=>*cause,error=>error };
                            assert!(matches!(cause,Error::Query(query) if query.0.is_closed()));
                            match case {
                                Case::Orphan=>{
                                    assert!(commit.is_ok(),"successful read COMMIT before FATAL: {commit:?}");
                                    assert!(matches!(termination,Err(platform::database::Error::Postgres(error)) if error.code()==Some(&SqlState::CONNECTION_FAILURE)));
                                    for column in [0,2,3] { assert_eq!(recovered[column],original[column]); }
                                    let row=reader.client().query_one("SELECT status,finished_at,(to_jsonb(message)-'status'-'finished_at')::text FROM identity.messages message WHERE id=$1",&[&message_id]).await.expect("orphan conclusion before replay");
                                    assert_eq!(row.get::<_,String>(0),"unknown");
                                    let finished_at:DateTime<Utc>=row.get(1);
                                    assert!(finished_at>=started_at && finished_at<=database.now().await);
                                    assert_eq!(row.get::<_,String>(2),message_facts);
                                }
                                Case::Rollback=>{
                                    assert!(matches!(commit,Err(EndError::CommitTimeout(_))));
                                    termination.expect("actual locally ended read driver");
                                    assert_eq!(recovered,original,"original rollback preserves all fields before any later cleanup");
                                }
                                Case::Incomplete=>unreachable!(),
                            }
                        }
                        proxy.release();
                        timeout(Duration::from_secs(5),async { loop {
                            if reader.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))",&[&vec![original_pid,recovery_pid]]).await.expect("actual backend ends").get::<_,bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("original and readback backends both ended");
                        service.ready().await.expect("ordinary observation failure does not close unrelated work");
                        if !matches!(case,Case::Incomplete) {
                            service.retention().run().await.expect("later round");
                        }
                        if matches!(case,Case::Orphan) {
                            let receipt=service.send(input(1)).await.expect("original orphan conclusion");
                            assert_eq!(receipt.id(),message_id);
                            assert_eq!(receipt.status().as_ref(),"unknown");
                        } else { assert_eq!(group().await,[None,None,None,None]); }
                        stop(context,service).await;
                    }=>{},
                }
                reader.terminate().await.expect("reader ended");
                admin.terminate().await.expect("backend controller ended");
                challenge.stop().await;
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn sending_after_empty_history_keeps_the_durable_ordinal() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            seed(
                &database,
                1,
                database.now().await - TimeDelta::hours(25),
                History::Sent,
            )
            .await;
            let challenge = Supplier::new().await;
            let mut supplier = fixture::supplier::Supplier::start().await;
            let (context, service) = authentication::dispatch::tests::start(
                &database,
                &challenge,
                &supplier,
                message::Limits::default(),
            )
            .await;
            service
                .retention()
                .run()
                .await
                .expect("delete all message history");
            let reader = database
                .connections(&database.user)
                .recover()
                .await
                .expect("reader");
            assert_eq!(counts(reader.client()).await, (0, 0));
            // Reusing a deleted operation still requires a fresh human verification.
            let mut challenge = challenge;
            let mut request = input(1);
            request.token = "fictional-fresh-token".parse().expect("fresh token");
            let sending = service.clone();
            let task = tokio::spawn(async move { sending.send(request).await });
            challenge.request().await.reply(200, serde_json::json!({
                "success": true, "challenge_ts": database.now().await.to_rfc3339(),
                "hostname": "example.com", "action": "send_authentication_code", "cdata": input(1).operation_id().to_string(),
            }).to_string());
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("one new SMS");
            authentication::dispatch::tests::reply(request, "sent");
            let receipt = task.await.expect("worker").expect("sent");
            assert!(matches!(receipt.status(), message::Status::Sent { .. }));
            let ordinal: i64 = reader
                .client()
                .query_one("SELECT ordinal FROM identity.messages", &[])
                .await
                .expect("new ordinal")
                .get(0);
            assert_eq!(ordinal, 2);
            let mut request = input(1);
            request.token = "fictional-fresh-token".parse().expect("fresh token");
            let replay = service.send(request).await.expect("replay");
            assert_eq!(replay, receipt);
            assert!(
                timeout(Duration::from_millis(30), supplier.request())
                    .await
                    .is_err()
            );
            reader.terminate().await.expect("reader ended");
            stop(context, service).await;
            challenge.stop().await;
        });
    }

    #[test]
    fn deletion_readback_preserves_a_replacement_with_the_same_operation_id() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            seed(
                &database,
                1,
                database.now().await - TimeDelta::hours(25),
                History::Sent,
            )
            .await;
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let writer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("writer");
            let mut attempt = Attempt::Reading;
            let outcome = service
                .0
                .write_retention(writer.client(), input(1).operation_id(), &mut attempt)
                .await
                .expect("original deletion body");
            let Completion::Known(Known { end, .. }) = commit(
                outcome,
                tokio::time::Instant::now() + authentication::TRANSACTION_BUDGET,
                writer.client().batch_execute("COMMIT"),
            )
            .await
            else {
                panic!("confirmed deletion")
            };
            end.expect("original deletion committed");
            let Attempt::Deleting(original) = &attempt else {
                panic!("deletion evidence retained");
            };
            assert_eq!(counts(writer.client()).await, (0, 0));
            seed(&database, 1, database.now().await, History::Sent).await;
            let replacement = authentication::database::challenge::find_receipt(
                writer.client(),
                &keys(),
                &input(1),
            )
            .await
            .expect("read replacement")
            .expect("new original facts");
            assert_ne!(replacement.id(), original.challenge_id);
            let readback = service
                .0
                .recover_retention(input(1).operation_id(), &attempt)
                .await
                .expect("readback confirms only the old group deletion");
            assert!(readback.0.value);
            readback.0.end.expect("read transaction committed");
            readback.1.expect("read driver ended");
            service
                .0
                .reclaim(input(1).operation_id())
                .await
                .expect("stale candidate is harmless");
            assert_eq!(counts(writer.client()).await, (1, 1));
            assert_eq!(
                authentication::database::challenge::find_receipt(
                    writer.client(),
                    &keys(),
                    &input(1)
                )
                .await
                .expect("read current facts")
                .expect("replacement retained"),
                replacement
            );
            writer.terminate().await.expect("writer ended");
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn concurrent_reinsertion_requires_committed_deletion_and_supported_isolation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for mode in ["COMMIT", "ROLLBACK", "OLD SNAPSHOT"] {
                let database = Database::initialized(Arc::clone(&server)).await;
                seed(&database, 1, database.now().await - TimeDelta::hours(25), History::Rejected).await;
                let deleting = database.connections(&database.user).recover().await.expect("deletion writer");
                let inserting = database.connections(&database.user).recover().await.expect("insertion writer");
                let observer = database.connections("postgres").recover().await.expect("fixture observer");
                let pid: i32 = inserting.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("insertion backend").get(0);
                if mode == "OLD SNAPSHOT" {
                    inserting.client().batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT count(*) FROM identity.challenges").await.expect("old snapshot");
                } else {
                    inserting.client().batch_execute("BEGIN").await.expect("insertion transaction");
                }
                deleting.client().batch_execute("BEGIN").await.expect("delete transaction");
                delete(deleting.client(), input(1).operation_id()).await;
                if mode == "OLD SNAPSHOT" { deleting.client().batch_execute("COMMIT").await.expect("deletion commits after old snapshot"); }
                let now = database.now().await;
                let task = tokio::spawn(async move {
                    let mut input = input(1);
                    input.token = "fictional-replacement-token".parse().expect("different token");
                    let result = match authentication::database::challenge::insert(inserting.client(), &keys(), &input, authentication::database::challenge::Subject::Anonymous, now).await {
                        Ok(_) => inserting.client().batch_execute("COMMIT").await.map_err(Error::from),
                        Err(error) => Err(error),
                    };
                    inserting.terminate().await.expect("insert driver ended");
                    result
                });
                if mode != "OLD SNAPSHOT" {
                    timeout(Duration::from_secs(5), async {
                        loop {
                            let waiting: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')", &[&pid]).await.expect("lock wait observation").get(0);
                            if waiting { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("reinsertion waits for old row deletion");
                    deleting.client().batch_execute(mode).await.expect("deletion outcome confirmed");
                }
                let result = timeout(Duration::from_secs(5), task).await.expect("insert ends").expect("insert task");
                if mode == "COMMIT" {
                    result.expect("deleted operation UUID can identify a fresh request");
                } else {
                    assert!(matches!(result, Err(Error::Query(query)) if query.code() == Some(if mode == "OLD SNAPSHOT" { &SqlState::INVALID_TRANSACTION_STATE } else { &SqlState::UNIQUE_VIOLATION })));
                }
                assert_eq!(counts(observer.client()).await, (i64::from(mode != "OLD SNAPSHOT"), 0));
                observer.terminate().await.expect("observer ended");
                deleting.terminate().await.expect("deletion driver ended");
            }
        });
    }

    #[test]
    fn retention_preserves_facts_when_time_or_recovery_evidence_is_invalid() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            seed(
                &database,
                1,
                database.now().await - TimeDelta::hours(25),
                History::Rejected,
            )
            .await;
            let supplier = Supplier::new().await;
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let (context, service) = database
                .start_on(supplier.client(), proxied(&database, &proxy))
                .await;
            let readback = service
                .0
                .recover_retention(input(2).operation_id(), &Attempt::Reading)
                .await
                .expect("no deletion was attempted");
            assert!(!readback.0.value);
            readback.0.end.expect("read transaction committed");
            readback.1.expect("read driver ended");
            let admin = database
                .connections("postgres")
                .recover()
                .await
                .expect("fixture admin");
            proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
            let mut retention = service.retention();
            let task = tokio::spawn(async move { retention.run().await });
            timeout(Duration::from_secs(2), proxy.intercepted())
                .await
                .expect("retention observes rewound time");
            proxy.release();
            assert!(task.await.expect("retention task").is_err());
            assert!(service.ready().await.is_err());
            assert_eq!(counts(admin.client()).await, (1, 0));
            admin.terminate().await.expect("admin ended");
            service.drain().await.expect("drained");
            let _ = context.stop().await;
            supplier.stop().await;
            proxy.stop().await;
        });
    }

    #[traced_test]
    #[test]
    fn cancelled_retention_waiter_keeps_the_workers_final_fact_failure_diagnostic() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            seed(&database, 1, database.now().await - TimeDelta::hours(25), History::Rejected).await;
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let admin = database.connections("postgres").recover().await.expect("fixture admin");
            admin.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(1314210636, 1)").await.expect("hold execution barrier");
            let pid: i32 = admin.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker pid").get(0);
            let observer = database.connections("postgres").recover().await.expect("observer");
            let mut retention = service.retention();
            let mut task = tokio::spawn(async move { retention.run().await }.in_current_span());
            timeout(Duration::from_secs(5), async {
                loop {
                    let waiting: bool = observer.client().query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE $1 = ANY(pg_catalog.pg_blocking_pids(pid)))", &[&pid],
                    ).await.expect("registered worker is blocked").get(0);
                    if waiting { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("worker reached execution barrier");
            assert!(timeout(Duration::from_millis(30), &mut task).await.is_err());
            admin.client().batch_execute("ALTER TABLE identity.challenges DISABLE TRIGGER USER;
                UPDATE identity.challenges SET created_at=created_at+interval '27 hours', expires_at=expires_at+interval '27 hours', finished_at=finished_at+interval '27 hours';
                ALTER TABLE identity.challenges ENABLE TRIGGER USER; COMMIT").await.expect("publish corrupt facts and release worker");
            timeout(Duration::from_secs(5), async {
                loop {
                    if service.ready().await.is_err() { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("fact failure closes admission without scanner");
            assert_eq!(counts(observer.client()).await, (1, 0));
            assert!(matches!(task.await.expect("retention task"), Err(Error::Unknown { cause, recovery })
                if matches!(cause.as_ref(), Error::Time { stage: Stage::ChallengeHistory, .. })
                    && matches!(recovery.as_ref(), Error::Time { stage: Stage::ChallengeHistory, .. })));
            service.drain().await.expect("request work ended");
            context.stop().await.expect("runtime ended");
            admin.terminate().await.expect("admin ended");
            observer.terminate().await.expect("observer ended");
            supplier.stop().await;
        });
        logs_assert(|lines| {
            let events: Vec<_> = lines
                .iter()
                .filter(|line| line.contains(r#"stage="retention_completion""#))
                .collect();
            assert_eq!(events.len(), 1);
            assert!(events[0].contains(&format!("operation_id={}", input(1).operation_id())));
            assert!(events[0].contains("Time"));
            assert!(events[0].contains("ChallengeHistory"));
            Ok(())
        });
        for secret in [
            "13800000000",
            "fictional-token",
            "fictional-secret",
            fixture::PASSWORD,
        ] {
            assert!(!logs_contain(secret));
        }
    }

    #[test]
    fn cursor_advances_past_busy_work_wraps_and_restarts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let old = database.now().await - TimeDelta::hours(25);
            // One writer per fixture is deliberately bounded, and never uses a real database.
            for number in 2..=131 {
                seed(&database, number, old, History::Rejected).await;
            }
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let mut permit = service
                .0
                .registry
                .try_acquire(input(2).operation_id())
                .await
                .expect("first candidate occupied");
            permit.begin();
            let mut retention = service.retention();
            retention.run().await.expect("first page");
            let reader = database
                .connections(&database.user)
                .recover()
                .await
                .expect("reader");
            assert_eq!(counts(reader.client()).await, (3, 0));
            seed(&database, 1, old, History::Rejected).await;
            retention.run().await.expect("later candidates");
            assert_eq!(counts(reader.client()).await, (2, 0));
            permit.finish();
            retention
                .run()
                .await
                .expect("wrap revisits skipped and earlier UUID");
            assert_eq!(counts(reader.client()).await, (0, 0));
            seed(&database, 132, old, History::Rejected).await;
            stop(context, service).await;
            let (context, service) = database.start(supplier.client()).await;
            service
                .retention()
                .run()
                .await
                .expect("restart from beginning");
            assert_eq!(counts(reader.client()).await, (0, 0));
            reader.terminate().await.expect("reader ended");
            stop(context, service).await;
            supplier.stop().await;
        });
    }
}
