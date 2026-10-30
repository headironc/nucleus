//! SMS R1 has one local transaction and recovers its original committed result.

mod database;
pub(super) mod retention;
pub mod settings;

use chrono::{DateTime, Utc};
use platform::cryptography::{self, Encoded};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;

use crate::{
    database::transaction::{Completion, Known, commit, confirm},
    identity::{
        account,
        authentication::{
            Command, Error, Execute, Id, Inner, Stage, TRANSACTION_BUDGET, registry::Family,
            transaction,
        },
        keys, message,
        mobile::Mobile,
        operation, registration,
        request::Secret,
        session, website,
    },
};

pub struct Input {
    operation_id: operation::Id,
    secret: Secret,
    message_id: message::Id,
    mobile: Mobile,
    code: message::Code,
}

impl Input {
    pub fn new(
        operation_id: operation::Id,
        secret: Secret,
        message_id: message::Id,
        mobile: Mobile,
        code: message::Code,
    ) -> Self {
        Self {
            operation_id,
            secret,
            message_id,
            mobile,
            code,
        }
    }
    fn encoded(&self, app_id: &website::AppId) -> Result<Encoded, cryptography::Error> {
        struct Payload<'a> {
            input: &'a Input,
            app_id: &'a website::AppId,
        }
        impl cryptography::Input<11> for Payload<'_> {
            fn fields(&self) -> [&[u8]; 11] {
                [
                    b"nucleus/identity/request/v1",
                    b"POST",
                    b"/sessions",
                    b"create_session",
                    self.app_id.as_ref().as_bytes(),
                    self.input.operation_id.as_bytes(),
                    b"cookie",
                    b"message",
                    self.input.message_id.as_bytes(),
                    self.input.mobile.as_bytes(),
                    self.input.code.as_bytes(),
                ]
            }
        }
        Encoded::new(&Payload {
            input: self,
            app_id,
        })
    }
}

impl Command for Input {
    fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
    fn family(&self) -> Family {
        Family::CreateSession
    }
}

/// Only the R1 transaction and original-proof checks can construct these results.
pub enum Receipt {
    Session(Session),
    Registration(Registration),
}

pub struct Session {
    id: session::Id,
    token: session::Token,
    expires_at: DateTime<Utc>,
}
impl Session {
    pub fn id(&self) -> session::Id {
        self.id
    }
    pub fn token(&self) -> &session::Token {
        &self.token
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}
pub struct Registration {
    id: registration::Id,
    token: registration::Token,
    expires_at: DateTime<Utc>,
}
impl Registration {
    pub fn id(&self) -> registration::Id {
        self.id
    }
    pub fn token(&self) -> &registration::Token {
        &self.token
    }
    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

pub(super) struct Action;
impl Execute<Input> for Action {
    type Output = Receipt;
    async fn execute(self, service: &Inner, input: &Input) -> Result<Receipt, Error> {
        service.authenticate(input).await
    }
}

enum Outcome {
    Credential(Receipt),
    Invalid,
    Suspended,
}
impl Outcome {
    fn deliver(self) -> std::result::Result<Receipt, Error> {
        match self {
            Self::Credential(receipt) => Ok(receipt),
            Self::Invalid => Err(Error::CodeInvalid),
            Self::Suspended => Err(Error::Suspended),
        }
    }
}

enum Recovery {
    Readback(
        Known<Option<Outcome>>,
        Result<(), platform::database::Error>,
    ),
    Refused(Error),
}

struct Creation {
    id: session::Id,
    created_at: DateTime<Utc>,
    pending: crate::history::Pending,
}

impl Inner {
    async fn authenticate(&self, input: &Input) -> std::result::Result<Receipt, Error> {
        let mut creation = None;
        let cause = match self.context.connections().lease().await {
            Ok(lease) => {
                let deadline = Instant::now() + TRANSACTION_BUDGET;
                match timeout_at(
                    deadline,
                    self.write_session(lease.client(), input, &mut creation),
                )
                .await
                {
                    Ok(Ok(outcome)) => {
                        let completion =
                            commit(outcome, deadline, lease.client().batch_execute("COMMIT")).await;
                        let (completion, cleanup) = completion.recycle(lease).await;
                        match completion {
                            Completion::Known(Known { value, end }) => {
                                if let Some(creation) = creation.take() {
                                    creation.pending.committed()?;
                                }
                                if end.is_err() || cleanup.is_err() {
                                    tracing::error!(operation_id = %input.operation_id, stage = "authentication_completion", kind = "unavailable", outcome = "committed", commit = ?end, cleanup = ?cleanup);
                                }
                                return value.deliver();
                            }
                            Completion::Pending { error, .. } => {
                                Error::from(error).combine(cleanup)
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        if error.rejection() {
                            return Err(transaction::rollback(lease, error).await);
                        }
                        error.combine(lease.discard().await)
                    }
                    Err(error) => Error::from(error).combine(lease.discard().await),
                }
            }
            Err(error) => error.into(),
        };
        match self.recover_session(input, &mut creation).await {
            Ok(Recovery::Readback(
                Known {
                    value: Some(result),
                    end: commit,
                },
                termination,
            )) => {
                if cause.unreliable() {
                    self.registry.close();
                }
                if cause.unreliable() || commit.is_err() || termination.is_err() {
                    tracing::error!(operation_id = %input.operation_id, stage = "authentication_completion", kind = "unavailable", outcome = "committed", error = ?cause, commit = ?commit, termination = ?termination);
                }
                result.deliver()
            }
            Ok(Recovery::Readback(
                Known {
                    value: None,
                    end: commit,
                },
                termination,
            )) => Err(Error::NotCommitted {
                cause: Box::new(Error::SessionReadback {
                    cause: Box::new(cause),
                    commit,
                    termination,
                }),
            }),
            Ok(Recovery::Refused(refusal)) => Err(Error::SessionRefused {
                cause: Box::new(cause),
                refusal: Box::new(refusal),
            }),
            Err(recovery) => {
                if let Some(creation) = creation {
                    creation.pending.uncertain();
                }
                Err(Error::Unknown {
                    cause: Box::new(cause),
                    recovery: Box::new(recovery),
                })
            }
        }
    }

    async fn write_session(
        &self,
        client: &Client,
        input: &Input,
        creation: &mut Option<Creation>,
    ) -> std::result::Result<crate::database::transaction::Outcome<Outcome>, Error> {
        // Discover account changes before any effect; retry the complete lock set at most twice.
        for _ in 0..3 {
            transaction::begin_for(
                &self.context,
                client,
                b"authentication/operation",
                input.operation_id,
            )
            .await?;
            if let Some(result) = self.read_original_session(client, input).await? {
                return Ok(crate::database::transaction::Outcome::Known(result));
            }
            let hash = self.keys.mobile.hash(&input.mobile)?;
            let account = account::state::discover_mobile(client, &hash).await?;
            if let Some(account) = account {
                database::lock_account(client, account).await?;
            }
            transaction::object(client, b"identity/mobile", input.mobile.as_bytes()).await?;
            if account::state::discover_mobile(client, &hash).await? != account {
                // A retained account that moved its phone cannot turn an in-flight authentication into registration.
                let now = self.now(client).await?;
                if let Some(account) = account
                    && crate::history::find::<account::state::State>(
                        client,
                        self.app.keys(),
                        self.app.anchors(),
                        crate::history::Read::Locked,
                        account,
                        now,
                    )
                    .await?
                    .is_some_and(|state| state.current().is_some())
                {
                    return Err(Error::Verification);
                }
                client.batch_execute("ROLLBACK").await?;
                continue;
            }
            database::lock_check(client, input.message_id).await?;
            let now = self.now(client).await?;
            let current = account::state::located(
                client,
                self.app.keys(),
                self.app.anchors(),
                crate::history::Read::Locked,
                account::state::Location::Mobile(&hash),
                now,
            )
            .await?;
            if current.as_ref().map(|state| state.id()) != account {
                return Err(inconsistent(
                    "locked mobile location retains its discovered account",
                ));
            }
            let verification = database::verification(self, client, input, now).await?;
            let id = Id::new();
            let result = if !verification.matches(&self.keys.message, input)? {
                database::insert(self, client, input, id, database::Status::Invalid, now).await?;
                client
                    .execute(
                        "UPDATE identity.checks SET failures = failures + 1 WHERE id = $1",
                        &[&input.message_id],
                    )
                    .await?;
                Outcome::Invalid
            } else if let Some(account) = current {
                if account
                    .current()
                    .is_some_and(|current| current.suspension_id().is_some())
                {
                    database::insert(self, client, input, id, database::Status::Suspended, now)
                        .await?;
                    database::consume(client, input.message_id, id).await?;
                    Outcome::Suspended
                } else {
                    database::insert(
                        self,
                        client,
                        input,
                        id,
                        database::Status::Authenticated,
                        now,
                    )
                    .await?;
                    database::consume(client, input.message_id, id).await?;
                    let (id, token, expires_at, pending) = session::create(
                        client,
                        &self.app,
                        &self.keys.credential,
                        self.settings.session().issuance(),
                        session::Input {
                            source: session::Source::Authentication(id),
                            account_id: account.id(),
                            operation_id: input.operation_id,
                            created_at: now,
                        },
                    )
                    .await?;
                    *creation = Some(Creation {
                        id,
                        created_at: now,
                        pending,
                    });
                    Outcome::Credential(Receipt::Session(Session {
                        id,
                        token,
                        expires_at,
                    }))
                }
            } else {
                database::insert(self, client, input, id, database::Status::Registrable, now)
                    .await?;
                database::consume(client, input.message_id, id).await?;
                Outcome::Credential(
                    database::create_registration(self, client, input, id, now).await?,
                )
            };
            return Ok(crate::database::transaction::Outcome::Pending(result));
        }
        Err(Error::Busy)
    }

    async fn recover_session(
        &self,
        input: &Input,
        creation: &mut Option<Creation>,
    ) -> std::result::Result<Recovery, Error> {
        let session = self.context.connections().recover().await?;
        let deadline = Instant::now() + TRANSACTION_BUDGET;
        let result = timeout_at(deadline, async {
            transaction::begin_for(
                &self.context,
                session.client(),
                b"authentication/operation",
                input.operation_id,
            )
            .await?;
            if let Some(candidate) = creation.as_ref() {
                let committed = match candidate.pending.resolution()? {
                    Some(committed) => committed,
                    None => {
                        session::state::issued(
                            session.client(),
                            self.app.keys(),
                            self.app.anchors(),
                            candidate.id,
                            input.operation_id,
                            candidate.created_at,
                        )
                        .await?
                    }
                };
                if let Some(candidate) = creation.take() {
                    if committed {
                        candidate.pending.committed()?;
                    } else {
                        candidate.pending.rolled_back()?;
                    }
                }
            }
            self.read_original_session(session.client(), input).await
        })
        .await;
        // A verified refusal survives both the read budget and recovery-session cleanup.
        let result = match result {
            Ok(Err(cause)) if cause.rejection() => {
                let (completion, cleanup) = crate::database::transaction::rollback(
                    cause,
                    Instant::now() + TRANSACTION_BUDGET,
                    session.client().batch_execute("ROLLBACK"),
                )
                .await
                .terminate(session)
                .await;
                return Ok(Recovery::Refused(
                    transaction::refusal(completion).combine(cleanup),
                ));
            }
            result => result,
        };
        let result = match result {
            Ok(Ok(result)) => {
                Ok(confirm(result, deadline, session.client().batch_execute("COMMIT")).await)
            }
            Ok(Err(error)) => Err(error),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(result) => {
                let (result, cleanup) = result.terminate(session).await;
                Ok(Recovery::Readback(result, cleanup))
            }
            Err(error) => Err(error.combine(session.terminate().await)),
        }
    }

    /// Both callers hold the original-operation lock and retain their outer deadline.
    async fn read_original_session(
        &self,
        client: &Client,
        input: &Input,
    ) -> Result<Option<Outcome>, Error> {
        let mut retried = false;
        loop {
            let Some(authentication) =
                database::find_by_operation(client, input.operation_id).await?
            else {
                return Ok(None);
            };
            authentication.verify(self, input)?;
            match authentication.deliver(self, client).await {
                Err(Error::History(crate::history::Error::Changed)) if !retried => {
                    // Release every discovered lock before rereading the original and its credentials.
                    if let Err(error) = client.batch_execute("ROLLBACK").await {
                        return Err(crate::history::Error::Retry(error.into()).into());
                    }
                    transaction::begin_for(
                        &self.context,
                        client,
                        b"authentication/operation",
                        input.operation_id,
                    )
                    .await?;
                    retried = true;
                }
                result => return result.map(Some),
            }
        }
    }
}

fn inconsistent(rule: &'static str) -> Error {
    Error::Facts {
        stage: Stage::CreateSession,
        rule,
    }
}

pub(super) async fn verify_all(
    client: &Client,
    app: &crate::kernel::App,
    keys: &keys::Keys,
    app_id: &website::AppId,
    now: DateTime<Utc>,
) -> std::result::Result<(), Error> {
    database::verify_all(client, app, keys, app_id, now).await
}

#[cfg(test)]
#[allow(clippy::expect_used)]
pub(crate) mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use platform::cryptography::hmac;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use crate::{
        database::transaction::EndError,
        identity::{
            account,
            authentication::{
                self, Error, Service,
                database::message::Message,
                session::{Input, Receipt},
                verification::tests::{Database, Supplier, keys, server},
            },
            message,
            mobile::envelope::Envelope,
        },
    };

    const SECRET: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    pub(crate) const CODE: &str = "001234";

    pub(crate) fn input(operation: Uuid, id: message::Id, mobile: &str, code: &str) -> Input {
        Input::new(
            operation.to_string().parse().expect("operation"),
            SECRET.parse().expect("secret"),
            id,
            mobile.parse().expect("mobile"),
            code.parse().expect("code"),
        )
    }

    pub(crate) async fn message(
        database: &Database,
        service: &Service,
        supplier: &mut Supplier,
        mobile: &str,
    ) -> message::Id {
        permit(database, service, supplier, mobile, true).await
    }

    async fn permit(
        database: &Database,
        service: &Service,
        supplier: &mut Supplier,
        mobile: &str,
        committed: bool,
    ) -> message::Id {
        let operation = Uuid::new_v4();
        let request = authentication::Input::new(
            operation.to_string().parse().expect("operation"),
            mobile.parse().expect("mobile"),
            operation.to_string().parse().expect("token"),
            "192.0.2.1".parse().expect("source"),
        );
        let worker = service.clone();
        let task = tokio::spawn(async move { worker.verify(request).await });
        supplier.request().await.reply(200, serde_json::json!({"success":true,"challenge_ts":database.now().await.to_rfc3339(),"hostname":"example.com","action":"send_authentication_code","cdata":operation.to_string()}).to_string());
        let challenge = task.await.expect("worker").expect("verified");
        let session = database
            .connections(&database.user)
            .recover()
            .await
            .expect("runtime fixture");
        session
            .client()
            .batch_execute("BEGIN")
            .await
            .expect("transaction");
        let now = service.0.context.sample().await.expect("trusted time");
        let ordinal: i64 = session
            .client()
            .query_one("SELECT nextval('identity.ordinals')", &[])
            .await
            .expect("ordinal")
            .get(0);
        let message = Message::new(
            &keys().message,
            challenge.id(),
            ordinal,
            now,
            &message::Limits::default(),
            &CODE.parse().expect("code"),
            crate::identity::authentication::database::message::Binding::Authentication,
        )
        .expect("message");
        message
            .insert(session.client())
            .await
            .expect("paired permit");
        session
            .client()
            .execute(
                "UPDATE identity.messages SET status = 'failed', finished_at = $2 WHERE id = $1",
                &[&message.id(), &now],
            )
            .await
            .expect("known no-send fixture");
        session
            .client()
            .batch_execute(if committed { "COMMIT" } else { "ROLLBACK" })
            .await
            .expect("permit transaction ended");
        session.terminate().await.expect("fixture ended");
        message.id()
    }

    pub(crate) async fn account(database: &Database, mobile: &str, suspended: bool) -> account::Id {
        let keys = keys();
        let keys = crate::access::Keys::new(crate::identity::authentication::inventory(keys));
        let administrator = if suspended {
            if mobile == "13700000000" {
                "13600000000"
            } else {
                "13700000000"
            }
        } else {
            mobile
        };
        let operator = serde_json::from_value(
            serde_json::json!({"mobile":administrator,"name":"隔离测试","idNumber":if suspended {"00000020000229007X"} else {"000000200002290010"}}),
        )
        .expect("fictitious operator");
        let connections = database.connections("postgres");
        let context = crate::runtime::Context::new(connections)
            .await
            .expect("independent clock");
        let mut session = context
            .connections()
            .recover()
            .await
            .expect("isolated owner");
        let transaction = session
            .client_mut()
            .transaction()
            .await
            .expect("initialization transaction");
        // Retained R2 fixtures move back at most 74 hours; keep one hour before their first appointment.
        let at = transaction
            .query_one("SELECT clock_timestamp() - interval '75 hours'", &[])
            .await
            .expect("historical initialization time")
            .get(0);
        let receipt = crate::access::initialize::seed(&transaction, &keys, &operator, at)
            .await
            .expect("complete identity and access");
        transaction
            .commit()
            .await
            .expect("complete initialization committed");
        session.terminate().await.expect("fixture ended");
        context.stop().await.expect("clock ended");
        if !suspended {
            return receipt.account_id();
        }
        let mut supplier = Supplier::new().await;
        let (context, services) =
            crate::access::role::tests::start(database.connections(&database.user), &supplier)
                .await;
        let id = message(
            database,
            services.authentication(),
            &mut supplier,
            administrator,
        )
        .await;
        let Receipt::Session(credential) =
            Box::pin(services.authentication().create_session(input(
                Uuid::new_v4(),
                id,
                administrator,
                CODE,
            )))
            .await
            .expect("administrator R1")
        else {
            panic!("initialized administrator");
        };
        let actor = services
            .current()
            .authenticate(crate::identity::current::Proof {
                id: credential.id(),
                token: credential
                    .token()
                    .text()
                    .parse()
                    .expect("administrator proof"),
            })
            .await
            .expect("administrator actor");
        let id = message(database, services.authentication(), &mut supplier, mobile).await;
        let Receipt::Registration(credential) = Box::pin(
            services
                .authentication()
                .create_session(input(Uuid::new_v4(), id, mobile, CODE)),
        )
        .await
        .expect("target R1") else {
            panic!("unregistered target");
        };
        Box::pin(
            services
                .registration()
                .register(crate::registration::Input::new(
                    Uuid::new_v4()
                        .to_string()
                        .parse()
                        .expect("registration operation"),
                    credential.id(),
                    credential
                        .token()
                        .text()
                        .parse()
                        .expect("registration proof"),
                    "Target Person".parse().expect("target name"),
                    "000000200002290010".parse().expect("target number"),
                )),
        )
        .await
        .expect("ordinary R2 target");
        let candidate = services
            .account()
            .candidate(&actor, mobile.parse().expect("mobile"))
            .await
            .expect("manageable target");
        let account_id = candidate.identity().id();
        let input = serde_json::from_value(serde_json::json!({"personId":candidate.identity().person_id(), "revisionId":candidate.identity().revision_id(), "mobile":mobile})).expect("target confirmation");
        Box::pin(
            services.suspension().suspend(
                actor,
                Uuid::new_v4()
                    .to_string()
                    .parse()
                    .expect("suspension operation"),
                account_id,
                input,
            ),
        )
        .await
        .expect("real suspension");
        services.close();
        services.drain().await.expect("fixture work drained");
        context.stop().await.expect("fixture clock ended");
        supplier.stop().await;
        account_id
    }

    pub(crate) async fn count(database: &Database, sql: &str) -> i64 {
        let session = database
            .connections(&database.user)
            .recover()
            .await
            .expect("observer");
        let value = session
            .client()
            .query_one(sql, &[])
            .await
            .expect("count")
            .get(0);
        session.terminate().await.expect("observer ended");
        value
    }

    pub(crate) async fn age(
        database: &Database,
        service: &authentication::Service,
        duration: TimeDelta,
    ) {
        let owner = database
            .connections("postgres")
            .recover()
            .await
            .expect("isolated historical fixture");
        owner
            .client()
            .batch_execute("BEGIN; SET LOCAL session_replication_role = replica")
            .await
            .expect("fixture-only history rewrite");
        let app = &service.0.app;
        let now = database.now().await;
        let (accounts, persons) =
            crate::identity::account::record::age(owner.client(), app.keys(), now, duration)
                .await
                .expect("authenticated historical account timeline");
        let sessions =
            crate::identity::session::state::age(owner.client(), app.keys(), now, duration)
                .await
                .expect("authenticated complete historical session groups");
        let holders =
            crate::access::holder::age_registrations(owner.client(), app.keys(), duration)
                .await
                .expect("paired historical registration holders");
        for template in [
            crate::identity::revocation::COMPLETIONS,
            crate::suspension::SUSPENSIONS,
            crate::suspension::RESUMPTIONS,
            crate::identity::transfer::completion::COMPLETIONS,
        ] {
            crate::kernel::completion::Seed::age_family(
                owner.client(),
                app.keys(),
                template,
                duration,
            )
            .await
            .expect("authenticated original completion lifetime");
        }
        let interval = format!(
            "{} microseconds",
            duration.num_microseconds().expect("bounded fixture age")
        );
        for sql in [
            "UPDATE identity.challenges SET created_at=created_at-$1::text::interval,expires_at=expires_at-$1::text::interval,solved_at=solved_at-$1::text::interval,finished_at=finished_at-$1::text::interval",
            "UPDATE identity.messages SET created_at=created_at-$1::text::interval,expires_at=expires_at-$1::text::interval,finished_at=finished_at-$1::text::interval",
            "UPDATE identity.claims SET created_at=created_at-$1::text::interval",
            "UPDATE identity.checks SET expires_at=expires_at-$1::text::interval",
            "UPDATE identity.confirmations SET expires_at=expires_at-$1::text::interval",
            "UPDATE identity.transfers SET created_at=created_at-$1::text::interval",
            "UPDATE identity.rejections SET rejected_at=rejected_at-$1::text::interval",
            "UPDATE identity.authentications SET finished_at=finished_at-$1::text::interval",
            "UPDATE identity.handoffs SET expires_at=expires_at-$1::text::interval",
            "UPDATE identity.registrations SET created_at=created_at-$1::text::interval,expires_at=expires_at-$1::text::interval",
            "UPDATE identity.attempts SET accepted_at=accepted_at-$1::text::interval",
        ] {
            owner
                .client()
                .execute(sql, &[&interval])
                .await
                .expect("age isolated facts together");
        }
        owner
            .client()
            .batch_execute("COMMIT")
            .await
            .expect("historical fixture committed");
        for state in persons {
            crate::history::Seed::anchor(app.anchors(), &state).expect("person fixture anchor");
        }
        for state in accounts {
            crate::history::Seed::anchor(app.anchors(), &state).expect("account fixture anchor");
        }
        for state in sessions {
            crate::history::Seed::anchor(app.anchors(), &state).expect("session fixture anchor");
        }
        for state in holders {
            crate::history::Seed::anchor(app.anchors(), &state).expect("holder fixture anchor");
        }
        owner.terminate().await.expect("fixture ended");
    }

    #[test]
    fn three_results_commit_consumption_and_replay_only_the_original_credentials() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (mobile, kind) in [("13800000000", "session"), ("13800000001", "suspended"), ("13800000002", "registration")] {
                let database = Database::new(Arc::clone(&server)).await;
                account(&database, if kind == "registration" { "13900000000" } else { mobile }, kind == "suspended").await;
                let initial_authentications = count(&database, "SELECT count(*) FROM identity.authentications").await;
                let initial_sessions = count(&database, "SELECT count(*) FROM identity.sessions").await;
                let initial_handoffs = count(&database, "SELECT count(*) FROM identity.handoffs").await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let id = message(&database, &service, &mut supplier, mobile).await;
                let operation = Uuid::new_v4();
                let result = service.create_session(input(operation, id, mobile, CODE)).await;
                if kind == "suspended" {
                    assert!(matches!(result, Err(Error::Suspended)));
                    assert!(matches!(service.create_session(input(operation, id, mobile, CODE)).await, Err(Error::Suspended)));
                } else {
                    let original = result.expect("credential");
                    let replay = service.create_session(input(operation, id, mobile, CODE)).await.expect("original replay");
                    match (original, replay) {
                        (Receipt::Session(original), Receipt::Session(replay)) => {
                            assert_eq!(kind, "session");
                            assert_eq!(original.id(), replay.id());
                            assert_eq!(original.token().text(), replay.token().text());
                            assert_eq!(original.expires_at(), replay.expires_at());
                        }
                        (Receipt::Registration(original), Receipt::Registration(replay)) => {
                            assert_eq!(kind, "registration");
                            assert_eq!(original.id(), replay.id());
                            assert_eq!(original.token().text(), replay.token().text());
                            assert_eq!(original.expires_at(), replay.expires_at());
                        }
                        _ => panic!("original result kind changed"),
                    }
                }
                assert!(matches!(service.create_session(input(Uuid::new_v4(), id, mobile, CODE)).await, Err(Error::Verification)));
                let mut changed = input(operation, id, mobile, CODE);
                changed.secret = "__________________________________________8".parse().expect("other secret");
                assert!(matches!(service.create_session(changed).await, Err(Error::Proof)));
                assert!(matches!(service.create_session(input(operation, id, mobile, "001235")).await, Err(Error::Mismatch)));
                let observer = database.connections(&database.user).recover().await.expect("independent pairing observer");
                let row = observer.client().query_one("SELECT authentication.message_id = $2 AND verification.authentication_id = authentication.id AND verification.failures = 0 AND authentication.finished_at >= message.created_at AND authentication.finished_at < verification.expires_at, authentication.status, session.id IS NOT NULL, registration.id IS NOT NULL, COALESCE(session.created_at = authentication.finished_at AND session.app_id = authentication.app_id AND (account.mobile).hash = (challenge.mobile).hash, true), COALESCE(registration.created_at = authentication.finished_at AND (registration.mobile).hash = (challenge.mobile).hash, true) FROM identity.authentications authentication JOIN identity.checks verification ON verification.id=authentication.message_id JOIN identity.messages message ON message.id=verification.id JOIN identity.challenges challenge ON challenge.id=message.challenge_id LEFT JOIN identity.sessions session ON session.authentication_id=authentication.id LEFT JOIN identity.accounts account ON account.id=session.account_id LEFT JOIN identity.registrations registration ON registration.authentication_id=authentication.id WHERE authentication.operation_id=$1", &[&operation, &id]).await.expect("original pair");
                assert!(row.get::<_, bool>(0));
                assert_eq!(row.get::<_, &str>(1), match kind { "session" => "authenticated", "registration" => "registrable", _ => "suspended" });
                assert_eq!(row.get::<_, bool>(2), kind == "session");
                assert_eq!(row.get::<_, bool>(3), kind == "registration");
                assert!(row.get::<_, bool>(4) && row.get::<_, bool>(5));
                observer.terminate().await.expect("observer ended");
                assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, initial_authentications + 1);
                assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, initial_sessions + i64::from(kind == "session"));
                assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, initial_handoffs + i64::from(kind == "session"));
                assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, i64::from(kind == "registration"));
                assert_eq!(count(&database, "SELECT count(*) FROM identity.checks WHERE authentication_id IS NOT NULL").await, initial_authentications + 1);
                service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
            }
        });
    }

    #[test]
    fn runtime_rejects_a_session_from_another_application_before_committing_authentication() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(25), async {
                let database = Database::new(server).await;
                let account_id = account(&database, "13800000000", false).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let message_id = message(&database, &service, &mut supplier, "13800000000").await;
                let request = input(Uuid::new_v4(), message_id, "13800000000", CODE);
                let runtime = database.connections(&database.user).recover().await.expect("runtime writer");
                let now = service.0.context.sample().await.expect("trusted time");
                let authentication_id = authentication::Id::new();
                runtime.client().batch_execute("BEGIN").await.expect("creation transaction");
                super::database::insert(&service.0, runtime.client(), &request, authentication_id, super::database::Status::Authenticated, now).await.expect("authentication before deferred check");
                super::database::consume(runtime.client(), message_id, authentication_id).await.expect("original code consumption");
                let settings = crate::identity::session::settings::Settings::new("other-website".parse().expect("different application"), 3600, 600).expect("valid lifetimes");
                let (_, _, _, pending) = crate::identity::session::create(
                    runtime.client(), &service.0.app, &service.0.keys.credential, &settings,
                    crate::identity::session::Input {
                        source: crate::identity::session::Source::Authentication(authentication_id),
                        account_id,
                        operation_id: request.operation_id,
                        created_at: now,
                    },
                ).await.expect("session before deferred check");
                let error = runtime.client().batch_execute("COMMIT").await.expect_err("source mismatch cannot commit");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                pending.rolled_back().expect("rejected authentication settles its Session candidate");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM identity.authentications), (SELECT count(*) FROM identity.sessions), (SELECT count(*) FROM identity.handoffs), authentication_id IS NULL FROM identity.checks WHERE id = $1", &[&message_id]).await.expect("facts before any replay");
                assert_eq!(row.get::<_, i64>(0), 0);
                assert_eq!(row.get::<_, i64>(1), 0);
                assert_eq!(row.get::<_, i64>(2), 0);
                assert!(row.get::<_, bool>(3));
                observer.terminate().await.expect("observer ended");
                runtime.terminate().await.expect("writer ended");
                let original = service.create_session(request).await.expect("same code remains usable");
                assert!(matches!(original, Receipt::Session(_)));
                service.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            }).await.expect("source constraint budget");
        });
    }

    #[test]
    fn five_distinct_wrong_guesses_count_once_and_invalid_messages_never_count() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let id = message(&database, &service, &mut supplier, "13800000000").await;
            for failures in 1..=5 {
                let operation = Uuid::new_v4();
                for _ in 0..2 { assert!(matches!(service.create_session(input(operation, id, "13800000000", "000000")).await, Err(Error::CodeInvalid))); }
                assert_eq!(count(&database, "SELECT sum(failures)::bigint FROM identity.checks").await, failures);
                assert!(matches!(service.create_session(input(operation, id, "13800000000", CODE)).await, Err(Error::Mismatch)));
            }
            assert!(matches!(service.create_session(input(Uuid::new_v4(), id, "13800000000", CODE)).await, Err(Error::Verification)));
            assert!(matches!(service.create_session(input(Uuid::new_v4(), id, "13800000001", CODE)).await, Err(Error::Verification)));
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 5);
            let session = database.connections(&database.user).recover().await.expect("runtime");
            let error = session.client().execute("INSERT INTO identity.authentications (id,operation_id,proof,hash,key_id,purpose,app_id,transport,message_id,status,finished_at) SELECT $1,$2,proof,hash,key_id,purpose,app_id,transport,message_id,status,finished_at FROM identity.authentications LIMIT 1", &[&Uuid::new_v4(), &Uuid::new_v4()]).await.expect_err("standalone refusal cannot counterfeit a counted guess");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            let error = session.client().execute("DELETE FROM identity.authentications", &[]).await.expect_err("retained code protects original refusal evidence");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            let error = session.client().execute("UPDATE identity.accounts SET id = id", &[]).await.expect_err("an account write requires its paired record and advancing head");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            assert_eq!(error.as_db_error().expect("structural rejection").constraint(), Some("accounts_records_check"));
            session.terminate().await.expect("runtime ended");
            service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        });
    }

    #[test]
    fn an_earlier_session_result_cannot_hide_behind_a_later_wrong_guess() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let account_id = account(&database, "13800000000", false).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let message_id = message(&database, &service, &mut supplier, "13800000000").await;
            let wrong_operation = Uuid::new_v4();
            assert!(matches!(service.create_session(input(wrong_operation, message_id, "13800000000", "000000")).await, Err(Error::CodeInvalid)));
            let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
            let snapshot = "SELECT row_to_json(authentication)::text, row_to_json(verification)::text FROM identity.authentications authentication JOIN identity.checks verification ON verification.id=authentication.message_id WHERE authentication.operation_id=$1";
            let original = observer.client().query_one(snapshot, &[&wrong_operation]).await.expect("original wrong guess and check");
            let earlier: chrono::DateTime<chrono::Utc> = observer.client().query_one("SELECT created_at FROM identity.messages WHERE id=$1", &[&message_id]).await.expect("valid earliest code time").get(0);
            let later: chrono::DateTime<chrono::Utc> = observer.client().query_one("SELECT finished_at FROM identity.authentications WHERE operation_id=$1 AND status='invalid'", &[&wrong_operation]).await.expect("later original refusal").get(0);
            assert!(earlier < later);

            let runtime = database.connections(&database.user).recover().await.expect("direct runtime writer");
            runtime.client().batch_execute("BEGIN ISOLATION LEVEL READ COMMITTED").await.expect("atomic forged result");
            let authentication_id = authentication::Id::new();
            let request = input(Uuid::new_v4(), message_id, "13800000000", CODE);
            let key = service.0.keys.input.active();
            let hash = key.sign(&request.encoded(service.0.settings.session().app_id()).expect("canonical command"));
            runtime.client().execute("INSERT INTO identity.authentications (id,operation_id,proof,hash,key_id,purpose,app_id,transport,message_id,status,finished_at) VALUES ($1,$2,$3,$4,$5,'create_session',$6,'cookie',$7,'authenticated',$8)", &[&authentication_id, &request.operation_id, &request.secret.proof(), &hash.as_ref().as_slice(), &key.id(), service.0.settings.session().app_id(), &message_id, &earlier]).await.expect("earlier authentication is deferred until commit");
            let (_, _, _, pending) = crate::identity::session::create(
                runtime.client(), &service.0.app, &service.0.keys.credential,
                service.0.settings.session().issuance(),
                crate::identity::session::Input {
                    source: crate::identity::session::Source::Authentication(authentication_id),
                    account_id, operation_id: request.operation_id, created_at: earlier,
                },
            ).await.expect("paired session, issuance and handoff in the same transaction");
            // Deliberately omit Check consumption; the later invalid result must not mask this row.
            let error = runtime.client().batch_execute("COMMIT").await.expect_err("each inserted authentication must consume its own check");
            pending.rolled_back().expect("rejected authentication settles its session candidate");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            assert_eq!(error.as_db_error().expect("constraint error").message(), "authentication and code effects must agree");
            runtime.terminate().await.expect("writer ended");

            let actual = observer.client().query_one(snapshot, &[&wrong_operation]).await.expect("original facts survived failed commit");
            for column in 0..2 { assert_eq!(actual.get::<_, String>(column), original.get::<_, String>(column)); }
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 1);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, 0);
            observer.terminate().await.expect("observer ended");
            service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        });
    }

    #[test]
    fn recovery_refusals_preserve_read_proof_and_rollback_failures() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::{Failure, dispatch::tests::proxied};

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                let id = message(&database, &service, &mut supplier, "13800000000").await;
                let operation = Uuid::new_v4();
                assert!(matches!(service.create_session(input(operation, id, "13800000000", "000000")).await, Err(Error::CodeInvalid)));
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let snapshot = "SELECT jsonb_build_object(
                    'authentications', (SELECT jsonb_agg(to_jsonb(authentication) ORDER BY id) FROM identity.authentications authentication),
                    'checks', (SELECT jsonb_agg(to_jsonb(checks) ORDER BY id) FROM identity.checks checks),
                    'sessions', (SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session),
                    'registrations', (SELECT jsonb_agg(to_jsonb(registration) ORDER BY id) FROM identity.registrations registration)
                )::text";
                let before: String = observer.client().query_one(snapshot, &[]).await.expect("committed refusal").get(0);
                assert_eq!(count(&database, "SELECT sum(failures)::bigint FROM identity.checks").await, 1);
                let first = context.connections().lease().await.expect("occupy first online slot");
                let second = context.connections().lease().await.expect("occupy second online slot");
                let backends = "SELECT array_agg(pid ORDER BY pid) FROM pg_stat_activity WHERE datname=current_database() AND usename=current_user";
                let original_pids: Vec<i32> = observer.client().query_one(backends, &[]).await.expect("original connections").get(0);
                for fault in [None, Some(Fault::RejectedRollback), Some(Fault::StalledRollback)] {
                    let failed = fault.is_some();
                    let stalled = matches!(fault, Some(Fault::StalledRollback));
                    if let Some(fault) = fault { proxy.arm(fault); }
                    let worker = service.clone();
                    let task = tokio::spawn(async move {
                        let mut request = input(operation, id, "13800000000", "000000");
                        request.secret = "__________________________________________8".parse().expect("wrong original secret");
                        worker.0.recover_session(&request, &mut None).await
                    });
                    if failed { timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("recovery refusal reached ROLLBACK"); }
                    let outcome = timeout(Duration::from_secs(12), task).await.expect("separate rollback budget").expect("recovery worker").expect("known original refusal");
                    let super::Recovery::Refused(error) = outcome else { panic!("wrong original proof"); };
                    if failed {
                        assert_eq!(error.failure(), Failure::Unavailable);
                        let error = match error {
                            Error::Combined { cause, termination } => {
                                assert!(matches!(termination, platform::database::Error::Postgres(_)), "{termination:?}");
                                *cause
                            }
                            error => error,
                        };
                        match error {
                            Error::Rollback { cause, rollback: EndError::Rollback(rollback) } if !stalled => {
                                assert!(matches!(*cause, Error::Proof));
                                assert!(rollback.0.is_closed(), "{rollback:?}");
                            }
                            Error::Rollback { cause, rollback: EndError::RollbackTimeout(_) } if stalled => assert!(matches!(*cause, Error::Proof)),
                            error => panic!("retain the verified refusal and actual cleanup failure: {error:?}"),
                        }
                    } else {
                        assert!(matches!(error, Error::Proof));
                    }
                    let after: String = observer.client().query_one(snapshot, &[]).await.expect("facts before another readback").get(0);
                    assert_eq!(before, after);
                    if stalled { proxy.release(); }
                    timeout(Duration::from_secs(2), async {
                        loop {
                            let pids: Vec<i32> = observer.client().query_one(backends, &[]).await.expect("recovery connection ended").get(0);
                            if pids == original_pids { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("no retained recovery backend");
                }
                first.release();
                second.release();
                // The committed invalid outcome still replays without incrementing the count.
                assert!(matches!(service.create_session(input(operation, id, "13800000000", "000000")).await, Err(Error::CodeInvalid)));
                assert_eq!(count(&database, "SELECT sum(failures)::bigint FROM identity.checks").await, 1);
                observer.terminate().await.expect("observer ended");
                service.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(50), supplier.request()).await.is_err(), "no extra supplier call through replay and drain");
                supplier.stop().await;
                proxy.stop().await;
            }).await.expect("bounded refusal recovery");
        });
    }

    #[test]
    fn committed_invalid_code_survives_readback_commit_response_loss() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                let id = message(&database, &service, &mut supplier, "13800000000").await;
                let operation = Uuid::parse_str("00000000-0000-4000-8000-000000000001").expect("fixed operation");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                proxy.arm(Fault::DelayedCommit);
                let worker = service.clone();
                let task = tokio::spawn(async move { worker.create_session(input(operation, id, "13800000000", "000000")).await });
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original write commit intercepted");
                timeout(Duration::from_secs(3), async { loop {
                    // Python hashlib over the two length-prefixed fields independently fixes this operation's lock.
                    let waiting: bool = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks waiting JOIN pg_catalog.pg_locks original ON original.locktype=waiting.locktype AND original.database=waiting.database AND original.classid=waiting.classid AND original.objid=waiting.objid AND original.objsubid=waiting.objsubid AND original.pid=ANY(pg_catalog.pg_blocking_pids(waiting.pid)) WHERE waiting.locktype='advisory' AND waiting.classid=4118810614::oid AND waiting.objid=3337126846::oid AND waiting.objsubid=1 AND NOT waiting.granted AND waiting.mode='ExclusiveLock' AND original.granted AND original.mode='ExclusiveLock' AND waiting.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()))", &[]).await.expect("same original operation lock").get(0);
                    if waiting { break; } tokio::task::yield_now().await;
                }}).await.expect("readback waits for original transaction");
                proxy.arm(Fault::LostReadCommit);
                proxy.release();
                timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback commit intercepted");
                let result = timeout(Duration::from_secs(3), task).await.expect("bounded readback").expect("worker");
                let row = observer.client().query_one("SELECT authentication.status, authentication.message_id=$2 AND authentication.purpose='create_session' AND authentication.transport='cookie' AS paired, check_record.failures, check_record.authentication_id IS NULL AS unconsumed, NOT EXISTS(SELECT 1 FROM identity.sessions) AND NOT EXISTS(SELECT 1 FROM identity.registrations) AND NOT EXISTS(SELECT 1 FROM identity.handoffs) AS no_credential FROM identity.authentications authentication JOIN identity.checks check_record ON check_record.id=authentication.message_id WHERE authentication.operation_id=$1", &[&operation, &id]).await.expect("independent committed refusal before replay");
                assert_eq!(row.get::<_, &str>("status"), "invalid");
                assert!(row.get::<_, bool>("paired") && row.get::<_, bool>("unconsumed") && row.get::<_, bool>("no_credential"));
                assert_eq!(row.get::<_, i16>("failures"), 1);
                assert!(matches!(result, Err(Error::CodeInvalid)), "known invalid code retained: {:?}", result.err());
                proxy.release();
                assert!(matches!(service.create_session(input(operation, id, "13800000000", "000000")).await, Err(Error::CodeInvalid)));
                assert_eq!(count(&database, "SELECT sum(failures)::bigint FROM identity.checks").await, 1);
                proxy.arm(Fault::StalledReadCommit);
                let worker = service.clone();
                let task = tokio::spawn(async move { worker.create_session(input(operation, id, "13800000000", "000000")).await });
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("complete original invalid result before first read COMMIT ends");
                proxy.arm(Fault::RejectedBegin);
                proxy.release();
                let result = timeout(Duration::from_secs(3),task).await.expect("original read completion bounded").expect("worker");
                let row = observer.client().query_one("SELECT authentication.status, check_record.failures, check_record.authentication_id IS NULL AS unconsumed FROM identity.authentications authentication JOIN identity.checks check_record ON check_record.id=authentication.message_id WHERE authentication.operation_id=$1", &[&operation]).await.expect("unchanged original refusal before further replay");
                assert_eq!(row.get::<_, &str>("status"),"invalid");
                assert_eq!(row.get::<_, i16>("failures"),1);
                assert!(row.get::<_, bool>("unconsumed"));
                assert!(matches!(result,Err(Error::CodeInvalid)), "first complete original read must survive cleanup without a second recovery: {:?}",result.err());
                observer.terminate().await.expect("observer ended");
                service.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no extra supplier request through replay and drain");
                supplier.stop().await; proxy.stop().await;
            }).await.expect("readback acceptance budget");
        });
    }

    #[test]
    fn absent_session_readback_keeps_cleanup_causes_and_incomplete_reads_unknown() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::{Failure, dispatch::tests::proxied};

        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("real driver observation").block_on(async {
            for fault in [Fault::LostReadCommit, Fault::StalledReadCommit, Fault::ReadCommitTermination, Fault::StalledQuery] {
                timeout(Duration::from_secs(35), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                    let id = message(&database, &service, &mut supplier, "13800000000").await;
                    let operation = Uuid::new_v4();
                    proxy.arm(Fault::DelayedRollback);
                    let worker = service.clone();
                    let task = tokio::spawn(async move { worker.create_session(input(operation, id, "13800000000", CODE)).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("write held before rollback");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let gate = database.connections(&database.user).recover().await.expect("test operation gate");
                    let gate_pid: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate backend").get(0);
                    let gate_context = context.clone();
                    let gate_task = tokio::spawn(async move {
                        super::transaction::begin_for(&gate_context, gate.client(), b"authentication/operation", operation.to_string().parse().expect("operation")).await.expect("gate takes exact original operation");
                        gate
                    });
                    let mut backends: Vec<i32> = timeout(Duration::from_secs(3), async { loop {
                        let blockers: Vec<i32> = observer.client().query_one("SELECT pg_catalog.pg_blocking_pids($1)", &[&gate_pid]).await.expect("gate waits for original backend").get(0);
                        if !blockers.is_empty() { break blockers; } tokio::task::yield_now().await;
                    }}).await.expect("gate really queued before releasing write");
                    proxy.release();
                    let gate = timeout(Duration::from_secs(3), gate_task).await.expect("write rollback releases gate").expect("gate task");
                    let recovery_pid = timeout(Duration::from_secs(3), async { loop {
                        if let Some(row) = observer.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&gate_pid]).await.expect("actual recovery waits for exact operation gate") { break row.get::<_, i32>(0); }
                        tokio::task::yield_now().await;
                    }}).await.expect("recovery gate wait");
                    backends.push(recovery_pid);
                    let stalled = matches!(fault, Fault::StalledReadCommit);
                    let terminated = matches!(fault, Fault::ReadCommitTermination);
                    let incomplete = matches!(fault, Fault::StalledQuery);
                    proxy.arm(fault);
                    gate.client().batch_execute("ROLLBACK").await.expect("release gate"); gate.terminate().await.expect("gate ended");
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("readback fault intercepted");
                    let error = timeout(Duration::from_secs(12), task).await.expect("same readback deadline").expect("worker").err().expect("no original authentication");
                    let row = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.authentications) AND NOT EXISTS(SELECT 1 FROM identity.sessions) AND NOT EXISTS(SELECT 1 FROM identity.registrations) AND NOT EXISTS(SELECT 1 FROM identity.handoffs) AS absent, failures=0 AND authentication_id IS NULL AS untouched FROM identity.checks WHERE id=$1", &[&id]).await.expect("independent absence and intact code before replay");
                    assert!(row.get::<_, bool>("absent") && row.get::<_, bool>("untouched"));
                    if incomplete {
                        let Error::Unknown { recovery, .. } = error else { panic!("unfinished read is unknown: {error:?}"); };
                        assert!(matches!(*recovery, Error::Timeout(_)));
                    } else {
                        assert_eq!(error.failure(), Failure::Unavailable);
                        let Error::NotCommitted { cause } = error else { panic!("complete absence remains known: {error:?}"); };
                        let Error::SessionReadback { cause, commit, termination } = *cause else { panic!("retain original and cleanup causes"); };
                        assert!(matches!(*cause, Error::Query(_) | Error::Timeout(_) | Error::Combined { .. }));
                        if stalled {
                            assert!(matches!(commit, Err(EndError::CommitTimeout(_)))); assert!(termination.is_ok());
                        } else if terminated {
                            assert!(commit.is_ok());
                            assert!(matches!(termination, Err(platform::database::Error::Postgres(ref error)) if error.code()==Some(&tokio_postgres::error::SqlState::CONNECTION_FAILURE)));
                        } else {
                            assert!(matches!(commit, Err(EndError::Commit(ref error)) if error.0.is_closed()));
                        }
                    }
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        let ended: bool = observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&backends]).await.expect("actual original and recovery backend end").get(0);
                        if ended { break; } tokio::task::yield_now().await;
                    }}).await.expect("both backends ended before retry");
                    assert!(matches!(service.create_session(input(operation, id, "13800000000", CODE)).await, Ok(Receipt::Registration(_))));
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 1);
                    observer.terminate().await.expect("observer ended");
                    service.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no extra request through replay and drain");
                    supplier.stop().await; proxy.stop().await;
                }).await.expect("readback scenario budget");
            }
        });
    }

    #[test]
    fn real_commit_faults_recover_atomic_results_and_confirm_rollback() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for wrong in [false, true] {
                for fault in [Fault::LostCommit, Fault::DelayedCommit, Fault::RolledBack] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
                    let id = message(&database, &service, &mut supplier, "13800000000").await;
                    let operation = Uuid::nil();
                    let code = if wrong { "000000" } else { CODE };
                    let delayed = matches!(fault, Fault::DelayedCommit);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    proxy.arm(fault);
                    let worker = service.clone();
                    let task = tokio::spawn(async move { worker.create_session(input(operation, id, "13800000000", code)).await });
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("R1 COMMIT intercepted");
                    if delayed {
                        let observer = database.connections(&database.user).recover().await.expect("observer");
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let waiting: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND NOT granted AND database = (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database()) AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0)", &[]).await.expect("lock observation").get(0);
                                if waiting { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("recovery waits for the original transaction");
                        assert!(!task.is_finished());
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 0);
                        observer.terminate().await.expect("observer ended");
                    }
                    proxy.release();
                    let result = timeout(Duration::from_secs(5), task).await.expect("bounded recovery").expect("worker");
                    if rolled_back {
                        assert!(matches!(result, Err(Error::NotCommitted { .. })));
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 0);
                        assert_eq!(count(&database, "SELECT sum(failures)::bigint FROM identity.checks").await, 0);
                        assert_eq!(count(&database, "SELECT count(*) FROM identity.checks WHERE authentication_id IS NOT NULL").await, 0);
                    } else if wrong {
                        assert!(matches!(result, Err(Error::CodeInvalid)));
                        assert_eq!(count(&database, "SELECT sum(failures)::bigint FROM identity.checks").await, 1);
                    } else {
                        let Receipt::Registration(original) = result.expect("recovered credential") else { panic!("registration"); };
                        let Receipt::Registration(replay) = service.create_session(input(operation, id, "13800000000", code)).await.expect("replay") else { panic!("registration"); };
                        assert_eq!(original.id(), replay.id());
                        assert_eq!(original.token().text(), replay.token().text());
                    }
                    service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await; proxy.stop().await;
                }
            }
        });
    }

    #[test]
    fn cancellation_and_other_secrets_do_not_steal_the_original_commit() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, service) = database
                .start_on(supplier.client(), proxied(&database, &proxy))
                .await;
            let id = message(&database, &service, &mut supplier, "13800000000").await;
            let operation = Uuid::new_v4();
            proxy.arm(Fault::DelayedCommit);
            let worker = service.clone();
            let original = tokio::spawn(async move {
                worker
                    .create_session(input(operation, id, "13800000000", CODE))
                    .await
            });
            timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("commit intercepted");
            original.abort();
            assert!(matches!(original.await, Err(error) if error.is_cancelled()));
            let worker = service.clone();
            let valid = tokio::spawn(async move {
                worker
                    .create_session(input(operation, id, "13800000000", CODE))
                    .await
            });
            let worker = service.clone();
            let invalid = tokio::spawn(async move {
                let mut request = input(operation, id, "13800000000", CODE);
                request.secret = "__________________________________________8"
                    .parse()
                    .expect("other secret");
                worker.create_session(request).await
            });
            proxy.release();
            let Receipt::Registration(original) = timeout(Duration::from_secs(5), valid)
                .await
                .expect("bounded")
                .expect("waiter")
                .expect("same proof")
            else {
                panic!("registration");
            };
            assert!(matches!(
                timeout(Duration::from_secs(5), invalid)
                    .await
                    .expect("bounded")
                    .expect("waiter"),
                Err(Error::Proof)
            ));
            assert_eq!(
                count(&database, "SELECT count(*) FROM identity.authentications").await,
                1
            );
            service.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            let (context, service) = database.start(supplier.client()).await;
            let Receipt::Registration(replay) = service
                .create_session(input(operation, id, "13800000000", CODE))
                .await
                .expect("restart recovery")
            else {
                panic!("registration");
            };
            assert_eq!(original.id(), replay.id());
            assert_eq!(original.token().text(), replay.token().text());
            assert_eq!(original.expires_at(), replay.expires_at());
            let first = context
                .connections()
                .lease()
                .await
                .expect("first occupied slot");
            let second = context
                .connections()
                .lease()
                .await
                .expect("second occupied slot");
            let Receipt::Registration(recovered) = timeout(
                Duration::from_secs(6),
                service.create_session(input(operation, id, "13800000000", CODE)),
            )
            .await
            .expect("bounded pool recovery")
            .expect("recovery connection bypasses exhausted online pool") else {
                panic!("registration");
            };
            assert_eq!(original.token().text(), recovered.token().text());
            first.release();
            second.release();
            service.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn begin_failure_recovers_existing_results_and_unknown_commit_waits_for_original_backend() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, service) = database
                .start_on(supplier.client(), proxied(&database, &proxy))
                .await;
            let id = message(&database, &service, &mut supplier, "13800000000").await;
            let operation = Uuid::new_v4();
            proxy.arm(Fault::RejectedBegin);
            assert!(matches!(
                service
                    .create_session(input(operation, id, "13800000000", CODE))
                    .await,
                Err(Error::NotCommitted { .. })
            ));
            assert_eq!(
                count(&database, "SELECT count(*) FROM identity.authentications").await,
                0
            );
            let Receipt::Registration(original) = service
                .create_session(input(operation, id, "13800000000", CODE))
                .await
                .expect("new accepted operation after confirmed no write")
            else {
                panic!("registration");
            };
            proxy.arm(Fault::RejectedBegin);
            let Receipt::Registration(recovered) = service
                .create_session(input(operation, id, "13800000000", CODE))
                .await
                .expect("existing result through independent recovery")
            else {
                panic!("registration");
            };
            assert_eq!(original.token().text(), recovered.token().text());
            let id = message(&database, &service, &mut supplier, "13800000001").await;
            let operation = Uuid::new_v4();
            proxy.arm(Fault::UnknownCommit);
            let worker = service.clone();
            let task = tokio::spawn(async move {
                worker
                    .create_session(input(operation, id, "13800000001", CODE))
                    .await
            });
            timeout(Duration::from_secs(5), proxy.intercepted())
                .await
                .expect("original commit held");
            let result = timeout(Duration::from_secs(5), task)
                .await
                .expect("bounded unknown outcome")
                .expect("worker");
            assert!(matches!(result, Err(Error::Unknown { .. })));
            assert_eq!(
                count(&database, "SELECT count(*) FROM identity.authentications").await,
                1
            );
            proxy.release();
            let Receipt::Registration(recovered) = timeout(
                Duration::from_secs(5),
                service.create_session(input(operation, id, "13800000001", CODE)),
            )
            .await
            .expect("late commit recovery bounded")
            .expect("original late commit") else {
                panic!("registration");
            };
            let Receipt::Registration(replay) = service
                .create_session(input(operation, id, "13800000001", CODE))
                .await
                .expect("original replay")
            else {
                panic!("registration");
            };
            assert_eq!(recovered.token().text(), replay.token().text());
            assert_eq!(
                count(&database, "SELECT count(*) FROM identity.authentications").await,
                2
            );
            service.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
            proxy.stop().await;
        });
    }

    #[test]
    fn original_handoff_damage_remains_a_failure_even_after_its_delivery_deadline() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for expired in [false, true] {
                for (corruption, expected) in [
                    ("UPDATE identity.sessions SET hash=set_byte(hash,0,get_byte(hash,0)#1)", "session digest matches its original handoff token"),
                    ("UPDATE identity.handoffs SET expires_at=(SELECT created_at FROM identity.sessions WHERE id=identity.handoffs.id)", "session preserves its authentication source and credential lifetime"),
                    ("UPDATE identity.handoffs SET expires_at=(SELECT expires_at + interval '1 second' FROM identity.sessions WHERE id=identity.handoffs.id)", "session preserves its authentication source and credential lifetime"),
                ] {
                timeout(Duration::from_secs(25), async {
                    let database = Database::new(Arc::clone(&server)).await;
                    account(&database, "13800000000", false).await;
                    let mut supplier = Supplier::new().await;
                    let (context, service) = database.start(supplier.client()).await;
                    let message_id = message(&database, &service, &mut supplier, "13800000000").await;
                    let operation_id = Uuid::new_v4();
                    service.create_session(input(operation_id, message_id, "13800000000", CODE)).await.expect("original session");
                    if expired { age(&database, &service, TimeDelta::minutes(11)).await; }
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption fixture");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("fixture-only corruption transaction");
                    assert_eq!(owner.client().execute(corruption, &[]).await.expect("one damaged handoff relationship"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("corruption committed");
                    owner.terminate().await.expect("fixture ended");
                    let error = match service.create_session(input(operation_id, message_id, "13800000000", CODE)).await {
                        Ok(_) => panic!("damaged original handoff cannot be delivered"),
                        Err(error) => error,
                    };
                    assert!(error.unreliable());
                    let Error::Unknown { cause, recovery } = error else {
                        panic!("damaged readback must preserve both failures: {error:?}");
                    };
                    for error in [cause, recovery] {
                        eprintln!("handoff corruption: expired={expired}, session_state={}, error={error}", corruption.starts_with("UPDATE identity.sessions"));
                        if corruption.starts_with("UPDATE identity.sessions") {
                            assert!(matches!(*error, Error::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))), "expiry must not hide a damaged session seal: {error}");
                        } else {
                            assert!(matches!(*error, Error::Facts { rule, .. } if rule == expected), "expiry must not hide handoff damage: {error}");
                        }
                    }
                    assert!(matches!(service.ready().await, Err(Error::Closed)));
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 1);
                    assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, 1);
                    service.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    supplier.stop().await;
                }).await.expect("original handoff failure budget");
                }
            }
        });
    }

    #[test]
    fn credential_lifetimes_do_not_erase_original_results_or_allow_reissue() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let account = account(&database, "13800000000", false).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let message_id = message(&database, &service, &mut supplier, "13800000000").await;
            let operation = Uuid::new_v4();
            let Receipt::Session(original) = service.create_session(input(operation, message_id, "13800000000", CODE)).await.expect("session") else { panic!("session"); };
            let observer = database.connections(&database.user).recover().await.expect("observer");
            let handoff = observer.client().query_one("SELECT token, expires_at FROM identity.handoffs WHERE id=$1", &[&original.id()]).await.expect("original handoff");
            age(&database, &service, TimeDelta::minutes(11)).await;
            assert!(matches!(service.create_session(input(operation, message_id, "13800000000", CODE)).await, Err(Error::Verification)));
            let mut cleanup = service.retention(); cleanup.run().await.expect("handoff cleanup");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 1);
            let error = observer.client().execute("INSERT INTO identity.handoffs (id,token,expires_at) VALUES ($1,$2,$3)", &[&original.id(), &handoff.get::<_, Vec<u8>>(0), &handoff.get::<_, chrono::DateTime<chrono::Utc>>(1)]).await.expect_err("old session cannot rebuild delivery");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            let error = observer.client().execute("INSERT INTO identity.authentications (id,operation_id,proof,hash,key_id,purpose,app_id,transport,message_id,status,finished_at,transaction_id) SELECT $1,$2,proof,hash,key_id,purpose,app_id,transport,message_id,status,finished_at,pg_current_xact_id() FROM identity.authentications", &[&Uuid::new_v4(), &Uuid::new_v4()]).await.expect_err("creation transaction is not caller-controlled");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
            age(&database, &service, TimeDelta::hours(24)).await;
            service.0.context.sample().await.expect("observed time");
            let error = observer.client().execute("DELETE FROM identity.sessions", &[]).await.expect_err("expired session retains a day of history");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            cleanup.run().await.expect("retention respects session history");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 1);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 1);
            let logout_message_id = message(&database, &service, &mut supplier, "13800000000").await;
            let Receipt::Session(revoked) = service.create_session(input(Uuid::new_v4(), logout_message_id, "13800000000", CODE)).await.expect("later current session") else { panic!("session"); };
            let actor = service.0.current.authenticate(crate::identity::current::Proof {
                id: revoked.id(), token: revoked.token().text().parse().expect("original token"),
            }).await.expect("current logout actor");
            let operation_id = Uuid::new_v4().to_string().parse().expect("logout operation");
            let revocation = crate::identity::revocation::Service::prepare(service.0.app.clone(), service.0.current.clone());
            let completed = revocation.revoke(actor, operation_id, crate::identity::revocation::Scope::Current).await.expect("actual current logout");
            assert_eq!(completed.count().get(), 1);
            let original_logout = observer.client().query_one("SELECT account_id=$2 AND initiator_id=$3 AND scope='current' AND sessions=ARRAY[$3::uuid] AND revoked_at=$4 AS original FROM identity.logouts WHERE id=$1", &[&operation_id, &account, &revoked.id(), &completed.revoked_at()]).await.expect("permanent original logout and fixed session set");
            assert!(original_logout.get::<_, bool>("original"));
            let error = observer.client().execute("DELETE FROM identity.sessions WHERE id=$1", &[&revoked.id()]).await.expect_err("revocation retains its session for 24 hours");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            cleanup.run().await.expect("fresh revocation remains retained");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 2);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.issuances").await, 2);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.revocations").await, 1);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 2);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, 0);
            age(&database, &service, TimeDelta::hours(24)).await;
            assert!(observer.client().query_one("SELECT revoked_at + interval '24 hours' <= clock_timestamp() AND expires_at + interval '24 hours' > clock_timestamp() FROM identity.sessions WHERE id=$1", &[&revoked.id()]).await.expect("revocation retention ended before expiry retention").get::<_, bool>(0));
            // The later session is eligible by revocation, while its expiry history is still retained.
            cleanup.run().await.expect("revoked session retention ended");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.sessions").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.issuances").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.revocations").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.handoffs").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 1);
            let reference = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.checks WHERE id=authentication.message_id AND authentication_id=authentication.id) AS retained, greatest(challenge.expires_at,challenge.finished_at,message.expires_at,message.finished_at)+interval '24 hours' AS until FROM identity.authentications authentication JOIN identity.messages message ON message.id=authentication.message_id JOIN identity.challenges challenge ON challenge.id=message.challenge_id WHERE authentication.message_id=$1", &[&logout_message_id]).await.expect("last authentication source and its actual retention deadline");
            assert!(reference.get::<_, bool>("retained"));
            let until: chrono::DateTime<chrono::Utc> = reference.get("until");
            let now = database.now().await;
            assert!(until > now, "the code's last reference outlives the revoked session group");
            let error = observer.client().execute("DELETE FROM identity.authentications WHERE message_id=$1", &[&logout_message_id]).await.expect_err("retained check still protects the original authentication");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION));
            assert_eq!(error.as_db_error().expect("retained check reference").constraint(), Some("checks_authentication_id_fkey"));
            let remaining = until.signed_duration_since(now) + TimeDelta::microseconds(1);
            age(&database, &service, remaining).await;
            cleanup.run().await.expect("last C1 reference retention ended");
            cleanup.run().await.expect("original authentication cleanup after C1");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.checks").await, 0);
            let revoked_at = completed.revoked_at() - TimeDelta::hours(24) - remaining;
            let retained = observer.client().query_one("SELECT account_id=$2 AND initiator_id=$3 AND scope='current' AND sessions=ARRAY[$3::uuid] AND revoked_at=$4 AS original FROM identity.logouts WHERE id=$1", &[&operation_id, &account, &revoked.id(), &revoked_at]).await.expect("permanent logout survives temporary session group cleanup");
            assert!(retained.get::<_, bool>("original"));
            assert_eq!(count(&database, "SELECT count(*) FROM identity.logouts").await, 1);
            assert!(matches!(service.create_session(input(operation, message_id, "13800000000", CODE)).await, Err(Error::Verification)));
            observer.terminate().await.expect("observer ended");
            service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        });
    }

    #[test]
    fn replacement_and_expiry_close_registration_without_changing_the_old_authentication() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let first = message(&database, &service, &mut supplier, "13800000000").await;
            let operation = Uuid::new_v4();
            service.create_session(input(operation, first, "13800000000", CODE)).await.expect("first registration");
            let second = message(&database, &service, &mut supplier, "13800000000").await;
            service.create_session(input(Uuid::new_v4(), second, "13800000000", CODE)).await.expect("replacement registration");
            assert!(matches!(service.create_session(input(operation, first, "13800000000", CODE)).await, Err(Error::Verification)));
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications WHERE status='registrable'").await, 2);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, 1);
            age(&database, &service, TimeDelta::minutes(11)).await;
            let observer = database.connections(&database.user).recover().await.expect("observer");
            let saved = observer.client().query_one("SELECT id,authentication_id,mobile,token,created_at,expires_at FROM identity.registrations", &[]).await.expect("original credential");
            service.retention().run().await.expect("expired registration cleanup");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, 0);
            let error = observer.client().execute("INSERT INTO identity.registrations (id,authentication_id,mobile,token,created_at,expires_at) VALUES ($1,$2,$3,$4,$5,$6)", &[&saved.get::<_, Uuid>(0), &saved.get::<_, Uuid>(1), &saved.get::<_, Envelope>(2), &saved.get::<_, Vec<u8>>(3), &saved.get::<_, chrono::DateTime<chrono::Utc>>(4), &saved.get::<_, chrono::DateTime<chrono::Utc>>(5)]).await.expect_err("old authentication cannot recreate a credential");
            assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
            observer.terminate().await.expect("observer ended");
            service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        });
    }

    #[test]
    fn cleanup_faults_preserve_errors_and_reconcile_the_original_credential() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for fault in [
                Fault::LostCommit,
                Fault::DelayedCommit,
                Fault::RolledBack,
                Fault::RejectedBegin,
            ] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, service) = database
                    .start_on(supplier.client(), proxied(&database, &proxy))
                    .await;
                let id = message(&database, &service, &mut supplier, "13800000000").await;
                let operation = Uuid::new_v4();
                service
                    .create_session(input(operation, id, "13800000000", CODE))
                    .await
                    .expect("registration");
                age(&database, &service, TimeDelta::minutes(11)).await;
                let committed = matches!(fault, Fault::LostCommit | Fault::DelayedCommit);
                let delayed = matches!(fault, Fault::DelayedCommit);
                proxy.arm(fault);
                let worker = service.clone();
                let task = tokio::spawn(async move {
                    super::retention::reclaim(
                        &worker.0,
                        operation.to_string().parse().expect("operation"),
                    )
                    .await
                });
                timeout(Duration::from_secs(5), proxy.intercepted())
                    .await
                    .expect("cleanup fault intercepted");
                if delayed {
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.registrations").await,
                        1
                    );
                    assert!(!task.is_finished());
                }
                proxy.release();
                let result = timeout(Duration::from_secs(5), task)
                    .await
                    .expect("bounded cleanup recovery")
                    .expect("worker");
                assert_eq!(result.is_ok(), committed, "{result:?}");
                assert_eq!(
                    count(&database, "SELECT count(*) FROM identity.registrations").await,
                    i64::from(!committed)
                );
                assert_eq!(
                    count(&database, "SELECT count(*) FROM identity.authentications").await,
                    1
                );
                service.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
                proxy.stop().await;
            }
        });
    }

    #[test]
    fn cleanup_selection_failure_preserves_the_original_error_and_all_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let id = message(&database, &service, &mut supplier, "13800000000").await;
            let operation = Uuid::new_v4();
            service.create_session(input(operation, id, "13800000000", CODE)).await.expect("registration");
            age(&database, &service, TimeDelta::minutes(11)).await;
            let owner = database.connections("postgres").recover().await.expect("isolated failure fixture");
            let snapshot = "SELECT row_to_json(authentication)::text, row_to_json(registration)::text FROM identity.authentications authentication JOIN identity.registrations registration ON registration.authentication_id=authentication.id WHERE authentication.operation_id=$1";
            let original = owner.client().query_one(snapshot, &[&operation]).await.expect("original facts");
            let revoke: String = owner.client().query_one("SELECT format('REVOKE SELECT ON identity.registrations FROM %I', $1::text)", &[&database.user]).await.expect("quote runtime role").get(0);
            owner.client().batch_execute(&revoke).await.expect("fail selection after original authentication lookup");
            let result = super::retention::reclaim(&service.0, operation.to_string().parse().expect("operation")).await;
            assert!(matches!(result, Err(Error::Query(ref error)) if error.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)));
            let actual = owner.client().query_one(snapshot, &[&operation]).await.expect("facts survived selection failure");
            for column in 0..2 { assert_eq!(actual.get::<_, String>(column), original.get::<_, String>(column)); }
            let grant: String = owner.client().query_one("SELECT format('GRANT SELECT ON identity.registrations TO %I', $1::text)", &[&database.user]).await.expect("quote runtime role").get(0);
            owner.client().batch_execute(&grant).await.expect("restore runtime permission");
            owner.terminate().await.expect("fixture ended");
            super::retention::reclaim(&service.0, operation.to_string().parse().expect("operation")).await.expect("later complete selection can clean up");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, 0);
            service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        });
    }

    #[test]
    fn cleanup_rollback_allows_another_request_to_replace_its_short_lived_child() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::dispatch::tests::proxied;
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for option in [None, Some(Fault::StalledReadCommit), Some(Fault::StalledQuery)] {
            timeout(Duration::from_secs(35), async {
            let database = Database::initialized(Arc::clone(&server)).await;
            let mut supplier = Supplier::new().await;
            let proxy = Proxy::start(&server).await;
            let (context, service) = database.start_on(supplier.client(), proxied(&database, &proxy)).await;
            let first = message(&database, &service, &mut supplier, "13800000000").await;
            let operation = Uuid::nil();
            service.create_session(input(operation, first, "13800000000", CODE)).await.expect("old registration");
            age(&database, &service, TimeDelta::hours(25)).await;
            let owner = database.connections("postgres").recover().await.expect("historical fixture");
            let original = owner.client().query_one("SELECT authentication.id AS authentication_id,registration.id AS registration_id FROM identity.authentications authentication JOIN identity.registrations registration ON registration.authentication_id=authentication.id WHERE authentication.operation_id=$1", &[&operation]).await.expect("selected original identities");
            let authentication_id: Uuid = original.get("authentication_id");
            let registration_id: Uuid = original.get("registration_id");
            let challenge: Uuid = owner.client().query_one("SELECT challenge_id FROM identity.messages WHERE id=$1", &[&first]).await.expect("old challenge").get(0);
            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("fixture history cleanup");
            owner.client().execute("DELETE FROM identity.checks WHERE id=$1", &[&first]).await.expect("old check");
            owner.client().execute("DELETE FROM identity.messages WHERE id=$1", &[&first]).await.expect("old message");
            owner.client().execute("DELETE FROM identity.challenges WHERE id=$1", &[&challenge]).await.expect("old challenge");
            owner.client().batch_execute("COMMIT").await.expect("fixture cleanup committed");
            let second = message(&database, &service, &mut supplier, "13800000000").await;
            proxy.arm(Fault::DelayedRollback);
            let worker = service.clone();
            let task = tokio::spawn(async move { super::retention::reclaim(&worker.0, operation.to_string().parse().expect("operation")).await });
            timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("deletion ready to roll back");
            let gate = database.connections(&database.user).recover().await.expect("recovery gate");
            let gate_pid: i32 = gate.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("gate PID").get(0);
            gate.client().batch_execute("BEGIN").await.expect("gate transaction");
            // Independent Python SHA-256 of E("authentication/operation", nil UUID).
            let waiting = tokio::spawn(async move {
                gate.client().query_one("SELECT pg_advisory_xact_lock($1)", &[&-8_296_254_324_722_009_340_i64]).await.expect("gate takes original operation after rollback");
                gate
            });
            let mut backends: Vec<i32> = timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(row) = owner.client().query_opt("SELECT pg_catalog.pg_blocking_pids(pid) FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' AND NOT granted AND classid=2363345061::oid AND objid=2829417220::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database())", &[&gate_pid]).await.expect("gate queued behind original deletion") {
                        let blockers: Vec<i32> = row.get(0);
                        if !blockers.is_empty() { break blockers; }
                    }
                    tokio::task::yield_now().await;
                }
            }).await.expect("gate precedes recovery");
            proxy.release();
            let gate = timeout(Duration::from_secs(5), waiting).await.expect("gate acquired").expect("gate task");
            service.create_session(input(Uuid::new_v4(), second, "13800000000", CODE)).await.expect("other original operation replaces registration");
            assert!(!task.is_finished());
            let recovery_pid = timeout(Duration::from_secs(3),async { loop {
                if let Some(row) = owner.client().query_opt("SELECT pid FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND classid=2363345061::oid AND objid=2829417220::oid AND objsubid=1 AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND $1=ANY(pg_catalog.pg_blocking_pids(pid))", &[&gate_pid]).await.expect("recovery queued on exact original operation") { break row.get::<_,i32>(0); }
                tokio::task::yield_now().await;
            }}).await.expect("recovery waits after legal replacement");
            backends.push(recovery_pid);
            let incomplete = matches!(option,Some(Fault::StalledQuery));
            let injected = option.is_some();
            if let Some(fault) = option { proxy.arm(fault); }
            let recovery = tokio::time::Instant::now();
            gate.client().batch_execute("COMMIT").await.expect("allow deletion recovery");
            gate.terminate().await.expect("gate ended");
            if injected { timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual recovery fault"); }
            let error = timeout(Duration::from_secs(12),task).await.expect("recovery bounded by one deadline").expect("worker").expect_err("retained original owner");
            assert!(recovery.elapsed()<Duration::from_secs(12));
            assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.authentications WHERE id=$1 AND operation_id=$3) AND NOT EXISTS(SELECT 1 FROM identity.registrations WHERE id=$2) AND EXISTS(SELECT 1 FROM identity.registrations WHERE authentication_id<>$1)", &[&authentication_id,&registration_id,&operation]).await.expect("independent original owner and lawful child replacement before another cleanup").get::<_,bool>(0));
            if incomplete {
                assert!(matches!(error,Error::Unknown { recovery,.. } if matches!(*recovery,Error::Timeout(_))));
            } else {
                let Error::NotCommitted { cause } = error else { panic!("original owner remains known: {error:?}"); };
                let Error::SessionReadback { cause,commit,termination } = *cause else { panic!("cleanup causes retained"); };
                assert!(matches!(*cause,Error::Query(_) | Error::Combined{..}));
                if injected { assert!(matches!(commit,Err(EndError::CommitTimeout(_)))); }
                else { assert!(commit.is_ok()); }
                assert!(termination.is_ok());
            }
            proxy.release();
            timeout(Duration::from_secs(3),async { loop {
                if owner.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1))", &[&backends]).await.expect("actual backend termination").get::<_,bool>(0) { break; }
                tokio::task::yield_now().await;
            }}).await.expect("original and recovery backends ended");
            service.ready().await.expect("lawful replacement does not close admission");
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 2);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.registrations").await, 1);
            owner.terminate().await.expect("observer ended");
            service.retention().run().await.expect("later cleanup of remaining original owner");
            service.drain().await.expect("drained"); context.stop().await.expect("stopped");
            assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(), "no supplier call through recovery, later cleanup and drain");
            supplier.stop().await; proxy.stop().await;
            }).await.expect("legal replacement cleanup budget");
            }
        });
    }

    #[test]
    fn account_lock_conflicts_with_writers_and_creation_provenance_survives_savepoints() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::new(server).await;
            let id = account(&database, "13800000000", false).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let runtime = database
                .connections(&database.user)
                .recover()
                .await
                .expect("runtime");
            runtime
                .client()
                .batch_execute("BEGIN")
                .await
                .expect("read transaction");
            runtime
                .client()
                .query_one("SELECT identity.lock_account($1)", &[&id])
                .await
                .expect("limited lock");
            let owner = database
                .connections("postgres")
                .recover()
                .await
                .expect("fixture writer");
            owner
                .client()
                .batch_execute("SET statement_timeout = '100ms'")
                .await
                .expect("bounded conflicting write");
            let error = owner
                .client()
                .execute(
                    "UPDATE identity.accounts SET mobile=mobile WHERE id=$1",
                    &[&id],
                )
                .await
                .expect_err("shared lock blocks account changes");
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
            );
            runtime
                .client()
                .batch_execute("COMMIT")
                .await
                .expect("release lock");
            let error = owner
                .client()
                .execute(
                    "UPDATE identity.accounts SET mobile=mobile WHERE id=$1",
                    &[&id],
                )
                .await
                .expect_err("released writer reaches the mobile completion guard");
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
            );
            owner.terminate().await.expect("writer ended");
            let message_id = message(&database, &service, &mut supplier, "13800000001").await;
            let request = input(Uuid::new_v4(), message_id, "13800000001", CODE);
            runtime
                .client()
                .batch_execute("BEGIN; SAVEPOINT accepted")
                .await
                .expect("subtransaction");
            let at = service.0.context.sample().await.expect("time");
            let authentication = authentication::Id::new();
            super::database::insert(
                &service.0,
                runtime.client(),
                &request,
                authentication,
                super::database::Status::Registrable,
                at,
            )
            .await
            .expect("authentication in savepoint");
            super::database::consume(runtime.client(), message_id, authentication)
                .await
                .expect("paired consumption");
            super::database::create_registration(
                &service.0,
                runtime.client(),
                &request,
                authentication,
                at,
            )
            .await
            .expect("credential in savepoint");
            runtime
                .client()
                .batch_execute("RELEASE SAVEPOINT accepted; COMMIT")
                .await
                .expect("full xid provenance accepts savepoints");
            assert_eq!(
                count(&database, "SELECT count(*) FROM identity.registrations").await,
                1
            );
            runtime.terminate().await.expect("runtime ended");
            service.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
        });
    }

    #[test]
    fn competing_operations_can_consume_a_code_only_once() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let id = message(&database, &service, &mut supplier, "13800000000").await;
            let (first, second) = tokio::join!(
                service.create_session(input(Uuid::new_v4(), id, "13800000000", CODE)),
                service.create_session(input(Uuid::new_v4(), id, "13800000000", CODE))
            );
            assert!(matches!(
                (first, second),
                (Ok(Receipt::Registration(_)), Err(Error::Verification))
                    | (Err(Error::Verification), Ok(Receipt::Registration(_)))
            ));
            assert_eq!(
                count(&database, "SELECT count(*) FROM identity.authentications").await,
                1
            );
            service.drain().await.expect("drained");
            context.stop().await.expect("stopped");
            supplier.stop().await;
        });
    }

    #[test]
    fn check_wait_crossing_expiry_does_not_guess_or_consume() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let mut supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let id = message(&database, &service, &mut supplier, "13800000000").await;
            age(&database, &service, TimeDelta::seconds(297)).await;
            let blocker = database.connections(&database.user).recover().await.expect("check blocker");
            blocker.client().batch_execute("BEGIN").await.expect("blocker transaction");
            let deadline: chrono::DateTime<chrono::Utc> = blocker.client().query_one("SELECT expires_at FROM identity.checks WHERE id=$1 FOR UPDATE", &[&id]).await.expect("locked check").get(0);
            let pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker identity").get(0);
            let worker = service.clone();
            let task = tokio::spawn(async move { worker.create_session(input(Uuid::new_v4(), id, "13800000000", "000000")).await });
            let observer = database.connections(&database.user).recover().await.expect("observer");
            timeout(Duration::from_secs(5), async {
                loop {
                    let waiting: bool = observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE $1=ANY(pg_catalog.pg_blocking_pids(pid)))", &[&pid]).await.expect("check waiter").get(0);
                    if waiting { break; } tokio::task::yield_now().await;
                }
                assert!(service.0.context.sample().await.expect("sample while check is locked") < deadline);
                while service.0.context.sample().await.expect("fresh trusted sample") < deadline { tokio::task::yield_now().await; }
            }).await.expect("wait reaches original expiry");
            assert!(!task.is_finished());
            blocker.client().batch_execute("COMMIT").await.expect("release check after expiry");
            assert!(matches!(timeout(Duration::from_secs(5), task).await.expect("bounded").expect("worker"), Err(Error::Verification)));
            assert_eq!(count(&database, "SELECT count(*) FROM identity.authentications").await, 0);
            assert_eq!(count(&database, "SELECT count(*) FROM identity.checks WHERE failures=0 AND authentication_id IS NULL").await, 1);
            blocker.terminate().await.expect("blocker ended"); observer.terminate().await.expect("observer ended");
            service.drain().await.expect("drained"); context.stop().await.expect("stopped"); supplier.stop().await;
        });
    }

    #[test]
    fn startup_scan_rejects_incompatible_facts_and_unsupported_envelopes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for (registered, unsupported_version) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let database = Database::new(Arc::clone(&server)).await;
                account(
                    &database,
                    if registered {
                        "13800000000"
                    } else {
                        "13900000000"
                    },
                    false,
                )
                .await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let id = message(&database, &service, &mut supplier, "13800000000").await;
                let operation_id = Uuid::new_v4();
                service
                    .create_session(input(operation_id, id, "13800000000", CODE))
                    .await
                    .expect("original result");
                if registered && !unsupported_version {
                    age(&database, &service, TimeDelta::minutes(11)).await;
                    service
                        .retention()
                        .run()
                        .await
                        .expect("expired handoff removed");
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.handoffs").await,
                        0
                    );
                    assert_eq!(
                        count(&database, "SELECT count(*) FROM identity.sessions").await,
                        1
                    );
                }
                let owner = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("isolated corruption fixture");
                owner
                    .client()
                    .batch_execute("BEGIN; SET LOCAL session_replication_role = replica")
                    .await
                    .expect("fixture transaction");
                owner
                    .client()
                    .batch_execute(match (registered, unsupported_version) {
                        (true, true) => {
                            "UPDATE identity.handoffs SET token=set_byte(token::bytea,0,2)"
                        }
                        (false, true) => {
                            "UPDATE identity.registrations SET token=set_byte(token::bytea,0,2)"
                        }
                        (true, false) => "UPDATE identity.sessions SET app_id='another-website'",
                        (false, false) => {
                            "UPDATE identity.authentications SET status='authenticated'"
                        }
                    })
                    .await
                    .expect("corrupt retained source, conclusion or envelope version");
                owner
                    .client()
                    .batch_execute("COMMIT")
                    .await
                    .expect("fixture committed");
                owner.terminate().await.expect("fixture ended");
                let reader = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("runtime scanner");
                reader
                    .client()
                    .batch_execute("BEGIN")
                    .await
                    .expect("scan transaction");
                let now = service.0.context.sample().await.expect("trusted time");
                let error = super::verify_all(
                    reader.client(),
                    &service.0.app,
                    &service.0.keys,
                    service.0.settings.session().app_id(),
                    now,
                )
                .await
                .expect_err("invalid retained fact must fail startup");
                if unsupported_version {
                    let Error::Query(query) = error else {
                        panic!("unsupported envelope must fail SQL decoding: {error:?}");
                    };
                    let cause = std::error::Error::source(&query.0)
                        .expect("column decoding retains the envelope error");
                    assert!(matches!(
                        cause.downcast_ref::<platform::cryptography::envelope::ParseError>(),
                        Some(platform::cryptography::envelope::ParseError::Version)
                    ));
                    let query = reader
                        .client()
                        .query_one("SELECT 1/0", &[])
                        .await
                        .expect_err("ordinary SQL error");
                    assert_eq!(
                        query.code(),
                        Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)
                    );
                    assert!(!Error::from(query).unreliable());
                } else {
                    eprintln!("session startup corruption: registered={registered}, unsupported_version={unsupported_version}, error={error}");
                    if registered {
                        assert!(matches!(error, Error::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))), "protected session alteration retains its seal error: {error}");
                    } else {
                        assert!(matches!(error, Error::Facts { .. }), "unmigrated authentication inconsistency retains its fact error: {error}");
                    }
                }
                reader
                    .client()
                    .batch_execute("ROLLBACK")
                    .await
                    .expect("scan ended");
                reader.terminate().await.expect("reader ended");
                if unsupported_version {
                    let error = match service
                        .create_session(input(operation_id, id, "13800000000", CODE))
                        .await
                    {
                        Ok(_) => panic!("corrupted credential cannot be handed off"),
                        Err(error) => error,
                    };
                    assert!(error.unreliable());
                    assert!(matches!(error, Error::Unknown { .. }));
                    assert!(matches!(service.ready().await, Err(Error::Closed)));
                }
                service.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            }
        });
    }

    #[test]
    fn only_a_committed_new_permit_supersedes_the_old_code() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for committed in [false, true] {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, service) = database.start(supplier.client()).await;
                let old = message(&database, &service, &mut supplier, "13800000000").await;
                permit(&database, &service, &mut supplier, "13800000000", committed).await;
                let result = service
                    .create_session(input(Uuid::new_v4(), old, "13800000000", CODE))
                    .await;
                if committed {
                    assert!(matches!(result, Err(Error::Verification)));
                } else {
                    assert!(matches!(result, Ok(Receipt::Registration(_))));
                }
                assert_eq!(
                    count(
                        &database,
                        "SELECT sum(failures)::bigint FROM identity.checks"
                    )
                    .await,
                    0
                );
                service.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                supplier.stop().await;
            }
        });
    }

    #[test]
    fn command_encoding_matches_independent_python_hmac() {
        let input = Input::new(
            "00000000-0000-0000-0000-000000000001"
                .parse()
                .expect("operation"),
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"
                .parse()
                .expect("secret"),
            "00000000-0000-0000-0000-000000000002"
                .parse()
                .expect("message"),
            "13800138000".parse().expect("mobile"),
            "001234".parse().expect("code"),
        );
        let encoded = input
            .encoded(&"website-fixture".parse().expect("app"))
            .expect("encoding");
        assert_eq!(encoded.as_ref().len(), 175);
        let key = hmac::Key::new(Zeroizing::new(std::array::from_fn(|index| {
            u8::try_from(index).expect("byte")
        })));
        assert_eq!(
            hex::encode(key.sign(&encoded).as_ref()),
            "428bcf528b98c80691a502b9806f85ecdf8d0862bef8b7ee20360fa0d7d51f4c"
        );
    }
}
