//! Shared key file shape and explicit conversion into validated values.

use std::{fmt, marker::PhantomData};

use platform::{credentials::deserialize_version, cryptography::key};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::keys::{self, Id, Material};

/// Parsed file input; value and key-set validation remain explicit.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct Input(Object<Document>);

impl TryFrom<Input> for (Id, Vec<keys::Entry>) {
    type Error = Error;

    fn try_from(
        Input(Object(Document {
            version: (),
            active_id,
            keys,
        })): Input,
    ) -> Result<Self, Error> {
        let active_id = match Id::new(active_id) {
            Ok(key_id) => key_id,
            Err(source) => return Err(Error::Active(source)),
        };
        let entries = keys
            .into_iter()
            .enumerate()
            .map(|(index, Object(Entry { id, material }))| {
                let id = match Id::new(id) {
                    Ok(id) => id,
                    Err(source) => return Err(Error::Id { index, source }),
                };
                let material = match Material::new(material) {
                    Ok(material) => material,
                    Err(source) => {
                        return Err(Error::Material {
                            index,
                            key_id: id,
                            source,
                        });
                    }
                };
                Ok(keys::Entry::new(id, material))
            })
            .collect::<Result<_, Error>>()?;
        Ok((active_id, entries))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(deserialize_with = "deserialize_version")]
    version: (),
    #[serde(rename = "active")]
    active_id: i64,
    keys: Vec<Object<Entry>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: i64,
    material: Zeroizing<String>,
}

// Derived struct deserializers also accept arrays; key records require objects.
struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = Object<T>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a key configuration object")
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                Ok(Object(T::deserialize(MapAccessDeserializer::new(map))?))
            }
        }
        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid key id at active")]
    Active(#[source] key::Error),
    #[error("invalid key id at keys[{index}].id")]
    Id {
        index: usize,
        #[source]
        source: key::Error,
    },
    #[error("invalid key material at keys[{index}].material for {key_id:?}")]
    Material {
        index: usize,
        key_id: Id,
        #[source]
        source: keys::MaterialError,
    },
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::keys::{
        self, Id,
        input::{Error, Input},
    };

    const JSON: &str = r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}"#;

    #[test]
    fn rejects_nonobject_records_and_invalid_schema() {
        for json in [
            r#"[1,0,[]]"#,
            r#"{"version":1,"active":0,"keys":[[0,"sensitive"]]}"#,
            r#"{"version":2,"active":0,"keys":[]}"#,
            r#"{"version":1.0,"active":0,"keys":[]}"#,
            r#"{"version":1,"active":0,"keys":[],"extra":0}"#,
            r#"{"version":1,"active":0,"active":0,"keys":[]}"#,
            r#"{"version":1,"active":0,"keys":[{"id":0}]}"#,
        ] {
            assert!(serde_json::from_str::<Input>(json).is_err());
        }
    }

    #[test]
    fn separates_parsing_value_conversion_and_set_validation() {
        let input: Input = serde_json::from_str(JSON).expect("input");
        let (active_id, entries): (Id, Vec<keys::Entry>) = input.try_into().expect("values");
        assert_eq!(active_id, Id::new(0).expect("id"));
        assert_eq!(&entries[0].material.0[..], &(0_u8..32).collect::<Vec<_>>());
        keys::Keys::new(active_id, entries).expect("set");

        let input: Input =
            serde_json::from_str(r#"{"version":1,"active":0,"keys":[]}"#).expect("shape");
        let (active_id, entries): (Id, Vec<keys::Entry>) = input.try_into().expect("values");
        assert!(matches!(
            keys::Keys::new(active_id, entries),
            Err(keys::Error::Active(_))
        ));
    }

    #[test]
    fn retains_field_context_without_secret_values() {
        for (json, field) in [
            (
                r#"{"version":1,"active":-1,"keys":[]}"#.to_owned(),
                "active",
            ),
            (JSON.replace(r#""id":0"#, r#""id":-1"#), "keys[0].id"),
            (
                JSON.replace("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8", "sensitive"),
                "keys[0].material",
            ),
        ] {
            let input: Input = serde_json::from_str(&json).expect("shape does not validate values");
            let result: Result<(Id, Vec<keys::Entry>), Error> = input.try_into();
            let error = result.err().expect("invalid value");
            assert!(error.to_string().contains(field));
            assert!(!format!("{error:?} {error}").contains("sensitive"));
            assert!(std::error::Error::source(&error).is_some());
        }
    }
}
