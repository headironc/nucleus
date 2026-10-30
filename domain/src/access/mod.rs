//! Roles, institutional appointments, domain admission, and initialization.

pub mod appoint;
pub mod appointment;
pub mod assignment;
mod capability;
pub(crate) use capability::Scope;
mod entry;
pub mod grant;
pub(crate) mod holder;
pub mod initialize;
pub mod role;
pub mod verification;

pub use crate::access::entry::Entry;
#[cfg(any(test, feature = "test-support"))]
pub use crate::access::initialize::Keys;

pub(crate) async fn authorize(
    client: &tokio_postgres::Client,
    roles: &role::Reader,
    current: &crate::identity::current::Service,
    actor: &crate::identity::current::Actor,
    code: role::Code,
) -> Result<
    (
        role::Role,
        crate::identity::person::Id,
        crate::identity::person::revision::Id,
        chrono::DateTime<chrono::Utc>,
    ),
    role::Error,
> {
    let snapshot = role::database::find_by_actor(client, roles, current, actor).await?;
    let now = snapshot.state.observed_at();
    let (role, person_id, revision_id) = snapshot.authorize(roles, now, code)?;
    Ok((role, person_id, revision_id, now))
}

/// Capability authorization for a session the command executor already qualified.
pub(crate) async fn authorize_subject(
    client: &tokio_postgres::Client,
    roles: &role::Reader,
    identity: crate::identity::current::database::State,
    code: role::Code,
) -> Result<
    (
        role::Role,
        crate::identity::person::Id,
        crate::identity::person::revision::Id,
        chrono::DateTime<chrono::Utc>,
    ),
    role::Error,
> {
    let snapshot = role::database::find_by_identity(client, roles, identity).await?;
    let now = snapshot.state.observed_at();
    let (role, person_id, revision_id) = snapshot.authorize(roles, now, code)?;
    Ok((role, person_id, revision_id, now))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Revision(#[from] crate::identity::person::revision::Error),
    #[error("capability catalog does not match this application")]
    Catalog,
    #[error("persisted access facts are inconsistent: {rule}")]
    Facts { rule: &'static str },
    #[error("{0}")]
    Timeout(#[from] crate::database::verification::Timeout),
    #[error("{0}")]
    History(#[from] crate::history::Error),
    #[error("access verification database operation failed: {0}")]
    Database(#[from] crate::database::Query),
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(crate::database::Query(error))
    }
}
