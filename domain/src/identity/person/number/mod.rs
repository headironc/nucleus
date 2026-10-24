//! Stable resident identity-number rules; only a new application checks its trusted day.

pub mod hash;

use std::{fmt, str::FromStr};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize, Serializer};
use utoipa::{PartialSchema, ToSchema};
use zeroize::Zeroizing;

#[derive(Clone, Deserialize)]
#[serde(try_from = "Zeroizing<String>")]
pub struct IdNumber {
    value: Zeroizing<String>,
    birthday: NaiveDate,
}

impl Serialize for IdNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_ref())
    }
}

impl PartialSchema for IdNumber {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        String::schema()
    }
}

impl ToSchema for IdNumber {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("person.IdNumber")
    }
}

impl TryFrom<Zeroizing<String>> for IdNumber {
    type Error = ParseError;

    fn try_from(mut value: Zeroizing<String>) -> Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        if bytes.len() != 18
            || !bytes[..17].iter().all(u8::is_ascii_digit)
            || !(bytes[17].is_ascii_digit() || matches!(bytes[17], b'X' | b'x'))
        {
            return Err(ParseError::Format);
        }
        let sum: usize = bytes[..17]
            .iter()
            .zip([7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2])
            .map(|(digit, weight)| usize::from(digit - b'0') * weight)
            .sum();
        if bytes[17].to_ascii_uppercase() != b"10X98765432"[sum % 11] {
            return Err(ParseError::Checksum);
        }
        if bytes[17] == b'x' {
            value.replace_range(17.., "X");
        }
        let birthday =
            NaiveDate::parse_from_str(&value[6..14], "%Y%m%d").map_err(ParseError::Date)?;
        Ok(Self { value, birthday })
    }
}

impl IdNumber {
    pub(crate) fn validate_day(&self, day: NaiveDate) -> Result<(), ParseError> {
        if self.birthday > day {
            return Err(ParseError::Future);
        }
        Ok(())
    }

    pub fn suffix(&self) -> &str {
        &self.value[14..]
    }
}

impl AsRef<str> for IdNumber {
    fn as_ref(&self) -> &str {
        &self.value
    }
}

impl FromStr for IdNumber {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Zeroizing::new(input.to_owned()).try_into()
    }
}

impl fmt::Debug for IdNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdNumber([redacted])")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("resident identity number must contain 18 ASCII characters")]
    Format,
    #[error("resident identity number checksum is invalid")]
    Checksum,
    #[error("resident identity number birth date is invalid")]
    Date(#[source] chrono::ParseError),
    #[error("birth date is later than the trusted application day")]
    Future,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use chrono::NaiveDate;

    use crate::identity::person::number::{IdNumber, ParseError};

    #[test]
    fn separates_stable_format_from_the_application_day() {
        let number: IdNumber = "11010519491231002x"
            .parse()
            .expect("published MOD 11-2 example");
        assert_eq!(number.as_ref(), "11010519491231002X");
        assert_eq!(number.suffix(), "002X");
        assert!(
            number
                .validate_day(NaiveDate::from_ymd_opt(1949, 12, 31).expect("day"))
                .is_ok()
        );
        assert!(
            number
                .validate_day(NaiveDate::from_ymd_opt(1949, 12, 30).expect("day"))
                .is_err()
        );
        for invalid in [
            "110105194912310020",
            "11010519491231002X ",
            "１1010519491231002X",
            "110105194902300024",
        ] {
            assert!(invalid.parse::<IdNumber>().is_err());
        }
        let decoded: IdNumber =
            serde_json::from_str(r#""11010519491231002x""#).expect("owned input");
        assert_eq!(decoded.as_ref(), number.as_ref());
        assert!(!format!("{decoded:?}").contains("19491231"));
        for invalid in ["000000190002290014", "000000200002300012"] {
            assert!(matches!(
                invalid.parse::<IdNumber>(),
                Err(ParseError::Date(_))
            ));
        }
    }
}
