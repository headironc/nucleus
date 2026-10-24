use chrono::TimeDelta;
use thiserror::Error;

/// Durable permit limits; loading environment keys belongs to the entrypoint.
pub struct Limits {
    lifetime: TimeDelta,
    daily: usize,
    mobile: Mobile,
    source: Source,
}

impl Limits {
    pub fn new(
        lifetime_seconds: u64,
        daily: usize,
        mobile: Mobile,
        source: Source,
    ) -> Result<Self, Error> {
        if !(120..=600).contains(&lifetime_seconds) || !(1..=100).contains(&daily) {
            return Err(Error);
        }
        let lifetime = TimeDelta::seconds(i64::try_from(lifetime_seconds).map_err(Error::from)?);
        Ok(Self {
            lifetime,
            daily,
            mobile,
            source,
        })
    }

    pub fn lifetime(&self) -> TimeDelta {
        self.lifetime
    }

    pub fn daily(&self) -> usize {
        self.daily
    }

    pub(in crate::identity) fn mobile(&self) -> &Mobile {
        &self.mobile
    }

    pub(in crate::identity) fn source(&self) -> &Source {
        &self.source
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            lifetime: TimeDelta::seconds(300),
            daily: 100,
            mobile: Mobile::default(),
            source: Source::default(),
        }
    }
}

pub struct Mobile {
    interval: TimeDelta,
    hourly: usize,
    daily: usize,
}

impl Mobile {
    pub fn new(interval_seconds: u64, hourly: usize, daily: usize) -> Result<Self, Error> {
        if !(60..=600).contains(&interval_seconds)
            || !(1..=5).contains(&hourly)
            || !(1..=10).contains(&daily)
            || hourly > daily
        {
            return Err(Error);
        }
        Ok(Self {
            interval: TimeDelta::seconds(i64::try_from(interval_seconds).map_err(Error::from)?),
            hourly,
            daily,
        })
    }

    pub fn interval(&self) -> TimeDelta {
        self.interval
    }

    pub fn hourly(&self) -> usize {
        self.hourly
    }

    pub fn daily(&self) -> usize {
        self.daily
    }
}

impl Default for Mobile {
    fn default() -> Self {
        Self {
            interval: TimeDelta::seconds(60),
            hourly: 5,
            daily: 10,
        }
    }
}

pub struct Source {
    minute: usize,
    hourly: usize,
}

impl Source {
    pub fn new(minute: usize, hourly: usize) -> Result<Self, Error> {
        if !(1..=3).contains(&minute) || !(1..=10).contains(&hourly) {
            return Err(Error);
        }
        Ok(Self { minute, hourly })
    }

    pub fn minute(&self) -> usize {
        self.minute
    }

    pub fn hourly(&self) -> usize {
        self.hourly
    }
}

impl Default for Source {
    fn default() -> Self {
        Self {
            minute: 3,
            hourly: 10,
        }
    }
}

#[derive(Debug, Error)]
#[error("invalid message permit limits")]
pub struct Error;

impl From<std::num::TryFromIntError> for Error {
    fn from(_: std::num::TryFromIntError) -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_only_documented_bounds_and_consistent_mobile_limits() {
        for lifetime in [0, 119, 601, u64::MAX] {
            assert!(Limits::new(lifetime, 100, Mobile::default(), Source::default()).is_err());
        }
        for daily in [0, 101] {
            assert!(Limits::new(300, daily, Mobile::default(), Source::default()).is_err());
        }
        for lifetime in [120, 300, 600] {
            assert!(Limits::new(lifetime, 1, Mobile::default(), Source::default()).is_ok());
        }
        for (interval, hourly, daily) in [
            (59, 1, 1),
            (601, 1, 1),
            (60, 0, 1),
            (60, 6, 10),
            (60, 1, 0),
            (60, 1, 11),
            (60, 2, 1),
        ] {
            assert!(Mobile::new(interval, hourly, daily).is_err());
        }
        for (minute, hourly) in [(0, 1), (4, 1), (1, 0), (1, 11)] {
            assert!(Source::new(minute, hourly).is_err());
        }
        assert!(Mobile::new(600, 5, 10).is_ok());
        assert!(Source::new(3, 10).is_ok());
    }
}
