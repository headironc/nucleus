//! The current actor and target required to continue one mobile change.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{self, appointment, role},
    database::transaction,
    identity::{
        account,
        current::{Actor, Service},
        keys::Keys,
        mobile::{Mobile, envelope::Binding},
        person,
        transfer::{Error, Id, database},
    },
};

/// Current qualification is reconstructed under the operation's account locks.
pub(crate) struct Target {
    account_id: account::Id,
    person_id: person::Id,
    revision_id: person::revision::Id,
    operator_id: person::Id,
    operator_revision_id: person::revision::Id,
    mobile: Mobile,
    observed_at: DateTime<Utc>,
}

impl Target {
    pub(super) fn qualified(
        account_id: account::Id,
        person_id: person::Id,
        revision_id: person::revision::Id,
        authority: &crate::kernel::Authorization,
        mobile: Mobile,
    ) -> Self {
        Self {
            account_id,
            person_id,
            revision_id,
            operator_id: authority.person_id,
            operator_revision_id: authority.revision_id,
            mobile,
            observed_at: authority.now,
        }
    }
    pub(super) async fn current(
        client: &Client,
        current: &Service,
        actor: &Actor,
    ) -> Result<Self, Error> {
        let contact = current.fetch_contact(client, actor).await?;
        Ok(Self {
            account_id: actor.account_id(),
            person_id: contact.person_id(),
            revision_id: contact.revision_id(),
            operator_id: contact.person_id(),
            operator_revision_id: contact.revision_id(),
            observed_at: contact.observed_at(),
            mobile: contact.into_mobile(),
        })
    }

    pub(crate) async fn assisted(
        client: &Client,
        roles: &role::Reader,
        current: &Service,
        keys: &Keys,
        actor: &Actor,
        account_id: account::Id,
    ) -> Result<Self, Error> {
        transaction::shared(client, b"access/scope", b"institution").await?;
        actor.lock_accounts(client, account_id).await?;
        let (role, operator_id, operator_revision_id, observed_at) = access::authorize(
            client,
            roles,
            current,
            actor,
            role::Code::InstitutionAccounts,
        )
        .await?;
        let target = appointment::find_by_account(client, roles, account_id, observed_at)
            .await?
            .ok_or(Error::Absent)?;
        if target.person_id() == operator_id || target.role().level() >= role.level() {
            return Err(Error::Absent);
        }
        let account = crate::history::find::<account::state::State>(
            client,
            roles.keys(),
            roles.anchors(),
            roles.read()?,
            account_id,
            observed_at,
        )
        .await?
        .filter(|account| account.current().is_some())
        .ok_or(Error::Absent)?;
        let mobile = account.current().ok_or(Error::Absent)?.mobile();
        let person = crate::identity::person::state::find(
            client,
            roles.keys(),
            roles.anchors(),
            roles.read()?,
            account.person_id(),
            observed_at,
        )
        .await?
        .ok_or(Error::Facts {
            rule: "a managed account retains its permanent person",
        })?;
        Ok(Self {
            account_id,
            person_id: target.person_id(),
            revision_id: person.revision_id(),
            operator_id,
            operator_revision_id,
            mobile: mobile.open(&keys.encryption, &keys.mobile, Binding::Account(account_id))?,
            observed_at,
        })
    }

    pub(super) fn account_id(&self) -> account::Id {
        self.account_id
    }
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(crate) fn revision_id(&self) -> person::revision::Id {
        self.revision_id
    }
    pub(crate) fn operator_id(&self) -> person::Id {
        self.operator_id
    }
    pub(crate) fn operator_revision_id(&self) -> person::revision::Id {
        self.operator_revision_id
    }
    pub(crate) fn mobile(&self) -> &Mobile {
        &self.mobile
    }
    pub(crate) fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }
}

/// Only assisted sends use Access, before taking any token or account locks.
pub(in crate::identity) async fn lock_scope(client: &Client, id: Id) -> Result<(), Error> {
    let row = client
        .query_opt(
            "SELECT message_id IS NULL AS assisted FROM identity.transfers WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or(Error::Verification)?;
    if row.try_get::<_, bool>("assisted")? {
        transaction::shared(client, b"access/scope", b"institution").await?;
    }
    Ok(())
}

async fn sender(
    client: &Client,
    roles: &role::Reader,
    current: &Service,
    keys: &Keys,
    actor: &Actor,
    id: Id,
) -> Result<(Target, database::Sender), Error> {
    let sender = database::find_sender_by_id(client, id)
        .await?
        .ok_or(Error::Verification)?;
    if sender.session_id() != actor.session_id() {
        return Err(Error::Verification);
    }
    let account_id = account::state::discover_person(client, sender.person_id())
        .await?
        .ok_or(Error::Verification)?;
    let target = if account_id == actor.account_id() {
        actor.lock_account(client).await?;
        Target::current(client, current, actor).await?
    } else {
        Target::assisted(client, roles, current, keys, actor, account_id).await?
    };
    // The first read locates immutable ownership; waiting for the account locks can change latestness.
    let sender = database::find_sender_by_id(client, id)
        .await?
        .ok_or(Error::Verification)?;
    Ok((target, sender))
}

/// Replays keep current reader qualification without acquiring permission for a new send.
pub(in crate::identity) async fn authenticate_sender(
    client: &Client,
    roles: &role::Reader,
    current: &Service,
    keys: &Keys,
    actor: &Actor,
    id: Id,
) -> Result<(), Error> {
    sender(client, roles, current, keys, actor, id).await?;
    Ok(())
}

pub(in crate::identity) async fn qualify_sender(
    client: &Client,
    roles: &role::Reader,
    current: &Service,
    keys: &Keys,
    actor: &Actor,
    id: Id,
    mobile: &Mobile,
) -> Result<Target, Error> {
    let (target, sender) = sender(client, roles, current, keys, actor, id).await?;
    let now = current.now(client).await?;
    sender.qualify(actor, &target, now)?;
    if account::record::replacement(client, roles.keys(), id, now)
        .await?
        .is_some()
    {
        return Err(Error::Verification);
    }
    if target.mobile().as_bytes() == mobile.as_bytes() {
        return Err(Error::Conflict);
    }
    Ok(target)
}
