//! `encryption` keys reserved for short-lived delivery of original credentials.

use std::collections::BTreeMap;

use platform::cryptography::envelope;
use zeroize::Zeroizing;

use crate::keys::{Derived, Id};

pub struct Keys {
    active: envelope::Key,
    retained: BTreeMap<Id, envelope::Key>,
}

impl Keys {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, retained) = derived.split(envelope::Key::new);
        Self { active, retained }
    }
    pub(crate) fn seal(&self, input: &impl envelope::Input) -> Result<envelope::Envelope, Error> {
        Ok(self.active.seal(input)?)
    }
    pub(crate) fn open(
        &self,
        value: &envelope::Envelope,
        binding: &impl envelope::Binding,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let id = value.header().key_id();
        let key = if id == self.active.id() {
            &self.active
        } else {
            self.retained.get(&id).ok_or(Error::Missing(id))?
        };
        Ok(key.open(value, binding)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing credential sealing key {0:?}")]
    Missing(Id),
    #[error("{0}")]
    Envelope(#[from] envelope::Error),
}
