//! Number writes share the kernel's completion, commit and recovery pipeline.

use std::sync::atomic::{AtomicI32, Ordering};

use chrono::{DateTime, Datelike, Utc};
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    access::{
        appointment,
        grant::{self, member},
        role,
    },
    history::{self, Read, Verified},
    identity::{
        account,
        current::{self, Actor},
        person,
    },
    kernel::{
        Authorization, Command, completion,
        operation::Family,
        tx::{self, Tx},
    },
    workforce::number::{
        Assistance, Correction, Error, Id, Input, Number, Rejection, Revision, Service, Source,
        Supplement, record, revision,
        state::{self, State},
        transition,
    },
};

fn person_target(target: &str) -> bool {
    target
        .parse::<person::Id>()
        .is_ok_and(|id| id.to_string() == target)
}

pub(super) const ALLOCATIONS: completion::Template =
    completion::Template::personal(Family::NumberAllocation, |action, target| {
        matches!(action, "self_service" | "assistance" | "supplement") && person_target(target)
    });
pub(super) const CORRECTIONS: completion::Template =
    completion::Template::personal(Family::NumberCorrection, |action, target| {
        action == "correct" && person_target(target)
    });
pub(super) const RECOVERIES: completion::Template =
    completion::Template::personal(Family::NumberRecovery, |action, target| {
        action == "recover" && person_target(target)
    });

pub(super) fn year(now: DateTime<Utc>) -> i32 {
    (now + chrono::Duration::hours(8)).year()
}

/// Person locks stabilize discovery while account and Grant states keep eligibility fixed.
pub(super) async fn participants(
    tx: &Tx,
    actor: &Actor,
    persons: &[person::Id],
) -> Result<(), Error> {
    tx.shared(b"access/scope", b"institution").await?;
    tx.shared(b"access/scope", b"workforce").await?;
    account::state::lock_participants(tx.client(), actor.account_id(), persons).await?;
    let owner = account::state::discover_owner(tx.client(), actor.account_id()).await?;
    person::state::lock_participants(tx.client(), owner, persons).await?;
    actor.lock_session(tx.client()).await?;
    grant::state::lock_participants(tx.client(), actor.account_id(), persons).await?;
    Ok(())
}

pub(super) async fn pools(client: &Client, mut pools: Vec<(i32, i32)>) -> Result<(), Error> {
    pools.sort_unstable();
    pools.dedup();
    for (category, year) in pools {
        let bytes = [category.to_be_bytes(), year.to_be_bytes()].concat();
        tx::object(client, b"workforce/number/pool", &bytes).await?;
    }
    Ok(())
}

pub(super) async fn manager(
    service: &Service,
    tx: &Tx,
    subject: Option<current::database::State>,
) -> Result<Authorization, Error> {
    let subject =
        subject.ok_or_else(|| Error::corrupt("a number command has a qualified session"))?;
    let (_, person_id, revision_id, now) = member::authorize_subject(
        tx.client(),
        &service.roles,
        subject,
        role::Code::WorkforceNumbers,
    )
    .await?;
    Ok(Authorization {
        person_id,
        revision_id,
        now,
    })
}

pub(super) async fn manageable(
    client: &Client,
    roles: &role::Reader,
    actor: &Actor,
    id: person::Id,
    account: Option<account::Id>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    if member::find_by_person(client, roles, id, now)
        .await?
        .is_some()
    {
        let account =
            account.ok_or_else(|| Error::corrupt("a current member retains its account"))?;
        let office = appointment::find_by_account(client, roles, actor.account_id(), now)
            .await?
            .ok_or(Error::Denied)?;
        let target = appointment::find_by_account(client, roles, account, now)
            .await?
            .ok_or(Rejection::Absent)?;
        if target.role().level() >= office.role().level() {
            return Err(Error::Denied);
        }
    }
    Ok(())
}

pub(super) async fn target(
    client: &Client,
    roles: &role::Reader,
    id: person::Id,
    now: DateTime<Utc>,
) -> Result<(account::Id, member::Member), Error> {
    let account = account::state::located(
        client,
        roles.keys(),
        roles.anchors(),
        roles.read()?,
        account::state::Location::Person(id),
        now,
    )
    .await?
    .filter(|account| account.current().is_some())
    .map(|account| account.id())
    .ok_or(Rejection::Absent)?;
    let member = member::find_by_person(client, roles, id, now)
        .await?
        .ok_or(Rejection::Absent)?;
    Ok((account, member))
}

pub(super) async fn qualify(
    client: &Client,
    roles: &role::Reader,
    actor: &Actor,
    account: account::Id,
    member: &member::Member,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let office = appointment::find_by_account(client, roles, actor.account_id(), now)
        .await?
        .ok_or_else(|| Error::corrupt("a number manager retains its institutional assignment"))?;
    let target = appointment::find_by_account(client, roles, account, now)
        .await?
        .ok_or(Rejection::Absent)?;
    if target.role().level() >= office.role().level() {
        return Err(Error::Denied);
    }
    if member.suspended() {
        return Err(Rejection::StateConflict.into());
    }
    Ok(())
}

pub(super) async fn confirm(
    service: &Service,
    client: &Client,
    id: person::Id,
    revision: person::revision::Id,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let identity = person::Reader::new(
        service.accounts.clone(),
        std::sync::Arc::clone(&service.keys),
    )
    .with_read(Read::Shared)
    .profile(client, id, now)
    .await?
    .ok_or(Rejection::Absent)?;
    if identity.revision_id() != revision {
        return Err(Rejection::TargetChanged.into());
    }
    Ok(())
}

pub(super) async fn fixed(
    service: &Service,
    client: &Client,
    record: &record::Record,
    now: DateTime<Utc>,
) -> Result<Revision, Error> {
    let value =
        history::find_value::<revision::Value>(client, service.app.keys(), record.number_id(), now)
            .await?
            .ok_or_else(|| Error::corrupt("a number record retains its immutable value"))?;
    if value.person_id() != record.person_id() {
        return Err(Error::corrupt(
            "a number record and value belong to the same person",
        ));
    }
    Ok(value.revision())
}

pub(super) async fn original(
    service: &Service,
    tx: &Tx,
    kind: &str,
    request: &completion::Request,
    now: DateTime<Utc>,
) -> Result<Verified<record::Record>, Error> {
    let record = record::original(
        tx.client(),
        service.app.keys(),
        kind,
        tx.operation_id(),
        now,
    )
    .await?;
    let identities = service
        .revisions
        .fetch_identities(tx.client(), &[record.operator_id()])
        .await?;
    let operator = identities
        .get(&record.operator_id())
        .ok_or(person::revision::ReadError::Missing)?;
    if record.person_id().to_string() != request.target()
        || operator.person_id() != request.person_id()
    {
        return Err(Error::corrupt(
            "the number result agrees with its authenticated completion",
        ));
    }
    Ok(record)
}

pub(super) async fn apply(
    service: &Service,
    tx: &Tx,
    created: transition::Created,
    pending: &mut Option<history::Pending>,
) -> Result<Revision, Error> {
    let result = created.value.revision;
    history::insert_value::<revision::Value>(tx.client(), service.app.keys(), created.value)
        .await?;
    *pending = Some(
        created
            .change
            .write(tx.client(), service.app.keys(), service.app.anchors())
            .await?,
    );
    Ok(result)
}

fn select_serial(requested: Option<i16>, occupied: &[i16]) -> Result<i16, Rejection> {
    match requested {
        Some(serial) if occupied.contains(&serial) => Err(Rejection::Occupied),
        Some(serial) => Ok(serial),
        None => (1..=999)
            .find(|serial| !occupied.contains(serial))
            .ok_or(Rejection::Exhausted),
    }
}

pub(super) enum Allocation {
    Current(Input),
    Assistance {
        person: person::Id,
        input: Assistance,
    },
    Supplement {
        person: person::Id,
        input: Supplement,
    },
}

impl Allocation {
    fn person(&self, authority: &Authorization) -> person::Id {
        match self {
            Self::Current(_) => authority.person_id,
            Self::Assistance { person, .. } | Self::Supplement { person, .. } => *person,
        }
    }
    fn source(&self) -> Source {
        match self {
            Self::Current(_) => Source::SelfService,
            Self::Assistance { .. } => Source::Assistance,
            Self::Supplement { .. } => Source::Supplement,
        }
    }
}

pub(super) struct Allocate {
    pub(super) service: Service,
    pub(super) actor: Actor,
    pub(super) input: Allocation,
    pub(super) id: Uuid,
    pub(super) number_id: Id,
    pub(super) locked_year: AtomicI32,
}

pub(super) struct AllocationFacts {
    existing: bool,
    occupied: Vec<i16>,
    person: person::Id,
    revision: person::revision::Id,
    operation: transition::Operation,
}

impl Command for Allocate {
    const COMPLETIONS: completion::Template = ALLOCATIONS;
    type Facts = AllocationFacts;
    type Change = transition::Created;
    type Output = Revision;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        let target = self.input.person(authority).to_string();
        match &self.input {
            Allocation::Current(input) => {
                completion::Request::new("self_service", target, input, authority)
            }
            Allocation::Assistance { input, .. } => {
                completion::Request::new("assistance", target, input, authority)
            }
            Allocation::Supplement { input, .. } => {
                completion::Request::new("supplement", target, input, authority)
            }
        }
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        let persons = match &self.input {
            Allocation::Current(_) => vec![],
            Allocation::Assistance { person, .. } | Allocation::Supplement { person, .. } => {
                vec![*person]
            }
        };
        participants(tx, &self.actor, &persons).await?;
        let (subtype, year) = match &self.input {
            Allocation::Current(input) => (
                input.subtype,
                year(tx.now(self.service.app.context()).await?),
            ),
            Allocation::Assistance { input, .. } => (input.subtype, input.year),
            Allocation::Supplement { input, .. } => (input.subtype, input.year),
        };
        pools(tx.client(), vec![(subtype.category(), year)]).await?;
        self.locked_year.store(year, Ordering::Relaxed);
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        if matches!(self.input, Allocation::Current(_)) {
            let subject =
                subject.ok_or_else(|| Error::corrupt("self allocation has a qualified session"))?;
            if year(subject.observed_at()) != self.locked_year.load(Ordering::Relaxed) {
                return Err(Error::lock_set_changed());
            }
            Ok(Authorization {
                person_id: subject.person_id(),
                revision_id: subject.revision_id(),
                now: subject.observed_at(),
            })
        } else {
            manager(&self.service, tx, subject).await
        }
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<AllocationFacts, Error> {
        let person = self.input.person(authority);
        let (subtype, revision) = match &self.input {
            Allocation::Current(input) => {
                member::find_active(tx.client(), &self.service.roles, person, authority.now)
                    .await?
                    .ok_or(Error::Denied)?;
                (input.subtype, authority.revision_id)
            }
            Allocation::Assistance {
                input:
                    Assistance {
                        subtype,
                        target: confirmation,
                        ..
                    },
                ..
            }
            | Allocation::Supplement {
                input:
                    Supplement {
                        subtype,
                        target: confirmation,
                        ..
                    },
                ..
            } => {
                let (account, member) =
                    target(tx.client(), &self.service.roles, person, authority.now).await?;
                if *confirmation.person_id() != person || *confirmation.account_id() != account {
                    return Err(Rejection::TargetChanged.into());
                }
                let identity = self
                    .service
                    .accounts
                    .with_read(Read::Shared)
                    .find_by_id(tx.client(), account, confirmation.mobile(), authority.now)
                    .await?
                    .ok_or(Rejection::TargetChanged)?;
                if identity.person_id() != person
                    || identity.revision_id() != *confirmation.revision_id()
                {
                    return Err(Rejection::TargetChanged.into());
                }
                qualify(
                    tx.client(),
                    &self.service.roles,
                    &self.actor,
                    account,
                    &member,
                    authority.now,
                )
                .await?;
                (*subtype, identity.revision_id())
            }
        };
        let existing = history::find::<State>(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            person,
            authority.now,
        )
        .await?
        .is_some();
        let occupied = state::pool(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            subtype.category(),
            self.locked_year.load(Ordering::Relaxed),
            authority.now,
        )
        .await?
        .into_iter()
        .map(|state| state.revision().number.serial())
        .collect();
        Ok(AllocationFacts {
            existing,
            occupied,
            person,
            revision,
            operation: transition::Operation {
                id: tx.operation_id(),
                operator_id: authority.revision_id,
                time: authority.now,
            },
        })
    }

    fn decide(
        &self,
        facts: AllocationFacts,
        now: DateTime<Utc>,
    ) -> Result<transition::Created, Error> {
        let (subtype, year, serial, reason) = match &self.input {
            Allocation::Current(input) => (input.subtype, year(now), None, None),
            Allocation::Assistance { input, .. } => {
                if input.year >= year(now) {
                    return Err(Rejection::Year.into());
                }
                (input.subtype, input.year, None, Some(input.reason.clone()))
            }
            Allocation::Supplement { input, .. } => {
                if input.year > year(now) {
                    return Err(Rejection::Year.into());
                }
                (
                    input.subtype,
                    input.year,
                    Some(input.serial),
                    Some(input.reason.clone()),
                )
            }
        };
        if facts.existing {
            return Err(Rejection::Conflict.into());
        }
        let serial = select_serial(serial, &facts.occupied)?;
        let number = Number::new(subtype, year, serial).map_err(invalid_number)?;
        Ok(transition::allocate(
            transition::Allocation {
                id: self.id,
                person_id: facts.person,
                revision_id: facts.revision,
                number: Revision {
                    id: self.number_id,
                    number,
                },
                source: self.input.source(),
                reason,
            },
            &facts.operation,
        ))
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: transition::Created,
        pending: &mut Option<history::Pending>,
    ) -> Result<Revision, Error> {
        apply(&self.service, tx, change, pending).await
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Revision, Error> {
        let record = original(&self.service, tx, "allocation", request, now).await?;
        let number = fixed(&self.service, tx.client(), &record, now).await?;
        self.service.app.anchors().confirm::<State>(&record)?;
        Ok(number)
    }
}

fn invalid_number(_: super::ValueError) -> Error {
    Error::corrupt("a number assembled from qualified fields is valid")
}

pub(super) struct Correct {
    pub(super) service: Service,
    pub(super) actor: Actor,
    pub(super) person: person::Id,
    pub(super) input: Correction,
    pub(super) id: Uuid,
    pub(super) number_id: Id,
}

pub(super) struct CorrectionFacts {
    original: Verified<State>,
    occupied: bool,
    operation: transition::Operation,
}

impl Command for Correct {
    const COMPLETIONS: completion::Template = CORRECTIONS;
    type Facts = CorrectionFacts;
    type Change = transition::Created;
    type Output = Revision;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }
    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        completion::Request::new("correct", self.person.to_string(), &self.input, authority)
    }
    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        participants(tx, &self.actor, &[self.person]).await?;
        let mut locations = vec![(
            self.input.replacement.subtype().category(),
            self.input.replacement.year(),
        )];
        if let Some((_, number)) = state::discover(tx.client(), self.person).await? {
            locations.push((number.subtype().category(), number.year()));
        }
        pools(tx.client(), locations).await
    }
    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        manager(&self.service, tx, subject).await
    }
    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<CorrectionFacts, Error> {
        confirm(
            &self.service,
            tx.client(),
            self.person,
            self.input.revision_id,
            authority.now,
        )
        .await?;
        let account = self
            .service
            .accounts
            .with_read(Read::Shared)
            .find_by_person(tx.client(), self.person, authority.now)
            .await?;
        manageable(
            tx.client(),
            &self.service.roles,
            &self.actor,
            self.person,
            account,
            authority.now,
        )
        .await?;
        let original = history::find::<State>(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            self.person,
            authority.now,
        )
        .await?
        .ok_or(Rejection::Absent)?;
        let occupied = if original.held() {
            state::holder(
                tx.client(),
                self.service.app.keys(),
                self.service.app.anchors(),
                Read::Locked,
                self.input.replacement,
                authority.now,
            )
            .await?
            .is_some_and(|state| state.id() != self.person)
        } else {
            false
        };
        Ok(CorrectionFacts {
            original,
            occupied,
            operation: transition::Operation {
                id: tx.operation_id(),
                operator_id: authority.revision_id,
                time: authority.now,
            },
        })
    }
    fn decide(
        &self,
        facts: CorrectionFacts,
        _: DateTime<Utc>,
    ) -> Result<transition::Created, Error> {
        if facts.original.revision().id != self.input.expected {
            return Err(Rejection::TargetChanged.into());
        }
        if facts.original.revision().number == self.input.replacement {
            return Err(Rejection::Unchanged.into());
        }
        if facts.occupied {
            return Err(Rejection::Occupied.into());
        }
        Ok(transition::correct(
            &facts.original,
            transition::Correction {
                id: self.id,
                revision_id: self.input.revision_id,
                number: Revision {
                    id: self.number_id,
                    number: self.input.replacement,
                },
                reason: self.input.reason.clone(),
            },
            &facts.operation,
        )?)
    }
    async fn apply(
        &self,
        tx: &Tx,
        change: transition::Created,
        pending: &mut Option<history::Pending>,
    ) -> Result<Revision, Error> {
        apply(&self.service, tx, change, pending).await
    }
    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Revision, Error> {
        let record = original(&self.service, tx, "correction", request, now).await?;
        let number = fixed(&self.service, tx.client(), &record, now).await?;
        self.service.app.anchors().confirm::<State>(&record)?;
        Ok(number)
    }
}

#[cfg(test)]
mod tests {
    use super::{Rejection, select_serial};

    #[test]
    fn allocation_year_changes_at_china_standard_midnight() {
        for (time, expected) in [
            ("2026-12-31T15:59:59.999999Z", 2026),
            ("2026-12-31T16:00:00Z", 2027),
        ] {
            let time = time
                .parse()
                .unwrap_or_else(|error| panic!("fixed UTC boundary: {error}"));
            assert_eq!(super::year(time), expected);
        }
    }

    #[test]
    fn serial_selection_uses_the_first_gap_and_never_exceeds_the_pool() {
        assert_eq!(select_serial(None, &[]), Ok(1));
        let mut occupied: Vec<i16> = (1..=999).rev().collect();
        assert_eq!(select_serial(None, &occupied), Err(Rejection::Exhausted));
        occupied.retain(|serial| *serial != 7 && *serial != 998);
        assert_eq!(select_serial(None, &occupied), Ok(7));
        assert_eq!(select_serial(Some(998), &occupied), Ok(998));
        assert_eq!(
            select_serial(Some(999), &occupied),
            Err(Rejection::Occupied)
        );
        assert_eq!(select_serial(None, &(1..999).collect::<Vec<_>>()), Ok(999));
    }
}
