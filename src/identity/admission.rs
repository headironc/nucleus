//! Identity source groups share one process budget independently of durable permits.

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

#[cfg(not(test))]
type AdmissionClock = DefaultClock;

#[cfg(test)]
#[derive(Clone)]
enum AdmissionClock {
    Real(DefaultClock),
    Frozen(<DefaultClock as Clock>::Instant),
}

#[cfg(test)]
impl Default for AdmissionClock {
    fn default() -> Self {
        Self::Real(DefaultClock::default())
    }
}

#[cfg(test)]
impl Clock for AdmissionClock {
    type Instant = <DefaultClock as Clock>::Instant;

    fn now(&self) -> Self::Instant {
        match self {
            Self::Real(clock) => clock.now(),
            Self::Frozen(now) => *now,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Admission(
    Arc<Mutex<Budget<AdmissionClock>>>,
    Arc<tokio::sync::Semaphore>,
);

impl Admission {
    pub(crate) fn new() -> Result<Self, Error> {
        Self::with_clock(AdmissionClock::default())
    }

    #[cfg(test)]
    pub(crate) fn with_frozen_clock() -> Result<Self, Error> {
        Self::with_clock(AdmissionClock::Frozen(DefaultClock::default().now()))
    }

    fn with_clock(clock: AdmissionClock) -> Result<Self, Error> {
        Ok(Self(
            Arc::new(Mutex::new(Budget::new(clock)?)),
            Arc::new(tokio::sync::Semaphore::new(8)),
        ))
    }

    pub(crate) fn accept(&self, source: IpAddr) -> Result<(), Error> {
        self.0.lock().map_err(poisoned)?.accept(source)
    }

    pub(crate) fn create_session(&self, source: IpAddr) -> Result<(), Error> {
        self.0.lock().map_err(poisoned)?.accept_session(source)
    }
    pub(crate) fn read(&self, source: IpAddr) -> Result<(), Error> {
        self.0.lock().map_err(poisoned)?.metadata.accept(source)
    }
    pub(crate) fn write(&self, source: IpAddr) -> Result<(), Error> {
        self.0.lock().map_err(poisoned)?.writes.accept(source)
    }
    pub(crate) fn image(&self, source: IpAddr) -> Result<(), Error> {
        self.0.lock().map_err(poisoned)?.images.accept(source)
    }
    pub(crate) fn transfer(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Error> {
        Ok(Arc::clone(&self.1).try_acquire_owned()?)
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> Error {
    Error::State
}

struct Budget<C: Clock> {
    challenges: Sources<C>,
    sessions: Sources<C>,
    total: Limiter<C>,
    metadata: Group<C>,
    images: Group<C>,
    writes: Group<C>,
}

struct Group<C: Clock> {
    sources: Sources<C>,
    total: Limiter<C>,
}

impl<C: Clock + Clone> Group<C> {
    fn new(clock: &C, quota: Quota, recovery: Duration, total: Quota) -> Self {
        Self {
            sources: Sources::new(clock, quota, recovery),
            total: RateLimiter::direct_with_clock(total, clock.clone()),
        }
    }
    fn accept(&mut self, source: IpAddr) -> Result<(), Error> {
        self.sources.accept(source, self.total.clock())?;
        if let Err(denied) = self.total.check() {
            return Err(Error::Limited(
                denied.wait_time_from(self.total.clock().now()),
            ));
        }
        Ok(())
    }
}

struct Sources<C: Clock> {
    entries: HashMap<IpAddr, Source<C>>,
    reclaim_at: C::Instant,
    quota: Quota,
    recovery: Duration,
}

struct Source<C: Clock> {
    limiter: Limiter<C>,
    expires_at: C::Instant,
}

impl<C: Clock + Clone> Budget<C> {
    fn new(clock: C) -> Result<Self, Error> {
        Ok(Self {
            writes: Group::new(
                &clock,
                Quota::per_second(nonzero(2)?).allow_burst(nonzero(4)?),
                Duration::from_secs(2),
                Quota::per_second(nonzero(10)?).allow_burst(nonzero(20)?),
            ),
            metadata: Group::new(
                &clock,
                Quota::per_second(nonzero(20)?).allow_burst(nonzero(40)?),
                Duration::from_secs(2),
                Quota::per_second(nonzero(100)?).allow_burst(nonzero(100)?),
            ),
            images: Group::new(
                &clock,
                Quota::per_minute(nonzero(120)?).allow_burst(nonzero(20)?),
                Duration::from_secs(10),
                Quota::per_second(nonzero(100)?).allow_burst(nonzero(100)?),
            ),
            challenges: Sources::new(
                &clock,
                Quota::per_minute(nonzero(6)?).allow_burst(nonzero(2)?),
                SOURCE_RECOVERY,
            ),
            sessions: Sources::new(
                &clock,
                Quota::per_minute(nonzero(60)?).allow_burst(nonzero(10)?),
                Duration::from_secs(10),
            ),
            total: RateLimiter::direct_with_clock(
                Quota::per_second(nonzero(20)?).allow_burst(nonzero(40)?),
                clock,
            ),
        })
    }

    fn accept(&mut self, address: IpAddr) -> Result<(), Error> {
        self.challenges.accept(address, self.total.clock())?;
        self.debit_total()
    }

    fn accept_session(&mut self, address: IpAddr) -> Result<(), Error> {
        self.sessions.accept(address, self.total.clock())?;
        self.debit_total()
    }

    fn debit_total(&self) -> Result<(), Error> {
        if let Err(denied) = self.total.check() {
            // A process rejection does not refund the source debit.
            return Err(Error::Limited(
                denied.wait_time_from(self.total.clock().now()),
            ));
        }
        Ok(())
    }
}

impl<C: Clock + Clone> Sources<C> {
    fn new(clock: &C, quota: Quota, recovery: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            reclaim_at: clock.now(),
            quota,
            recovery,
        }
    }

    fn accept(&mut self, address: IpAddr, clock: &C) -> Result<(), Error> {
        if !self.entries.contains_key(&address) {
            if self.entries.len() >= SOURCE_CAPACITY {
                self.reclaim(clock);
            }
            if self.entries.len() >= SOURCE_CAPACITY {
                return Err(Error::Capacity);
            }
            self.entries.insert(
                address,
                Source {
                    limiter: RateLimiter::direct_with_clock(self.quota, clock.clone()),
                    expires_at: clock.now(),
                },
            );
        }
        let source = self.entries.get_mut(&address).ok_or(Error::State)?;
        if let Err(denied) = source.limiter.check() {
            return Err(Error::Limited(
                denied.wait_time_from(source.limiter.clock().now()),
            ));
        }
        // Sample after the debit so expiry can never precede full replenishment.
        source.expires_at = source.limiter.clock().now() + self.recovery.into();
        Ok(())
    }

    fn reclaim(&mut self, clock: &C) {
        let now = clock.now();
        if now < self.reclaim_at {
            return;
        }
        // Repeated capacity refusals cannot reclaim anything before this bound.
        let mut reclaim_at = now + self.recovery.into();
        self.entries.retain(|_, source| {
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
    #[error("HTTP image transfer permit is unavailable")]
    Transfer(#[from] tokio::sync::TryAcquireError),
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
        assert!(budget.challenges.entries.contains_key(&address));
        clock.advance(Duration::from_secs(1));
        budget
            .accept("198.51.100.3".parse().expect("IP"))
            .expect("recovery boundary");
        assert!(budget.challenges.entries.contains_key(&address));
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
        assert_eq!(budget.challenges.entries.len(), 10_000);
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
        assert_eq!(budget.challenges.entries.len(), 2);
        assert!(budget.challenges.entries.contains_key(&existing));
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
        assert_eq!(budget.challenges.entries.len(), 2);
        assert!(!budget.challenges.entries.contains_key(&rejected));
        assert!(
            matches!(budget.accept(active), Err(Error::Limited(wait)) if wait == Duration::from_secs(9))
        );
    }

    #[test]
    fn session_sources_are_independent_and_debit_the_shared_process_budget() {
        let clock = FakeRelativeClock::default();
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        let address = "192.0.2.1".parse().expect("IP");
        budget.accept(address).expect("challenge first");
        budget.accept(address).expect("challenge burst");
        for _ in 0..10 {
            budget
                .accept_session(address)
                .expect("separate session burst");
        }
        assert!(
            matches!(budget.accept_session(address), Err(Error::Limited(wait)) if wait == Duration::from_secs(1))
        );
        for octet in 2..30 {
            budget
                .accept_session(IpAddr::V4(Ipv4Addr::new(192, 0, 2, octet)))
                .expect("remaining total burst");
        }
        assert!(
            matches!(budget.accept("198.51.100.1".parse().expect("IP")), Err(Error::Limited(wait)) if wait == Duration::from_millis(50))
        );
        clock.advance(Duration::from_secs(1));
        budget
            .accept_session(address)
            .expect("session one-second refill");
        assert!(
            matches!(budget.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_secs(9))
        );
    }

    #[test]
    fn metadata_and_images_have_independent_source_and_total_budgets() {
        let clock = FakeRelativeClock::default();
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        let address = "192.0.2.1".parse().expect("IP");
        for _ in 0..40 {
            budget.metadata.accept(address).expect("metadata burst");
        }
        assert!(
            matches!(budget.metadata.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_millis(50))
        );
        for _ in 0..20 {
            budget
                .images
                .accept(address)
                .expect("independent image burst");
        }
        assert!(
            matches!(budget.images.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_millis(500))
        );
        for value in 1..=80 {
            let source = IpAddr::V4(Ipv4Addr::from(value));
            budget
                .images
                .accept(source)
                .expect("remaining image process burst");
            if value <= 60 {
                budget
                    .metadata
                    .accept(source)
                    .expect("remaining metadata process burst");
            }
        }
        let other = "198.51.100.1".parse().expect("IP");
        assert!(
            matches!(budget.images.accept(other), Err(Error::Limited(wait)) if wait == Duration::from_millis(10))
        );
        assert!(
            matches!(budget.metadata.accept(other), Err(Error::Limited(wait)) if wait == Duration::from_millis(10))
        );
        budget.accept(address).expect("reading does not debit C1");
        budget
            .accept_session(address)
            .expect("reading does not debit R1/R2");
        clock.advance(Duration::from_millis(50));
        budget.metadata.accept(address).expect("metadata refill");
        assert!(
            matches!(budget.images.accept(address), Err(Error::Limited(wait)) if wait == Duration::from_millis(450))
        );
    }

    #[test]
    fn authenticated_writes_have_independent_source_and_process_limits() {
        let clock = FakeRelativeClock::default();
        let mut budget = Budget::new(clock.clone()).expect("fixed quotas");
        let source = "192.0.2.1".parse().expect("source");
        for _ in 0..4 {
            budget.writes.accept(source).expect("write burst");
        }
        assert!(
            matches!(budget.writes.accept(source), Err(Error::Limited(wait)) if wait == Duration::from_millis(500))
        );
        for value in 1..=16 {
            budget
                .writes
                .accept(IpAddr::V4(Ipv4Addr::from(value)))
                .expect("remaining process burst");
        }
        assert!(
            matches!(budget.writes.accept("192.0.2.2".parse().expect("other source")), Err(Error::Limited(wait)) if wait == Duration::from_millis(100))
        );
        budget
            .metadata
            .accept(source)
            .expect("metadata is independent");
        budget
            .images
            .accept(source)
            .expect("images are independent");
        budget.accept(source).expect("C1 is independent");
        budget
            .accept_session(source)
            .expect("R1/R2 are independent");
        clock.advance(Duration::from_millis(499));
        assert!(
            matches!(budget.writes.accept(source), Err(Error::Limited(wait)) if wait == Duration::from_millis(1))
        );
        clock.advance(Duration::from_millis(1));
        budget.writes.accept(source).expect("exact source refill");
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
