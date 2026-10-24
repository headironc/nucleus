//! Credential decoding from a caller-owned source.

use std::{
    fmt,
    io::{self, Read},
};

use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeOwned},
};
use thiserror::Error;
use zeroize::Zeroizing;

/// Decode one JSON object without retaining the source bytes.
pub fn read<T: DeserializeOwned>(mut reader: impl Read) -> Result<T, Error> {
    let mut buffer = Zeroizing::new(Vec::new());
    reader.read_to_end(&mut buffer)?;
    // Derived struct deserializers also accept sequences.
    if buffer.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(Error::Decode(de::Error::custom(
            "expected a credential JSON object",
        )));
    }
    Ok(serde_json::from_slice(&buffer)?)
}

pub fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    match u64::deserialize(deserializer)? {
        1 => Ok(()),
        _ => Err(de::Error::custom("unsupported credential version")),
    }
}

/// Raw source diagnostics may contain secrets; only categories are printable.
#[derive(Error)]
pub enum Error {
    #[error("cannot read credential source")]
    Read(#[from] io::Error),
    #[error("invalid credential JSON object")]
    Decode(#[from] serde_json::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::{self, Read};

    use serde::Deserialize;

    use crate::credentials::{Error, deserialize_version, read};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        #[serde(deserialize_with = "deserialize_version")]
        version: (),
    }

    #[test]
    fn accepts_one_object_and_rejects_other_shapes_and_versions() {
        let Input { version: () } = read(br#" {"version":1} "#.as_slice()).expect("object");
        for bytes in [
            b"".as_slice(),
            b"null",
            b"[1]",
            br#"{"version":1} {}"#,
            br#"{"version":1,"version":1}"#,
            br#"{"version":2}"#,
            br#"{"version":1.0}"#,
            br#"{"version":1,"extra":0}"#,
        ] {
            assert!(matches!(read::<Input>(bytes), Err(Error::Decode(_))));
        }
    }

    #[test]
    fn hides_read_and_decode_causes_without_discarding_them() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("sensitive source"))
            }
        }
        let read_error = read::<Input>(Broken).err().expect("read failure");
        assert!(matches!(read_error, Error::Read(_)));
        let decode_error = read::<Input>(br#"{"version":"sensitive value"}"#.as_slice())
            .err()
            .expect("decode failure");
        assert!(matches!(decode_error, Error::Decode(_)));
        for error in [read_error, decode_error] {
            assert!(!format!("{error:?} {error}").contains("sensitive"));
            assert!(std::error::Error::source(&error).is_some());
        }
    }
}
