//! Pure invitation changes; commands own current authority and transaction completion.

#[cfg(test)]
#[path = "transition_tests.rs"]
mod tests;

use chrono::{DateTime, Utc};

use super::{
    Capacity, Expiry, adjustment,
    record::{self, Event},
    state::{Creation, Data, State},
};
use crate::{
    access::grant,
    history::{Change, Draft, Error, Verified},
    identity::{operation, person::revision},
};

fn change(
    original: &Verified<State>,
    data: Data,
    operator_id: revision::Id,
    operation_id: operation::Id,
    event: Event,
) -> Result<Change<State>, Error> {
    let record = record::Data::from_state(&data, operation_id, operator_id, event);
    Ok(Change::new(Draft::advance(original, data)?, record))
}

pub(super) fn create(creation: Creation) -> Change<State> {
    let operator_id = creation.operator_id;
    let operation_id = creation.operation_id;
    let data = Data::created(creation);
    let record = record::Data::from_state(&data, operation_id, operator_id, Event::Creation);
    Change::new(Draft::create(data), record)
}

pub(super) struct Adjustment {
    pub(super) id: adjustment::Id,
    pub(super) capacity: Capacity,
    pub(super) expires_at: Expiry,
}

pub(super) fn adjust(
    original: &Verified<State>,
    adjustment: Adjustment,
    operator_id: revision::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.expired(now)
        || DateTime::<Utc>::from(adjustment.expires_at) <= now
        || i64::from(adjustment.capacity) < original.used()
        || (adjustment.capacity, adjustment.expires_at)
            == (original.capacity(), original.expires_at())
    {
        return Err(Error::Integrity(
            "an adjustment changes live terms without reducing capacity below use",
        ));
    }
    let mut data = original.next(now)?;
    data.capacity = adjustment.capacity;
    data.expires_at = adjustment.expires_at;
    change(
        original,
        data,
        operator_id,
        operation_id,
        Event::Adjustment { id: adjustment.id },
    )
}

pub(super) fn withdraw(
    original: &Verified<State>,
    operator_id: revision::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.expired(now) {
        return Err(Error::Integrity("a withdrawal precedes invitation expiry"));
    }
    let mut data = original.next(now)?;
    data.withdrawn_at = Some(now);
    change(original, data, operator_id, operation_id, Event::Withdrawal)
}

pub(super) fn accept(
    original: &Verified<State>,
    grant_id: grant::Id,
    operator_id: revision::Id,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Change<State>, Error> {
    if original.expired(now) || original.used() >= i64::from(original.capacity()) {
        return Err(Error::Integrity(
            "an acceptance consumes a live invitation's free slot",
        ));
    }
    let mut data = original.next(now)?;
    data.used = data.used.checked_add(1).ok_or(Error::Integrity(
        "invitation usage fits its cumulative counter",
    ))?;
    change(
        original,
        data,
        operator_id,
        operation_id,
        Event::Acceptance { grant_id },
    )
}
