//! Database assembly from caller-selected settings and loaded material.

pub mod input;

use std::{
    fmt,
    path::{Path, PathBuf},
};

use rustls::RootCertStore;
use rustls_pki_types::{
    CertificateDer,
    pem::{self, PemObject},
};
use thiserror::Error;

use crate::database::{self, Connections, credentials::Password};

pub struct Settings {
    connection: database::Settings,
    authority: PathBuf,
}

impl Settings {
    pub fn new(connection: database::Settings, authority: String) -> Result<Self, SettingsError> {
        if authority.chars().any(char::is_control) || !PathBuf::from(&authority).is_absolute() {
            return Err(SettingsError);
        }
        Ok(Self {
            connection,
            authority: authority.into(),
        })
    }

    pub fn authority(&self) -> &Path {
        &self.authority
    }

    /// Assemble from parsed trust roots without reading files or opening a connection.
    pub fn build(self, password: Password, roots: RootCertStore) -> Result<Connections, Error> {
        Ok(Connections::new(self.connection, password, roots)?)
    }
}

/// Parse and load certificate roots; connection construction rejects an empty store.
pub fn roots(pem: &[u8]) -> Result<RootCertStore, Error> {
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(pem) {
        roots.add(certificate?)?;
    }
    Ok(roots)
}

#[derive(Debug, Error)]
#[error("authority must be an absolute path without control characters")]
pub struct SettingsError;

#[derive(Error)]
pub enum Error {
    #[error("invalid database authority PEM")]
    Pem(#[from] pem::Error),
    #[error("invalid database authority certificate")]
    Certificate(#[from] rustls::Error),
    #[error("cannot assemble database connections: {0}")]
    Build(#[from] database::BuildError),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use rcgen::generate_simple_self_signed;
    use rustls::RootCertStore;

    use crate::database::assembly::{Error, roots};

    #[test]
    fn loads_all_certificates_and_ignores_other_pem_sections() {
        let first = generate_simple_self_signed(vec!["first.example".into()]).expect("first CA");
        let second = generate_simple_self_signed(vec!["second.example".into()]).expect("second CA");
        let pem = format!(
            "{}{}{}",
            first.cert.pem(),
            first.signing_key.serialize_pem(),
            second.cert.pem(),
        );
        let mut expected = RootCertStore::empty();
        expected.add(first.cert.der().clone()).expect("first root");
        expected
            .add(second.cert.der().clone())
            .expect("second root");
        assert_eq!(
            roots(pem.as_bytes()).expect("trust roots").roots,
            expected.roots
        );
    }

    #[test]
    fn rejects_a_bad_certificate_after_a_valid_one() {
        let certificate =
            generate_simple_self_signed(vec!["fixture.example".into()]).expect("fixture CA");
        let pem = format!(
            "{}-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----\n",
            certificate.cert.pem(),
        );
        assert!(matches!(roots(pem.as_bytes()), Err(Error::Pem(_))));
        let pem = format!(
            "{}-----BEGIN CERTIFICATE-----\nAAE=\n-----END CERTIFICATE-----\n",
            certificate.cert.pem(),
        );
        assert!(matches!(roots(pem.as_bytes()), Err(Error::Certificate(_))));
    }
}
