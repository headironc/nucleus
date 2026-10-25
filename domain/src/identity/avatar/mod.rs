//! Current avatar metadata, frozen defaults and signed content reads.

pub(crate) mod codec;
mod database;
pub mod link;
pub mod retention;
pub mod storage;
pub(crate) mod upload;
use std::{str::FromStr, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
pub use codec::Media;
pub(crate) use database::Snapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use storage::{Error as StorageError, Settings as StorageSettings};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, timeout_at},
};
use tokio_postgres::types::{FromSql, ToSql};
pub(crate) use upload::COMPLETIONS;
pub use upload::{
    Completion as UploadCompletion, Limits as UploadLimits, LimitsError as UploadLimitsError,
    Rejection as UploadRejection,
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    identity::{current, keys::Keys, mobile::Mobile},
    kernel::Error as KernelError,
    runtime::{Context, registry::Registry},
};

#[derive(Clone)]
pub struct Service(Arc<Inner>);

struct Inner {
    app: crate::kernel::App,
    context: Context,
    keys: Arc<Keys>,
    link: link::Link,
    defaults: Defaults,
    registry: Arc<Registry>,
    storage: storage::Storage,
    limits: upload::Limits,
    slots: Arc<Semaphore>,
}

pub struct Image {
    bytes: Vec<u8>,
    cache: Duration,
}

impl Image {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn cache(&self) -> Duration {
        self.cache
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub struct Current {
    revision: Revision,
    link: link::Signed,
    width: u16,
    height: u16,
}

impl Current {
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn link(&self) -> &link::Signed {
        &self.link
    }
    pub fn width(&self) -> u16 {
        self.width
    }
    pub fn height(&self) -> u16 {
        self.height
    }
}

impl Service {
    pub(crate) async fn verify(
        &self,
        client: &tokio_postgres::Client,
        now: DateTime<Utc>,
    ) -> Result<(), KernelError<std::convert::Infallible>> {
        upload::verify(client, &self.0.app, now)
            .await
            .map_err(retention::cleanup_error)?;
        retention::verify(client, &self.0.app, now).await?;
        let mut cursor: Option<Id> = None;
        loop {
            let complete = verify_batch(async {
            let rows = client.query("SELECT id, revision AS avatar, account_id, digest, size, width, height, content_seal, content_seal_key_id, content_seal_version FROM identity.avatars WHERE $1::uuid IS NULL OR id>$1 ORDER BY id LIMIT 128", &[&cursor])
                .await.map_err(KernelError::query)?;
            if rows.is_empty() {
                return Ok(true);
            }
            for row in rows {
                let snapshot = self.snapshot(&row).map_err(retention::avatar_error)?;
                if self.0.defaults.find(snapshot.digest().as_bytes()).is_none() {
                    let size = u64::try_from(snapshot.size())
                        .map_err(|_| KernelError::corrupt("avatar size is positive"))?;
                    let bytes = self
                        .0
                        .storage
                        .read(snapshot.id(), size)
                        .map_err(retention::file_error)?;
                    let digest: [u8; 32] = Sha256::digest(&bytes).into();
                    if &digest != snapshot.digest().as_bytes() {
                        return Err(KernelError::corrupt(
                            "retained private avatar bytes match their digest",
                        ));
                    }
                }
                cursor = Some(snapshot.id());
            }
            Ok(false)
            }).await?;
            if complete {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn prepare(
        app: crate::kernel::App,
        context: Context,
        keys: Arc<Keys>,
        link: link::Link,
        registry: Arc<Registry>,
        storage: Option<storage::Settings>,
        limits: upload::Limits,
    ) -> Result<Self, Error> {
        let storage = match storage {
            Some(settings) => storage::Storage::open(&settings)?,
            #[cfg(test)]
            None => storage::Storage::temporary_for_test()?,
            #[cfg(not(test))]
            None => return Err(Error::Storage),
        };
        Ok(Self(Arc::new(Inner {
            app,
            context,
            keys,
            link,
            defaults: Defaults::verify()?,
            registry,
            storage,
            limits,
            slots: Arc::new(Semaphore::new(2)),
        })))
    }

    pub fn upload_slot(&self) -> Result<OwnedSemaphorePermit, Error> {
        self.0.registry.ready()?;
        Arc::clone(&self.0.slots)
            .try_acquire_owned()
            .map_err(|_| Error::Busy)
    }

    fn snapshot(&self, row: &tokio_postgres::Row) -> Result<Snapshot, Error> {
        Snapshot::read(row, self.0.app.keys().seals, &self.0.defaults)
    }

    pub(crate) async fn find_by_account(
        &self,
        client: &tokio_postgres::Client,
        account: &crate::history::Verified<crate::identity::account::state::State>,
    ) -> Result<Option<Snapshot>, Error> {
        database::find_by_account(client, account, self.0.app.keys().seals, &self.0.defaults).await
    }

    pub(crate) fn sign(&self, snapshot: Snapshot, now: DateTime<Utc>) -> Result<Current, Error> {
        Ok(Current {
            revision: snapshot.revision(),
            link: self
                .0
                .link
                .sign(&self.0.keys.avatar, snapshot.digest(), now)?,
            width: snapshot.width(),
            height: snapshot.height(),
        })
    }

    pub async fn read(
        &self,
        proof: link::Proof,
        capacity: Arc<OwnedSemaphorePermit>,
    ) -> Result<Image, Error> {
        let permit = self.0.registry.transfer(Arc::clone(&capacity))?;
        let service = self.clone();
        self.0
            .app
            .work()
            .run_reserved(
                Arc::clone(&self.0.registry),
                crate::runtime::registry::Family::Image,
                crate::identity::operation::Id::allocate(),
                permit,
                async move {
                    let runtime = tokio::runtime::Handle::current();
                    tokio::task::spawn_blocking(move || {
                        let _capacity = capacity;
                        runtime.block_on(service.read_content(proof))
                    })
                    .await
                    .map_err(|_| Error::Work(crate::runtime::work::Error::Worker))?
                },
            )
            .await
    }

    async fn read_content(&self, proof: link::Proof) -> Result<Image, Error> {
        self.0.registry.ready()?;
        proof.verify(&self.0.keys.avatar)?;
        let now = self.0.context.read_time().await.inspect_err(|error| {
            if error.unreliable() {
                self.0.registry.close();
            }
        })?;
        let cache = proof.remaining(now)?;
        if let Some(content) = self.0.defaults.find(proof.digest().as_bytes()) {
            return Ok(Image {
                bytes: content.bytes().to_vec(),
                cache,
            });
        }
        let lease = self
            .0
            .context
            .connections()
            .lease()
            .await
            .map_err(Error::Connection)?;
        let digest = *proof.digest().as_bytes();
        let result = timeout_at(Instant::now() + Duration::from_secs(3), async {
            lease.client().batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout='3s'").await?;
            let row = lease.client().query_opt(
                "SELECT id, revision AS avatar, account_id, digest, size, width, height, content_seal, content_seal_key_id, content_seal_version FROM identity.avatars WHERE digest=$1 ORDER BY id LIMIT 1 FOR SHARE",
                &[&&digest[..]],
            ).await?;
            let Some(row) = row else { return Ok(None); };
            let snapshot = self.snapshot(&row)?;
            // A deletion must take the conflicting row lock. Keep this descriptor after
            // releasing that lock so a later unlink cannot invalidate the accepted read.
            let reader = self.0.storage.open_file(snapshot.id())?
                .ok_or(Error::Facts { rule: "a retained avatar owns its private file" })?;
            Ok::<_, Error>(Some((reader, snapshot.size())))
        }).await;
        let opened = match result {
            Ok(Ok(opened)) => {
                match timeout_at(
                    Instant::now() + Duration::from_secs(3),
                    lease.client().batch_execute("COMMIT"),
                )
                .await
                {
                    Ok(Ok(())) => lease.release(),
                    end => {
                        tracing::error!(stage = "avatar_read_end", error = ?end);
                        if let Err(error) = lease.discard().await {
                            tracing::error!(stage = "avatar_read_connection_end", error = ?error);
                        }
                    }
                }
                opened
            }
            Ok(Err(error)) => {
                if let Err(cleanup) = lease.discard().await {
                    tracing::error!(stage = "avatar_read_connection_end", error = ?cleanup);
                }
                if error.unreliable() {
                    self.0.registry.close();
                }
                return Err(error);
            }
            Err(elapsed) => {
                if let Err(error) = lease.discard().await {
                    tracing::error!(stage = "avatar_read_connection_end", error = ?error);
                }
                return Err(Error::Timeout(elapsed));
            }
        };
        let (reader, size) = opened.ok_or(Error::Missing)?;
        let bytes = reader
            .read(u64::try_from(size)?)
            .map_err(Error::Files)
            .inspect_err(|error| {
                if error.unreliable() {
                    self.0.registry.close();
                }
            })?;
        let stored: [u8; 32] = Sha256::digest(&bytes).into();
        if stored != digest {
            self.0.registry.close();
            return Err(Error::Facts {
                rule: "private avatar bytes match their signed digest",
            });
        }
        Ok(Image { bytes, cache })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Original input and execution resources have distinct owners."
    )]
    pub async fn upload(
        &self,
        current: &current::Service,
        actor: current::Actor,
        operation_id: crate::identity::operation::Id,
        expected: Revision,
        declared: String,
        bytes: Vec<u8>,
        permit: OwnedSemaphorePermit,
        total: Instant,
    ) -> Result<Completed, KernelError<upload::Rejection>> {
        let completion = upload::run(
            &self.0.app,
            current,
            self.0.storage.clone(),
            self.0.defaults.clone(),
            self.0.limits,
            actor.clone(),
            operation_id,
            expected,
            declared,
            bytes,
            permit,
            total,
        )
        .await?;
        let upload::Lookup::Available(completed) = upload::lookup(
            &self.0.app,
            &self.0.defaults,
            current,
            &actor,
            operation_id,
            total,
        )
        .await?
        else {
            return Err(KernelError::corrupt(
                "a completed upload retains its publicly readable result",
            ));
        };
        if completion.revision() != completed.completion.revision()
            || completion.changed() != completed.completion.changed()
            || completion.completed_at() != completed.completion.completed_at()
        {
            return Err(KernelError::corrupt(
                "response preserves the original upload judgment",
            ));
        }
        let now = self
            .0
            .context
            .read_time()
            .await
            .map_err(KernelError::clock)?;
        let avatar = self
            .sign(completed.snapshot, now)
            .map_err(upload::avatar_fault)?;
        Ok(Completed { completion, avatar })
    }

    pub async fn original(
        &self,
        current: &current::Service,
        actor: &current::Actor,
        operation_id: crate::identity::operation::Id,
        deadline: Instant,
    ) -> Result<OriginalView, KernelError<upload::Rejection>> {
        match upload::lookup(
            &self.0.app,
            &self.0.defaults,
            current,
            actor,
            operation_id,
            deadline,
        )
        .await?
        {
            upload::Lookup::Available(completed) => {
                let now = self
                    .0
                    .context
                    .read_time()
                    .await
                    .map_err(KernelError::clock)?;
                let avatar = self
                    .sign(completed.snapshot, now)
                    .map_err(upload::avatar_fault)?;
                Ok(OriginalView::Available {
                    completion: completed.completion,
                    avatar,
                })
            }
            upload::Lookup::Pending => Ok(OriginalView::Pending),
            upload::Lookup::Absent => Ok(OriginalView::Absent),
        }
    }
}

pub struct Completed {
    completion: upload::Completion,
    avatar: Current,
}

impl Completed {
    pub fn completion(&self) -> &upload::Completion {
        &self.completion
    }
    pub fn avatar(&self) -> &Current {
        &self.avatar
    }
}

pub enum OriginalView {
    Available {
        completion: upload::Completion,
        avatar: Current,
    },
    Pending,
    Absent,
}

impl OriginalView {
    pub fn available(self) -> Option<(upload::Completion, Current)> {
        match self {
            Self::Available { completion, avatar } => Some((completion, avatar)),
            _ => None,
        }
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
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
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

impl FromStr for Revision {
    type Err = uuid::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(input)?))
    }
}

#[derive(Clone)]
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

#[derive(Clone)]
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

async fn verify_batch<T, R>(
    batch: impl std::future::Future<Output = Result<T, KernelError<R>>>,
) -> Result<T, KernelError<R>> {
    use crate::kernel::error::{Fault, FaultKind, Stage};

    crate::database::verification::Stage::Avatars
        .batch(Duration::from_secs(10), batch)
        .await
        .map_err(|error| {
            KernelError::Unavailable(Fault::new(
                Stage::Command,
                FaultKind::Avatar(Error::Verification(error)),
            ))
        })?
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
    #[error("avatar content proof failed: {0}")]
    Seal(#[from] crate::keys::seal::Error),
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
    #[error("request work capacity is exhausted")]
    Busy,
    #[error("avatar storage is not configured")]
    Storage,
    #[error("lossless avatar encoding failed")]
    Encode(#[source] image::ImageError),
    #[error("{0}")]
    Files(#[from] storage::Error),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("avatar read budget expired")]
    Timeout(tokio::time::error::Elapsed),
    #[error("{0}")]
    Verification(#[source] crate::database::verification::Timeout),
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
            Self::Mismatch
            | Self::Facts { .. }
            | Self::Decode(_)
            | Self::Seal(_)
            | Self::Storage
            | Self::Files(_) => true,
            _ => false,
        }
    }
}

impl crate::runtime::work::Failure for Error {
    fn unreliable(&self) -> bool {
        Error::unreliable(self)
    }

    fn report(&self, operation_id: crate::identity::operation::Id) {
        if !matches!(self, Self::Link(link::Error::Invalid) | Self::Missing) {
            tracing::error!(stage = "avatar_content", %operation_id, error = ?self);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) fn read_capacity() -> Arc<OwnedSemaphorePermit> {
    Arc::new(
        Arc::new(Semaphore::new(1))
            .try_acquire_owned()
            .expect("new fixture capacity"),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        identity::{
            authentication::{self, Database},
            avatar::{Content, Defaults, Error, Service, link, upload},
        },
        runtime::{Context, registry::Registry},
    };

    #[test]
    fn image_work_retains_capacity_and_drain_after_its_waiter_leaves() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let registry = Arc::new(Registry::new(1, tokio::sync::watch::channel(false).0));
                let write = registry
                    .acquire(crate::identity::operation::Id::allocate())
                    .await
                    .expect("occupy the independent write capacity");
                let transfers = Arc::new(tokio::sync::Semaphore::new(8));
                let other = Arc::clone(&transfers)
                    .try_acquire_many_owned(7)
                    .expect("seven other transfers");
                let capacity = Arc::new(
                    Arc::clone(&transfers)
                        .try_acquire_owned()
                        .expect("eighth transfer"),
                );
                let permit = registry
                    .transfer(Arc::clone(&capacity))
                    .expect("read reservation");
                let work = Arc::new(crate::runtime::work::Work::default());
                let (started, entered) = tokio::sync::oneshot::channel();
                let (release, released) = std::sync::mpsc::channel();
                let waiter = tokio::spawn({
                    let work = Arc::clone(&work);
                    let registry = Arc::clone(&registry);
                    async move {
                        work.run_reserved(
                            registry,
                            crate::runtime::registry::Family::Image,
                            crate::identity::operation::Id::allocate(),
                            permit,
                            async move {
                                tokio::task::spawn_blocking(move || {
                                    let _capacity = capacity;
                                    started.send(()).expect("observer remains");
                                    released
                                        .recv_timeout(Duration::from_secs(5))
                                        .expect("release worker");
                                })
                                .await
                                .expect("blocking worker");
                                Ok::<(), Error>(())
                            },
                        )
                        .await
                    }
                });
                timeout(Duration::from_secs(2), entered)
                    .await
                    .expect("worker starts")
                    .expect("worker entered");
                waiter.abort();
                assert!(waiter.await.expect_err("waiter aborted").is_cancelled());
                assert!(Arc::clone(&transfers).try_acquire_owned().is_err());
                let drain = work.drain(&registry);
                tokio::pin!(drain);
                assert!(
                    timeout(Duration::from_millis(20), &mut drain)
                        .await
                        .is_err(),
                    "the single runtime thread remains responsive while drain waits for real work"
                );
                assert_eq!(transfers.available_permits(), 0);
                release.send(()).expect("real worker is still owned");
                timeout(Duration::from_secs(2), drain)
                    .await
                    .expect("drain completes")
                    .expect("successful work");
                assert_eq!(transfers.available_permits(), 1);
                drop(other);
                assert_eq!(transfers.available_permits(), 8);
                drop(write);
            });
    }

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
                let inventory = authentication::inventory(authentication::keys());
                let app = crate::kernel::App::new(
                    context.clone(),
                    Arc::clone(&registry),
                    Arc::new(crate::runtime::work::Work::default()),
                    Arc::new(inventory.encryption),
                    Arc::new(inventory.seal),
                );
                let service = Service::prepare(
                    app,
                    context.clone(),
                    Arc::clone(&keys),
                    authentication::avatar_settings(),
                    Arc::clone(&registry),
                    None,
                    upload::Limits::default(),
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
                assert!(matches!(
                    service.read(proof, super::read_capacity()).await,
                    Err(Error::Missing)
                ));
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
                    service.read(invalid, super::read_capacity()).await,
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
                assert!(matches!(
                    service.read(proof, super::read_capacity()).await,
                    Err(Error::Runtime(_))
                ));
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
