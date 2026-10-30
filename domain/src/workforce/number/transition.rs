//! Pure number changes selected by the command after current authority and input checks.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    history::{Change, Draft, Error, Verified},
    identity::{operation, person},
    workforce::number::{
        Reason, Revision, Source,
        record::{self, Event},
        revision::{self, Creation},
        state::{self, State},
    },
};

pub(super) struct Operation {
    pub(super) id: operation::Id,
    pub(super) operator_id: person::revision::Id,
    pub(super) time: DateTime<Utc>,
}

pub(super) struct Created {
    pub(super) value: revision::Data,
    pub(super) change: Change<State>,
}

fn record(
    id: Uuid,
    revision_id: person::revision::Id,
    state: &state::Data,
    operation: &Operation,
    event: Event,
) -> record::Data {
    record::Data {
        id,
        revision_id,
        number_id: state.revision.id,
        operator_id: operation.operator_id,
        time: operation.time,
        operation_id: operation.id,
        tenure_id: state.tenure_id,
        held: state.held,
        event,
    }
}

pub(super) struct Allocation {
    pub(super) id: Uuid,
    pub(super) person_id: person::Id,
    pub(super) revision_id: person::revision::Id,
    pub(super) number: Revision,
    pub(super) source: Source,
    pub(super) reason: Option<Reason>,
}

pub(super) fn allocate(allocation: Allocation, operation: &Operation) -> Created {
    let state = state::Data {
        id: allocation.person_id,
        revision: allocation.number,
        held: true,
        tenure_id: allocation.id,
        changed_at: operation.time,
    };
    let record = record(
        allocation.id,
        allocation.revision_id,
        &state,
        operation,
        Event::Allocation {
            source: allocation.source,
            reason: allocation.reason,
        },
    );
    Created {
        value: revision::Data {
            person_id: allocation.person_id,
            revision: allocation.number,
            creation: Creation::Allocation(allocation.id),
        },
        change: Change::new(Draft::create(state), record),
    }
}

pub(super) struct Correction {
    pub(super) id: Uuid,
    pub(super) revision_id: person::revision::Id,
    pub(super) number: Revision,
    pub(super) reason: Reason,
}

pub(super) fn correct(
    original: &Verified<State>,
    correction: Correction,
    operation: &Operation,
) -> Result<Created, Error> {
    let tenure_id = if !original.held()
        || original
            .revision()
            .number
            .shares_pool(correction.number.number)
    {
        original.tenure_id()
    } else {
        correction.id
    };
    let state = state::Data {
        id: original.id(),
        revision: correction.number,
        held: original.held(),
        tenure_id,
        changed_at: operation.time,
    };
    let record = record(
        correction.id,
        correction.revision_id,
        &state,
        operation,
        Event::Correction {
            before_id: original.revision().id,
            reason: correction.reason,
        },
    );
    Ok(Created {
        value: revision::Data {
            person_id: original.id(),
            revision: correction.number,
            creation: Creation::Correction(correction.id),
        },
        change: Change::new(Draft::advance(original, state)?, record),
    })
}

pub(super) struct Recovery {
    pub(super) id: Uuid,
    pub(super) person_id: person::Id,
    pub(super) revision_id: person::revision::Id,
    pub(super) number: Revision,
    pub(super) origin_id: super::Id,
    pub(super) occupier_id: Uuid,
    pub(super) occupier_revision_id: person::revision::Id,
    pub(super) revocation_id: Uuid,
    pub(super) reason: Reason,
}

pub(super) struct Restored {
    pub(super) beneficiary: Created,
    pub(super) occupier: Change<State>,
}

pub(super) fn restore(
    beneficiary: Option<&Verified<State>>,
    occupier: &Verified<State>,
    recovery: Recovery,
    operation: &Operation,
) -> Result<Restored, Error> {
    let revoked = state::Data {
        id: occupier.id(),
        revision: occupier.revision(),
        held: false,
        tenure_id: recovery.occupier_id,
        changed_at: operation.time,
    };
    let occupier_record = record(
        recovery.occupier_id,
        recovery.occupier_revision_id,
        &revoked,
        operation,
        Event::Revocation {
            recovery_id: recovery.id,
            reason: recovery.reason.clone(),
        },
    );
    let occupier = Change::new(Draft::advance(occupier, revoked)?, occupier_record);

    let mut preceding = Vec::new();
    if let Some(original) = beneficiary.filter(|state| state.held()) {
        let revoked = state::Data {
            id: original.id(),
            revision: original.revision(),
            held: false,
            tenure_id: recovery.revocation_id,
            changed_at: operation.time,
        };
        preceding.push(record(
            recovery.revocation_id,
            recovery.revision_id,
            &revoked,
            operation,
            Event::Revocation {
                recovery_id: recovery.id,
                reason: recovery.reason.clone(),
            },
        ));
    }
    let state = state::Data {
        id: recovery.person_id,
        revision: recovery.number,
        held: true,
        tenure_id: recovery.id,
        changed_at: operation.time,
    };
    let last = record(
        recovery.id,
        recovery.revision_id,
        &state,
        operation,
        Event::Recovery {
            origin_id: recovery.origin_id,
            occupier_id: recovery.occupier_id,
            revocation_id: (!preceding.is_empty()).then_some(recovery.revocation_id),
            reason: recovery.reason,
        },
    );
    Ok(Restored {
        beneficiary: Created {
            value: revision::Data {
                person_id: recovery.person_id,
                revision: recovery.number,
                creation: Creation::Recovery(recovery.id),
            },
            change: Change::multiple(beneficiary, state, preceding, last)?,
        },
        occupier,
    })
}
