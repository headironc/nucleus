use chrono::TimeDelta;

use crate::identity::website::AppId;

pub const DEFAULT_SESSION_SECONDS: u64 = 86400;
pub const DEFAULT_RECOVERY_SECONDS: u64 = 600;

#[derive(Clone)]
pub struct Settings {
    app_id: AppId,
    lifetime: TimeDelta,
    recovery: TimeDelta,
}

impl Settings {
    pub fn new(app_id: AppId, session_seconds: u64, recovery_seconds: u64) -> Result<Self, Error> {
        if !(3600..=86400).contains(&session_seconds)
            || !(60..=600).contains(&recovery_seconds)
            || recovery_seconds > session_seconds
        {
            return Err(Error);
        }
        Ok(Self {
            app_id,
            lifetime: TimeDelta::seconds(i64::try_from(session_seconds).map_err(invalid)?),
            recovery: TimeDelta::seconds(i64::try_from(recovery_seconds).map_err(invalid)?),
        })
    }

    pub(crate) fn app_id(&self) -> &AppId {
        &self.app_id
    }
    pub(crate) fn lifetime(&self) -> TimeDelta {
        self.lifetime
    }
    pub(crate) fn recovery(&self) -> TimeDelta {
        self.recovery
    }
}

fn invalid(_: std::num::TryFromIntError) -> Error {
    Error
}

#[derive(Debug, thiserror::Error)]
#[error("invalid website SMS session or handoff lifetime")]
pub struct Error;
