//! Atomic number restoration and the fixed result reconstructed from its records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::grant::member,
    history::{self, Read, Verified},
    identity::{
        account,
        current::{self, Actor},
        mobile::Mobile,
        operation, person,
    },
    kernel::{Authorization, Command, completion, tx::Tx},
    workforce::number::{
        Error, Id, Reason, Rejection, Revision, Service, command, record, revision,
        state::{self, State},
        transition,
    },
};

/// The identifier of one permanent number recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = number::recovery::Id, value_type = Uuid)]
pub struct RecoveryId(Uuid);

/// The identifier of one permanent number revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = number::revocation::Id, value_type = Uuid)]
pub struct RevocationId(Uuid);

/// The beneficiary's confirmed real-name revision, current number (null when none) and the
/// original revision whose exact value is restored.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = number::recovery::Beneficiary)]
pub struct Beneficiary {
    pub(super) person_id: person::Id,
    pub(super) revision_id: person::revision::Id,
    #[schema(required)]
    #[serde(deserialize_with = "Option::deserialize")]
    pub(super) current: Option<Id>,
    pub(super) origin: Id,
}

/// The occupier's confirmed real-name revision and the current number that holds the original slot.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = number::recovery::Occupier)]
pub struct Occupier {
    pub(super) person_id: person::Id,
    pub(super) revision_id: person::revision::Id,
    pub(super) current: Id,
}

/// Restore an original number by revoking its occupier and, when present, the beneficiary's current number.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = number::recovery::Recovery)]
pub struct Recovery {
    pub(super) beneficiary: Beneficiary,
    pub(super) occupier: Occupier,
    pub(super) reason: Reason,
}

/// One revoked current number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    id: RevocationId,
    person_id: person::Id,
    number: Revision,
}

impl Revoked {
    pub fn id(&self) -> RevocationId {
        self.id
    }
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn number(&self) -> &Revision {
        &self.number
    }
}

/// A permanent recovery: the beneficiary's new revision and its revocations, occupier first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    id: RecoveryId,
    person_id: person::Id,
    origin: Id,
    number: Revision,
    revocations: Vec<Revoked>,
}

impl Restored {
    pub fn id(&self) -> RecoveryId {
        self.id
    }
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn origin(&self) -> Id {
        self.origin
    }
    pub fn number(&self) -> &Revision {
        &self.number
    }
    pub fn revocations(&self) -> &[Revoked] {
        &self.revocations
    }
}

/// A manageable beneficiary's masked identity and current number, which may be absent.
pub struct RecoveryCandidate {
    identity: account::Subject,
    mobile: Mobile,
    number: Option<Revision>,
}

impl RecoveryCandidate {
    pub fn identity(&self) -> &account::Subject {
        &self.identity
    }
    pub fn mobile(&self) -> &Mobile {
        &self.mobile
    }
    pub fn number(&self) -> Option<&Revision> {
        self.number.as_ref()
    }
}

impl Service {
    pub async fn recovery_candidate(
        &self,
        actor: &Actor,
        mobile: Mobile,
    ) -> Result<RecoveryCandidate, Error> {
        let (identity, _, number) = self
            .app
            .query(&super::query::Candidate {
                service: self,
                actor,
                mobile: &mobile,
                kind: super::query::CandidateKind::Recovery,
            })
            .await?;
        Ok(RecoveryCandidate {
            identity,
            mobile,
            number,
        })
    }

    pub async fn restore(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Recovery,
    ) -> Result<Restored, Error> {
        self.app
            .command(
                operation_id,
                Restore {
                    service: self.clone(),
                    actor,
                    input,
                    id: Uuid::new_v4(),
                    number_id: Id::allocate(),
                    occupier_id: Uuid::new_v4(),
                    revocation_id: Uuid::new_v4(),
                },
            )
            .await
    }
}

pub(super) struct Restore {
    service: Service,
    actor: Actor,
    input: Recovery,
    id: Uuid,
    number_id: Id,
    occupier_id: Uuid,
    revocation_id: Uuid,
}

pub(super) struct Facts {
    beneficiary: Option<Verified<State>>,
    occupier: Verified<State>,
    origin: Option<Verified<revision::Value>>,
    occupier_cancelled: bool,
    beneficiary_grant: Option<bool>,
    operation: transition::Operation,
}

pub(super) struct Change {
    states: transition::Restored,
    result: Restored,
}

impl Command for Restore {
    const COMPLETIONS: completion::Template = command::RECOVERIES;
    type Facts = Facts;
    type Change = Change;
    type Output = Restored;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }
    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        completion::Request::new(
            "recover",
            self.input.beneficiary.person_id.to_string(),
            &self.input,
            authority,
        )
    }
    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        let persons = [
            self.input.beneficiary.person_id,
            self.input.occupier.person_id,
        ];
        command::participants(tx, &self.actor, &persons).await?;
        let mut pools = Vec::new();
        for person in persons {
            if let Some((_, number)) = state::discover(tx.client(), person).await? {
                pools.push((number.subtype().category(), number.year()));
            }
        }
        let now = tx.now(self.service.app.context()).await?;
        if let Some(value) = history::find_value::<revision::Value>(
            tx.client(),
            self.service.app.keys(),
            self.input.beneficiary.origin,
            now,
        )
        .await?
        {
            let number = value.revision().number();
            pools.push((number.subtype().category(), number.year()));
        }
        command::pools(tx.client(), pools).await
    }
    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        command::manager(&self.service, tx, subject).await
    }
    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        if self.input.beneficiary.person_id == self.input.occupier.person_id {
            return Err(Rejection::Recovery.into());
        }
        let beneficiary = &self.input.beneficiary;
        let occupier = &self.input.occupier;
        let account = self
            .service
            .accounts
            .with_read(Read::Shared)
            .find_by_person(tx.client(), beneficiary.person_id, authority.now)
            .await?
            .ok_or(Rejection::Absent)?;
        command::confirm(
            &self.service,
            tx.client(),
            beneficiary.person_id,
            beneficiary.revision_id,
            authority.now,
        )
        .await?;
        command::manageable(
            tx.client(),
            &self.service.roles,
            &self.actor,
            beneficiary.person_id,
            Some(account),
            authority.now,
        )
        .await?;
        let occupied_account = self
            .service
            .accounts
            .with_read(Read::Shared)
            .find_by_person(tx.client(), occupier.person_id, authority.now)
            .await?;
        command::confirm(
            &self.service,
            tx.client(),
            occupier.person_id,
            occupier.revision_id,
            authority.now,
        )
        .await?;
        command::manageable(
            tx.client(),
            &self.service.roles,
            &self.actor,
            occupier.person_id,
            occupied_account,
            authority.now,
        )
        .await?;
        let beneficiary_state = history::find::<State>(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            beneficiary.person_id,
            authority.now,
        )
        .await?;
        let occupier_state = history::find::<State>(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            occupier.person_id,
            authority.now,
        )
        .await?
        .ok_or(Rejection::TargetChanged)?;
        let origin = history::find_value::<revision::Value>(
            tx.client(),
            self.service.app.keys(),
            beneficiary.origin,
            authority.now,
        )
        .await?;
        let beneficiary_grant = if origin
            .as_ref()
            .is_some_and(|value| value.person_id() == beneficiary.person_id)
        {
            None
        } else {
            member::find_by_person(
                tx.client(),
                &self.service.roles,
                beneficiary.person_id,
                authority.now,
            )
            .await?
            .map(|member| member.suspended())
        };
        Ok(Facts {
            beneficiary: beneficiary_state,
            occupier: occupier_state,
            origin,
            occupier_cancelled: occupied_account.is_none(),
            beneficiary_grant,
            operation: transition::Operation {
                id: tx.operation_id(),
                operator_id: authority.revision_id,
                time: authority.now,
            },
        })
    }
    fn decide(&self, facts: Facts, _: DateTime<Utc>) -> Result<Change, Error> {
        let beneficiary = &self.input.beneficiary;
        let occupier = &self.input.occupier;
        if facts
            .beneficiary
            .as_ref()
            .and_then(|state| state.current())
            .map(|revision| revision.id())
            != beneficiary.current
            || facts.occupier.current().map(|revision| revision.id()) != Some(occupier.current)
        {
            return Err(Rejection::TargetChanged.into());
        }
        let origin = facts.origin.as_ref().ok_or(Rejection::Recovery)?;
        let original = origin.revision().number();
        if origin.person_id() == beneficiary.person_id {
            if !facts.occupier.revision().number().shares_pool(original) {
                return Err(Rejection::Recovery.into());
            }
        } else {
            if beneficiary.origin != occupier.current
                || origin.person_id() != occupier.person_id
                || !facts.occupier_cancelled
                || facts.beneficiary.is_some()
            {
                return Err(Rejection::Recovery.into());
            }
            match facts.beneficiary_grant {
                None => return Err(Rejection::Absent.into()),
                Some(true) => return Err(Rejection::StateConflict.into()),
                Some(false) => {}
            }
        }
        let number = Revision {
            id: self.number_id,
            number: original,
        };
        let mut revocations = vec![Revoked {
            id: RevocationId(self.occupier_id),
            person_id: occupier.person_id,
            number: facts.occupier.revision(),
        }];
        if let Some(number) = facts.beneficiary.as_ref().and_then(|state| state.current()) {
            revocations.push(Revoked {
                id: RevocationId(self.revocation_id),
                person_id: beneficiary.person_id,
                number,
            });
        }
        let states = transition::restore(
            facts.beneficiary.as_ref(),
            &facts.occupier,
            transition::Recovery {
                id: self.id,
                person_id: beneficiary.person_id,
                revision_id: beneficiary.revision_id,
                number,
                origin_id: beneficiary.origin,
                occupier_id: self.occupier_id,
                occupier_revision_id: occupier.revision_id,
                revocation_id: self.revocation_id,
                reason: self.input.reason.clone(),
            },
            &facts.operation,
        )?;
        Ok(Change {
            states,
            result: Restored {
                id: RecoveryId(self.id),
                person_id: beneficiary.person_id,
                origin: beneficiary.origin,
                number,
                revocations,
            },
        })
    }
    async fn apply(
        &self,
        tx: &Tx,
        change: Change,
        pending: &mut Option<history::Pending>,
    ) -> Result<Restored, Error> {
        history::insert_value::<revision::Value>(
            tx.client(),
            self.service.app.keys(),
            change.states.beneficiary.value,
        )
        .await?;
        *pending = Some(
            change
                .states
                .occupier
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await?,
        );
        let next = change
            .states
            .beneficiary
            .change
            .write(
                tx.client(),
                self.service.app.keys(),
                self.service.app.anchors(),
            )
            .await?;
        pending
            .as_mut()
            .ok_or_else(|| Error::corrupt("recovery retains both state candidates"))?
            .extend(next)?;
        Ok(change.result)
    }
    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Restored, Error> {
        let recovery = command::original(&self.service, tx, "recovery", request, now).await?;
        let record::Event::Recovery {
            origin_id,
            occupier_id,
            revocation_id,
            ..
        } = recovery.event()
        else {
            return Err(Error::corrupt(
                "a recovery completion names a recovery record",
            ));
        };
        let number = command::fixed(&self.service, tx.client(), &recovery, now).await?;
        let mut revocations = Vec::new();
        for id in std::iter::once(*occupier_id).chain(*revocation_id) {
            let record =
                record::by_id(tx.client(), self.service.app.keys(), "revocation", id, now).await?;
            if !matches!(record.event(), record::Event::Revocation { recovery_id, .. } if *recovery_id == recovery.id())
            {
                return Err(Error::corrupt("a recovery retains its fixed revocations"));
            }
            let number = command::fixed(&self.service, tx.client(), &record, now).await?;
            // The beneficiary's intermediate revocation is not its final state candidate.
            if id == *occupier_id {
                self.service.app.anchors().confirm::<State>(&record)?;
            }
            revocations.push(Revoked {
                id: RevocationId(id),
                person_id: record.person_id(),
                number,
            });
        }
        self.service.app.anchors().confirm::<State>(&recovery)?;
        Ok(Restored {
            id: RecoveryId(recovery.id()),
            person_id: recovery.person_id(),
            origin: *origin_id,
            number,
            revocations,
        })
    }
}

impl RecoveryId {
    pub(super) fn from_record(id: Uuid) -> Self {
        Self(id)
    }
}
