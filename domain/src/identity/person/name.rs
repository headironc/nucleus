//! Stable personal-name normalization, independent of the date of a new application.

use std::{fmt, str::FromStr};

use derive_more::AsRef;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use zeroize::Zeroizing;

/// A personal name with 1 to 64 Unicode scalars after trimming whitespace and applying NFC.
#[derive(Clone, AsRef, Serialize, Deserialize, ToSchema)]
#[as_ref(str)]
#[serde(try_from = "Zeroizing<String>")]
#[schema(as = person::Name, value_type = String)]
pub struct Name(Zeroizing<String>);

impl Name {
    pub(crate) fn masked(&self) -> String {
        let mut characters = self.0.chars();
        match (characters.next(), characters.next()) {
            (Some(first), Some(_)) => format!("{first}*"),
            _ => "*".to_owned(),
        }
    }
}

impl TryFrom<Zeroizing<String>> for Name {
    type Error = ParseError;

    fn try_from(input: Zeroizing<String>) -> Result<Self, Self::Error> {
        let value = Zeroizing::new(input.trim().nfc().collect::<String>());
        if !(1..=64).contains(&value.chars().count()) || value.chars().any(char::is_control) {
            return Err(ParseError);
        }
        Ok(Self(value))
    }
}

impl FromStr for Name {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Zeroizing::new(input.to_owned()).try_into()
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Name([redacted])")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("name must contain 1 to 64 normalized Unicode scalars without controls")]
pub struct ParseError;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::person::name::Name;

    #[test]
    fn normalizes_before_counting_and_rejects_controls() {
        let name: Name = serde_json::from_str(r#""  A\u0301名  ""#).expect("normalized name");
        assert_eq!(name.as_ref(), "Á名");
        assert_eq!(
            serde_json::to_value(&name).expect("normalized JSON name"),
            "Á名"
        );
        let decomposed =
            serde_json::to_string(&"A\u{301}".repeat(64)).expect("decomposed JSON name");
        let normalized: Name = serde_json::from_str(&decomposed).expect("64 normalized scalars");
        assert_eq!(
            serde_json::to_value(&normalized).expect("normalized JSON boundary"),
            "Á".repeat(64)
        );
        assert!("界".repeat(64).parse::<Name>().is_ok());
        for invalid in [String::new(), " \t ".into(), "A\0B".into(), "界".repeat(65)] {
            let error = invalid.parse::<Name>().expect_err("invalid name");
            assert!(!format!("{error:?}").contains(&format!("\"{invalid}\"")));
        }
        assert!(!format!("{name:?}").contains("Á名"));
    }
}
