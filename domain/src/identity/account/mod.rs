//! Account identity preserved across lookup, locking and mobile envelope binding.

pub(crate) mod record;
pub(crate) mod state;
pub(crate) mod transition;

use std::{error, str::FromStr, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
#[cfg(any(test, feature = "test-support"))]
pub use state::rekey_for_test;
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    history::{self, Change, Draft, Read, Verified, View},
    identity::{
        avatar, keys,
        mobile::{
            Mobile,
            envelope::{Binding, Envelope},
        },
        operation,
        person::{self, name::Name, number::IdNumber, revision},
        suspension, wechat,
    },
    kernel::App,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, FromSql, ToSql, ToSchema,
)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = account::Id)]
pub struct Id(Uuid);

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Id {
    pub(crate) fn allocate() -> Self {
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

/// The authenticated identity fields an institutional manager may confirm.
pub struct Subject {
    id: Id,
    person_id: person::Id,
    revision_id: revision::Id,
    name: String,
    suffix: String,
    suspension_id: Option<suspension::Id>,
}

impl Subject {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn revision_id(&self) -> revision::Id {
        self.revision_id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn suffix(&self) -> &str {
        &self.suffix
    }
    pub(crate) fn suspension_id(&self) -> Option<suspension::Id> {
        self.suspension_id
    }
}

#[derive(Clone)]
pub(crate) struct Reader {
    app: App,
    keys: Arc<keys::Keys>,
    read: Read,
}

impl Reader {
    pub(in crate::identity) fn history_keys(&self) -> history::Keys<'_> {
        self.app.keys()
    }
    pub(in crate::identity) fn anchors(&self) -> &history::Anchors {
        self.app.anchors()
    }
    pub(crate) fn new(app: App, keys: Arc<keys::Keys>) -> Self {
        Self {
            app,
            keys,
            read: Read::Locked,
        }
    }

    pub(crate) fn at(&self, view: View) -> Self {
        self.with_read(Read::Snapshot(view))
    }

    pub(crate) fn with_read(&self, read: Read) -> Self {
        Self {
            app: self.app.clone(),
            keys: Arc::clone(&self.keys),
            read,
        }
    }

    pub(crate) async fn state(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Verified<state::State>>, ReadError> {
        Ok(history::find(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read,
            id,
            now,
        )
        .await?)
    }

    pub(crate) async fn find_by_person(
        &self,
        client: &tokio_postgres::Client,
        person_id: person::Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Id>, ReadError> {
        Ok(self
            .person_state(client, person_id, now)
            .await?
            .filter(|state| state.current().is_some())
            .map(|state| state.id()))
    }

    pub(crate) async fn person_state(
        &self,
        client: &tokio_postgres::Client,
        person_id: person::Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Verified<state::State>>, ReadError> {
        Ok(state::located(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read,
            state::Location::Person(person_id),
            now,
        )
        .await?)
    }

    pub(crate) async fn find_by_mobile(
        &self,
        client: &tokio_postgres::Client,
        mobile: &Mobile,
        now: DateTime<Utc>,
    ) -> Result<Option<Id>, ReadError> {
        let hash = self.keys.mobile.hash(mobile)?;
        Ok(state::located(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read,
            state::Location::Mobile(&hash),
            now,
        )
        .await?
        .map(|state| state.id()))
    }

    pub(crate) async fn number_occupied(
        &self,
        client: &tokio_postgres::Client,
        number: &IdNumber,
        person: person::Id,
        now: DateTime<Utc>,
    ) -> Result<bool, ReadError> {
        let hash = self.keys.number.hash(number)?;
        Ok(state::located(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read,
            state::Location::Number(&hash),
            now,
        )
        .await?
        .is_some_and(|state| state.person_id() != person))
    }

    /// Access first decides whether this target is disclosable in the same snapshot.
    pub(crate) async fn find_by_id(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        mobile: &Mobile,
        now: DateTime<Utc>,
    ) -> Result<Option<Subject>, ReadError> {
        self.read(client, id, Some(mobile), now).await
    }

    pub(crate) async fn find_current(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        now: DateTime<Utc>,
    ) -> Result<Option<Subject>, ReadError> {
        self.read(client, id, None, now).await
    }

    async fn read(
        &self,
        client: &tokio_postgres::Client,
        id: Id,
        mobile: Option<&Mobile>,
        now: DateTime<Utc>,
    ) -> Result<Option<Subject>, ReadError> {
        let Some(state) = self.state(client, id, now).await? else {
            return Ok(None);
        };
        let Some(current) = state.current() else {
            return Ok(None);
        };
        let stored = current.mobile().open(
            &self.keys.encryption,
            &self.keys.mobile,
            Binding::Account(id),
        )?;
        if mobile.is_some_and(|mobile| stored.as_ref() != mobile.as_ref()) {
            return Ok(None);
        }
        let person = person::state::find(
            client,
            self.app.keys(),
            self.app.anchors(),
            self.read,
            state.person_id(),
            now,
        )
        .await?
        .ok_or(ReadError::Facts)?;
        let revision_id = person.revision_id();
        let profile = revision::fetch(client, self.app.keys(), revision_id).await?;
        if profile.person_id() != state.person_id()
            || current.number().as_ref() != self.keys.number.hash(profile.id_number())?.as_ref()
        {
            return Err(ReadError::Facts);
        }
        Ok(Some(Subject {
            id,
            person_id: state.person_id(),
            revision_id,
            name: profile.name().masked(),
            suffix: profile.id_number().suffix().to_owned(),
            suspension_id: current.suspension_id(),
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{0}")]
    History(#[from] history::Error),
    #[error("account confirmation query failed")]
    Query(#[from] crate::database::Query),
    #[error("{0}")]
    Revision(#[from] revision::Error),
    #[error("{0}")]
    Mobile(#[from] crate::identity::mobile::envelope::Error),
    #[error(
        "account identity is missing required facts or does not match its authenticated revision"
    )]
    Facts,
    #[error("account confirmation digest computation failed")]
    Crypto(#[from] platform::cryptography::Error),
}

impl From<tokio_postgres::Error> for ReadError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl ReadError {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::History(error) => error.corrupt(),
            Self::Revision(error) => error.unreliable(),
            Self::Mobile(_) | Self::Facts => true,
            Self::Query(query) => {
                let mut option = error::Error::source(query);
                while let Some(cause) = option {
                    if cause.is::<platform::cryptography::envelope::ParseError>() {
                        return true;
                    }
                    option = cause.source();
                }
                false
            }
            Self::Crypto(_) => false,
        }
    }
}

/// Preallocated identities are a construction plan, not evidence of a committed account.
#[derive(Clone, Copy)]
pub(crate) struct Creation {
    pub(crate) person_id: person::Id,
    pub(crate) revision_id: revision::Id,
    pub(crate) account_id: Id,
    pub(crate) avatar_id: avatar::Id,
    pub(crate) avatar: avatar::Revision,
    pub(crate) wechat: wechat::Revision,
}

impl Creation {
    pub(crate) fn allocate() -> Self {
        Self {
            person_id: person::Id::allocate(),
            revision_id: revision::Id::allocate(),
            account_id: Id::allocate(),
            avatar_id: avatar::Id::allocate(),
            avatar: avatar::Revision::allocate(),
            wechat: wechat::Revision::allocate(),
        }
    }

    pub(crate) fn prepare(
        &self,
        input: Input<'_>,
        keys: Keys<'_>,
        source: record::Source,
        operation_id: operation::Id,
        now: DateTime<Utc>,
    ) -> Result<Prepared, Error> {
        let revision = revision::Prepared::new(
            keys.encryption,
            self.revision_id,
            self.person_id,
            input.name,
            input.id_number,
        )?;
        let mobile = Envelope::seal(
            keys.encryption,
            keys.mobile,
            Binding::Account(self.account_id),
            input.mobile,
        )?;
        let number = keys.number.hash(input.id_number)?;
        let defaults = avatar::Defaults::verify()?;
        let avatar = defaults.select(input.mobile);
        Ok(Prepared {
            identity: *self,
            revision,
            person: person::transition::create(
                self.person_id,
                self.revision_id,
                self.account_id,
                operation_id,
                now,
            ),
            digest: *avatar.digest(),
            size: avatar.size(),
            account: Change::new(
                Draft::create(state::Data {
                    id: self.account_id,
                    person_id: self.person_id,
                    status: state::Status::Current(state::Current {
                        plaintext: state::open_mobile(&mobile, keys.encryption, self.account_id)?,
                        mobile: Arc::new(mobile),
                        number: Arc::new(number),
                        suspension_id: None,
                    }),
                    avatar: self.avatar,
                    wechat: self.wechat,
                    created_at: now,
                    changed_at: now,
                }),
                record::Data {
                    person_id: self.person_id,
                    operation_id,
                    happened_at: now,
                    event: record::Event::Creation {
                        revision_id: self.revision_id,
                        source,
                    },
                },
            ),
        })
    }
}

pub(crate) struct Prepared {
    identity: Creation,
    revision: revision::Prepared,
    person: Change<person::state::State>,
    digest: [u8; 32],
    size: i64,
    account: Change<state::State>,
}

impl Prepared {
    /// Write the Person record and state before its revision and account dependencies.
    async fn dependencies(
        self,
        client: &tokio_postgres::Client,
        keys: history::Keys<'_>,
        anchors: &history::Anchors,
        pending: &mut Option<history::Pending>,
    ) -> Result<Change<state::State>, Error> {
        history::retain(pending, self.person.write(client, keys, anchors).await?)?;
        self.revision.write(client, keys).await?;
        client
            .execute(
                "INSERT INTO identity.avatars (id, account_id, digest, size, width, height) VALUES ($1, $2, $3, $4, 512, 512)",
                &[&self.identity.avatar_id, &self.identity.account_id, &&self.digest[..], &self.size],
            )
            .await?;
        Ok(self.account)
    }

    pub(crate) async fn write(
        self,
        client: &tokio_postgres::Client,
        keys: history::Keys<'_>,
        anchors: &history::Anchors,
        pending: &mut Option<history::Pending>,
    ) -> Result<(), Error> {
        let account = self.dependencies(client, keys, anchors, pending).await?;
        history::retain(pending, account.write(client, keys, anchors).await?)?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn write_offline(
        self,
        client: &tokio_postgres::Client,
        keys: history::Keys<'_>,
    ) -> Result<(), Error> {
        self.person.write_offline(client, keys).await?;
        self.revision.write(client, keys).await?;
        client
            .execute(
                "INSERT INTO identity.avatars (id, account_id, digest, size, width, height) VALUES ($1, $2, $3, $4, 512, 512)",
                &[&self.identity.avatar_id, &self.identity.account_id, &&self.digest[..], &self.size],
            )
            .await?;
        let account = self.account;
        account.write_offline(client, keys).await?;
        Ok(())
    }
}

pub(crate) struct Input<'a> {
    pub(crate) mobile: &'a Mobile,
    pub(crate) name: &'a Name,
    pub(crate) id_number: &'a IdNumber,
}

pub(crate) struct Keys<'a> {
    pub(crate) mobile: &'a keys::mobile::Keys,
    pub(crate) encryption: &'a keys::encryption::Keys,
    pub(crate) number: &'a keys::number::Keys,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("account construction database operation failed: {0}")]
    Database(#[from] crate::database::Query),
    #[error("{0}")]
    Revision(#[source] revision::Error),
    #[error("{0}")]
    Mobile(#[from] crate::identity::mobile::envelope::Error),
    #[error("{0}")]
    Avatar(#[from] avatar::Error),
    #[error("account lookup computation failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("{0}")]
    History(#[source] history::Error),
}

impl From<revision::Error> for Error {
    fn from(error: revision::Error) -> Self {
        match error {
            revision::Error::History(error) => error.into(),
            error => Self::Revision(error),
        }
    }
}

impl From<history::Error> for Error {
    fn from(error: history::Error) -> Self {
        match error {
            history::Error::Query(query) => Self::Database(query),
            error => Self::History(error),
        }
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(crate::database::Query(error))
    }
}
