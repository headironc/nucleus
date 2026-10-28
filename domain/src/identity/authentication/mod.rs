//! C1 verification, code sending, and original-input recovery.

mod database;
pub mod dispatch;
mod error;
mod quota;
mod retention;
pub mod session;
pub mod settings;
mod transaction;
mod verification;

use std::{future::Future, net::IpAddr, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use tokio_postgres::{
    Client,
    types::{FromSql, ToSql},
};
use uuid::Uuid;

use crate::{
    access::role,
    identity::{
        challenge::{self, Token, client},
        current, keys, message,
        mobile::Mobile,
        operation, transfer, website,
    },
    kernel::App,
    runtime::{Context, registry, registry::Registry, work::Work},
};
pub use crate::{
    identity::authentication::{
        error::{Error, Failure, Stage},
        retention::Retention,
        settings::Settings,
        verification::Status,
    },
    runtime::work::Task,
};

const TRANSACTION_BUDGET: Duration = Duration::from_secs(10);

pub(crate) async fn verify(
    client: &Client,
    app: &App,
    keys: &keys::Keys,
    app_id: &website::AppId,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    database::challenge::verify_all(client, keys, now).await?;
    database::message::verify_all(client, &keys.message, now).await?;
    session::verify_all(client, app, keys, app_id, now).await
}

#[cfg(test)]
pub(crate) use self::{
    dispatch::tests::{proxied, unused_client},
    verification::tests::{
        Database, Supplier, avatar_settings, inventory, keys, server, session_settings,
    },
};

/// The identity of an atomic R1 conclusion, distinct from its request operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromSql, ToSql)]
#[postgres(transparent)]
pub(crate) struct Id(Uuid);

impl Id {
    fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

pub struct Input {
    operation_id: operation::Id,
    mobile: Mobile,
    token: Token,
    source: IpAddr,
    sender: Sender,
}

enum Sender {
    Anonymous,
    Current(current::Actor),
    Transfer {
        actor: current::Actor,
        transfer_id: transfer::Id,
    },
}

impl Input {
    async fn lock_scope(&self, client: &Client) -> Result<(), Error> {
        if let Sender::Transfer { transfer_id, .. } = self.sender {
            transfer::lock_scope(client, transfer_id).await?;
        }
        Ok(())
    }
    pub fn new(operation_id: operation::Id, mobile: Mobile, token: Token, source: IpAddr) -> Self {
        Self {
            operation_id,
            mobile,
            token,
            source: source.to_canonical(),
            sender: Sender::Anonymous,
        }
    }

    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }

    fn purpose(&self) -> challenge::Purpose {
        match self.sender {
            Sender::Anonymous => challenge::Purpose::Authentication,
            Sender::Current(_) => challenge::Purpose::MobileChange,
            Sender::Transfer { .. } => challenge::Purpose::MobileBinding,
        }
    }
}

trait Command: Send + Sync + 'static {
    fn operation_id(&self) -> operation::Id;
    fn family(&self) -> registry::Family {
        registry::Family::Challenge
    }
}

impl Command for Input {
    fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

trait Execute<I = Input>: Send + 'static {
    type Output: Send + 'static;

    fn execute(
        self,
        service: &Inner,
        input: &I,
    ) -> impl Future<Output = Result<Self::Output, Error>> + Send;
}

/// The original Challenge facts, returned only after independently checking its input.
///
/// Callers obtain the receipt from [`Service::verify`] and inspect its original facts:
///
/// ```no_run
/// use chrono::{DateTime, Utc};
/// use domain::identity::{authentication::{Error, Input, Service, Status}, challenge};
///
/// async fn inspect(service: &Service, input: Input) -> Result<(), Error> {
///     let receipt = service.verify(input).await?;
///     let _: challenge::Id = receipt.id();
///     let _: DateTime<Utc> = receipt.created_at();
///     let _: DateTime<Utc> = receipt.expires_at();
///     let _: Status = receipt.status();
///     Ok(())
/// }
/// ```
///
/// Raw facts do not establish that the original input was checked:
///
/// ```compile_fail
/// use chrono::{DateTime, Utc};
/// use domain::identity::{authentication::{Receipt, Status}, challenge};
///
/// fn fabricate(id: challenge::Id, at: DateTime<Utc>) -> Receipt {
///     Receipt {
///         id,
///         created_at: at,
///         expires_at: at,
///         calls: 1,
///         status: Status::Passed { solved_at: at, finished_at: at },
///     }
/// }
/// ```
///
/// Callers cannot rewrite a receipt's identity, times, call count or conclusion:
///
/// ```compile_fail
/// use domain::identity::{authentication::Receipt, challenge};
/// fn replace(receipt: &mut Receipt, id: challenge::Id) {
///     receipt.id = id;
/// }
/// ```
///
/// ```compile_fail
/// use chrono::{DateTime, Utc};
/// use domain::identity::authentication::Receipt;
/// fn replace(receipt: &mut Receipt, at: DateTime<Utc>) {
///     receipt.created_at = at;
/// }
/// ```
///
/// ```compile_fail
/// use chrono::{DateTime, Utc};
/// use domain::identity::authentication::Receipt;
/// fn replace(receipt: &mut Receipt, at: DateTime<Utc>) {
///     receipt.expires_at = at;
/// }
/// ```
///
/// ```compile_fail
/// use domain::identity::authentication::Receipt;
/// fn replace(receipt: &mut Receipt) {
///     receipt.calls = 1;
/// }
/// ```
///
/// ```compile_fail
/// use domain::identity::authentication::{Receipt, Status};
/// fn replace(receipt: &mut Receipt, status: Status) {
///     receipt.status = status;
/// }
/// ```
///
/// Neither a default value nor deserialization can replace the input checks:
///
/// ```compile_fail
/// use domain::identity::authentication::Receipt;
/// let _: Receipt = Default::default();
/// ```
///
/// ```compile_fail
/// use domain::identity::authentication::Receipt;
/// let _ = serde_json::from_str::<Receipt>("{}");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    id: challenge::Id,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    calls: i16,
    status: Status,
}

impl Receipt {
    pub fn status(self) -> Status {
        self.status
    }
    pub fn id(self) -> challenge::Id {
        self.id
    }
    pub fn created_at(self) -> DateTime<Utc> {
        self.created_at
    }
    pub fn expires_at(self) -> DateTime<Utc> {
        self.expires_at
    }
}

pub use crate::identity::keys::Keys;

/// Clones share the C1/R1 execution registry, admission state and supplier capacity.
#[derive(Clone)]
pub struct Service(Arc<Inner>);

pub(crate) struct Suppliers {
    pub(crate) challenge: client::Client,
    pub(crate) message: message::client::Client,
}

impl Service {
    /// Create the process-owned cursors for bounded C1/R1 retention rounds.
    pub fn retention(&self) -> Retention {
        Retention::new(self.clone())
    }

    #[cfg(test)]
    async fn start(
        barrier: crate::database::Barrier,
        keys: Keys,
        settings: crate::startup::Settings,
        supplier: client::Client,
        message: message::client::Client,
    ) -> Result<Self, crate::startup::Error> {
        Ok(
            crate::startup::Services::start(barrier, inventory(keys), settings, supplier, message)
                .await?
                .authentication()
                .clone(),
        )
    }

    pub(crate) fn prepare(
        app: App,
        keys: Arc<Keys>,
        settings: Settings,
        current: current::Service,
        suppliers: Suppliers,
    ) -> Self {
        Self(Arc::new(Inner {
            context: app.context().clone(),
            keys,
            registry: Arc::clone(app.registry()),
            settings,
            work: Arc::clone(app.work()),
            current,
            roles: role::Reader::new(app.clone()),
            app,
            supplier: suppliers.challenge,
            dispatch: dispatch::Dispatch::new(suppliers.message),
        }))
    }

    /// Cancelling this waiter never cancels already accepted database work.
    pub async fn verify(&self, input: Input) -> Result<Receipt, Error> {
        self.0.run(input, verification::Action).await
    }

    /// Verify and send within one supervised operation. Retained messages are never resent.
    pub async fn send(&self, input: Input) -> Result<dispatch::Receipt, Error> {
        self.0.run(input, dispatch::Action).await
    }

    /// Send to the acting website account's current phone, using the shared C1 budgets.
    pub async fn send_current(
        &self,
        actor: current::Actor,
        mut input: Input,
    ) -> Result<dispatch::Receipt, Error> {
        input.sender = Sender::Current(actor);
        self.0.run(input, dispatch::Action).await
    }

    /// Send a new-phone code for the transfer's original website session.
    pub async fn send_binding(
        &self,
        actor: current::Actor,
        transfer_id: transfer::Id,
        mut input: Input,
    ) -> Result<dispatch::Receipt, Error> {
        input.sender = Sender::Transfer { actor, transfer_id };
        self.0.run(input, dispatch::Action).await
    }

    /// Authenticate an SMS code and deliver only its original, currently valid credential.
    pub async fn create_session(&self, input: session::Input) -> Result<session::Receipt, Error> {
        self.0.run(input, session::Action).await
    }

    /// Stop accepting new C1 work; accepted workers remain owned until drain.
    #[cfg(test)]
    pub fn close(&self) {
        self.0.registry.close();
    }

    /// Observe transaction time, then check admission.
    #[cfg(test)]
    pub async fn ready(&self) -> Result<(), Error> {
        self.0.context.sample().await?;
        self.0.work.ready(&self.0.registry).await?;
        Ok(())
    }

    /// Compute Retry-After using trusted time, never the HTTP host's wall clock.
    #[cfg(test)]
    pub async fn retry_after(&self, retry_at: DateTime<Utc>) -> Result<Duration, Error> {
        let now = self.0.context.sample().await?;
        Ok((retry_at - now).to_std().unwrap_or(Duration::ZERO))
    }

    /// Close admission and drain request tasks; the application also awaits retention.
    #[cfg(test)]
    pub async fn drain(&self) -> Result<(), Error> {
        self.0.work.drain(&self.0.registry).await?;
        Ok(())
    }
}

struct Inner {
    app: App,
    context: Context,
    keys: Arc<Keys>,
    settings: Settings,
    registry: Arc<Registry>,
    work: Arc<Work>,
    current: current::Service,
    roles: role::Reader,
    supplier: client::Client,
    dispatch: dispatch::Dispatch,
}

impl Inner {
    async fn authenticate_sender(&self, client: &Client, input: &Input) -> Result<(), Error> {
        match &input.sender {
            Sender::Anonymous => {}
            Sender::Current(actor) => {
                self.contact(client, actor, &input.mobile).await?;
            }
            Sender::Transfer { actor, transfer_id } => {
                transfer::authenticate_sender(
                    client,
                    &self.roles,
                    &self.current,
                    &self.keys,
                    actor,
                    *transfer_id,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn contact(
        &self,
        client: &Client,
        actor: &current::Actor,
        mobile: &Mobile,
    ) -> Result<current::Contact, Error> {
        actor.lock_account(client).await?;
        let contact = self.current.fetch_contact(client, actor).await?;
        if contact.mobile().as_bytes() != mobile.as_bytes() {
            return Err(Error::TargetChanged);
        }
        Ok(contact)
    }

    async fn transfer_contact(
        &self,
        client: &Client,
        actor: &current::Actor,
        transfer_id: transfer::Id,
        mobile: &Mobile,
    ) -> Result<transfer::Target, Error> {
        Ok(transfer::qualify_sender(
            client,
            &self.roles,
            &self.current,
            &self.keys,
            actor,
            transfer_id,
            mobile,
        )
        .await?)
    }

    async fn run<I: Command, A: Execute<I>>(
        self: &Arc<Self>,
        input: I,
        action: A,
    ) -> Result<A::Output, Error> {
        let service = Arc::clone(self);
        self.work
            .run(
                Arc::clone(&self.registry),
                input.family(),
                input.operation_id(),
                async move { action.execute(&service, &input).await },
            )
            .await
    }

    async fn now(&self, client: &tokio_postgres::Client) -> Result<DateTime<Utc>, Error> {
        Ok(self.context.now(client).await?)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use tokio::{runtime::Runtime, sync::oneshot, time::timeout};
    use tracing::Instrument;
    use tracing_test::traced_test;

    use crate::identity::authentication::{
        Error, Execute, Inner, Input,
        verification::tests::{Database, Supplier, input, server},
    };

    // Doctests compile outside domain and cannot detect pub(crate) or pub(super).
    fn assert_receipt_visibility(source: &str) {
        let file = syn::parse_file(source).expect("valid receipt declaration module");
        let mut declarations = 0;
        for item in file.items {
            match item {
                syn::Item::Struct(receipt) if receipt.ident == "Receipt" => {
                    declarations += 1;
                    for field in &receipt.fields {
                        assert!(
                            matches!(field.vis, syn::Visibility::Inherited),
                            "receipt field {} must stay private to its owner",
                            field.ident.as_ref().expect("named receipt field"),
                        );
                    }
                }
                syn::Item::Impl(implementation) => {
                    let syn::Type::Path(path) = &*implementation.self_ty else {
                        continue;
                    };
                    if !path
                        .path
                        .segments
                        .last()
                        .is_some_and(|part| part.ident == "Receipt")
                    {
                        continue;
                    }
                    assert!(
                        implementation.trait_.is_none(),
                        "receipt trait implementations need explicit boundary checks",
                    );
                    for item in implementation.items {
                        if let syn::ImplItem::Fn(method) = item {
                            if matches!(method.vis, syn::Visibility::Inherited) {
                                continue;
                            }
                            let reads_field = match method.block.stmts.as_slice() {
                                [syn::Stmt::Expr(syn::Expr::Field(field), None)] => {
                                    matches!(&*field.base, syn::Expr::Path(base) if base.path.is_ident("self"))
                                }
                                _ => false,
                            };
                            assert!(
                                method.sig.inputs.len() == 1
                                    && method.sig.receiver().is_some_and(|receiver| {
                                        receiver.mutability.is_none()
                                            && matches!(
                                                &receiver.kind,
                                                syn::ReceiverKind::Value
                                                    | syn::ReceiverKind::Reference(_, _, None)
                                            )
                                    })
                                    && reads_field,
                                "receipt method {} must be a read-only field accessor or stay private",
                                method.sig.ident,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        assert_eq!(declarations, 1, "check the module that declares Receipt");
    }

    #[test]
    fn challenge_receipt_construction_stays_private() {
        assert_receipt_visibility(include_str!("mod.rs"));
    }

    #[test]
    fn message_receipt_construction_stays_private() {
        assert_receipt_visibility(include_str!("dispatch/mod.rs"));
    }

    #[test]
    fn receipt_guard_accepts_private_construction_and_read_only_accessors() {
        assert_receipt_visibility(
            r#"
            #[derive(Clone, Copy)]
            pub struct Receipt { value: u8 }
            impl Receipt {
                fn new(value: u8) -> Self { Self { value } }
                pub fn value(self) -> u8 { self.value }
                pub(crate) fn borrowed(&self) -> u8 { self.value }
            }

            pub struct Input { pub value: u8 }
            impl Input {
                pub fn new(value: u8) -> Self { Self { value } }
            }
            impl From<u8> for Input {
                fn from(value: u8) -> Self { Self { value } }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "receipt field value must stay private to its owner")]
    fn receipt_guard_rejects_public_fields() {
        assert_receipt_visibility(r#"pub struct Receipt { pub value: u8 }"#);
    }

    #[test]
    #[should_panic(expected = "receipt field value must stay private to its owner")]
    fn receipt_guard_rejects_crate_visible_fields() {
        assert_receipt_visibility(r#"pub struct Receipt { pub(crate) value: u8 }"#);
    }

    #[test]
    #[should_panic(expected = "receipt field value must stay private to its owner")]
    fn receipt_guard_rejects_parent_visible_fields() {
        assert_receipt_visibility(r#"pub struct Receipt { pub(super) value: u8 }"#);
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_exposed_constructors() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub(crate) fn new(value: u8) -> Self { Self { value } }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_setters() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn set(&mut self, value: u8) { self.value = value; }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_modified_receipts() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn with(self, value: u8) -> Self { Self { value, ..self } }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_mutable_reference_accessors() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn value_mut(&mut self) -> &mut u8 { &mut self.value }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_mutable_receivers_even_for_direct_reads() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn value(&mut self) -> u8 { self.value }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_typed_receivers_even_for_direct_reads() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn value(self: &Self) -> u8 { self.value }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_mutable_owned_receivers_even_for_direct_reads() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn value(mut self) -> u8 { self.value }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_additional_accessor_inputs() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn value(self, _other: u8) -> u8 { self.value }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_replaced_field_values() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl Receipt {
                pub fn value(self) -> u8 { 0 }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "must be a read-only field accessor or stay private")]
    fn receipt_guard_rejects_another_receipts_field() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            const OTHER: Receipt = Receipt { value: 0 };
            impl Receipt {
                pub fn value(self) -> u8 { OTHER.value }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "receipt trait implementations need explicit boundary checks")]
    fn receipt_guard_rejects_raw_trait_conversions() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            impl From<u8> for Receipt {
                fn from(value: u8) -> Self { Self { value } }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "receipt trait implementations need explicit boundary checks")]
    fn receipt_guard_rejects_mutation_traits() {
        assert_receipt_visibility(
            r#"
            pub struct Receipt { value: u8 }
            pub trait Change { fn set(&mut self, value: u8); }
            impl Change for Receipt {
                fn set(&mut self, value: u8) { self.value = value; }
            }
            "#,
        );
    }

    #[test]
    #[should_panic(expected = "valid receipt declaration module")]
    fn receipt_guard_rejects_unparseable_source() {
        assert_receipt_visibility(r#"pub struct Receipt {"#);
    }

    #[test]
    #[should_panic(expected = "check the module that declares Receipt")]
    fn receipt_guard_rejects_missing_declarations() {
        assert_receipt_visibility(r#"pub struct Input { value: u8 }"#);
    }

    #[traced_test]
    #[test]
    fn cancelled_waiter_keeps_the_panicking_workers_safe_diagnostic() {
        struct Action {
            started: oneshot::Sender<()>,
            released: oneshot::Receiver<()>,
        }
        impl Execute for Action {
            type Output = ();

            async fn execute(self, _: &Inner, _: &Input) -> Result<(), Error> {
                self.started.send(()).expect("worker started");
                self.released.await.expect("release worker");
                panic!("fixture action failed");
            }
        }
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            let database = Database::initialized(server).await;
            let supplier = Supplier::new().await;
            let (context, service) = database.start(supplier.client()).await;
            let (started, running) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let worker = service.clone();
            let waiter = tokio::spawn(
                async move { worker.0.run(input(), Action { started, released }).await }
                    .in_current_span(),
            );
            timeout(Duration::from_secs(1), running)
                .await
                .expect("start budget")
                .expect("registered worker");
            waiter.abort();
            assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
            release.send(()).expect("retained worker");
            let error = timeout(Duration::from_secs(1), service.drain())
                .await
                .expect("drain budget")
                .expect_err("panic retained");
            assert!(matches!(error, Error::Drain(ref failures)
                if failures.len() == 1 && failures[0].is_panic() && !failures[0].is_cancelled()));
            assert!(matches!(service.verify(input()).await, Err(Error::Closed)));
            context.stop().await.expect("runtime stopped");
            supplier.stop().await;
        });
        logs_assert(|lines| {
            let events: Vec<_> = lines
                .iter()
                .filter(|line| line.contains(r#"stage="authentication_completion""#))
                .collect();
            assert_eq!(events.len(), 1);
            assert!(events[0].contains(&format!("operation_id={}", input().operation_id())));
            assert!(events[0].contains("error=Worker"));
            assert!(events[0].contains(r#"kind="unavailable""#));
            Ok(())
        });
        for secret in [
            "13800000000",
            "fictional-token",
            "fictional-secret",
            fixture::PASSWORD,
        ] {
            assert!(!logs_contain(secret));
        }
    }
}
