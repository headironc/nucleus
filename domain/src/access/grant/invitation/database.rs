//! Permanent invitation facts, their derived availability, and retained-code verification.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{
        capability::Scope,
        grant::{
            self, Error,
            invitation::{Capacity, Code, Expiry, Id, Invitation, Reason, keys},
            termination::Mode,
        },
        role,
    },
    database::sealed::Sealed,
    identity::{account, operation, person},
};

/// The authenticated original Grant's current facts.
#[derive(Clone, Copy)]
struct GrantFacts {
    epoch: i64,
    ended: Option<Mode>,
    suspended: bool,
}

/// The creator Account's current state; a tombstone has no current suspension.
#[derive(Clone, Copy)]
struct AccountFacts {
    tombstoned: bool,
    suspended: bool,
}

/// Current invitation values and authenticated facts that decide that invitation.
#[derive(Clone, Copy)]
struct Availability {
    now: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    capacity: i64,
    used: i64,
    withdrawn: bool,
    created_epoch: i64,
    grant: GrantFacts,
    account: AccountFacts,
    assignable: bool,
}

impl Availability {
    /// Every current reason in the documented order without stopping at the first one.
    fn reasons(&self) -> Vec<Reason> {
        let mut reasons = Vec::new();
        if self.expires_at <= self.now {
            reasons.push(Reason::Expired);
        }
        if self.withdrawn {
            reasons.push(Reason::Withdrawn);
        }
        match self.grant.ended {
            Some(Mode::Current | Mode::Manager) => reasons.push(Reason::GrantTerminated),
            Some(Mode::Cancellation) => reasons.push(Reason::AccountCancelled),
            None => {}
        }
        if self.grant.epoch != self.created_epoch {
            reasons.push(Reason::AdmissionLost);
        }
        if !self.account.tombstoned && self.account.suspended {
            reasons.push(Reason::AccountSuspended);
        }
        if self.grant.ended.is_none() && self.grant.suspended {
            reasons.push(Reason::GrantSuspended);
        }
        if self.unassignable() {
            reasons.push(Reason::RoleUnassignable);
        }
        if self.used >= self.capacity {
            reasons.push(Reason::CapacityReached);
        }
        reasons
    }

    #[cfg(test)]
    fn is_manageable(&self) -> bool {
        manageable(&self.reasons())
    }

    /// Only an unfinished Grant whose selected current definition is no longer a strict
    /// subordinate is unassignable; an ended Grant keeps its historical selection.
    fn unassignable(&self) -> bool {
        self.grant.ended.is_none() && !self.assignable
    }
}

/// Management excludes permanent reasons and both suspensions; capacity and role
/// unassignability are temporary and do not prevent copy, adjustment or withdrawal.
fn manageable(reasons: &[Reason]) -> bool {
    reasons
        .iter()
        .all(|reason| matches!(reason, Reason::RoleUnassignable | Reason::CapacityReached))
}

pub(in crate::access) async fn in_use(
    client: &Client,
    roles: &role::Reader,
    id: role::Id,
    now: DateTime<Utc>,
) -> Result<bool, Error> {
    for state in super::state::for_role(client, roles, id, roles.read()?, now).await? {
        let fact = Fact::new(state).qualify(client, roles, now).await?;
        if !fact.reasons.iter().any(|reason| {
            matches!(
                reason,
                Reason::Expired
                    | Reason::Withdrawn
                    | Reason::GrantTerminated
                    | Reason::AccountCancelled
                    | Reason::AdmissionLost
            )
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Unverified discovery is used only to acquire the decisive locks.
#[derive(Clone, Copy)]
pub(super) struct Hint {
    pub(super) id: Id,
    pub(super) person_id: person::Id,
    pub(super) grant_id: grant::Id,
}

pub(super) async fn discover_code(
    client: &Client,
    keys: &keys::Lookup,
    code: &Code,
) -> Result<Option<Hint>, Error> {
    let mut ids = Vec::new();
    let mut hashes = Vec::new();
    for key in keys.all() {
        ids.push(key.id());
        hashes.push(code.lookup(key)?.to_vec());
    }
    client.query_opt("SELECT invitation.id,invitation.person_id,invitation.grant_id FROM access.invitations invitation JOIN unnest($1::bigint[], $2::bytea[]) AS candidate (key_id, hash) ON candidate.key_id = invitation.key_id AND candidate.hash = invitation.hash", &[&ids, &hashes]).await?
        .map(|row| Ok(Hint {
            id: row.try_get("id").map_err(Error::decode)?,
            person_id: row.try_get("person_id").map_err(Error::decode)?,
            grant_id: row.try_get("grant_id").map_err(Error::decode)?,
        })).transpose()
}

pub(super) async fn find_by_code(
    client: &Client,
    roles: &role::Reader,
    keys: &keys::Keys,
    code: &Code,
    now: DateTime<Utc>,
) -> Result<Option<Fact>, Error> {
    match super::state::locate_code(client, keys, code, roles.read()?, roles, now).await? {
        Some(state) => Ok(Some(Fact::new(state).qualify(client, roles, now).await?)),
        None => Ok(None),
    }
}

pub(super) struct Fact {
    pub(super) state: crate::history::Verified<super::state::State>,
    id: Id,
    pub(super) grant_id: grant::Id,
    epoch: i64,
    pub(super) person_id: person::Id,
    pub(super) role_id: role::Id,
    pub(super) created_at: DateTime<Utc>,
    pub(super) current_capacity: Capacity,
    pub(super) current_expiry: Expiry,
    withdrawn_at: Option<DateTime<Utc>>,
    used: i64,
    operation_id: operation::Id,
    code: Arc<Sealed>,
    hash: Vec<u8>,
    key_id: crate::keys::Id,
    reasons: Vec<Reason>,
    manageable: bool,
}

impl Fact {
    async fn qualify(
        mut self,
        client: &Client,
        roles: &role::Reader,
        now: DateTime<Utc>,
    ) -> Result<Self, Error> {
        let state = grant::state::find(client, roles, self.grant_id, roles.read()?, now)
            .await?
            .ok_or(Error::Facts {
                rule: "an invitation retains its original authenticated Grant",
            })?;
        if state.person_id() != self.person_id
            || self.epoch < 0
            || self.epoch > state.epoch()
            || self.created_at < state.granted_at()
        {
            return Err(Error::Facts {
                rule: "an invitation retains its creator, original Grant period and admitted epoch",
            });
        }
        let account = account::state::located(
            client,
            roles.keys(),
            roles.anchors(),
            roles.read()?,
            account::state::Location::Person(self.person_id),
            now,
        )
        .await?
        .ok_or(Error::Facts {
            rule: "an invitation creator retains its permanent account",
        })?;
        let manager = if state.unfinished() {
            Some(
                roles
                    .find(client, state.role_id(), Scope::Workforce, now)
                    .await?
                    .ok_or(Error::Facts {
                        rule: "an unfinished invitation Grant retains its role",
                    })?,
            )
        } else {
            None
        };
        let selected = roles
            .find(client, self.role_id, Scope::Workforce, now)
            .await?;
        let availability = Availability {
            now,
            expires_at: self.current_expiry.into(),
            capacity: self.current_capacity.into(),
            used: self.used,
            withdrawn: self.withdrawn_at.is_some(),
            created_epoch: self.epoch,
            grant: GrantFacts {
                epoch: state.epoch(),
                ended: state.termination(),
                suspended: state.suspension_id().is_some(),
            },
            account: AccountFacts {
                tombstoned: account.current().is_none(),
                suspended: account
                    .current()
                    .is_some_and(|current| current.suspension_id().is_some()),
            },
            assignable: manager.as_ref().zip(selected.as_ref()).is_some_and(
                |(manager, selected)| crate::access::Entry::Domain.can_assign(manager, selected),
            ),
        };
        let reasons = availability.reasons();
        self.manageable = manageable(&reasons);
        self.reasons = reasons;
        Ok(self)
    }
    fn new(state: crate::history::Verified<super::state::State>) -> Self {
        Self {
            id: state.id(),
            grant_id: state.grant_id(),
            epoch: state.epoch(),
            person_id: state.person_id(),
            role_id: state.role_id(),
            current_capacity: state.capacity(),
            current_expiry: state.expires_at(),
            created_at: state.created_at(),
            withdrawn_at: state.withdrawn_at(),
            used: state.used(),
            operation_id: state.operation_id(),
            code: Arc::clone(&state.data().creation.code),
            hash: state.hash().to_vec(),
            key_id: state.key_id(),
            reasons: Vec::new(),
            manageable: false,
            state,
        }
    }

    /// Whether its creator may still copy, adjust or withdraw this invitation.
    pub(super) fn manageable(&self) -> bool {
        self.manageable
    }

    pub(super) fn available(&self) -> bool {
        self.reasons.is_empty()
    }

    /// Authenticates the retained code and its lookup digest under their recorded versions.
    pub(super) fn open(&self, keys: &keys::Keys) -> Result<Code, Error> {
        let code = Code::open(keys, self.id, self.grant_id, self.epoch, &self.code)?;
        if code.lookup(keys.lookup.fetch(self.key_id)?)?[..] != self.hash[..] {
            return Err(Error::Facts {
                rule: "invitation lookup digest belongs to its retained original code",
            });
        }
        Ok(code)
    }

    /// Integrity is checked for every delivery; only disclosure depends on the copy condition.
    pub(super) fn deliver(self, keys: &keys::Keys) -> Result<Invitation, Error> {
        let code = self.open(keys)?;
        let code = self.manageable.then_some(code);
        Ok(Invitation {
            id: self.id,
            role_id: self.role_id,
            capacity: self.current_capacity,
            expires_at: self.current_expiry.into(),
            created_at: self.created_at,
            operation_id: self.operation_id,
            reasons: self.reasons,
            code,
        })
    }
}

pub(super) async fn find_by_id(
    client: &Client,
    roles: &role::Reader,
    id: Id,
    now: DateTime<Utc>,
) -> Result<Option<Fact>, Error> {
    let option = super::state::find(client, roles, id, roles.read()?, now).await?;
    match option {
        Some(state) => Ok(Some(Fact::new(state).qualify(client, roles, now).await?)),
        None => Ok(None),
    }
}

/// Only the creator's own row is decoded, so other creators' codes stay indistinguishable.
pub(super) async fn find_by_creator(
    client: &Client,
    roles: &role::Reader,
    id: Id,
    person_id: person::Id,
    read: crate::history::Read,
    now: DateTime<Utc>,
) -> Result<Option<Fact>, Error> {
    let option = super::state::locate_creator(client, roles, id, person_id, read, now).await?;
    match option {
        Some(state) => Ok(Some(Fact::new(state).qualify(client, roles, now).await?)),
        None => Ok(None),
    }
}

pub(super) async fn find_by_operation(
    client: &Client,
    roles: &role::Reader,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Fact>, Error> {
    let option =
        super::state::locate_operation(client, roles, operation_id, roles.read()?, now).await?;
    match option {
        Some(state) => Ok(Some(Fact::new(state).qualify(client, roles, now).await?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use chrono::{DateTime, Utc};

    use super::{AccountFacts, Availability, GrantFacts};
    use crate::access::grant::{invitation::Reason, termination::Mode};

    fn time(second: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(second, 0).expect("representable test instant")
    }

    /// A healthy invitation under an unfinished Grant and a strict subordinate selection.
    fn current() -> Availability {
        Availability {
            now: time(100),
            expires_at: time(200),
            capacity: 2,
            used: 0,
            withdrawn: false,
            created_epoch: 3,
            grant: GrantFacts {
                epoch: 3,
                ended: None,
                suspended: false,
            },
            account: AccountFacts {
                tombstoned: false,
                suspended: false,
            },
            assignable: true,
        }
    }

    #[test]
    fn every_reason_comes_from_current_facts_in_the_documented_order() {
        let mut facts = current();
        assert_eq!(facts.reasons(), []);
        assert!(facts.is_manageable());

        facts.expires_at = facts.now;
        assert_eq!(facts.reasons(), [Reason::Expired]);
        assert!(!facts.is_manageable());

        let mut facts = current();
        facts.withdrawn = true;
        assert_eq!(facts.reasons(), [Reason::Withdrawn]);
        assert!(!facts.is_manageable());

        // No reason short-circuits another: temporary and permanent facts coexist in order.
        let mut facts = current();
        facts.expires_at = facts.now;
        facts.withdrawn = true;
        facts.grant.ended = Some(Mode::Manager);
        facts.grant.epoch = 4;
        facts.account.suspended = true;
        facts.used = facts.capacity;
        assert_eq!(
            facts.reasons(),
            [
                Reason::Expired,
                Reason::Withdrawn,
                Reason::GrantTerminated,
                Reason::AdmissionLost,
                Reason::AccountSuspended,
                Reason::CapacityReached,
            ]
        );
        assert!(!facts.is_manageable());

        facts.grant.ended = Some(Mode::Cancellation);
        assert_eq!(
            facts.reasons(),
            [
                Reason::Expired,
                Reason::Withdrawn,
                Reason::AccountCancelled,
                Reason::AdmissionLost,
                Reason::AccountSuspended,
                Reason::CapacityReached,
            ]
        );
    }

    #[test]
    fn ending_the_grant_selects_its_mode_and_stops_role_and_grant_suspension_checks() {
        for (mode, reason) in [
            (Mode::Current, Reason::GrantTerminated),
            (Mode::Manager, Reason::GrantTerminated),
            (Mode::Cancellation, Reason::AccountCancelled),
        ] {
            let mut facts = current();
            facts.grant.ended = Some(mode);
            facts.grant.suspended = true;
            facts.assignable = false;
            // An ended Grant has no current unassignability or suspension; its epoch is unchanged.
            assert_eq!(facts.reasons(), [reason]);
            assert!(!facts.is_manageable());
        }
    }

    #[test]
    fn an_account_tombstone_drops_its_historical_suspension() {
        let mut facts = current();
        facts.account = AccountFacts {
            tombstoned: true,
            suspended: true,
        };
        assert_eq!(facts.reasons(), []);
        assert!(facts.is_manageable());

        // The deserved permanent fact is the Grant's cancellation, not the former suspension.
        facts.grant.ended = Some(Mode::Cancellation);
        assert_eq!(facts.reasons(), [Reason::AccountCancelled]);
        assert!(!facts.is_manageable());
    }

    #[test]
    fn a_lost_admission_survives_restored_current_authority() {
        let mut facts = current();
        facts.grant.epoch = 4;
        assert_eq!(facts.reasons(), [Reason::AdmissionLost]);
        assert!(!facts.is_manageable());

        // A current definition suitable for new invitations does not lower the irreversible epoch.
        facts.assignable = true;
        assert_eq!(facts.reasons(), [Reason::AdmissionLost]);

        // A suspension does not change the epoch, and full capacity adds only its own reason.
        facts.grant.suspended = true;
        facts.used = facts.capacity;
        assert_eq!(
            facts.reasons(),
            [
                Reason::AdmissionLost,
                Reason::GrantSuspended,
                Reason::CapacityReached,
            ]
        );
        assert!(!facts.is_manageable());
    }

    #[test]
    fn capacity_and_role_changes_are_temporary_but_suspensions_are_not() {
        let mut facts = current();
        facts.used = facts.capacity;
        assert_eq!(facts.reasons(), [Reason::CapacityReached]);
        assert!(facts.is_manageable(), "a full code may still be adjusted");

        facts.assignable = false;
        assert_eq!(
            facts.reasons(),
            [Reason::RoleUnassignable, Reason::CapacityReached]
        );
        assert!(
            facts.is_manageable(),
            "a temporarily unassignable selection may still be withdrawn"
        );

        facts.account.suspended = true;
        assert_eq!(
            facts.reasons(),
            [
                Reason::AccountSuspended,
                Reason::RoleUnassignable,
                Reason::CapacityReached,
            ]
        );
        assert!(!facts.is_manageable());

        let mut facts = current();
        facts.grant.suspended = true;
        assert_eq!(facts.reasons(), [Reason::GrantSuspended]);
        assert!(!facts.is_manageable());

        let mut facts = current();
        facts.expires_at = facts.now;
        facts.used = facts.capacity;
        assert!(!facts.is_manageable(), "expiry is permanent");
        assert!(!facts.reasons().is_empty());
    }
}
