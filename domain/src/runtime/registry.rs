use std::collections::HashMap;

use platform::database::{self, Connections, Lease};
use tokio::sync::{Mutex, watch};

use crate::{identity::operation, runtime::work::Error};

/// Request execution and retention share these identities and this fixed capacity.
pub(crate) struct Registry {
    capacity: usize,
    closed: watch::Sender<bool>,
    entries: Mutex<HashMap<(Family, operation::Id), watch::Receiver<()>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Family {
    Challenge,
    CreateSession,
    Register,
    Transfer,
    /// Families on the use-case executor share its vocabulary.
    Kernel(crate::kernel::operation::Family),
}

impl Registry {
    /// Closure is shared with the runtime context, whose instance session can also close it.
    pub(crate) fn new(capacity: usize, closed: watch::Sender<bool>) -> Self {
        Self {
            capacity,
            closed,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn close(&self) {
        self.closed.send_replace(true);
    }

    pub(crate) fn ready(&self) -> Result<(), Error> {
        if *self.closed.borrow() {
            return Err(Error::Closed);
        }
        Ok(())
    }

    /// Observe closure even when it preceded subscription or the first poll.
    pub(crate) async fn closed(&self) {
        let mut closed = self.closed.subscribe();
        let _ = closed.wait_for(|closed| *closed).await;
    }

    /// Discovery can stop waiting for a lease; admitted work must finish independently.
    pub(crate) async fn lease_until_closed<E>(&self, connections: &Connections) -> Result<Lease, E>
    where
        E: From<Error> + From<database::Error>,
    {
        self.ready()?;
        let lease = tokio::select! {
            biased;
            _ = self.closed() => return Err(Error::Closed.into()),
            lease = connections.lease() => lease?,
        };
        if let Err(error) = self.ready() {
            lease.release();
            return Err(error.into());
        }
        Ok(lease)
    }

    #[cfg(test)]
    pub(crate) async fn acquire(&self, operation_id: operation::Id) -> Result<Permit, Error> {
        self.acquire_for(Family::Challenge, operation_id).await
    }

    pub(crate) async fn acquire_for(
        &self,
        family: Family,
        operation_id: operation::Id,
    ) -> Result<Permit, Error> {
        let identity = (family, operation_id);
        loop {
            let mut entries = self.entries.lock().await;
            self.ready()?;
            entries.retain(|_, completion| completion.has_changed().is_ok());
            if let Some(completion) = entries.get(&identity) {
                let mut completion = completion.clone();
                drop(entries);
                let _ = completion.changed().await;
                continue;
            }
            return self.reserve(&mut entries, identity);
        }
    }

    /// Observing an original result neither reserves work nor consumes write capacity.
    pub(crate) async fn wait_for(
        &self,
        family: Family,
        operation_id: operation::Id,
    ) -> Result<(), Error> {
        loop {
            let entries = self.entries.lock().await;
            self.ready()?;
            let Some(completion) = entries.get(&(family, operation_id)) else {
                return Ok(());
            };
            if completion.has_changed().is_err() {
                return Ok(());
            }
            let mut completion = completion.clone();
            drop(entries);
            let _ = completion.changed().await;
        }
    }

    pub(crate) async fn try_acquire(&self, operation_id: operation::Id) -> Result<Permit, Error> {
        self.try_acquire_for(Family::Challenge, operation_id).await
    }

    pub(crate) async fn try_acquire_for(
        &self,
        family: Family,
        operation_id: operation::Id,
    ) -> Result<Permit, Error> {
        let mut entries = self.entries.lock().await;
        self.ready()?;
        entries.retain(|_, completion| completion.has_changed().is_ok());
        self.reserve(&mut entries, (family, operation_id))
    }

    fn reserve(
        &self,
        entries: &mut HashMap<(Family, operation::Id), watch::Receiver<()>>,
        operation_id: (Family, operation::Id),
    ) -> Result<Permit, Error> {
        if entries.contains_key(&operation_id) || entries.len() >= self.capacity {
            return Err(Error::Busy);
        }
        let (completion, receiver) = watch::channel(());
        entries.insert(operation_id, receiver);
        Ok(Permit {
            closed: self.closed.clone(),
            completion,
            running: false,
        })
    }
}

/// Dropping a reservation is harmless; abandoning execution closes admission first.
pub(crate) struct Permit {
    closed: watch::Sender<bool>,
    completion: watch::Sender<()>,
    running: bool,
}

impl Permit {
    pub(crate) fn completion(&self) -> watch::Receiver<()> {
        self.completion.subscribe()
    }

    pub(crate) fn begin(&mut self) {
        self.running = true;
    }

    pub(crate) fn finish(mut self) {
        self.running = false;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.running {
            self.closed.send_replace(true);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::Future, task::Poll, time::Duration};

    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        identity::operation,
        runtime::{registry::Registry, work::Error},
    };

    #[test]
    fn closure_wakes_all_waiters_and_remains_observable() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(1), async {
                let registry = Registry::new(1, tokio::sync::watch::channel(false).0);
                let operation_id = "00000000-0000-0000-0000-000000000000"
                    .parse::<operation::Id>()
                    .expect("operation");
                let mut permit = registry.acquire(operation_id).await.expect("reservation");
                permit.begin();
                let completion = permit.completion();
                let mut first = Box::pin(registry.closed());
                let mut second = Box::pin(registry.closed());
                let unpolled = registry.closed();
                std::future::poll_fn(|context| {
                    assert!(first.as_mut().poll(context).is_pending());
                    assert!(second.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
                registry.close();
                first.await;
                second.await;
                unpolled.await;
                registry.closed().await;
                registry.close();
                assert!(matches!(registry.ready(), Err(Error::Closed)));
                assert!(
                    completion.has_changed().is_ok(),
                    "admitted work is still running"
                );
                permit.finish();
                assert!(
                    completion.has_changed().is_err(),
                    "only finishing ends admitted work"
                );

                let registry = Registry::new(1, tokio::sync::watch::channel(false).0);
                registry.close();
                registry.closed().await;
            })
            .await
            .expect("closure notification budget");
        });
    }

    #[test]
    fn requests_wait_for_the_same_identity_while_retention_skips_occupied_capacity() {
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(3), async {
                let registry = Registry::new(1, tokio::sync::watch::channel(false).0);
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
                let registry = Registry::new(1, tokio::sync::watch::channel(false).0);
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
                let mut closed = Box::pin(registry.closed());
                std::future::poll_fn(|context| {
                    assert!(repeated.as_mut().poll(context).is_pending());
                    assert!(closed.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(permit);
                closed.await;
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
