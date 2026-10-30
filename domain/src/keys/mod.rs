//! The versioned application root and every key use derived from it.

pub mod database;
pub mod input;
pub mod metadata;
pub mod seal;

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub use platform::cryptography::key::Id;
use platform::cryptography::{self, Encoded, hkdf, sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{access::grant::invitation, identity, kernel, keys::metadata::Metadata};

/// Each capability owns its use-specific key sets; all of them derive from one validated root.
pub struct Keys {
    pub identity: identity::keys::Keys,
    pub encryption: kernel::keys::Keys,
    pub invitation: invitation::keys::Keys,
    pub seal: seal::Keys,
    metadata: Metadata,
}

impl Keys {
    /// Validates the root versions and derives every use for each of them.
    pub fn new(active_id: Id, entries: Vec<Entry>) -> Result<Self, Error> {
        let root = Root::new(active_id, entries)?;
        Ok(Self {
            identity: identity::keys::Keys {
                challenge: identity::keys::challenge::Keys::new(root.derive(Use::Lookup)?),
                mobile: identity::keys::mobile::Keys::new(root.derive(Use::Lookup)?),
                encryption: identity::keys::encryption::Keys::new(root.derive(Use::Encryption)?),
                message: identity::keys::message::Keys::new(root.derive(Use::Proof)?),
                input: identity::keys::request::Keys::new(root.derive(Use::Proof)?),
                credential: identity::keys::credential::Keys::new(root.derive(Use::Encryption)?),
                number: identity::keys::number::Keys::new(root.derive(Use::Lookup)?),
                avatar: identity::keys::avatar::Keys::new(root.derive(Use::Link)?),
            },
            encryption: kernel::keys::Keys::new(root.derive(Use::Encryption)?),
            invitation: invitation::keys::Keys {
                data: invitation::keys::Data::new(root.derive(Use::Encryption)?),
                lookup: invitation::keys::Lookup::new(root.derive(Use::Lookup)?),
            },
            seal: seal::Keys::new(root.derive(Use::Seal)?),
            metadata: root.metadata,
        })
    }

    /// The non-secret inventory of root versions that the database must match.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

pub struct Material(pub(crate) Zeroizing<[u8; 32]>);

impl Material {
    pub fn new(text: Zeroizing<String>) -> Result<Self, MaterialError> {
        if text.len() != 43 {
            return Err(MaterialError);
        }
        let mut material = Zeroizing::new([0; 32]);
        match URL_SAFE_NO_PAD.decode_slice(text.as_bytes(), &mut material[..]) {
            Ok(32) => Ok(Self(material)),
            _ => Err(MaterialError),
        }
    }
}

#[derive(Debug, Error)]
#[error("invalid key material")]
pub struct MaterialError;

/// One root version as declared in the key file.
pub struct Entry {
    pub(crate) id: Id,
    pub(crate) material: Material,
}

impl Entry {
    pub fn new(id: Id, material: Material) -> Self {
        Self { id, material }
    }
}

fn validate(active_id: Id, entries: &[Entry]) -> Result<(), Error> {
    if entries.len() > 256 {
        return Err(Error::Count);
    }
    for (index, entry) in entries.iter().enumerate() {
        for previous in &entries[..index] {
            if entry.id == previous.id {
                return Err(Error::DuplicateId(entry.id));
            }
            if bool::from(entry.material.0[..].ct_eq(&previous.material.0[..])) {
                return Err(Error::DuplicateMaterial {
                    first_id: previous.id,
                    second_id: entry.id,
                });
            }
        }
    }
    if !entries.iter().any(|entry| entry.id == active_id) {
        return Err(Error::Active(active_id));
    }
    Ok(())
}

/// The closed set of derived uses; each name is its frozen HKDF info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Use {
    Encryption,
    Lookup,
    Proof,
    Seal,
    Link,
}

impl Use {
    fn info(self) -> &'static [u8] {
        match self {
            Self::Encryption => b"encryption",
            Self::Lookup => b"lookup",
            Self::Proof => b"proof",
            Self::Seal => b"seal",
            Self::Link => b"link",
        }
    }
}

const SALT: &[u8] = b"nucleus/key/v1";

/// Root material lives only while the uses are derived; afterwards only fingerprints remain.
struct Root {
    active: (Id, Material),
    retained: BTreeMap<Id, Material>,
    metadata: Metadata,
}

impl Root {
    fn new(active_id: Id, entries: Vec<Entry>) -> Result<Self, Error> {
        validate(active_id, &entries)?;
        let mut versions = BTreeMap::new();
        let mut retained = BTreeMap::new();
        for Entry { id, material } in entries {
            versions.insert(id, fingerprint(id, &material.0)?);
            retained.insert(id, material);
        }
        let material = retained
            .remove(&active_id)
            .ok_or(Error::Active(active_id))?;
        Ok(Self {
            active: (active_id, material),
            retained,
            metadata: Metadata::new(active_id, versions),
        })
    }

    fn derive(&self, kind: Use) -> Result<Derived, cryptography::Error> {
        let (active_id, material) = &self.active;
        let mut retained = BTreeMap::new();
        for (id, material) in &self.retained {
            retained.insert(*id, hkdf::derive(SALT, &material.0, kind.info())?);
        }
        Ok(Derived {
            active: (*active_id, hkdf::derive(SALT, &material.0, kind.info())?),
            retained,
        })
    }
}

/// One use's derived material for the active root version and every retained one.
pub(crate) struct Derived {
    active: (Id, Zeroizing<[u8; 32]>),
    retained: BTreeMap<Id, Zeroizing<[u8; 32]>>,
}

impl Derived {
    /// Builds the use's key for each version, keeping the active version apart.
    pub(crate) fn split<K>(
        self,
        mut build: impl FnMut(Id, Zeroizing<[u8; 32]>) -> K,
    ) -> (K, BTreeMap<Id, K>) {
        let (active_id, material) = self.active;
        let active = build(active_id, material);
        let retained = self
            .retained
            .into_iter()
            .map(|(id, material)| (id, build(id, material)))
            .collect();
        (active, retained)
    }

    /// Use material given directly, for owners' vectors over a fixed use key.
    #[cfg(test)]
    #[allow(clippy::expect_used)]
    pub(crate) fn fixed(active_id: Id, entries: Vec<Entry>) -> Self {
        validate(active_id, &entries).expect("valid fixed use keys");
        let mut retained: BTreeMap<_, _> = entries
            .into_iter()
            .map(|Entry { id, material }| (id, material.0))
            .collect();
        let material = retained.remove(&active_id).expect("active use key");
        Self {
            active: (active_id, material),
            retained,
        }
    }
}

fn fingerprint(key_id: Id, material: &[u8; 32]) -> Result<sha256::Digest, cryptography::Error> {
    struct Input<'a> {
        key_id: [u8; 8],
        material: &'a [u8; 32],
    }
    impl cryptography::Input<4> for Input<'_> {
        fn fields(&self) -> [&[u8]; 4] {
            [
                b"nucleus/identity/key/v1",
                &self.key_id,
                b"root",
                self.material,
            ]
        }
    }
    Ok(sha256::Digest::new(&Encoded::new(&Input {
        key_id: key_id.as_ref().to_be_bytes(),
        material,
    })?))
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("key set exceeds 256 entries")]
    Count,
    #[error("duplicate key id {0:?}")]
    DuplicateId(Id),
    #[error("duplicate key material at {first_id:?} and {second_id:?}")]
    DuplicateMaterial { first_id: Id, second_id: Id },
    #[error("active key {0:?} is not declared")]
    Active(Id),
    #[error("cannot fingerprint or derive key material: {0}")]
    Crypto(#[from] cryptography::Error),
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]

    use platform::cryptography::envelope;
    use zeroize::Zeroizing;

    use crate::keys::{Entry, Error, Id, Keys, Material, Root, Use, seal::Seal, validate};

    /// The fixed root every domain test database is initialized with.
    pub(crate) fn fixture() -> Keys {
        let id = Id::new(0).expect("id");
        Keys::new(id, vec![Entry::new(id, Material(Zeroizing::new([1; 32])))])
            .expect("fixture root")
    }

    fn sequential() -> Material {
        Material(Zeroizing::new(std::array::from_fn(|index| {
            u8::try_from(index).expect("32 bytes")
        })))
    }

    #[test]
    fn constructs_only_canonical_material() {
        let material = Material::new(Zeroizing::new(
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8".into(),
        ))
        .expect("fixed bytes 0 through 31");
        assert_eq!(&material.0[..], &(0_u8..32).collect::<Vec<_>>());
        for text in [
            String::new(),
            "sensitive-material".into(),
            format!("{}=", "A".repeat(43)),
            "/".repeat(43),
            "+".repeat(43),
            format!("{}B", "A".repeat(42)),
            "A".repeat(42),
            "A".repeat(44),
        ] {
            let error = match Material::new(Zeroizing::new(text)) {
                Err(error) => error,
                Ok(_) => panic!("invalid material accepted"),
            };
            assert_eq!(format!("{error}"), "invalid key material");
        }
    }

    fn entries(count: u16) -> Vec<Entry> {
        (0..count)
            .map(|id| {
                let mut material = [0; 32];
                material[..2].copy_from_slice(&id.to_be_bytes());
                Entry::new(
                    Id::new(i64::from(id)).expect("id"),
                    Material(Zeroizing::new(material)),
                )
            })
            .collect()
    }

    #[test]
    fn rejects_invalid_key_sets() {
        let id = Id::new(0).expect("id");
        let mut duplicate = entries(2);
        duplicate[1].id = id;
        assert!(matches!(
            validate(id, &duplicate),
            Err(Error::DuplicateId(_))
        ));
        let shared = vec![
            entries(1).remove(0),
            Entry::new(Id::new(1).expect("id"), Material(Zeroizing::new([0; 32]))),
        ];
        assert!(matches!(
            validate(id, &shared),
            Err(Error::DuplicateMaterial { .. })
        ));
        assert!(matches!(validate(id, &[]), Err(Error::Active(_))));
        assert!(matches!(
            Keys::new(Id::new(2).expect("id"), entries(1)),
            Err(Error::Active(_))
        ));
        assert!(Keys::new(id, entries(256)).is_ok());
        assert!(matches!(Keys::new(id, entries(257)), Err(Error::Count)));
        assert!(Id::new(-1).is_err());
        assert!(Id::new(i64::MAX).is_ok());
    }

    #[test]
    fn fingerprints_each_root_version_with_the_fixed_root_purpose() {
        // Python hashlib over LP32("nucleus/identity/key/v1", i64be(id), "root", bytes 0..31).
        for (id, expected) in [
            (
                0,
                "87c83764e937aeb8e3ebfbf9f79c85a4aef1da267ed3341bace6a7b90f1db728",
            ),
            (
                i64::MAX,
                "0735af8aa85b0b202110cf62ed6c05285e22f9fc64210c851925e1893f38f949",
            ),
        ] {
            let id = Id::new(id).expect("id");
            let keys = Keys::new(id, vec![Entry::new(id, sequential())]).expect("root");
            let versions: Vec<_> = keys.metadata().versions().collect();
            assert_eq!(versions.len(), 1);
            assert_eq!(versions[0].0, id);
            assert!(versions[0].2, "the only version is active");
            assert_eq!(hex::encode(versions[0].1.as_ref()), expected);
        }
    }

    #[test]
    fn derives_each_use_with_independent_hkdf_vectors() {
        // Python RFC 5869 HKDF-SHA-256: salt "nucleus/key/v1", ikm bytes 0..31, info the use name.
        let id = Id::new(0).expect("id");
        let root = Root::new(id, vec![Entry::new(id, sequential())]).expect("root");
        for (kind, expected) in [
            (
                Use::Encryption,
                "e274e356668ac4e10092ed98b843d18e7c16126479dcdbec9039e914bfcde61b",
            ),
            (
                Use::Lookup,
                "1ed7ac6e6e3b7f3330eb9434260c6e15037d9663bce2bba06dc220e9bf83a84a",
            ),
            (
                Use::Proof,
                "3c84651daa3ee6afcabd424d4b10370c43cff6bea107a239f95922363be3aada",
            ),
            (
                Use::Seal,
                "52a93befce6218d78674aec7f34fdeb07a0479180126c4b70744646724985c9a",
            ),
            (
                Use::Link,
                "1459138e6bf67b0f3e29a8533d6d812d455a24ce97f8bdb09677e442d74c83a8",
            ),
        ] {
            let derived = root.derive(kind).expect("derive");
            assert_eq!(derived.active.0, id);
            assert_eq!(hex::encode(&derived.active.1[..]), expected);
            assert!(derived.retained.is_empty());
        }
    }

    #[test]
    fn independent_vectors_through_the_root_for_lookups_seal_and_envelope() {
        // Python: the HKDF above, then hmac.digest over LP32 fields and AESGCMSIV with the
        // four-field AAD LP32(domain, header, identity, field); root bytes 0..31.
        let id = Id::new(0).expect("id");
        let keys = Keys::new(id, vec![Entry::new(id, sequential())]).expect("root");
        let mobile = "13800000000".parse().expect("mobile");
        assert_eq!(
            hex::encode(keys.identity.mobile.hash(&mobile).expect("hash").as_ref()),
            "457ec8af561ebe19cb7b7c68cae659077204fddce68350d3fa28a7d538ae013a"
        );
        let number = "000000200002290010"
            .parse()
            .expect("fictitious valid number");
        assert_eq!(
            hex::encode(keys.identity.number.hash(&number).expect("hash").as_ref()),
            "3663341a08466888636b0e1c4595228b9750b37c6f2a7e6211ca05560c82a3f0"
        );
        let seal = keys
            .seal
            .sign("test.fixture", 1, &[b"a", b""])
            .expect("seal");
        assert_eq!(
            hex::encode(seal.tag()),
            "8b4e89e68c584fedc8b6f39cc62241fa399c101e44ceaacb9b1a78b66bfe8683"
        );
        struct Binding;
        impl envelope::Binding for Binding {
            fn domain(&self) -> &[u8] {
                b"nucleus/identity/envelope/v1"
            }
            fn identity(&self) -> &[u8] {
                &[7; 16]
            }
            fn field(&self) -> &[u8] {
                b"account.mobile"
            }
        }
        let sealed = envelope::Envelope::new(
            hex::decode("010000000000000000000102030405060708090a0bbd41d99b9be1fe5867a6222d10b0c91d21611eb2b3282fb93460fd")
                .expect("vector"),
        )
        .expect("envelope");
        assert_eq!(
            &keys
                .identity
                .encryption
                .open(&sealed, &Binding)
                .expect("open")[..],
            b"13800000000"
        );
    }

    #[test]
    fn every_use_switches_with_the_root_and_retained_versions_still_verify() {
        let old = Id::new(0).expect("old");
        let new = Id::new(1).expect("new");
        let original = Keys::new(old, vec![Entry::new(old, sequential())]).expect("original");
        let keys = Keys::new(
            new,
            vec![
                Entry::new(old, sequential()),
                Entry::new(new, Material(Zeroizing::new([42; 32]))),
            ],
        )
        .expect("rotated");
        assert_eq!(
            keys.metadata()
                .versions()
                .map(|(id, _, active)| (id, active))
                .collect::<Vec<_>>(),
            [(old, false), (new, true)]
        );
        assert_eq!(
            keys.metadata().versions().next().expect("old").1,
            original.metadata().versions().next().expect("old").1,
            "a version's fingerprint does not depend on the active choice"
        );
        let mobile = "13800000000".parse().expect("mobile");
        assert_ne!(
            original
                .identity
                .mobile
                .hash(&mobile)
                .expect("hash")
                .as_ref(),
            keys.identity.mobile.hash(&mobile).expect("hash").as_ref(),
            "lookups use only the active root"
        );
        let fields: &[&[u8]] = &[b"a", b""];
        let seal = original.seal.sign("test.fixture", 1, fields).expect("seal");
        keys.seal
            .check("test.fixture", 1, fields, &seal)
            .expect("retained root verifies");
        let only_new = Keys::new(
            new,
            vec![Entry::new(new, Material(Zeroizing::new([42; 32])))],
        )
        .expect("new only");
        assert!(
            only_new
                .seal
                .check("test.fixture", 1, fields, &seal)
                .is_err()
        );
        assert_eq!(
            Seal::stored(seal.tag().to_vec(), seal.key_id(), seal.version()).expect("stored"),
            seal
        );
    }
}
