use crate::identity::message;

/// Settings owned by the authentication use cases.
pub struct Settings {
    message: message::Limits,
    session: crate::identity::authentication::session::settings::Settings,
}

impl Settings {
    pub fn new(
        message: message::Limits,
        session: crate::identity::authentication::session::settings::Settings,
    ) -> Self {
        Self { message, session }
    }

    pub(super) fn message(&self) -> &message::Limits {
        &self.message
    }

    pub(crate) fn session(&self) -> &crate::identity::authentication::session::settings::Settings {
        &self.session
    }
}
