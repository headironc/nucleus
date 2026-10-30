//! Bootstrap creates one complete administrator through the shared command pipeline.

use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;

#[cfg(any(test, feature = "test-support"))]
use crate::keys;
use crate::{
    access::{appointment, holder, role},
    history::{self, Change, Pending, Read, Record as _},
    identity::{
        account, keys as identity_keys,
        mobile::Mobile,
        operation,
        person::{self, name::Name, number::IdNumber, revision},
    },
    kernel::{
        self, App, Authorization, Command, Order, completion,
        operation::Family,
        tx::{self, Tx},
    },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operator {
    mobile: Mobile,
    name: Name,
    id_number: IdNumber,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        database::Barrier,
        identity::{
            authentication::{self, Database, server},
            operation,
        },
        kernel,
        runtime::Context,
        startup::Administration,
    };

    fn input() -> super::Operator {
        serde_json::from_value(json!({
            "mobile": "13900000000",
            "name": "Recovered Administrator",
            "idNumber": "000000200002290010",
        }))
        .expect("fictional bootstrap identity")
    }

    async fn start(connections: platform::database::Connections) -> (Context, Arc<Administration>) {
        let context = Context::hold(connections)
            .await
            .expect("exclusive instance");
        let app = Administration::start(
            Barrier::begin(context.clone())
                .await
                .expect("startup barrier"),
            authentication::inventory(authentication::keys()),
            authentication::session_settings().app_id().clone(),
        )
        .await
        .expect("complete startup verification");
        app.confirm().await.expect("admission");
        (context, Arc::new(app))
    }

    async fn counts(client: &tokio_postgres::Client) -> Vec<i64> {
        let row = client
            .query_one(
                "SELECT (SELECT count(*) FROM identity.persons),
                (SELECT count(*) FROM identity.accounts),
                (SELECT count(*) FROM access.roles),
                (SELECT count(*) FROM access.changes),
                (SELECT count(*) FROM access.holders),
                (SELECT count(*) FROM access.appointments),
                (SELECT count(*) FROM access.completions)",
                &[],
            )
            .await
            .expect("independent complete graph");
        (0..7).map(|index| row.get(index)).collect()
    }

    #[test]
    fn original_lookup_precedes_clock_sampling_on_admin_replay() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for rewind in [false, true] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::new(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let (context, app) = start(authentication::proxied(&database, &proxy)).await;
                    let operation = operation::Id::allocate();
                    let original = app.initialize(operation, input(), false).await
                        .expect("initialization").expect("known completion");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    let before = counts(owner.client()).await;
                    owner.client().batch_execute("BEGIN; LOCK TABLE access.completions IN ACCESS EXCLUSIVE MODE")
                        .await.expect("hold only the original completion read");
                    let owner_pid: i32 = owner.client().query_one("SELECT pg_backend_pid()", &[])
                        .await.expect("bound table-lock owner").get(0);
                    // Maintenance uses the instance session; replay samples its own transaction.
                    let clock = context.clock_session().await;
                    let observations = proxy.time_queries();
                    proxy.arm(if rewind { Fault::RewoundCurrent } else { Fault::StalledTime });
                    let worker = Arc::clone(&app);
                    let task = tokio::spawn(async move { worker.initialize(operation, input(), false).await });
                    let backend: i32 = timeout(Duration::from_secs(3), async {
                        loop {
                            if let Some(row) = owner.client().query_opt(
                                "SELECT pid FROM pg_catalog.pg_stat_activity
                                 WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid))",
                                &[&owner_pid],
                            ).await.expect("the retry waits on the original completion table") {
                                break row.get(0);
                            }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("original completion is read before the armed time query");
                    assert!(!task.is_finished());
                    assert_eq!(proxy.time_queries(), observations, "no time query precedes the original lookup");
                    owner.client().batch_execute("COMMIT").await.expect("release the original completion");
                    assert_eq!(timeout(Duration::from_secs(2), proxy.intercepted()).await
                        .expect("replay reaches its decisive time query"), backend,
                        "time is sampled on the transaction that read the original completion");
                    assert_eq!(proxy.time_queries(), observations + 1);
                    assert!(!task.is_finished(), "replay cannot return before time validation");
                    proxy.release();
                    let result = timeout(Duration::from_secs(5), task).await
                        .expect("retry ends after its original read").expect("retry work");
                    if rewind {
                        let error = result.expect_err("a located completion still requires trusted time validation");
                        let kernel::Error::Unavailable(fault) = error else {
                            panic!("time failure remains unavailable: {error:?}");
                        };
                        assert!(matches!(fault.kind(), kernel::FaultKind::Clock(crate::runtime::Error::ClockInvalid)), "{fault:?}");
                        assert!(*context.admission().borrow(), "rewound time closes admission");
                    } else {
                        assert_eq!(result.expect("valid transaction time"), Some(original));
                        assert!(!*context.admission().borrow());
                    }
                    assert_eq!(counts(owner.client()).await, before, "replay creates no facts");
                    drop(clock);
                    app.close();
                    app.drain().await.expect("accepted work drained");
                    context.stop().await.expect("runtime ended");
                    owner.terminate().await.expect("observer ended");
                    proxy.stop().await;
                })).await.expect("original-first replay budget");
            }
        }));
    }

    #[test]
    fn bootstrap_recovers_all_five_states_and_late_commit_before_replaying() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::new(Arc::clone(&server)).await;
                    let proxy = Proxy::start(&server).await;
                    let (context, app) = start(authentication::proxied(&database, &proxy)).await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                    let worker = Arc::clone(&app);
                    let delayed = matches!(fault, Fault::UnknownCommit);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    proxy.arm(fault);
                    let task = tokio::spawn(async move {
                        worker.initialize(operation, input(), false).await
                    });
                    timeout(Duration::from_secs(3), proxy.intercepted())
                        .await.expect("real bootstrap commit");
                    let result = timeout(Duration::from_secs(13), task)
                        .await.expect("bounded original recovery").expect("accepted bootstrap work");
                    if delayed {
                        assert!(matches!(result, Err(kernel::Error::Unknown(_))), "{:?}", result.as_ref().err());
                        assert_eq!(counts(observer.client()).await, [0; 7], "unconfirmed transaction is still invisible");
                    } else if rolled_back {
                        assert!(matches!(result, Err(kernel::Error::Unavailable(_))), "{:?}", result.as_ref().err());
                        assert_eq!(counts(observer.client()).await, [0; 7], "rollback retains no partial role or holder");
                        app.confirm().await.expect("all five candidates rolled back");
                        app.initialize(operation, input(), false).await.expect("explicit retry after confirmed rollback")
                            .expect("committed initialization");
                    } else {
                        result.expect("lost reply recovered").expect("committed initialization");
                        app.confirm().await.expect("all five candidates confirmed");
                    }
                    if !delayed { proxy.release(); }
                    app.close();
                    app.drain().await.expect("accepted work drained");
                    context.stop().await.expect("instance released");

                    let connections = database.connections(&database.user);
                    let backend: Option<i32> = if delayed {
                        Some(observer.client().query_one(
                            "SELECT pid FROM pg_catalog.pg_stat_activity
                             WHERE datname=current_database() AND state='idle in transaction'
                               AND query LIKE 'INSERT INTO access.completions%'",
                            &[],
                        ).await.expect("original completion is staged but not committed").get(0))
                    } else {
                        None
                    };
                    let restarting = tokio::spawn(async move { start(connections).await });
                    if let Some(backend) = backend {
                        timeout(Duration::from_secs(3), async {
                            loop {
                                if observer.client().query_one(
                                    "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity
                                     WHERE datname=current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))",
                                    &[&backend],
                                ).await.expect("startup blocked by the actual old transaction").get::<_, bool>(0) {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("successor waits for the original transaction");
                        proxy.release();
                    }
                    let (context, app) = timeout(Duration::from_secs(8), restarting)
                        .await.expect("startup completes after the old outcome").expect("startup task");
                    assert_eq!(counts(observer.client()).await, [1, 1, 4, 4, 1, 1, 1]);
                    let before = observer.client().query_one(
                        "SELECT holder.id, holder.assignment_id, appointment.operation_id
                         FROM access.holders holder JOIN access.appointments appointment ON appointment.id=holder.assignment_id",
                        &[],
                    ).await.expect("permanent first record before replay");
                    let replay = app.initialize(operation, input(), false).await
                        .expect("same original survives a full restart").expect("completed replay");
                    assert_eq!(replay.account_id(), before.get("id"));
                    assert_eq!(replay.operation_id(), before.get("operation_id"));
                    assert_eq!(counts(observer.client()).await, [1, 1, 4, 4, 1, 1, 1], "replay creates no new state");
                    app.close();
                    app.drain().await.expect("restart drained");
                    context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer ended");
                    proxy.stop().await;
                })).await.expect("bootstrap fault case deadline");
            }
        }));
    }
}

impl Operator {
    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        #[derive(Serialize)]
        struct Input<'a> {
            mobile: &'a str,
            name: &'a str,
            id_number: &'a str,
        }
        completion::Request::bootstrap(
            "self".to_owned(),
            &Input {
                mobile: self.mobile.as_ref(),
                name: self.name.as_ref(),
                id_number: self.id_number.as_ref(),
            },
            authority,
        )
    }

    pub fn masked(&self) -> String {
        format!(
            "{}****{} / ****{}",
            &self.mobile.as_ref()[..3],
            &self.mobile.as_ref()[7..],
            self.id_number.suffix()
        )
    }
}

/// Validated material is retained once for both metadata and identity construction.
#[cfg(any(test, feature = "test-support"))]
pub struct Keys {
    mobile: crate::identity::keys::mobile::Keys,
    number: crate::identity::keys::number::Keys,
    metadata: keys::metadata::Metadata,
    data: crate::kernel::keys::Keys,
    seals: keys::seal::Keys,
}

#[cfg(any(test, feature = "test-support"))]
impl Keys {
    pub fn new(keys: keys::Keys) -> Self {
        let metadata = keys.metadata().clone();
        Self {
            mobile: keys.identity.mobile,
            number: keys.identity.number,
            metadata,
            data: keys.encryption,
            seals: keys.seal,
        }
    }

    pub fn metadata(&self) -> &keys::metadata::Metadata {
        &self.metadata
    }

    pub(super) fn mobile(&self) -> &crate::identity::keys::mobile::Keys {
        &self.mobile
    }

    pub(super) fn number(&self) -> &crate::identity::keys::number::Keys {
        &self.number
    }

    pub(super) fn history(&self) -> crate::history::Keys<'_> {
        crate::history::Keys {
            data: &self.data,
            seals: &self.seals,
        }
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Occupied,
    FutureBirth,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Occupied => kernel::Status::Conflict,
            Self::FutureBirth => kernel::Status::Unprocessable,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Occupied => "state_conflict",
            Self::FutureBirth => "invalid_id_number",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    person_id: person::Id,
    revision_id: revision::Id,
    account_id: account::Id,
    operation_id: operation::Id,
    completed_at: DateTime<Utc>,
}

impl Receipt {
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn revision_id(&self) -> revision::Id {
        self.revision_id
    }
    pub fn account_id(&self) -> account::Id {
        self.account_id
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    pub fn completed_at(&self) -> DateTime<Utc> {
        self.completed_at
    }
}

fn accepts(action: &str, target: &str) -> bool {
    action == "initialize" && target == "self"
}

pub(crate) const COMPLETIONS: completion::Template =
    completion::Template::new(Family::Initialization, accepts);

pub(crate) async fn run(
    app: App,
    keys: Arc<identity_keys::Keys>,
    operation_id: operation::Id,
    input: Operator,
    dry_run: bool,
) -> Result<Option<Receipt>, Error> {
    let command = Write {
        app: app.clone(),
        keys,
        input,
        creation: Creation::allocate(),
    };
    if dry_run {
        app.dry_run(operation_id, command).await
    } else {
        app.command(operation_id, command).await.map(Some)
    }
}

struct Write {
    app: App,
    keys: Arc<identity_keys::Keys>,
    input: Operator,
    creation: Creation,
}

struct Creation {
    account: account::Creation,
    roles: [(role::Id, role::change::Id); 4],
    appointment_id: appointment::Id,
}

impl Creation {
    fn allocate() -> Self {
        Self {
            account: account::Creation::allocate(),
            roles: std::array::from_fn(|_| (role::Id::allocate(), role::change::Id::allocate())),
            appointment_id: appointment::Id::allocate(),
        }
    }
}

struct Facts {
    empty: bool,
    day: NaiveDate,
    operation_id: operation::Id,
}

struct Selected {
    roles: Vec<Change<role::state::State>>,
    holder: Change<holder::State>,
    receipt: Receipt,
}

fn select(
    input: &Operator,
    creation: &Creation,
    facts: Facts,
    now: DateTime<Utc>,
) -> Result<Selected, Error> {
    if !facts.empty {
        return Err(Error::Rejected(Rejection::Occupied));
    }
    input
        .id_number
        .validate_day(facts.day)
        .map_err(|_| Error::Rejected(Rejection::FutureBirth))?;
    let mut roles = Vec::with_capacity(4);
    let mut administrator = None;
    for (system, (id, change_id)) in role::Builtin::ALL.into_iter().zip(creation.roles) {
        let (role, change) = role::maintenance::create(role::maintenance::Creation {
            id,
            change_id,
            system,
            operator_id: creation.account.revision_id,
            changed_at: now,
            operation_id: facts.operation_id,
        })?;
        if system == role::Builtin::SystemAdministrator {
            administrator = Some(role);
        }
        roles.push(change);
    }
    let role = administrator.ok_or(Error::corrupt(
        "initialization includes the institutional administrator",
    ))?;
    let holder = holder::first(
        &creation.account,
        &role,
        holder::Assignment {
            id: creation.appointment_id,
            previous_id: None,
            change_id: role.change_id(),
            operator_id: creation.account.revision_id,
            revision_id: creation.account.revision_id,
            source: holder::Source::Initialization,
        },
        facts.operation_id,
        now,
    )?;
    Ok(Selected {
        roles,
        holder,
        receipt: Receipt {
            person_id: creation.account.person_id,
            revision_id: creation.account.revision_id,
            account_id: creation.account.account_id,
            operation_id: facts.operation_id,
            completed_at: now,
        },
    })
}

async fn facts(
    client: &Client,
    operation_id: operation::Id,
    empty: bool,
    now: DateTime<Utc>,
) -> Result<Facts, Error> {
    let row = client
        .query_one(
            "SELECT ($1::timestamptz AT TIME ZONE 'Asia/Shanghai')::date AS day",
            &[&now],
        )
        .await
        .map_err(Error::query)?;
    Ok(Facts {
        empty: empty && role::database::empty(client).await?,
        day: row.try_get("day").map_err(Error::decode)?,
        operation_id,
    })
}

impl Command for Write {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    const ORDER: Order = Order::OriginalFirst;
    type Facts = Facts;
    type Change = Selected;
    type Output = Receipt;
    type Rejection = Rejection;

    fn session(
        &self,
    ) -> Option<(
        &crate::identity::current::Service,
        &crate::identity::current::Actor,
    )> {
        None
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        self.input.request(authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        tx::object(tx.client(), b"access/scope", b"institution").await?;
        tx::object(tx.client(), b"access/scope", b"workforce").await?;
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        _: Option<crate::identity::current::database::State>,
    ) -> Result<Authorization, Error> {
        Ok(Authorization {
            person_id: self.creation.account.person_id,
            revision_id: self.creation.account.revision_id,
            now: tx.now(self.app.context()).await?,
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let accounts = history::all::<account::state::State>(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            Read::Snapshot(self.app.anchors().view()?),
            authority.now,
        )
        .await?;
        facts(
            tx.client(),
            tx.operation_id(),
            accounts.is_empty(),
            authority.now,
        )
        .await
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, Error> {
        select(&self.input, &self.creation, facts, now)
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<Pending>,
    ) -> Result<Receipt, Error> {
        let account = self.creation.account.prepare(
            account::Input {
                mobile: &self.input.mobile,
                name: &self.input.name,
                id_number: &self.input.id_number,
            },
            account::Keys {
                mobile: &self.keys.mobile,
                encryption: &self.keys.encryption,
                number: &self.keys.number,
            },
            account::record::Source::Initialization,
            tx.operation_id(),
            selected.receipt.completed_at,
        )?;
        account
            .write(tx.client(), self.app.keys(), self.app.anchors(), pending)
            .await?;
        for change in selected.roles {
            retain(
                pending,
                change
                    .write(tx.client(), self.app.keys(), self.app.anchors())
                    .await?,
            )?;
        }
        retain(
            pending,
            selected
                .holder
                .write(tx.client(), self.app.keys(), self.app.anchors())
                .await?,
        )?;
        Ok(selected.receipt)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Receipt, Error> {
        let receipt = original(tx.client(), &self.app, tx.operation_id(), now).await?;
        if receipt.person_id != request.person_id() {
            return Err(Error::corrupt(
                "bootstrap completion retains its first person",
            ));
        }
        Ok(receipt)
    }
}

fn retain(pending: &mut Option<Pending>, next: Pending) -> Result<(), Error> {
    match pending {
        Some(pending) => pending.extend(next)?,
        None => *pending = Some(next),
    }
    Ok(())
}

async fn original(
    client: &Client,
    app: &App,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Receipt, Error> {
    let record = holder::initialization(client, app.keys(), operation_id, now)
        .await?
        .ok_or(Error::corrupt(
            "initialization retains its first appointment",
        ))?;
    let assignment = record
        .assignment()
        .ok_or(Error::corrupt("initialization refers to an appointment"))?;
    let identity = revision::Reader::new(app.clone())
        .fetch_identity(client, assignment.revision_id)
        .await?;
    if record.sequence() != 1 || identity.person_id() != record.person_id() {
        return Err(Error::corrupt(
            "initialization retains its first identity and holder",
        ));
    }
    role::maintenance::confirm_initialization(
        client,
        &role::Reader::new(app.clone()),
        operation_id,
        assignment.revision_id,
        record.assigned_at(),
        now,
    )
    .await?;
    app.anchors().confirm::<holder::State>(&record)?;
    Ok(Receipt {
        person_id: record.person_id(),
        revision_id: assignment.revision_id,
        account_id: record.holder_id(),
        operation_id,
        completed_at: record.assigned_at(),
    })
}

pub(crate) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    let completions = completion::fetch_all(client, app.keys(), COMPLETIONS).await?;
    let count = holder::initializations(client).await?;
    if usize::try_from(count).ok() != Some(completions.len()) {
        return Err(Error::corrupt(
            "every initialization retains its completion",
        ));
    }
    for completion in completions {
        let receipt = original(client, app, completion.operation_id(), now).await?;
        if completion.person_id() != receipt.person_id
            || completion.revision_id() != receipt.revision_id
            || completion.created_at() != receipt.completed_at
        {
            return Err(Error::corrupt(
                "initialization completion and original facts agree",
            ));
        }
    }
    Ok(())
}

/// Historical fixtures use the same construction and seal encoding before startup establishes anchors.
#[cfg(any(test, feature = "test-support"))]
pub async fn seed(
    transaction: &tokio_postgres::Transaction<'_>,
    keys: &Keys,
    input: &Operator,
    at: DateTime<Utc>,
) -> Result<Receipt, Error> {
    let creation = Creation::allocate();
    let operation_id = operation::Id::allocate();
    let accounts =
        history::inspect::<account::state::State>(transaction.client(), keys.history(), at).await?;
    let selected = select(
        input,
        &creation,
        facts(transaction.client(), operation_id, accounts.is_empty(), at).await?,
        at,
    )?;
    creation
        .account
        .prepare(
            account::Input {
                mobile: &input.mobile,
                name: &input.name,
                id_number: &input.id_number,
            },
            account::Keys {
                mobile: keys.mobile(),
                encryption: keys.history().data,
                number: keys.number(),
            },
            account::record::Source::Initialization,
            operation_id,
            at,
        )?
        .write_offline(transaction.client(), keys.history())
        .await?;
    for change in selected.roles {
        change
            .write_offline(transaction.client(), keys.history())
            .await?;
    }
    selected
        .holder
        .write_offline(transaction.client(), keys.history())
        .await?;
    let request = input.request(&Authorization {
        person_id: creation.account.person_id,
        revision_id: creation.account.revision_id,
        now: at,
    })?;
    completion::insert::<Rejection>(
        transaction.client(),
        keys.history(),
        COMPLETIONS,
        operation_id,
        &request,
        at,
        COMPLETIONS.expires(at),
    )
    .await?;
    Ok(selected.receipt)
}
