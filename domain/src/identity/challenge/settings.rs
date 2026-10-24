//! Validated deployment settings for Siteverify response checks.

use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};
use nonempty::NonEmpty;
use thiserror::Error;
use url::Host;

/// The deployment values consumed by the Siteverify client.
pub struct Settings {
    hostnames: AllowedHostnames,
}

impl Settings {
    pub fn new(hostnames: AllowedHostnames) -> Self {
        Self { hostnames }
    }

    pub(super) fn hostnames(&self) -> &AllowedHostnames {
        &self.hostnames
    }
}

/// Hostnames permitted for a Siteverify success response.
pub struct AllowedHostnames(NonEmpty<Host>);

impl AllowedHostnames {
    /// Validate and deduplicate the deployment allowlist using normalized hosts.
    pub fn new(hostnames: NonEmpty<String>) -> Result<Self, HostnamesError> {
        let head = match parse(&hostnames.head) {
            Ok(host) => host,
            Err(source) => return Err(HostnamesError { index: 0, source }),
        };
        let mut allowed = NonEmpty::new(head);
        for (index, name) in hostnames.tail.into_iter().enumerate() {
            let host = match parse(&name) {
                Ok(host) => host,
                Err(source) => {
                    return Err(HostnamesError {
                        index: index + 1,
                        source,
                    });
                }
            };
            if !allowed.iter().any(|previous| previous == &host) {
                allowed.push(host);
            }
        }
        Ok(Self(allowed))
    }

    pub(super) fn contains(&self, hostname: &Host) -> bool {
        self.0.iter().any(|allowed| allowed == hostname)
    }
}

fn parse(name: &str) -> Result<Host, HostnameError> {
    if name.contains('%')
        || name
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(HostnameError::Characters);
    }
    let host = Host::parse(name)?;
    match &host {
        Host::Ipv6(_) => return Err(HostnameError::Ipv6),
        Host::Domain(_) => {
            Uts46::new().to_ascii(
                name.as_bytes(),
                AsciiDenyList::STD3,
                Hyphens::CheckFirstLast,
                DnsLength::VerifyAllowRootDot,
            )?;
        }
        Host::Ipv4(_) => {}
    }
    Ok(host)
}

/// Why a Siteverify hostname allowlist cannot be used.
#[derive(Debug, Error)]
#[error("invalid siteverify hostname at index {index}")]
pub struct HostnamesError {
    index: usize,
    #[source]
    source: HostnameError,
}

#[derive(Debug, Error)]
enum HostnameError {
    #[error("hostname contains forbidden characters")]
    Characters,
    #[error("IPv6 is not permitted")]
    Ipv6,
    #[error("invalid URL host")]
    Url(#[from] url::ParseError),
    #[error("invalid DNS domain")]
    Domain(#[from] idna::Errors),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use nonempty::NonEmpty;
    use url::Host;

    use crate::identity::challenge::settings::{AllowedHostnames, HostnamesError};

    #[test]
    fn enforces_domain_boundaries_and_reports_the_array_index() {
        let long_label = format!("{}.com", "a".repeat(64));
        let long_domain = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(62)
        );
        for invalid in [
            "",
            "%65xample.com",
            "example.com\n",
            "exam\u{2003}ple.com",
            "[::1]",
            "example.com:443",
            "*.example.com",
            "https://example.com",
            "-example.com",
            "example-.com",
            "under_score.example",
            "example..com",
            &long_label,
            &long_domain,
        ] {
            let error = match AllowedHostnames::new(NonEmpty::from((
                "example.com".into(),
                vec![invalid.into()],
            ))) {
                Ok(_) => panic!("invalid hostname must fail"),
                Err(error) => error,
            };
            assert_eq!(error.index, 1);
            assert!(!format!("{error:?} {error}").contains("example.com"));
        }
    }

    #[test]
    fn normalizes_and_deduplicates_hosts_for_matching() {
        let allowed = AllowedHostnames::new(NonEmpty::from((
            "Example.COM".into(),
            vec![
                "example.com".into(),
                "例子.测试".into(),
                "127.0.0.1".into(),
                "example.org.".into(),
            ],
        )))
        .expect("valid deployment hosts");
        assert_eq!(allowed.0.len(), 4);
        for name in [
            "example.com",
            "xn--fsqu00a.xn--0zwm56d",
            "127.0.0.1",
            "example.org.",
        ] {
            assert!(allowed.contains(&Host::parse(name).expect("valid comparison host")));
        }
        assert!(!allowed.contains(&Host::parse("other.example").expect("valid unrelated host")));
        let longest = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        AllowedHostnames::new(NonEmpty::new(longest)).expect("253-byte DNS name is valid");
    }

    #[test]
    fn rejects_invalid_allowlist_hosts() {
        assert!(matches!(
            AllowedHostnames::new(NonEmpty::from((
                "example.com".to_owned(),
                vec![" ".to_owned()]
            ))),
            Err(HostnamesError { index: 1, .. })
        ));
        assert!(matches!(
            AllowedHostnames::new(NonEmpty::new(" ".to_owned())),
            Err(HostnamesError { index: 0, .. })
        ));
        assert!(matches!(
            AllowedHostnames::new(NonEmpty::new("https://example.com".to_owned())),
            Err(HostnamesError { index: 0, .. })
        ));
    }
}
