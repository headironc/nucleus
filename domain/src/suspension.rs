//! Account suspension and resumption use the common command and snapshot boundaries.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{self, appointment, role},
    history::{self, Read, Record as _, Verified, View},
    identity::{
        account::{self, record::Event, transition},
        current::{self, Actor},
        keys::Keys,
        mobile::Mobile,
        operation,
        person::{self, revision},
        session,
        suspension::{self, Receipt, Resumption, Suspension},
    },
    kernel::{self, App, Authorization, Command, Query, completion, operation::Family, tx::Tx},
    pagination::Pagination,
};

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    TargetChanged,
    Conflict,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent => kernel::Status::NotFound,
            Self::TargetChanged | Self::Conflict => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::Conflict => "state_conflict",
        }
    }
}

impl From<Rejection> for Error {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = suspension::Input)]
pub struct Input {
    person_id: person::Id,
    revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = suspension::Resume)]
pub struct Resume {
    person_id: person::Id,
    revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    mobile: Mobile,
    suspension_id: suspension::Id,
}

fn accepts_suspension(action: &str, target: &str) -> bool {
    action == "suspend"
        && target
            .parse::<account::Id>()
            .is_ok_and(|id| id.to_string() == target)
}

fn accepts_resumption(action: &str, target: &str) -> bool {
    action == "resume"
        && target
            .parse::<account::Id>()
            .is_ok_and(|id| id.to_string() == target)
}

pub(crate) const SUSPENSIONS: completion::Template =
    completion::Template::personal(Family::Suspension, accepts_suspension);
pub(crate) const RESUMPTIONS: completion::Template =
    completion::Template::personal(Family::Resumption, accepts_resumption);

#[derive(Serialize)]
struct Confirmation<'a> {
    person_id: person::Id,
    revision_id: revision::Id,
    mobile: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    suspension_id: Option<suspension::Id>,
}

/// Both actions share target confirmation, locking and history, with separate originals.
trait Action: Send + Sync + 'static {
    const COMPLETIONS: completion::Template;
    const RESUMING: bool;
    const NAME: &'static str;
    fn confirmation(&self) -> Confirmation<'_>;
    fn mobile(&self) -> &Mobile;
}

impl Action for Input {
    const COMPLETIONS: completion::Template = SUSPENSIONS;
    const RESUMING: bool = false;
    const NAME: &'static str = "suspend";
    fn confirmation(&self) -> Confirmation<'_> {
        Confirmation {
            person_id: self.person_id,
            revision_id: self.revision_id,
            mobile: self.mobile.as_ref(),
            suspension_id: None,
        }
    }
    fn mobile(&self) -> &Mobile {
        &self.mobile
    }
}

impl Action for Resume {
    const COMPLETIONS: completion::Template = RESUMPTIONS;
    const RESUMING: bool = true;
    const NAME: &'static str = "resume";
    fn confirmation(&self) -> Confirmation<'_> {
        Confirmation {
            person_id: self.person_id,
            revision_id: self.revision_id,
            mobile: self.mobile.as_ref(),
            suspension_id: Some(self.suspension_id),
        }
    }
    fn mobile(&self) -> &Mobile {
        &self.mobile
    }
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
    accounts: account::Reader,
    revisions: revision::Reader,
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service, keys: Arc<Keys>) -> Self {
        Self {
            roles: role::Reader::new(app.clone()),
            accounts: account::Reader::new(app.clone(), Arc::clone(&keys)),
            revisions: revision::Reader::new(app.clone()),
            app,
            current,
        }
    }

    pub async fn suspend(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        account_id: account::Id,
        input: Input,
    ) -> Result<Suspension, Error> {
        match self
            .app
            .command(
                operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    operation_id,
                    account_id,
                    input,
                    id: suspension::Id::new(),
                },
            )
            .await?
        {
            Receipt::Suspension(value) => Ok(value),
            Receipt::Resumption(_) => Err(Error::corrupt("suspension returns its declared event")),
        }
    }

    pub async fn resume(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        account_id: account::Id,
        input: Resume,
    ) -> Result<Resumption, Error> {
        match self
            .app
            .command(
                operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    operation_id,
                    account_id,
                    id: input.suspension_id,
                    input,
                },
            )
            .await?
        {
            Receipt::Resumption(value) => Ok(value),
            Receipt::Suspension(_) => Err(Error::corrupt("resumption returns its declared event")),
        }
    }

    pub async fn history(
        &self,
        actor: &Actor,
        account_id: account::Id,
        pagination: Pagination,
    ) -> Result<suspension::history::List, Error> {
        self.app
            .query(&History {
                service: self,
                actor,
                account_id,
                pagination,
            })
            .await
    }
}

struct Write<A> {
    service: Service,
    actor: Actor,
    operation_id: operation::Id,
    account_id: account::Id,
    input: A,
    id: suspension::Id,
}

struct Target {
    account: Verified<account::state::State>,
    subject: Option<account::Subject>,
    sessions: Vec<Verified<session::state::State>>,
}

struct Facts {
    target: Option<Target>,
    operator_id: revision::Id,
}

struct Selected {
    change: transition::Revocation,
    now: DateTime<Utc>,
}

impl<A: Action> Command for Write<A> {
    const COMPLETIONS: completion::Template = A::COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Receipt;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        completion::Request::new(
            A::NAME,
            self.account_id.to_string(),
            &self.input.confirmation(),
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        tx.shared(b"access/scope", b"institution").await?;
        self.actor
            .lock_accounts(tx.client(), self.account_id)
            .await?;
        if A::RESUMING {
            self.actor.lock_session(tx.client()).await?;
        } else {
            session::state::lock_management(tx.client(), self.actor.session_id(), self.account_id)
                .await?;
        }
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject = subject.ok_or(Error::corrupt("account management declares its session"))?;
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

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let client = tx.client();
        let now = authority.now;
        let manager =
            appointment::find_by_account(client, &self.service.roles, self.actor.account_id(), now)
                .await?
                .ok_or(Error::corrupt(
                    "an authorized manager retains its institutional role",
                ))?;
        let target =
            appointment::find_by_account(client, &self.service.roles, self.account_id, now).await?;
        let mut facts = Facts {
            target: None,
            operator_id: authority.revision_id,
        };
        // Hidden targets need no identity decryption or session collection.
        if target.is_none_or(|target| {
            target.person_id() == authority.person_id
                || target.role().level() >= manager.role().level()
        }) {
            return Ok(facts);
        }
        let account = self
            .service
            .accounts
            .state(client, self.account_id, now)
            .await?
            .ok_or(Error::corrupt(
                "a current institutional target retains its account",
            ))?;
        let subject = self
            .service
            .accounts
            .find_by_id(client, self.account_id, self.input.mobile(), now)
            .await?;
        let sessions = if A::RESUMING {
            Vec::new()
        } else {
            session::state::for_account(
                client,
                self.service.app.keys(),
                self.service.app.anchors(),
                Read::Locked,
                self.account_id,
                now,
            )
            .await?
        };
        facts.target = Some(Target {
            account,
            subject,
            sessions,
        });
        Ok(facts)
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Self::Change, Error> {
        let target = facts.target.ok_or(Rejection::Absent)?;
        let input = self.input.confirmation();
        let subject = target.subject.ok_or(Rejection::TargetChanged)?;
        if subject.person_id() != input.person_id || subject.revision_id() != input.revision_id {
            return Err(Rejection::TargetChanged.into());
        }
        let current = target.account.current().ok_or(Error::corrupt(
            "a current target is not a cancelled account",
        ))?;
        if if A::RESUMING {
            current.suspension_id() != Some(self.id)
        } else {
            current.suspension_id().is_some()
        } {
            return Err(Rejection::Conflict.into());
        }
        let confirmation = transition::confirmation(
            input.revision_id,
            facts.operator_id,
            self.input.mobile().clone(),
        );
        let change = if A::RESUMING {
            transition::Revocation {
                account: transition::resume(
                    &target.account,
                    self.id,
                    confirmation,
                    self.operation_id,
                    now,
                )?,
                sessions: Vec::new(),
            }
        } else {
            transition::suspend(
                &target.account,
                self.id,
                confirmation,
                &target.sessions,
                self.operation_id,
                now,
            )?
        };
        Ok(Selected { change, now })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Self::Change,
        pending: &mut Option<history::Pending>,
    ) -> Result<Receipt, Error> {
        let app = &self.service.app;
        let change = selected.change;
        *pending = Some(
            change
                .account
                .write(tx.client(), app.keys(), app.anchors())
                .await?,
        );
        let group = pending
            .as_mut()
            .ok_or(Error::corrupt("account management retains its change"))?;
        for (id, change) in change.sessions {
            group.extend(change.write(tx.client(), app.keys(), app.anchors()).await?)?;
            session::handoff::remove(tx.client(), id).await?;
        }
        let record = account::record::management(
            tx.client(),
            app.keys(),
            A::RESUMING,
            self.operation_id,
            selected.now,
        )
        .await?
        .ok_or(Error::corrupt(
            "account management wrote its permanent record",
        ))?;
        Ok(Receipt::read(&record)?)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Receipt, Error> {
        let app = &self.service.app;
        let record = account::record::management(
            tx.client(),
            app.keys(),
            A::RESUMING,
            self.operation_id,
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "account management retains its permanent result",
        ))?;
        let (confirmation, _) = confirmation(&record, A::RESUMING)?;
        let input = self.input.confirmation();
        if record.account_id() != self.account_id
            || confirmation.person_id != input.person_id
            || confirmation.revision_id != input.revision_id
            || confirmation.mobile != input.mobile
            || confirmation.suspension_id != input.suspension_id
        {
            return Err(Error::corrupt(
                "account completion and original input agree",
            ));
        }
        let operator_id = match record.event() {
            Event::Suspension { confirmation, .. } | Event::Resumption { confirmation, .. } => {
                confirmation.operator_id()
            }
            _ => return Err(Error::corrupt("account management has its declared event")),
        };
        let operator = self
            .service
            .revisions
            .fetch_identity(tx.client(), operator_id)
            .await?;
        if operator.person_id() != request.person_id() {
            return Err(Error::corrupt(
                "account management retains its original operator",
            ));
        }
        app.anchors().confirm::<account::state::State>(&record)?;
        Ok(Receipt::read(&record)?)
    }
}

fn confirmation(
    record: &account::record::Record,
    resuming: bool,
) -> Result<(Confirmation<'_>, revision::Id), Error> {
    let (confirmation, suspension_id) = match (resuming, record.event()) {
        (false, Event::Suspension { confirmation, .. }) => (confirmation, None),
        (true, Event::Resumption { id, confirmation }) => (confirmation, Some(*id)),
        _ => return Err(Error::corrupt("account management has its declared event")),
    };
    Ok((
        Confirmation {
            person_id: record.person_id(),
            revision_id: confirmation.revision_id(),
            mobile: confirmation.mobile().as_ref(),
            suspension_id,
        },
        confirmation.operator_id(),
    ))
}

struct History<'a> {
    service: &'a Service,
    actor: &'a Actor,
    account_id: account::Id,
    pagination: Pagination,
}

impl Query for History<'_> {
    type Authorized = (Verified<account::state::State>, DateTime<Utc>);
    type Output = suspension::history::List;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Self::Authorized, Error> {
        let roles = self.service.roles.at(view);
        let current = self.service.current.at(view);
        let (manager, person_id, _, now) = access::authorize(
            client,
            &roles,
            &current,
            self.actor,
            role::Code::InstitutionAccounts,
        )
        .await?;
        let target = appointment::find_by_account(client, &roles, self.account_id, now)
            .await?
            .ok_or(Rejection::Absent)?;
        if target.person_id() == person_id || target.role().level() >= manager.level() {
            return Err(Rejection::Absent.into());
        }
        let account = self
            .service
            .accounts
            .at(view)
            .state(client, self.account_id, now)
            .await?
            .filter(|account| account.current().is_some())
            .ok_or(Rejection::Absent)?;
        Ok((account, now))
    }

    async fn read(
        &self,
        client: &Client,
        (account, now): Self::Authorized,
        _: View,
    ) -> Result<Self::Output, Error> {
        Ok(suspension::history::fetch_list(
            client,
            self.service.app.keys(),
            &self.service.revisions,
            &account,
            self.pagination,
            now,
        )
        .await?)
    }
}

pub(crate) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    for (resuming, template, action) in [
        (false, SUSPENSIONS, "suspend"),
        (true, RESUMPTIONS, "resume"),
    ] {
        let mut completions: HashMap<_, _> =
            completion::fetch_all::<Rejection>(client, app.keys(), template)
                .await?
                .into_iter()
                .map(|record| (record.operation_id(), record))
                .collect();
        for record in account::record::management_records(client, app.keys(), resuming, now).await?
        {
            let completion = completions
                .remove(&record.operation_id())
                .ok_or(Error::corrupt(
                    "every account management result retains its completion",
                ))?;
            let (input, operator_id) = confirmation(&record, resuming)?;
            if completion.created_at() != record.happened_at()
                || completion.revision_id() != operator_id
            {
                return Err(Error::corrupt(
                    "account completion retains its original operator and time",
                ));
            }
            let authority = Authorization {
                person_id: completion.person_id(),
                revision_id: operator_id,
                now,
            };
            let request = completion::Request::new(
                action,
                record.account_id().to_string(),
                &input,
                &authority,
            )?;
            match completion.matches(template, app.keys(), &request) {
                Err(Error::Mismatch) => {
                    return Err(Error::corrupt(
                        "account completion binds its original input",
                    ));
                }
                result => result?,
            }
        }
        if !completions.is_empty() {
            return Err(Error::corrupt(
                "every account management completion retains its result",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::time::{Instant, timeout, timeout_at};
    use uuid::Uuid;

    use crate::{
        access::{
            appointment,
            role::tests::{actor, start},
        },
        account::Candidate,
        identity::{
            authentication::{self, Database, Supplier, server},
            current, suspension,
        },
        kernel::{FaultKind, Stage},
        pagination::Pagination,
        runtime::work::Failure as _,
        suspension::{Error, Input, Rejection, Resume},
    };

    fn input(candidate: &Candidate) -> Input {
        serde_json::from_value(json!({"personId": candidate.identity().person_id(), "revisionId": candidate.identity().revision_id(), "mobile":"13800000000"})).expect("typed confirmation")
    }

    fn resume(candidate: &Candidate, suspension_id: suspension::Id) -> Resume {
        serde_json::from_value(json!({"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800000000","suspensionId":suspension_id})).expect("typed resumption")
    }

    async fn inspect(
        client: &tokio_postgres::Client,
        service: &super::Service,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), crate::history::Error> {
        crate::history::inspect::<crate::identity::account::state::State>(
            client,
            service.app.keys(),
            now,
        )
        .await?;
        crate::history::inspect::<crate::identity::session::state::State>(
            client,
            service.app.keys(),
            now,
        )
        .await?;
        Ok(())
    }

    #[test]
    fn accepted_account_states_recover_after_session_revocation() {
        use fixture::database_proxy::Proxy;

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            let mut failures = Vec::new();
            for resuming in [false, true] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    Box::pin(appointment::tests::register(&database, &services, &mut supplier)).await;
                    let candidate = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("confirmed account");
                    let account_id = candidate.identity().id();
                    let paused = if resuming {
                        let paused = Box::pin(services.suspension().suspend(manager.clone(), Uuid::new_v4().to_string().parse().expect("setup operation"), account_id, input(&candidate))).await.expect("actual suspension");
                        Some(paused.id())
                    } else { None };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let command = || async {
                        if let Some(id) = paused {
                            Box::pin(services.suspension().resume(manager.clone(), operation, account_id, resume(&candidate, id))).await.map(suspension::Receipt::Resumption)
                        } else {
                            Box::pin(services.suspension().suspend(manager.clone(), operation, account_id, input(&candidate))).await.map(suspension::Receipt::Suspension)
                        }
                    };
                    let result = Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, command())).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent facts");
                    let row = observer.client().query_one("WITH original AS (SELECT id,account_id,person_id,sequence,false AS resumed FROM identity.suspensions WHERE operation_id=$1 UNION ALL SELECT id,account_id,person_id,sequence,true AS resumed FROM identity.resumptions WHERE operation_id=$1) SELECT original.id,original.sequence,original.resumed,account.sequence AS account_sequence,person.sequence AS person_sequence FROM original JOIN identity.accounts account ON account.id=original.account_id JOIN identity.persons person ON person.id=original.person_id WHERE original.person_id=$2", &[&operation,&candidate.identity().person_id()]).await.expect("one permanent original state");
                    assert_eq!(row.get::<_,bool>("resumed"), resuming);
                    let sequence = if resuming { 3 } else { 2 };
                    assert_eq!((row.get::<_,i64>("sequence"),row.get::<_,i64>("account_sequence")), (sequence,sequence));
                    assert_eq!(row.get::<_,i64>("person_sequence"), 1);
                    match result {
                        Ok(suspension::Receipt::Suspension(value)) => { assert!(!resuming); assert_eq!(value.id(),row.get("id")); }
                        Ok(suspension::Receipt::Resumption(value)) => { assert!(resuming); assert_eq!(value.id(),row.get("id")); }
                        Err(error) => failures.push(format!("resuming={resuming}: {error:?}")),
                    }
                    let Err(error) = Box::pin(command()).await else { panic!("new request must be refused"); };
                    assert!(matches!(error, Error::Unauthenticated));
                    assert_eq!(observer.client().query_one("SELECT sequence FROM identity.accounts WHERE id=$1", &[&account_id]).await.expect("no replay writes").get::<_,i64>(0), sequence);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("state history survives restart");
                    services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("account state recovery after session loss deadline");
            }
            assert!(failures.is_empty(), "accepted account states lost their results: {failures:?}");
        }));
    }

    async fn reader(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
        manager: &current::Actor,
    ) -> current::Actor {
        use crate::identity::authentication::session::{
            Receipt,
            tests::{CODE, input, message},
        };
        let message_id =
            message(database, services.authentication(), supplier, "13700000000").await;
        let Receipt::Registration(credential) =
            Box::pin(services.authentication().create_session(input(
                Uuid::new_v4(),
                message_id,
                "13700000000",
                CODE,
            )))
            .await
            .expect("reader credential")
        else {
            panic!("new registration");
        };
        let session = services
            .registration()
            .register(crate::registration::Input::new(
                Uuid::new_v4().to_string().parse().expect("operation"),
                credential.id(),
                credential.token().text().parse().expect("proof"),
                "History Reader".parse().expect("name"),
                "000000200002290029".parse().expect("number"),
            ))
            .await
            .expect("reader account");
        let actor = services
            .current()
            .authenticate(current::Proof {
                id: session.id(),
                token: session.token().text().parse().expect("proof"),
            })
            .await
            .expect("reader session");
        let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), serde_json::from_value(json!({"name":"History reader","level":500,"permissions":["institution.accounts"]})).expect("role"))).await.expect("reader role");
        let candidate = services
            .appointment()
            .candidate(manager, "13700000000".parse().expect("mobile"))
            .await
            .expect("reader confirmation");
        let input=serde_json::from_value(json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13700000000","assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("reader confirmation");
        Box::pin(services.appointment().assign(
            manager.clone(),
            Uuid::new_v4().to_string().parse().expect("operation"),
            input,
        ))
        .await
        .expect("reader assigned");
        actor
    }

    #[test]
    fn history_discloses_full_pages_and_fixed_identities_after_current_graph_changes() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            let preparation = Instant::now() + Duration::from_secs(60);
            let database = timeout_at(preparation, Database::initialized(Arc::clone(&server))).await.expect("history database setup");
            let mut supplier = timeout_at(preparation, Supplier::new()).await.expect("history supplier setup");
            let proxy = timeout_at(preparation, Proxy::start(&server)).await.expect("history proxy setup");
            let (context, services) = timeout_at(preparation, start(authentication::proxied(&database, &proxy), &supplier)).await.expect("history service setup");
            let administrator = timeout_at(preparation, actor(&database, &services, &mut supplier)).await.expect("history administrator setup");
            let manager = timeout_at(preparation, Box::pin(crate::correction::tests::register_person(&database, &services, &mut supplier, "13600000000", "History Operator", "000000200002290037"))).await.expect("history manager setup");
            let selected=Box::pin(services.role().create(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),serde_json::from_value(json!({"name":"History managers","level":700,"permissions":["institution.accounts","institution.corrections"]})).expect("role"))).await.expect("manager role");
            let confirmed=services.appointment().candidate(&administrator,"13600000000".parse().expect("mobile")).await.expect("manager confirmation");
            Box::pin(services.appointment().assign(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),serde_json::from_value(json!({"target":{"accountId":confirmed.identity().id(),"personId":confirmed.identity().person_id(),"revisionId":confirmed.identity().revision_id(),"mobile":"13600000000","assignmentId":confirmed.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("manager assignment input"))).await.expect("real delegated manager");
            timeout_at(preparation, Box::pin(appointment::tests::register(&database, &services, &mut supplier))).await.expect("history target setup");
            let mut candidate = timeout_at(preparation, services.account().candidate(&manager, "13800000000".parse().expect("mobile"))).await.expect("history candidate deadline").expect("target");
            let account_id = candidate.identity().id();
            assert_eq!(timeout_at(preparation, services.suspension().history(&manager, account_id, Pagination::new(1,20).expect("page"))).await.expect("empty history deadline").expect("empty history").count(),0);
            let reader = timeout_at(preparation, Box::pin(reader(&database, &services, &mut supplier, &administrator))).await.expect("history reader setup");
            let observer = timeout_at(preparation, database.connections(&database.user).recover()).await.expect("history observer deadline").expect("independent SQL");
            let original_operator = timeout_at(preparation, observer.client().query_one("SELECT account.id,person.id AS person_id,person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1", &[&manager.session_id()])).await.expect("original operator deadline").expect("original operator");
            let mut ids = Vec::new();
            for index in 0..100 {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let paused = Box::pin(services.suspension().suspend(manager.clone(), Uuid::new_v4().to_string().parse().expect("operation"), account_id, input(&candidate))).await.expect("real pause");
                    if index==0 {
                        let confirmation=services.correction().candidate_by_person(&manager,candidate.identity().person_id()).await.expect("paused target may be corrected");
                        Box::pin(services.correction().correct(manager.clone(),Uuid::new_v4().to_string().parse().expect("correction operation"),serde_json::from_value(json!({"target":confirmation.target(),"name":"Target Revised","idNumber":"00000020000229007X"})).expect("correction input"))).await.expect("real correction between pause and resume");
                        candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("new confirmed identity");
                    }
                    Box::pin(services.suspension().resume(if index==0 {reader.clone()} else {manager.clone()}, Uuid::new_v4().to_string().parse().expect("operation"), account_id, resume(&candidate, paused.id()))).await.expect("real resume");
                    ids.push(paused.id());
                })).await.unwrap_or_else(|error| panic!("history preparation pair {index}: {error}"));
            }
            timeout(Duration::from_secs(60), Box::pin(async {
                ids.reverse();
                let rows = observer.client().query("SELECT suspension.id,suspension.sequence,suspension.revision_id,suspension.operator_id,resumption.sequence AS resumed_sequence,resumption.revision_id AS resumed_revision,resumption.operator_id AS resumed_operator,suspension.operation_id,resumption.operation_id AS resumed_operation FROM identity.suspensions suspension LEFT JOIN identity.resumptions resumption USING(id) WHERE suspension.person_id=$1 ORDER BY suspension.sequence DESC,suspension.id DESC", &[&candidate.identity().person_id()]).await.expect("all committed facts before list");
                assert_eq!(rows.len(),100);
                assert_eq!(rows.iter().map(|row|row.get::<_,suspension::Id>("id")).collect::<Vec<_>>(),ids);
                proxy.arm(Fault::DelayedStateQueries);
                let started = std::time::Instant::now();
                let result = services.suspension().history(&reader,account_id,Pagination::new(1,100).expect("maximum page")).await;
                let delayed = proxy.delayed_queries();
                eprintln!("full Service history observation: {:?}; queries delayed by 8 ms: {delayed}",started.elapsed());
                assert!(delayed > 0,"actual history queries were delayed");
                let page = result.expect("complete real authenticated page within existing deadline despite query latency");
                assert_eq!(page.count(),100);assert_eq!(page.suspensions().len(),100);
                for (record,row) in page.suspensions().iter().zip(&rows) {
                    assert_eq!(record.id(),row.get("id"));assert_eq!(record.sequence(),row.get::<_,i64>("sequence"));assert_eq!(record.operation_id(),row.get("operation_id"));
                    assert_eq!(record.subject().revision_id(),row.get("revision_id"));assert_eq!(record.subject().name(),"T*");
                    assert_eq!(record.operator().revision_id(),row.get("operator_id"));
                    let resumed=record.resumption().expect("actual recovery");assert_eq!(resumed.sequence(),row.get::<_,i64>("resumed_sequence"));assert_eq!(resumed.operation_id(),row.get("resumed_operation"));assert_eq!(resumed.subject().revision_id(),row.get("resumed_revision"));assert_eq!(resumed.operator().revision_id(),row.get("resumed_operator"));
                }
                let first=page.suspensions().last().expect("first pause");let first_resume=first.resumption().expect("first resume");assert_ne!(first.subject().revision_id(),first_resume.subject().revision_id());assert_ne!(first.operator().person_id(),first_resume.operator().person_id());assert_eq!(first_resume.operator().name(),"H*");
                let confirmation=services.correction().candidate_by_person(&manager,candidate.identity().person_id()).await.expect("later target version");
                Box::pin(services.correction().correct(manager.clone(),Uuid::new_v4().to_string().parse().expect("correction operation"),serde_json::from_value(json!({"target":confirmation.target(),"name":"Changed Target","idNumber":"00000020000229007X"})).expect("correction input"))).await.expect("real later subject correction");
                let operator_account:crate::identity::account::Id=original_operator.get("id");
                let confirmation=services.correction().candidate_by_person(&administrator,original_operator.get("person_id")).await.expect("real operator correction confirmation");
                Box::pin(services.correction().correct(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),serde_json::from_value(json!({"target":confirmation.target(),"name":"Changed Operator","idNumber":"000000200002290037"})).expect("correction input"))).await.expect("real later operator correction");
                let confirmation=services.account().candidate(&administrator,"13600000000".parse().expect("mobile")).await.expect("operator cancellation confirmation");
                Box::pin(services.cancellation().cancel_account(administrator,Uuid::new_v4().to_string().parse().expect("operation"),operator_account,serde_json::from_value(json!({"personId":confirmation.identity().person_id(),"revisionId":confirmation.identity().revision_id(),"mobile":"13600000000"})).expect("cancellation input"))).await.expect("real operator cancellation");
                let row=observer.client().query_one("SELECT count(*) AS count,EXISTS(SELECT 1 FROM identity.accounts WHERE id=$2 AND cancellation_id IS NOT NULL) AS cancelled,NOT EXISTS(SELECT 1 FROM identity.sessions WHERE account_id=$2) AS cleaned FROM identity.suspensions WHERE person_id=$1",&[&candidate.identity().person_id(),&operator_account]).await.expect("independent later graph before reread");
                assert_eq!(row.get::<_,i64>("count"),100);assert!(row.get::<_,bool>("cancelled"));assert!(row.get::<_,bool>("cleaned"));
                let later=services.suspension().history(&reader,account_id,Pagination::new(1,100).expect("page")).await.expect("fixed identities survive real operator cancellation");
                assert_eq!(later.suspensions().iter().map(|record|record.id()).collect::<Vec<_>>(),ids);
                for (record,old) in later.suspensions().iter().zip(page.suspensions()) {
                    for (identity,original) in [(record.subject(),old.subject()),(record.operator(),old.operator()),(record.resumption().expect("resume").subject(),old.resumption().expect("resume").subject()),(record.resumption().expect("resume").operator(),old.resumption().expect("resume").operator())] {
                        assert_eq!(identity.revision_id(),original.revision_id());assert_eq!(identity.name(),original.name());
                    }
                }
                let second=services.suspension().history(&reader,account_id,Pagination::new(2,20).expect("page")).await.expect("second page");assert_eq!(second.count(),100);assert_eq!(second.suspensions()[0].id(),ids[20]);
                let empty=services.suspension().history(&reader,account_id,Pagination::new(6,20).expect("page")).await.expect("out of range");assert_eq!(empty.count(),100);assert!(empty.suspensions().is_empty());
                observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
            })).await.expect("bounded full-page lifecycle");
        }));
    }

    #[test]
    fn history_keeps_authority_target_and_page_in_one_snapshot_and_deadline() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::access::role;
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                let (context,services)=crate::access::role::tests::start_snapshot(authentication::proxied(&database,&proxy),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;
                Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                let reader=Box::pin(reader(&database,&services,&mut supplier,&manager)).await;
                let reader_role=services.role().read_current(&reader).await.expect("reader role");
                let target_role=Box::pin(services.role().create(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),serde_json::from_value(json!({"name":"History target","level":20,"permissions":[]})).expect("definition"))).await.expect("target role");
                let target=services.appointment().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                Box::pin(services.appointment().assign(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),appointment::tests::command(&target,&target_role))).await.expect("target rank");
                let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("confirmed");let account_id=candidate.identity().id();
                let paused=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,input(&candidate))).await.expect("pause");
                let observer=database.connections(&database.user).recover().await.expect("independent SQL");
                let page=||Pagination::new(1,100).expect("page");
                proxy.arm(Fault::StalledRoleCount);
                let service=services.suspension().clone();let actor=reader.clone();let task=tokio::spawn(async move{service.history(&actor,account_id,page()).await});
                timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("count follows current authority and target in snapshot");
                Box::pin(services.suspension().resume(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,resume(&candidate,paused.id()))).await.expect("concurrent recovery");
                let changed=Box::pin(services.role().update(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),reader_role.id(),role::Update::new(reader_role.change_id(),serde_json::from_value(json!({"name":"History reader","level":500,"permissions":[]})).expect("removed precise permission")))).await.expect("current permission changed");
                Box::pin(services.role().update(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),target_role.id(),role::Update::new(target_role.change_id(),serde_json::from_value(json!({"name":"History target","level":600,"permissions":[]})).expect("higher target")))).await.expect("current target changed");
                proxy.release();let original=timeout(Duration::from_secs(2),task).await.expect("snapshot finishes").expect("reader").expect("original qualified snapshot");assert_eq!(original.count(),1);assert_eq!(original.suspensions()[0].id(),paused.id());assert!(original.suspensions()[0].resumption().is_none());
                // A new pause requires a recovery committed in an earlier transaction.
                // Exercise that transition across a second observed read snapshot.
                proxy.arm(Fault::StalledRolePage);
                let service=services.suspension().clone();let actor=manager.clone();let task=tokio::spawn(async move{service.history(&actor,account_id,page()).await});
                timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("page follows count in recovered snapshot");
                let next=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,input(&candidate))).await.expect("concurrent new pause");
                proxy.release();let recovered=task.await.expect("reader").expect("recovered snapshot before next pause");
                assert_eq!(recovered.count(),1);assert_eq!(recovered.suspensions()[0].id(),paused.id());assert!(recovered.suspensions()[0].resumption().is_some());
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.suspensions WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("two committed pauses").get::<_,i64>(0),2);
                let latest=services.suspension().history(&manager,account_id,page()).await.expect("fresh authorized snapshot");
                assert_eq!(latest.count(),2);assert_eq!(latest.suspensions()[0].id(),next.id());assert!(latest.suspensions()[1].resumption().is_some());
                assert!(matches!(services.suspension().history(&reader,account_id,page()).await,Err(Error::Denied)));
                proxy.arm(Fault::RejectedRollback);
                let error=services.suspension().history(&reader,account_id,page()).await.err().expect("failed denial cleanup");
                assert!(matches!(error,Error::Denied),"{error:?}");
                timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual rejected read rollback intercepted");
                Box::pin(services.role().update(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),changed.id(),role::Update::new(changed.change_id(),serde_json::from_value(json!({"name":"History reader","level":500,"permissions":["institution.accounts"]})).expect("restored permission")))).await.expect("permission restored");
                assert!(matches!(services.suspension().history(&reader,account_id,page()).await,Err(Error::Rejected(Rejection::Absent))),"new snapshot hides higher target");
                assert_eq!(services.suspension().history(&manager,account_id,page()).await.expect("current manager page").suspensions()[0].id(),next.id());
                // Deadline and finalization checks retain the production read budget.
                services.close(); services.drain().await.expect("snapshot services drained"); context.stop().await.expect("snapshot context stopped");
                let (context, services) = start(crate::identity::authentication::proxied(&database, &proxy), &supplier).await;
                for cancelled in [true,false] {
                    proxy.arm(Fault::StalledRoleCount);let service=services.suspension().clone();let actor=manager.clone();let started=tokio::time::Instant::now();let task=tokio::spawn(async move{service.history(&actor,account_id,page()).await});
                    let pid=timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual count stopped");
                    if cancelled {task.abort();assert!(task.await.err().expect("cancelled").is_cancelled());}
                    else {tokio::time::sleep(Duration::from_millis(1500)).await;proxy.arm(Fault::StalledRolePage);proxy.release();timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("page reuses original deadline");let error=timeout(Duration::from_millis(2200),task).await.expect("one read deadline").expect("reader").err().expect("incomplete page");assert!(matches!(error,Error::Unavailable(ref fault) if matches!(fault.kind(),FaultKind::Timeout(_))),"{error:?}");assert!(started.elapsed()<Duration::from_millis(3900));}
                    proxy.release();timeout(Duration::from_secs(3),async{loop{if !observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)",&[&pid]).await.expect("actual backend lifetime").get::<_,bool>(0){break;}tokio::task::yield_now().await;}}).await.expect("connection discarded and backend ended");
                    assert!(services.suspension().app.registry().ready().is_ok());
                }
                {
                    proxy.arm(Fault::StalledRoleCount);let service=services.suspension().clone();let actor=manager.clone();let task=tokio::spawn(async move{service.history(&actor,account_id,page()).await});timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("authorized read before final fault");
                    proxy.arm(Fault::LostReadCommit);proxy.release();
                    // A frontend error can precede the proxy's interception notification.
                    // Consume this boundary before the next iteration arms another fault.
                    timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual completion fault intercepted");
                    let error=timeout(Duration::from_secs(4),task).await.expect("completed read cleanup").expect("reader").err().expect("failed cleanup withholds page");
                    assert!(matches!(error,Error::Unavailable(ref fault) if fault.stage()==Stage::Read && matches!(fault.kind(),FaultKind::Query(_))),"{error:?}");assert!(services.suspension().app.registry().ready().is_ok());
                    proxy.release();
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.suspensions WHERE person_id=$1",&[&candidate.identity().person_id()]).await.expect("read failures never write").get::<_,i64>(0),2);
                observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");let _=context.stop().await;assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
            })).await.expect("history snapshot and failure budget");
        }));
    }

    #[test]
    fn history_rejects_damaged_pages_without_hiding_missing_recovery_or_identity() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["subject_name","subject_number","operator_name","operator_number","pause_mobile","resume_mobile","pause_subject","pause_operator","resume_subject","resume_operator","query"] {
                timeout(Duration::from_secs(15),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                    let (context,services)=start(database.connections(&database.user),&supplier).await;let manager=actor(&database,&services,&mut supplier).await;
                    let target=Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                    let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");let account_id=candidate.identity().id();
                    let first=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,input(&candidate))).await.expect("first pause");
                    Box::pin(services.suspension().resume(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,resume(&candidate,first.id()))).await.expect("recovery");
                    Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,input(&candidate))).await.expect("latest pause");
                    let owner=database.connections("postgres").recover().await.expect("isolated owner");let observer=database.connections(&database.user).recover().await.expect("independent runtime observer");
                    let original=observer.client().query_one("SELECT suspension.revision_id,suspension.operator_id,operator.person_id AS operator_person FROM identity.suspensions suspension JOIN identity.revisions operator ON operator.id=suspension.operator_id WHERE suspension.id=$1",&[&first.id()]).await.expect("historical identities before damage");
                    let operator_id:crate::identity::person::revision::Id=original.get("operator_id");
                    // Keep current authorization healthy while corrupting only an old operator version.
                    crate::correction::tests::rename_administrator(
                        &services, original.get("operator_person"), operator_id,
                        "Healthy Manager", "000000200002290010",
                    ).await;
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated permanent damage");
                    match case {
                        "subject_name"|"operator_name" => {let revision=if case=="subject_name"{candidate.identity().revision_id()}else{operator_id};owner.client().execute("UPDATE identity.revisions SET name=id_number WHERE id=$1",&[&revision]).await.expect("name field AAD damage");}
                        "subject_number"|"operator_number" => {let revision=if case=="subject_number"{candidate.identity().revision_id()}else{operator_id};owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=$1",&[&revision]).await.expect("identity-number ciphertext damage");}
                        "pause_mobile" => {owner.client().execute("UPDATE identity.suspensions SET mobile=set_byte(mobile,0,0) WHERE id=$1",&[&first.id()]).await.expect("parent envelope malformed");}
                        "resume_mobile" => {owner.client().execute("UPDATE identity.resumptions SET mobile=(SELECT mobile FROM identity.suspensions WHERE id=$1) WHERE id=$1",&[&first.id()]).await.expect("different same-id field AAD");}
                        "pause_subject" => {owner.client().execute("UPDATE identity.suspensions SET revision_id=gen_random_uuid() WHERE id=$1",&[&first.id()]).await.expect("missing parent subject");}
                        "pause_operator" => {owner.client().execute("UPDATE identity.suspensions SET operator_id=gen_random_uuid() WHERE id=$1",&[&first.id()]).await.expect("missing parent operator");}
                        "resume_subject" => {owner.client().execute("UPDATE identity.resumptions SET revision_id=gen_random_uuid() WHERE id=$1",&[&first.id()]).await.expect("present recovery with missing subject");}
                        "resume_operator" => {owner.client().execute("UPDATE identity.resumptions SET operator_id=gen_random_uuid() WHERE id=$1",&[&first.id()]).await.expect("present recovery with missing operator");}
                        "query" => {owner.client().batch_execute(&format!("REVOKE SELECT ON identity.resumptions FROM {}",database.user)).await.expect("ordinary statement failure");}
                        _=>unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let before=owner.client().query_one("SELECT jsonb_build_object('suspensions',(SELECT jsonb_agg(to_jsonb(suspension) ORDER BY sequence) FROM identity.suspensions suspension WHERE person_id=$1),'resumptions',(SELECT jsonb_agg(to_jsonb(resumption) ORDER BY sequence) FROM identity.resumptions resumption WHERE person_id=$1))::text",&[&candidate.identity().person_id()]).await.expect("independent exact damaged graph before list").get::<_,String>(0);
                    let page=||Pagination::new(1,20).expect("page");
                    if case!="query" {let invalid=services.suspension().history(&target,account_id,page()).await.err().expect("actor invalid before historical damage");assert!(matches!(invalid,Error::Unauthenticated),"{case}: {invalid:?}");assert!(services.suspension().app.registry().ready().is_ok());}
                    let error=services.suspension().history(&manager,account_id,page()).await.err().expect("one damaged record withholds whole page");assert!(matches!(error,Error::Unavailable(_) | Error::Corrupt(_)),"{case}: {error:?}");assert_eq!(error.unreliable(),case!="query","{case}: {error:?}");assert_eq!(services.suspension().app.registry().ready().is_err(),case!="query");
                    match case {
                        "subject_name"|"operator_name"|"subject_number"|"operator_number" => assert!(matches!(error,Error::Corrupt(ref fault) if matches!(fault.kind(),FaultKind::Suspension(suspension::Error::Revision(crate::identity::person::revision::ReadError::Revision(_)))))),
                        "pause_mobile"|"resume_mobile"|"pause_subject"|"pause_operator"|"resume_subject"|"resume_operator"=>assert!(matches!(error,Error::Corrupt(ref fault) if matches!(fault.kind(),FaultKind::History(_) | FaultKind::Suspension(suspension::Error::History(_)))),"{case}: {error:?}"),
                        "query"=>assert!(matches!(error,Error::Unavailable(ref fault) if matches!(fault.kind(),FaultKind::History(crate::history::Error::Query(cause)) | FaultKind::Suspension(suspension::Error::History(crate::history::Error::Query(cause))) if cause.code()==Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE))),"{error:?}"),
                        _=>assert!(matches!(error,Error::Corrupt(ref fault) if matches!(fault.kind(),FaultKind::History(_) | FaultKind::Suspension(suspension::Error::History(_) | suspension::Error::Facts{..}))),"{case}: {error:?}"),
                    }
                    assert_eq!(owner.client().query_one("SELECT jsonb_build_object('suspensions',(SELECT jsonb_agg(to_jsonb(suspension) ORDER BY sequence) FROM identity.suspensions suspension WHERE person_id=$1),'resumptions',(SELECT jsonb_agg(to_jsonb(resumption) ORDER BY sequence) FROM identity.resumptions resumption WHERE person_id=$1))::text",&[&candidate.identity().person_id()]).await.expect("list made no writes").get::<_,String>(0),before);
                    if case=="query" {owner.client().batch_execute(&format!("GRANT SELECT ON identity.resumptions TO {}",database.user)).await.expect("restore fixture grants");assert_eq!(services.suspension().history(&manager,account_id,page()).await.expect("ordinary failure does not poison admission").count(),2);}
                    observer.terminate().await.expect("observer ended");owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded historical damage");
            }
        }));
    }

    async fn raw_management_completion(
        client: &tokio_postgres::Client,
        resuming: bool,
        actor: &current::Actor,
        account_id: crate::identity::account::Id,
        operation: Uuid,
        at: chrono::DateTime<chrono::Utc>,
    ) {
        let (family, action) = if resuming {
            ("resumption", "resume")
        } else {
            ("suspension", "suspend")
        };
        assert_eq!(client.execute(
            "INSERT INTO identity.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version)
                SELECT $1,$2,$3,$4::uuid::text,decode(repeat('00',37),'hex'),'session',person.id,person.revision_id,$5,NULL,
                    decode(repeat('00',32),'hex'),(SELECT min(id) FROM security.keys),1
                FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id
                JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$6",
            &[&family,&operation,&action,&account_id,&at,&actor.session_id()],
        ).await.expect("complete dummy envelope used only in a rolled-back structural control"),1);
    }

    #[test]
    fn runtime_resumption_requires_completed_sequence_before_new_credentials() {
        use tokio_postgres::error::SqlState;

        use crate::identity::authentication::session::tests::{
            CODE, input as session_input, message,
        };

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;
                Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                let paused=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),candidate.identity().id(),input(&candidate))).await.expect("real suspension");
                let verification=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                assert!(matches!(Box::pin(services.authentication().create_session(session_input(Uuid::new_v4(),verification,"13800000000",CODE))).await,Err(authentication::Error::Suspended)));
                let runtime=database.connections(&database.user).recover().await.expect("runtime writer");
                assert_eq!(runtime.client().query_one("SELECT current_user::text",&[]).await.expect("runtime identity").get::<_,String>(0),database.user.to_string());
                let operator=runtime.client().query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1",&[&manager.session_id()]).await.expect("operator").get::<_,crate::identity::person::revision::Id>(0);
                let before=runtime.client().query_one("SELECT row_to_json(account)::text FROM identity.accounts account WHERE id=$1",&[&candidate.identity().id()]).await.expect("original account").get::<_,String>(0);
                for (defect, expected, constraint) in [
                    ("control",None,""),
                    ("missing_completion",Some(SqlState::FOREIGN_KEY_VIOLATION),"resumptions_family_operation_id_fkey"),
                    ("parent",Some(SqlState::FOREIGN_KEY_VIOLATION),"resumptions_id_person_id_fkey"),
                    ("revision",Some(SqlState::FOREIGN_KEY_VIOLATION),"resumptions_revision_id_person_id_fkey"),
                    ("sequence",Some(SqlState::CHECK_VIOLATION),"resumptions_sequence_check"),
                    ("missing_record",Some(SqlState::CHECK_VIOLATION),"accounts_records_check"),
                    ("gap",Some(SqlState::CHECK_VIOLATION),"accounts_records_check"),
                    ("version",Some(SqlState::CHECK_VIOLATION),"accounts_records_check"),
                ] {
                    runtime.client().batch_execute("BEGIN").await.expect("structural attempt");
                    let now=context.sample().await.expect("trusted time");let operation=Uuid::new_v4();
                    let result:Result<(),tokio_postgres::Error>=async {
                        if defect!="missing_completion" {raw_management_completion(runtime.client(),true,&manager,candidate.identity().id(),operation,now).await;}
                        if defect!="missing_record" {
                            runtime.client().execute(
                                "INSERT INTO identity.resumptions(id,account_id,person_id,revision_id,operator_id,mobile,resumed_at,sequence,operation_id,seal,seal_key_id,seal_version)
                                    SELECT CASE WHEN $6='parent' THEN gen_random_uuid() ELSE suspension.id END,suspension.account_id,suspension.person_id,
                                        CASE WHEN $6='revision' THEN $2 ELSE suspension.revision_id END,$2,suspension.mobile,$3,
                                        CASE WHEN $6='sequence' THEN 1 WHEN $6='gap' THEN account.sequence+2 ELSE account.sequence+1 END,
                                        $4,decode(repeat('00',32),'hex'),suspension.seal_key_id,1
                                    FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id WHERE suspension.id=$1 AND account.id=$5",
                                &[&paused.id(),&operator,&now,&operation,&candidate.identity().id(),&defect],
                            ).await?;
                        }
                        runtime.client().execute(
                            "UPDATE identity.accounts SET suspension_id=NULL,changed_at=$2,sequence=sequence+CASE WHEN $3='gap' THEN 2 ELSE 1 END,
                                version=version+CASE WHEN $3='version' THEN 2 ELSE 1 END,seal=decode(repeat('00',32),'hex') WHERE id=$1",
                            &[&candidate.identity().id(),&now,&defect],
                        ).await?;
                        runtime.client().batch_execute("SET CONSTRAINTS identity.accounts_records_check IMMEDIATE; SET CONSTRAINTS ALL IMMEDIATE").await
                    }.await;
                    match expected {
                        None=>result.expect("all mandatory fields and paired records satisfy the structural control"),
                        Some(expected)=>{
                            let error=result.expect_err("the named structural defect is rejected");
                            assert_eq!(error.code(),Some(&expected),"{defect}: {error}");
                            assert_eq!(error.as_db_error().expect("server constraint").constraint(),Some(constraint),"{defect}: {error}");
                            eprintln!("resumption structure case={defect}, code={expected:?}, constraint={constraint}");
                        }
                    }
                    runtime.client().batch_execute("ROLLBACK").await.expect("dummy seals and envelopes never commit");
                    assert_eq!(runtime.client().query_one("SELECT row_to_json(account)::text FROM identity.accounts account WHERE id=$1",&[&candidate.identity().id()]).await.expect("unchanged account").get::<_,String>(0),before,"{defect}");
                    assert!(runtime.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.resumptions) AND NOT EXISTS(SELECT 1 FROM identity.completions WHERE family='resumption')",&[]).await.expect("no partial original").get::<_,bool>(0),"{defect}");
                }
                Box::pin(services.suspension().resume(manager,Uuid::new_v4().to_string().parse().expect("operation"),candidate.identity().id(),resume(&candidate,paused.id()))).await.expect("real confirmation succeeds after rolled-back SQL");
                for sql in ["UPDATE identity.resumptions SET resumed_at=resumed_at+interval '1 second'","DELETE FROM identity.resumptions"] {
                    assert_eq!(runtime.client().batch_execute(sql).await.expect_err("runtime cannot mutate permanent recovery records").code(),Some(&SqlState::INSUFFICIENT_PRIVILEGE));
                }
                let row=runtime.client().query_one("SELECT account.sequence,person.sequence AS person_sequence FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.id=$1",&[&candidate.identity().id()]).await.expect("real recovery counters");
                assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)),(3,1));
                runtime.terminate().await.expect("runtime ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded direct SQL protection");
        }));
    }

    #[test]
    fn resumption_damage_blocks_original_delivery_and_startup() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (case, original_damaged, sql) in [
                ("mobile_format", true, "UPDATE identity.resumptions SET mobile=set_byte(mobile,0,2)"),
                ("mobile_ciphertext", true, "UPDATE identity.resumptions SET mobile=set_byte(mobile,47,get_byte(mobile,47)#1)"),
                ("mobile_binding", true, "UPDATE identity.resumptions resumption SET mobile=suspension.mobile FROM identity.suspensions suspension WHERE suspension.id=resumption.id"),
                ("operator", true, "UPDATE identity.resumptions SET operator_id=gen_random_uuid()"),
                ("revision", true, "UPDATE identity.resumptions SET revision_id=operator_id"),
                ("sequence", true, "UPDATE identity.resumptions SET sequence=sequence+2"),
                ("time", true, "UPDATE identity.resumptions SET resumed_at=resumed_at-interval '1 hour'"),
                ("session_revocation", false, "DELETE FROM identity.revocations WHERE mode='suspension'"),
                ("prior_suspension", false, "DELETE FROM identity.suspensions"),
            ] {
                timeout(Duration::from_secs(35),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                    let (context,services)=start(database.connections(&database.user),&supplier).await;let manager=actor(&database,&services,&mut supplier).await;
                    Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                    let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                    let paused=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),candidate.identity().id(),input(&candidate))).await.expect("real suspension");
                    let operation=Uuid::new_v4().to_string().parse().expect("operation");
                    let original=Box::pin(services.suspension().resume(manager.clone(),operation,candidate.identity().id(),resume(&candidate,paused.id()))).await.expect("real resumption");
                    let owner=database.connections("postgres").recover().await.expect("isolated damage owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated corruption");owner.client().batch_execute(sql).await.expect("damage only fixture");owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let snapshot=owner.client().query_one("SELECT jsonb_build_object('resumptions',(SELECT jsonb_agg(to_jsonb(resumption)) FROM identity.resumptions resumption),'suspensions',(SELECT jsonb_agg(to_jsonb(suspension)) FROM identity.suspensions suspension),'revocations',(SELECT jsonb_agg(to_jsonb(revocation)) FROM identity.revocations revocation))::text",&[]).await.expect("independent corrupt original before replay").get::<_,String>(0);
                    let replay=Box::pin(services.suspension().resume(manager,operation,candidate.identity().id(),resume(&candidate,paused.id()))).await;
                    if original_damaged {
                        let error=replay.err().unwrap_or_else(||panic!("{case}: damaged original withheld"));
                        assert!(matches!(error,Error::Corrupt(_)),"{case}: {error:?}");assert!(error.unreliable(),"{case}: {error:?}");assert!(services.ready().await.is_err(),"{case}");
                    } else {
                        let replay=replay.unwrap_or_else(|error|panic!("{case}: authenticated original remains available: {error:?}"));
                        assert_eq!((replay.id(),replay.resumed_at()),(original.id(),original.resumed_at()),"{case}");
                        services.ready().await.expect("unread history does not close admission");
                    }
                    let now=context.sample().await.expect("now");
                    let account_damaged=original_damaged || case=="prior_suspension";
                    let startup=if account_damaged {
                        crate::history::inspect::<crate::identity::account::state::State>(owner.client(),services.suspension().app.keys(),now).await.err().expect("Account startup scan rejects its damaged permanent history")
                    } else {
                        crate::history::inspect::<crate::identity::account::state::State>(owner.client(),services.suspension().app.keys(),now).await.expect("unrelated Account history remains valid");
                        crate::history::inspect::<crate::identity::session::state::State>(owner.client(),services.suspension().app.keys(),now).await.err().expect("Session startup scan rejects its missing revocation")
                    };
                    assert!(startup.corrupt(),"{case}: {startup:?}");
                    eprintln!("resumption damage case={case}, original_damaged={original_damaged}, account_damaged={account_damaged}, startup={startup:?}");
                    assert_eq!(owner.client().query_one("SELECT jsonb_build_object('resumptions',(SELECT jsonb_agg(to_jsonb(resumption)) FROM identity.resumptions resumption),'suspensions',(SELECT jsonb_agg(to_jsonb(suspension)) FROM identity.suspensions suspension),'revocations',(SELECT jsonb_agg(to_jsonb(revocation)) FROM identity.revocations revocation))::text",&[]).await.expect("replay cannot repair history").get::<_,String>(0),snapshot);
                    owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");assert!(timeout(Duration::from_millis(25),supplier.request()).await.is_err());supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("resumption corruption case {sql}: {error}"));
            }
        }));
    }

    #[test]
    fn resumption_preserves_original_results_and_later_sessions() {
        use crate::identity::authentication::session::{
            Receipt,
            tests::{CODE, input as session_input, message},
        };
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for missing_old in [false,true] {
                timeout(Duration::from_secs(35),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                    let (mut context,mut services)=start(database.connections(&database.user),&supplier).await;
                    let manager=actor(&database,&services,&mut supplier).await;let target=Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                    let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("confirmation");
                    let account_id=candidate.identity().id();let operation=Uuid::new_v4().to_string().parse().expect("operation shared across families");
                    let message_id=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                    let Receipt::Session(old)=Box::pin(services.authentication().create_session(session_input(Uuid::new_v4(),message_id,"13800000000",CODE))).await.expect("old R1") else {panic!("session");};
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    let old_row=observer.client().query_one("SELECT sequence,created_at FROM identity.sessions WHERE id=$1",&[&old.id()]).await.expect("independently observe actual captured sequence");
                    assert_eq!(old_row.get::<_,i64>("sequence"),1);
                    let paused=Box::pin(services.suspension().suspend(manager.clone(),operation,account_id,input(&candidate))).await.expect("pause");
                    let blocked_message=message(&database,services.authentication(),&mut supplier,"13800000000").await;let blocked_operation=Uuid::new_v4();
                    assert!(matches!(Box::pin(services.authentication().create_session(session_input(blocked_operation,blocked_message,"13800000000",CODE))).await,Err(authentication::Error::Suspended)));
                    let before=observer.client().query_one("SELECT (SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id)::text FROM identity.revocations revocation WHERE account_id=$1) AS revocations,(SELECT count(*) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$1) AS handoffs",&[&account_id]).await.expect("effects before resume");
                    let restored=Box::pin(services.suspension().resume(manager.clone(),operation,account_id,resume(&candidate,paused.id()))).await.expect("resume shares UUID but not operation family");assert_eq!(restored.id(),paused.id());
                    let row=observer.client().query_one("SELECT resumption.person_id,resumption.revision_id,resumption.sequence,person.sequence AS person_sequence,resumption.transaction_id<>suspension.transaction_id AS separate,(SELECT jsonb_agg(to_jsonb(revocation) ORDER BY id)::text FROM identity.revocations revocation WHERE account_id=suspension.account_id) AS revocations,(SELECT count(*) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=suspension.account_id) AS handoffs FROM identity.resumptions resumption JOIN identity.suspensions suspension USING(id) JOIN identity.persons person ON person.id=resumption.person_id WHERE resumption.id=$1",&[&paused.id()]).await.expect("independent complete resume before replay");
                    assert_eq!(row.get::<_,crate::identity::person::Id>("person_id"),candidate.identity().person_id());assert_eq!(row.get::<_,crate::identity::person::revision::Id>("revision_id"),candidate.identity().revision_id());assert_eq!(row.get::<_,i64>("sequence"),3);assert_eq!(row.get::<_,i64>("person_sequence"),1);assert!(row.get::<_,bool>("separate"));assert_eq!(row.get::<_,String>("revocations"),before.get::<_,String>("revocations"));assert_eq!(row.get::<_,i64>("handoffs"),before.get::<_,i64>("handoffs"));
                    assert!(matches!(services.current().read(&target).await,Err(current::Error::Invalid)));
                    assert!(matches!(Box::pin(services.authentication().create_session(session_input(blocked_operation,blocked_message,"13800000000",CODE))).await,Err(authentication::Error::Suspended)));
                    let message_id=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                    let new_operation=Uuid::new_v4();
                    let Receipt::Session(new)=Box::pin(services.authentication().create_session(session_input(new_operation,message_id,"13800000000",CODE))).await.expect("new R1 after real resumption") else {panic!("session");};
                    let row=observer.client().query_one("SELECT sequence,created_at FROM identity.sessions WHERE id=$1",&[&new.id()]).await.expect("independent new session snapshot");assert_eq!(row.get::<_,i64>("sequence"),1);
                    let new_actor=services.current().authenticate(current::Proof{id:new.id(),token:new.token().text().parse().expect("proof")}).await.expect("new credentials work");
                    let owner=database.connections("postgres").recover().await.expect("isolated owner");
                    if missing_old {owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated missing record");owner.client().execute("DELETE FROM identity.revocations WHERE id=$1",&[&old.id()]).await.expect("deliberately remove required old member");owner.client().batch_execute("COMMIT").await.expect("isolated damage committed");}
                    let facts=observer.client().query_one("SELECT suspension.suspended_at<=resumption.resumed_at AND resumption.resumed_at<=(SELECT created_at FROM identity.sessions WHERE id=$3) AS ordered,(SELECT sequence=2 FROM identity.sessions WHERE id=$2) AND (SELECT sequence=1 FROM identity.sessions WHERE id=$3) AS captured,EXISTS(SELECT 1 FROM identity.revocations WHERE id=$2) AS old_revoked,NOT EXISTS(SELECT 1 FROM identity.revocations WHERE id=$3) AND EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$3) AS new_usable,(SELECT count(*) FROM identity.revocations WHERE suspension_id=$1) AS members FROM identity.suspensions suspension JOIN identity.resumptions resumption USING(id) WHERE suspension.id=$1",&[&paused.id(),&old.id(),&new.id()]).await.expect("independent actual lifecycle before replay");
                    for field in ["ordered","captured","new_usable"] {assert!(facts.get::<_,bool>(field));}assert_eq!(facts.get::<_,bool>("old_revoked"),!missing_old);assert_eq!(facts.get::<_,i64>("members"),if missing_old {1}else{2});
                    if missing_old {
                        let original=Box::pin(services.suspension().resume(manager.clone(),operation,account_id,resume(&candidate,paused.id()))).await.expect("unread Session record does not alter the permanent recovery");assert_eq!((original.id(),original.resumed_at()),(restored.id(),restored.resumed_at()));
                        assert!(matches!(inspect(owner.client(),services.suspension(),context.sample().await.expect("now")).await,Err(error) if error.corrupt()),"startup rejects the missing Session head record");
                    } else {
                        services.current().read(&new_actor).await.expect("later session stays usable");
                        let original=Box::pin(services.suspension().resume(manager.clone(),operation,account_id,resume(&candidate,paused.id()))).await.expect("permanent original");assert_eq!(original.resumed_at(),restored.resumed_at());
                        let Receipt::Session(replay)=Box::pin(services.authentication().create_session(session_input(new_operation,message_id,"13800000000",CODE))).await.expect("later R1 retains Handoff") else {panic!("session");};assert_eq!(replay.id(),new.id());assert_eq!(*replay.token().text(),*new.token().text());
                        services.close();services.drain().await.expect("normal resumed drain");context.stop().await.expect("resumed context ended");
                        (context,services)=start(database.connections(&database.user),&supplier).await;
                        services.current().read(&new_actor).await.expect("normal restart keeps the later session valid");
                        let Receipt::Session(replay)=Box::pin(services.authentication().create_session(session_input(new_operation,message_id,"13800000000",CODE))).await.expect("restart retained the later Handoff") else {panic!("session");};assert_eq!(replay.id(),new.id());assert_eq!(*replay.token().text(),*new.token().text());
                        let next=Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("next pause"),account_id,input(&candidate))).await.expect("later suspension");
                        let row=observer.client().query_one("SELECT (SELECT sequence FROM identity.accounts WHERE person_id=$1) AS sequence,(SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1) AS personal_sequence,EXISTS(SELECT 1 FROM identity.revocations WHERE id=$2 AND suspension_id=$3) AS revoked,NOT EXISTS(SELECT 1 FROM identity.handoffs WHERE id=$2) AS removed,NOT EXISTS(SELECT 1 FROM identity.resumptions WHERE id=$3) AS open",&[&candidate.identity().person_id(),&new.id(),&next.id()]).await.expect("second pause before any replay");assert_eq!(row.get::<_,i64>("sequence"),4);assert_eq!(row.get::<_,i64>("personal_sequence"),1);for field in ["revoked","removed","open"]{assert!(row.get::<_,bool>(field));}
                        assert_eq!(Box::pin(services.suspension().resume(manager.clone(),operation,account_id,resume(&candidate,paused.id()))).await.expect("late original never clears new pause").id(),paused.id());
                        assert!(matches!(Box::pin(services.suspension().resume(manager.clone(),Uuid::new_v4().to_string().parse().expect("different operation"),account_id,resume(&candidate,paused.id()))).await,Err(Error::Rejected(Rejection::Conflict))));
                    }
                    owner.terminate().await.expect("owner ended");observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");
                    if !missing_old {let (context,services)=start(database.connections(&database.user),&supplier).await;services.close();services.drain().await.expect("restart drained");context.stop().await.expect("restart context ended");}
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded resumption lifecycle");
            }
        }));
    }

    #[test]
    fn account_state_and_r1_follow_the_account_lock_in_both_orders() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication::session::{
            Receipt,
            tests::{CODE, input as session_input, message},
        };
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (resuming,suspension_first) in [(false,false),(false,true),(true,false),(true,true)] {
                timeout(Duration::from_secs(20),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;
                    let proxy=Proxy::start(&server).await;
                    let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let manager=actor(&database,&services,&mut supplier).await;
                    Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                    let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                    let option=if resuming {Some(Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),candidate.identity().id(),input(&candidate))).await.expect("real prior suspension").id())}else{None};
                    let message_id=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                    let authentication_id=Uuid::new_v4();let operation=Uuid::new_v4().to_string().parse().expect("operation");
                    let service=services.suspension().clone();let original=manager.clone();let confirmation=input(&candidate);let account_id=candidate.identity().id();
                    let pause=Box::pin(async move {match option {Some(suspension_id)=>service.resume(original,operation,account_id,Resume {person_id:confirmation.person_id,revision_id:confirmation.revision_id,mobile:confirmation.mobile,suspension_id}).await.map(|receipt|receipt.id()),None=>service.suspend(original,operation,account_id,confirmation).await.map(|receipt|receipt.id())}});
                    let authentication=services.authentication().clone();
                    let create=Box::pin(async move {authentication.create_session(session_input(authentication_id,message_id,"13800000000",CODE)).await});
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    proxy.arm(Fault::DelayedCommit);
                    let (paused,created)=if suspension_first {
                        let paused=tokio::spawn(pause);timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("suspension pending COMMIT");(paused,tokio::spawn(create))
                    } else {
                        let created=tokio::spawn(create);timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("R1 pending COMMIT");(tokio::spawn(pause),created)
                    };
                    let option=if resuming && suspension_first {
                        let service=services.suspension().clone();let original=manager.clone();let command=resume(&candidate,option.expect("original pause"));
                        Some(tokio::spawn(async move {service.resume(original,Uuid::new_v4().to_string().parse().expect("different operation"),account_id,command).await}))
                    }else{None};
                    let query=if suspension_first {"SELECT identity.lock_account(%"}else{"SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE"};
                    timeout(Duration::from_secs(3),async {loop {
                        let waiting:bool=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity waiter JOIN pg_stat_activity blocker ON blocker.pid=ANY(pg_blocking_pids(waiter.pid)) WHERE waiter.datname=current_database() AND blocker.datname=current_database() AND waiter.wait_event_type='Lock' AND waiter.query LIKE $1)",&[&query]).await.expect("actual account lock waits behind the unresolved original or its tuple-lock queue").get(0);
                        if waiting {break;}tokio::task::yield_now().await;
                    }}).await.expect("opposing account lock waits");
                    if option.is_some() {
                        timeout(Duration::from_secs(3),async {loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity waiter JOIN pg_stat_activity blocker ON blocker.pid=ANY(pg_blocking_pids(waiter.pid)) WHERE waiter.datname=current_database() AND blocker.datname=current_database() AND waiter.wait_event_type='Lock' AND waiter.query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE')",&[]).await.expect("the second resumption also waits on the held target account").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                        }}).await.expect("different resumption operation reached its account lock before release");
                        let graph:String=observer.client().query_one("SELECT jsonb_agg(jsonb_build_object('pid',waiter.pid,'blockers',pg_blocking_pids(waiter.pid),'stage',CASE WHEN waiter.query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE' THEN 'resumption' ELSE 'R1' END) ORDER BY waiter.pid)::text FROM pg_stat_activity waiter WHERE waiter.datname=current_database() AND waiter.wait_event_type='Lock' AND (waiter.query = 'SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE' OR waiter.query LIKE 'SELECT identity.lock_account(%')",&[]).await.expect("independent target-account wait graph before release").get(0);
                        eprintln!("Concurrent resumption and R1 account wait graph: {graph}");
                    }
                    proxy.release();
                    let completion=paused.await.expect("account state worker").expect("state committed");
                    let result=created.await.expect("R1 worker");
                    if let Some(task)=option {assert!(matches!(task.await.expect("competing original worker"),Err(Error::Rejected(Rejection::Conflict))));assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.resumptions",&[]).await.expect("only one original restoration").get::<_,i64>(0),1);}
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM identity.sessions WHERE account_id=$1) AS sessions,(SELECT count(*) FROM identity.revocations WHERE suspension_id=$2) AS revoked,NOT EXISTS(SELECT 1 FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=$1) AS removed,(SELECT status FROM identity.authentications WHERE operation_id=$3) AS status",&[&account_id,&completion,&authentication_id]).await.expect("independent complete graph before either replay");
                    let authenticated=resuming==suspension_first;
                    assert_eq!(row.get::<_,i64>("sessions"),if authenticated {2}else{1});assert_eq!(row.get::<_,i64>("revoked"),if resuming || suspension_first {1}else{2});assert_eq!(row.get::<_,bool>("removed"),!(resuming && suspension_first));
                    assert_eq!(row.get::<_,String>("status"),if authenticated {"authenticated"}else{"suspended"});
                    if !authenticated {assert!(matches!(result,Err(authentication::Error::Suspended)));assert!(matches!(Box::pin(services.authentication().create_session(session_input(authentication_id,message_id,"13800000000",CODE))).await,Err(authentication::Error::Suspended)));}
                    else {match result {Ok(Receipt::Session(_))=>{},Err(error)=>assert_eq!(error.failure(),authentication::Failure::Verification),_=>panic!("existing account never gets Registration")}}
                    if resuming {assert_eq!(Box::pin(services.suspension().resume(manager,operation,account_id,resume(&candidate,completion))).await.expect("original resumption").id(),completion);}
                    else {assert_eq!(Box::pin(services.suspension().suspend(manager,operation,account_id,input(&candidate))).await.expect("original suspension").id(),completion);}
                    observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
                })).await.expect("both account orders bounded");
            }
        }));
    }

    #[test]
    fn equal_rank_is_hidden_and_waiting_suspension_rechecks_lost_permission() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::{
            access::role,
            identity::authentication::session::{
                Receipt,
                tests::{CODE, input as session_input, message},
            },
        };
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                let administrator=actor(&database,&services,&mut supplier).await;let manager=Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                let definition=Box::pin(services.role().create(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),serde_json::from_value(json!({"name":"Account managers","level":80,"permissions":["institution.accounts"]})).expect("definition"))).await.expect("manager role");
                let candidate=services.appointment().candidate(&administrator,"13800000000".parse().expect("mobile")).await.expect("manager confirmation");
                Box::pin(services.appointment().assign(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),appointment::tests::command(&candidate,&definition))).await.expect("real manager appointment");
                let message_id=message(&database,services.authentication(),&mut supplier,"13700000000").await;
                let Receipt::Registration(credential)=Box::pin(services.authentication().create_session(session_input(Uuid::new_v4(),message_id,"13700000000",CODE))).await.expect("third account R1") else {panic!("registration");};
                Box::pin(services.registration().register(crate::registration::Input::new(Uuid::new_v4().to_string().parse().expect("operation"),credential.id(),credential.token().text().parse().expect("proof"),"Third Person".parse().expect("name"),"000000200002290029".parse().expect("number")))).await.expect("ordinary third account R2");
                let target=services.account().candidate(&manager,"13700000000".parse().expect("mobile")).await.expect("lower target");
                let account_id=target.identity().id();
                let confirmation=|| serde_json::from_value(json!({"personId":target.identity().person_id(),"revisionId":target.identity().revision_id(),"mobile":"13700000000"})).expect("confirmation");
                let candidate=services.appointment().candidate(&administrator,"13700000000".parse().expect("mobile")).await.expect("target appointment");
                let assignment=serde_json::from_value(json!({"target":{"accountId":account_id,"personId":target.identity().person_id(),"revisionId":target.identity().revision_id(),"mobile":"13700000000","assignmentId":candidate.assignment_id()},"role":{"id":definition.id(),"changeId":definition.change_id()}})).expect("third account confirmation");
                Box::pin(services.appointment().assign(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),assignment)).await.expect("equal rank target");
                assert!(matches!(services.account().candidate(&manager,"13700000000".parse().expect("mobile")).await,Err(crate::account::Error::Absent)));
                assert!(matches!(Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,confirmation())).await,Err(Error::Rejected(Rejection::Absent))));
                let observer=database.connections(&database.user).recover().await.expect("observer");assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.suspensions",&[]).await.expect("same rank never creates history").get::<_,i64>(0),0);
                let lower=Box::pin(services.role().create(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),serde_json::from_value(json!({"name":"Lower target","level":2,"permissions":[]})).expect("lower definition"))).await.expect("lower role");
                let selected=services.appointment().candidate(&administrator,"13700000000".parse().expect("mobile")).await.expect("new assignment confirmation");
                let assignment=serde_json::from_value(json!({"target":{"accountId":account_id,"personId":target.identity().person_id(),"revisionId":target.identity().revision_id(),"mobile":"13700000000","assignmentId":selected.assignment_id()},"role":{"id":lower.id(),"changeId":lower.change_id()}})).expect("lower confirmation");
                Box::pin(services.appointment().assign(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),assignment)).await.expect("lower target now manageable");
                let paused=Box::pin(services.suspension().suspend(administrator.clone(),Uuid::new_v4().to_string().parse().expect("operation"),account_id,confirmation())).await.expect("real pause awaiting manager restoration");
                proxy.arm(Fault::DelayedCommit);let role_service=services.role().clone();let original=administrator.clone();let role_id=definition.id();let change_id=definition.change_id();
                let changed=tokio::spawn(async move {role_service.update(original,Uuid::new_v4().to_string().parse().expect("operation"),role_id,role::Update::new(change_id,serde_json::from_value(json!({"name":"Account managers","level":80,"permissions":[]})).expect("remove permission"))).await});
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("role update holds institution exclusive lock");
                let service=services.suspension().clone();let command=confirmation();let original=manager.clone();let waiting=tokio::spawn(async move {service.suspend(original,Uuid::new_v4().to_string().parse().expect("operation"),account_id,command).await});
                let service=services.suspension().clone();let command=serde_json::from_value(json!({"personId":target.identity().person_id(),"revisionId":target.identity().revision_id(),"mobile":"13700000000","suspensionId":paused.id()})).expect("valid restore confirmation");let restoring=tokio::spawn(async move {service.resume(manager,Uuid::new_v4().to_string().parse().expect("operation"),account_id,command).await});
                timeout(Duration::from_secs(3),async {loop {if observer.client().query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock_shared%' AND cardinality(pg_blocking_pids(pid))>0",&[]).await.expect("both account state operations wait on institution permission").get::<_,i64>(0)==2{break;}tokio::task::yield_now().await;}}).await.expect("pause and resumption both wait before decisive authorization");
                proxy.release();changed.await.expect("role worker").expect("permission removed");let error=waiting.await.expect("suspension worker").err().expect("lost permission rejected");assert!(matches!(error,Error::Denied));
                let error=restoring.await.expect("resumption worker").err().expect("lost permission cannot resume");assert!(matches!(error,Error::Denied));
                assert!(observer.client().query_one("SELECT (SELECT count(*) FROM identity.suspensions)=1 AND NOT EXISTS(SELECT 1 FROM identity.resumptions) AND (SELECT suspension_id=$2 AND sequence=2 FROM identity.accounts WHERE id=$1)",&[&account_id,&paused.id()]).await.expect("original paused target unchanged after both lock waits").get::<_,bool>(0));
                observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
            })).await.expect("rank and permission budget");
        }));
    }

    #[test]
    fn original_damage_never_becomes_a_mismatch_or_known_completion() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (case, original_damaged, sql) in [
                    ("mobile_format", true, "UPDATE identity.suspensions SET mobile=set_byte(mobile,0,2)"),
                    ("mobile_ciphertext", true, "UPDATE identity.suspensions SET mobile=set_byte(mobile,47,get_byte(mobile,47)#1)"),
                    ("operator", true, "UPDATE identity.suspensions SET operator_id=gen_random_uuid()"),
                    ("revision", true, "UPDATE identity.suspensions SET revision_id=operator_id"),
                    ("missing_revocation", false, "DELETE FROM identity.revocations WHERE mode='suspension'"),
                    ("revocation_time", false, "UPDATE identity.revocations SET revoked_at=revoked_at+interval '1 microsecond' WHERE mode='suspension'"),
                    ("revocation_cause", false, "UPDATE identity.revocations SET mode='logout',cause_id=operation_id,revoked_at=revoked_at+interval '1 microsecond' WHERE mode='suspension'"),
                    ("revocation_operation", false, "UPDATE identity.revocations SET operation_id=gen_random_uuid() WHERE mode='suspension'"),
                    ("missing_session", false, "DELETE FROM identity.sessions WHERE account_id IN (SELECT account_id FROM identity.suspensions)"),
            ] {
                timeout(Duration::from_secs(40),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;
                    let (context,services)=start(database.connections(&database.user),&supplier).await;
                    let manager=actor(&database,&services,&mut supplier).await;
                    Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                    let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                    let operation=Uuid::new_v4().to_string().parse().expect("operation");
                    let original=Box::pin(services.suspension().suspend(manager.clone(),operation,candidate.identity().id(),input(&candidate))).await.expect("real original");
                    let owner=database.connections("postgres").recover().await.expect("isolated damage owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated corruption");
                    owner.client().batch_execute(sql).await.expect("explicit corrupted history");
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let snapshot:String=owner.client().query_one("SELECT jsonb_build_object('suspensions',(SELECT jsonb_agg(to_jsonb(suspension)) FROM identity.suspensions suspension),'revocations',(SELECT jsonb_agg(to_jsonb(revocation)) FROM identity.revocations revocation),'persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person))::text",&[]).await.expect("independent corrupt graph before replay").get(0);
                    let replay=Box::pin(services.suspension().suspend(manager,operation,candidate.identity().id(),input(&candidate))).await;
                    if original_damaged {
                        let error=replay.err().unwrap_or_else(||panic!("{case}: corruption is never an original success"));
                        assert!(error.unreliable(),"{case}: {error:?}");assert!(matches!(error,Error::Corrupt(_)),"{case}: {error:?}");
                        assert!(services.ready().await.is_err(),"{case}: fact damage closes write admission");
                    } else {
                        let replay=replay.unwrap_or_else(|error|panic!("{case}: authenticated original remains available: {error:?}"));
                        assert_eq!((replay.id(),replay.suspended_at()),(original.id(),original.suspended_at()),"{case}");
                        services.ready().await.expect("unread history does not close admission");
                    }
                    let now=context.sample().await.expect("now");
                    let startup=if original_damaged {
                        crate::history::inspect::<crate::identity::account::state::State>(owner.client(),services.suspension().app.keys(),now).await.err().expect("Account startup scan rejects its damaged permanent original")
                    } else {
                        crate::history::inspect::<crate::identity::account::state::State>(owner.client(),services.suspension().app.keys(),now).await.expect("unrelated Account history remains valid");
                        crate::history::inspect::<crate::identity::session::state::State>(owner.client(),services.suspension().app.keys(),now).await.err().expect("Session startup scan rejects its damaged retained history")
                    };
                    assert!(startup.corrupt(),"{case}: {startup:?}");
                    eprintln!("suspension damage case={case}, original_damaged={original_damaged}, startup={startup:?}");
                    assert_eq!(owner.client().query_one("SELECT jsonb_build_object('suspensions',(SELECT jsonb_agg(to_jsonb(suspension)) FROM identity.suspensions suspension),'revocations',(SELECT jsonb_agg(to_jsonb(revocation)) FROM identity.revocations revocation),'persons',(SELECT jsonb_agg(to_jsonb(person) ORDER BY id) FROM identity.persons person))::text",&[]).await.expect("rejection does not alter history").get::<_,String>(0),snapshot);
                    assert_eq!(owner.client().query_one("SELECT id FROM identity.suspensions WHERE operation_id=$1",&[&operation]).await.expect("original identity retained").get::<_,suspension::Id>(0),original.id());
                    owner.terminate().await.expect("owner ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(25),supplier.request()).await.is_err());supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("suspension corruption case {sql}: {error}"));
            }
        }));
    }

    #[test]
    fn runtime_sql_rejects_partial_suspensions_and_credentials_after_early_checks() {
        use tokio_postgres::error::SqlState;

        use crate::identity::authentication::session::tests::{
            CODE, input as session_input, message,
        };

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30),Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let manager=actor(&database,&services,&mut supplier).await;
                let target=Box::pin(appointment::tests::register(&database,&services,&mut supplier)).await;
                let candidate=services.account().candidate(&manager,"13800000000".parse().expect("mobile")).await.expect("target");
                let runtime=database.connections(&database.user).recover().await.expect("actual runtime SQL");
                assert_eq!(runtime.client().query_one("SELECT current_user::text",&[]).await.expect("runtime identity").get::<_,String>(0),database.user.to_string());
                let operator=runtime.client().query_one("SELECT person.revision_id FROM identity.sessions session JOIN identity.accounts account ON account.id=session.account_id JOIN identity.persons person ON person.id=account.person_id WHERE session.id=$1",&[&manager.session_id()]).await.expect("current manager revision").get::<_,crate::identity::person::revision::Id>(0);
                let before=runtime.client().query_one("SELECT jsonb_build_object('account',(SELECT to_jsonb(account) FROM identity.accounts account WHERE id=$1),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=$1),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY id) FROM identity.handoffs handoff WHERE id IN (SELECT id FROM identity.sessions WHERE account_id=$1)))::text",&[&candidate.identity().id()]).await.expect("unchanged real graph").get::<_,String>(0);
                for (defect, expected, constraint) in [
                    ("control",None,""),
                    ("missing_completion",Some(SqlState::FOREIGN_KEY_VIOLATION),"suspensions_family_operation_id_fkey"),
                    ("revision",Some(SqlState::FOREIGN_KEY_VIOLATION),"suspensions_revision_id_person_id_fkey"),
                    ("missing_record",Some(SqlState::CHECK_VIOLATION),"accounts_records_check"),
                    ("gap",Some(SqlState::CHECK_VIOLATION),"accounts_records_check"),
                    ("missing_revocation",Some(SqlState::CHECK_VIOLATION),"sessions_records_check"),
                    ("revocation_owner",Some(SqlState::FOREIGN_KEY_VIOLATION),"revocations_suspension_id_account_id_fkey"),
                    ("revocation_sequence",Some(SqlState::CHECK_VIOLATION),"revocations_sequence_check"),
                    ("session_version",Some(SqlState::CHECK_VIOLATION),"sessions_check"),
                ] {
                    runtime.client().batch_execute("BEGIN").await.expect("structural attempt");
                    let now=context.sample().await.expect("trusted time");let operation=Uuid::new_v4();let id=Uuid::new_v4();
                    let result:Result<(),tokio_postgres::Error>=async {
                        if defect!="missing_completion" {raw_management_completion(runtime.client(),false,&manager,candidate.identity().id(),operation,now).await;}
                        if defect!="missing_record" {
                            runtime.client().execute(
                                "INSERT INTO identity.suspensions(id,account_id,person_id,revision_id,operator_id,mobile,suspended_at,sequence,operation_id,sessions,seal,seal_key_id,seal_version)
                                    SELECT $1,account.id,account.person_id,CASE WHEN $7='revision' THEN $2 ELSE person.revision_id END,$2,
                                        decode(repeat('00',48),'hex'),$3,account.sequence+CASE WHEN $7='gap' THEN 2 ELSE 1 END,$4,ARRAY[$6::uuid],
                                        decode(repeat('00',32),'hex'),account.seal_key_id,1
                                    FROM identity.accounts account JOIN identity.persons person ON person.id=account.person_id WHERE account.id=$5",
                                &[&id,&operator,&now,&operation,&candidate.identity().id(),&target.session_id(),&defect],
                            ).await?;
                        }
                        runtime.client().execute(
                            "UPDATE identity.accounts SET suspension_id=$2,changed_at=$3,sequence=sequence+CASE WHEN $4='gap' THEN 2 ELSE 1 END,
                                version=version+1,seal=decode(repeat('00',32),'hex') WHERE id=$1",
                            &[&candidate.identity().id(),&id,&now,&defect],
                        ).await?;
                        runtime.client().batch_execute("SET CONSTRAINTS identity.accounts_records_check IMMEDIATE").await?;
                        if defect!="missing_revocation" {
                            runtime.client().execute(
                                "INSERT INTO identity.revocations(id,account_id,sequence,mode,cause_id,operation_id,revoked_at,seal,seal_key_id,seal_version)
                                    SELECT $1,CASE WHEN $7='revocation_owner' THEN $6 ELSE account.id END,
                                        CASE WHEN $7='revocation_sequence' THEN 1 ELSE 2 END,'suspension',$2,$3,$4,
                                        decode(repeat('00',32),'hex'),account.seal_key_id,1 FROM identity.accounts account WHERE account.id=$5",
                                &[&target.session_id(),&id,&operation,&now,&candidate.identity().id(),&manager.account_id(),&defect],
                            ).await?;
                        }
                        runtime.client().execute(
                            "UPDATE identity.sessions SET sequence=2,version=CASE WHEN $3='session_version' THEN 1 ELSE 2 END,
                                revoked_at=$2,seal=decode(repeat('00',32),'hex') WHERE id=$1",
                            &[&target.session_id(),&now,&defect],
                        ).await?;
                        runtime.client().batch_execute("SET CONSTRAINTS identity.sessions_records_check IMMEDIATE").await?;
                        runtime.client().execute("DELETE FROM identity.handoffs WHERE id=$1",&[&target.session_id()]).await?;
                        runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await
                    }.await;
                    match expected {
                        None=>result.expect("full Account and Session pairing passes the structural control"),
                        Some(expected)=>{
                            let error=result.expect_err("the named structural defect is rejected");
                            assert_eq!(error.code(),Some(&expected),"{defect}: {error}");
                            assert_eq!(error.as_db_error().expect("server constraint").constraint(),Some(constraint),"{defect}: {error}");
                            eprintln!("suspension structure case={defect}, code={expected:?}, constraint={constraint}");
                        }
                    }
                    runtime.client().batch_execute("ROLLBACK").await.expect("dummy seals and envelopes never commit");
                    assert!(runtime.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM identity.suspensions) AND NOT EXISTS(SELECT 1 FROM identity.revocations) AND NOT EXISTS(SELECT 1 FROM identity.completions WHERE family='suspension')",&[]).await.expect("no partial effects").get::<_,bool>(0),"{defect}");
                    assert_eq!(runtime.client().query_one("SELECT jsonb_build_object('account',(SELECT to_jsonb(account) FROM identity.accounts account WHERE id=$1),'sessions',(SELECT jsonb_agg(to_jsonb(session) ORDER BY id) FROM identity.sessions session WHERE account_id=$1),'handoffs',(SELECT jsonb_agg(to_jsonb(handoff) ORDER BY id) FROM identity.handoffs handoff WHERE id IN (SELECT id FROM identity.sessions WHERE account_id=$1)))::text",&[&candidate.identity().id()]).await.expect("rolled-back graph").get::<_,String>(0),before,"{defect}");
                }
                Box::pin(services.suspension().suspend(manager.clone(),Uuid::new_v4().to_string().parse().expect("operation"),candidate.identity().id(),input(&candidate))).await.expect("real API still accepts the unchanged confirmation");
                let verification=message(&database,services.authentication(),&mut supplier,"13800000000").await;
                assert!(matches!(Box::pin(services.authentication().create_session(session_input(Uuid::new_v4(),verification,"13800000000",CODE))).await,Err(authentication::Error::Suspended)));
                assert!(matches!(Box::pin(services.suspension().suspend(manager,Uuid::new_v4().to_string().parse().expect("operation"),candidate.identity().id(),input(&candidate))).await,Err(Error::Rejected(Rejection::Conflict))));
                for sql in ["UPDATE identity.suspensions SET suspended_at=suspended_at+interval '1 second'","DELETE FROM identity.suspensions"] {
                    assert_eq!(runtime.client().batch_execute(sql).await.expect_err("runtime cannot mutate permanent suspension records").code(),Some(&SqlState::INSUFFICIENT_PRIVILEGE));
                }
                runtime.terminate().await.expect("runtime ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(25),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("SQL protection budget");
        }));
    }

    #[test]
    fn suspension_preserves_personal_history_and_revokes_without_repeating_effects() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let target = appointment::tests::register(&database, &services, &mut supplier).await;
                let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), serde_json::from_value(json!({"name":"Suspension target","level":20,"permissions":[]})).expect("role input"))).await.expect("selected role");
                let previous = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("appointment confirmation");
                let assignment_operation = Uuid::new_v4().to_string().parse().expect("assignment operation");
                let first = Box::pin(services.appointment().assign(manager.clone(), assignment_operation, appointment::tests::command(&previous, &selected))).await.expect("first replacement");
                let candidate = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("suspension confirmation");
                assert!(candidate.suspension_id().is_none());
                let operation = Uuid::new_v4().to_string().parse().expect("suspension operation");
                let original = Box::pin(services.suspension().suspend(manager.clone(), operation, candidate.identity().id(), input(&candidate))).await.expect("suspended");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                let row = observer.client().query_one("SELECT suspension.sequence, account.sequence AS account_sequence,person.sequence AS person_sequence, holder.assignment_id, (SELECT count(*) FROM identity.revocations WHERE mode='suspension' AND cause_id=suspension.id AND account_id=suspension.account_id AND operation_id=suspension.operation_id AND revoked_at=suspension.suspended_at) AS revocations, (SELECT count(*) FROM identity.sessions WHERE account_id = suspension.account_id AND revoked_at=suspension.suspended_at AND sequence=2 AND version=2) AS sessions, (SELECT count(*) FROM identity.handoffs handoff JOIN identity.sessions session ON session.id=handoff.id WHERE session.account_id=suspension.account_id) AS handoffs FROM identity.suspensions suspension JOIN identity.accounts account ON account.id=suspension.account_id JOIN identity.persons person ON person.id = suspension.person_id JOIN access.holders holder ON holder.id = suspension.account_id WHERE suspension.id = $1", &[&original.id()]).await.expect("independent committed facts before replay");
                assert_eq!(row.get::<_,i64>("sequence"), 2); assert_eq!(row.get::<_,i64>("account_sequence"), 2); assert_eq!(row.get::<_,i64>("person_sequence"), 1);
                assert_eq!(row.get::<_,appointment::Id>("assignment_id"), first.id());
                assert_eq!(row.get::<_,i64>("revocations"), 1); assert_eq!(row.get::<_,i64>("sessions"), 1); assert_eq!(row.get::<_,i64>("handoffs"), 0);
                assert!(matches!(services.current().read(&target).await, Err(current::Error::Invalid)));
                let candidate_after = services.account().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("paused candidate remains disclosable");
                assert_eq!(candidate_after.suspension_id(), Some(original.id()));
                assert!(matches!(Box::pin(services.suspension().suspend(manager.clone(), Uuid::new_v4().to_string().parse().expect("new operation"), candidate.identity().id(), input(&candidate))).await, Err(Error::Rejected(Rejection::Conflict))));
                let second_role = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("role operation"), serde_json::from_value(json!({"name":"Suspended reassignment","level":21,"permissions":[]})).expect("role input"))).await.expect("second role");
                let confirmed = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("suspended target can be reassigned");
                let next = Box::pin(services.appointment().assign(manager.clone(), Uuid::new_v4().to_string().parse().expect("next operation"), appointment::tests::command(&confirmed, &second_role))).await.expect("appointment after suspension");
                assert_eq!(next.sequence(), 3);
                assert_eq!(observer.client().query_one("SELECT sequence FROM identity.accounts WHERE id = $1", &[&candidate.identity().id()]).await.expect("independent sequence before replay").get::<_,i64>(0), 2);
                let replay = Box::pin(services.suspension().suspend(manager.clone(), operation, candidate.identity().id(), input(&candidate))).await.expect("old suspension result");
                assert_eq!(replay.id(), original.id()); assert_eq!(replay.suspended_at(), original.suspended_at());
                assert_eq!(Box::pin(services.appointment().assign(manager.clone(), assignment_operation, appointment::tests::command(&previous, &selected))).await.expect("old appointment remains verifiable").id(), first.id());
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.suspension().suspend(manager, operation, candidate.identity().id(), input(&candidate))).await.expect("restart original").id(), original.id());
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart context ended");
                assert!(timeout(Duration::from_millis(40), supplier.request()).await.is_err(), "no supplier request through suspension, replay and drain"); supplier.stop().await;
            })).await.expect("bounded lifecycle");
        }));
    }
}
