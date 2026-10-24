//! Original C1 token proof; identity and input matching remain with Challenge.

use std::fmt;

use bytes::BytesMut;
use platform::cryptography::{self, Encoded, hmac};
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type, to_sql_checked};

use crate::identity::challenge::Token;

/// A proof does not grant permission to call a supplier or reveal an original result.
pub struct Proof([u8; 32]);

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

impl Proof {
    pub(crate) fn new(key: &hmac::Key, token: &Token) -> Result<Self, cryptography::Error> {
        Ok(Self(*key.sign(&Encoded::new(&Input { token })?).as_ref()))
    }

    pub(crate) fn verify(
        &self,
        key: &hmac::Key,
        token: &Token,
    ) -> Result<bool, cryptography::Error> {
        Ok(key.verify(&Encoded::new(&Input { token })?, &self.0))
    }
}

impl AsRef<[u8]> for Proof {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

struct Input<'a> {
    token: &'a Token,
}

impl cryptography::Input<2> for Input<'_> {
    fn fields(&self) -> [&[u8]; 2] {
        [
            b"nucleus/identity/challenge-token/v1",
            self.token.as_bytes(),
        ]
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use bytes::BytesMut;
    use platform::cryptography::hmac;
    use tokio_postgres::types::{FromSql, ToSql, Type};
    use zeroize::Zeroizing;

    use crate::identity::challenge::{Token, proof::Proof};

    #[test]
    fn sql_decoding_restores_bytes_without_claiming_a_token_match() {
        let proof = Proof::from_sql(&Type::BYTEA, &[42; 32]).expect("fixed-width stored proof");
        let key = hmac::Key::new(Zeroizing::new([7; 32]));
        assert!(
            !proof
                .verify(&key, &"fictional-token".parse().expect("token"))
                .expect("verification")
        );
        for bytes in [&[][..], &[42; 31][..], &[42; 33][..]] {
            assert!(Proof::from_sql(&Type::BYTEA, bytes).is_err());
        }
        assert!(Proof::from_sql_nullable(&Type::BYTEA, None).is_err());
        assert!(!<Proof as FromSql>::accepts(&Type::TEXT));
        assert!(
            proof
                .to_sql_checked(&Type::UUID, &mut BytesMut::new())
                .is_err()
        );
        assert_eq!(format!("{proof:?}"), "Proof(..)");
    }

    #[test]
    fn matches_independent_proof_vector() {
        // Python hmac/hashlib over big-endian length prefixes and key bytes 0..31.
        let material = std::array::from_fn(|index| u8::try_from(index).expect("32 bytes"));
        let key = hmac::Key::new(Zeroizing::new(material));
        let token: Token = " AbÇ-09\n".parse().expect("opaque token");
        let proof = Proof::new(&key, &token).expect("proof");
        assert_eq!(
            proof.as_ref(),
            &[
                69, 78, 35, 30, 176, 5, 72, 35, 164, 156, 220, 70, 157, 26, 75, 148, 27, 211, 189,
                212, 150, 184, 8, 106, 23, 246, 155, 196, 129, 123, 131, 124
            ]
        );
        assert!(proof.verify(&key, &token).expect("same token"));
        for changed in ["AbÇ-09", " Abç-09\n", " AbC\u{327}-09\n"] {
            assert!(
                !proof
                    .verify(&key, &changed.parse().expect("token"))
                    .expect("changed token")
            );
        }
        let boundary = "界".repeat(2048).parse().expect("maximum character count");
        assert!(
            Proof::new(&key, &boundary)
                .expect("proof")
                .verify(&key, &boundary)
                .expect("verify")
        );
    }
}
