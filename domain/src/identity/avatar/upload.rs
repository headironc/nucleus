//! Self-service avatar replacement: input digest, publication intention, private file effects,
//! original result and the single result transaction.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tokio::{
    sync::OwnedSemaphorePermit,
    time::{Instant, timeout_at},
};
use tokio_postgres::{Client, Row};
use uuid::Uuid;

use crate::{
    history,
    identity::{
        account,
        avatar::{self, Defaults, Id, Revision, Snapshot, codec, storage::Storage},
        current, operation,
    },
    kernel::{
        App, Authorization, Error, Query, Rejection as KernelRejection, Status,
        completion::{self, Template},
        error::{Fault, FaultKind, Stage},
        operation::Family,
        tx::{Connection, Tx},
    },
};

pub(crate) const UPLOADS: &str = "identity.uploads";
pub(crate) const PUBLICATIONS: &str = "identity.publications";

pub(crate) const COMPLETIONS: Template = Template::personal(Family::Avatar, |action, target| {
    action == "replace" && target.parse::<account::Id>().is_ok()
});

const BUSINESS: Duration = Duration::from_secs(10);
const DECODE: Duration = Duration::from_secs(5);
const UPLOAD_SEAL: &str = "identity.avatar.upload";
const PUBLICATION_SEAL: &str = "identity.avatar.publication";

pub(super) async fn verify(
    client: &Client,
    app: &App,
    now: DateTime<Utc>,
) -> Result<(), Error<Rejection>> {
    let mut completions: std::collections::HashMap<_, _> =
        completion::fetch_all(client, app.keys(), COMPLETIONS)
            .await?
            .into_iter()
            .map(|record| (record.operation_id(), record))
            .collect();
    let mut cursor: Option<operation::Id> = None;
    loop {
        let complete = super::verify_batch(async {
        let rows = client.query("SELECT operation_id, account_id, changed, revision, completed_at, seal, seal_key_id, seal_version FROM identity.uploads WHERE $1::uuid IS NULL OR operation_id>$1 ORDER BY operation_id LIMIT 128", &[&cursor])
            .await.map_err(Error::query)?;
        if rows.is_empty() {
            return Ok(true);
        }
        for row in rows {
            let original = Original::decode(&row, app.keys())?;
            let completion = completions.remove(&original.operation)
                    .ok_or_else(|| {
                        Error::corrupt("retained upload has its authenticated original completion")
                    })?;
            completion.check_time(now)?;
            if completion.target() != original.account.to_string()
                || completion.created_at() != original.completed_at
            {
                return Err(Error::corrupt("upload and original completion agree"));
            }
            if original.changed {
                let record = account::record::avatar(client, app.keys(), original.operation, now)
                    .await
                    .map_err(history_fault)?;
                if record.account_id() != original.account
                    || record.happened_at() != original.completed_at
                    || !matches!(record.event(), account::record::Event::Avatar { revision, .. } if *revision == original.revision)
                {
                    return Err(Error::corrupt(
                        "changed upload retains its permanent account advancement",
                    ));
                }
            }
            cursor = Some(original.operation);
        }
        Ok(false)
        }).await?;
        if complete {
            break;
        }
    }
    if !completions.is_empty() {
        return Err(Error::corrupt(
            "every retained avatar completion has its upload result",
        ));
    }
    cursor = None;
    loop {
        let complete = super::verify_batch(async {
        let rows = client.query("SELECT operation_id FROM identity.publications WHERE $1::uuid IS NULL OR operation_id>$1 ORDER BY operation_id LIMIT 128", &[&cursor])
            .await.map_err(Error::query)?;
        if rows.is_empty() {
            return Ok(true);
        }
        for row in rows {
            let operation = row.try_get("operation_id").map_err(Error::decode)?;
            let publication = Publication::find(client, app.keys(), operation)
                .await?
                .ok_or_else(|| Error::corrupt("startup retains each discovered publication"))?;
            if publication.created_at > now
                || publication
                    .completed_at
                    .is_some_and(|at| at < publication.created_at || at > now)
            {
                return Err(Error::corrupt(
                    "publication times are ordered and not future facts",
                ));
            }
            let original = Original::find_optional(client, app.keys(), operation).await?;
            match (publication.changed, publication.completed_at, original) {
                (None, None, None) => {}
                (Some(changed), Some(at), Some(original))
                    if original.account == publication.account
                        && original.changed == changed
                        && original.revision == publication.revision
                        && original.completed_at == at => {}
                (Some(_), Some(_), None) => {
                    let account = history::find::<account::state::State>(
                        client,
                        app.keys(),
                        app.anchors(),
                        history::Read::Shared,
                        publication.account,
                        now,
                    )
                    .await
                    .map_err(history_fault)?
                    .ok_or_else(|| Error::corrupt("publication retains its account"))?;
                    if account.cancellation_id().is_none() {
                        return Err(Error::corrupt(
                            "only cancellation removes a publication's public result",
                        ));
                    }
                }
                _ => {
                    return Err(Error::corrupt(
                        "publication and retained original result agree",
                    ));
                }
            }
            cursor = Some(operation);
        }
        Ok(false)
        }).await?;
        if complete {
            break;
        }
    }
    Ok(())
}

pub(crate) async fn remove_account(
    client: &Client,
    keys: history::Keys<'_>,
    account: account::Id,
    now: DateTime<Utc>,
) -> Result<(), Error<std::convert::Infallible>> {
    let rows = client.query("SELECT operation_id, account_id, changed, revision, completed_at, seal, seal_key_id, seal_version FROM identity.uploads WHERE account_id=$1 ORDER BY operation_id", &[&account])
        .await.map_err(Error::query)?;
    for row in rows {
        let original = Original::decode(&row, keys).map_err(super::retention::cleanup_error)?;
        if original.account != account || original.completed_at > now {
            return Err(Error::corrupt(
                "cancellation removes its authenticated upload results",
            ));
        }
    }
    client
        .execute(
            "DELETE FROM identity.uploads WHERE account_id=$1",
            &[&account],
        )
        .await
        .map_err(Error::query)?;
    Ok(())
}

pub(crate) async fn removed(
    client: &Client,
    account: account::Id,
) -> Result<bool, Error<std::convert::Infallible>> {
    client
        .query_one(
            "SELECT NOT EXISTS (SELECT 1 FROM identity.uploads WHERE account_id=$1)",
            &[&account],
        )
        .await
        .map_err(Error::query)?
        .try_get(0)
        .map_err(Error::decode)
}

#[derive(Clone, Copy)]
pub struct Limits {
    hourly: usize,
}

impl Limits {
    pub fn new(hourly: usize) -> Result<Self, LimitsError> {
        if hourly == 0 || i64::try_from(hourly).is_err() {
            return Err(LimitsError);
        }
        Ok(Self { hourly })
    }
    pub fn hourly(self) -> usize {
        self.hourly
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self { hourly: 10 }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("avatar hourly replacement limit must be positive and fit the database counter")]
pub struct LimitsError;

/// A completed upload result; the caller signs the decisive current snapshot afterwards.
pub(crate) struct Completed {
    pub(crate) completion: Completion,
    pub(crate) snapshot: Snapshot,
}

pub(crate) enum Lookup {
    Available(Completed),
    Pending,
    Absent,
}

pub(crate) async fn lookup(
    app: &App,
    defaults: &Defaults,
    current: &current::Service,
    actor: &current::Actor,
    operation_id: operation::Id,
    deadline: Instant,
) -> Result<Lookup, Error<Rejection>> {
    timeout_at(
        deadline,
        app.query(&OriginalQuery {
            app,
            defaults,
            current,
            actor,
            operation_id,
        }),
    )
    .await
    .unwrap_or_else(Error::elapsed)
}

struct OriginalQuery<'a> {
    app: &'a App,
    defaults: &'a Defaults,
    current: &'a current::Service,
    actor: &'a current::Actor,
    operation_id: operation::Id,
}

impl Query for OriginalQuery<'_> {
    const RETAIN_ON_END_FAILURE: bool = true;
    type Authorized = current::database::State;
    type Output = Lookup;
    type Rejection = Rejection;

    async fn authorize(
        &self,
        client: &Client,
        view: history::View,
    ) -> Result<Self::Authorized, Error<Rejection>> {
        self.current
            .at(view)
            .fetch_state(client, self.actor, history::Read::Snapshot(view))
            .await
            .map_err(identity_fault)
    }

    async fn read(
        &self,
        client: &Client,
        state: Self::Authorized,
        _: history::View,
    ) -> Result<Lookup, Error<Rejection>> {
        let Self {
            app,
            defaults,
            operation_id,
            ..
        } = self;
        // Authorization, original result and current avatar share the same read snapshot.
        // A registry entry alone never proves that this account owns an in-flight upload.
        let row = client.query_opt(
            "SELECT upload.operation_id, upload.account_id, upload.changed, upload.revision, upload.completed_at, upload.seal, upload.seal_key_id, upload.seal_version, avatar.id, avatar.revision AS avatar, avatar.digest, avatar.size, avatar.width, avatar.height, avatar.content_seal, avatar.content_seal_key_id, avatar.content_seal_version FROM identity.uploads upload LEFT JOIN identity.avatars avatar ON avatar.account_id=upload.account_id AND avatar.released_at IS NULL WHERE upload.operation_id=$1 AND upload.account_id=$2",
            &[operation_id, &state.account_id()],
        ).await.map_err(Error::query)?;
        let Some(row) = row else {
            return match Publication::find(client, app.keys(), *operation_id).await? {
                Some(publication) if publication.account == state.account_id() => {
                    if publication.completed_at.is_some() {
                        Err(Error::corrupt(
                            "an active account retains its completed upload result",
                        ))
                    } else {
                        Ok(Lookup::Pending)
                    }
                }
                _ => Ok(Lookup::Absent),
            };
        };
        let original = Original::decode(&row, app.keys())?;
        let record = completion::original(client, app.keys(), COMPLETIONS, *operation_id)
            .await?
            .ok_or_else(|| Error::corrupt("an upload result retains its original completion"))?;
        record.check_time(state.observed_at())?;
        if record.target() != state.account_id().to_string()
            || record.person_id() != state.person_id()
            || record.created_at() != original.completed_at
        {
            return Err(Error::corrupt(
                "upload result and original authorization agree",
            ));
        }
        let snapshot = Snapshot::read(&row, app.keys().seals, defaults).map_err(avatar_fault)?;
        if snapshot.revision() != state.account_state().avatar() {
            return Err(Error::corrupt(
                "current avatar belongs to the authenticated account revision",
            ));
        }
        Ok(Lookup::Available(Completed {
            completion: Completion::original(original),
            snapshot,
        }))
    }
}

pub struct Completion {
    id: operation::Id,
    changed: bool,
    revision: Revision,
    completed_at: DateTime<Utc>,
}

impl Completion {
    pub(crate) fn original(original: Original) -> Self {
        Self {
            id: original.operation,
            changed: original.changed,
            revision: original.revision,
            completed_at: original.completed_at,
        }
    }
    pub fn id(&self) -> operation::Id {
        self.id
    }
    pub fn changed(&self) -> bool {
        self.changed
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn completed_at(&self) -> DateTime<Utc> {
        self.completed_at
    }
}

#[derive(Debug)]
pub enum Rejection {
    Invalid,
    TooLarge,
    Unsupported,
    Changed,
    Limited,
}

impl Rejection {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_input",
            Self::TooLarge => "avatar_too_large",
            Self::Unsupported => "avatar_format_unsupported",
            Self::Changed => "avatar_changed",
            Self::Limited => "rate_limited",
        }
    }
}

impl KernelRejection for Rejection {
    fn status(&self) -> Status {
        match self {
            Self::Invalid | Self::TooLarge | Self::Unsupported => Status::Unprocessable,
            Self::Changed => Status::Conflict,
            Self::Limited => Status::Forbidden,
        }
    }
    fn code(&self) -> &'static str {
        Rejection::code(self)
    }
}

/// Run one accepted upload under the original-operation work identity.
#[expect(
    clippy::too_many_arguments,
    reason = "Move each original input and resource owner into supervised work."
)]
pub(crate) async fn run(
    app: &App,
    current: &current::Service,
    storage: Storage,
    defaults: Defaults,
    limits: Limits,
    actor: current::Actor,
    operation_id: operation::Id,
    expected: Revision,
    declared: String,
    bytes: Vec<u8>,
    permit: OwnedSemaphorePermit,
    total: Instant,
) -> Result<Completion, Error<Rejection>> {
    let family = Family::Avatar;
    let app = app.clone();
    let work_app = app.clone();
    let current = current.clone();
    app.work()
        .run(
            std::sync::Arc::clone(app.registry()),
            crate::runtime::registry::Family::Kernel(family),
            operation_id,
            async move {
                let runtime = tokio::runtime::Handle::current();
                tokio::task::spawn_blocking(move || {
                    runtime.block_on(execute(
                        &work_app,
                        &current,
                        &storage,
                        &defaults,
                        limits,
                        &actor,
                        operation_id,
                        expected,
                        &declared,
                        &bytes,
                        permit,
                        total,
                    ))
                })
                .await
                .map_err(|_| Error::Unavailable(Fault::new(Stage::Command, FaultKind::Worker)))?
            },
        )
        .await
}

#[allow(clippy::too_many_arguments)]
async fn execute(
    app: &App,
    current: &current::Service,
    storage: &Storage,
    defaults: &Defaults,
    limits: Limits,
    actor: &current::Actor,
    operation_id: operation::Id,
    expected: Revision,
    declared: &str,
    bytes: &[u8],
    permit: OwnedSemaphorePermit,
    total: Instant,
) -> Result<Completion, Error<Rejection>> {
    // The supervised operation retains capacity through decoding, file effects and recovery.
    // A blocking decoder also holds a clone until its actual computation ends.
    let permit = std::sync::Arc::new(permit);
    let input_digest: [u8; 32] = Sha256::digest(bytes).into();
    let start = start(
        app,
        current,
        actor,
        operation_id,
        expected,
        declared,
        input_digest,
        bytes.len(),
        total,
    )
    .await?;
    match start {
        Start::Replay(original) => Ok(Completion::original(original)),
        Start::Resume(publication) => {
            let total = bounded(total, BUSINESS);
            let declared =
                codec::Media::parse(declared).ok_or(Error::Rejected(Rejection::Unsupported))?;
            let output = ensure_candidate(
                storage,
                defaults,
                declared,
                bytes,
                &publication,
                std::sync::Arc::clone(&permit),
                total,
            )
            .await?;
            result(
                app,
                current,
                storage,
                defaults,
                limits,
                actor,
                operation_id,
                expected,
                &publication,
                &output,
                total,
            )
            .await
        }
        Start::Fresh => {
            let declared =
                codec::Media::parse(declared).ok_or(Error::Rejected(Rejection::Unsupported))?;
            let output = decode(std::sync::Arc::clone(&permit), declared, bytes, total).await?;
            let (fixed, current_bytes) =
                current_content(app, current, storage, defaults, actor, operation_id, total)
                    .await?;
            let unchanged = output.digest == *fixed.digest().as_bytes();
            if unchanged && current_bytes != output.bytes {
                return Err(Error::corrupt(
                    "an equal avatar digest retains its exact current bytes",
                ));
            }
            let changed = !unchanged;
            let private = changed && !default_output(defaults, &output)?;
            let total = bounded(total, BUSINESS);
            let publication = publish_intent(
                app,
                current,
                actor,
                operation_id,
                expected,
                declared,
                input_digest,
                bytes.len(),
                &output,
                changed,
                private,
                total,
            )
            .await?;
            if private {
                storage
                    .publish(publication.candidate, &output.bytes)
                    .map_err(file_fault)?;
            }
            result(
                app,
                current,
                storage,
                defaults,
                limits,
                actor,
                operation_id,
                expected,
                &publication,
                &output.bytes,
                total,
            )
            .await
        }
    }
}

enum Start {
    Replay(Original),
    Resume(Publication),
    Fresh,
}

#[allow(clippy::too_many_arguments)]
async fn start(
    app: &App,
    current: &current::Service,
    actor: &current::Actor,
    operation_id: operation::Id,
    expected: Revision,
    declared: &str,
    input_digest: [u8; 32],
    input_size: usize,
    total: Instant,
) -> Result<Start, Error<Rejection>> {
    let lease = app
        .context()
        .connections()
        .lease()
        .await
        .map_err(Error::connection)?;
    let (tx, begun) = Tx::begin(
        app.context(),
        Connection::Online(lease),
        bounded(total, BUSINESS),
        Family::Avatar,
        operation_id,
    )
    .await;
    if let Err(fault) = begun {
        return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
    }
    let outcome = timeout_at(tx.deadline(), async {
        let state = lock(app, current, tx.client(), actor).await?;
        let account = state.account_state();
        let request = request(
            account.id(),
            state.person_id(),
            state.revision_id(),
            expected,
            declared,
            &input_digest,
            input_size,
            state.observed_at(),
        )?;
        match completion::find(tx.client(), app.keys(), COMPLETIONS, operation_id, &request)
            .await
            .map_err(|error| error.at(Stage::Completion))?
        {
            Some(record) => {
                record
                    .check_time(state.observed_at())
                    .map_err(|error| error.at(Stage::Completion))?;
                let original = Original::find(tx.client(), app.keys(), operation_id).await?;
                Ok(Start::Replay(original))
            }
            None => match Publication::find(tx.client(), app.keys(), operation_id).await? {
                Some(publication) => {
                    if !publication.matches(
                        account.id(),
                        expected,
                        declared,
                        &input_digest,
                        input_size,
                    ) {
                        Err(Error::Mismatch)
                    } else if publication.completed_at.is_some() {
                        Err(Error::corrupt(
                            "a completed publication retains its public original result",
                        ))
                    } else {
                        Ok(Start::Resume(publication))
                    }
                }
                None => Ok(Start::Fresh),
            },
        }
    })
    .await
    .unwrap_or_else(Error::elapsed);
    match outcome {
        Ok(start) => {
            let (known, cleanup) = tx.confirm(start).await;
            report_end(known.end, cleanup);
            Ok(known.value)
        }
        Err(error) => Err(ended(error, tx.discard().await)),
    }
}

async fn lock(
    app: &App,
    current: &current::Service,
    client: &Client,
    actor: &current::Actor,
) -> Result<current::database::State, Error<Rejection>> {
    account::state::lock(client, actor.account_id())
        .await
        .map_err(history_fault)?;
    account::state::lock_person(client, actor.account_id())
        .await
        .map_err(history_fault)?;
    actor.lock_session(client).await.map_err(identity_fault)?;
    let state = app.context().now(client).await.map_err(Error::clock)?;
    current
        .resolve(client, actor, state)
        .await
        .map_err(identity_fault)
}

struct Output {
    bytes: Vec<u8>,
    digest: [u8; 32],
    width: u16,
    height: u16,
}

impl Output {
    fn size(&self) -> i64 {
        i64::try_from(self.bytes.len()).unwrap_or(i64::MAX)
    }
}

fn default_output(defaults: &Defaults, output: &Output) -> Result<bool, Error<Rejection>> {
    let Some(content) = defaults.find(&output.digest) else {
        return Ok(false);
    };
    if output.width != 512 || output.height != 512 || content.bytes() != output.bytes {
        return Err(Error::corrupt(
            "an output with a default digest has the exact frozen bytes and dimensions",
        ));
    }
    Ok(true)
}

async fn decode(
    permit: std::sync::Arc<OwnedSemaphorePermit>,
    declared: codec::Media,
    bytes: &[u8],
    total: Instant,
) -> Result<Output, Error<Rejection>> {
    let input = bytes.to_vec();
    // Keep the upload permit inside the blocking closure: cancellation of the HTTP wait
    // cannot release it before the actual computation exits.
    let mut task = tokio::task::spawn_blocking(move || {
        let _hold = permit;
        codec::convert(declared, &input)
    });
    let deadline = bounded(total, DECODE);
    let joined = match timeout_at(deadline, &mut task).await {
        Ok(joined) => joined,
        Err(elapsed) => {
            let _ = task.await;
            return Err(Error::Unavailable(Fault::new(
                Stage::Command,
                FaultKind::Timeout(elapsed),
            )));
        }
    };
    let output = joined
        .map_err(|_| Error::Unavailable(Fault::new(Stage::Command, FaultKind::Worker)))?
        .map_err(|error| match error {
            codec::Error::TooLarge => Error::Rejected(Rejection::TooLarge),
            codec::Error::Unsupported => Error::Rejected(Rejection::Unsupported),
            codec::Error::Invalid => Error::Rejected(Rejection::Invalid),
            codec::Error::Encode(error) => Error::Unavailable(Fault::new(
                Stage::Command,
                FaultKind::Avatar(super::Error::Encode(error)),
            )),
        })?;
    let digest: [u8; 32] = Sha256::digest(&output.bytes).into();
    Ok(Output {
        bytes: output.bytes,
        digest,
        width: output.width,
        height: output.height,
    })
}

async fn current_content(
    app: &App,
    current: &current::Service,
    storage: &Storage,
    defaults: &Defaults,
    actor: &current::Actor,
    operation_id: operation::Id,
    total: Instant,
) -> Result<(Snapshot, Vec<u8>), Error<Rejection>> {
    let lease = app
        .context()
        .connections()
        .lease()
        .await
        .map_err(Error::connection)?;
    let (tx, begun) = Tx::begin(
        app.context(),
        Connection::Online(lease),
        bounded(total, BUSINESS),
        Family::Avatar,
        operation_id,
    )
    .await;
    if let Err(fault) = begun {
        return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
    }
    let selected = timeout_at(tx.deadline(), async {
        let state = lock(app, current, tx.client(), actor).await?;
        let fixed = snapshot(tx.client(), app.keys().seals, defaults, state.account_id()).await?;
        if fixed.revision() != state.account_state().avatar() {
            return Err(Error::corrupt(
                "the current avatar matches its qualified account revision",
            ));
        }
        // Acquire the inode while cancellation/replacement cannot release this current row.
        // Bytes are read after the transaction; a later unlink cannot invalidate the reader.
        let reader = if defaults.contains(fixed.digest().as_bytes(), fixed.size()) {
            None
        } else {
            Some(
                storage
                    .open_file(fixed.id())
                    .map_err(file_fault)?
                    .ok_or_else(|| file_fault(avatar::storage::Error::Identity))?,
            )
        };
        Ok((fixed, reader))
    })
    .await
    .unwrap_or_else(Error::elapsed);
    let (fixed, reader) = match selected {
        Ok(value) => {
            let (known, cleanup) = tx.confirm(value).await;
            report_end(known.end, cleanup);
            known.value
        }
        Err(error) => return Err(ended(error, tx.discard().await)),
    };
    let bytes = current_bytes(defaults, &fixed, reader)?;
    Ok((fixed, bytes))
}

async fn snapshot(
    client: &Client,
    seals: &crate::keys::seal::Keys,
    defaults: &Defaults,
    account_id: account::Id,
) -> Result<Snapshot, Error<Rejection>> {
    client
        .query_opt(
            "SELECT avatar.id, avatar.account_id, avatar.revision AS avatar, avatar.digest, avatar.size, avatar.width, avatar.height, avatar.content_seal, avatar.content_seal_key_id, avatar.content_seal_version FROM identity.avatars avatar WHERE avatar.account_id=$1 AND avatar.released_at IS NULL",
            &[&account_id],
        )
        .await
        .map_err(Error::query)?
        .as_ref()
        .map(|row| Snapshot::read(row, seals, defaults))
        .transpose()
        .map_err(avatar_fault)?
        .ok_or_else(|| Error::corrupt("a current account retains one current avatar"))
}

fn current_bytes(
    defaults: &Defaults,
    snapshot: &Snapshot,
    reader: Option<platform::files::Reader>,
) -> Result<Vec<u8>, Error<Rejection>> {
    if defaults.contains(snapshot.digest().as_bytes(), snapshot.size()) {
        return defaults
            .find(snapshot.digest().as_bytes())
            .map(|content| content.bytes().to_vec())
            .ok_or_else(|| Error::corrupt("default avatar content is frozen"));
    }
    let bytes = reader
        .ok_or_else(|| Error::corrupt("private current content retains its opened inode"))?
        .read(
            u64::try_from(snapshot.size())
                .map_err(|_| Error::corrupt("avatar size is positive"))?,
        )
        .map_err(file_fault)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    if digest != *snapshot.digest().as_bytes() {
        return Err(Error::corrupt(
            "the current avatar file matches its stored digest",
        ));
    }
    Ok(bytes)
}

#[allow(clippy::too_many_arguments)]
async fn publish_intent(
    app: &App,
    current: &current::Service,
    actor: &current::Actor,
    operation_id: operation::Id,
    expected: Revision,
    declared: codec::Media,
    input_digest: [u8; 32],
    input_size: usize,
    output: &Output,
    changed: bool,
    private: bool,
    total: Instant,
) -> Result<Publication, Error<Rejection>> {
    let lease = app
        .context()
        .connections()
        .lease()
        .await
        .map_err(Error::connection)?;
    let (tx, begun) = Tx::begin(
        app.context(),
        Connection::Online(lease),
        bounded(total, BUSINESS),
        Family::Avatar,
        operation_id,
    )
    .await;
    if let Err(fault) = begun {
        return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
    }
    let created = timeout_at(tx.deadline(), async {
        let state = lock(app, current, tx.client(), actor).await?;
        if state.account_state().avatar() != expected {
            return Err(Error::Rejected(Rejection::Changed));
        }
        if state.account_state().current().is_none() {
            return Err(Error::Unauthenticated);
        }
        if Publication::find(tx.client(), app.keys(), operation_id)
            .await?
            .is_some()
        {
            return Err(Error::corrupt(
                "one original upload operation retains one publication intention",
            ));
        }
        let publication = Publication {
            operation: operation_id,
            account: state.account_id(),
            expected,
            media: declared,
            input_digest,
            input_size: i64::try_from(input_size)
                .map_err(|_| Error::corrupt("upload input size is representable"))?,
            candidate: Id::allocate(),
            work: Uuid::new_v4(),
            output_digest: output.digest,
            output_size: output.size(),
            width: output.width,
            height: output.height,
            private,
            revision: if changed {
                Revision::allocate()
            } else {
                expected
            },
            changed: None,
            completed_at: None,
            created_at: state.observed_at(),
        };
        publication.insert(tx.client(), app.keys()).await?;
        Ok(publication)
    })
    .await
    .unwrap_or_else(Error::elapsed);
    match created {
        Ok(publication) => match tx.commit().await {
            Ok(()) => Ok(publication),
            Err(fault) => Err(Error::Unknown(fault)),
        },
        Err(error) => Err(ended(error, tx.discard().await)),
    }
}

async fn ensure_candidate(
    storage: &Storage,
    defaults: &Defaults,
    declared: codec::Media,
    input: &[u8],
    publication: &Publication,
    permit: std::sync::Arc<OwnedSemaphorePermit>,
    total: Instant,
) -> Result<Vec<u8>, Error<Rejection>> {
    let size = u64::try_from(publication.output_size)
        .map_err(|_| Error::corrupt("publication output size is positive"))?;
    if publication.private
        && let Some(reader) = storage
            .open_file(publication.candidate)
            .map_err(file_fault)?
    {
        let bytes = reader.read(size).map_err(file_fault)?;
        verify_output(publication, &bytes)?;
        storage
            .publish(publication.candidate, &bytes)
            .map_err(file_fault)?;
        return Ok(bytes);
    }
    // The previous accepted work ended before this operation lock was acquired. Reusing the
    // same original input is not a second upload; the encoder must still produce the recorded
    // output, otherwise the retained intention is damaged.
    let output = decode(permit, declared, input, total).await?;
    verify_output(publication, &output.bytes)?;
    let private =
        publication.revision != publication.expected && !default_output(defaults, &output)?;
    if publication.private != private {
        return Err(Error::corrupt(
            "publication storage choice matches its exact canonical output",
        ));
    }
    if private {
        storage
            .publish(publication.candidate, &output.bytes)
            .map_err(file_fault)?;
    }
    Ok(output.bytes)
}

fn verify_output(publication: &Publication, bytes: &[u8]) -> Result<(), Error<Rejection>> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    if digest != publication.output_digest
        || i64::try_from(bytes.len()).ok() != Some(publication.output_size)
    {
        return Err(Error::corrupt(
            "a published candidate matches its intention",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn result(
    app: &App,
    current: &current::Service,
    storage: &Storage,
    defaults: &Defaults,
    limits: Limits,
    actor: &current::Actor,
    operation_id: operation::Id,
    expected: Revision,
    publication: &Publication,
    output: &[u8],
    total: Instant,
) -> Result<Completion, Error<Rejection>> {
    let mut outcome;
    let mut attempt = 0;
    loop {
        let (fixed, fixed_bytes) =
            current_content(app, current, storage, defaults, actor, operation_id, total).await?;
        outcome = result_once(
            app,
            defaults,
            current,
            limits,
            actor,
            operation_id,
            expected,
            publication,
            output,
            &fixed,
            &fixed_bytes,
            bounded(total, BUSINESS),
        )
        .await;
        attempt += 1;
        if outcome.as_ref().is_err_and(Error::changed_locks) && attempt < 2 {
            continue;
        }
        break;
    }
    // Retention acquires the original work permit before settling this intention. In
    // particular cancellation may remove the public result before this worker returns.
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn result_once(
    app: &App,
    defaults: &Defaults,
    current: &current::Service,
    limits: Limits,
    actor: &current::Actor,
    operation_id: operation::Id,
    expected: Revision,
    publication: &Publication,
    output: &[u8],
    fixed: &Snapshot,
    fixed_bytes: &[u8],
    deadline: Instant,
) -> Result<Completion, Error<Rejection>> {
    let lease = app
        .context()
        .connections()
        .lease()
        .await
        .map_err(Error::connection)?;
    let (tx, begun) = Tx::begin(
        app.context(),
        Connection::Online(lease),
        deadline,
        Family::Avatar,
        operation_id,
    )
    .await;
    if let Err(fault) = begun {
        return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
    }
    let mut pending = None;
    let outcome = timeout_at(tx.deadline(), async {
        let state = lock(app, current, tx.client(), actor).await?;
        let account = state.account_state();
        if account.current().is_none() {
            return Err(Error::Unauthenticated);
        }
        if let Some(original) = Original::find_optional(tx.client(), app.keys(), operation_id).await?
        {
            return Ok(Executed::Replay(original));
        }
        if account.avatar() != expected {
            return Err(Error::Rejected(Rejection::Changed));
        }
        let locked = snapshot_locked(tx.client(), app.keys().seals, defaults, account.id()).await?;
        if locked.id() != fixed.id() {
            return Err(Error::lock_set_changed());
        }
        if locked.revision() != expected || locked.digest().as_bytes() != fixed.digest().as_bytes() {
            return Err(Error::corrupt(
                "the fixed current avatar retains its revision and content",
            ));
        }
        let publication = publication::find_locked(tx.client(), app.keys(), operation_id)
            .await?
            .ok_or_else(|| Error::corrupt("an accepted upload retains its publication intention"))?;
        if publication.expected != expected
            || publication.account != account.id()
            || publication.changed.is_some()
        {
            return Err(Error::corrupt(
                "publication and current avatar facts agree",
            ));
        }
        let unchanged = publication.revision == publication.expected;
        if unchanged {
            if locked.digest().as_bytes() != &publication.output_digest
                || fixed_bytes != output
            {
                return Err(Error::corrupt(
                    "an unchanged result retains the current avatar bytes",
                ));
            }
        } else if locked.digest().as_bytes() == &publication.output_digest {
            return Err(Error::corrupt(
                "an equal current digest is the unchanged result, not a replacement",
            ));
        }
        let now = state.observed_at();
        let completion = if unchanged {
            Original::insert(
                tx.client(),
                app.keys(),
                operation_id,
                account.id(),
                false,
                expected,
                now,
            )
            .await?;
            publication::complete(tx.client(), operation_id).await?;
            Completion {
                id: operation_id,
                changed: false,
                revision: expected,
                completed_at: now,
            }
        } else {
            if quota(tx.client(), account.id(), now, limits).await? {
                return Err(Error::Rejected(Rejection::Limited));
            }
            let portrait = account::portrait::Id::allocate();
            let change = account::transition::avatar(
                account,
                portrait,
                expected,
                publication.revision,
                operation_id,
                now,
            )
            .map_err(history_fault)?;
            history::retain(
                &mut pending,
                change
                    .write(tx.client(), app.keys(), app.anchors())
                    .await
                    .map_err(history_fault)?,
            )
            .map_err(history_fault)?;
            tx.client()
                .execute(
                    "UPDATE identity.avatars SET released_at=$2, release='replacement', release_operation_id=$3, release_transaction_id=pg_catalog.pg_current_xact_id() WHERE id=$1",
                    &[&locked.id(), &now, &operation_id],
                )
                .await
                .map_err(Error::query)?;
            let content_seal = publication.private.then(|| super::database::seal_content(
                app.keys().seals, publication.candidate, account.id(), publication.revision,
                &publication.output_digest, publication.output_size, publication.width, publication.height,
            )).transpose().map_err(avatar_fault)?;
            tx.client()
                .execute(
                    "INSERT INTO identity.avatars (id, account_id, revision, digest, size, width, height, content_seal, content_seal_key_id, content_seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                    &[&publication.candidate, &account.id(), &publication.revision,
                      &&publication.output_digest[..], &publication.output_size,
                      &i32::from(publication.width), &i32::from(publication.height),
                      &content_seal.as_ref().map(crate::keys::seal::Seal::tag),
                      &content_seal.as_ref().map(crate::keys::seal::Seal::key_id),
                      &content_seal.as_ref().map(crate::keys::seal::Seal::version)],
                )
                .await
                .map_err(Error::query)?;
            Original::insert(
                tx.client(),
                app.keys(),
                operation_id,
                account.id(),
                true,
                publication.revision,
                now,
            )
            .await?;
            publication::complete(tx.client(), operation_id).await?;
            Completion {
                id: operation_id,
                changed: true,
                revision: publication.revision,
                completed_at: now,
            }
        };
        completion::insert(
            tx.client(),
            app.keys(),
            COMPLETIONS,
            operation_id,
            &request(
                account.id(),
                state.person_id(),
                state.revision_id(),
                expected,
                publication.media.as_str(),
                &publication.input_digest,
                usize::try_from(publication.input_size)
                    .map_err(|_| Error::corrupt("upload input size is representable"))?,
                now,
            )?,
            now,
            None,
        )
        .await
        .map_err(|error| error.at(Stage::Completion))?;
        Ok(Executed::New(completion))
    })
    .await
    .unwrap_or_else(Error::elapsed);
    match outcome {
        Ok(Executed::Replay(original)) => {
            let (known, cleanup) = tx.confirm(Completion::original(original)).await;
            report_end(known.end, cleanup);
            Ok(known.value)
        }
        Ok(Executed::New(completion)) => {
            let (end, cleanup) = tx
                .complete(crate::kernel::tx::Outcome::Pending(completion))
                .await;
            match end {
                crate::kernel::tx::Completion::Known(known) => {
                    report_end(known.end, cleanup);
                    if let Some(pending) = pending.take() {
                        pending.committed().map_err(history_fault)?;
                    }
                    Ok(known.value)
                }
                crate::kernel::tx::Completion::Pending { error, .. } => {
                    let fault = end_fault(error).then_result(cleanup.map_err(|error| {
                        Fault::new(Stage::Termination, FaultKind::Connection(error))
                    }));
                    match recover_result(app, publication).await {
                        Ok(Some(completion)) => {
                            if let Some(pending) = pending.take() {
                                pending.committed().map_err(history_fault)?;
                            }
                            tracing::error!(stage = "avatar_commit_recovered", error = ?fault);
                            Ok(completion)
                        }
                        Ok(None) => {
                            if let Some(pending) = pending.take() {
                                pending.rolled_back().map_err(history_fault)?;
                            }
                            Err(Error::Unavailable(fault))
                        }
                        Err(recovery) => {
                            if let Some(pending) = pending.take() {
                                pending.uncertain();
                            }
                            tracing::error!(stage = "avatar_result_recovery", error = ?recovery);
                            if recovery.is_corrupt() {
                                return Err(recovery);
                            }
                            Err(Error::Unknown(fault))
                        }
                    }
                }
            }
        }
        Err(error) => {
            let end = tx.discard().await;
            if let Some(pending) = pending {
                pending.rolled_back().map_err(history_fault)?;
            }
            Err(ended(error, end))
        }
    }
}

enum Executed {
    Replay(Original),
    New(Completion),
}

async fn recover_result(
    app: &App,
    attempted: &Publication,
) -> Result<Option<Completion>, Error<Rejection>> {
    // Tx::complete has already awaited the old driver's end. This independent session
    // establishes the original result under the exact original-operation lock.
    let session = app
        .context()
        .connections()
        .recover()
        .await
        .map_err(Error::connection)?;
    let (tx, begun) = Tx::begin(
        app.context(),
        Connection::Recovery(session),
        Instant::now() + BUSINESS,
        Family::Avatar,
        attempted.operation,
    )
    .await;
    if let Err(fault) = begun {
        return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
    }
    let result = timeout_at(tx.deadline(), async {
        account::state::lock(tx.client(), attempted.account)
            .await
            .map_err(history_fault)?;
        let retained = Publication::find(tx.client(), app.keys(), attempted.operation)
            .await?
            .ok_or_else(|| {
                Error::corrupt("active original work retains its publication judgment")
            })?;
        if retained.fields() != attempted.fields() {
            return Err(Error::corrupt(
                "recovery retains the exact original publication identity",
            ));
        }
        let original =
            Original::find_optional(tx.client(), app.keys(), attempted.operation).await?;
        match (retained.changed, retained.completed_at) {
            (Some(changed), Some(completed_at)) => {
                if let Some(original) = original
                    && (original.account != retained.account
                        || original.changed != changed
                        || original.revision != retained.revision
                        || original.completed_at != completed_at)
                {
                    return Err(Error::corrupt(
                        "public result and retained publication judgment agree",
                    ));
                }
                Ok(Some(Completion::original(Original {
                    operation: retained.operation,
                    account: retained.account,
                    changed,
                    revision: retained.revision,
                    completed_at,
                })))
            }
            (None, None) if original.is_none() => Ok(None),
            _ => Err(Error::corrupt(
                "publication and original result commit together",
            )),
        }
    })
    .await
    .unwrap_or_else(Error::elapsed);
    match result {
        Ok(result) => {
            let (known, cleanup) = tx.confirm(result).await;
            report_end(known.end, cleanup);
            Ok(known.value)
        }
        Err(error) => Err(ended(error, tx.discard().await)),
    }
}

fn end_fault(error: crate::kernel::tx::EndError) -> Fault {
    use crate::kernel::tx::EndError;
    match error {
        EndError::Commit(error) => Fault::new(Stage::Commit, FaultKind::Query(error)),
        EndError::CommitTimeout(error) => Fault::new(Stage::Commit, FaultKind::Timeout(error)),
        EndError::Rollback(error) => Fault::new(Stage::Rollback, FaultKind::Query(error)),
        EndError::RollbackTimeout(error) => Fault::new(Stage::Rollback, FaultKind::Timeout(error)),
    }
}

async fn snapshot_locked(
    client: &Client,
    seals: &crate::keys::seal::Keys,
    defaults: &Defaults,
    account_id: account::Id,
) -> Result<Snapshot, Error<Rejection>> {
    client
        .query_opt(
            "SELECT avatar.id, avatar.account_id, avatar.revision AS avatar, avatar.digest, avatar.size, avatar.width, avatar.height, avatar.content_seal, avatar.content_seal_key_id, avatar.content_seal_version FROM identity.avatars avatar WHERE avatar.account_id=$1 AND avatar.released_at IS NULL FOR UPDATE",
            &[&account_id],
        )
        .await
        .map_err(Error::query)?
        .as_ref()
        .map(|row| Snapshot::read(row, seals, defaults))
        .transpose()
        .map_err(avatar_fault)?
        .ok_or_else(|| Error::corrupt("a current account retains one current avatar"))
}

async fn quota(
    client: &Client,
    account: account::Id,
    now: DateTime<Utc>,
    limits: Limits,
) -> Result<bool, Error<Rejection>> {
    let since = now
        .checked_sub_signed(TimeDelta::hours(1))
        .ok_or_else(|| Error::corrupt("avatar quota window is representable"))?;
    let count: i64 = client
        .query_one(
            "SELECT count(*) FROM identity.uploads WHERE account_id=$1 AND changed AND completed_at > $2",
            &[&account, &since],
        )
        .await
        .map_err(Error::query)?
        .try_get(0)
        .map_err(Error::decode)?;
    Ok(count >= i64::try_from(limits.hourly).unwrap_or(i64::MAX))
}

/// Called only while retention owns the idle original operation's work permit.
pub(super) async fn settle(
    app: &App,
    storage: &Storage,
    operation: operation::Id,
) -> Result<(), Error<Rejection>> {
    // Prove the original work ended and no completion adopted this candidate.
    let lease = app
        .context()
        .connections()
        .lease()
        .await
        .map_err(Error::connection)?;
    let (tx, begun) = Tx::begin(
        app.context(),
        Connection::Online(lease),
        Instant::now() + BUSINESS,
        Family::Avatar,
        operation,
    )
    .await;
    if let Err(fault) = begun {
        return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
    }
    let found = timeout_at(tx.deadline(), async {
        let Some(publication) = Publication::find(tx.client(), app.keys(), operation).await? else {
            return Ok(None);
        };
        if publication.completed_at.is_none() {
            if Original::find_optional(tx.client(), app.keys(), operation)
                .await?
                .is_some()
            {
                return Err(Error::corrupt(
                    "an unadopted publication has no original success",
                ));
            }
            let adopted: bool = tx
                .client()
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM identity.avatars WHERE id=$1)",
                    &[&publication.candidate],
                )
                .await
                .map_err(Error::query)?
                .try_get(0)
                .map_err(Error::decode)?;
            if adopted {
                return Err(Error::corrupt(
                    "an unadopted candidate has no avatar resource",
                ));
            }
        }
        Ok(Some(publication))
    })
    .await
    .unwrap_or_else(Error::elapsed);
    let publication = match found {
        Ok(publication) => {
            let (known, cleanup) = tx.confirm(publication).await;
            report_end(known.end, cleanup);
            known.value
        }
        Err(error) => return Err(ended(error, tx.discard().await)),
    };
    let Some(publication) = publication else {
        return Ok(());
    };
    if publication.private && publication.completed_at.is_none() {
        let storage = storage.clone();
        let candidate = publication.candidate;
        tokio::task::spawn_blocking(move || storage.remove(candidate))
            .await
            .map_err(|_| Error::Unavailable(Fault::new(Stage::Command, FaultKind::Worker)))?
            .map_err(file_fault)?;
    }
    // Keep the intent until all file effects have completed; restart can repeat this cleanup.
    publication::remove(app, &publication).await
}

mod publication {
    use super::*;

    pub(super) async fn remove(
        app: &App,
        publication: &Publication,
    ) -> Result<(), Error<Rejection>> {
        let lease = app
            .context()
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            app.context(),
            Connection::Online(lease),
            Instant::now() + BUSINESS,
            Family::Avatar,
            publication.operation,
        )
        .await;
        if let Err(fault) = begun {
            return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
        }
        let result = timeout_at(tx.deadline(), async {
            let Some(retained) =
                Publication::find(tx.client(), app.keys(), publication.operation).await?
            else {
                return Ok(());
            };
            if retained.fields() != publication.fields()
                || retained.changed != publication.changed
                || retained.completed_at != publication.completed_at
            {
                return Err(Error::corrupt(
                    "publication cleanup retains the settled original judgment",
                ));
            }
            tx.client()
                .execute(
                    "DELETE FROM identity.publications WHERE operation_id=$1",
                    &[&publication.operation],
                )
                .await
                .map_err(Error::query)?;
            Ok(())
        })
        .await
        .unwrap_or_else(Error::elapsed);
        match result {
            Ok(()) => tx.commit().await.map_err(Error::Unknown),
            Err(error) => Err(ended(error, tx.discard().await)),
        }
    }

    pub(super) async fn find_locked(
        client: &Client,
        keys: history::Keys<'_>,
        operation: operation::Id,
    ) -> Result<Option<Publication>, Error<Rejection>> {
        Publication::find_with(client, keys, operation, true).await
    }

    pub(super) async fn complete(
        client: &Client,
        operation: operation::Id,
    ) -> Result<(), Error<Rejection>> {
        let updated = client
            .execute(
                "UPDATE identity.publications publication SET changed=result.changed, completed_at=result.completed_at, result_seal=result.seal, result_seal_key_id=result.seal_key_id, result_seal_version=result.seal_version FROM identity.uploads result WHERE publication.operation_id=$1 AND result.operation_id=publication.operation_id AND publication.completed_at IS NULL",
                &[&operation],
            )
            .await
            .map_err(Error::query)?;
        if updated != 1 {
            return Err(Error::corrupt("an accepted publication is completed once"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct Publication {
    pub(crate) operation: operation::Id,
    pub(crate) account: account::Id,
    pub(crate) expected: Revision,
    media: codec::Media,
    input_digest: [u8; 32],
    input_size: i64,
    pub(crate) candidate: Id,
    work: Uuid,
    output_digest: [u8; 32],
    output_size: i64,
    width: u16,
    height: u16,
    private: bool,
    pub(crate) revision: Revision,
    changed: Option<bool>,
    completed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl Publication {
    pub(crate) async fn find(
        client: &Client,
        keys: history::Keys<'_>,
        operation: operation::Id,
    ) -> Result<Option<Self>, Error<Rejection>> {
        Self::find_with(client, keys, operation, false).await
    }

    async fn find_with(
        client: &Client,
        keys: history::Keys<'_>,
        operation: operation::Id,
        lock: bool,
    ) -> Result<Option<Self>, Error<Rejection>> {
        let row = client
            .query_opt(
                &format!(
                    "SELECT operation_id, account_id, expected_revision, media_type, input_digest, input_size, candidate_id, work_id, output_digest, output_size, width, height, private, revision, changed, completed_at, created_at, seal, seal_key_id, seal_version, result_seal, result_seal_key_id, result_seal_version FROM identity.publications WHERE operation_id=$1{}",
                    if lock { " FOR UPDATE" } else { "" }
                ),
                &[&operation],
            )
            .await
            .map_err(Error::query)?;
        row.as_ref().map(|row| Self::decode(row, keys)).transpose()
    }

    fn decode(row: &Row, keys: history::Keys<'_>) -> Result<Self, Error<Rejection>> {
        let media: String = row.try_get("media_type").map_err(Error::decode)?;
        let media = codec::Media::parse(&media)
            .ok_or_else(|| Error::corrupt("publication retains a supported media type"))?;
        let changed: Option<bool> = row.try_get("changed").map_err(Error::decode)?;
        let completed_at: Option<DateTime<Utc>> =
            row.try_get("completed_at").map_err(Error::decode)?;
        if changed.is_some() != completed_at.is_some() {
            return Err(Error::corrupt("publication completion and adoption agree"));
        }
        let publication = Self {
            operation: row.try_get("operation_id").map_err(Error::decode)?,
            account: row.try_get("account_id").map_err(Error::decode)?,
            expected: row.try_get("expected_revision").map_err(Error::decode)?,
            media,
            input_digest: bytes32(row, "input_digest")?,
            input_size: row.try_get("input_size").map_err(Error::decode)?,
            candidate: row.try_get("candidate_id").map_err(Error::decode)?,
            work: row.try_get("work_id").map_err(Error::decode)?,
            output_digest: bytes32(row, "output_digest")?,
            output_size: row.try_get("output_size").map_err(Error::decode)?,
            width: u16::try_from(row.try_get::<_, i32>("width").map_err(Error::decode)?)
                .map_err(|_| Error::corrupt("publication width is a u16"))?,
            height: u16::try_from(row.try_get::<_, i32>("height").map_err(Error::decode)?)
                .map_err(|_| Error::corrupt("publication height is a u16"))?,
            private: row.try_get("private").map_err(Error::decode)?,
            revision: row.try_get("revision").map_err(Error::decode)?,
            changed,
            completed_at,
            created_at: row.try_get("created_at").map_err(Error::decode)?,
        };
        let seal = crate::keys::seal::Seal::stored(
            row.try_get("seal").map_err(Error::decode)?,
            row.try_get("seal_key_id").map_err(Error::decode)?,
            row.try_get("seal_version").map_err(Error::decode)?,
        )
        .map_err(|_| Error::corrupt("publication seal shape"))?;
        let input = publication.fields();
        keys.seals
            .check(
                PUBLICATION_SEAL,
                1,
                &input.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                &seal,
            )
            .map_err(|_| Error::corrupt("publication identity retains its seal"))?;
        if let (Some(changed), Some(completed_at)) = (changed, completed_at) {
            if changed != (publication.revision != publication.expected) {
                return Err(Error::corrupt(
                    "publication judgment agrees with its revision",
                ));
            }
            let seal = crate::keys::seal::Seal::stored(
                row.try_get("result_seal").map_err(Error::decode)?,
                row.try_get("result_seal_key_id").map_err(Error::decode)?,
                row.try_get("result_seal_version").map_err(Error::decode)?,
            )
            .map_err(|_| Error::corrupt("publication result seal shape"))?;
            let input = fields(
                publication.operation,
                publication.account,
                changed,
                publication.revision,
                completed_at,
            );
            keys.seals
                .check(
                    UPLOAD_SEAL,
                    1,
                    &input.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                    &seal,
                )
                .map_err(|_| Error::corrupt("publication result retains its seal"))?;
        }
        Ok(publication)
    }

    fn matches(
        &self,
        account: account::Id,
        expected: Revision,
        media: &str,
        input_digest: &[u8; 32],
        input_size: usize,
    ) -> bool {
        self.account == account
            && self.expected == expected
            && self.media.as_str() == media
            && self.input_digest == *input_digest
            && usize::try_from(self.input_size).ok() == Some(input_size)
    }

    async fn insert(
        &self,
        client: &Client,
        keys: history::Keys<'_>,
    ) -> Result<(), Error<Rejection>> {
        let fields = self.fields();
        let seal = keys
            .seals
            .sign(
                PUBLICATION_SEAL,
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .map_err(|_| Error::corrupt("publication seal computation"))?;
        client
            .execute(
                "INSERT INTO identity.publications (operation_id, account_id, expected_revision, media_type, input_digest, input_size, candidate_id, work_id, output_digest, output_size, width, height, private, revision, created_at, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)",
                &[&self.operation, &self.account, &self.expected, &self.media.as_str(),
                  &&self.input_digest[..], &self.input_size, &self.candidate, &self.work,
                  &&self.output_digest[..], &self.output_size,
                  &i32::from(self.width), &i32::from(self.height), &self.private,
                  &self.revision, &self.created_at, &seal.tag(), &seal.key_id(), &seal.version()],
            )
            .await
            .map_err(Error::query)?;
        Ok(())
    }

    fn fields(&self) -> Vec<Vec<u8>> {
        vec![
            self.operation.as_bytes().to_vec(),
            self.account.as_bytes().to_vec(),
            self.expected.as_bytes().to_vec(),
            self.media.as_str().as_bytes().to_vec(),
            self.input_digest.to_vec(),
            self.input_size.to_be_bytes().to_vec(),
            self.candidate.as_bytes().to_vec(),
            self.work.as_bytes().to_vec(),
            self.output_digest.to_vec(),
            self.output_size.to_be_bytes().to_vec(),
            self.width.to_be_bytes().to_vec(),
            self.height.to_be_bytes().to_vec(),
            vec![u8::from(self.private)],
            self.revision.as_bytes().to_vec(),
            self.created_at.timestamp_micros().to_be_bytes().to_vec(),
        ]
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Original {
    pub(crate) operation: operation::Id,
    account: account::Id,
    pub(crate) changed: bool,
    pub(crate) revision: Revision,
    pub(crate) completed_at: DateTime<Utc>,
}

impl Original {
    async fn find(
        client: &Client,
        keys: history::Keys<'_>,
        operation: operation::Id,
    ) -> Result<Self, Error<Rejection>> {
        Self::find_optional(client, keys, operation)
            .await?
            .ok_or_else(|| Error::corrupt("an original upload completion retains its result"))
    }

    async fn find_optional(
        client: &Client,
        keys: history::Keys<'_>,
        operation: operation::Id,
    ) -> Result<Option<Self>, Error<Rejection>> {
        let row = client
            .query_opt(
                "SELECT operation_id, account_id, changed, revision, completed_at, seal, seal_key_id, seal_version FROM identity.uploads WHERE operation_id=$1",
                &[&operation],
            )
            .await
            .map_err(Error::query)?;
        row.as_ref().map(|row| Self::decode(row, keys)).transpose()
    }

    pub(crate) fn decode(row: &Row, keys: history::Keys<'_>) -> Result<Self, Error<Rejection>> {
        let operation = row.try_get("operation_id").map_err(Error::decode)?;
        let account = row.try_get("account_id").map_err(Error::decode)?;
        let changed = row.try_get("changed").map_err(Error::decode)?;
        let revision = row.try_get("revision").map_err(Error::decode)?;
        let completed_at = row.try_get("completed_at").map_err(Error::decode)?;
        let seal = bytes32(row, "seal")?;
        let key_id = row.try_get("seal_key_id").map_err(Error::decode)?;
        let version = row.try_get("seal_version").map_err(Error::decode)?;
        let fields = fields(operation, account, changed, revision, completed_at);
        keys.seals
            .check(
                UPLOAD_SEAL,
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                &crate::keys::seal::Seal::stored(seal.to_vec(), key_id, version)
                    .map_err(|_| Error::corrupt("upload result retains its seal"))?,
            )
            .map_err(|_| Error::corrupt("upload result retains its seal"))?;
        Ok(Self {
            operation,
            account,
            changed,
            revision,
            completed_at,
        })
    }

    async fn insert(
        client: &Client,
        keys: history::Keys<'_>,
        operation: operation::Id,
        account: account::Id,
        changed: bool,
        revision: Revision,
        completed_at: DateTime<Utc>,
    ) -> Result<(), Error<Rejection>> {
        let fields = fields(operation, account, changed, revision, completed_at);
        let seal = keys
            .seals
            .sign(
                UPLOAD_SEAL,
                1,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .map_err(|_| Error::corrupt("upload result seal computation"))?;
        client
            .execute(
                "INSERT INTO identity.uploads (operation_id, account_id, changed, revision, completed_at, seal, seal_key_id, seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
                &[&operation, &account, &changed, &revision, &completed_at, &seal.tag(), &seal.key_id(), &seal.version()],
            )
            .await
            .map_err(Error::query)?;
        Ok(())
    }
}

fn fields(
    operation: operation::Id,
    account: account::Id,
    changed: bool,
    revision: Revision,
    completed_at: DateTime<Utc>,
) -> Vec<Vec<u8>> {
    vec![
        operation.as_bytes().to_vec(),
        account.as_bytes().to_vec(),
        vec![u8::from(changed)],
        revision.as_bytes().to_vec(),
        completed_at.timestamp_micros().to_be_bytes().to_vec(),
    ]
}

#[expect(
    clippy::too_many_arguments,
    reason = "Completion inputs bind each immutable original identity explicitly."
)]
fn request(
    account_id: account::Id,
    person_id: crate::identity::person::Id,
    revision_id: crate::identity::person::revision::Id,
    expected: Revision,
    media: &str,
    input_digest: &[u8; 32],
    input_size: usize,
    now: DateTime<Utc>,
) -> Result<completion::Request, Error<Rejection>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Input<'a> {
        account_id: account::Id,
        expected_revision: Revision,
        media_type: &'a str,
        input_digest: String,
        input_size: usize,
    }
    completion::Request::new(
        "replace",
        account_id.to_string(),
        &Input {
            account_id,
            expected_revision: expected,
            media_type: media,
            input_digest: hex::encode(input_digest),
            input_size,
        },
        &Authorization {
            person_id,
            revision_id,
            now,
        },
    )
}

fn bytes32(row: &Row, column: &str) -> Result<[u8; 32], Error<Rejection>> {
    row.try_get::<_, Vec<u8>>(column)
        .map_err(Error::decode)?
        .try_into()
        .map_err(|_| Error::corrupt("stored avatar digest is 32 bytes"))
}

fn file_fault(error: avatar::storage::Error) -> Error<Rejection> {
    let corrupt = matches!(error, avatar::storage::Error::Identity);
    let fault = Fault::new(Stage::Command, FaultKind::File(error));
    if corrupt {
        Error::Corrupt(fault)
    } else {
        Error::Unavailable(fault)
    }
}

pub(super) fn avatar_fault(error: avatar::Error) -> Error<Rejection> {
    let corrupt = error.unreliable();
    let fault = Fault::new(Stage::Command, FaultKind::Avatar(error));
    if corrupt {
        Error::Corrupt(fault)
    } else {
        Error::Unavailable(fault)
    }
}

pub(super) fn report_end(
    end: Result<(), crate::kernel::tx::EndError>,
    cleanup: Result<(), platform::database::Error>,
) {
    if let Err(error) = end {
        tracing::error!(stage = "avatar_transaction_end", error = ?error);
    }
    if let Err(error) = cleanup {
        tracing::error!(stage = "avatar_connection_end", error = ?error);
    }
}

fn history_fault(error: history::Error) -> Error<Rejection> {
    match error {
        history::Error::Changed => Error::lock_set_changed(),
        history::Error::Query(error) => {
            Error::Unavailable(Fault::new(Stage::Command, FaultKind::Query(error)))
        }
        error => Error::Corrupt(Fault::new(Stage::Command, FaultKind::History(error))),
    }
}

fn identity_fault(error: current::Error) -> Error<Rejection> {
    error.into()
}

fn ended(error: Error<Rejection>, end: Result<(), Fault>) -> Error<Rejection> {
    match (error, end) {
        (Error::Unavailable(fault), end) => Error::Unavailable(fault.then_result(end)),
        (Error::Unknown(fault), end) => Error::Unknown(fault.then_result(end)),
        (Error::Corrupt(fault), end) => Error::Corrupt(fault.then_result(end)),
        (error, _) => error,
    }
}

fn bounded(total: Instant, duration: Duration) -> Instant {
    std::cmp::min(Instant::now() + duration, total)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use fixture::database_proxy::{Fault as ProxyFault, Proxy};
    use image::ImageEncoder as _;
    use tokio::{runtime::Runtime, time::timeout};

    use super::*;
    use crate::{
        access::role::tests::{actor, start},
        identity::authentication::{self, Database, Supplier, server},
    };

    fn png(rgba: &[u8; 16]) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(rgba, 2, 2, image::ExtendedColorType::Rgba8)
            .expect("real PNG fixture");
        bytes
    }

    async fn replace(
        services: &crate::startup::Services,
        actor: &current::Actor,
        operation: operation::Id,
        revision: Revision,
        media: &str,
        bytes: &[u8],
    ) -> Result<avatar::Completed, Error<Rejection>> {
        let service = services.avatar();
        service
            .upload(
                services.current(),
                actor.clone(),
                operation,
                revision,
                media.to_owned(),
                bytes.to_vec(),
                service.upload_slot().expect("upload capacity"),
                Instant::now() + Duration::from_secs(30),
            )
            .await
    }

    #[test]
    fn startup_rejects_an_avatar_completion_after_its_upload_result_is_deleted() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(40),
                Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let before = services
                        .current()
                        .read(&actor)
                        .await
                        .expect("current owner");
                    let operation = operation::Id::allocate();
                    replace(
                        &services,
                        &actor,
                        operation,
                        before.avatar().revision(),
                        "image/png",
                        &png(&[79; 16]),
                    )
                    .await
                    .expect("completed upload");
                    let avatar = services.avatar();
                    settle(services.app(), &avatar.0.storage, operation)
                        .await
                        .expect("retire publication intent");
                    let owner = database
                        .connections(&database.owner)
                        .recover()
                        .await
                        .expect("fixture owner");
                    avatar
                        .verify(owner.client(), Utc::now())
                        .await
                        .expect("valid retained completion and upload");
                    assert!(
                        Publication::find(owner.client(), services.app().keys(), operation)
                            .await
                            .expect("intent lookup")
                            .is_none()
                    );
                    owner
                        .client()
                        .batch_execute(
                            "ALTER TABLE identity.uploads DISABLE TRIGGER uploads_deletion_check",
                        )
                        .await
                        .expect("enable partial deletion fixture");
                    assert_eq!(
                        owner
                            .client()
                            .execute(
                                "DELETE FROM identity.uploads WHERE operation_id=$1",
                                &[&operation]
                            )
                            .await
                            .expect("remove only upload result"),
                        1
                    );
                    owner
                        .client()
                        .batch_execute(
                            "ALTER TABLE identity.uploads ENABLE TRIGGER uploads_deletion_check",
                        )
                        .await
                        .expect("restore deletion guard");
                    assert!(
                        completion::original::<Rejection>(
                            owner.client(),
                            services.app().keys(),
                            COMPLETIONS,
                            operation
                        )
                        .await
                        .expect("authenticated original remains")
                        .is_some()
                    );
                    assert!(
                        matches!(
                            avatar.verify(owner.client(), Utc::now()).await,
                            Err(Error::Corrupt(_))
                        ),
                        "startup rejects partial deletion even without a publication intent"
                    );
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    owner.terminate().await.expect("owner ended");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded orphan original verification");
        }));
    }

    #[test]
    fn retained_upload_verification_renews_each_batch_budget_and_bounds_a_stalled_batch() {
        async fn start_retained(
            database: &Database,
            proxy: &Proxy,
            supplier: &Supplier,
            storage: &avatar::storage::Settings,
        ) -> (crate::runtime::Context, crate::startup::Services) {
            let context = crate::runtime::Context::new(authentication::proxied(database, proxy))
                .await
                .expect("retained context");
            let settings = crate::startup::Settings::new(
                4,
                crate::identity::message::Limits::default(),
                authentication::session_settings(),
                crate::registration::Limits::default(),
                authentication::avatar_settings(),
            )
            .expect("settings")
            .with_avatar_storage(Some(storage.clone()));
            let services = Box::pin(crate::startup::Services::start(
                crate::database::Barrier::begin(context.clone())
                    .await
                    .expect("barrier"),
                authentication::inventory(authentication::keys()),
                settings,
                supplier.client(),
                authentication::unused_client().await,
            ))
            .await
            .expect("verified retained services");
            (context, services)
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(180), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let mut supplier = Supplier::new().await;
                let (root, mount, device) = server.files();
                let storage = avatar::storage::Settings::new(root, mount, device).expect("retained storage");
                let (context, services) = start_retained(&database, &proxy, &supplier, &storage).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let before = services.current().read(&actor).await.expect("current owner");
                let bytes = png(&[71; 16]);
                // Fixture construction performs 129 durable writes; its CI I/O
                // budget is separate from the startup and per-batch assertions.
                timeout(Duration::from_secs(120), async {
                    let first = replace(&services, &actor, operation::Id::allocate(), before.avatar().revision(), "image/png", &bytes)
                        .await.expect("first private upload");
                    for _ in 0..128 {
                        let result = replace(&services, &actor, operation::Id::allocate(), first.completion().revision(), "image/png", &bytes)
                            .await.expect("retained unchanged upload");
                        assert!(!result.completion().changed());
                    }
                }).await.expect("bounded retained upload fixture construction");
                services.close(); services.drain().await.expect("original drained"); context.stop().await.expect("original stopped");
                proxy.arm(ProxyFault::StalledAvatarUploads);
                let delays = async {
                    timeout(Duration::from_secs(10), proxy.intercepted()).await.expect("first upload batch reached");
                    tokio::time::sleep(Duration::from_secs(6)).await;
                    proxy.arm(ProxyFault::StalledAvatarUploads);
                    proxy.release();
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("second upload batch reached");
                    tokio::time::sleep(Duration::from_secs(6)).await;
                    proxy.release();
                };
                let started = Instant::now();
                let ((context, services), ()) = timeout(Duration::from_secs(30), async {
                    tokio::join!(start_retained(&database, &proxy, &supplier, &storage), delays)
                }).await.expect("healthy startup spans more than one batch budget");
                assert!(started.elapsed() >= Duration::from_secs(12));
                services.current().read(&actor).await.expect("restarted service admits requests");
                let observer = authentication::proxied(&database, &proxy).recover().await.expect("verification observer");
                proxy.arm(ProxyFault::StalledAvatarUploads);
                let avatar = services.avatar();
                let (result, intercepted) = tokio::join!(
                    timeout(Duration::from_secs(12), avatar.verify(observer.client(), Utc::now())),
                    timeout(Duration::from_secs(3), proxy.intercepted())
                );
                proxy.release();
                intercepted.expect("stalled verification batch reached");
                let failure = result.expect("one stalled batch is bounded").expect_err("stalled batch fails verification");
                assert!(matches!(failure, Error::Unavailable(ref fault)
                    if matches!(fault.kind(), FaultKind::Avatar(avatar::Error::Verification(timeout))
                        if matches!(timeout.stage(), crate::database::verification::Stage::Avatars))));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                proxy.stop().await; supplier.stop().await;
            })).await.expect("bounded retained upload startup acceptance");
        }));
    }

    #[test]
    fn reused_candidates_finish_interrupted_publication_before_adoption() {
        use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let (root, mount, device) = server.files();
            let storage = Storage::open(
                &super::super::storage::Settings::new(root.clone(), mount, device)
                    .expect("private storage settings"),
            )
            .expect("private storage");
            let input = png(&[47; 16]);
            let converted = codec::convert(codec::Media::Png, &input).expect("real static output");
            let candidate = Id::allocate();
            let publication = Publication {
                operation: operation::Id::allocate(),
                account: Uuid::new_v4().to_string().parse().expect("account ID"),
                expected: Revision::allocate(),
                media: codec::Media::Png,
                input_digest: Sha256::digest(&input).into(),
                input_size: input.len() as i64,
                candidate,
                work: Uuid::new_v4(),
                output_digest: Sha256::digest(&converted.bytes).into(),
                output_size: converted.bytes.len() as i64,
                width: converted.width,
                height: converted.height,
                private: true,
                revision: Revision::allocate(),
                changed: None,
                completed_at: None,
                created_at: Utc::now(),
            };
            storage
                .publish(candidate, &converted.bytes)
                .expect("visible candidate");
            let staging = root
                .join("avatars")
                .join(hex::encode(&candidate.as_bytes()[..1]))
                .join(hex::encode(&candidate.as_bytes()[1..2]))
                .join(format!(
                    "{}.candidate",
                    Uuid::from_bytes(*candidate.as_bytes())
                ));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staging)
                .expect("interrupted publication artifact")
                .write_all(b"unfinished")
                .expect("partial staging bytes");
            let permit = Arc::new(
                Arc::new(tokio::sync::Semaphore::new(1))
                    .acquire_owned()
                    .await
                    .expect("upload permit"),
            );
            let bytes = ensure_candidate(
                &storage,
                &Defaults::verify().expect("defaults"),
                codec::Media::Png,
                &input,
                &publication,
                permit,
                Instant::now() + BUSINESS,
            )
            .await
            .expect("repaired publication");
            assert_eq!(bytes, converted.bytes);
            assert!(
                !staging.exists(),
                "reuse completes the existing-file publication protocol"
            );
            assert_eq!(
                storage
                    .read(candidate, bytes.len() as u64)
                    .expect("same immutable inode bytes"),
                bytes
            );
        });
    }

    #[test]
    fn cancellation_between_upload_authorization_and_content_selection_keeps_admission_open() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(40),
                Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let manager = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let actor = Box::pin(crate::access::appointment::tests::register(
                        &database,
                        &services,
                        &mut supplier,
                    ))
                    .await;
                    let before = services
                        .current()
                        .read(&actor)
                        .await
                        .expect("qualified upload owner");
                    let operation = operation::Id::allocate();
                    let input = png(&[37; 16]);
                    assert!(matches!(
                        super::start(
                            services.app(),
                            services.current(),
                            &actor,
                            operation,
                            before.avatar().revision(),
                            "image/png",
                            Sha256::digest(&input).into(),
                            input.len(),
                            Instant::now() + BUSINESS
                        )
                        .await
                        .expect("upload preflight"),
                        Start::Fresh
                    ));
                    services
                        .cancellation()
                        .cancel(
                            actor.clone(),
                            operation::Id::allocate(),
                            crate::cancellation::tests::confirmation(&before),
                        )
                        .await
                        .expect("cancellation during decoding");
                    let avatar = services.avatar();
                    let outcome = current_content(
                        services.app(),
                        services.current(),
                        &avatar.0.storage,
                        &avatar.0.defaults,
                        &actor,
                        operation,
                        Instant::now() + BUSINESS,
                    )
                    .await;
                    assert!(
                        matches!(outcome, Err(Error::Unauthenticated)),
                        "legitimate cancellation is not corruption"
                    );
                    services
                        .current()
                        .read(&manager)
                        .await
                        .expect("unrelated account remains admitted");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded cancellation race");
        }));
    }

    #[test]
    fn pending_originals_require_account_ownership_and_restart_settles_retained_intents() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let other = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                let before = services.current().read(&actor).await.expect("current owner");
                let input = png(&[41; 16]);
                let converted = codec::convert(codec::Media::Png, &input).expect("static output");
                let output = Output { digest: Sha256::digest(&converted.bytes).into(), bytes: converted.bytes,
                    width: converted.width, height: converted.height };
                let operation = operation::Id::allocate();
                publish_intent(services.app(), services.current(), &actor, operation, before.avatar().revision(),
                    codec::Media::Png, Sha256::digest(&input).into(), input.len(), &output, true, true,
                    Instant::now() + BUSINESS).await.expect("durable pending intent before file publication");
                let mut permit = services.app().registry().acquire_for(
                    crate::runtime::registry::Family::Kernel(Family::Avatar), operation).await.expect("active original");
                permit.begin();
                for (reader, own) in [(&other, false), (&actor, true)] {
                    let value = services.avatar().original(services.current(), reader, operation,
                        Instant::now() + Duration::from_millis(500)).await.expect("bounded lookup during active work");
                    assert!(matches!((value, own), (avatar::OriginalView::Pending, true) | (avatar::OriginalView::Absent, false)));
                }
                permit.finish();
                services.close(); services.drain().await.expect("old work ended"); context.stop().await.expect("old instance ended");
                let (context, services) = timeout(Duration::from_secs(5), start(database.connections(&database.user), &supplier))
                    .await.expect("startup recovery must not wait behind its own exclusive barrier");
                let observer = database.connections(&database.user).recover().await.expect("independent reader");
                let remains: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.publications WHERE operation_id=$1)",
                    &[&operation]).await.expect("startup intent readback").get(0);
                assert!(!remains, "startup completes unused publication cleanup before exposing services");
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart ended");
                observer.terminate().await.expect("reader ended"); supplier.stop().await;
            })).await.expect("bounded pending original and restart acceptance");
        }));
    }

    #[test]
    fn original_lookup_closes_admission_on_a_damaged_retained_seal() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(40),
                Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let before = services
                        .current()
                        .read(&actor)
                        .await
                        .expect("current owner");
                    let operation = operation::Id::allocate();
                    replace(
                        &services,
                        &actor,
                        operation,
                        before.avatar().revision(),
                        "image/png",
                        &png(&[43; 16]),
                    )
                    .await
                    .expect("completed upload");
                    let owner = database
                        .connections(&database.owner)
                        .recover()
                        .await
                        .expect("fixture owner");
                    owner
                        .client()
                        .batch_execute(
                            "ALTER TABLE identity.uploads DISABLE TRIGGER uploads_immutable_check",
                        )
                        .await
                        .expect("enable stored-fact damage fixture");
                    owner
                        .client()
                        .execute(
                            "UPDATE identity.uploads SET seal=$2 WHERE operation_id=$1",
                            &[&operation, &vec![0_u8; 32]],
                        )
                        .await
                        .expect("damage retained seal");
                    owner
                        .client()
                        .batch_execute(
                            "ALTER TABLE identity.uploads ENABLE TRIGGER uploads_immutable_check",
                        )
                        .await
                        .expect("restore immutable guard");
                    let result = services
                        .avatar()
                        .original(
                            services.current(),
                            &actor,
                            operation,
                            Instant::now() + BUSINESS,
                        )
                        .await;
                    assert!(matches!(result, Err(Error::Corrupt(_))));
                    assert!(
                        services.app().registry().ready().is_err(),
                        "detected corruption closes common admission"
                    );
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    owner.terminate().await.expect("fixture owner ended");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded corrupt original read");
        }));
    }

    #[test]
    fn real_uploads_preserve_original_results_pixels_and_unchanged_revisions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let before = services.current().read(&actor).await.expect("current account");
                let initial = before.avatar().revision();
                let pixels = [231, 71, 19, 0, 3, 5, 7, 127, 11, 13, 17, 255, 23, 29, 31, 255];
                let input = png(&pixels);
                let first = operation::Id::allocate();
                let changed = replace(&services, &actor, first, initial, "image/png", &input).await.expect("changed upload");
                assert!(changed.completion().changed());
                let revision = changed.completion().revision();
                assert_ne!(revision, initial);
                let (_, stored) = current_content(services.app(), services.current(), &services.avatar().0.storage,
                    &services.avatar().0.defaults, &actor, operation::Id::allocate(), Instant::now() + BUSINESS)
                    .await.expect("current private avatar bytes");
                assert_eq!(image::load_from_memory_with_format(&stored, image::ImageFormat::WebP).expect("independent WebP decode").to_rgba8().as_raw(), &pixels);
                let unchanged = replace(&services, &actor, operation::Id::allocate(), revision, "image/png", &input).await.expect("unchanged upload");
                assert!(!unchanged.completion().changed());
                assert_eq!(unchanged.completion().revision(), revision);
                let mismatch = replace(&services, &actor, first, initial, "application/octet-stream", &input).await.err().expect("different original media");
                assert!(matches!(mismatch, Error::Mismatch));
                let next = png(&[47, 53, 59, 255, 61, 67, 71, 255, 73, 79, 83, 255, 89, 97, 101, 255]);
                let later = replace(&services, &actor, operation::Id::allocate(), revision, "image/png", &next).await.expect("later upload");
                let replay = replace(&services, &actor, first, initial, "image/png", &input).await.expect("original success before stale revision");
                assert_eq!(replay.completion().revision(), revision);
                assert_eq!(replay.completion().completed_at(), changed.completion().completed_at());
                assert_eq!(replay.avatar().revision(), later.avatar().revision());
                let other = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                assert!(matches!(services.avatar().original(services.current(), &other, first, Instant::now() + Duration::from_secs(10)).await.expect("qualified other account lookup"), avatar::OriginalView::Absent));
                let avatar::OriginalView::Available { completion, avatar } = services.avatar().original(services.current(), &actor, first, Instant::now() + Duration::from_secs(10)).await.expect("original result lookup") else { panic!("retained original is available"); };
                assert_eq!(completion.revision(), revision);
                assert_eq!(avatar.revision(), later.avatar().revision());
                let observer = database.connections(&database.user).recover().await.expect("runtime-role observer");
                let counts: (i64, i64) = {
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.uploads WHERE account_id=$1), (SELECT count(*) FROM identity.portraits WHERE account_id=$1)", &[&before.id()]).await.expect("independent business counts");
                    (row.get(0), row.get(1))
                };
                assert_eq!(counts, (3, 2));
                for (sql, code) in [
                    ("UPDATE identity.uploads SET changed=NOT changed", "42501"),
                    ("DELETE FROM identity.uploads", "23514"),
                    ("DELETE FROM identity.portraits", "42501"),
                    ("UPDATE identity.publications SET changed=NOT changed WHERE changed IS NOT NULL", "23514"),
                    ("UPDATE identity.publications SET expected_revision=gen_random_uuid()", "42501"),
                    ("DELETE FROM identity.avatars WHERE released_at IS NOT NULL", "23514"),
                    ("UPDATE identity.avatars SET digest=decode(repeat('00',32),'hex')", "42501"),
                    ("UPDATE identity.avatars SET content_seal=NULL, content_seal_key_id=NULL, content_seal_version=NULL", "42501"),
                    ("DELETE FROM identity.completions WHERE family='avatar'", "23514"),
                ] {
                    let error = observer.client().batch_execute(sql).await.expect_err("runtime identity cannot bypass paired facts");
                    assert_eq!(error.code().expect("database rejection").code(), code, "{sql}: {error}");
                }
                assert_eq!(Original::find(observer.client(), services.app().keys(), first).await.expect("original survives refused mutations").revision, revision);
                observer.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("accepted uploads drained");
                context.stop().await.expect("clock stopped");
                supplier.stop().await;
            })).await.expect("upload integration budget");
        }));
    }

    #[test]
    fn private_metadata_proof_rejects_damaged_or_transplanted_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let before = services.current().read(&actor).await.expect("current account");
                let input = png(&[17, 31, 47, 255, 53, 61, 71, 255, 79, 83, 89, 255, 97, 101, 103, 255]);
                replace(&services, &actor, operation::Id::allocate(), before.avatar().revision(), "image/png", &input).await.expect("private upload");
                let observer = database.connections(&database.user).recover().await.expect("runtime reader");
                let query = "SELECT changed.id, changed.account_id, changed.revision AS avatar, changed.digest, changed.size, changed.width, changed.height, changed.content_seal, changed.content_seal_key_id, changed.content_seal_version FROM identity.avatars original CROSS JOIN LATERAL jsonb_populate_record(NULL::identity.avatars, to_jsonb(original) || $2::text::jsonb) changed WHERE original.account_id=$1 AND original.released_at IS NULL";
                let row = observer.client().query_one(query, &[&actor.account_id(), &"{}"]).await.expect("actual immutable metadata");
                let original = services.avatar().snapshot(&row).expect("adopted private proof");
                assert!(row.get::<_, Option<Vec<u8>>>("content_seal").is_some());
                for damage in [
                    serde_json::json!({"id": uuid::Uuid::new_v4()}),
                    serde_json::json!({"account_id": uuid::Uuid::new_v4()}),
                    serde_json::json!({"revision": uuid::Uuid::new_v4()}),
                    serde_json::json!({"digest": format!("\\x{}", "00".repeat(32))}),
                    serde_json::json!({"size": original.size() + 1}),
                    serde_json::json!({"width": 3, "height": 3}),
                    serde_json::json!({"content_seal": format!("\\x{}", "00".repeat(32))}),
                    serde_json::json!({"content_seal_version": 2}),
                    serde_json::json!({"content_seal_key_id": null}),
                    serde_json::json!({"content_seal": null, "content_seal_key_id": null, "content_seal_version": null}),
                ] {
                    let row = observer.client().query_one(query, &[&actor.account_id(), &damage.to_string()]).await.expect("independently changed stored facts");
                    let error = services.avatar().snapshot(&row).err().expect("untrusted metadata cannot become a signable snapshot");
                    assert!(error.unreliable(), "{damage}: {error:?}");
                }
                observer.terminate().await.expect("reader ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("clock ended"); supplier.stop().await;
            })).await.expect("bounded metadata proof test");
        }));
    }

    #[test]
    fn result_commit_recovery_preserves_the_original_candidate_and_ends_the_old_driver() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (committed, cancelled) in [(false, false), (true, false), (true, true)] {
                timeout(Duration::from_secs(50), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let actor = if cancelled {
                        Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await
                    } else { actor };
                    let before = services.current().read(&actor).await.expect("initial account");
                    let operation = operation::Id::allocate();
                    let input = png(&[1, 2, 3, 255, 5, 7, 11, 255, 13, 17, 19, 255, 23, 29, 31, 255]);
                    let converted = codec::convert(codec::Media::Png, &input).expect("real output");
                    let output = Output {
                        digest: Sha256::digest(&converted.bytes).into(),
                        bytes: converted.bytes, width: converted.width, height: converted.height,
                    };
                    let avatar = services.avatar();
                    let publication = publish_intent(
                        services.app(), services.current(), &actor, operation, before.avatar().revision(),
                        codec::Media::Png, Sha256::digest(&input).into(), input.len(), &output,
                        true, true, Instant::now() + Duration::from_secs(10),
                    ).await.expect("confirmed first transaction");
                    avatar.0.storage.publish(publication.candidate, &output.bytes).expect("durable private candidate");
                    let observer = database.connections(&database.user).recover().await.expect("independent reader");
                    proxy.arm(if committed { ProxyFault::DelayedCommit } else { ProxyFault::DelayedRollback });
                    let attempt = result(services.app(), services.current(), &avatar.0.storage, &avatar.0.defaults,
                            avatar.0.limits, &actor, operation, before.avatar().revision(), &publication,
                            &output.bytes, Instant::now() + Duration::from_secs(30));
                    tokio::pin!(attempt);
                    let backend = tokio::select! {
                        backend = timeout(Duration::from_secs(5), proxy.intercepted()) => backend.expect("result COMMIT reached proxy"),
                        outcome = &mut attempt => panic!("result returned before its commit fault: {:?}", outcome.err()),
                    };
                    let visible: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.uploads WHERE operation_id=$1)", &[&operation]).await.expect("original result before decision").get(0);
                    assert!(!visible);
                    let outcome = if cancelled {
                        let cancellation = services.cancellation().cancel(actor.clone(), operation::Id::allocate(), crate::cancellation::tests::confirmation(&before));
                        tokio::pin!(cancellation);
                        tokio::select! {
                            result = &mut cancellation => panic!("cancellation must wait behind original upload: {:?}", result.err()),
                            waiting = timeout(Duration::from_secs(5), async {
                                loop {
                                    let queued: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE' AND $1=ANY(pg_blocking_pids(pid)))", &[&backend]).await.expect("cancellation is queued behind the real upload account lock").get(0);
                                    if queued { break; }
                                    tokio::task::yield_now().await;
                                }
                            }) => waiting.expect("bounded cancellation lock wait"),
                        }
                        proxy.release();
                        let (outcome, cancellation) = tokio::join!(attempt, cancellation);
                        cancellation.expect("real cancellation removes public upload results before recovery");
                        outcome
                    } else {
                        proxy.release();
                        attempt.await
                    };
                    assert_eq!(outcome.is_ok(), committed, "{:?}", outcome.as_ref().err());
                    let original = Original::find_optional(observer.client(), services.app().keys(), operation).await.expect("sealed original readback");
                    assert_eq!(original.is_some(), committed && !cancelled);
                    if cancelled {
                        assert!(services.current().read(&actor).await.is_err(), "cancelled session loses its current projection");
                        let count: i64 = observer.client().query_one("SELECT count(*) FROM identity.portraits WHERE operation_id=$1", &[&operation]).await.expect("permanent advancement survives cancellation").get(0);
                        assert_eq!(count, 1);
                    } else {
                        let account = services.current().read(&actor).await.expect("admission after known outcome");
                        assert_eq!(account.avatar().revision(), if committed { publication.revision } else { before.avatar().revision() });
                    }
                    assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&backend]).await.expect("old backend ended").get::<_, bool>(0));
                    services.avatar_retention().run().await.expect("settle ended original file effects");
                    assert_eq!(avatar.0.storage.open_file(publication.candidate).expect("candidate readback").is_some(), committed);
                    assert!(Publication::find(observer.client(), services.app().keys(), operation).await.expect("intent readback").is_none());
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("clock ended");
                    observer.terminate().await.expect("reader ended"); proxy.stop().await; supplier.stop().await;
                }).await.expect("bounded original result recovery");
            }
        });
    }

    #[test]
    fn publication_commit_unknown_never_publishes_before_original_recovery() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for committed in [false, true] {
                timeout(Duration::from_secs(45), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                    let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let before = services.current().read(&actor).await.expect("initial account");
                    let operation = operation::Id::allocate();
                    let input = png(&[2, 3, 5, 255, 7, 11, 13, 255, 17, 19, 23, 255, 29, 31, 37, 255]);
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    proxy.arm(if committed { ProxyFault::DelayedCommit } else { ProxyFault::DelayedRollback });
                    let (outcome, backend) = tokio::join!(
                        replace(&services, &actor, operation, before.avatar().revision(), "image/png", &input),
                        async {
                            let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("publication COMMIT reached proxy");
                            let visible: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.publications WHERE operation_id=$1)", &[&operation]).await.expect("intent remains unconfirmed").get(0);
                            assert!(!visible);
                            proxy.release();
                            backend
                        },
                    );
                    assert!(matches!(outcome, Err(Error::Unknown(_))), "the first response retains the unknown publication outcome");
                    let recovered = super::start(services.app(), services.current(), &actor, operation,
                        before.avatar().revision(), "image/png", Sha256::digest(&input).into(), input.len(),
                        Instant::now() + Duration::from_secs(10)).await.expect("same original lock resolves the intent");
                    match recovered {
                        Start::Resume(publication) if committed => {
                            assert!(services.avatar().0.storage.open_file(publication.candidate).expect("real file readback").is_none(), "unknown publication never starts file effects");
                        }
                        Start::Fresh if !committed => {}
                        _ => panic!("the original intent matches the actual transaction outcome"),
                    }
                    let current = services.current().read(&actor).await.expect("old avatar retained");
                    assert_eq!(current.avatar().revision(), before.avatar().revision());
                    assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&backend]).await.expect("old execution ended").get::<_, bool>(0));
                    let completed = replace(&services, &actor, operation, before.avatar().revision(), "image/png", &input).await.expect("same original resumes after confirmed outcome");
                    assert!(completed.completion().changed());
                    let count: i64 = observer.client().query_one("SELECT count(*) FROM identity.uploads WHERE operation_id=$1", &[&operation]).await.expect("one original completion").get(0);
                    assert_eq!(count, 1);
                    services.avatar_retention().run().await.expect("original publication cleanup");
                    services.close(); services.drain().await.expect("work drained"); context.stop().await.expect("clock stopped");
                    observer.terminate().await.expect("observer ended"); proxy.stop().await; supplier.stop().await;
                }).await.expect("bounded publication recovery");
            }
        });
    }

    #[test]
    fn replacement_quota_preserves_unchanged_and_original_success_at_the_hour_boundary() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(60), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let before = services.current().read(&actor).await.expect("initial account");
                let initial = before.avatar().revision();
                let first_operation = operation::Id::allocate();
                let first_input = png(&[1; 16]);
                let first = replace(&services, &actor, first_operation, initial, "image/png", &first_input).await.expect("first replacement");
                let mut revision = first.completion().revision();
                let mut input = first_input.clone();
                for color in 2..=10 {
                    input = png(&[color; 16]);
                    let completed = replace(&services, &actor, operation::Id::allocate(), revision, "image/png", &input).await.expect("remaining hourly replacement");
                    assert!(completed.completion().changed());
                    revision = completed.completion().revision();
                }
                let refused = replace(&services, &actor, operation::Id::allocate(), revision, "image/png", &png(&[11; 16])).await;
                assert!(matches!(refused, Err(Error::Rejected(Rejection::Limited))));
                let unchanged = replace(&services, &actor, operation::Id::allocate(), revision, "image/png", &input).await.expect("unchanged does not consume quota");
                assert!(!unchanged.completion().changed());
                assert_eq!(unchanged.avatar().revision(), revision);
                let replay = replace(&services, &actor, first_operation, initial, "image/png", &first_input).await.expect("original success precedes quota and stale revision");
                assert_eq!(replay.completion().revision(), first.completion().revision());
                assert_eq!(replay.avatar().revision(), revision);
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.uploads WHERE account_id=$1), (SELECT count(*) FROM identity.portraits WHERE account_id=$1)", &[&before.id()]).await.expect("independent counts");
                assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (11, 10));
                let boundary = first.completion().completed_at() + TimeDelta::hours(1);
                assert!(quota(observer.client(), before.id(), boundary - TimeDelta::microseconds(1), Limits::default()).await.expect("inside window"));
                assert!(!quota(observer.client(), before.id(), boundary, Limits::default()).await.expect("first replacement expires at equality"));
                services.avatar_retention().run().await.expect("unused over-quota candidate is reclaimed");
                assert_eq!(services.current().read(&actor).await.expect("current account after cleanup").avatar().revision(), revision);
                services.close(); services.drain().await.expect("work drained"); context.stop().await.expect("clock stopped");
                observer.terminate().await.expect("observer ended"); supplier.stop().await;
            }).await.expect("bounded quota acceptance");
        });
    }

    #[test]
    fn dropped_upload_waiters_keep_capacity_until_competing_original_work_ends() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(45), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = Box::pin(actor(&database, &services, &mut supplier)).await;
                let account = services.current().read(&actor).await.expect("current account");
                let revision = account.avatar().revision();
                let blocker = database.connections(&database.user).recover().await.expect("runtime-role lock owner");
                let observer = database.connections(&database.user).recover().await.expect("independent activity observer");
                blocker.client().batch_execute("BEGIN").await.expect("blocker transaction");
                account::state::lock(blocker.client(), account.id()).await.expect("hold real account lock");
                let operations = [operation::Id::allocate(), operation::Id::allocate()];
                let mut waiters = Vec::new();
                for operation in operations {
                    let avatar = services.avatar().clone();
                    let current = services.current().clone();
                    let actor = actor.clone();
                    let input = png(&[31, 41, 59, 255, 26, 53, 58, 255, 97, 93, 23, 255, 84, 62, 64, 255]);
                    waiters.push(tokio::spawn(async move {
                        let permit = avatar.upload_slot().expect("one of two upload permits");
                        avatar.upload(&current, actor, operation, revision, "image/png".into(), input, permit,
                            Instant::now() + Duration::from_secs(30)).await
                    }));
                }
                timeout(Duration::from_secs(5), async {
                    loop {
                        let waiting: i64 = observer.client().query_one(
                            "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE' AND cardinality(pg_blocking_pids(pid))>0", &[],
                        ).await.expect("actual waiting operations").get(0);
                        if waiting >= 2 { break; }
                        tokio::task::yield_now().await;
                    }
                }).await.expect("both admitted operations reach their real SQL lock");
                for waiter in waiters {
                    waiter.abort();
                    assert!(waiter.await.is_err(), "caller wait ends independently");
                }
                assert_eq!(services.avatar().0.slots.available_permits(), 0);
                assert!(matches!(services.avatar().upload_slot(), Err(avatar::Error::Busy)));
                blocker.client().batch_execute("COMMIT").await.expect("release account");
                for operation in operations {
                    timeout(Duration::from_secs(20), services.app().registry().wait_for(
                        crate::runtime::registry::Family::Kernel(Family::Avatar), operation,
                    )).await.expect("original work deadline").expect("original ended");
                }
                assert_eq!(services.avatar().0.slots.available_permits(), 2);
                let count: i64 = blocker.client().query_one("SELECT count(*) FROM identity.uploads WHERE account_id=$1", &[&account.id()]).await.expect("decisive original results").get(0);
                assert_eq!(count, 1, "only one competing original can consume the expected revision");
                let after = services.current().read(&actor).await.expect("decisive current projection");
                assert_ne!(after.avatar().revision(), revision);
                services.close(); services.drain().await.expect("no work left at drain"); context.stop().await.expect("clock stopped");
                blocker.terminate().await.expect("lock owner ended");
                observer.terminate().await.expect("activity observer ended"); supplier.stop().await;
            }).await.expect("bounded dropped-waiter acceptance");
        });
    }
}
