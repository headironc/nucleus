use thiserror::Error;
use url::{Origin, Url};

use crate::identity::challenge;

/// Anonymous website identity requests share their allowed origin.
#[derive(Clone)]
pub struct Settings {
    origin: Origin,
    challenge: challenge::Settings,
}

impl Settings {
    pub fn new(origin: String, site_key: String) -> Result<Self, Error> {
        let url = Url::parse(&origin).map_err(Error::Origin)?;
        if url.scheme() != "https"
            || url.host().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || origin != url.origin().ascii_serialization()
        {
            return Err(Error::OriginShape);
        }
        Ok(Self {
            origin: url.origin(),
            challenge: challenge::Settings::new(site_key)?,
        })
    }

    pub(super) fn origin(&self) -> &Origin {
        &self.origin
    }

    pub(super) fn challenge(&self) -> &challenge::Settings {
        &self.challenge
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Challenge(#[from] challenge::Error),
    #[error("invalid HTTP origin")]
    Origin(#[source] url::ParseError),
    #[error("HTTP origin must be an exact HTTPS origin without credentials or a path")]
    OriginShape,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::Settings;

    #[test]
    fn requires_an_exact_origin() {
        assert!(Settings::new("https://example.com".into(), "fixture-site-key".into()).is_ok());
        for origin in [
            "http://example.com",
            "https://example.com/",
            "https://a:b@example.com",
            "https://example.com?x",
            "https://example.com#x",
            "https://example.com/path",
            "https://EXAMPLE.com",
            "https://example.com:443",
            "null",
        ] {
            assert!(Settings::new(origin.into(), "fixture-site-key".into()).is_err());
        }
        let settings = Settings::new("https://example.com:8443".into(), "fixture-site-key".into())
            .expect("canonical HTTPS origin");
        assert_eq!(
            settings.origin(),
            &url::Origin::Tuple(
                "https".into(),
                url::Host::Domain("example.com".into()),
                8443
            ),
        );
    }
}
