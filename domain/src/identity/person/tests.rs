#![allow(clippy::expect_used)]

use chrono::{DateTime, TimeDelta};
use tokio_postgres::types::{FromSql, Type};
use uuid::Uuid;

use super::{Id, record, revision, state, transition};
use crate::{
    history::{self, Kind as _, Record as _, Seed, State as _, Verified},
    identity::keys::encryption,
    keys::{Derived, Entry, Id as KeyId, Material, seal},
};

fn id<T: for<'a> FromSql<'a>>(value: u8) -> T {
    let uuid = format!("00000000-0000-4000-8000-{value:012x}")
        .parse::<Uuid>()
        .expect("fixed UUID");
    T::from_sql(&Type::UUID, uuid.as_bytes()).expect("stored identifier")
}

fn derived() -> Derived {
    let id = KeyId::new(0).expect("key ID");
    Derived::fixed(
        id,
        vec![Entry::new(
            id,
            Material(zeroize::Zeroizing::new(std::array::from_fn(|index| {
                u8::try_from(index).expect("32 key bytes")
            }))),
        )],
    )
}

fn vector(
    record: &Verified<record::Record>,
    length: usize,
    digest: &str,
    tag: &str,
    keys: history::Keys<'_>,
) {
    let fields = record.fields();
    let encoded = seal::encode_fields(
        record::Record::NAME,
        record::Record::VERSION,
        &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    )
    .expect("record encoding");
    assert_eq!(encoded.as_ref().len(), length);
    assert_eq!(
        hex::encode(platform::cryptography::sha256::Digest::new(&encoded).as_ref()),
        digest
    );
    assert_eq!(hex::encode(record.seal().tag()), tag);
    for index in 0..fields.len() {
        let mut changed = record.fields();
        changed[index].push(0);
        assert!(
            keys.seals
                .check(
                    record::Record::NAME,
                    record::Record::VERSION,
                    &changed.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                    record.seal(),
                )
                .is_err(),
            "field {index}"
        );
    }
}

#[test]
fn equal_time_transitions_keep_explicit_versions_and_independent_record_vectors() {
    let data = encryption::Keys::new(derived());
    let seals = seal::Keys::new(derived());
    let keys = history::Keys {
        data: &data,
        seals: &seals,
    };
    let at = DateTime::from_timestamp_micros(1_700_000_000_123_456).expect("time");
    let person: Id = id(1);
    let first: revision::Id = id(2);
    let second: revision::Id = id(3);
    let third: revision::Id = id(4);
    let operator = id(5);
    let account = id(6);
    let mobile = "13800000000".parse().expect("fictional mobile");

    let (created, origin) =
        Seed::change(transition::create(person, first, account, id(8), at), keys)
            .expect("creation");
    assert_eq!(
        (created.sequence(), created.version(), created.revision_id()),
        (1, 1, first)
    );
    assert!(created.has_head(&origin));

    let (corrected, correction) = Seed::change(
        transition::correct(&created, first, second, operator, None, id(9), at)
            .expect("equal-time correction"),
        keys,
    )
    .expect("correction");
    assert_eq!(
        (
            corrected.sequence(),
            corrected.version(),
            corrected.revision_id()
        ),
        (2, 2, second)
    );
    assert!(correction.follows(&origin));
    assert!(
        matches!(correction.event(), record::Event::Correction { before_id, revision_id, .. }
        if *before_id == first && *revision_id == second)
    );

    let (cancelled, cancellation) = Seed::change(
        transition::cancel(
            &corrected,
            second,
            id(7),
            account,
            operator,
            &mobile,
            id(10),
            at,
        )
        .expect("equal-time cancellation"),
        keys,
    )
    .expect("cancellation");
    assert_eq!(
        (
            cancelled.sequence(),
            cancelled.version(),
            cancelled.revision_id()
        ),
        (3, 3, second)
    );
    assert!(cancellation.follows(&correction));

    let (later, subsequent) = Seed::change(
        transition::correct(
            &cancelled,
            second,
            third,
            operator,
            Some(id(12)),
            id(11),
            at,
        )
        .expect("accountless Person remains correctable"),
        keys,
    )
    .expect("later correction");
    assert_eq!(
        (later.sequence(), later.version(), later.revision_id()),
        (4, 4, third)
    );
    assert!(subsequent.follows(&cancellation));
    assert!(later.has_head(&subsequent));
    assert!(!later.has_head(&cancellation));

    // Independently computed with Python struct.pack('>I' / '>q'), UUID bytes and hashlib/hmac.
    vector(
        &origin,
        164,
        "dfef0f14eadbc4f27076c2ad5b9e0c555970f55d42e5c9caae5231a6f9ff48c9",
        "23671570e685657303980605f3f3e221c3ba58363b12f4629ac9c0f51fe4c224",
        keys,
    );
    vector(
        &correction,
        192,
        "aca5b12d8794f8b567db2366b2071cd73f2df660563b89a0e68bc2f881248f2f",
        "48fb9657aa4dda5436699a2a5ee1f524deb46e50564f1623c46bffda09096dab",
        keys,
    );
    vector(
        &cancellation,
        225,
        "c296f9636dce40a01b94a53a88c7573a6fe0af015fbb81d5bf36b05dfaadcc41",
        "71ce075b6ea1f10f1061d31f9598de216d3884bc35a94e8f1dd711d47dbe2913",
        keys,
    );
    vector(
        &subsequent,
        208,
        "ea0f06cc42398804b391434725c2ff0cc1c0c9f0313ec26813e3729f9d9f3622",
        "1b667fdd90df595114b747f43174ed076bfaff157cba91340bb7aadd87f1d410",
        keys,
    );

    let (_, different_origin) =
        Seed::change(transition::create(person, id(99), account, id(8), at), keys)
            .expect("different lineage fixture");
    assert!(!correction.follows(&different_origin));
}

#[test]
fn transitions_reject_stale_versions_backward_time_and_exhausted_counters() {
    let data = encryption::Keys::new(derived());
    let seals = seal::Keys::new(derived());
    let keys = history::Keys {
        data: &data,
        seals: &seals,
    };
    let at = DateTime::from_timestamp_micros(1_700_000_000_123_456).expect("time");
    let first = id(2);
    let mobile = "13800000000".parse().expect("fictional mobile");
    let (created, _) =
        Seed::change(transition::create(id(1), first, id(6), id(8), at), keys).expect("creation");
    assert!(transition::correct(&created, id(99), id(3), id(5), None, id(9), at).is_err());
    assert!(transition::correct(&created, first, first, id(5), None, id(9), at).is_err());
    assert!(
        transition::cancel(&created, id(99), id(7), id(6), id(5), &mobile, id(10), at).is_err()
    );
    let earlier = at - TimeDelta::microseconds(1);
    assert!(transition::correct(&created, first, id(3), id(5), None, id(9), earlier).is_err());
    assert!(
        transition::cancel(
            &created,
            first,
            id(7),
            id(6),
            id(5),
            &mobile,
            id(10),
            earlier
        )
        .is_err()
    );

    for (sequence, version) in [(i64::MAX, 1), (1, i64::MAX)] {
        let exhausted = Seed::state::<state::State>(
            state::Data {
                id: id(1),
                revision_id: first,
                changed_at: at,
            },
            sequence,
            version,
            keys,
        )
        .expect("exhausted signed fixture");
        assert!(transition::correct(&exhausted, first, id(3), id(5), None, id(9), at).is_err());
        assert!(
            transition::cancel(&exhausted, first, id(7), id(6), id(5), &mobile, id(10), at)
                .is_err()
        );
    }
    assert_eq!(
        (created.sequence(), created.version(), created.revision_id()),
        (1, 1, first)
    );
}
