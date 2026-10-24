//! R2 owns its short-lived attempts and session credentials under the original R2 operation.

use chrono::{DateTime, Utc};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_postgres::Client;

use crate::{
    database::transaction::{self, Known},
    history,
    identity::{
        account,
        mobile::envelope::{Binding, Envelope},
        operation,
        registration::attempt,
        session,
    },
    registration::{BUDGET, Error, Inner, Service, database},
    runtime::{
        registry::Family,
        work::{self, Failure},
    },
};

pub struct Retention {
    service: Service,
    cursor: Option<operation::Id>,
}

impl Retention {
    pub(super) fn new(service: Service) -> Self {
        Self {
            service,
            cursor: None,
        }
    }

    /// A round visits at most 128 original operations; cancellation remains owned by the process.
    pub async fn run(&mut self) -> Result<(), Error> {
        let service = &self.service.0;
        service.registry.ready()?;
        let candidates = match fetch_candidates(service, self.cursor).await {
            Ok(candidates) => candidates,
            Err(error @ Error::Work(work::Error::Closed)) => return Err(error),
            Err(error) if error.unreliable() => {
                service.registry.close();
                return Err(error);
            }
            Err(error) => {
                tracing::error!(stage = "registration_retention_scan", error = ?error);
                return Ok(());
            }
        };
        let complete = candidates.len() < 128;
        for operation_id in candidates {
            service.registry.ready()?;
            self.cursor = Some(operation_id);
            let mut permit = match service
                .registry
                .try_acquire_for(Family::Register, operation_id)
                .await
            {
                Ok(permit) => permit,
                Err(work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let completion = scopeguard::guard((), |_| {
                service.registry.close();
                tracing::error!(%operation_id, stage = "registration_retention_completion", error = ?work::Error::Worker);
            });
            let result = reclaim(service, operation_id).await;
            scopeguard::ScopeGuard::into_inner(completion);
            permit.finish();
            if let Err(error) = result {
                if error.unreliable() {
                    service.registry.close();
                    return Err(error);
                }
                tracing::error!(%operation_id, stage = "registration_retention", error = ?error);
            }
        }
        if complete {
            self.cursor = None;
        }
        Ok(())
    }
}

async fn fetch_candidates(
    service: &Inner,
    cursor: Option<operation::Id>,
) -> Result<Vec<operation::Id>, Error> {
    let lease = service
        .registry
        .lease_until_closed::<Error>(service.context.connections())
        .await?;
    let result = timeout(BUDGET, async {
        let now = service.now(lease.client()).await?;
        let rows = lease.client().query("SELECT operation_id FROM identity.attempts WHERE accepted_at + interval '24 hours' <= $2 AND ($1::uuid IS NULL OR operation_id > $1) ORDER BY operation_id LIMIT 128", &[&cursor, &now]).await?;
        let mut candidates = rows.into_iter().map(|row| row.try_get(0).map_err(Error::from)).collect::<Result<Vec<operation::Id>, Error>>()?;
        candidates.extend(session::state::registration_candidates(lease.client(), cursor, now).await?);
        candidates.sort_unstable_by_key(|id| *id.as_bytes());
        candidates.dedup();
        candidates.truncate(128);
        Ok::<_, Error>(candidates)
    }).await;
    match result {
        Ok(Ok(candidates)) => {
            lease.release();
            Ok(candidates)
        }
        Ok(Err(error)) => Err(error.combine(lease.discard().await)),
        Err(error) => Err(Error::from(error).combine(lease.discard().await)),
    }
}

enum Deletion {
    Attempt(attempt::Id),
    Credential(session::Deletion),
    Both {
        attempt_id: attempt::Id,
        credential: session::Deletion,
    },
}

impl Deletion {
    async fn remove(
        &self,
        service: &Inner,
        client: &Client,
        operation_id: operation::Id,
        now: DateTime<Utc>,
        pending: &mut Option<history::Pending>,
    ) -> Result<(), Error> {
        match self {
            Self::Credential(credential) | Self::Both { credential, .. } => {
                credential
                    .remove(client, &service.app, operation_id, now, pending)
                    .await?
            }
            Self::Attempt(_) => {}
        }
        match self {
            Self::Attempt(id) | Self::Both { attempt_id: id, .. } => {
                client
                    .execute("DELETE FROM identity.attempts WHERE id = $1", &[id])
                    .await?;
            }
            Self::Credential(_) => {}
        }
        Ok(())
    }

    async fn complete(&self, service: &Inner, client: &Client) -> Result<bool, Error> {
        let credential = match self {
            Self::Credential(credential) | Self::Both { credential, .. } => {
                credential.lock(client).await?;
                credential
                    .remains(client, &service.app, service.now(client).await?)
                    .await?
            }
            Self::Attempt(_) => false,
        };
        let attempt = match self {
            Self::Attempt(id) | Self::Both { attempt_id: id, .. } => client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM identity.attempts WHERE id = $1)",
                    &[id],
                )
                .await?
                .try_get::<_, bool>(0)?,
            Self::Credential(_) => false,
        };
        // These independent lifetimes may end in another cleanup or account cancellation.
        // A remaining entity does not imply that every selected child is unchanged.
        Ok(!attempt && !credential)
    }
}

async fn reclaim(service: &Inner, operation_id: operation::Id) -> Result<(), Error> {
    let lease = service.context.connections().lease().await?;
    let deadline = Instant::now() + BUDGET;
    let selection = timeout_at(deadline, async {
        transaction::begin(
            &service.context,
            lease.client(),
            b"registration/operation",
            operation_id,
        )
        .await?;
        select(service, lease.client(), operation_id).await
    })
    .await;
    let (option, now) = match selection {
        Ok(Ok(option)) => option,
        Ok(Err(error)) => return Err(error.combine(lease.discard().await)),
        Err(error) => return Err(Error::from(error).combine(lease.discard().await)),
    };
    let mut pending = None;
    let result = timeout_at(deadline, async {
        if let Some(deletion) = &option {
            deletion
                .remove(service, lease.client(), operation_id, now, &mut pending)
                .await?;
        }
        lease.client().batch_execute("COMMIT").await?;
        Ok::<_, Error>(())
    })
    .await;
    let error = match result {
        Ok(Ok(())) => {
            lease.release();
            if let Some(pending) = pending {
                pending.committed()?;
            }
            return Ok(());
        }
        Ok(Err(error)) => error,
        Err(error) => error.into(),
    };
    let cause = error.combine(lease.discard().await);
    let Some(deletion) = option else {
        return Err(cause);
    };
    let recovery = async {
        let session = service.context.connections().recover().await?;
        let deadline = Instant::now() + BUDGET;
        let result = timeout_at(deadline, async {
            transaction::begin(
                &service.context,
                session.client(),
                b"registration/operation",
                operation_id,
            )
            .await?;
            let complete = deletion.complete(service, session.client()).await?;
            if let Some(candidate) = pending.take() {
                match candidate.resolution()? {
                    Some(true) => candidate.committed()?,
                    Some(false) => candidate.rolled_back()?,
                    None => {
                        candidate.uncertain();
                        return Err(Error::Facts {
                            rule: "cleanup readback resolves its session candidate",
                        });
                    }
                }
            }
            Ok::<_, Error>(complete)
        })
        .await;
        let result = match result {
            Ok(Ok(complete)) => Ok(transaction::confirm(
                complete,
                deadline,
                session.client().batch_execute("COMMIT"),
            )
            .await),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(result) => {
                let (Known { value, end }, cleanup) = result.terminate(session).await;
                Ok((value, end, cleanup))
            }
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }
    .await;
    if let Some(pending) = pending {
        pending.uncertain();
    }
    match recovery {
        Ok((true, Ok(()), Ok(()))) if !cause.unreliable() => Ok(()),
        Ok((complete, commit, termination)) => {
            let cause = Box::new(Error::Readback {
                cause: Box::new(cause),
                commit,
                termination,
            });
            if complete {
                Err(Error::Deleted { cause })
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

async fn select(
    service: &Inner,
    client: &Client,
    operation_id: operation::Id,
) -> Result<(Option<Deletion>, DateTime<Utc>), Error> {
    if let Some(row) = client
        .query_opt(
            "SELECT account_id FROM identity.receipts WHERE operation_id = $1",
            &[&operation_id],
        )
        .await?
    {
        database::lock_account(client, row.try_get::<_, account::Id>(0)?).await?;
    }
    if let Some(row) = client
        .query_opt(
            "SELECT id, mobile FROM identity.attempts WHERE operation_id = $1",
            &[&operation_id],
        )
        .await?
    {
        let mobile: Envelope = row.try_get("mobile")?;
        let mobile = mobile.open(
            &service.keys.encryption,
            &service.keys.mobile,
            Binding::Attempt(row.try_get("id")?),
        )?;
        transaction::object(client, b"identity/mobile", mobile.as_bytes()).await?;
    }
    if let Some(row) = client
        .query_opt(
            "SELECT id FROM identity.receipts WHERE operation_id=$1",
            &[&operation_id],
        )
        .await?
    {
        session::state::lock_receipt(client, row.try_get("id")?).await?;
    }
    let now = service.now(client).await?;
    let option = database::find_attempt_by_operation(client, operation_id).await?;
    if let Some(attempt) = &option {
        attempt.validate(
            &service.keys.input,
            &service.keys.encryption,
            &service.keys.mobile,
            service.settings.app_id(),
            now,
        )?;
    }
    let mut credential = None;
    if let Some(receipt) = database::find_receipt_by_operation(client, operation_id).await? {
        let account = receipt
            .validate(
                client,
                &service.app,
                history::Read::Locked,
                service.settings.app_id(),
                now,
            )
            .await?;
        if let Some(attempt) = option {
            attempt.validate_receipt(&receipt, &account)?;
        }
        if let Some(session) = session::find_by_receipt(
            client,
            &service.app,
            crate::history::Read::Locked,
            receipt.id,
            now,
        )
        .await?
        {
            receipt.validate_session(&session, &account)?;
            session::verify(client, &session, &service.keys.credential, now).await?;
            credential = session
                .deletion(client, &service.keys.credential, now)
                .await?;
        }
    }
    let option = client.query_opt("SELECT id FROM identity.attempts WHERE operation_id = $1 AND accepted_at + interval '24 hours' <= $2 AND NOT EXISTS (SELECT 1 FROM identity.registrations WHERE id = attempts.credential_id AND expires_at > $2)", &[&operation_id, &now]).await?.map(|row| row.try_get(0)).transpose()?;
    let Some(credential) = credential else {
        return Ok((option.map(Deletion::Attempt), now));
    };
    Ok((
        Some(match option {
            Some(attempt_id) => Deletion::Both {
                attempt_id,
                credential,
            },
            None => Deletion::Credential(credential),
        }),
        now,
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::poll_fn, sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::role::tests::start,
        database::transaction::EndError,
        identity::authentication::{
            Database, Supplier, proxied, server,
            session::{
                Receipt,
                tests::{CODE, account, count, input, message},
            },
        },
        registration::{
            Error,
            retention::reclaim,
            tests::{age_facts, registration_input, registration_services},
        },
    };

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
                let mut retention = services.registration().retention();
                let mut discovery = Box::pin(tokio::task::unconstrained(retention.run()));
                // Discovery reaches the exhausted pool before observing transaction time.
                poll_fn(|context| {
                    assert!(discovery.as_mut().poll(context).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                services.close();
                let result = timeout(Duration::from_millis(500), discovery)
                    .await
                    .expect("queued discovery stops without the pool wait timeout");
                assert!(matches!(
                    result,
                    Err(Error::Work(crate::runtime::work::Error::Closed))
                ));
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
    fn deleted_original_entities_remain_known_after_readback_commit_response_loss() {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("driver observation runtime").block_on(async {
            for fault in [Fault::LostReadCommit, Fault::StalledReadCommit, Fault::ReadCommitTermination] {
            timeout(Duration::from_secs(25), async {
                let database = Database::new(Arc::clone(&server)).await;
                account(&database, "13900000000", false).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 registration credential") {
                    Receipt::Registration(value) => value,
                    Receipt::Session(_) => panic!("unregistered mobile"),
                };
                let operation = "00000000-0000-4000-8000-000000000001".parse().expect("operation");
                services.registration().register(registration_input(operation, &credential, "00000020000229007X")).await.expect("complete registration");
                age_facts(&database, &services, TimeDelta::hours(49)).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let original = observer.client().query_one("SELECT attempt.id AS attempt_id, session.id AS session_id FROM identity.attempts attempt JOIN identity.receipts receipt ON receipt.id=attempt.id JOIN identity.sessions session ON session.receipt_id=receipt.id WHERE attempt.operation_id=$1", &[&operation]).await.expect("original entities");
                let attempt_id: Uuid = original.get("attempt_id");
                let session_id: Uuid = original.get("session_id");
                proxy.arm(Fault::DelayedCommit);
                let service = services.registration().clone();
                let task = tokio::spawn(async move { reclaim(&service.0, operation).await });
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original deletion COMMIT held");
                // Independently encoded SHA-256 LP(registration/operation, UUID), first eight bytes.
                let backends = timeout(Duration::from_secs(3), async { loop {
                    if let Some(row) = observer.client().query_opt("SELECT waiter.pid, pg_catalog.pg_blocking_pids(waiter.pid) AS blockers FROM pg_catalog.pg_locks waiter WHERE waiter.locktype='advisory' AND NOT waiter.granted AND waiter.mode='ExclusiveLock' AND waiter.classid=112050784 AND waiter.objid=3250154416 AND waiter.objsubid=1 AND waiter.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(waiter.pid))>0", &[]).await.expect("exact R2 operation waiter") {
                        let mut backends: Vec<i32> = row.get("blockers");
                        backends.push(row.get("pid"));
                        break backends;
                    }
                    tokio::task::yield_now().await;
                }}).await.expect("readback waits for original deletion");
                let stalled = matches!(fault, Fault::StalledReadCommit);
                let terminated = matches!(fault, Fault::ReadCommitTermination);
                proxy.arm(fault);
                let recovery = tokio::time::Instant::now();
                proxy.release();
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback COMMIT response lost");
                let result = timeout(Duration::from_secs(12), task).await.expect("readback finished within its deadline").expect("cleanup task");
                assert!(recovery.elapsed() < Duration::from_secs(12));
                let row = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.attempts WHERE id=$1) AS attempt, EXISTS(SELECT 1 FROM identity.sessions WHERE id=$2) AS session, EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2) AS handoff, EXISTS(SELECT 1 FROM identity.receipts WHERE id=$1 AND operation_id=$3) AS receipt", &[&attempt_id, &session_id, &operation]).await.expect("independent effects before another cleanup");
                assert!(!row.get::<_,bool>("attempt"));
                assert!(!row.get::<_,bool>("session"));
                assert!(!row.get::<_,bool>("handoff"));
                assert!(row.get::<_,bool>("receipt"));
                let error = result.expect_err("readback cleanup failure remains observable");
                let Error::Deleted { cause } = error else { panic!("complete deletion remains known: {error:?}"); };
                let Error::Readback { cause, commit, termination } = *cause else { panic!("cleanup causes remain available"); };
                assert!(matches!(*cause, Error::Query(_) | Error::Combined { .. }));
                if stalled {
                    assert!(matches!(commit, Err(EndError::CommitTimeout(_))));
                    assert!(termination.is_ok());
                } else if terminated {
                    assert!(commit.is_ok());
                    assert!(matches!(termination, Err(platform::database::Error::Postgres(ref cause)) if cause.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                } else {
                    assert!(matches!(commit, Err(EndError::Commit(ref cause)) if cause.0.is_closed()));
                }
                proxy.release();
                timeout(Duration::from_secs(3), async { loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&backends]).await.expect("actual backend completion").get::<_,bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("original and recovery backends ended");
                services.registration().retention().run().await.expect("later cleanup round");
                observer.terminate().await.expect("observer ended");
                services.drain().await.expect("work drained");
                context.stop().await.expect("runtime stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "cleanup, recovery and drain make no supplier call");
                supplier.stop().await;
                proxy.stop().await;
            }).await.expect("cleanup readback budget");
            }
        });
    }

    #[test]
    fn retained_entities_and_incomplete_cleanup_reads_keep_distinct_outcomes() {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("driver observation runtime").block_on(async {
            for fault in [Fault::LostReadCommit, Fault::StalledReadCommit, Fault::ReadCommitTermination, Fault::StalledQuery] {
                timeout(Duration::from_secs(35), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("R1 credential") {
                        Receipt::Registration(value) => value,
                        Receipt::Session(_) => panic!("unregistered mobile"),
                    };
                    let operation = "00000000-0000-4000-8000-000000000001".parse().expect("operation");
                    assert!(matches!(services.registration().register(registration_input(operation, &credential, "000000200002290010")).await, Err(Error::IdNumberTaken)));
                    age_facts(&database, &services, TimeDelta::hours(49)).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let attempt_id: Uuid = observer.client().query_one("SELECT id FROM identity.attempts WHERE operation_id=$1", &[&operation]).await.expect("original refused attempt").get(0);
                    proxy.arm(Fault::DelayedRollback);
                    let service = services.registration().clone();
                    let task = tokio::spawn(async move { reclaim(&service.0, operation).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("deletion held before rollback");
                    let gate = database.connections(&database.user).recover().await.expect("original operation gate");
                    let gate_pid: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate PID").get(0);
                    let gate_context = context.clone();
                    let gate_task = tokio::spawn(async move {
                        crate::database::transaction::begin(&gate_context, gate.client(), b"registration/operation", operation).await.expect("same operation gate");
                        gate
                    });
                    let mut backends = timeout(Duration::from_secs(3), async { loop {
                        let pids: Vec<i32> = observer.client().query_one("SELECT pg_catalog.pg_blocking_pids($1)", &[&gate_pid]).await.expect("gate queued behind original deletion").get(0);
                        if !pids.is_empty() { break pids; }
                        tokio::task::yield_now().await;
                    }}).await.expect("gate actually waits before rollback");
                    proxy.release();
                    let gate = timeout(Duration::from_secs(3), gate_task).await.expect("original rollback releases gate").expect("gate task");
                    let recovery_pid = timeout(Duration::from_secs(3), async { loop {
                        if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND classid=112050784 AND objid=3250154416 AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&gate_pid]).await.expect("exact operation recovery waiter") { break row.get::<_,i32>(0); }
                        tokio::task::yield_now().await;
                    }}).await.expect("readback waits on gate");
                    backends.push(recovery_pid);
                    let incomplete = matches!(fault, Fault::StalledQuery);
                    let stalled = matches!(fault, Fault::StalledReadCommit);
                    let terminated = matches!(fault, Fault::ReadCommitTermination);
                    proxy.arm(fault);
                    let recovery = tokio::time::Instant::now();
                    gate.client().batch_execute("ROLLBACK").await.expect("release readback");
                    gate.terminate().await.expect("gate ended");
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback fault intercepted");
                    let error = timeout(Duration::from_secs(12), task).await.expect("one recovery deadline").expect("cleanup task").expect_err("retained entity or unknown read");
                    assert!(recovery.elapsed() < Duration::from_secs(12));
                    let retained: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.attempts WHERE id=$1 AND operation_id=$2 AND status='id_number_taken') AND NOT EXISTS(SELECT 1 FROM identity.receipts WHERE operation_id=$2) AND EXISTS(SELECT 1 FROM identity.registrations WHERE id=$3)", &[&attempt_id, &operation, &credential.id()]).await.expect("independent original refusal before another cleanup").get(0);
                    assert!(retained);
                    if incomplete {
                        assert!(matches!(error, Error::Unknown { recovery, .. } if matches!(*recovery, Error::Timeout(_))));
                    } else {
                        let Error::NotCommitted { cause } = error else { panic!("original entity remains known: {error:?}"); };
                        let Error::Readback { cause, commit, termination } = *cause else { panic!("complete cleanup causes"); };
                        assert!(matches!(*cause, Error::Query(_) | Error::Combined { .. }));
                        if stalled {
                            assert!(matches!(commit, Err(EndError::CommitTimeout(_))));
                            assert!(termination.is_ok());
                        } else if terminated {
                            assert!(commit.is_ok());
                            assert!(matches!(termination, Err(platform::database::Error::Postgres(ref cause)) if cause.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        } else { assert!(matches!(commit, Err(EndError::Commit(ref cause)) if cause.0.is_closed())); }
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&backends]).await.expect("original and recovery backend termination").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("both actual backends ended");
                    services.registration().retention().run().await.expect("later cleanup may delete the original refusal");
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 0);
                    observer.terminate().await.expect("observer ended");
                    services.drain().await.expect("work drained"); context.stop().await.expect("runtime stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no supplier call through later cleanup and drain");
                    supplier.stop().await; proxy.stop().await;
                }).await.expect("retained cleanup readback budget");
            }
        });
    }

    #[test]
    fn cleanup_commit_faults_reconcile_the_original_entities_and_keep_the_permanent_receipt() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for refusal in [false, true] {
                for fault in [Fault::LostCommit, Fault::DelayedCommit, Fault::RolledBack, Fault::RejectedBegin] {
                    timeout(Duration::from_secs(25), async {
                        let database = Database::new(Arc::clone(&server)).await;
                        account(&database, "13900000000", false).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = registration_services(proxied(&database, &proxy), &supplier, 4).await;
                        let message_id = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                        let credential = match services.authentication().create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE)).await.expect("registration credential") {
                            Receipt::Registration(value) => value, Receipt::Session(_) => panic!("unregistered mobile"),
                        };
                        let operation = Uuid::new_v4().to_string().parse().expect("operation");
                        let result = services.registration().register(registration_input(operation, &credential, if refusal { "000000200002290010" } else { "00000020000229007X" })).await;
                        if refusal { assert!(matches!(result, Err(Error::IdNumberTaken))); } else { assert!(result.is_ok()); }
                        age_facts(&database, &services, TimeDelta::hours(49)).await;
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        services.registration().verify(observer.client(), context.sample().await.expect("trusted history time")).await.expect("healthy complete graph before cleanup");
                        let retained = matches!(fault, Fault::RolledBack | Fault::RejectedBegin);
                        let early = matches!(fault, Fault::RejectedBegin);
                        let delayed = matches!(fault, Fault::DelayedCommit);
                        proxy.arm(fault);
                        let service = services.registration().clone();
                        let task = tokio::spawn(async move { reclaim(&service.0, operation).await });
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual R2 cleanup boundary intercepted");
                        if delayed {
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let blocked: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0)", &[]).await.expect("recovery waiter").get(0);
                                    if blocked { break; } tokio::task::yield_now().await;
                                }
                            }).await.expect("cleanup recovery waits for original backend");
                            assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 1);
                            assert!(!task.is_finished());
                        }
                        proxy.release();
                        let result = timeout(Duration::from_secs(5), task).await.expect("bounded cleanup recovery").expect("cleanup task");
                        if early { assert!(result.is_err(), "early failure is not reported as completed deletion"); }
                        else if retained { assert!(matches!(result, Err(Error::NotCommitted { .. }))); }
                        else { result.expect("original complete deletion recovered"); }
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, i64::from(retained));
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.receipts").await, i64::from(!refusal));
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, i64::from(retained && !refusal));
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, i64::from(retained && !refusal));
                        services.registration().retention().run().await.expect("later cleanup completes any confirmed remainder");
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.attempts").await, 0);
                        let mut authentication = services.authentication().retention();
                        authentication.run().await.expect("C1 check retention ends after R2 children");
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.checks").await, 0);
                        authentication.run().await.expect("R1 parent becomes eligible after its check is gone");
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 0);
                        if !refusal {
                            assert!(matches!(services.registration().register(registration_input(operation, &credential, "00000020000229007X")).await, Err(Error::Verification)));
                        }
                        observer.terminate().await.expect("observer ended"); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
                    }).await.expect("cleanup scenario budget");
                }
            }
        });
    }

    #[test]
    fn cleanup_rejects_corrupt_pairing_before_deletion_and_closes_shared_writes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (corruption, fact_rule) in [
                (
                    "UPDATE identity.attempts SET status='id_number_taken'",
                    Some("retained attempt and receipt preserve the same completion"),
                ),
                (
                    "UPDATE identity.authentications SET app_id='different-website'",
                    Some("attempt retains the running website source"),
                ),
                (
                    "UPDATE identity.sessions SET hash=set_byte(hash,0,get_byte(hash,0)#1)",
                    None,
                ),
            ] {
                timeout(Duration::from_secs(20), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13900000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        registration_services(database.connections(&database.user), &supplier, 4)
                            .await;
                    let message_id = message(
                        &database,
                        services.authentication(),
                        &mut supplier,
                        "13800000000",
                    )
                    .await;
                    let credential = match services
                        .authentication()
                        .create_session(input(Uuid::new_v4(), message_id, "13800000000", CODE))
                        .await
                        .expect("credential")
                    {
                        Receipt::Registration(value) => value,
                        Receipt::Session(_) => panic!("unregistered"),
                    };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    services
                        .registration()
                        .register(registration_input(
                            operation,
                            &credential,
                            "00000020000229007X",
                        ))
                        .await
                        .expect("complete registration");
                    age_facts(&database, &services, TimeDelta::hours(49)).await;
                    let owner = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("isolated corruption");
                    owner
                        .client()
                        .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
                        .await
                        .expect("isolated corruption transaction");
                    owner
                        .client()
                        .batch_execute(corruption)
                        .await
                        .expect("one incompatible retained fact");
                    owner
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .expect("corruption committed");
                    owner.terminate().await.expect("fixture ended");
                    let error = services
                        .registration()
                        .retention()
                        .run()
                        .await
                        .expect_err("corrupt retained facts prevent cleanup");
                    let expected = match fact_rule {
                        Some(rule) => {
                            matches!(&error, Error::Facts { rule: actual } if *actual == rule)
                        }
                        None => matches!(
                            &error,
                            Error::History(crate::history::Error::Seal(
                                crate::keys::seal::Error::Mismatch
                            ))
                        ),
                    };
                    assert!(expected, "{corruption}: {error:?}");
                    assert!(
                        crate::runtime::work::Failure::unreliable(&error),
                        "corruption closes shared writes: {error:?}"
                    );
                    assert!(services.ready().await.is_err());
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.attempts").await,
                        1
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.receipts").await,
                        1
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.sessions").await,
                        1
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.handoffs").await,
                        1
                    );
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                })
                .await
                .expect("corrupt cleanup budget");
            }
        });
    }
}
