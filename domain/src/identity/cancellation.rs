//! Permanent cancellation identity and removal of account-owned access material.

use std::{convert::Infallible, error};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    database::verification,
    history::{self, Read, Record as _, Verified},
    identity::{
        account, keys,
        mobile::Mobile,
        operation,
        person::{
            self,
            record::{self, Event as RecordEvent},
            revision,
            state::State,
        },
        session,
    },
    kernel::{self, App, Authorization, completion, operation::Family, tx::Tx},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = cancellation::Id)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = cancellation::Cancellation)]
pub struct Cancellation {
    id: Id,
    #[schema(value_type = String, format = DateTime)]
    cancelled_at: DateTime<Utc>,
}

impl Cancellation {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn cancelled_at(&self) -> DateTime<Utc> {
        self.cancelled_at
    }
}

pub(crate) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::Cancellation, |action, target| {
        matches!(action, "self" | "managed") && target.parse::<account::Id>().is_ok()
    });

/// Both entrypoints and permanent-fact verification use this exact canonical input.
pub(crate) fn request<R>(
    account_id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
    mobile: Option<&Mobile>,
    authority: &Authorization,
) -> Result<completion::Request, kernel::Error<R>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Confirmed<'a> {
        account_id: account::Id,
        person_id: person::Id,
        revision_id: revision::Id,
        #[serde(skip_serializing_if = "Option::is_none")]
        mobile: Option<&'a str>,
    }
    completion::Request::new(
        if mobile.is_some() { "managed" } else { "self" },
        account_id.to_string(),
        &Confirmed {
            account_id,
            person_id,
            revision_id,
            mobile: mobile.map(AsRef::as_ref),
        },
        authority,
    )
}

pub(crate) async fn absent<R>(tx: &Tx, app: &App) -> Result<(), kernel::Error<R>> {
    let now = tx.now(app.context()).await?;
    if record::find_cancellation(tx.client(), app.keys(), tx.operation_id(), now)
        .await?
        .is_some()
    {
        return Err(kernel::Error::corrupt(
            "a retained cancellation cannot lack its completion",
        ));
    }
    Ok(())
}

/// Internal recovery holds the original-operation lock but has not taken any object locks.
pub(crate) async fn lock_replay<R>(tx: &Tx, target: account::Id) -> Result<(), kernel::Error<R>> {
    tx.shared(b"access/scope", b"institution").await?;
    tx.shared(b"access/scope", b"workforce").await?;
    account::state::lock(tx.client(), target).await?;
    account::state::lock_person(tx.client(), target).await?;
    crate::access::holder::lock(tx.client(), target).await?;
    session::state::lock_account(tx.client(), target).await?;
    if let Some(person) = account::state::discover_owner(tx.client(), target).await? {
        crate::access::grant::state::lock_history(tx.client(), person).await?;
    }
    Ok(())
}

pub(crate) struct New<'a> {
    pub(crate) account_id: account::Id,
    pub(crate) person_id: person::Id,
    pub(crate) revision_id: revision::Id,
    pub(crate) operator_id: revision::Id,
    pub(crate) mobile: &'a Mobile,
    pub(crate) operation_id: operation::Id,
    pub(crate) cancelled_at: DateTime<Utc>,
}

pub(crate) async fn insert(
    client: &Client,
    keys: history::Keys<'_>,
    anchors: &history::Anchors,
    state: &Verified<State>,
    pending: &mut Option<history::Pending>,
    new: New<'_>,
) -> Result<Cancellation, Error> {
    if state.id() != new.person_id || state.revision_id() != new.revision_id {
        return Err(Error::Facts {
            rule: "cancellation retains the confirmed person and revision",
        });
    }
    let id = Id::allocate();
    let change = person::transition::cancel(
        state,
        new.revision_id,
        id,
        new.account_id,
        new.operator_id,
        new.mobile,
        new.operation_id,
        new.cancelled_at,
    )?;
    history::retain(pending, change.write(client, keys, anchors).await?)?;
    Ok(Cancellation {
        id,
        cancelled_at: new.cancelled_at,
    })
}

/// Permanent Identity history survives the removal of every current access credential.
pub(crate) async fn remove(
    client: &Client,
    app: &App,
    original: &Verified<account::state::State>,
    id: Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
    pending: &mut Option<history::Pending>,
) -> Result<(), Error> {
    let account_id = original.id();
    let sessions = session::state::for_account(
        client,
        app.keys(),
        app.anchors(),
        Read::Locked,
        account_id,
        now,
    )
    .await?;
    for session in sessions {
        let candidate = history::Draft::remove(&session, operation_id)
            .write(client, app.anchors())
            .await?;
        history::retain(pending, candidate)?;
    }
    client
        .execute(
            "DELETE FROM identity.receipts WHERE account_id = $1",
            &[&account_id],
        )
        .await?;
    if client.execute("UPDATE identity.avatars SET released_at = $2 WHERE account_id = $1 AND released_at IS NULL", &[&account_id, &now]).await? != 1 {
        return Err(Error::Facts { rule: "cancellation releases exactly one current avatar" });
    }
    let candidate = account::transition::cancel(original, id, operation_id, now)?
        .write(client, app.keys(), app.anchors())
        .await?;
    history::retain(pending, candidate)?;
    Ok(())
}

pub(crate) struct Original {
    cancellation: Cancellation,
    account_id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
    operator_id: person::Id,
    operator_revision_id: revision::Id,
    mobile: Mobile,
    operation_id: operation::Id,
}

impl Original {
    pub(crate) fn id(&self) -> Id {
        self.cancellation.id()
    }

    pub(crate) fn matches_current(
        &self,
        account_id: account::Id,
        person_id: person::Id,
        revision_id: revision::Id,
    ) -> bool {
        self.account_id == account_id
            && self.person_id == person_id
            && self.revision_id == revision_id
            && self.operator_id == person_id
    }
    pub(crate) fn matches_account(
        &self,
        account_id: account::Id,
        person_id: person::Id,
        revision_id: revision::Id,
        operator_id: person::Id,
        mobile: &Mobile,
    ) -> bool {
        self.account_id == account_id
            && self.person_id == person_id
            && self.revision_id == revision_id
            && self.operator_id == operator_id
            && self.operator_id != self.person_id
            && self.mobile.as_bytes() == mobile.as_bytes()
    }
    pub(crate) fn into_cancellation(self) -> Cancellation {
        self.cancellation
    }

    fn check_account(&self, state: &Verified<account::state::State>) -> Result<(), Error> {
        if state.id() != self.account_id
            || state.person_id() != self.person_id
            || state.cancellation_id() != Some(self.id())
            || state.changed_at() != self.cancellation.cancelled_at
        {
            return Err(Error::Facts {
                rule: "cancellation retains its original account tombstone",
            });
        }
        Ok(())
    }

    async fn verify_source(
        &self,
        client: &Client,
        keys: history::Keys<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        let removed: bool = client.query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM identity.avatars WHERE account_id=$1 AND released_at IS NULL) AND NOT EXISTS (SELECT 1 FROM identity.receipts WHERE account_id=$1)",
            &[&self.account_id],
        ).await?.try_get(0)?;
        if !removed {
            return Err(Error::Facts {
                rule: "cancellation releases its avatar and removes all retained receipts",
            });
        }
        let record =
            completion::original::<Infallible>(client, keys, COMPLETIONS, self.operation_id)
                .await?
                .ok_or(Error::Facts {
                    rule: "a cancellation retains its permanent completion",
                })?;
        record.check_time::<Infallible>(now)?;
        if record.revision_id() != self.operator_revision_id
            || record.created_at() != self.cancellation.cancelled_at
        {
            return Err(Error::Facts {
                rule: "cancellation and completion retain the original operator revision and time",
            });
        }
        let authority = Authorization {
            person_id: self.operator_id,
            revision_id: self.operator_revision_id,
            now: self.cancellation.cancelled_at,
        };
        let request = request::<Infallible>(
            self.account_id,
            self.person_id,
            self.revision_id,
            (self.operator_id != self.person_id).then_some(&self.mobile),
            &authority,
        )?;
        match record.matches(COMPLETIONS, keys, &request) {
            Ok(()) => {}
            Err(kernel::Error::Mismatch) => {
                return Err(Error::Facts {
                    rule: "cancellation completion binds its action, target, operator and canonical input",
                });
            }
            Err(error) => return Err(Error::Completion(error)),
        }
        let record = account::record::cancellation(client, keys, self.id(), now).await?;
        if record.account_id() != self.account_id
            || record.person_id() != self.person_id
            || record.operation_id() != self.operation_id
            || record.happened_at() != self.cancellation.cancelled_at
        {
            return Err(Error::Facts {
                rule: "cancellation and its sealed account projection have the same origin",
            });
        }
        Ok(())
    }

    pub(crate) async fn verify_removal(
        &self,
        client: &Client,
        app: &App,
        read: Read,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        crate::access::grant::confirm_cancellation(
            client,
            app,
            self.person_id,
            self.id(),
            self.operation_id,
            read,
            now,
        )
        .await?;
        let state = history::find::<account::state::State>(
            client,
            app.keys(),
            app.anchors(),
            read,
            self.account_id,
            now,
        )
        .await?
        .ok_or(Error::Facts {
            rule: "cancellation retains its permanent account",
        })?;
        self.check_account(&state)?;
        if !session::state::for_account(
            client,
            app.keys(),
            app.anchors(),
            read,
            self.account_id,
            now,
        )
        .await?
        .is_empty()
        {
            return Err(Error::Facts {
                rule: "a cancelled account retains no sessions",
            });
        }
        Ok(())
    }
}

impl Original {
    fn from_record(
        record: &Verified<record::Record>,
        operator: revision::Profile,
    ) -> Result<Self, Error> {
        let RecordEvent::Cancellation {
            id,
            account_id,
            revision_id,
            operator_id,
            mobile,
        } = record.event()
        else {
            return Err(Error::Facts {
                rule: "a cancellation loader selects cancellation records",
            });
        };
        if operator.person_id() == record.person_id() && *operator_id != *revision_id {
            return Err(Error::Facts {
                rule: "self cancellation retains the confirmed revision as operator",
            });
        }
        Ok(Self {
            cancellation: Cancellation {
                id: *id,
                cancelled_at: record.happened_at(),
            },
            account_id: *account_id,
            person_id: record.person_id(),
            revision_id: *revision_id,
            operator_id: operator.person_id(),
            operator_revision_id: *operator_id,
            mobile: mobile.as_ref().clone(),
            operation_id: record.operation_id(),
        })
    }
}

pub(crate) async fn find_by_operation(
    client: &Client,
    keys: history::Keys<'_>,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Original>, Error> {
    let Some(record) = record::find_cancellation(client, keys, operation_id, now).await? else {
        return Ok(None);
    };
    let (revision_id, operator_id) = match record.event() {
        RecordEvent::Cancellation {
            revision_id,
            operator_id,
            ..
        } => (*revision_id, *operator_id),
        _ => {
            return Err(Error::Facts {
                rule: "a cancellation loader selects cancellation records",
            });
        }
    };
    let profiles = revision::fetch_many(client, keys, &[revision_id, operator_id]).await?;
    let subject = profiles.get(&revision_id).ok_or(revision::Error::Missing)?;
    if subject.person_id() != record.person_id() {
        return Err(revision::Error::Person.into());
    }
    let operator = profiles
        .get(&operator_id)
        .ok_or(revision::Error::Missing)?
        .clone();
    let original = Original::from_record(&record, operator)?;
    original.verify_source(client, keys, now).await?;
    Ok(Some(original))
}

pub(crate) async fn verify(
    client: &Client,
    keys: history::Keys<'_>,
    accounts: &[Verified<account::state::State>],
    sessions: &[Verified<session::state::State>],
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let mut operations = completion::fetch_all::<Infallible>(client, keys, COMPLETIONS)
        .await?
        .into_iter()
        .map(|record| record.operation_id())
        .collect::<std::collections::HashSet<_>>();
    let accounts = accounts
        .iter()
        .map(|state| (state.id(), state))
        .collect::<std::collections::HashMap<_, _>>();
    let sessions = sessions
        .iter()
        .map(|state| state.account_id())
        .collect::<std::collections::HashSet<_>>();
    let mut cursor = None;
    loop {
        let records = record::batch(
            client,
            keys,
            "cancellation",
            verification::Stage::Cancellations,
            cursor,
            now,
        )
        .await?;
        let Some(last) = records.last() else {
            break;
        };
        cursor = Some((last.person_id(), last.sequence()));
        verification::Stage::Cancellations
            .batch(std::time::Duration::from_secs(10), async {
                for record in records {
                    let (revision_id, operator_id) = match record.event() {
                        RecordEvent::Cancellation {
                            revision_id,
                            operator_id,
                            ..
                        } => (*revision_id, *operator_id),
                        _ => {
                            return Err(Error::Facts {
                                rule: "a cancellation loader selects cancellation records",
                            });
                        }
                    };
                    let profiles =
                        revision::fetch_many(client, keys, &[revision_id, operator_id]).await?;
                    let subject = profiles.get(&revision_id).ok_or(revision::Error::Missing)?;
                    if subject.person_id() != record.person_id() {
                        return Err(revision::Error::Person.into());
                    }
                    let operator = profiles
                        .get(&operator_id)
                        .ok_or(revision::Error::Missing)?
                        .clone();
                    let original = Original::from_record(&record, operator)?;
                    original.verify_source(client, keys, now).await?;
                    operations.remove(&original.operation_id);
                    original.check_account(accounts.get(&original.account_id).ok_or(
                        Error::Facts {
                            rule: "cancellation retains its permanent account",
                        },
                    )?)?;
                    if sessions.contains(&original.account_id) {
                        return Err(Error::Facts {
                            rule: "a cancelled account retains no sessions",
                        });
                    }
                }
                Ok::<(), Error>(())
            })
            .await??;
    }
    if !operations.is_empty() {
        return Err(Error::Facts {
            rule: "every permanent cancellation completion retains its cancellation",
        });
    }
    Ok(())
}

/// Authenticate the cancellation sources of a page of derived domain ends in one query.
/// `required` persons must have one; every cancellation of `persons` is authenticated either way.
pub(crate) async fn check_retained(
    client: &Client,
    app: &App,
    read: Read,
    persons: &[person::Id],
    required: &[person::Id],
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if persons.is_empty() {
        return Ok(());
    }
    let records = record::cancellations_for_persons(client, app.keys(), persons, now).await?;
    let mut found = Vec::with_capacity(records.len());
    for record in records {
        let (revision_id, operator_id) = match record.event() {
            RecordEvent::Cancellation {
                revision_id,
                operator_id,
                ..
            } => (*revision_id, *operator_id),
            _ => {
                return Err(Error::Facts {
                    rule: "a cancellation loader selects cancellation records",
                });
            }
        };
        let profiles =
            revision::fetch_many(client, app.keys(), &[revision_id, operator_id]).await?;
        let subject = profiles.get(&revision_id).ok_or(revision::Error::Missing)?;
        if subject.person_id() != record.person_id() {
            return Err(revision::Error::Person.into());
        }
        let operator = profiles
            .get(&operator_id)
            .ok_or(revision::Error::Missing)?
            .clone();
        let original = Original::from_record(&record, operator)?;
        found.push(original.person_id);
        original.verify_source(client, app.keys(), now).await?;
        original.verify_removal(client, app, read, now).await?;
    }
    if required.iter().any(|person| !found.contains(person)) {
        return Err(Error::Facts {
            rule: "derived ends retain their original cancellations",
        });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Revision(#[from] revision::Error),
    #[error("{0}")]
    History(#[from] history::Error),
    #[error("cancellation fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("cancellation query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("{0}")]
    Timeout(#[from] verification::Timeout),
    #[error("{0}")]
    Completion(#[from] kernel::Error<Infallible>),
    #[error("{0}")]
    Account(#[from] crate::identity::mobile::envelope::Error),
    #[error("{0}")]
    Encryption(#[from] keys::encryption::Error),
    #[error("historical cancellation mobile is not UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("historical cancellation mobile is invalid")]
    Mobile(#[from] crate::identity::mobile::ParseError),
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl Error {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::History(error) => error.corrupt(),
            Self::Revision(error) => error.unreliable(),
            Self::Facts { .. }
            | Self::Account(_)
            | Self::Encryption(_)
            | Self::Utf8(_)
            | Self::Mobile(_) => true,
            Self::Completion(error) => error.is_corrupt(),
            Self::Query(error) => {
                let mut option = error::Error::source(error);
                while let Some(cause) = option {
                    if cause.is::<platform::cryptography::envelope::ParseError>() {
                        return true;
                    }
                    option = cause.source();
                }
                false
            }
            Self::Timeout(_) => false,
        }
    }
}
