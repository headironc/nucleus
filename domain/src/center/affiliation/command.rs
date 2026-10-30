//! First fill and qualified correction share one sealed state transition and executor.

use chrono::{DateTime, Utc};

use crate::{
    access::{self, appointment, role},
    center::affiliation::{
        COMPLETIONS, Error, Id, Rejection, Request, Service, database,
        management::Confirmation,
        profile::{self, Profile},
    },
    history::{self, Change, Draft, Read, Verified},
    identity::{
        account,
        current::{self, Actor},
        person,
    },
    kernel::{Authorization, Command, Fault, FaultKind, Stage, completion, tx::Tx},
};

pub(super) struct Write {
    pub(super) service: Service,
    pub(super) actor: Actor,
    pub(super) request: Request,
    pub(super) id: Id,
}

pub(super) struct Facts {
    profile: Option<Verified<Profile>>,
    target_confirmed: bool,
    person_id: person::Id,
    record: database::Data,
}

impl Write {
    fn person_id(&self, authority: &Authorization) -> person::Id {
        match &self.request {
            Request::Fill(_) => authority.person_id,
            Request::Correct { input } => input.target.person_id(),
        }
    }
}

impl Command for Write {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Change<Profile>;
    type Output = ();
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        #[derive(serde::Serialize)]
        struct Input {
            affiliation: super::Code,
        }
        completion::Request::new(
            match self.request {
                Request::Fill(_) => "fill",
                Request::Correct { .. } => "correct",
            },
            self.person_id(authority).to_string(),
            &Input {
                affiliation: self.request.code(),
            },
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        match &self.request {
            Request::Fill(_) => database::lock(tx.client(), &self.actor).await,
            Request::Correct { input } => {
                tx.shared(b"access/scope", b"institution").await?;
                let target = input.target.person_id();
                match account::state::discover_person(tx.client(), target).await? {
                    Some(account) => {
                        database::lock_accounts(tx.client(), self.actor.account_id(), account)
                            .await?
                    }
                    None => self.actor.lock_account(tx.client()).await?,
                }
                database::lock_person(tx.client(), target).await
            }
        }?;
        Ok(self.actor.lock_session(tx.client()).await?)
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject =
            subject.ok_or_else(|| Error::corrupt("a Center command has a qualified session"))?;
        let (person_id, revision_id, now) = match &self.request {
            Request::Fill(_) => (
                subject.person_id(),
                subject.revision_id(),
                subject.observed_at(),
            ),
            Request::Correct { input } => {
                let (role, person, revision, now) = access::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    role::Code::InstitutionAffiliationsCorrect,
                )
                .await?;
                let target = input.target.person_id();
                let target_level = match account::state::located(
                    tx.client(),
                    self.service.roles.keys(),
                    self.service.roles.anchors(),
                    self.service.roles.read()?,
                    account::state::Location::Person(target),
                    now,
                )
                .await?
                .filter(|account| account.current().is_some())
                .map(|account| account.id())
                {
                    Some(account) => Some(
                        appointment::find_by_account(
                            tx.client(),
                            &self.service.roles,
                            account,
                            now,
                        )
                        .await?
                        .ok_or(Error::corrupt(
                            "a target account retains its institutional assignment",
                        ))?
                        .role()
                        .level(),
                    ),
                    None => None,
                };
                qualify(person == target, role.level(), target_level)?;
                (person, revision, now)
            }
        };
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let person_id = self.person_id(authority);
        let target_confirmed = match &self.request {
            Request::Fill(_) => true,
            Request::Correct { input } => {
                let identity = self
                    .service
                    .persons
                    .with_read(self.service.roles.read()?)
                    .find_identity(tx.client(), person_id, authority.now)
                    .await?;
                let account = account::state::located(
                    tx.client(),
                    self.service.roles.keys(),
                    self.service.roles.anchors(),
                    self.service.roles.read()?,
                    account::state::Location::Person(person_id),
                    authority.now,
                )
                .await?
                .filter(|account| account.current().is_some())
                .map(|account| account.id());
                let identity_matches = identity
                    .is_some_and(|identity| identity.revision_id() == input.target.revision_id());
                let account_matches = match (&input.target, account) {
                    (
                        Confirmation::Account {
                            account_id, mobile, ..
                        },
                        Some(current),
                    ) if *account_id == current => match mobile {
                        Some(mobile) => self
                            .service
                            .accounts
                            .find_by_id(tx.client(), current, mobile, authority.now)
                            .await?
                            .is_some(),
                        None => true,
                    },
                    (Confirmation::Person { .. }, None) => true,
                    _ => false,
                };
                identity_matches && account_matches
            }
        };
        let profile = history::find(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            person_id,
            authority.now,
        )
        .await?;
        Ok(Facts {
            profile,
            target_confirmed,
            person_id,
            record: database::Data {
                id: self.id,
                code: self.request.code(),
                operator_id: authority.revision_id,
                #[cfg(test)]
                operator_person: authority.person_id,
                created_at: authority.now,
                operation_id: tx.operation_id(),
            },
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Change<Profile>, Error> {
        transition(facts, matches!(self.request, Request::Fill(_)), now)
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: Change<Profile>,
        pending: &mut Option<crate::history::Pending>,
    ) -> Result<(), Error> {
        *pending = Some(
            change
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await?,
        );
        Ok(())
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        let record = history::original::<database::Record>(
            tx.client(),
            self.service.app.keys(),
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "an affiliation completion retains its original record",
        ))?;
        let operator = self
            .service
            .revisions
            .fetch_identity(tx.client(), record.operator_id())
            .await?;
        if record.person_id().to_string() != request.target()
            || operator.person_id() != request.person_id()
            || record.code() != self.request.code()
        {
            return Err(Error::corrupt(
                "the original affiliation agrees with its authenticated completion",
            ));
        }
        self.service.app.anchors().confirm::<Profile>(&record)?;
        Ok(())
    }
}

fn qualify(
    same_person: bool,
    level: role::Level,
    target_level: Option<role::Level>,
) -> Result<(), Rejection> {
    if same_person || target_level.is_some_and(|target| target >= level) {
        return Err(Rejection::Absent);
    }
    Ok(())
}

fn transition(facts: Facts, fill: bool, now: DateTime<Utc>) -> Result<Change<Profile>, Error> {
    if !facts.target_confirmed {
        return Err(Rejection::TargetChanged.into());
    }
    if fill && facts.profile.is_some() {
        return Err(Rejection::Conflict.into());
    }
    advance(facts.profile, facts.person_id, facts.record, now)
}

/// First fill and both correction paths share the strict time and state advance.
pub(super) fn advance<R>(
    profile: Option<Verified<Profile>>,
    person_id: person::Id,
    record: database::Data,
    now: DateTime<Utc>,
) -> Result<Change<Profile>, crate::kernel::Error<R>> {
    let data = profile::Data {
        person_id,
        affiliation_id: record.id,
        created_at: now,
    };
    let draft = match &profile {
        None => Draft::create(data),
        Some(profile) => {
            if now <= profile.created_at() {
                return Err(crate::kernel::Error::Unavailable(Fault::new(
                    Stage::Time,
                    FaultKind::Integrity("affiliation time strictly advances the previous value"),
                )));
            }
            Draft::advance(profile, data)?
        }
    };
    Ok(Change::new(draft, record))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use chrono::{DateTime, Utc};

    use super::{Facts, qualify, transition};
    use crate::{
        access::role,
        center::affiliation::{
            Code, Error, Id, Rejection, database,
            profile::{self, Profile},
        },
        history::{self, Seed, Verified},
        identity::{authentication, operation, person},
        kernel::{FaultKind, Stage},
    };

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 123_000).expect("fixed time")
    }
    fn operation() -> operation::Id {
        uuid::Uuid::new_v4().to_string().parse().expect("operation")
    }
    fn state(person_id: person::Id, sequence: i64, version: i64) -> Verified<Profile> {
        let inventory = authentication::inventory(authentication::keys());
        Seed::state(
            profile::Data {
                person_id,
                affiliation_id: Id::allocate(),
                created_at: at(),
            },
            sequence,
            version,
            history::Keys {
                data: &inventory.encryption,
                seals: &inventory.seal,
            },
        )
        .expect("verified pure-test state")
    }
    fn facts(
        person_id: person::Id,
        profile: Option<Verified<Profile>>,
        confirmed: bool,
        now: DateTime<Utc>,
    ) -> Facts {
        Facts {
            profile,
            target_confirmed: confirmed,
            person_id,
            record: database::Data {
                id: Id::allocate(),
                code: Code::CommunistParty,
                operator_id: person::revision::Id::allocate(),
                #[cfg(test)]
                operator_person: person_id,
                created_at: now,
                operation_id: operation(),
            },
        }
    }

    #[test]
    fn first_fill_correction_and_target_rejections_are_pure() {
        let person = person::Id::allocate();
        let later = at() + chrono::Duration::microseconds(1);
        assert!(transition(facts(person, None, true, at()), true, at()).is_ok());
        assert!(transition(facts(person, None, true, at()), false, at()).is_ok());
        assert!(
            transition(
                facts(person, Some(state(person, 1, 1)), true, later),
                false,
                later
            )
            .is_ok(),
            "same-value correction still appends"
        );
        assert!(matches!(
            transition(
                facts(person, Some(state(person, 1, 1)), true, later),
                true,
                later
            ),
            Err(Error::Rejected(Rejection::Conflict))
        ));
        for filled in [false, true] {
            assert!(
                matches!(
                    transition(
                        facts(person, filled.then(|| state(person, 1, 1)), false, later),
                        filled,
                        later
                    ),
                    Err(Error::Rejected(Rejection::TargetChanged))
                ),
                "stale target confirmation precedes state conflict"
            );
        }
    }

    #[test]
    fn corrections_reject_equal_or_earlier_times_and_counter_overflow() {
        let person = person::Id::allocate();
        for now in [at(), at() - chrono::Duration::microseconds(1)] {
            let error = transition(
                facts(person, Some(state(person, 1, 1)), true, now),
                false,
                now,
            )
            .err()
            .expect("time does not advance");
            assert!(
                matches!(error, Error::Unavailable(fault) if fault.stage()==Stage::Time && matches!(fault.kind(), FaultKind::Integrity("affiliation time strictly advances the previous value")))
            );
        }
        let now = at() + chrono::Duration::microseconds(1);
        for (sequence, version) in [(i64::MAX, 1), (1, i64::MAX)] {
            assert!(
                matches!(
                    transition(
                        facts(person, Some(state(person, sequence, version)), true, now),
                        false,
                        now
                    ),
                    Err(Error::Corrupt(_))
                ),
                "no wrapped counter can be persisted"
            );
        }
    }

    #[test]
    fn correction_requires_another_person_with_a_strictly_lower_account_level() {
        let level = role::Level::try_from(20).expect("operator level");
        for (target, permitted) in [
            (None, true),
            (Some(10), true),
            (Some(20), false),
            (Some(30), false),
        ] {
            let target = target.map(|value| role::Level::try_from(value).expect("target level"));
            assert_eq!(qualify(false, level, target).is_ok(), permitted);
            assert!(
                matches!(qualify(true, level, target), Err(Rejection::Absent)),
                "self is always concealed"
            );
        }
    }
}
