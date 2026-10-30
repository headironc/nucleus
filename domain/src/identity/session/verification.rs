//! Retained-source audit under the caller's startup barrier or original-operation lock.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::identity::{
    keys,
    session::{Error, Session, handoff},
};

pub(crate) async fn verify(
    client: &Client,
    session: &Session,
    keys: &keys::credential::Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let row = client.query_one("SELECT app_id, status, purpose, transport, finished_at FROM identity.authentications WHERE id = $1", &[&session.authentication_id()]).await?;
    let authenticated_at: DateTime<Utc> = row.try_get("finished_at")?;
    let status = row.try_get::<_, &str>("status")?;
    let source = match session.receipt_id() {
        None => status == "authenticated" && authenticated_at == session.created_at(),
        Some(_) => status == "registrable" && authenticated_at <= session.created_at(),
    };
    if !source
        || session.origin().app_id() != row.try_get::<_, &str>("app_id")?
        || row.try_get::<_, &str>("purpose")? != "create_session"
        || row.try_get::<_, &str>("transport")? != session.origin().transport()
    {
        return Err(Error::Facts {
            rule: "session preserves its authentication source and credential lifetime",
        });
    }
    session.validate_time(now)?;
    handoff::verify(client, session, keys).await
}
