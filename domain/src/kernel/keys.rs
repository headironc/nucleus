//! Versioned encryption keys for personal data, state records and original-operation inputs.
//! Record owners supply the domain and identity of each envelope.

use std::collections::BTreeMap;

use platform::cryptography::envelope;
use zeroize::Zeroizing;

use crate::keys::{Derived, Id};

/// `encryption` versions; the active one seals and every retained one still opens.
pub struct Keys {
    active: envelope::Key,
    retained: BTreeMap<Id, envelope::Key>,
}

impl Keys {
    pub(crate) fn new(derived: Derived) -> Self {
        let (active, retained) = derived.split(envelope::Key::new);
        Self { active, retained }
    }

    fn fetch(&self, key_id: Id) -> Result<&envelope::Key, Error> {
        if key_id == self.active.id() {
            return Ok(&self.active);
        }
        self.retained.get(&key_id).ok_or(Error::Missing(key_id))
    }

    pub(crate) fn seal(&self, input: &impl envelope::Input) -> Result<envelope::Envelope, Error> {
        Ok(self.active.seal(input)?)
    }

    pub(crate) fn open(
        &self,
        value: &envelope::Envelope,
        binding: &impl envelope::Binding,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        Ok(self.fetch(value.header().key_id())?.open(value, binding)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing record encryption key {0:?}")]
    Missing(Id),
    #[error("{0}")]
    Envelope(#[from] envelope::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use crate::{
        kernel::keys::Keys,
        keys::{Derived, Entry, Id, Material},
    };

    #[test]
    fn opens_only_declared_versions() {
        let id = Id::new(0).expect("id");
        let keys = Keys::new(Derived::fixed(
            id,
            vec![Entry::new(
                id,
                Material(Zeroizing::new(std::array::from_fn(|i| {
                    u8::try_from(i).expect("32 bytes")
                }))),
            )],
        ));
        assert_eq!(keys.active.id(), id);
        assert!(keys.fetch(id).is_ok());
        assert!(keys.fetch(Id::new(1).expect("id")).is_err());
    }
}
