use chrono::TimeDelta;

pub use crate::identity::session::settings::{DEFAULT_RECOVERY_SECONDS, DEFAULT_SESSION_SECONDS};
use crate::identity::website::AppId;
pub const DEFAULT_REGISTRATION_SECONDS: u64 = 600;

#[derive(Clone)]
pub struct Settings {
    issuance: crate::identity::session::settings::Settings,
    registration: TimeDelta,
}
impl Settings {
    pub fn new(
        app_id: AppId,
        session_seconds: u64,
        registration_seconds: u64,
        recovery_seconds: u64,
    ) -> Result<Self, Error> {
        if !(60..=600).contains(&registration_seconds) {
            return Err(Error);
        }
        Ok(Self {
            issuance: crate::identity::session::settings::Settings::new(
                app_id,
                session_seconds,
                recovery_seconds,
            )
            .map_err(invalid_issuance)?,
            registration: TimeDelta::seconds(i64::try_from(registration_seconds).map_err(invalid)?),
        })
    }
    pub(crate) fn app_id(&self) -> &AppId {
        self.issuance.app_id()
    }
    pub(crate) fn issuance(&self) -> &crate::identity::session::settings::Settings {
        &self.issuance
    }
    pub(super) fn registration(&self) -> TimeDelta {
        self.registration
    }
}
fn invalid_issuance(_: crate::identity::session::settings::Error) -> Error {
    Error
}
fn invalid(_: std::num::TryFromIntError) -> Error {
    Error
}
#[derive(Debug, thiserror::Error)]
#[error("invalid SMS session, registration or recovery lifetime")]
pub struct Error;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::Settings;
    #[test]
    fn validates_current_consumers_and_exact_lifetime_bounds() {
        for (session, registration, recovery, accepted) in [
            (3600, 60, 60, true),
            (86400, 600, 600, true),
            (3599, 60, 60, false),
            (86401, 60, 60, false),
            (3600, 59, 60, false),
            (3600, 601, 60, false),
            (3600, 60, 59, false),
            (3600, 60, 601, false),
            (u64::MAX, 600, 600, false),
        ] {
            assert_eq!(
                Settings::new(
                    "website".parse().expect("app"),
                    session,
                    registration,
                    recovery
                )
                .is_ok(),
                accepted
            );
        }
    }
}
