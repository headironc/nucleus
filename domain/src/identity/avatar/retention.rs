//! Reclaim released default metadata under the original cancellation's work and transaction lock.

use std::{convert::Infallible, sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
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
}

#[derive(Clone, Copy)]
struct Candidate {
    id: Id,
    account: account::Id,
    operation: operation::Id,
}

impl Retention {
    pub(crate) fn new(app: App, avatar: Service, settings: Settings) -> Self {
        Self {
            app,
            avatar,
            settings,
            cursor: None,
        }
    }

    /// The process owns this round through cancellation, recovery and connection termination.
    pub async fn run(&mut self) -> Result<(), Error<Infallible>> {
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
                    registry::Family::Kernel(Family::Cancellation),
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
                "SELECT id, account_id, released_at FROM identity.avatars WHERE released_at <= $1 AND ($2::timestamptz IS NULL OR (released_at,id) > ($2,$3)) ORDER BY released_at,id LIMIT 128",
                &[&cutoff, &self.cursor.map(|cursor| cursor.0), &self.cursor.map(|cursor| cursor.1)],
            ).await.map_err(Error::query)?;
            let mut candidates = Vec::with_capacity(rows.len());
            for row in rows {
                let id = row.try_get("id").map_err(Error::decode)?;
                let account = row.try_get("account_id").map_err(Error::decode)?;
                let released = row.try_get("released_at").map_err(Error::decode)?;
                let operation = account::record::cancellation_operation(lease.client(), account).await?
                    .ok_or_else(|| Error::corrupt("released default avatar retains its original cancellation"))?;
                candidates.push((released, Candidate { id, account, operation }));
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
            Family::Cancellation,
            candidate.operation,
        )
        .await;
        if let Err(fault) = begun {
            return Err(Error::Unavailable(fault.then_result(tx.discard().await)));
        }
        let deleted = match timeout_at(tx.deadline(), self.delete(&tx, candidate))
            .await
            .unwrap_or_else(Error::elapsed)
        {
            Ok(deleted) => deleted,
            Err(error) => return Err(ended(error, tx.discard().await)),
        };
        let cause = match tx.commit().await {
            Ok(()) => return Ok(()),
            Err(fault) if !deleted => return Err(Error::Unavailable(fault)),
            Err(fault) => fault,
        };
        match self.deleted(candidate).await {
            Ok((true, end)) => {
                tracing::error!(stage = "avatar_retention", outcome = "deleted", error = ?cause.then_result(end));
                Ok(())
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

    async fn delete(&self, tx: &Tx, candidate: &Candidate) -> Result<bool, Error<Infallible>> {
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
        .ok_or_else(|| Error::corrupt("released avatar retains its cancelled account"))?;
        let Some(cancellation) = state.cancellation_id() else {
            return Ok(false);
        };
        let record =
            account::record::cancellation(tx.client(), self.app.keys(), cancellation, now).await?;
        if record.account_id() != candidate.account
            || record.operation_id() != candidate.operation
            || record.happened_at() != state.changed_at()
        {
            return Err(Error::corrupt(
                "released avatar belongs to the original account cancellation",
            ));
        }
        let row = tx.client().query_opt(
            "SELECT account_id, digest, size, width, height, released_at FROM identity.avatars WHERE id=$1 FOR UPDATE",
            &[&candidate.id],
        ).await.map_err(Error::query)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let released = self.validate(&row, candidate.account)?;
        if released != state.changed_at() {
            return Err(Error::corrupt(
                "default avatar release matches the account cancellation",
            ));
        }
        if !self.settings.expired(released, now) {
            return Ok(false);
        }
        tx.client()
            .execute("DELETE FROM identity.avatars WHERE id=$1", &[&candidate.id])
            .await
            .map_err(Error::query)?;
        Ok(true)
    }

    fn validate(
        &self,
        row: &Row,
        account: account::Id,
    ) -> Result<DateTime<Utc>, Error<Infallible>> {
        let digest: Vec<u8> = row.try_get("digest").map_err(Error::decode)?;
        let digest: [u8; 32] = digest.try_into().map_err(invalid_digest)?;
        let size = row.try_get("size").map_err(Error::decode)?;
        if row
            .try_get::<_, account::Id>("account_id")
            .map_err(Error::decode)?
            != account
            || row.try_get::<_, i32>("width").map_err(Error::decode)? != 512
            || row.try_get::<_, i32>("height").map_err(Error::decode)? != 512
            || !self.avatar.0.defaults.contains(&digest, size)
        {
            return Err(Error::corrupt(
                "released default avatar matches its immutable content and owner",
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
            Family::Cancellation,
            candidate.operation,
        )
        .await;
        let result = match begun {
            Ok(()) => timeout_at(tx.deadline(), async {
                tx.client()
                    .query_one(
                        "SELECT NOT EXISTS (SELECT 1 FROM identity.avatars WHERE id=$1)",
                        &[&candidate.id],
                    )
                    .await
                    .map_err(Error::query)?
                    .try_get(0)
                    .map_err(Error::decode)
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
        let manager = Box::pin(actor(database, services, supplier)).await;
        let target = Box::pin(appointment::tests::register(database, services, supplier)).await;
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
                    assert_eq!(services.avatar().read(proof(&case.href)).await.expect("old default link remains readable").bytes(), include_bytes!("../../../../assets/avatars/0-cat.webp"));
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
                    assert_eq!(services.avatar().read(proof(&case.href)).await.expect("original link after restart").bytes(), include_bytes!("../../../../assets/avatars/0-cat.webp"));
                    services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer stopped"); supplier.stop().await;
                })).await.expect("bounded cancellation and avatar lifecycle");
            }
        }));
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
