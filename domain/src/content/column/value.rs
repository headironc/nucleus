use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql, Type};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    FromSql,
    ToSql,
)]
#[serde(transparent)]
#[schema(as = column::Id)]
#[postgres(transparent)]
pub struct Id(Uuid);

impl Id {
    pub(super) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
    pub(super) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for Id {
    type Err = uuid::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema, ToSql)]
#[serde(transparent)]
#[schema(as = column::Key, value_type = String, min_length = 2, max_length = 32, pattern = "^[a-z][a-z0-9-]{1,31}$")]
#[postgres(transparent)]
pub struct Key(String);

impl Key {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Key {
    type Err = InputError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !(2..=32).contains(&value.len())
            || !value.as_bytes()[0].is_ascii_lowercase()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(InputError::Key);
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl<'a> FromSql<'a> for Key {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema, ToSql)]
#[serde(transparent)]
#[schema(as = column::Name, value_type = String, min_length = 1, max_length = 20)]
#[postgres(transparent)]
pub struct Name(String);

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn normalize(value: &str, minimum: usize, maximum: usize) -> Result<String, InputError> {
    // Check before trimming so a supplied control cannot disappear at the boundary.
    if value.chars().any(char::is_control) {
        return Err(InputError::Text);
    }
    let value = value.nfc().collect::<String>().trim().to_owned();
    if !(minimum..=maximum).contains(&value.chars().count()) {
        return Err(InputError::Text);
    }
    Ok(value)
}

impl FromStr for Name {
    type Err = InputError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize(value, 1, 20).map(Self)
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl<'a> FromSql<'a> for Name {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let stored = <&str>::from_sql(kind, raw)?;
        let value: Self = stored.parse()?;
        if value.as_str() != stored {
            return Err(Box::new(InputError::Text));
        }
        Ok(value)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema, ToSql)]
#[serde(transparent)]
#[schema(as = column::Description, value_type = String, max_length = 100)]
#[postgres(transparent)]
pub struct Description(String);

impl Description {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Description {
    type Err = InputError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        normalize(value, 0, 100).map(Self)
    }
}

impl<'de> Deserialize<'de> for Description {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl<'a> FromSql<'a> for Description {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let stored = <&str>::from_sql(kind, raw)?;
        let value: Self = stored.parse()?;
        if value.as_str() != stored {
            return Err(Box::new(InputError::Text));
        }
        Ok(value)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error(
        "column key must contain 2 to 32 lowercase ASCII letters, digits or hyphens, starting with a letter"
    )]
    Key,
    #[error("column text must satisfy its scalar length limit and contain no controls")]
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = column::Creation)]
pub struct Creation {
    pub(super) key: Key,
    /// NFC-normalized and trimmed before checking the 1 to 20 scalar limit.
    #[schema(value_type = String)]
    pub(super) name: Name,
    /// NFC-normalized and trimmed before checking the 100 scalar limit.
    #[schema(value_type = String)]
    pub(super) description: Description,
    pub(super) enabled: bool,
}

impl Creation {
    /// Frozen field order is the request shape, using only controlled normalized values.
    pub(super) fn digest(&self) -> Result<Vec<u8>, super::Error> {
        let bytes = serde_json::to_vec(self).map_err(|error| {
            super::Error::Unavailable(crate::kernel::Fault::new(
                crate::kernel::Stage::Encoding,
                crate::kernel::FaultKind::Encoding(error),
            ))
        })?;
        Ok(ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = column::Column)]
pub struct Column {
    pub(super) id: Id,
    pub(super) key: Key,
    pub(super) name: Name,
    pub(super) description: Description,
    pub(super) enabled: bool,
    pub(super) index: i32,
    pub(super) change_id: super::change::Id,
}

impl Column {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn change_id(&self) -> super::change::Id {
        self.change_id
    }
}
