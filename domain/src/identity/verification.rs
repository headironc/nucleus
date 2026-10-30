//! Reconstruct retained identity facts without requiring their original accounts to survive.

use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, Utc};
use platform::cryptography;
use tokio_postgres::Client;

use crate::{
    database::verification,
    history::{self, Verified},
    identity::{
        account, avatar, keys,
        mobile::envelope::Binding,
        person::{self, revision},
        session,
    },
    kernel::App,
    keys::seal,
};

/// The caller retains its startup or maintenance boundary and owns connection termination.
#[cfg(test)]
pub(crate) async fn verify(
    client: &Client,
    encryption: &keys::encryption::Keys,
    mobile: &keys::mobile::Keys,
    number: &keys::number::Keys,
    seals: &seal::Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    load(client, encryption, mobile, number, seals, now).await?;
    Ok(())
}

pub(crate) async fn start(
    client: &Client,
    app: &App,
    keys: &keys::Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let (persons, accounts, sessions) = load(
        client,
        &keys.encryption,
        &keys.mobile,
        &keys.number,
        app.keys().seals,
        now,
    )
    .await?;
    history::initialize(app.anchors(), persons)?;
    history::initialize(app.anchors(), accounts)?;
    history::initialize(app.anchors(), sessions)?;
    Ok(())
}

async fn load(
    client: &Client,
    encryption: &keys::encryption::Keys,
    mobile: &keys::mobile::Keys,
    number: &keys::number::Keys,
    seals: &seal::Keys,
    now: DateTime<Utc>,
) -> Result<
    (
        Vec<Verified<person::state::State>>,
        Vec<Verified<account::state::State>>,
        Vec<Verified<session::state::State>>,
    ),
    Error,
> {
    let keys = history::Keys {
        data: encryption,
        seals,
    };
    let persons = history::inspect::<person::state::State>(client, keys, now).await?;
    let accounts = history::inspect::<account::state::State>(client, keys, now).await?;
    let sessions = history::inspect::<session::state::State>(client, keys, now).await?;
    crate::identity::correction::verify(client, keys, now).await?;
    crate::identity::cancellation::verify(client, keys, &accounts, &sessions, now).await?;
    let defaults = avatar::Defaults::verify()?;
    revision::verify(client, keys, now).await?;
    let persons_by_id = persons
        .iter()
        .map(|state| (*state.id().as_bytes(), state.revision_id()))
        .collect::<BTreeMap<_, _>>();
    for state in &accounts {
        if let Some(current) = state.current() {
            current
                .mobile()
                .open(encryption, mobile, Binding::Account(state.id()))?;
            let revision_id =
                *persons_by_id
                    .get(state.person_id().as_bytes())
                    .ok_or(Error::Facts {
                        rule: "a current account retains its permanent person state",
                    })?;
            let profile = verification::Stage::Accounts
                .batch(
                    Duration::from_secs(10),
                    revision::fetch(client, keys, revision_id),
                )
                .await??;
            if profile.person_id() != state.person_id()
                || current.number() != &number.hash(profile.id_number())?
            {
                return Err(Error::Facts {
                    rule: "current account identity-number hash",
                });
            }
        }
    }
    let mut active = accounts
        .iter()
        .filter(|state| state.current().is_some())
        .map(|state| (*state.id().as_bytes(), (state.avatar(), 0_usize)))
        .collect::<BTreeMap<_, _>>();
    let mut option: Option<avatar::Id> = None;
    loop {
        let rows = verification::Stage::Avatars.query(Duration::from_secs(10), client.query("SELECT id, revision AS avatar, account_id, digest, size, width, height, released_at, content_seal, content_seal_key_id, content_seal_version FROM identity.avatars WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT 128", &[&option])).await??;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let id: avatar::Id = row.try_get("id")?;
            let snapshot = avatar::Snapshot::read(&row, seals, &defaults)?;
            let account_id: account::Id = row.try_get("account_id")?;
            match row.try_get::<_, Option<DateTime<Utc>>>("released_at")? {
                None => {
                    let (revision, count) =
                        active.get_mut(account_id.as_bytes()).ok_or(Error::Facts {
                            rule: "unreleased avatar belongs to a current account",
                        })?;
                    if *revision != snapshot.revision() {
                        return Err(Error::Facts {
                            rule: "current avatar revision matches its authenticated account",
                        });
                    }
                    *count += 1;
                }
                Some(at) if at > now => {
                    return Err(Error::Facts {
                        rule: "avatar release time",
                    });
                }
                Some(_) => {}
            }
            option = Some(id);
        }
    }
    if active.values().any(|(_, count)| *count != 1) {
        return Err(Error::Facts {
            rule: "current account has one unreleased avatar",
        });
    }
    Ok((persons, accounts, sessions))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    History(#[from] history::Error),
    #[error("{0}")]
    Cancellation(#[from] crate::identity::cancellation::Error),
    #[error("{0}")]
    Correction(#[from] crate::identity::correction::Error),
    #[error("persisted identity facts are inconsistent: {rule}")]
    Facts { rule: &'static str },
    #[error("{0}")]
    Timeout(#[from] crate::database::verification::Timeout),
    #[error("identity verification database operation failed: {0}")]
    Database(#[from] crate::database::Query),
    #[error("{0}")]
    Avatar(#[from] avatar::Error),
    #[error("{0}")]
    Revision(#[from] revision::Error),
    #[error("{0}")]
    Mobile(#[from] crate::identity::mobile::envelope::Error),
    #[error("identity verification digest computation failed")]
    Crypto(#[from] cryptography::Error),
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(crate::database::Query(error))
    }
}
