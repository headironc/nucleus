//! Current avatar metadata, frozen defaults and signed content reads.

mod database;
pub mod link;
pub mod retention;

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
pub(crate) use database::{Snapshot, find_by_account};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    identity::{keys::Keys, mobile::Mobile},
    runtime::{Context, registry::Registry},
};

#[derive(Clone)]
pub struct Service(Arc<Inner>);

struct Inner {
    context: Context,
    keys: Arc<Keys>,
    link: link::Link,
    defaults: Defaults,
    registry: Arc<Registry>,
}

pub struct Image {
    bytes: &'static [u8],
    cache: Duration,
}

impl Image {
    pub fn bytes(&self) -> &'static [u8] {
        self.bytes
    }
    pub fn cache(&self) -> Duration {
        self.cache
    }
}

pub struct Current {
    revision: Revision,
    link: link::Signed,
}

impl Current {
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn link(&self) -> &link::Signed {
        &self.link
    }
}

impl Service {
    pub(crate) fn prepare(
        context: Context,
        keys: Arc<Keys>,
        link: link::Link,
        registry: Arc<Registry>,
    ) -> Result<Self, Error> {
        Ok(Self(Arc::new(Inner {
            context,
            keys,
            link,
            defaults: Defaults::verify()?,
            registry,
        })))
    }

    pub(crate) fn sign(&self, snapshot: Snapshot, now: DateTime<Utc>) -> Result<Current, Error> {
        if !self
            .0
            .defaults
            .contains(snapshot.digest.as_bytes(), snapshot.size)
        {
            self.0.registry.close();
            return Err(Error::Mismatch);
        }
        Ok(Current {
            revision: snapshot.revision,
            link: self
                .0
                .link
                .sign(&self.0.keys.avatar, snapshot.digest, now)?,
        })
    }

    pub async fn read(&self, proof: link::Proof) -> Result<Image, Error> {
        self.0.registry.ready()?;
        proof.verify(&self.0.keys.avatar)?;
        let now = self.0.context.read_time().await.inspect_err(|error| {
            if error.unreliable() {
                self.0.registry.close();
            }
        })?;
        let cache = proof.remaining(now)?;
        let content = self
            .0
            .defaults
            .find(proof.digest().as_bytes())
            .ok_or(Error::Missing)?;
        Ok(Image {
            bytes: content.bytes(),
            cache,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql)]
#[serde(transparent)]
#[postgres(transparent)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = avatar::Revision)]
pub struct Revision(Uuid);

impl Revision {
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }
}

pub struct Defaults([Content; 10]);

impl Defaults {
    pub fn verify() -> Result<Self, Error> {
        let content = [
            Content::verify(
                include_bytes!("../../../../assets/avatars/0-cat.webp"),
                9242,
                "a92a8f686199a7388e2132dd61643a8df504b13379771e147c8157e06fb197cb",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/1-dog.webp"),
                5650,
                "44c8082012c527d0802bca6e257d3b582b657f49344e8714fcc6d24337b8f729",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/2-rabbit.webp"),
                5264,
                "d123aecfa88571e88966e457b597e2b70f8eadb4038cebfa225c4f0ea9c666cd",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/3-panda.webp"),
                7798,
                "cfa2bc678fa0ec2e889882b3e76c0267f874954a37b3a7e9481dd60a34f398c3",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/4-fox.webp"),
                9774,
                "51385dc3874dc5de94b6e104e04f059252c7e98c2d8795f7fa728f4f3847f51f",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/5-penguin.webp"),
                8732,
                "56637ab4029144eeb3a858bbc3be9c2dd9b4f952c51f08b159954938ab53c8ba",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/6-pig.webp"),
                7596,
                "2d6e500017e7e606c00c2f8d076b89b05645c015b308216c3338efaebbc41035",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/7-chick.webp"),
                7636,
                "187b99dc10a1a33d397b4b08d7a96911302c9cec7f2d7c7f3e529b7994bca2f7",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/8-bear.webp"),
                8916,
                "2a39d88cbb98c766ba5129783270e767709e91c09e8bf12029c4431e75a95010",
            )?,
            Content::verify(
                include_bytes!("../../../../assets/avatars/9-koala.webp"),
                7618,
                "bf110fccbcea510801c89c863c7644abeb41b4ff22aff9d307a7819ef0293c38",
            )?,
        ];
        Ok(Self(content))
    }

    pub(crate) fn select(&self, mobile: &Mobile) -> &Content {
        &self.0[usize::from(mobile.as_bytes()[10] - b'0')]
    }

    pub(crate) fn contains(&self, digest: &[u8; 32], size: i64) -> bool {
        self.0
            .iter()
            .any(|content| &content.digest == digest && content.size == size)
    }

    pub(crate) fn find(&self, digest: &[u8; 32]) -> Option<&Content> {
        self.0.iter().find(|content| &content.digest == digest)
    }
}

pub(crate) struct Content {
    digest: [u8; 32],
    size: i64,
    bytes: &'static [u8],
}

impl Content {
    pub(crate) fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    pub(crate) fn size(&self) -> i64 {
        self.size
    }
    pub(crate) fn bytes(&self) -> &'static [u8] {
        self.bytes
    }
    fn verify(bytes: &'static [u8], size: i64, expected: &str) -> Result<Self, Error> {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if i64::try_from(bytes.len())? != size || hex::encode(digest) != expected {
            return Err(Error::Mismatch);
        }
        // Frozen hashes identify the previously decoded 512×512 static lossless images.
        Ok(Self {
            digest,
            size,
            bytes,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("avatar content does not exist")]
    Missing,
    #[error("default avatar does not match the frozen manifest")]
    Mismatch,
    #[error("avatar fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("avatar metadata decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("avatar query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("default avatar size is not representable")]
    Size(#[from] std::num::TryFromIntError),
    #[error("{0}")]
    Link(#[from] link::Error),
    #[error("{0}")]
    Runtime(#[from] crate::runtime::Error),
    #[error("{0}")]
    Work(#[from] crate::runtime::work::Error),
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl Error {
    fn decode(error: tokio_postgres::Error) -> Self {
        Self::Decode(error.into())
    }

    fn invalid_digest(_: Vec<u8>) -> Self {
        Self::Facts {
            rule: "avatar content digest contains exactly 32 bytes",
        }
    }

    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::Runtime(error) => error.unreliable(),
            Self::Mismatch | Self::Facts { .. } | Self::Decode(_) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        identity::{
            authentication::{self, Database},
            avatar::{Content, Defaults, Error, Service, link},
        },
        runtime::{Context, registry::Registry},
    };

    #[test]
    fn a_valid_unknown_digest_is_missing_and_invalid_proof_does_not_require_the_clock() {
        let server = authentication::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(10), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let context = Context::new(database.connections(&database.user))
                    .await
                    .expect("runtime context");
                let keys = Arc::new(authentication::keys());
                let registry = Arc::new(Registry::new(1, tokio::sync::watch::channel(false).0));
                let service = Service::prepare(
                    context.clone(),
                    Arc::clone(&keys),
                    authentication::avatar_settings(),
                    Arc::clone(&registry),
                )
                .expect("frozen content");
                let digest = link::Digest::new([0; 32]);
                let signed = authentication::avatar_settings()
                    .sign(
                        &keys.avatar,
                        digest,
                        context.sample().await.expect("trusted signing time"),
                    )
                    .expect("signed unknown content");
                let url = url::Url::parse(signed.href()).expect("signed URL");
                let signature = url
                    .query_pairs()
                    .find_map(|(name, value)| (name == "signature").then(|| value.into_owned()))
                    .expect("signature");
                let expires = signed.expires_at().timestamp().to_string();
                let proof = link::Proof::new(
                    digest,
                    "0".parse().expect("key id"),
                    expires.parse().expect("expiration"),
                    signature.parse().expect("signature"),
                );
                assert!(matches!(service.read(proof).await, Err(Error::Missing)));
                context.stop().await.expect("clock and database stopped");
                let invalid = link::Proof::new(
                    digest,
                    "0".parse().expect("key id"),
                    expires.parse().expect("expiration"),
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                        .parse()
                        .expect("wrong signature"),
                );
                assert!(matches!(
                    service.read(invalid).await,
                    Err(Error::Link(link::Error::Invalid))
                ));
                registry
                    .ready()
                    .expect("invalid proof did not sample the closed clock");
                let proof = link::Proof::new(
                    digest,
                    "0".parse().expect("key id"),
                    expires.parse().expect("expiration"),
                    signature.parse().expect("signature"),
                );
                assert!(matches!(service.read(proof).await, Err(Error::Runtime(_))));
                assert!(
                    registry.ready().is_err(),
                    "a verified proof observes the unavailable clock"
                );
            })
            .await
            .expect("avatar refusal boundary budget");
        });
    }

    #[test]
    fn validates_every_published_asset_before_selection() {
        let defaults = Defaults::verify().expect("frozen approved bytes");
        let content = defaults.select(&"13800000000".parse().expect("mobile"));
        assert_eq!(content.size, 9242);
        assert_eq!(
            hex::encode(content.digest),
            "a92a8f686199a7388e2132dd61643a8df504b13379771e147c8157e06fb197cb"
        );
        assert!(matches!(
            Content::verify(b"damaged", 7, "invalid"),
            Err(Error::Mismatch)
        ));
    }
}
