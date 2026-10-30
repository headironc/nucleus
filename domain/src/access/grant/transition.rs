//! Pure Grant transitions; the command owns authorization, locks and transaction completion.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    access::{
        Entry, assignment,
        grant::{
            record::{self, Event},
            state::{Admission, Data, State},
            suspension,
            termination::Mode,
        },
        role::{Code, Role},
    },
    history::{Change, Draft, Error, Verified},
    identity::{cancellation, operation, person::revision},
};

fn change(
    original: &Verified<State>,
    data: Data,
    operation_id: operation::Id,
    event: Event,
) -> Result<Change<State>, Error> {
    let record = record::Data::new(&data, operation_id, event);
    Ok(Change::new(Draft::advance(original, data)?, record))
}

pub(super) fn first(
    admission: Admission,
    role: &Role,
    assignment_id: assignment::Id,
) -> Change<State> {
    let operation_id = admission.operation_id;
    let event = Event::Assignment {
        previous_id: None,
        change_id: role.change_id(),
        entry: None,
        operator_id: admission.operator_id,
        revision_id: admission.revision_id,
    };
    let data = Data {
        changed_at: admission.granted_at,
        admission,
        role_id: role.id(),
        assignment_id,
        suspension_id: None,
        termination: None,
        epoch: 0,
    };
    let record = record::Data::new(&data, operation_id, event);
    Change::new(Draft::create(data), record)
}

pub(in crate::access) struct Assignment {
    pub(in crate::access) id: assignment::Id,
    pub(in crate::access) entry: Entry,
    pub(in crate::access) operator_id: revision::Id,
    pub(in crate::access) revision_id: revision::Id,
}

pub(in crate::access) fn assign(
    original: &Verified<State>,
    before: &Role,
    after: &Role,
    assignment: Assignment,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.role_id() != before.id() || before.id() == after.id() {
        return Err(Error::Integrity(
            "a replacement changes the grant's verified current role",
        ));
    }
    let mut data = original.next(now)?;
    data.role_id = after.id();
    data.assignment_id = assignment.id;
    if loses_admission(before, after) {
        advance_epoch(&mut data)?;
    }
    change(
        original,
        data,
        operation_id,
        Event::Assignment {
            previous_id: Some(original.assignment_id()),
            change_id: after.change_id(),
            entry: Some(assignment.entry),
            operator_id: assignment.operator_id,
            revision_id: assignment.revision_id,
        },
    )
}

pub(super) fn suspend(
    original: &Verified<State>,
    id: suspension::Id,
    operator_id: revision::Id,
    revision_id: revision::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.suspension_id().is_some() {
        return Err(Error::Integrity(
            "a grant can only acquire its first unresolved suspension",
        ));
    }
    let mut data = original.next(now)?;
    data.suspension_id = Some(id);
    change(
        original,
        data,
        operation_id,
        Event::Suspension {
            id,
            operator_id,
            revision_id,
        },
    )
}

pub(super) fn resume(
    original: &Verified<State>,
    id: suspension::Id,
    operator_id: revision::Id,
    revision_id: revision::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.suspension_id() != Some(id) {
        return Err(Error::Integrity(
            "a resumption clears exactly the verified current suspension",
        ));
    }
    let mut data = original.next(now)?;
    data.suspension_id = None;
    change(
        original,
        data,
        operation_id,
        Event::Resumption {
            id,
            operator_id,
            revision_id,
        },
    )
}

pub(super) fn terminate(
    original: &Verified<State>,
    mode: Mode,
    operator_id: revision::Id,
    revision_id: revision::Id,
    operation_id: operation::Id,
    cancellation_id: Option<cancellation::Id>,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if (mode == Mode::Cancellation) != cancellation_id.is_some()
        || (mode == Mode::Current && operator_id != revision_id)
        || (mode == Mode::Manager && operator_id == revision_id)
    {
        return Err(Error::Integrity(
            "a grant termination retains its exact source",
        ));
    }
    let mut data = original.next(now)?;
    data.termination = Some(mode);
    change(
        original,
        data,
        operation_id,
        Event::Termination {
            mode,
            operator_id,
            revision_id,
            cancellation_id,
        },
    )
}

pub(in crate::access) fn loses_admission(before: &Role, after: &Role) -> bool {
    before.permissions().contains(&Code::WorkforceAdmission)
        && !after.permissions().contains(&Code::WorkforceAdmission)
}

fn advance_epoch(data: &mut Data) -> Result<(), Error> {
    data.epoch = data.epoch.checked_add(1).ok_or(Error::Integrity(
        "the grant admission epoch is representable",
    ))?;
    Ok(())
}

pub(in crate::access) fn lose(
    original: &Verified<State>,
    before: &Role,
    after: &Role,
    id: Uuid,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.role_id() != before.id()
        || before.id() != after.id()
        || !loses_admission(before, after)
    {
        return Err(Error::Integrity(
            "an admission-loss projection names the current role losing that permission",
        ));
    }
    let mut data = original.next(now)?;
    advance_epoch(&mut data)?;
    change(
        original,
        data,
        operation_id,
        Event::Loss {
            id,
            change_id: after.change_id(),
        },
    )
}
