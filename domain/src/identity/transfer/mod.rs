//! Mobile transfer facts, self-service establishment, completion and retention.

pub mod completion;
mod database;
mod error;
mod qualification;
mod retention;

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
pub use completion::State;
pub(crate) use completion::database::find_by_id;
pub(crate) use database::{Original, find_by_operation};
pub use error::{Error, Failure};
use platform::cryptography;
pub(crate) use qualification::Target;
pub(in crate::identity) use qualification::{authenticate_sender, lock_scope, qualify_sender};
pub use retention::Retention;
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::role,
    database::transaction::{self, Completion, Known, Outcome},
    identity::{
        current::{self, Actor},
        keys::Keys,
        message,
        mobile::Mobile,
        operation, person, session,
    },
    kernel::App,
    runtime::{
        Context,
        registry::{Family, Registry},
        work::{self, Work},
    },
};

const BUDGET: Duration = Duration::from_secs(10);
pub(crate) const FAMILY: &[u8] = b"transfer/operation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = transfer::Id)]
pub struct Id(Uuid);

impl Id {
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = transfer::Transfer)]
pub struct Transfer {
    id: Id,
    #[schema(value_type = String, format = DateTime)]
    expires_at: DateTime<Utc>,
}

impl Transfer {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

pub struct Input {
    operation_id: operation::Id,
    message_id: message::Id,
    code: message::Code,
}

impl Input {
    pub fn new(operation_id: operation::Id, message_id: message::Id, code: message::Code) -> Self {
        Self {
            operation_id,
            message_id,
            code,
        }
    }
}

/// Canonical assisted input, separate from the current qualification required to use it.
pub(crate) struct Assistance<'a> {
    pub(crate) operation_id: operation::Id,
    pub(crate) operator_id: person::Id,
    pub(crate) person_id: person::Id,
    pub(crate) revision_id: person::revision::Id,
    pub(crate) session_id: session::Id,
    pub(crate) mobile: &'a Mobile,
    pub(crate) verified: &'a Verified,
}

impl cryptography::Input<9> for Assistance<'_> {
    fn fields(&self) -> [&[u8]; 9] {
        [
            b"nucleus/identity/mobile-transfer/v1",
            b"assisted",
            self.operation_id.as_bytes(),
            self.operator_id.as_bytes(),
            self.person_id.as_bytes(),
            self.revision_id.as_bytes(),
            self.session_id.as_bytes(),
            self.verified.as_bytes(),
            self.mobile.as_bytes(),
        ]
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(try_from = "bool")]
#[schema(as = transfer::Verified, value_type = bool)]
pub(crate) struct Verified(());

impl Verified {
    fn as_bytes(&self) -> &'static [u8] {
        b"verified"
    }
}

impl TryFrom<bool> for Verified {
    type Error = VerificationError;

    fn try_from(value: bool) -> Result<Self, Self::Error> {
        if value {
            Ok(Self(()))
        } else {
            Err(VerificationError)
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("identity verification must be explicitly confirmed")]
pub(crate) struct VerificationError;

struct Proof<'a> {
    input: &'a Input,
    person_id: person::Id,
    revision_id: person::revision::Id,
    session_id: session::Id,
}

impl cryptography::Input<9> for Proof<'_> {
    fn fields(&self) -> [&[u8]; 9] {
        [
            b"nucleus/identity/mobile-transfer/v1",
            b"self",
            self.input.operation_id.as_bytes(),
            self.person_id.as_bytes(),
            self.person_id.as_bytes(),
            self.revision_id.as_bytes(),
            self.session_id.as_bytes(),
            self.input.message_id.as_bytes(),
            self.input.code.as_bytes(),
        ]
    }
}

enum Conclusion {
    Created(Transfer),
    Rejected,
}

impl Conclusion {
    fn deliver(self) -> Result<Transfer, Error> {
        match self {
            Self::Created(transfer) => Ok(transfer),
            Self::Rejected => Err(Error::CodeInvalid),
        }
    }
}

#[derive(Clone)]
pub struct Service(Arc<Inner>);

struct Inner {
    app: App,
    accounts: crate::identity::account::Reader,
    context: Context,
    current: current::Service,
    roles: role::Reader,
    keys: Arc<Keys>,
    registry: Arc<Registry>,
    work: Arc<Work>,
}

pub(crate) async fn verify(
    client: &Client,
    app: &App,
    keys: &Keys,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    database::verify(client, keys, app.keys(), now).await?;
    Ok(completion::verify(client, app, keys, now).await?)
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service, keys: Arc<Keys>) -> Self {
        Self(Arc::new(Inner {
            app: app.clone(),
            accounts: crate::identity::account::Reader::new(app.clone(), Arc::clone(&keys)),
            context: app.context().clone(),
            current,
            keys,
            registry: Arc::clone(app.registry()),
            work: Arc::clone(app.work()),
            roles: role::Reader::new(app),
        }))
    }

    pub async fn create(&self, actor: Actor, input: Input) -> Result<Transfer, Error> {
        let inner = Arc::clone(&self.0);
        self.0
            .work
            .run(
                Arc::clone(&self.0.registry),
                Family::Transfer,
                input.operation_id,
                async move { inner.execute(&actor, &input).await?.deliver() },
            )
            .await
    }

    pub fn retention(&self) -> Retention {
        Retention::new(self.clone())
    }
}

impl Inner {
    async fn execute(&self, actor: &Actor, input: &Input) -> Result<Conclusion, Error> {
        let lease = self.context.connections().lease().await?;
        let deadline = Instant::now() + BUDGET;
        let mut attempted = false;
        let result = timeout_at(
            deadline,
            self.write(lease.client(), actor, input, &mut attempted),
        )
        .await;
        let cause = match result {
            Ok(Ok(outcome)) => {
                let (completion, cleanup) =
                    transaction::commit(outcome, deadline, lease.client().batch_execute("COMMIT"))
                        .await
                        .recycle(lease)
                        .await;
                match completion {
                    Completion::Known(Known { value, end }) => {
                        if end.is_err() || cleanup.is_err() {
                            tracing::error!(operation_id = %input.operation_id, stage = "transfer_completion", outcome = "known", commit = ?end, cleanup = ?cleanup);
                        }
                        return Ok(value);
                    }
                    Completion::Pending { error, .. } => Error::from(error).combine(cleanup),
                }
            }
            Ok(Err(cause)) if cause.rejected() => {
                let (completion, cleanup) = transaction::rollback(
                    cause,
                    deadline,
                    lease.client().batch_execute("ROLLBACK"),
                )
                .await
                .recycle(lease)
                .await;
                let cause = match completion.end {
                    Ok(()) => completion.value,
                    Err(rollback) => Error::Rollback {
                        cause: Box::new(completion.value),
                        rollback,
                    },
                };
                return Err(cause.combine(cleanup));
            }
            Ok(Err(cause)) => cause.combine(lease.discard().await),
            Err(cause) => Error::from(cause).combine(lease.discard().await),
        };
        if !attempted {
            return Err(cause);
        }
        match self.recover(actor, input).await {
            Ok((
                Known {
                    value: Some(value),
                    end,
                },
                termination,
            )) => {
                let unreliable = work::Failure::unreliable(&cause);
                if unreliable {
                    self.registry.close();
                }
                if unreliable || end.is_err() || termination.is_err() {
                    tracing::error!(operation_id = %input.operation_id, stage = "transfer_completion", outcome = "committed", error = ?cause, commit = ?end, termination = ?termination);
                }
                Ok(value)
            }
            Ok((
                Known {
                    value: None,
                    end: commit,
                },
                termination,
            )) => Err(Error::NotCommitted {
                cause: Box::new(cause),
                commit,
                termination,
            }),
            Err(recovery) => Err(Error::Unknown {
                cause: Box::new(cause),
                recovery: Box::new(recovery),
            }),
        }
    }

    async fn write(
        &self,
        client: &Client,
        actor: &Actor,
        input: &Input,
        attempted: &mut bool,
    ) -> Result<Outcome<Conclusion>, Error> {
        transaction::begin(&self.context, client, FAMILY, input.operation_id).await?;
        let option =
            database::find_by_operation(client, self.app.keys(), input.operation_id).await?;
        if let Some(original) = option {
            let result =
                original.deliver(&self.keys, actor, input, self.context.now(client).await?)?;
            actor.lock_account(client).await?;
            self.current.fetch_contact(client, actor).await?;
            return Ok(Outcome::Known(result));
        }
        actor.lock_accounts(client, actor.account_id()).await?;
        let contact = self.current.fetch_contact(client, actor).await?;
        transaction::object(client, b"identity/mobile", contact.mobile().as_bytes()).await?;
        database::lock_confirmation(client, input.message_id).await?;
        let contact = self.current.fetch_contact(client, actor).await?;
        let now = self.context.now(client).await?;
        let confirmation = database::find_confirmation(client, input.message_id)
            .await?
            .ok_or(Error::Verification)?;
        confirmation.qualify(&self.keys, &contact, now)?;
        let valid = confirmation.verify(&self.keys, &input.code)?;
        let original = database::Original::new(actor, input, &contact, &self.keys, now, valid)?;
        *attempted = true;
        original.insert(client).await?;
        Ok(Outcome::Pending(original.conclusion))
    }

    async fn recover(
        &self,
        actor: &Actor,
        input: &Input,
    ) -> Result<
        (
            Known<Option<Conclusion>>,
            Result<(), platform::database::Error>,
        ),
        Error,
    > {
        let session = self.context.connections().recover().await?;
        let deadline = Instant::now() + BUDGET;
        let result = async {
            let option = timeout_at(deadline, async {
                transaction::begin(&self.context, session.client(), FAMILY, input.operation_id)
                    .await?;
                let option = database::find_by_operation(
                    session.client(),
                    self.app.keys(),
                    input.operation_id,
                )
                .await?;
                let now = self.context.now(session.client()).await?;
                option
                    .map(|original| original.deliver(&self.keys, actor, input, now))
                    .transpose()
            })
            .await??;
            Ok::<_, Error>(
                transaction::confirm(option, deadline, session.client().batch_execute("COMMIT"))
                    .await,
            )
        }
        .await;
        match result {
            Ok(result) => Ok(result.terminate(session).await),
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::role::tests::actor,
        database::Barrier,
        identity::{
            authentication::{self, Database, Supplier, keys, server, session::tests::age},
            current::Actor,
            message, operation,
            transfer::{self, Error, Input},
        },
        runtime::Context,
        startup::{Services, Settings},
    };

    pub(crate) async fn start(
        database: &Database,
        supplier: &Supplier,
        sms: &fixture::supplier::Supplier,
    ) -> (Context, Services) {
        start_on(database.connections(&database.user), supplier, sms).await
    }

    pub(crate) async fn start_on(
        connections: platform::database::Connections,
        supplier: &Supplier,
        sms: &fixture::supplier::Supplier,
    ) -> (Context, Services) {
        let context = Context::new(connections).await.expect("runtime");
        let services = Box::pin(Services::start(
            Barrier::begin(context.clone()).await.expect("barrier"),
            authentication::inventory(keys()),
            Settings::new(
                8,
                message::Limits::default(),
                authentication::session_settings(),
                crate::registration::Limits::default(),
                authentication::avatar_settings(),
            )
            .expect("settings"),
            supplier.client(),
            authentication::dispatch::tests::client(sms),
        ))
        .await
        .expect("services");
        (context, services)
    }

    fn sending(operation_id: operation::Id, mobile: &str) -> authentication::Input {
        authentication::Input::new(
            operation_id,
            mobile.parse().expect("mobile"),
            operation_id.to_string().parse().expect("token"),
            "192.0.2.12".parse().expect("source"),
        )
    }

    pub(super) fn input(operation_id: operation::Id, message_id: message::Id, code: &str) -> Input {
        Input::new(operation_id, message_id, code.parse().expect("code"))
    }

    pub(super) fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    pub(super) async fn send(
        database: &Database,
        services: &Services,
        actor: &Actor,
        supplier: &mut Supplier,
        sms: &mut fixture::supplier::Supplier,
    ) -> (operation::Id, authentication::dispatch::Receipt, String) {
        send_to(database, services, actor, supplier, sms, "13900000000").await
    }

    pub(crate) async fn send_to(
        database: &Database,
        services: &Services,
        actor: &Actor,
        supplier: &mut Supplier,
        sms: &mut fixture::supplier::Supplier,
        mobile: &'static str,
    ) -> (operation::Id, authentication::dispatch::Receipt, String) {
        let operation_id = operation();
        let service = services.authentication().clone();
        let actor = actor.clone();
        let mut task = tokio::spawn(async move {
            service
                .send_current(actor, sending(operation_id, mobile))
                .await
        });
        let request = tokio::select! {
            request = supplier.request() => request,
            result = &mut task => panic!("sender stopped before verification: {result:?}"),
        };
        request.reply(200, json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_change_code","cdata":operation_id.to_string()}).to_string());
        let request = timeout(Duration::from_secs(5), sms.request())
            .await
            .expect("bounded SMS");
        let code = deliver(request, mobile, "change");
        (
            operation_id,
            task.await.expect("sender").expect("message"),
            code,
        )
    }

    pub(crate) fn deliver(
        request: fixture::supplier::Request,
        mobile: &str,
        template: &str,
    ) -> String {
        let target = std::str::from_utf8(&request.bytes)
            .expect("request")
            .lines()
            .next()
            .expect("line")
            .split(' ')
            .nth(1)
            .expect("target");
        let values: BTreeMap<String, String> =
            url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}"))
                .expect("URL")
                .query_pairs()
                .into_owned()
                .collect();
        assert_eq!(values["TemplateCode"], template);
        assert_eq!(values["PhoneNumber"], mobile);
        let parameters: serde_json::Value =
            serde_json::from_str(&values["TemplateParam"]).expect("parameters");
        let code = parameters["code"]
            .as_str()
            .expect("original generated code")
            .to_owned();
        let body = json!({"Code":"OK","Success":true,"Model":{"OutId":values["OutId"],"BizId":"fixture","RequestId":"57fdfbd7-e79d-465a-a121-eab3e91e7290"}}).to_string();
        request
            .reply
            .send(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .into_bytes(),
            )
            .expect("SMS response");
        code
    }

    #[test]
    fn current_phone_transfer_pairs_codes_and_replays_the_original_conclusion() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let account = services.current().read(&actor).await.expect("current account");
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (sending_id, sent, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                assert!(matches!(sent.status(), message::Status::Sent { .. }));
                // A later authentication-purpose code must not replace the current-phone proof.
                crate::access::role::tests::actor(&database, &services, &mut supplier).await;
                let observer = database.connections(&database.user).recover().await.expect("independent runtime reader");
                assert!(observer.client().query_one("SELECT confirmation.person_id=$2 AND confirmation.operator_id=$3 AND challenge.person_id=$2 AND challenge.session_id=$4 AND challenge.purpose='mobile_change' AND NOT EXISTS(SELECT 1 FROM identity.checks WHERE id=confirmation.id) FROM identity.confirmations confirmation JOIN identity.messages message ON message.id=confirmation.id JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE confirmation.id=$1", &[&sent.id(), &account.person_id(), &account.revision_id(), &actor.session_id()]).await.expect("independent purpose and identity facts").get::<_, bool>(0));
                let completion = Box::pin(async {
                    let wrong = if code == "000000" { "000001" } else { "000000" };
                    let rejected = operation();
                    for _ in 0..2 {
                        assert!(matches!(Box::pin(services.transfer().create(actor.clone(), input(rejected, sent.id(), wrong))).await, Err(Error::CodeInvalid)));
                    }
                    assert!(matches!(Box::pin(services.transfer().create(actor.clone(), input(rejected, sent.id(), &code))).await, Err(Error::Mismatch)));
                    assert_eq!(observer.client().query_one("SELECT failures FROM identity.confirmations WHERE id=$1", &[&sent.id()]).await.expect("one durable error").get::<_, i16>(0), 1);
                    let operation_id = operation();
                    let (first, replay) = tokio::join!(Box::pin(services.transfer().create(actor.clone(), input(operation_id, sent.id(), &code))), Box::pin(services.transfer().create(actor.clone(), input(operation_id, sent.id(), &code))));
                    let first = first.expect("created transfer");
                    assert_eq!(first, replay.expect("same original result"));
                    let fact = observer.client().query_one("SELECT id, created_at, person_id=$2 AND operator_id=$3 AND revision_id=$3 AND session_id=$4 AND message_id=$5 AS bound FROM identity.transfers WHERE operation_id=$1", &[&operation_id, &account.person_id(), &account.revision_id(), &actor.session_id(), &sent.id()]).await.expect("original fact before further requests");
                    assert_eq!(first.id(), fact.get("id")); assert!(fact.get::<_, bool>("bound"));
                    assert_eq!(first.expires_at(), fact.get::<_, chrono::DateTime<chrono::Utc>>("created_at") + TimeDelta::minutes(10));
                    assert!(matches!(Box::pin(services.transfer().create(actor.clone(), input(operation(), sent.id(), &code))).await, Err(Error::Verification)));
                    assert_eq!(Box::pin(services.authentication().send_current(actor.clone(), sending(sending_id, "13900000000"))).await.expect("same SMS fact").id(), sent.id());
                    assert!(matches!(Box::pin(services.authentication().send(sending(sending_id, "13900000000"))).await, Err(authentication::Error::Mismatch)));
                    assert!(matches!(Box::pin(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(), sent.id(), "13900000000", &code))).await, Err(authentication::Error::Verification)));
                    services.drain().await.expect("all writes ended");
                    context.stop().await.expect("runtime ended");
                });
                tokio::select! {
                    biased;
                    _ = sms.request() => panic!("replays and transfer creation never send another SMS"),
                    _ = supplier.observe() => panic!("replays never call verification again"),
                    () = completion => {},
                }
                observer.terminate().await.expect("reader ended");
                sms.stop().await; supplier.stop().await;
            })).await.expect("bounded complete flow");
        }));
    }

    #[test]
    fn confirmation_limits_session_binding_and_retention_survive_restart() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let first = actor(&database, &services, &mut supplier).await;
                let second = actor(&database, &services, &mut supplier).await;
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (_, original, original_code) = send(&database, &services, &first, &mut supplier, &mut sms).await;
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (sending_id, latest, code) = send(&database, &services, &first, &mut supplier, &mut sms).await;
                let observer = database.connections(&database.user).recover().await.expect("independent reader");
                let verification = Box::pin(async {
                    assert!(matches!(Box::pin(services.transfer().create(first.clone(), input(operation(), original.id(), &original_code))).await, Err(Error::Verification)));
                    assert_eq!(observer.client().query_one("SELECT failures FROM identity.confirmations WHERE id=$1", &[&original.id()]).await.expect("inapplicable code does not count").get::<_,i16>(0), 0);
                    assert!(matches!(Box::pin(services.authentication().send_current(second.clone(), sending(sending_id, "13900000000"))).await, Err(authentication::Error::Mismatch)));
                    assert!(matches!(Box::pin(services.authentication().send_current(first.clone(), sending(operation(), "13800000000"))).await, Err(authentication::Error::TargetChanged)));
                    let wrong = if code == "000000" { "000001" } else { "000000" };
                    let original_rejection = operation();
                    for index in 0..5 {
                        let operation_id = if index == 0 { original_rejection } else { operation() };
                        assert!(matches!(Box::pin(services.transfer().create(first.clone(), input(operation_id, latest.id(), wrong))).await, Err(Error::CodeInvalid)));
                    }
                    for proof in [&code, wrong] {
                        assert!(matches!(Box::pin(services.transfer().create(first.clone(), input(operation(), latest.id(), proof))).await, Err(Error::Verification)));
                    }
                    assert!(matches!(Box::pin(services.transfer().create(second.clone(), input(original_rejection, latest.id(), wrong))).await, Err(Error::Mismatch)));
                    assert!(observer.client().query_one("SELECT (SELECT failures=5 FROM identity.confirmations WHERE id=$1) AND (SELECT count(*)=5 FROM identity.rejections WHERE message_id=$1) AND NOT EXISTS(SELECT 1 FROM identity.transfers)", &[&latest.id()]).await.expect("exact five durable rejections").get::<_,bool>(0));
                    services.drain().await.expect("first process drained"); context.stop().await.expect("first runtime ended");
                    (original_rejection, wrong.to_owned())
                });
                let (original_rejection, wrong) = tokio::select! {
                    biased;
                    _ = sms.request() => panic!("verification and refusal never send SMS"),
                    _ = supplier.observe() => panic!("mismatched or original C1 never calls supplier"),
                    result = verification => result,
                };
                let (context, services) = start(&database, &supplier, &sms).await;
                assert!(matches!(Box::pin(services.transfer().create(first.clone(), input(original_rejection, latest.id(), &wrong))).await, Err(Error::CodeInvalid)));
                for sql in ["DELETE FROM identity.rejections", "DELETE FROM identity.confirmations"] {
                    let error = observer.client().batch_execute(sql).await.expect_err("fresh recovery facts retained");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                }
                age(&database, services.authentication(), TimeDelta::hours(26)).await;
                services.authentication().retention().run().await.expect("dependent original rejections retained");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.confirmations WHERE id=$1", &[&latest.id()]).await.expect("parent retained before rejection cleanup").get::<_,i64>(0), 1);
                services.transfer().retention().run().await.expect("expired original rejections removed");
                services.authentication().retention().run().await.expect("expired codes and messages removed");
                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.rejections) AND NOT EXISTS(SELECT 1 FROM identity.confirmations) AND NOT EXISTS(SELECT 1 FROM identity.messages)", &[]).await.expect("complete technical cleanup").get::<_,bool>(0));
                services.drain().await.expect("second process drained"); context.stop().await.expect("second runtime ended");
                observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded limits and cleanup");
        }));
    }

    #[test]
    fn transfer_recovers_lost_and_delayed_commits_without_recounting_or_calling_suppliers() {
        use std::sync::Arc;

        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for valid in [true, false] {
                for (fault, delayed, rolled_back) in [
                    (Fault::LostCommit, false, false),
                    (Fault::DelayedCommit, true, false),
                    (Fault::RolledBack, false, true),
                ] {
                    timeout(Duration::from_secs(25), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let mut sms = fixture::supplier::Supplier::start().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                        let actor = actor(&database, &services, &mut supplier).await;
                        age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                        let (_, sent, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                        let code = if valid { code } else if code == "000000" { "000001".into() } else { "000000".into() };
                        let observer = database.connections(&database.user).recover().await.expect("independent facts");
                        let operation_id = operation();
                        proxy.arm(fault);
                        let service = services.transfer().clone();
                        let original = actor.clone();
                        let command = input(operation_id, sent.id(), &code);
                        let task = tokio::spawn(async move { service.create(original, command).await });
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual transfer COMMIT fault");
                        if delayed {
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND mode='ExclusiveLock' AND NOT granted AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))", &[]).await.expect("recovery waits for the original transaction").get::<_,bool>(0) { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("bounded recovery wait");
                            task.abort(); assert!(task.await.expect_err("caller cancelled").is_cancelled());
                            assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1) AND NOT EXISTS(SELECT 1 FROM identity.rejections WHERE operation_id=$1)", &[&operation_id]).await.expect("pending transaction has no visible conclusion").get::<_,bool>(0));
                            proxy.release();
                        } else {
                            let result = task.await.expect("recovery worker");
                            if rolled_back {
                                assert!(matches!(result, Err(Error::NotCommitted { .. })));
                            } else if valid { result.expect("lost success recovered"); } else { assert!(matches!(result, Err(Error::CodeInvalid))); }
                        }
                        let verification = Box::pin(async {
                            if rolled_back {
                                assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1) AND NOT EXISTS(SELECT 1 FROM identity.rejections WHERE operation_id=$1) AND (SELECT failures=0 FROM identity.confirmations WHERE id=$2)", &[&operation_id, &sent.id()]).await.expect("confirmed rollback has no partial conclusion or count").get::<_,bool>(0));
                                let result = Box::pin(services.transfer().create(actor.clone(), input(operation_id, sent.id(), &code))).await;
                                if valid { result.expect("explicit original retry after rollback"); } else { assert!(matches!(result, Err(Error::CodeInvalid))); }
                            }
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1) OR EXISTS(SELECT 1 FROM identity.rejections WHERE operation_id=$1)", &[&operation_id]).await.expect("original commit before replay").get::<_,bool>(0) { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("independent original conclusion");
                            let counts = observer.client().query_one("SELECT (SELECT count(*) FROM identity.transfers WHERE operation_id=$1), (SELECT count(*) FROM identity.rejections WHERE operation_id=$1), (SELECT failures FROM identity.confirmations WHERE id=$2)", &[&operation_id, &sent.id()]).await.expect("atomic original outcome");
                            assert_eq!(counts.get::<_,i64>(0), i64::from(valid)); assert_eq!(counts.get::<_,i64>(1), i64::from(!valid)); assert_eq!(counts.get::<_,i16>(2), i16::from(!valid));
                            let result = Box::pin(services.transfer().create(actor, input(operation_id, sent.id(), &code))).await;
                            if valid { result.expect("same successful original"); } else { assert!(matches!(result, Err(Error::CodeInvalid))); }
                            assert_eq!(observer.client().query_one("SELECT failures FROM identity.confirmations WHERE id=$1", &[&sent.id()]).await.expect("replay never recounts").get::<_,i16>(0), i16::from(!valid));
                            services.drain().await.expect("recovery remains owned until drain"); context.stop().await.expect("runtime ended");
                        });
                        tokio::select! {
                            biased;
                            _ = sms.request() => panic!("recovery never repeats SMS"),
                            _ = supplier.observe() => panic!("recovery never repeats verification"),
                            () = verification => {},
                        }
                        observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                    })).await.expect("bounded original commit recovery");
                }
            }
        }));
    }

    #[test]
    fn revoked_sender_stops_new_effects_but_finishes_the_original_supplier_call() {
        use std::sync::Arc;

        use crate::identity::revocation::Scope;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for called_sms in [false, true] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let (context, services) = start(&database, &supplier, &sms).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                    let operation_id = operation();
                    let service = services.authentication().clone();
                    let original = actor.clone();
                    let task = tokio::spawn(async move { service.send_current(original, sending(operation_id, "13900000000")).await });
                    let verification = supplier.request().await;
                    let response = json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_change_code","cdata":operation_id.to_string()}).to_string();
                    let observer = database.connections(&database.user).recover().await.expect("independent reader");
                    if called_sms {
                        verification.reply(200, response);
                        let sent = timeout(Duration::from_secs(5), sms.request()).await.expect("actual claimed SMS");
                        Box::pin(services.revocation().revoke(actor.clone(), operation(), Scope::Current)).await.expect("revoke while SMS is outstanding");
                        let target = std::str::from_utf8(&sent.bytes).expect("wire").lines().next().expect("line").split(' ').nth(1).expect("target");
                        let message_id = url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("URL").query_pairs().find(|(name,_)|name=="OutId").expect("original message").1.into_owned();
                        let body = json!({"Code":"OK","Success":true,"Model":{"OutId":message_id,"BizId":"fixture","RequestId":"57fdfbd7-e79d-465a-a121-eab3e91e7290"}}).to_string();
                        sent.reply.send(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).into_bytes()).expect("original supplier response");
                    } else {
                        Box::pin(services.revocation().revoke(actor.clone(), operation(), Scope::Current)).await.expect("revoke while verification is outstanding");
                        verification.reply(200, response);
                    }
                    let conclusion = Box::pin(async {
                        let result = task.await.expect("original sender");
                        if called_sms {
                            let receipt = result.expect("original external conclusion persists after revocation");
                            assert!(matches!(receipt.status(), message::Status::Sent { .. }));
                            assert!(observer.client().query_one("SELECT message.status='sent' AND message.finished_at IS NOT NULL AND EXISTS(SELECT 1 FROM identity.claims WHERE id=message.id) FROM identity.messages message WHERE id=$1", &[&receipt.id()]).await.expect("independent conclusion before replay").get::<_,bool>(0));
                            assert!(matches!(Box::pin(services.transfer().create(actor.clone(), input(operation(),receipt.id(),"000000"))).await, Err(Error::Current(crate::identity::current::Error::Invalid))));
                        } else {
                            assert!(matches!(result, Err(authentication::Error::Current(crate::identity::current::Error::Invalid))));
                            assert!(observer.client().query_one("SELECT challenge.status='passed' AND NOT EXISTS(SELECT 1 FROM identity.messages WHERE challenge_id=challenge.id) FROM identity.challenges challenge WHERE operation_id=$1", &[&operation_id]).await.expect("verification conclusion saved without a new sending permit").get::<_,bool>(0));
                        }
                        assert!(matches!(Box::pin(services.authentication().send_current(actor, sending(operation_id,"13900000000"))).await, Err(authentication::Error::Current(crate::identity::current::Error::Invalid))));
                        services.drain().await.expect("all original work completed"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased;
                        _ = sms.request() => panic!("qualification loss cannot start another SMS"),
                        _ = supplier.observe() => panic!("original verification cannot repeat"),
                        () = conclusion => {},
                    }
                    observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await;
                })).await.expect("bounded in-flight qualification loss");
            }
        }));
    }

    #[test]
    fn transfer_refusal_preserves_actual_rollback_failure() {
        use std::sync::Arc;

        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::transfer::Failure;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
                timeout(Duration::from_secs(20), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let operation_id = operation();
                    let message_id = Uuid::new_v4().to_string().parse().expect("absent message");
                    proxy.arm(Fault::RejectedRollback);
                    let service = services.transfer().clone();
                    let original = actor.clone();
                    let task = tokio::spawn(async move { service.create(original, input(operation_id,message_id,"000000")).await });
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual refusal ROLLBACK fault");
                    let completion = Box::pin(async {
                        let error = task.await.expect("worker").expect_err("refusal cleanup failed");
                        assert_eq!(error.failure(), Failure::Unavailable);
                        let cause = match &error {
                            Error::Combined { cause, .. } => cause.as_ref(),
                            _ => &error,
                        };
                        assert!(matches!(cause, Error::Rollback { cause, rollback: crate::database::transaction::EndError::Rollback(_) } if matches!(cause.as_ref(), Error::Verification)), "{error:?}");
                        let observer = database.connections(&database.user).recover().await.expect("independent reader");
                        assert!(observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers) AND NOT EXISTS(SELECT 1 FROM identity.rejections)", &[]).await.expect("refusal made no partial conclusion before retry").get::<_,bool>(0));
                        assert!(matches!(Box::pin(services.transfer().create(actor,input(operation_id,message_id,"000000"))).await, Err(Error::Verification)));
                        services.drain().await.expect("failed original work drained"); context.stop().await.expect("runtime ended"); observer.terminate().await.expect("observer ended");
                    });
                    tokio::select! { biased;
                        _ = sms.request() => panic!("refusal cannot send SMS"),
                        _ = supplier.observe() => panic!("refusal cannot call verification"),
                        () = completion => {},
                    }
                    sms.stop().await; supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded refusal cleanup verification");
        }));
    }

    #[test]
    fn transfer_cleanup_recovers_original_deletions_and_preserves_dependent_codes() {
        use std::sync::Arc;

        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for valid in [true, false] {
                for (fault, delayed, rolled_back) in [
                    (Fault::LostCommit, false, false),
                    (Fault::DelayedCommit, true, false),
                    (Fault::RolledBack, false, true),
                ] {
                    timeout(Duration::from_secs(25), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let mut sms = fixture::supplier::Supplier::start().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                        let actor = actor(&database, &services, &mut supplier).await;
                        age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                        let (_, sent, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                        let code = if valid { code } else if code == "000000" { "000001".into() } else { "000000".into() };
                        let operation_id = operation();
                        let result = Box::pin(services.transfer().create(actor, input(operation_id, sent.id(), &code))).await;
                        if valid { result.expect("transfer"); } else { assert!(matches!(result, Err(Error::CodeInvalid))); }
                        age(&database, services.authentication(), TimeDelta::hours(26)).await;
                        services.authentication().retention().run().await.expect("release eligible code dependencies");
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        let sql = "SELECT EXISTS(SELECT 1 FROM identity.transfers WHERE operation_id=$1) OR EXISTS(SELECT 1 FROM identity.rejections WHERE operation_id=$1)";
                        assert!(observer.client().query_one(sql,&[&operation_id]).await.expect("original result retained").get::<_,bool>(0));
                        assert_eq!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.confirmations WHERE id=$1)", &[&sent.id()]).await.expect("only rejection still retains the code").get::<_,bool>(0), !valid);
                        proxy.arm(fault);
                        let mut retention = services.transfer().retention();
                        let task = tokio::spawn(async move { retention.run().await });
                        timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual deletion COMMIT fault");
                        if delayed {
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='advisory' AND mode='ExclusiveLock' AND NOT granted AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))", &[]).await.expect("cleanup recovery waits for original transaction").get::<_,bool>(0) { break; }
                                    tokio::task::yield_now().await;
                                }
                            }).await.expect("bounded recovery fence");
                            assert!(!task.is_finished());
                            assert!(observer.client().query_one(sql,&[&operation_id]).await.expect("unconfirmed deletion still visible").get::<_,bool>(0));
                            proxy.release();
                        }
                        let completion = Box::pin(async {
                            task.await.expect("retention task").expect("bounded cleanup handles known rollback");
                            assert_eq!(observer.client().query_one(sql,&[&operation_id]).await.expect("independent outcome before another cleanup pass").get::<_,bool>(0), rolled_back);
                            if rolled_back { services.transfer().retention().run().await.expect("next bounded cleanup attempt"); }
                            services.authentication().retention().run().await.expect("dependent code can now end");
                            assert!(!observer.client().query_one(sql,&[&operation_id]).await.expect("original conclusion removed").get::<_,bool>(0));
                            assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.confirmations WHERE id=$1)", &[&sent.id()]).await.expect("complete dependency cleanup").get::<_,bool>(0));
                            services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                        });
                        tokio::select! { biased;
                            _ = sms.request() => panic!("cleanup never sends SMS"),
                            _ = supplier.observe() => panic!("cleanup never verifies again"),
                            () = completion => {},
                        }
                        observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
                    })).await.expect("bounded original cleanup recovery");
                }
            }
        }));
    }

    #[test]
    fn transfer_proof_and_database_lock_match_independent_protocol_vectors() {
        use platform::cryptography::Encoded;

        use crate::identity::transfer::{FAMILY, Proof};

        let operation_id = Uuid::from_u128(1).to_string().parse().expect("operation");
        let input = input(
            operation_id,
            Uuid::from_u128(5).to_string().parse().expect("message"),
            "001234",
        );
        let proof = Proof {
            input: &input,
            person_id: serde_json::from_value(json!(Uuid::from_u128(2))).expect("person"),
            revision_id: serde_json::from_value(json!(Uuid::from_u128(3))).expect("revision"),
            session_id: Uuid::from_u128(4).to_string().parse().expect("session"),
        };
        // Independently calculated with Python struct-length encoding and hashlib/hmac:
        // Python HKDF-SHA-256 of the fixture root (32 bytes of 1; salt "nucleus/key/v1", info "proof"), then hmac.
        assert_eq!(
            hex::encode(
                keys()
                    .input
                    .active()
                    .sign(&Encoded::new(&proof).expect("encoding"))
                    .as_ref()
            ),
            "2263f4e0f0034e70b2e8316f19b24b465060fac83e593dbe4b7e19c864715540"
        );
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let session = database.connections("postgres").recover().await.expect("isolated owner");
            for sql in [false, true] {
                if sql {
                    session.client().batch_execute("BEGIN").await.expect("begin");
                    session.client().query_one("SELECT identity.lock_transfer($1)", &[&operation_id]).await.expect("database guard lock");
                } else {
                    session.client().batch_execute("BEGIN").await.expect("begin");
                    crate::database::transaction::object(session.client(), FAMILY, operation_id.as_bytes()).await.expect("service operation lock");
                }
                assert!(session.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=pg_backend_pid() AND locktype='advisory' AND granted AND mode='ExclusiveLock' AND classid=3617224218 AND objid=310554803 AND objsubid=1)", &[]).await.expect("same independently derived lock").get::<_,bool>(0));
                session.client().batch_execute("ROLLBACK").await.expect("released");
            }
            session.terminate().await.expect("owner ended");
        });
    }

    #[test]
    fn database_guards_require_code_pairing_atomic_rejections_and_bound_transfers() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let account = services.current().read(&actor).await.expect("current account");
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (_, sent, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                let writer = database.connections(&database.user).recover().await.expect("runtime adversarial writer");
                let client = writer.client();
                client.batch_execute("BEGIN").await.expect("begin");
                client.execute("INSERT INTO identity.checks (id, hash, key_id, expires_at) SELECT id, hash, key_id, expires_at FROM identity.confirmations WHERE id=$1", &[&sent.id()]).await.expect("deferred incompatible companion");
                assert_eq!(client.batch_execute("COMMIT").await.expect_err("purpose-exclusive pairing").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                client.batch_execute("BEGIN").await.expect("begin");
                client.execute("UPDATE identity.confirmations SET failures=failures+1 WHERE id=$1", &[&sent.id()]).await.expect("deferred unpaired increment");
                assert_eq!(client.batch_execute("COMMIT").await.expect_err("counter needs rejection").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                for sql in [
                    "INSERT INTO identity.transfers (id, person_id, operator_id, revision_id, session_id, message_id, created_at, operation_id, hash, key_id) VALUES (gen_random_uuid(),$1,$2,$2,gen_random_uuid(),$3,$4,gen_random_uuid(),decode(repeat('00',32),'hex'),0)",
                    "INSERT INTO identity.rejections (purpose, operation_id, person_id, operator_id, revision_id, session_id, message_id, rejected_at, hash, key_id) VALUES ('transfer',gen_random_uuid(),$1,$2,$2,$5,$3,$4,decode(repeat('00',32),'hex'),0)",
                ] {
                    client.batch_execute("BEGIN").await.expect("negative transaction begins before trusted time");
                    let now = context.sample().await.expect("trusted time");
                    let statement = client.prepare(sql).await.expect("fixed negative query");
                    let error = if statement.params().len() == 4 {
                        client.execute(&statement, &[&account.person_id(), &account.revision_id(), &sent.id(), &now]).await.expect_err("forged session")
                    } else {
                        client.execute(&statement, &[&account.person_id(), &account.revision_id(), &sent.id(), &now, &actor.session_id()]).await.expect_err("rejection without increment")
                    };
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                    client.batch_execute("ROLLBACK").await.expect("negative transaction ended");
                }
                assert!(client.query_one("SELECT (SELECT failures=0 FROM identity.confirmations WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.checks WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.transfers) AND NOT EXISTS(SELECT 1 FROM identity.rejections)", &[&sent.id()]).await.expect("all negative writes rolled back").get::<_,bool>(0));
                let operation_id = operation();
                let created = Box::pin(services.transfer().create(actor.clone(), input(operation_id, sent.id(), &code))).await.expect("normal transfer after SQL refusals");
                assert_eq!(client.batch_execute("DELETE FROM identity.transfers").await.expect_err("fresh transfer retained").code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                age(&database, services.authentication(), TimeDelta::hours(26)).await;
                services.transfer().retention().run().await.expect("message reference keeps transfer");
                assert!(client.query_one("SELECT EXISTS(SELECT 1 FROM identity.transfers WHERE id=$1)", &[&created.id()]).await.expect("dependent transfer still present").get::<_,bool>(0));
                services.authentication().retention().run().await.expect("expired code removed");
                services.transfer().retention().run().await.expect("unreferenced transfer removed");
                assert!(client.query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers) AND NOT EXISTS(SELECT 1 FROM identity.confirmations)", &[]).await.expect("complete cleanup").get::<_,bool>(0));
                services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                writer.terminate().await.expect("writer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded database protection checks");
        }));
    }

    pub(super) async fn bind(
        database: &Database,
        services: &Services,
        actor: &Actor,
        transfer_id: transfer::Id,
        supplier: &mut Supplier,
        sms: &mut fixture::supplier::Supplier,
    ) -> (operation::Id, authentication::dispatch::Receipt, String) {
        bind_to(
            database,
            services,
            actor,
            transfer_id,
            supplier,
            sms,
            "13800000000",
        )
        .await
    }

    pub(crate) async fn bind_to(
        database: &Database,
        services: &Services,
        actor: &Actor,
        transfer_id: transfer::Id,
        supplier: &mut Supplier,
        sms: &mut fixture::supplier::Supplier,
        mobile: &'static str,
    ) -> (operation::Id, authentication::dispatch::Receipt, String) {
        let operation_id = operation();
        let service = services.authentication().clone();
        let actor = actor.clone();
        let mut task = tokio::spawn(async move {
            service
                .send_binding(actor, transfer_id, sending(operation_id, mobile))
                .await
        });
        let request = tokio::select! {
            request = supplier.request() => request,
            result = &mut task => panic!("binding ended before verification: {result:?}"),
        };
        request.reply(200, json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_binding_code","cdata":operation_id.to_string()}).to_string());
        let request = tokio::select! {
            request = sms.request() => request,
            result = &mut task => panic!("binding ended before sending: {result:?}"),
        };
        let code = deliver(request, mobile, "bind");
        (
            operation_id,
            task.await.expect("sender").expect("new-phone message"),
            code,
        )
    }

    #[test]
    fn binding_preserves_session_purpose_quota_and_original_results_after_expiry() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let original = actor(&database, &services, &mut supplier).await;
                let other = actor(&database, &services, &mut supplier).await;
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (_, current, code) = send(&database, &services, &original, &mut supplier, &mut sms).await;
                let transfer = services.transfer().create(original.clone(), input(operation(), current.id(), &code)).await.expect("transfer");
                let observer = database.connections(&database.user).recover().await.expect("independent reader");
                let refusals = Box::pin(async {
                    for (actor, id, mobile, expected) in [
                        (original.clone(), transfer.id(), "13900000000", authentication::Failure::Conflict),
                        (other.clone(), transfer.id(), "13800000000", authentication::Failure::Verification),
                        (original.clone(), transfer::Id(Uuid::new_v4()), "13800000000", authentication::Failure::Verification),
                    ] {
                        let operation_id = operation();
                        assert_eq!(services.authentication().send_binding(actor, id, sending(operation_id, mobile)).await.expect_err("ineligible new send").failure(), expected);
                        assert!(!observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.challenges WHERE operation_id=$1)", &[&operation_id]).await.expect("no admission for refusal").get::<_,bool>(0));
                    }
                });
                tokio::select! { biased;
                    _ = sms.request() => panic!("refusals must not send"),
                    _ = supplier.observe() => panic!("refusals must not call verification"),
                    () = refusals => {},
                }
                let (sending_id, sent, code) = bind(&database, &services, &original, transfer.id(), &mut supplier, &mut sms).await;
                let fact = observer.client().query_one("SELECT challenge.transfer_id=$2 AND challenge.session_id=$3 AND challenge.person_id=transfer.person_id AND confirmation.person_id=transfer.person_id AND confirmation.operator_id=transfer.operator_id AND challenge.purpose='mobile_binding' AND message.status='sent' AND confirmation.failures=0 AND NOT EXISTS(SELECT 1 FROM identity.checks WHERE id=message.id) FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id JOIN identity.confirmations confirmation ON confirmation.id=message.id JOIN identity.transfers transfer ON transfer.id=challenge.transfer_id WHERE message.id=$1", &[&sent.id(), &transfer.id(), &original.session_id()]).await.expect("independent persisted binding before replay");
                assert!(fact.get::<_,bool>(0));
                // Authentication shares the same phone quota even though its purpose differs.
                let service = services.authentication().clone();
                let quota_id = operation();
                let mut task = tokio::spawn(async move { service.send(sending(quota_id, "13800000000")).await });
                let request = supplier.request().await;
                request.reply(200, json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_authentication_code","cdata":quota_id.to_string()}).to_string());
                tokio::select! { biased;
                    _ = sms.request() => panic!("shared phone quota must prevent another SMS"),
                    _ = supplier.observe() => panic!("no verification retry"),
                    result = &mut task => assert!(matches!(result.expect("sender").expect_err("quota").failure(), authentication::Failure::Quota { .. })),
                }
                let verification = Box::pin(async {
                    assert!(matches!(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(), sent.id(), "13800000000", &code)).await, Err(authentication::Error::Verification)));
                    assert!(matches!(services.transfer().create(original.clone(), input(operation(), sent.id(), &code)).await, Err(transfer::Error::Verification)));
                    for (actor, id, mobile) in [(other, transfer.id(), "13800000000"), (original.clone(), transfer::Id(Uuid::new_v4()), "13800000000"), (original.clone(), transfer.id(), "13700000000")] {
                        assert!(matches!(services.authentication().send_binding(actor, id, sending(sending_id, mobile)).await, Err(authentication::Error::Mismatch)));
                    }
                    assert!(matches!(services.authentication().send(sending(sending_id, "13800000000")).await, Err(authentication::Error::Mismatch)));
                    age(&database, services.authentication(), TimeDelta::minutes(11)).await;
                    assert_eq!(services.authentication().send_binding(original.clone(), transfer.id(), sending(sending_id, "13800000000")).await.expect("expired transfer does not replace original result").id(), sent.id());
                    assert_eq!(services.authentication().send_binding(original.clone(), transfer.id(), sending(operation(), "13800000000")).await.expect_err("expired transfer blocks new send").failure(), authentication::Failure::Verification);
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased;
                    _ = sms.request() => panic!("no additional SMS through replay and drain"),
                    _ = supplier.observe() => panic!("no additional verification through replay and drain"),
                    () = verification => {},
                }
                let (context, services) = start(&database, &supplier, &sms).await;
                let verification = Box::pin(async {
                    assert_eq!(services.authentication().send_binding(original, transfer.id(), sending(sending_id, "13800000000")).await.expect("restart preserves original binding").id(), sent.id());
                    services.drain().await.expect("replay drained"); context.stop().await.expect("restarted runtime ended");
                });
                tokio::select! { biased;
                    _ = sms.request() => panic!("restart never repeats SMS"),
                    _ = supplier.observe() => panic!("restart never repeats verification"),
                    () = verification => {},
                }
                observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded binding and restart");
        }));
    }

    #[test]
    fn superseded_binding_stops_new_effects_and_finishes_an_already_called_supplier() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for called_sms in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let (context, services) = start(&database, &supplier, &sms).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                    let (_, current, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                    let transfer = services.transfer().create(actor.clone(), input(operation(), current.id(), &code)).await.expect("first transfer");
                    age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                    let (_, next, next_code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                    let operation_id = operation();
                    let service = services.authentication().clone();
                    let original = actor.clone();
                    let mut task = tokio::spawn(async move { service.send_binding(original, transfer.id(), sending(operation_id, "13800000000")).await });
                    let request = supplier.request().await;
                    let response = json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_binding_code","cdata":operation_id.to_string()}).to_string();
                    if called_sms {
                        request.reply(200, response);
                        let request = sms.request().await;
                        services.transfer().create(actor.clone(), input(operation(), next.id(), &next_code)).await.expect("replacement while original SMS is pending");
                        task.abort(); assert!(task.await.expect_err("caller cancelled").is_cancelled());
                        deliver(request, "13800000000", "bind");
                    } else {
                        services.transfer().create(actor.clone(), input(operation(), next.id(), &next_code)).await.expect("replacement while human response is pending");
                        request.reply(200, response);
                        tokio::select! { biased;
                            _ = sms.request() => panic!("replaced transfer cannot start SMS"),
                            _ = supplier.observe() => panic!("replaced transfer cannot repeat verification"),
                            result = &mut task => assert_eq!(result.expect("caller").expect_err("replaced before permit").failure(), authentication::Failure::Verification),
                        }
                    }
                    let observer = database.connections(&database.user).recover().await.expect("independent reader");
                    let verification = Box::pin(async {
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let row = observer.client().query_one("SELECT challenge.status='passed', message.status FROM identity.challenges challenge LEFT JOIN identity.messages message ON message.challenge_id=challenge.id WHERE challenge.operation_id=$1", &[&operation_id]).await.expect("original conclusion without replay");
                                assert!(row.get::<_,bool>(0));
                                let option: Option<&str> = row.get(1);
                                if called_sms { if option == Some("sent") { break; } } else { assert!(option.is_none()); break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("original worker finishes despite replacement and caller cancellation");
                        let result = services.authentication().send_binding(actor.clone(), transfer.id(), sending(operation_id, "13800000000")).await;
                        if called_sms { assert!(matches!(result.expect("known result").status(), message::Status::Sent { .. })); }
                        else { assert_eq!(result.expect_err("no new permit on replay").failure(), authentication::Failure::Verification); }
                        assert_eq!(services.authentication().send_binding(actor, transfer.id(), sending(operation(), "13800000000")).await.expect_err("new send on replaced transfer").failure(), authentication::Failure::Verification);
                        services.drain().await.expect("all original work ended"); context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased;
                        _ = sms.request() => panic!("no repeated SMS through replay and drain"),
                        _ = supplier.observe() => panic!("no repeated verification through replay and drain"),
                        () = verification => {},
                    }
                    observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await;
                })).await.expect("bounded replacement race");
            }
        }));
    }

    #[test]
    fn binding_database_guards_ownership_and_retains_transfer_until_its_codes_are_cleared() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = start(&database, &supplier, &sms).await;
                let actor = actor(&database, &services, &mut supplier).await;
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (_, current, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                let transfer = services.transfer().create(actor.clone(), input(operation(), current.id(), &code)).await.expect("transfer");
                age(&database, services.authentication(), TimeDelta::minutes(9)).await;
                let (_, sent, _) = bind(&database, &services, &actor, transfer.id(), &mut supplier, &mut sms).await;
                let writer = database.connections(&database.user).recover().await.expect("actual runtime writer");
                let verification = Box::pin(async {
                    for (purpose, session, transfer_id) in [
                        ("authentication", Some(actor.session_id()), Some(transfer.id())),
                        ("mobile_binding", None, Some(transfer.id())),
                        ("mobile_binding", Some(actor.session_id()), None),
                        ("mobile_binding", Some(actor.session_id()), Some(transfer::Id(Uuid::new_v4()))),
                    ] {
                        writer.client().batch_execute("BEGIN").await.expect("negative transaction");
                        let error = writer.client().execute("INSERT INTO identity.challenges (id,operation_id,purpose,person_id,session_id,transfer_id,hash,key_id,mobile,source,calls,created_at,expires_at) SELECT $1,$1,$2,person_id,$3,$4,hash,key_id,mobile,source,calls,created_at,expires_at FROM identity.challenges WHERE transfer_id=$5", &[&Uuid::new_v4(), &purpose, &session, &transfer_id, &transfer.id()]).await.expect_err("invalid purpose or transfer binding");
                        assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                        writer.client().batch_execute("ROLLBACK").await.expect("negative transaction ended");
                    }
                    let error = writer.client().execute("UPDATE identity.challenges SET transfer_id=$1 WHERE transfer_id=$1", &[&transfer.id()]).await.expect_err("runtime cannot rewrite original transfer binding");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
                    age(&database, services.authentication(), TimeDelta::hours(24) + TimeDelta::minutes(2)).await;
                    services.authentication().retention().run().await.expect("old current-phone records cleared");
                    services.transfer().retention().run().await.expect("new-phone dependency keeps transfer");
                    assert!(writer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.messages WHERE id=$1) AND EXISTS(SELECT 1 FROM identity.messages WHERE id=$2) AND EXISTS(SELECT 1 FROM identity.transfers WHERE id=$3)", &[&current.id(), &sent.id(), &transfer.id()]).await.expect("original message gone but dependent binding preserved").get::<_,bool>(0));
                    let error = writer.client().execute("DELETE FROM identity.transfers WHERE id=$1", &[&transfer.id()]).await.expect_err("runtime cannot delete a referenced transfer");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                    age(&database, services.authentication(), TimeDelta::minutes(5)).await;
                    services.authentication().retention().run().await.expect("binding records now eligible");
                    services.transfer().retention().run().await.expect("unreferenced transfer now eligible");
                    assert!(writer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.transfers) AND NOT EXISTS(SELECT 1 FROM identity.challenges WHERE transfer_id IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM identity.confirmations)", &[]).await.expect("all dependencies ended without orphaned facts").get::<_,bool>(0));
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased;
                    _ = sms.request() => panic!("SQL refusals and cleanup never send"),
                    _ = supplier.observe() => panic!("SQL refusals and cleanup never verify"),
                    () = verification => {},
                }
                writer.terminate().await.expect("writer ended"); sms.stop().await; supplier.stop().await;
            })).await.expect("bounded dependency cleanup");
        }));
    }

    #[test]
    fn binding_expiry_at_each_insert_preserves_refusals_and_saved_messages() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for table in ["challenges", "confirmations", "claims"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let mut sms = fixture::supplier::Supplier::start().await;
                    let (context, services) = start(&database, &supplier, &sms).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                    let (_, current, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                    let transfer = services.transfer().create(actor.clone(), input(operation(), current.id(), &code)).await.expect("transfer");
                    let blocker = database.connections("postgres").recover().await.expect("isolated insert barrier");
                    blocker.client().batch_execute(&format!("CREATE FUNCTION public.binding_deadline() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path='' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(1071, 1); RETURN NEW; END $$; CREATE TRIGGER binding_deadline BEFORE INSERT ON identity.{table} FOR EACH ROW EXECUTE FUNCTION public.binding_deadline()")).await.expect("install the insert barrier");
                    blocker.client().batch_execute("BEGIN; SELECT pg_catalog.pg_advisory_xact_lock(1071, 1)").await.expect("pause before the real sender guard");
                    let blocker_id: i32 = blocker.client().query_one("SELECT pg_catalog.pg_backend_pid()", &[]).await.expect("barrier backend").get(0);
                    age(&database, services.authentication(), TimeDelta::seconds(595)).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let operation_id = operation();
                    let service = services.authentication().clone();
                    let original = actor.clone();
                    let transfer_id = transfer.id();
                    let mut task = tokio::spawn(async move { service.send_binding(original, transfer_id, sending(operation_id, "13800000000")).await });
                    if table != "challenges" {
                        let request = tokio::select! {
                            request = supplier.request() => request,
                            result = &mut task => panic!("sender ended before verification at {table}: {result:?}"),
                        };
                        request.reply(200, json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_mobile_binding_code","cdata":operation_id.to_string()}).to_string());
                    }
                    let verification = Box::pin(async {
                        timeout(Duration::from_secs(3), async {
                            loop {
                                if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE datname=pg_catalog.current_database() AND $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&blocker_id]).await.expect("observe the actual blocked insert").get::<_, bool>(0) { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("request qualified before the insert barrier");
                        assert!(observer.client().query_one("SELECT pg_catalog.clock_timestamp()<created_at+interval '10 minutes' FROM identity.transfers WHERE id=$1", &[&transfer.id()]).await.expect("transfer remains valid at the barrier").get::<_, bool>(0));
                        observer.client().query_one("SELECT pg_catalog.pg_sleep_until(created_at+interval '10 minutes') FROM identity.transfers WHERE id=$1", &[&transfer.id()]).await.expect("wait for the persisted deadline");
                        assert!(observer.client().query_one("SELECT pg_catalog.clock_timestamp()>=created_at+interval '10 minutes' FROM identity.transfers WHERE id=$1", &[&transfer.id()]).await.expect("expiry before releasing the insert").get::<_, bool>(0));
                        blocker.client().batch_execute("COMMIT").await.expect("release after expiry");
                        let result = task.await.expect("sender ended");
                        if table == "claims" {
                            assert!(matches!(result.expect("saved message has a terminal receipt").status(), message::Status::Failed { .. }));
                        } else {
                            let error = result.expect_err("expired transfer refuses a new fact");
                            assert_eq!(error.failure(), authentication::Failure::Verification, "{table}: {error:?}");
                        }
                        let facts = observer.client().query_one("SELECT (SELECT count(*) FROM identity.challenges WHERE operation_id=$1), (SELECT count(*) FROM identity.messages message JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE challenge.operation_id=$1 AND message.status='failed' AND message.finished_at IS NOT NULL), (SELECT count(*) FROM identity.confirmations confirmation JOIN identity.messages message ON message.id=confirmation.id JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE challenge.operation_id=$1), (SELECT count(*) FROM identity.claims claim JOIN identity.messages message ON message.id=claim.id JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE challenge.operation_id=$1)", &[&operation_id]).await.expect("independent committed facts before replay");
                        assert_eq!(facts.get::<_, i64>(0), i64::from(table != "challenges"));
                        assert_eq!(facts.get::<_, i64>(1), i64::from(table == "claims"));
                        assert_eq!(facts.get::<_, i64>(2), i64::from(table == "claims"));
                        assert_eq!(facts.get::<_, i64>(3), 0);
                        let replay = services.authentication().send_binding(actor, transfer.id(), sending(operation_id, "13800000000")).await;
                        if table == "claims" {
                            assert!(matches!(replay.expect("original failed message survives expiry").status(), message::Status::Failed { .. }));
                        } else {
                            assert_eq!(replay.expect_err("expired original cannot issue a new message").failure(), authentication::Failure::Verification);
                        }
                        services.drain().await.expect("drained after replay");
                        context.stop().await.expect("runtime ended");
                    });
                    tokio::select! { biased;
                        _ = sms.request() => panic!("expired transfer must not call SMS at {table}"),
                        _ = supplier.observe() => panic!("expiry and replay must not repeat verification at {table}"),
                        () = verification => {},
                    }
                    observer.terminate().await.expect("reader ended");
                    blocker.terminate().await.expect("barrier ended");
                    sms.stop().await;
                    supplier.stop().await;
                })).await.expect("bounded insert expiry race");
            }
        }));
    }

    #[test]
    fn binding_recovers_a_lost_acceptance_commit_with_its_original_transfer() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start_on(authentication::proxied(&database, &proxy), &supplier, &sms).await;
                let actor = actor(&database, &services, &mut supplier).await;
                age(&database, services.authentication(), TimeDelta::seconds(61)).await;
                let (_, current, code) = send(&database, &services, &actor, &mut supplier, &mut sms).await;
                let transfer = services.transfer().create(actor.clone(), input(operation(), current.id(), &code)).await.expect("transfer");
                proxy.arm(Fault::LostCommit);
                let (sending_id, sent, _) = bind(&database, &services, &actor, transfer.id(), &mut supplier, &mut sms).await;
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual acceptance COMMIT response lost");
                let observer = database.connections(&database.user).recover().await.expect("independent reader");
                assert!(observer.client().query_one("SELECT challenge.transfer_id=$2 AND challenge.session_id=$3 AND challenge.calls=1 AND challenge.status='passed' AND message.id=$4 AND message.status='sent' AND (SELECT count(*)=1 FROM identity.claims WHERE id=message.id) FROM identity.challenges challenge JOIN identity.messages message ON message.challenge_id=challenge.id WHERE challenge.operation_id=$1", &[&sending_id, &transfer.id(), &actor.session_id(), &sent.id()]).await.expect("original persisted identity before replay").get::<_,bool>(0));
                let verification = Box::pin(async {
                    assert_eq!(services.authentication().send_binding(actor, transfer.id(), sending(sending_id, "13800000000")).await.expect("known original result").id(), sent.id());
                    services.drain().await.expect("drained"); context.stop().await.expect("runtime ended");
                });
                tokio::select! { biased;
                    _ = sms.request() => panic!("recovery never repeats SMS"),
                    _ = supplier.observe() => panic!("recovery never repeats verification"),
                    () = verification => {},
                }
                observer.terminate().await.expect("reader ended"); sms.stop().await; supplier.stop().await; proxy.stop().await;
            })).await.expect("bounded binding recovery");
        }));
    }
}
