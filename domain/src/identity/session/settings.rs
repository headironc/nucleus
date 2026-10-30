use chrono::TimeDelta;

use crate::identity::{mini, session::Origin, website::AppId};

pub const DEFAULT_SESSION_SECONDS: u64 = 86400;
pub const DEFAULT_RECOVERY_SECONDS: u64 = 600;
pub const DEFAULT_MOBILE_SECONDS: u64 = 2_592_000;

#[derive(Clone)]
pub struct Settings {
    app_id: AppId,
    lifetime: TimeDelta,
    recovery: TimeDelta,
    mini: Option<(mini::AppId, TimeDelta)>,
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
            mini: None,
        })
    }

    pub(crate) fn app_id(&self) -> &AppId {
        &self.app_id
    }
    pub fn with_mini(mut self, app_id: mini::AppId, seconds: u64) -> Result<Self, Error> {
        if app_id.as_ref() == self.app_id.as_ref()
            || !(3600..=DEFAULT_MOBILE_SECONDS).contains(&seconds)
        {
            return Err(Error);
        }
        self.mini = Some((
            app_id,
            TimeDelta::seconds(i64::try_from(seconds).map_err(invalid)?),
        ));
        Ok(self)
    }

    pub(crate) fn accepts(&self, origin: &Origin) -> bool {
        match origin {
            Origin::WebsiteMessage(id) => id == &self.app_id,
            Origin::MiniMobile(id) => self.mini.as_ref().is_some_and(|(fixed, _)| fixed == id),
        }
    }

    pub(crate) fn mini_app_id(&self) -> Option<&mini::AppId> {
        self.mini.as_ref().map(|(app_id, _)| app_id)
    }

    pub(crate) fn lifetime_for(&self, origin: &Origin) -> Result<TimeDelta, Error> {
        match origin {
            Origin::WebsiteMessage(id) if id == &self.app_id => Ok(self.lifetime),
            Origin::MiniMobile(id) => self
                .mini
                .as_ref()
                .filter(|(fixed, _)| fixed == id)
                .map(|(_, lifetime)| *lifetime)
                .ok_or(Error),
            _ => Err(Error),
        }
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
