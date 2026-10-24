//! C1 ingress budgets are independent of durable message permits.

use std::{
    collections::HashMap,
    net::IpAddr,
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use governor::{
    Quota, RateLimiter,
    clock::{Clock, DefaultClock},
    middleware::NoOpMiddleware,
    state::{InMemoryState, NotKeyed},
};
use thiserror::Error;

type Limiter<C> = RateLimiter<NotKeyed, InMemoryState, C, NoOpMiddleware<<C as Clock>::Instant>>;

const SOURCE_CAPACITY: usize = 10_000;
// Even an empty source bucket is full twenty seconds after its last debit.
const SOURCE_RECOVERY: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub(crate) struct Admission(Arc<Mutex<Budget<DefaultClock>>>);

impl Admission {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self(Arc::new(Mutex::new(Budget::new(
            DefaultClock::default(),
        )?))))
    }

    pub(crate) fn accept(&self, source: IpAddr) -> Result<(), Error> {
        self.0.lock().map_err(poisoned)?.accept(source)
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> Error {
    Error::State
}

struct Budget<C: Clock> {
    sources: HashMap<IpAddr, Source<C>>,
    reclaim_at: C::Instant,
    total: Limiter<C>,
    quota: Quota,
}

struct Source<C: Clock> {
    limiter: Limiter<C>,
    expires_at: C::Instant,
}

impl<C: Clock + Clone> Budget<C> {
    fn new(clock: C) -> Result<Self, Error> {
        Ok(Self {
            sources: HashMap::new(),
            reclaim_at: clock.now(),
            total: RateLimiter::direct_with_clock(
                Quota::per_second(nonzero(20)?).allow_burst(nonzero(40)?),
                clock,
            ),
            quota: Quota::per_minute(nonzero(6)?).allow_burst(nonzero(2)?),
        })
    }

    fn accept(&mut self, address: IpAddr) -> Result<(), Error> {
        if !self.sources.contains_key(&address) {
            if self.sources.len() >= SOURCE_CAPACITY {
                self.reclaim();
            }
            if self.sources.len() >= SOURCE_CAPACITY {
                return Err(Error::Capacity);
            }
            self.sources.insert(
                address,
                Source {
                    limiter: RateLimiter::direct_with_clock(self.quota, self.total.clock().clone()),
                    expires_at: self.total.clock().now(),
                },
            );
        }
        let source = self.sources.get_mut(&address).ok_or(Error::State)?;
        if let Err(denied) = source.limiter.check() {
            return Err(Error::Limited(
                denied.wait_time_from(source.limiter.clock().now()),
            ));
        }
        // Sample after the debit so expiry can never precede full replenishment.
        source.expires_at = source.limiter.clock().now() + SOURCE_RECOVERY.into();
        if let Err(denied) = self.total.check() {
            // A process rejection does not refund the source debit.
            return Err(Error::Limited(
                denied.wait_time_from(self.total.clock().now()),
            ));
        }
        Ok(())
    }

    fn reclaim(&mut self) {
        let now = self.total.clock().now();
        if now < self.reclaim_at {
            return;
        }
        // Repeated capacity refusals cannot reclaim anything before this bound.
        let mut reclaim_at = now + SOURCE_RECOVERY.into();
        self.sources.retain(|_, source| {
            if source.expires_at <= now {
                return false;
            }
            reclaim_at = reclaim_at.min(source.expires_at);
            true
        });
        self.reclaim_at = reclaim_at;
    }
}

fn nonzero(value: u32) -> Result<NonZeroU32, Error> {
    NonZeroU32::new(value).ok_or(Error::State)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP ingress budget exhausted")]
    Limited(Duration),
    #[error("HTTP source capacity exhausted")]
    Capacity,
    #[error("HTTP ingress budget state is unavailable")]
    State,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Duration,
    };

    use governor::clock::FakeRelativeClock;

    use crate::identity::admission::{Admission, Budget, Error};

    #[test]
    fn source_rejection_preserves_global_quota_and_retained_sources_refill() {
        let clock = FakeRelativeClock::default();
        let address = "192.0.2.1".parse().expect("IP");
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        budget.accept(address).expect("first");
        budget.accept(address).expect("burst");
        assert!(
            matches!(budget.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_secs(10))
        );
        for octet in 2..40 {
            budget
                .accept(IpAddr::V4(Ipv4Addr::new(192, 0, 2, octet)))
                .expect("remaining global burst");
        }
        assert!(
            matches!(budget.accept("198.51.100.1".parse().expect("IP")), Err(Error::Limited(wait)) if wait == Duration::from_millis(50))
        );
        clock.advance(Duration::from_millis(9999));
        assert!(
            matches!(budget.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_millis(1))
        );
        clock.advance(Duration::from_millis(1));
        budget.accept(address).expect("source refill boundary");
        clock.advance(Duration::from_secs(19));
        budget
            .accept("198.51.100.2".parse().expect("IP"))
            .expect("another source");
        assert!(budget.sources.contains_key(&address));
        clock.advance(Duration::from_secs(1));
        budget
            .accept("198.51.100.3".parse().expect("IP"))
            .expect("recovery boundary");
        assert!(budget.sources.contains_key(&address));
        budget.accept(address).expect("retained source refill");
        budget.accept(address).expect("retained source burst");
        assert!(
            matches!(budget.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_secs(10))
        );
    }

    #[test]
    fn global_rejection_keeps_source_debits() {
        let clock = FakeRelativeClock::default();
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        for octet in 0..40 {
            budget
                .accept(IpAddr::V4(Ipv4Addr::new(192, 0, 2, octet)))
                .expect("global burst");
        }
        let address = "198.51.100.1".parse().expect("IP");
        for _ in 0..2 {
            assert!(
                matches!(budget.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_millis(50))
            );
        }
        clock.advance(Duration::from_millis(50));
        assert!(
            matches!(budget.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_millis(9950))
        );
        budget
            .accept("198.51.100.2".parse().expect("IP"))
            .expect("global refill remains available");
    }

    #[test]
    fn capacity_rejects_new_sources_without_evicting_existing_debits() {
        let clock = FakeRelativeClock::default();
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        for value in 0_u32..20_000 {
            let result = budget.accept(IpAddr::V4(Ipv4Addr::from(value)));
            match value {
                0..40 => assert!(result.is_ok()),
                40..10_000 => assert!(matches!(result, Err(Error::Limited(_)))),
                _ => assert!(matches!(result, Err(Error::Capacity))),
            }
        }
        assert_eq!(budget.sources.len(), 10_000);
        clock.advance(Duration::from_secs(1));
        let existing = IpAddr::V4(Ipv4Addr::from(0));
        budget
            .accept(existing)
            .expect("existing source still has one token");
        clock.advance(Duration::from_millis(18999));
        assert!(matches!(
            budget.accept("192.0.2.1".parse().expect("IP")),
            Err(Error::Capacity)
        ));
        clock.advance(Duration::from_millis(1));
        budget
            .accept("192.0.2.1".parse().expect("IP"))
            .expect("reclaim before allocating");
        assert_eq!(budget.sources.len(), 2);
        assert!(budget.sources.contains_key(&existing));
    }

    #[test]
    fn reclamation_preserves_recent_debits_and_rejections_do_not_delay_it() {
        let clock = FakeRelativeClock::default();
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        for value in 0_u32..10_000 {
            let result = budget.accept(IpAddr::V4(Ipv4Addr::from(value)));
            assert!(result.is_ok() || matches!(result, Err(Error::Limited(_))));
        }
        let rejected = IpAddr::V4(Ipv4Addr::from(1));
        assert!(matches!(budget.accept(rejected), Err(Error::Limited(_))));
        clock.advance(Duration::from_secs(9));
        assert!(
            matches!(budget.accept(rejected), Err(Error::Limited(wait)) if wait == Duration::from_secs(1))
        );
        clock.advance(Duration::from_secs(10));
        let active = IpAddr::V4(Ipv4Addr::from(0));
        budget.accept(active).expect("active source refill");
        budget.accept(active).expect("active source burst");
        let newcomer = "192.0.2.1".parse().expect("IP");
        assert!(matches!(budget.accept(newcomer), Err(Error::Capacity)));
        clock.advance(Duration::from_secs(1));
        budget.accept(newcomer).expect("reclaimed capacity");
        assert_eq!(budget.sources.len(), 2);
        assert!(!budget.sources.contains_key(&rejected));
        assert!(
            matches!(budget.accept(active), Err(Error::Limited(wait)) if wait == Duration::from_secs(9))
        );
    }

    #[test]
    fn concurrent_requests_share_one_source_burst() {
        let admission = Admission::new().expect("fixed quotas");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let admission = admission.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    admission.accept("192.0.2.1".parse().expect("IP")).is_ok()
                })
            })
            .collect();
        let accepted = tasks
            .into_iter()
            .map(|task| usize::from(task.join().expect("request thread")))
            .sum::<usize>();
        assert_eq!(accepted, 2);
    }
}
