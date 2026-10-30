//! Bounded cleanup retains each original result through its recovery and code dependencies.

use tokio::time::{Instant, timeout, timeout_at};

use crate::{
    database::transaction::{self, Known},
    identity::{
        operation,
        transfer::{BUDGET, Error, FAMILY, Inner, Service, completion, database},
    },
    runtime::{
        registry::Family,
        work::{self, Failure},
    },
};

pub struct Retention {
    service: Service,
    cursor: Option<Candidate>,
}

#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Transfer,
    Completion,
}

impl Purpose {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::Completion => "completion",
        }
    }

    fn namespace(self) -> &'static [u8] {
        match self {
            Self::Transfer => FAMILY,
            Self::Completion => completion::FAMILY,
        }
    }

    fn family(self) -> Family {
        match self {
            Self::Transfer => Family::Transfer,
            Self::Completion => Family::Kernel(crate::kernel::operation::Family::MobileCompletion),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Candidate {
    pub(super) purpose: Purpose,
    pub(super) operation_id: operation::Id,
}

impl Retention {
    pub(super) fn new(service: Service) -> Self {
        Self {
            service,
            cursor: None,
        }
    }

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
                tracing::error!(stage = "transfer_retention_scan", error = ?error);
                return Ok(());
            }
        };
        let complete = candidates.len() < 128;
        for candidate in candidates {
            let operation_id = candidate.operation_id;
            service.registry.ready()?;
            self.cursor = Some(candidate);
            let mut permit = match service
                .registry
                .try_acquire_for(candidate.purpose.family(), operation_id)
                .await
            {
                Ok(permit) => permit,
                Err(work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let completion = scopeguard::guard((), |_| {
                service.registry.close();
                tracing::error!(%operation_id, stage = "transfer_retention", error = ?work::Error::Worker);
            });
            let result = reclaim(service, candidate).await;
            scopeguard::ScopeGuard::into_inner(completion);
            permit.finish();
            if let Err(error) = result {
                if error.unreliable() {
                    service.registry.close();
                    return Err(error);
                }
                tracing::error!(%operation_id, stage = "transfer_retention", error = ?error);
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
    cursor: Option<Candidate>,
) -> Result<Vec<Candidate>, Error> {
    let lease = service
        .registry
        .lease_until_closed::<Error>(service.context.connections())
        .await?;
    let cause = match timeout(BUDGET, async {
        let now = service.context.now(lease.client()).await?;
        database::fetch_candidates(lease.client(), cursor, now).await
    })
    .await
    {
        Ok(Ok(candidates)) => {
            lease.release();
            return Ok(candidates);
        }
        Ok(Err(cause)) => cause,
        Err(cause) => cause.into(),
    };
    Err(cause.combine(lease.discard().await))
}

async fn reclaim(service: &Inner, candidate: Candidate) -> Result<(), Error> {
    let operation_id = candidate.operation_id;
    let namespace = candidate.purpose.namespace();
    let lease = service.context.connections().lease().await?;
    let deadline = Instant::now() + BUDGET;
    let selection = timeout_at(deadline, async {
        transaction::begin(&service.context, lease.client(), namespace, operation_id).await?;
        let now = service.context.now(lease.client()).await?;
        match candidate.purpose {
            Purpose::Transfer => {
                if let Some(original) =
                    database::find_by_operation(lease.client(), service.app.keys(), operation_id)
                        .await?
                {
                    original.validate(&service.keys, now)?;
                }
            }
            Purpose::Completion => {
                completion::verify_retained(
                    lease.client(),
                    &service.app,
                    &service.keys,
                    operation_id,
                    now,
                )
                .await?;
            }
        }
        database::find_deletion_by_operation(lease.client(), candidate, now).await
    })
    .await;
    let option = match selection {
        Ok(Ok(option)) => option,
        Ok(Err(error)) => return Err(error.combine(lease.discard().await)),
        Err(error) => return Err(Error::from(error).combine(lease.discard().await)),
    };
    let cause = match timeout_at(deadline, async {
        if let Some(deletion) = &option {
            deletion.remove(lease.client()).await?;
            if matches!(candidate.purpose, Purpose::Completion) {
                crate::kernel::completion::remove_expired::<completion::Rejection>(
                    lease.client(),
                    service.app.keys(),
                    completion::COMPLETIONS,
                    operation_id,
                    service.context.now(lease.client()).await?,
                )
                .await?;
            }
        }
        lease.client().batch_execute("COMMIT").await?;
        Ok::<_, Error>(())
    })
    .await
    {
        Ok(Ok(())) => {
            lease.release();
            return Ok(());
        }
        Ok(Err(cause)) => cause,
        Err(cause) => cause.into(),
    }
    .combine(lease.discard().await);
    let Some(deletion) = option else {
        return Err(cause);
    };
    let recovery = async {
        let session = service.context.connections().recover().await?;
        let deadline = Instant::now() + BUDGET;
        let result = match timeout_at(deadline, async {
            transaction::begin(&service.context, session.client(), namespace, operation_id).await?;
            let deleted = deletion.complete(session.client()).await?;
            if deleted && matches!(candidate.purpose, Purpose::Completion) {
                let retained = crate::kernel::completion::original::<completion::Rejection>(
                    session.client(),
                    service.app.keys(),
                    completion::COMPLETIONS,
                    operation_id,
                )
                .await?;
                if retained.is_some() {
                    return Err(Error::Facts {
                        rule: "rejection cleanup removes its completion in the same transaction",
                    });
                }
            }
            Ok(deleted)
        })
        .await
        {
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
            Ok(result) => Ok(result.terminate(session).await),
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }
    .await;
    match recovery {
        Ok((
            Known {
                value: true,
                end: Ok(()),
            },
            Ok(()),
        )) if !cause.unreliable() => Ok(()),
        Ok((
            Known {
                value: true,
                end: commit,
            },
            termination,
        )) => Err(Error::Deleted {
            cause: Box::new(cause),
            commit,
            termination,
        }),
        Ok((
            Known {
                value: false,
                end: commit,
            },
            termination,
        )) => Err(Error::NotCommitted {
            cause: Box::new(cause),
            commit,
            termination,
        }),
        Err(recovery) => Err(Error::Unknown {
            cause: Box::new(cause),
            recovery: Box::new(recovery),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::poll_fn, time::Duration};

    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        access::role::tests::start,
        identity::{
            authentication::{Database, Supplier, server},
            transfer::Error,
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
                let mut retention = services.transfer().retention();
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
}
