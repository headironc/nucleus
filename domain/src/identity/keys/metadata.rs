//! Non-secret inventory derived from the loaded purpose-specific key sets.

use std::collections::BTreeMap;

use platform::cryptography::sha256;

use crate::identity::keys::{Id, Purpose, challenge, encryption, message, mobile};

/// Non-secret metadata can only be derived from all delivered validated key sets.
pub struct Metadata {
    versions: BTreeMap<(Purpose, Id), Version>,
}

impl Metadata {
    pub fn new(
        challenge: &challenge::Keys,
        mobile: &mobile::Keys,
        encryption: &encryption::Keys,
        message: &message::Keys,
    ) -> Self {
        let mut versions = BTreeMap::new();
        for key in challenge.all() {
            versions.insert(
                (Purpose::Challenge, key.id()),
                Version {
                    fingerprint: key.fingerprint().clone(),
                    active: key.id() == challenge.active().id(),
                },
            );
        }
        for (id, fingerprint) in mobile.fingerprints() {
            versions.insert(
                (Purpose::Mobile, id),
                Version {
                    fingerprint: fingerprint.clone(),
                    active: id == mobile.active_id(),
                },
            );
        }
        for (id, fingerprint) in encryption.fingerprints() {
            versions.insert(
                (Purpose::Encryption, id),
                Version {
                    fingerprint: fingerprint.clone(),
                    active: id == encryption.active_id(),
                },
            );
        }
        for key in message.all() {
            versions.insert(
                (Purpose::Message, key.id()),
                Version {
                    fingerprint: key.fingerprint().clone(),
                    active: key.id() == message.active().id(),
                },
            );
        }
        Self { versions }
    }

    pub(super) fn versions(&self) -> &BTreeMap<(Purpose, Id), Version> {
        &self.versions
    }
}

pub(super) struct Version {
    pub(super) fingerprint: sha256::Digest,
    pub(super) active: bool,
}
