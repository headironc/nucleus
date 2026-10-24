//! Non-secret inventory of the loaded root versions.

use std::collections::BTreeMap;

use platform::cryptography::sha256;

use crate::keys::Id;

/// Root fingerprints and the active version; only `Keys::new` derives it from validated material.
#[derive(Clone)]
pub struct Metadata {
    active_id: Id,
    versions: BTreeMap<Id, sha256::Digest>,
}

impl Metadata {
    pub(super) fn new(active_id: Id, versions: BTreeMap<Id, sha256::Digest>) -> Self {
        Self {
            active_id,
            versions,
        }
    }

    /// Every version in ascending key ID order, with its fingerprint and whether it is active.
    pub(super) fn versions(&self) -> impl Iterator<Item = (Id, &sha256::Digest, bool)> {
        self.versions
            .iter()
            .map(|(id, fingerprint)| (*id, fingerprint, *id == self.active_id))
    }

    pub(super) fn find(&self, id: Id) -> Option<(&sha256::Digest, bool)> {
        self.versions
            .get(&id)
            .map(|fingerprint| (fingerprint, id == self.active_id))
    }

    pub(super) fn len(&self) -> usize {
        self.versions.len()
    }
}
