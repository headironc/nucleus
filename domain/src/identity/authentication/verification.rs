use bytes::BytesMut;
use chrono::{DateTime, Utc};
use strum::AsRefStr;
use tokio::time::{Instant, timeout};
use tokio_postgres::{
    Client,
    types::{IsNull, ToSql, Type, to_sql_checked},
};

use crate::identity::{
    authentication::{
        self, Error, Inner, Input, Receipt, Stage, TRANSACTION_BUDGET, database, transaction,
    },
    challenge::{Purpose, client, response},
};

pub(super) struct Action;

impl authentication::Execute for Action {
    type Output = Receipt;

    async fn execute(self, service: &Inner, input: &Input) -> Result<Receipt, Error> {
        service.verify_challenge(input).await
    }
}

/// A durable stage, not an independently constructible verification credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum Status {
    Calling,
    Retryable {
        finished_at: DateTime<Utc>,
    },
    Passed {
        solved_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
    },
    Rejected {
        finished_at: DateTime<Utc>,
    },
    Inconclusive {
        finished_at: DateTime<Utc>,
    },
}

impl Status {
    fn complete(self) -> bool {
        matches!(
            self,
            Self::Passed { .. } | Self::Rejected { .. } | Self::Inconclusive { .. }
        )
    }
    pub(super) fn finished_at(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Calling => None,
            Self::Retryable { finished_at }
            | Self::Passed { finished_at, .. }
            | Self::Rejected { finished_at }
            | Self::Inconclusive { finished_at } => Some(finished_at),
        }
    }
    pub(super) fn solved_at(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Passed { solved_at, .. } => Some(solved_at),
            _ => None,
        }
    }
}

impl ToSql for Status {
    fn to_sql(
        &self,
        kind: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Send + Sync>> {
        self.as_ref().to_sql(kind, out)
    }
    fn accepts(kind: &Type) -> bool {
        <&str as ToSql>::accepts(kind)
    }
    to_sql_checked!();
}

enum Change<'a> {
    Retry,
    Abandon,
    Finish(&'a Result<response::Context, client::Error>),
}

enum Acceptance {
    Existing(Receipt),
    Acquired(Receipt),
}

impl Inner {
    async fn accept_challenge(&self, input: &Input) -> Result<Acceptance, Error> {
        let lease = self.context.connections().lease().await?;
        let mut inserted = false;
        let error = match timeout(TRANSACTION_BUDGET, async {
            let client = lease.client();
            transaction::begin(client, input.operation_id).await?;
            let acceptance = match database::challenge::find(client, &self.keys, input).await? {
                Some(receipt) => Acceptance::Existing(receipt),
                None => {
                    transaction::token(client, &input.token).await?;
                    let now = self.sample().await?;
                    let receipt =
                        database::challenge::insert(client, &self.keys, input, now).await?;
                    inserted = true;
                    Acceptance::Acquired(receipt)
                }
            };
            client.batch_execute("COMMIT").await?;
            Ok::<_, Error>(acceptance)
        })
        .await
        {
            Ok(Ok(acceptance)) => {
                lease.release();
                return Ok(acceptance);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        if error.rejection()
            && matches!(
                timeout(TRANSACTION_BUDGET, lease.client().batch_execute("ROLLBACK")).await,
                Ok(Ok(()))
            )
        {
            lease.release();
            return Err(error);
        }
        let error = error.combine(lease.discard().await);
        // No writes or supplier calls are permitted during this original-operation readback.
        let recovery = match self.recover_challenge(input).await {
            // This task has not called the supplier. Recovering its confirmed first claim
            // preserves ownership; a later caller does not inherit it.
            Ok(Some(receipt)) => {
                return Ok(if inserted {
                    Acceptance::Acquired(receipt)
                } else {
                    Acceptance::Existing(receipt)
                });
            }
            Ok(None) => {
                return Err(Error::NotCommitted {
                    cause: Box::new(error),
                });
            }
            Err(recovery) => recovery,
        };
        if recovery.rejection() {
            return Err(recovery);
        }
        Err(Error::Unknown {
            cause: Box::new(error),
            recovery: Box::new(recovery),
        })
    }

    async fn recover_challenge(&self, input: &Input) -> Result<Option<Receipt>, Error> {
        let session = self.context.connections().recover().await?;
        let error = match timeout(TRANSACTION_BUDGET, async {
            transaction::begin(session.client(), input.operation_id).await?;
            let option = database::challenge::find(session.client(), &self.keys, input).await?;
            session.client().batch_execute("COMMIT").await?;
            Ok::<_, Error>(option)
        })
        .await
        {
            Ok(Ok(option)) => {
                session.terminate().await?;
                return Ok(option);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        Err(error.combine(session.terminate().await))
    }

    pub(super) async fn verify_challenge(&self, input: &Input) -> Result<Receipt, Error> {
        let mut receipt = match self.accept_challenge(input).await? {
            // A surviving claim does not prove what the former supplier invocation returned.
            Acceptance::Existing(receipt) if receipt.status == Status::Calling => {
                return self.change(input, receipt, &Change::Abandon).await;
            }
            Acceptance::Existing(receipt) | Acceptance::Acquired(receipt) => receipt,
        };
        if receipt.status.complete() {
            return Ok(receipt);
        }
        let sampled = Instant::now();
        let now = self.sample().await?;
        let remaining = receipt
            .expires_at
            .signed_duration_since(now)
            .to_std()
            .unwrap_or_default();
        let deadline = sampled + remaining;
        loop {
            if matches!(receipt.status, Status::Retryable { .. }) {
                receipt = self.change(input, receipt, &Change::Retry).await?;
                if matches!(receipt.status, Status::Inconclusive { .. }) {
                    return Err(client::Error::Expired.into());
                }
                if receipt.status.complete() {
                    return Ok(receipt);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.change(input, receipt, &Change::Abandon).await?;
                return Err(client::Error::Expired.into());
            }
            let response = match timeout(
                remaining,
                self.supplier.verify(
                    &input.token,
                    receipt.id,
                    Purpose::Authentication,
                    input.operation_id,
                    remaining,
                ),
            )
            .await
            {
                Ok(response) => response,
                Err(error) => Err(client::Error::Timeout(error)),
            };
            receipt = self
                .change(input, receipt, &Change::Finish(&response))
                .await?;
            if matches!(receipt.status, Status::Retryable { .. }) {
                continue;
            }
            if let Status::Inconclusive { finished_at } = receipt.status {
                match response {
                    Err(error) => return Err(error.into()),
                    Ok(context) => {
                        if let Err(error) = context.accept(finished_at) {
                            return Err(error.into());
                        }
                    }
                }
            }
            return Ok(receipt);
        }
    }

    async fn change(
        &self,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<Receipt, Error> {
        let cause = match self.context.connections().lease().await {
            Ok(lease) => {
                let error = match timeout(
                    TRANSACTION_BUDGET,
                    self.write(lease.client(), input, previous, change),
                )
                .await
                {
                    Ok(Ok(receipt)) => {
                        lease.release();
                        return Ok(receipt);
                    }
                    Ok(Err(error)) => error,
                    Err(error) => error.into(),
                };
                error.combine(lease.discard().await)
            }
            Err(error) => error.into(),
        };
        // Keep the response in this worker. The independent retry only writes local facts.
        match self.rewrite(input, previous, change).await {
            Ok(receipt) => Ok(receipt),
            Err(recovery) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(recovery),
            }),
        }
    }

    async fn rewrite(
        &self,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<Receipt, Error> {
        let session = self.context.connections().recover().await?;
        let error = match timeout(
            TRANSACTION_BUDGET,
            self.write(session.client(), input, previous, change),
        )
        .await
        {
            Ok(Ok(receipt)) => {
                session.terminate().await?;
                return Ok(receipt);
            }
            Ok(Err(error)) => error,
            Err(error) => error.into(),
        };
        Err(error.combine(session.terminate().await))
    }

    async fn write(
        &self,
        client: &Client,
        input: &Input,
        previous: Receipt,
        change: &Change<'_>,
    ) -> Result<Receipt, Error> {
        transaction::begin(client, input.operation_id).await?;
        let current = database::challenge::find(client, &self.keys, input)
            .await?
            .ok_or(Error::Facts {
                stage: Stage::ChallengeTransition,
                rule: "the original challenge must still exist",
            })?;
        let next = if current.status.complete()
            || (matches!(change, Change::Retry)
                && current.calls == 2
                && current.status == Status::Calling)
            || (matches!(change, Change::Finish(_))
                && current.calls == previous.calls
                && matches!(current.status, Status::Retryable { .. }))
        {
            current
        } else {
            if current.id != previous.id {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "challenge identity must match the original receipt",
                });
            }
            if current.calls != previous.calls {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "call generation must match the previous receipt",
                });
            }
            if current.status != previous.status {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "status must match the previous receipt",
                });
            }
            let now = self.sample().await?;
            if now < current.created_at {
                return Err(Error::Time {
                    stage: Stage::ChallengeTransition,
                    rule: "trusted time must not precede created_at",
                });
            }
            if current
                .status
                .finished_at()
                .is_some_and(|finished_at| now < finished_at)
            {
                return Err(Error::Time {
                    stage: Stage::ChallengeTransition,
                    rule: "trusted time must not precede the previous finished_at",
                });
            }
            let next = advance(current, change, now)?;
            database::challenge::update(client, input, current, next).await?;
            next
        };
        client.batch_execute("COMMIT").await?;
        Ok(next)
    }
}

fn advance(
    mut receipt: Receipt,
    change: &Change<'_>,
    now: DateTime<Utc>,
) -> Result<Receipt, Error> {
    match change {
        Change::Retry
            if matches!(receipt.status, Status::Retryable { .. })
                && receipt.calls == 1
                && now < receipt.expires_at =>
        {
            receipt.calls = 2;
            receipt.status = Status::Calling;
        }
        Change::Retry | Change::Abandon => {
            receipt.status = Status::Inconclusive { finished_at: now }
        }
        Change::Finish(result) => {
            if receipt.status != Status::Calling {
                return Err(Error::Facts {
                    stage: Stage::ChallengeTransition,
                    rule: "a supplier conclusion requires the calling state",
                });
            }
            let retryable = match result {
                Err(error) => error.retryable(),
                Ok(response::Context::SupplierFailed { error_codes }) => {
                    error_codes == &[response::Code::InternalError]
                }
                _ => false,
            };
            if retryable && receipt.calls == 1 && now < receipt.expires_at {
                receipt.status = Status::Retryable { finished_at: now };
            } else {
                receipt.status = Status::Inconclusive { finished_at: now };
                if let Ok(context) = result {
                    match context.accept(now) {
                        Ok(proof) => {
                            receipt.status = Status::Passed {
                                solved_at: proof.solved_at(),
                                finished_at: now,
                            };
                        }
                        Err(response::Error::Mismatch) => {
                            receipt.status = Status::Rejected { finished_at: now }
                        }
                        Err(response::Error::SupplierFailed { error_codes })
                            if !error_codes.is_empty()
                                && error_codes.iter().all(|code| {
                                    matches!(
                                        code,
                                        response::Code::InvalidInputResponse
                                            | response::Code::TimeoutOrDuplicate
                                            | response::Code::MissingInputResponse
                                    )
                                }) =>
                        {
                            receipt.status = Status::Rejected { finished_at: now }
                        }
                        Err(_) => {}
                    }
                }
            }
        }
    }
    Ok(receipt)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(super) mod tests {
    use std::{
        sync::{Arc, Mutex, Weak},
        time::Duration,
    };

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use chrono::{DateTime, Utc};
    use nonempty::NonEmpty;
    use platform::database::{self, Connections, credentials::Password};
    use rustls::RootCertStore;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        runtime::Runtime,
        sync::{mpsc, oneshot},
        task::JoinHandle,
        time::timeout,
    };
    use url::Url;
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        database::Barrier,
        identity::{
            authentication::{
                Error, Input, Keys, Receipt, Service, Stage, Status,
                verification::{Acceptance, Change},
            },
            challenge::{
                client, response,
                settings::{AllowedHostnames, Settings},
            },
            keys::{self, Entry, Material, metadata::Metadata},
        },
        runtime::Context,
    };

    const OPERATION: &str = "00000000-0000-0000-0000-000000000000";
    static SERVER: Mutex<Weak<fixture::Server>> = Mutex::new(Weak::new());

    pub(in crate::identity::authentication) fn server() -> Arc<fixture::Server> {
        let mut shared = SERVER.lock().expect("fixture lock");
        if let Some(server) = shared.upgrade() {
            return server;
        }
        let server = Arc::new(fixture::Server::start());
        *shared = Arc::downgrade(&server);
        server
    }

    pub(in crate::identity::authentication) struct Database {
        server: Arc<fixture::Server>,
        pub(in crate::identity::authentication) name: String,
        owner: String,
        pub(in crate::identity::authentication) user: String,
    }

    impl Database {
        pub(in crate::identity::authentication) fn connections(&self, user: &str) -> Connections {
            let mut roots = RootCertStore::empty();
            roots
                .add(self.server.certificate().clone())
                .expect("fixture CA");
            Connections::new(
                database::Settings::new(
                    "127.0.0.1".into(),
                    self.server.port(),
                    self.name.clone(),
                    user.into(),
                    2,
                    database::settings::Budgets::default(),
                )
                .expect("settings"),
                Password::new(fixture::PASSWORD.into()).expect("fixture password"),
                roots,
            )
            .expect("connections")
        }

        pub(in crate::identity::authentication) async fn new(server: Arc<fixture::Server>) -> Self {
            let id = Uuid::new_v4().simple().to_string();
            let mut database = Self {
                server,
                name: "postgres".into(),
                owner: format!("owner_{id}"),
                user: format!("runtime_{id}"),
            };
            let admin = database
                .connections("postgres")
                .recover()
                .await
                .expect("admin");
            let name = format!("test_{id}");
            admin
                .client()
                .batch_execute(&format!("CREATE DATABASE {name}"))
                .await
                .expect("disposable database");
            for user in [&database.owner, &database.user] {
                let sql: String = admin
                    .client()
                    .query_one(
                        "SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)",
                        &[user, &fixture::PASSWORD],
                    )
                    .await
                    .expect("quote fixture role")
                    .get(0);
                admin
                    .client()
                    .batch_execute(&sql)
                    .await
                    .expect("fixture role");
            }
            admin.terminate().await.expect("admin ended");
            database.name = name;
            let admin = database
                .connections("postgres")
                .recover()
                .await
                .expect("database admin");
            admin.client().query_one("SELECT set_config('nucleus.deployment_user', $1, false), set_config('nucleus.runtime_user', $2, false)", &[&database.owner, &database.user]).await.expect("identities");
            admin
                .client()
                .batch_execute(include_str!("../../../../deployment/roles.sql"))
                .await
                .expect("role boundary");
            admin
                .client()
                .batch_execute(include_str!("../../../../deployment/prepare.sql"))
                .await
                .expect("prepare");
            admin.terminate().await.expect("end preparation");
            let mut owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("owner");
            owner
                .client()
                .query_one(
                    "SELECT set_config('nucleus.runtime_user', $1, false)",
                    &[&database.user],
                )
                .await
                .expect("runtime identity");
            owner
                .client()
                .batch_execute(
                    "CREATE SCHEMA deployment; REVOKE ALL ON SCHEMA deployment FROM PUBLIC",
                )
                .await
                .expect("history schema");
            let transaction = owner
                .client_mut()
                .transaction()
                .await
                .expect("migration transaction");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V1__keys.sql"
                ))
                .await
                .expect("keys migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V2__challenge_acceptance.sql"
                ))
                .await
                .expect("challenge migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V3__message_sending.sql"
                ))
                .await
                .expect("message migration");
            transaction
                .batch_execute(include_str!(
                    "../../../../deployment/migrations/V4__retention.sql"
                ))
                .await
                .expect("retention migration");
            let keys = keys();
            keys::database::initialize(
                &Metadata::new(
                    &keys.challenge,
                    &keys.mobile,
                    &keys.encryption,
                    &keys.message,
                ),
                &transaction,
            )
            .await
            .expect("metadata");
            transaction.commit().await.expect("migrations commit");
            owner.terminate().await.expect("owner ended");
            database
        }

        pub(in crate::identity::authentication) async fn start(
            &self,
            client: client::Client,
        ) -> (Context, Service) {
            self.start_on(client, self.connections(&self.user)).await
        }

        pub(in crate::identity::authentication) async fn start_on(
            &self,
            client: client::Client,
            connections: Connections,
        ) -> (Context, Service) {
            let context = Context::new(connections).await.expect("runtime");
            let barrier = Barrier::begin(context.clone())
                .await
                .expect("checked barrier");
            let service = Service::start(
                barrier,
                keys(),
                super::super::Settings::new(4, crate::identity::message::Limits::default())
                    .expect("capacity"),
                client,
                super::super::dispatch::tests::unused_client().await,
            )
            .await
            .expect("service");
            (context, service)
        }

        pub(in crate::identity::authentication) async fn now(&self) -> DateTime<Utc> {
            let session = self
                .connections(&self.user)
                .recover()
                .await
                .expect("time reader");
            let now = session
                .client()
                .query_one("SELECT clock_timestamp()", &[])
                .await
                .expect("database time")
                .get(0);
            session.terminate().await.expect("time reader ended");
            now
        }
    }

    pub(in crate::identity::authentication) fn keys() -> Keys {
        fn entries(byte: u8) -> Vec<Entry> {
            vec![Entry::new(
                keys::Id::new(0).expect("id"),
                Material::new(Zeroizing::new(URL_SAFE_NO_PAD.encode([byte; 32])))
                    .expect("material"),
            )]
        }
        Keys::new(
            keys::challenge::Keys::new(keys::Id::new(0).expect("id"), entries(1)).expect("keys"),
            keys::mobile::Keys::new(keys::Id::new(0).expect("id"), entries(2)).expect("keys"),
            keys::encryption::Keys::new(keys::Id::new(0).expect("id"), entries(3)).expect("keys"),
            keys::message::Keys::new(keys::Id::new(0).expect("id"), entries(4)).expect("keys"),
        )
    }

    pub(in crate::identity::authentication) fn input() -> Input {
        Input::new(
            OPERATION.parse().expect("id"),
            "13800000000".parse().expect("mobile"),
            "fictional-token".parse().expect("token"),
            "192.0.2.1".parse().expect("IP"),
        )
    }

    pub(in crate::identity::authentication) struct Request {
        body: serde_json::Value,
        reply: oneshot::Sender<(u16, String)>,
    }
    impl Request {
        pub(in crate::identity::authentication) fn reply(self, status: u16, body: String) {
            assert!(
                self.reply.send((status, body)).is_ok(),
                "original HTTP attempt is still waiting"
            );
        }
    }

    pub(in crate::identity::authentication) struct Supplier {
        address: Url,
        requests: mpsc::Receiver<Request>,
        task: JoinHandle<()>,
    }
    impl Supplier {
        pub(in crate::identity::authentication) async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback supplier");
            let address = Url::parse(&format!(
                "http://{}/siteverify",
                listener.local_addr().expect("address")
            ))
            .expect("URL");
            let (sender, requests) = mpsc::channel(4);
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.expect("HTTP connection");
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    let header = loop {
                        let count = socket.read(&mut buffer).await.expect("request headers");
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&bytes[..header]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .expect("content length");
                    while bytes.len() < header + length {
                        let count = socket.read(&mut buffer).await.expect("request body");
                        assert!(count > 0);
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    let body = serde_json::from_slice(&bytes[header..header + length])
                        .expect("fixture JSON request");
                    let (reply, receiver) = oneshot::channel();
                    if sender.send(Request { body, reply }).await.is_err() {
                        break;
                    }
                    let Ok((status, body)) = receiver.await else {
                        break;
                    };
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            });
            Self {
                address,
                requests,
                task,
            }
        }
        pub(in crate::identity::authentication) fn client(&self) -> client::Client {
            client::Client::with_address(
                self.address.clone(),
                Settings::new(
                    AllowedHostnames::new(NonEmpty::new("example.com".into())).expect("hostnames"),
                ),
                "fictional-secret".parse().expect("secret"),
            )
            .expect("client")
        }
        pub(in crate::identity::authentication) async fn request(&mut self) -> Request {
            timeout(Duration::from_secs(5), self.requests.recv())
                .await
                .expect("bounded supplier call")
                .expect("request")
        }
        pub(in crate::identity::authentication) async fn stop(self) {
            self.task.abort();
            let _ = self.task.await;
        }
    }

    fn success(now: DateTime<Utc>) -> String {
        serde_json::json!({"success":true, "challenge_ts":now.to_rfc3339(), "hostname":"example.com", "action":"send_authentication_code", "cdata":OPERATION}).to_string()
    }
    fn run(service: &Service) -> JoinHandle<Result<Receipt, Error>> {
        let service = service.clone();
        tokio::spawn(async move { service.verify(input()).await })
    }
    async fn stop(context: Context, service: Service) {
        service.drain().await.expect("drain");
        context.stop().await.expect("runtime stopped");
    }

    #[test]
    fn persists_http_success_and_replays_it_across_a_restart_without_another_call() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let first = run(&service);
            let request = supplier.request().await;
            assert!(request.body["response"] == "fictional-token");
            let second = run(&service);
            request.reply(200, success(database.now().await));
            let receipt = first.await.expect("worker").expect("passed");
            assert!(matches!(receipt.status(), Status::Passed { .. }));
            assert_eq!(
                second.await.expect("waiter").expect("original result"),
                receipt
            );
            let mut wrong = input();
            wrong.token = "different-token".parse().expect("token");
            assert!(matches!(service.verify(wrong).await, Err(Error::Proof)));
            stop(context, service).await;
            let (context, service) = database.start(supplier.client()).await;
            assert_eq!(
                service.verify(input()).await.expect("restart result"),
                receipt
            );
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn retries_only_after_a_durable_second_claim_with_the_original_identity() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let first = supplier.request().await;
            let original = first.body.clone();
            first.reply(503, "temporarily unavailable".into());
            let second = supplier.request().await;
            assert!(second.body == original);
            let session = database.connections(&database.user).recover().await.expect("observer");
            let row = session.client().query_one("SELECT calls, status, expires_at - created_at = interval '10 seconds' AS fixed FROM identity.challenges", &[]).await.expect("second claim precedes HTTP");
            assert_eq!(row.get::<_, i16>("calls"), 2);
            assert_eq!(row.get::<_, String>("status"), "calling");
            assert!(row.get::<_, bool>("fixed"));
            session.terminate().await.expect("observer ended");
            second.reply(200, success(database.now().await));
            assert!(matches!(task.await.expect("worker").expect("result").status(), Status::Passed { .. }));
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn a_second_internal_error_is_terminal_and_never_grants_a_third_call() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            for _ in 0..2 {
                supplier.request().await.reply(
                    200,
                    r#"{"success":false,"error-codes":["internal-error"]}"#.into(),
                );
            }
            assert!(matches!(
                task.await.expect("worker"),
                Err(Error::Response(_))
            ));
            let receipt = service.verify(input()).await.expect("original conclusion");
            assert_eq!(receipt.calls, 2);
            assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn runtime_can_only_advance_the_claim_and_cannot_reopen_a_conclusion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            service.0.accept_challenge(&input()).await.expect("durable first claim");
            let session = database.connections(&database.user).recover().await.expect("runtime");
            for sql in [
                "UPDATE identity.challenges SET calls = 2",
                "UPDATE identity.challenges SET status = 'passed'",
                "UPDATE identity.challenges SET status = 'passed', solved_at = clock_timestamp() + interval '1 hour', finished_at = clock_timestamp()",
                "UPDATE identity.challenges SET status = 'rejected', finished_at = created_at - interval '1 second'",
            ] {
                assert_eq!(session.client().batch_execute(sql).await.expect_err("invalid transition").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            }
            session.client().batch_execute("UPDATE identity.challenges SET status = 'retryable', finished_at = clock_timestamp()").await.expect("first retryable result");
            assert_eq!(session.client().batch_execute("UPDATE identity.challenges SET status = 'passed', solved_at = clock_timestamp(), finished_at = clock_timestamp()").await.expect_err("no success without a new claim").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            session.client().batch_execute("UPDATE identity.challenges SET calls = 2, status = 'calling', finished_at = NULL").await.expect("second claim");
            assert_eq!(session.client().batch_execute("UPDATE identity.challenges SET status = 'retryable', finished_at = clock_timestamp()").await.expect_err("second attempt cannot become retryable").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            session.client().batch_execute("UPDATE identity.challenges SET status = 'inconclusive', finished_at = clock_timestamp()").await.expect("close second attempt");
            for sql in [
                "UPDATE identity.challenges SET status = 'calling', finished_at = NULL",
                "UPDATE identity.challenges SET calls = 3",
                "UPDATE identity.challenges SET status = 'passed', solved_at = clock_timestamp(), finished_at = clock_timestamp()",
                "UPDATE identity.challenges SET finished_at = clock_timestamp()",
            ] {
                assert_eq!(session.client().batch_execute(sql).await.expect_err("terminal facts cannot change").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            }
            for values in [
                "'calling'::text, NULL::timestamptz, NULL::timestamptz",
                "'passed'::text, clock_timestamp(), clock_timestamp()",
                "'rejected'::text, NULL::timestamptz, clock_timestamp()",
            ] {
                let sql = format!("SELECT * FROM (SELECT {values}) AS facts(status, solved_at, finished_at)");
                let row = session.client().query_one(&sql, &[]).await.expect("typed row");
                assert!(Status::try_from(&row).is_ok());
            }
            for values in [
                "'calling'::text, NULL::timestamptz, clock_timestamp()",
                "'passed'::text, NULL::timestamptz, clock_timestamp()",
                "'rejected'::text, clock_timestamp(), clock_timestamp()",
                "'unknown'::text, NULL::timestamptz, NULL::timestamptz",
            ] {
                let sql = format!("SELECT * FROM (SELECT {values}) AS facts(status, solved_at, finished_at)");
                let row = session.client().query_one(&sql, &[]).await.expect("invalid stored shape");
                assert!(matches!(Status::try_from(&row), Err(Error::Facts { stage: Stage::ChallengeDecode, .. })));
            }
            session.terminate().await.expect("runtime ended");
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn closes_unreliable_responses_without_retrying_or_reopening_the_challenge() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for case in ["rejection", "configuration", "mixed", "unknown", "unknown-retry", "unknown-rejection", "decode", "large", "future", "expired", "status"] {
                let database = Database::new(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let task = run(&service);
                let request = supplier.request().await;
                let (status, body) = match case {
                    "rejection" => (200, r#"{"success":false,"error-codes":["invalid-input-response"]}"#.into()),
                    "configuration" => (200, r#"{"success":false,"error-codes":["invalid-input-secret"]}"#.into()),
                    "mixed" => (200, r#"{"success":false,"error-codes":["internal-error","invalid-input-response"]}"#.into()),
                    "unknown" => (200, r#"{"success":false,"error-codes":["sensitive-marker"]}"#.into()),
                    "unknown-retry" => (200, r#"{"success":false,"error-codes":["internal-error","sensitive-marker"]}"#.into()),
                    "unknown-rejection" => (200, r#"{"success":false,"error-codes":["invalid-input-response","sensitive-marker"]}"#.into()),
                    "decode" => (200, r#"{"success":"sensitive-marker"}"#.into()),
                    "large" => (503, "x".repeat(65_537)),
                    "future" => (200, success(database.now().await + chrono::TimeDelta::seconds(10))),
                    "expired" => (200, success(database.now().await - chrono::TimeDelta::seconds(300))),
                    "status" => (429, "sensitive-marker".into()),
                    _ => unreachable!(),
                };
                request.reply(status, body);
                let result = task.await.expect("worker");
                if case == "rejection" { assert!(matches!(result.expect("rejected result").status(), Status::Rejected { .. })); }
                else {
                    let error = result.expect_err("technical response does not yield success");
                    assert!(!format!("{error:?} {error}").contains("sensitive-marker"));
                }
                let receipt = service.verify(input()).await.expect("durable original result");
                if case == "rejection" {
                    assert!(matches!(receipt.status(), Status::Rejected { .. }));
                } else {
                    assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
                }
                assert_eq!(receipt.calls, 1);
                assert!(supplier.requests.try_recv().is_err());
                stop(context, service).await;
                supplier.stop().await;
            }
        });
    }

    #[test]
    fn restart_closes_an_orphaned_claim_but_can_continue_a_confirmed_retryable_stage() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for retryable in [false, true] {
                let database = Database::new(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let Acceptance::Acquired(receipt) = service
                    .0
                    .accept_challenge(&input())
                    .await
                    .expect("first claim committed")
                else {
                    panic!("first acceptance acquires the claim");
                };
                if retryable {
                    let response = Ok(response::Context::SupplierFailed {
                        error_codes: vec![response::Code::InternalError],
                    });
                    service
                        .0
                        .change(&input(), receipt, &Change::Finish(&response))
                        .await
                        .expect("retryable stage committed");
                }
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("restart observer");
                let sql =
                    "SELECT id, created_at, expires_at, calls, status, solved_at, finished_at,
                                  (to_jsonb(challenge) - 'calls' - 'status'
                                      - 'solved_at' - 'finished_at')::text AS original
                           FROM identity.challenges challenge WHERE operation_id = $1";
                let read = async || {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_one(sql, &[&input().operation_id()]),
                    )
                    .await
                    .expect("restart observation bounded")
                    .expect("original challenge remains")
                };
                let original = read().await;
                assert_eq!(original.get::<_, i16>("calls"), 1);
                assert_eq!(
                    original.get::<_, &str>("status"),
                    if retryable { "retryable" } else { "calling" }
                );
                assert_eq!(
                    original.get::<_, Uuid>("id").to_string(),
                    receipt.id().to_string()
                );
                assert_eq!(
                    original.get::<_, DateTime<Utc>>("created_at"),
                    receipt.created_at()
                );
                assert_eq!(
                    original.get::<_, DateTime<Utc>>("expires_at"),
                    receipt.expires_at()
                );
                let restart_client = supplier.client();
                let restart = async {
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("first drain bounded");
                    timeout(Duration::from_secs(5), database.start(restart_client))
                        .await
                        .expect("restart bounded")
                };
                let (context, service) = tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => panic!("restart must not call the supplier"),
                    started = restart => started,
                };
                let task = run(&service);
                if retryable {
                    let request = supplier.request().await;
                    // Hold the response while an independent connection checks the claim.
                    let claimed = read().await;
                    assert_eq!(
                        claimed.get::<_, &str>("original"),
                        original.get::<_, &str>("original")
                    );
                    assert_eq!(claimed.get::<_, &str>("status"), "calling");
                    assert_eq!(claimed.get::<_, i16>("calls"), 2);
                    assert_eq!(claimed.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                    assert_eq!(claimed.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(request.body["response"], "fictional-token");
                    assert_eq!(
                        request.body["idempotency_key"],
                        original.get::<_, Uuid>("id").to_string()
                    );
                    request.reply(200, success(database.now().await));
                }
                let recovery = async {
                    let restored = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("restart work bounded")
                        .expect("worker")
                        .expect("original result");
                    // Inspect before replay so another request cannot repair the conclusion.
                    let stored = read().await;
                    assert_eq!(
                        stored.get::<_, &str>("original"),
                        original.get::<_, &str>("original")
                    );
                    if retryable {
                        assert_eq!(stored.get::<_, &str>("status"), "passed");
                        assert_eq!(stored.get::<_, i16>("calls"), 2);
                        assert_eq!(
                            restored.status(),
                            Status::Passed {
                                solved_at: stored.get("solved_at"),
                                finished_at: stored.get("finished_at"),
                            }
                        );
                        assert_eq!(restored.calls, 2);
                    } else {
                        assert_eq!(stored.get::<_, &str>("status"), "inconclusive");
                        assert_eq!(stored.get::<_, i16>("calls"), 1);
                        assert_eq!(stored.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                        assert_eq!(
                            restored.status(),
                            Status::Inconclusive {
                                finished_at: stored.get("finished_at"),
                            }
                        );
                        assert_eq!(restored.calls, 1);
                    }
                    assert_eq!(restored.id(), receipt.id());
                    assert_eq!(restored.created_at(), receipt.created_at());
                    assert_eq!(restored.expires_at(), receipt.expires_at());
                    assert_eq!(
                        timeout(Duration::from_secs(5), service.verify(input()))
                            .await
                            .expect("replay bounded")
                            .expect("original conclusion"),
                        restored
                    );
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("restart drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => {
                        panic!("restart recovery, replay and drain must not call again")
                    }
                    () = recovery => {}
                }
                assert!(supplier.requests.try_recv().is_err());
                observer.terminate().await.expect("restart observer ends");
                supplier.stop().await;
            }
        });
    }

    #[test]
    fn initial_commit_recovery_only_calls_the_supplier_for_a_confirmed_claim() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for fault in [
                fixture::database_proxy::Fault::LostCommit,
                fixture::database_proxy::Fault::DelayedCommit,
                fixture::database_proxy::Fault::RolledBack,
            ] {
                let database = Database::new(Arc::clone(&server)).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy CA");
                let connections = Connections::new(
                    database::Settings::new(
                        "127.0.0.1".into(),
                        proxy.port(),
                        database.name.clone(),
                        database.user.clone(),
                        2,
                        database::settings::Budgets::default(),
                    )
                    .expect("settings"),
                    Password::new(fixture::PASSWORD.into()).expect("password"),
                    roots,
                )
                .expect("proxy connections");
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start_on(supplier.client(), connections).await;
                let rolled_back = matches!(fault, fixture::database_proxy::Fault::RolledBack);
                let delayed = matches!(fault, fixture::database_proxy::Fault::DelayedCommit);
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("fixture observer");
                let operation_id = input().operation_id();
                let read = || async {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_opt(
                            "SELECT id, calls, created_at, expires_at, status, solved_at, finished_at
                             FROM identity.challenges WHERE operation_id = $1",
                            &[&operation_id],
                        ),
                    )
                    .await
                    .expect("claim observation bounded")
                    .expect("claim observation")
                };
                proxy.arm(fault);
                let task = run(&service);
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("initial commit intercepted");
                if delayed {
                    // Independent SHA-256 vector: E("challenge/operation", OPERATION's
                    // nil UUID bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes
                    // the bigint lock's high/low halves as classid/objid, with objsubid=1.
                    let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                    tokio::select! {
                        biased;
                        _ = supplier.requests.recv() => panic!("unconfirmed claim must not call the supplier"),
                        _ = async {
                            // This isolated database has one C1 worker. Recovery must
                            // cross its original transaction's lock before reading the claim.
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let waiting: bool = observer
                                        .client()
                                        .query_one(
                                            "SELECT EXISTS (
                                                SELECT 1 FROM pg_catalog.pg_locks
                                                WHERE locktype = 'advisory' AND NOT granted
                                                  AND mode = 'ExclusiveLock'
                                                  AND classid = $1 AND objid = $2 AND objsubid = 1
                                                  AND database = (
                                                      SELECT oid FROM pg_catalog.pg_database
                                                      WHERE datname = current_database()
                                                  )
                                                  AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                                            )",
                                            &[&lock.0, &lock.1],
                                        )
                                        .await
                                        .expect("recovery lock observation")
                                        .get(0);
                                    if waiting {
                                        break;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("initial recovery waits for the intercepted transaction");
                            assert!(read().await.is_none(), "original claim remains uncommitted");
                            assert!(!task.is_finished());
                        } => {}
                    }
                }
                proxy.release();
                let option = if rolled_back {
                    None
                } else {
                    let request = supplier.request().await;
                    // Read committed facts directly while the supplier response is held.
                    let original = read().await.expect("claim precedes the supplier call");
                    assert_eq!(original.get::<_, &str>("status"), "calling");
                    assert_eq!(original.get::<_, i16>("calls"), 1);
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(
                        original.get::<_, DateTime<Utc>>("expires_at")
                            - original.get::<_, DateTime<Utc>>("created_at"),
                        chrono::TimeDelta::seconds(10)
                    );
                    assert_eq!(
                        request.body["idempotency_key"],
                        original.get::<_, Uuid>("id").to_string()
                    );
                    request.reply(200, success(database.now().await));
                    Some(original)
                };
                tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => panic!("recovery and replay must not add a supplier call"),
                    _ = async {
                        let result = timeout(Duration::from_secs(5), task)
                            .await
                            .expect("recovery bounded")
                            .expect("worker");
                        if let Some(original) = option {
                            let receipt = result.expect("original worker retains its claim");
                            // Inspect before replay so a later request cannot repair the conclusion.
                            let stored = read().await.expect("original claim retained");
                            let id: Uuid = original.get("id");
                            assert_eq!(stored.get::<_, Uuid>("id"), id);
                            assert_eq!(receipt.id().to_string(), id.to_string());
                            assert_eq!(stored.get::<_, i16>("calls"), 1);
                            assert_eq!(receipt.calls, 1);
                            assert_eq!(stored.get::<_, &str>("status"), "passed");
                            assert_eq!(receipt.status(), Status::Passed {
                                solved_at: stored.get("solved_at"),
                                finished_at: stored.get("finished_at"),
                            });
                            for (column, actual) in [
                                ("created_at", receipt.created_at()),
                                ("expires_at", receipt.expires_at()),
                            ] {
                                let expected: DateTime<Utc> = original.get(column);
                                assert_eq!(stored.get::<_, DateTime<Utc>>(column), expected);
                                assert_eq!(actual, expected);
                            }
                            assert_eq!(
                                timeout(Duration::from_secs(5), service.verify(input()))
                                    .await
                                    .expect("replay bounded")
                                    .expect("original conclusion"),
                                receipt
                            );
                        } else {
                            assert!(matches!(result, Err(Error::NotCommitted { .. })));
                            assert!(read().await.is_none(), "rolled-back claim remains absent");
                        }
                        timeout(Duration::from_secs(5), stop(context, service))
                            .await
                            .expect("drain bounded");
                    } => {}
                }
                assert!(supplier.requests.try_recv().is_err());
                observer.terminate().await.expect("end fixture observer");
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn retained_supplier_success_survives_lost_commit_delayed_commit_and_rollback() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for fault in [
                fixture::database_proxy::Fault::LostCommit,
                fixture::database_proxy::Fault::DelayedCommit,
                fixture::database_proxy::Fault::RolledBack,
            ] {
                let database = Database::new(Arc::clone(&server)).await;
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let mut roots = RootCertStore::empty();
                roots.add(proxy.authority()).expect("proxy CA");
                let connections = Connections::new(
                    database::Settings::new(
                        "127.0.0.1".into(),
                        proxy.port(),
                        database.name.clone(),
                        database.user.clone(),
                        2,
                        database::settings::Budgets::default(),
                    )
                    .expect("settings"),
                    Password::new(fixture::PASSWORD.into()).expect("password"),
                    roots,
                )
                .expect("proxy connections");
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start_on(supplier.client(), connections).await;
                let observer = database
                    .connections(&database.owner)
                    .recover()
                    .await
                    .expect("fixture observer");
                let task = run(&service);
                let request = supplier.request().await;
                // Keep the supplier's timestamp distinct from the local conclusion time.
                let expected_solved_at = database.now().await - chrono::TimeDelta::seconds(1);
                let delayed = matches!(fault, fixture::database_proxy::Fault::DelayedCommit);
                proxy.arm(fault);
                request.reply(200, success(expected_solved_at));
                let recovery = async {
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("conclusion commit intercepted");
                    if delayed {
                        // Independent SHA-256 vector: E("challenge/operation", OPERATION's
                        // nil UUID bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes
                        // the bigint lock's high/low halves as classid/objid, with objsubid=1.
                        let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let waiting: bool = observer
                                    .client()
                                    .query_one(
                                        "SELECT EXISTS (
                                        SELECT 1 FROM pg_catalog.pg_locks
                                        WHERE locktype = 'advisory' AND NOT granted
                                          AND mode = 'ExclusiveLock'
                                          AND classid = $1 AND objid = $2 AND objsubid = 1
                                          AND database = (
                                              SELECT oid FROM pg_catalog.pg_database
                                              WHERE datname = current_database()
                                          )
                                          AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                                    )",
                                        &[&lock.0, &lock.1],
                                    )
                                    .await
                                    .expect("recovery lock observation")
                                    .get(0);
                                if waiting {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("conclusion recovery waits for the original operation lock");
                        let pending = observer
                            .client()
                            .query_one(
                                "SELECT status, solved_at, finished_at FROM identity.challenges
                             WHERE operation_id = $1",
                                &[&input().operation_id()],
                            )
                            .await
                            .expect("conclusion remains uncommitted");
                        assert_eq!(pending.get::<_, &str>("status"), "calling");
                        assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                        assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                        assert!(!task.is_finished());
                    }
                    proxy.release();
                    let receipt = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("recovery budget")
                        .expect("worker")
                        .expect("retained success");
                    let Status::Passed {
                        solved_at,
                        finished_at,
                    } = receipt.status()
                    else {
                        panic!("retained supplier success must pass");
                    };
                    assert_eq!(solved_at, expected_solved_at);
                    assert_eq!(receipt.calls, 1);
                    // Read before replay so a later request cannot repair a missing conclusion.
                    let stored = observer
                        .client()
                        .query_one(
                            "SELECT status, calls, solved_at, finished_at FROM identity.challenges
                         WHERE operation_id = $1",
                            &[&input().operation_id()],
                        )
                        .await
                        .expect("original worker persisted its conclusion");
                    assert_eq!(stored.get::<_, &str>("status"), "passed");
                    assert_eq!(stored.get::<_, i16>("calls"), 1);
                    assert_eq!(
                        stored.get::<_, DateTime<Utc>>("solved_at"),
                        expected_solved_at
                    );
                    assert_eq!(stored.get::<_, DateTime<Utc>>("finished_at"), finished_at);
                    assert_eq!(
                        timeout(Duration::from_secs(5), service.verify(input()))
                            .await
                            .expect("replay bounded")
                            .expect("original conclusion"),
                        receipt
                    );
                    observer.terminate().await.expect("end fixture observer");
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.requests.recv() => {
                        panic!("conclusion recovery, replay and drain must not call again")
                    }
                    () = recovery => {}
                }
                assert!(supplier.requests.try_recv().is_err());
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn initial_commit_recovery_observes_persisted_expiry_without_calling_the_supplier() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(Arc::clone(&server)).await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("fixture owner");
            // Backdate only this fixture's persisted facts, preserving their ten-second
            // lifetime. Recovery must use them instead of the fresh in-memory receipt.
            owner
                .client()
                .batch_execute(
                    r#"
                CREATE FUNCTION identity.expire_challenge() RETURNS trigger
                LANGUAGE plpgsql AS $$
                BEGIN
                    NEW.created_at := NEW.created_at - interval '1 hour';
                    NEW.expires_at := NEW.expires_at - interval '1 hour';
                    RETURN NEW;
                END
                $$;
                CREATE TRIGGER challenge_expiry BEFORE INSERT ON identity.challenges
                    FOR EACH ROW EXECUTE FUNCTION identity.expire_challenge()
            "#,
                )
                .await
                .expect("expired claim fixture");
            owner.terminate().await.expect("end fixture owner");
            let proxy = fixture::database_proxy::Proxy::start(&server).await;
            let mut roots = RootCertStore::empty();
            roots.add(proxy.authority()).expect("proxy CA");
            let connections = Connections::new(
                database::Settings::new(
                    "127.0.0.1".into(),
                    proxy.port(),
                    database.name.clone(),
                    database.user.clone(),
                    2,
                    database::settings::Budgets::default(),
                )
                .expect("settings"),
                Password::new(fixture::PASSWORD.into()).expect("password"),
                roots,
            )
            .expect("proxy connections");
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start_on(supplier.client(), connections).await;
            let observer = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("fixture observer");
            proxy.arm(fixture::database_proxy::Fault::DelayedCommit);
            let restart_client = supplier.client();
            let task = run(&service);
            let recovery = async {
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("initial commit intercepted");
                // Independent SHA-256 vector: E("challenge/operation", OPERATION's
                // nil UUID bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes
                // the bigint lock's high/low halves as classid/objid, with objsubid=1.
                let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                timeout(Duration::from_secs(5), async {
                    loop {
                        let waiting: bool = observer
                            .client()
                            .query_one(
                                "SELECT EXISTS (
                                SELECT 1 FROM pg_catalog.pg_locks
                                WHERE locktype = 'advisory' AND NOT granted
                                  AND mode = 'ExclusiveLock'
                                  AND classid = $1 AND objid = $2 AND objsubid = 1
                                  AND database = (
                                      SELECT oid FROM pg_catalog.pg_database
                                      WHERE datname = current_database()
                                  )
                                  AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                            )",
                                &[&lock.0, &lock.1],
                            )
                            .await
                            .expect("recovery lock observation")
                            .get(0);
                        if waiting {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("expired claim recovery waits for the original operation lock");
                let row = observer
                .client()
                .query_one(
                    "SELECT observed_at - interval '1 hour' + interval '10 seconds' AS expires_at,
                            clock_timestamp() AS now,
                            NOT EXISTS (SELECT 1 FROM identity.challenges) AS uncommitted
                     FROM security.clocks",
                    &[],
                )
                .await
                .expect("expiry precedes the delayed commit");
                let expires_at: DateTime<Utc> = row.get("expires_at");
                assert!(expires_at < row.get::<_, DateTime<Utc>>("now"));
                assert!(row.get::<_, bool>("uncommitted"));
                assert!(!task.is_finished());
                proxy.release();
                let result = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("recovery finishes")
                    .expect("worker");
                assert!(matches!(
                    result,
                    Err(Error::Supplier(client::Error::Expired))
                ));
                // Inspect before replay: a later verify could itself close an orphaned claim.
                let stored = observer
                .client()
                .query_one(
                    "SELECT status, finished_at FROM identity.challenges WHERE operation_id = $1",
                    &[&input().operation_id()],
                )
                .await
                .expect("original worker persisted its conclusion");
                assert_eq!(stored.get::<_, &str>("status"), "inconclusive");
                let stored_at: DateTime<Utc> = stored.get("finished_at");
                let receipt = timeout(Duration::from_secs(5), service.verify(input()))
                    .await
                    .expect("replay bounded")
                    .expect("original conclusion");
                let Status::Inconclusive { finished_at } = receipt.status() else {
                    panic!("expired call is closed");
                };
                assert_eq!(receipt.expires_at(), expires_at);
                assert_eq!(
                    receipt.expires_at() - receipt.created_at(),
                    chrono::TimeDelta::seconds(10)
                );
                assert!(finished_at >= receipt.expires_at());
                assert_eq!(finished_at, stored_at);
                assert_eq!(receipt.calls, 1);
                observer.terminate().await.expect("end fixture observer");
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("first drain bounded");
                let (context, service) = database.start(restart_client).await;
                assert_eq!(
                    timeout(Duration::from_secs(5), service.verify(input()))
                        .await
                        .expect("restart replay bounded")
                        .expect("restart result"),
                    receipt
                );
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("restart drain bounded");
            };
            tokio::select! {
                biased;
                _ = supplier.requests.recv() => {
                    panic!("expired recovery, replay, restart and drain must not call")
                }
                () = recovery => {}
            }
            assert!(supplier.requests.try_recv().is_err());
            supplier.stop().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn expired_retryable_stage_reports_expiry_once_and_preserves_its_conclusion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let Acceptance::Acquired(receipt) = service
                .0
                .accept_challenge(&input())
                .await
                .expect("first claim")
            else {
                panic!("first acceptance acquires the claim");
            };
            service
                .0
                .change(
                    &input(),
                    receipt,
                    &Change::Finish(&Ok(response::Context::SupplierFailed {
                        error_codes: vec![response::Code::InternalError],
                    })),
                )
                .await
                .expect("retryable stage");
            stop(context, service).await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("offline fixture owner");
            owner
                .client()
                .batch_execute(
                    r#"
                BEGIN;
                ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check;
                UPDATE identity.challenges
                SET created_at = created_at - interval '1 hour',
                    expires_at = expires_at - interval '1 hour',
                    finished_at = finished_at - interval '1 hour';
                ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check;
                COMMIT
            "#,
                )
                .await
                .expect("expired retained history fixture");
            owner.terminate().await.expect("end fixture owner");
            let (context, service) = database.start(supplier.client()).await;
            assert!(matches!(
                service.verify(input()).await,
                Err(Error::Supplier(client::Error::Expired))
            ));
            let restored = service.verify(input()).await.expect("original conclusion");
            assert!(matches!(restored.status(), Status::Inconclusive { .. }));
            assert_eq!(restored.id(), receipt.id());
            assert_eq!(restored.calls, 1);
            assert_eq!(
                restored.expires_at(),
                receipt.expires_at() - chrono::TimeDelta::hours(1)
            );
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn exhausted_deadline_does_not_start_another_call_or_reopen_after_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            let result = timeout(Duration::from_secs(12), task)
                .await
                .expect("original budget")
                .expect("worker");
            assert!(matches!(result, Err(Error::Supplier(_))));
            drop(request);
            let receipt = service.verify(input()).await.expect("original result");
            assert!(matches!(receipt.status(), Status::Inconclusive { .. }));
            assert_eq!(receipt.calls, 1);
            stop(context, service).await;
            let (context, service) = database.start(supplier.client()).await;
            assert_eq!(
                service.verify(input()).await.expect("restart result"),
                receipt
            );
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn two_failed_local_writes_preserve_both_causes_and_do_not_repeat_siteverify() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("owner");
            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("observer");
            let sql = "SELECT id, created_at, expires_at, status, calls, solved_at, finished_at,
                                      to_jsonb(challenge)::text AS facts,
                                      (to_jsonb(challenge) - 'status' - 'solved_at' - 'finished_at')::text AS original
                               FROM identity.challenges challenge WHERE operation_id = $1";
            let read = async || {
                timeout(
                    Duration::from_secs(5),
                    observer.client().query_one(sql, &[&input().operation_id()]),
                )
                .await
                .expect("fact observation bounded")
                .expect("original challenge remains")
            };
            let original = read().await;
            assert_eq!(original.get::<_, &str>("status"), "calling");
            assert_eq!(original.get::<_, i16>("calls"), 1);
            assert_eq!(original.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
            assert_eq!(
                original.get::<_, Option<DateTime<Utc>>>("finished_at"),
                None
            );
            assert_eq!(
                request.body["idempotency_key"],
                original.get::<_, Uuid>("id").to_string()
            );
            owner
                .client()
                .batch_execute(&format!(
                    "REVOKE UPDATE (status) ON identity.challenges FROM {}",
                    database.user
                ))
                .await
                .expect("inject local write failure");
            request.reply(200, success(database.now().await));
            let recovery = async {
                let error = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("failed writes bounded")
                    .expect("worker")
                    .expect_err("write outcome unresolved");
                let Error::Unknown { cause, recovery } = &error else {
                    panic!("both local write failures remain visible");
                };
                for failure in [cause, recovery] {
                    let Error::Query(query) = failure.as_ref() else {
                        panic!("local write must fail at the database");
                    };
                    assert_eq!(
                        query.code(),
                        Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
                    );
                }
                let diagnostic = format!("{error:?} {error}");
                for secret in ["fictional-token", "fictional-secret", "13800000000"] {
                    assert!(!diagnostic.contains(secret));
                }
                let unresolved = read().await;
                assert_eq!(
                    unresolved.get::<_, &str>("facts"),
                    original.get::<_, &str>("facts")
                );
                owner
                    .client()
                    .batch_execute(&format!(
                        "GRANT UPDATE (status) ON identity.challenges TO {}",
                        database.user
                    ))
                    .await
                    .expect("restore fixture permission");
                let closing_at = database.now().await;
                let receipt = timeout(Duration::from_secs(5), service.verify(input()))
                    .await
                    .expect("orphan recovery bounded")
                    .expect("orphan closed");
                // Inspect before replay so another request cannot repair the conclusion.
                let stored = read().await;
                assert_eq!(
                    stored.get::<_, &str>("original"),
                    original.get::<_, &str>("original")
                );
                assert_eq!(stored.get::<_, &str>("status"), "inconclusive");
                assert_eq!(stored.get::<_, Option<DateTime<Utc>>>("solved_at"), None);
                let finished_at: DateTime<Utc> = stored.get("finished_at");
                assert!(finished_at >= closing_at && finished_at <= database.now().await);
                assert_eq!(receipt.status(), Status::Inconclusive { finished_at });
                assert_eq!(receipt.calls, 1);
                assert_eq!(
                    receipt.id().to_string(),
                    original.get::<_, Uuid>("id").to_string()
                );
                assert_eq!(
                    receipt.created_at(),
                    original.get::<_, DateTime<Utc>>("created_at")
                );
                assert_eq!(
                    receipt.expires_at(),
                    original.get::<_, DateTime<Utc>>("expires_at")
                );
                assert_eq!(
                    timeout(Duration::from_secs(5), service.verify(input()))
                        .await
                        .expect("replay bounded")
                        .expect("original conclusion"),
                    receipt
                );
                owner.terminate().await.expect("owner ended");
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("drain bounded");
            };
            tokio::select! {
                biased;
                _ = supplier.requests.recv() => {
                    panic!("failed writes, recovery and drain must not repeat Siteverify")
                }
                () = recovery => {}
            }
            assert!(supplier.requests.try_recv().is_err());
            observer.terminate().await.expect("observer ended");
            supplier.stop().await;
        });
    }

    #[test]
    fn a_clock_rollback_during_the_call_keeps_the_claim_unresolved_and_closes_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            let owner = database.connections(&database.owner).recover().await.expect("owner");
            owner.client().batch_execute("UPDATE security.clocks SET observed_at = clock_timestamp() + interval '1 hour'").await.expect("simulate rollback against persistent time");
            request.reply(200, success(database.now().await));
            assert!(matches!(task.await.expect("worker"), Err(Error::Unknown { .. })));
            assert!(matches!(service.verify(input()).await, Err(Error::Closed)));
            assert_eq!(owner.client().query_one("SELECT status FROM identity.challenges", &[]).await.expect("unresolved claim").get::<_, &str>(0), "calling");
            owner.terminate().await.expect("owner ended");
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }

    #[test]
    fn cancelled_waiter_does_not_release_the_supplier_work_or_its_stop_obligation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let task = run(&service);
            let request = supplier.request().await;
            task.abort();
            let _ = task.await;
            let drain = service.clone();
            let mut drain = tokio::spawn(async move { drain.drain().await });
            assert!(
                timeout(Duration::from_millis(30), &mut drain)
                    .await
                    .is_err()
            );
            request.reply(200, success(database.now().await));
            timeout(Duration::from_secs(5), drain)
                .await
                .expect("supplier work ends")
                .expect("drain task")
                .expect("drain");
            context.stop().await.expect("stop context");
            let (context, service) = database.start(supplier.client()).await;
            assert!(matches!(
                service
                    .verify(input())
                    .await
                    .expect("original outcome")
                    .status(),
                Status::Passed { .. }
            ));
            assert!(supplier.requests.try_recv().is_err());
            stop(context, service).await;
            supplier.stop().await;
        });
    }
}
