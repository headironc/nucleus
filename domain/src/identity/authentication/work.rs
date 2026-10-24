use std::sync::Arc;

use tokio::{
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use tracing::Instrument;

use crate::identity::authentication::{Error, Execute, Inner, Input, registry::Registry};

/// Own accepted request tasks independently of their HTTP waiters.
#[derive(Default)]
pub(super) struct Work {
    tasks: Mutex<Tasks>,
}

impl Work {
    pub(super) async fn ready(&self, registry: &Registry) -> Result<(), Error> {
        let mut tasks = self.tasks.lock().await;
        tasks.reap(registry).await;
        registry.ready()
    }

    pub(super) async fn run<A: Execute>(
        &self,
        service: Arc<Inner>,
        input: Input,
        action: A,
    ) -> Result<A::Output, Error> {
        self.ready(&service.registry).await?;
        let operation_id = input.operation_id;
        let mut permit = service.registry.acquire(operation_id).await?;
        let mut tasks = self.tasks.lock().await;
        tasks.reap(&service.registry).await;
        service.registry.ready()?;
        let (reply, receiver) = oneshot::channel();
        permit.begin();
        // No suspension between accepting execution and retaining its handle.
        let task = tokio::spawn(
            async move {
                let completion = scopeguard::guard((), |_| {
                    service.registry.close();
                    Error::Worker.report(operation_id);
                });
                let result = action.execute(&service, &input).await;
                scopeguard::ScopeGuard::into_inner(completion);
                match &result {
                    Ok(_) => {
                        tracing::info!(stage = "authentication_completion", outcome = "succeeded")
                    }
                    Err(cause) => {
                        if cause.unreliable() {
                            service.registry.close();
                        }
                        cause.report(operation_id);
                    }
                }
                permit.finish();
                let _ = reply.send(result);
            }
            .instrument(tracing::info_span!("authentication", %operation_id)),
        );
        tasks.handles.push(task);
        drop(tasks);
        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(Error::Worker),
        }
    }

    pub(super) async fn drain(&self, registry: &Registry) -> Result<(), Error> {
        registry.close();
        let mut tasks = self.tasks.lock().await;
        while let Some(task) = tasks.handles.last_mut() {
            if let Err(error) = task.await {
                tasks.failures.push(error.into());
            }
            tasks.handles.pop();
        }
        let failures = std::mem::take(&mut tasks.failures);
        if failures.is_empty() {
            return Ok(());
        }
        Err(Error::Drain(failures))
    }
}

#[derive(Default)]
struct Tasks {
    handles: Vec<JoinHandle<()>>,
    failures: Vec<Task>,
}

impl Tasks {
    async fn reap(&mut self, registry: &Registry) {
        let mut index = 0;
        while index < self.handles.len() {
            if !self.handles[index].is_finished() {
                index += 1;
                continue;
            }
            if let Err(error) = (&mut self.handles[index]).await {
                registry.close();
                self.failures.push(error.into());
            }
            self.handles.swap_remove(index);
        }
    }
}

/// Task diagnostics never retain or format a panic payload.
#[derive(Debug, thiserror::Error)]
#[error("authentication task failed")]
pub struct Task {
    id: tokio::task::Id,
    panicked: bool,
    cancelled: bool,
}

impl Task {
    pub fn id(&self) -> tokio::task::Id {
        self.id
    }

    pub fn is_panic(&self) -> bool {
        self.panicked
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

impl From<tokio::task::JoinError> for Task {
    fn from(error: tokio::task::JoinError) -> Self {
        Self {
            id: error.id(),
            panicked: error.is_panic(),
            cancelled: error.is_cancelled(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, task::Poll, time::Duration};

    use tokio::{runtime::Runtime, sync::oneshot, time::timeout};

    use crate::identity::authentication::{Error, registry::Registry, work::Work};

    #[test]
    fn cancelled_drain_preserves_collected_failures_and_unfinished_handles() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(3), async {
                let work = Work::default();
                let registry = Registry::new(2);
                let (release, released) = oneshot::channel();
                let remaining = tokio::spawn(async move {
                    released.await.expect("release remaining execution");
                });
                let failed = tokio::spawn(std::future::pending::<()>());
                failed.abort();
                while !failed.is_finished() {
                    tokio::task::yield_now().await;
                }
                work.tasks.lock().await.handles.extend([remaining, failed]);
                let mut drain = Box::pin(work.drain(&registry));
                std::future::poll_fn(|context| {
                    assert!(drain.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                }).await;
                drop(drain);
                let tasks = work.tasks.lock().await;
                assert_eq!(tasks.handles.len(), 1, "the failed task was already joined");
                assert_eq!(tasks.failures.len(), 1);
                drop(tasks);
                release.send(()).expect("remaining task is still owned");
                let error = work.drain(&registry).await.expect_err("failure retained");
                assert!(matches!(error, Error::Drain(ref failures)
                    if matches!(failures.as_slice(), [failure] if failure.is_cancelled() && !failure.is_panic())));
                work.drain(&registry).await.expect("failure consumed once");
                assert!(matches!(work.ready(&registry).await, Err(Error::Closed)));
            }).await.expect("drain budget");
        });
    }

    #[test]
    fn reaped_panic_closes_admission_and_survives_cancelled_drain_without_its_payload() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(3), async {
                let work = Work::default();
                let registry = Registry::new(2);
                let (release, released) = oneshot::channel();
                let remaining = tokio::spawn(async move {
                    released.await.expect("release remaining execution");
                });
                let failed = tokio::spawn(async { panic!("fictional-private-payload"); });
                let task_id = failed.id();
                while !failed.is_finished() {
                    tokio::task::yield_now().await;
                }
                work.tasks.lock().await.handles.extend([remaining, failed]);
                assert!(matches!(work.ready(&registry).await, Err(Error::Closed)));
                assert!(matches!(work.ready(&registry).await, Err(Error::Closed)));
                let mut drain = Box::pin(work.drain(&registry));
                std::future::poll_fn(|context| {
                    assert!(drain.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                }).await;
                drop(drain);
                release.send(()).expect("remaining task is still owned");
                let error = work.drain(&registry).await.expect_err("reaped panic retained");
                assert!(matches!(error, Error::Drain(ref failures)
                    if matches!(failures.as_slice(), [failure] if failure.id() == task_id && failure.is_panic() && !failure.is_cancelled())));
                assert!(!format!("{error:?}").contains("fictional-private-payload"));
                work.drain(&registry).await.expect("failure consumed once");
            }).await.expect("reaping budget");
        });
    }
}
