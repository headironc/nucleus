//! Independent LP32/SHA-256/HMAC vectors for the number owners' frozen encodings.

#![allow(clippy::expect_used)]

use chrono::DateTime;
use serde::de::DeserializeOwned;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::{
    Id, Number, Revision, Subtype,
    record::{self, Event},
    revision, state,
};
use crate::{
    history::{Kind, Seed, Verified},
    keys::{self, seal},
};

fn id<T: DeserializeOwned>(byte: u8) -> T {
    serde_json::from_value(serde_json::json!(Uuid::from_bytes([byte; 16]))).expect("fixed ID")
}

fn check<K: Kind>(value: &Verified<K>, count: usize, length: usize, digest: &str, tag: &str) {
    let fields = value.fields();
    let bytes = seal::encode_fields(
        K::NAME,
        K::VERSION,
        &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    )
    .expect("encoding");
    assert_eq!(fields.len(), count);
    assert_eq!(bytes.as_ref().len(), length);
    assert_eq!(
        hex::encode(platform::cryptography::sha256::Digest::new(&bytes).as_ref()),
        digest
    );
    assert_eq!(hex::encode(value.seal().tag()), tag);
}

#[test]
fn encodings_match_independent_python_vectors() {
    let key = keys::Id::new(0).expect("key");
    let seals = seal::Keys::new(keys::Derived::fixed(
        key,
        vec![keys::Entry::new(
            key,
            keys::Material(Zeroizing::new(std::array::from_fn(|index| {
                u8::try_from(index).expect("byte")
            }))),
        )],
    ));
    let inventory = keys::tests::fixture();
    let keys = crate::history::Keys {
        data: &inventory.encryption,
        seals: &seals,
    };
    let time = DateTime::from_timestamp_micros(1_700_000_000_123_456).expect("time");
    let number = Revision {
        id: id::<Id>(2),
        number: Number::new(Subtype::Internship, 2024, 42).expect("number"),
    };
    // Python struct.pack(">I"), UUID bytes, hashlib.sha256 and hmac.digest; key bytes 0..31.
    let state = Seed::state::<state::State>(
        state::Data {
            id: id(1),
            revision: number,
            held: true,
            tenure_id: id(3),
            changed_at: time,
        },
        5,
        4,
        keys,
    )
    .expect("state");
    check(
        &state,
        10,
        163,
        "96959bdc5315f313e57bd7010e6b968a1a2e5fcd9eb502cb2b8995c50f16bb1e",
        "68830326e59b6b3ef9bcb3637f2e0e349fd809fbd7454cf4c60f0df37e008fee",
    );
    let reason = || "Verified".to_owned().try_into().expect("reason");
    for (event, held, count, length, digest, tag) in [
        (
            Event::Allocation {
                source: super::Source::SelfService,
                reason: None,
            },
            true,
            13,
            258,
            "4006570d72a262fecc0f88f0b06aff706ba619b71dcbfd3c1354bd7a35ede4e5",
            "bba036757f08a5277fd5de6eb6b165d87ba36843c35ea6c22b9caee2537d7b6a",
        ),
        (
            Event::Correction {
                before_id: id(8),
                reason: reason(),
            },
            true,
            13,
            270,
            "b79da78cd7ae2e27b32833232a924a33064902a3122899f8dd44d6b4b7d85f3c",
            "3f2415b6141fc2c4fb765c7b821cded07f084c0912b7e116ad743a6c2474d321",
        ),
        (
            Event::Recovery {
                origin_id: id(8),
                occupier_id: id(9),
                revocation_id: Some(id(10)),
                reason: reason(),
            },
            true,
            15,
            308,
            "f04a1527c8275bdcb085ebcde1995ae5bbd151ca852403ee87fc6f4b7690fd6f",
            "f81eb330d079e8a0e321b450e8c3803f620b25b267344d0eff644e51729c9fa4",
        ),
        (
            Event::Recovery {
                origin_id: id(8),
                occupier_id: id(9),
                revocation_id: None,
                reason: reason(),
            },
            true,
            15,
            292,
            "25e0c10eda1997ab02b7ed998fed7ff20dbcb922ce10c42488a5e3bf8205c33f",
            "d6aeaca303edaf95f9d17559cd16163b2923c774f69ede7cd7b6bbe310ee3890",
        ),
        (
            Event::Revocation {
                recovery_id: id(4),
                reason: reason(),
            },
            false,
            13,
            271,
            "dfdb277653c0881ee7c4e788fa50643bf117d3d190652fc3ea04e0e43abf4702",
            "fdda2db318361c0d792985bf695d10a978d89916364cebe8cd2137a946045427",
        ),
    ] {
        let record = Seed::record::<record::Record>(
            id(1),
            5,
            record::Data {
                id: id(4),
                revision_id: id(5),
                number_id: number.id,
                operator_id: id(6),
                time,
                operation_id: id(7),
                tenure_id: id(3),
                held,
                event,
            },
            keys,
        )
        .expect("record");
        check(&record, count, length, digest, tag);
    }
    for (creation, digest, tag) in [
        (
            revision::Creation::Allocation(id(4)),
            "06096be0618a0627d5b1e0b628b428a64160c0a1f11dc7e2f7bc5f58089aa229",
            "30474e821ed2d65c0c95df95db35baf10517be0f969cf580c7339b77ad574c2b",
        ),
        (
            revision::Creation::Correction(id(4)),
            "aeec9cc9180fb0a6053260e60a16195ff5314639ad7a7b90830654324c36229f",
            "70def13515c8b5c44a0018765c1fe231a8f9d2545d0d17bb7c4b7df514a3871d",
        ),
        (
            revision::Creation::Recovery(id(4)),
            "97c003c5b5098304346886a97740c89c11e58f60664fc22d624d96e9615a5e8b",
            "d1cd649089dffbc31661cf862cc4fa059485abcfd1df7d220f7e47ed76c3d20a",
        ),
    ] {
        let value = Seed::value::<revision::Value>(
            revision::Data {
                person_id: id(1),
                revision: number,
                creation,
            },
            keys,
        )
        .expect("value");
        check(&value, 8, 135, digest, tag);
    }
}
