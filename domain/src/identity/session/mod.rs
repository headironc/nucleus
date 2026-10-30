//! Opaque website SMS session identifiers and original token delivery.

pub mod settings;
pub use crate::identity::credential::ParseError;

pub(crate) mod handoff;
mod record;
pub(crate) mod state;
mod verification;
use std::str::FromStr;

use derive_more::Display;
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::Zeroizing;

pub(crate) use crate::identity::session::{
    record::{Deletion, Session, find_by_authentication, find_by_receipt},
    verification::verify,
};
use crate::{
    history,
    identity::{account, authentication, credential, keys, operation, registration},
    kernel::App,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Display, Serialize, Deserialize, FromSql, ToSql, ToSchema,
)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = session::Id)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid UUID text")]
pub struct ParseIdError(#[from] uuid::Error);

pub struct Token(pub(crate) credential::Secret);
impl FromStr for Token {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.parse()?))
    }
}
impl Token {
    /// Explicitly expose the original token only to the cookie delivery boundary.
    pub fn text(&self) -> Zeroizing<String> {
        self.0.text()
    }
}

pub(crate) enum Source {
    Authentication(authentication::Id),
    Registration {
        authentication_id: authentication::Id,
        receipt_id: registration::attempt::Id,
    },
}

pub(crate) struct Input {
    pub(crate) source: Source,
    pub(crate) account_id: account::Id,
    pub(crate) operation_id: operation::Id,
    pub(crate) created_at: chrono::DateTime<chrono::Utc>,
}

/// Issue both facts in the caller's transaction; the caller still owns commit and delivery.
pub(crate) async fn create(
    client: &tokio_postgres::Client,
    app: &App,
    keys: &keys::credential::Keys,
    settings: &settings::Settings,
    input: Input,
) -> Result<(Id, Token, chrono::DateTime<chrono::Utc>, history::Pending), Error> {
    let (authentication_id, receipt_id) = match input.source {
        Source::Authentication(id) => (id, None),
        Source::Registration {
            authentication_id,
            receipt_id,
        } => (authentication_id, Some(receipt_id)),
    };
    let id = Id::new();
    let token = credential::Secret::generate()?;
    let sealed = token.seal(keys, credential::Binding::Handoff(id))?;
    let expires_at = input
        .created_at
        .checked_add_signed(settings.lifetime())
        .ok_or(Error::Time)?;
    let handoff_expires_at = input
        .created_at
        .checked_add_signed(settings.recovery())
        .ok_or(Error::Time)?;
    let change = state::create(
        state::Input {
            id,
            account_id: input.account_id,
            authentication_id,
            receipt_id,
            hash: token.hash(),
            app_id: settings.app_id().clone(),
            expires_at,
        },
        input.operation_id,
        input.created_at,
    )?;
    // No fallible write follows registration of the pending anchor in this helper.
    client
        .execute(
            "INSERT INTO identity.handoffs (id, token, expires_at) VALUES ($1, $2, $3)",
            &[&id, &sealed.as_ref(), &handoff_expires_at],
        )
        .await?;
    let pending = change.write(client, app.keys(), app.anchors()).await?;
    Ok((id, Token(token), expires_at, pending))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    History(#[from] crate::history::Error),
    #[error("session query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("session decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("{0}")]
    Credential(#[from] credential::Error),
    #[error("session credential generation failed")]
    Random(#[from] getrandom::Error),
    #[error("session and handoff lifetimes must form valid timestamps")]
    Time,
    #[error("session fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("original session handoff is unavailable")]
    Unavailable,
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(crate::database::Query(error))
    }
}
