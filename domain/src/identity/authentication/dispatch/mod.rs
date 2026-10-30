mod transaction;

use chrono::{DateTime, TimeDelta, Utc};
use tokio::{
    sync::{Mutex, Semaphore},
    time::{Instant, sleep_until, timeout},
};
use uuid::Uuid;

use crate::{
    identity::{
        authentication::{
            self, Error, Inner, Input, Stage, TRANSACTION_BUDGET, database::message::Message,
        },
        keys, message,
        message::{Conclusion, worker},
    },
    kernel::time::Sample,
};

pub(super) struct Action;

impl authentication::Execute for Action {
    type Output = Receipt;

    async fn execute(self, service: &Inner, input: &Input) -> Result<Receipt, Error> {
        let verification = service.verify_challenge(input).await?.allow_send()?;
        match service.send_message(input, verification.receipt()).await {
            Ok((receipt, Some(error))) => {
                tracing::error!(operation_id = %input.operation_id, stage = "message_completion", outcome = receipt.status.as_ref(), challenge = ?verification.diagnostic(), error = ?error);
                Ok(receipt)
            }
            Ok((receipt, None)) => verification.finish(input, Ok(receipt)),
            Err(error) => verification.finish(input, Err(error)),
        }
    }
}

pub(super) struct Dispatch {
    client: message::client::Client,
    capacity: Semaphore,
    next_at: Mutex<Instant>,
}

impl Dispatch {
    pub(super) fn new(client: message::client::Client) -> Self {
        Self {
            client,
            capacity: Semaphore::new(2),
            // The caller has confirmed the previous process ended before constructing us.
            // Waiting one interval also preserves spacing across an ordinary restart.
            next_at: Mutex::new(Instant::now() + std::time::Duration::from_secs(1)),
        }
    }
}

/// An original message receipt returned after checking its Challenge proof and input.
///
/// Callers obtain the receipt from [`authentication::Service::send`] and inspect its facts:
///
/// ```no_run
/// use chrono::{DateTime, Utc};
/// use domain::identity::{authentication::{Error, Input, Service}, message};
///
/// async fn inspect(service: &Service, input: Input) -> Result<(), Error> {
///     let receipt = service.send(input).await?;
///     let _: message::Id = receipt.id();
///     let _: DateTime<Utc> = receipt.created_at();
///     let _: DateTime<Utc> = receipt.expires_at();
///     let _: message::Status = receipt.status();
///     Ok(())
/// }
/// ```
///
/// Raw message facts do not establish that the original proof and input were checked:
///
/// ```compile_fail
/// use chrono::{DateTime, Utc};
/// use domain::identity::{authentication::dispatch::Receipt, message};
///
/// fn fabricate(id: message::Id, at: DateTime<Utc>) -> Receipt {
///     Receipt {
///         id,
///         created_at: at,
///         expires_at: at,
///         status: message::Status::Sent { finished_at: at },
///     }
/// }
/// ```
///
/// Callers cannot rewrite a receipt's identity, times or conclusion:
///
/// ```compile_fail
/// use domain::identity::{authentication::dispatch::Receipt, message};
/// fn replace(receipt: &mut Receipt, id: message::Id) {
///     receipt.id = id;
/// }
/// ```
///
/// ```compile_fail
/// use chrono::{DateTime, Utc};
/// use domain::identity::authentication::dispatch::Receipt;
/// fn replace(receipt: &mut Receipt, at: DateTime<Utc>) {
///     receipt.created_at = at;
/// }
/// ```
///
/// ```compile_fail
/// use chrono::{DateTime, Utc};
/// use domain::identity::authentication::dispatch::Receipt;
/// fn replace(receipt: &mut Receipt, at: DateTime<Utc>) {
///     receipt.expires_at = at;
/// }
/// ```
///
/// ```compile_fail
/// use domain::identity::{authentication::dispatch::Receipt, message};
/// fn replace(receipt: &mut Receipt, status: message::Status) {
///     receipt.status = status;
/// }
/// ```
///
/// Neither a default value nor deserialization can replace the proof and input checks:
///
/// ```compile_fail
/// use domain::identity::authentication::dispatch::Receipt;
/// let _: Receipt = Default::default();
/// ```
///
/// ```compile_fail
/// use domain::identity::authentication::dispatch::Receipt;
/// let _ = serde_json::from_str::<Receipt>("{}");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    id: message::Id,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    status: message::Status,
}

impl Receipt {
    fn new(message: &Message) -> Self {
        Self {
            id: message.id(),
            created_at: message.created_at(),
            expires_at: message.expires_at(),
            status: message.status(),
        }
    }
    pub fn id(self) -> message::Id {
        self.id
    }
    pub fn created_at(self) -> DateTime<Utc> {
        self.created_at
    }
    pub fn expires_at(self) -> DateTime<Utc> {
        self.expires_at
    }
    pub fn status(self) -> message::Status {
        self.status
    }
}

struct Prepared {
    message: Message,
    code: message::Code,
}

impl Prepared {
    fn verify(&self, keys: &keys::message::Keys, message: &Message) -> Result<(), Error> {
        if self.message.id() != message.id() {
            return Err(Error::Facts {
                stage: Stage::MessageRecovery,
                rule: "recovered message identity must match the original permit",
            });
        }
        if self.message.worker_id() != message.worker_id() {
            return Err(Error::Facts {
                stage: Stage::MessageRecovery,
                rule: "recovered worker must match the original execution",
            });
        }
        if !message.verify_code(keys, &self.code)? {
            return Err(Error::Facts {
                stage: Stage::MessageRecovery,
                rule: "the retained code must verify against the recovered proof",
            });
        }
        Ok(())
    }
}

enum Preparation {
    Original(Message),
    Ready(Prepared),
}

enum Change<'a> {
    Claim {
        worker_id: worker::Id,
        code: &'a message::Code,
    },
    Finish {
        worker_id: worker::Id,
        conclusion: Conclusion,
    },
    Orphan,
}

impl Inner {
    pub(super) async fn send_message(
        &self,
        input: &Input,
        challenge: authentication::Receipt,
    ) -> Result<(Receipt, Option<Box<Error>>), Error> {
        let (preparation, option) = self.prepare(input, challenge).await?;
        if let Preparation::Original(message) = &preparation
            && message.status() != message::Status::Sending
        {
            return Ok((Receipt::new(message), option));
        }
        let option = match option {
            // Preparation already closed admission. Do not claim, abandon, or call PNVS.
            Some(cause) if cause.unreliable() => return Err(*cause),
            option => option,
        };
        let result = async {
            let prepared = match preparation {
                Preparation::Original(_) => {
                    let (message, _, option) = self
                        .change_message(input, challenge.id, &Change::Orphan)
                        .await?;
                    return Ok((Receipt::new(&message), option));
                }
                Preparation::Ready(prepared) => prepared,
            };
            let worker_id = prepared.message.worker_id();
            let _allowance = match self.dispatch.capacity.try_acquire() {
                Ok(allowance) => allowance,
                Err(_) => {
                    return self
                        .finish_message(input, challenge.id, worker_id, Conclusion::Failed)
                        .await;
                }
            };
            let mut next_at = match timeout(TRANSACTION_BUDGET, async {
                let next_at = self.dispatch.next_at.lock().await;
                sleep_until(*next_at).await;
                next_at
            })
            .await
            {
                Ok(next_at) => next_at,
                Err(_) => {
                    return self
                        .finish_message(input, challenge.id, worker_id, Conclusion::Failed)
                        .await;
                }
            };
            let (message, sample, option) = self
                .change_message(
                    input,
                    challenge.id,
                    &Change::Claim {
                        worker_id,
                        code: &prepared.code,
                    },
                )
                .await?;
            let receipt = Receipt::new(&message);
            if receipt.status != message::Status::Sending {
                return Ok((receipt, option));
            }
            let result = async {
                let date = sampled_date(sample)?;
                let remaining = receipt.expires_at.signed_duration_since(date).num_seconds();
                if remaining < 60 {
                    return self
                        .finish_message(input, challenge.id, worker_id, Conclusion::Failed)
                        .await;
                }
                let request = match self.dispatch.client.request(
                    message::request::Input {
                        mobile: &input.mobile,
                        code: &prepared.code,
                        message_id: receipt.id,
                        valid_seconds: remaining.unsigned_abs(),
                        purpose: input.purpose(),
                    },
                    message::request::Context {
                        date,
                        nonce: Uuid::new_v4(),
                    },
                ) {
                    Ok(request) => request,
                    Err(error) => {
                        return conclude(
                            Err(error.into()),
                            self.finish_message(input, challenge.id, worker_id, Conclusion::Failed)
                                .await,
                        );
                    }
                };
                if receipt
                    .expires_at
                    .signed_duration_since(sampled_date(sample)?)
                    < TimeDelta::seconds(60)
                {
                    return self
                        .finish_message(input, challenge.id, worker_id, Conclusion::Failed)
                        .await;
                }
                // Hold spacing through the actual call so scheduling delays cannot compress starts.
                let started = std::time::Instant::now();
                let attempt = request.execute().await;
                let elapsed = started.elapsed();
                *next_at = Instant::now() + std::time::Duration::from_secs(1);
                drop(next_at);
                let (conclusion, result) = match attempt {
                    Err(cause) => (
                        Conclusion::Unknown,
                        Err(Error::MessageRequest { cause, elapsed }),
                    ),
                    Ok(attempt) => {
                        let status = attempt.status();
                        let elapsed = attempt.elapsed();
                        match attempt.into_outcome() {
                            message::response::Outcome::Accepted(_) => (Conclusion::Sent, Ok(())),
                            message::response::Outcome::Rejected(_) => (Conclusion::Failed, Ok(())),
                            message::response::Outcome::Unknown(reason) => (
                                Conclusion::Unknown,
                                Err(Error::MessageOutcome {
                                    reason,
                                    status,
                                    elapsed,
                                }),
                            ),
                        }
                    }
                };
                conclude(
                    result,
                    self.finish_message(input, challenge.id, worker_id, conclusion)
                        .await,
                )
            }
            .await;
            match (result, option) {
                (Ok((receipt, Some(cause))), Some(transition)) => Ok((
                    receipt,
                    Some(Box::new(Error::MessageTransition { cause, transition })),
                )),
                (Ok((receipt, None)), option) => Ok((receipt, option)),
                (Err(cause), Some(transition)) => Err(Error::MessageTransition {
                    cause: Box::new(cause),
                    transition,
                }),
                (result, None) => result,
            }
        }
        .await;
        match result {
            Ok((receipt, Some(cause))) => Ok((
                receipt,
                Some(match option {
                    Some(preparation) => Box::new(Error::MessagePreparation { cause, preparation }),
                    None => cause,
                }),
            )),
            Ok((receipt, None)) => Ok((receipt, option)),
            Err(cause) => Err(match option {
                Some(preparation) => Error::MessagePreparation {
                    cause: Box::new(cause),
                    preparation,
                },
                None => cause,
            }),
        }
    }
}

fn conclude(
    result: Result<(), Error>,
    persistence: Result<(Receipt, Option<Box<Error>>), Error>,
) -> Result<(Receipt, Option<Box<Error>>), Error> {
    match (result, persistence) {
        (Ok(()), result) => result,
        (Err(cause), Ok((_, Some(transition)))) => Err(Error::MessageTransition {
            cause: Box::new(cause),
            transition,
        }),
        (Err(error), Ok((_, None))) => Err(error),
        (Err(cause), Err(persistence)) => Err(Error::MessageConclusion {
            cause: Box::new(cause),
            persistence: Box::new(persistence),
        }),
    }
}

fn sampled_date(sample: Sample) -> Result<DateTime<Utc>, Error> {
    let elapsed = match TimeDelta::from_std(sample.started_at().elapsed()) {
        Ok(elapsed) => elapsed,
        Err(_) => {
            return Err(Error::Time {
                stage: Stage::MessageSigning,
                rule: "elapsed monotonic time must fit the timestamp duration",
            });
        }
    };
    sample
        .observed_at()
        .checked_add_signed(elapsed)
        .ok_or(Error::Time {
            stage: Stage::MessageSigning,
            rule: "observed_at plus elapsed monotonic time must be representable",
        })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::Row;
    use uuid::Uuid;

    use crate::{
        database::{Barrier, transaction::EndError},
        identity::{
            authentication::{
                Error, Input, Receipt, Service, Stage, dispatch,
                dispatch::{Change, Preparation},
                quota,
                verification::tests::{Database, Supplier, keys, server},
            },
            message,
            message::{
                Conclusion,
                client::Client,
                credentials::{AccessKey, Id, Secret},
                settings::{Settings, Signature, TemplateCode, Templates},
                worker,
            },
        },
        runtime::Context,
    };

    pub(crate) fn client(supplier: &fixture::supplier::Supplier) -> Client {
        client_with_budget(supplier, 1000)
    }

    fn client_with_budget(supplier: &fixture::supplier::Supplier, milliseconds: u64) -> Client {
        Client::for_test(
            supplier.address(),
            reqwest::Certificate::from_der(supplier.authority()).expect("fixture certificate"),
            Settings::new(
                Signature::new("测试".into()).expect("signature"),
                Templates::new(
                    TemplateCode::new("auth".into()).expect("template"),
                    TemplateCode::new("change".into()).expect("template"),
                    TemplateCode::new("bind".into()).expect("template"),
                ),
                milliseconds,
            )
            .expect("settings"),
            AccessKey::new(
                Id::new("FictionalAccessKey".into()).expect("id"),
                Secret::new("fictional-secret".into()).expect("secret"),
            ),
        )
        .expect("loopback client")
    }

    pub(crate) async fn unused_client() -> Client {
        client(&fixture::supplier::Supplier::start().await)
    }

    fn input(operation_id: Uuid, mobile: &str, source: &str) -> Input {
        Input::new(
            operation_id.to_string().parse().expect("operation"),
            mobile.parse().expect("mobile"),
            format!("fictional:{operation_id}").parse().expect("token"),
            source.parse().expect("source"),
        )
    }

    pub(in crate::identity::authentication) async fn start(
        database: &Database,
        challenge: &Supplier,
        supplier: &fixture::supplier::Supplier,
        limits: message::Limits,
    ) -> (Context, Service) {
        start_on(
            database.connections(&database.user),
            challenge,
            supplier,
            limits,
        )
        .await
    }

    async fn start_on(
        connections: platform::database::Connections,
        challenge: &Supplier,
        supplier: &fixture::supplier::Supplier,
        limits: message::Limits,
    ) -> (Context, Service) {
        let context = Context::new(connections).await.expect("context");
        let barrier = Barrier::begin(context.clone()).await.expect("barrier");
        let service = Service::start(
            barrier,
            keys(),
            crate::startup::Settings::new(
                8,
                limits,
                crate::identity::authentication::verification::tests::session_settings(),
                crate::registration::Limits::default(),
                crate::identity::authentication::verification::tests::avatar_settings(),
            )
            .expect("settings"),
            challenge.client(),
            client(supplier),
        )
        .await
        .expect("service");
        (context, service)
    }

    async fn passed(
        database: &Database,
        service: &Service,
        challenge: &mut Supplier,
        operation_id: Uuid,
        mobile: &str,
        source: &str,
    ) -> Receipt {
        let request = input(operation_id, mobile, source);
        let service = service.clone();
        let task = tokio::spawn(async move { service.verify(request).await });
        challenge.request().await.reply(200, serde_json::json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),
            "hostname":"example.com","action":"send_authentication_code","cdata":operation_id.to_string()}).to_string());
        task.await.expect("verification worker").expect("passed")
    }

    fn sending(
        service: &Service,
        operation_id: Uuid,
        mobile: &str,
        source: &str,
    ) -> tokio::task::JoinHandle<Result<dispatch::Receipt, Error>> {
        let service = service.clone();
        let input = input(operation_id, mobile, source);
        tokio::spawn(async move { service.send(input).await })
    }

    fn values(request: &fixture::supplier::Request) -> BTreeMap<String, String> {
        let line = std::str::from_utf8(&request.bytes)
            .expect("fictional request")
            .lines()
            .next()
            .expect("request line");
        let target = line.split(' ').nth(1).expect("target");
        url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}"))
            .expect("URL")
            .query_pairs()
            .into_owned()
            .collect()
    }

    pub(in crate::identity::authentication) fn reply(
        request: fixture::supplier::Request,
        mode: &str,
    ) {
        if mode == "lost" {
            drop(request);
            return;
        }
        let body = match mode {
            "sent" => serde_json::json!({"Code":"OK","Success":true,"Model":{"OutId":values(&request)["OutId"],"BizId":"fictional","RequestId":"57fdfbd7-e79d-465a-a121-eab3e91e7290"}}).to_string(),
            "failed" => r#"{"Code":"biz.FREQUENCY","Success":false}"#.into(),
            _ => r#"{"Code":"fictional-secret","Success":false}"#.into(),
        };
        assert!(
            request
                .reply
                .send(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .into_bytes()
                )
                .is_ok()
        );
    }

    async fn stop(context: Context, service: Service) {
        service.drain().await.expect("drain");
        context.stop().await.expect("stop");
    }

    #[test]
    #[tracing_test::traced_test]
    fn pnvs_failures_reach_one_safe_final_event_even_after_waiter_cancellation() {
        use tracing::Instrument;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(40), async {
                for mode in ["status", "timeout", "cancelled", "truncated", "json"] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut challenge = Supplier::new().await;
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let (context, service) = start(&database, &challenge, &supplier, message::Limits::default()).await;
                    let operation_id = Uuid::new_v4();
                    let receipt = passed(&database, &service, &mut challenge, operation_id, "13800000000", "192.0.2.1").await;
                    let sender = service.clone();
                    let task = tokio::spawn(async move {
                        sender.send(input(operation_id, "13800000000", "192.0.2.1")).await
                    }.in_current_span());
                    let request = supplier.request().await;
                    let query = values(&request);
                    let parameters: serde_json::Value = serde_json::from_str(&query["TemplateParam"]).expect("template parameters");
                    let code = parameters["code"].as_str().expect("fixture code").to_owned();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let sql = "SELECT message.id, message.status, message.finished_at,
                               (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                               to_jsonb(checks)::text AS proof, to_jsonb(claim)::text AS claim
                               FROM identity.messages message
                               JOIN identity.checks checks ON checks.id=message.id
                               JOIN identity.claims claim ON claim.id=message.id
                               WHERE message.challenge_id=$1";
                    let original = observer.client().query_one(sql, &[&receipt.id]).await.expect("claimed before reply");
                    assert_eq!(original.get::<_, &str>("status"), "sending");
                    let completion = async {
                        let expected = if mode == "cancelled" {
                            let release = request.reply.hold(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{".to_vec()).await;
                            task.abort();
                            assert!(task.await.expect_err("waiter cancelled").is_cancelled());
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let row = observer.client().query_one(sql, &[&receipt.id]).await.expect("cancelled work observed independently");
                                    if row.get::<_, &str>("status") == "unknown" { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("original work persisted despite cancelled waiter");
                            release.send(()).expect("release supplier body after timeout");
                            "MessageRequest { cause: Read { status: 200, timeout: true".to_owned()
                        } else {
                            let error = if mode == "timeout" {
                                let release = request.reply.hold(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{".to_vec()).await;
                                let error = task.await.expect("sender task").expect_err("body timeout");
                                release.send(()).expect("release timed out body");
                                error
                            } else {
                                let bytes = match mode {
                                "status" => b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec(),
                                "truncated" => b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{".to_vec(),
                                _ => b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\n\r\nfictional-sensitive-body".to_vec(),
                                };
                                request.reply.send(bytes).expect("supplier response");
                                task.await.expect("sender task").expect_err("unknown supplier result")
                            };
                            assert_eq!(error.failure(), crate::identity::authentication::Failure::Unavailable);
                            match (&error, mode) {
                                (Error::MessageOutcome { reason, status, elapsed }, "status") => {
                                    assert!(matches!(reason, message::response::Unknown::HttpStatus));
                                    assert_eq!(*status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
                                    assert!(*elapsed > Duration::ZERO);
                                }
                                (Error::MessageRequest { cause: message::request::Error::Read { status, source }, elapsed }, "truncated") => {
                                    assert_eq!(*status, reqwest::StatusCode::OK);
                                    assert!(!source.is_timeout());
                                    assert!(*elapsed > Duration::ZERO);
                                }
                                (Error::MessageRequest { cause: message::request::Error::Read { status, source }, elapsed }, "timeout") => {
                                    assert_eq!(*status, reqwest::StatusCode::OK);
                                    assert!(source.is_timeout());
                                    assert!(*elapsed >= Duration::from_secs(1));
                                }
                                (Error::MessageRequest { cause: message::request::Error::Decode { status, category, .. }, elapsed }, "json") => {
                                    assert_eq!(*status, reqwest::StatusCode::OK);
                                    assert_eq!(*category, serde_json::error::Category::Syntax);
                                    assert!(*elapsed > Duration::ZERO);
                                }
                                _ => panic!("unexpected supplier classification: {error:?}"),
                            }
                            format!("{error:?}")
                        };
                        // Inspect durable facts before any replay can perform orphan recovery.
                        let row = observer.client().query_one(sql, &[&receipt.id]).await.expect("independent original conclusion");
                        assert_eq!(row.get::<_, &str>("status"), "unknown");
                        let finished_at = row.get::<_, DateTime<Utc>>("finished_at");
                        let id = row.get::<_, message::Id>("id");
                        assert_eq!(id, original.get::<_, message::Id>("id"));
                        for field in ["permit", "proof", "claim"] {
                            assert_eq!(row.get::<_, &str>(field), original.get::<_, &str>(field));
                        }
                        let replay = service.send(input(operation_id, "13800000000", "192.0.2.1")).await.expect("original unknown replay");
                        assert_eq!(replay.id(), id);
                        assert_eq!(replay.status(), message::Status::Unknown { finished_at });
                        stop(context, service).await;
                        let operation = operation_id.to_string();
                        logs_assert(|lines| {
                            let events: Vec<_> = lines.iter().filter(|line| line.contains(&operation) && line.contains("authentication_completion")).collect();
                            assert_eq!(events.len(), 1, "one final failure for {mode}: {events:?}");
                            let event = events[0];
                            assert!(event.contains(&expected), "{event}");
                            assert!(event.contains("elapsed:"), "{event}");
                            for secret in ["13800000000", code.as_str(), "FictionalAccessKey", "fictional-secret", "fictional-sensitive-body", "SendSmsVerifyCode", "TemplateParam", "Authorization"] {
                                assert!(!event.contains(secret), "secret appeared in final event");
                            }
                            Ok(())
                        });
                    };
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("failure, replay or drain repeated PNVS"),
                        _ = challenge.request() => panic!("failure, replay or drain repeated verification"),
                        () = completion => {}
                    }
                    observer.terminate().await.expect("observer ended");
                    supplier.stop().await;
                    challenge.stop().await;
                }
            }).await.expect("PNVS final diagnostics bounded");
        });
    }

    #[test]
    #[tracing_test::traced_test]
    fn pnvs_final_event_preserves_supplier_and_unconfirmed_conclusion_causes() {
        use fixture::database_proxy::{Fault, Proxy};
        use tracing::Instrument;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy)).await.expect("context");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("barrier"), keys(),
                    crate::startup::Settings::new(4, message::Limits::default(),
                        crate::identity::authentication::verification::tests::session_settings(),
                        crate::registration::Limits::default(),
                        crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                    challenge.client(), client(&supplier),
                ).await.expect("service");
                let operation_id = Uuid::new_v4();
                let receipt = passed(&database, &service, &mut challenge, operation_id, "13800000000", "192.0.2.1").await;
                let sender = service.clone();
                let task = tokio::spawn(async move {
                    sender.send(input(operation_id, "13800000000", "192.0.2.1")).await
                }.in_current_span());
                let request = supplier.request().await;
                // A received PNVS request proves the earlier claim already committed.
                proxy.arm(Fault::UnknownCommit);
                request.reply.send(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec()).expect("503 response");
                let completion = async {
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("conclusion commit intercepted");
                    let error = task.await.expect("sender task").expect_err("conclusion outcome unresolved");
                    assert_eq!(error.failure(), crate::identity::authentication::Failure::Unknown);
                    let Error::MessageConclusion { cause, persistence } = &error else { panic!("both failures retained: {error:?}"); };
                    assert!(matches!(cause.as_ref(), Error::MessageOutcome { reason: message::response::Unknown::HttpStatus, status: reqwest::StatusCode::SERVICE_UNAVAILABLE, .. }));
                    let Error::Unknown { cause: write, recovery } = persistence.as_ref() else { panic!("both persistence attempts retained: {persistence:?}"); };
                    assert!(format!("{write:?}").contains("Query"));
                    assert!(format!("{recovery:?}").contains("Query"));
                    proxy.release();
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let row = timeout(Duration::from_secs(5), async {
                        loop {
                            let row = observer.client().query_one(
                                "SELECT message.id, message.status, message.finished_at,
                                 checks.id AS check_id, claim.id AS claim_id,
                                 message.worker_id, claim.worker_id AS claim_worker_id
                                 FROM identity.messages message
                                 JOIN identity.checks checks ON checks.id=message.id
                                 JOIN identity.claims claim ON claim.id=message.id
                                 WHERE message.challenge_id=$1", &[&receipt.id]
                            ).await.expect("independent late commit observation");
                            if row.get::<_, &str>("status") == "unknown" { break row; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("original conclusion committed before replay");
                    let id = row.get::<_, message::Id>("id");
                    assert_eq!(row.get::<_, message::Id>("check_id"), id);
                    assert_eq!(row.get::<_, message::Id>("claim_id"), id);
                    assert_eq!(row.get::<_, worker::Id>("worker_id"), row.get::<_, worker::Id>("claim_worker_id"));
                    let replay = service.send(input(operation_id, "13800000000", "192.0.2.1")).await.expect("original unknown replay");
                    assert_eq!(replay.id(), id);
                    assert_eq!(replay.status(), message::Status::Unknown { finished_at: row.get("finished_at") });
                    stop(context, service).await;
                    let operation = operation_id.to_string();
                    logs_assert(|lines| {
                        let events: Vec<_> = lines.iter().filter(|line| line.contains(&operation) && line.contains("authentication_completion")).collect();
                        assert_eq!(events.len(), 1, "{events:?}");
                        let event = events[0];
                        assert!(event.contains(&format!("{error:?}")), "{event}");
                        assert!(event.contains("elapsed:"));
                        assert!(!event.contains("13800000000"));
                        assert!(!event.contains("fictional-secret"));
                        assert!(!event.contains("SendSmsVerifyCode"));
                        Ok(())
                    });
                    observer.terminate().await.expect("observer ended");
                };
                tokio::select! {
                    biased;
                    _ = supplier.request() => panic!("conclusion recovery, replay or drain repeated PNVS"),
                    _ = challenge.request() => panic!("conclusion recovery, replay or drain repeated verification"),
                    () = completion => {}
                }
                supplier.stop().await;
                challenge.stop().await;
                proxy.stop().await;
            }).await.expect("combined PNVS diagnostic bounded");
        });
    }

    pub(crate) fn proxied(
        database: &Database,
        proxy: &fixture::database_proxy::Proxy,
    ) -> platform::database::Connections {
        use platform::database::{Connections, Settings, credentials::Password, settings::Budgets};
        let mut roots = rustls::RootCertStore::empty();
        roots.add(proxy.authority()).expect("proxy CA");
        Connections::new(
            Settings::new(
                "127.0.0.1".into(),
                proxy.port(),
                database.name.clone(),
                database.user.clone(),
                2,
                Budgets::default(),
            )
            .expect("settings"),
            Password::new(fixture::PASSWORD.into()).expect("password"),
            roots,
        )
        .expect("connections")
    }

    #[test]
    fn known_refusals_preserve_rollback_failures_without_new_effects() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::{Failure, session};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy)).await.expect("context");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("barrier"),
                    keys(),
                    crate::startup::Settings::new(4, message::Limits::default(),
                        crate::identity::authentication::verification::tests::session_settings(),
                        crate::registration::Limits::default(),
                        crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                    challenge.client(), client(&supplier),
                ).await.expect("service");
                let first = Uuid::new_v4();
                let receipt = passed(&database, &service, &mut challenge, first, "13800000000", "192.0.2.1").await;
                let original = input(first, "13800000000", "192.0.2.1");
                let (Preparation::Ready(prepared), None) = service.0.prepare(&original, receipt).await.expect("first permit") else { panic!("new permit"); };
                let retry_at = prepared.message.created_at() + chrono::TimeDelta::seconds(60);
                service.0.finish_message(&original, receipt.id, prepared.message.worker_id(), Conclusion::Failed).await.expect("failed send retains quota");
                let second = Uuid::new_v4();
                passed(&database, &service, &mut challenge, second, "13800000000", "192.0.2.1").await;
                let absent_message = message::Id::new();
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let snapshot = "SELECT jsonb_build_object(
                    'challenges', (SELECT jsonb_agg(to_jsonb(challenge) ORDER BY id) FROM identity.challenges challenge),
                    'messages', (SELECT jsonb_agg(to_jsonb(message) ORDER BY id) FROM identity.messages message),
                    'checks', (SELECT jsonb_agg(to_jsonb(checks) ORDER BY id) FROM identity.checks checks),
                    'claims', (SELECT jsonb_agg(to_jsonb(claim) ORDER BY id) FROM identity.claims claim),
                    'authentications', (SELECT jsonb_agg(to_jsonb(authentication) ORDER BY id) FROM identity.authentications authentication)
                )::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("initial facts").get(0);
                let held = context.connections().lease().await.expect("occupy other pool slot");
                for case in ["challenge", "quota", "session"] {
                    for failed in [false, true] {
                        let lease = context.connections().lease().await.expect("selected backend");
                        let pid: i32 = lease.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("backend PID").get(0);
                        lease.release();
                        if failed { proxy.arm(Fault::RejectedRollback); }
                        let (result, expected) = match case {
                            "challenge" => {
                                let mut request = input(second, "13800000000", "192.0.2.1");
                                request.token = "different-fictional-token".parse().expect("wrong proof");
                                (service.verify(request).await.map(|_| ()), Failure::Proof)
                            }
                            "quota" => (service.send(input(second, "13800000000", "192.0.2.1")).await.map(|_| ()), Failure::Quota { retry_at }),
                            _ => (service.create_session(session::tests::input(second, absent_message, "13800000000", session::tests::CODE)).await.map(|_| ()), Failure::Verification),
                        };
                        let error = result.expect_err("known refusal");
                        if failed {
                            timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual ROLLBACK intercepted");
                            assert_eq!(error.failure(), Failure::Unavailable);
                            let error = match error {
                                Error::Combined { cause, termination } => {
                                    assert!(matches!(termination, platform::database::Error::Postgres(_)), "{termination:?}");
                                    *cause
                                }
                                error => error,
                            };
                            let Error::Rollback { cause, rollback: EndError::Rollback(rollback) } = error else { panic!("refusal and actual rollback query failure required: {error:?}"); };
                            assert_eq!(cause.failure(), expected);
                            assert!(rollback.0.is_closed(), "{rollback:?}");
                        } else {
                            assert_eq!(error.failure(), expected);
                        }
                        // Independent observation precedes the next request or replay.
                        let after: String = observer.client().query_one(snapshot, &[]).await.expect("facts after refusal").get(0);
                        assert_eq!(before, after);
                        if failed {
                            timeout(Duration::from_secs(2), async {
                                loop {
                                    let gone: bool = observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid]).await.expect("old backend lifetime").get(0);
                                    if gone { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("discarded backend ended");
                        }
                        let lease = context.connections().lease().await.expect("next backend");
                        let next: i32 = lease.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("next PID").get(0);
                        assert_eq!(pid == next, !failed);
                        lease.release();
                    }
                }
                held.release();
                observer.terminate().await.expect("observer ended");
                stop(context, service).await;
                assert!(timeout(Duration::from_millis(50), challenge.request()).await.is_err());
                assert!(timeout(Duration::from_millis(50), supplier.request()).await.is_err());
                challenge.stop().await;
                supplier.stop().await;
                proxy.stop().await;
            }).await.expect("bounded refusals and cleanup");
        });
    }

    #[test]
    fn permit_recovery_confirms_original_code_before_sending_once() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("actual driver scheduling").block_on(async {
            for (fault, option) in [
                (Fault::LostCommit, None),
                (Fault::DelayedCommit, None),
                (Fault::RolledBack, None),
                (Fault::DelayedCommit, Some(Fault::LostReadCommit)),
                (Fault::DelayedCommit, Some(Fault::ReadCommitTermination)),
            ] {
                let driver = matches!(option, Some(Fault::ReadCommitTermination));
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy))
                    .await
                    .expect("context");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("barrier"),
                    keys(),
                    crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings())
                        .expect("settings"),
                    challenge.client(),
                    client(&supplier),
                )
                .await
                .expect("service");
                let operation_id = Uuid::nil();
                let receipt = passed(
                    &database,
                    &service,
                    &mut challenge,
                    operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("permit observer");
                let sql = "SELECT message.id, message.worker_id, message.created_at,
                                  message.expires_at, message.status, message.finished_at,
                                  (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                                  to_jsonb(checks)::text AS proof,
                                  checks.key_id, checks.hash, checks.expires_at AS check_expires_at,
                                  claim.worker_id AS claim_worker_id, claim.created_at AS claimed_at
                           FROM identity.messages message
                           JOIN identity.checks checks ON checks.id = message.id
                           LEFT JOIN identity.claims claim ON claim.id = message.id
                           WHERE message.challenge_id = $1";
                let read = async || {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_opt(sql, &[&receipt.id()]),
                    )
                    .await
                    .expect("permit observation bounded")
                    .expect("permit observation")
                };
                let absent = async || {
                    let row = timeout(
                        Duration::from_secs(5),
                        observer.client().query_one(
                            "SELECT (SELECT count(*) FROM identity.messages) AS messages,
                                    (SELECT count(*) FROM identity.checks) AS checks,
                                    (SELECT count(*) FROM identity.claims) AS claims",
                            &[],
                        ),
                    )
                    .await
                    .expect("absence observation bounded")
                    .expect("absence observation");
                    for column in ["messages", "checks", "claims"] {
                        assert_eq!(row.get::<_, i64>(column), 0, "no durable {column}");
                    }
                };
                // Pause the public sender before claiming so we can capture the
                // committed permit independently of the recovered in-memory result.
                let spacing = service.0.dispatch.next_at.lock().await;
                let delayed = matches!(fault, Fault::DelayedCommit);
                let rolled_back = matches!(fault, Fault::RolledBack);
                proxy.arm(fault);
                let mut task = sending(&service, operation_id, "13800000000", "192.0.2.1");
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("permit COMMIT intercepted");
                let spacing = if delayed {
                    // The database recovery barrier must prevent early sending;
                    // the test's spacing guard must not supply that protection.
                    drop(spacing);
                    // Independent SHA-256 vector: E("challenge/operation", nil UUID
                    // bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes the
                    // bigint lock's high/low halves as classid/objid, with objsubid=1.
                    let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("unconfirmed permit must not call PNVS"),
                        result = async {
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
                                        .expect("permit recovery lock observation")
                                        .get(0);
                                    if waiting {
                                        break;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("permit recovery waits for the original operation lock");
                            absent().await;
                            assert!(!task.is_finished());
                            // Reacquire before releasing COMMIT to snapshot the permit
                            // before the original sender can claim it.
                            timeout(Duration::from_secs(5), service.0.dispatch.next_at.lock())
                                .await
                                .expect("snapshot gate acquired while recovery waits")
                        } => result,
                    }
                } else {
                    spacing
                };
                let cleanup = option.is_some();
                if let Some(fault) = option {
                    proxy.arm(fault);
                }
                proxy.release();
                if cleanup {
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("completed permit readback COMMIT intercepted");
                    proxy.release();
                }
                if rolled_back {
                    drop(spacing);
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("rolled-back permit must not call PNVS"),
                        _ = async {
                            let result = timeout(Duration::from_secs(5), task)
                                .await
                                .expect("rollback recovery bounded")
                                .expect("sender");
                            assert!(matches!(result, Err(Error::NotCommitted { .. })));
                            absent().await;
                            stop(context, service).await;
                        } => {}
                    }
                } else {
                    let original = tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("unclaimed permit must not call PNVS"),
                        result = timeout(Duration::from_secs(5), async {
                            loop {
                                if let Some(row) = read().await {
                                    break row;
                                }
                                tokio::task::yield_now().await;
                            }
                        }) => result.expect("original permit committed before claim"),
                    };
                    let message_id: message::Id = original.get("id");
                    let worker_id: worker::Id = original.get("worker_id");
                    let created_at: DateTime<Utc> = original.get("created_at");
                    let expires_at: DateTime<Utc> = original.get("expires_at");
                    assert_eq!(original.get::<_, &str>("status"), "sending");
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(original.get::<_, Option<worker::Id>>("claim_worker_id"), None);
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                    assert_eq!(original.get::<_, DateTime<Utc>>("check_expires_at"), expires_at);
                    let unchanged = |row: &Row| {
                        for column in ["permit", "proof"] {
                            assert_eq!(row.get::<_, &str>(column), original.get::<_, &str>(column));
                        }
                    };
                    drop(spacing);
                    let request = tokio::select! {
                        result = &mut task => panic!("known permit must remain sendable after readback cleanup: {result:?}"),
                        result = timeout(Duration::from_secs(5), supplier.request()) =>
                            result.expect("one PNVS call after permit recovery"),
                    };
                    let claimed = read().await.expect("claim committed before the PNVS call");
                    unchanged(&claimed);
                    assert_eq!(claimed.get::<_, &str>("status"), "sending");
                    assert_eq!(claimed.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    assert_eq!(claimed.get::<_, worker::Id>("claim_worker_id"), worker_id);
                    let claimed_at: DateTime<Utc> = claimed.get("claimed_at");
                    assert!(claimed_at >= created_at && claimed_at < expires_at);
                    let fields = values(&request);
                    assert_eq!(fields["OutId"], message_id.to_string());
                    let parameters: serde_json::Value =
                        serde_json::from_str(&fields["TemplateParam"]).expect("template");
                    let code: message::Code = parameters["code"]
                        .as_str()
                        .expect("code")
                        .parse()
                        .expect("code");
                    assert!(
                        keys()
                            .message
                            .fetch(original.get("key_id"))
                            .expect("original key")
                            .verify(message_id, &code, &original.get("hash"))
                            .expect("sent code matches the original permit")
                    );
                    if driver {
                        request.reply.send(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).expect("supplier 503");
                    } else {
                        reply(request, "sent");
                    }
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("permit recovery and replay must not resend"),
                        _ = async {
                            let result = timeout(Duration::from_secs(5), task)
                                .await
                                .expect("sender bounded")
                                .expect("sender");
                            let option = if driver {
                                let Error::MessagePreparation { cause, preparation } = result.expect_err("supplier failure retains preparation cleanup") else { panic!("both actual errors required") };
                                assert!(matches!(*cause, Error::MessageOutcome { reason: message::response::Unknown::HttpStatus, status: reqwest::StatusCode::SERVICE_UNAVAILABLE, .. }));
                                assert!(matches!(*preparation, Error::MessageReadback { commit: Ok(()), termination: Err(platform::database::Error::Postgres(ref error)), .. } if error.code()==Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                                None
                            } else { Some(result.expect("original sender finishes")) };
                            // Read before replay so another request cannot repair the facts.
                            let stored = read().await.expect("original sender saved its conclusion");
                            unchanged(&stored);
                            assert_eq!(stored.get::<_, worker::Id>("claim_worker_id"), worker_id);
                            assert_eq!(stored.get::<_, DateTime<Utc>>("claimed_at"), claimed_at);
                            assert_eq!(stored.get::<_, &str>("status"), if driver {"unknown"} else {"sent"});
                            let finished_at: DateTime<Utc> = stored.get("finished_at");
                            assert!(finished_at >= claimed_at);
                            let replay = timeout(
                                Duration::from_secs(5),
                                service.send(input(operation_id, "13800000000", "192.0.2.1")),
                            )
                            .await
                            .expect("replay bounded")
                            .expect("original result");
                            assert_eq!(replay.id(), message_id);
                            assert_eq!(replay.created_at(), created_at);
                            assert_eq!(replay.expires_at(), expires_at);
                            assert_eq!(replay.status(), if driver {message::Status::Unknown {finished_at}} else {message::Status::Sent {finished_at}});
                            if let Some(conclusion) = option { assert_eq!(replay, conclusion); }
                            stop(context, service).await;
                        } => {}
                    }
                }
                observer.terminate().await.expect("permit observer ends");
                assert!(timeout(Duration::from_millis(50), challenge.request()).await.is_err());
                assert!(timeout(Duration::from_millis(50), supplier.request()).await.is_err());
                challenge.stop().await;
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    async fn waiting_permit(client: &tokio_postgres::Client) -> (i32, i32) {
        timeout(Duration::from_secs(3), async { loop {
            // Independent LP(challenge/operation, nil UUID) SHA-256 prefix.
            if let Some(row) = client.query_opt("SELECT waiting.pid, held.pid FROM pg_locks waiting JOIN pg_locks held USING(locktype,database,classid,objid,objsubid) WHERE waiting.locktype='advisory' AND NOT waiting.granted AND held.granted AND waiting.mode='ExclusiveLock' AND held.mode='ExclusiveLock' AND waiting.classid=2647903637::oid AND waiting.objid=2112932477::oid AND waiting.objsubid=1 AND waiting.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND held.pid=ANY(pg_blocking_pids(waiting.pid))", &[]).await.expect("exact permit operation lock") {
                break (row.get(0), row.get(1));
            }
            tokio::task::yield_now().await;
        }}).await.expect("permit recovery waits for original write")
    }

    #[test]
    fn preparation_readback_distinguishes_absence_from_incomplete_qualification() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                for absent in [true, false] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut challenge = Supplier::new().await;
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let context = Context::new(proxied(&database, &proxy)).await.expect("context");
                    let service = Service::start(Barrier::begin(context.clone()).await.expect("barrier"), keys(), crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"), challenge.client(), client(&supplier)).await.expect("service");
                    let receipt = passed(&database, &service, &mut challenge, Uuid::nil(), "13800000000", "192.0.2.1").await;
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    proxy.arm(Fault::DelayedCommit);
                    let task = sending(&service, Uuid::nil(), "13800000000", "192.0.2.1");
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("permit write held");
                    let (recovery, original) = waiting_permit(observer.client()).await;
                    proxy.arm(if absent { Fault::LostReadCommit } else { Fault::StalledQuery });
                    if absent {
                        let admin = database.connections("postgres").recover().await.expect("isolated backend control");
                        assert!(admin.client().query_one("SELECT pg_terminate_backend($1)", &[&original]).await.expect("roll back original held write").get::<_,bool>(0));
                        admin.terminate().await.expect("admin ended");
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback fault");
                    if absent { proxy.release(); }
                    let error = tokio::select! {
                        _ = supplier.request() => panic!("absence or incomplete qualification must not call PNVS"),
                        result = timeout(Duration::from_secs(12), task) => result.expect("readback transaction deadline").expect("worker").expect_err("no send qualification"),
                    };
                    if absent {
                        let Error::NotCommitted { cause } = error else { panic!("known absence: {error:?}") };
                        assert!(matches!(*cause, Error::MessageReadback { commit: Err(EndError::Commit(ref cause)), .. } if cause.0.is_closed()));
                    } else {
                        let Error::Unknown { recovery, .. } = error else { panic!("incomplete readback: {error:?}") };
                        let cause = match *recovery { Error::Combined { cause, .. } => *cause, cause => cause };
                        assert!(matches!(cause, Error::Timeout(_)), "necessary read did not finish: {cause:?}");
                        assert!(service.0.registry.ready().is_ok(), "ordinary read timeout is limited to this operation");
                    }
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.messages WHERE challenge_id=$1) AS messages, (SELECT count(*) FROM identity.checks) AS checks, (SELECT count(*) FROM identity.claims) AS claims", &[&receipt.id()]).await.expect("durable facts before replay or next work");
                    assert_eq!(row.get::<_,i64>("messages"), i64::from(!absent));
                    assert_eq!(row.get::<_,i64>("checks"), i64::from(!absent));
                    assert_eq!(row.get::<_,i64>("claims"), 0);
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 OR pid=$2)", &[&original,&recovery]).await.expect("actual backends end").get::<_,bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("original and readback backends ended");
                    stop(context, service).await;
                    assert!(timeout(Duration::from_millis(30), challenge.request()).await.is_err());
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    observer.terminate().await.expect("observer ended"); challenge.stop().await; supplier.stop().await; proxy.stop().await;
                }
            }).await.expect("absence and qualification cases bounded");
        });
    }

    #[test]
    fn original_message_read_cleanup_keeps_terminal_and_unclaimed_facts() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                for terminal in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut challenge = Supplier::new().await;
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let context = Context::new(proxied(&database, &proxy)).await.expect("context");
                    let service = Service::start(Barrier::begin(context.clone()).await.expect("barrier"), keys(), crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"), challenge.client(), client(&supplier)).await.expect("service");
                    let receipt = passed(&database, &service, &mut challenge, Uuid::nil(), "13800000000", "192.0.2.1").await;
                    let input = input(Uuid::nil(), "13800000000", "192.0.2.1");
                    let (Preparation::Ready(prepared), None) = service.0.prepare(&input, receipt).await.expect("new permit") else { panic!("new permit") };
                    if terminal { service.0.finish_message(&input, receipt.id(), prepared.message.worker_id(), Conclusion::Failed).await.expect("known terminal"); }
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    let sql = "SELECT (to_jsonb(message)-'status'-'finished_at')::text, to_jsonb(check_row)::text, to_jsonb(claim)::text, message.status, message.finished_at FROM identity.messages message JOIN identity.checks check_row ON check_row.id=message.id LEFT JOIN identity.claims claim ON claim.id=message.id WHERE message.challenge_id=$1";
                    let before = observer.client().query_one(sql, &[&receipt.id()]).await.expect("original durable graph");
                    proxy.arm(Fault::LostReadCommit);
                    let (message, Some(cause)) = service.0.send_message(&input, receipt).await.expect("known original despite initial read cleanup") else { panic!("original with diagnostic") };
                    let cause = match *cause { Error::Combined { cause, .. } => *cause, cause => cause };
                    assert!(matches!(cause, Error::Query(cause) if cause.0.is_closed()));
                    assert_eq!(message.id(), prepared.message.id());
                    assert!(matches!(message.status(), message::Status::Failed { .. }));
                    let after = observer.client().query_one(sql, &[&receipt.id()]).await.expect("independent unchanged facts before replay");
                    for column in 0..3 { assert_eq!(before.get::<_,Option<String>>(column), after.get::<_,Option<String>>(column)); }
                    assert_eq!(after.get::<_,&str>(3), "failed");
                    if terminal { assert_eq!(before.get::<_,DateTime<Utc>>(4), after.get::<_,DateTime<Utc>>(4)); }
                    else { assert!(before.get::<_,Option<DateTime<Utc>>>(4).is_none()); assert!(after.get::<_,Option<DateTime<Utc>>>(4).is_some()); }
                    proxy.release();
                    let replay = service.send(input).await.expect("original terminal or orphan closure");
                    assert_eq!(replay.id(), prepared.message.id());
                    assert!(matches!(replay.status(), message::Status::Failed { .. }));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.claims", &[]).await.expect("no transferred claim").get::<_,i64>(0), 0);
                    stop(context, service).await;
                    assert!(timeout(Duration::from_millis(30), challenge.request()).await.is_err());
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    observer.terminate().await.expect("observer ended"); challenge.stop().await; supplier.stop().await; proxy.stop().await;
                }
            }).await.expect("original read cleanup bounded");
        });
    }

    #[test]
    fn conclusion_recovery_preserves_original_supplier_result() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
            for (fault, option) in [
                (Fault::LostCommit, None),
                (Fault::DelayedCommit, None),
                (Fault::RolledBack, None),
                (Fault::DelayedCommit, Some(Fault::LostReadCommit)),
                (Fault::DelayedCommit, Some(Fault::StalledReadCommit)),
                (Fault::DelayedCommit, Some(Fault::ReadCommitTermination)),
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy))
                    .await
                    .expect("context");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("barrier"),
                    keys(),
                    crate::startup::Settings::new(
                        4,
                        message::Limits::default(),
                        crate::identity::authentication::verification::tests::session_settings(),
                        crate::registration::Limits::default(),
                        crate::identity::authentication::verification::tests::avatar_settings(),
                    )
                    .expect("settings"),
                    challenge.client(),
                    client(&supplier),
                )
                .await
                .expect("service");
                let operation_id = Uuid::nil();
                let receipt = passed(
                    &database,
                    &service,
                    &mut challenge,
                    operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let input = input(operation_id, "13800000000", "192.0.2.1");
                let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
                let request = timeout(Duration::from_secs(5), supplier.request())
                    .await
                    .expect("PNVS call");
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("conclusion observer");
                let sql = "SELECT message.id, message.worker_id, message.created_at,
                                  message.expires_at, message.status, message.finished_at,
                                  claim.worker_id AS claim_worker_id,
                                  claim.created_at AS claimed_at
                           FROM identity.messages message
                           JOIN identity.claims claim ON claim.id = message.id
                           WHERE message.challenge_id = $1";
                let original = observer
                    .client()
                    .query_one(sql, &[&receipt.id()])
                    .await
                    .expect("claim precedes the supplier response");
                let message_id: message::Id = original.get("id");
                let worker_id: worker::Id = original.get("worker_id");
                let claimed_at: DateTime<Utc> = original.get("claimed_at");
                assert_eq!(values(&request)["OutId"], message_id.to_string());
                assert_eq!(original.get::<_, worker::Id>("claim_worker_id"), worker_id);
                assert_eq!(original.get::<_, &str>("status"), "sending");
                assert_eq!(
                    original.get::<_, Option<DateTime<Utc>>>("finished_at"),
                    None
                );
                let delayed = matches!(fault, Fault::DelayedCommit);
                let readback = option.is_some();
                let stalled = matches!(option, Some(Fault::StalledReadCommit));
                let driver = matches!(option, Some(Fault::ReadCommitTermination));
                let unavailable = stalled || driver;
                proxy.arm(fault);
                if unavailable {
                    request.reply.send(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).expect("supplier 503");
                } else {
                    reply(request, "sent");
                }
                let recovery = async {
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("conclusion COMMIT intercepted");
                    let option = if delayed {
                        // Independent SHA-256 vector: E("challenge/operation", nil UUID
                        // bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes the
                        // bigint lock's high/low halves as classid/objid, with objsubid=1.
                        let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                        let backends = timeout(Duration::from_secs(5), async {
                            loop {
                                let option = observer
                                    .client()
                                    .query_opt(
                                        "SELECT waiting.pid AS recovery, held.pid AS original
                                        FROM pg_catalog.pg_locks waiting
                                        JOIN pg_catalog.pg_locks held
                                          USING (locktype, database, classid, objid, objsubid, mode)
                                        WHERE waiting.locktype = 'advisory' AND NOT waiting.granted
                                          AND held.granted AND waiting.mode = 'ExclusiveLock'
                                          AND waiting.classid = $1 AND waiting.objid = $2
                                          AND waiting.objsubid = 1
                                          AND held.pid = ANY(pg_catalog.pg_blocking_pids(waiting.pid))
                                          AND waiting.database = (
                                              SELECT oid FROM pg_catalog.pg_database
                                              WHERE datname = current_database()
                                          )",
                                        &[&lock.0, &lock.1],
                                    )
                                    .await
                                    .expect("conclusion recovery lock observation");
                                if let Some(row) = option {
                                    break (row.get::<_, i32>("original"), row.get::<_, i32>("recovery"));
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("conclusion recovery waits for the original operation lock");
                        let pending = observer
                            .client()
                            .query_one(sql, &[&receipt.id()])
                            .await
                            .expect("conclusion remains uncommitted");
                        assert_eq!(pending.get::<_, &str>("status"), "sending");
                        assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                        assert!(!task.is_finished());
                        assert_ne!(backends.0, backends.1);
                        if let Some(fault) = option {
                            proxy.arm(fault);
                        }
                        Some(backends)
                    } else { None };
                    proxy.release();
                    if readback {
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("readback COMMIT intercepted");
                        if !stalled { proxy.release(); }
                    }
                    let conclusion = timeout(Duration::from_secs(15), task)
                        .await
                        .expect("recovery bounded")
                        .expect("worker");
                    // Inspect before replay so a later request cannot repair the conclusion.
                    let stored = observer
                        .client()
                        .query_one(sql, &[&receipt.id()])
                        .await
                        .expect("original worker persisted its conclusion");
                    assert_eq!(stored.get::<_, message::Id>("id"), message_id);
                    assert_eq!(stored.get::<_, worker::Id>("worker_id"), worker_id);
                    assert_eq!(stored.get::<_, worker::Id>("claim_worker_id"), worker_id);
                    assert_eq!(stored.get::<_, DateTime<Utc>>("claimed_at"), claimed_at);
                    assert_eq!(stored.get::<_, &str>("status"), if unavailable { "unknown" } else { "sent" });
                    let finished_at: DateTime<Utc> = stored.get("finished_at");
                    assert!(finished_at >= claimed_at);
                    let expected = if unavailable { message::Status::Unknown { finished_at } } else { message::Status::Sent { finished_at } };
                    if unavailable {
                        let error = conclusion.expect_err("original supplier failure survives known persistence");
                        assert_eq!(error.failure(), crate::identity::authentication::Failure::Unavailable);
                        let Error::MessageTransition { cause, transition } = error else { panic!("supplier and cleanup errors must remain") };
                        assert!(matches!(*cause, Error::MessageOutcome { reason: message::response::Unknown::HttpStatus, status: reqwest::StatusCode::SERVICE_UNAVAILABLE, .. }));
                        let Error::MessageReadback { cause, commit, termination } = *transition else { panic!("readback causes remain typed") };
                        assert!(matches!(*cause, Error::Query(ref error) if error.0.is_closed()) || matches!(*cause, Error::Combined { cause, .. } if matches!(*cause, Error::Query(ref error) if error.0.is_closed())));
                        if stalled {
                            assert!(matches!(commit, Err(EndError::CommitTimeout(_))));
                            assert!(termination.is_ok());
                        } else {
                            assert!(commit.is_ok());
                            assert!(matches!(termination, Err(platform::database::Error::Postgres(ref error)) if error.code()==Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        }
                    } else {
                        let conclusion = conclusion.expect("the independently confirmed conclusion survives readback cleanup");
                        assert_eq!(conclusion.id(), message_id);
                        assert_eq!(conclusion.status(), expected);
                        assert_eq!(conclusion.created_at(), original.get::<_, DateTime<Utc>>("created_at"));
                        assert_eq!(conclusion.expires_at(), original.get::<_, DateTime<Utc>>("expires_at"));
                    }
                    for column in ["created_at", "expires_at"] {
                        let expected: DateTime<Utc> = original.get(column);
                        assert_eq!(stored.get::<_, DateTime<Utc>>(column), expected);
                    }
                    proxy.release();
                    if let Some((original, recovery)) = option {
                        timeout(Duration::from_secs(3), async { loop {
                            let ended: bool = observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid = ANY($1))", &[&vec![original,recovery]]).await.expect("backend completion").get(0);
                            if ended { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("both backends actually end");
                    }
                    let replay = timeout(Duration::from_secs(5), service.send(input))
                        .await
                        .expect("replay bounded")
                        .expect("original conclusion");
                    assert_eq!(replay.id(), message_id);
                    assert_eq!(replay.status(), expected);
                    assert_eq!(replay.created_at(), original.get::<_, DateTime<Utc>>("created_at"));
                    assert_eq!(replay.expires_at(), original.get::<_, DateTime<Utc>>("expires_at"));
                    observer
                        .terminate()
                        .await
                        .expect("conclusion observer ends");
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.request() => {
                        panic!("conclusion recovery, replay and drain must not resend")
                    }
                    () = recovery => {}
                }
                challenge.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn claim_recovery_confirms_original_ownership_before_sending_once() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
            for (fault, driver) in [(Fault::LostCommit, false), (Fault::DelayedCommit, false), (Fault::RolledBack, false), (Fault::DelayedCommit, true)] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy))
                    .await
                    .expect("context");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("barrier"),
                    keys(),
                    crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings())
                        .expect("settings"),
                    challenge.client(),
                    client(&supplier),
                )
                .await
                .expect("service");
                let operation_id = Uuid::nil();
                let receipt = passed(
                    &database,
                    &service,
                    &mut challenge,
                    operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("claim observer");
                let sql = "SELECT message.id, message.worker_id, message.created_at,
                                  message.expires_at, message.status, message.finished_at,
                                  (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                                  to_jsonb(checks)::text AS proof,
                                  checks.key_id, checks.hash, checks.expires_at AS check_expires_at,
                                  claim.worker_id AS claim_worker_id, claim.created_at AS claimed_at
                           FROM identity.messages message
                           JOIN identity.checks checks ON checks.id = message.id
                           LEFT JOIN identity.claims claim ON claim.id = message.id
                           WHERE message.challenge_id = $1";
                let read = async || {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_opt(sql, &[&receipt.id()]),
                    )
                    .await
                    .expect("claim observation bounded")
                    .expect("claim observation")
                };
                // Keep the public sender's original code and worker alive while we
                // observe its committed permit, then fault only the claim transaction.
                let spacing = service.0.dispatch.next_at.lock().await;
                let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
                let original = timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(row) = read().await {
                            break row;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("original permit committed before claim");
                let message_id: message::Id = original.get("id");
                let worker_id: worker::Id = original.get("worker_id");
                let created_at: DateTime<Utc> = original.get("created_at");
                let expires_at: DateTime<Utc> = original.get("expires_at");
                assert_eq!(original.get::<_, &str>("status"), "sending");
                assert_eq!(original.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                assert_eq!(original.get::<_, Option<worker::Id>>("claim_worker_id"), None);
                assert_eq!(original.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                assert_eq!(original.get::<_, DateTime<Utc>>("check_expires_at"), expires_at);
                let unchanged = |row: &Row| {
                    for column in ["permit", "proof"] {
                        assert_eq!(row.get::<_, &str>(column), original.get::<_, &str>(column));
                    }
                };
                let delayed = matches!(fault, Fault::DelayedCommit);
                proxy.arm(fault);
                drop(spacing);
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("claim COMMIT intercepted");
                if delayed {
                    // Independent SHA-256 vector: E("challenge/operation", nil UUID
                    // bytes) starts with 9dd3cd95 7df0ca7d. PostgreSQL exposes the
                    // bigint lock's high/low halves as classid/objid, with objsubid=1.
                    let lock = (2_647_903_637_u32, 2_112_932_477_u32);
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("unconfirmed claim must not call PNVS"),
                        _ = async {
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let waiting: bool = observer
                                        .client()
                                        .query_one(
                                            "SELECT EXISTS (
                                                SELECT 1 FROM pg_catalog.pg_locks waiting
                                                JOIN pg_catalog.pg_locks held
                                                  USING (locktype, database, classid, objid, objsubid, mode)
                                                WHERE waiting.locktype = 'advisory' AND NOT waiting.granted
                                                  AND held.granted AND waiting.mode = 'ExclusiveLock'
                                                  AND waiting.classid = $1 AND waiting.objid = $2 AND waiting.objsubid = 1
                                                  AND waiting.database = (
                                                      SELECT oid FROM pg_catalog.pg_database
                                                      WHERE datname = current_database()
                                                  )
                                                  AND held.pid = ANY(pg_catalog.pg_blocking_pids(waiting.pid))
                                            )",
                                            &[&lock.0, &lock.1],
                                        )
                                        .await
                                        .expect("claim recovery lock observation")
                                        .get(0);
                                    if waiting {
                                        break;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("claim recovery waits for the original operation lock");
                            let pending = read().await.expect("original permit remains");
                            unchanged(&pending);
                            assert_eq!(pending.get::<_, &str>("status"), "sending");
                            assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                            assert_eq!(pending.get::<_, Option<worker::Id>>("claim_worker_id"), None);
                            assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                            assert!(!task.is_finished());
                        } => {}
                    }
                }
                if driver { proxy.arm(Fault::ReadCommitTermination); }
                proxy.release();
                if driver { timeout(Duration::from_secs(5),proxy.intercepted()).await.expect("original claim readback ended with driver failure"); }
                let request = timeout(Duration::from_secs(5), supplier.request())
                    .await
                    .expect("one PNVS call after claim recovery");
                let claimed = read().await.expect("claim committed before the PNVS call");
                unchanged(&claimed);
                assert_eq!(claimed.get::<_, &str>("status"), "sending");
                assert_eq!(claimed.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                assert_eq!(claimed.get::<_, worker::Id>("claim_worker_id"), worker_id);
                let claimed_at: DateTime<Utc> = claimed.get("claimed_at");
                assert!(claimed_at >= created_at && claimed_at < expires_at);
                let fields = values(&request);
                assert_eq!(fields["OutId"], message_id.to_string());
                let parameters: serde_json::Value =
                    serde_json::from_str(&fields["TemplateParam"]).expect("template");
                let code: message::Code = parameters["code"]
                    .as_str()
                    .expect("code")
                    .parse()
                    .expect("code");
                assert!(
                    keys()
                        .message
                        .fetch(original.get("key_id"))
                        .expect("original key")
                        .verify(message_id, &code, &original.get("hash"))
                        .expect("sent code matches the original permit")
                );
                if driver {
                    request.reply.send(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()).expect("supplier 503");
                } else { reply(request, "sent"); }
                tokio::select! {
                    biased;
                    _ = supplier.request() => panic!("claim recovery and replay must not resend"),
                    _ = async {
                        let conclusion = timeout(Duration::from_secs(5), task)
                            .await
                            .expect("sender bounded")
                            .expect("sender");
                        // Read before replay so another request cannot repair the facts.
                        let stored = read().await.expect("original sender saved its conclusion");
                        unchanged(&stored);
                        assert_eq!(stored.get::<_, worker::Id>("claim_worker_id"), worker_id);
                        assert_eq!(stored.get::<_, DateTime<Utc>>("claimed_at"), claimed_at);
                        assert_eq!(stored.get::<_, &str>("status"), if driver { "unknown" } else { "sent" });
                        let finished_at: DateTime<Utc> = stored.get("finished_at");
                        assert!(finished_at >= claimed_at);
                        let expected = if driver { message::Status::Unknown { finished_at } } else { message::Status::Sent { finished_at } };
                        if driver {
                            let error = conclusion.expect_err("PNVS error retains claim cleanup");
                            assert_eq!(error.failure(),crate::identity::authentication::Failure::Unavailable);
                            let Error::MessageTransition { cause, transition } = error else { panic!("both stages retained") };
                            assert!(matches!(*cause, Error::MessageOutcome { status: reqwest::StatusCode::SERVICE_UNAVAILABLE, .. }));
                            assert!(matches!(*transition,Error::MessageReadback { commit:Ok(()),termination:Err(platform::database::Error::Postgres(ref error)),.. } if error.code()==Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        } else {
                            let conclusion = conclusion.expect("original sender finishes");
                            assert_eq!(conclusion.id(), message_id);
                            assert_eq!(conclusion.created_at(), created_at);
                            assert_eq!(conclusion.expires_at(), expires_at);
                            assert_eq!(conclusion.status(), expected);
                        }
                        let replay = timeout(
                            Duration::from_secs(5),
                            service.send(input(operation_id, "13800000000", "192.0.2.1")),
                        )
                        .await
                        .expect("replay bounded")
                        .expect("original result");
                        assert_eq!(replay.id(), message_id);
                        assert_eq!(replay.created_at(), created_at);
                        assert_eq!(replay.expires_at(), expires_at);
                        assert_eq!(replay.status(), expected);
                        stop(context, service).await;
                    } => {}
                }
                observer.terminate().await.expect("claim observer ends");
                challenge.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn quota_and_replays_keep_the_first_source_and_do_not_refund_failed_permits() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database=Database::initialized(server).await;
            let mut challenge=Supplier::new().await;
            let supplier=fixture::supplier::Supplier::start().await;
            let (context,service)=start(&database,&challenge,&supplier,message::Limits::default()).await;
            let operation_id=Uuid::new_v4();
            let receipt=passed(&database,&service,&mut challenge,operation_id,"13800000000","192.0.2.1").await;
            let original=input(operation_id,"13800000000","192.0.2.99");
            let (Preparation::Ready(prepared), None)=service.0.prepare(&original,receipt).await.expect("permit") else {panic!("new");};
            let permit=super::Receipt::new(&prepared.message);
            service.0.finish_message(&original,receipt.id,prepared.message.worker_id(),Conclusion::Failed).await.expect("failed send");
            let second_operation_id=Uuid::new_v4();
            let receipt=passed(&database,&service,&mut challenge,second_operation_id,"13800000000","192.0.2.99").await;
            let error=service.0.prepare(&input(second_operation_id,"13800000000","192.0.2.99"),receipt).await.err().expect("minimum interval still charged");
            assert!(matches!(error,Error::Quota{retry_at} if retry_at==permit.created_at+chrono::TimeDelta::seconds(60)));
            let reader=database.connections(&database.user).recover().await.expect("reader");
            let row=reader.client().query_one("SELECT count(*) AS total FROM identity.messages",&[]).await.expect("permits");
            assert_eq!(row.get::<_,i64>("total"),1);
            let source: std::net::IpAddr=reader.client().query_one("SELECT source FROM identity.challenges WHERE operation_id=$1",&[&original.operation_id]).await.expect("source").get(0);
            assert_eq!(source,"192.0.2.1".parse::<std::net::IpAddr>().expect("source"));
            let hash=keys().mobile.hash(&original.mobile).expect("hash");
            // Strict lower window edge: the original permit is excluded at exactly 24 hours.
            quota::verify(reader.client(),&message::Limits::default(),&hash,source,permit.created_at+chrono::TimeDelta::hours(24)).await.expect("old permit outside all windows");
            reader.terminate().await.expect("reader ended");
            stop(context,service).await;
            challenge.stop().await;
        });
    }

    #[test]
    fn database_enforces_pairing_original_claim_and_irreversible_outcomes() {
        use tokio_postgres::error::SqlState;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database=Database::initialized(server).await;
            let mut challenge=Supplier::new().await;
            let supplier=fixture::supplier::Supplier::start().await;
            let (context,service)=start(&database,&challenge,&supplier,message::Limits::default()).await;
            let operation_id=Uuid::new_v4();
            let receipt=passed(&database,&service,&mut challenge,operation_id,"13800000000","192.0.2.1").await;
            let input=input(operation_id,"13800000000","192.0.2.1");
            let (Preparation::Ready(prepared), None)=service.0.prepare(&input,receipt).await.expect("permit") else {panic!("new");};
            let other_operation_id=Uuid::new_v4();
            let other_receipt=passed(&database,&service,&mut challenge,other_operation_id,"13900000000","192.0.2.2").await;
            let runtime=database.connections(&database.user).recover().await.expect("runtime");
            let message_id=prepared.message.id();
            let error=runtime.client().execute("UPDATE identity.messages SET status='sent',finished_at=clock_timestamp() WHERE id=$1",&[&message_id]).await.expect_err("sent requires claim");
            assert_eq!(error.code(),Some(&SqlState::CHECK_VIOLATION));
            let error=runtime.client().execute("INSERT INTO identity.claims (id,worker_id,created_at) VALUES($1,$2,clock_timestamp())",&[&message_id,&worker::Id::new()]).await.expect_err("other worker cannot claim");
            assert_eq!(error.code(),Some(&SqlState::CHECK_VIOLATION));
            let wrong_code = if prepared.code.as_ref() == "000000" { "000001" } else { "000000" }.parse().expect("different valid code");
            for (change, field) in [
                (Change::Claim { worker_id: worker::Id::new(), code: &prepared.code }, "worker"),
                (Change::Claim { worker_id: prepared.message.worker_id(), code: &wrong_code }, "code"),
            ] {
                let error = service.0.change_message(&input, receipt.id, &change).await.err().expect("claim invariant rejected");
                let Error::MessageReadback { cause, commit: Ok(()), termination: Ok(()) } = &error else {
                    panic!("original violation and complete read-only recovery remain represented: {error:?}");
                };
                assert!(matches!(cause.as_ref(), Error::Facts { stage: Stage::MessageClaim, rule } if rule.contains(field)));
                assert!(error.unreliable());
                assert!(matches!(service.0.registry.ready(), Err(crate::runtime::work::Error::Closed)));
                let diagnostic = format!("{error:?} {error}");
                for secret in [input.mobile.as_ref(), input.token.as_ref(), prepared.code.as_ref(), wrong_code.as_ref()] {
                    assert!(!diagnostic.contains(secret));
                }
            }
            service.0.change_message(&input,receipt.id,&Change::Claim{worker_id:prepared.message.worker_id(),code:&prepared.code}).await.expect("original claim");
            let error=runtime.client().execute("INSERT INTO identity.claims (id,worker_id,created_at) VALUES($1,$2,clock_timestamp())",&[&message_id,&prepared.message.worker_id()]).await.expect_err("claim unique");
            assert_eq!(error.code(),Some(&SqlState::UNIQUE_VIOLATION));
            service.0.finish_message(&input,receipt.id,prepared.message.worker_id(),Conclusion::Sent).await.expect("sent");
            let error=runtime.client().execute("UPDATE identity.messages SET status='failed',finished_at=clock_timestamp() WHERE id=$1",&[&message_id]).await.expect_err("terminal immutable");
            assert_eq!(error.code(),Some(&SqlState::CHECK_VIOLATION));
            runtime.client().batch_execute("BEGIN").await.expect("begin");
            runtime.client().execute("INSERT INTO identity.messages(id,challenge_id,ordinal,worker_id,created_at,expires_at) VALUES($1,$2,2,$3,clock_timestamp(),clock_timestamp()+interval '300 seconds')",&[&message::Id::new(),&other_receipt.id,&worker::Id::new()]).await.expect("message before paired check");
            let error=runtime.client().batch_execute("COMMIT").await.expect_err("unpaired permit cannot commit");
            assert_eq!(error.code(),Some(&SqlState::CHECK_VIOLATION));
            runtime.terminate().await.expect("end runtime");
            stop(context,service).await;
            challenge.stop().await;
        });
    }

    #[test]
    fn concurrent_permits_share_the_deployment_limit_and_latest_order() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for daily in [1, 2] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let supplier = fixture::supplier::Supplier::start().await;
                let limits = message::Limits::new(
                    300,
                    daily,
                    message::limits::Mobile::default(),
                    message::limits::Source::default(),
                )
                .expect("limits");
                let (context, service) = start(&database, &challenge, &supplier, limits).await;
                let first_operation_id = Uuid::new_v4();
                let second_operation_id = Uuid::new_v4();
                let first_receipt = passed(
                    &database,
                    &service,
                    &mut challenge,
                    first_operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let second_receipt = passed(
                    &database,
                    &service,
                    &mut challenge,
                    second_operation_id,
                    "13900000000",
                    "192.0.2.2",
                )
                .await;
                let first = input(first_operation_id, "13800000000", "192.0.2.1");
                let second = input(second_operation_id, "13900000000", "192.0.2.2");
                let (first, second) = timeout(Duration::from_secs(10), async {
                    tokio::join!(
                        service.0.prepare(&first, first_receipt),
                        service.0.prepare(&second, second_receipt)
                    )
                })
                .await
                .expect("bounded concurrent permits");
                if daily == 1 {
                    let (permit, error) = match (first, second) {
                        (Ok((Preparation::Ready(permit), None)), Err(error))
                        | (Err(error), Ok((Preparation::Ready(permit), None))) => (permit, error),
                        _ => panic!("exactly one quota winner"),
                    };
                    assert!(matches!(error, Error::Quota { retry_at }
                        if retry_at == permit.message.created_at() + chrono::TimeDelta::hours(24)));
                } else {
                    let (Ok((Preparation::Ready(_), None)), Ok((Preparation::Ready(_), None))) =
                        (first, second)
                    else {
                        panic!("both permits fit the quota");
                    };
                }
                let reader = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("permit reader");
                let ordinals: Vec<i64> = reader
                    .client()
                    .query(
                        "SELECT ordinal FROM identity.messages ORDER BY ordinal",
                        &[],
                    )
                    .await
                    .expect("durable permit order")
                    .into_iter()
                    .map(|row| row.get(0))
                    .collect();
                assert_eq!(ordinals, if daily == 1 { vec![1] } else { vec![1, 2] });
                reader.terminate().await.expect("permit reader ends");
                stop(context, service).await;
                challenge.stop().await;
            }
        });
    }

    #[test]
    fn only_a_committed_newer_permit_for_the_same_mobile_prevents_sending() {
        use fixture::database_proxy::{Fault, Proxy};
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (second_mobile, rolled_back) in [
                ("13800000000", false),
                ("13800000000", true),
                ("13900000000", false),
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let context = Context::new(proxied(&database, &proxy))
                    .await
                    .expect("context");
                let service = Service::start(
                    Barrier::begin(context.clone()).await.expect("barrier"),
                    keys(),
                    crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                    challenge.client(),
                    client(&supplier),
                )
                .await
                .expect("service");
                let first_operation = Uuid::new_v4();
                let second_operation = Uuid::new_v4();
                let first_verified = passed(
                    &database,
                    &service,
                    &mut challenge,
                    first_operation,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let second_verified = passed(
                    &database,
                    &service,
                    &mut challenge,
                    second_operation,
                    second_mobile,
                    "192.0.2.1",
                )
                .await;
                let admin = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("fixture admin");
                // Age only the first history so a later same-mobile permit fits the
                // real quota rules without waiting a minute. Keep both fixed lifetimes.
                admin
                    .client()
                    .batch_execute(&format!(
                        r#"
                    BEGIN;
                    ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check;
                    UPDATE identity.challenges SET
                        created_at = created_at - interval '65 seconds',
                        expires_at = expires_at - interval '65 seconds',
                        solved_at = solved_at - interval '65 seconds',
                        finished_at = finished_at - interval '65 seconds'
                    WHERE id = '{}';
                    ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check;
                    CREATE FUNCTION identity.age_first_permit() RETURNS trigger
                    LANGUAGE plpgsql AS $$
                    BEGIN
                        IF NOT EXISTS (SELECT 1 FROM identity.messages) THEN
                            NEW.created_at := NEW.created_at - interval '65 seconds';
                            NEW.expires_at := NEW.expires_at - interval '65 seconds';
                        END IF;
                        RETURN NEW;
                    END
                    $$;
                    CREATE TRIGGER messages_age_fixture BEFORE INSERT ON identity.messages
                        FOR EACH ROW EXECUTE FUNCTION identity.age_first_permit();
                    CREATE FUNCTION identity.match_permit_deadline() RETURNS trigger
                    LANGUAGE plpgsql AS $$
                    BEGIN
                        SELECT expires_at INTO NEW.expires_at FROM identity.messages WHERE id = NEW.id;
                        RETURN NEW;
                    END
                    $$;
                    CREATE TRIGGER checks_deadline_fixture BEFORE INSERT ON identity.checks
                        FOR EACH ROW EXECUTE FUNCTION identity.match_permit_deadline();
                    COMMIT;
                "#,
                        first_verified.id()
                    ))
                    .await
                    .expect("first permit age fixture");
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("runtime observer");
                let sql = "SELECT message.id, message.worker_id, message.ordinal,
                                  message.created_at, message.expires_at, message.status, message.finished_at,
                                  (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                                  to_jsonb(checks)::text AS proof, checks.key_id, checks.hash,
                                  checks.expires_at AS check_expires_at,
                                  to_jsonb(challenge)::text AS challenge, challenge.id AS challenge_id,
                                  challenge.operation_id, challenge.finished_at AS verified_at,
                                  claim.worker_id AS claim_worker_id, claim.created_at AS claimed_at
                           FROM identity.messages message
                           JOIN identity.checks checks ON checks.id = message.id
                           JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                           LEFT JOIN identity.claims claim ON claim.id = message.id
                           ORDER BY message.ordinal";
                let read = async || {
                    timeout(Duration::from_secs(5), observer.client().query(sql, &[]))
                        .await
                        .expect("observation bounded")
                        .expect("record groups")
                };
                let (original, first, option) = tokio::select! {
                    biased;
                    _ = supplier.request() => panic!("permit observation must precede every send"),
                    result = async {
                        let spacing = service.0.dispatch.next_at.lock().await;
                        let first = sending(&service, first_operation, "13800000000", "192.0.2.1");
                        let original = timeout(Duration::from_secs(5), async {
                            loop {
                                let rows = read().await;
                                if rows.len() == 1 {
                                    break rows;
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("first permit committed while original code is retained");
                        assert_eq!(
                            original[0].get::<_, crate::identity::challenge::Id>("challenge_id"),
                            first_verified.id()
                        );
                        assert_eq!(original[0].get::<_, Uuid>("operation_id"), first_operation);
                        if rolled_back {
                            proxy.arm(Fault::RolledBack);
                        }
                        let second = sending(&service, second_operation, second_mobile, "192.0.2.1");
                        let option = if rolled_back {
                            timeout(Duration::from_secs(5), proxy.intercepted())
                                .await
                                .expect("new permit COMMIT intercepted");
                            let result = timeout(Duration::from_secs(5), second)
                                .await
                                .expect("new permit recovery bounded")
                                .expect("new sender");
                            assert!(matches!(result, Err(Error::NotCommitted { .. })));
                            // Read after recovery has crossed the original operation lock.
                            let row = observer
                                .client()
                                .query_one(
                                    "SELECT (SELECT count(*) FROM identity.messages) AS messages,
                                        (SELECT count(*) FROM identity.checks) AS checks,
                                        (SELECT count(*) FROM identity.claims) AS claims",
                                    &[],
                                )
                                .await
                                .expect("rollback leaves only the first permit");
                            assert_eq!(row.get::<_, i64>("messages"), 1);
                            assert_eq!(row.get::<_, i64>("checks"), 1);
                            assert_eq!(row.get::<_, i64>("claims"), 0);
                            None
                        } else {
                            timeout(Duration::from_secs(5), async {
                                while read().await.len() != 2 {
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("new permit committed before either claim");
                            Some(second)
                        };
                        let rows = read().await;
                        assert_eq!(rows.len(), if rolled_back { 1 } else { 2 });
                        for column in ["permit", "proof", "challenge"] {
                            assert_eq!(
                                rows[0].get::<_, &str>(column),
                                original[0].get::<_, &str>(column),
                                "{column}"
                            );
                        }
                        for row in &rows {
                            let created_at: DateTime<Utc> = row.get("created_at");
                            let expires_at: DateTime<Utc> = row.get("expires_at");
                            assert_eq!(expires_at - created_at, chrono::TimeDelta::seconds(300));
                            assert_eq!(row.get::<_, DateTime<Utc>>("check_expires_at"), expires_at);
                            assert!(row.get::<_, DateTime<Utc>>("verified_at") <= created_at);
                            assert!(expires_at - database.now().await > chrono::TimeDelta::seconds(60));
                            assert_eq!(row.get::<_, &str>("status"), "sending");
                            assert_eq!(row.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                            assert_eq!(row.get::<_, Option<worker::Id>>("claim_worker_id"), None);
                            assert_eq!(row.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                        }
                        if !rolled_back {
                            assert_eq!(
                                rows[1].get::<_, crate::identity::challenge::Id>("challenge_id"),
                                second_verified.id()
                            );
                            assert_eq!(rows[1].get::<_, Uuid>("operation_id"), second_operation);
                            assert!(rows[1].get::<_, i64>("ordinal") > rows[0].get::<_, i64>("ordinal"));
                            assert!(
                                rows[1].get::<_, DateTime<Utc>>("created_at")
                                    - rows[0].get::<_, DateTime<Utc>>("created_at")
                                    >= chrono::TimeDelta::seconds(60)
                            );
                        }
                        assert!(
                            !first.is_finished(),
                            "the original sending work is still retained"
                        );
                        drop(spacing);
                        (rows, first, option)
                    } => result,
                };
                let superseded = second_mobile == "13800000000" && !rolled_back;
                let mut expected: Vec<_> = original
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != 0 || !superseded)
                    .map(|(_, row)| row.get::<_, message::Id>("id"))
                    .collect();
                let mut claims = BTreeMap::new();
                while !expected.is_empty() {
                    let request = timeout(Duration::from_secs(5), supplier.request())
                        .await
                        .expect("eligible original sender calls once");
                    let fields = values(&request);
                    let id: message::Id = fields["OutId"].parse().expect("message identity");
                    let index = expected
                        .iter()
                        .position(|expected| *expected == id)
                        .expect("only an eligible message may call PNVS");
                    expected.remove(index);
                    let row = original
                        .iter()
                        .find(|row| row.get::<_, message::Id>("id") == id)
                        .expect("original permit");
                    assert_eq!(
                        fields["PhoneNumber"],
                        if row.get::<_, Uuid>("operation_id") == first_operation {
                            "13800000000"
                        } else {
                            second_mobile
                        }
                    );
                    let parameters: serde_json::Value =
                        serde_json::from_str(&fields["TemplateParam"]).expect("template");
                    let code: message::Code = parameters["code"]
                        .as_str()
                        .expect("code")
                        .parse()
                        .expect("six-digit code");
                    assert!(
                        keys()
                            .message
                            .fetch(row.get("key_id"))
                            .expect("original key")
                            .verify(id, &code, &row.get("hash"))
                            .expect("original code proof")
                    );
                    let rows = read().await;
                    let claimed = rows
                        .iter()
                        .find(|row| row.get::<_, message::Id>("id") == id)
                        .expect("durable claim before call");
                    for column in ["permit", "proof", "challenge"] {
                        assert_eq!(
                            claimed.get::<_, &str>(column),
                            row.get::<_, &str>(column),
                            "{column}"
                        );
                    }
                    assert_eq!(
                        claimed.get::<_, worker::Id>("claim_worker_id"),
                        row.get::<_, worker::Id>("worker_id")
                    );
                    assert_eq!(claimed.get::<_, &str>("status"), "sending");
                    let claimed_at: DateTime<Utc> = claimed.get("claimed_at");
                    assert!(claimed_at >= row.get::<_, DateTime<Utc>>("created_at"));
                    assert!(
                        claimed_at
                            <= row.get::<_, DateTime<Utc>>("expires_at") - chrono::TimeDelta::seconds(60)
                    );
                    claims.insert(id.to_string(), claimed_at);
                    // A newer permit supersedes the old one even when its send is rejected.
                    reply(request, if superseded { "failed" } else { "sent" });
                }
                let completion = async {
                    let first = timeout(Duration::from_secs(5), first)
                        .await
                        .expect("old sender bounded")
                        .expect("old sender")
                        .expect("old receipt");
                    let mut receipts = vec![first];
                    if let Some(second) = option {
                        receipts.push(
                            timeout(Duration::from_secs(5), second)
                                .await
                                .expect("new sender bounded")
                                .expect("new sender")
                                .expect("new receipt"),
                        );
                    }
                    // Inspect before replay so a later request cannot repair the conclusion.
                    let stored = read().await;
                    assert_eq!(stored.len(), original.len());
                    for ((row, original), receipt) in stored.iter().zip(&original).zip(&receipts) {
                        for column in ["permit", "proof", "challenge"] {
                            assert_eq!(
                                row.get::<_, &str>(column),
                                original.get::<_, &str>(column),
                                "{column}"
                            );
                        }
                        let id: message::Id = row.get("id");
                        let claimed_at = claims.get(&id.to_string()).copied();
                        assert_eq!(
                            row.get::<_, Option<DateTime<Utc>>>("claimed_at"),
                            claimed_at
                        );
                        assert_eq!(
                            row.get::<_, Option<worker::Id>>("claim_worker_id"),
                            claimed_at.map(|_| original.get::<_, worker::Id>("worker_id"))
                        );
                        let finished_at: DateTime<Utc> = row.get("finished_at");
                        assert!(
                            finished_at >= claimed_at.unwrap_or(original.get("created_at"))
                                && finished_at <= database.now().await
                        );
                        assert_eq!(
                            row.get::<_, &str>("status"),
                            if superseded { "failed" } else { "sent" }
                        );
                        assert_eq!(
                            receipt.status(),
                            if superseded {
                                message::Status::Failed { finished_at }
                            } else {
                                message::Status::Sent { finished_at }
                            }
                        );
                        assert_eq!(receipt.id(), id);
                        assert_eq!(
                            receipt.created_at(),
                            original.get::<_, DateTime<Utc>>("created_at")
                        );
                        assert_eq!(
                            receipt.expires_at(),
                            original.get::<_, DateTime<Utc>>("expires_at")
                        );
                        let operation: Uuid = original.get("operation_id");
                        let mobile = if operation == first_operation {
                            "13800000000"
                        } else {
                            second_mobile
                        };
                        assert_eq!(
                            timeout(
                                Duration::from_secs(5),
                                service.send(input(operation, mobile, "192.0.2.1"))
                            )
                            .await
                            .expect("replay bounded")
                            .expect("original conclusion"),
                            *receipt
                        );
                    }
                    admin.terminate().await.expect("fixture admin ended");
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.request() => panic!("new permit resolution, replay and drain must not resend"),
                    () = completion => {}
                }
                observer.terminate().await.expect("observer ended");
                challenge.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn a_failed_clock_or_two_failed_writes_never_repeats_the_supplier_call() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for mode in ["sent", "lost", "unknown"] {
                for rollback in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut challenge = Supplier::new().await;
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let proxy = fixture::database_proxy::Proxy::start(&server).await;
                    let (context, service) = start_on(
                        proxied(&database, &proxy), &challenge, &supplier, message::Limits::default()
                    ).await;
                    let operation_id = Uuid::new_v4();
                    let receipt = passed(
                        &database,
                        &service,
                        &mut challenge,
                        operation_id,
                        "13800000000",
                        "192.0.2.1",
                    )
                    .await;
                    let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
                    let request = timeout(Duration::from_secs(5), supplier.request())
                        .await
                        .expect("call");
                    let parameters: serde_json::Value =
                        serde_json::from_str(&values(&request)["TemplateParam"])
                            .expect("fictional template");
                    let code = parameters["code"].as_str().expect("fictional code");
                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("observer");
                    let sql = "SELECT message.id, message.worker_id, message.created_at,
                                              message.expires_at, message.status, message.finished_at,
                                              to_jsonb(message)::text AS facts,
                                              (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                                              to_jsonb(checks)::text AS proof, to_jsonb(claim)::text AS claim,
                                              to_jsonb(challenge)::text AS challenge,
                                              claim.worker_id AS claim_worker_id
                                       FROM identity.messages message
                                       JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                                       JOIN identity.checks checks ON checks.id = message.id
                                       JOIN identity.claims claim ON claim.id = message.id
                                       WHERE message.challenge_id = $1";
                    let read = async || {
                        timeout(
                            Duration::from_secs(5),
                            observer.client().query_one(sql, &[&receipt.id()]),
                        )
                        .await
                        .expect("fact observation bounded")
                        .expect("original record group remains")
                    };
                    let original = read().await;
                    assert_eq!(original.get::<_, &str>("status"), "sending");
                    assert_eq!(
                        original.get::<_, Option<DateTime<Utc>>>("finished_at"),
                        None
                    );
                    assert_eq!(
                        original.get::<_, message::Id>("id").to_string(),
                        values(&request)["OutId"]
                    );
                    assert_eq!(
                        original.get::<_, worker::Id>("worker_id"),
                        original.get::<_, worker::Id>("claim_worker_id")
                    );
                    let assert_original = |actual: &tokio_postgres::Row| {
                        for column in ["permit", "proof", "claim", "challenge"] {
                            assert_eq!(
                                actual.get::<_, &str>(column),
                                original.get::<_, &str>(column),
                                "{column}"
                            );
                        }
                    };
                    let admin = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("fixture admin");
                    if rollback {
                        proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
                    } else {
                        admin.client().batch_execute("CREATE FUNCTION identity.reject_message() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fictional-sensitive-detail' USING ERRCODE='23514'; END $$; CREATE TRIGGER messages_rejected BEFORE UPDATE ON identity.messages FOR EACH ROW EXECUTE FUNCTION identity.reject_message()").await.expect("failed local writes fixture");
                    }
                    reply(request, mode);
                    if rollback {
                        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("conclusion observes rewound time");
                        proxy.release();
                    }
                    let recovery = async {
                        let error = timeout(Duration::from_secs(5), task)
                            .await
                            .expect("bounded writes")
                            .expect("worker")
                            .expect_err("conclusion not saved");
                        let persistence = if mode == "sent" {
                            &error
                        } else {
                            let Error::MessageConclusion { cause, persistence } = &error else {
                                panic!("supplier failure and recording failure are both retained");
                            };
                            match mode {
                                "lost" => {
                                    assert!(matches!(
                                        cause.as_ref(),
                                        Error::MessageRequest { cause: message::request::Error::Reqwest(_), .. }
                                    ));
                                }
                                "unknown" => {
                                    assert!(matches!(
                                        cause.as_ref(),
                                        Error::MessageOutcome { reason: message::response::Unknown::UnrecognizedCode(_), .. }
                                    ));
                                }
                                _ => unreachable!(),
                            }
                            persistence.as_ref()
                        };
                        let Error::Unknown { cause, recovery } = persistence else {
                            panic!("recording retains the unresolved commit outcome");
                        };
                        if rollback {
                            assert!(matches!(
                                cause.as_ref(),
                                Error::Runtime(crate::runtime::Error::ClockInvalid)
                            ));
                            assert!(matches!(
                                recovery.as_ref(),
                                Error::Runtime(crate::runtime::Error::ClockClosed)
                            ));
                        } else {
                            for failure in [cause, recovery] {
                                let Error::Query(query) = failure.as_ref() else {
                                    panic!("local write must fail at the database");
                                };
                                assert_eq!(
                                    query.code(),
                                    Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
                                );
                            }
                        }
                        assert_eq!(error.unreliable(), rollback);
                        let diagnostic = format!("{error:?} {error}");
                        for secret in [
                            "fictional-sensitive-detail",
                            "fictional-secret",
                            "13800000000",
                            code,
                        ] {
                            assert!(!diagnostic.contains(secret));
                        }
                        let unresolved = read().await;
                        assert_original(&unresolved);
                        assert_eq!(
                            unresolved.get::<_, &str>("facts"),
                            original.get::<_, &str>("facts")
                        );
                        if rollback {
                            assert!(matches!(
                                timeout(
                                    Duration::from_secs(5),
                                    service.send(input(operation_id, "13800000000", "192.0.2.1"))
                                )
                                .await
                                .expect("closed admission bounded"),
                                Err(Error::Closed)
                            ));
                            let closed = read().await;
                            assert_original(&closed);
                            assert_eq!(
                                closed.get::<_, &str>("facts"),
                                original.get::<_, &str>("facts")
                            );
                        } else {
                            admin.client().batch_execute("DROP TRIGGER messages_rejected ON identity.messages; DROP FUNCTION identity.reject_message()").await.expect("remove fixture fault");
                            let closing_at = database.now().await;
                            let receipt = timeout(
                                Duration::from_secs(5),
                                service.send(input(operation_id, "13800000000", "192.0.2.1")),
                            )
                            .await
                            .expect("orphan recovery bounded")
                            .expect("orphan result");
                            // Inspect before replay so another request cannot repair the conclusion.
                            let stored = read().await;
                            assert_original(&stored);
                            assert_eq!(stored.get::<_, &str>("status"), "unknown");
                            let finished_at: DateTime<Utc> = stored.get("finished_at");
                            assert!(finished_at >= closing_at && finished_at <= database.now().await);
                            assert_eq!(receipt.status(), message::Status::Unknown { finished_at });
                            assert_eq!(receipt.id(), original.get::<_, message::Id>("id"));
                            assert_eq!(
                                receipt.created_at(),
                                original.get::<_, DateTime<Utc>>("created_at")
                            );
                            assert_eq!(
                                receipt.expires_at(),
                                original.get::<_, DateTime<Utc>>("expires_at")
                            );
                            assert_eq!(
                                timeout(
                                    Duration::from_secs(5),
                                    service.send(input(operation_id, "13800000000", "192.0.2.1"))
                                )
                                .await
                                .expect("replay bounded")
                                .expect("original conclusion"),
                                receipt
                            );
                        }
                        admin.terminate().await.expect("end fixture admin");
                        timeout(Duration::from_secs(5), stop(context, service))
                            .await
                            .expect("drain bounded");
                    };
                    tokio::select! {
                        biased;
                        _ = supplier.request() => {
                            panic!("failed writes, recovery and drain must not resend")
                        }
                        () = recovery => {}
                    }
                    observer.terminate().await.expect("observer ended");
                    challenge.stop().await;
                    proxy.stop().await;
                }
            }
        });
    }

    #[test]
    fn insufficient_send_validity_persists_failure_before_and_after_claim_commit() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for delayed_commit in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let (context, service) =
                    start(&database, &challenge, &supplier, message::Limits::default()).await;
                let operation_id = Uuid::new_v4();
                let verified = passed(
                    &database,
                    &service,
                    &mut challenge,
                    operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let admin = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("fixture admin");
                // Age this isolated history instead of waiting five minutes. Keep the
                // Challenge's event order, the permit's lifetime and the paired deadline.
                let age = if delayed_commit { 235 } else { 241 };
                admin
                    .client()
                    .batch_execute(&format!(
                        r#"
                        BEGIN;
                        ALTER TABLE identity.challenges DISABLE TRIGGER challenges_transition_check;
                        UPDATE identity.challenges SET
                            created_at = created_at - interval '{age} seconds',
                            expires_at = expires_at - interval '{age} seconds',
                            solved_at = solved_at - interval '{age} seconds',
                            finished_at = finished_at - interval '{age} seconds';
                        ALTER TABLE identity.challenges ENABLE TRIGGER challenges_transition_check;
                        CREATE FUNCTION identity.age_permit() RETURNS trigger
                        LANGUAGE plpgsql AS $$
                        BEGIN
                            NEW.expires_at := NEW.expires_at - interval '{age} seconds';
                            IF TG_TABLE_NAME = 'messages' THEN
                                NEW.created_at := NEW.created_at - interval '{age} seconds';
                            END IF;
                            RETURN NEW;
                        END
                        $$;
                        CREATE TRIGGER messages_age_fixture BEFORE INSERT ON identity.messages
                            FOR EACH ROW EXECUTE FUNCTION identity.age_permit();
                        CREATE TRIGGER checks_age_fixture BEFORE INSERT ON identity.checks
                            FOR EACH ROW EXECUTE FUNCTION identity.age_permit();
                        COMMIT;
                        "#
                    ))
                    .await
                    .expect("near-expiry fixture facts");
                if delayed_commit {
                    admin
                        .client()
                        .batch_execute(
                            r#"
                            SELECT pg_advisory_lock(20261009, 1);
                            CREATE FUNCTION identity.hold_claim_commit() RETURNS trigger
                            LANGUAGE plpgsql AS $$
                            BEGIN
                                PERFORM pg_advisory_xact_lock(20261009, 1);
                                RETURN NULL;
                            END
                            $$;
                            CREATE CONSTRAINT TRIGGER claims_commit_fixture
                                AFTER INSERT ON identity.claims DEFERRABLE INITIALLY DEFERRED
                                FOR EACH ROW EXECUTE FUNCTION identity.hold_claim_commit();
                            "#,
                        )
                        .await
                        .expect("claim commit gate");
                }
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("runtime observer");
                let sql = "SELECT message.id, message.worker_id, message.created_at,
                                  message.expires_at, message.status, message.finished_at,
                                  (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                                  to_jsonb(checks)::text AS proof,
                                  checks.expires_at AS check_expires_at,
                                  to_jsonb(challenge)::text AS challenge,
                                  challenge.finished_at AS verified_at,
                                  claim.worker_id AS claim_worker_id, claim.created_at AS claimed_at
                           FROM identity.messages message
                           JOIN identity.checks checks ON checks.id = message.id
                           JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                           LEFT JOIN identity.claims claim ON claim.id = message.id
                           WHERE message.challenge_id = $1";
                let read = async || {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_opt(sql, &[&verified.id()]),
                    )
                    .await
                    .expect("fact observation bounded")
                    .expect("fact observation")
                };
                let work = async {
                    let spacing = service.0.dispatch.next_at.lock().await;
                    let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
                    let original = timeout(Duration::from_secs(5), async {
                        loop {
                            if let Some(row) = read().await {
                                break row;
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("original permit committed before claim");
                    let created_at: DateTime<Utc> = original.get("created_at");
                    let expires_at: DateTime<Utc> = original.get("expires_at");
                    let cutoff = expires_at - chrono::TimeDelta::seconds(60);
                    assert_eq!(expires_at - created_at, chrono::TimeDelta::seconds(300));
                    assert!(original.get::<_, DateTime<Utc>>("verified_at") <= created_at);
                    assert_eq!(
                        original.get::<_, DateTime<Utc>>("check_expires_at"),
                        expires_at
                    );
                    assert_eq!(original.get::<_, &str>("status"), "sending");
                    assert_eq!(
                        original.get::<_, Option<DateTime<Utc>>>("finished_at"),
                        None
                    );
                    assert_eq!(
                        original.get::<_, Option<worker::Id>>("claim_worker_id"),
                        None
                    );
                    assert_eq!(original.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                    let released_at;
                    if delayed_commit {
                        assert!(
                            database.now().await < cutoff,
                            "fixture still permits a claim"
                        );
                        drop(spacing);
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let waiting: bool = observer
                                    .client()
                                    .query_one(
                                        "SELECT EXISTS (
                                        SELECT 1 FROM pg_catalog.pg_locks locks
                                        JOIN pg_catalog.pg_stat_activity activity USING (pid)
                                        WHERE locks.locktype = 'advisory' AND NOT locks.granted
                                          AND locks.mode = 'ExclusiveLock'
                                          AND locks.classid = 20261009 AND locks.objid = 1
                                          AND locks.objsubid = 2
                                          AND locks.database = (
                                              SELECT oid FROM pg_catalog.pg_database
                                              WHERE datname = current_database()
                                          )
                                          AND activity.query = 'COMMIT'
                                          AND cardinality(pg_catalog.pg_blocking_pids(locks.pid)) > 0
                                    )",
                                        &[],
                                    )
                                    .await
                                    .expect("claim COMMIT observation")
                                    .get(0);
                                if waiting {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("claim reaches deferred COMMIT gate");
                        let held = read().await.expect("permit while commit is held");
                        assert_eq!(held.get::<_, &str>("status"), "sending");
                        assert_eq!(held.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                        released_at = timeout(Duration::from_secs(8), async {
                            // Wait for the measured deadline, with a one-second margin;
                            // the gate, rather than a sleep, establishes the interleaving.
                            let release_at = cutoff + chrono::TimeDelta::seconds(1);
                            loop {
                                let now = database.now().await;
                                if now >= release_at {
                                    break now;
                                }
                                tokio::time::sleep((release_at - now).to_std().expect("future deadline")).await;
                            }
                        })
                        .await
                        .expect("commit delay stays inside transaction budget");
                        let unlocked: bool = admin
                            .client()
                            .query_one("SELECT pg_advisory_unlock(20261009, 1)", &[])
                            .await
                            .expect("release claim commit")
                            .get(0);
                        assert!(unlocked);
                    } else {
                        released_at = database.now().await;
                        assert!(
                            released_at > cutoff,
                            "less than sixty seconds remain before claim"
                        );
                        drop(spacing);
                    }
                    let receipt = timeout(Duration::from_secs(5), task)
                        .await
                        .expect("sender bounded")
                        .expect("sender task")
                        .expect("failed receipt");
                    // Observe before replay so another request cannot repair an unsaved result.
                    let stored = read().await.expect("persisted conclusion");
                    for column in ["permit", "proof", "challenge"] {
                        assert_eq!(
                            stored.get::<_, &str>(column),
                            original.get::<_, &str>(column),
                            "{column}"
                        );
                    }
                    assert_eq!(stored.get::<_, &str>("status"), "failed");
                    let claimed_at = stored.get::<_, Option<DateTime<Utc>>>("claimed_at");
                    assert_eq!(claimed_at.is_some(), delayed_commit);
                    if let Some(claimed_at) = claimed_at {
                        assert!(claimed_at >= created_at && claimed_at <= cutoff);
                    }
                    assert_eq!(
                        stored.get::<_, Option<worker::Id>>("claim_worker_id"),
                        claimed_at.map(|_| original.get::<_, worker::Id>("worker_id"))
                    );
                    let finished_at: DateTime<Utc> = stored.get("finished_at");
                    assert!(finished_at >= released_at && finished_at <= database.now().await);
                    assert_eq!(receipt.status(), message::Status::Failed { finished_at });
                    assert_eq!(receipt.id(), original.get::<_, message::Id>("id"));
                    assert_eq!(receipt.created_at(), created_at);
                    assert_eq!(receipt.expires_at(), expires_at);
                    assert_eq!(
                        timeout(
                            Duration::from_secs(5),
                            service.send(input(operation_id, "13800000000", "192.0.2.1"))
                        )
                        .await
                        .expect("replay bounded")
                        .expect("original result"),
                        receipt
                    );
                    admin.terminate().await.expect("fixture admin ended");
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("drain bounded");
                };
                tokio::select! {
                    biased;
                    _ = supplier.request() => panic!("insufficient validity must never call PNVS"),
                    () = work => {}
                }
                observer.terminate().await.expect("observer ended");
                challenge.stop().await;
            }
        });
    }

    #[test]
    fn construction_errors_survive_successful_and_failed_conclusion_writes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for rejected in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let (context, service) = start(&database, &challenge, &supplier, message::Limits::default()).await;
                let operation_id = Uuid::new_v4();
                let receipt = passed(&database, &service, &mut challenge, operation_id, "13800000000", "192.0.2.1").await;
                let input = input(operation_id, "13800000000", "192.0.2.1");
                let (Preparation::Ready(prepared), None) = service.0.prepare(&input, receipt).await.expect("original permit") else {
                    panic!("first message has its original code");
                };
                service.0.change_message(&input, receipt.id, &Change::Claim {
                    worker_id: prepared.message.worker_id(),
                    code: &prepared.code,
                }).await.expect("original claim");
                let error = service.0.dispatch.client.request(
                    message::request::Input {
                        mobile: &input.mobile,
                        code: &prepared.code,
                        message_id: prepared.message.id(),
                        valid_seconds: 59,
                        purpose: crate::identity::challenge::Purpose::Authentication,
                    },
                    message::request::Context { date: database.now().await, nonce: Uuid::new_v4() },
                ).expect_err("insufficient remaining validity rejects construction");
                let admin = database.connections("postgres").recover().await.expect("fixture admin");
                if rejected {
                    admin.client().batch_execute("CREATE FUNCTION identity.reject_message() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fictional-sensitive-detail' USING ERRCODE='23514'; END $$; CREATE TRIGGER messages_rejected BEFORE UPDATE ON identity.messages FOR EACH ROW EXECUTE FUNCTION identity.reject_message()").await.expect("failed local writes fixture");
                }
                let error = super::conclude(
                    Err(error.into()),
                    service.0.finish_message(&input, receipt.id, prepared.message.worker_id(), Conclusion::Failed).await,
                ).expect_err("construction failure remains visible");
                let cause = if rejected {
                    let Error::MessageConclusion { cause, persistence } = &error else {
                        panic!("construction and recording failures are both retained");
                    };
                    let Error::Unknown { cause: first, recovery } = persistence.as_ref() else {
                        panic!("both local writes remain unresolved");
                    };
                    for failure in [first, recovery] {
                        assert!(matches!(failure.as_ref(), Error::Query(query) if query.code() == Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)));
                    }
                    cause.as_ref()
                } else {
                    &error
                };
                assert!(matches!(cause, Error::MessageBuild(message::request::BuildError::Validity)));
                assert!(!format!("{error:?} {error}").contains("fictional-sensitive-detail"));
                let status: String = admin.client().query_one("SELECT status FROM identity.messages", &[]).await.expect("stored conclusion").get(0);
                assert_eq!(status, if rejected { "sending" } else { "failed" });
                assert!(timeout(Duration::from_millis(50), supplier.request()).await.is_err());
                admin.terminate().await.expect("end fixture admin");
                stop(context, service).await;
                challenge.stop().await;
            }
        });
    }

    #[test]
    fn signing_sample_starts_after_waiting_for_the_claim_connection() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut challenge = Supplier::new().await;
            let supplier = fixture::supplier::Supplier::start().await;
            let (context, service) =
                start(&database, &challenge, &supplier, message::Limits::default()).await;
            let operation_id = Uuid::new_v4();
            let receipt = passed(
                &database,
                &service,
                &mut challenge,
                operation_id,
                "13800000000",
                "192.0.2.1",
            )
            .await;
            let input = input(operation_id, "13800000000", "192.0.2.1");
            let (Preparation::Ready(prepared), None) =
                service.0.prepare(&input, receipt).await.expect("permit")
            else {
                panic!("new");
            };
            let first = context
                .connections()
                .lease()
                .await
                .expect("held first slot");
            let second = context
                .connections()
                .lease()
                .await
                .expect("held second slot");
            let change = Change::Claim {
                worker_id: prepared.message.worker_id(),
                code: &prepared.code,
            };
            let work = service.0.change_message(&input, receipt.id, &change);
            tokio::pin!(work);
            assert!(
                timeout(Duration::from_millis(100), &mut work)
                    .await
                    .is_err()
            );
            let released_at = tokio::time::Instant::now();
            first.release();
            second.release();
            let (_, sample, option) = timeout(Duration::from_secs(5), &mut work)
                .await
                .expect("claim budget")
                .expect("claim");
            assert!(option.is_none());
            assert!(
                sample.started_at() >= released_at,
                "pool wait is not added to a newer database time"
            );
            let date = super::sampled_date(sample).expect("signing time");
            assert!(date >= sample.observed_at());
            stop(context, service.clone()).await;
            challenge.stop().await;
        });
    }

    #[test]
    fn concurrent_sends_keep_the_interval_after_success_rejection_and_response_loss() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for mode in ["sent", "failed", "lost"] {
                timeout(Duration::from_secs(20), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut challenge = Supplier::new().await;
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let context = Context::new(database.connections(&database.user))
                        .await
                        .expect("context");
                    let service = Service::start(
                        Barrier::begin(context.clone()).await.expect("barrier"),
                        keys(),
                        crate::startup::Settings::new(8, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings())
                            .expect("settings"),
                        challenge.client(),
                        // Leave time to observe the second committed permit while holding the first reply.
                        client_with_budget(&supplier, 5000),
                    )
                    .await
                    .expect("service");
                    let inputs = [
                        (Uuid::new_v4(), "13800000000", "192.0.2.1"),
                        (Uuid::new_v4(), "13900000000", "192.0.2.2"),
                    ];
                    for &(operation, mobile, source) in &inputs {
                        passed(
                            &database, &service, &mut challenge, operation, mobile, source,
                        )
                        .await;
                    }
                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("runtime observer");
                    let sql = "SELECT message.id, message.status, message.finished_at,
                                      message.worker_id, claim.worker_id AS claim_worker_id
                               FROM identity.messages message
                               JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                               LEFT JOIN identity.claims claim ON claim.id = message.id
                               WHERE challenge.operation_id = $1";
                    let first = sending(&service, inputs[0].0, inputs[0].1, inputs[0].2);
                    let request = supplier.request().await;
                    let received_at = request.received_at;
                    let first_id: message::Id = values(&request)["OutId"]
                        .parse()
                        .expect("first message");
                    assert_eq!(values(&request)["PhoneNumber"], inputs[0].1);
                    let row = observer
                        .client()
                        .query_one(sql, &[&inputs[0].0])
                        .await
                        .expect("first committed claim");
                    assert_eq!(row.get::<_, message::Id>("id"), first_id);
                    assert_eq!(
                        row.get::<_, worker::Id>("claim_worker_id"),
                        row.get::<_, worker::Id>("worker_id")
                    );
                    assert_eq!(row.get::<_, &str>("status"), "sending");

                    let second = sending(&service, inputs[1].0, inputs[1].1, inputs[1].2);
                    let second_id = timeout(Duration::from_secs(3), async {
                        loop {
                            if let Some(row) = observer
                                .client()
                                .query_opt(sql, &[&inputs[1].0])
                                .await
                                .expect("second permit observation")
                            {
                                break row.get::<_, message::Id>("id");
                            }
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("second permit commits while the first response is held");
                    assert_ne!(first_id, second_id);
                    assert!(!first.is_finished());
                    // The fixture keeps accepting while this response is held. Its timestamp
                    // exposes an early second request even if it is already queued here.
                    let preparation = received_at.elapsed();
                    assert!(
                        preparation < Duration::from_secs(1),
                        "{mode}: setup took {preparation:?}, exhausting the measured interval"
                    );
                    reply(request, mode);
                    let request = supplier.request().await;
                    assert_eq!(values(&request)["OutId"], second_id.to_string());
                    assert_eq!(values(&request)["PhoneNumber"], inputs[1].1);
                    let interval = request.received_at.duration_since(received_at);
                    assert!(
                        interval >= Duration::from_secs(1),
                        "{mode}: supplier requests were only {interval:?} apart"
                    );
                    let row = observer
                        .client()
                        .query_one(sql, &[&inputs[1].0])
                        .await
                        .expect("second committed claim");
                    assert_eq!(
                        row.get::<_, worker::Id>("claim_worker_id"),
                        row.get::<_, worker::Id>("worker_id")
                    );
                    assert_eq!(row.get::<_, &str>("status"), "sending");
                    reply(request, "sent");

                    let completion = async {
                        let first = first.await.expect("first sender");
                        if mode == "lost" {
                            assert!(matches!(
                                first,
                                Err(Error::MessageRequest { cause: message::request::Error::Reqwest(ref error), .. })
                                    if !error.is_timeout()
                            ));
                        } else {
                            assert_eq!(first.expect("first conclusion").id(), first_id);
                        }
                        let second = second
                            .await
                            .expect("second sender")
                            .expect("second conclusion");
                        assert_eq!(second.id(), second_id);
                        // Inspect both durable conclusions before replay can repair either one.
                        let mut originals = Vec::new();
                        for (index, &(operation, _, _)) in inputs.iter().enumerate() {
                            let row = observer
                                .client()
                                .query_one(sql, &[&operation])
                                .await
                                .expect("durable conclusion");
                            let expected = if index == 1 || mode == "sent" {
                                "sent"
                            } else if mode == "failed" {
                                "failed"
                            } else {
                                "unknown"
                            };
                            assert_eq!(row.get::<_, &str>("status"), expected);
                            let _: DateTime<Utc> = row.get("finished_at");
                            originals.push(row);
                        }
                        for (index, &(operation, mobile, source)) in inputs.iter().enumerate() {
                            let replay = service
                                .send(input(operation, mobile, source))
                                .await
                                .expect("original conclusion");
                            let row = &originals[index];
                            assert_eq!(replay.id(), row.get::<_, message::Id>("id"));
                            let finished_at = row.get("finished_at");
                            let expected = match row.get::<_, &str>("status") {
                                "sent" => message::Status::Sent { finished_at },
                                "failed" => message::Status::Failed { finished_at },
                                _ => message::Status::Unknown { finished_at },
                            };
                            assert_eq!(replay.status(), expected);
                        }
                        stop(context, service).await;
                    };
                    tokio::select! {
                        biased;
                        _ = supplier.request() => panic!("completion, replay and drain must not send again"),
                        () = completion => {}
                    }
                    observer.terminate().await.expect("observer ended");
                    supplier.stop().await;
                    challenge.stop().await;
                })
                .await
                .expect("same-process sending scenario bounded");
            }
        });
    }

    #[test]
    fn restarting_does_not_reset_actual_send_spacing() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut challenge = Supplier::new().await;
            let mut supplier = fixture::supplier::Supplier::start().await;
            let (context, service) =
                start(&database, &challenge, &supplier, message::Limits::default()).await;
            let operation_id = Uuid::new_v4();
            passed(
                &database,
                &service,
                &mut challenge,
                operation_id,
                "13800000000",
                "192.0.2.1",
            )
            .await;
            let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("first request");
            let first = tokio::time::Instant::now();
            reply(request, "sent");
            task.await.expect("first worker").expect("first outcome");
            stop(context, service).await;
            let (context, service) =
                start(&database, &challenge, &supplier, message::Limits::default()).await;
            let operation_id = Uuid::new_v4();
            passed(
                &database,
                &service,
                &mut challenge,
                operation_id,
                "13900000000",
                "192.0.2.2",
            )
            .await;
            let task = sending(&service, operation_id, "13900000000", "192.0.2.2");
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("second request");
            assert!(first.elapsed() >= Duration::from_secs(1));
            reply(request, "sent");
            task.await.expect("second worker").expect("second outcome");
            stop(context, service).await;
            challenge.stop().await;
        });
    }

    #[test]
    fn persists_each_outcome_and_replays_original_input_without_resending() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for mode in ["sent", "failed", "lost", "unknown"] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let (context, service) =
                    start(&database, &challenge, &supplier, message::Limits::default()).await;
                let operation_id = Uuid::new_v4();
                passed(
                    &database,
                    &service,
                    &mut challenge,
                    operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
                let request = timeout(Duration::from_secs(5), supplier.request())
                    .await
                    .expect("PNVS call");
                let fields = values(&request);
                let message_id: message::Id = fields["OutId"].parse().expect("message id");
                let template: serde_json::Value =
                    serde_json::from_str(&fields["TemplateParam"]).expect("template");
                let code: message::Code = template["code"]
                    .as_str()
                    .expect("code")
                    .parse()
                    .expect("six digits");
                assert_eq!(fields["PhoneNumber"], "13800000000");
                assert_eq!(fields["AutoRetry"], "0");
                let reader = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("reader");
                let row = reader
                    .client()
                    .query_one(
                        "SELECT hash, key_id FROM identity.checks WHERE id = $1",
                        &[&message_id],
                    )
                    .await
                    .expect("committed check before sending");
                assert!(
                    keys()
                        .message
                        .fetch(row.get("key_id"))
                        .expect("key")
                        .verify(message_id, &code, &row.get("hash"))
                        .expect("original code proof")
                );
                let claims: i64 = reader
                    .client()
                    .query_one(
                        "SELECT count(*) FROM identity.claims WHERE id=$1",
                        &[&message_id],
                    )
                    .await
                    .expect("claim")
                    .get(0);
                assert_eq!(claims, 1);
                reader.terminate().await.expect("reader ended");
                let duplicate = sending(&service, operation_id, "13800000000", "192.0.2.99");
                reply(request, mode);
                let result = task.await.expect("original worker");
                if matches!(mode, "lost" | "unknown") {
                    assert!(result.is_err());
                    assert!(!format!("{result:?}").contains("fictional-secret"));
                } else {
                    assert!(result.is_ok());
                }
                let original = duplicate
                    .await
                    .expect("duplicate")
                    .expect("original result");
                assert_eq!(original.id(), message_id);
                assert!(match mode {
                    "sent" => matches!(original.status(), message::Status::Sent { .. }),
                    "failed" => matches!(original.status(), message::Status::Failed { .. }),
                    _ => matches!(original.status(), message::Status::Unknown { .. }),
                });
                assert!(matches!(
                    service
                        .send(input(operation_id, "13900000000", "192.0.2.1"))
                        .await,
                    Err(Error::Mismatch)
                ));
                stop(context, service).await;
                let (context, service) =
                    start(&database, &challenge, &supplier, message::Limits::default()).await;
                assert_eq!(
                    service
                        .send(input(operation_id, "13800000000", "192.0.2.2"))
                        .await
                        .expect("restart result"),
                    original
                );
                assert!(
                    timeout(Duration::from_millis(100), supplier.request())
                        .await
                        .is_err()
                );
                stop(context, service).await;
                challenge.stop().await;
            }
        });
    }

    #[test]
    fn restart_closes_orphans_without_transferring_the_original_send_claim() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for claimed in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut challenge = Supplier::new().await;
                let mut supplier = fixture::supplier::Supplier::start().await;
                let (context, service) =
                    start(&database, &challenge, &supplier, message::Limits::default()).await;
                let operation_id = Uuid::new_v4();
                let receipt = passed(
                    &database,
                    &service,
                    &mut challenge,
                    operation_id,
                    "13800000000",
                    "192.0.2.1",
                )
                .await;
                let input = input(operation_id, "13800000000", "192.0.2.1");
                let (Preparation::Ready(prepared), None) =
                    service.0.prepare(&input, receipt).await.expect("permit")
                else {
                    panic!("new permit");
                };
                if claimed {
                    service
                        .0
                        .change_message(
                            &input,
                            receipt.id,
                            &Change::Claim {
                                worker_id: prepared.message.worker_id(),
                                code: &prepared.code,
                            },
                        )
                        .await
                        .expect("claim");
                }
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("restart observer");
                let sql = "SELECT message.id, message.worker_id, message.created_at,
                                  message.expires_at, message.status, message.finished_at,
                                  (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                                  to_jsonb(checks)::text AS proof, to_jsonb(claim)::text AS claim,
                                  claim.worker_id AS claim_worker_id
                           FROM identity.messages message
                           JOIN identity.checks checks ON checks.id = message.id
                           LEFT JOIN identity.claims claim ON claim.id = message.id
                           WHERE message.challenge_id = $1";
                let read = async || {
                    timeout(
                        Duration::from_secs(5),
                        observer.client().query_one(sql, &[&receipt.id()]),
                    )
                    .await
                    .expect("restart observation bounded")
                    .expect("original permit remains")
                };
                let original = read().await;
                assert_eq!(original.get::<_, message::Id>("id"), prepared.message.id());
                assert_eq!(
                    original.get::<_, worker::Id>("worker_id"),
                    prepared.message.worker_id()
                );
                assert_eq!(original.get::<_, &str>("status"), "sending");
                assert_eq!(
                    original.get::<_, Option<DateTime<Utc>>>("finished_at"),
                    None
                );
                assert_eq!(
                    original.get::<_, Option<worker::Id>>("claim_worker_id"),
                    claimed.then_some(prepared.message.worker_id())
                );
                drop(prepared);
                // Build clients before monitoring so the request receiver stays available.
                let challenge_client = challenge.client();
                let message_client = client(&supplier);
                let recovery = async {
                    timeout(Duration::from_secs(5), stop(context, service))
                        .await
                        .expect("first drain bounded");
                    let (context, service) = timeout(Duration::from_secs(5), async {
                        let context = Context::new(database.connections(&database.user))
                            .await
                            .expect("context");
                        let service = Service::start(
                            Barrier::begin(context.clone()).await.expect("barrier"),
                            keys(),
                            crate::startup::Settings::new(8, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings())
                                .expect("settings"),
                            challenge_client,
                            message_client,
                        )
                        .await
                        .expect("service");
                        (context, service)
                    })
                    .await
                    .expect("restart bounded");
                    let restored = timeout(
                        Duration::from_secs(5),
                        service.send(self::input(operation_id, "13800000000", "192.0.2.1")),
                    )
                    .await
                    .expect("orphan recovery bounded")
                    .expect("orphan conclusion");
                    // Inspect before replay so another request cannot repair the conclusion.
                    let stored = read().await;
                    for column in ["permit", "proof"] {
                        assert_eq!(
                            stored.get::<_, &str>(column),
                            original.get::<_, &str>(column)
                        );
                    }
                    assert_eq!(
                        stored.get::<_, Option<&str>>("claim"),
                        original.get::<_, Option<&str>>("claim")
                    );
                    assert_eq!(
                        stored.get::<_, &str>("status"),
                        if claimed { "unknown" } else { "failed" }
                    );
                    let finished_at = stored.get("finished_at");
                    assert_eq!(
                        restored.status(),
                        if claimed {
                            message::Status::Unknown { finished_at }
                        } else {
                            message::Status::Failed { finished_at }
                        }
                    );
                    assert_eq!(restored.id(), original.get::<_, message::Id>("id"));
                    assert_eq!(
                        restored.created_at(),
                        original.get::<_, DateTime<Utc>>("created_at")
                    );
                    assert_eq!(
                        restored.expires_at(),
                        original.get::<_, DateTime<Utc>>("expires_at")
                    );
                    assert_eq!(
                        timeout(Duration::from_secs(5), service.send(input))
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
                    _ = supplier.request() => {
                        panic!("restart recovery, replay and drain must not send")
                    }
                    () = recovery => {}
                }
                observer.terminate().await.expect("restart observer ends");
                challenge.stop().await;
            }
        });
    }

    #[test]
    fn send_capacity_is_retained_until_conclusion_work_ends() {
        use fixture::database_proxy::{Fault, Proxy};

        fn assert_call(request: &fixture::supplier::Request, row: &Row, mobile: &str) {
            let fields = values(request);
            let id: message::Id = row.get("id");
            assert_eq!(fields["OutId"], id.to_string());
            assert_eq!(fields["PhoneNumber"], mobile);
            let parameters: serde_json::Value =
                serde_json::from_str(&fields["TemplateParam"]).expect("template");
            let code: message::Code = parameters["code"]
                .as_str()
                .expect("code")
                .parse()
                .expect("six-digit code");
            assert!(
                keys()
                    .message
                    .fetch(row.get("key_id"))
                    .expect("original key")
                    .verify(id, &code, &row.get("hash"))
                    .expect("original code proof")
            );
            assert_eq!(row.get::<_, &str>("status"), "sending");
            assert_eq!(row.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
            assert_eq!(
                row.get::<_, worker::Id>("claim_worker_id"),
                row.get::<_, worker::Id>("worker_id")
            );
            let claimed_at: DateTime<Utc> = row.get("claimed_at");
            assert!(claimed_at >= row.get::<_, DateTime<Utc>>("created_at"));
            assert!(
                claimed_at
                    <= row.get::<_, DateTime<Utc>>("expires_at") - chrono::TimeDelta::seconds(60)
            );
        }

        fn assert_original(row: &Row, original: &Row) {
            for column in ["permit", "proof", "challenge", "claim"] {
                assert_eq!(
                    row.get::<_, &str>(column),
                    original.get::<_, &str>(column),
                    "{column}"
                );
            }
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut challenge = Supplier::new().await;
            let mut supplier = fixture::supplier::Supplier::start().await;
            let proxy = Proxy::start(&server).await;
            let context = Context::new(proxied(&database, &proxy))
                .await
                .expect("context");
            let service = Service::start(
                Barrier::begin(context.clone()).await.expect("barrier"),
                keys(),
                crate::startup::Settings::new(4, message::Limits::default(), crate::identity::authentication::verification::tests::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::verification::tests::avatar_settings()).expect("settings"),
                challenge.client(),
                client(&supplier),
            )
            .await
            .expect("service");
            let inputs = [
                (Uuid::nil(), "13800000000", "192.0.2.1"),
                (Uuid::new_v4(), "13900000000", "192.0.2.2"),
                (Uuid::new_v4(), "13700000000", "192.0.2.3"),
                (Uuid::new_v4(), "13600000000", "192.0.2.4"),
            ];
            let mut verified = Vec::new();
            for &(operation, mobile, source) in &inputs {
                verified.push(
                    passed(
                        &database,
                        &service,
                        &mut challenge,
                        operation,
                        mobile,
                        source,
                    )
                    .await,
                );
            }
            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("runtime observer");
            let admin = database
                .connections("postgres")
                .recover()
                .await
                .expect("fixture row lock");
            let blocker: i32 = admin
                .client()
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .expect("blocker identity")
                .get(0);
            let sql =
                "SELECT message.id, message.worker_id, message.created_at, message.expires_at,
                              message.status, message.finished_at,
                              (to_jsonb(message) - 'status' - 'finished_at')::text AS permit,
                              to_jsonb(checks)::text AS proof, checks.key_id, checks.hash,
                              to_jsonb(challenge)::text AS challenge,
                              COALESCE(to_jsonb(claim)::text, 'null') AS claim,
                              claim.worker_id AS claim_worker_id, claim.created_at AS claimed_at
                       FROM identity.messages message
                       JOIN identity.checks checks ON checks.id = message.id
                       JOIN identity.challenges challenge ON challenge.id = message.challenge_id
                       LEFT JOIN identity.claims claim ON claim.id = message.id
                       WHERE challenge.operation_id = $1";
            let read = async |operation: Uuid| {
                timeout(
                    Duration::from_secs(5),
                    observer.client().query_one(sql, &[&operation]),
                )
                .await
                .expect("observation bounded")
                .expect("original record group")
            };
            let blocked = async || {
                // Independent SHA-256 vector for E("challenge/operation", nil UUID).
                // The other wait is bound to the fixture backend holding only message 2.
                let row = observer.client().query_one(
                    "SELECT EXISTS (
                         SELECT 1 FROM pg_catalog.pg_locks
                         WHERE locktype = 'advisory' AND NOT granted AND mode = 'ExclusiveLock'
                           AND classid = 2647903637 AND objid = 2112932477 AND objsubid = 1
                           AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())
                           AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0
                     ), EXISTS (
                         SELECT 1 FROM pg_catalog.pg_locks
                         WHERE NOT granted AND $1 = ANY(pg_catalog.pg_blocking_pids(pid))
                     )",
                    &[&blocker],
                ).await.expect("bound recovery and conclusion waits");
                (row.get::<_, bool>(0), row.get::<_, bool>(1))
            };
            let first = sending(&service, inputs[0].0, inputs[0].1, inputs[0].2);
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("first call");
            let mut original = vec![read(inputs[0].0).await];
            assert_call(&request, &original[0], inputs[0].1);
            proxy.arm(Fault::DelayedCommit);
            reply(request, "sent");
            tokio::select! {
                biased;
                _ = supplier.request() => panic!("the first send must not repeat during recovery"),
                () = async {
                    timeout(Duration::from_secs(5), proxy.intercepted())
                        .await
                        .expect("conclusion COMMIT intercepted");
                    timeout(Duration::from_secs(5), async {
                        while !blocked().await.0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("recovery waits for the original operation");
                } => {}
            }
            let second = sending(&service, inputs[1].0, inputs[1].1, inputs[1].2);
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("second call");
            original.push(read(inputs[1].0).await);
            assert_call(&request, &original[1], inputs[1].1);
            admin
                .client()
                .batch_execute("BEGIN")
                .await
                .expect("row lock transaction");
            let locked: message::Id = admin
                .client()
                .query_one(
                    "SELECT id FROM identity.messages WHERE id = $1 FOR UPDATE",
                    &[&original[1].get::<_, message::Id>("id")],
                )
                .await
                .expect("only the second conclusion is blocked")
                .get(0);
            assert_eq!(locked, original[1].get::<_, message::Id>("id"));
            reply(request, "sent");
            let (rejected, fourth) = tokio::select! {
                biased;
                _ = supplier.request() => panic!("full send capacity must not admit another call"),
                result = async {
                    timeout(Duration::from_secs(5), async {
                        while blocked().await != (true, true) {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("one recovery and one conclusion write remain in flight");
                    assert!(!first.is_finished());
                    assert!(!second.is_finished());
                    first.abort();
                    assert!(
                        first
                            .await
                            .expect_err("first waiter cancelled")
                            .is_cancelled()
                    );

                    let rejected = timeout(
                        Duration::from_secs(5),
                        service.send(input(inputs[2].0, inputs[2].1, inputs[2].2)),
                    )
                    .await
                    .expect("capacity rejection bounded")
                    .expect("failed message receipt");
                    let stored = read(inputs[2].0).await;
                    let finished_at: DateTime<Utc> = stored.get("finished_at");
                    assert_eq!(stored.get::<_, &str>("status"), "failed");
                    assert_eq!(stored.get::<_, Option<worker::Id>>("claim_worker_id"), None);
                    assert_eq!(stored.get::<_, Option<DateTime<Utc>>>("claimed_at"), None);
                    assert_eq!(rejected.id(), stored.get::<_, message::Id>("id"));
                    assert_eq!(rejected.status(), message::Status::Failed { finished_at });
                    original.push(stored);
                    assert_eq!(blocked().await, (true, true));
                    for (index, original) in original.iter().take(2).enumerate() {
                        let pending = read(inputs[index].0).await;
                        assert_original(&pending, original);
                        assert_eq!(pending.get::<_, &str>("status"), "sending");
                        assert_eq!(pending.get::<_, Option<DateTime<Utc>>>("finished_at"), None);
                    }

                    proxy.release();
                    // Verification waits for the cancelled sender's work to end, but
                    // cannot repair an SMS conclusion before the independent SQL read.
                    let completed = timeout(
                        Duration::from_secs(5),
                        service.verify(input(inputs[0].0, inputs[0].1, inputs[0].2)),
                    )
                    .await
                    .expect("original work completion bounded")
                    .expect("original verification");
                    assert_eq!(completed, verified[0]);
                    let stored = read(inputs[0].0).await;
                    assert_original(&stored, &original[0]);
                    assert_eq!(stored.get::<_, &str>("status"), "sent");
                    assert_eq!(blocked().await, (false, true));
                    assert!(!second.is_finished());
                    (
                        rejected,
                        sending(&service, inputs[3].0, inputs[3].1, inputs[3].2),
                    )
                } => result,
            };
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("released capacity admits a new send");
            original.push(read(inputs[3].0).await);
            assert_call(&request, &original[3], inputs[3].1);
            reply(request, "sent");
            let completion = async {
                let fourth = timeout(Duration::from_secs(5), fourth)
                    .await
                    .expect("fourth sender bounded")
                    .expect("fourth sender")
                    .expect("fourth receipt");
                assert_eq!(blocked().await, (false, true));
                assert!(!second.is_finished());
                admin
                    .client()
                    .batch_execute("ROLLBACK")
                    .await
                    .expect("release second conclusion");
                let second = timeout(Duration::from_secs(5), second)
                    .await
                    .expect("second sender bounded")
                    .expect("second sender")
                    .expect("second receipt");

                // Inspect every conclusion before an SMS replay could repair it.
                let mut stored = Vec::new();
                for (index, original) in original.iter().enumerate() {
                    let row = read(inputs[index].0).await;
                    assert_original(&row, original);
                    assert_eq!(
                        row.get::<_, &str>("status"),
                        if index == 2 { "failed" } else { "sent" }
                    );
                    let finished_at: DateTime<Utc> = row.get("finished_at");
                    assert!(
                        finished_at
                            >= row
                                .get::<_, Option<DateTime<Utc>>>("claimed_at")
                                .unwrap_or(row.get("created_at"))
                    );
                    assert!(finished_at <= database.now().await);
                    stored.push(row);
                }
                let counts = observer
                    .client()
                    .query_one(
                        "SELECT (SELECT count(*) FROM identity.messages),
                            (SELECT count(*) FROM identity.checks),
                            (SELECT count(*) FROM identity.claims)",
                        &[],
                    )
                    .await
                    .expect("exact durable permits and claims");
                assert_eq!(
                    (
                        counts.get::<_, i64>(0),
                        counts.get::<_, i64>(1),
                        counts.get::<_, i64>(2)
                    ),
                    (4, 4, 3)
                );
                for (index, row) in stored.iter().enumerate() {
                    let (operation, mobile, source) = inputs[index];
                    let replay = timeout(
                        Duration::from_secs(5),
                        service.send(input(operation, mobile, source)),
                    )
                    .await
                    .expect("replay bounded")
                    .expect("original conclusion");
                    let finished_at = row.get("finished_at");
                    assert_eq!(replay.id(), row.get::<_, message::Id>("id"));
                    assert_eq!(
                        replay.created_at(),
                        row.get::<_, DateTime<Utc>>("created_at")
                    );
                    assert_eq!(
                        replay.expires_at(),
                        row.get::<_, DateTime<Utc>>("expires_at")
                    );
                    assert_eq!(
                        replay.status(),
                        if index == 2 {
                            message::Status::Failed { finished_at }
                        } else {
                            message::Status::Sent { finished_at }
                        }
                    );
                    match index {
                        1 => assert_eq!(replay, second),
                        2 => assert_eq!(replay, rejected),
                        3 => assert_eq!(replay, fourth),
                        _ => {}
                    }
                    let after = read(operation).await;
                    assert_original(&after, row);
                    assert_eq!(after.get::<_, DateTime<Utc>>("finished_at"), finished_at);
                }
                admin.terminate().await.expect("fixture row lock ended");
                timeout(Duration::from_secs(5), stop(context, service))
                    .await
                    .expect("drain bounded");
            };
            tokio::select! {
                biased;
                _ = supplier.request() => panic!("conclusion completion, replay and drain must not resend"),
                () = completion => {}
            }
            observer.terminate().await.expect("observer ended");
            challenge.stop().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn cancelled_waiter_and_stop_still_wait_for_the_original_sending_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut challenge = Supplier::new().await;
            let mut supplier = fixture::supplier::Supplier::start().await;
            let (context, service) =
                start(&database, &challenge, &supplier, message::Limits::default()).await;
            let operation_id = Uuid::new_v4();
            passed(
                &database,
                &service,
                &mut challenge,
                operation_id,
                "13800000000",
                "192.0.2.1",
            )
            .await;
            let task = sending(&service, operation_id, "13800000000", "192.0.2.1");
            let request = timeout(Duration::from_secs(5), supplier.request())
                .await
                .expect("call");
            task.abort();
            assert!(task.await.expect_err("waiter cancelled").is_cancelled());
            let draining = service.drain();
            tokio::pin!(draining);
            assert!(
                timeout(Duration::from_millis(50), &mut draining)
                    .await
                    .is_err()
            );
            reply(request, "sent");
            timeout(Duration::from_secs(5), &mut draining)
                .await
                .expect("bounded drain")
                .expect("drained");
            context.stop().await.expect("stop");
            let reader = database
                .connections(&database.user)
                .recover()
                .await
                .expect("reader");
            assert_eq!(
                reader
                    .client()
                    .query_one("SELECT status FROM identity.messages", &[])
                    .await
                    .expect("outcome")
                    .get::<_, String>(0),
                "sent"
            );
            reader.terminate().await.expect("reader ended");
            challenge.stop().await;
        });
    }
}
