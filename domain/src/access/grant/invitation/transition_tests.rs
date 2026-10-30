#![allow(clippy::expect_used)]

use std::sync::Arc;

use chrono::DateTime;
use platform::cryptography::sha256;
use serde::de::DeserializeOwned;
use serde_json::json;
use zeroize::Zeroizing;

use super::*;
use crate::{
    access::grant::invitation::{Code, state},
    history::{Keys, Kind, Record as _, Seed, State as _},
    identity::authentication,
    keys::{self, Derived, Entry, Material, seal},
};

fn id<T: DeserializeOwned>(value: u8) -> T {
    serde_json::from_value(json!(format!("00000000-0000-4000-8000-{value:012}"))).expect("fixed ID")
}

fn creation(inventory: &keys::Keys) -> Creation {
    let now = DateTime::from_timestamp_micros(1_700_000_000_123_456).expect("time");
    let expiry = DateTime::from_timestamp_micros(1_700_003_600_123_456).expect("expiry");
    let plaintext: Code =
        serde_json::from_value(json!("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
            .expect("32 zero bytes");
    let code = plaintext
        .seal(&inventory.invitation, id(1), id(3), 3)
        .expect("encrypted original");
    Creation {
        id: id(1),
        person_id: id(2),
        grant_id: id(3),
        epoch: 3,
        role_id: id(4),
        change_id: id(5),
        operator_id: id(6),
        capacity: serde_json::from_value(json!(2)).expect("capacity"),
        expires_at: serde_json::from_value(json!(expiry)).expect("expiry"),
        code: Arc::new(code),
        plaintext: Arc::new(plaintext),
        hash: [90; 32],
        key_id: keys::Id::new(0).expect("key"),
        created_at: now,
        operation_id: id(7),
    }
}

fn check<K: Kind>(value: &Verified<K>, length: usize, digest: &str, tag: &str) {
    let fields = value.fields();
    let encoded = seal::encode_fields(
        K::NAME,
        K::VERSION,
        &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    )
    .expect("encoding");
    assert_eq!(encoded.as_ref().len(), length);
    assert_eq!(hex::encode(sha256::Digest::new(&encoded).as_ref()), digest);
    assert_eq!(hex::encode(value.seal().tag()), tag);
}

#[test]
fn equal_time_transitions_match_independent_seal_vectors_and_preserve_boundaries() {
    let inventory = authentication::inventory(authentication::keys());
    let key_id = keys::Id::new(0).expect("key ID");
    let seals = seal::Keys::new(Derived::fixed(
        key_id,
        vec![Entry::new(
            key_id,
            Material(Zeroizing::new(std::array::from_fn(|index| {
                u8::try_from(index).expect("32 bytes")
            }))),
        )],
    ));
    let keys = Keys {
        data: &inventory.encryption,
        seals: &seals,
    };
    let creation = creation(&inventory);
    let now = creation.created_at;
    let expiry = creation.expires_at;
    let (created, first) = Seed::change(create(creation.clone()), keys).expect("creation");
    // Python UUID.bytes, struct.pack(">I"), SHA-256 and HMAC; key bytes 0..31.
    check(
        &created,
        389,
        "4a0be4a71395790383170e32cc40acfb2155381f7b429bea35b9384497000d86",
        "505f3ffba5e7bd65ffab171a1b668ecbeadc0f1170d17e1503dd8ca381220856",
    );
    check(
        &first,
        272,
        "25647f9d3e19f104bf7dcd58e9089aa8fe7729b35a4db2c975e2bb5b610a5d16",
        "08bf49642ae6f39a26300de80386207953772ffc8cb75b4ababcc497f5b3cf38",
    );
    let mut reencrypted = creation;
    reencrypted.code = Arc::new(
        reencrypted
            .plaintext
            .seal(&inventory.invitation, id(1), id(3), 3)
            .expect("new envelope"),
    );
    let (same, _) = Seed::change(create(reencrypted), keys).expect("same plaintext");
    assert_eq!(
        same.seal(),
        created.seal(),
        "envelope randomness is not a business fact"
    );
    assert!(
        accept(
            &created,
            id(9),
            id(6),
            id(10),
            now - chrono::Duration::microseconds(1)
        )
        .is_err()
    );
    assert!(accept(&created, id(9), id(6), id(10), expiry.into()).is_err());
    assert!(withdraw(&created, id(6), id(10), expiry.into()).is_err());
    let capacity = |value| serde_json::from_value(json!(value)).expect("capacity");
    assert!(
        adjust(
            &created,
            Adjustment {
                id: id(8),
                capacity: capacity(3),
                expires_at: expiry
            },
            id(6),
            id(11),
            expiry.into()
        )
        .is_err()
    );
    assert!(
        adjust(
            &created,
            Adjustment {
                id: id(8),
                capacity: capacity(2),
                expires_at: expiry
            },
            id(6),
            id(11),
            now
        )
        .is_err()
    );
    let (accepted, second) = Seed::change(
        accept(&created, id(9), id(6), id(10), now).expect("same-time acceptance"),
        keys,
    )
    .expect("sealed acceptance");
    check(
        &accepted,
        389,
        "e75020722416ed146692522186f73c9607291d4083d760177b737791e70e4a8e",
        "50099aef72770f927bf32f317801299e2e49b862e3919c8bf2d5f9f8f295deed",
    );
    check(
        &second,
        290,
        "00668d642d7689f25b013d88b2717479651f29e15aee4afe7bd8aabe0ce816c9",
        "5557d7dfaca871ae2cd2a5051305942ba23378bdde8c165f886d10413d321a7a",
    );
    assert!(second.follows(&first));
    assert_eq!(accepted.used(), 1);
    let (full, _) = Seed::change(
        accept(&accepted, id(13), id(6), id(14), now).expect("last unit"),
        keys,
    )
    .expect("full state");
    assert!(accept(&full, id(15), id(6), id(16), now).is_err());
    assert!(
        adjust(
            &full,
            Adjustment {
                id: id(8),
                capacity: capacity(1),
                expires_at: expiry
            },
            id(6),
            id(11),
            now
        )
        .is_err()
    );
    let (adjusted, third) = Seed::change(
        adjust(
            &accepted,
            Adjustment {
                id: id(8),
                capacity: capacity(3),
                expires_at: expiry,
            },
            id(6),
            id(11),
            now,
        )
        .expect("same-time adjustment"),
        keys,
    )
    .expect("sealed adjustment");
    check(
        &adjusted,
        389,
        "fb28cc039cf3f0ef6a9646829101cc7233d6d06945fa5764c074b2e1c78d4411",
        "da4ba6ded69c6207a7a6251d9e33e7db6c6b7c20f45fbd3fc356b22c9a69c812",
    );
    check(
        &third,
        290,
        "ef203a0b95d2cf2ad5e9588fd66e0c7e90f717647026603f9db5e21329e82ed6",
        "87093e46bbbf2c79ddefbcbd606f95d99e4a1d7458aec3d05bc25c7e94210f5d",
    );
    assert!(third.follows(&second));
    let (withdrawn, fourth) = Seed::change(
        withdraw(&adjusted, id(6), id(12), now).expect("same-time withdrawal"),
        keys,
    )
    .expect("sealed withdrawal");
    check(
        &withdrawn,
        405,
        "b523a00f6417543eb095d2270f4d13690abf2c37315a0fd49b29b7e2f6c954a4",
        "4527768245ec81b9204e7edbbf9e99c4b856bcf7c7a16e1f0c1de28e5e193685",
    );
    check(
        &fourth,
        306,
        "3d1f579501e6f141602f6b5607000fde35faf5d13877b578ee46a3f7b9534751",
        "1891eedc0154ecd6736926c77128f99938e20840fa470f808f074ce99651221f",
    );
    assert!(fourth.follows(&third));
    assert_eq!(
        (withdrawn.sequence(), withdrawn.version(), withdrawn.used()),
        (4, 4, 1)
    );
    assert!(withdraw(&withdrawn, id(6), id(13), now).is_err());
    assert!(accept(&withdrawn, id(9), id(6), id(13), now).is_err());
    assert!(
        adjust(
            &withdrawn,
            Adjustment {
                id: id(8),
                capacity: capacity(4),
                expires_at: expiry
            },
            id(6),
            id(13),
            now
        )
        .is_err()
    );
    let mut bad = adjusted.data().clone();
    bad.used = 0;
    let forged = Seed::record::<record::Record>(
        adjusted.id(),
        4,
        record::Data::from_state(&bad, id(13), id(6), Event::Acceptance { grant_id: id(9) }),
        keys,
    )
    .expect("signed but invalid delta");
    assert!(
        !forged.follows(&third),
        "valid seals do not replace transition invariants"
    );
    let rebuilt = Seed::state::<state::State>(withdrawn.data().clone(), 4, 4, keys)
        .expect("reconstructed fixture");
    assert_eq!(rebuilt.seal(), withdrawn.seal());
}
