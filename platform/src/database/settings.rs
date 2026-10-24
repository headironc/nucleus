//! Validated database connection inputs; loading remains with the root application.

use std::{
    alloc::Layout,
    fmt,
    time::{Duration, Instant},
};

use rustls::pki_types::{InvalidDnsNameError, ServerName};
use thiserror::Error;
use tokio::sync::Semaphore;

use crate::database::{Manager, User, user};

/// The connection stage whose budget is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Wait,
    Connect,
    Recycle,
}

impl fmt::Display for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Wait => "wait",
            Self::Connect => "connect",
            Self::Recycle => "recycle",
        })
    }
}

pub const DEFAULT_WAIT_SECONDS: u64 = 3;
pub const DEFAULT_CONNECT_SECONDS: u64 = 5;
pub const DEFAULT_RECYCLE_SECONDS: u64 = 3;

/// Per-stage limits, independent of business deadlines and query timeouts.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    wait: Duration,
    connect: Duration,
    recycle: Duration,
}

impl Budgets {
    pub(super) fn wait(&self) -> Duration {
        self.wait
    }
    pub(super) fn connect(&self) -> Duration {
        self.connect
    }
    pub(super) fn recycle(&self) -> Duration {
        self.recycle
    }

    /// Reject zero budgets and durations that cannot form a timer deadline.
    pub fn new(wait: Duration, connect: Duration, recycle: Duration) -> Result<Self, Error> {
        for (stage, duration) in [
            (Stage::Wait, wait),
            (Stage::Connect, connect),
            (Stage::Recycle, recycle),
        ] {
            if duration.is_zero() || Instant::now().checked_add(duration).is_none() {
                return Err(Error::Budget(stage));
            }
        }
        Ok(Self {
            wait,
            connect,
            recycle,
        })
    }
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            wait: Duration::from_secs(DEFAULT_WAIT_SECONDS),
            connect: Duration::from_secs(DEFAULT_CONNECT_SECONDS),
            recycle: Duration::from_secs(DEFAULT_RECYCLE_SECONDS),
        }
    }
}

/// Non-secret connection settings. Only TCP hosts are accepted.
pub struct Settings {
    host: String,
    port: u16,
    name: String,
    user: User,
    capacity: usize,
    budgets: Budgets,
}

impl Settings {
    pub(super) fn host(&self) -> &str {
        &self.host
    }
    pub(super) fn port(&self) -> u16 {
        self.port
    }
    pub(super) fn name(&self) -> &str {
        &self.name
    }
    pub(super) fn user(&self) -> &User {
        &self.user
    }
    pub(super) fn capacity(&self) -> usize {
        self.capacity
    }
    pub(super) fn budgets(&self) -> Budgets {
        self.budgets
    }

    /// Validate before building a connector. At least two online sessions are required.
    pub fn new(
        host: String,
        port: u16,
        name: String,
        user: String,
        capacity: usize,
        budgets: Budgets,
    ) -> Result<Self, Error> {
        ServerName::try_from(host.as_str())?;
        if port == 0 {
            return Err(Error::Port);
        }
        if name.is_empty()
            || name.len() > 63
            || name
                .chars()
                .any(|value| value.is_whitespace() || value.is_control())
        {
            return Err(Error::Name);
        }
        let user = User::new(user)?;
        if capacity < 2 {
            return Err(Error::Capacity);
        }
        // Deadpool eagerly allocates its slots. Its public object wrapper contains
        // the private slot, so its layout provides a conservative overflow bound.
        // This is a representability check, not a guarantee of available memory.
        if capacity > Semaphore::MAX_PERMITS
            || Layout::array::<deadpool::managed::Object<Manager>>(capacity).is_err()
        {
            return Err(Error::CapacityLimit);
        }
        Ok(Self {
            host,
            port,
            name,
            user,
            capacity,
            budgets,
        })
    }
}

/// Invalid settings contain no submitted values.
#[derive(Debug, Error)]
pub enum Error {
    #[error("database host must be a valid TLS server name or IP address")]
    Host(#[from] InvalidDnsNameError),
    #[error("database port must be nonzero")]
    Port,
    #[error("database name must contain 1 to 63 bytes without whitespace or controls")]
    Name,
    #[error("{0}")]
    User(#[from] user::Error),
    #[error("database pool capacity must be at least 2")]
    Capacity,
    #[error("database pool capacity exceeds the semaphore or allocation layout limit")]
    CapacityLimit,
    #[error("database {0} budget must be nonzero and fit a timer deadline")]
    Budget(Stage),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use crate::database::settings::{Budgets, Error, Settings, Stage};

    #[test]
    fn rejects_invalid_connection_inputs() {
        for host in [
            "",
            " host",
            "host\n",
            "/tmp",
            "host/name",
            "https://example.com",
            "host:5432",
            "[::1]",
            "%65xample.com",
            "例子.测试",
            "-host",
            "host..example",
        ] {
            assert!(matches!(
                Settings::new(
                    host.into(),
                    5432,
                    "test".into(),
                    "fixture".into(),
                    2,
                    Budgets::default()
                ),
                Err(Error::Host(_))
            ));
        }
        for name in [
            "".to_owned(),
            "a".repeat(64),
            "test db".into(),
            "test\0".into(),
        ] {
            assert!(matches!(
                Settings::new(
                    "localhost".into(),
                    5432,
                    name,
                    "fixture".into(),
                    2,
                    Budgets::default()
                ),
                Err(Error::Name)
            ));
        }
        assert!(matches!(
            Settings::new(
                "localhost".into(),
                0,
                "test".into(),
                "fixture".into(),
                2,
                Budgets::default()
            ),
            Err(Error::Port)
        ));
        for capacity in [0, 1] {
            assert!(matches!(
                Settings::new(
                    "localhost".into(),
                    5432,
                    "test".into(),
                    "fixture".into(),
                    capacity,
                    Budgets::default()
                ),
                Err(Error::Capacity)
            ));
        }
        for (host, capacity) in [
            ("127.0.0.1", 2),
            ("localhost", 65),
            ("::1", 2),
            ("Db.Example.com", 2),
            ("xn--fsqu00a.xn--0zwm56d", 2),
        ] {
            let settings = Settings::new(
                host.into(),
                65535,
                "a".repeat(63),
                "fixture".into(),
                capacity,
                Budgets::default(),
            )
            .expect("valid boundary");
            assert_eq!(settings.host(), host);
        }
    }

    #[test]
    fn validates_user_without_changing_the_identity() {
        for user in ["", "invalid\0user"] {
            assert!(matches!(
                Settings::new(
                    "localhost".into(),
                    5432,
                    "test".into(),
                    user.into(),
                    2,
                    Budgets::default()
                ),
                Err(Error::User(_))
            ));
        }
        for user in ["fixture", "Case Sensitive User"] {
            let settings = Settings::new(
                "localhost".into(),
                5432,
                "test".into(),
                user.into(),
                2,
                Budgets::default(),
            )
            .expect("valid login identity");
            assert_eq!(settings.user().as_ref(), user);
        }
    }

    #[test]
    fn rejects_unrepresentable_capacity_before_pool_allocation() {
        for capacity in [usize::MAX, tokio::sync::Semaphore::MAX_PERMITS] {
            assert!(matches!(
                Settings::new(
                    "localhost".into(),
                    5432,
                    "test".into(),
                    "fixture".into(),
                    capacity,
                    Budgets::default()
                ),
                Err(Error::CapacityLimit)
            ));
        }
    }

    #[test]
    fn bounds_each_budget_independently() {
        for invalid in [Duration::ZERO, Duration::MAX] {
            let valid = Duration::from_secs(1);
            assert!(matches!(
                Budgets::new(invalid, valid, valid),
                Err(Error::Budget(Stage::Wait))
            ));
            assert!(matches!(
                Budgets::new(valid, invalid, valid),
                Err(Error::Budget(Stage::Connect))
            ));
            assert!(matches!(
                Budgets::new(valid, valid, invalid),
                Err(Error::Budget(Stage::Recycle))
            ));
        }
        Budgets::new(
            Duration::from_nanos(1),
            Duration::from_secs(61),
            Duration::from_secs(1),
        )
        .expect("valid budgets");
    }
}
