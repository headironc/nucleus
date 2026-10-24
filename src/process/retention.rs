//! Own the single retention task and deliver its failure to the process supervisor.

use std::time::Duration;

use domain::identity::authentication;
use tokio::{sync::watch, task::JoinHandle, time::sleep};

pub(super) struct Retention {
    task: JoinHandle<Result<(), authentication::Error>>,
    stopped: watch::Sender<bool>,
    joined: bool,
}

impl Retention {
    pub(super) fn start(service: authentication::Service) -> Self {
        let (stopped, mut stopping) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut retention = service.retention();
            loop {
                if *stopping.borrow() {
                    return Ok(());
                }
                let result = retention.run().await;
                if matches!(result, Err(authentication::Error::Closed)) && *stopping.borrow() {
                    return Ok(());
                }
                result?;
                tokio::select! {
                    _ = stopping.changed() => return Ok(()),
                    _ = sleep(Duration::from_secs(60)) => {},
                }
            }
        });
        Self {
            task,
            stopped,
            joined: false,
        }
    }

    pub(super) fn close(&self) {
        self.stopped.send_replace(true);
    }

    pub(super) fn finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(super) async fn wait(&mut self) -> Result<(), Error> {
        if self.joined {
            return Ok(());
        }
        let result = (&mut self.task).await;
        self.joined = true;
        result??;
        Ok(())
    }

    pub(super) async fn drain(&mut self) -> Result<(), Error> {
        self.close();
        self.wait().await
    }
}

impl Drop for Retention {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Operation(#[from] authentication::Error),
    #[error("retention task failed")]
    Task {
        task_id: tokio::task::Id,
        panicked: bool,
        cancelled: bool,
    },
}

impl From<tokio::task::JoinError> for Error {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Task {
            task_id: error.id(),
            panicked: error.is_panic(),
            cancelled: error.is_cancelled(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, task::Poll, time::Duration};

    use domain::identity::authentication;
    use tokio::{
        runtime::Runtime,
        sync::{oneshot, watch},
        time::timeout,
    };

    use crate::process::retention::{Error, Retention};

    #[test]
    fn cancelled_wait_keeps_the_task_until_its_failure_is_delivered() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(3), async {
                let (release, released) = oneshot::channel();
                let mut retention = Retention {
                    task: tokio::spawn(async move {
                        released.await.expect("release task");
                        Err(authentication::Error::Closed)
                    }),
                    stopped: watch::channel(false).0,
                    joined: false,
                };
                let mut wait = Box::pin(retention.wait());
                std::future::poll_fn(|context| {
                    assert!(wait.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(wait);
                release.send(()).expect("cancelled waiter retains task");
                assert!(matches!(
                    retention.wait().await,
                    Err(Error::Operation(authentication::Error::Closed))
                ));
                retention
                    .wait()
                    .await
                    .expect("completed task is not rejoined");
                retention.drain().await.expect("failure reported once");
            })
            .await
            .expect("retention task budget");
        });
    }
}
