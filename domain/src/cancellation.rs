//! One cancellation command joins permanent Identity facts and all current Access removal.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    access::{self, appointment, grant, holder, role},
    history::{self, Read, Verified},
    identity::{
        account,
        cancellation::{self, Cancellation},
        current::{self, Actor},
        keys::Keys,
        mobile::{self, Mobile},
        operation,
        person::{self, revision},
        session,
    },
    kernel::{self, App, Authorization, Command, completion, tx::Tx},
};

pub type Error = kernel::Error<Rejection>;

#[derive(Debug)]
pub enum Rejection {
    Absent,
    TargetChanged,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent => kernel::Status::NotFound,
            Self::TargetChanged => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::TargetChanged => "target_changed",
        }
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = cancellation::Input)]
pub struct Input {
    account_id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = cancellation::Management)]
pub struct Management {
    person_id: person::Id,
    revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

enum Action {
    Current(Input),
    Account {
        account_id: account::Id,
        input: Management,
    },
}

impl Action {
    fn target(&self) -> account::Id {
        match self {
            Self::Current(input) => input.account_id,
            Self::Account { account_id, .. } => *account_id,
        }
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        match self {
            Self::Current(input) => cancellation::request(
                input.account_id,
                input.person_id,
                input.revision_id,
                None,
                authority,
            ),
            Self::Account { account_id, input } => cancellation::request(
                *account_id,
                input.person_id,
                input.revision_id,
                Some(&input.mobile),
                authority,
            ),
        }
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
    keys: Arc<Keys>,
    accounts: account::Reader,
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service, keys: Arc<Keys>) -> Self {
        Self {
            current,
            roles: role::Reader::new(app.clone()),
            accounts: account::Reader::new(app.clone(), Arc::clone(&keys)),
            app,
            keys,
        }
    }

    pub async fn cancel(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Cancellation, Error> {
        self.app
            .command(
                operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    action: Action::Current(input),
                },
            )
            .await
    }

    pub async fn cancel_account(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        account_id: account::Id,
        input: Management,
    ) -> Result<Cancellation, Error> {
        self.app
            .command(
                operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    action: Action::Account { account_id, input },
                },
            )
            .await
    }
}

struct Write {
    service: Service,
    actor: Actor,
    action: Action,
}

enum Facts {
    Current {
        account: Verified<account::state::State>,
        mobile: Mobile,
        authority: Authorization,
    },
    Managed {
        account: Option<Verified<account::state::State>>,
        subject: Option<account::Subject>,
        target: Option<(person::Id, role::Level)>,
        manager: role::Level,
        authority: Authorization,
    },
}

struct Change {
    account: Verified<account::state::State>,
    mobile: Mobile,
    revision_id: revision::Id,
    operator_id: revision::Id,
    now: DateTime<Utc>,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = cancellation::COMPLETIONS;
    type Facts = Facts;
    type Change = Change;
    type Output = Cancellation;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        self.action.request(authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        tx.shared(b"access/scope", b"institution").await?;
        tx.shared(b"access/scope", b"workforce").await?;
        let target = match &self.action {
            Action::Current(_) => self.actor.account_id(),
            Action::Account { account_id, .. } => *account_id,
        };
        self.actor.lock_accounts(tx.client(), target).await?;
        account::state::lock_person(tx.client(), target).await?;
        holder::lock(tx.client(), target).await?;
        session::state::lock_management(tx.client(), self.actor.session_id(), target).await?;
        if let Some(person) = account::state::discover_owner(tx.client(), target).await? {
            grant::state::lock_history(tx.client(), person).await?;
        }
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject = subject.ok_or(Error::corrupt("cancellation declares its current session"))?;
        match self.action {
            Action::Current(_) => Ok(Authorization {
                person_id: subject.person_id(),
                revision_id: subject.revision_id(),
                now: subject.observed_at(),
            }),
            Action::Account { .. } => {
                let (_, person_id, revision_id, now) = access::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    role::Code::InstitutionAccounts,
                )
                .await?;
                Ok(Authorization {
                    person_id,
                    revision_id,
                    now,
                })
            }
        }
    }

    async fn absent(&self, tx: &Tx) -> Result<(), Error> {
        cancellation::absent(tx, &self.service.app).await
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let client = tx.client();
        match &self.action {
            Action::Current(_) => {
                let account = self
                    .service
                    .accounts
                    .state(client, self.actor.account_id(), authority.now)
                    .await?
                    .ok_or(Error::corrupt("the authorized self retains its account"))?;
                let current = account
                    .current()
                    .ok_or(Error::corrupt("the authorized self is current"))?;
                let mobile = current
                    .mobile()
                    .open(
                        &self.service.keys.encryption,
                        &self.service.keys.mobile,
                        mobile::envelope::Binding::Account(account.id()),
                    )
                    .map_err(cancellation::Error::from)?;
                Ok(Facts::Current {
                    account,
                    mobile,
                    authority: *authority,
                })
            }
            Action::Account { account_id, input } => {
                let manager = appointment::find_by_account(
                    client,
                    &self.service.roles,
                    self.actor.account_id(),
                    authority.now,
                )
                .await?
                .ok_or(Error::corrupt(
                    "the authorized manager retains its institutional role",
                ))?;
                let target = appointment::find_by_account(
                    client,
                    &self.service.roles,
                    *account_id,
                    authority.now,
                )
                .await?
                .map(|target| (target.person_id(), target.role().level()));
                let account = self
                    .service
                    .accounts
                    .state(client, *account_id, authority.now)
                    .await?;
                let subject = self
                    .service
                    .accounts
                    .find_by_id(client, *account_id, &input.mobile, authority.now)
                    .await?;
                Ok(Facts::Managed {
                    account,
                    subject,
                    target,
                    manager: manager.role().level(),
                    authority: *authority,
                })
            }
        }
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Change, Error> {
        match (&self.action, facts) {
            (
                Action::Current(input),
                Facts::Current {
                    account,
                    mobile,
                    authority,
                },
            ) => {
                if input.account_id != account.id()
                    || input.person_id != authority.person_id
                    || input.revision_id != authority.revision_id
                {
                    return Err(Error::Rejected(Rejection::TargetChanged));
                }
                Ok(Change {
                    account,
                    mobile,
                    revision_id: authority.revision_id,
                    operator_id: authority.revision_id,
                    now,
                })
            }
            (
                Action::Account { input, .. },
                Facts::Managed {
                    account,
                    subject,
                    target,
                    manager,
                    authority,
                },
            ) => {
                let (person, level) = target.ok_or(Error::Rejected(Rejection::Absent))?;
                if person == authority.person_id || level >= manager {
                    return Err(Error::Rejected(Rejection::Absent));
                }
                let subject = subject.ok_or(Error::Rejected(Rejection::TargetChanged))?;
                if subject.person_id() != input.person_id
                    || subject.revision_id() != input.revision_id
                {
                    return Err(Error::Rejected(Rejection::TargetChanged));
                }
                let account =
                    account.ok_or(Error::corrupt("the confirmed target retains its account"))?;
                Ok(Change {
                    account,
                    mobile: input.mobile.clone(),
                    revision_id: input.revision_id,
                    operator_id: authority.revision_id,
                    now,
                })
            }
            _ => Err(Error::corrupt(
                "cancellation facts belong to the declared action",
            )),
        }
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: Change,
        pending: &mut Option<history::Pending>,
    ) -> Result<Cancellation, Error> {
        let client = tx.client();
        let operation_id = tx.operation_id();
        let account_id = change.account.id();
        let person = history::find::<person::state::State>(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            change.account.person_id(),
            change.now,
        )
        .await?
        .ok_or(Error::corrupt(
            "cancellation retains its permanent person state",
        ))?;
        let result = cancellation::insert(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            &person,
            pending,
            cancellation::New {
                account_id,
                person_id: change.account.person_id(),
                revision_id: change.revision_id,
                operator_id: change.operator_id,
                mobile: &change.mobile,
                operation_id,
                cancelled_at: change.now,
            },
        )
        .await?;
        let holder = history::find::<holder::State>(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            account_id,
            change.now,
        )
        .await?
        .ok_or(Error::corrupt(
            "cancellation retains its institutional holder",
        ))?;
        history::retain(
            pending,
            holder::cancel(&holder, result.id(), operation_id, change.now)?
                .write(client, self.service.app.keys(), self.service.app.anchors())
                .await?,
        )?;
        grant::cancel(
            client,
            &self.service.roles,
            change.account.person_id(),
            grant::Cancellation {
                id: result.id(),
                operator_id: change.operator_id,
                revision_id: change.revision_id,
                operation_id,
                cancelled_at: change.now,
            },
            pending,
        )
        .await?;
        cancellation::remove(
            client,
            &self.service.app,
            &change.account,
            result.id(),
            operation_id,
            change.now,
            pending,
        )
        .await?;
        Ok(result)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Cancellation, Error> {
        // Recovery enters here directly, without the current-session or permission pipeline.
        cancellation::lock_replay(tx, self.action.target()).await?;
        let original = cancellation::find_by_operation(
            tx.client(),
            self.service.app.keys(),
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "a cancellation completion retains its original fact",
        ))?;
        let matches = match &self.action {
            Action::Current(input) => {
                request.person_id() == input.person_id
                    && original.matches_current(
                        input.account_id,
                        input.person_id,
                        input.revision_id,
                    )
            }
            Action::Account { account_id, input } => original.matches_account(
                *account_id,
                input.person_id,
                input.revision_id,
                request.person_id(),
                &input.mobile,
            ),
        };
        if !matches {
            return Err(Error::corrupt(
                "cancellation and its accepted completion agree",
            ));
        }
        original
            .verify_removal(tx.client(), &self.service.app, Read::Locked, now)
            .await?;
        holder::confirm_cancellation(
            tx.client(),
            self.service.app.keys(),
            self.service.app.anchors(),
            original.id(),
            tx.operation_id(),
            now,
        )
        .await?;
        Ok(original.into_cancellation())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            appointment,
            role::tests::{actor, start},
        },
        cancellation::{Error, Input, Management},
        identity::{
            authentication::{Database, Supplier, server},
            current, operation,
        },
    };

    pub(crate) fn confirmation(account: &current::Account) -> Input {
        serde_json::from_value(json!({
            "accountId": account.id(),
            "personId": account.person_id(),
            "revisionId": account.revision_id(),
        }))
        .expect("complete current-account confirmation")
    }

    pub(crate) fn management(account: &current::Account) -> Management {
        serde_json::from_value(json!({"personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13800000000"})).expect("confirmed managed account")
    }

    async fn replay_self(
        service: &super::Service,
        actor: current::Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<super::Cancellation, Error> {
        use crate::kernel::{
            Authorization, Command,
            operation::Family,
            tx::{Connection, Tx},
        };
        let connection = service
            .app
            .context()
            .connections()
            .recover()
            .await
            .expect("recovery connection");
        let (tx, begun) = Tx::begin(
            service.app.context(),
            Connection::Recovery(connection),
            tokio::time::Instant::now() + Duration::from_secs(10),
            Family::Cancellation,
            operation_id,
        )
        .await;
        begun.expect("original-operation lock");
        let now = tx
            .now::<super::Rejection>(service.app.context())
            .await
            .expect("recovery time");
        // The fixture retained these values from the authenticated account before cancellation.
        let authority = Authorization {
            person_id: input.person_id,
            revision_id: input.revision_id,
            now,
        };
        let command = super::Write {
            service: service.clone(),
            actor,
            action: super::Action::Current(input),
        };
        let request = command
            .request(&authority)
            .expect("original accepted request");
        let result = command.replay(&tx, &request, now).await;
        tx.rollback().await.expect("read-only recovery ended");
        result
    }

    pub(crate) async fn before_cancellation(
        client: &tokio_postgres::Client,
        account: &current::Account,
    ) -> tokio_postgres::Row {
        let row = client.query_one(
            "SELECT created_at,
                ARRAY(SELECT id FROM identity.sessions WHERE account_id=$1 ORDER BY id) AS sessions,
                ARRAY(SELECT id FROM identity.creations WHERE id=$1 ORDER BY id) AS creations,
                ARRAY(SELECT id FROM identity.replacements WHERE account_id=$1 ORDER BY id) AS replacements,
                ARRAY(SELECT id FROM identity.logouts WHERE account_id=$1 ORDER BY id) AS logouts
             FROM identity.accounts WHERE id=$1 AND person_id=$2",
            &[&account.id(), &account.person_id()],
        ).await.expect("original account and permanent history");
        assert_eq!(
            row.get::<_, Vec<crate::identity::account::Id>>("creations"),
            vec![account.id()]
        );
        row
    }

    pub(crate) async fn assert_cancelled_account(
        client: &tokio_postgres::Client,
        account: &current::Account,
        before: &tokio_postgres::Row,
        cancellation_id: crate::identity::cancellation::Id,
        cancelled_at: chrono::DateTime<chrono::Utc>,
    ) {
        let created_at: chrono::DateTime<chrono::Utc> = before.get("created_at");
        let sessions: Vec<Uuid> = before.get("sessions");
        let row = client.query_one(
            "SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$2
                    AND created_at=$3 AND cancellation_id=$4 AND changed_at=$5
                    AND mobile IS NULL AND id_number IS NULL AND suspension_id IS NULL) AS account,
                EXISTS(SELECT 1 FROM identity.cancellations cancellation
                    JOIN identity.completions completion ON completion.family='cancellation'
                        AND completion.operation_id=cancellation.operation_id
                    JOIN identity.revisions operator ON operator.id=cancellation.operator_id
                    WHERE cancellation.id=$4 AND completion.target=$1::uuid::text
                        AND completion.subject='session' AND completion.person_id=operator.person_id
                        AND completion.revision_id=cancellation.operator_id
                        AND completion.action=CASE WHEN operator.person_id=$2 THEN 'self' ELSE 'managed' END
                        AND completion.created_at=$5 AND completion.expires_at IS NULL
                        AND completion.transaction_id=cancellation.transaction_id) AS completion,
                NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$1) AS sessions,
                NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=ANY($6)) AS handoffs,
                NOT EXISTS(SELECT 1 FROM identity.revocations WHERE account_id=$1) AS revocations,
                NOT EXISTS(SELECT 1 FROM identity.receipts WHERE account_id=$1) AS receipts,
                EXISTS(SELECT 1 FROM access.holders holder JOIN access.projections projection
                    ON projection.id=holder.cancellation_id WHERE holder.id=$1 AND holder.person_id=$2
                    AND holder.cancellation_id=$4 AND projection.holder_id=holder.id
                    AND projection.sequence=holder.sequence AND holder.changed_at=$5) AS holder,
                (SELECT count(*)=1 FROM identity.avatars WHERE account_id=$1 AND released_at=$5)
                    AND NOT EXISTS(SELECT 1 FROM identity.avatars WHERE account_id=$1 AND released_at IS NULL) AS avatar,
                ARRAY(SELECT id FROM identity.creations WHERE id=$1 ORDER BY id) AS creations,
                ARRAY(SELECT id FROM identity.replacements WHERE account_id=$1 ORDER BY id) AS replacements,
                ARRAY(SELECT id FROM identity.logouts WHERE account_id=$1 ORDER BY id) AS logouts",
            &[&account.id(), &account.person_id(), &created_at, &cancellation_id, &cancelled_at, &sessions],
        ).await.expect("permanent tombstone and complete credential removal");
        for column in [
            "account",
            "completion",
            "sessions",
            "handoffs",
            "revocations",
            "receipts",
            "holder",
            "avatar",
        ] {
            assert!(row.get::<_, bool>(column), "{column}");
        }
        for column in ["creations", "replacements", "logouts"] {
            assert_eq!(
                row.get::<_, Vec<Uuid>>(column),
                before.get::<_, Vec<Uuid>>(column),
                "permanent {column} retained"
            );
        }
    }

    #[test]
    fn accepted_cancellations_recover_after_session_revocation() {
        use fixture::database_proxy::Proxy;

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let holder = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                Box::pin(crate::access::grant::tests::admit(&services, &manager, crate::access::Entry::Institution, "13800000000", crate::access::role::Kind::Default)).await;
                let account = services.current().read(&holder).await.expect("confirmed target");
                let observer = database.connections(&database.user).recover().await.expect("independent facts");
                let before = before_cancellation(observer.client(), &account).await;
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                let result = Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, services.cancellation().cancel_account(manager.clone(), operation, account.id(), management(&account)))).await;
                let row = observer.client().query_one("SELECT cancellation.id,cancellation.cancelled_at,cancellation.sequence,person.sequence AS person_sequence FROM identity.cancellations cancellation JOIN identity.persons person ON person.id=cancellation.person_id WHERE cancellation.operation_id=$1 AND cancellation.person_id=$2 AND cancellation.account_id=$3", &[&operation,&account.person_id(),&account.id()]).await.expect("one permanent cancellation");
                assert_cancelled_account(observer.client(), &account, &before, row.get("id"), row.get("cancelled_at")).await;
                assert_eq!((row.get::<_,i64>("sequence"),row.get::<_,i64>("person_sequence")), (2,2));
                if let Ok(original) = &result { assert_eq!(original.id(),row.get("id")); }
                let Err(error) = Box::pin(services.cancellation().cancel_account(manager, operation, account.id(), management(&account))).await else { panic!("new request must be refused"); };
                assert_eq!(error.code(), Some("authentication_invalid"));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.cancellations WHERE operation_id=$1", &[&operation]).await.expect("no replay writes").get::<_,i64>(0), 1);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.ready().await.expect("cancellation history survives restart");
                assert_cancelled_account(observer.client(), &account, &before, row.get("id"), row.get("cancelled_at")).await;
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                assert!(result.is_ok(), "accepted cancellation lost its result: {:?}", result.as_ref().err());
            })).await.expect("cancellation recovery after session loss deadline");
        }));
    }

    async fn manager(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
        administrator: &current::Actor,
    ) -> (current::Actor, crate::access::role::Role) {
        use crate::identity::authentication::session::{
            Receipt,
            tests::{CODE, input, message},
        };
        let id = message(database, services.authentication(), supplier, "13700000000").await;
        let Receipt::Registration(credential) =
            Box::pin(services.authentication().create_session(input(
                Uuid::new_v4(),
                id,
                "13700000000",
                CODE,
            )))
            .await
            .expect("manager registration credential")
        else {
            panic!("new manager");
        };
        let issued = Box::pin(
            services
                .registration()
                .register(crate::registration::Input::new(
                    Uuid::new_v4().to_string().parse().expect("operation"),
                    credential.id(),
                    credential.token().text().parse().expect("proof"),
                    "Account Manager".parse().expect("name"),
                    "000000200002290029".parse().expect("number"),
                )),
        )
        .await
        .expect("manager registration");
        let actor = services
            .current()
            .authenticate(current::Proof {
                id: issued.id(),
                token: issued.token().text().parse().expect("token"),
            })
            .await
            .expect("manager actor");
        let role = Box::pin(services.role().create(administrator.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(json!({"name": "Account managers", "level": 80, "permissions": ["institution.accounts"]})).expect("role input"))).await.expect("specific account permission");
        let target = services
            .appointment()
            .candidate(administrator, "13700000000".parse().expect("mobile"))
            .await
            .expect("assignment target");
        let input = serde_json::from_value(json!({"target": {"accountId": target.identity().id(), "personId": target.identity().person_id(), "revisionId": target.identity().revision_id(), "mobile": "13700000000", "assignmentId": target.assignment_id()}, "role": {"id": role.id(), "changeId": role.change_id()}})).expect("manager assignment");
        Box::pin(services.appointment().assign(
            administrator.clone(),
            Uuid::new_v4().to_string().parse().expect("operation"),
            input,
        ))
        .await
        .expect("actual manager assignment");
        (actor, role)
    }

    #[test]
    fn management_cancellation_retains_bound_results_and_the_managers_access() {
        use crate::access::role;
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for paused in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let account = services.current().read(&target).await.expect("target identity");
                    let (manager, definition) = Box::pin(manager(&database, &services, &mut supplier, &administrator)).await;
                    let operator = services.current().read(&manager).await.expect("manager identity");
                    let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                    for field in ["personId", "revisionId", "mobile"] {
                        let mut input = json!({"personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13800000000"});
                        input[field] = if field == "mobile" { json!("13600000000") } else { json!(Uuid::new_v4()) };
                        assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation, account.id(), serde_json::from_value(input).expect("valid mismatched confirmation"))).await, Err(Error::Rejected(super::Rejection::TargetChanged))));
                    }
                    assert_eq!(Box::pin(services.cancellation().cancel_account(target.clone(), operation, operator.id(), management(&account))).await.err().expect("ordinary account lacks management permission").code(), Some("permission_denied"));
                    for id in [operator.id(), services.current().read(&administrator).await.expect("higher account").id()] {
                        assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation, id, management(&account))).await, Err(Error::Rejected(super::Rejection::Absent))));
                    }
                    if paused {
                        Box::pin(services.suspension().suspend(manager.clone(), Uuid::new_v4().to_string().parse().expect("pause operation"), account.id(), serde_json::from_value(json!({"personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13800000000"})).expect("pause confirmation"))).await.expect("actual paused target");
                    }
                    let candidate = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("normal and paused targets remain manageable");
                    assert_eq!(candidate.suspension_id().is_some(), paused);
                    let before = before_cancellation(observer.client(), &account).await;
                    let result = Box::pin(services.cancellation().cancel_account(manager.clone(), operation, account.id(), management(&account))).await.expect("management cancellation");
                    let fact = observer.client().query_one("SELECT id, cancelled_at, sequence, account_id=$2 AND person_id=$3 AND revision_id=$4 AND operator_id=$5 AS bound, sequence=2 AS sequenced FROM identity.cancellations WHERE operation_id=$1", &[&operation, &account.id(), &account.person_id(), &account.revision_id(), &operator.revision_id()]).await.expect("permanent result before replay");
                    assert!(fact.get::<_, bool>("bound"));
                    assert!(fact.get::<_, bool>("sequenced"), "paused={paused}, sequence={}", fact.get::<_, i64>("sequence"));
                    assert_eq!(result.id(), fact.get("id")); assert_eq!(result.cancelled_at(), fact.get::<_, chrono::DateTime<chrono::Utc>>("cancelled_at"));
                    assert_cancelled_account(observer.client(), &account, &before, result.id(), result.cancelled_at()).await;
                    let manager_state = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1) AS session, EXISTS(SELECT 1 FROM identity.accounts WHERE id=$2 AND cancellation_id IS NULL) AS account", &[&manager.session_id(), &operator.id()]).await.expect("target cancellation preserves the manager");
                    assert!(manager_state.get::<_, bool>("session"), "manager session");
                    assert!(manager_state.get::<_, bool>("account"), "manager account");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.suspensions WHERE person_id=$1", &[&account.person_id()]).await.expect("permanent pause retained").get::<_, i64>(0), i64::from(paused));
                    services.current().read(&manager).await.expect("manager session still valid");
                    assert!(matches!(services.current().read(&target).await, Err(current::Error::Invalid)));
                    let replay = Box::pin(services.cancellation().cancel_account(manager.clone(), operation, account.id(), management(&account))).await.expect("deleted target is not requalified before the original");
                    assert_eq!(replay.id(), result.id()); assert_eq!(replay.cancelled_at(), result.cancelled_at());
                    assert!(matches!(Box::pin(services.cancellation().cancel_account(administrator.clone(), operation, account.id(), management(&account))).await, Err(Error::Mismatch)));
                    for field in ["personId", "revisionId", "mobile"] {
                        let mut input = json!({"personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13800000000"});
                        input[field] = if field == "mobile" { json!("13600000000") } else { json!(Uuid::new_v4()) };
                        assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation, account.id(), serde_json::from_value(input).expect("valid different original input"))).await, Err(Error::Mismatch)));
                    }
                    assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation, operator.id(), management(&account))).await, Err(Error::Mismatch)));
                    Box::pin(services.correction().correct(administrator.clone(), Uuid::new_v4().to_string().parse().expect("correction operation"), serde_json::from_value(json!({"target": {"personId": operator.person_id(), "revisionId": operator.revision_id()}, "name": "Revised Manager", "idNumber": "000000200002290029"})).expect("manager correction"))).await.expect("operator identity later changes");
                    assert_eq!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation, account.id(), management(&account))).await.expect("original binds operator person, not current revision").id(), result.id());
                    let retained = crate::kernel::completion::original::<super::Rejection>(
                        observer.client(), services.app().keys(), crate::identity::cancellation::COMPLETIONS, operation,
                    ).await.expect("authenticated completion").expect("original completion");
                    assert_eq!(retained.person_id(), operator.person_id());
                    assert_eq!(retained.revision_id(), operator.revision_id(), "later correction never replaces original attribution");
                    assert_eq!(retained.input::<serde_json::Value, super::Rejection>(services.app().keys()).expect("canonical managed input"),
                        json!({"accountId": account.id(), "personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13800000000"}));
                    let changed = Box::pin(services.role().update(administrator.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), definition.id(), role::Update::new(definition.change_id(), serde_json::from_value(json!({"name": "Account managers", "level": 80, "permissions": []})).expect("remove account permission")))).await.expect("permission removed");
                    assert_eq!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation, account.id(), management(&account))).await.err().expect("original operation grants no current authority").code(), Some("permission_denied"));
                    Box::pin(services.role().update(administrator, Uuid::new_v4().to_string().parse().expect("role operation"), changed.id(), role::Update::new(changed.change_id(), serde_json::from_value(json!({"name": "Account managers", "level": 80, "permissions": ["institution.accounts"]})).expect("restore permission")))).await.expect("permission restored");
                    services.drain().await.expect("original work drained"); context.stop().await.expect("original context ended");
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let renewed = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let renewed = services.current().read(&renewed).await.expect("new independent account");
                    assert_ne!(renewed.id(), account.id());
                    assert_ne!(renewed.person_id(), account.person_id());
                    assert_eq!(Box::pin(services.cancellation().cancel_account(manager, operation, account.id(), management(&account))).await.expect("restart and same-mobile registration keep the old result").id(), result.id());
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM identity.cancellations WHERE person_id=$2) AND (SELECT count(*) FROM identity.cancellations)=1", &[&renewed.id(), &renewed.person_id()]).await.expect("replay never deletes a replacement identity").get::<_, bool>(0));
                    assert_cancelled_account(observer.client(), &account, &before, result.id(), result.cancelled_at()).await;
                    services.drain().await.expect("drained"); context.stop().await.expect("context ended"); observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded management lifecycle");
            }
        }));
    }

    #[test]
    fn runtime_sql_requires_one_complete_cancellation_before_removing_access() {
        use tokio_postgres::error::SqlState;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let account = services.current().read(&target).await.expect("target");
                let other = services.current().read(&manager).await.expect("other identity");
                let runtime = database.connections(&database.user).recover().await.expect("runtime SQL");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                const FACTS: &str = "SELECT jsonb_build_array(
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY family,operation_id) FROM identity.completions value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.accounts value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.persons value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.sessions value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.issuances value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.revocations value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.handoffs value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.receipts value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.projections value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM access.holders value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.avatars value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM identity.cancellations value),
                    (SELECT jsonb_agg(to_jsonb(value) ORDER BY id) FROM access.projections value))::text";
                let before: String = observer.client().query_one(FACTS, &[]).await.expect("original graph").get(0);
                for (sql, expected) in [
                    ("DELETE FROM identity.accounts WHERE id = $1", SqlState::INSUFFICIENT_PRIVILEGE),
                    ("DELETE FROM access.holders WHERE id = $1", SqlState::INSUFFICIENT_PRIVILEGE),
                    ("DELETE FROM identity.sessions WHERE account_id = $1", SqlState::CHECK_VIOLATION),
                    ("DELETE FROM identity.handoffs WHERE id IN (SELECT id FROM identity.sessions WHERE account_id = $1)", SqlState::CHECK_VIOLATION),
                    ("DELETE FROM identity.receipts WHERE account_id = $1", SqlState::CHECK_VIOLATION),
                    ("UPDATE identity.avatars SET released_at = clock_timestamp() WHERE account_id = $1", SqlState::CHECK_VIOLATION),
                ] {
                    let error = runtime.client().execute(sql, &[&account.id()]).await.expect_err("current access has no cancellation source");
                    assert_eq!(error.code(), Some(&expected), "{sql}");
                }
                for defect in ["complete", "transaction", "account", "person", "revision", "sequence", "holder", "projection", "session", "receipt", "avatar", "retained_account", "account_projection", "duplicate", "other_account", "completion_missing", "completion_actor", "completion_revision", "completion_target", "completion_action", "completion_time", "completion_expiry"] {
                    runtime.client().batch_execute("BEGIN").await.expect("isolated attempt");
                    let now = context.sample().await.expect("trusted time");
                    let result: Result<(), tokio_postgres::Error> = async {
                        let id = Uuid::new_v4();
                        let operation = Uuid::new_v4();
                        runtime.client().execute("INSERT INTO identity.cancellations (id, account_id, person_id, revision_id, operator_id, mobile, cancelled_at, sequence, operation_id, seal, seal_key_id, seal_version, transaction_id) VALUES ($1, $2, $3, $4, $5, decode(repeat('00', 48), 'hex'), $6, 2, $7, decode(repeat('01',32),'hex'),0,1,CASE WHEN $8 THEN '0'::xid8 ELSE pg_current_xact_id() END)", &[
                            &id,
                            &if defect == "account" { other.id() } else { account.id() },
                            &if defect == "person" { other.person_id() } else { account.person_id() },
                            &if defect == "revision" { other.revision_id() } else { account.revision_id() },
                            &account.revision_id(),
                            &now,
                            &operation,
                            &(defect == "transaction"),
                        ]).await?;
                        if defect == "duplicate" {
                            runtime.client().execute("INSERT INTO identity.cancellations (id, account_id, person_id, revision_id, operator_id, mobile, cancelled_at, sequence, operation_id, seal, seal_key_id, seal_version) SELECT gen_random_uuid(), account_id, person_id, revision_id, operator_id, mobile, cancelled_at, sequence, gen_random_uuid(),seal,seal_key_id,seal_version FROM identity.cancellations WHERE id = $1", &[&id]).await?;
                        }
                        if defect != "sequence" {
                            runtime.client().execute("UPDATE identity.persons SET sequence=2,version=2,changed_at=$2,seal=decode(repeat('01',32),'hex') WHERE id=$1", &[&account.person_id(),&now]).await?;
                        }
                        let projected = if defect == "other_account" { other.id() } else { account.id() };
                        if defect != "projection" {
                            runtime.client().execute(
                                "INSERT INTO access.projections(id,holder_id,person_id,role_id,assignment_id,projected_at,sequence,operation_id,seal,seal_key_id,seal_version)
                                 SELECT $2,id,person_id,role_id,assignment_id,$3,sequence+1,$4,decode(repeat('01',32),'hex'),0,1 FROM access.holders WHERE id=$1",
                                &[&projected, &id, &now, &operation],
                            ).await?;
                        }
                        if defect != "holder" {
                            runtime.client().execute(
                                "UPDATE access.holders SET cancellation_id=$2,changed_at=$3,sequence=sequence+1,version=version+1,seal=decode(repeat('01',32),'hex') WHERE id=$1",
                                &[&projected, &id, &now],
                            ).await?;
                        }
                        if defect != "session" {
                            runtime.client().execute("DELETE FROM identity.handoffs WHERE id IN (SELECT id FROM identity.sessions WHERE account_id = $1)", &[&account.id()]).await?;
                            runtime.client().execute("DELETE FROM identity.revocations WHERE account_id = $1", &[&account.id()]).await?;
                            runtime.client().execute("DELETE FROM identity.issuances WHERE account_id = $1", &[&account.id()]).await?;
                            runtime.client().execute("DELETE FROM identity.sessions WHERE account_id = $1", &[&account.id()]).await?;
                        }
                        if !matches!(defect, "receipt" | "session") {
                            runtime.client().execute("DELETE FROM identity.receipts WHERE account_id = $1", &[&account.id()]).await?;
                        }
                        if defect != "avatar" {
                            runtime.client().execute("UPDATE identity.avatars SET released_at = $2 WHERE account_id = $1", &[&account.id(), &now]).await?;
                        }
                        if defect != "retained_account" {
                            if defect != "account_projection" {
                                runtime.client().execute(
                                    "INSERT INTO identity.projections(id,cause,account_id,person_id,sequence,operation_id,projected_at,seal,seal_key_id,seal_version)
                                     SELECT $2,'cancellation',id,person_id,sequence+1,$4,$3,decode(repeat('01',32),'hex'),0,1 FROM identity.accounts WHERE id=$1",
                                    &[&account.id(), &id, &now, &operation],
                                ).await?;
                            }
                            runtime.client().execute(
                                "UPDATE identity.accounts SET mobile=NULL,id_number=NULL,suspension_id=NULL,cancellation_id=$2,changed_at=$3,sequence=sequence+1,version=version+1,seal=decode(repeat('01',32),'hex') WHERE id=$1",
                                &[&account.id(), &id, &now],
                            ).await?;
                        }
                        if defect != "completion_missing" {
                            runtime.client().execute(
                                "INSERT INTO identity.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version)
                                 VALUES ('cancellation',$1,$2,$3::uuid::text,decode(repeat('00',37),'hex'),'session',$4,$5,$6,$7,decode(repeat('01',32),'hex'),0,1)",
                                &[&operation,
                                  &if defect == "completion_action" { "managed" } else { "self" },
                                  &if defect == "completion_target" { other.id() } else { account.id() },
                                  &if defect == "completion_actor" { other.person_id() } else { account.person_id() },
                                  &if matches!(defect, "completion_actor" | "completion_revision") { other.revision_id() } else { account.revision_id() },
                                  &if defect == "completion_time" { now + chrono::TimeDelta::microseconds(1) } else { now },
                                  &(defect == "completion_expiry").then_some(now + chrono::TimeDelta::days(1))],
                            ).await?;
                        }
                        runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await
                    }.await;
                    if defect == "complete" {
                        result.expect("complete SQL graph satisfies every deferred structural constraint");
                    } else {
                        let Err(error) = result else {
                            panic!("{defect}: partial or misbound cancellation must fail");
                        };
                        assert!(matches!(error.code(), Some(&SqlState::CHECK_VIOLATION | &SqlState::FOREIGN_KEY_VIOLATION | &SqlState::UNIQUE_VIOLATION)), "{defect}: {error}");
                    }
                    runtime.client().batch_execute("ROLLBACK").await.expect("end rejected attempt");
                    assert_eq!(observer.client().query_one(FACTS, &[]).await.expect("rollback observed independently").get::<_, String>(0), before, "{defect}");
                }
                runtime.client().batch_execute("BEGIN").await.expect("orphan completion attempt");
                runtime.client().execute(
                    "INSERT INTO identity.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,seal,seal_key_id,seal_version)
                     VALUES ('cancellation',$1,'self',$2::uuid::text,decode(repeat('00',37),'hex'),'session',$3,$4,$5,decode(repeat('01',32),'hex'),0,1)",
                    &[&Uuid::new_v4(), &account.id(), &account.person_id(), &account.revision_id(), &context.sample().await.expect("time")],
                ).await.expect("well-shaped completion before deferred pairing");
                assert_eq!(runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await
                    .expect_err("completion without a cancellation cannot commit").code(), Some(&SqlState::CHECK_VIOLATION));
                runtime.client().batch_execute("ROLLBACK").await.expect("orphan attempt ended");
                let result = services.cancellation().cancel(target, Uuid::new_v4().to_string().parse().expect("operation"), confirmation(&account)).await.expect("unaltered target still completes normally");
                for sql in ["UPDATE identity.cancellations SET cancelled_at = clock_timestamp() WHERE id = $1", "DELETE FROM identity.cancellations WHERE id = $1"] {
                    assert_eq!(runtime.client().execute(sql, &[&result.id()]).await.expect_err("permanent history is immutable").code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
                }
                for (sql, expected) in [
                    ("UPDATE identity.completions SET created_at=clock_timestamp() WHERE family='cancellation' AND target=$1::uuid::text", SqlState::INSUFFICIENT_PRIVILEGE),
                    ("DELETE FROM identity.completions WHERE family='cancellation' AND target=$1::uuid::text", SqlState::CHECK_VIOLATION),
                ] {
                    assert_eq!(runtime.client().execute(sql, &[&account.id()]).await
                        .expect_err("permanent cancellation completion cannot be mutated").code(), Some(&expected));
                }
                runtime.terminate().await.expect("runtime ended");
                observer.terminate().await.expect("observer ended");
                services.drain().await.expect("drained");
                context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("bounded direct runtime SQL verification");
        }));
    }

    #[test]
    fn management_cancellation_rechecks_rank_permission_and_actor_after_waiting() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::{access::role, identity::authentication};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let administrator = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let account = services.current().read(&target).await.expect("target");
                let (manager, definition) = Box::pin(manager(&database, &services, &mut supplier, &administrator)).await;
                let operator = services.current().read(&manager).await.expect("manager");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let higher = services.current().read(&administrator).await.expect("system administrator");
                assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), higher.id(), management(&higher))).await, Err(Error::Rejected(super::Rejection::Absent))));
                assert!(matches!(Box::pin(services.cancellation().cancel_account(target.clone(), Uuid::new_v4().to_string().parse().expect("operation"), operator.id(), management(&operator))).await, Err(Error::Denied)));
                for level in [80, 100, 2] {
                    let role = Box::pin(services.role().create(administrator.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), serde_json::from_value(json!({"name": format!("Target level {level}"), "level": level, "permissions": []})).expect("target role"))).await.expect("actual target role");
                    let candidate = services.appointment().candidate(&administrator, "13800000000".parse().expect("mobile")).await.expect("assignment candidate");
                    Box::pin(services.appointment().assign(administrator.clone(), Uuid::new_v4().to_string().parse().expect("assignment operation"), appointment::tests::command(&candidate, &role))).await.expect("actual target rank");
                    if level >= 80 {
                        assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), account.id(), management(&account))).await, Err(Error::Rejected(super::Rejection::Absent))));
                    }
                }
                let service = services.role().clone();
                let actor = administrator.clone();
                proxy.arm(Fault::DelayedCommit);
                let changing = tokio::spawn(Box::pin(async move {
                    service.update(actor, Uuid::new_v4().to_string().parse().expect("role operation"), definition.id(), role::Update::new(definition.change_id(), serde_json::from_value(json!({"name": "Account managers", "level": 80, "permissions": []})).expect("no account permission"))).await
                }));
                let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("permission removal awaits commit");
                let service = services.cancellation().clone();
                let actor = manager.clone();
                let account_id = account.id();
                let input = management(&account);
                let cancelling = tokio::spawn(Box::pin(async move {
                    service.cancel_account(actor, Uuid::new_v4().to_string().parse().expect("cancellation operation"), account_id, input).await
                }));
                timeout(Duration::from_secs(3), async { loop {
                    if observer.client().query_one(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock_shared%')",
                        &[&blocker],
                    ).await.expect("command waits for permission scope").get::<_, bool>(0) { break; }
                    tokio::task::yield_now().await;
                }}).await.expect("observed permission waiter");
                proxy.release();
                let changed = changing.await.expect("permission worker").expect("manager permission removed");
                assert!(matches!(cancelling.await.expect("cancellation worker"), Err(Error::Denied)));
                Box::pin(services.role().update(administrator.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), changed.id(), role::Update::new(changed.change_id(), serde_json::from_value(json!({"name": "Account managers", "level": 80, "permissions": ["institution.accounts"]})).expect("restore permission")))).await.expect("manager permission restored");
                Box::pin(services.correction().correct(administrator.clone(), Uuid::new_v4().to_string().parse().expect("correction operation"), serde_json::from_value(json!({"target": {"personId": operator.person_id(), "revisionId": operator.revision_id()}, "name": "Revised Manager", "idNumber": "000000200002290029"})).expect("manager correction"))).await.expect("current manager revision changed");
                let operator = services.current().read(&manager).await.expect("revised manager");
                Box::pin(services.suspension().suspend(administrator.clone(), Uuid::new_v4().to_string().parse().expect("pause operation"), operator.id(), serde_json::from_value(json!({"personId": operator.person_id(), "revisionId": operator.revision_id(), "mobile": "13700000000"})).expect("pause manager"))).await.expect("manager paused through real use case");
                assert!(matches!(Box::pin(services.cancellation().cancel_account(manager, Uuid::new_v4().to_string().parse().expect("operation"), account.id(), management(&account))).await, Err(Error::Unauthenticated)));
                assert!(observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.cancellations) AND (SELECT count(*) FROM identity.accounts)=3 AND EXISTS(SELECT 1 FROM identity.sessions WHERE id=$1)", &[&target.session_id()]).await.expect("all rejected commands preserve target access").get::<_, bool>(0));
                let cancelled = services.cancellation().cancel(administrator.clone(), Uuid::new_v4().to_string().parse().expect("self cancellation"), confirmation(&higher)).await.expect("administrator self cancellation");
                assert!(matches!(Box::pin(services.cancellation().cancel_account(administrator, Uuid::new_v4().to_string().parse().expect("operation"), account.id(), management(&account))).await, Err(Error::Unauthenticated)));
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.cancellations)=1 AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND cancellation_id=$2) AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$3 AND cancellation_id IS NULL) AND EXISTS(SELECT 1 FROM identity.sessions WHERE id=$4)", &[&higher.id(), &cancelled.id(), &account.id(), &target.session_id()]).await.expect("former administrator has no current authority").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await; proxy.stop().await;
            })).await.expect("bounded management qualification");
        }));
    }

    #[test]
    fn qualification_binds_all_confirmation_fields_and_rechecks_current_suspension() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(20), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let account = services.current().read(&target).await.expect("current target");
                let other = services.current().read(&manager).await.expect("other account");
                for field in ["accountId", "personId", "revisionId"] {
                    let mut input = json!({"accountId": account.id(), "personId": account.person_id(), "revisionId": account.revision_id()});
                    input[field] = json!(Uuid::new_v4());
                    assert!(matches!(services.cancellation().cancel(target.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(input).expect("typed mismatched input")).await, Err(Error::Rejected(super::Rejection::TargetChanged))));
                }
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("target confirmation");
                let suspension = serde_json::from_value(json!({"personId": account.person_id(), "revisionId": account.revision_id(), "mobile": "13800000000"})).expect("suspension input");
                Box::pin(services.suspension().suspend(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), candidate.identity().id(), suspension)).await.expect("real suspension invalidates the old actor");
                assert!(matches!(services.cancellation().cancel(target, Uuid::new_v4().to_string().parse().expect("operation"), confirmation(&account)).await, Err(Error::Unauthenticated)));
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.cancellations", &[]).await.expect("rejections write nothing").get::<_, i64>(0), 0);
                services.cancellation().cancel(manager, Uuid::new_v4().to_string().parse().expect("operation"), confirmation(&other)).await.expect("normal administrator can cancel itself without an invented role restriction");
                assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM identity.accounts WHERE id = $1) AND EXISTS (SELECT 1 FROM identity.suspensions WHERE account_id = $1)", &[&account.id()]).await.expect("other account and operator history survive").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.drain().await.expect("drained");
                context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("bounded qualification verification");
        }));
    }

    #[test]
    fn management_cancellation_and_permission_changes_share_the_decisive_scope() {
        use fixture::database_proxy::{Fault, Proxy};
        use tokio_postgres::Client;

        use crate::{access::role, identity::authentication};

        async fn scope_waiting(client: &Client, mode: &str) -> bool {
            // Match the int64 institution scope, not recovery's original-operation lock.
            client.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks waiting JOIN pg_stat_activity activity ON activity.pid=waiting.pid WHERE activity.datname=current_database() AND waiting.locktype='advisory' AND NOT waiting.granted AND waiting.objsubid=1 AND ((waiting.classid::bigint << 32) | waiting.objid::bigint)=-3487418652284273221 AND waiting.mode=$1)", &[&mode]).await.expect("actual institution scope wait").get(0)
        }

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for cancellation_first in [false, true] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await; let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let account = services.current().read(&target).await.expect("target");
                    let (manager, definition) = Box::pin(manager(&database, &services, &mut supplier, &administrator)).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent lock observer");
                    let before = before_cancellation(observer.client(), &account).await;
                    let receipts: Vec<Uuid> = observer.client().query_one("SELECT ARRAY(SELECT id FROM identity.receipts WHERE account_id=$1 ORDER BY id)", &[&account.id()]).await.expect("original registration receipts").get(0);
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("cancellation operation");
                    let service = services.cancellation().clone(); let actor = manager.clone(); let input = management(&account); let account_id = account.id();
                    let cancel = Box::pin(async move { service.cancel_account(actor, operation, account_id, input).await });
                    let service = services.role().clone();
                    let change = Box::pin(async move { service.update(administrator, Uuid::new_v4().to_string().parse().expect("role operation"), definition.id(), role::Update::new(definition.change_id(), serde_json::from_value(json!({"name": "Account managers", "level": 80, "permissions": []})).expect("remove permission"))).await });
                    proxy.arm(Fault::DelayedCommit);
                    let (cancelled, changed) = if cancellation_first {
                        let cancelled = tokio::spawn(cancel);
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("cancellation holds shared permission scope");
                        // Recovery can already be waiting before the role writer even starts.
                        timeout(Duration::from_secs(3), async { loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock%' AND cardinality(pg_blocking_pids(pid))>0)", &[]).await.expect("recovery waits on the original operation").get::<_, bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }}).await.expect("independent recovery reached the still-open cancellation");
                        assert!(!scope_waiting(observer.client(), "ExclusiveLock").await, "original recovery is not a permission change");
                        (cancelled, tokio::spawn(change))
                    } else {
                        let changed = tokio::spawn(change);
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("definition change holds exclusive permission scope");
                        (tokio::spawn(cancel), changed)
                    };
                    let mode = if cancellation_first { "ExclusiveLock" } else { "ShareLock" };
                    timeout(Duration::from_secs(3), async { loop {
                        if scope_waiting(observer.client(), mode).await { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("second writer cannot pass the original permission decision");
                    proxy.release();
                    changed.await.expect("role worker").expect("permission removed");
                    let result = cancelled.await.expect("cancellation worker");
                    if cancellation_first {
                        let original = result.expect("accepted cancellation survives the later loss of permission");
                        let row = observer.client().query_one("SELECT id,cancelled_at FROM identity.cancellations WHERE operation_id=$1", &[&operation]).await.expect("permanent original result before replay");
                        assert_eq!(original.id(),row.get("id"));
                        assert_eq!(original.cancelled_at(),row.get::<_,chrono::DateTime<chrono::Utc>>("cancelled_at"));
                        assert_cancelled_account(observer.client(), &account, &before, original.id(), original.cancelled_at()).await;
                    }
                    else {
                        assert_eq!(result.err().expect("permission lost before cancellation").code(), Some("permission_denied"));
                        let created_at: chrono::DateTime<chrono::Utc> = before.get("created_at");
                        let retained = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$2 AND created_at=$3 AND cancellation_id IS NULL AND (mobile).value IS NOT NULL AND (mobile).hash IS NOT NULL AND id_number IS NOT NULL AND suspension_id IS NULL) AS current, ARRAY(SELECT id FROM identity.sessions WHERE account_id=$1 ORDER BY id) AS sessions, ARRAY(SELECT id FROM identity.receipts WHERE account_id=$1 ORDER BY id) AS receipts", &[&account.id(), &account.person_id(), &created_at]).await.expect("refused cancellation retains the current account");
                        assert!(retained.get::<_, bool>("current"));
                        assert_eq!(retained.get::<_, Vec<Uuid>>("sessions"), before.get::<_, Vec<Uuid>>("sessions"));
                        assert_eq!(retained.get::<_, Vec<Uuid>>("receipts"), receipts);
                    }
                    let fact = observer.client().query_one("SELECT EXISTS(SELECT 1 FROM identity.cancellations WHERE operation_id=$1), EXISTS(SELECT 1 FROM identity.accounts WHERE id=$2 AND cancellation_id IS NULL), EXISTS(SELECT 1 FROM identity.sessions WHERE id=$3)", &[&operation, &account.id(), &manager.session_id()]).await.expect("ordered durable consequences");
                    assert_eq!(fact.get::<_, bool>(0), cancellation_first); assert_eq!(fact.get::<_, bool>(1), !cancellation_first); assert!(fact.get::<_, bool>(2));
                    assert_eq!(Box::pin(services.cancellation().cancel_account(manager, operation, account.id(), management(&account))).await.err().expect("both original read and new write require current permission").code(), Some("permission_denied"));
                    services.drain().await.expect("drained"); context.stop().await.expect("context ended"); observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded permission and cancellation orders");
            }
        }));
    }

    #[test]
    fn self_cancellation_removes_access_atomically_and_retains_history_across_restart() {
        use crate::identity::{
            authentication::session::{
                Receipt,
                tests::{CODE, input as session_input, message},
            },
            revocation::Scope,
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let original = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                services.revocation().revoke(original, Uuid::new_v4().to_string().parse().expect("logout operation"), Scope::Current).await.expect("retained revoked R2 session");
                let code = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                let session_operation = Uuid::new_v4();
                let Receipt::Session(issued) = services.authentication().create_session(session_input(session_operation, code, "13800000000", CODE)).await.expect("new R1 session") else { panic!("existing account"); };
                let target = services.current().authenticate(current::Proof { id: issued.id(), token: issued.token().text().parse().expect("token") }).await.expect("new current session");
                let account = services.current().read(&target).await.expect("real R2 account");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                let records = "SELECT jsonb_build_array((SELECT count(*) FROM identity.authentications), (SELECT count(*) FROM identity.attempts), (SELECT count(*) FROM identity.checks), (SELECT count(*) FROM identity.messages), (SELECT count(*) FROM identity.claims), (SELECT count(*) FROM identity.revisions), (SELECT count(*) FROM access.appointments), (SELECT count(*) FROM access.changes))::text";
                let before: String = observer.client().query_one(records, &[]).await.expect("retained responsibilities before cancellation").get(0);
                let mobile: Vec<u8> = observer.client().query_one("SELECT (mobile).value FROM identity.accounts WHERE id = $1", &[&account.id()]).await.expect("original account envelope").get(0);
                let original_account = before_cancellation(observer.client(), &account).await;
                let operation_id: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                let result = services.cancellation().cancel(target.clone(), operation_id, confirmation(&account)).await.expect("complete self cancellation");
                let facts = observer.client().query_one("SELECT id = $1 AS identified, account_id = $2 AS account, person_id = $3 AS person, operator_id = $4 AND revision_id = $4 AS identities, cancelled_at = $5 AS time, sequence = 2 AS sequence, octet_length(mobile) = 48 AND mobile <> $6 AS sealed FROM identity.cancellations WHERE operation_id = $7", &[&result.id(), &account.id(), &account.person_id(), &account.revision_id(), &result.cancelled_at(), &mobile, &operation_id]).await.expect("independently locate the permanent completion before any replay");
                for column in ["identified", "account", "person", "identities", "time", "sequence", "sealed"] {
                    assert!(facts.get::<_, bool>(column), "{column}");
                }
                assert_cancelled_account(observer.client(), &account, &original_account, result.id(), result.cancelled_at()).await;
                assert!(observer.client().query_one("SELECT sequence=2 AND revision_id=$1 FROM identity.persons WHERE id=$2", &[&account.revision_id(), &account.person_id()]).await.expect("cancellation preserves the confirmed personal lineage").get::<_, bool>(0));
                assert_eq!(observer.client().query_one(records, &[]).await.expect("protocol and permanent history retained").get::<_, String>(0), before);
                assert!(matches!(services.current().read(&target).await, Err(current::Error::Invalid)));
                assert_eq!(services.authentication().create_session(session_input(session_operation, code, "13800000000", CODE)).await.err().expect("original R1 cannot recover deleted access").failure(), crate::identity::authentication::Failure::Verification);
                let repeated = services.cancellation().cancel(target.clone(), operation_id, confirmation(&account)).await;
                assert!(matches!(&repeated, Err(Error::Unauthenticated)), "original cancelled session: {:?}", repeated.as_ref().err());
                assert_eq!(repeated.err().expect("cancelled session rejected").code(), Some("authentication_invalid"));
                services.current().read(&manager).await.expect("other account remains usable");
                assert!(matches!(Box::pin(services.cancellation().cancel_account(manager.clone(), operation_id, account.id(), management(&account))).await, Err(Error::Mismatch)), "a management request cannot adopt a self-cancellation operation");
                services.drain().await.expect("original work drained");
                context.stop().await.expect("original context ended");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let renewed = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let renewed = services.current().read(&renewed).await.expect("same numbers form a new current identity");
                assert_ne!(renewed.id(), account.id());
                assert_ne!(renewed.person_id(), account.person_id());
                let retained = observer.client().query_one("SELECT (SELECT count(*) FROM identity.cancellations WHERE person_id = $1) = 1 AS original, NOT EXISTS (SELECT 1 FROM identity.cancellations WHERE person_id = $2) AS separate, (SELECT sequence = 1 FROM identity.persons WHERE id = $2) AS initial, (SELECT role.level = 1 FROM access.holders holder JOIN access.roles role ON role.id = holder.role_id WHERE holder.id = $3) AS ordinary", &[&account.person_id(), &renewed.person_id(), &renewed.id()]).await.expect("same-number registration retains independent ownership");
                for column in ["original", "separate", "initial", "ordinary"] {
                    assert!(retained.get::<_, bool>(column), "{column}");
                }
                assert_cancelled_account(observer.client(), &account, &original_account, result.id(), result.cancelled_at()).await;
                services.drain().await.expect("new work drained");
                context.stop().await.expect("new context ended");
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                supplier.stop().await;
            })).await.expect("bounded actual cancellation lifecycle");
        }));
    }

    #[test]
    fn cancellation_and_r1_serialize_without_resurrecting_the_old_account() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::{
            self,
            session::{
                Receipt,
                tests::{CODE, input as session_input, message},
            },
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (managed, cancellation_first) in [(false, false), (false, true), (true, false), (true, true)] {
                timeout(Duration::from_secs(20), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let account = services.current().read(&target).await.expect("target");
                    let code = message(&database, services.authentication(), &mut supplier, "13800000000").await;
                    let authentication = services.authentication().clone();
                    let session_operation = Uuid::new_v4();
                    let create = Box::pin(async move { authentication.create_session(session_input(session_operation, code, "13800000000", CODE)).await });
                    let cancellation = services.cancellation().clone();
                    let input = confirmation(&account);
                    let managed_input = management(&account);
                    let account_id = account.id();
                    let cancel = Box::pin(async move {
                        let operation = Uuid::new_v4().to_string().parse().expect("operation");
                        if managed { cancellation.cancel_account(manager, operation, account_id, managed_input).await }
                        else { cancellation.cancel(target, operation, input).await }
                    });
                    let observer = database.connections(&database.user).recover().await.expect("independent lock observer");
                    let before = before_cancellation(observer.client(), &account).await;
                    proxy.arm(Fault::DelayedCommit);
                    let (cancelled, created, blocker) = if cancellation_first {
                        let cancelled = tokio::spawn(cancel);
                        let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("cancellation holds its account before commit");
                        (cancelled, tokio::spawn(create), blocker)
                    } else {
                        let created = tokio::spawn(create);
                        let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("R1 holds its account before commit");
                        (tokio::spawn(cancel), created, blocker)
                    };
                    let query = if cancellation_first { "SELECT identity.lock_account($1)" } else { "SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE" };
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity waiter JOIN pg_stat_activity original ON original.pid = ANY(pg_blocking_pids(waiter.pid)) WHERE waiter.datname = current_database() AND original.datname = current_database() AND original.pid=$1 AND waiter.wait_event_type = 'Lock' AND waiter.query=$2)", &[&blocker, &query]).await.expect("actual account coordination").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("competing writer waits on the account");
                    proxy.release();
                    let cancelled = cancelled.await.expect("cancellation worker").expect("cancellation committed");
                    let result = created.await.expect("R1 worker");
                    if cancellation_first {
                        assert!(matches!(result, Ok(Receipt::Registration(_))), "new authentication may offer registration but cannot restore the deleted account");
                    } else {
                        match result {
                            Ok(Receipt::Session(_)) => {},
                            Err(error) => assert_eq!(error.failure(), authentication::Failure::Verification),
                            _ => panic!("R1 accepted before cancellation belongs to the original account"),
                        }
                        assert_eq!(services.authentication().create_session(session_input(session_operation, code, "13800000000", CODE)).await.err().expect("late delivery cannot recover deleted session").failure(), authentication::Failure::Verification);
                    }
                    assert_cancelled_account(observer.client(), &account, &before, cancelled.id(), cancelled.cancelled_at()).await;
                    assert!(observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM identity.sessions WHERE account_id = $1) AND NOT EXISTS (SELECT 1 FROM identity.receipts WHERE account_id = $1) AND EXISTS (SELECT 1 FROM identity.cancellations WHERE account_id = $1 AND id=$3) AND EXISTS (SELECT 1 FROM identity.authentications WHERE operation_id = $2)", &[&account.id(), &session_operation, &cancelled.id()]).await.expect("old access gone, protocol conclusion retained").get::<_, bool>(0));
                    services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded opposing R1 and cancellation order");
            }
        }));
    }

    #[test]
    fn cancellation_and_correction_keep_confirmation_and_permanent_lineage() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (managed, cancellation_first) in [(false, false), (false, true), (true, false), (true, true)] {
                timeout(Duration::from_secs(20), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let account = services.current().read(&target).await.expect("target");
                    let operator = manager.clone();
                    let input = serde_json::from_value(json!({"target": {"personId": account.person_id(), "revisionId": account.revision_id()}, "name": "Corrected Target", "idNumber": "00000020000229007X"})).expect("controlled correction input");
                    let service = services.correction().clone();
                    let correct = Box::pin(async move { service.correct(manager, Uuid::new_v4().to_string().parse().expect("operation"), input).await });
                    let service = services.cancellation().clone();
                    let input = confirmation(&account);
                    let managed_input = management(&account);
                    let account_id = account.id();
                    let cancel = Box::pin(async move {
                        let operation = Uuid::new_v4().to_string().parse().expect("operation");
                        if managed { service.cancel_account(operator, operation, account_id, managed_input).await }
                        else { service.cancel(target, operation, input).await }
                    });
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let before = before_cancellation(observer.client(), &account).await;
                    let receipts: Vec<Uuid> = observer.client().query_one("SELECT ARRAY(SELECT id FROM identity.receipts WHERE account_id=$1 ORDER BY id)", &[&account.id()]).await.expect("original registration receipts").get(0);
                    proxy.arm(Fault::DelayedCommit);
                    let (cancelled, corrected, blocker) = if cancellation_first {
                        let cancelled = tokio::spawn(cancel);
                        let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("cancellation pending");
                        (cancelled, tokio::spawn(correct), blocker)
                    } else {
                        let corrected = tokio::spawn(correct);
                        let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("correction pending");
                        (tokio::spawn(cancel), corrected, blocker)
                    };
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity waiter JOIN pg_stat_activity original ON original.pid = ANY(pg_blocking_pids(waiter.pid)) WHERE waiter.datname = current_database() AND original.datname = current_database() AND original.pid=$1 AND waiter.wait_event_type = 'Lock' AND waiter.query='SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE')", &[&blocker]).await.expect("shared account lock before person lock").get::<_, bool>(0) { break; }
                        tokio::task::yield_now().await;
                    }}).await.expect("competing mutation waits without crossing lock order");
                    proxy.release();
                    let correction = corrected.await.expect("correction worker").expect("lawful permanent correction");
                    let result = cancelled.await.expect("cancellation worker");
                    if cancellation_first {
                        let cancelled = result.expect("original account cancelled");
                        assert_cancelled_account(observer.client(), &account, &before, cancelled.id(), cancelled.cancelled_at()).await;
                        assert!(observer.client().query_one("SELECT cancellation.revision_id = $2 AND cancellation.sequence = 2 AND person.revision_id = $3 AND person.sequence = 3 AND correction.sequence = 3 FROM identity.cancellations cancellation JOIN identity.persons person ON person.id = cancellation.person_id JOIN identity.corrections correction ON correction.id = person.revision_id WHERE cancellation.account_id = $1", &[&account.id(), &account.revision_id(), &correction.correction().id()]).await.expect("later correction preserves cancellation's frozen original identity").get::<_, bool>(0));
                        let now = context.sample().await.expect("time");
                        let accounts = crate::history::inspect::<crate::identity::account::state::State>(
                            observer.client(), services.app().keys(), now,
                        ).await.expect("authenticated permanent accounts");
                        let sessions = crate::history::inspect::<crate::identity::session::state::State>(
                            observer.client(), services.app().keys(), now,
                        ).await.expect("authenticated retained sessions");
                        crate::identity::cancellation::verify(observer.client(), services.app().keys(), &accounts, &sessions, now).await.expect("permanent original remains reconstructible");
                    } else {
                        assert!(matches!(result, Err(Error::Rejected(super::Rejection::TargetChanged))));
                        let created_at: chrono::DateTime<chrono::Utc> = before.get("created_at");
                        assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM identity.accounts WHERE id=$1 AND person_id=$2 AND created_at=$4 AND cancellation_id IS NULL AND (mobile).value IS NOT NULL AND (mobile).hash IS NOT NULL AND id_number IS NOT NULL AND suspension_id IS NULL) AND NOT EXISTS (SELECT 1 FROM identity.cancellations WHERE account_id = $1) AND (SELECT revision_id = $3 AND sequence = 2 FROM identity.persons WHERE id = $2)", &[&account.id(), &account.person_id(), &correction.correction().id(), &created_at]).await.expect("stale confirmation never cancels the corrected account").get::<_, bool>(0));
                        let retained = observer.client().query_one("SELECT ARRAY(SELECT id FROM identity.sessions WHERE account_id=$1 ORDER BY id) AS sessions, ARRAY(SELECT id FROM identity.receipts WHERE account_id=$1 ORDER BY id) AS receipts", &[&account.id()]).await.expect("refused cancellation preserves credentials");
                        assert_eq!(retained.get::<_, Vec<Uuid>>("sessions"), before.get::<_, Vec<Uuid>>("sessions"));
                        assert_eq!(retained.get::<_, Vec<Uuid>>("receipts"), receipts);
                    }
                    services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded opposing correction and cancellation order");
            }
        }));
    }

    #[test]
    fn internal_replay_waits_for_holder_and_every_retained_grant() {
        use crate::access::{Entry, grant, role::Kind};

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let actor = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                let account = services.current().read(&actor).await.expect("original account");
                let ended = Box::pin(grant::tests::admit(&services, &manager, Entry::Institution, "13800000000", Kind::Default)).await;
                services.grant().terminate(actor.clone(), Uuid::new_v4().to_string().parse().expect("termination operation"), ended.id()).await.expect("earlier ended grant");
                let active = Box::pin(grant::tests::admit(&services, &manager, Entry::Institution, "13800000000", Kind::Default)).await;
                let operation = Uuid::new_v4().to_string().parse().expect("cancellation operation");
                let original = services.cancellation().cancel(actor.clone(), operation, confirmation(&account)).await.expect("complete self cancellation");
                let blocker = database.connections(&database.user).recover().await.expect("object coordinator");
                let observer = database.connections(&database.user).recover().await.expect("wait observer");
                let backend: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocking backend").get(0);
                for object in ["holder", "earlier grant", "cancelled grant"] {
                    blocker.client().batch_execute("BEGIN").await.expect("hold object");
                    if object == "holder" {
                        blocker.client().query_one("SELECT id FROM access.holders WHERE id=$1 FOR UPDATE", &[&account.id()]).await.expect("holder locked");
                    } else {
                        let id = if object == "earlier grant" { ended.id() } else { active.id() };
                        blocker.client().query_one("SELECT id FROM access.grants WHERE id=$1 FOR UPDATE", &[&id]).await.expect("retained grant locked");
                    }
                    let service = services.cancellation().clone();
                    let actor = actor.clone();
                    let input = confirmation(&account);
                    let replay = tokio::spawn(async move { replay_self(&service, actor, operation, input).await });
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one(
                            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database()
                                AND $1=ANY(pg_blocking_pids(pid)) AND wait_event_type='Lock'
                                AND query LIKE $2)",
                            &[&backend, &if object == "holder" { "SELECT id FROM access.holders%FOR UPDATE" } else { "SELECT id FROM access.grants%FOR UPDATE" }],
                        ).await.expect("exact object coordination").get::<_, bool>(0) { break; }
                        assert!(!replay.is_finished(), "{object}: replay must await the object");
                        tokio::task::yield_now().await;
                    }}).await.expect("replay obtains real conflicting object locks");
                    blocker.client().batch_execute("ROLLBACK").await.expect("release object");
                    let recovered = replay.await.expect("replay worker").expect("verified original");
                    assert_eq!(recovered.id(), original.id());
                    assert_eq!(recovered.cancelled_at(), original.cancelled_at());
                }
                blocker.terminate().await.expect("blocker ended");
                observer.terminate().await.expect("observer ended");
                services.drain().await.expect("drained");
                context.stop().await.expect("context stopped");
                supplier.stop().await;
            })).await.expect("bounded object-lock verification");
        }));
    }

    #[test]
    fn authenticated_orphan_completions_fail_closed_for_both_actions() {
        use crate::{
            database::Barrier,
            identity::{authentication, cancellation},
            kernel::{Authorization, completion},
            runtime::Context,
            startup::{Services, Settings},
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for managed in [false, true] {
                timeout(Duration::from_secs(20), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let administrator = actor(&database, &services, &mut supplier).await;
                    let target = Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let account = services.current().read(&target).await.expect("target identity");
                    let actor = if managed { administrator } else { target.clone() };
                    let operator = services.current().read(&actor).await.expect("original operator");
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let now = context.sample().await.expect("time");
                    let authority = Authorization { person_id: operator.person_id(), revision_id: operator.revision_id(), now };
                    let mobile = "13800000000".parse().expect("mobile");
                    let request = cancellation::request::<super::Rejection>(
                        account.id(), account.person_id(), account.revision_id(),
                        managed.then_some(&mobile), &authority,
                    ).expect("canonical original request");
                    let owner = database.connections("postgres").recover().await.expect("isolated fixture owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("isolated orphan injection");
                    completion::insert::<super::Rejection>(
                        owner.client(), services.app().keys(), cancellation::COMPLETIONS,
                        operation, &request, now, None,
                    ).await.expect("authentic completion with no permanent cancellation");
                    owner.client().batch_execute("COMMIT").await.expect("orphan committed");
                    let retained = completion::original::<super::Rejection>(
                        owner.client(), services.app().keys(), cancellation::COMPLETIONS, operation,
                    ).await.expect("seal authenticates").expect("retained orphan");
                    assert_eq!(retained.person_id(), operator.person_id());
                    assert_eq!(retained.revision_id(), operator.revision_id());
                    let mut expected = json!({"accountId": account.id(), "personId": account.person_id(), "revisionId": account.revision_id()});
                    if managed { expected["mobile"] = json!("13800000000"); }
                    assert_eq!(retained.input::<serde_json::Value, super::Rejection>(services.app().keys()).expect("authenticated input"), expected);
                    let result = if managed {
                        services.cancellation().cancel_account(actor, operation, account.id(), management(&account)).await
                    } else {
                        services.cancellation().cancel(actor, operation, confirmation(&account)).await
                    };
                    assert!(result.err().expect("orphan cannot be delivered").is_corrupt());
                    assert!(owner.client().query_one(
                        "SELECT NOT EXISTS(SELECT 1 FROM identity.cancellations)
                            AND EXISTS(SELECT 1 FROM identity.accounts WHERE id=$1 AND cancellation_id IS NULL)
                            AND EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$1)
                            AND EXISTS(SELECT 1 FROM access.holders WHERE id=$1 AND cancellation_id IS NULL)
                            AND EXISTS(SELECT 1 FROM identity.avatars WHERE account_id=$1 AND released_at IS NULL)",
                        &[&account.id()],
                    ).await.expect("corrupt original creates no cancellation consequences").get::<_, bool>(0));
                    assert!(services.cancellation().cancel(
                        target, Uuid::new_v4().to_string().parse().expect("new operation"), confirmation(&account),
                    ).await.err().expect("admission closed").is_closed(), "corruption closes command admission");
                    services.drain().await.expect("drained");
                    context.stop().await.expect("original context stopped");
                    let restarted = Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Services::start(
                        Barrier::begin(restarted.clone()).await.expect("startup barrier"),
                        authentication::inventory(authentication::keys()),
                        Settings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings"),
                        supplier.client(), authentication::unused_client().await,
                    ).await;
                    assert!(result.is_err(), "an authenticated orphan must prevent startup");
                    restarted.stop().await.expect("failed startup stopped");
                    owner.terminate().await.expect("owner stopped");
                    supplier.stop().await;
                })).await.expect("bounded orphan verification");
            }
        }));
    }

    #[test]
    fn damaged_cancellation_is_neither_a_known_success_nor_a_valid_startup() {
        use crate::{
            database::Barrier,
            identity::{authentication, cancellation},
            runtime::Context,
            startup::{Services, Settings},
        };

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for defect in ["mobile_owner", "envelope", "future", "sequence", "avatar", "operator", "completion_missing", "completion_actor", "completion_target", "completion_action", "completion_time", "completion_input", "completion_sealed_input", "completion_sealed_action"] {
                timeout(Duration::from_secs(15), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let actor = actor(&database, &services, &mut supplier).await;
                    let account = services.current().read(&actor).await.expect("current account");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let mobile: Vec<u8> = observer.client().query_one("SELECT (mobile).value FROM identity.accounts WHERE id = $1", &[&account.id()]).await.expect("different authenticated row owner").get(0);
                    let operation: operation::Id = Uuid::new_v4().to_string().parse().expect("operation");
                    let result = services.cancellation().cancel(actor.clone(), operation, confirmation(&account)).await.expect("real completed cancellation");
                    let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role = replica").await.expect("isolated corruption");
                    match defect {
                        "completion_sealed_input" | "completion_sealed_action" => {
                            use crate::kernel::{Authorization, completion};
                            let authority = Authorization { person_id: account.person_id(), revision_id: account.revision_id(), now: result.cancelled_at() };
                            let mut input = json!({"accountId": account.id(), "personId": account.person_id(), "revisionId": account.revision_id()});
                            let action = if defect == "completion_sealed_action" {
                                input["mobile"] = json!("13900000000");
                                "managed"
                            } else {
                                input["revisionId"] = json!(Uuid::new_v4());
                                "self"
                            };
                            let request = completion::Request::new::<super::Rejection>(action, account.id().to_string(), &input, &authority).expect("well-shaped contradictory original");
                            owner.client().execute("DELETE FROM identity.completions WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("replace only the isolated completion");
                            completion::insert::<super::Rejection>(owner.client(), services.app().keys(), cancellation::COMPLETIONS, operation, &request, result.cancelled_at(), None).await.expect("validly sealed contradictory completion");
                            completion::original::<super::Rejection>(owner.client(), services.app().keys(), cancellation::COMPLETIONS, operation).await.expect("contradiction is not an invalid seal").expect("original");
                        }
                        "completion_missing" => { owner.client().execute("DELETE FROM identity.completions WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("retained fact without completion"); }
                        "completion_actor" => { owner.client().execute("UPDATE identity.completions SET person_id=gen_random_uuid() WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("wrong accepted actor"); }
                        "completion_target" => { owner.client().execute("UPDATE identity.completions SET target=gen_random_uuid()::text WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("wrong fixed target"); }
                        "completion_action" => { owner.client().execute("UPDATE identity.completions SET action='managed' WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("wrong action"); }
                        "completion_time" => { owner.client().execute("UPDATE identity.completions SET created_at=created_at+interval '1 microsecond' WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("wrong completion time"); }
                        "completion_input" => { owner.client().execute("UPDATE identity.completions SET input=decode(repeat('ff',48),'hex') WHERE family='cancellation' AND operation_id=$1", &[&operation]).await.expect("damaged confirmation envelope"); }
                        "operator" => { owner.client().execute("UPDATE identity.cancellations SET operator_id = gen_random_uuid() WHERE id = $1", &[&result.id()]).await.expect("missing permanent operator must not hide the original row"); }
                        "mobile_owner" => { owner.client().execute("UPDATE identity.cancellations SET mobile = $2 WHERE id = $1", &[&result.id(), &mobile]).await.expect("same plaintext under the wrong row and field binding"); }
                        "envelope" => { owner.client().execute("UPDATE identity.cancellations SET mobile = decode(repeat('ff', 48), 'hex') WHERE id = $1", &[&result.id()]).await.expect("unparseable envelope with SQL-valid length"); }
                        "future" => { owner.client().execute("UPDATE identity.cancellations SET cancelled_at = cancelled_at + interval '1 day' WHERE id = $1", &[&result.id()]).await.expect("future completion"); }
                        "sequence" => { owner.client().execute("UPDATE identity.persons SET sequence = sequence + 1 WHERE id = $1", &[&account.person_id()]).await.expect("incomplete permanent lineage"); }
                        "avatar" => { owner.client().execute("UPDATE identity.avatars SET released_at = NULL WHERE account_id = $1", &[&account.id()]).await.expect("missing release consequence"); }
                        _ => unreachable!("selected defects"),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let before: String = observer.client().query_one("SELECT to_jsonb(cancellation)::text FROM identity.cancellations cancellation WHERE id = $1", &[&result.id()]).await.expect("independent damaged original").get(0);
                    let replay = replay_self(services.cancellation(), actor.clone(), operation, confirmation(&account)).await;
                    if defect == "sequence" {
                        assert_eq!(replay.expect("original completion does not depend on today's Person state").id(), result.id());
                    } else {
                        let error = replay.err().expect("recovery must fully authenticate its completion");
                        assert!(error.is_corrupt(), "{defect}: {error:?}");
                    }
                    let now = context.sample().await.expect("time");
                    let verified: Result<(), cancellation::Error> = async {
                        crate::identity::person::verify(observer.client(), services.app().keys(), now).await?;
                        let accounts = crate::history::inspect::<crate::identity::account::state::State>(
                            observer.client(), services.app().keys(), now,
                        ).await?;
                        let sessions = crate::history::inspect::<crate::identity::session::state::State>(
                            observer.client(), services.app().keys(), now,
                        ).await?;
                        cancellation::verify(observer.client(), services.app().keys(), &accounts, &sessions, now).await
                    }.await;
                    assert!(verified.is_err());
                    assert_eq!(observer.client().query_one("SELECT to_jsonb(cancellation)::text FROM identity.cancellations cancellation WHERE id = $1", &[&result.id()]).await.expect("failed recovery does not repair history").get::<_, String>(0), before);
                    services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                    let restarted = Context::new(database.connections(&database.user)).await.expect("new runtime context");
                    let result = Services::start(
                        Barrier::begin(restarted.clone()).await.expect("startup barrier"), authentication::inventory(authentication::keys()),
                        Settings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings"),
                        supplier.client(), authentication::unused_client().await,
                    ).await;
                    assert!(result.is_err(), "{defect}: corrupted cancellation cannot become ready");
                    restarted.stop().await.expect("failed startup releases resources");
                    observer.terminate().await.expect("observer ended"); owner.terminate().await.expect("owner ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded corruption verification");
            }
        }));
    }
}
