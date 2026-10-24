use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::{Mutex, watch};

use crate::identity::{authentication::Error, operation};

/// Request execution and retention share these identities and this fixed capacity.
pub(super) struct Registry {
    capacity: usize,
    closed: Arc<AtomicBool>,
    entries: Mutex<HashMap<operation::Id, watch::Receiver<()>>>,
}

impl Registry {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            closed: Arc::new(AtomicBool::new(false)),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(super) fn ready(&self) -> Result<(), Error> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        Ok(())
    }

    pub(super) async fn acquire(&self, operation_id: operation::Id) -> Result<Permit, Error> {
        loop {
            let mut entries = self.entries.lock().await;
            self.ready()?;
            entries.retain(|_, completion| completion.has_changed().is_ok());
            if let Some(completion) = entries.get(&operation_id) {
                let mut completion = completion.clone();
                drop(entries);
                let _ = completion.changed().await;
                continue;
            }
            return self.reserve(&mut entries, operation_id);
        }
    }

    pub(super) async fn try_acquire(&self, operation_id: operation::Id) -> Result<Permit, Error> {
        let mut entries = self.entries.lock().await;
        self.ready()?;
        entries.retain(|_, completion| completion.has_changed().is_ok());
        self.reserve(&mut entries, operation_id)
    }

    fn reserve(
        &self,
        entries: &mut HashMap<operation::Id, watch::Receiver<()>>,
        operation_id: operation::Id,
    ) -> Result<Permit, Error> {
        if entries.contains_key(&operation_id) || entries.len() >= self.capacity {
            return Err(Error::Busy);
        }
        let (completion, receiver) = watch::channel(());
        entries.insert(operation_id, receiver);
        Ok(Permit {
            closed: Arc::clone(&self.closed),
            _completion: completion,
            running: false,
        })
    }
}

/// Dropping a reservation is harmless; abandoning execution closes admission first.
pub(super) struct Permit {
    closed: Arc<AtomicBool>,
    _completion: watch::Sender<()>,
    running: bool,
}

impl Permit {
    pub(super) fn begin(&mut self) {
        self.running = true;
    }

    pub(super) fn finish(mut self) {
        self.running = false;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.running {
            self.closed.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, task::Poll, time::Duration};

    use tokio::{runtime::Runtime, time::timeout};

    use crate::identity::{
        authentication::{Error, registry::Registry},
        operation,
    };

    #[test]
    fn requests_wait_for_the_same_identity_while_retention_skips_occupied_capacity() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(3), async {
                let registry = Registry::new(1);
                let operation_id = "00000000-0000-0000-0000-000000000000"
                    .parse::<operation::Id>()
                    .expect("operation");
                let other_id = "ffffffff-ffff-ffff-ffff-ffffffffffff"
                    .parse::<operation::Id>()
                    .expect("other operation");
                let mut permit = registry
                    .acquire(operation_id)
                    .await
                    .expect("first reservation");
                permit.begin();
                assert!(matches!(
                    registry.try_acquire(operation_id).await,
                    Err(Error::Busy)
                ));
                assert!(matches!(
                    registry.try_acquire(other_id).await,
                    Err(Error::Busy)
                ));
                assert!(matches!(registry.acquire(other_id).await, Err(Error::Busy)));
                let mut repeated = Box::pin(registry.acquire(operation_id));
                std::future::poll_fn(|context| {
                    assert!(repeated.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
                permit.finish();
                let mut permit = repeated.await.expect("same identity released");
                permit.begin();
                permit.finish();
                registry
                    .try_acquire(other_id)
                    .await
                    .expect("shared capacity released");
                registry.close();
                assert!(matches!(
                    registry.acquire(operation_id).await,
                    Err(Error::Closed)
                ));
                assert!(matches!(
                    registry.try_acquire(other_id).await,
                    Err(Error::Closed)
                ));
            })
            .await
            .expect("registry budget");
        });
    }

    #[test]
    fn abandoned_reservation_is_harmless_but_abandoned_execution_closes_before_reuse() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(3), async {
                let registry = Registry::new(1);
                let operation_id = "00000000-0000-0000-0000-000000000000"
                    .parse::<operation::Id>()
                    .expect("operation");
                drop(registry.acquire(operation_id).await.expect("reservation"));
                registry.ready().expect("unstarted reservation is harmless");
                let mut permit = registry
                    .acquire(operation_id)
                    .await
                    .expect("replacement reservation");
                permit.begin();
                let mut repeated = Box::pin(registry.acquire(operation_id));
                std::future::poll_fn(|context| {
                    assert!(repeated.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(permit);
                assert!(matches!(repeated.await, Err(Error::Closed)));
                assert!(matches!(
                    registry.try_acquire(operation_id).await,
                    Err(Error::Closed)
                ));
            })
            .await
            .expect("abandonment budget");
        });
    }
}
