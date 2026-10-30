//! Expired credential cleanup uses the same R1 execution and original-operation locks.

use chrono::{DateTime, Utc};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_postgres::Client;

use crate::{
    database::transaction::{Known, confirm},
    history::{self, Read},
    identity::{
        authentication::{
            self, Error, Inner, Service, TRANSACTION_BUDGET, registry::Family,
            session::inconsistent, transaction,
        },
        mobile::envelope::{Binding, Envelope},
        operation, registration, session,
    },
};

pub(in crate::identity::authentication) async fn run(
    service: &Service,
    cursor: &mut Option<operation::Id>,
) -> Result<(), Error> {
    service.0.registry.ready()?;
    let candidates = match fetch_candidates(service, *cursor).await {
        Ok(candidates) => candidates,
        Err(error @ Error::Closed) => return Err(error),
        Err(error) if error.unreliable() => {
            service.0.registry.close();
            return Err(error);
        }
        Err(error) => {
            tracing::error!(stage = "session_retention_scan", error = ?error);
            return Ok(());
        }
    };
    let complete = candidates.len() < 128;
    for operation_id in candidates {
        service.0.registry.ready()?;
        *cursor = Some(operation_id);
        let mut permit = match service
            .0
            .registry
            .try_acquire_for(Family::CreateSession, operation_id)
            .await
        {
            Ok(permit) => permit,
            Err(crate::runtime::work::Error::Busy) => continue,
            Err(error) => return Err(error.into()),
        };
        permit.begin();
        let completion = scopeguard::guard((), |_| {
            service.0.registry.close();
            tracing::error!(%operation_id, stage = "session_retention_completion", error = ?Error::Worker);
        });
        let result = reclaim(&service.0, operation_id).await;
        scopeguard::ScopeGuard::into_inner(completion);
        permit.finish();
        if let Err(error) = result {
            if error.unreliable() {
                service.0.registry.close();
                return Err(error);
            }
            tracing::error!(%operation_id, stage = "session_retention", error = ?error);
        }
    }
    if complete {
        *cursor = None;
    }
    Ok(())
}

async fn fetch_candidates(
    service: &Service,
    cursor: Option<operation::Id>,
) -> Result<Vec<operation::Id>, Error> {
    let lease = service
        .0
        .registry
        .lease_until_closed::<Error>(service.0.context.connections())
        .await?;
    let result = timeout(TRANSACTION_BUDGET, async {
        let now = service.0.now(lease.client()).await?;
        let rows = lease.client().query("SELECT operation_id FROM identity.authentications WHERE ($1::uuid IS NULL OR operation_id > $1) AND (finished_at + interval '24 hours' <= $2 OR EXISTS (SELECT 1 FROM identity.registrations WHERE authentication_id = authentications.id AND expires_at <= $2)) ORDER BY operation_id LIMIT 128", &[&cursor, &now]).await?;
        let mut candidates = rows.into_iter().map(|row| row.try_get(0).map_err(Error::from)).collect::<Result<Vec<operation::Id>, Error>>()?;
        candidates.extend(session::state::authentication_candidates(lease.client(), cursor, now).await?);
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
    Handoff(session::Id),
    Session(session::Id),
    Registration(registration::Id),
    Authentication(authentication::Id),
    AuthenticationSession {
        authentication_id: authentication::Id,
        session_id: session::Id,
    },
    AuthenticationRegistration {
        authentication_id: authentication::Id,
        registration_id: registration::Id,
    },
}

impl Deletion {
    fn credential(&self) -> Option<session::Deletion> {
        match self {
            Self::Handoff(id) => Some(session::Deletion::Handoff(*id)),
            Self::Session(id) | Self::AuthenticationSession { session_id: id, .. } => {
                Some(session::Deletion::Session(*id))
            }
            _ => None,
        }
    }

    async fn remove(
        &self,
        service: &Inner,
        client: &Client,
        operation_id: operation::Id,
        now: DateTime<Utc>,
        pending: &mut Option<history::Pending>,
    ) -> Result<(), Error> {
        if let Some(credential) = self.credential() {
            credential
                .remove(client, &service.app, operation_id, now, pending)
                .await?;
        }
        match self {
            Self::Registration(id)
            | Self::AuthenticationRegistration {
                registration_id: id,
                ..
            } => {
                client
                    .execute("DELETE FROM identity.registrations WHERE id = $1", &[id])
                    .await?;
            }
            Self::Authentication(_)
            | Self::Handoff(_)
            | Self::Session(_)
            | Self::AuthenticationSession { .. } => {}
        }
        match self {
            Self::Authentication(id)
            | Self::AuthenticationSession {
                authentication_id: id,
                ..
            }
            | Self::AuthenticationRegistration {
                authentication_id: id,
                ..
            } => {
                client
                    .execute("DELETE FROM identity.authentications WHERE id = $1", &[id])
                    .await?;
            }
            Self::Handoff(_) | Self::Session(_) | Self::Registration(_) => {}
        }
        Ok(())
    }

    async fn complete(&self, service: &Inner, client: &Client) -> Result<bool, Error> {
        let credential = if let Some(credential) = self.credential() {
            credential.lock(client).await?;
            credential
                .remains(client, &service.app, service.now(client).await?)
                .await?
        } else {
            false
        };
        // A lawful replacement can remove a shorter-lived child while connections change.
        // The longest-lived selected entity determines this attempt's remaining work.
        let retained: bool = match self {
            Self::Handoff(_) | Self::Session(_) => credential,
            Self::Registration(id) => client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM identity.registrations WHERE id = $1)",
                    &[id],
                )
                .await?
                .try_get(0)?,
            Self::Authentication(id) => client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM identity.authentications WHERE id = $1)",
                    &[id],
                )
                .await?
                .try_get(0)?,
            Self::AuthenticationSession {
                authentication_id, ..
            } => {
                let row = client
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM identity.authentications WHERE id = $1)",
                        &[authentication_id],
                    )
                    .await?;
                let authentication: bool = row.try_get(0)?;
                if !authentication && credential {
                    return Err(inconsistent(
                        "a deleted owner cannot retain a dependent credential",
                    ));
                }
                authentication
            }
            Self::AuthenticationRegistration {
                authentication_id,
                registration_id,
            } => {
                let row = client.query_one("SELECT EXISTS (SELECT 1 FROM identity.authentications WHERE id = $1), EXISTS (SELECT 1 FROM identity.registrations WHERE id = $2)", &[authentication_id, registration_id]).await?;
                let authentication: bool = row.try_get(0)?;
                if !authentication && row.try_get::<_, bool>(1)? {
                    return Err(inconsistent(
                        "a deleted authentication cannot retain a registration",
                    ));
                }
                authentication
            }
        };
        Ok(!retained)
    }
}

pub(super) async fn reclaim(service: &Inner, operation_id: operation::Id) -> Result<(), Error> {
    let lease = service.context.connections().lease().await?;
    let deadline = Instant::now() + TRANSACTION_BUDGET;
    let selection = timeout_at(deadline, async {
        transaction::begin_for(
            &service.context,
            lease.client(),
            b"authentication/operation",
            operation_id,
        )
        .await?;
        select(service, lease.client(), operation_id).await
    })
    .await;
    let (deletion, now) = match selection {
        Ok(Ok(deletion)) => deletion,
        Ok(Err(error)) => return Err(error.combine(lease.discard().await)),
        Err(error) => return Err(Error::from(error).combine(lease.discard().await)),
    };
    let mut pending = None;
    let result = timeout_at(deadline, async {
        if let Some(deletion) = &deletion {
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
    let Some(deletion) = deletion else {
        return Err(cause);
    };
    let recovery = async {
        let session = service.context.connections().recover().await?;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let result = timeout_at(deadline, async {
            transaction::begin_for(
                &service.context,
                session.client(),
                b"authentication/operation",
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
                        return Err(inconsistent(
                            "cleanup readback resolves its session candidate",
                        ));
                    }
                }
            }
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
            let cause = Box::new(Error::SessionReadback {
                cause: Box::new(cause),
                commit,
                termination,
            });
            if complete {
                Err(Error::SessionDeleted { cause })
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
    let option = client
        .query_opt(
            "SELECT id FROM identity.authentications WHERE operation_id = $1",
            &[&operation_id],
        )
        .await?;
    let Some(row) = option else {
        return Ok((None, service.now(client).await?));
    };
    let id: authentication::Id = row.try_get(0)?;
    session::state::lock_authentication(client, id).await?;
    if let Some(row) = client
        .query_opt(
            "SELECT id, mobile FROM identity.registrations WHERE authentication_id = $1",
            &[&id],
        )
        .await?
    {
        let mobile: Envelope = row.try_get("mobile")?;
        let mobile = mobile.open(
            &service.keys.encryption,
            &service.keys.mobile,
            Binding::Registration(row.try_get("id")?),
        )?;
        transaction::object(client, b"identity/mobile", mobile.as_bytes()).await?;
    }
    session::state::lock_source(client, id).await?;
    let now = service.now(client).await?;
    // Select one complete group before DELETE; a reused operation is not this attempt.
    let mut eligible: bool = client.query_one("SELECT finished_at + interval '24 hours' <= $2 AND NOT EXISTS (SELECT 1 FROM identity.checks WHERE id = authentications.message_id) AND NOT EXISTS (SELECT 1 FROM identity.attempts WHERE authentication_id = authentications.id) AND NOT EXISTS (SELECT 1 FROM identity.registrations WHERE authentication_id = authentications.id AND expires_at > $2) FROM identity.authentications WHERE id = $1", &[&id, &now]).await?.try_get(0)?;
    if let Some(session) =
        session::find_by_authentication(client, &service.app, Read::Locked, id, now).await?
    {
        session::verify(client, &session, &service.keys.credential, now).await?;
        if session.receipt_id().is_some() {
            eligible = false;
        } else {
            return Ok((
                match session
                    .deletion(client, &service.keys.credential, now)
                    .await?
                {
                    Some(session::Deletion::Session(session_id)) => Some(if eligible {
                        Deletion::AuthenticationSession {
                            authentication_id: id,
                            session_id,
                        }
                    } else {
                        Deletion::Session(session_id)
                    }),
                    Some(session::Deletion::Handoff(session_id)) => {
                        Some(Deletion::Handoff(session_id))
                    }
                    None => None,
                },
                now,
            ));
        }
    }
    if let Some(row) = client.query_opt("SELECT id FROM identity.registrations WHERE authentication_id = $1 AND expires_at <= $2", &[&id, &now]).await? {
        let registration_id = row.try_get("id")?;
        return Ok((Some(if eligible {
            Deletion::AuthenticationRegistration { authentication_id: id, registration_id }
        } else {
            Deletion::Registration(registration_id)
        }), now));
    }
    Ok((eligible.then_some(Deletion::Authentication(id)), now))
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
        identity::authentication::{
            Database, Error, Supplier, proxied, server,
            session::{
                retention::{reclaim, run},
                tests::{CODE, age, input, message},
            },
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
                let service = services.authentication();
                let mut cursor = None;
                let mut discovery = Box::pin(tokio::task::unconstrained(run(service, &mut cursor)));
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
    fn deleted_registration_survives_readback_commit_failure() {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver observation runtime").block_on(async {
            for fault in [Fault::LostReadCommit, Fault::ReadCommitTermination] {
            timeout(Duration::from_secs(25), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, service) = database.start_on(supplier.client(), proxied(&database,&proxy)).await;
                let message_id = message(&database,&service,&mut supplier,"13800000000").await;
                let operation = Uuid::nil();
                service.create_session(input(operation,message_id,"13800000000",CODE)).await.expect("R1 registration");
                age(&database, &service,TimeDelta::minutes(11)).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let original = observer.client().query_one("SELECT authentication.id AS authentication_id,registration.id AS registration_id FROM identity.authentications authentication JOIN identity.registrations registration ON registration.authentication_id=authentication.id WHERE authentication.operation_id=$1", &[&operation]).await.expect("selected original identities");
                let authentication_id: Uuid = original.get("authentication_id");
                let registration_id: Uuid = original.get("registration_id");
                proxy.arm(Fault::DelayedCommit);
                let worker = service.clone();
                let task = tokio::spawn(async move { reclaim(&worker.0,operation.to_string().parse().expect("operation")).await });
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("original cleanup COMMIT held");
                // Independent SHA-256 of LP(authentication/operation, nil UUID), first eight bytes.
                let backends = timeout(Duration::from_secs(3),async { loop {
                    if let Some(row) = observer.client().query_opt("SELECT pid,pg_catalog.pg_blocking_pids(pid) AS blockers FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND classid=2363345061::oid AND objid=2829417220::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid))>0", &[]).await.expect("exact original operation recovery waiter") {
                        let mut backends: Vec<i32> = row.get("blockers"); backends.push(row.get("pid")); break backends;
                    }
                    tokio::task::yield_now().await;
                }}).await.expect("recovery waits for original deletion");
                let terminated = matches!(fault, Fault::ReadCommitTermination);
                proxy.arm(fault); proxy.release();
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("readback COMMIT fault");
                let result = timeout(Duration::from_secs(3),task).await.expect("bounded recovery").expect("cleanup task");
                let deleted: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.registrations WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.authentications WHERE id=$2 AND operation_id=$3 AND status='registrable')", &[&registration_id,&authentication_id,&operation]).await.expect("independent effects before later cleanup").get(0);
                assert!(deleted);
                let error = result.expect_err("cleanup failure remains observable");
                let Error::SessionDeleted { cause } = error else { panic!("complete deletion remains known: {error:?}"); };
                let Error::SessionReadback { cause, commit, termination } = *cause else { panic!("all cleanup causes retained"); };
                assert!(matches!(*cause, Error::Query(_) | Error::Combined { .. }));
                if terminated {
                    assert!(commit.is_ok());
                    assert!(matches!(termination, Err(platform::database::Error::Postgres(ref cause)) if cause.code()==Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                } else { assert!(matches!(commit, Err(crate::database::transaction::EndError::Commit(ref cause)) if cause.0.is_closed())); }
                proxy.release();
                timeout(Duration::from_secs(3),async { loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&backends]).await.expect("actual backend termination").get::<_,bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("original and recovery backends ended");
                service.retention().run().await.expect("later cleanup round");
                observer.terminate().await.expect("observer ended"); service.drain().await.expect("work drained"); context.stop().await.expect("runtime stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(), "no external request through recovery, later cleanup and drain");
                supplier.stop().await; proxy.stop().await;
            }).await.expect("cleanup scenario budget");
            }
        });
    }
}
