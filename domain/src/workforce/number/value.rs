//! The complete Workforce number value and its closed subtype vocabulary.

use std::fmt;

use derive_more::AsRef;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;

/// A fixed person subtype; its code divided by 100 names the category that owns a pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i16", into = "i16")]
pub enum Subtype {
    Clinical,
    Nursing,
    Technology,
    Pharmacy,
    Finance,
    Administration,
    Logistics,
    Company,
    Security,
    Property,
    Volunteer,
    Observation,
    Internship,
    Training,
    Student,
    Temporary,
}

impl Subtype {
    fn code(self) -> i16 {
        match self {
            Self::Clinical => 100,
            Self::Nursing => 200,
            Self::Technology => 300,
            Self::Pharmacy => 400,
            Self::Finance => 500,
            Self::Administration => 600,
            Self::Logistics => 700,
            Self::Company => 801,
            Self::Security => 802,
            Self::Property => 803,
            Self::Volunteer => 804,
            Self::Observation => 901,
            Self::Internship => 902,
            Self::Training => 903,
            Self::Student => 904,
            Self::Temporary => 1000,
        }
    }

    /// The category 1 to 10 that shares one serial pool per full year.
    pub fn category(self) -> i32 {
        i32::from(self.code() / 100)
    }
}

impl Subtype {
    pub(crate) const ALL: [Self; 16] = [
        Self::Clinical,
        Self::Nursing,
        Self::Technology,
        Self::Pharmacy,
        Self::Finance,
        Self::Administration,
        Self::Logistics,
        Self::Company,
        Self::Security,
        Self::Property,
        Self::Volunteer,
        Self::Observation,
        Self::Internship,
        Self::Training,
        Self::Student,
        Self::Temporary,
    ];
}

impl utoipa::PartialSchema for Subtype {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::Integer)
            .enum_values(Some(Self::ALL.iter().map(|subtype| subtype.code())))
            .into()
    }
}

impl ToSchema for Subtype {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("number.Subtype")
    }
}

impl TryFrom<i16> for Subtype {
    type Error = ValueError;

    fn try_from(code: i16) -> Result<Self, ValueError> {
        Ok(match code {
            100 => Self::Clinical,
            200 => Self::Nursing,
            300 => Self::Technology,
            400 => Self::Pharmacy,
            500 => Self::Finance,
            600 => Self::Administration,
            700 => Self::Logistics,
            801 => Self::Company,
            802 => Self::Security,
            803 => Self::Property,
            804 => Self::Volunteer,
            901 => Self::Observation,
            902 => Self::Internship,
            903 => Self::Training,
            904 => Self::Student,
            1000 => Self::Temporary,
            _ => return Err(ValueError::Subtype),
        })
    }
}

impl From<Subtype> for i16 {
    fn from(subtype: Subtype) -> Self {
        subtype.code()
    }
}

/// A complete number; equal display text does not make two numbers equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = number::Number)]
pub struct Number {
    subtype: Subtype,
    #[serde(deserialize_with = "year")]
    #[schema(minimum = 1)]
    year: i32,
    #[serde(deserialize_with = "serial")]
    #[schema(minimum = 1, maximum = 999)]
    serial: i16,
}

impl Number {
    pub(super) fn new(subtype: Subtype, year: i32, serial: i16) -> Result<Self, ValueError> {
        if year < 1 {
            return Err(ValueError::Year);
        }
        if !(1..=999).contains(&serial) {
            return Err(ValueError::Serial);
        }
        Ok(Self {
            subtype,
            year,
            serial,
        })
    }

    pub fn subtype(&self) -> Subtype {
        self.subtype
    }
    pub fn year(&self) -> i32 {
        self.year
    }
    pub fn serial(&self) -> i16 {
        self.serial
    }

    /// Numbers of one category, year and serial occupy the same pool slot.
    pub(super) fn shares_pool(&self, other: Number) -> bool {
        self.subtype.category() == other.subtype.category()
            && self.year == other.year
            && self.serial == other.serial
    }
}

pub(super) fn year<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let year = i32::deserialize(deserializer)?;
    if year < 1 {
        return Err(serde::de::Error::custom(ValueError::Year));
    }
    Ok(year)
}

pub(super) fn serial<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i16, D::Error> {
    let serial = i16::deserialize(deserializer)?;
    if !(1..=999).contains(&serial) {
        return Err(serde::de::Error::custom(ValueError::Serial));
    }
    Ok(serial)
}

/// A manager's nonempty normalized reason for a number operation.
#[derive(Clone, PartialEq, Eq, AsRef, Serialize, Deserialize, ToSchema)]
#[as_ref(str)]
#[serde(try_from = "String")]
#[schema(as = number::Reason, value_type = String)]
pub struct Reason(String);

impl TryFrom<String> for Reason {
    type Error = ValueError;

    fn try_from(input: String) -> Result<Self, Self::Error> {
        let value = input.trim().nfc().collect::<String>();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(ValueError::Reason);
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Reason([redacted])")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ValueError {
    #[error("unknown Workforce subtype code")]
    Subtype,
    #[error("Workforce entry year must be positive")]
    Year,
    #[error("Workforce serial must be within 1 to 999")]
    Serial,
    #[error("a number operation reason must be nonempty and contain no control characters")]
    Reason,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use serde_json::json;

    use super::{Number, Subtype};

    #[test]
    fn reasons_require_one_normalized_scalar_without_controls() {
        let reason: super::Reason =
            serde_json::from_str(r#""  核实 2024 年入职  ""#).expect("normalized reason");
        assert_eq!(reason.as_ref(), "核实 2024 年入职");
        for invalid in ["", "   ", "a\u{0000}b"] {
            assert!(
                serde_json::from_str::<super::Reason>(
                    &serde_json::to_string(invalid).expect("text")
                )
                .is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn subtypes_are_exactly_the_sixteen_fixed_codes() {
        let codes = [
            100, 200, 300, 400, 500, 600, 700, 801, 802, 803, 804, 901, 902, 903, 904, 1000,
        ];
        assert_eq!(
            Subtype::ALL.map(i16::from),
            codes,
            "the schema lists every fixed code once"
        );
        for code in codes {
            let subtype: Subtype = serde_json::from_value(json!(code)).expect("fixed code");
            assert_eq!(i16::from(subtype), code);
            assert_eq!(subtype.category(), i32::from(code / 100));
        }
        for code in [0, 1, 800, 805, 900, 999, 1001, -100] {
            assert!(
                serde_json::from_value::<Subtype>(json!(code)).is_err(),
                "{code}"
            );
        }
        assert!(serde_json::from_value::<Subtype>(json!("100")).is_err());
    }

    #[test]
    fn numbers_require_positive_years_and_pool_serials() {
        assert!(Number::new(Subtype::Clinical, 2026, 1).is_ok());
        assert!(Number::new(Subtype::Clinical, 2026, 999).is_ok());
        for (year, serial) in [(0, 1), (2026, 0), (2026, 1000), (-1, 5)] {
            assert!(Number::new(Subtype::Clinical, year, serial).is_err());
        }
    }
}
