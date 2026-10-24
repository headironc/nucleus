//! A code proof bound to its original Message; restoring it does not authenticate a code.

use std::fmt;

use bytes::BytesMut;
use platform::cryptography::{self, Encoded, hmac};
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

use crate::identity::message::{Code, Id};

pub struct Proof([u8; 32]);

impl Proof {
    pub(crate) fn new(
        key: &hmac::Key,
        message_id: Id,
        code: &Code,
    ) -> Result<Self, cryptography::Error> {
        Ok(Self(
            *key.sign(&Encoded::new(&Input { message_id, code })?)
                .as_ref(),
        ))
    }

    pub(crate) fn verify(
        &self,
        key: &hmac::Key,
        message_id: Id,
        code: &Code,
    ) -> Result<bool, cryptography::Error> {
        Ok(key.verify(&Encoded::new(&Input { message_id, code })?, &self.0))
    }
}

impl fmt::Debug for Proof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Proof(..)")
    }
}

impl<'a> FromSql<'a> for Proof {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self(<&[u8]>::from_sql(kind, raw)?.try_into()?))
    }

    fn accepts(kind: &Type) -> bool {
        <&[u8] as FromSql>::accepts(kind)
    }
}

impl ToSql for Proof {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.0.as_slice().to_sql(kind, out)
    }

    fn accepts(kind: &Type) -> bool {
        <&[u8] as ToSql>::accepts(kind)
    }

    to_sql_checked!();
}

struct Input<'a> {
    message_id: Id,
    code: &'a Code,
}

impl cryptography::Input<3> for Input<'_> {
    fn fields(&self) -> [&[u8]; 3] {
        [
            b"nucleus/identity/message-code/v1",
            self.message_id.as_bytes(),
            self.code.as_bytes(),
        ]
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use zeroize::Zeroizing;

    use super::*;

    #[test]
    fn binds_the_original_message_and_exact_code_with_an_independent_vector() {
        let key = hmac::Key::new(Zeroizing::new(std::array::from_fn(|index| {
            u8::try_from(index).expect("32 bytes")
        })));
        let message_id = "00000000-0000-0000-0000-000000000001".parse().expect("id");
        let code = "00001234".parse().expect("code");
        let proof = Proof::new(&key, message_id, &code).expect("proof");
        // Python struct.pack('>I') and hmac.digest, independent of the Rust encoder.
        assert_eq!(
            hex::encode(proof.0),
            "9f6b90f4a1f3844d0dd25bbe0bd27f17c25133b1d2ca852eeb6d12db91172c75"
        );
        assert!(proof.verify(&key, message_id, &code).expect("verify"));
        assert!(!proof.verify(&key, Id::new(), &code).expect("wrong message"));
        assert!(
            !proof
                .verify(&key, message_id, &"00001235".parse().expect("code"))
                .expect("wrong code")
        );
        assert!(
            !proof
                .verify(&hmac::Key::new(Zeroizing::new([0; 32])), message_id, &code)
                .expect("wrong key")
        );
        for length in [0, 31, 33] {
            assert!(Proof::from_sql(&Type::BYTEA, &vec![0; length]).is_err());
        }
        let restored = Proof::from_sql(&Type::BYTEA, &proof.0).expect("restore representation");
        assert!(
            restored
                .verify(&key, message_id, &code)
                .expect("authenticate restored proof")
        );
        assert_eq!(format!("{restored:?}"), "Proof(..)");
    }
}
