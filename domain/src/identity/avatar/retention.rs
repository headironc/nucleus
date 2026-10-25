//! Reclaim released avatar metadata and private files under their exact releasing work.

use std::{convert::Infallible, sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use sha2::{Digest as _, Sha256};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Row;

use crate::{
    history::{self, Read, Record as _},
    identity::{
        account,
        avatar::{Id, Service},
        operation,
    },
    kernel::{
        App, Error,
        error::{Fault, FaultKind, Stage},
        operation::Family,
        tx::{Connection, Tx},
    },
    runtime::{registry, work},
};

const BUDGET: Duration = Duration::from_secs(10);
const BATCH: usize = 128;
pub(crate) const DELETIONS: &str = "identity.deletions";

#[derive(Clone, Copy)]
pub struct Settings {
    grace: TimeDelta,
}

impl Settings {
    pub fn new(seconds: u64) -> Result<Self, SettingsError> {
        let seconds = i64::try_from(seconds).map_err(SettingsError::range)?;
        let grace = TimeDelta::try_seconds(seconds)
            .filter(|value| *value > TimeDelta::zero())
            .ok_or(SettingsError)?;
        Ok(Self { grace })
    }

    pub fn seconds(self) -> u64 {
        self.grace.num_seconds().unsigned_abs()
    }

    fn expired(self, released: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(released) >= self.grace
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            grace: TimeDelta::days(1),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("avatar retention must be positive whole seconds representable by the clock")]
pub struct SettingsError;

impl SettingsError {
    fn range(_: std::num::TryFromIntError) -> Self {
        Self
    }
}

pub struct Retention {
    app: App,
    avatar: Service,
    settings: Settings,
    cursor: Option<(DateTime<Utc>, Id)>,
    publication_cursor: Option<operation::Id>,
    deletion_cursor: Option<Id>,
}

#[derive(Clone, Copy)]
struct Candidate {
    id: Id,
    account: account::Id,
    operation: operation::Id,
    family: Family,
}

impl Retention {
    pub(crate) fn new(app: App, avatar: Service, settings: Settings) -> Self {
        Self {
            app,
            avatar,
            settings,
            cursor: None,
            publication_cursor: None,
            deletion_cursor: None,
        }
    }

    /// Startup owns the instance and exposes no request service until all retained file
    /// intentions have crossed their original lock and reached a confirmed outcome.
    pub(crate) async fn restore(&mut self) -> Result<(), Error<Infallible>> {
        loop {
            Box::pin(self.recover_files()).await?;
            if self.publication_cursor.is_none() && self.deletion_cursor.is_none() {
                return Ok(());
            }
        }
    }

    /// The process owns this round through cancellation, recovery and connection termination.
    pub async fn run(&mut self) -> Result<(), Error<Infallible>> {
        if let Err(error) = Box::pin(self.recover_files()).await {
            return self.handle(error, "avatar_file_recovery");
        }
        let registry = self.app.registry();
        registry.ready()?;
        let rows = match self.candidates().await {
            Ok(rows) => rows,
            Err(error) => return self.handle(error, "avatar_retention_scan"),
        };
        let complete = rows.len() < BATCH;
        for (released, candidate) in rows {
            registry.ready()?;
            self.cursor = Some((released, candidate.id));
            let mut permit = match registry
                .try_acquire_for(
                    registry::Family::Kernel(candidate.family),
                    candidate.operation,
                )
                .await
            {
                Ok(permit) => permit,
                Err(work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let closing = Arc::clone(registry);
            let guard = scopeguard::guard((), |_| {
                closing.close();
                tracing::error!(stage = "avatar_retention", error = ?work::Error::Worker);
            });
            let result = self.reclaim(&candidate).await;
            scopeguard::ScopeGuard::into_inner(guard);
            permit.finish();
            if let Err(error) = result {
                self.handle(error, "avatar_retention")?;
            }
        }
        if complete {
            self.cursor = None;
        }
        Ok(())
    }

    async fn recover_files(&mut self) -> Result<(), Error<Infallible>> {
        let lease = self
            .app
            .context()
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let scanned = timeout_at(Instant::now() + BUDGET, async {
            let publications = lease.client().query(
                "SELECT operation_id FROM identity.publications WHERE $1::uuid IS NULL OR operation_id>$1 ORDER BY operation_id LIMIT 128",
                &[&self.publication_cursor],
            ).await.map_err(Error::query)?;
            let deletions = lease.client().query(
                "SELECT id, account_id, release_operation_id, release FROM identity.deletions WHERE $1::uuid IS NULL OR id>$1 ORDER BY id LIMIT 128",
                &[&self.deletion_cursor],
            ).await.map_err(Error::query)?;
            Ok((publications, deletions))
        }).await.unwrap_or_else(Error::elapsed);
        let (publications, deletions) = match scanned {
            Ok(rows) => {
                lease.release();
                rows
            }
            Err(error) => return Err(ended(error, lease.discard().await.map_err(termination))),
        };
        let publications_complete = publications.len() < BATCH;
        for row in publications {
            let operation = row.try_get("operation_id").map_err(Error::decode)?;
            self.publication_cursor = Some(operation);
            let mut permit = match self
                .app
                .registry()
                .try_acquire_for(registry::Family::Kernel(Family::Avatar), operation)
                .await
            {
                Ok(permit) => permit,
                Err(work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let registry = Arc::clone(self.app.registry());
            let guard = scopeguard::guard((), |_| registry.close());
            let result = super::upload::settle(&self.app, &self.avatar.0.storage, operation).await;
            scopeguard::ScopeGuard::into_inner(guard);
            permit.finish();
            result.map_err(cleanup_error)?;
        }
        if publications_complete {
            self.publication_cursor = None;
        }
        let deletions_complete = deletions.len() < BATCH;
        for row in deletions {
            let candidate = Candidate {
                id: row.try_get("id").map_err(Error::decode)?,
                account: row.try_get("account_id").map_err(Error::decode)?,
                operation: row.try_get("release_operation_id").map_err(Error::decode)?,
                family: release_family(row.try_get::<_, &str>("release").map_err(Error::decode)?)?,
            };
            self.deletion_cursor = Some(candidate.id);
            let mut permit = match self
                .app
                .registry()
                .try_acquire_for(
                    registry::Family::Kernel(candidate.family),
                    candidate.operation,
                )
                .await
            {
                Ok(permit) => permit,
                Err(work::Error::Busy) => continue,
                Err(error) => return Err(error.into()),
            };
            permit.begin();
            let registry = Arc::clone(self.app.registry());
            let guard = scopeguard::guard((), |_| registry.close());
            let result = self.finish_deletion(&candidate).await;
            scopeguard::ScopeGuard::into_inner(guard);
            permit.finish();
            result?;
        }
        if deletions_complete {
            self.deletion_cursor = None;
        }
        Ok(())
    }

    fn handle(
        &self,
        error: Error<Infallible>,
        stage: &'static str,
    ) -> Result<(), Error<Infallible>> {
        tracing::error!(stage, error = ?error);
        if error.is_corrupt() {
            self.app.registry().close();
        }
        if error.is_corrupt() || error.is_closed() || self.app.registry().ready().is_err() {
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn candidates(&self) -> Result<Vec<(DateTime<Utc>, Candidate)>, Error<Infallible>> {
        let context = self.app.context();
        let lease = self
            .app
            .registry()
            .lease_until_closed::<Waiting>(context.connections())
            .await
            .map_err(Waiting::into_error)?;
        let result = timeout_at(Instant::now() + BUDGET, async {
            let now = context.now(lease.client()).await.map_err(Error::clock)?;
            let Some(cutoff) = now.checked_sub_signed(self.settings.grace) else { return Ok(Vec::new()); };
            let rows = lease.client().query(
                "SELECT id, account_id, released_at, release, release_operation_id FROM identity.avatars WHERE released_at <= $1 AND ($2::timestamptz IS NULL OR (released_at,id) > ($2,$3)) ORDER BY released_at,id LIMIT 128",
                &[&cutoff, &self.cursor.map(|cursor| cursor.0), &self.cursor.map(|cursor| cursor.1)],
            ).await.map_err(Error::query)?;
            let mut candidates = Vec::with_capacity(rows.len());
            for row in rows {
                let id = row.try_get("id").map_err(Error::decode)?;
                let account = row.try_get("account_id").map_err(Error::decode)?;
                let released = row.try_get("released_at").map_err(Error::decode)?;
                let operation = row.try_get("release_operation_id").map_err(Error::decode)?;
                let family = release_family(row.try_get::<_, &str>("release").map_err(Error::decode)?)?;
                candidates.push((released, Candidate { id, account, operation, family }));
            }
            Ok(candidates)
        }).await.unwrap_or_else(Error::elapsed);
        match result {
            Ok(candidates) => {
                lease.release();
                Ok(candidates)
            }
            Err(error) => Err(ended(error, lease.discard().await.map_err(termination))),
        }
    }

    async fn reclaim(&self, candidate: &Candidate) -> Result<(), Error<Infallible>> {
        let fixed = self.retained(candidate).await?;
        let Some(fixed) = fixed else {
            return self.finish_deletion(candidate).await;
        };
        let context = self.app.context();
        let lease = context
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            context,
            Connection::Online(lease),
            Instant::now() + BUDGET,
            candidate.family,
            candidate.operation,
        )
        .await;
        if let Err(fault) = begun {
            return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
        }
        let deleted = match timeout_at(tx.deadline(), self.delete(&tx, candidate, &fixed))
            .await
            .unwrap_or_else(Error::elapsed)
        {
            Ok(deleted) => deleted,
            Err(error) => return Err(ended(error, tx.discard().await)),
        };
        let cause = match tx.commit().await {
            Ok(()) => return self.finish_deletion(candidate).await,
            Err(fault) if !deleted => return Err(Error::Unavailable(fault)),
            Err(fault) => fault,
        };
        match self.deleted(candidate).await {
            Ok((true, end)) => {
                tracing::error!(stage = "avatar_retention", outcome = "deleted", error = ?cause.then_result(end));
                self.finish_deletion(candidate).await
            }
            Ok((false, end)) => Err(Error::Unavailable(cause.then_result(end))),
            Err(error) => {
                if error.is_corrupt() {
                    self.app.registry().close();
                }
                Err(Error::Unknown(cause.then_error(error)))
            }
        }
    }

    async fn retained(
        &self,
        candidate: &Candidate,
    ) -> Result<Option<super::Snapshot>, Error<Infallible>> {
        let lease = self
            .app
            .context()
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let result = timeout_at(Instant::now() + BUDGET, async {
            lease.client().query_opt(
                "SELECT id, revision AS avatar, account_id, digest, size, width, height, released_at, release, release_operation_id, content_seal, content_seal_key_id, content_seal_version FROM identity.avatars WHERE id=$1",
                &[&candidate.id],
            ).await.map_err(Error::query)
        }).await.unwrap_or_else(Error::elapsed);
        let row = match result {
            Ok(row) => {
                lease.release();
                row
            }
            Err(error) => return Err(ended(error, lease.discard().await.map_err(termination))),
        };
        let Some(row) = row else {
            return Ok(None);
        };
        self.validate(&row, candidate)?;
        let snapshot = self.avatar.snapshot(&row).map_err(avatar_error)?;
        if self
            .avatar
            .0
            .defaults
            .find(snapshot.digest().as_bytes())
            .is_none()
        {
            // File work is outside the deletion transaction; its locked row is compared below.
            let storage = self.avatar.0.storage.clone();
            let (id, size, expected) = (
                snapshot.id(),
                snapshot.size() as u64,
                *snapshot.digest().as_bytes(),
            );
            tokio::task::spawn_blocking(move || {
                let bytes = storage.read(id, size).map_err(file_error)?;
                let digest: [u8; 32] = Sha256::digest(&bytes).into();
                if digest != expected {
                    return Err(Error::corrupt(
                        "a retained private avatar matches its file digest",
                    ));
                }
                Ok(())
            })
            .await
            .map_err(|_| Error::Unavailable(Fault::new(Stage::Command, FaultKind::Worker)))??;
        }
        Ok(Some(snapshot))
    }

    async fn delete(
        &self,
        tx: &Tx,
        candidate: &Candidate,
        fixed: &super::Snapshot,
    ) -> Result<bool, Error<Infallible>> {
        account::state::lock(tx.client(), candidate.account).await?;
        let now = tx.now(self.app.context()).await?;
        let state = history::find::<account::state::State>(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            Read::Locked,
            candidate.account,
            now,
        )
        .await?
        .ok_or_else(|| Error::corrupt("released avatar retains its account"))?;
        let record = match candidate.family {
            Family::Cancellation => {
                let cancellation = state.cancellation_id().ok_or_else(|| {
                    Error::corrupt("cancellation release retains its account tombstone")
                })?;
                account::record::cancellation(tx.client(), self.app.keys(), cancellation, now)
                    .await?
            }
            Family::Avatar => {
                account::record::avatar(tx.client(), self.app.keys(), candidate.operation, now)
                    .await?
            }
            _ => {
                return Err(Error::corrupt(
                    "avatar release belongs to its declared work family",
                ));
            }
        };
        if record.account_id() != candidate.account || record.operation_id() != candidate.operation
        {
            return Err(Error::corrupt(
                "released avatar belongs to its original account operation",
            ));
        }
        let row = tx.client().query_opt(
            "SELECT id, revision AS avatar, account_id, digest, size, width, height, released_at, release, release_operation_id, content_seal, content_seal_key_id, content_seal_version FROM identity.avatars WHERE id=$1 FOR UPDATE",
            &[&candidate.id],
        ).await.map_err(Error::query)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let released = self.validate(&row, candidate)?;
        let locked = self.avatar.snapshot(&row).map_err(avatar_error)?;
        if locked.id() != fixed.id()
            || locked.revision() != fixed.revision()
            || locked.digest().as_bytes() != fixed.digest().as_bytes()
            || locked.size() != fixed.size()
            || locked.width() != fixed.width()
            || locked.height() != fixed.height()
        {
            return Err(Error::corrupt(
                "the deletion retains the verified avatar file metadata",
            ));
        }
        if released != record.happened_at() {
            return Err(Error::corrupt(
                "avatar release matches its permanent account record",
            ));
        }
        if !self.settings.expired(released, now) {
            return Ok(false);
        }
        let deletion = Deletion::released(&row, now)?;
        deletion.insert(tx.client(), self.app.keys()).await?;
        tx.client()
            .execute("DELETE FROM identity.avatars WHERE id=$1", &[&candidate.id])
            .await
            .map_err(Error::query)?;
        Ok(true)
    }

    fn validate(
        &self,
        row: &Row,
        candidate: &Candidate,
    ) -> Result<DateTime<Utc>, Error<Infallible>> {
        if row
            .try_get::<_, account::Id>("account_id")
            .map_err(Error::decode)?
            != candidate.account
            || row
                .try_get::<_, operation::Id>("release_operation_id")
                .map_err(Error::decode)?
                != candidate.operation
            || release_family(row.try_get::<_, &str>("release").map_err(Error::decode)?)?
                != candidate.family
        {
            return Err(Error::corrupt(
                "released avatar matches its immutable content and owner",
            ));
        }
        row.try_get::<_, Option<DateTime<Utc>>>("released_at")
            .map_err(Error::decode)?
            .ok_or_else(|| Error::corrupt("current avatar is not a cleanup candidate"))
    }

    async fn deleted(
        &self,
        candidate: &Candidate,
    ) -> Result<(bool, Result<(), Fault>), Error<Infallible>> {
        let context = self.app.context();
        let session = context
            .connections()
            .recover()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            context,
            Connection::Recovery(session),
            Instant::now() + BUDGET,
            candidate.family,
            candidate.operation,
        )
        .await;
        let result = match begun {
            Ok(()) => timeout_at(tx.deadline(), async {
                let retained: bool = tx
                    .client()
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM identity.avatars WHERE id=$1)",
                        &[&candidate.id],
                    )
                    .await
                    .map_err(Error::query)?
                    .try_get(0)
                    .map_err(Error::decode)?;
                let deletion = Deletion::find(tx.client(), self.app.keys(), candidate.id).await?;
                if retained && deletion.is_some() {
                    return Err(Error::corrupt(
                        "a deletion intent cannot retain its avatar resource",
                    ));
                }
                if let Some(deletion) = deletion {
                    deletion.check(candidate)?;
                } else if !retained {
                    return Err(Error::corrupt(
                        "removed avatar has its exact confirmed deletion intent",
                    ));
                }
                Ok(!retained)
            })
            .await
            .unwrap_or_else(Error::elapsed),
            Err(fault) => Err(Error::Unavailable(fault)),
        };
        match result {
            Ok(deleted) => Ok((deleted, tx.commit().await)),
            Err(error) => Err(ended(error.at(Stage::Recovery), tx.discard().await)),
        }
    }

    async fn finish_deletion(&self, candidate: &Candidate) -> Result<(), Error<Infallible>> {
        let session = self
            .app
            .context()
            .connections()
            .recover()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            self.app.context(),
            Connection::Recovery(session),
            Instant::now() + BUDGET,
            candidate.family,
            candidate.operation,
        )
        .await;
        if let Err(fault) = begun {
            return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
        }
        let found = timeout_at(
            tx.deadline(),
            Deletion::find(tx.client(), self.app.keys(), candidate.id),
        )
        .await
        .unwrap_or_else(Error::elapsed);
        let deletion = match found {
            Ok(deletion) => {
                let (known, cleanup) = tx.confirm(deletion).await;
                super::upload::report_end(known.end, cleanup);
                known.value
            }
            Err(error) => return Err(ended(error, tx.discard().await)),
        };
        let Some(deletion) = deletion else {
            return Ok(());
        };
        deletion.check(candidate)?;
        // The authenticated intent was observed in a separate transaction after removal.
        // Default bytes are embedded and never enter the private filesystem lifecycle.
        match self.avatar.0.defaults.find(&deletion.digest) {
            Some(content) if content.size() != deletion.size => {
                return Err(Error::corrupt("default deletion retains its frozen size"));
            }
            Some(_) => {}
            None => {
                let storage = self.avatar.0.storage.clone();
                let id = deletion.id;
                tokio::task::spawn_blocking(move || storage.remove(id))
                    .await
                    .map_err(|_| Error::Unavailable(Fault::new(Stage::Command, FaultKind::Worker)))?
                    .map_err(file_error)?;
            }
        }
        let lease = self
            .app
            .context()
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            self.app.context(),
            Connection::Online(lease),
            Instant::now() + BUDGET,
            candidate.family,
            candidate.operation,
        )
        .await;
        if let Err(fault) = begun {
            return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
        }
        let result = timeout_at(tx.deadline(), async {
            let retained = Deletion::find(tx.client(), self.app.keys(), candidate.id).await?;
            if let Some(retained) = retained {
                if retained.fields() != deletion.fields() {
                    return Err(Error::corrupt(
                        "file cleanup retains its exact deletion intent",
                    ));
                }
                tx.client()
                    .execute(
                        "DELETE FROM identity.deletions WHERE id=$1",
                        &[&candidate.id],
                    )
                    .await
                    .map_err(Error::query)?;
            }
            Ok(())
        })
        .await
        .unwrap_or_else(Error::elapsed);
        match result {
            Ok(()) => tx.commit().await.map_err(Error::Unknown),
            Err(error) => Err(ended(error, tx.discard().await)),
        }
    }
}

const DELETION_SEAL: &str = "identity.avatar.deletion";

pub(super) async fn verify(
    client: &tokio_postgres::Client,
    app: &App,
    now: DateTime<Utc>,
) -> Result<(), Error<Infallible>> {
    let mut cursor: Option<Id> = None;
    loop {
        let complete = super::verify_batch(async {
        let rows = client.query("SELECT id FROM identity.deletions WHERE $1::uuid IS NULL OR id>$1 ORDER BY id LIMIT 128", &[&cursor])
            .await.map_err(Error::query)?;
        if rows.is_empty() {
            return Ok(true);
        }
        for row in rows {
            let id = row.try_get("id").map_err(Error::decode)?;
            let deletion = Deletion::find(client, app.keys(), id)
                .await?
                .ok_or_else(|| Error::corrupt("startup retains each discovered deletion intent"))?;
            if deletion.released_at > deletion.created_at || deletion.created_at > now {
                return Err(Error::corrupt(
                    "private deletion times are ordered and not future facts",
                ));
            }
            release_family(&deletion.release)?;
            let retained: bool = client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM identity.avatars WHERE id=$1)",
                    &[&id],
                )
                .await
                .map_err(Error::query)?
                .try_get(0)
                .map_err(Error::decode)?;
            if retained {
                return Err(Error::corrupt(
                    "private deletion follows confirmed avatar removal",
                ));
            }
            cursor = Some(id);
        }
        Ok(false)
        }).await?;
        if complete {
            break;
        }
    }
    Ok(())
}

struct Deletion {
    id: Id,
    account: account::Id,
    digest: [u8; 32],
    size: i64,
    released_at: DateTime<Utc>,
    release: String,
    operation: operation::Id,
    created_at: DateTime<Utc>,
}

impl Deletion {
    fn released(row: &Row, created_at: DateTime<Utc>) -> Result<Self, Error<Infallible>> {
        Ok(Self {
            id: row.try_get("id").map_err(Error::decode)?,
            account: row.try_get("account_id").map_err(Error::decode)?,
            digest: row
                .try_get::<_, Vec<u8>>("digest")
                .map_err(Error::decode)?
                .try_into()
                .map_err(invalid_digest)?,
            size: row.try_get("size").map_err(Error::decode)?,
            released_at: row.try_get("released_at").map_err(Error::decode)?,
            release: row.try_get("release").map_err(Error::decode)?,
            operation: row.try_get("release_operation_id").map_err(Error::decode)?,
            created_at,
        })
    }

    fn fields(&self) -> Vec<Vec<u8>> {
        vec![
            self.id.as_bytes().to_vec(),
            self.account.as_bytes().to_vec(),
            self.digest.to_vec(),
            self.size.to_be_bytes().to_vec(),
            self.released_at.timestamp_micros().to_be_bytes().to_vec(),
            self.release.as_bytes().to_vec(),
            self.operation.as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_be_bytes().to_vec(),
        ]
    }

    fn check(&self, candidate: &Candidate) -> Result<(), Error<Infallible>> {
        if self.id != candidate.id
            || self.account != candidate.account
            || self.operation != candidate.operation
            || release_family(&self.release)? != candidate.family
        {
            return Err(Error::corrupt(
                "deletion intent retains its resource, account and original release",
            ));
        }
        Ok(())
    }

    async fn find(
        client: &tokio_postgres::Client,
        keys: history::Keys<'_>,
        id: Id,
    ) -> Result<Option<Self>, Error<Infallible>> {
        let row = client.query_opt("SELECT id, account_id, digest, size, released_at, release, release_operation_id, created_at, seal, seal_key_id, seal_version FROM identity.deletions WHERE id=$1", &[&id])
            .await.map_err(Error::query)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let value = Self::released(&row, row.try_get("created_at").map_err(Error::decode)?)?;
        let seal = crate::keys::seal::Seal::stored(
            row.try_get("seal").map_err(Error::decode)?,
            row.try_get("seal_key_id").map_err(Error::decode)?,
            row.try_get("seal_version").map_err(Error::decode)?,
        )
        .map_err(|_| Error::corrupt("private deletion seal shape"))?;
        let fields = value.fields();
        keys.seals
            .check(
                DELETION_SEAL,
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                &seal,
            )
            .map_err(|_| Error::corrupt("private deletion retains its authenticated intent"))?;
        Ok(Some(value))
    }

    async fn insert(
        &self,
        client: &tokio_postgres::Client,
        keys: history::Keys<'_>,
    ) -> Result<(), Error<Infallible>> {
        let fields = self.fields();
        let seal = keys
            .seals
            .sign(
                DELETION_SEAL,
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .map_err(|_| Error::corrupt("private deletion seal computation"))?;
        client.execute("INSERT INTO identity.deletions (id, account_id, digest, size, released_at, release, release_operation_id, created_at, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[&self.id, &self.account, &&self.digest[..], &self.size, &self.released_at, &self.release,
                &self.operation, &self.created_at, &seal.tag(), &seal.key_id(), &seal.version()])
            .await.map_err(Error::query)?;
        Ok(())
    }
}

fn release_family(value: &str) -> Result<Family, Error<Infallible>> {
    match value {
        "cancellation" => Ok(Family::Cancellation),
        "replacement" => Ok(Family::Avatar),
        _ => Err(Error::corrupt(
            "avatar release has a declared original work family",
        )),
    }
}

pub(super) fn avatar_error(error: super::Error) -> Error<Infallible> {
    let corrupt = error.unreliable();
    let fault = Fault::new(Stage::Command, FaultKind::Avatar(error));
    if corrupt {
        Error::Corrupt(fault)
    } else {
        Error::Unavailable(fault)
    }
}

pub(super) fn file_error(error: super::storage::Error) -> Error<Infallible> {
    let corrupt = matches!(error, super::storage::Error::Identity);
    let fault = Fault::new(Stage::Command, FaultKind::File(error));
    if corrupt {
        Error::Corrupt(fault)
    } else {
        Error::Unavailable(fault)
    }
}

pub(super) fn cleanup_error(error: Error<super::upload::Rejection>) -> Error<Infallible> {
    match error {
        Error::Unavailable(fault) => Error::Unavailable(fault),
        Error::Unknown(fault) => Error::Unknown(fault),
        Error::Corrupt(fault) => Error::Corrupt(fault),
        Error::Busy => Error::Busy,
        Error::Unauthenticated | Error::Denied | Error::Mismatch | Error::Rejected(_) => {
            Error::corrupt("internal publication cleanup has no new business judgment")
        }
    }
}

fn ended(error: Error<Infallible>, end: Result<(), Fault>) -> Error<Infallible> {
    match error {
        Error::Unavailable(fault) => Error::Unavailable(fault.then_result(end)),
        Error::Unknown(fault) => Error::Unknown(fault.then_result(end)),
        Error::Corrupt(fault) => Error::Corrupt(fault.then_result(end)),
        error => error,
    }
}

fn termination(error: platform::database::Error) -> Fault {
    Fault::new(Stage::Termination, FaultKind::Connection(error))
}

fn invalid_digest(_: Vec<u8>) -> Error<Infallible> {
    Error::corrupt("avatar digest is SHA-256")
}

enum Waiting {
    Closed,
    Connection(platform::database::Error),
}

impl From<work::Error> for Waiting {
    fn from(_: work::Error) -> Self {
        Self::Closed
    }
}

impl From<platform::database::Error> for Waiting {
    fn from(error: platform::database::Error) -> Self {
        Self::Connection(error)
    }
}

impl Waiting {
    fn into_error(self) -> Error<Infallible> {
        match self {
            Self::Closed => work::Error::Closed.into(),
            Self::Connection(error) => Error::connection(error),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use chrono::{TimeDelta, Utc};
    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::Client;

    use super::{Candidate, Settings};
    use crate::{
        access::{
            appointment,
            role::tests::{actor, start},
        },
        cancellation::tests::{confirmation, management},
        history::Read,
        identity::{
            authentication::{self, Database, Supplier, server},
            avatar::{self, link},
            cancellation, current, operation,
        },
        startup::Services,
    };

    struct Case {
        account: current::Account,
        manager: current::Actor,
        original: cancellation::Cancellation,
        candidate: Candidate,
        href: String,
    }

    async fn cancel(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
        managed: bool,
    ) -> Case {
        cancel_with_avatar(database, services, supplier, managed, false).await
    }

    async fn cancel_with_avatar(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
        managed: bool,
        uploaded: bool,
    ) -> Case {
        let manager = Box::pin(actor(database, services, supplier)).await;
        let target = Box::pin(appointment::tests::register(database, services, supplier)).await;
        if uploaded {
            use image::ImageEncoder as _;
            let initial = services
                .current()
                .read(&target)
                .await
                .expect("initial avatar");
            let mut bytes = Vec::new();
            image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(&[17, 31, 47, 255], 1, 1, image::ExtendedColorType::Rgba8)
                .expect("actual uploaded PNG");
            services
                .avatar()
                .upload(
                    services.current(),
                    target.clone(),
                    operation::Id::allocate(),
                    initial.avatar().revision(),
                    "image/png".into(),
                    bytes,
                    services.avatar().upload_slot().expect("upload capacity"),
                    tokio::time::Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect("private avatar before cancellation");
        }
        let account = services
            .current()
            .read(&target)
            .await
            .expect("current account");
        let href = account.avatar().link().href().to_owned();
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("observer");
        let id = observer
            .client()
            .query_one(
                "SELECT id FROM identity.avatars WHERE account_id=$1 AND released_at IS NULL",
                &[&account.id()],
            )
            .await
            .expect("original avatar")
            .get(0);
        observer.terminate().await.expect("observer stopped");
        let operation = operation::Id::allocate();
        let original = if managed {
            services
                .cancellation()
                .cancel_account(
                    manager.clone(),
                    operation,
                    account.id(),
                    management(&account),
                )
                .await
        } else {
            services
                .cancellation()
                .cancel(target, operation, confirmation(&account))
                .await
        }
        .expect("real account cancellation");
        let candidate = Candidate {
            id,
            account: account.id(),
            operation,
            family: crate::kernel::operation::Family::Cancellation,
        };
        Case {
            account,
            manager,
            original,
            candidate,
            href,
        }
    }

    async fn count(client: &Client, id: avatar::Id) -> i64 {
        client
            .query_one("SELECT count(*) FROM identity.avatars WHERE id=$1", &[&id])
            .await
            .expect("exact avatar count")
            .get(0)
    }

    async fn expire(client: &Client, case: &Case) {
        // Wait for the actual stored release deadline, not an assumed operation duration.
        let deadline = case.original.cancelled_at() + TimeDelta::seconds(1);
        client.query_one("SELECT pg_sleep(GREATEST(0, EXTRACT(EPOCH FROM ($1::timestamptz-clock_timestamp()))))", &[&deadline]).await.expect("release grace elapsed");
    }

    fn proof(href: &str) -> link::Proof {
        let url = url::Url::parse(href).expect("signed URL");
        let digest = url
            .path()
            .strip_prefix("/api/v1/avatars/")
            .expect("avatar route")
            .strip_suffix(".webp")
            .expect("WebP")
            .parse()
            .expect("digest");
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        link::Proof::new(
            digest,
            query["key_id"].parse().expect("key"),
            query["expires_at"].parse().expect("expiry"),
            query["signature"].parse().expect("signature"),
        )
    }

    #[test]
    fn grace_rejects_invalid_settings_and_expires_at_equality() {
        assert_eq!(Settings::default().seconds(), 86400);
        assert!(Settings::new(0).is_err());
        assert!(Settings::new(u64::MAX).is_err());
        let settings = Settings::new(1).expect("one-second grace");
        let released =
            chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("fixed instant");
        assert!(!settings.expired(released, released + TimeDelta::milliseconds(999)));
        assert!(settings.expired(released, released + TimeDelta::seconds(1)));
    }

    #[test]
    fn real_cancellations_keep_original_results_and_default_links_after_cleanup_and_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for managed in [false, true] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let case = Box::pin(cancel(&database, &services, &mut supplier, managed)).await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let mut retention = services.avatar_retention();
                    retention.run().await.expect("default grace retains recent release");
                    assert_eq!(count(observer.client(), case.candidate.id).await, 1);
                    retention.settings = Settings::new(1).expect("short test grace");
                    expire(observer.client(), &case).await;
                    retention.run().await.expect("expired metadata reclaimed");
                    assert_eq!(count(observer.client(), case.candidate.id).await, 0);
                    assert_eq!(services.avatar().read(proof(&case.href), avatar::read_capacity()).await.expect("old default link remains readable").bytes(), include_bytes!("../../../../assets/avatars/0-cat.webp"));
                    let original = cancellation::find_by_operation(observer.client(), services.app().keys(), case.candidate.operation, context.sample().await.expect("time")).await.expect("original source").expect("permanent completion");
                    original.verify_removal(observer.client(), services.app(), Read::Locked, context.sample().await.expect("time")).await.expect("original removal does not require reclaimed metadata");
                    assert_eq!(original.id(), case.original.id());
                    if managed {
                        let replay = services.cancellation().cancel_account(case.manager.clone(), case.candidate.operation, case.account.id(), management(&case.account)).await.expect("managed replay after cleanup");
                        assert_eq!(replay.id(), case.original.id());
                    }
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.avatars WHERE account_id=$1 AND released_at IS NULL", &[&case.manager.account_id()]).await.expect("manager's current avatar retained").get::<_,i64>(0), 1);
                    assert_eq!(observer.client().execute("DELETE FROM identity.avatars WHERE account_id=$1", &[&case.manager.account_id()]).await.expect_err("runtime cannot delete current avatar").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("restart accepts legally cleaned metadata");
                    assert_eq!(services.avatar().read(proof(&case.href), avatar::read_capacity()).await.expect("original link after restart").bytes(), include_bytes!("../../../../assets/avatars/0-cat.webp"));
                    services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer stopped"); supplier.stop().await;
                })).await.expect("bounded cancellation and avatar lifecycle");
            }
        }));
    }

    #[test]
    fn file_discovery_dependency_failure_retries_at_runtime_but_fails_startup() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(45), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let owner = database
                    .connections(&database.owner)
                    .recover()
                    .await
                    .expect("fixture owner");
                owner
                    .client()
                    .batch_execute(
                        "BEGIN; LOCK TABLE identity.publications IN ACCESS EXCLUSIVE MODE",
                    )
                    .await
                    .expect("hold file discovery independently of business reads");
                let mut retention = services.avatar_retention();
                retention
                    .run()
                    .await
                    .expect("runtime defers ordinary discovery failure");
                services
                    .current()
                    .read(&actor)
                    .await
                    .expect("unrelated requests remain admitted");
                assert!(
                    matches!(
                        retention.restore().await,
                        Err(crate::kernel::Error::Unavailable(_))
                    ),
                    "startup cannot expose services before recovery discovery succeeds"
                );
                owner
                    .client()
                    .batch_execute("ROLLBACK")
                    .await
                    .expect("restore discovery dependency");
                retention.run().await.expect("next runtime round succeeds");
                retention
                    .restore()
                    .await
                    .expect("startup succeeds after dependency recovery");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                owner.terminate().await.expect("owner ended");
                supplier.stop().await;
            })
            .await
            .expect("bounded runtime and startup discovery failures");
        });
    }

    #[test]
    fn deletion_commit_faults_resolve_the_exact_avatar_under_the_original_lock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for committed in [false, true] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let case = Box::pin(cancel(&database, &services, &mut supplier, false)).await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    expire(observer.client(), &case).await;
                    let mut retention = services.avatar_retention(); retention.settings = Settings::new(1).expect("test grace");
                    proxy.arm(if committed { Fault::DelayedCommit } else { Fault::DelayedRollback });
                    let pending = tokio::spawn(async move { retention.reclaim(&case.candidate).await });
                    let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original COMMIT intercepted");
                    assert_eq!(count(observer.client(), case.candidate.id).await, 1, "undecided deletion is not visible");
                    proxy.release();
                    let result = timeout(Duration::from_secs(15), pending).await.expect("recovery finished").expect("retention task");
                    assert_eq!(result.is_ok(), committed, "{result:?}");
                    assert_eq!(count(observer.client(), case.candidate.id).await, i64::from(!committed));
                    assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&backend]).await.expect("old backend ended").get::<_,bool>(0));
                    services.app().registry().ready().expect("known outcome preserves admission");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    observer.terminate().await.expect("observer ended"); proxy.stop().await; supplier.stop().await;
                })).await.expect("bounded deletion recovery");
            }
        }));
    }

    #[test]
    fn both_cancellations_remove_public_uploads_and_reclaim_private_bytes_without_erasing_history()
    {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for managed in [false, true] {
                timeout(Duration::from_secs(50), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let case = Box::pin(cancel_with_avatar(&database, &services, &mut supplier, managed, true)).await;
                    let observer = database.connections(&database.user).recover().await.expect("runtime-role observer");
                    let row = observer.client().query_one(
                        "SELECT (SELECT count(*) FROM identity.uploads WHERE account_id=$1), (SELECT count(*) FROM identity.portraits WHERE account_id=$1)", &[&case.account.id()],
                    ).await.expect("cancellation public and permanent facts");
                    assert_eq!(row.get::<_, i64>(0), 0);
                    assert_eq!(row.get::<_, i64>(1), 1);
                    assert!(services.avatar().0.storage.open_file(case.candidate.id).expect("retained file").is_some());
                    let image = services.avatar().read(proof(&case.href), avatar::read_capacity()).await.expect("signed private bytes survive cancellation during grace");
                    assert_eq!(image::load_from_memory_with_format(image.bytes(), image::ImageFormat::WebP).expect("retained WebP").to_rgba8().get_pixel(0, 0).0, [17, 31, 47, 255]);
                    let mut retention = services.avatar_retention();
                    retention.run().await.expect("default grace preserves file");
                    assert_eq!(count(observer.client(), case.candidate.id).await, 1);
                    retention.settings = Settings::new(1).expect("test grace");
                    expire(observer.client(), &case).await;
                    retention.run().await.expect("both release sources settle");
                    assert_eq!(count(observer.client(), case.candidate.id).await, 0);
                    assert!(services.avatar().0.storage.open_file(case.candidate.id).expect("physical deletion").is_none());
                    assert!(matches!(services.avatar().read(proof(&case.href), avatar::read_capacity()).await, Err(avatar::Error::Missing)));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.portraits WHERE account_id=$1", &[&case.account.id()]).await.expect("permanent account advancement").get::<_, i64>(0), 1);
                    services.close(); services.drain().await.expect("drain"); context.stop().await.expect("clock stopped");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("restart verifies retained account history");
                    assert!(matches!(services.avatar().read(proof(&case.href), avatar::read_capacity()).await, Err(avatar::Error::Missing)));
                    services.close(); services.drain().await.expect("restart drain"); context.stop().await.expect("restart clock stopped");
                    observer.terminate().await.expect("observer ended"); supplier.stop().await;
                }).await.expect("bounded private avatar cancellation and cleanup");
            }
        });
    }

    #[test]
    fn batches_skip_active_original_work_and_revisit_it_after_the_scan() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let case = Box::pin(cancel(&database, &services, &mut supplier, false)).await;
                let owner = database.connections(&database.owner).recover().await.expect("fixture owner");
                expire(owner.client(), &case).await;
                let mut retention = services.avatar_retention();
                retention.settings = Settings::new(1).expect("test grace");
                let mut permit = services.app().registry().try_acquire_for(crate::runtime::registry::Family::Kernel(crate::kernel::operation::Family::Cancellation), case.candidate.operation).await.expect("original work still running");
                permit.begin();
                retention.run().await.expect("scan skips active original");
                assert!(retention.cursor.is_none());
                let remaining = || async { owner.client().query_one("SELECT count(*) FROM identity.avatars WHERE released_at IS NOT NULL", &[]).await.expect("backlog count").get::<_,i64>(0) };
                assert_eq!(remaining().await, 1);
                permit.finish();
                for index in 100..228 {
                    let target = Box::pin(appointment::tests::register_numbered(&database, &services, &mut supplier, index)).await;
                    let account = services.current().read(&target).await.expect("registered account");
                    services.cancellation().cancel(target, operation::Id::allocate(), confirmation(&account)).await.expect("real distinct cancellation");
                }
                owner.client().query_one("SELECT pg_sleep(GREATEST(0, EXTRACT(EPOCH FROM (max(released_at)+INTERVAL '1 second'-clock_timestamp())))) FROM identity.avatars", &[]).await.expect("all release grace periods elapsed");
                assert_eq!(remaining().await, 129);
                retention.run().await.expect("first bounded page deleted");
                assert_eq!(remaining().await, 1);
                retention.run().await.expect("remaining row deleted");
                assert_eq!(remaining().await, 0);
                assert!(retention.cursor.is_none());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                owner.terminate().await.expect("fixture owner ended"); supplier.stop().await;
            })).await.expect("bounded backlog lifecycle");
        }));
    }

    #[test]
    fn unknown_cleanup_waits_for_late_commit_and_closure_retains_active_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for unknown in [true, false] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let case = Box::pin(cancel(&database, &services, &mut supplier, false)).await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    expire(observer.client(), &case).await;
                    let mut retention = services.avatar_retention(); retention.settings = Settings::new(1).expect("test grace");
                    proxy.arm(if unknown { Fault::UnknownCommit } else { Fault::DelayedCommit });
                    let mut task = tokio::spawn(async move {
                        if unknown { retention.reclaim(&case.candidate).await } else { retention.run().await }
                    });
                    let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("delete COMMIT held");
                    if unknown {
                        let result = timeout(Duration::from_secs(5), task).await.expect("bounded failed recovery").expect("cleanup task");
                        assert!(matches!(result, Err(crate::kernel::Error::Unknown(_))), "{result:?}");
                        assert_eq!(count(observer.client(), case.candidate.id).await, 1);
                        proxy.release();
                        let (deleted, end) = services.avatar_retention().deleted(&case.candidate).await.expect("late commit observed under original lock");
                        assert!(deleted); end.expect("recovery ended");
                        services.close(); services.drain().await.expect("drained");
                    } else {
                        services.close();
                        assert!(timeout(Duration::from_millis(30), &mut task).await.is_err(), "the supervisor retains active cleanup while the old backend may commit");
                        assert_eq!(count(observer.client(), case.candidate.id).await, 1);
                        proxy.release();
                        timeout(Duration::from_secs(5), task).await.expect("cleanup finished").expect("cleanup task").expect("known deletion");
                        services.drain().await.expect("services drained after retention");
                    }
                    assert_eq!(count(observer.client(), case.candidate.id).await, 0);
                    assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&backend]).await.expect("old backend ended").get::<_,bool>(0));
                    context.stop().await.expect("stopped"); observer.terminate().await.expect("observer ended"); proxy.stop().await; supplier.stop().await;
                })).await.expect("bounded unknown outcome and drain");
            }
        }));
    }
    #[test]
    fn damaged_released_metadata_is_retained_and_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(20),
                Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let case = Box::pin(cancel(&database, &services, &mut supplier, false)).await;
                    let owner = database
                        .connections(&database.owner)
                        .recover()
                        .await
                        .expect("fixture owner");
                    expire(owner.client(), &case).await;
                    owner
                        .client()
                        .batch_execute(
                            "ALTER TABLE identity.avatars DISABLE TRIGGER avatars_transition_check",
                        )
                        .await
                        .expect("inject stored corruption");
                    owner
                        .client()
                        .execute(
                            "UPDATE identity.avatars SET digest=$2 WHERE id=$1",
                            &[&case.candidate.id, &vec![0_u8; 32]],
                        )
                        .await
                        .expect("damage frozen digest");
                    owner
                        .client()
                        .batch_execute(
                            "ALTER TABLE identity.avatars ENABLE TRIGGER avatars_transition_check",
                        )
                        .await
                        .expect("restore guard");
                    let mut retention = services.avatar_retention();
                    retention.settings = Settings::new(1).expect("test grace");
                    assert!(
                        retention
                            .run()
                            .await
                            .expect_err("corrupt content rejected")
                            .is_corrupt()
                    );
                    assert!(!services.admitted());
                    assert_eq!(count(owner.client(), case.candidate.id).await, 1);
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    owner.terminate().await.expect("fixture owner ended");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded corrupt cleanup");
        }));
    }
}
