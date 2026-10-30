//! Current website identity and one decisive self-account projection.

pub(crate) mod database;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::time::{Instant, timeout_at};

use crate::{
    history::{self, Read, View},
    identity::{
        account, avatar,
        keys::Keys,
        mobile::{Mobile, envelope},
        person::{self, name::Name, revision},
        session,
        website::AppId,
    },
    kernel::App,
    runtime::{self, Context, registry::Registry},
};

/// Created only by the current proof check, never by deserializing a request or SQL row.
/// Consumers may clone an authenticated actor and read its session selection.
///
/// ```
/// use domain::identity::{current::Actor, session};
/// fn selected(actor: &Actor) -> session::Id {
///     actor.clone().session_id()
/// }
/// ```
///
/// Knowing the stored identifiers does not qualify a caller to construct an actor.
///
/// ```compile_fail
/// use domain::identity::{account, current::Actor, session, website::AppId};
/// fn forge(account_id: account::Id, session_id: session::Id, app_id: AppId) -> Actor {
///     Actor { account_id, session_id, app_id }
/// }
/// ```
///
/// Each part of the original identity remains immutable to consumers.
///
/// ```compile_fail
/// use domain::identity::{account, current::Actor};
/// fn replace(actor: &mut Actor, value: account::Id) {
///     actor.account_id = value;
/// }
/// ```
///
/// ```compile_fail
/// use domain::identity::{current::Actor, session};
/// fn replace(actor: &mut Actor, value: session::Id) {
///     actor.session_id = value;
/// }
/// ```
///
/// ```compile_fail
/// use domain::identity::{current::Actor, website::AppId};
/// fn replace(actor: &mut Actor, value: AppId) {
///     actor.app_id = value;
/// }
/// ```
///
/// Neither defaults nor deserialization establish authentication.
///
/// ```compile_fail
/// use domain::identity::current::Actor;
/// let actor = Actor::default();
/// ```
///
/// ```compile_fail
/// use domain::identity::current::Actor;
/// let actor = serde_json::from_str::<Actor>("{}");
/// ```
#[derive(Clone)]
pub struct Actor {
    account_id: account::Id,
    session_id: session::Id,
    app_id: AppId,
}

impl Actor {
    pub fn session_id(&self) -> session::Id {
        self.session_id
    }

    pub(crate) fn account_id(&self) -> account::Id {
        self.account_id
    }

    /// Access writes stabilize this actor's account before their decisive qualification query.
    pub(crate) async fn lock_account(&self, client: &tokio_postgres::Client) -> Result<(), Error> {
        database::lock_account(client, self.account_id).await
    }

    /// Session locks follow the command's account/person locks and precede Grant locks.
    pub(crate) async fn lock_session(&self, client: &tokio_postgres::Client) -> Result<(), Error> {
        Ok(session::state::lock_shared(client, self.session_id).await?)
    }

    /// Stabilize the acting and managed accounts in the same UUID order.
    pub(crate) async fn lock_accounts(
        &self,
        client: &tokio_postgres::Client,
        target: account::Id,
    ) -> Result<(), Error> {
        database::lock_accounts(client, self.account_id, target).await
    }

    /// Stabilize another acting account and this managed account in the same UUID order.
    pub(crate) async fn lock_as_target(
        &self,
        client: &tokio_postgres::Client,
        operator: account::Id,
    ) -> Result<(), Error> {
        database::lock_accounts(client, operator, self.account_id).await
    }
}

/// One qualified current phone and identity, read from the same decisive snapshot.
pub(in crate::identity) struct Contact {
    person_id: person::Id,
    revision_id: revision::Id,
    mobile: Mobile,
    observed_at: DateTime<Utc>,
}

impl Contact {
    pub(in crate::identity) fn person_id(&self) -> person::Id {
        self.person_id
    }

    pub(in crate::identity) fn revision_id(&self) -> revision::Id {
        self.revision_id
    }

    pub(in crate::identity) fn mobile(&self) -> &Mobile {
        &self.mobile
    }

    pub(in crate::identity) fn observed_at(&self) -> DateTime<Utc> {
        self.observed_at
    }

    pub(in crate::identity) fn into_mobile(self) -> Mobile {
        self.mobile
    }
}

/// Unverified proof for the website session selected by this request.
pub struct Proof {
    pub id: session::Id,
    pub token: session::Token,
}

pub struct Account {
    id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
    name: Name,
    suffix: String,
    avatar: avatar::Current,
}

impl Account {
    pub fn id(&self) -> account::Id {
        self.id
    }
    pub fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub fn revision_id(&self) -> revision::Id {
        self.revision_id
    }
    pub fn name(&self) -> &Name {
        &self.name
    }
    pub fn suffix(&self) -> &str {
        &self.suffix
    }
    pub fn avatar(&self) -> &avatar::Current {
        &self.avatar
    }
}

#[derive(Clone)]
pub struct Service(Arc<Inner>, Option<View>);

struct Inner {
    app: App,
    context: Context,
    keys: Arc<Keys>,
    app_id: AppId,
    avatar: avatar::Service,
    registry: Arc<Registry>,
}

impl Service {
    pub(crate) fn at(&self, view: View) -> Self {
        Self(Arc::clone(&self.0), Some(view))
    }

    fn read_mode(&self) -> Result<Read, Error> {
        Ok(Read::Snapshot(match self.1 {
            Some(view) => view,
            None => self.0.app.anchors().view()?,
        }))
    }

    pub(crate) async fn now(
        &self,
        client: &tokio_postgres::Client,
    ) -> Result<DateTime<Utc>, Error> {
        Ok(self.0.context.now(client).await?)
    }

    fn confirm(&self, actor: &Actor, state: &database::State) -> Result<DateTime<Utc>, Error> {
        if let Err(error) = state.validate(&self.0.app_id) {
            if error.unreliable() {
                self.0.registry.close();
            }
            return Err(error);
        }
        if state.session.id() != actor.session_id
            || state.session.account_id() != actor.account_id
            || state.session.app_id() != &actor.app_id
        {
            self.0.registry.close();
            return Err(Error::Facts {
                rule: "actor retains the original session account and application",
            });
        }
        Ok(state.observed_at)
    }

    /// The executor has validated this time after taking the command's object locks.
    /// Shared row locks preserve authorization without upgrading an already shared Account.
    pub(crate) async fn resolve(
        &self,
        client: &tokio_postgres::Client,
        actor: &Actor,
        now: DateTime<Utc>,
    ) -> Result<database::State, Error> {
        let state = database::find_at(client, &self.0.app, Read::Shared, actor.session_id, now)
            .await?
            .ok_or(Error::Invalid)?;
        self.confirm(actor, &state)?;
        database::profile(client, &state, &self.0.keys, self.0.app.keys()).await?;
        Ok(state)
    }

    pub(crate) fn prepare(
        app: App,
        keys: Arc<Keys>,
        app_id: AppId,
        avatar: avatar::Service,
    ) -> Self {
        Self(
            Arc::new(Inner {
                context: app.context().clone(),
                registry: Arc::clone(app.registry()),
                app,
                keys,
                app_id,
                avatar,
            }),
            None,
        )
    }

    /// Confirm the original session, person and current revision within the caller's snapshot.
    pub(crate) async fn fetch_person(
        &self,
        client: &tokio_postgres::Client,
        actor: &Actor,
    ) -> Result<(person::Id, revision::Id, DateTime<Utc>), Error> {
        let state = self.fetch_state(client, actor, self.read_mode()?).await?;
        Ok((state.person_id, state.revision_id, state.observed_at))
    }

    pub(crate) async fn fetch_state(
        &self,
        client: &tokio_postgres::Client,
        actor: &Actor,
        read: Read,
    ) -> Result<database::State, Error> {
        let state = database::find_person_by_actor(client, &self.0.app, read, actor)
            .await?
            .ok_or(Error::Invalid)?;
        self.confirm(actor, &state)?;
        database::profile(client, &state, &self.0.keys, self.0.app.keys()).await?;
        Ok(state)
    }

    /// The write owner locks the account before using this current phone.
    pub(in crate::identity) async fn fetch_contact(
        &self,
        client: &tokio_postgres::Client,
        actor: &Actor,
    ) -> Result<Contact, Error> {
        let state = self.fetch_state(client, actor, self.read_mode()?).await?;
        let observed_at = state.observed_at;
        let current = state.account.current().ok_or(Error::Invalid)?;
        let mobile = current.mobile().open(
            &self.0.keys.encryption,
            &self.0.keys.mobile,
            envelope::Binding::Account(actor.account_id),
        )?;
        Ok(Contact {
            person_id: state.person_id,
            revision_id: state.revision_id,
            mobile,
            observed_at,
        })
    }

    pub async fn authenticate(&self, proof: Proof) -> Result<Actor, Error> {
        self.0.registry.ready()?;
        let lease = self.0.context.connections().lease().await?;
        let budget = self.0.context.read_budget();
        let deadline = budget.deadline();
        let client = lease.client();
        let proof = &proof;
        let result = timeout_at(
            deadline,
            budget.run(client, self.0.app.anchors(), |view| async move {
                let state = database::find_session_by_id(
                    client,
                    &self.0.app,
                    Read::Snapshot(view),
                    proof.id,
                )
                .await?;
                let state = state.ok_or(Error::Invalid)?;
                state.validate(&self.0.app_id)?;
                if state.session.id() != proof.id || !proof.token.0.matches(state.session.hash()) {
                    return Err(Error::Invalid);
                }
                database::profile(client, &state, &self.0.keys, self.0.app.keys()).await?;
                Ok(Actor {
                    account_id: state.session.account_id(),
                    session_id: state.session.id(),
                    app_id: state.session.app_id().clone(),
                })
            }),
        )
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn read(&self, actor: &Actor) -> Result<Account, Error> {
        self.0.registry.ready()?;
        let lease = self.0.context.connections().lease().await?;
        let budget = self.0.context.read_budget();
        let deadline = budget.deadline();
        let client = lease.client();
        let result = timeout_at(
            deadline,
            budget.run(client, self.0.app.anchors(), |view| async move {
                let state = database::find_person_by_actor(
                    client,
                    &self.0.app,
                    Read::Snapshot(view),
                    actor,
                )
                .await?;
                let state = state.ok_or(Error::Invalid)?;
                self.confirm(actor, &state)?;
                database::deliver(client, &state, &self.0.keys, &self.0.app, &self.0.avatar).await
            }),
        )
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    async fn finish<T>(
        &self,
        lease: platform::database::Lease,
        deadline: Instant,
        result: Result<T, Error>,
    ) -> Result<T, Error> {
        if result.as_ref().is_err_and(Error::unreliable) {
            self.0.registry.close();
        }
        match result {
            Ok(value) => match timeout_at(deadline, lease.client().batch_execute("COMMIT")).await {
                Ok(Ok(())) => {
                    lease.release();
                    Ok(value)
                }
                Ok(Err(error)) => Err(Error::from(error).combine(lease.discard().await)),
                Err(error) => Err(Error::from(error).combine(lease.discard().await)),
            },
            Err(Error::Invalid) => {
                match timeout_at(deadline, lease.client().batch_execute("ROLLBACK"))
                    .await
                    .map_err(Error::from)
                    .and_then(|result| result.map_err(Error::from))
                {
                    Ok(()) => lease.release(),
                    Err(failure) => {
                        let cleanup = lease.discard().await;
                        tracing::error!(stage = "current_identity_read", outcome = "invalid", error = ?failure, cleanup = ?cleanup);
                    }
                }
                Err(Error::Invalid)
            }
            Err(error) => Err(error.combine(lease.discard().await)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("current session authentication is invalid")]
    Invalid,
    #[error("{0}")]
    History(#[from] history::Error),
    #[error("current account fact check failed: {rule}")]
    Facts { rule: &'static str },
    #[error("current account query failed: {0}")]
    Query(#[from] crate::database::Query),
    #[error("current account decoding failed: {0}")]
    Decode(#[source] crate::database::Query),
    #[error("{0}")]
    Session(#[from] session::Error),
    #[error("{0}")]
    Revision(#[source] revision::Error),
    #[error("{0}")]
    Mobile(#[from] envelope::Error),
    #[error("{0}")]
    Avatar(#[from] avatar::Error),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Runtime(#[from] runtime::Error),
    #[error("{0}")]
    Work(#[from] runtime::work::Error),
    #[error("current account query budget expired")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("current account digest computation failed")]
    Crypto(#[from] platform::cryptography::Error),
    #[error("current account query and connection termination failed")]
    Combined {
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(error.into())
    }
}

impl From<revision::Error> for Error {
    fn from(error: revision::Error) -> Self {
        match error {
            revision::Error::History(crate::history::Error::Query(error)) => Self::Query(error),
            revision::Error::History(crate::history::Error::Decode(error)) => Self::Decode(error),
            error => Self::Revision(error),
        }
    }
}

impl Error {
    pub(crate) fn unreliable(&self) -> bool {
        match self {
            Self::Facts { .. }
            | Self::Decode(_)
            | Self::Mobile(_)
            | Self::Session(session::Error::Facts { .. } | session::Error::Decode(_)) => true,
            Self::Runtime(error) => error.unreliable(),
            Self::Avatar(error) => error.unreliable(),
            Self::History(error) => error.corrupt(),
            Self::Revision(error) => error.unreliable(),
            Self::Combined { cause, .. } => cause.unreliable(),
            _ => false,
        }
    }

    fn combine(self, ended: Result<(), platform::database::Error>) -> Self {
        match ended {
            Ok(()) => self,
            Err(termination) => Self::Combined {
                cause: Box::new(self),
                termination,
            },
        }
    }
}

impl crate::database::read::Failure for Error {
    fn changed(&self) -> bool {
        crate::database::read::changed(self)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use chrono::TimeDelta;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        database::Barrier,
        identity::{
            authentication::{
                self, Database, Supplier, keys, server,
                session::{
                    Receipt,
                    tests::{CODE, age, input, message},
                },
            },
            avatar::{self, link},
            current::{Error, Proof},
            session,
        },
        runtime::Context,
        startup::{Services, Settings},
    };

    // Rustdoc cannot observe crate-visible construction. This guard covers the declaration
    // and direct impls here, not factories, child modules, aliases or macro expansion.
    // Authentication and decisive rechecks retain their separate behavior tests below.
    fn check_actor_boundary(source: &str) -> Result<(), String> {
        let file = syn::parse_file(source).map_err(|_| "cannot parse Actor module")?;
        let mut declarations = 0;
        for item in file.items {
            match item {
                syn::Item::Struct(actor) if actor.ident == "Actor" => {
                    declarations += 1;
                    if actor
                        .fields
                        .iter()
                        .any(|field| !matches!(field.vis, syn::Visibility::Inherited))
                    {
                        return Err("Actor fields must stay private".into());
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
                        .is_some_and(|part| part.ident == "Actor")
                    {
                        continue;
                    }
                    if implementation.trait_.is_some() {
                        return Err(
                            "Actor trait implementations need explicit boundary coverage".into(),
                        );
                    }
                    for item in implementation.items {
                        if let syn::ImplItem::Const(constant) = &item
                            && !matches!(constant.vis, syn::Visibility::Inherited)
                        {
                            return Err("Actor associated constants must stay private".into());
                        }
                        let syn::ImplItem::Fn(method) = item else {
                            continue;
                        };
                        if matches!(method.vis, syn::Visibility::Inherited) {
                            continue;
                        }
                        let borrowed = method.sig.receiver().is_some_and(|receiver| {
                            receiver.mutability.is_none()
                                && matches!(
                                    &receiver.kind,
                                    syn::ReceiverKind::Reference(_, _, None)
                                )
                        });
                        let field = match method.block.stmts.as_slice() {
                            [syn::Stmt::Expr(expression, None)] => match expression {
                                syn::Expr::Reference(reference)
                                    if reference.mutability.is_none() =>
                                {
                                    Some(&*reference.expr)
                                }
                                expression => Some(expression),
                            },
                            _ => None,
                        };
                        let getter = method.sig.inputs.len() == 1
                            && matches!(field, Some(syn::Expr::Field(field))
                                if matches!(&*field.base, syn::Expr::Path(base) if base.path.is_ident("self")));
                        let mut clients = 0;
                        let mut accounts = 0;
                        let lock_inputs = method.sig.inputs.iter().skip(1).all(|input| {
                            let syn::FnArg::Typed(input) = input else {
                                return false;
                            };
                            match &*input.ty {
                                syn::Type::Reference(reference)
                                    if reference.mutability.is_none() =>
                                {
                                    let syn::Type::Path(path) = &*reference.elem else {
                                        return false;
                                    };
                                    let names = path
                                        .path
                                        .segments
                                        .iter()
                                        .map(|part| part.ident.to_string())
                                        .collect::<Vec<_>>();
                                    if names == ["tokio_postgres", "Client"] || names == ["Client"]
                                    {
                                        clients += 1;
                                        true
                                    } else {
                                        false
                                    }
                                }
                                syn::Type::Path(path) => {
                                    let names = path
                                        .path
                                        .segments
                                        .iter()
                                        .map(|part| part.ident.to_string())
                                        .collect::<Vec<_>>();
                                    if names == ["account", "Id"] {
                                        accounts += 1;
                                        true
                                    } else {
                                        false
                                    }
                                }
                                _ => false,
                            }
                        });
                        let unit_result = match &method.sig.output {
                            syn::ReturnType::Type(_, output) => match &**output {
                                syn::Type::Path(path) if path.path.segments.len() == 1 => {
                                    let segment = &path.path.segments[0];
                                    match &segment.arguments {
                                        syn::PathArguments::AngleBracketed(arguments)
                                            if segment.ident == "Result" =>
                                        {
                                            matches!(arguments.args.iter().collect::<Vec<_>>().as_slice(),
                                                [syn::GenericArgument::Type(syn::Type::Tuple(unit)), syn::GenericArgument::Type(syn::Type::Path(error))]
                                                if unit.elems.is_empty() && error.path.is_ident("Error"))
                                        }
                                        _ => false,
                                    }
                                }
                                _ => false,
                            },
                            _ => false,
                        };
                        let lock = method.sig.asyncness.is_some()
                            && lock_inputs
                            && clients == 1
                            && accounts <= 1
                            && unit_result;
                        if !borrowed || !(getter || lock) {
                            return Err(format!(
                                "Actor method {} must preserve the getter or lock boundary",
                                method.sig.ident
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        if declarations != 1 {
            return Err("expected exactly one Actor declaration".into());
        }
        Ok(())
    }

    #[test]
    fn actor_construction_stays_private() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/identity/current/mod.rs"
        ))
        .expect("read the Actor declaration module");
        check_actor_boundary(&source).expect("Actor retains its authenticated identity");
    }

    #[test]
    fn actor_guard_accepts_private_construction_getters_and_lock_signatures() {
        check_actor_boundary(r#"
            #[derive(Clone)]
            pub struct Actor { value: u8 }
            impl Actor {
                const INTERNAL: Self = Self { value: 1 };
                fn new(value: u8) -> Self { Self { value } }
                pub fn renamed(&self) -> u8 { self.value }
                pub(crate) fn borrowed(&self) -> &u8 { &self.value }
                pub(crate) async fn coordinate(&self, connection: &Client) -> Result<(), Error> {
                    arbitrary_lock_body().await
                }
                pub(crate) async fn another(&self, target: account::Id, connection: &tokio_postgres::Client) -> Result<(), Error> {
                    another_lock_body().await
                }
            }
            struct Other;
            impl From<u8> for Other { fn from(_: u8) -> Self { Self } }
        "#).expect("names, argument order and lock bodies do not define the boundary");
    }

    #[test]
    fn actor_guard_rejects_exposed_fields() {
        for visibility in ["pub", "pub(crate)", "pub(super)"] {
            let source = format!("pub struct Actor {{ {visibility} value: u8 }}");
            assert_eq!(
                check_actor_boundary(&source).expect_err("exposed identity"),
                "Actor fields must stay private"
            );
        }
    }

    #[test]
    fn actor_guard_rejects_construction_and_mutation_methods() {
        for method in [
            "pub fn new(value: u8) -> Self { Self { value } }",
            "pub(crate) fn new(value: u8) -> Self { Self { value } }",
            "pub fn set(&mut self, value: u8) { self.value = value; }",
            "pub fn with(&self, value: u8) -> Self { Self { value } }",
            "pub fn value_mut(&mut self) -> &mut u8 { &mut self.value }",
            "pub fn mutable(&mut self) -> u8 { self.value }",
            "pub fn typed(self: &Self) -> u8 { self.value }",
            "pub fn owned(self) -> u8 { self.value }",
            "pub fn fake(&self) -> u8 { 0 }",
            "pub fn extra(&self, value: u8) -> u8 { self.value }",
            "pub async fn lock(&mut self, client: &Client) -> Result<(), Error> { Ok(()) }",
            "pub async fn lock(&self, client: &mut Client) -> Result<(), Error> { Ok(()) }",
            "pub async fn lock(&self, client: &Client, target: session::Id) -> Result<(), Error> { Ok(()) }",
            "pub async fn lock(&self, client: &Client) -> Result<Self, Error> { todo!() }",
            "pub async fn lock(&self, client: &Client, second: &Client) -> Result<(), Error> { Ok(()) }",
        ] {
            let source = format!("pub struct Actor {{ value: u8 }} impl Actor {{ {method} }}");
            let error = check_actor_boundary(&source).expect_err("unsupported Actor API");
            assert!(
                error.contains("must preserve the getter or lock boundary"),
                "{method}: {error}"
            );
        }
    }

    #[test]
    fn actor_guard_rejects_handwritten_traits_and_invalid_declarations() {
        for implementation in [
            "impl From<u8> for Actor { fn from(value: u8) -> Self { Self { value } } }",
            "impl Change for Actor { fn set(&mut self, value: u8) { self.value = value; } }",
        ] {
            let source = format!("pub struct Actor {{ value: u8 }} {implementation}");
            assert_eq!(
                check_actor_boundary(&source).expect_err("new trait needs coverage"),
                "Actor trait implementations need explicit boundary coverage"
            );
        }
        assert_eq!(
            check_actor_boundary("struct Other;").expect_err("missing Actor"),
            "expected exactly one Actor declaration"
        );
        assert_eq!(
            check_actor_boundary("struct Actor; struct Actor;").expect_err("duplicate Actor"),
            "expected exactly one Actor declaration"
        );
        assert_eq!(
            check_actor_boundary("struct Actor {").expect_err("invalid syntax"),
            "cannot parse Actor module"
        );
    }

    #[test]
    fn actor_guard_rejects_exposed_associated_values() {
        for visibility in ["pub", "pub(crate)"] {
            let source = format!(
                "pub struct Actor {{ value: u8 }} impl Actor {{ {visibility} const UNCHECKED: Self = Self {{ value: 1 }}; }}"
            );
            assert_eq!(
                check_actor_boundary(&source).expect_err("visible constant constructs an Actor"),
                "Actor associated constants must stay private"
            );
        }
    }

    async fn start(database: &Database, supplier: &Supplier) -> (Context, Services) {
        start_on(database.connections(&database.user), supplier).await
    }

    async fn start_on(
        connections: platform::database::Connections,
        supplier: &Supplier,
    ) -> (Context, Services) {
        let context = Context::new(connections).await.expect("context");
        let services = Services::start(
            Barrier::begin(context.clone()).await.expect("barrier"),
            authentication::inventory(keys()),
            Settings::new(
                4,
                crate::identity::message::Limits::default(),
                authentication::session_settings(),
                crate::registration::Limits::default(),
                authentication::avatar_settings(),
            )
            .expect("settings"),
            supplier.client(),
            authentication::unused_client().await,
        )
        .await
        .expect("verified services");
        (context, services)
    }

    async fn expire(
        database: &Database,
        services: &Services,
        id: session::Id,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) {
        let elapsed = expires_at - database.now().await + TimeDelta::seconds(1);
        age(database, services.authentication(), elapsed).await;
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("expiry observer");
        assert!(observer.client().query_one(
            "SELECT session.created_at<session.expires_at AND session.expires_at<clock_timestamp() AND issuance.created_at=session.created_at AND issuance.expires_at=session.expires_at FROM identity.sessions session JOIN identity.issuances issuance ON issuance.id=session.id WHERE session.id=$1",
            &[&id],
        ).await.expect("independent complete expired session").get::<_, bool>(0));
        observer.terminate().await.expect("expiry observer ended");
    }

    #[test]
    fn authentication_damage_closes_admission_but_invalid_proofs_and_queries_do_not() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["account", "range", "app", "app value", "hash", "revocation", "future", "query", "token", "expired", "missing"] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(&database, &supplier).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13900000000").await;
                    let Receipt::Session(issued) = Box::pin(services.authentication().create_session(input(Uuid::new_v4(), message_id, "13900000000", CODE))).await.expect("real R1") else { panic!("initialized account"); };
                    let actor = services.current().authenticate(Proof { id: issued.id(), token: issued.token().text().parse().expect("original token") }).await.expect("healthy original session");
                    if case == "expired" {
                        expire(&database, &services, issued.id(), issued.expires_at()).await;
                    }
                    let owner = database.connections("postgres").recover().await.expect("disposable fault injector");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated post-startup damage");
                    match case {
                        "account" => { owner.client().execute("DELETE FROM identity.accounts WHERE id=$1", &[&actor.account_id()]).await.expect("orphaned retained session"); }
                        "range" => { owner.client().execute("UPDATE identity.sessions SET expires_at='280000-01-01 00:00:00+00' WHERE id=$1", &[&issued.id()]).await.expect("finite timestamp beyond the decoder range"); }
                        "app" => { owner.client().execute("UPDATE identity.sessions SET app_id='private-foreign-app' WHERE id=$1", &[&issued.id()]).await.expect("foreign stored source"); }
                        "app value" => {
                            owner.client().batch_execute("ALTER TABLE identity.sessions DROP CONSTRAINT sessions_app_id_check").await.expect("isolated row-check removal");
                            owner.client().execute("UPDATE identity.sessions SET app_id=' private-invalid-app ' WHERE id=$1", &[&issued.id()]).await.expect("invalid stored application value");
                        }
                        "hash" => {
                            owner.client().batch_execute("ALTER TABLE identity.sessions DROP CONSTRAINT sessions_hash_check").await.expect("isolated row-check removal");
                            owner.client().execute("UPDATE identity.sessions SET hash=decode(repeat('00',31),'hex') WHERE id=$1", &[&issued.id()]).await.expect("invalid stored hash length");
                        }
                        "revocation" => { owner.client().execute("UPDATE identity.sessions SET revoked_at=clock_timestamp(),sequence=2,version=2 WHERE id=$1", &[&issued.id()]).await.expect("tamper with the consumed revocation state without resealing"); }
                        "future" => { owner.client().execute("UPDATE identity.sessions SET created_at=clock_timestamp()+interval '1 day',expires_at=clock_timestamp()+interval '2 days' WHERE id=$1", &[&issued.id()]).await.expect("future stored lifetime"); }
                        "query" => { owner.client().batch_execute(&format!("REVOKE SELECT ON identity.sessions FROM {}", database.user)).await.expect("ordinary statement denial"); }
                        "expired" | "token" | "missing" => {}
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("independently visible fault");
                    if case == "revocation" {
                        assert!(owner.client().query_one(
                            "SELECT session.revoked_at IS NOT NULL AND session.sequence=2 AND session.version=2 AND NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=session.id) FROM identity.sessions session WHERE session.id=$1",
                            &[&issued.id()],
                        ).await.expect("independent state-only revocation damage").get::<_, bool>(0));
                    }
                    let count: i64 = owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("independent baseline").get(0);
                    let error = services.current().authenticate(Proof {
                        id: if case == "missing" { session::Id::new() } else { issued.id() },
                        token: if case == "token" { "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".parse().expect("other token") } else { issued.token().text().parse().expect("original token") },
                    }).await.err().expect("failed authentication withholds actor");
                    let invalid = matches!(case, "token" | "expired" | "missing");
                    let damaged = !invalid && case != "query";
                    assert_eq!(matches!(error, Error::Invalid), invalid, "{case}: {error:?}");
                    assert_eq!(error.unreliable(), damaged, "{case}: {error:?}");
                    if case == "revocation" {
                        assert!(matches!(error, Error::History(crate::history::Error::Seal(crate::keys::seal::Error::Mismatch))), "{case}: {error:?}");
                    }
                    assert_eq!(services.ready().await.is_err(), damaged, "{case}: readiness follows actual integrity");
                    assert!(!format!("{error:?}").contains("private-"));
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON identity.sessions TO {}", database.user)).await.expect("restore disposable permission");
                        services.current().authenticate(Proof { id: issued.id(), token: issued.token().text().parse().expect("original token") }).await.expect("fresh authentication succeeds after query repair");
                    }
                    if damaged || case == "query" {
                        let result = Box::pin(services.role().create(actor, Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(serde_json::json!({"name":"After authentication failure","level":20,"permissions":[]})).expect("role input"))).await;
                        if damaged {
                            assert!(matches!(result, Err(error) if error.is_closed()), "damage rejects subsequent business admission");
                        } else {
                            result.expect("ordinary query failure preserves new work");
                        }
                        assert_eq!(owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("actual subsequent write effects").get::<_, i64>(0), count + i64::from(!damaged));
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no later supplier call through drain"); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("authentication case {case}: {error}"));
            }
        }));
    }

    #[test]
    fn pool_wait_qualifies_at_fresh_transaction_time_and_rejects_a_rewind() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                for rollback in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start_on(proxied(&database, &proxy), &supplier).await;
                    let message_id = message(
                        &database,
                        services.authentication(),
                        &mut supplier,
                        "13900000000",
                    )
                    .await;
                    let Receipt::Session(issued) = services
                        .authentication()
                        .create_session(input(Uuid::new_v4(), message_id, "13900000000", CODE))
                        .await
                        .expect("R1")
                    else {
                        panic!("session");
                    };
                    let actor = services
                        .current()
                        .authenticate(Proof {
                            id: issued.id(),
                            token: issued.token().text().parse().expect("token"),
                        })
                        .await
                        .expect("actor");
                    let admin = database
                        .connections("postgres")
                        .recover()
                        .await
                        .expect("clock observer");
                    let first = context
                        .connections()
                        .lease()
                        .await
                        .expect("first pooled lease");
                    let second = context
                        .connections()
                        .lease()
                        .await
                        .expect("second pooled lease");
                    let before = proxy.time_queries();
                    let current = services.current().clone();
                    let (waiting, queued) = tokio::sync::oneshot::channel();
                    let reader = tokio::spawn(async move {
                        use std::future::Future as _;
                        let mut result = Box::pin(current.read(&actor));
                        std::future::poll_fn(|cx| {
                            assert!(result.as_mut().poll(cx).is_pending());
                            std::task::Poll::Ready(())
                        })
                        .await;
                        waiting.send(()).expect("queued reader");
                        result.await
                    });
                    queued.await.expect("reader reached the exhausted pool");
                    assert_eq!(
                        proxy.time_queries(),
                        before,
                        "pool waits do not sample separately"
                    );
                    assert!(!reader.is_finished());
                    if rollback {
                        proxy.arm(Fault::RewoundCurrent);
                        first.release();
                        second.release();
                        timeout(Duration::from_secs(1), proxy.intercepted())
                            .await
                            .expect("decisive query reached after normal pool recycling");
                        proxy.release();
                    } else {
                        expire(&database, &services, issued.id(), issued.expires_at()).await;
                        first.release();
                        second.release();
                    }
                    let result = timeout(Duration::from_secs(2), reader)
                        .await
                        .expect("read ends")
                        .expect("reader");
                    if rollback {
                        assert!(matches!(
                            result,
                            Err(Error::Runtime(crate::runtime::Error::ClockInvalid))
                        ));
                        assert!(
                            services.ready().await.is_err(),
                            "a transaction time rewind closes admission"
                        );
                    } else {
                        let error = result.err().expect("expired original session is rejected");
                        assert!(matches!(error, Error::Invalid), "{error:?}");
                    }
                    admin.terminate().await.expect("observer ended");
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    proxy.stop().await;
                    supplier.stop().await;
                }
            })
            .await
            .expect("pool and rollback budget");
        });
    }

    #[test]
    fn current_reads_sample_authorization_and_signing_time_without_the_instance_session() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::proxied;

        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                for cancel in [true, false] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start_on(proxied(&database, &proxy), &supplier).await;
                    let message_id = message(
                        &database,
                        services.authentication(),
                        &mut supplier,
                        "13900000000",
                    )
                    .await;
                    let Receipt::Session(issued) = services
                        .authentication()
                        .create_session(input(Uuid::new_v4(), message_id, "13900000000", CODE))
                        .await
                        .expect("R1")
                    else {
                        panic!("session");
                    };
                    let actor = services
                        .current()
                        .authenticate(Proof {
                            id: issued.id(),
                            token: issued.token().text().parse().expect("token"),
                        })
                        .await
                        .expect("actor");
                    let instance = context.clock_session().await;
                    let before = proxy.time_queries();
                    proxy.arm(Fault::StalledCurrent);
                    let current = services.current().clone();
                    let reader = tokio::spawn(async move { current.read(&actor).await });
                    timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("decisive read is in the snapshot");
                    if cancel {
                        reader.abort();
                        assert!(matches!(reader.await, Err(error) if error.is_cancelled()));
                    } else {
                        proxy.release();
                        timeout(Duration::from_secs(2), reader)
                            .await
                            .expect("read ended")
                            .expect("task")
                            .expect("snapshot result needs no separate clock transaction");
                    }
                    proxy.release();
                    assert_eq!(
                        proxy.time_queries() - before,
                        if cancel { 1 } else { 2 },
                        "authorization observes the snapshot; successful delivery samples fresh signing time"
                    );
                    if cancel {
                        assert!(
                            !*context.admission().borrow(),
                            "cancelling a reader does not close trusted time"
                        );
                        let actor = services
                            .current()
                            .authenticate(Proof {
                                id: issued.id(),
                                token: issued.token().text().parse().expect("token"),
                            })
                            .await
                            .expect("a later request authenticates with fresh trusted time");
                        services
                            .current()
                            .read(&actor)
                            .await
                            .expect("a later request reads the real account");
                        services.ready().await.expect("admission remains open");
                    }
                    drop(instance);
                    services.drain().await.expect("drained");
                    let _ = context.stop().await;
                    proxy.stop().await;
                    supplier.stop().await;
                }
            })
            .await
            .expect("clock fault budget");
        });
    }

    #[test]
    fn decisive_read_rechecks_the_original_actor_and_distinguishes_damage_from_invalidation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
                for (change, invalid) in [
                    ("expired", true),
                    ("revoked", true),
                    ("suspended", true),
                    ("DELETE FROM identity.sessions", false),
                    ("DELETE FROM identity.accounts", false),
                    ("DELETE FROM identity.persons", false),
                    ("DELETE FROM identity.revisions", false),
                    ("UPDATE identity.revisions SET person_id=gen_random_uuid()", false),
                    ("UPDATE identity.revisions SET seal=set_byte(seal,0,get_byte(seal,0)#1)", false),
                    ("UPDATE identity.revisions SET seal_key_id=999", false),
                    ("DELETE FROM identity.avatars", false),
                    ("UPDATE identity.revisions SET name=set_byte(name,0,255)", false),
                    ("UPDATE identity.revisions SET name=set_byte(name, octet_length(name)-1, get_byte(name, octet_length(name)-1) # 1)", false),
                    ("UPDATE identity.accounts SET id_number=decode(repeat('00',32),'hex')", false),
                    ("ALTER TABLE identity.accounts DROP CONSTRAINT accounts_id_number_check; UPDATE identity.accounts SET id_number=decode(repeat('00',31),'hex')", false),
                    ("UPDATE identity.avatars SET digest=decode(repeat('00',32),'hex')", false),
                    ("UPDATE identity.sessions SET app_id='other-website'", false),
                ] {
                  timeout(Duration::from_secs(15), async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(&database, &supplier).await;
                    let message_id = message(&database, services.authentication(), &mut supplier, "13900000000").await;
                    let Receipt::Session(issued) = services.authentication().create_session(input(Uuid::new_v4(), message_id, "13900000000", CODE)).await.expect("R1") else { panic!("existing account session"); };
                    let manager = services.current().authenticate(Proof { id: issued.id(), token: issued.token().text().parse().expect("token") }).await.expect("middleware actor");
                    let actor = if change == "suspended" {
                        Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await
                    } else { manager.clone() };
                    services.current().read(&actor).await.expect("healthy complete projection before injection");
                    let admin = database.connections("postgres").recover().await.expect("isolated post-authentication fixture");
                    match change {
                        "expired" => expire(&database, &services, issued.id(), issued.expires_at()).await,
                        "revoked" => {
                            services.revocation().revoke(actor.clone(), Uuid::new_v4().to_string().parse().expect("logout operation"), crate::identity::revocation::Scope::Current).await.expect("real session revocation");
                        }
                        "suspended" => {
                            let candidate = services.account().candidate(&manager, "13800000000".parse().expect("registered mobile")).await.expect("real management candidate");
                            let input = serde_json::from_value(serde_json::json!({
                                "personId": candidate.identity().person_id(),
                                "revisionId": candidate.identity().revision_id(),
                                "mobile": "13800000000",
                            })).expect("confirmed suspension input");
                            services.suspension().suspend(manager, Uuid::new_v4().to_string().parse().expect("suspension operation"), actor.account_id(), input).await.expect("real management suspension");
                        }
                        _ => {
                            admin.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {change}; COMMIT")).await.expect("isolated mutation");
                        }
                    }
                    if change == "DELETE FROM identity.sessions" {
                        assert_eq!(admin.client().query_one("SELECT count(*) FROM identity.sessions WHERE id=$1", &[&actor.session_id()]).await.expect("independent missing anchored state").get::<_, i64>(0), 0);
                    }
                    match services.current().read(&actor).await {
                        Err(Error::Invalid) => assert!(invalid, "damage must not appear as logout: {change}"),
                        Err(error) => {
                            assert!(!invalid, "unavailable original session must be rejected: {change}: {error:?}");
                            if change == "DELETE FROM identity.sessions" {
                                assert!(matches!(&error, Error::History(history) if history.corrupt()), "{change}: {error:?}");
                            }
                            assert!(error.unreliable(), "{change}: {error:?}");
                        }
                        Ok(_) => panic!("changed original actor cannot yield a partial or stale account: {change}"),
                    }
                    assert_eq!(services.ready().await.is_ok(), invalid, "only a normal identity rejection preserves admission: {change}");
                    admin.terminate().await.expect("fixture ended");
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                  }).await.unwrap_or_else(|error| panic!("profile case {change}: {error}"));
                }
        });
    }

    #[test]
    fn current_r1_and_r2_profiles_and_frozen_images_do_not_depend_on_handoff() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(30), async {
                for registering in [false, true] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(&database, &supplier).await;
                    let mobile = if registering { "13800000000" } else { "13900000000" };
                    let message_id = message(&database, services.authentication(), &mut supplier, mobile).await;
                    let issued = services.authentication().create_session(input(Uuid::new_v4(), message_id, mobile, CODE)).await.expect("R1");
                    let (id, token) = match issued {
                        Receipt::Session(session) => (session.id(), session.token().text()),
                        Receipt::Registration(credential) => {
                            let receipt = services.registration().register(crate::registration::Input::new(Uuid::new_v4().to_string().parse().expect("operation"), credential.id(), credential.token().text().parse().expect("proof"), "Fixture Operator".parse().expect("name"), "00000020000229007X".parse().expect("number"))).await.expect("R2");
                            (receipt.id(), receipt.token().text())
                        }
                    };
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let counts: Vec<i64> = observer.client().query_one("SELECT ARRAY[(SELECT count(*) FROM identity.authentications),(SELECT count(*) FROM identity.sessions),(SELECT count(*) FROM identity.attempts),(SELECT count(*) FROM identity.receipts),(SELECT count(*) FROM identity.persons),(SELECT count(*) FROM identity.revisions),(SELECT count(*) FROM identity.accounts),(SELECT count(*) FROM identity.avatars)]", &[]).await.expect("original facts").get(0);
                    let actor = services.current().authenticate(Proof { id, token: token.parse().expect("token") }).await.expect("current actor");
                    assert!(matches!(services.current().authenticate(Proof { id: session::Id::new(), token: token.parse().expect("token") }).await, Err(Error::Invalid)));
                    assert!(matches!(services.current().authenticate(Proof { id, token: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".parse().expect("other token") }).await, Err(Error::Invalid)));
                    let account = services.current().read(&actor).await.expect("complete profile");
                    assert_eq!(account.name().as_ref(), if registering { "Fixture Operator" } else { "隔离测试" });
                    assert_eq!(account.suffix(), if registering { "007X" } else { "0010" });
                    let row = observer.client().query_one("SELECT account.id, person.id, revision.id, account.avatar FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id JOIN identity.revisions revision ON revision.id=person.revision_id WHERE account.id=$1", &[&account.id()]).await.expect("independent current identities");
                    assert_eq!(account.person_id(), row.get(1));
                    assert_eq!(account.revision_id(), row.get(2));
                    assert_eq!(account.avatar().revision(), row.get(3));
                    let url = url::Url::parse(account.avatar().link().href()).expect("signed URL");
                    let digest = url.path().strip_prefix("/api/v1/avatars/").expect("path").strip_suffix(".webp").expect("suffix").parse().expect("digest");
                    let query: std::collections::HashMap<_,_> = url.query_pairs().collect();
                    let image = services.avatar().read(link::Proof::new(digest, query["key_id"].parse().expect("key id"), query["expires_at"].parse().expect("expiration"), query["signature"].parse().expect("signature")), avatar::read_capacity()).await.expect("anonymous image");
                    assert_eq!(image.bytes(), include_bytes!("../../../../assets/avatars/0-cat.webp"));
                    assert!(image.cache() <= Duration::from_secs(86400));
                    let after: Vec<i64> = observer.client().query_one("SELECT ARRAY[(SELECT count(*) FROM identity.authentications),(SELECT count(*) FROM identity.sessions),(SELECT count(*) FROM identity.attempts),(SELECT count(*) FROM identity.receipts),(SELECT count(*) FROM identity.persons),(SELECT count(*) FROM identity.revisions),(SELECT count(*) FROM identity.accounts),(SELECT count(*) FROM identity.avatars)]", &[]).await.expect("unchanged facts").get(0);
                    assert_eq!(counts, after);
                    observer.terminate().await.expect("observer ended");
                    {
                        if registering {
                            crate::registration::tests::age_facts(&database, &services, TimeDelta::seconds(601)).await;
                        } else {
                            age(&database, services.authentication(), TimeDelta::seconds(601)).await;
                        }
                        let cleaner = database.connections(&database.user).recover().await.expect("runtime cleaner");
                        assert_eq!(cleaner.client().execute("DELETE FROM identity.handoffs WHERE id=$1", &[&id]).await.expect("expired handoff cleanup"), 1);
                        cleaner.terminate().await.expect("cleaner ended");
                        services.current().read(&actor).await.expect("read after handoff cleanup");
                        services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        let (context, services) = start(&database, &supplier).await;
                        let actor = services.current().authenticate(Proof { id, token: token.parse::<session::Token>().expect("original token") }).await.expect("actor after restart");
                        services.current().read(&actor).await.expect("profile after restart");
                        services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    }
                    supplier.stop().await;
                }
            }).await.expect("current profile budget");
        });
    }
}
