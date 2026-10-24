//! Commands and queries, and the write pipeline implemented once for every command.
//!
//! ```text
//! admission → request → Work (one executor per original operation, bounded slots)
//!   Tx::begin → locks → resolve session at decisive time → authorize permission
//!   → original completion: replay, or request_mismatch
//!   → load → decide (pure) → apply + completion → commit
//!   → unknown commit: recovery session, same original-operation lock, completion lookup
//! ```

use std::{future::Future, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use platform::database::Lease;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;

use crate::{
    history::{self, Anchors, Pending, View},
    identity::{current, operation, person, person::revision},
    kernel::{
        completion::{self, Request, Template},
        error::{Error, Fault, FaultKind, Rejection, Stage},
        tx::{Connection, Tx},
    },
    runtime::{
        Context,
        registry::{self, Registry},
        work::Work,
    },
};

/// Write transactions, their commit and their recovery share this deadline.
const WRITE: Duration = Duration::from_secs(10);

/// The session identity already authorized under this transaction's locks.
#[derive(Clone, Copy)]
pub(crate) struct Authorization {
    pub(crate) person_id: person::Id,
    pub(crate) revision_id: revision::Id,
    pub(crate) now: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    AuthorizationFirst,
    OriginalFirst,
}

/// One write use case. The executor owns its identity, order, recovery and classification.
pub(crate) trait Command: Send + Sync + 'static {
    const COMPLETIONS: Template;
    const ORDER: Order = Order::AuthorizationFirst;
    /// Decisive facts read after the locks.
    type Facts: Send;
    /// The change `decide` selected; `apply` writes it.
    type Change: Send;
    type Output: Send + 'static;
    type Rejection: Rejection;

    /// Online commands declare their original session. System and bootstrap commands
    /// have no session and retain their own qualification after original-result lookup.
    fn session(&self) -> Option<(&current::Service, &current::Actor)>;

    /// The closed action, canonical target and canonical input that identify this request.
    fn request(&self, authority: &Authorization) -> Result<Request, Error<Self::Rejection>>;

    /// Confirms that a miss of this command's original completion is not contradicted by
    /// retained facts this family cannot interpret through the shared completion table.
    fn absent(&self, _tx: &Tx) -> impl Future<Output = Result<(), Error<Self::Rejection>>> + Send {
        std::future::ready(Ok(()))
    }

    /// Acquires this command's locks, including its declared session, in global object order.
    fn locks(&self, tx: &Tx) -> impl Future<Output = Result<(), Error<Self::Rejection>>> + Send;

    /// Applies capability permission to the subject already resolved by the executor.
    fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> impl Future<Output = Result<Authorization, Error<Self::Rejection>>> + Send;

    fn load(
        &self,
        tx: &Tx,
        authority: &Authorization,
    ) -> impl Future<Output = Result<Self::Facts, Error<Self::Rejection>>> + Send;

    /// Pure: no database, clock or supplier access.
    fn decide(
        &self,
        facts: Self::Facts,
        now: DateTime<Utc>,
    ) -> Result<Self::Change, Error<Self::Rejection>>;

    /// Most families have a fixed lifetime; a code rejection follows its verification deadline.
    fn expires_at(&self, _: &Self::Change, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        Self::COMPLETIONS.expires(now)
    }

    fn apply(
        &self,
        tx: &Tx,
        change: Self::Change,
        pending: &mut Option<Pending>,
    ) -> impl Future<Output = Result<Self::Output, Error<Self::Rejection>>> + Send;

    /// Rebuilds the original result from the original records after a matching completion.
    fn replay(
        &self,
        tx: &Tx,
        request: &Request,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<Self::Output, Error<Self::Rejection>>> + Send;
}

/// One read: permission and the result share a read-only snapshot.
pub(crate) trait Query: Send + Sync {
    /// A complete original result remains known if ending its read-only transaction fails.
    const RETAIN_ON_END_FAILURE: bool = false;
    type Authorized: Send;
    type Output: Send;
    type Rejection: Rejection;

    fn authorize(
        &self,
        client: &Client,
        view: View,
    ) -> impl Future<Output = Result<Self::Authorized, Error<Self::Rejection>>> + Send;

    fn read(
        &self,
        client: &Client,
        authorized: Self::Authorized,
        view: View,
    ) -> impl Future<Output = Result<Self::Output, Error<Self::Rejection>>> + Send;
}

/// The runtime context, admission, registry and work every command and query shares.
#[derive(Clone)]
pub(crate) struct App {
    context: Context,
    registry: Arc<Registry>,
    work: Arc<Work>,
    data: Arc<crate::kernel::keys::Keys>,
    seals: Arc<crate::keys::seal::Keys>,
    anchors: Anchors,
}

/// An attempt either reached a known result or wrote a pending candidate.
enum Step<T> {
    Known(T),
    Pending(T),
    Preview,
}

struct Attempt {
    dry_run: bool,
    request: Option<Request>,
    wrote: bool,
    pending: Option<Pending>,
}

impl App {
    pub(crate) fn new(
        context: Context,
        registry: Arc<Registry>,
        work: Arc<Work>,
        data: Arc<crate::kernel::keys::Keys>,
        seals: Arc<crate::keys::seal::Keys>,
    ) -> Self {
        let anchors = Anchors::new(context.admission());
        Self {
            context,
            registry,
            work,
            data,
            seals,
            anchors,
        }
    }

    pub(crate) fn context(&self) -> &Context {
        &self.context
    }

    pub(crate) fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    pub(crate) fn work(&self) -> &Arc<Work> {
        &self.work
    }

    pub(crate) fn keys(&self) -> history::Keys<'_> {
        history::Keys {
            data: &self.data,
            seals: &self.seals,
        }
    }

    pub(crate) fn anchors(&self) -> &Anchors {
        &self.anchors
    }

    #[cfg(test)]
    pub(crate) fn close(&self) {
        self.registry.close();
    }

    #[cfg(test)]
    pub(crate) async fn drain(&self) -> Result<(), crate::runtime::work::Error> {
        self.work.drain(&self.registry).await
    }

    /// Runs one command to its known result. Cancelling the wait never cancels accepted work.
    pub(crate) async fn command<C: Command>(
        &self,
        operation_id: operation::Id,
        command: C,
    ) -> Result<C::Output, Error<C::Rejection>> {
        self.registry.ready()?;
        let app = self.clone();
        self.work
            .run(
                Arc::clone(&self.registry),
                registry::Family::Kernel(C::COMPLETIONS.family()),
                operation_id,
                async move {
                    app.execute(&command, operation_id, false)
                        .await?
                        .ok_or_else(|| Error::corrupt("an applied command returns its result"))
                },
            )
            .await
    }

    /// Returns an existing completion, or previews a new decision and rolls back before apply.
    pub(crate) async fn dry_run<C: Command>(
        &self,
        operation_id: operation::Id,
        command: C,
    ) -> Result<Option<C::Output>, Error<C::Rejection>> {
        self.registry.ready()?;
        let app = self.clone();
        self.work
            .run(
                Arc::clone(&self.registry),
                registry::Family::Kernel(C::COMPLETIONS.family()),
                operation_id,
                async move { app.execute(&command, operation_id, true).await },
            )
            .await
    }

    /// Runs one query in a read-only snapshot within the shared read budget.
    pub(crate) async fn query<Q: Query>(
        &self,
        query: &Q,
    ) -> Result<Q::Output, Error<Q::Rejection>> {
        self.registry.ready()?;
        let lease = self
            .context
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, &self.anchors, |view| async move {
                    let authorized = query.authorize(client, view).await.map_err(read)?;
                    query.read(client, authorized, view).await.map_err(read)
                })
                .await
        })
        .await
        .unwrap_or_else(Error::elapsed);
        if result.as_ref().is_err_and(Error::is_corrupt) {
            self.registry.close();
        }
        match result {
            Ok(value) => match end_read(lease, deadline, "COMMIT").await {
                Ok(()) => Ok(value),
                Err(fault) if Q::RETAIN_ON_END_FAILURE => {
                    tracing::error!(stage = "read_commit", outcome = "known", error = ?fault);
                    Ok(value)
                }
                Err(fault) => Err(Error::Unavailable(fault)),
            },
            Err(error) if error.is_rejection() => {
                if let Err(fault) = end_read(lease, deadline, "ROLLBACK").await {
                    rejected(&error, &fault);
                }
                Err(error)
            }
            Err(Error::Unavailable(fault)) | Err(Error::Unknown(fault)) => {
                Err(Error::Unavailable(discarded(fault, lease).await))
            }
            Err(Error::Corrupt(fault)) => Err(Error::Corrupt(discarded(fault, lease).await)),
            Err(error) => {
                let _ = lease.discard().await;
                Err(error)
            }
        }
    }

    async fn execute<C: Command>(
        &self,
        command: &C,
        operation_id: operation::Id,
        dry_run: bool,
    ) -> Result<Option<C::Output>, Error<C::Rejection>> {
        let deadline = Instant::now() + WRITE;
        let result = self
            .execute_once(command, operation_id, dry_run, deadline)
            .await;
        if result.as_ref().is_err_and(Error::changed_locks) {
            self.execute_once(command, operation_id, dry_run, deadline)
                .await
        } else {
            result
        }
    }

    async fn execute_once<C: Command>(
        &self,
        command: &C,
        operation_id: operation::Id,
        dry_run: bool,
        deadline: Instant,
    ) -> Result<Option<C::Output>, Error<C::Rejection>> {
        let lease = self
            .context
            .connections()
            .lease()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            &self.context,
            Connection::Online(lease),
            deadline,
            C::COMPLETIONS.family(),
            operation_id,
        )
        .await;
        let mut attempt = Attempt {
            dry_run,
            request: None,
            wrote: false,
            pending: None,
        };
        let result = match begun {
            Ok(()) => timeout_at(
                tx.deadline(),
                self.attempt(&tx, command, operation_id, &mut attempt),
            )
            .await
            .unwrap_or_else(Error::elapsed),
            Err(fault) => Err(Error::Unavailable(fault)),
        };
        let (cause, corrupt) = match result {
            Ok(Step::Preview) => {
                tx.rollback().await.map_err(Error::Unavailable)?;
                return Ok(None);
            }
            Ok(Step::Known(output)) => {
                let end = if dry_run {
                    tx.rollback().await
                } else {
                    tx.commit().await
                };
                if let Err(fault) = end {
                    tracing::error!(%operation_id, stage = "completion_replay", outcome = "known", error = ?fault);
                }
                return Ok(Some(output));
            }
            Ok(Step::Pending(output)) => match tx.commit().await {
                Ok(()) => {
                    if let Some(pending) = attempt.pending {
                        pending.committed()?;
                    }
                    return Ok(Some(output));
                }
                Err(fault) => (fault, false),
            },
            Err(error) if error.changed_locks() && !attempt.wrote => {
                // Retry only after the first transaction has certainly ended.
                tx.rollback().await.map_err(Error::Unavailable)?;
                return Err(error);
            }
            Err(error) if error.is_rejection() => {
                match tx.rollback().await {
                    Ok(()) => {
                        if let Some(pending) = attempt.pending {
                            pending.rolled_back()?;
                        }
                    }
                    Err(fault) => {
                        if let Some(pending) = attempt.pending {
                            pending.uncertain();
                        }
                        rejected(&error, &fault);
                    }
                }
                return Err(error);
            }
            Err(Error::Corrupt(fault)) => (fault.then_result(tx.discard().await), true),
            Err(Error::Unavailable(fault)) | Err(Error::Unknown(fault)) => {
                (fault.then_result(tx.discard().await), false)
            }
            Err(error) => return Err(error),
        };
        if corrupt {
            self.registry.close();
        }
        if !attempt.wrote {
            return Err(classified(cause, corrupt));
        }
        let Some(request) = attempt.request.as_ref() else {
            self.registry.close();
            return Err(Error::Corrupt(cause.then_error(
                Error::<C::Rejection>::corrupt("an attempted write retains its accepted request"),
            )));
        };
        match self.recover(command, operation_id, request).await {
            Ok((Some(output), end)) => {
                if let Some(pending) = attempt.pending {
                    pending.committed()?;
                }
                // A lost receipt alone is an expected recovery; damage or a failed end is not.
                if corrupt || end.is_err() {
                    tracing::error!(%operation_id, stage = "completion_recovery", outcome = "committed", error = ?cause.then_result(end));
                }
                Ok(Some(output))
            }
            Ok((None, end)) => {
                if let Some(pending) = attempt.pending {
                    pending.rolled_back()?;
                }
                Err(classified(cause.then_result(end), corrupt))
            }
            Err(recovery) => {
                if let Some(pending) = attempt.pending {
                    pending.uncertain();
                }
                if recovery.is_corrupt() {
                    self.registry.close();
                }
                Err(Error::Unknown(cause.then_error(recovery)))
            }
        }
    }

    async fn attempt<C: Command>(
        &self,
        tx: &Tx,
        command: &C,
        operation_id: operation::Id,
        attempt: &mut Attempt,
    ) -> Result<Step<C::Output>, Error<C::Rejection>> {
        if C::ORDER == Order::OriginalFirst {
            match completion::original(tx.client(), self.keys(), C::COMPLETIONS, operation_id)
                .await
                .map_err(original)?
            {
                Some(record) => {
                    let now = tx.now(&self.context).await?;
                    record.check_time(now).map_err(original)?;
                    // Canonical input is rebuilt with the authenticated original attribution.
                    // Neither a changed mobile nor a removed current account erases completion.
                    let request = command.request(&record.authority(now))?;
                    record.matches(C::COMPLETIONS, self.keys(), &request)?;
                    return Ok(Step::Known(
                        command.replay(tx, &request, now).await.map_err(replay)?,
                    ));
                }
                None => command.absent(tx).await.map_err(original)?,
            }
        }
        command.locks(tx).await.map_err(lock)?;
        let subject = match command.session() {
            Some((current, actor)) => {
                let now = tx.now(&self.context).await.map_err(authorize)?;
                Some(
                    current
                        .resolve(tx.client(), actor, now)
                        .await
                        .map_err(Error::from)
                        .map_err(authorize)?,
                )
            }
            None => None,
        };
        let authority = command.authorize(tx, subject).await.map_err(authorize)?;
        let request = command.request(&authority)?;
        attempt.request = Some(request.clone());
        if C::ORDER == Order::AuthorizationFirst {
            if let Some(record) = completion::find(
                tx.client(),
                self.keys(),
                C::COMPLETIONS,
                operation_id,
                &request,
            )
            .await
            .map_err(original)?
            {
                record.check_time(authority.now).map_err(original)?;
                return Ok(Step::Known(
                    command
                        .replay(tx, &request, authority.now)
                        .await
                        .map_err(replay)?,
                ));
            }
            command.absent(tx).await.map_err(original)?;
        }
        let facts = command.load(tx, &authority).await.map_err(load)?;
        let change = command.decide(facts, authority.now)?;
        if attempt.dry_run {
            return Ok(Step::Preview);
        }
        attempt.wrote = true;
        let expires_at = command.expires_at(&change, authority.now);
        // The attempt owns partial anchor updates even if a later write fails or times out.
        let output = command
            .apply(tx, change, &mut attempt.pending)
            .await
            .map_err(write)?;
        completion::insert(
            tx.client(),
            self.keys(),
            C::COMPLETIONS,
            operation_id,
            &request,
            authority.now,
            expires_at,
        )
        .await
        .map_err(write)?;
        Ok(Step::Pending(output))
    }

    /// The original work's subject stays bound; recovery neither re-authorizes nor writes.
    /// `None` is confirmed absence after the original transaction ended.
    async fn recover<C: Command>(
        &self,
        command: &C,
        operation_id: operation::Id,
        request: &Request,
    ) -> Result<(Option<C::Output>, Result<(), Fault>), Error<C::Rejection>> {
        let session = self
            .context
            .connections()
            .recover()
            .await
            .map_err(Error::connection)?;
        let (tx, begun) = Tx::begin(
            &self.context,
            Connection::Recovery(session),
            Instant::now() + WRITE,
            C::COMPLETIONS.family(),
            operation_id,
        )
        .await;
        let result = match begun {
            Ok(()) => timeout_at(tx.deadline(), async {
                match completion::find(
                    tx.client(),
                    self.keys(),
                    C::COMPLETIONS,
                    operation_id,
                    request,
                )
                .await
                {
                    Ok(Some(record)) => {
                        let now = tx.now(&self.context).await?;
                        record.check_time(now)?;
                        command.replay(&tx, request, now).await.map(Some)
                    }
                    Ok(None) => {
                        // The attempted write must end with either its own completion or a
                        // confirmed absence. A different accepted source is a contradiction.
                        match command.absent(&tx).await {
                            Ok(()) => Ok(None),
                            Err(Error::Mismatch) => Err(Error::corrupt(
                                "a recovery miss contradicts retained original facts",
                            )),
                            Err(error) => Err(error),
                        }
                    }
                    Err(Error::Mismatch) => Err(Error::corrupt(
                        "a recovery completion retains the original request",
                    )),
                    Err(error) => Err(error),
                }
            })
            .await
            .unwrap_or_else(Error::elapsed),
            Err(fault) => Err(Error::Unavailable(fault)),
        };
        match result {
            Ok(option) => {
                let end = tx.commit().await;
                Ok((option, end))
            }
            Err(error) => {
                let error = error.at(Stage::Recovery);
                Err(match tx.discard().await {
                    Ok(()) => error,
                    Err(termination) => match error {
                        Error::Unavailable(fault) => Error::Unavailable(fault.then(termination)),
                        Error::Unknown(fault) => Error::Unknown(fault.then(termination)),
                        Error::Corrupt(fault) => Error::Corrupt(fault.then(termination)),
                        error => error,
                    },
                })
            }
        }
    }
}

/// A determined rejection stays the result when its rollback fails: COMMIT was never sent,
/// and the failed connection is discarded and its driver awaited.
/// The rejection and the rollback failure are reported together, once.
fn rejected<R: Rejection>(error: &Error<R>, fault: &Fault) {
    tracing::error!(
        stage = "rejection_rollback",
        rejection = error.code(),
        error = ?fault,
        "rejection kept after its rollback failed",
    );
}

fn classified<R>(fault: Fault, corrupt: bool) -> Error<R> {
    if corrupt {
        Error::Corrupt(fault)
    } else {
        Error::Unavailable(fault)
    }
}

async fn end_read(lease: Lease, deadline: Instant, command: &str) -> Result<(), Fault> {
    let end = match timeout_at(deadline, lease.client().batch_execute(command)).await {
        Ok(Ok(())) => {
            lease.release();
            return Ok(());
        }
        Ok(Err(error)) => FaultKind::Query(crate::database::Query(error)),
        Err(elapsed) => FaultKind::Timeout(elapsed),
    };
    Err(discarded(Fault::new(Stage::Read, end), lease).await)
}

async fn discarded(fault: Fault, lease: Lease) -> Fault {
    match lease.discard().await {
        Ok(()) => fault,
        Err(error) => fault.then(Fault::new(Stage::Termination, FaultKind::Connection(error))),
    }
}

fn lock<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Lock)
}

fn authorize<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Authorize)
}

fn original<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Completion)
}

fn replay<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Replay)
}

fn load<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Load)
}

fn write<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Write)
}

fn read<R>(error: Error<R>) -> Error<R> {
    error.at(Stage::Read)
}

#[cfg(test)]
pub(super) mod tests {
    //! Test-only toy command and the executor behaviour it exercises against real PostgreSQL:
    //! replay, request_mismatch, lost commit receipt, late commit, confirmed rollback, recovery
    //! failure, slot exhaustion, closed admission, waiter cancellation, drain, and a rejection whose
    //! rollback fails.

    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use fixture::database_proxy::{Fault, Proxy};
    use platform::database::Connections;
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::Client;
    use uuid::Uuid;

    use crate::{
        database::Barrier,
        history::View,
        identity::{
            authentication::{self, Database, server},
            operation,
        },
        kernel::{
            App, Authorization, Command, Error, FaultKind, Query, Rejection, Stage, Status,
            completion::{Request, Retention, Template},
            operation::Family,
            tx::{self, Tx},
        },
        runtime::{Context, registry::Registry, work::Work},
    };

    #[derive(Debug, PartialEq, Eq)]
    pub(in crate::kernel) struct Taken;

    impl Rejection for Taken {
        fn status(&self) -> Status {
            Status::Conflict
        }

        fn code(&self) -> &'static str {
            "toy_taken"
        }
    }

    /// Claims one unique toy name.
    pub(in crate::kernel) struct Toy {
        context: Context,
        name: &'static str,
        denied: bool,
    }

    pub(in crate::kernel) const COMPLETIONS: Template =
        Template::new(Family::Toy, |action, target| {
            action == "create" && target == "toys"
        });

    impl Command for Toy {
        fn session(
            &self,
        ) -> Option<(
            &crate::identity::current::Service,
            &crate::identity::current::Actor,
        )> {
            None
        }

        const COMPLETIONS: Template = COMPLETIONS;
        type Facts = bool;
        type Change = ();
        type Output = String;
        type Rejection = Taken;

        fn request(&self, authority: &Authorization) -> Result<Request, Error<Taken>> {
            Request::new("create", "toys".to_owned(), &self.name, authority)
        }

        async fn locks(&self, tx: &Tx) -> Result<(), Error<Taken>> {
            Ok(tx::object(tx.client(), b"testing/toy", self.name.as_bytes()).await?)
        }

        async fn authorize(
            &self,
            tx: &Tx,
            _: Option<crate::identity::current::database::State>,
        ) -> Result<Authorization, Error<Taken>> {
            if self.denied {
                return Err(Error::Denied);
            }
            Ok(Authorization {
                person_id: crate::identity::person::Id::allocate(),
                revision_id: crate::identity::person::revision::Id::allocate(),
                now: tx.now(&self.context).await?,
            })
        }

        async fn load(&self, tx: &Tx, _: &Authorization) -> Result<bool, Error<Taken>> {
            tx.client()
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM testing.toys WHERE name = $1)",
                    &[&self.name],
                )
                .await
                .map_err(Error::query)?
                .try_get(0)
                .map_err(Error::decode)
        }

        fn decide(&self, taken: bool, _: DateTime<Utc>) -> Result<(), Error<Taken>> {
            if taken {
                return Err(Error::Rejected(Taken));
            }
            Ok(())
        }

        async fn apply(
            &self,
            tx: &Tx,
            (): (),
            _: &mut Option<crate::history::Pending>,
        ) -> Result<String, Error<Taken>> {
            let output: String = tx
                .client()
                .query_one(
                    "INSERT INTO testing.toys (name, operation_id) VALUES ($1, $2) RETURNING name",
                    &[&self.name, &tx.operation_id()],
                )
                .await
                .map_err(Error::query)?
                .try_get(0)
                .map_err(Error::decode)?;
            Ok(output)
        }

        async fn replay(
            &self,
            tx: &Tx,
            _: &Request,
            _: DateTime<Utc>,
        ) -> Result<String, Error<Taken>> {
            tx.client()
                .query_opt(
                    "SELECT name FROM testing.toys WHERE operation_id = $1",
                    &[&tx.operation_id()],
                )
                .await
                .map_err(Error::query)?
                .ok_or(Error::corrupt("an original toy completion retains its toy"))?
                .try_get(0)
                .map_err(Error::decode)
        }
    }

    /// Counts toys in the read snapshot.
    struct Count;

    impl Query for Count {
        type Authorized = ();
        type Output = i64;
        type Rejection = Taken;

        async fn authorize(&self, _: &Client, _: View) -> Result<(), Error<Taken>> {
            Ok(())
        }

        async fn read(
            &self,
            client: &Client,
            (): (),
            _: crate::history::View,
        ) -> Result<i64, Error<Taken>> {
            client
                .query_one("SELECT count(*) FROM testing.toys", &[])
                .await
                .map_err(Error::query)?
                .try_get(0)
                .map_err(Error::decode)
        }
    }

    #[test]
    fn identity_snapshot_changes_retry_authorization_and_the_complete_read() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::identity::{account, current, suspension};

        struct Retry {
            owner: u8,
            authorizations: AtomicUsize,
            reads: AtomicUsize,
        }

        impl Query for Retry {
            type Authorized = ();
            type Output = i64;
            type Rejection = Taken;

            async fn authorize(&self, _: &Client, _: View) -> Result<(), Error<Taken>> {
                self.authorizations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }

            async fn read(&self, client: &Client, (): (), view: View) -> Result<i64, Error<Taken>> {
                if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    let changed = crate::history::Error::Changed;
                    return Err(match self.owner {
                        0 => current::Error::History(changed).into(),
                        1 => account::ReadError::History(changed).into(),
                        _ => suspension::Error::History(changed).into(),
                    });
                }
                Count.read(client, (), view).await
            }
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let app = start(&database, database.connections(&database.user), 4).await;
            for owner in 0..3 {
                let query = Retry {
                    owner,
                    authorizations: AtomicUsize::new(0),
                    reads: AtomicUsize::new(0),
                };
                assert_eq!(app.query(&query).await.expect("fresh complete snapshot"), 0);
                assert_eq!(query.authorizations.load(Ordering::SeqCst), 2);
                assert_eq!(query.reads.load(Ordering::SeqCst), 2);
                app.registry()
                    .ready()
                    .expect("snapshot race leaves admission open");
            }
            stop(app).await;
        });
    }

    pub(in crate::kernel) fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    /// A fenced runtime context and an executor with `capacity` work slots, plus the toy tables
    /// built from the completion template.
    pub(in crate::kernel) async fn start(
        database: &Database,
        connections: Connections,
        capacity: usize,
    ) -> App {
        let context = Context::new(connections).await.expect("runtime context");
        let barrier = Barrier::begin(context.clone())
            .await
            .expect("startup barrier");
        barrier.confirm().await.expect("barrier confirmed");
        barrier.terminate().await.expect("barrier ended");
        let owner = database
            .connections("postgres")
            .recover()
            .await
            .expect("isolated owner");
        owner.client().batch_execute(&format!(r#"CREATE SCHEMA testing; CREATE TABLE testing.completions (LIKE workforce.completions INCLUDING DEFAULTS INCLUDING CONSTRAINTS INCLUDING INDEXES); ALTER TABLE testing.completions DROP CONSTRAINT completions_family_check, DROP CONSTRAINT completions_action_check, DROP CONSTRAINT completions_retention_check, ADD CHECK (family='toy' AND action='create' AND expires_at IS NOT NULL AND expires_at=created_at+interval '24 hours'); CREATE TABLE testing.toys (name text PRIMARY KEY, operation_id uuid NOT NULL UNIQUE); GRANT USAGE ON SCHEMA testing TO "{user}"; GRANT SELECT, INSERT ON testing.completions, testing.toys TO "{user}""#, user = database.user)).await.expect("toy tables from the completion template");
        owner.terminate().await.expect("owner ended");
        let keys = authentication::inventory(authentication::keys());
        App::new(
            context.clone(),
            Arc::new(Registry::new(capacity, context.admission())),
            Arc::new(Work::default()),
            Arc::new(keys.encryption),
            Arc::new(keys.seal),
        )
    }

    pub(in crate::kernel) fn toy(app: &App, name: &'static str) -> Toy {
        Toy {
            context: app.context().clone(),
            name,
            denied: false,
        }
    }

    pub(in crate::kernel) async fn stop(app: App) {
        app.close();
        app.drain().await.expect("drained");
        app.context().stop().await.expect("context stopped");
    }

    pub(in crate::kernel) async fn counts(
        database: &Database,
        operation_id: operation::Id,
    ) -> (i64, i64) {
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("independent observer");
        let row = observer
            .client()
            .query_one(
                "SELECT (SELECT count(*) FROM testing.toys WHERE operation_id = $1), (SELECT count(*) FROM testing.completions WHERE operation_id = $1)",
                &[&operation_id],
            )
            .await
            .expect("independent facts");
        observer.terminate().await.expect("observer ended");
        (row.get(0), row.get(1))
    }

    struct ChangingLocks {
        inner: Toy,
        changes: usize,
        calls: Arc<std::sync::Mutex<Vec<&'static str>>>,
        transactions: Arc<std::sync::Mutex<Vec<(String, tokio::time::Instant)>>>,
    }

    impl Command for ChangingLocks {
        const COMPLETIONS: Template = COMPLETIONS;
        type Facts = bool;
        type Change = ();
        type Output = String;
        type Rejection = Taken;

        fn session(
            &self,
        ) -> Option<(
            &crate::identity::current::Service,
            &crate::identity::current::Actor,
        )> {
            None
        }

        fn request(&self, authority: &Authorization) -> Result<Request, Error<Taken>> {
            self.inner.request(authority)
        }

        async fn locks(&self, tx: &Tx) -> Result<(), Error<Taken>> {
            self.calls.lock().expect("call trace").push("locks");
            // Each attempt must reacquire the same exclusive transaction lock.
            self.inner.locks(tx).await?;
            let transaction: String = tx
                .client()
                .query_one("SELECT pg_current_xact_id()::text", &[])
                .await
                .map_err(Error::query)?
                .try_get(0)
                .map_err(Error::decode)?;
            let attempt = {
                let mut transactions = self.transactions.lock().expect("transaction trace");
                transactions.push((transaction, tx.deadline()));
                transactions.len()
            };
            if attempt <= self.changes {
                return Err(Error::lock_set_changed());
            }
            Ok(())
        }

        async fn authorize(
            &self,
            tx: &Tx,
            subject: Option<crate::identity::current::database::State>,
        ) -> Result<Authorization, Error<Taken>> {
            self.calls.lock().expect("call trace").push("authorize");
            self.inner.authorize(tx, subject).await
        }

        async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<bool, Error<Taken>> {
            self.calls.lock().expect("call trace").push("load");
            self.inner.load(tx, authority).await
        }

        fn decide(&self, facts: bool, now: DateTime<Utc>) -> Result<(), Error<Taken>> {
            self.calls.lock().expect("call trace").push("decide");
            self.inner.decide(facts, now)
        }

        async fn apply(
            &self,
            tx: &Tx,
            change: (),
            pending: &mut Option<crate::history::Pending>,
        ) -> Result<String, Error<Taken>> {
            self.calls.lock().expect("call trace").push("apply");
            self.inner.apply(tx, change, pending).await
        }

        async fn replay(
            &self,
            tx: &Tx,
            request: &Request,
            now: DateTime<Utc>,
        ) -> Result<String, Error<Taken>> {
            self.calls.lock().expect("call trace").push("replay");
            self.inner.replay(tx, request, now).await
        }
    }

    #[test]
    fn changed_lock_sets_retry_once_in_a_new_transaction_with_the_original_deadline() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for changes in [1, usize::MAX] {
                timeout(Duration::from_secs(20), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let app = start(&database, database.connections(&database.user), 4).await;
                    let original = operation();
                    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                    let transactions = Arc::new(std::sync::Mutex::new(Vec::new()));
                    let command = ChangingLocks {
                        inner: toy(&app, "alpha"),
                        changes,
                        calls: Arc::clone(&calls),
                        transactions: Arc::clone(&transactions),
                    };
                    let result = app.command(original, command).await;
                    {
                        let transactions = transactions.lock().expect("transaction trace");
                        assert_eq!(transactions.len(), 2, "at most one complete retry");
                        assert_ne!(
                            transactions[0].0, transactions[1].0,
                            "retry ends the old transaction and reacquires its lock"
                        );
                        assert_eq!(
                            transactions[0].1, transactions[1].1,
                            "retry does not renew the write budget"
                        );
                    }
                    if changes == 1 {
                        assert_eq!(result.expect("new lock set succeeds"), "alpha");
                        assert_eq!(
                            *calls.lock().expect("call trace"),
                            ["locks", "locks", "authorize", "load", "decide", "apply"]
                        );
                        assert_eq!(counts(&database, original).await, (1, 1));
                    } else {
                        assert!(
                            result
                                .expect_err("continued changes exhaust the retry")
                                .changed_locks()
                        );
                        assert_eq!(*calls.lock().expect("call trace"), ["locks", "locks"]);
                        assert_eq!(counts(&database, original).await, (0, 0));
                    }
                    app.registry()
                        .ready()
                        .expect("lock races leave admission open");
                    stop(app).await;
                })
                .await
                .expect("bounded lock-set retry scenario");
            }
        });
    }

    #[test]
    fn changed_lock_sets_do_not_retry_without_a_confirmed_rollback() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                let transactions = Arc::new(std::sync::Mutex::new(Vec::new()));
                let command = ChangingLocks {
                    inner: toy(&app, "alpha"),
                    changes: 1,
                    calls: Arc::clone(&calls),
                    transactions: Arc::clone(&transactions),
                };
                proxy.arm(Fault::RejectedRollback);
                let result = app.command(original, command).await;
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("actual rollback connection failure");
                let Err(Error::Unavailable(fault)) = result else {
                    panic!("rollback failure remains a dependency failure: {result:?}");
                };
                assert_eq!(fault.stage(), Stage::Rollback);
                assert!(
                    matches!(fault.kind(), FaultKind::Query(_)),
                    "rollback cause is retained: {fault:?}"
                );
                assert_eq!(*calls.lock().expect("call trace"), ["locks"]);
                assert_eq!(transactions.lock().expect("transaction trace").len(), 1);
                assert_eq!(counts(&database, original).await, (0, 0));
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("bounded failed rollback scenario");
        });
    }

    #[test]
    fn recovery_confirms_absence_before_consulting_an_unavailable_clock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::new(server).await;
                let app = start(&database, database.connections(&database.user), 2).await;
                let command = toy(&app, "alpha");
                let original = operation();
                let request = command
                    .request(&Authorization {
                        person_id: crate::identity::person::Id::allocate(),
                        revision_id: crate::identity::person::revision::Id::allocate(),
                        now: app
                            .context()
                            .sample()
                            .await
                            .expect("original acceptance time"),
                    })
                    .expect("accepted original input");
                let owner = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("isolated owner");
                let clock_pid: i32 = app
                    .context()
                    .clock_session()
                    .await
                    .client()
                    .query_one("SELECT pg_backend_pid()", &[])
                    .await
                    .expect("bound clock backend")
                    .get(0);
                assert!(
                    owner
                        .client()
                        .query_one("SELECT pg_terminate_backend($1)", &[&clock_pid])
                        .await
                        .expect("fail the exact clock connection")
                        .get::<_, bool>(0)
                );
                assert!(
                    app.context().heartbeat().await.is_err(),
                    "the clock is actually unavailable"
                );
                let (result, end) = app.recover(&command, original, &request).await.expect(
                    "absence is determined under the original lock without a time-dependent record",
                );
                assert!(result.is_none());
                end.expect("recovery transaction ended");
                assert_eq!(counts(&database, original).await, (0, 0));
                app.close();
                app.drain().await.expect("work drained");
                let _ = app.context().stop().await;
                owner.terminate().await.expect("observer ended");
            })
            .await
            .expect("recovery absence budget");
        });
    }

    #[test]
    fn queries_keep_the_snapshot_result_without_post_read_time_confirmation() {
        #[derive(Clone, Copy, Debug)]
        enum Outcome {
            Value,
            Denied,
            Unavailable,
        }
        struct Paused {
            context: Context,
            entered: Arc<tokio::sync::Barrier>,
            resumed: Arc<tokio::sync::Barrier>,
            outcome: Outcome,
        }
        impl Query for Paused {
            type Authorized = ();
            type Output = i64;
            type Rejection = Taken;
            async fn authorize(&self, client: &Client, _: View) -> Result<(), Error<Taken>> {
                self.context.now(client).await.map_err(Error::clock)?;
                Ok(())
            }
            async fn read(
                &self,
                client: &Client,
                (): (),
                view: crate::history::View,
            ) -> Result<i64, Error<Taken>> {
                self.entered.wait().await;
                self.resumed.wait().await;
                match self.outcome {
                    Outcome::Value => Count.read(client, (), view).await,
                    Outcome::Denied => Err(Error::Denied),
                    Outcome::Unavailable => client
                        .query_one("SELECT fictional_missing_column FROM testing.toys", &[])
                        .await
                        .map_err(Error::query)?
                        .try_get(0)
                        .map_err(Error::decode),
                }
            }
        }
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (outcome, during_end) in [
                (Outcome::Value, false),
                (Outcome::Denied, false),
                (Outcome::Unavailable, false),
                (Outcome::Value, true),
                (Outcome::Denied, true),
            ] {
                timeout(Duration::from_secs(30), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                    let instance = app.context().clock_session().await;
                    let before = proxy.time_queries();
                    let entered = Arc::new(tokio::sync::Barrier::new(2));
                    let resumed = Arc::new(tokio::sync::Barrier::new(2));
                    let task = tokio::spawn({
                        let app = app.clone();
                        let query = Paused {
                            context: app.context().clone(),
                            entered: Arc::clone(&entered),
                            resumed: Arc::clone(&resumed),
                            outcome,
                        };
                        async move { app.query(&query).await }
                    });
                    timeout(Duration::from_secs(5), entered.wait())
                        .await
                        .expect("authorization finished and read reached its gate");
                    if during_end {
                        proxy.arm(Fault::PausedReadEnd);
                        resumed.wait().await;
                        timeout(Duration::from_secs(5), proxy.intercepted())
                            .await
                            .expect("snapshot ended while its receipt is held");
                    }
                    if during_end {
                        proxy.release();
                    } else {
                        resumed.wait().await;
                    }
                    let result = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("query ends without the instance session")
                        .expect("query task");
                    match (outcome, result) {
                        (Outcome::Value, Ok(0)) | (Outcome::Denied, Err(Error::Denied)) => {}
                        (Outcome::Unavailable, Err(Error::Unavailable(fault))) => {
                            assert!(matches!(fault.kind(), FaultKind::Query(_)), "{fault:?}");
                            assert!(
                                !fault
                                    .later()
                                    .any(|(_, kind)| matches!(kind, FaultKind::Clock(_)))
                            );
                        }
                        (outcome, result) => {
                            panic!("{outcome:?}, during_end={during_end}: {result:?}")
                        }
                    }
                    assert_eq!(proxy.time_queries() - before, 1);
                    assert!(app.registry().ready().is_ok());
                    drop(instance);
                    stop(app).await;
                    proxy.stop().await;
                })
                .await
                .expect("query final confirmation budget");
            }
        });
    }

    #[test]
    fn retention_closes_admission_on_rewound_discovery_and_reclaim_time() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for phase in ["discovery", "reclaim"] {
                timeout(Duration::from_secs(30), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                    let original = operation();
                    let owner = database.connections("postgres").recover().await.expect("fixture owner");
                    let gate = if phase == "reclaim" {
                        app.command(original, toy(&app, "alpha")).await.expect("completion to expire");
                        crate::kernel::completion::Seed::age(owner.client(), app.keys(), COMPLETIONS, original, chrono::Duration::hours(25)).await.expect("expired completion fixture");
                        owner.client().batch_execute(&format!(
                            "GRANT DELETE ON testing.completions TO \"{}\"", database.user
                        )).await.expect("completion retention grant");
                        let gate = database.connections(&database.user).recover().await.expect("operation gate");
                        gate.client().batch_execute("BEGIN").await.expect("gate transaction");
                        tx::object(gate.client(), Family::Toy.lock(), original.as_bytes()).await.expect("hold original operation");
                        Some(gate)
                    } else {
                        proxy.arm(Fault::RewoundCurrent);
                        None
                    };
                    let task = tokio::spawn({
                        let mut retention = Retention::new(app.clone(), COMPLETIONS);
                        async move { retention.run().await }
                    });
                    if let Some(gate) = gate {
                        let gate_pid: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate backend").get(0);
                        timeout(Duration::from_secs(5), async {
                            while !owner.client().query_one(
                                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&gate_pid]
                            ).await.expect("reclaim waits after discovery").get::<_, bool>(0) {
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("reclaim reached its operation lock");
                        proxy.arm(Fault::RewoundCurrent);
                        gate.client().batch_execute("ROLLBACK").await.expect("release reclaim");
                        gate.terminate().await.expect("gate ended");
                    }
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual transaction time rewind");
                    proxy.release();
                    let result = timeout(Duration::from_secs(5), task).await.expect("fatal failure is delivered in this pass").expect("retention task");
                    let Err(Error::Unavailable(fault)) = result else {
                        panic!("{phase}: clock failure must reach the supervisor: {result:?}");
                    };
                    assert!(matches!(fault.kind(), FaultKind::Clock(_)), "{phase}: {fault:?}");
                    assert!(app.registry().ready().is_err(), "{phase}: admission is closed");
                    if phase == "reclaim" {
                        assert_eq!(counts(&database, original).await, (1, 1), "clock failure retains the candidate");
                    }
                    owner.terminate().await.expect("owner ended");
                    stop(app).await;
                    proxy.stop().await;
                }).await.expect("retention clock failure budget");
            }
        });
    }

    #[test]
    fn absent_recovery_preserves_commit_and_termination_failures() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for lost_commit in [true, false] {
                timeout(Duration::from_secs(30), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                    let original = operation();
                    proxy.arm(Fault::DelayedRollback);
                    let task = tokio::spawn({
                        let app = app.clone();
                        let command = toy(&app, "alpha");
                        async move { app.command(original, command).await }
                    });
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original transaction will roll back");
                    proxy.arm(if lost_commit { Fault::LostReadCommit } else { Fault::ReadCommitTermination });
                    proxy.release();
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual recovery finalization fault");
                    let result = timeout(Duration::from_secs(5), task).await.expect("known absence is returned promptly").expect("command task");
                    proxy.release();
                    let Err(Error::Unavailable(fault)) = result else {
                        panic!("confirmed absence retains the original failure: {result:?}");
                    };
                    assert_eq!(fault.stage(), Stage::Commit);
                    if lost_commit {
                        assert!(fault.later().any(|(stage, _)| stage == Stage::Commit), "recovery commit failure is retained: {fault:?}");
                    } else {
                        assert!(fault.later().any(|(stage, cause)| matches!(
                            (stage, cause),
                            (Stage::Termination, FaultKind::Connection(platform::database::Error::Postgres(source)))
                                if source.code() == Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)
                        )), "recovery driver failure is retained: {fault:?}");
                    }
                    assert_eq!(counts(&database, original).await, (0, 0));
                    app.registry().ready().expect("confirmed rollback leaves admission open");
                    assert_eq!(app.command(original, toy(&app, "alpha")).await.expect("explicit retry after confirmed absence"), "alpha");
                    assert_eq!(counts(&database, original).await, (1, 1));
                    stop(app).await;
                    proxy.stop().await;
                }).await.expect("absent recovery fault budget");
            }
        });
    }

    #[test]
    fn replay_returns_the_original_and_a_different_request_is_a_mismatch() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let app = start(&database, database.connections(&database.user), 4).await;
                let original = operation();
                assert_eq!(
                    app.command(original, toy(&app, "alpha"))
                        .await
                        .expect("original"),
                    "alpha"
                );
                assert_eq!(
                    app.command(original, toy(&app, "alpha"))
                        .await
                        .expect("replay"),
                    "alpha"
                );
                assert!(matches!(
                    app.command(original, toy(&app, "beta")).await,
                    Err(Error::Mismatch)
                ));
                assert!(matches!(
                    app.command(operation(), toy(&app, "alpha")).await,
                    Err(Error::Rejected(Taken))
                ));
                let mut denied = toy(&app, "gamma");
                denied.denied = true;
                assert!(matches!(
                    app.command(operation(), denied).await,
                    Err(Error::Denied)
                ));
                assert_eq!(counts(&database, original).await, (1, 1));
                assert_eq!(app.query(&Count).await.expect("snapshot read"), 1);
                stop(app).await;
            })
            .await
            .expect("bounded replay scenario");
        });
    }

    #[test]
    fn a_mismatched_recovery_retains_integrity_evidence_and_closes_admission() {
        struct Foreign(Toy);

        impl Command for Foreign {
            fn session(
                &self,
            ) -> Option<(
                &crate::identity::current::Service,
                &crate::identity::current::Actor,
            )> {
                None
            }

            const COMPLETIONS: Template = COMPLETIONS;
            type Facts = bool;
            type Change = ();
            type Output = String;
            type Rejection = Taken;

            fn request(&self, authority: &Authorization) -> Result<Request, Error<Taken>> {
                self.0.request(authority)
            }

            async fn locks(&self, tx: &Tx) -> Result<(), Error<Taken>> {
                self.0.locks(tx).await
            }

            async fn authorize(
                &self,
                tx: &Tx,
                subject: Option<crate::identity::current::database::State>,
            ) -> Result<Authorization, Error<Taken>> {
                self.0.authorize(tx, subject).await
            }

            async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<bool, Error<Taken>> {
                self.0.load(tx, authority).await
            }

            fn decide(&self, facts: bool, now: DateTime<Utc>) -> Result<(), Error<Taken>> {
                self.0.decide(facts, now)
            }

            async fn apply(
                &self,
                tx: &Tx,
                (): (),
                _: &mut Option<crate::history::Pending>,
            ) -> Result<String, Error<Taken>> {
                // Simulate an out-of-band writer that ignores original-operation ownership.
                let foreign = self
                    .0
                    .context
                    .connections()
                    .recover()
                    .await
                    .map_err(Error::connection)?;
                let inventory = authentication::inventory(authentication::keys());
                let keys = crate::history::Keys {
                    data: &inventory.encryption,
                    seals: &inventory.seal,
                };
                let authority = self.0.authorize(tx, None).await?;
                let request = Request::new("create", "toys".to_owned(), &"foreign", &authority)?;
                crate::kernel::completion::insert(
                    foreign.client(),
                    keys,
                    COMPLETIONS,
                    tx.operation_id(),
                    &request,
                    authority.now,
                    COMPLETIONS.expires(authority.now),
                )
                .await?;
                foreign.terminate().await.map_err(Error::connection)?;
                tx.client()
                    .query_one("SELECT missing_kernel_column", &[])
                    .await
                    .map_err(Error::query)?;
                Ok(String::new())
            }

            async fn replay(
                &self,
                tx: &Tx,
                request: &Request,
                now: DateTime<Utc>,
            ) -> Result<String, Error<Taken>> {
                self.0.replay(tx, request, now).await
            }
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let app = start(&database, database.connections(&database.user), 4).await;
                let original = operation();
                let result = app.command(original, Foreign(toy(&app, "alpha"))).await;
                let Err(Error::Unknown(fault)) = result else {
                    panic!(
                        "the damaged completion cannot establish the original outcome: {result:?}"
                    );
                };
                assert!(
                    matches!(fault.kind(), FaultKind::Query(_)),
                    "the failed write is retained: {fault:?}"
                );
                assert!(
                    fault.later().any(|(stage, kind)| matches!(
                        (stage, kind),
                        (Stage::Recovery, FaultKind::Integrity(_))
                    )),
                    "the inconsistent original request is retained: {fault:?}"
                );
                assert!(
                    app.registry().ready().is_err(),
                    "recovery integrity damage closes admission"
                );
                assert_eq!(
                    counts(&database, original).await,
                    (0, 1),
                    "the mismatched completion is preserved"
                );
                stop(app).await;
            })
            .await
            .expect("bounded mismatched recovery scenario");
        });
    }

    #[test]
    fn a_lost_commit_receipt_recovers_the_original_result() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                proxy.arm(Fault::LostCommit);
                assert_eq!(
                    app.command(original, toy(&app, "alpha"))
                        .await
                        .expect("recovered from the completion"),
                    "alpha"
                );
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("actual lost receipt");
                assert_eq!(counts(&database, original).await, (1, 1));
                assert_eq!(
                    app.command(original, toy(&app, "alpha"))
                        .await
                        .expect("replay"),
                    "alpha"
                );
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("bounded lost receipt scenario");
        });
    }

    #[test]
    fn recovery_waits_for_a_late_commit_under_the_original_operation_lock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                proxy.arm(Fault::DelayedCommit);
                let command = tokio::spawn({
                    let app = app.clone();
                    let toy = toy(&app, "alpha");
                    async move { app.command(original, toy).await }
                });
                timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("commit response lost");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                timeout(Duration::from_secs(5), async {
                    // Recovery queues behind the original backend's original-operation lock.
                    while !observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND NOT granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()))", &[]).await.expect("lock graph").get::<_, bool>(0) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("recovery waits for the original transaction");
                assert!(!command.is_finished(), "a late commit is not decided before it ends");
                proxy.release();
                assert_eq!(command.await.expect("command task").expect("late commit recovered"), "alpha");
                assert_eq!(counts(&database, original).await, (1, 1));
                observer.terminate().await.expect("observer ended");
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("bounded late commit scenario");
        });
    }

    #[test]
    fn a_confirmed_rollback_is_unavailable_and_the_operation_stays_usable() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                proxy.arm(Fault::RolledBack);
                assert!(matches!(
                    app.command(original, toy(&app, "alpha")).await,
                    Err(Error::Unavailable(_))
                ));
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("actual rollback");
                assert_eq!(counts(&database, original).await, (0, 0));
                app.registry()
                    .ready()
                    .expect("a confirmed rollback keeps admission open");
                assert_eq!(
                    app.command(original, toy(&app, "alpha"))
                        .await
                        .expect("new attempt"),
                    "alpha"
                );
                assert_eq!(counts(&database, original).await, (1, 1));
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("bounded rollback scenario");
        });
    }

    #[test]
    fn a_failed_recovery_leaves_the_outcome_unknown_until_a_later_replay() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                let original = operation();
                proxy.arm(Fault::UnknownCommit);
                let command = tokio::spawn({
                    let app = app.clone();
                    let toy = toy(&app, "alpha");
                    async move { app.command(original, toy).await }
                });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("commit outcome lost");
                proxy.release();
                let result = command.await.expect("command task");
                let Err(Error::Unknown(fault)) = result else {
                    panic!("recovery failure keeps the outcome unknown: {result:?}");
                };
                assert!(
                    fault
                        .later()
                        .any(|(stage, _)| stage == crate::kernel::Stage::Begin)
                );
                timeout(Duration::from_secs(5), async {
                    while counts(&database, original).await != (1, 1) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("the original transaction committed after release");
                assert_eq!(
                    app.command(original, toy(&app, "alpha"))
                        .await
                        .expect("later replay"),
                    "alpha"
                );
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("bounded unknown scenario");
        });
    }

    #[test]
    fn exhausted_slots_are_busy_closed_admission_is_unavailable_and_drain_outlives_waiters() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 1).await;
                let original = operation();
                proxy.arm(Fault::DelayedCommit);
                let waiter = tokio::spawn({
                    let app = app.clone();
                    let toy = toy(&app, "alpha");
                    async move { app.command(original, toy).await }
                });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("first work holds the only slot");
                assert!(matches!(
                    app.command(operation(), toy(&app, "beta")).await,
                    Err(Error::Busy)
                ));
                waiter.abort();
                assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
                app.close();
                let closed = app.command(operation(), toy(&app, "gamma")).await;
                assert!(
                    matches!(&closed, Err(Error::Unavailable(_)))
                        && closed.as_ref().is_err_and(Error::is_closed)
                );
                assert!(
                    app.query(&Count)
                        .await
                        .is_err_and(|error| error.is_closed())
                );
                assert!(
                    timeout(Duration::from_millis(50), app.drain())
                        .await
                        .is_err(),
                    "accepted work survives its cancelled waiter"
                );
                proxy.release();
                app.drain()
                    .await
                    .expect("drain waits for the original work");
                assert_eq!(counts(&database, original).await, (1, 1));
                app.context().stop().await.expect("context stopped");
                proxy.stop().await;
            })
            .await
            .expect("bounded capacity scenario");
        });
    }

    /// Always rejects, so its read-only snapshot ends with ROLLBACK.
    struct Unclaimed;

    impl Query for Unclaimed {
        type Authorized = ();
        type Output = ();
        type Rejection = Taken;

        async fn authorize(&self, _: &Client, _: View) -> Result<(), Error<Taken>> {
            Ok(())
        }

        async fn read(
            &self,
            _: &Client,
            (): (),
            _: crate::history::View,
        ) -> Result<(), Error<Taken>> {
            Err(Error::Rejected(Taken))
        }
    }

    #[test]
    #[tracing_test::traced_test]
    fn a_rejection_survives_its_failed_rollback_and_both_are_reported() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let proxy = Proxy::start(&server).await;
                let app = start(&database, authentication::proxied(&database, &proxy), 4).await;
                app.command(operation(), toy(&app, "alpha"))
                    .await
                    .expect("first claim");
                proxy.arm(Fault::RejectedRollback);
                let rejected = operation();
                assert!(matches!(
                    app.command(rejected, toy(&app, "alpha")).await,
                    Err(Error::Rejected(Taken))
                ));
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("actual command rollback failure");
                assert_eq!(counts(&database, rejected).await, (0, 0));
                proxy.arm(Fault::RejectedRollback);
                assert!(matches!(
                    app.query(&Unclaimed).await,
                    Err(Error::Rejected(Taken))
                ));
                timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("actual read rollback failure");
                assert!(logs_contain("rejection_rollback"));
                assert!(logs_contain("toy_taken"));
                app.registry()
                    .ready()
                    .expect("a determined rejection keeps admission open");
                assert_eq!(
                    app.command(operation(), toy(&app, "beta"))
                        .await
                        .expect("replaced connection"),
                    "beta"
                );
                stop(app).await;
                proxy.stop().await;
            })
            .await
            .expect("bounded rollback failure scenario");
        });
    }
}
