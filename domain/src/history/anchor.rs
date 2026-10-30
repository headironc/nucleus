//! Freshness metadata only: no business state, cached permission or database access.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU8, Ordering},
    },
};

use tokio::sync::watch;

use crate::{
    history::{Error, Locator, Object, Read, Record as _, Stamp, State, Verified},
    identity::operation,
};

const PENDING: u8 = 0;
const COMMITTED: u8 = 1;
const ROLLED_BACK: u8 = 2;
const UNCERTAIN: u8 = 3;

#[derive(Clone)]
pub(crate) struct Anchors {
    inner: Arc<Mutex<Inner>>,
    closed: watch::Sender<bool>,
}

#[derive(Default)]
struct Inner {
    generation: u64,
    removed: u64,
    entries: HashMap<Object, Entry>,
    locators: HashMap<Locator, HashSet<Object>>,
}

struct Entry {
    current: Option<Stamp>,
    changed: u64,
    pending: Option<Candidate>,
}

impl Entry {
    fn locators(&self) -> impl Iterator<Item = &Locator> {
        self.current
            .iter()
            .chain(self.pending.iter().flat_map(|pending| &pending.value))
            .flat_map(|stamp| &stamp.locators)
    }

    fn permits_absence(&self, locator: &Locator) -> bool {
        self.current
            .as_ref()
            .is_none_or(|stamp| !stamp.locators.contains(locator))
            || self.pending.as_ref().is_some_and(|pending| {
                pending
                    .value
                    .as_ref()
                    .is_none_or(|stamp| !stamp.locators.contains(locator))
            })
    }
}

struct Candidate {
    value: Option<Stamp>,
    operation: operation::Id,
    sequence: Option<i64>,
    outcome: Arc<AtomicU8>,
}

#[derive(Clone, Copy)]
pub(crate) struct View(u64);

impl Anchors {
    pub(crate) fn new(closed: watch::Sender<bool>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            closed,
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>, Error> {
        self.inner
            .lock()
            .map_err(|_| Error::Integrity("anchor synchronization remains intact"))
    }

    pub(crate) fn view(&self) -> Result<View, Error> {
        Ok(View(self.lock()?.generation))
    }

    pub(super) fn objects<S: State>(&self) -> Result<Vec<Object>, Error> {
        Ok(self
            .lock()?
            .entries
            .keys()
            .filter(|object| object.kind == S::NAME)
            .copied()
            .collect())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn seed(&self, object: Object, stamp: Stamp) -> Result<(), Error> {
        let mut inner = self.lock()?;
        if inner
            .entries
            .get(&object)
            .is_some_and(|entry| entry.pending.is_some())
        {
            return Err(Error::Integrity(
                "fixture installation has no pending application work",
            ));
        }
        let changed = inner.next()?;
        inner.unindex(object);
        inner.entries.insert(
            object,
            Entry {
                current: Some(stamp),
                changed,
                pending: None,
            },
        );
        inner.index(object);
        Ok(())
    }

    pub(super) fn initialize(&self, object: Object, stamp: Stamp) -> Result<(), Error> {
        let mut inner = self.lock()?;
        if inner.entries.contains_key(&object) {
            return Err(Error::Integrity("startup installs each state exactly once"));
        }
        if stamp
            .locators
            .iter()
            .any(|locator| inner.locators.contains_key(locator))
        {
            return Err(Error::Integrity("a startup locator has one state owner"));
        }
        inner.entries.insert(
            object,
            Entry {
                current: Some(stamp),
                changed: 0,
                pending: None,
            },
        );
        inner.index(object);
        Ok(())
    }

    pub(super) fn prepare(
        &self,
        object: Object,
        operation: operation::Id,
        sequence: i64,
        original: Option<Stamp>,
        value: Stamp,
    ) -> Result<Pending, Error> {
        self.stage(object, operation, Some(sequence), original, Some(value))
    }

    pub(super) fn remove(
        &self,
        object: Object,
        operation: operation::Id,
        original: Stamp,
    ) -> Result<Pending, Error> {
        self.stage(object, operation, None, Some(original), None)
    }

    fn stage(
        &self,
        object: Object,
        operation: operation::Id,
        sequence: Option<i64>,
        original: Option<Stamp>,
        value: Option<Stamp>,
    ) -> Result<Pending, Error> {
        let mut inner = self.lock()?;
        let generation = inner.next()?;
        let entry = inner.entries.entry(object).or_insert_with(|| Entry {
            current: None,
            changed: generation,
            pending: None,
        });
        if entry.current != original || entry.pending.is_some() {
            return Err(Error::Integrity("a writer stages the exact locked anchor"));
        }
        let outcome = Arc::new(AtomicU8::new(PENDING));
        entry.pending = Some(Candidate {
            value,
            operation,
            sequence,
            outcome: Arc::clone(&outcome),
        });
        entry.changed = generation;
        inner.index(object);
        Ok(Pending {
            anchors: self.clone(),
            operation,
            candidates: vec![Settlement { object, outcome }],
            settled: false,
        })
    }

    pub(super) fn check_locator(
        &self,
        locator: &Locator,
        observed: Option<Object>,
        read: Read,
    ) -> Result<(), Error> {
        self.lock()?.check_locator(locator, observed, read)
    }

    pub(super) fn check_related(
        &self,
        prefix: &Locator,
        observed: &HashMap<Locator, Object>,
        read: Read,
    ) -> Result<(), Error> {
        let inner = self.lock()?;
        for (locator, object) in observed {
            inner.check_locator(locator, Some(*object), read)?;
        }
        for locator in inner.locators.keys() {
            if locator.within(prefix) && !observed.contains_key(locator) {
                inner.check_locator(locator, None, read)?;
            }
        }
        Ok(())
    }

    /// An authenticated original result can settle its candidate without reading today's state.
    pub(crate) fn confirm<S: State>(&self, record: &Verified<S::Record>) -> Result<(), Error> {
        self.lock()?.confirm(
            Object::of::<S>(record.object()),
            record.operation_id(),
            record.sequence(),
        )
    }

    pub(super) fn check(
        &self,
        object: Object,
        observed: Option<Stamp>,
        read: Read,
    ) -> Result<(), Error> {
        let mut inner = self.lock()?;
        let Some(entry) = inner.entries.get(&object) else {
            return if observed.is_none() {
                Ok(())
            } else if matches!(read, Read::Snapshot(view) if inner.removed > view.0) {
                Err(Error::Changed)
            } else {
                Err(Error::Integrity("a retained state has an anchor"))
            };
        };
        match read {
            Read::Locked | Read::Shared => {
                let resolution = match &entry.pending {
                    Some(candidate) if observed == candidate.value => Some(COMMITTED),
                    Some(_) if observed == entry.current => Some(ROLLED_BACK),
                    Some(_) => {
                        return Err(Error::Integrity(
                            "a locked pending state is its original or candidate",
                        ));
                    }
                    None if observed == entry.current => return Ok(()),
                    None => {
                        return Err(Error::Integrity("a locked state equals its latest anchor"));
                    }
                };
                if let Some(resolution) = resolution {
                    inner.resolve(object, resolution)?;
                }
                Ok(())
            }
            Read::Snapshot(view) => {
                if observed == entry.current
                    || entry
                        .pending
                        .as_ref()
                        .is_some_and(|candidate| observed == candidate.value)
                {
                    return Ok(());
                }
                if let (Some(observed), Some(current)) = (&observed, &entry.current)
                    && observed.version > current.version
                {
                    return Ok(());
                }
                if entry.changed > view.0 {
                    Err(Error::Changed)
                } else {
                    Err(Error::Integrity(
                        "snapshot state is not older than its anchor view",
                    ))
                }
            }
        }
    }
}

impl Inner {
    fn check_locator(
        &self,
        locator: &Locator,
        observed: Option<Object>,
        read: Read,
    ) -> Result<(), Error> {
        let owners = self.locators.get(locator);
        if let Some(observed) = observed
            && !owners.is_some_and(|owners| owners.contains(&observed))
        {
            return Err(match (read, self.entries.get(&observed)) {
                (Read::Snapshot(view), Some(entry)) if entry.changed > view.0 => Error::Changed,
                _ => Error::Integrity("a located object owns its anchored lookup key"),
            });
        }
        for object in owners.into_iter().flatten() {
            if Some(*object) == observed {
                continue;
            }
            let entry = self
                .entries
                .get(object)
                .ok_or(Error::Integrity("an indexed location retains its state"))?;
            if entry.permits_absence(locator) {
                continue;
            }
            return Err(match read {
                Read::Snapshot(view) if entry.changed > view.0 => Error::Changed,
                _ => Error::Integrity("an occupied location retains its expected state"),
            });
        }
        Ok(())
    }

    fn confirm(
        &mut self,
        object: Object,
        operation: operation::Id,
        sequence: i64,
    ) -> Result<(), Error> {
        if self
            .entries
            .get(&object)
            .and_then(|entry| entry.pending.as_ref())
            .is_some_and(|candidate| {
                candidate.operation == operation && candidate.sequence == Some(sequence)
            })
        {
            self.resolve(object, COMMITTED)?;
        }
        Ok(())
    }

    fn index(&mut self, object: Object) {
        if let Some(entry) = self.entries.get(&object) {
            for locator in entry.locators() {
                self.locators
                    .entry(locator.clone())
                    .or_default()
                    .insert(object);
            }
        }
    }

    fn unindex(&mut self, object: Object) {
        if let Some(entry) = self.entries.get(&object) {
            for locator in entry.locators() {
                if let Some(owners) = self.locators.get_mut(locator) {
                    owners.remove(&object);
                    if owners.is_empty() {
                        self.locators.remove(locator);
                    }
                }
            }
        }
    }

    fn next(&mut self) -> Result<u64, Error> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(Error::Integrity("anchor generation is representable"))?;
        Ok(self.generation)
    }

    fn resolve(&mut self, object: Object, resolution: u8) -> Result<(), Error> {
        let generation = self.next()?;
        self.unindex(object);
        let entry = self
            .entries
            .get_mut(&object)
            .ok_or(Error::Integrity("a pending object retains its anchor"))?;
        let candidate = entry
            .pending
            .take()
            .ok_or(Error::Integrity("a settlement owns a candidate"))?;
        if resolution == COMMITTED {
            entry.current = candidate.value;
        }
        entry.changed = generation;
        candidate.outcome.store(resolution, Ordering::Release);
        if entry.current.is_none() {
            self.entries.remove(&object);
            // An older snapshot may still contain a removed row. Only the watermark remains;
            // a retry with a fresh view must reject any unanchored row that reappears.
            self.removed = generation;
        }
        self.index(object);
        Ok(())
    }
}

/// Abandonment closes admission; an explicitly unknown outcome remains available to the
/// next holder of the object's database lock, which can settle it from original/candidate.
pub(crate) struct Pending {
    anchors: Anchors,
    operation: operation::Id,
    candidates: Vec<Settlement>,
    settled: bool,
}

struct Settlement {
    object: Object,
    outcome: Arc<AtomicU8>,
}

impl Pending {
    /// Initialization and other common transactions settle every affected object together.
    pub(crate) fn extend(&mut self, mut other: Self) -> Result<(), Error> {
        if !Arc::ptr_eq(&self.anchors.inner, &other.anchors.inner)
            || self.operation != other.operation
        {
            return Err(Error::Integrity(
                "one transaction settles candidates from the same application and operation",
            ));
        }
        self.candidates.append(&mut other.candidates);
        other.settled = true;
        Ok(())
    }

    /// A later locked reader may prove this transaction before short-lived rows are removed.
    pub(crate) fn resolution(&self) -> Result<Option<bool>, Error> {
        let mut known = None;
        for candidate in &self.candidates {
            let committed = match candidate.outcome.load(Ordering::Acquire) {
                COMMITTED => true,
                ROLLED_BACK => false,
                PENDING | UNCERTAIN => continue,
                _ => {
                    return Err(Error::Integrity(
                        "a pending candidate has a declared outcome",
                    ));
                }
            };
            if known.is_some_and(|previous| previous != committed) {
                return Err(Error::Integrity(
                    "objects in one transaction have the same commit outcome",
                ));
            }
            known = Some(committed);
        }
        Ok(known)
    }

    pub(crate) fn committed(mut self) -> Result<(), Error> {
        self.finish(COMMITTED)?;
        self.settled = true;
        Ok(())
    }

    pub(crate) fn rolled_back(mut self) -> Result<(), Error> {
        self.finish(ROLLED_BACK)?;
        self.settled = true;
        Ok(())
    }

    fn finish(&self, resolution: u8) -> Result<(), Error> {
        let mut inner = self.anchors.lock()?;
        // Check the whole transaction before changing any anchor. A later locked reader
        // may already have confirmed one of its objects while COMMIT was being recovered.
        let mut unresolved = 0_u64;
        for candidate in &self.candidates {
            match candidate.outcome.load(Ordering::Acquire) {
                PENDING | UNCERTAIN => {
                    let retained = inner
                        .entries
                        .get(&candidate.object)
                        .and_then(|entry| entry.pending.as_ref());
                    if !retained
                        .is_some_and(|retained| Arc::ptr_eq(&retained.outcome, &candidate.outcome))
                    {
                        return Err(Error::Integrity(
                            "a settlement retains ownership of its pending candidate",
                        ));
                    }
                    unresolved = unresolved
                        .checked_add(1)
                        .ok_or(Error::Integrity("anchor generation is representable"))?;
                }
                previous if previous == resolution => {}
                _ => {
                    return Err(Error::Integrity(
                        "commit recovery agrees with a later locked settlement",
                    ));
                }
            }
        }
        inner
            .generation
            .checked_add(unresolved)
            .ok_or(Error::Integrity("anchor generation is representable"))?;
        for candidate in &self.candidates {
            if matches!(
                candidate.outcome.load(Ordering::Acquire),
                PENDING | UNCERTAIN
            ) {
                inner.resolve(candidate.object, resolution)?;
            }
        }
        Ok(())
    }

    pub(crate) fn uncertain(mut self) {
        for candidate in &self.candidates {
            let _ = candidate.outcome.compare_exchange(
                PENDING,
                UNCERTAIN,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
        self.settled = true;
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.settled {
            self.anchors.closed.send_replace(true);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::collections::HashMap;

    use tokio::sync::watch;

    use super::Anchors;
    use crate::{
        history::{Error, Locator, Object, Read, Stamp},
        identity::operation,
        keys::{Id, seal::Seal},
    };

    fn object(id: u8) -> Object {
        Object {
            kind: "test.state",
            id: [id; 16],
        }
    }
    fn operation() -> operation::Id {
        uuid::Uuid::new_v4().to_string().parse().expect("operation")
    }
    fn stamp(version: i64) -> Stamp {
        Stamp {
            version,
            seal: Seal::stored(
                vec![u8::try_from(version).expect("small version"); 32],
                Id::new(0).expect("key"),
                1,
            )
            .expect("seal"),
            locators: Vec::new(),
        }
    }

    fn locator(value: u8) -> Locator {
        Locator {
            kind: "test.state",
            name: "mobile",
            value: vec![value; 32],
        }
    }

    fn located(version: i64, value: u8) -> Stamp {
        Stamp {
            locators: vec![locator(value)],
            ..stamp(version)
        }
    }

    #[test]
    fn occupied_locations_reject_missing_or_foreign_rows_and_duplicate_startup_owners() {
        let anchors = Anchors::new(watch::channel(false).0);
        anchors
            .initialize(object(1), located(1, 7))
            .expect("first owner");
        let read = Read::Snapshot(anchors.view().expect("view"));
        anchors
            .check_locator(&locator(7), Some(object(1)), read)
            .expect("current owner");
        anchors
            .check_locator(&locator(8), None, read)
            .expect("unoccupied location");
        for observed in [None, Some(object(2))] {
            assert!(matches!(
                anchors.check_locator(&locator(7), observed, read),
                Err(Error::Integrity(_)),
            ));
        }
        assert!(matches!(
            anchors.initialize(object(2), located(1, 7)),
            Err(Error::Integrity(_)),
        ));
        assert!(matches!(
            anchors.initialize(object(1), located(2, 8)),
            Err(Error::Integrity(_)),
        ));
        anchors
            .check(object(1), Some(located(1, 7)), Read::Locked)
            .expect("rejected startup inserts preserve the original");
        anchors
            .check(object(2), None, Read::Locked)
            .expect("a duplicate location never installs another owner");
    }

    #[test]
    fn related_lookup_requires_every_retained_member_of_only_the_requested_group() {
        let anchors = Anchors::new(watch::channel(false).0);
        let prefix = Locator {
            kind: "test.state",
            name: "mobile",
            value: vec![7],
        };
        let first = locator(7);
        let second = Locator {
            value: [vec![7], vec![8; 31]].concat(),
            ..first.clone()
        };
        let unrelated = locator(9);
        for (id, location) in [(1, &first), (2, &second), (3, &unrelated)] {
            anchors
                .initialize(
                    object(id),
                    Stamp {
                        locators: vec![location.clone()],
                        ..stamp(1)
                    },
                )
                .expect("retained member");
        }
        let read = Read::Snapshot(anchors.view().expect("view"));
        let mut observed = HashMap::from([(first, object(1)), (second.clone(), object(2))]);
        anchors
            .check_related(&prefix, &observed, read)
            .expect("a complete group does not require unrelated states");
        observed.remove(&second);
        assert!(matches!(
            anchors.check_related(&prefix, &observed, read),
            Err(Error::Integrity(_)),
        ));
        observed.insert(second.clone(), object(3));
        assert!(matches!(
            anchors.check_related(&prefix, &observed, read),
            Err(Error::Integrity(_)),
        ));
        observed.remove(&second);
        let pending = anchors
            .remove(
                object(2),
                operation(),
                Stamp {
                    locators: vec![second],
                    ..stamp(1)
                },
            )
            .expect("authorized retention removal");
        anchors
            .check_related(&prefix, &observed, read)
            .expect("pending removal permits the database's post-commit set");
        pending
            .rolled_back()
            .expect("failed removal restores the obligation");
        assert!(matches!(
            anchors.check_related(&prefix, &observed, Read::Locked),
            Err(Error::Integrity(_)),
        ));
    }

    #[test]
    fn pending_release_and_reoccupation_retain_both_location_owners_until_settlement() {
        let anchors = Anchors::new(watch::channel(false).0);
        anchors
            .initialize(object(1), located(1, 7))
            .expect("original owner");
        let before = anchors.view().expect("old view");
        let released = anchors
            .prepare(object(1), operation(), 2, Some(located(1, 7)), stamp(2))
            .expect("release location");
        let claimed = anchors
            .prepare(object(2), operation(), 1, None, located(1, 7))
            .expect("a following transaction claims the released location");
        for observed in [None, Some(object(1)), Some(object(2))] {
            anchors
                .check_locator(&locator(7), observed, Read::Snapshot(before))
                .expect("snapshots may precede, lie between or follow the two commits");
        }
        claimed
            .committed()
            .expect("the later acknowledgement arrives first");
        anchors
            .check_locator(&locator(7), Some(object(2)), Read::Locked)
            .expect("the old pending release does not conceal the new owner");
        released
            .committed()
            .expect("the original acknowledgement arrives later");
        assert!(matches!(
            anchors.check_locator(&locator(7), Some(object(1)), Read::Snapshot(before)),
            Err(Error::Changed),
        ));
        let fresh = Read::Snapshot(anchors.view().expect("fresh view"));
        anchors
            .check_locator(&locator(7), Some(object(2)), fresh)
            .expect("only the new owner remains");
        for observed in [None, Some(object(1))] {
            assert!(matches!(
                anchors.check_locator(&locator(7), observed, fresh),
                Err(Error::Integrity(_)),
            ));
        }
    }

    #[test]
    fn deletion_settles_existence_and_locations_without_retaining_removed_objects() {
        for (read, committed) in [
            (Read::Locked, false),
            (Read::Locked, true),
            (Read::Shared, false),
            (Read::Shared, true),
        ] {
            let anchors = Anchors::new(watch::channel(false).0);
            anchors
                .initialize(object(1), located(1, 7))
                .expect("retained short-lived state");
            let before = anchors.view().expect("old snapshot");
            let pending = anchors
                .remove(object(1), operation(), located(1, 7))
                .expect("deletion candidate");
            assert_eq!(pending.resolution().expect("unproven deletion"), None);
            for observed in [None, Some(located(1, 7))] {
                anchors
                    .check(object(1), observed, Read::Snapshot(before))
                    .expect("either side of deletion");
            }
            anchors
                .check_locator(&locator(7), None, Read::Snapshot(before))
                .expect("the pending deletion permits absence");
            pending.uncertain();
            anchors
                .check(object(1), (!committed).then(|| located(1, 7)), read)
                .expect("a locked read resolves the actual database outcome");
            if committed {
                assert!(
                    !anchors
                        .lock()
                        .expect("metadata")
                        .entries
                        .contains_key(&object(1))
                );
                assert!(matches!(
                    anchors.check(object(1), Some(located(1, 7)), Read::Snapshot(before)),
                    Err(Error::Changed),
                ));
                let fresh = Read::Snapshot(anchors.view().expect("fresh view"));
                assert!(matches!(
                    anchors.check(object(1), Some(located(1, 7)), fresh),
                    Err(Error::Integrity(_)),
                ));
                anchors
                    .check_locator(&locator(7), None, fresh)
                    .expect("the location is free after deletion");
                anchors
                    .initialize(object(2), located(1, 7))
                    .expect("the removed mapping has no remaining owner");
            } else {
                anchors
                    .check_locator(&locator(7), Some(object(1)), Read::Locked)
                    .expect("rollback restores the original owner");
                assert!(matches!(
                    anchors.check_locator(&locator(7), None, Read::Locked),
                    Err(Error::Integrity(_)),
                ));
            }
        }
    }

    #[test]
    fn one_commit_settles_every_object_after_a_locked_reader_confirms_one() {
        let anchors = Anchors::new(watch::channel(false).0);
        let first = object(1);
        let second = object(2);
        anchors.initialize(first, stamp(1)).expect("first state");
        let operation = operation();
        let mut pending = anchors
            .prepare(first, operation, 2, Some(stamp(1)), stamp(2))
            .expect("updated first state");
        pending
            .extend(
                anchors
                    .prepare(second, operation, 1, None, stamp(1))
                    .expect("new second state"),
            )
            .expect("one transaction");
        assert_eq!(pending.resolution().expect("unproven transaction"), None);
        anchors
            .check(first, Some(stamp(2)), Read::Locked)
            .expect("a later writer observes the committed first state");
        assert_eq!(
            pending.resolution().expect("one object proves commit"),
            Some(true)
        );
        pending.committed().expect("the common commit is recovered");
        let view = anchors.view().expect("settled view");
        anchors
            .check(first, Some(stamp(2)), Read::Snapshot(view))
            .expect("first current state");
        anchors
            .check(second, Some(stamp(1)), Read::Snapshot(view))
            .expect("second current state");
        assert!(matches!(
            anchors.check(second, None, Read::Snapshot(view)),
            Err(Error::Integrity(_))
        ));
    }

    #[test]
    fn a_conflicting_settlement_does_not_partially_commit_other_objects() {
        let (closed, admission) = watch::channel(false);
        let anchors = Anchors::new(closed);
        let first = object(1);
        let second = object(2);
        for object in [first, second] {
            anchors.initialize(object, stamp(1)).expect("old state");
        }
        let operation = operation();
        let mut pending = anchors
            .prepare(first, operation, 2, Some(stamp(1)), stamp(2))
            .expect("first candidate");
        pending
            .extend(
                anchors
                    .prepare(second, operation, 2, Some(stamp(1)), stamp(2))
                    .expect("second candidate"),
            )
            .expect("one transaction");
        anchors
            .check(second, Some(stamp(1)), Read::Locked)
            .expect("the database proves rollback");
        assert_eq!(
            pending.resolution().expect("one object proves rollback"),
            Some(false)
        );
        assert!(matches!(pending.committed(), Err(Error::Integrity(_))));
        assert!(
            *admission.borrow(),
            "contradictory recovery closes admission"
        );
        anchors
            .check(first, Some(stamp(1)), Read::Locked)
            .expect("the first anchor was not partially advanced");
    }

    #[test]
    fn uncertain_common_work_can_be_resolved_for_each_locked_object() {
        let (closed, admission) = watch::channel(false);
        let anchors = Anchors::new(closed);
        let first = object(1);
        let second = object(2);
        let operation = operation();
        let mut pending = anchors
            .prepare(first, operation, 1, None, stamp(1))
            .expect("first new state");
        pending
            .extend(
                anchors
                    .prepare(second, operation, 1, None, stamp(1))
                    .expect("second new state"),
            )
            .expect("one transaction");
        pending.uncertain();
        assert!(
            !*admission.borrow(),
            "explicit uncertainty keeps recovery open"
        );
        for object in [first, second] {
            anchors
                .check(object, None, Read::Locked)
                .expect("a later lock confirms the common rollback");
        }
        let view = anchors.view().expect("rollback view");
        for object in [first, second] {
            anchors
                .check(object, None, Read::Snapshot(view))
                .expect("rolled-back creations remain absent");
        }
    }

    #[test]
    fn snapshots_retry_only_when_a_later_settlement_can_explain_staleness() {
        let anchors = Anchors::new(watch::channel(false).0);
        let object = object(1);
        anchors
            .initialize(object, stamp(1))
            .expect("startup anchor");
        let before = anchors.view().expect("old view");
        let pending = anchors
            .prepare(object, operation(), 2, Some(stamp(1)), stamp(2))
            .expect("candidate before COMMIT");
        for observed in [stamp(1), stamp(2)] {
            anchors
                .check(object, Some(observed), Read::Snapshot(before))
                .expect("either database snapshot is legitimate while pending");
        }
        pending.committed().expect("commit acknowledged");
        assert!(matches!(
            anchors.check(object, Some(stamp(1)), Read::Snapshot(before)),
            Err(Error::Changed)
        ));
        let fresh = anchors.view().expect("fresh view");
        for observed in [
            None,
            Some(stamp(1)),
            Some(Stamp {
                version: 2,
                seal: stamp(3).seal,
                locators: Vec::new(),
            }),
        ] {
            assert!(
                matches!(
                    anchors.check(object, observed, Read::Snapshot(fresh)),
                    Err(Error::Integrity(_))
                ),
                "a fresh view never accepts deletion, rollback or a changed seal"
            );
        }
        anchors
            .check(object, Some(stamp(2)), Read::Snapshot(fresh))
            .expect("current state");
    }

    #[test]
    fn unknown_candidates_are_settled_by_the_next_locked_writer() {
        for (read, committed) in [
            (Read::Locked, false),
            (Read::Locked, true),
            (Read::Shared, false),
            (Read::Shared, true),
        ] {
            let anchors = Anchors::new(watch::channel(false).0);
            let object = object(1);
            anchors
                .initialize(object, stamp(1))
                .expect("startup anchor");
            anchors
                .prepare(object, operation(), 2, Some(stamp(1)), stamp(2))
                .expect("candidate")
                .uncertain();
            let retained = if committed { stamp(2) } else { stamp(1) };
            anchors
                .check(object, Some(retained.clone()), read)
                .expect("database lock proves the previous outcome");
            anchors
                .prepare(object, operation(), 3, Some(retained), stamp(3))
                .expect("following write can advance")
                .committed()
                .expect("following commit");
            anchors
                .check(object, Some(stamp(3)), Read::Locked)
                .expect("following state is current");
        }
    }

    #[test]
    fn a_late_acknowledgement_cannot_overwrite_a_following_candidate() {
        let anchors = Anchors::new(watch::channel(false).0);
        let object = object(1);
        anchors.initialize(object, stamp(1)).expect("startup");
        let first = anchors
            .prepare(object, operation(), 2, Some(stamp(1)), stamp(2))
            .expect("first candidate");
        anchors
            .check(object, Some(stamp(2)), Read::Locked)
            .expect("next writer sees the committed row before the first acknowledgement");
        let second = anchors
            .prepare(object, operation(), 3, Some(stamp(2)), stamp(3))
            .expect("following candidate");
        first
            .committed()
            .expect("late acknowledgement of the same known outcome");
        anchors
            .check(
                object,
                Some(stamp(2)),
                Read::Snapshot(anchors.view().expect("view")),
            )
            .expect("following transaction can still be uncommitted");
        anchors
            .check(object, Some(stamp(3)), Read::Locked)
            .expect("latest anchor was retained");
        anchors
            .remove(object, operation(), stamp(3))
            .expect("later retention transaction")
            .committed()
            .expect("short-lived state deleted");
        anchors
            .check(object, None, Read::Locked)
            .expect("state is absent");
        assert_eq!(
            second
                .resolution()
                .expect("late original recovery after deletion"),
            Some(true)
        );
        second
            .committed()
            .expect("late acknowledgement retains the original commit");
    }

    #[test]
    fn multi_record_replay_confirms_the_final_record_and_shared_commit() {
        let anchors = Anchors::new(watch::channel(false).0);
        let beneficiary = object(1);
        let occupier = object(2);
        let operation = operation();
        anchors
            .initialize(beneficiary, stamp(1))
            .expect("beneficiary");
        anchors.initialize(occupier, stamp(1)).expect("occupier");
        let mut pending = anchors
            .prepare(beneficiary, operation, 3, Some(stamp(1)), stamp(2))
            .expect("revocation and recovery advance one version");
        pending
            .extend(
                anchors
                    .prepare(occupier, operation, 2, Some(stamp(1)), stamp(2))
                    .expect("occupier revocation"),
            )
            .expect("one transaction");
        anchors
            .lock()
            .expect("anchors")
            .confirm(beneficiary, operation, 2)
            .expect("intermediate revocation is authentic");
        assert_eq!(pending.resolution().expect("unresolved operation"), None);
        anchors
            .lock()
            .expect("anchors")
            .confirm(beneficiary, operation, 3)
            .expect("final recovery proves this candidate committed");
        assert_eq!(
            pending.resolution().expect("known shared outcome"),
            Some(true)
        );
        pending.committed().expect("settle both objects together");
        for object in [beneficiary, occupier] {
            anchors
                .check(object, Some(stamp(2)), Read::Locked)
                .expect("both final states are current");
            assert!(anchors.check(object, Some(stamp(1)), Read::Locked).is_err());
        }
    }

    #[test]
    fn replay_confirms_only_the_original_record_when_families_reuse_a_uuid() {
        for replay_current in [false, true] {
            let anchors = Anchors::new(watch::channel(false).0);
            let object = object(1);
            let operation = operation();
            anchors.initialize(object, stamp(1)).expect("startup");
            anchors
                .prepare(object, operation, 2, Some(stamp(1)), stamp(2))
                .expect("original appointment")
                .committed()
                .expect("original result");
            anchors
                .prepare(object, operation, 3, Some(stamp(2)), stamp(3))
                .expect("another family reuses the UUID for the same object")
                .uncertain();

            anchors
                .lock()
                .expect("anchors")
                .confirm(object, operation, if replay_current { 3 } else { 2 })
                .expect("authenticated replay");
            anchors
                .check(
                    object,
                    Some(stamp(if replay_current { 3 } else { 2 })),
                    Read::Locked,
                )
                .expect("only the current record confirms the later candidate");
        }
    }

    #[test]
    fn pending_creation_preserves_absence_and_abandonment_closes_admission() {
        for committed in [false, true] {
            let (closed, _) = watch::channel(false);
            let anchors = Anchors::new(closed.clone());
            let object = object(1);
            let view = anchors.view().expect("view");
            let pending = anchors
                .prepare(object, operation(), 1, None, stamp(1))
                .expect("creation");
            anchors
                .check(object, None, Read::Snapshot(view))
                .expect("pre-commit absence");
            anchors
                .check(object, Some(stamp(1)), Read::Snapshot(view))
                .expect("post-commit visibility");
            if committed {
                pending.committed().expect("known commit");
            } else {
                pending.rolled_back().expect("known rollback");
            }
            assert!(!*closed.borrow());
            anchors
                .check(object, committed.then(|| stamp(1)), Read::Locked)
                .expect("known outcome");
            drop(
                anchors
                    .prepare(
                        Object {
                            kind: "test.state",
                            id: [2; 16],
                        },
                        operation(),
                        1,
                        None,
                        stamp(1),
                    )
                    .expect("abandoned candidate"),
            );
            assert!(*closed.borrow(), "unsettled accepted work closes admission");
        }
    }
}
