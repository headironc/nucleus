//! Pure Person transitions. They read verified state and complete normalized inputs only.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::{
    access::appointment,
    history::{Change, Draft, Error, Verified},
    identity::{
        account, cancellation,
        mobile::Mobile,
        operation,
        person::{self, record, revision, state::State},
    },
};

/// A correction changes the current real-name revision. Equal times are valid because the
/// object-local sequence orders them; an earlier time is not a legal transition.
pub(crate) fn correct(
    state: &Verified<State>,
    before: revision::Id,
    after: revision::Id,
    operator: revision::Id,
    appointment: Option<appointment::Id>,
    operation: operation::Id,
    at: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if before != state.revision_id() || after == before {
        return Err(Error::Integrity(
            "a correction advances from the current real-name revision",
        ));
    }
    if at < state.changed_at() {
        return Err(Error::Integrity(
            "a person transition does not precede the current state",
        ));
    }
    let draft = Draft::advance(
        state,
        person::state::Data {
            id: state.id(),
            revision_id: after,
            changed_at: at,
        },
    )?;
    Ok(Change::new(
        draft,
        record::Data {
            operation_id: operation,
            happened_at: at,
            event: record::Event::Correction {
                id: crate::identity::correction::Id::from_revision(after),
                revision_id: after,
                before_id: before,
                operator_id: operator,
                appointment_id: appointment,
            },
        },
    ))
}

/// Cancellation advances the object sequence and time while preserving the current revision.
#[expect(
    clippy::too_many_arguments,
    reason = "the pure transition names every independently bound cancellation fact"
)]
pub(crate) fn cancel(
    state: &Verified<State>,
    revision: revision::Id,
    id: cancellation::Id,
    account: account::Id,
    operator: revision::Id,
    mobile: &Mobile,
    operation: operation::Id,
    at: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if revision != state.revision_id() {
        return Err(Error::Integrity(
            "cancellation preserves the confirmed real-name revision",
        ));
    }
    if at < state.changed_at() {
        return Err(Error::Integrity(
            "a person transition does not precede the current state",
        ));
    }
    let draft = Draft::advance(
        state,
        person::state::Data {
            id: state.id(),
            revision_id: state.revision_id(),
            changed_at: at,
        },
    )?;
    Ok(Change::new(
        draft,
        record::Data {
            operation_id: operation,
            happened_at: at,
            event: record::Event::Cancellation {
                id,
                account_id: account,
                revision_id: revision,
                operator_id: operator,
                mobile: Arc::new(mobile.clone()),
            },
        },
    ))
}

/// Creation starts both counters at one and pairs the initial origin record.
pub(crate) fn create(
    id: person::Id,
    revision: revision::Id,
    account: account::Id,
    operation: operation::Id,
    at: DateTime<Utc>,
) -> Change<State> {
    Change::new(
        Draft::create(person::state::Data {
            id,
            revision_id: revision,
            changed_at: at,
        }),
        record::Data {
            operation_id: operation,
            happened_at: at,
            event: record::Event::Origin {
                account_id: account,
                revision_id: revision,
            },
        },
    )
}
