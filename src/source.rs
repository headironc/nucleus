//! Accept a single client address from an explicitly trusted direct proxy.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts, connect_info::Connected},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
    serve::IncomingStream,
};
use ipnet::IpNet;
use thiserror::Error;

use crate::{
    extension::{self, Extension},
    json::Json,
    response::{Common, Failure},
};

/// An address resolved using the configured proxy trust boundary.
#[derive(Clone, Copy)]
pub(crate) struct Source(IpAddr);

impl Source {
    pub(crate) fn address(self) -> IpAddr {
        self.0
    }
}

impl<S> FromRequestParts<S> for Source
where
    S: Send + Sync,
    Settings: FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Error> {
        let Extension(ConnectInfo(peer)) =
            Extension::<ConnectInfo<Peer>>::from_request_parts(parts, state).await?;
        Ok(Self(
            Settings::from_ref(state).resolve(&peer, &parts.headers)?,
        ))
    }
}

// Every serving connection carries the bounded listener's actual transfer owner.
#[derive(Clone)]
pub(crate) struct Peer {
    address: SocketAddr,
    transmission: crate::listener::Transmission,
}

impl Peer {
    #[cfg(test)]
    fn new(address: SocketAddr) -> Self {
        Self {
            address,
            transmission: crate::listener::Transmission::new(tokio::sync::watch::channel(false).1),
        }
    }
    pub(crate) fn transmission(&self) -> &crate::listener::Transmission {
        &self.transmission
    }
}

impl Connected<IncomingStream<'_, crate::listener::Listener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, crate::listener::Listener>) -> Self {
        Self {
            address: *stream.remote_addr(),
            transmission: stream.io().transmission(),
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    proxies: Arc<[IpNet]>,
}

impl Settings {
    pub fn new(proxies: Vec<IpNet>) -> Result<Self, SettingsError> {
        if proxies.is_empty() {
            return Err(SettingsError::Empty);
        }
        let proxies = proxies
            .into_iter()
            .map(canonical)
            .collect::<Result<Vec<_>, _>>()?;
        let proxies = IpNet::aggregate(&proxies);
        if proxies.iter().any(|network| network.prefix_len() == 0) {
            return Err(SettingsError::Proxies);
        }
        Ok(Self {
            proxies: proxies.into(),
        })
    }

    pub(crate) fn resolve(&self, peer: &Peer, headers: &HeaderMap) -> Result<IpAddr, Error> {
        let peer = peer.address.ip().to_canonical();
        if !self.proxies.iter().any(|network| network.contains(&peer)) {
            return Err(Error::Untrusted);
        }
        let mut values = headers.get_all("x-real-ip").iter();
        let value = values.next().ok_or(Error::Missing)?;
        if values.next().is_some() {
            return Err(Error::Repeated);
        }
        Ok(value.to_str()?.parse::<IpAddr>()?.to_canonical())
    }
}

fn canonical(network: IpNet) -> Result<IpNet, SettingsError> {
    if let IpNet::V6(network) = network
        && let Some(address) = network.addr().to_ipv4_mapped()
    {
        let prefix = network
            .prefix_len()
            .checked_sub(96)
            .ok_or(SettingsError::Proxies)?;
        return IpNet::new(address.into(), prefix).map_err(SettingsError::Prefix);
    }
    Ok(network)
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("at least one trusted direct proxy is required")]
    Empty,
    #[error("trusted proxies must not contain a default route")]
    Proxies,
    #[error("invalid trusted proxy prefix")]
    Prefix(#[source] ipnet::PrefixLenError),
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("request source has no connection information")]
    Connection(#[from] extension::Error),
    #[error("request peer is not a trusted proxy")]
    Untrusted,
    #[error("missing client address header")]
    Missing,
    #[error("repeated client address header")]
    Repeated,
    #[error("invalid client address header")]
    Header(#[from] axum::http::header::ToStrError),
    #[error("invalid client IP address")]
    Address(#[from] std::net::AddrParseError),
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::Connection(cause) => return cause.into_response(),
            Self::Untrusted => (StatusCode::FORBIDDEN, Common::PermissionDenied),
            Self::Missing | Self::Repeated | Self::Header(_) | Self::Address(_) => {
                (StatusCode::BAD_REQUEST, Common::InvalidInput)
            }
        };
        (status, Json(Failure { code })).into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::net::IpAddr;

    use axum::{
        body::{Body, to_bytes},
        extract::{ConnectInfo, FromRequestParts},
        http::{HeaderMap, HeaderValue, Request, StatusCode},
        response::IntoResponse,
    };

    use crate::source::{Error, Peer, Settings, Source};

    #[test]
    fn requires_explicit_proxy_networks() {
        assert!(Settings::new(vec![]).is_err());
        for network in ["0.0.0.0/0", "::/0"] {
            assert!(Settings::new(vec![network.parse().expect("CIDR")]).is_err());
        }
        for networks in [vec!["0.0.0.0/1", "128.0.0.0/1"], vec!["::ffff:0:0/96"]] {
            assert!(
                Settings::new(
                    networks
                        .into_iter()
                        .map(|network| network.parse().expect("CIDR"))
                        .collect()
                )
                .is_err()
            );
        }
        let settings = Settings::new(vec!["::ffff:192.0.2.0/120".parse().expect("mapped CIDR")])
            .expect("canonical proxies");
        assert!(settings.proxies[0].contains(&"192.0.2.1".parse::<IpAddr>().expect("IP")));
    }

    #[test]
    fn accepts_only_one_address_from_a_trusted_direct_proxy() {
        let settings =
            Settings::new(vec!["10.0.0.0/8".parse().expect("CIDR")]).expect("trusted proxy");
        let peer = Peer::new("10.0.0.2:8080".parse().expect("peer"));
        let mut headers = HeaderMap::new();
        assert!(matches!(
            settings.resolve(&peer, &headers),
            Err(Error::Missing)
        ));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.0.2.1, 10.0.0.1"),
        );
        assert!(matches!(
            settings.resolve(&peer, &headers),
            Err(Error::Missing)
        ));
        headers.insert("forwarded", HeaderValue::from_static("for=192.0.2.1"));
        assert!(matches!(
            settings.resolve(&peer, &headers),
            Err(Error::Missing)
        ));
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.1"));
        assert_eq!(
            settings.resolve(&peer, &headers).expect("single address"),
            "198.51.100.1".parse::<IpAddr>().expect("IP")
        );
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("invalid ignored chain"),
        );
        assert_eq!(
            settings.resolve(&peer, &headers).expect("no chain parsing"),
            "198.51.100.1".parse::<IpAddr>().expect("IP")
        );
        for address in ["192.0.2.8:1234", "[::ffff:192.0.2.8]:1234"] {
            let peer = Peer::new(address.parse().expect("untrusted peer"));
            assert!(matches!(
                settings.resolve(&peer, &headers),
                Err(Error::Untrusted)
            ));
        }
        headers.append("x-real-ip", HeaderValue::from_static("198.51.100.1"));
        assert!(matches!(
            settings.resolve(&peer, &headers),
            Err(Error::Repeated)
        ));
        for address in [
            "",
            "invalid",
            "192.0.2.1, 192.0.2.2",
            "192.0.2.1:80",
            "[2001:db8::1]:80",
            " 192.0.2.1 ",
        ] {
            headers.insert("x-real-ip", HeaderValue::from_str(address).expect("header"));
            assert!(matches!(
                settings.resolve(&peer, &headers),
                Err(Error::Address(_))
            ));
        }
        headers.insert(
            "x-real-ip",
            HeaderValue::from_bytes(&[0xff]).expect("non-text header"),
        );
        assert!(matches!(
            settings.resolve(&peer, &headers),
            Err(Error::Header(_))
        ));
    }

    #[test]
    fn normalizes_mapped_addresses_and_preserves_ipv6_hosts() {
        let settings = Settings::new(vec!["::ffff:10.0.0.0/104".parse().expect("CIDR")])
            .expect("trusted proxy");
        for peer in ["10.0.0.2:8080", "[::ffff:10.0.0.2]:8080"] {
            let peer = Peer::new(peer.parse().expect("peer"));
            for (address, expected) in [
                ("::ffff:192.0.2.1", "192.0.2.1"),
                ("2001:db8::1", "2001:db8::1"),
                ("2001:db8::2", "2001:db8::2"),
                ("10.0.0.1", "10.0.0.1"),
            ] {
                let mut headers = HeaderMap::new();
                headers.insert("x-real-ip", HeaderValue::from_str(address).expect("header"));
                assert_eq!(
                    settings.resolve(&peer, &headers).expect("client address"),
                    expected.parse::<IpAddr>().expect("IP")
                );
            }
        }
    }

    #[tokio::test]
    async fn extraction_requires_connection_evidence_before_trusting_headers() {
        let settings =
            Settings::new(vec!["10.0.0.0/8".parse().expect("CIDR")]).expect("trusted proxy");
        for (option, status, code) in [
            (None, StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
            (
                Some("192.0.2.1:8080"),
                StatusCode::FORBIDDEN,
                "permission_denied",
            ),
        ] {
            let (mut parts, _) = Request::builder()
                .header("x-real-ip", "198.51.100.1")
                .body(Body::empty())
                .expect("request")
                .into_parts();
            if let Some(peer) = option {
                parts
                    .extensions
                    .insert(ConnectInfo(Peer::new(peer.parse().expect("peer"))));
            }
            let response = match Source::from_request_parts(&mut parts, &settings).await {
                Ok(_) => panic!("a header cannot establish a trusted connection"),
                Err(error) => error.into_response(),
            };
            assert_eq!(response.status(), status);
            let body = to_bytes(response.into_body(), 1024)
                .await
                .expect("safe body");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).expect("JSON"),
                serde_json::json!({"code":code})
            );
        }
    }
}
