//! Workforce definition reads and maintenance with scoped permissions.

use serde::Deserialize;
use utoipa::ToSchema;

use crate::{
    access::{
        Entry,
        capability::Scope,
        role::{
            self, Code, CommandError, Id, InputError, Level, List, Name, Role, Service, change,
            operation::{Action, Receipt},
        },
    },
    identity::{current::Actor, operation},
    pagination::Pagination,
};

impl Service {
    pub async fn changes_workforce(
        &self,
        actor: &Actor,
        filter: change::Filter,
    ) -> Result<change::List, CommandError> {
        self.fetch_changes(actor, filter, Scope::Workforce).await
    }

    pub async fn list_workforce(
        &self,
        actor: &Actor,
        pagination: Pagination,
    ) -> Result<List, CommandError> {
        self.fetch_list(actor, pagination, Scope::Workforce).await
    }

    pub async fn create_workforce(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Role, CommandError> {
        match self
            .run(actor, operation_id, Action::CreateWorkforce(input))
            .await?
        {
            Receipt::Role(role) => Ok(role),
            Receipt::Change(_) => Err(CommandError::corrupt(
                "a role definition operation returns its definition",
            )),
        }
    }

    pub async fn update_workforce(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
        input: Update,
    ) -> Result<Role, CommandError> {
        match self
            .run(
                actor,
                operation_id,
                Action::UpdateWorkforce {
                    id,
                    change_id: input.change_id,
                    input: Input {
                        entry: input.entry,
                        name: input.name,
                        level: input.level,
                        permissions: input.permissions,
                    },
                },
            )
            .await?
        {
            Receipt::Role(role) => Ok(role),
            Receipt::Change(_) => Err(CommandError::corrupt(
                "a role modification returns its definition",
            )),
        }
    }

    pub async fn delete_workforce(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
        input: Delete,
    ) -> Result<change::Change, CommandError> {
        match self
            .run(
                actor,
                operation_id,
                Action::DeleteWorkforce {
                    id,
                    change_id: input.change_id,
                    entry: input.entry,
                },
            )
            .await?
        {
            Receipt::Change(change) => Ok(change),
            Receipt::Role(_) => Err(CommandError::corrupt(
                "a role deletion returns its terminal change",
            )),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = role::workforce::Input)]
pub struct Input {
    pub(super) entry: Entry,
    pub(super) name: Name,
    pub(super) level: Level,
    pub(super) permissions: Permissions,
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = role::workforce::Update)]
pub struct Update {
    entry: Entry,
    change_id: change::Id,
    name: Name,
    level: Level,
    permissions: Permissions,
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = role::workforce::Delete)]
pub struct Delete {
    entry: Entry,
    change_id: change::Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "Vec<Code>")]
pub(super) struct Permissions(Vec<Code>);

impl Permissions {
    pub(super) fn values(&self) -> &[Code] {
        &self.0
    }
}

impl TryFrom<Vec<Code>> for Permissions {
    type Error = InputError;

    fn try_from(mut values: Vec<Code>) -> Result<Self, InputError> {
        role::Permissions::validate(Scope::Workforce, &values)?;
        values.sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
        Ok(Self(values))
    }
}

impl utoipa::PartialSchema for Permissions {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        role::Permissions::schema_for(Scope::Workforce)
    }
}

impl ToSchema for Permissions {
    fn name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("role.workforce.Permissions")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use serde_json::{Value, json};
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            Entry,
            appointment::tests::register,
            grant,
            role::{
                self, Error, Kind, Rejection,
                change::Filter,
                tests::{actor, start},
                workforce::{Delete, Input, Update},
            },
        },
        identity::{
            authentication::{self, Database, Supplier, server},
            current, operation,
            person::revision,
        },
        kernel,
        pagination::Pagination,
        runtime::work::Failure,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    fn input(value: &Value) -> Input {
        serde_json::from_value(value.clone()).expect("typed Workforce definition")
    }

    fn deletion(entry: &str, role: &role::Role) -> Delete {
        serde_json::from_value(json!({"entry":entry,"changeId":role.change_id()}))
            .expect("typed deletion confirmation")
    }

    fn modification(role: &role::Role, value: &Value) -> Update {
        let mut value = value.clone();
        value["changeId"] = json!(role.change_id());
        serde_json::from_value(value).expect("complete typed modification")
    }

    async fn register_operator(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
    ) -> current::Actor {
        use authentication::session::{
            Receipt,
            tests::{CODE, input, message},
        };
        let message = message(database, services.authentication(), supplier, "13700000000").await;
        let Receipt::Registration(credential) =
            Box::pin(services.authentication().create_session(input(
                Uuid::new_v4(),
                message,
                "13700000000",
                CODE,
            )))
            .await
            .expect("R1")
        else {
            panic!("registration credential");
        };
        let receipt = services
            .registration()
            .register(crate::registration::Input::new(
                operation(),
                credential.id(),
                credential.token().text().parse().expect("proof"),
                "Operator".parse().expect("name"),
                "000000200002290029".parse().expect("id number"),
            ))
            .await
            .expect("R2 operator");
        services
            .current()
            .authenticate(current::Proof {
                id: receipt.id(),
                token: receipt.token().text().parse().expect("proof"),
            })
            .await
            .expect("operator session")
    }

    fn admission(
        candidate: &grant::Candidate,
        entry: &str,
        mobile: &str,
        selected: &role::Role,
    ) -> grant::Input {
        let identity = candidate.identity();
        serde_json::from_value(json!({"entry":entry,"target":{
            "accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),
            "mobile":mobile,"assignmentId":candidate.assignment_id(),"suspensionId":candidate.suspension_id(),"grant":candidate.grant()
        },"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("typed admission")
    }

    fn history() -> Filter {
        Filter::new(Pagination::new(1, 100).expect("complete page"), None, None)
    }

    async fn reader(
        database: &Database,
        services: &crate::startup::Services,
        supplier: &mut Supplier,
        institution: &current::Actor,
    ) -> (current::Actor, role::Role) {
        let actor = Box::pin(register(database, services, supplier)).await;
        let role = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
            "entry":"institution", "name":"Reader", "level":200, "permissions":["workforce.roles"]
        })))).await.expect("history reader role");
        let candidate = services
            .grant()
            .candidate(
                institution,
                Entry::Institution,
                "13800000000".parse().expect("mobile"),
                Pagination::new(1, 100).expect("page"),
            )
            .await
            .expect("reader candidate");
        Box::pin(services.grant().create(
            institution.clone(),
            operation(),
            admission(&candidate, "institution", "13800000000", &role),
        ))
        .await
        .expect("real reader membership");
        (actor, role)
    }

    #[test]
    fn workforce_history_keeps_one_snapshot_and_ends_failed_or_cancelled_reads() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["snapshot", "cancel", "commit", "timeout"] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = if case == "snapshot" { crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier).await } else { start(authentication::proxied(&database, &proxy), &supplier).await };
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let (reader, role) = Box::pin(reader(&database, &services, &mut supplier, &institution)).await;
                    let observer = database.connections("postgres").recover().await.expect("independent observer");
                    proxy.arm(Fault::StalledRoleHistoryCount);
                    let service = services.role().clone();
                    let current = reader.clone();
                    let task = tokio::spawn(async move { service.changes_workforce(&current, history()).await });
                    let pid = timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("authorized count reached");
                    if case == "snapshot" {
                        let added = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                            "entry":"institution", "name":"Later", "level":700, "permissions":[]
                        })))).await.expect("concurrent role");
                        let changed = Box::pin(services.role().update_workforce(institution.clone(), operation(), role.id(), modification(&role, &json!({
                            "entry":"institution", "name":"Reader", "level":200, "permissions":[]
                        })))).await.expect("concurrent permission removal");
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE scope='workforce'", &[]).await.expect("later facts").get::<_, i64>(0), 5);
                        proxy.release();
                        let page = task.await.expect("page task").expect("original snapshot");
                        assert_eq!(page.count(), 3);
                        assert_eq!(page.changes().len(), 3);
                        assert!(!page.changes().iter().any(|change| change.role_id() == added.id()));
                        assert!(matches!(page.changes().iter().find(|change| change.role_id() == role.id()).expect("original role").action(),
                            role::change::Action::Create { permissions } if permissions == &[role::Code::WorkforceRoles]));
                        assert!(matches!(services.role().changes_workforce(&reader, history()).await, Err(kernel::Error::Denied)));
                        Box::pin(services.role().update_workforce(institution.clone(), operation(), changed.id(), modification(&changed, &json!({"entry":"institution","name":"Reader","level":200,"permissions":["workforce.roles"]})))).await.expect("reader permission restored");
                        let latest = services.role().changes_workforce(&reader, history()).await.expect("fresh history snapshot");
                        assert_eq!(latest.count(), 6);
                        assert!(latest.changes().iter().any(|change| change.role_id() == added.id()));
                    } else {
                        if case == "cancel" {
                            task.abort();
                            assert!(task.await.is_err_and(|error| error.is_cancelled()));
                        } else {
                            if case == "commit" {
                                proxy.arm(Fault::LostReadCommit);
                                proxy.release();
                                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual read COMMIT");
                            }
                            let error = timeout(Duration::from_secs(5), task).await.expect("bounded read")
                                .expect("page task").err().expect("unacknowledged page withheld");
                            let kernel::Error::Unavailable(failure) = &error else { panic!("history read keeps its technical cause: {error:?}"); };
                            if case == "commit" {
                                assert_eq!(failure.stage(), kernel::Stage::Read);
                                assert!(matches!(failure.kind(), kernel::FaultKind::Query(_)), "{error:?}");
                            } else {
                                assert_eq!(failure.stage(), kernel::Stage::Command);
                                assert!(matches!(failure.kind(), kernel::FaultKind::Timeout(_)), "{error:?}");
                            }
                            assert!(!Failure::unreliable(&error), "transport failure is not damaged facts: {error:?}");
                        }
                        proxy.release();
                        timeout(Duration::from_secs(2), async {
                            while observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&pid])
                                .await.expect("backend state").get::<_, bool>(0) { tokio::task::yield_now().await; }
                        }).await.expect("original backend ended before reuse");
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("no writes before retry").get::<_, i64>(0), 5);
                        assert_eq!(services.role().changes_workforce(&reader, history()).await.expect("fresh read").count(), 3);
                    }
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("history {case}: {error}"));
            }
        }));
    }

    #[test]
    fn workforce_history_rejects_broken_facts_after_authority_and_recovers_query_failures() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["entry", "name", "id_number", "operator", "query"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let (reader, _) = Box::pin(reader(&database, &services, &mut supplier, &institution)).await;
                    let target = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                        "entry":"institution", "name":"Retained", "level":700, "permissions":[]
                    })))).await.expect("higher definition");
                    Box::pin(services.role().delete_workforce(institution.clone(), operation(), target.id(), deletion("institution", &target)))
                        .await.expect("real deletion");
                    let selected = || Filter::new(Pagination::new(1, 20).expect("page"), Some(target.id()), None);
                    assert_eq!(services.role().changes_workforce(&reader, selected()).await.expect("deleted higher role").count(), 2);
                    let owner = database.connections("postgres").recover().await.expect("isolated fault injector");
                    let count: i64 = owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("facts before corruption").get(0);
                    if matches!(case, "name" | "id_number") {
                        let original: revision::Id = owner.client().query_one("SELECT operator_id FROM access.changes WHERE id=$1", &[&target.change_id()]).await.expect("historical operator").get(0);
                        let history = services.role().app.keys();
                        let identity = revision::fetch(owner.client(), history, original).await.expect("original identity");
                        crate::correction::tests::rename_administrator(&services, identity.person_id(), identity.revision_id(), "Healthy Current Operator", identity.id_number().as_ref()).await;
                    }
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated fault");
                    match case {
                        "entry" => {
                            owner.client().batch_execute("ALTER TABLE access.changes DROP CONSTRAINT changes_entry_check").await.expect("isolate missing authority entry");
                            owner.client().execute("UPDATE access.changes SET entry=NULL WHERE role_id=$1", &[&target.id()]).await.expect("missing entry");
                        }
                        "name" | "id_number" => {
                            let query = if case == "name" {
                                "UPDATE identity.revisions SET name=set_byte(name,octet_length(name)-1,get_byte(name,octet_length(name)-1)#1) WHERE id=(SELECT operator_id FROM access.changes WHERE id=$1)"
                            } else {
                                "UPDATE identity.revisions SET id_number=set_byte(id_number,octet_length(id_number)-1,get_byte(id_number,octet_length(id_number)-1)#1) WHERE id=(SELECT operator_id FROM access.changes WHERE id=$1)"
                            };
                            owner.client().execute(query, &[&target.change_id()]).await.expect("damaged original identity authentication");
                        }
                        "operator" => {
                            owner.client().execute("UPDATE access.changes SET operator_id=gen_random_uuid() WHERE role_id=$1", &[&target.id()]).await.expect("missing fixed operator");
                        }
                        "query" => {
                            owner.client().batch_execute(&format!("REVOKE SELECT ON identity.revisions FROM {}", database.user)).await.expect("read failure");
                        }
                        _ => unreachable!(),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("fault visible");
                    let rejection = services.role().changes_workforce(&institution, selected()).await.err();
                    if case == "query" {
                        assert!(matches!(&rejection, Some(kernel::Error::Unavailable(fault))
                            if fault.stage() == kernel::Stage::Read
                                && matches!(fault.kind(), kernel::FaultKind::Identity(crate::identity::current::Error::Query(query))
                                    if query.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE))),
                            "current identity qualification needs the revoked relation: {rejection:?}");
                    } else {
                        assert!(matches!(&rejection, Some(kernel::Error::Denied)), "{case}: institution authority cannot disclose history or its damage: {rejection:?}");
                    }
                    services.ready().await.expect("unread damage has no effect");
                    let error = services.role().changes_workforce(&reader, selected()).await.err().expect("no partial page");
                    assert_eq!(Failure::unreliable(&error), case != "query", "{case}: {error:?}");
                    assert_eq!(services.ready().await.is_err(), case != "query");
                    for secret in ["隔离", "000000200002290010", "13900000000"] { assert!(!format!("{error:?}").contains(secret)); }
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM access.changes", &[]).await.expect("no repairs or writes").get::<_, i64>(0), count);
                    if case == "query" {
                        owner.client().batch_execute(&format!("GRANT SELECT ON identity.revisions TO {}", database.user)).await.expect("repair isolated grant");
                        assert_eq!(services.role().changes_workforce(&reader, selected()).await.expect("fresh successful read").count(), 2);
                    }
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("history fault {case}: {error}"));
            }
        }));
    }

    #[test]
    fn workforce_history_checks_successors_outside_the_filter_across_batches() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in ["recreation", "scope"] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let (reader, _) = Box::pin(reader(&database, &services, &mut supplier, &institution)).await;
                    let operation = operation();
                    let target = Box::pin(services.role().create_workforce(institution.clone(), operation, input(&json!({
                        "entry":"institution", "name":"Version 0", "level":700, "permissions":[]
                    })))).await.expect("original role");
                    let selected = || Filter::new(Pagination::new(1, 1).expect("single record"), Some(target.id()), Some(operation));
                    let mut current = target.clone();
                    timeout(Duration::from_secs(30), Box::pin(async {
                        for sequence in 1..=130 {
                            let definition = json!({"entry":"institution","name":format!("Version {sequence}"),"level":700,"permissions":[]});
                            current = Box::pin(services.role().update_workforce(institution.clone(), crate::identity::operation::Id::allocate(), current.id(), modification(&current, &definition))).await.expect("actual sealed successor");
                        }
                    })).await.expect("bounded preparation of 130 authentic successors");
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    let page = services.role().changes_workforce(&reader, selected()).await.expect("valid cross-batch history");
                    assert_eq!(page.count(), 1); assert_eq!(page.changes().len(), 1);
                    assert_eq!(page.changes()[0].id(), target.change_id());
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("late corruption");
                    let query = if fault == "recreation" {
                        "UPDATE access.changes SET action='create' WHERE role_id=$1 AND name='Version 129'"
                    } else {
                        "UPDATE access.changes SET scope='institution',entry=NULL WHERE role_id=$1 AND name='Version 129'"
                    };
                    assert_eq!(owner.client().execute(query, &[&target.id()]).await.expect("late fault outside selected operation"), 1);
                    owner.client().batch_execute("COMMIT").await.expect("fault visible");
                    assert!(matches!(services.role().changes_workforce(&reader, selected()).await, Err(error) if error.is_corrupt()), "{fault}");
                    assert!(services.ready().await.is_err());
                    owner.terminate().await.expect("owner ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded cross-batch history verification");
            }
        }));
    }

    #[test]
    fn workforce_role_list_requires_exact_authority_and_returns_complete_pages() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                let reader = Box::pin(register(&database, &services, &mut supplier)).await;
                let outsider = Box::pin(register_operator(&database, &services, &mut supplier)).await;
                let pagination = Pagination::new(1, 100).expect("complete page");
                assert!(matches!(services.role().list_workforce(&reader, pagination).await, Err(kernel::Error::Denied)));
                let authority = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                    "entry":"institution", "name":"Reader", "level":200, "permissions":["workforce.roles"]
                })))).await.expect("domain reader definition");
                let first = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                    "entry":"institution", "name":"Alpha", "level":700, "permissions":["workforce.numbers"]
                })))).await.expect("higher role with an unowned permission");
                let second = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                    "entry":"institution", "name":"Beta", "level":700, "permissions":[]
                })))).await.expect("same-level definition");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), pagination).await.expect("reader candidate");
                Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13800000000", &authority))).await.expect("real membership");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime reader");
                let facts = observer.client().query(r#"SELECT role.id,role.change_id,role.name,role.level,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id) AS permissions FROM access.roles role WHERE scope='workforce' ORDER BY level DESC,name COLLATE "C",id"#, &[]).await.expect("independent complete definitions");
                assert_eq!(facts.len(), 5);
                for actor in [&institution, &reader] {
                    let list = services.role().list_workforce(actor, pagination).await.expect("either exact authority");
                    assert_eq!(list.count(), 5);
                    assert_eq!(list.roles().len(), facts.len());
                    for (role, fact) in list.roles().iter().zip(&facts) {
                        assert_eq!(role.id(), fact.get("id"));
                        assert_eq!(role.change_id(), fact.get("change_id"));
                        assert_eq!(json!(role.name()), fact.get::<_, String>("name"));
                        assert_eq!(i16::from(role.level()), fact.get::<_, i16>("level"));
                        assert_eq!(json!(role.permissions()), json!(fact.get::<_, Vec<String>>("permissions")));
                    }
                    assert_eq!(list.roles().iter().map(role::Role::kind).collect::<Vec<_>>(), [Kind::Full, Kind::Custom, Kind::Custom, Kind::Custom, Kind::Default]);
                    assert_eq!((list.roles()[1].id(), list.roles()[2].id()), (first.id(), second.id()));
                    let middle = services.role().list_workforce(actor, Pagination::new(2, 2).expect("middle page")).await.expect("middle definitions");
                    assert_eq!(middle.count(), 5);
                    assert_eq!(middle.roles().iter().map(role::Role::id).collect::<Vec<_>>(), [second.id(), authority.id()]);
                    let empty = services.role().list_workforce(actor, Pagination::new(i64::MAX, 1).expect("last representable page")).await.expect("out-of-range page");
                    assert_eq!(empty.count(), 5);
                    assert!(empty.roles().is_empty());
                }
                assert!(matches!(services.role().list(&reader, pagination).await, Err(kernel::Error::Denied)), "domain read authority does not grant institution management");
                for (permission, allowed) in [("institution.roles", false), ("institution.assignments", false), ("institution.workforce", true)] {
                    let selected = Box::pin(services.role().create(institution.clone(), operation(), serde_json::from_value(json!({"name":permission,"level":2,"permissions":[permission]})).expect("institution input"))).await.expect("exact institution role");
                    let candidate = services.appointment().candidate(&institution, "13700000000".parse().expect("mobile")).await.expect("outsider candidate");
                    let identity = candidate.identity();
                    let input = serde_json::from_value(json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13700000000","assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("outsider appointment input");
                    Box::pin(services.appointment().assign(institution.clone(), operation(), input)).await.expect("real institution assignment");
                    let result = services.role().list_workforce(&outsider, pagination).await;
                    if allowed { assert_eq!(result.expect("institution authority without membership or domain rank").count(), 5); }
                    else { assert!(matches!(result, Err(kernel::Error::Denied))); }
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants", &[]).await.expect("reads never admit members").get::<_, i64>(0), 1);
                let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                owner.client().execute("UPDATE access.roles SET permissions=ARRAY[]::text[] WHERE id=$1", &[&first.id()]).await.expect("damage the sealed current definition");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                assert!(matches!(services.role().list_workforce(&reader, pagination).await, Err(error) if error.is_corrupt()), "no partial page or empty fallback");
                assert!(services.ready().await.is_err(), "observed inconsistent definitions close admission");
                owner.terminate().await.expect("owner ended");
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded complete role-list acceptance");
        }));
    }

    #[test]
    fn workforce_role_list_rechecks_live_permissions_and_membership() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                let reader = Box::pin(register(&database, &services, &mut supplier)).await;
                let manager = Box::pin(register_operator(&database, &services, &mut supplier)).await;
                let pagination = Pagination::new(1, 100).expect("page");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13700000000".parse().expect("mobile"), pagination).await.expect("manager candidate");
                let full = candidate.roles().iter().find(|role| role.kind() == Kind::Full).expect("full domain role");
                Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13700000000", full))).await.expect("domain manager");
                let definition = json!({"entry":"institution","name":"Reader","level":200,"permissions":["workforce.roles"]});
                let selected = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&definition))).await.expect("reader definition");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), pagination).await.expect("reader candidate");
                let (grant, _) = Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13800000000", &selected))).await.expect("reader membership");
                Box::pin(services.role().changes_workforce(&reader, history())).await.expect("normal member history");
                assert!(matches!(Box::pin(services.role().changes_workforce(&institution, history())).await, Err(kernel::Error::Denied)), "institution authority alone does not read history");
                let removed = Box::pin(services.role().update_workforce(institution.clone(), operation(), selected.id(), modification(&selected, &json!({"entry":"institution","name":"Reader","level":200,"permissions":[]})))).await.expect("remove live permission");
                assert!(matches!(services.role().list_workforce(&reader, pagination).await, Err(kernel::Error::Denied)));
                assert!(matches!(Box::pin(services.role().changes_workforce(&reader, history())).await, Err(kernel::Error::Denied)));
                Box::pin(services.role().update_workforce(institution.clone(), operation(), removed.id(), modification(&removed, &definition))).await.expect("restore live permission");
                services.role().list_workforce(&reader, pagination).await.expect("restored role applies");
                Box::pin(services.role().changes_workforce(&reader, history())).await.expect("restored history permission");
                let candidate = services.grant().suspension_candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("suspension candidate");
                let identity = candidate.identity();
                let target = json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000"}});
                let suspension = Box::pin(services.grant().suspend(manager.clone(), operation(), candidate.grant_id(), serde_json::from_value(target.clone()).expect("suspension input"))).await.expect("real domain suspension");
                assert!(matches!(services.role().list_workforce(&reader, pagination).await, Err(kernel::Error::Denied)), "paused membership provides no domain authority");
                assert!(matches!(Box::pin(services.role().changes_workforce(&reader, history())).await, Err(kernel::Error::Denied)));
                let authority = Box::pin(services.role().create(institution.clone(), operation(), serde_json::from_value(json!({"name":"Institution reader","level":2,"permissions":["institution.workforce"]})).expect("institution input"))).await.expect("independent authority");
                let candidate = services.appointment().candidate(&institution, "13800000000".parse().expect("mobile")).await.expect("institution candidate");
                Box::pin(services.appointment().assign(institution.clone(), operation(), crate::access::appointment::tests::command(&candidate, &authority))).await.expect("institution assignment while domain paused");
                services.role().list_workforce(&reader, pagination).await.expect("domain pause does not remove institution authority");
                assert!(matches!(Box::pin(services.role().changes_workforce(&reader, history())).await, Err(kernel::Error::Denied)), "institution authority cannot bypass domain suspension for history");
                Box::pin(services.grant().resume(manager, operation(), suspension.id(), serde_json::from_value(target).expect("resumption input"))).await.expect("real domain recovery");
                Box::pin(services.role().changes_workforce(&reader, history())).await.expect("history after domain recovery");
                Box::pin(services.grant().terminate(reader.clone(), operation(), grant.id())).await.expect("self exit");
                services.role().list_workforce(&reader, pagination).await.expect("institution authority survives domain exit");
                assert!(matches!(Box::pin(services.role().changes_workforce(&reader, history())).await, Err(kernel::Error::Denied)), "ended membership cannot read history");
                let ordinary = services.appointment().roles(&institution, pagination).await.expect("institution choices");
                let ordinary = ordinary.roles().iter().find(|role| role.kind() == Kind::Default).expect("ordinary role");
                let candidate = services.appointment().candidate(&institution, "13800000000".parse().expect("mobile")).await.expect("remove institution authority");
                Box::pin(services.appointment().assign(institution.clone(), operation(), crate::access::appointment::tests::command(&candidate, ordinary))).await.expect("ordinary institution role");
                assert!(matches!(services.role().list_workforce(&reader, pagination).await, Err(kernel::Error::Denied)), "ended membership cannot authorize a later request");
                services.revocation().revoke(reader.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("revoke current session");
                assert!(matches!(services.role().list_workforce(&reader, pagination).await, Err(kernel::Error::Unauthenticated)), "session authentication precedes both permission paths");
                assert!(matches!(Box::pin(services.role().changes_workforce(&reader, history())).await, Err(kernel::Error::Unauthenticated)));
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded live role-list authority");
        }));
    }

    #[test]
    fn workforce_role_list_binds_authority_count_and_definitions_to_one_snapshot() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (domain, retry) in [(false, true), (false, false), (true, true), (true, false)] {
                timeout(Duration::from_secs(35), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(crate::access::role::tests::start_snapshot(authentication::proxied(&database, &proxy), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let member = Box::pin(register(&database, &services, &mut supplier)).await;
                    let pagination = Pagination::new(1, 100).expect("page");
                    let selected = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Reader","level":200,"permissions":["workforce.roles"]})))).await.expect("reader definition");
                    let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), pagination).await.expect("candidate");
                    Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate, "institution", "13800000000", &selected))).await.expect("reader membership");
                    let reader = if domain { member.clone() } else { institution.clone() };
                    proxy.arm(if retry { Fault::StalledRoleStates } else { Fault::PausedReadEnd });
                    let service = services.role().clone();
                    let actor = reader.clone();
                    let task = tokio::spawn(async move { service.list_workforce(&actor, pagination).await });
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("before state validation or after the validated snapshot ends");
                    let added = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Later","level":400,"permissions":[]})))).await.expect("concurrent definition");
                    if domain {
                        Box::pin(services.role().update_workforce(institution.clone(), operation(), selected.id(), modification(&selected, &json!({"entry":"institution","name":"Reader","level":200,"permissions":[]})))).await.expect("concurrent domain permission removal");
                    } else {
                        services.revocation().revoke(institution.clone(), operation(), crate::identity::revocation::Scope::Current).await.expect("concurrent session loss");
                    }
                    proxy.release();
                    let result = task.await.expect("reader task");
                    if retry {
                        if domain { assert!(matches!(result, Err(kernel::Error::Denied)), "a changed anchor restarts membership authorization"); }
                        else { assert!(matches!(result, Err(kernel::Error::Unauthenticated)), "a changed anchor rechecks the session"); }
                    } else {
                        let list = result.expect("original validated snapshot");
                        assert_eq!(list.count(), 3);
                        assert_eq!(list.roles().len(), 3);
                        assert!(!list.roles().iter().any(|role| role.id() == added.id()));
                        let original = list.roles().iter().find(|role| role.id() == selected.id()).expect("original definition");
                        assert_eq!(original.change_id(), selected.change_id());
                        assert_eq!(original.permissions(), &[role::Code::WorkforceRoles]);
                    }
                    let error = services.role().list_workforce(&reader, pagination).await.err().expect("new snapshot uses changed authority");
                    if domain { assert!(matches!(error, kernel::Error::Denied)); }
                    else { assert!(matches!(error, kernel::Error::Unauthenticated)); }
                    let latest = services.role().list_workforce(if domain { &institution } else { &member }, pagination).await.expect("fresh authorized snapshot");
                    assert_eq!(latest.count(), 4);
                    assert!(latest.roles().iter().any(|role| role.id() == added.id()));
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended"); proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded role-list snapshot");
            }
        }));
    }

    #[test]
    fn workforce_input_requires_explicit_authority_and_scoped_permissions() {
        let valid = json!({"entry":"domain","name":" Cafe\u{301} ","level":200,"permissions":["workforce.roles","workforce.admission"]});
        let parsed = input(&valid);
        assert_eq!(
            serde_json::to_value(&parsed.name).expect("normalized name"),
            "Café"
        );
        assert_eq!(
            parsed.permissions.values(),
            [role::Code::WorkforceAdmission, role::Code::WorkforceRoles]
        );
        for (field, value) in [
            ("entry", json!(null)),
            ("entry", json!("fallback")),
            ("level", json!(0)),
            ("level", json!(1001)),
            ("level", json!(1.5)),
            ("name", json!(" ")),
            ("name", json!("a\u{0000}b")),
            ("permissions", json!(["institution.workforce"])),
            ("permissions", json!(["workforce.roles", "workforce.roles"])),
            ("permissions", json!(["workforce.unknown"])),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<Input>(invalid).is_err(), "{field}");
        }
        for field in ["entry", "name", "level", "permissions"] {
            let mut invalid = valid.clone();
            invalid.as_object_mut().expect("input object").remove(field);
            assert!(
                serde_json::from_value::<Input>(invalid).is_err(),
                "missing {field}"
            );
        }
        let mut unknown = valid.clone();
        unknown["kind"] = json!("custom");
        assert!(serde_json::from_value::<Input>(unknown).is_err());
        for level in [1, 1000] {
            let mut system = valid.clone();
            system["level"] = json!(level);
            assert!(
                serde_json::from_value::<Input>(system).is_ok(),
                "business qualification owns system levels"
            );
        }
    }

    #[test]
    fn deletion_requires_an_explicit_authority_and_typed_confirmation() {
        let valid = json!({"entry":"domain","changeId":Uuid::new_v4()});
        assert!(serde_json::from_value::<Delete>(valid.clone()).is_ok());
        for value in [
            json!({"entry":"domain"}),
            json!({"changeId":Uuid::new_v4()}),
            json!({"entry":"fallback","changeId":Uuid::new_v4()}),
            json!({"entry":"institution","changeId":"invalid"}),
            json!({"entry":"institution","changeId":null}),
            json!({"entry":"domain","changeId":Uuid::new_v4(),"roleId":Uuid::new_v4()}),
        ] {
            assert!(serde_json::from_value::<Delete>(value).is_err());
        }
    }

    #[test]
    fn modification_requires_the_complete_scoped_definition_and_confirmation() {
        let valid = json!({"entry":"domain","changeId":Uuid::new_v4(),"name":"Updated","level":200,"permissions":["workforce.roles"]});
        assert!(serde_json::from_value::<Update>(valid.clone()).is_ok());
        for field in ["entry", "changeId", "name", "level", "permissions"] {
            let mut missing = valid.clone();
            missing.as_object_mut().expect("object").remove(field);
            assert!(
                serde_json::from_value::<Update>(missing).is_err(),
                "{field}"
            );
        }
        for (field, value) in [
            ("changeId", json!("invalid")),
            ("entry", json!("fallback")),
            ("permissions", json!(["institution.roles"])),
            ("permissions", json!(["workforce.roles", "workforce.roles"])),
            ("name", json!(" ")),
            ("level", json!(0)),
            ("level", json!(1001)),
            ("extra", json!(true)),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(
                serde_json::from_value::<Update>(invalid).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn workforce_modification_preserves_domain_qualification_and_original_history() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45),Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                let institution=Box::pin(actor(&database,&services,&mut supplier)).await;let manager=Box::pin(register(&database,&services,&mut supplier)).await;
                let authority=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"Maintainer","level":300,"permissions":["workforce.roles","workforce.admission"]})))).await.expect("domain authority");
                let original_creation=operation();let body=json!({"entry":"institution","name":"Original","level":200,"permissions":["workforce.numbers"]});
                let created=Box::pin(services.role().create_workforce(institution.clone(),original_creation,input(&body))).await.expect("target with unowned permission");
                let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",&authority))).await.expect("domain maintainer");
                let changed=json!({"entry":"domain","name":" Cafe\u{301} ","level":201,"permissions":["workforce.roles","workforce.numbers"]});
                let original=operation();
                let updated=Box::pin(services.role().update_workforce(manager.clone(),original,created.id(),modification(&created,&changed))).await.expect("retained unowned permission and newly owned permission");
                assert_eq!(serde_json::to_value(updated.name()).expect("canonical name"),"Café");
                for level in [1,300,1000] {
                    let invalid=json!({"entry":"domain","name":"Rejected","level":level,"permissions":[]});
                    assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),operation(),updated.id(),modification(&updated,&invalid))).await,Err(kernel::Error::Denied)));
                }
                let elevated=json!({"entry":"institution","name":"Higher","level":400,"permissions":[]});
                let higher=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&elevated))).await.expect("higher target");
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),operation(),higher.id(),modification(&higher,&json!({"entry":"domain","name":"Lowered","level":100,"permissions":[]})))).await,Err(kernel::Error::Denied)),"old rank also matters");
                let removed=json!({"entry":"domain","name":"Reduced","level":202,"permissions":[]});
                let reduced=Box::pin(services.role().update_workforce(manager.clone(),operation(),updated.id(),modification(&updated,&removed))).await.expect("removing unowned permissions");
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),operation(),reduced.id(),modification(&reduced,&changed))).await,Err(kernel::Error::Denied)),"removed unowned permission cannot be added back");
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),operation(),reduced.id(),modification(&reduced,&removed))).await,Err(kernel::Error::Rejected(Rejection::Unchanged))));
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),operation(),reduced.id(),modification(&created,&removed))).await,Err(kernel::Error::Rejected(Rejection::Changed))));
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                let permission=Box::pin(services.role().create(institution.clone(),operation(),serde_json::from_value(json!({"name":"Institution maintainer","level":2,"permissions":["institution.workforce"]})).expect("input"))).await.expect("institution authority");
                let candidate=services.appointment().candidate(&institution,"13800000000".parse().expect("mobile")).await.expect("candidate");
                Box::pin(services.appointment().assign(institution.clone(),operation(),crate::access::appointment::tests::command(&candidate,&permission))).await.expect("second authority");
                let mut mismatch=changed.clone();mismatch["entry"]=json!("institution");
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),original,created.id(),modification(&created,&mismatch))).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),original,higher.id(),modification(&created,&changed))).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().update_workforce(manager.clone(),original,created.id(),modification(&updated,&changed))).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),original,reduced.id(),deletion("domain",&reduced))).await,Err(kernel::Error::Mismatch)));
                Box::pin(services.role().delete_workforce(manager.clone(),operation(),reduced.id(),deletion("domain",&reduced))).await.expect("terminal history");
                assert_eq!(Box::pin(services.role().update_workforce(manager.clone(),original,created.id(),modification(&created,&changed))).await.expect("original modification after deletion").change_id(),updated.change_id());
                assert_eq!(Box::pin(services.role().create_workforce(institution.clone(),original_creation,input(&body))).await.expect("original creation after modification and deletion").change_id(),created.change_id());
                crate::access::verification::verify(observer.client(), services.app(),context.sample().await.expect("time")).await.expect("valid complete histories");
                services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");
                let (context,services)=Box::pin(start(database.connections(&database.user),&supplier)).await;
                assert_eq!(Box::pin(services.role().update_workforce(manager,original,created.id(),modification(&created,&changed))).await.expect("original after restart").change_id(),updated.change_id());
                services.close();services.drain().await.expect("restart drained");context.stop().await.expect("restart ended");observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded live modification and history");
        }));
    }

    #[test]
    fn workforce_modification_changes_live_permissions_without_rewriting_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(35),Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let institution=actor(&database,&services,&mut supplier).await;let manager=register(&database,&services,&mut supplier).await;
                let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                let full=candidate.roles().iter().find(|role|role.kind()==Kind::Full).expect("full");
                Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",full))).await.expect("domain authority");
                let admitted=Box::pin(services.role().create_workforce(manager.clone(),operation(),input(&json!({"entry":"domain","name":"Holder","level":100,"permissions":[]})))).await.expect("holder role");
                let candidate=services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("holder candidate");
                let admission_id=operation();let (grant,assignment)=Box::pin(services.grant().create(manager.clone(),admission_id,admission(&candidate,"domain","13900000000",&admitted))).await.expect("real holder");
                assert!(matches!(Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"domain","name":"Before","level":50,"permissions":[]})))).await,Err(kernel::Error::Denied)));
                let live=Box::pin(services.role().update_workforce(manager.clone(),operation(),admitted.id(),modification(&admitted,&json!({"entry":"domain","name":"Live maintainer","level":150,"permissions":["workforce.roles"]})))).await.expect("change held definition");
                assert_eq!(services.grant().read_role(&institution).await.expect("live role").change_id(),live.change_id());
                let subordinate=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"domain","name":"Now authorized","level":50,"permissions":[]})))).await.expect("new permission applies immediately");
                let cleared=Box::pin(services.role().update_workforce(manager.clone(),operation(),live.id(),modification(&live,&json!({"entry":"domain","name":"No maintenance","level":150,"permissions":[]})))).await.expect("remove live permission");
                assert!(matches!(Box::pin(services.role().delete_workforce(institution.clone(),operation(),subordinate.id(),deletion("domain",&subordinate))).await,Err(kernel::Error::Denied)));
                assert_eq!(Box::pin(services.grant().create(manager,admission_id,admission(&candidate,"domain","13900000000",&admitted))).await.expect("admission retains old definition").1.id(),assignment.id());
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                let row=observer.client().query_one("SELECT (SELECT change_id FROM access.roles WHERE id=$1) AS current,(SELECT count(*) FROM access.permissions WHERE role_id=$1) AS permissions,(SELECT count(*) FROM access.entries WHERE change_id=$2) AS original_permissions,(SELECT role_id FROM access.assignments WHERE id=$3) AS assigned",&[&cleared.id(),&live.change_id(),&assignment.id()]).await.expect("independent live and historical facts");
                assert_eq!(row.get::<_,role::change::Id>("current"),cleared.change_id());assert_eq!(row.get::<_,i64>("permissions"),0);assert_eq!(row.get::<_,i64>("original_permissions"),1);assert_eq!(row.get::<_,role::Id>("assigned"),admitted.id());
                assert_eq!(services.grant().read_latest(&institution).await.expect("membership remains").id(),grant.id());
                crate::access::verification::verify(observer.client(), services.app(),context.sample().await.expect("time")).await.expect("valid current and historical definitions");
                services.close();services.drain().await.expect("drained");context.stop().await.expect("ended");observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded live permission changes");
        }));
    }

    #[test]
    fn workforce_modification_checks_every_holder_including_paused_members() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40),Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let institution=actor(&database,&services,&mut supplier).await;let manager=register(&database,&services,&mut supplier).await;
                let authority=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"Domain administrator","level":999,"permissions":["workforce.admission","workforce.roles","workforce.suspension"]})))).await.expect("domain authority");
                let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",&authority))).await.expect("domain manager");
                let role=Box::pin(services.role().create_workforce(manager.clone(),operation(),input(&json!({"entry":"domain","name":"Held","level":900,"permissions":[]})))).await.expect("high domain role");
                let command=json!({"entry":"institution","name":"Maintained","level":950,"permissions":["workforce.numbers"]});
                let candidate=services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("administrator candidate");
                let (held,_)=Box::pin(services.grant().create(manager.clone(),operation(),admission(&candidate,"domain","13900000000",&role))).await.expect("administrator holds role");
                assert!(matches!(Box::pin(services.role().update_workforce(institution.clone(),operation(),role.id(),modification(&role,&command))).await,Err(kernel::Error::Denied)),"institution entry cannot modify its own role");
                let candidate=services.grant().suspension_candidate(&manager,"13900000000".parse().expect("mobile")).await.expect("pause candidate");let identity=candidate.identity();
                Box::pin(services.grant().suspend(manager.clone(),operation(),held.id(),serde_json::from_value(json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13900000000"}})).expect("pause"))).await.expect("actual domain pause");
                assert!(matches!(Box::pin(services.role().update_workforce(institution.clone(),operation(),role.id(),modification(&role,&command))).await,Err(kernel::Error::Denied)),"paused holder still participates");
                let other=register_operator(&database,&services,&mut supplier).await;
                let candidate=services.grant().candidate(&manager,Entry::Domain,"13700000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("ordinary holder");
                Box::pin(services.grant().create(manager.clone(),operation(),admission(&candidate,"domain","13700000000",&role))).await.expect("second lower institutional holder");
                assert!(matches!(Box::pin(services.role().update_workforce(institution.clone(),operation(),role.id(),modification(&role,&command))).await,Err(kernel::Error::Denied)),"one lower holder cannot hide the other");
                Box::pin(services.grant().terminate(institution.clone(),operation(),held.id())).await.expect("administrator exits");
                let updated=Box::pin(services.role().update_workforce(institution.clone(),operation(),role.id(),modification(&role,&command))).await.expect("all remaining holders are lower");
                assert_eq!(services.grant().read_role(&other).await.expect("current definition").change_id(),updated.change_id());
                let observer=database.connections(&database.user).recover().await.expect("observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.grants WHERE termination IS NULL AND role_id=$1",&[&role.id()]).await.expect("one remaining holder").get::<_,i64>(0),1);
                services.close();services.drain().await.expect("drained");context.stop().await.expect("ended");observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded holder qualification");
        }));
    }

    #[test]
    fn workforce_modification_serializes_institutional_holder_assignments_in_both_orders() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for assignment_first in [true,false] {
                timeout(Duration::from_secs(40),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                    let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let institution=actor(&database,&services,&mut supplier).await;register(&database,&services,&mut supplier).await;
                    let manager=register_operator(&database,&services,&mut supplier).await;
                    let authority=Box::pin(services.role().create(institution.clone(),operation(),serde_json::from_value(json!({"name":"Institution maintainer","level":300,"permissions":["institution.workforce"]})).expect("input"))).await.expect("authority");
                    let higher=Box::pin(services.role().create(institution.clone(),operation(),serde_json::from_value(json!({"name":"Higher holder","level":400,"permissions":[]})).expect("input"))).await.expect("higher role");
                    let candidate=services.appointment().candidate(&institution,"13700000000".parse().expect("mobile")).await.expect("operator candidate");let identity=candidate.identity();
                    Box::pin(services.appointment().assign(institution.clone(),operation(),serde_json::from_value(json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13700000000","assignmentId":candidate.assignment_id()},"role":{"id":authority.id(),"changeId":authority.change_id()}})).expect("operator appointment"))).await.expect("institution manager without domain membership");
                    let created=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"Concurrent","level":900,"permissions":[]})))).await.expect("domain definition");
                    let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("holder candidate");
                    Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",&created))).await.expect("current holder");
                    let candidate=services.appointment().candidate(&institution,"13800000000".parse().expect("mobile")).await.expect("promotion candidate");
                    let changed=json!({"entry":"institution","name":"Changed","level":950,"permissions":["workforce.numbers"]});
                    let appointment_id=operation();let update_id=operation();let id=created.id();
                    let observer=database.connections(&database.user).recover().await.expect("observer");
                    let roles=services.role().clone();let appointments=services.appointment().clone();let actor=institution.clone();let operator=manager.clone();
                    let update=modification(&created,&changed);let assign=crate::access::appointment::tests::command(&candidate,&higher);
                    proxy.arm(Fault::DelayedCommit);
                    let first=tokio::spawn(async move {
                        if assignment_first {Box::pin(appointments.assign(actor,appointment_id,assign)).await.expect("assignment first");}
                        else {Box::pin(roles.update_workforce(operator,update_id,id,update)).await.expect("update before holder promotion");}
                    });
                    let blocker=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("first actual commit");
                    let roles=services.role().clone();let appointments=services.appointment().clone();let operator=manager;
                    let update=modification(&created,&changed);let assign=crate::access::appointment::tests::command(&candidate,&higher);
                    let second=tokio::spawn(async move {
                        if assignment_first {assert!(matches!(Box::pin(roles.update_workforce(operator,update_id,id,update)).await,Err(kernel::Error::Denied)),"fresh institutional rank after holder lock");}
                        else {Box::pin(appointments.assign(institution,appointment_id,assign)).await.expect("holder promotion after update");}
                    });
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND (query LIKE 'SELECT access.lock_role_holders%' OR query LIKE 'UPDATE access.holders%' OR query LIKE 'SELECT%FROM access.holders%FOR UPDATE%'))",&[&blocker]).await.expect("exact holder conflict").get::<_,bool>(0) {break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("business operations coordinate on current holder");
                    proxy.release();first.await.expect("first worker");second.await.expect("second worker");
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM access.changes WHERE operation_id=$1) AS updates,(SELECT role_id FROM access.holders WHERE person_id=$2) AS holder,(SELECT change_id FROM access.roles WHERE id=$3) AS definition",&[&update_id,&candidate.identity().person_id(),&id]).await.expect("independent outcomes");
                    assert_eq!(row.get::<_,i64>("updates"),i64::from(!assignment_first));assert_eq!(row.get::<_,role::Id>("holder"),higher.id());
                    assert_eq!(row.get::<_,role::change::Id>("definition")==created.change_id(),assignment_first);
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded concurrent holder promotion");
            }
        }));
    }

    #[test]
    fn workforce_modification_recovers_atomic_definitions_and_serializes_originals() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in ["institution","domain"] {
                for fault in [Fault::LostCommit,Fault::RolledBack,Fault::UnknownCommit] {
                    timeout(Duration::from_secs(35),Box::pin(async {
                        let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                        let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                        let institution=actor(&database,&services,&mut supplier).await;
                        let operator=if entry=="institution" {institution} else {
                            let manager=register(&database,&services,&mut supplier).await;
                            let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("candidate");
                            let full=candidate.roles().iter().find(|role|role.kind()==Kind::Full).expect("full");
                            Box::pin(services.grant().create(institution,operation(),admission(&candidate,"institution","13800000000",full))).await.expect("domain authority");manager
                        };
                        let created=Box::pin(services.role().create_workforce(operator.clone(),operation(),input(&json!({"entry":entry,"name":"Before","level":200,"permissions":["workforce.numbers"]})))).await.expect("target");
                        let body=json!({"entry":entry,"name":"After","level":201,"permissions":["workforce.roles"]});let original=operation();
                        let observer=database.connections(&database.user).recover().await.expect("observer");
                        let rolled_back=matches!(fault,Fault::RolledBack);let unknown=matches!(fault,Fault::UnknownCommit);
                        proxy.arm(fault);
                        let result=Box::pin(services.role().update_workforce(operator.clone(),original,created.id(),modification(&created,&body))).await;
                        timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual update commit");
                        if rolled_back {assert!(matches!(&result,Err(kernel::Error::Unavailable(fault)) if fault.stage()==kernel::Stage::Commit && matches!(fault.kind(),kernel::FaultKind::Query(_))),"{:?}",result.as_ref().err());}
                        else if unknown {assert!(matches!(&result,Err(kernel::Error::Unknown(fault)) if fault.stage()==kernel::Stage::Commit && matches!(fault.kind(),kernel::FaultKind::Query(_))),"{:?}",result.as_ref().err());}
                        else {result.expect("lost response recovered");}
                        let row=observer.client().query_one("SELECT role.name,role.change_id,(SELECT count(*) FROM access.changes WHERE operation_id=$2) AS updates,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id) AS permissions,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=role.change_id ORDER BY capability_id) AS entries FROM access.roles role WHERE id=$1",&[&created.id(),&original]).await.expect("independent complete graph before release or replay");
                        let committed=!rolled_back&&!unknown;assert_eq!(row.get::<_,String>("name"),if committed {"After"} else {"Before"});assert_eq!(row.get::<_,i64>("updates"),i64::from(committed));
                        assert_eq!(row.get::<_,Vec<String>>("permissions"),if committed {vec!["workforce.roles"]}else{vec!["workforce.numbers"]});assert_eq!(row.get::<_,Vec<String>>("entries"),row.get::<_,Vec<String>>("permissions"));
                        assert_eq!(row.get::<_,role::change::Id>("change_id")==created.change_id(),!committed);
                        proxy.release();
                        if unknown {timeout(Duration::from_secs(3),async {loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.changes WHERE operation_id=$1)",&[&original]).await.expect("original delayed commit").get::<_,bool>(0){break;}
                            tokio::task::yield_now().await;
                        }}).await.expect("original becomes durable before replay");}
                        let (first,second)=tokio::join!(services.role().update_workforce(operator.clone(),original,created.id(),modification(&created,&body)),services.role().update_workforce(operator.clone(),original,created.id(),modification(&created,&body)));
                        let current=first.expect("first original waiter");assert_eq!(current.change_id(),second.expect("second original waiter").change_id());
                        let operations=[operation(),operation()];let changed=json!({"entry":entry,"name":"Competing","level":202,"permissions":[]});
                        let (first,second)=tokio::join!(services.role().update_workforce(operator.clone(),operations[0],current.id(),modification(&current,&changed)),services.role().update_workforce(operator,operations[1],current.id(),modification(&current,&changed)));
                        assert!(matches!((&first,&second),(Ok(_),Err(kernel::Error::Rejected(Rejection::Changed)))|(Err(kernel::Error::Rejected(Rejection::Changed)),Ok(_))));
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1",&[&created.id()]).await.expect("creation and exactly two complete updates").get::<_,i64>(0),3);
                        services.close();services.drain().await.expect("drained");context.stop().await.expect("ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                        assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                    })).await.expect("bounded modification recovery");
                }
            }
        }));
    }

    #[test]
    fn accepted_workforce_modification_recovers_its_fixed_operator_after_session_loss() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in ["institution","domain"] {
                for damaged in [false,true] {
                    timeout(Duration::from_secs(35),Box::pin(async {
                        let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                        let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                        let institution=actor(&database,&services,&mut supplier).await;
                        let operator=if entry=="institution" {institution} else {
                            let manager=register(&database,&services,&mut supplier).await;
                            let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("candidate");
                            let full=candidate.roles().iter().find(|role|role.kind()==Kind::Full).expect("full");
                            Box::pin(services.grant().create(institution,operation(),admission(&candidate,"institution","13800000000",full))).await.expect("domain authority");manager
                        };
                        let created=Box::pin(services.role().create_workforce(operator.clone(),operation(),input(&json!({"entry":entry,"name":"Before","level":200,"permissions":[]})))).await.expect("target");
                        let body=json!({"entry":entry,"name":"Accepted","level":201,"permissions":["workforce.roles"]});let original=operation();
                        let identity=services.current().read(&operator).await.expect("accepted identity");
                        let request=services.role().update_workforce(operator.clone(),original,created.id(),modification(&created,&body));
                        let result=if damaged {Box::pin(crate::recovery::after_revision_loss(&database,&proxy,&operator,request)).await}
                            else {Box::pin(crate::recovery::after_revocation(&database,&proxy,services.revocation(),&operator,request)).await};
                        let observer=database.connections(&database.user).recover().await.expect("observer");
                        let row=observer.client().query_one("SELECT change.id,change.operator_id,change.entry,role.change_id,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=change.id) AS entries,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id) AS permissions FROM access.changes change JOIN access.roles role ON role.id=change.role_id WHERE change.operation_id=$1 AND change.action='update'",&[&original]).await.expect("independent accepted graph before replay");
                        assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"),identity.revision_id());assert_eq!(row.get::<_,String>("entry"),entry);assert_eq!(row.get::<_,role::change::Id>("id"),row.get::<_,role::change::Id>("change_id"));assert_eq!(row.get::<_,Vec<String>>("entries"),["workforce.roles"]);assert_eq!(row.get::<_,Vec<String>>("permissions"),["workforce.roles"]);
                        if damaged {let error=result.err().expect("missing fixed identity fails recovery");assert!(matches!(&error,kernel::Error::Unknown(fault) if fault.later().any(|(stage,cause)| stage==kernel::Stage::Recovery && matches!(cause,kernel::FaultKind::Revision(revision::ReadError::Missing)))),"{error:?}");assert!(services.ready().await.is_err());}
                        else {assert_eq!(result.expect("accepted update survives revocation").change_id(),row.get("id"));assert!(matches!(Box::pin(services.role().update_workforce(operator,original,created.id(),modification(&created,&body))).await,Err(kernel::Error::Unauthenticated)));}
                        services.close();services.drain().await.expect("drained");context.stop().await.expect("ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                        if !damaged {let (context,services)=start(database.connections(&database.user),&supplier).await;services.ready().await.expect("valid restart");services.close();services.drain().await.expect("restart drained");context.stop().await.expect("restart ended");}
                        assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                    })).await.expect("bounded accepted update");
                }
            }
        }));
    }

    #[test]
    fn workforce_modification_rejections_leave_no_partial_sealed_facts() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                let authority = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Maintainer","level":300,"permissions":["workforce.roles","workforce.admission","workforce.numbers"]})))).await.expect("actual limited authority");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), Pagination::new(1,20).expect("page")).await.expect("candidate");
                Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate,"institution","13800000000",&authority))).await.expect("actual manager membership");
                let target = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Target","level":200,"permissions":["workforce.templates"]})))).await.expect("target retains an unowned permission");
                let higher = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Higher","level":400,"permissions":[]})))).await.expect("higher target");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("complete initial facts").get(0);
                for case in ["missing membership", "wrong authority", "old rank", "new rank", "unchanged", "added permission", "stale confirmation"] {
                    let selected = if case == "old rank" { &higher } else { &target };
                    let acting = if case == "missing membership" { &institution } else { &manager };
                    let entry = if case == "wrong authority" { "institution" } else { "domain" };
                    let name = if case == "unchanged" { "Target" } else { "Updated" };
                    let level = if case == "new rank" { 300 } else if case == "unchanged" { 200 } else { 201 };
                    let permissions = if case == "unchanged" { json!(["workforce.templates"]) } else if case == "added permission" { json!(["workforce.templates","workforce.production"]) } else { json!(["workforce.templates","workforce.numbers"]) };
                    let mut command = json!({"entry":entry,"changeId":selected.change_id(),"name":name,"level":level,"permissions":permissions});
                    if case == "stale confirmation" { command["changeId"] = json!(higher.change_id()); }
                    let result = Box::pin(services.role().update_workforce(acting.clone(), operation(), selected.id(), serde_json::from_value(command).expect("typed modification"))).await;
                    if case == "unchanged" { assert!(matches!(&result, Err(kernel::Error::Rejected(Rejection::Unchanged))), "{case}: {:?}", result.as_ref().err()); }
                    else if case == "stale confirmation" { assert!(matches!(&result, Err(kernel::Error::Rejected(Rejection::Changed))), "{case}: {:?}", result.as_ref().err()); }
                    else { assert!(matches!(&result, Err(kernel::Error::Denied)), "{case}: {:?}", result.as_ref().err()); }
                    assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("rejection leaves all role facts intact").get::<_, String>(0), before, "{case}");
                }
                let original = operation();
                let updated = Box::pin(services.role().update_workforce(manager.clone(), original, target.id(), modification(&target, &json!({"entry":"domain","name":"Updated","level":201,"permissions":["workforce.templates","workforce.numbers"]})))).await.expect("retain unowned permission and add an owned permission");
                let row = observer.client().query_one("SELECT state.active,state.sequence,state.version,record.previous_id,record.entry,record.action,completion.target,ROW(state.name,state.level,state.permissions,state.sequence,state.changed_at) IS NOT DISTINCT FROM ROW(record.name,record.level,record.permissions,record.sequence,record.changed_at) AS paired,completion.action=record.action AND completion.revision_id=record.operator_id AND completion.created_at=record.changed_at AND completion.expires_at IS NULL AS completed FROM access.roles state JOIN access.changes record ON record.id=state.change_id AND record.role_id=state.id JOIN access.completions completion ON completion.family='workforce_role' AND completion.operation_id=record.operation_id WHERE state.id=$1 AND record.operation_id=$2", &[&updated.id(), &original]).await.expect("independent complete successor and completion");
                assert!(row.get::<_, bool>("active")); assert!(row.get::<_, bool>("paired")); assert!(row.get::<_, bool>("completed"));
                assert_eq!(row.get::<_, i64>("sequence"), 2); assert_eq!(row.get::<_, i64>("version"), 2);
                assert_eq!(row.get::<_, Option<role::change::Id>>("previous_id"), Some(target.change_id()));
                assert_eq!(row.get::<_, String>("entry"), "domain"); assert_eq!(row.get::<_, String>("action"), "update");
                assert_eq!(row.get::<_, String>("target"), Uuid::from_bytes(*updated.id().as_bytes()).to_string());
                let page = services.role().changes_workforce(&manager, Filter::new(Pagination::new(1,1).expect("one record"), Some(updated.id()), Some(original))).await.expect("authenticated successor history");
                assert_eq!(page.count(), 1); assert_eq!(page.changes()[0].id(), updated.change_id());
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded Workforce modification atomicity");
        }));
    }

    #[test]
    fn workforce_modification_serializes_both_orders_of_real_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for admission_first in [true,false] {
                timeout(Duration::from_secs(30),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                    let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let institution=actor(&database,&services,&mut supplier).await;let member=register(&database,&services,&mut supplier).await;
                    let created=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"Before","level":200,"permissions":[]})))).await.expect("selected definition");
                    let changed=json!({"entry":"institution","name":"After","level":201,"permissions":["workforce.numbers"]});
                    let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("admission candidate");
                    let observer=database.connections(&database.user).recover().await.expect("observer");let admission_id=operation();let update_id=operation();let id=created.id();
                    let roles=services.role().clone();let grants=services.grant().clone();let actor=institution.clone();let update=modification(&created,&changed);let admit=admission(&candidate,"institution","13800000000",&created);
                    proxy.arm(Fault::DelayedCommit);
                    let first=tokio::spawn(async move {
                        if admission_first {Box::pin(grants.create(actor,admission_id,admit)).await.expect("admission first");}
                        else {Box::pin(roles.update_workforce(actor,update_id,id,update)).await.expect("update first");}
                    });
                    let blocker=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("first actual commit");
                    let roles=services.role().clone();let grants=services.grant().clone();let update=modification(&created,&changed);let admit=admission(&candidate,"institution","13800000000",&created);
                    let second=tokio::spawn(async move {
                        if admission_first {Box::pin(roles.update_workforce(institution,update_id,id,update)).await.expect("update sees new lower holder");}
                        else {assert!(matches!(Box::pin(grants.create(institution,admission_id,admit)).await,Err(grant::Error::RoleChanged)));}
                    });
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_locks waiter WHERE waiter.locktype='advisory' AND waiter.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND waiter.classid=3744913638::oid AND waiter.objid=3229083222::oid AND waiter.objsubid=1 AND NOT waiter.granted AND $1=ANY(pg_blocking_pids(waiter.pid)))",&[&blocker]).await.expect("exact Workforce scope wait").get::<_,bool>(0){break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("operations serialize on domain scope");
                    proxy.release();first.await.expect("first worker");second.await.expect("second worker");
                    let row=observer.client().query_one("SELECT (SELECT count(*) FROM access.changes WHERE operation_id=$1) AS updates,(SELECT count(*) FROM access.grants WHERE operation_id=$2) AS admissions",&[&update_id,&admission_id]).await.expect("independent exact outcomes");assert_eq!(row.get::<_,i64>("updates"),1);assert_eq!(row.get::<_,i64>("admissions"),i64::from(admission_first));
                    if admission_first {assert_eq!(serde_json::to_value(services.grant().read_role(&member).await.expect("current updated role").name()).expect("name"),"After");}
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded admission competition");
            }
        }));
    }

    #[test]
    fn workforce_deletion_preserves_authority_rank_and_original_binding() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                let authority = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                    "entry":"institution","name":"Maintainer","level":300,"permissions":["workforce.roles"]
                })))).await.expect("limited domain authority");
                let created = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({
                    "entry":"institution","name":"Unused","level":200,"permissions":["workforce.numbers"]
                })))).await.expect("target outside the manager's permission subset");
                assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),operation(),created.id(),deletion("institution",&created))).await,Err(kernel::Error::Denied)));
                assert!(matches!(Box::pin(services.role().delete_workforce(institution.clone(),operation(),created.id(),deletion("domain",&created))).await,Err(kernel::Error::Denied)));
                let candidate = services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",&authority))).await.expect("current domain authority");
                for selected in candidate.roles().iter().filter(|role| role.id()!=created.id()) {
                    assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),operation(),selected.id(),deletion("domain",selected))).await,Err(kernel::Error::Denied)),"equal and system roles cannot be deleted");
                }
                let wrong: Delete = serde_json::from_value(json!({"entry":"domain","changeId":Uuid::new_v4()})).expect("stale confirmation");
                assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),operation(),created.id(),wrong)).await,Err(kernel::Error::Rejected(Rejection::Changed))));
                let original = operation();
                let deleted = Box::pin(services.role().delete_workforce(manager.clone(),original,created.id(),deletion("domain",&created))).await.expect("deletion needs no permission subset");
                let permission = Box::pin(services.role().create(institution.clone(),operation(),serde_json::from_value(json!({"name":"Cross-scope manager","level":2,"permissions":["institution.workforce"]})).expect("institutional input"))).await.expect("low institutional role");
                let candidate = services.appointment().candidate(&institution,"13800000000".parse().expect("mobile")).await.expect("appointment candidate");
                Box::pin(services.appointment().assign(institution.clone(),operation(),crate::access::appointment::tests::command(&candidate,&permission))).await.expect("current institutional authority");
                assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),original,created.id(),deletion("institution",&created))).await,Err(kernel::Error::Mismatch)),"both authorities pass but source remains bound");
                assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),original,authority.id(),deletion("domain",&authority))).await,Err(kernel::Error::Mismatch)),"original comparison precedes new target eligibility");
                let higher = Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"Higher","level":900,"permissions":[]})))).await.expect("higher domain role");
                assert!(matches!(Box::pin(services.role().delete_workforce(manager.clone(),operation(),higher.id(),deletion("domain",&higher))).await,Err(kernel::Error::Denied)));
                Box::pin(services.role().delete_workforce(manager.clone(),operation(),higher.id(),deletion("institution",&higher))).await.expect("institution level 2 does not compare with domain level 900");
                assert_eq!(Box::pin(services.role().delete_workforce(manager.clone(),original,created.id(),deletion("domain",&created))).await.expect("original survives other maintenance").id(),deleted.id());
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE scope='workforce' AND action='delete'",&[]).await.expect("exact committed deletions").get::<_,i64>(0),2);
                crate::access::verification::verify(observer.client(), services.app(),context.sample().await.expect("time")).await.expect("valid terminal history");
                services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded deletion authority");
        }));
    }

    #[test]
    fn workforce_deletion_retains_used_role_history_after_real_termination() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in ["institution","domain"] {
                timeout(Duration::from_secs(45),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;
                    let (context,services)=start(database.connections(&database.user),&supplier).await;
                    let institution=actor(&database,&services,&mut supplier).await;
                    let manager=register(&database,&services,&mut supplier).await;
                    let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                    let full=candidate.roles().iter().find(|role|role.kind()==Kind::Full).expect("full");
                    Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",full))).await.expect("real domain administrator");
                    let body=json!({"entry":"institution","name":"Reusable","level":200,"permissions":["workforce.numbers"]});
                    let creation=operation();
                    let created=Box::pin(services.role().create_workforce(institution.clone(),creation,input(&body))).await.expect("target definition");
                    let candidate=services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("holder candidate");
                    let admission_id=operation();let command=||admission(&candidate,"domain","13900000000",&created);
                    let (grant,assignment)=Box::pin(services.grant().create(manager.clone(),admission_id,command())).await.expect("real current holder");
                    let operator=if entry=="institution" {institution.clone()} else {manager.clone()};
                    let original=operation();
                    assert!(matches!(Box::pin(services.role().delete_workforce(operator.clone(),original,created.id(),deletion(entry,&created))).await,Err(kernel::Error::Rejected(Rejection::InUse))));
                    let candidate=services.grant().suspension_candidate(&manager,"13900000000".parse().expect("mobile")).await.expect("suspension candidate");
                    let identity=candidate.identity();
                    let pause=serde_json::from_value(json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13900000000"}})).expect("pause input");
                    Box::pin(services.grant().suspend(manager.clone(),operation(),grant.id(),pause)).await.expect("real domain pause");
                    assert!(matches!(Box::pin(services.role().delete_workforce(operator.clone(),original,created.id(),deletion(entry,&created))).await,Err(kernel::Error::Rejected(Rejection::InUse))),"suspended membership still owns the role");
                    Box::pin(services.grant().terminate(institution.clone(),operation(),grant.id())).await.expect("real self termination");
                    let deleted=Box::pin(services.role().delete_workforce(operator.clone(),original,created.id(),deletion(entry,&created))).await.expect("unused role deletion");
                    assert!(matches!(deleted.action(),role::change::Action::Delete));
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    let row=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles WHERE id=$1 AND NOT active AND change_id=$2 AND sequence=2 AND version=2 AND cardinality(permissions)=0) AS terminal,NOT EXISTS(SELECT 1 FROM access.permissions WHERE role_id=$1) AS empty,(SELECT count(*) FROM access.changes WHERE role_id=$1) AS changes,(SELECT count(*) FROM access.entries WHERE change_id=$2) AS entries,(SELECT entry FROM access.changes WHERE id=$2) AS authority,(SELECT role_id FROM access.assignments WHERE id=$3) AS assigned",&[&created.id(),&deleted.id(),&assignment.id()]).await.expect("independent terminal graph before replay");
                    assert!(row.get::<_,bool>("terminal"));assert!(row.get::<_,bool>("empty"));assert_eq!(row.get::<_,i64>("changes"),2);assert_eq!(row.get::<_,i64>("entries"),0);assert_eq!(row.get::<_,String>("authority"),entry);assert_eq!(row.get::<_,role::Id>("assigned"),created.id());
                    assert_eq!(Box::pin(services.grant().create(manager.clone(),admission_id,command())).await.expect("old admission survives deleted current role").1.id(),assignment.id());
                    assert_eq!(Box::pin(services.role().create_workforce(institution.clone(),creation,input(&body))).await.expect("original creation survives deletion").change_id(),created.change_id());
                    assert!(matches!(Box::pin(services.role().delete_workforce(operator.clone(),operation(),created.id(),deletion(entry,&created))).await,Err(kernel::Error::Rejected(Rejection::Absent))));
                    let replacement=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&body))).await.expect("name reuse");assert_ne!(replacement.id(),created.id());
                    assert_eq!(Box::pin(services.role().delete_workforce(operator.clone(),original,created.id(),deletion(entry,&created))).await.expect("original deletion after name reuse").id(),deleted.id());
                    crate::access::verification::verify(observer.client(), services.app(),context.sample().await.expect("time")).await.expect("startup accepts deleted role and original assignment");
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");
                    let (context,services)=start(database.connections(&database.user),&supplier).await;
                    assert_eq!(Box::pin(services.role().delete_workforce(operator,original,created.id(),deletion(entry,&created))).await.expect("original deletion after restart").id(),deleted.id());
                    services.close();services.drain().await.expect("restart drained");context.stop().await.expect("restart ended");observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"supplier monitoring includes replay and drain");supplier.stop().await;
                })).await.expect("bounded retained history");
            }
        }));
    }

    #[test]
    fn workforce_deletion_rejections_preserve_facts_and_commit_tombstones() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                let authority = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Maintainer","level":300,"permissions":["workforce.roles"]})))).await.expect("limited authority");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), Pagination::new(1,20).expect("page")).await.expect("candidate");
                Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate,"institution","13800000000",&authority))).await.expect("actual member");
                let created = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Target","level":200,"permissions":["workforce.numbers"]})))).await.expect("unused target with an unowned permission");
                let higher = Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&json!({"entry":"institution","name":"Higher","level":400,"permissions":[]})))).await.expect("higher target");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("complete initial facts").get(0);
                for case in ["missing membership", "wrong authority", "old rank", "current member", "stale confirmation"] {
                    let selected = match case { "current member" => &authority, "old rank" => &higher, _ => &created };
                    let acting = if matches!(case, "missing membership" | "current member") { &institution } else { &manager };
                    let entry = if matches!(case, "wrong authority" | "current member") { "institution" } else { "domain" };
                    let mut command = json!({"entry":entry,"changeId":selected.change_id()});
                    if case == "stale confirmation" { command["changeId"] = json!(higher.change_id()); }
                    let result = Box::pin(services.role().delete_workforce(acting.clone(), operation(), selected.id(), serde_json::from_value(command).expect("typed deletion"))).await;
                    if case == "current member" { assert!(matches!(&result, Err(kernel::Error::Rejected(Rejection::InUse))), "{case}: {:?}", result.as_ref().err()); }
                    else if case == "stale confirmation" { assert!(matches!(&result, Err(kernel::Error::Rejected(Rejection::Changed))), "{case}: {:?}", result.as_ref().err()); }
                    else { assert!(matches!(&result, Err(kernel::Error::Denied)), "{case}: {:?}", result.as_ref().err()); }
                    assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("no partial terminal effect").get::<_, String>(0), before, "{case}");
                }
                let original = operation();
                let deleted = Box::pin(services.role().delete_workforce(manager.clone(), original, created.id(), deletion("domain", &created))).await.expect("deletion does not require the target permissions");
                let row = observer.client().query_one("SELECT state.active,state.sequence,state.version,state.change_id,cardinality(state.permissions) AS permissions,record.previous_id,record.permissions IS NULL AS terminal,record.entry,record.action,completion.target,completion.action=record.action AND completion.revision_id=record.operator_id AND completion.created_at=record.changed_at AND completion.expires_at IS NULL AS completed FROM access.roles state JOIN access.changes record ON record.id=state.change_id AND record.role_id=state.id AND record.sequence=state.sequence AND record.changed_at=state.changed_at JOIN access.completions completion ON completion.family='workforce_role' AND completion.operation_id=record.operation_id WHERE state.id=$1 AND record.operation_id=$2", &[&created.id(), &original]).await.expect("independent complete tombstone");
                assert!(!row.get::<_, bool>("active")); assert!(row.get::<_, bool>("terminal")); assert!(row.get::<_, bool>("completed"));
                assert_eq!(row.get::<_, i32>("permissions"), 0); assert_eq!(row.get::<_, i64>("sequence"), 2); assert_eq!(row.get::<_, i64>("version"), 2);
                assert_eq!(row.get::<_, role::change::Id>("change_id"), deleted.id()); assert_eq!(row.get::<_, Option<role::change::Id>>("previous_id"), Some(created.change_id()));
                assert_eq!(row.get::<_, String>("entry"), "domain"); assert_eq!(row.get::<_, String>("action"), "delete"); assert_eq!(row.get::<_, String>("target"), Uuid::from_bytes(*created.id().as_bytes()).to_string());
                let terminal: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("complete terminal snapshot").get(0);
                assert_eq!(Box::pin(services.role().delete_workforce(manager, original, created.id(), deletion("domain", &created))).await.expect("original terminal replay").id(), deleted.id());
                assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("replay does not rewrite retained facts").get::<_, String>(0), terminal);
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded Workforce deletion atomicity");
        }));
    }

    #[test]
    fn workforce_deletion_serializes_both_orders_of_real_admission() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for admission_first in [true,false] {
                timeout(Duration::from_secs(30),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                    let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let institution=actor(&database,&services,&mut supplier).await;
                    register(&database,&services,&mut supplier).await;
                    let created=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"Concurrent","level":200,"permissions":[]})))).await.expect("selected role");
                    let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("real admission confirmation");
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    let admission_id=operation();let deletion_id=operation();
                    let role=services.role().clone();let grants=services.grant().clone();let actor=institution.clone();
                    let command=admission(&candidate,"institution","13800000000",&created);let remove=deletion("institution",&created);let id=created.id();
                    proxy.arm(Fault::DelayedCommit);
                    let first=tokio::spawn(async move {
                        if admission_first {Box::pin(grants.create(actor,admission_id,command)).await.expect("admission commits first");}
                        else {Box::pin(role.delete_workforce(actor,deletion_id,id,remove)).await.expect("deletion commits first");}
                    });
                    let blocker=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("first real operation reached COMMIT");
                    let role=services.role().clone();let grants=services.grant().clone();let actor=institution;
                    let command=admission(&candidate,"institution","13800000000",&created);let remove=deletion("institution",&created);
                    let second=tokio::spawn(async move {
                        if admission_first {assert!(matches!(Box::pin(role.delete_workforce(actor,deletion_id,id,remove)).await,Err(kernel::Error::Rejected(Rejection::InUse))));}
                        else {assert!(matches!(Box::pin(grants.create(actor,admission_id,command)).await,Err(grant::Error::RoleChanged)));}
                    });
                    let mode=if admission_first {"ExclusiveLock"} else {"ShareLock"};
                    timeout(Duration::from_secs(3),async {loop {
                        if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_locks waiter WHERE waiter.locktype='advisory' AND waiter.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND waiter.classid=3744913638::oid AND waiter.objid=3229083222::oid AND waiter.objsubid=1 AND waiter.mode=$2 AND NOT waiter.granted AND $1=ANY(pg_blocking_pids(waiter.pid)))",&[&blocker,&mode]).await.expect("independent exact Workforce scope wait").get::<_,bool>(0) {break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("competing operation waits on the original backend");
                    proxy.release();first.await.expect("first worker");second.await.expect("second worker");
                    let row=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles WHERE id=$1) AS retained,(SELECT active FROM access.roles WHERE id=$1) AS active,(SELECT sequence FROM access.roles WHERE id=$1) AS sequence,EXISTS(SELECT 1 FROM access.roles state JOIN access.changes record ON record.id=state.change_id WHERE state.id=$1 AND record.role_id=state.id AND record.sequence=state.sequence AND record.changed_at=state.changed_at) AS paired,(SELECT count(*) FROM access.grants WHERE termination IS NULL AND role_id=$1) AS members,(SELECT count(*) FROM access.grants WHERE operation_id=$2) AS admissions,(SELECT count(*) FROM access.changes WHERE operation_id=$3 AND action='delete') AS deletions",&[&id,&admission_id,&deletion_id]).await.expect("independent exclusive outcomes");
                    assert!(row.get::<_,bool>("retained"));assert_eq!(row.get::<_,bool>("active"),admission_first);assert_eq!(row.get::<_,i64>("sequence"),if admission_first {1}else{2});assert!(row.get::<_,bool>("paired"));assert_eq!(row.get::<_,i64>("members"),i64::from(admission_first));assert_eq!(row.get::<_,i64>("admissions"),i64::from(admission_first));assert_eq!(row.get::<_,i64>("deletions"),i64::from(!admission_first));
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded admission and deletion competition");
            }
        }));
    }

    #[test]
    fn workforce_deletion_recovers_terminal_facts_across_commit_failures() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit,Fault::RolledBack,Fault::UnknownCommit] {
                timeout(Duration::from_secs(30),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                    let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let actor=actor(&database,&services,&mut supplier).await;
                    let body=json!({"entry":"institution","name":"Recovered deletion","level":200,"permissions":["workforce.numbers"]});
                    let created=Box::pin(services.role().create_workforce(actor.clone(),operation(),input(&body))).await.expect("actual target");
                    let observer=database.connections(&database.user).recover().await.expect("independent observer");
                    let operation_id=operation();let rolled_back=matches!(fault,Fault::RolledBack);let unknown=matches!(fault,Fault::UnknownCommit);
                    proxy.arm(fault);
                    let result=Box::pin(services.role().delete_workforce(actor.clone(),operation_id,created.id(),deletion("institution",&created))).await;
                    timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual deletion commit fault");
                    if rolled_back {assert!(matches!(&result,Err(kernel::Error::Unavailable(fault)) if fault.stage()==kernel::Stage::Commit && matches!(fault.kind(),kernel::FaultKind::Query(_))),"{:?}",result.as_ref().err());}
                    else if unknown {assert!(matches!(&result,Err(kernel::Error::Unknown(fault)) if fault.stage()==kernel::Stage::Commit && matches!(fault.kind(),kernel::FaultKind::Query(_))),"{:?}",result.as_ref().err());}
                    else {result.expect("complete terminal fact recovered");}
                    let row=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles WHERE id=$1) AS retained,(SELECT active FROM access.roles WHERE id=$1) AS active,(SELECT sequence FROM access.roles WHERE id=$1) AS sequence,EXISTS(SELECT 1 FROM access.roles state JOIN access.changes record ON record.id=state.change_id WHERE state.id=$1 AND record.role_id=state.id AND record.sequence=state.sequence AND record.changed_at=state.changed_at) AS paired,(SELECT count(*) FROM access.permissions WHERE role_id=$1) AS permissions,(SELECT count(*) FROM access.changes WHERE operation_id=$2 AND action='delete') AS deleted,(SELECT count(*) FROM access.entries entry JOIN access.changes change ON change.id=entry.change_id WHERE change.operation_id=$2) AS entries",&[&created.id(),&operation_id]).await.expect("independent state before replay or release");
                    assert!(row.get::<_,bool>("retained"));assert_eq!(row.get::<_,bool>("active"),rolled_back||unknown);assert_eq!(row.get::<_,i64>("sequence"),if rolled_back||unknown {1}else{2});assert!(row.get::<_,bool>("paired"));assert_eq!(row.get::<_,i64>("permissions"),i64::from(rolled_back||unknown));assert_eq!(row.get::<_,i64>("deleted"),i64::from(!rolled_back&&!unknown));assert_eq!(row.get::<_,i64>("entries"),0);
                    proxy.release();
                    if unknown {
                        timeout(Duration::from_secs(3),async {loop {
                            if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.changes WHERE operation_id=$1 AND action='delete') AND EXISTS(SELECT 1 FROM access.roles state JOIN access.changes record ON record.id=state.change_id WHERE state.id=$2 AND NOT state.active AND state.sequence=2 AND state.version=2 AND cardinality(state.permissions)=0 AND record.operation_id=$1 AND record.sequence=state.sequence)",&[&operation_id,&created.id()]).await.expect("original commits before replay").get::<_,bool>(0) {break;}
                            tokio::task::yield_now().await;
                        }}).await.expect("delayed original terminal commit");
                    }
                    let (first,second)=tokio::join!(services.role().delete_workforce(actor.clone(),operation_id,created.id(),deletion("institution",&created)),services.role().delete_workforce(actor.clone(),operation_id,created.id(),deletion("institution",&created)));
                    assert_eq!(first.expect("first original waiter").id(),second.expect("second original waiter").id());
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&operation_id]).await.expect("one permanent delete").get::<_,i64>(0),1);
                    let replacement=Box::pin(services.role().create_workforce(actor.clone(),operation(),input(&body))).await.expect("new role with reused name");
                    let operations=[operation(),operation()];
                    let (first,second)=tokio::join!(services.role().delete_workforce(actor.clone(),operations[0],replacement.id(),deletion("institution",&replacement)),services.role().delete_workforce(actor,operations[1],replacement.id(),deletion("institution",&replacement)));
                    assert!(matches!((&first,&second),(Ok(_),Err(kernel::Error::Rejected(Rejection::Absent)))|(Err(kernel::Error::Rejected(Rejection::Absent)),Ok(_))));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=ANY($1::uuid[])",&[&operations.as_slice()]).await.expect("losing deletion leaves no history").get::<_,i64>(0),1);
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded Workforce deletion recovery");
            }
        }));
    }

    #[test]
    fn accepted_workforce_deletion_keeps_its_original_operator_after_session_loss() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in ["institution","domain"] {
                for damaged in [false,true] {
                    timeout(Duration::from_secs(35),Box::pin(async {
                        let database=Database::initialized(Arc::clone(&server)).await;
                        let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                        let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                        let institution=actor(&database,&services,&mut supplier).await;
                        let operator=if entry=="institution" {institution} else {
                            let manager=register(&database,&services,&mut supplier).await;
                            let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                            let full=candidate.roles().iter().find(|role|role.kind()==Kind::Full).expect("full");
                            Box::pin(services.grant().create(institution,operation(),admission(&candidate,"institution","13800000000",full))).await.expect("real domain authority");manager
                        };
                        let created=Box::pin(services.role().create_workforce(operator.clone(),operation(),input(&json!({"entry":entry,"name":"Retained deletion","level":200,"permissions":["workforce.numbers"]})))).await.expect("actual target");
                        let identity=services.current().read(&operator).await.expect("accepted operator");
                        let operation_id=operation();
                        let request=services.role().delete_workforce(operator.clone(),operation_id,created.id(),deletion(entry,&created));
                        let result=if damaged {Box::pin(crate::recovery::after_revision_loss(&database,&proxy,&operator,request)).await}
                            else {Box::pin(crate::recovery::after_revocation(&database,&proxy,services.revocation(),&operator,request)).await};
                        let observer=database.connections(&database.user).recover().await.expect("independent observer");
                        let row=observer.client().query_one("SELECT change.id,change.operator_id,change.entry,EXISTS(SELECT 1 FROM access.roles state WHERE state.id=$2 AND NOT state.active AND state.change_id=change.id AND state.sequence=change.sequence AND state.version=2 AND cardinality(state.permissions)=0) AS terminal,NOT EXISTS(SELECT 1 FROM access.permissions WHERE role_id=$2) AS empty FROM access.changes change WHERE operation_id=$1 AND scope='workforce' AND action='delete'",&[&operation_id,&created.id()]).await.expect("independent fixed terminal graph before replay");
                        assert!(row.get::<_,bool>("terminal"));assert!(row.get::<_,bool>("empty"));assert_eq!(row.get::<_,String>("entry"),entry);assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"),identity.revision_id());
                        if damaged {
                            let error=result.err().expect("missing fixed identity is not a successful receipt");
                            assert!(matches!(&error,kernel::Error::Unknown(fault) if fault.later().any(|(stage,cause)| stage==kernel::Stage::Recovery && matches!(cause,kernel::FaultKind::Revision(revision::ReadError::Missing)))),"{error:?}");assert!(services.ready().await.is_err());
                        } else {
                            let original=result.expect("accepted deletion survives session loss");assert_eq!(original.id(),row.get("id"));assert_eq!(original.operator().revision_id(),identity.revision_id());
                            assert!(matches!(Box::pin(services.role().delete_workforce(operator,operation_id,created.id(),deletion(entry,&created))).await,Err(kernel::Error::Unauthenticated)));
                        }
                        services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");proxy.stop().await;
                        if !damaged {let (context,services)=start(database.connections(&database.user),&supplier).await;services.ready().await.expect("complete terminal history after restart");services.close();services.drain().await.expect("restart drained");context.stop().await.expect("restart ended");}
                        observer.terminate().await.expect("observer ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                    })).await.expect("bounded accepted deletion after identity change");
                }
            }
        }));
    }

    #[test]
    fn accepted_workforce_creation_recovers_fixed_identity_after_session_loss() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for entry in ["institution","domain"] {
                for damaged in [false,true] {
                    timeout(Duration::from_secs(35),Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context,services) = start(authentication::proxied(&database,&proxy),&supplier).await;
                        let institution = actor(&database,&services,&mut supplier).await;
                        let operator = if entry == "institution" { institution } else {
                            let manager = register(&database,&services,&mut supplier).await;
                            let candidate = services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("manager candidate");
                            let full = candidate.roles().iter().find(|role| role.kind()==Kind::Full).expect("administrator");
                            Box::pin(services.grant().create(institution,operation(),admission(&candidate,"institution","13800000000",full))).await.expect("real domain administrator");
                            manager
                        };
                        let identity = services.current().read(&operator).await.expect("original operator");
                        let operation_id = operation();
                        let body = json!({"entry":entry,"name":"Recovered","level":200,"permissions":["workforce.roles"]});
                        let request = services.role().create_workforce(operator.clone(),operation_id,input(&body));
                        let result = if damaged {
                            Box::pin(crate::recovery::after_revision_loss(&database,&proxy,&operator,request)).await
                        } else {
                            Box::pin(crate::recovery::after_revocation(&database,&proxy,services.revocation(),&operator,request)).await
                        };
                        let observer = database.connections(&database.user).recover().await.expect("independent observer");
                        let row = observer.client().query_one("SELECT change.id,change.role_id,change.entry,change.operator_id,role.change_id,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=change.id) AS entries,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id) AS permissions FROM access.changes change JOIN access.roles role ON role.change_id=change.id WHERE change.operation_id=$1 AND change.scope='workforce'", &[&operation_id]).await.expect("independent complete facts before any replay");
                        assert_eq!(row.get::<_,String>("entry"),entry);
                        assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"),identity.revision_id());
                        assert_eq!(row.get::<_,Vec<String>>("entries"),["workforce.roles"]);
                        assert_eq!(row.get::<_,Vec<String>>("permissions"),["workforce.roles"]);
                        if damaged {
                            let error = result.err().expect("missing original parent cannot deliver");
                            assert!(matches!(&error,kernel::Error::Unknown(fault) if fault.later().any(|(stage,cause)| stage==kernel::Stage::Recovery && matches!(cause,kernel::FaultKind::Revision(revision::ReadError::Missing)))),"{error:?}");
                            assert!(services.ready().await.is_err());
                        } else {
                            let recovered = result.expect("accepted work survives later revocation");
                            assert_eq!(recovered.id(),row.get("role_id")); assert_eq!(recovered.change_id(),row.get("id"));
                            assert!(matches!(Box::pin(services.role().create_workforce(operator,operation_id,input(&body))).await,Err(kernel::Error::Unauthenticated)),"a new request rechecks its current session");
                        }
                        services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended"); proxy.stop().await;
                        if !damaged {
                            let (context,services)=start(database.connections(&database.user),&supplier).await;
                            services.ready().await.expect("valid permanent role survives restart"); services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart ended");
                        }
                        observer.terminate().await.expect("observer ended");
                        assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
                    })).await.expect("bounded Workforce recovery");
                }
            }
        }));
    }

    #[test]
    fn workforce_originals_survive_lost_commit_and_serialize_competing_requests() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit,Fault::RolledBack,Fault::UnknownCommit] {
                timeout(Duration::from_secs(25),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await; let proxy=Proxy::start(&server).await;
                    let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                    let operator=actor(&database,&services,&mut supplier).await;
                    let observer=database.connections(&database.user).recover().await.expect("observer");
                    let operation_id=operation(); let body=json!({"entry":"institution","name":"Original","level":200,"permissions":["workforce.numbers"]});
                    let rolled_back=matches!(fault,Fault::RolledBack); let unknown=matches!(fault,Fault::UnknownCommit);
                    proxy.arm(fault);
                    let result=Box::pin(services.role().create_workforce(operator.clone(),operation_id,input(&body))).await;
                    timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual commit fault");
                    if rolled_back { assert!(matches!(&result,Err(kernel::Error::Unavailable(fault)) if fault.stage()==kernel::Stage::Commit && matches!(fault.kind(),kernel::FaultKind::Query(_))),"{:?}",result.as_ref().err()); }
                    else if unknown { assert!(matches!(&result,Err(kernel::Error::Unknown(fault)) if fault.stage()==kernel::Stage::Commit && matches!(fault.kind(),kernel::FaultKind::Query(_))),"{:?}",result.as_ref().err()); }
                    else { result.expect("lost response recovers original"); }
                    let count:i64=observer.client().query_one("SELECT count(*) FROM access.changes change JOIN access.roles role ON role.change_id=change.id WHERE change.operation_id=$1 AND change.scope='workforce' AND EXISTS(SELECT 1 FROM access.entries WHERE change_id=change.id AND capability_id='workforce.numbers') AND EXISTS(SELECT 1 FROM access.permissions WHERE role_id=role.id AND capability_id='workforce.numbers')",&[&operation_id]).await.expect("independent original or confirmed absence before retry").get(0);
                    assert_eq!(count,i64::from(!rolled_back && !unknown));
                    proxy.release();
                    if unknown {
                        timeout(Duration::from_secs(3),async {
                            loop {
                                let count:i64=observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1 AND scope='workforce'",&[&operation_id]).await.expect("independent delayed commit before replay").get(0);
                                if count==1 {break;} tokio::task::yield_now().await;
                            }
                        }).await.expect("original backend commits after unknown response");
                    }
                    let (first,second)=tokio::join!(services.role().create_workforce(operator.clone(),operation_id,input(&body)),services.role().create_workforce(operator.clone(),operation_id,input(&body)));
                    let first=first.expect("first caller");let second=second.expect("matching caller");assert_eq!(first.id(),second.id());assert_eq!(first.change_id(),second.change_id());
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1 AND scope='workforce'",&[&operation_id]).await.expect("one permanent operation").get::<_,i64>(0),1);
                    let mut other=body.clone();other["name"]=json!("Competing");
                    let operations=[operation(),operation()];
                    let (first,second)=tokio::join!(services.role().create_workforce(operator.clone(),operations[0],input(&other)),services.role().create_workforce(operator,operations[1],input(&other)));
                    assert!(matches!((&first,&second),(Ok(_),Err(kernel::Error::Rejected(Rejection::NameTaken)))|(Err(kernel::Error::Rejected(Rejection::NameTaken)),Ok(_))));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=ANY($1::uuid[])",&[&operations.as_slice()]).await.expect("no losing orphan").get::<_,i64>(0),1);
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded unknown and concurrent outcomes");
            }
        }));
    }

    #[test]
    fn workforce_maintenance_rechecks_membership_after_a_real_termination_wait() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for action in ["create","update","delete"] {
            timeout(Duration::from_secs(30),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;
                let mut supplier=Supplier::new().await; let proxy=Proxy::start(&server).await;
                let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                let institution=actor(&database,&services,&mut supplier).await;
                let manager=register(&database,&services,&mut supplier).await;
                let candidate=services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("candidate");
                let full=candidate.roles().iter().find(|role|role.kind()==Kind::Full).expect("full role");
                let (grant,_)=Box::pin(services.grant().create(institution,operation(),admission(&candidate,"institution","13800000000",full))).await.expect("real membership");
                let selected=Box::pin(services.role().create_workforce(manager.clone(),operation(),input(&json!({"entry":"domain","name":"Before exit","level":200,"permissions":[]})))).await.expect("existing target before qualification loss");
                let selected_id=selected.id();
                let observer=database.connections(&database.user).recover().await.expect("observer");
                proxy.arm(Fault::DelayedCommit);
                let service=services.grant().clone();let actor=manager.clone();
                let termination=tokio::spawn(async move {service.terminate(actor,operation(),grant.id()).await});
                let blocker=timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("real termination retains scope and grant locks before COMMIT");
                let service=services.role().clone();let actor=manager.clone();let operation_id=operation();
                let maintenance=tokio::spawn(async move {
                    match action {
                        "delete"=>Box::pin(service.delete_workforce(actor,operation_id,selected.id(),deletion("domain",&selected))).await.map(|_|()),
                        "update"=>Box::pin(service.update_workforce(actor,operation_id,selected.id(),modification(&selected,&json!({"entry":"domain","name":"After exit","level":201,"permissions":[]})))).await.map(|_|()),
                        _=>Box::pin(service.create_workforce(actor,operation_id,input(&json!({"entry":"domain","name":"After exit","level":200,"permissions":[]})))).await.map(|_|()),
                    }
                });
                timeout(Duration::from_secs(3),async {loop {
                    if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock(%')",&[&blocker]).await.expect("actual role scope wait").get::<_,bool>(0) {break;}
                    tokio::task::yield_now().await;
                }}).await.expect("role maintenance waits behind actual termination");
                proxy.release(); termination.await.expect("termination worker").expect("termination committed and recovered");
                assert!(matches!(maintenance.await.expect("maintenance worker"),Err(kernel::Error::Denied)),"missing current grant is denied after the lock wait");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&operation_id]).await.expect("no partial role").get::<_,i64>(0),0);
                assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles WHERE id=$1)",&[&selected_id]).await.expect("rejected deletion preserves its target").get::<_,bool>(0));
                assert!(services.grant().read_role(&manager).await.is_err());
                services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");observer.terminate().await.expect("observer ended");proxy.stop().await;
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded qualification wait");
            }
        }));
    }

    #[test]
    fn admission_role_pages_keep_authorization_count_and_rows_in_one_snapshot() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for retry in [true,false] {
                timeout(Duration::from_secs(25),Box::pin(async {
                    let database=Database::initialized(Arc::clone(&server)).await;
                    let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                    let (context,services)=crate::access::role::tests::start_snapshot(authentication::proxied(&database,&proxy),&supplier).await;
                    let institution=actor(&database,&services,&mut supplier).await;
                    register(&database,&services,&mut supplier).await;
                    proxy.arm(if retry {Fault::StalledRoleStates} else {Fault::PausedReadEnd});let service=services.grant().clone();let actor=institution.clone();
                    let read=tokio::spawn(async move {service.candidate(&actor,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await});
                    timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("candidate state validation or completed snapshot intercepted");
                    let added=Box::pin(services.role().create_workforce(institution.clone(),operation(),input(&json!({"entry":"institution","name":"New choice","level":200,"permissions":[]})))).await.expect("concurrent committed definition");
                    Box::pin(services.revocation().revoke(institution.clone(),operation(),crate::identity::revocation::Scope::Current)).await.expect("concurrent session loss");
                    proxy.release();let result=read.await.expect("reader");
                    if retry {
                        assert!(matches!(result,Err(grant::Error::Current(current::Error::Invalid))|Err(grant::Error::Role(Error::Current(current::Error::Invalid)))),"a changed anchor restarts candidate authorization");
                    } else {
                        let candidate=result.expect("original validated snapshot");
                        assert_eq!(candidate.count(),2);assert_eq!(candidate.roles().len(),2);assert!(!candidate.roles().iter().any(|role|role.id()==added.id()));
                    }
                    assert!(matches!(services.grant().candidate(&institution,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await,Err(grant::Error::Current(current::Error::Invalid))|Err(grant::Error::Role(Error::Current(current::Error::Invalid)))));
                    let current=role::tests::actor(&database,&services,&mut supplier).await;
                    let latest=services.grant().candidate(&current,Entry::Institution,"13800000000".parse().expect("mobile"),Pagination::new(1,20).expect("page")).await.expect("fresh authorized snapshot");
                    assert_eq!(latest.count(),3);assert!(latest.roles().iter().any(|role|role.id()==added.id()));
                    services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");proxy.stop().await;
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
                })).await.expect("bounded candidate snapshot");
            }
        }));
    }

    #[test]
    fn workforce_creation_rejections_leave_no_partial_sealed_facts() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                let creation = operation();
                let authority = Box::pin(services.role().create_workforce(institution.clone(), creation, input(&json!({"entry":"institution","name":"Maintainers","level":300,"permissions":["workforce.roles"]})))).await.expect("limited domain role");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), Pagination::new(1,20).expect("page")).await.expect("candidate");
                Box::pin(services.grant().create(institution.clone(), operation(), admission(&candidate,"institution","13800000000",&authority))).await.expect("actual domain membership");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("complete initial facts").get(0);
                for case in ["missing membership", "wrong authority", "equal rank", "higher rank", "permission expansion", "name conflict", "same operation"] {
                    let acting = if matches!(case, "missing membership" | "same operation") { &institution } else { &manager };
                    let entry = if matches!(case, "wrong authority" | "same operation") { "institution" } else { "domain" };
                    let level = match case { "equal rank" => 300, "higher rank" => 301, _ => 200 };
                    let name = if case == "name conflict" { "Maintainers" } else { "Candidate" };
                    let permissions = if case == "permission expansion" { json!(["workforce.numbers"]) } else { json!([]) };
                    let original = if case == "same operation" { creation } else { operation() };
                    let result = Box::pin(services.role().create_workforce(acting.clone(), original, input(&json!({"entry":entry,"name":name,"level":level,"permissions":permissions})))).await;
                    if case == "same operation" { assert!(matches!(&result, Err(kernel::Error::Mismatch)), "{case}: {:?}", result.as_ref().err()); }
                    else if case == "name conflict" { assert!(matches!(&result, Err(kernel::Error::Rejected(Rejection::NameTaken))), "{case}: {:?}", result.as_ref().err()); }
                    else { assert!(matches!(&result, Err(kernel::Error::Denied)), "{case}: {:?}", result.as_ref().err()); }
                    assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("no partial creation or changed original").get::<_, String>(0), before, "{case}");
                }
                let original = operation();
                let created = Box::pin(services.role().create_workforce(manager.clone(), original, input(&json!({"entry":"domain","name":"Committed candidate","level":200,"permissions":["workforce.roles"]})))).await.expect("actual authorized creation");
                let row = observer.client().query_one("SELECT state.active,state.sequence,state.version,record.previous_id,record.entry,record.action,completion.target,ROW(state.name,state.level,state.permissions,state.sequence,state.changed_at) IS NOT DISTINCT FROM ROW(record.name,record.level,record.permissions,record.sequence,record.changed_at) AS paired,completion.action=record.action AND completion.revision_id=record.operator_id AND completion.created_at=record.changed_at AND completion.expires_at IS NULL AS completed FROM access.roles state JOIN access.changes record ON record.id=state.change_id AND record.role_id=state.id JOIN access.completions completion ON completion.family='workforce_role' AND completion.operation_id=record.operation_id WHERE state.id=$1 AND record.operation_id=$2", &[&created.id(), &original]).await.expect("independent complete creation and completion");
                assert!(row.get::<_, bool>("active")); assert!(row.get::<_, bool>("paired")); assert!(row.get::<_, bool>("completed"));
                assert_eq!(row.get::<_, i64>("sequence"), 1); assert_eq!(row.get::<_, i64>("version"), 1); assert_eq!(row.get::<_, Option<role::change::Id>>("previous_id"), None);
                assert_eq!(row.get::<_, String>("entry"), "domain"); assert_eq!(row.get::<_, String>("action"), "create"); assert_eq!(row.get::<_, String>("target"), "roles");
                let page = services.role().changes_workforce(&manager, Filter::new(Pagination::new(1,1).expect("one record"), Some(created.id()), Some(original))).await.expect("authenticated creation history");
                assert_eq!(page.count(), 1); assert_eq!(page.changes()[0].id(), created.change_id());
                observer.terminate().await.expect("observer ended"); services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded Workforce creation atomicity");
        }));
    }

    #[test]
    fn workforce_creation_is_usable_by_admission_and_binds_its_authority() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = register(&database, &services, &mut supplier).await;
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let operator = services.current().read(&institution).await.expect("operator identity");
                let original = operation();
                let body = json!({"entry":"institution","name":" Coordinator ","level":300,"permissions":["workforce.roles","workforce.admission"]});
                assert!(matches!(Box::pin(services.role().create_workforce(manager.clone(), operation(), input(&body))).await, Err(kernel::Error::Denied)));
                let mut domain = body.clone(); domain["entry"] = json!("domain");
                assert!(matches!(Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&domain))).await, Err(kernel::Error::Denied)), "institution authority does not create domain membership");
                for level in [1, 1000] {
                    let mut system = body.clone(); system["level"] = json!(level);
                    assert!(matches!(Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&system))).await, Err(kernel::Error::Denied)));
                }
                let created = Box::pin(services.role().create_workforce(institution.clone(), original, input(&body))).await.expect("institution creates without domain membership");
                assert_eq!(created.kind(), Kind::Custom);
                let row = observer.client().query_one(r#"SELECT change.role_id, change.id, change.entry, change.name, change.level,
                    change.operator_id, change.operation_id, role.change_id, person.sequence,
                    ARRAY(SELECT capability_id FROM access.entries WHERE change_id=change.id ORDER BY capability_id COLLATE "C") AS entries,
                    ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id COLLATE "C") AS permissions
                    FROM access.changes change JOIN access.roles role ON role.id=change.role_id AND role.scope=change.scope
                    JOIN identity.revisions revision ON revision.id=change.operator_id JOIN identity.persons person ON person.id=revision.person_id
                    WHERE change.operation_id=$1 AND change.scope='workforce'"#, &[&original]).await.expect("independent complete graph before replay");
                assert_eq!(row.get::<_,role::Id>("role_id"),created.id());
                assert_eq!(row.get::<_,role::change::Id>("id"),created.change_id());
                assert_eq!(row.get::<_,role::change::Id>("change_id"),created.change_id());
                assert_eq!(row.get::<_,String>("entry"),"institution");
                assert_eq!(row.get::<_,String>("name"),"Coordinator");
                assert_eq!(row.get::<_,i16>("level"),300);
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"),operator.revision_id());
                assert_eq!(row.get::<_,operation::Id>("operation_id"),original);
                assert_eq!(row.get::<_,i64>("sequence"),1,"role creation does not advance personal history");
                assert_eq!(row.get::<_,Vec<String>>("entries"),["workforce.admission","workforce.roles"]);
                assert_eq!(row.get::<_,Vec<String>>("permissions"),["workforce.admission","workforce.roles"]);
                let replay = Box::pin(services.role().create_workforce(institution.clone(), original, input(&body))).await.expect("same canonical input");
                assert_eq!(replay.change_id(),created.change_id());
                assert!(matches!(Box::pin(services.role().create_workforce(institution.clone(), operation(), input(&body))).await, Err(kernel::Error::Rejected(Rejection::NameTaken))));
                let same_scope_name = serde_json::from_value(json!({"name":"Coordinator","level":20,"permissions":[]})).expect("institution input");
                services.role().create(institution.clone(), original, same_scope_name).await.expect("same name and UUID are separate in another scope");
                let candidate = services.grant().candidate(&institution, Entry::Institution, "13800000000".parse().expect("mobile"), Pagination::new(1,20).expect("page")).await.expect("role available for admission");
                assert_eq!(candidate.count(),3);
                assert_eq!(candidate.roles().iter().map(|role| i16::from(role.level())).collect::<Vec<_>>(),[1000,300,1]);
                let (_, assignment) = Box::pin(services.grant().create(institution.clone(),operation(),admission(&candidate,"institution","13800000000",&created))).await.expect("admit with custom role");
                assert_eq!(assignment.role().id(),created.id());
                let current = services.grant().read_role(&manager).await.expect("assigned current role");
                assert_eq!(current.permissions(),[role::Code::WorkforceAdmission,role::Code::WorkforceRoles]);
                for (name, level, permissions) in [
                    ("Equal",300,json!([])), ("Higher",301,json!([])), ("Expansion",200,json!(["workforce.numbers"])),
                ] {
                    let denied = json!({"entry":"domain","name":name,"level":level,"permissions":permissions});
                    assert!(matches!(Box::pin(services.role().create_workforce(manager.clone(),operation(),input(&denied))).await,Err(kernel::Error::Denied)));
                }
                let subordinate = json!({"entry":"domain","name":"Maintainer","level":200,"permissions":["workforce.roles"]});
                let subordinate = Box::pin(services.role().create_workforce(manager.clone(),operation(),input(&subordinate))).await.expect("strictly lower subset");
                let candidate = services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"),Pagination::new(1,1).expect("first page")).await.expect("domain selection");
                assert_eq!(candidate.count(),2); assert_eq!(candidate.roles().len(),1); assert_eq!(candidate.roles()[0].id(),subordinate.id());
                let empty = services.grant().candidate(&manager,Entry::Domain,"13900000000".parse().expect("mobile"),Pagination::new(3,1).expect("empty page")).await.expect("out-of-range selection");
                assert_eq!(empty.count(),2); assert!(empty.roles().is_empty());
                Box::pin(services.grant().create(manager.clone(),operation(),admission(&candidate,"domain","13900000000",&subordinate))).await.expect("domain admission ignores higher institution rank");
                assert!(matches!(Box::pin(services.role().create_workforce(institution.clone(),original,input(&domain))).await,Err(kernel::Error::Mismatch)),"both basic authorities pass, but original entry remains bound");
                let replay = Box::pin(services.role().create_workforce(institution.clone(),original,input(&body))).await.expect("original authority still returns its definition");
                assert_eq!(replay.id(),created.id());
                crate::access::verification::verify(observer.client(), services.app(),context.sample().await.expect("time")).await.expect("startup graph remains valid");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"no external calls through replay and drain");
                observer.terminate().await.expect("observer ended"); supplier.stop().await;
            })).await.expect("bounded role creation flow");
        }));
    }
}
