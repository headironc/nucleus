//! Structurally checked envelope storage; record owners supply authentication bindings.

use std::fmt;

use bytes::BytesMut;
use platform::cryptography::envelope;
use tokio_postgres::types::{FromSql, IsNull, Kind, ToSql, Type, to_sql_checked};

pub(crate) struct Sealed(pub(crate) envelope::Envelope);

impl fmt::Debug for Sealed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Sealed(..)")
    }
}

impl<'a> FromSql<'a> for Sealed {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(envelope::Envelope::new(Vec::<u8>::from_sql(
            kind, raw,
        )?)?))
    }
    fn accepts(kind: &Type) -> bool {
        accepts(kind)
    }
}
impl ToSql for Sealed {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.as_ref().to_sql(kind, out)
    }
    fn accepts(kind: &Type) -> bool {
        accepts(kind)
    }
    to_sql_checked!();
}

fn accepts(kind: &Type) -> bool {
    match kind.kind() {
        Kind::Domain(inner) => inner == &Type::BYTEA,
        _ => kind == &Type::BYTEA,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use tokio_postgres::types::{FromSql, Kind, ToSql, Type};

    use crate::database::sealed::Sealed;

    #[test]
    fn accepts_bytea_domains_and_checks_envelope_structure() {
        let bytes = hex::decode("010000000000000000000102030405060708090a0bb089c0edf060fbe8af85dc9060e507e1393773c7dabb6521ad283f").expect("independent envelope fixture");
        let mut unsupported = bytes.clone();
        unsupported[0] = 2;
        for kind in [
            Type::BYTEA,
            Type::new(
                "sealed".into(),
                12345,
                Kind::Domain(Type::BYTEA),
                "identity".into(),
            ),
            Type::new(
                "sealed".into(),
                12346,
                Kind::Domain(Type::BYTEA),
                "other".into(),
            ),
            Type::new(
                "payload".into(),
                12347,
                Kind::Domain(Type::BYTEA),
                "other".into(),
            ),
        ] {
            assert!(<Sealed as FromSql>::accepts(&kind));
            let sealed = Sealed::from_sql(&kind, &bytes).expect("compatible byte representation");
            assert_eq!(sealed.0.as_ref(), bytes);
            let mut output = BytesMut::new();
            sealed
                .to_sql_checked(&kind, &mut output)
                .expect("compatible parameter");
            assert_eq!(&output[..], bytes);
            for invalid in [&[][..], &[1; 36], unsupported.as_slice()] {
                assert!(Sealed::from_sql(&kind, invalid).is_err());
            }
            assert!(Sealed::from_sql_nullable(&kind, None).is_err());
        }
        let sealed = Sealed::from_sql(&Type::BYTEA, &bytes).expect("valid envelope");
        for kind in [
            Type::UUID,
            Type::TEXT,
            Type::new(
                "sealed".into(),
                12348,
                Kind::Domain(Type::TEXT),
                "identity".into(),
            ),
        ] {
            assert!(!<Sealed as FromSql>::accepts(&kind));
            assert!(sealed.to_sql_checked(&kind, &mut BytesMut::new()).is_err());
        }
    }
}
