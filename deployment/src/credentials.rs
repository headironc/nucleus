//! Fixed deployment credential files.

use std::{
    env, fmt,
    fs::File,
    io,
    path::{Path, PathBuf},
};

use platform::credentials;
use serde::de::DeserializeOwned;
use strum::{AsRefStr, Display};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, AsRefStr, Display)]
pub enum Filename {
    #[strum(serialize = "migration-password")]
    Database,
    #[strum(serialize = "database.json")]
    RuntimeDatabase,
    #[strum(serialize = "keys.json")]
    Keys,
}

pub(crate) fn directory() -> Result<PathBuf, Error> {
    let directory =
        PathBuf::from(env::var_os("CREDENTIALS_DIRECTORY").ok_or(Error::DirectoryMissing)?);
    if !directory.is_absolute() {
        return Err(Error::DirectoryInvalid);
    }
    Ok(directory)
}

pub(crate) fn read<T: DeserializeOwned>(directory: &Path, name: Filename) -> Result<T, Error> {
    let file = match File::open(directory.join(name.as_ref())) {
        Ok(file) => file,
        Err(source) => return Err(Error::Read { file: name, source }),
    };
    match credentials::read(file) {
        Ok(value) => Ok(value),
        Err(credentials::Error::Read(source)) => Err(Error::Read { file: name, source }),
        Err(credentials::Error::Decode(source)) => Err(Error::Decode { file: name, source }),
    }
}

/// File names and categories are safe; source diagnostics must not be logged.
#[derive(Error)]
pub enum Error {
    #[error("CREDENTIALS_DIRECTORY is missing")]
    DirectoryMissing,
    #[error("CREDENTIALS_DIRECTORY must be an absolute path")]
    DirectoryInvalid,
    #[error("cannot read credential file {file}")]
    Read {
        file: Filename,
        #[source]
        source: io::Error,
    },
    #[error("invalid credential file {file}")]
    Decode {
        file: Filename,
        #[source]
        source: serde_json::Error,
    },
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}
