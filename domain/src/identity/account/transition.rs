//! Pure account transitions preserve current occupancy and fix their session consequences.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::{
    history::{Change, Draft, Error, Verified},
    identity::{
        account::{
            record::{self, Confirmation, Event},
            state::{State, Status},
        },
        cancellation, correction,
        mobile::{Mobile, envelope::Envelope},
        operation,
        person::{number::hash::Hash, revision},
        revocation::Scope,
        session::{self, state::Cause},
        suspension,
    },
};

/// The immutable account event and the exact newly revoked sessions are one transaction.
pub(crate) struct Revocation {
    pub(crate) account: Change<State>,
    pub(crate) sessions: Vec<(session::Id, Change<session::state::State>)>,
}

pub(crate) fn suspend(
    original: &Verified<State>,
    id: suspension::Id,
    confirmation: Confirmation,
    sessions: &[Verified<session::state::State>],
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Revocation, Error> {
    let mut data = current(original, now)?;
    let Status::Current(current) = &mut data.status else {
        return Err(Error::Integrity("only a current account can be suspended"));
    };
    if current.suspension_id.is_some() {
        return Err(Error::Integrity(
            "a new suspension starts from an unsuspended account",
        ));
    }
    current.suspension_id = Some(id);
    let sessions = revocations(
        original,
        sessions,
        operation_id,
        Cause::Suspension(id),
        None,
        now,
    )?;
    Ok(Revocation {
        account: change(
            original,
            data,
            Event::Suspension {
                id,
                confirmation,
                sessions: sessions.iter().map(|(id, _)| *id).collect(),
            },
            operation_id,
            now,
        )?,
        sessions,
    })
}

pub(crate) fn resume(
    original: &Verified<State>,
    id: suspension::Id,
    confirmation: Confirmation,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    let mut data = current(original, now)?;
    let Status::Current(current) = &mut data.status else {
        return Err(Error::Integrity("only a current account can be resumed"));
    };
    if current.suspension_id != Some(id) {
        return Err(Error::Integrity(
            "a resumption names the account's current suspension",
        ));
    }
    current.suspension_id = None;
    change(
        original,
        data,
        Event::Resumption { id, confirmation },
        operation_id,
        now,
    )
}

pub(crate) fn logout(
    original: &Verified<State>,
    initiator_id: session::Id,
    scope: Scope,
    sessions: &[Verified<session::state::State>],
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Revocation, Error> {
    let data = current(original, now)?;
    if original
        .current()
        .is_none_or(|current| current.suspension_id().is_some())
    {
        return Err(Error::Integrity("logout requires an unsuspended account"));
    }
    let Some(initiator) = sessions.iter().find(|session| session.id() == initiator_id) else {
        return Err(Error::Integrity(
            "logout includes its original authenticated session",
        ));
    };
    if !initiator.available(now) {
        return Err(Error::Integrity(
            "logout starts from a currently usable original session",
        ));
    }
    let only = match scope {
        Scope::All => None,
        Scope::Current => Some(initiator_id),
        Scope::Other(id) => {
            if id == initiator_id
                || !sessions
                    .iter()
                    .any(|session| session.id() == id && session.available(now))
            {
                return Err(Error::Integrity(
                    "other logout selects one effective other session",
                ));
            }
            Some(id)
        }
    };
    let sessions = revocations(
        original,
        sessions,
        operation_id,
        Cause::Logout(operation_id),
        only,
        now,
    )?;
    Ok(Revocation {
        account: change(
            original,
            data,
            Event::Logout {
                initiator_id,
                scope,
                sessions: sessions.iter().map(|(id, _)| *id).collect(),
            },
            operation_id,
            now,
        )?,
        sessions,
    })
}

pub(crate) fn correct(
    original: &Verified<State>,
    id: correction::Id,
    number: Hash,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    let mut data = current(original, now)?;
    let Status::Current(current) = &mut data.status else {
        return Err(Error::Integrity(
            "a correction projects only onto a current account",
        ));
    };
    current.number = Arc::new(number);
    change(original, data, Event::Correction(id), operation_id, now)
}

pub(crate) fn cancel(
    original: &Verified<State>,
    id: cancellation::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    let mut data = current(original, now)?;
    data.status = Status::Cancelled(id);
    change(original, data, Event::Cancellation(id), operation_id, now)
}

/// Advance the account's avatar revision only after the replacement facts are fixed.
pub(crate) fn avatar(
    original: &Verified<State>,
    id: super::portrait::Id,
    previous: crate::identity::avatar::Revision,
    revision: crate::identity::avatar::Revision,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    let mut data = current(original, now)?;
    if original.avatar() != previous || revision == previous {
        return Err(Error::Integrity(
            "an avatar advancement names its previous and changed revision",
        ));
    }
    data.avatar = revision;
    change(
        original,
        data,
        Event::Avatar {
            id,
            previous,
            revision,
        },
        operation_id,
        now,
    )
}

pub(crate) fn confirmation(
    revision_id: revision::Id,
    operator_id: revision::Id,
    mobile: Mobile,
) -> Confirmation {
    Confirmation {
        revision_id,
        operator_id,
        mobile: Arc::new(mobile),
    }
}

/// The caller has already verified the code and the exact original transfer inputs.
pub(crate) fn replace(
    original: &Verified<State>,
    mobile: Envelope,
    keys: &crate::kernel::keys::Keys,
    event: Event,
    sessions: &[Verified<session::state::State>],
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Revocation, Error> {
    let mut data = current(original, now)?;
    let Status::Current(current) = &mut data.status else {
        return Err(Error::Integrity("a replacement retains a current account"));
    };
    let Event::Replacement {
        id,
        operator_id,
        session_id,
        message_id,
        ..
    } = event
    else {
        return Err(Error::Integrity(
            "a mobile replacement has its own permanent event",
        ));
    };
    if current.mobile.hash().as_ref() == mobile.hash().as_ref() {
        return Err(Error::Integrity("a replacement changes the current mobile"));
    }
    current.plaintext = super::state::open_mobile(&mobile, keys, original.id())?;
    current.mobile = Arc::new(mobile);
    let sessions = revocations(
        original,
        sessions,
        operation_id,
        Cause::Transfer(id),
        None,
        now,
    )?;
    Ok(Revocation {
        account: change(
            original,
            data,
            Event::Replacement {
                id,
                operator_id,
                session_id,
                message_id,
                sessions: sessions.iter().map(|(id, _)| *id).collect(),
            },
            operation_id,
            now,
        )?,
        sessions,
    })
}

fn current(original: &Verified<State>, now: DateTime<Utc>) -> Result<super::state::Data, Error> {
    if original.current().is_none() || original.changed_at() > now {
        return Err(Error::Integrity(
            "an account transition follows its current live state",
        ));
    }
    Ok(original.data())
}

fn change(
    original: &Verified<State>,
    mut data: super::state::Data,
    event: Event,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    data.changed_at = now;
    Ok(Change::new(
        Draft::advance(original, data)?,
        record::Data {
            person_id: original.person_id(),
            operation_id,
            happened_at: now,
            event,
        },
    ))
}

fn revocations(
    original: &Verified<State>,
    sessions: &[Verified<session::state::State>],
    operation_id: operation::Id,
    cause: Cause,
    only: Option<session::Id>,
    now: DateTime<Utc>,
) -> Result<Vec<(session::Id, Change<session::state::State>)>, Error> {
    let mut ordered: Vec<_> = sessions.iter().collect();
    ordered.sort_by_key(|session| *session.id().as_bytes());
    if ordered.windows(2).any(|pair| pair[0].id() == pair[1].id())
        || ordered
            .iter()
            .any(|session| session.account_id() != original.id())
    {
        return Err(Error::Integrity(
            "the fixed session set has unique members of this account",
        ));
    }
    let mut changes = Vec::new();
    for session in ordered {
        if session.revoked_at().is_none() && only.is_none_or(|id| id == session.id()) {
            changes.push((
                session.id(),
                session::state::revoke(session, operation_id, cause, now)?,
            ));
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::sync::Arc;

    use chrono::{DateTime, TimeDelta, Utc};
    use tokio_postgres::types::{FromSql, Type};

    use crate::{
        history::{self, Kind, Locator, Record as _, Seed, State as _, Verified},
        identity::{
            account::{
                self,
                record::{Confirmation, Event},
                state::{Current, Data, State, Status},
                transition::{cancel, confirmation, correct, logout, resume, suspend},
            },
            authentication,
            mobile::envelope::{Binding, Envelope},
            revocation::Scope,
            session::{self, state::Cause},
            suspension,
        },
        keys::Keys,
    };

    fn id<T: for<'a> FromSql<'a>>(number: u8) -> T {
        let mut bytes = [0; 16];
        bytes[6] = 0x40;
        bytes[8] = 0x80;
        bytes[15] = number;
        T::from_sql(&Type::UUID, &bytes).expect("fixed UUID fixture")
    }

    fn created_at() -> DateTime<Utc> {
        DateTime::from_timestamp_micros(1_700_000_000_123_000).expect("fixture creation")
    }

    fn now() -> DateTime<Utc> {
        created_at() + TimeDelta::hours(2)
    }

    fn keys(inventory: &Keys) -> history::Keys<'_> {
        history::Keys {
            data: &inventory.encryption,
            seals: &inventory.seal,
        }
    }

    fn account(inventory: &Keys, suspension_id: Option<suspension::Id>) -> Verified<State> {
        let mobile = Envelope::seal(
            &inventory.identity.encryption,
            &inventory.identity.mobile,
            Binding::Account(id(10)),
            &"13800000000".parse().expect("fixture mobile"),
        )
        .expect("mobile envelope");
        let number = inventory
            .identity
            .number
            .hash(
                &"000000200002290010"
                    .parse()
                    .expect("fictitious identity number"),
            )
            .expect("number digest");
        let sequence = if suspension_id.is_some() { 2 } else { 1 };
        Seed::state::<State>(
            Data {
                id: id(10),
                person_id: id(11),
                status: Status::Current(Current {
                    plaintext: Arc::new("13800000000".parse().expect("fixture mobile")),
                    mobile: Arc::new(mobile),
                    number: Arc::new(number),
                    suspension_id,
                }),
                avatar: id(12),
                wechat: id(13),
                created_at: created_at(),
                changed_at: if suspension_id.is_some() {
                    created_at() + TimeDelta::minutes(30)
                } else {
                    created_at()
                },
            },
            sequence,
            sequence,
            keys(inventory),
        )
        .expect("sealed account fixture")
    }

    fn confirmation_input() -> Confirmation {
        confirmation(
            id(20),
            id(21),
            "13800000000".parse().expect("confirmed mobile"),
        )
    }

    fn session(
        inventory: &Keys,
        number: u8,
        account_id: account::Id,
        expires_at: DateTime<Utc>,
        revoked_at: Option<DateTime<Utc>>,
    ) -> Verified<session::state::State> {
        let change = session::state::create(
            session::state::Input {
                id: id(number),
                account_id,
                authentication_id: id(number + 30),
                receipt_id: Some(id(number + 60)),
                hash: [number; 32],
                origin: crate::identity::session::Origin::WebsiteMessage(
                    "website-fixture".parse().expect("fixture app"),
                ),
                expires_at,
            },
            id(80),
            created_at(),
        )
        .expect("session issuance");
        let (state, _) = Seed::change(change, keys(inventory)).expect("issued fixture");
        if let Some(at) = revoked_at {
            let change = session::state::revoke(&state, id(81), Cause::Logout(id(81)), at)
                .expect("earlier logout");
            Seed::change(change, keys(inventory))
                .expect("revoked fixture")
                .0
        } else {
            state
        }
    }

    fn assert_identity_preserved(before: &State, after: &State, at: DateTime<Utc>) {
        assert_eq!(after.id(), before.id());
        assert_eq!(after.person_id(), before.person_id());
        assert_eq!(after.avatar(), before.avatar());
        assert_eq!(after.wechat(), before.wechat());
        assert_eq!(after.created_at(), before.created_at());
        assert_eq!(after.changed_at(), at);
        assert_eq!(after.sequence(), before.sequence() + 1);
        assert_eq!(after.version(), before.version() + 1);
    }

    fn assert_mobile_preserved(before: &State, after: &State) {
        let before = before.current().expect("previous current account");
        let after = after.current().expect("next current account");
        assert!(before.mobile().ciphertext().as_ref() == after.mobile().ciphertext().as_ref());
        assert!(before.mobile().hash().as_ref() == after.mobile().hash().as_ref());
    }

    #[test]
    fn suspension_fixes_every_unrevoked_session_including_expired_members() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, None);
        let active = session(
            &inventory,
            1,
            original.id(),
            now() + TimeDelta::hours(1),
            None,
        );
        let expired = session(
            &inventory,
            2,
            original.id(),
            now() - TimeDelta::hours(1),
            None,
        );
        let prior_at = created_at() + TimeDelta::minutes(15);
        let prior = session(
            &inventory,
            3,
            original.id(),
            now() + TimeDelta::hours(1),
            Some(prior_at),
        );
        let prior_fields = prior.fields();
        let sessions = [prior.clone(), expired.clone(), active.clone()];
        let result = suspend(
            &original,
            id(7),
            confirmation_input(),
            &sessions,
            id(90),
            now(),
        )
        .expect("suspend retained sessions");
        assert!(
            result
                .sessions
                .iter()
                .map(|(id, _)| *id)
                .eq([active.id(), expired.id()])
        );
        let (state, record) =
            Seed::change(result.account, keys(&inventory)).expect("account change");
        assert_identity_preserved(&original, &state, now());
        assert_mobile_preserved(&original, &state);
        assert_eq!(
            state.current().expect("current account").suspension_id(),
            Some(id(7))
        );
        assert!(state.has_head(&record));
        assert_eq!(record.operation_id(), id(90));
        assert!(
            matches!(record.event(), Event::Suspension { id: actual, sessions, .. }
            if *actual == id(7) && sessions.as_slice() == [active.id(), expired.id()])
        );
        for ((_, change), before) in result.sessions.into_iter().zip([&active, &expired]) {
            let (after, record) = Seed::change(change, keys(&inventory)).expect("session change");
            assert!(after.fields()[..9] == before.fields()[..9]);
            assert_eq!(after.revoked_at(), Some(now()));
            assert_eq!((after.sequence(), after.version()), (2, 2));
            assert!(!after.available(now()));
            assert!(after.has_head(&record));
            assert!(record.cause() == Some(Cause::Suspension(id(7))));
            assert_eq!(record.operation_id(), id(90));
        }
        assert_eq!(prior.revoked_at(), Some(prior_at));
        assert!(prior.fields() == prior_fields);
    }

    #[test]
    fn suspension_accepts_an_empty_set_and_rejects_an_already_suspended_account() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, None);
        let result = suspend(&original, id(7), confirmation_input(), &[], id(90), now())
            .expect("zero-session suspension");
        assert!(result.sessions.is_empty());
        let (state, record) = Seed::change(result.account, keys(&inventory)).expect("suspended");
        assert!(
            matches!(record.event(), Event::Suspension { sessions, .. } if sessions.is_empty())
        );
        assert!(state.has_head(&record));
        assert!(suspend(&state, id(8), confirmation_input(), &[], id(91), now()).is_err());
    }

    #[test]
    fn resumption_clears_only_its_current_suspension_and_never_revives_sessions() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, Some(id(7)));
        let old_session = session(
            &inventory,
            1,
            original.id(),
            now() + TimeDelta::hours(1),
            Some(created_at() + TimeDelta::minutes(30)),
        );
        assert!(resume(&original, id(8), confirmation_input(), id(90), now()).is_err());
        let change = resume(&original, id(7), confirmation_input(), id(90), now())
            .expect("resume current suspension");
        let (resumed, record) = Seed::change(change, keys(&inventory)).expect("resumption");
        assert_identity_preserved(&original, &resumed, now());
        assert_mobile_preserved(&original, &resumed);
        assert!(
            original.current().expect("paused").number()
                == resumed.current().expect("current").number()
        );
        assert_eq!(resumed.current().expect("current").suspension_id(), None);
        assert!(matches!(record.event(), Event::Resumption { id: actual, .. } if *actual == id(7)));
        assert!(resumed.has_head(&record));
        assert!(!old_session.available(now()));
        let later = now();
        let result = suspend(&resumed, id(8), confirmation_input(), &[], id(91), later)
            .expect("later suspension");
        let (paused, _) =
            Seed::change(result.account, keys(&inventory)).expect("later suspended state");
        assert!(resume(&paused, id(7), confirmation_input(), id(92), later).is_err());
        assert_eq!(
            paused.current().expect("still paused").suspension_id(),
            Some(id(8))
        );
        assert!(!old_session.available(later));
    }

    #[test]
    fn equal_time_suspensions_keep_their_original_session_sets() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, None);
        let old = session(
            &inventory,
            1,
            original.id(),
            now() + TimeDelta::hours(1),
            None,
        );
        let first = suspend(
            &original,
            id(7),
            confirmation_input(),
            &[old],
            id(90),
            now(),
        )
        .expect("first suspension");
        let (paused, first_record) =
            Seed::change(first.account, keys(&inventory)).expect("first suspended account");
        let (_, old_change) = first
            .sessions
            .into_iter()
            .next()
            .expect("old session revoked");
        let (old, old_record) =
            Seed::change(old_change, keys(&inventory)).expect("first session revocation");
        let resumed = resume(&paused, id(7), confirmation_input(), id(91), now())
            .expect("resumption may share the suspension timestamp");
        let (resumed, _) = Seed::change(resumed, keys(&inventory)).expect("resumed account");
        let new = session::state::create(
            session::state::Input {
                id: id(2),
                account_id: resumed.id(),
                authentication_id: id(32),
                receipt_id: Some(id(62)),
                hash: [2; 32],
                origin: crate::identity::session::Origin::WebsiteMessage(
                    "website-fixture".parse().expect("app"),
                ),
                expires_at: now() + TimeDelta::hours(1),
            },
            id(92),
            now(),
        )
        .expect("new session issued at the same timestamp");
        let (new, _) = Seed::change(new, keys(&inventory)).expect("new current session");
        assert!(new.available(now()));
        let second = suspend(
            &resumed,
            id(8),
            confirmation_input(),
            &[old.clone(), new.clone()],
            id(93),
            now(),
        )
        .expect("second suspension at the same timestamp");
        assert!(second.sessions.iter().map(|(id, _)| *id).eq([new.id()]));
        let (paused, second_record) =
            Seed::change(second.account, keys(&inventory)).expect("second suspended account");
        assert!(matches!(first_record.event(),
            Event::Suspension { id: actual, sessions, .. }
            if *actual == id(7) && sessions.as_slice() == [old.id()]));
        assert!(matches!(second_record.event(),
            Event::Suspension { id: actual, sessions, .. }
            if *actual == id(8) && sessions.as_slice() == [new.id()]));
        assert!(old_record.cause() == Some(Cause::Suspension(id(7))));
        assert!(old.has_head(&old_record));
        assert!(!old.available(now()));
        assert!(resume(&paused, id(7), confirmation_input(), id(94), now()).is_err());
        assert_eq!(
            paused.current().expect("current account").suspension_id(),
            Some(id(8))
        );
    }

    #[test]
    fn logout_current_and_all_record_their_exact_newly_revoked_sets() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, None);
        let initiator = session(
            &inventory,
            2,
            original.id(),
            now() + TimeDelta::hours(1),
            None,
        );
        let expired = session(
            &inventory,
            1,
            original.id(),
            now() - TimeDelta::hours(1),
            None,
        );
        let prior = session(
            &inventory,
            3,
            original.id(),
            now() + TimeDelta::hours(1),
            Some(created_at() + TimeDelta::minutes(15)),
        );
        let sessions = [prior.clone(), initiator.clone(), expired.clone()];
        for (scope, expected) in [
            (Scope::Current, vec![initiator.id()]),
            (Scope::All, vec![expired.id(), initiator.id()]),
        ] {
            let result =
                logout(&original, initiator.id(), scope, &sessions, id(90), now()).expect("logout");
            assert!(
                result
                    .sessions
                    .iter()
                    .map(|(id, _)| *id)
                    .eq(expected.iter().copied())
            );
            let (state, record) =
                Seed::change(result.account, keys(&inventory)).expect("logout account");
            assert_identity_preserved(&original, &state, now());
            assert_mobile_preserved(&original, &state);
            assert!(state.locators() == original.locators());
            assert!(state.has_head(&record));
            assert!(
                matches!(record.event(), Event::Logout { initiator_id, scope: actual, sessions }
                if *initiator_id == initiator.id() && actual.as_ref() == scope.as_ref() && sessions == &expected)
            );
            for (_, change) in result.sessions {
                let (state, record) =
                    Seed::change(change, keys(&inventory)).expect("logout session");
                assert_eq!(state.revoked_at(), Some(now()));
                assert!(state.has_head(&record));
                assert!(record.cause() == Some(Cause::Logout(id(90))));
            }
        }
        assert!(prior.revoked_at().is_some());
    }

    #[test]
    fn both_logout_scopes_reject_foreign_or_duplicate_input_and_invalid_initiators() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, None);
        let initiator = session(
            &inventory,
            1,
            original.id(),
            now() + TimeDelta::hours(1),
            None,
        );
        let prior = session(
            &inventory,
            2,
            original.id(),
            now() + TimeDelta::hours(1),
            Some(created_at() + TimeDelta::minutes(15)),
        );
        let foreign = session(&inventory, 3, id(99), now() + TimeDelta::hours(1), None);
        let foreign_prior = session(
            &inventory,
            4,
            id(99),
            now() + TimeDelta::hours(1),
            Some(created_at() + TimeDelta::minutes(15)),
        );
        let expired = session(&inventory, 5, original.id(), now(), None);
        let paused = account(&inventory, Some(id(7)));
        for scope in [Scope::Current, Scope::All] {
            for sessions in [
                vec![initiator.clone(), initiator.clone()],
                vec![initiator.clone(), foreign.clone()],
                vec![initiator.clone(), prior.clone(), prior.clone()],
                vec![initiator.clone(), foreign_prior.clone()],
            ] {
                assert!(
                    logout(&original, initiator.id(), scope, &sessions, id(90), now()).is_err()
                );
            }
            assert!(logout(&original, initiator.id(), scope, &[], id(90), now()).is_err());
            assert!(
                logout(
                    &original,
                    expired.id(),
                    scope,
                    std::slice::from_ref(&expired),
                    id(90),
                    now()
                )
                .is_err()
            );
            assert!(
                logout(
                    &original,
                    prior.id(),
                    scope,
                    std::slice::from_ref(&prior),
                    id(90),
                    now()
                )
                .is_err()
            );
            assert!(
                logout(
                    &paused,
                    initiator.id(),
                    scope,
                    std::slice::from_ref(&initiator),
                    id(90),
                    now()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cancellation_keeps_a_permanent_identity_but_releases_both_occupancies() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, Some(id(7)));
        assert_eq!(original.locators().len(), 3);
        let change = cancel(&original, id(9), id(90), now()).expect("cancel paused account");
        let (state, record) = Seed::change(change, keys(&inventory)).expect("tombstone");
        assert_identity_preserved(&original, &state, now());
        assert!(state.current().is_none());
        assert_eq!(state.cancellation_id(), Some(id(9)));
        assert!(
            state.locators()
                == vec![Locator::new::<State>(
                    "person",
                    original.person_id().as_bytes()
                )]
        );
        assert!(matches!(record.event(), Event::Cancellation(actual) if *actual == id(9)));
        assert!(state.has_head(&record));
        assert!(cancel(&state, id(8), id(91), now()).is_err());
        assert!(resume(&state, id(7), confirmation_input(), id(91), now()).is_err());
        assert!(suspend(&state, id(8), confirmation_input(), &[], id(91), now()).is_err());
    }

    #[test]
    fn correction_changes_only_the_number_while_preserving_mobile_and_suspension() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, Some(id(7)));
        let number = inventory
            .identity
            .number
            .hash(
                &"00000020000229007X"
                    .parse()
                    .expect("different fictitious number"),
            )
            .expect("new number hash");
        let expected = *number.as_ref();
        let old = original.current().expect("current");
        assert!(old.number().as_ref() != &expected);
        let change =
            correct(&original, id(9), number, id(90), now()).expect("correction while paused");
        let (state, record) = Seed::change(change, keys(&inventory)).expect("corrected state");
        assert_identity_preserved(&original, &state, now());
        assert_mobile_preserved(&original, &state);
        let current = state.current().expect("current");
        assert!(current.number().as_ref() == &expected);
        assert_eq!(current.suspension_id(), Some(id(7)));
        assert!(
            state.locators()
                == vec![
                    Locator::new::<State>("person", state.person_id().as_bytes()),
                    Locator::new::<State>("mobile", old.mobile().hash().as_ref()),
                    Locator::new::<State>("number", &expected),
                ]
        );
        assert!(matches!(record.event(), Event::Correction(actual) if *actual == id(9)));
        assert!(state.has_head(&record));
    }

    #[test]
    fn transitions_reject_a_time_before_the_verified_current_head() {
        let inventory = authentication::inventory(authentication::keys());
        let original = account(&inventory, None);
        let paused = account(&inventory, Some(id(7)));
        let at = original.changed_at() - TimeDelta::microseconds(1);
        assert!(suspend(&original, id(7), confirmation_input(), &[], id(90), at).is_err());
        assert!(resume(&paused, id(7), confirmation_input(), id(90), at).is_err());
        assert!(cancel(&original, id(9), id(90), at).is_err());
        let number = inventory
            .identity
            .number
            .hash(&"00000020000229007X".parse().expect("fictitious number"))
            .expect("number hash");
        assert!(correct(&original, id(9), number, id(90), at).is_err());
    }
}
