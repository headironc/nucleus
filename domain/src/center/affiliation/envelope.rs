use platform::cryptography::envelope;

use crate::{
    center::affiliation::{Code, Id},
    database::sealed::Sealed,
    history::Error,
    identity::person,
    kernel::keys::Keys,
};

struct Binding([u8; 32]);

impl Binding {
    fn new(person_id: person::Id, id: Id) -> Self {
        let mut identity = [0; 32];
        identity[..16].copy_from_slice(person_id.as_bytes());
        identity[16..].copy_from_slice(id.as_bytes());
        Self(identity)
    }
}

impl envelope::Binding for Binding {
    fn domain(&self) -> &[u8] {
        b"nucleus/center-affiliation/v1"
    }
    fn identity(&self) -> &[u8] {
        &self.0
    }
    fn field(&self) -> &[u8] {
        b"affiliation.value"
    }
}

pub(super) fn seal(
    keys: &Keys,
    person_id: person::Id,
    id: Id,
    code: Code,
) -> Result<Sealed, Error> {
    struct Input {
        binding: Binding,
        code: Code,
    }
    impl envelope::Binding for Input {
        fn domain(&self) -> &[u8] {
            self.binding.domain()
        }
        fn identity(&self) -> &[u8] {
            self.binding.identity()
        }
        fn field(&self) -> &[u8] {
            self.binding.field()
        }
    }
    impl envelope::Input for Input {
        fn plaintext(&self) -> &[u8] {
            self.code.as_ref().as_bytes()
        }
    }
    Ok(Sealed(keys.seal(&Input {
        binding: Binding::new(person_id, id),
        code,
    })?))
}

pub(super) fn open(
    keys: &Keys,
    person_id: person::Id,
    id: Id,
    value: &Sealed,
) -> Result<Code, Error> {
    let plaintext = keys.open(&value.0, &Binding::new(person_id, id))?;
    Ok(std::str::from_utf8(&plaintext)?.parse()?)
}
