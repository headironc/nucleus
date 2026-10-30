//! Workforce groups, departments, positions, department positions, the title tree and subtype
//! rules. Every configuration command and query runs on the kernel executor; the original
//! completions use the kernel completion template in `workforce.completions`.

mod command;
mod database;
mod query;
mod value;

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

pub use crate::workforce::directory::value::{
    Collection, Creation, Delete, Index, InputError, Item, Name, Target, Update, category,
    department, discipline, group, position, roster, rule, selection, title,
};
use crate::{
    access::role,
    history,
    identity::{
        current::{self, Actor},
        operation,
    },
    kernel::{
        self, App, Status,
        completion::{Retention, Template},
        operation::Family,
    },
    workforce::number::Subtype,
};

/// Directory commands and queries report their closed rejections or a kernel fault.
pub type Error = kernel::Error<Rejection>;

/// The closed business rejections of directory configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Input does not match its collection, path or immutable identity.
    Input,
    /// The target, a required parent, a department or a position is unavailable.
    Absent,
    /// The current value differs from the expected one.
    Changed,
    NameInUse,
    InvalidParent,
    /// The rule department and its ancestors must be selectable.
    InvalidDepartment,
    /// The item still has references or children.
    InUse,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> Status {
        match self {
            Self::Absent => Status::NotFound,
            Self::Changed | Self::NameInUse | Self::InUse => Status::Conflict,
            Self::Input | Self::InvalidParent | Self::InvalidDepartment => Status::Unprocessable,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Input => "invalid_input",
            Self::Absent => "target_unavailable",
            Self::Changed => "data_changed",
            Self::NameInUse => "name_in_use",
            Self::InvalidParent => "invalid_parent",
            Self::InvalidDepartment => "invalid_department",
            Self::InUse => "item_in_use",
        }
    }
}

impl From<Rejection> for Error {
    fn from(rejection: Rejection) -> Self {
        Self::Rejected(rejection)
    }
}

impl From<InputError> for Error {
    fn from(_: InputError) -> Self {
        Self::Rejected(Rejection::Input)
    }
}

/// Only the owning constraints become rejections; every other database error is technical.
impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        let Some(database) = error.as_db_error() else {
            return Self::query(error);
        };
        if database.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            && database.schema() == Some("workforce")
            && matches!(
                (database.table(), database.constraint()),
                (Some("groups"), Some("groups_name_key"))
                    | (Some("departments"), Some("departments_name_key"))
                    | (Some("positions"), Some("positions_name_key"))
                    | (Some("groups"), Some("groups_departments_name_key"))
                    | (Some("departments"), Some("departments_groups_name_key"))
                    | (Some("categories"), Some("categories_name_key"))
                    | (Some("disciplines"), Some("disciplines_name_key"))
                    | (Some("titles"), Some("titles_name_key"))
                    | (Some("categories"), Some("title_names_key"))
                    | (Some("disciplines"), Some("title_names_key"))
                    | (Some("titles"), Some("title_names_key"))
            )
        {
            return Self::Rejected(Rejection::NameInUse);
        }
        if database.code() == &tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION
            && database.schema() == Some("workforce")
            && matches!(
                database.constraint(),
                Some("departments_group_id_fkey")
                    | Some("departments_parent_id_group_id_fkey")
                    | Some("rules_department_id_fkey")
                    | Some("disciplines_category_id_fkey")
                    | Some("titles_discipline_id_fkey")
            )
        {
            return Self::Rejected(Rejection::InUse);
        }
        if database.code() == &tokio_postgres::error::SqlState::CHECK_VIOLATION
            && matches!(
                database.constraint(),
                Some("departments_parent_check")
                    | Some("departments_ancestor_check")
                    | Some("departments_subtree_check")
            )
        {
            return Self::Rejected(Rejection::InvalidParent);
        }
        if database.code() == &tokio_postgres::error::SqlState::CHECK_VIOLATION
            && database.constraint() == Some("rules_department_check")
        {
            return Self::Rejected(Rejection::InvalidDepartment);
        }
        Self::query(error)
    }
}

/// Whether a completion action names a target of its shape: creation names a delivered
/// collection or a subtype, update an item UUID or a subtype, and deletion or department
/// position replacement an item UUID, all in canonical text.
fn pairs(action: &str, target: &str) -> bool {
    let item = target
        .parse::<uuid::Uuid>()
        .is_ok_and(|id| id.to_string() == target);
    let rule = database::rule_target(target).is_some();
    match action {
        "create" => target.parse::<Collection>().is_ok() || rule,
        "update" => item || rule,
        "delete" | "replace" => item,
        _ => false,
    }
}

const COMPLETIONS: Template = Template::new(Family::WorkforceConfiguration, pairs);

/// The original or current result of a directory item write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    Present(Item),
    Deleted,
}

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
}

pub(crate) async fn verify(
    client: &Client,
    keys: history::Keys<'_>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    database::verify(client, keys, now).await
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service) -> Self {
        Self {
            roles: role::Reader::new(app.clone()),
            app,
            current,
        }
    }

    /// Reclaims expired configuration completions; configuration itself is never deleted.
    pub fn retention(&self) -> Retention {
        Retention::new(self.app.clone(), COMPLETIONS)
    }

    pub async fn list(&self, actor: &Actor, collection: Collection) -> Result<Vec<Item>, Error> {
        self.app
            .query(&query::List {
                manager: self.manager(actor),
                collection,
            })
            .await
    }

    pub async fn read_selection(&self, actor: &Actor) -> Result<selection::Selection, Error> {
        self.app
            .query(&query::Selection {
                actor,
                current: &self.current,
                roles: &self.roles,
            })
            .await
    }

    pub async fn list_rules(&self, actor: &Actor) -> Result<rule::Configuration, Error> {
        self.app
            .query(&query::Rules {
                manager: self.manager(actor),
            })
            .await
    }

    pub async fn list_roster(
        &self,
        actor: &Actor,
        department_id: department::Id,
    ) -> Result<Vec<roster::Entry>, Error> {
        self.app
            .query(&query::Roster {
                manager: self.manager(actor),
                department_id,
            })
            .await
    }

    /// Replaces a department's complete position list after comparing the complete current list.
    /// A replay whose department was later deleted returns [`roster::Receipt::Deleted`].
    pub async fn replace_roster(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        department_id: department::Id,
        input: roster::Replace,
    ) -> Result<roster::Receipt, Error> {
        let input = input.canonical()?;
        self.app
            .command(
                operation_id,
                command::ReplaceRoster {
                    session: self.session(actor),
                    department_id,
                    input,
                },
            )
            .await
    }

    pub async fn configure_rule(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        subtype: Subtype,
        input: rule::Input,
    ) -> Result<rule::Receipt, Error> {
        input.validate(subtype)?;
        let session = self.session(actor);
        match input {
            rule::Input::Create(input) => {
                self.app
                    .command(
                        operation_id,
                        command::CreateRule {
                            session,
                            subtype,
                            input,
                        },
                    )
                    .await
            }
            rule::Input::Update(input) => {
                self.app
                    .command(
                        operation_id,
                        command::UpdateRule {
                            session,
                            subtype,
                            input,
                        },
                    )
                    .await
            }
        }
    }

    pub async fn create(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        collection: Collection,
        input: Creation,
    ) -> Result<Receipt, Error> {
        if input.collection() != collection {
            return Err(Rejection::Input.into());
        }
        self.app
            .command(
                operation_id,
                command::Create {
                    session: self.session(actor),
                    input,
                },
            )
            .await
    }

    pub async fn update(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        target: Target,
        input: Update,
    ) -> Result<Receipt, Error> {
        input.validate(target)?;
        self.app
            .command(
                operation_id,
                command::Update {
                    session: self.session(actor),
                    target,
                    input,
                },
            )
            .await
    }

    pub async fn delete(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        target: Target,
        input: Delete,
    ) -> Result<Receipt, Error> {
        input.validate(target)?;
        self.app
            .command(
                operation_id,
                command::Delete {
                    session: self.session(actor),
                    target,
                    input,
                },
            )
            .await
    }

    fn session(&self, actor: Actor) -> command::Session {
        command::Session {
            actor,
            current: self.current.clone(),
            roles: self.roles.clone(),
        }
    }

    fn manager<'a>(&'a self, actor: &'a Actor) -> query::Manager<'a> {
        query::Manager {
            actor,
            current: &self.current,
            roles: &self.roles,
        }
    }

    #[cfg(test)]
    fn registry(&self) -> &crate::runtime::registry::Registry {
        self.app.registry()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{future::poll_fn, sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use super::{
        Collection, Creation, Delete, Error, Item, Receipt, Rejection, Target, Update, category,
        department, discipline, group, position, roster, rule, title,
    };
    use crate::{
        access::{
            Entry,
            appointment::tests::{mobile, register, register_numbered},
            grant,
            role::{
                Kind,
                tests::{actor, start},
            },
        },
        database::Barrier,
        identity::{
            authentication::{self, Database, Supplier, server},
            current::Actor,
            operation,
        },
        kernel::completion::Seed,
        runtime::Context,
        startup::{Services, Settings},
        workforce::number::Subtype,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    fn group_creation(name: &str, enabled: bool, index: i32) -> Creation {
        Creation::Group(
            serde_json::from_value(json!({"name":name,"enabled":enabled,"index":index}))
                .expect("explicit group creation"),
        )
    }

    fn position_creation(name: &str, enabled: bool, index: i32) -> Creation {
        Creation::Position(position::Creation {
            name: name.parse().expect("explicit position name"),
            enabled,
            index: index.try_into().expect("explicit position index"),
        })
    }

    fn department_creation(
        name: &str,
        enabled: bool,
        index: i32,
        group_id: group::Id,
        parent_id: Option<department::Id>,
    ) -> Creation {
        Creation::Department(serde_json::from_value(json!({"name":name,"enabled":enabled,"index":index,"groupId":group_id,"parentId":parent_id}))
            .expect("explicit department creation"))
    }

    fn modification(expected: &Item, replacement: &Item) -> Update {
        Update {
            expected: expected.clone(),
            replacement: replacement.clone(),
        }
    }

    fn deletion(expected: &Item) -> Delete {
        Delete {
            expected: expected.clone(),
        }
    }

    fn rule_creation(department_id: department::Id, position: bool) -> rule::Input {
        serde_json::from_value(json!({"departmentId":department_id,"position":position}))
            .expect("explicit rule creation")
    }

    fn rule_value(subtype: Subtype, department_id: department::Id, position: bool) -> rule::Rule {
        serde_json::from_value(
            json!({"id":i16::from(subtype),"departmentId":department_id,"position":position}),
        )
        .expect("complete rule")
    }

    fn rule_modification(expected: &rule::Rule, replacement: &rule::Rule) -> rule::Input {
        rule::Input::Update(rule::Update {
            expected: expected.clone(),
            replacement: replacement.clone(),
        })
    }

    fn configure<'a>(
        services: &'a Services,
        actor: &'a Actor,
        operation_id: operation::Id,
        subtype: Subtype,
        input: rule::Input,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = rule::Receipt> + 'a>> {
        Box::pin(async move {
            services
                .directory()
                .configure_rule(actor.clone(), operation_id, subtype, input)
                .await
                .expect("real rule configuration")
        })
    }

    fn configure_request<'a>(
        service: &'a super::Service,
        actor: Actor,
        operation_id: operation::Id,
        subtype: Subtype,
        input: rule::Input,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<rule::Receipt, Error>> + 'a>>
    {
        Box::pin(service.configure_rule(actor, operation_id, subtype, input))
    }

    fn present(receipt: Receipt) -> Item {
        let Receipt::Present(item) = receipt else {
            panic!("visible directory item")
        };
        item
    }

    fn create<'a>(
        services: &'a Services,
        actor: &'a Actor,
        operation_id: operation::Id,
        input: Creation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Item> + 'a>> {
        Box::pin(async move {
            let collection = input.collection();
            present(
                services
                    .directory()
                    .create(actor.clone(), operation_id, collection, input)
                    .await
                    .expect("real directory creation"),
            )
        })
    }

    async fn manager(
        database: &Database,
        services: &Services,
        supplier: &mut Supplier,
        institution: &Actor,
    ) -> Actor {
        let manager = Box::pin(register(database, services, supplier)).await;
        Box::pin(grant::tests::admit(
            services,
            institution,
            Entry::Institution,
            "13800000000",
            Kind::Full,
        ))
        .await;
        manager
    }

    fn update_request(
        service: &super::Service,
        actor: Actor,
        operation: operation::Id,
        target: Target,
        input: Update,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Receipt, Error>> + '_>> {
        Box::pin(service.update(actor, operation, target, input))
    }

    fn delete(
        service: &super::Service,
        actor: Actor,
        operation: operation::Id,
        target: Target,
        input: Delete,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Receipt, Error>> + '_>> {
        Box::pin(service.delete(actor, operation, target, input))
    }

    fn create_request(
        service: &super::Service,
        actor: Actor,
        operation: operation::Id,
        collection: Collection,
        input: Creation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Receipt, Error>> + '_>> {
        Box::pin(service.create(actor, operation, collection, input))
    }

    async fn stop(context: Context, services: Services, supplier: Supplier) {
        services.close();
        services.drain().await.expect("drained");
        context.stop().await.expect("stopped");
        supplier.stop().await;
    }

    async fn release_directory_waiters(client: &tokio_postgres::Client, count: i64) {
        wait_directory_waiters(client, count).await;
        client
            .batch_execute("COMMIT")
            .await
            .expect("release directory scope");
    }

    async fn wait_directory_waiters(client: &tokio_postgres::Client, count: i64) {
        timeout(Duration::from_secs(5), async {
            loop {
                let waiting: i64 = client.query_one("SELECT count(*) FROM pg_catalog.pg_locks WHERE locktype='advisory' AND NOT granted AND database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND classid=((pg_catalog.hashtextextended('workforce/directory',0)>>32)&4294967295)::oid AND objid=(pg_catalog.hashtextextended('workforce/directory',0)&4294967295)::oid AND objsubid=1", &[]).await.expect("directory wait graph").get(0);
                if waiting == count { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("both real service writes wait on the directory scope");
    }

    #[test]
    fn completion_actions_pair_only_with_targets_of_their_shape() {
        let item = Uuid::new_v4().to_string();
        for (action, target) in [
            ("create", "groups"),
            ("create", "titles"),
            ("create", "902"),
            ("update", item.as_str()),
            ("update", "100"),
            ("delete", item.as_str()),
            ("replace", item.as_str()),
        ] {
            assert!(super::pairs(action, target), "{action} {target}");
        }
        let uppercase = item.to_uppercase();
        for (action, target) in [
            ("create", item.as_str()),
            ("create", "rules"),
            ("create", "905"),
            ("update", "groups"),
            ("delete", "100"),
            ("delete", "groups"),
            ("replace", "902"),
            ("replace", uppercase.as_str()),
            ("remove", item.as_str()),
            ("update", "0100"),
        ] {
            assert!(!super::pairs(action, target), "{action} {target}");
        }
    }

    #[test]
    fn name_constraint_uses_a_fresh_snapshot_after_waiting() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = Box::pin(start(authentication::proxied(&database, &proxy), &supplier)).await;
                let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                let manager = Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                let group = create(&services, &manager, operation(), group_creation("Parent", true, 0)).await;
                let Item::Group(group) = group else { panic!("group") };
                proxy.arm(Fault::DelayedCommit);
                let writer = tokio::spawn({ let service = services.directory().clone(); async move { service.create(manager, operation(), Collection::Groups, group_creation("Competing", true, 0)).await } });
                timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("service holds directory lock before real commit");
                let runtime = database.connections(&database.user).recover().await.expect("runtime negative writer");
                let observer = database.connections(&database.user).recover().await.expect("independent wait graph");
                runtime.client().batch_execute("BEGIN").await.expect("runtime transaction");
                let (result, ()) = tokio::join!(
                    async { runtime.client().execute("INSERT INTO workforce.departments(id,operation_id,name,enabled,index,group_id) VALUES(gen_random_uuid(),gen_random_uuid(),'Competing',true,0,$1)", &[&group.id()]).await },
                    async { wait_directory_waiters(observer.client(), 1).await; proxy.release(); }
                );
                assert!(matches!(Error::from(result.expect_err("new statement sees committed cross-table name")), Error::Rejected(Rejection::NameInUse)));
                runtime.client().batch_execute("ROLLBACK").await.expect("negative write rolled back");
                writer.await.expect("service writer").expect("original creation committed");
                runtime.terminate().await.expect("runtime ended"); observer.terminate().await.expect("observer ended");
                stop(context, services, supplier).await; proxy.stop().await;
            })).await.expect("bounded stale-snapshot regression");
        }));
    }

    #[test]
    fn runtime_constraints_protect_names_tree_completions_and_cleanup() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                let manager = Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                let group = Box::pin(create(&services, &manager, operation(), group_creation("Protected", false, 0))).await;
                let Item::Group(value) = &group else { panic!("group") };
                let department = Box::pin(create(&services, &manager, operation(), department_creation("Occupied", false, 0, value.id(), None))).await;
                let rule_group = create(&services, &manager, operation(), group_creation("Protected rules", true, 0)).await;
                let Item::Group(rule_group_value) = &rule_group else { panic!("rule group") };
                let rule_department = create(&services, &manager, operation(), department_creation("Referenced rule department", true, 0, rule_group_value.id(), None)).await;
                let rule_department_id = rule_department.target().into_department();
                let rule_operation = operation();
                configure(&services, &manager, rule_operation, Subtype::Internship, rule_creation(rule_department_id, false)).await;
                let runtime = database.connections(&database.user).recover().await.expect("runtime SQL");
                for (sql, expected) in [
                    ("BEGIN; UPDATE workforce.groups SET name='Occupied'", "groups_departments_name_key"),
                    ("BEGIN; UPDATE workforce.departments SET name='Protected'", "departments_groups_name_key"),
                    ("BEGIN ISOLATION LEVEL REPEATABLE READ; UPDATE workforce.groups SET name='Other'", "directory_isolation_check"),
                    ("BEGIN; UPDATE workforce.departments SET parent_id=id", "departments_ancestor_check"),
                    ("BEGIN; UPDATE workforce.groups SET enabled=true; COMMIT", "directory_completion_check"),
                    ("BEGIN; DELETE FROM workforce.groups", "departments_group_id_fkey"),
                    ("BEGIN; DELETE FROM workforce.completions", "completions_retention_check"),
                ] {
                    let error = runtime.client().batch_execute(sql).await.expect_err("runtime constraint rejects invalid mutation");
                    assert_eq!(error.as_db_error().expect("structured constraint").constraint(), Some(expected));
                    runtime.client().batch_execute("ROLLBACK").await.expect("end failed transaction");
                }
                for invalid in ["", " padded ", "Cafe\u{301}", "a\n", &"字".repeat(65)] {
                    runtime.client().batch_execute("BEGIN").await.expect("negative transaction");
                    let error = runtime.client().execute("UPDATE workforce.groups SET name=$1", &[&invalid]).await.expect_err("canonical storage constraint");
                    assert_eq!(error.as_db_error().expect("name constraint").constraint(), Some("groups_name_check"));
                    runtime.client().batch_execute("ROLLBACK").await.expect("rollback");
                }
                for sql in ["UPDATE workforce.completions SET action='delete'", "UPDATE workforce.groups SET operation_id=gen_random_uuid()", "UPDATE workforce.departments SET id=gen_random_uuid()", "UPDATE workforce.rules SET id=1000", "DELETE FROM workforce.rules"] {
                    let error = runtime.client().batch_execute(sql).await.expect_err("immutable columns and rule history have no write privilege");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE));
                }
                runtime.client().batch_execute("BEGIN").await.expect("invalid subtype transaction");
                let error = runtime.client().execute("INSERT INTO workforce.rules(id,department_id,position) VALUES(999,$1,true)", &[&rule_department_id]).await.expect_err("fixed subtype check");
                assert_eq!(error.as_db_error().expect("subtype constraint").constraint(), Some("rules_id_check"));
                runtime.client().batch_execute("ROLLBACK").await.expect("rollback invalid subtype");
                runtime.client().batch_execute("BEGIN").await.expect("disabled department transaction");
                let error = runtime.client().execute("INSERT INTO workforce.rules(id,department_id,position) VALUES(100,$1,true)", &[&department.target().into_department()]).await.expect_err("disabled default department");
                assert_eq!(error.as_db_error().expect("selectability constraint").constraint(), Some("rules_department_check"));
                runtime.client().batch_execute("ROLLBACK").await.expect("rollback disabled department");
                let error = runtime.client().batch_execute("BEGIN; UPDATE workforce.rules SET position=true WHERE id=902; COMMIT").await.expect_err("rule update requires completion");
                assert_eq!(error.as_db_error().expect("completion pairing").constraint(), Some("rules_completion_check"));
                runtime.client().batch_execute("ROLLBACK").await.expect("rollback unpaired update");
                runtime.client().batch_execute("BEGIN").await.expect("referenced deletion transaction");
                let error = runtime.client().execute("DELETE FROM workforce.departments WHERE id=$1", &[&rule_department_id]).await.expect_err("referenced department cannot be deleted");
                assert_eq!(error.as_db_error().expect("rule reference").constraint(), Some("rules_department_id_fkey"));
                runtime.client().batch_execute("ROLLBACK").await.expect("rollback referenced deletion");
                runtime.client().batch_execute("BEGIN; INSERT INTO workforce.groups(id,operation_id,name,enabled,index) VALUES(gen_random_uuid(),gen_random_uuid(),'Same transaction',true,0)").await.expect("first table in a fresh transaction");
                let error = runtime.client().execute("INSERT INTO workforce.departments(id,operation_id,name,enabled,index,group_id) VALUES(gen_random_uuid(),gen_random_uuid(),'Same transaction',true,0,$1)", &[&value.id()]).await.expect_err("same-transaction cross-table collision");
                assert_eq!(error.as_db_error().expect("collision").constraint(), Some("departments_groups_name_key"));
                runtime.client().batch_execute("ROLLBACK").await.expect("rollback");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                for original in [group.operation_id(), rule_operation] {
                    Seed::age(owner.client(), services.directory().app.keys(), super::COMPLETIONS, original, chrono::Duration::hours(25)).await.expect("age and reseal isolated completion");
                }
                owner.terminate().await.expect("owner ended");
                let mut retention = services.directory().retention();
                retention.run().await.expect("expired original cleanup");
                assert!(!runtime.client().query_one("SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE operation_id=$1)", &[&group.operation_id()]).await.expect("original removed").get::<_,bool>(0));
                assert!(!runtime.client().query_one("SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE operation_id=$1)", &[&rule_operation]).await.expect("rule completion removed").get::<_,bool>(0));
                assert!(runtime.client().query_one("SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE operation_id=$1)", &[&department.operation_id()]).await.expect("young completion retained").get::<_,bool>(0));
                assert_eq!(runtime.client().query_one("SELECT count(*) FROM workforce.rules WHERE id=902", &[]).await.expect("cleanup preserves rule").get::<_,i64>(0), 1);
                let groups = Box::pin(services.directory().list(&manager, Collection::Groups)).await.expect("cleanup preserves configuration");
                assert_eq!(groups.len(), 2);
                assert!(groups.contains(&rule_group) && groups.contains(&group));
                runtime.terminate().await.expect("runtime ended");
                stop(context, services, supplier).await;
            })).await.expect("bounded runtime protections");
        }));
    }

    #[test]
    fn creation_operations_remain_unique_across_collections_after_retention() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let group = create(&services, &manager, operation(), group_creation("Retained group", false, 0)).await;
                let Item::Group(value) = &group else { panic!("group") };
                let department = create(&services, &manager, operation(), department_creation("Retained department", false, 0, value.id(), None)).await;
                let position = create(&services, &manager, operation(), position_creation("Retained position", false, 0)).await;
                let items = [group, department, position];
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                for item in &items {
                    Seed::age(owner.client(), services.directory().app.keys(), super::COMPLETIONS, item.operation_id(), chrono::Duration::hours(25)).await.expect("age and reseal original completion");
                }
                owner.terminate().await.expect("owner ended");
                services.directory().retention().run().await.expect("delivered retention removes expired completions");

                let runtime = database.connections(&database.user).recover().await.expect("runtime negative writer");
                let insertions = [
                    (Collection::Groups, "INSERT INTO workforce.groups(id, operation_id, name, enabled, index) VALUES(gen_random_uuid(), $1, 'New group', true, 0)"),
                    (Collection::Departments, "INSERT INTO workforce.departments(id, operation_id, name, enabled, index, group_id) VALUES(gen_random_uuid(), $1, 'New department', true, 0, (SELECT id FROM workforce.groups))"),
                    (Collection::Positions, "INSERT INTO workforce.positions(id, operation_id, name, enabled, index) VALUES(gen_random_uuid(), $1, 'New position', true, 0)"),
                ];
                for item in &items {
                    assert!(!runtime.client().query_one(
                        "SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE operation_id = $1)",
                        &[&item.operation_id()],
                    ).await.expect("original completion removed").get::<_, bool>(0));
                    for (collection, sql) in insertions {
                        if collection == item.collection() { continue; }
                        runtime.client().batch_execute("BEGIN").await.expect("negative transaction");
                        let error = runtime.client().execute(sql, &[&item.operation_id()]).await
                            .expect_err("creation operation remains occupied by another collection");
                        assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION));
                        assert_eq!(error.as_db_error().expect("creation constraint").constraint(), Some("directory_operation_id_key"));
                        runtime.client().batch_execute("ROLLBACK").await.expect("failed insertion rolled back");
                    }
                    assert_eq!(services.directory().list(&manager, item.collection()).await.expect("original item retained"), vec![item.clone()]);
                }
                runtime.terminate().await.expect("runtime ended");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped before restart");
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.ready().await.expect("startup verification accepts retained creation identities");
                stop(context, services, supplier).await;
            })).await.expect("bounded creation identity retention regression");
        }));
    }

    #[test]
    fn retention_stopped_during_time_observation_finishes_its_lease() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) =
                    Box::pin(start(authentication::proxied(&database, &proxy), &supplier)).await;
                let mut retention = services.directory().retention();
                proxy.arm(Fault::StalledTime);
                let task = tokio::spawn(async move { retention.run().await });
                let clock_pid = timeout(Duration::from_secs(2), proxy.intercepted())
                    .await
                    .expect("directory discovery is sampling time");
                services.close();
                proxy.release();
                let result = timeout(Duration::from_secs(1), task)
                    .await
                    .expect("closed discovery finishes its already borrowed slot")
                    .expect("retention task completed");
                assert!(matches!(result, Err(ref error) if error.is_closed()));
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert!(observer.client().query_one(
                    "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND state='idle')",
                    &[&clock_pid],
                ).await.expect("discovery returned its completed lease").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                stop(context, services, supplier).await;
                proxy.stop().await;
            }).await.expect("bounded stop during directory discovery");
        });
    }

    #[test]
    fn retention_stopped_while_waiting_for_the_pool_preserves_held_slots() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let first = context.connections().acquire().await.expect("first slot");
                let second = context.connections().acquire().await.expect("second slot");
                let mut pids = Vec::new();
                for session in [&first, &second] {
                    pids.push(
                        session
                            .client()
                            .query_one("SELECT pg_backend_pid()", &[])
                            .await
                            .expect("held backend")
                            .get::<_, i32>(0),
                    );
                }
                let mut retention = services.directory().retention();
                let (queued, waiting) = tokio::sync::oneshot::channel();
                let task = tokio::spawn(async move {
                    // Discovery reaches the exhausted pool before observing transaction time.
                    let mut discovery = Box::pin(tokio::task::unconstrained(retention.run()));
                    poll_fn(|context| {
                        assert!(discovery.as_mut().poll(context).is_pending());
                        std::task::Poll::Ready(())
                    })
                    .await;
                    queued.send(()).expect("queued discovery observer");
                    discovery.await
                });
                waiting
                    .await
                    .expect("discovery queued with both slots held");
                services.close();
                let result = timeout(Duration::from_millis(500), task)
                    .await
                    .expect("queued discovery stops without the pool wait timeout")
                    .expect("retention task completed");
                assert!(matches!(
                    result,
                    Err(ref error) if error.is_closed()
                ));
                for (session, pid) in [&first, &second].into_iter().zip(&pids) {
                    assert_eq!(
                        session
                            .client()
                            .query_one("SELECT pg_backend_pid()", &[])
                            .await
                            .expect("held slot remains usable")
                            .get::<_, i32>(0),
                        *pid
                    );
                }
                drop((first, second));
                timeout(Duration::from_secs(1), async {
                    let first = context
                        .connections()
                        .lease()
                        .await
                        .expect("first slot returned");
                    let second = context
                        .connections()
                        .lease()
                        .await
                        .expect("second slot returned");
                    let mut reused = Vec::new();
                    for lease in [&first, &second] {
                        reused.push(
                            lease
                                .client()
                                .query_one("SELECT pg_backend_pid()", &[])
                                .await
                                .expect("returned slot remains usable")
                                .get::<_, i32>(0),
                        );
                    }
                    reused.sort_unstable();
                    pids.sort_unstable();
                    assert_eq!(
                        reused, pids,
                        "both original connections survive cancellation"
                    );
                    first.release();
                    second.release();
                })
                .await
                .expect("cancellation leaks neither online slot");
                stop(context, services, supplier).await;
            })
            .await
            .expect("bounded stop during queued directory discovery");
        });
    }

    #[test]
    fn retention_recovers_commit_faults_and_distinguishes_query_failure_from_damage() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(authentication::proxied(&database, &proxy), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let manager = Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                    let original = operation();
                    let group = create(&services, &manager, original, group_creation("Retained", true, 0)).await;
                    let number = Box::pin(services.number().allocate(manager.clone(), original, serde_json::from_value(json!({"subtype":801})).expect("number input"))).await.expect("same UUID in an independent permanent family");
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    Seed::age(owner.client(), services.directory().app.keys(), super::COMPLETIONS, original, chrono::Duration::hours(25)).await.expect("age and reseal isolated completion");
                    let committed = matches!(fault, Fault::LostCommit);
                    proxy.arm(fault);
                    let mut retention = services.directory().retention();
                    Box::pin(retention.run()).await.expect("cleanup retains retry after confirmed rollback");
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual cleanup commit fault");
                    assert_eq!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE family='workforce_configuration' AND operation_id=$1)", &[&original]).await.expect("independent original cleanup outcome before another pass").get::<_, bool>(0), !committed);
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM workforce.groups WHERE operation_id=$1", &[&original]).await.expect("cleanup never deletes configuration").get::<_, i64>(0), 1);
                    Box::pin(retention.run()).await.expect("next cleanup pass");
                    assert!(!owner.client().query_one("SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE family='workforce_configuration' AND operation_id=$1)", &[&original]).await.expect("expired original gone").get::<_, bool>(0));
                    assert!(owner.client().query_one("SELECT EXISTS(SELECT 1 FROM workforce.completions WHERE family='number_allocation' AND operation_id=$1 AND expires_at IS NULL)", &[&original]).await.expect("permanent original retained").get::<_, bool>(0));
                    assert_eq!(Box::pin(services.number().allocate(manager.clone(), original, serde_json::from_value(json!({"subtype":801})).expect("same input"))).await.expect("permanent replay after cleanup").id(), number.id());
                    assert_eq!(Box::pin(services.directory().list(&manager, Collection::Groups)).await.expect("configuration still readable"), vec![group]);
                    owner.terminate().await.expect("owner ended");
                    stop(context, services, supplier).await; proxy.stop().await;
                })).await.expect("bounded cleanup fault scenario");
            }
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                let manager = Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                let original = operation();
                create(&services, &manager, original, group_creation("Damaged completion", true, 0)).await;
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                owner.client().batch_execute("ALTER TABLE workforce.completions RENAME TO withheld_completions").await.expect("temporary query failure");
                let mut retention = services.directory().retention();
                Box::pin(retention.run()).await.expect("query failure keeps the next scan available");
                services.directory().registry().ready().expect("ordinary query error does not invalidate facts");
                owner.client().batch_execute("ALTER TABLE workforce.withheld_completions RENAME TO completions").await.expect("restore completion table");
                Seed::age(owner.client(), services.directory().app.keys(), super::COMPLETIONS, original, chrono::Duration::hours(25)).await.expect("age and reseal isolated completion");
                owner.client().batch_execute("ALTER TABLE workforce.completions DROP CONSTRAINT completions_input_check; UPDATE workforce.completions SET input=decode('00','hex')").await.expect("inject isolated damaged envelope");
                assert!(Box::pin(retention.run()).await.is_err_and(|error| error.is_corrupt()));
                assert!(services.directory().registry().ready().is_err(), "damaged completion closes admission");
                assert_eq!(owner.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=$1", &[&original]).await.expect("damaged completion is preserved").get::<_, i64>(0), 1);
                owner.terminate().await.expect("owner ended");
                stop(context, services, supplier).await;
            })).await.expect("bounded cleanup scan scenario");
            // A completion whose action no longer pairs with its target shape is damage, not an
            // expired candidate: cleanup preserves it and closes admission.
            for (action, target) in [("create", Uuid::new_v4().to_string()), ("delete", "groups".to_owned()), ("replace", "100".to_owned()), ("update", "departments".to_owned())] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let manager = Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                    let original = operation();
                    create(&services, &manager, original, group_creation("Damaged pairing", true, 0)).await;
                    let owner = database.connections("postgres").recover().await.expect("isolated owner");
                    Seed::age(owner.client(), services.directory().app.keys(), super::COMPLETIONS, original, chrono::Duration::hours(25)).await.expect("age and reseal isolated completion");
                    owner.client().execute("UPDATE workforce.completions SET action=$1, target=$2 WHERE operation_id=$3", &[&action, &target, &original]).await.expect("inject an isolated damaged pairing");
                    let mut retention = services.directory().retention();
                    assert!(Box::pin(retention.run()).await.is_err_and(|error| error.is_corrupt()), "{action} {target}");
                    assert!(services.directory().registry().ready().is_err(), "damaged pairing closes admission");
                    assert_eq!(owner.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=$1", &[&original]).await.expect("damaged pairing is preserved").get::<_, i64>(0), 1);
                    owner.terminate().await.expect("owner ended");
                    stop(context, services, supplier).await;
                })).await.expect("bounded damaged pairing scenario");
            }
        }));
    }

    #[test]
    fn crud_names_conditional_writes_and_replays_use_real_runtime_identity() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                assert!(matches!(Box::pin(services.directory().list(&institution, Collection::Groups)).await, Err(Error::Denied)));

                let creation = operation();
                let group = create(&services, &manager, creation, group_creation("  Cafe\u{301}  ", false, 4)).await;
                let Item::Group(group_value) = &group else { panic!("group") };
                assert_eq!(group_value.name().as_str(), "Café");
                assert!(!group_value.enabled());
                let alpha = create(&services, &manager, operation(), group_creation("alpha", true, 4)).await;
                assert!(matches!(Box::pin(create_request(services.directory(), manager.clone(), operation(), Collection::Departments, department_creation("Café", true, 0, group_value.id(), None))).await, Err(Error::Rejected(Rejection::NameInUse))), "disabled groups still occupy the cross-table name scope");
                assert!(matches!(Box::pin(create_request(services.directory(), manager.clone(), operation(), Collection::Groups, group_creation("Cafe\u{301}", true, 0))).await, Err(Error::Rejected(Rejection::NameInUse))));
                assert!(matches!(Box::pin(create_request(services.directory(), manager.clone(), creation, Collection::Groups, group_creation("Different", true, 0))).await, Err(Error::Mismatch)));

                let other = create(&services, &manager, operation(), department_creation("Alpha", true, 3, group_value.id(), None)).await;
                let department_creation_operation = operation();
                let department = create(&services, &manager, department_creation_operation, department_creation("Clinical", true, 3, group_value.id(), None)).await;
                let child = create(&services, &manager, operation(), department_creation("Child", false, 0, group_value.id(), Some(department.target().into_department()))).await;
                assert!(matches!(Box::pin(create_request(services.directory(), manager.clone(), operation(), Collection::Groups, group_creation("Clinical", true, 0))).await, Err(Error::Rejected(Rejection::NameInUse))));
                assert!(matches!(Box::pin(delete(services.directory(), manager.clone(), operation(), group.target(), deletion(&group))).await, Err(Error::Rejected(Rejection::InUse))));
                assert!(matches!(Box::pin(delete(services.directory(), manager.clone(), operation(), department.target(), deletion(&department))).await, Err(Error::Rejected(Rejection::InUse))));

                let groups = Box::pin(services.directory().list(&manager, Collection::Groups)).await.expect("groups including disabled");
                assert_eq!(groups.len(), 2);
                assert!(groups.iter().any(|item| item == &group));
                let departments = Box::pin(services.directory().list(&manager, Collection::Departments)).await.expect("departments including disabled");
                assert_eq!(departments.len(), 3);
                assert_eq!(departments[0], child, "index then stable id orders the complete collection");
                assert!(departments.contains(&other) && departments.contains(&department));

                let (update_operation, update, later) = Box::pin(check_updates(&services, &manager, &department, &child, department_creation_operation)).await;

                assert_eq!(Box::pin(delete(services.directory(), manager.clone(), operation(), child.target(), deletion(&child))).await.expect("child deletion"), Receipt::Deleted);
                let delete_operation = operation();
                assert_eq!(Box::pin(delete(services.directory(), manager.clone(), delete_operation, department.target(), deletion(&later))).await.expect("delete without children"), Receipt::Deleted);
                assert_eq!(Box::pin(delete(services.directory(), manager.clone(), delete_operation, department.target(), deletion(&later))).await.expect("original deleted replay"), Receipt::Deleted);
                assert_eq!(Box::pin(update_request(services.directory(), manager.clone(), update_operation, department.target(), update)).await.expect("updated then deleted replay"), Receipt::Deleted);
                assert_eq!(Box::pin(create_request(services.directory(), manager.clone(), department_creation_operation, Collection::Departments, department_creation("Clinical", true, 3, group_value.id(), None))).await.expect("created then deleted replay"), Receipt::Deleted);
                assert!(matches!(Box::pin(delete(services.directory(), manager.clone(), operation(), department.target(), deletion(&later))).await, Err(Error::Rejected(Rejection::Absent))), "a new deletion of an already absent target");
                assert_eq!(Box::pin(delete(services.directory(), manager.clone(), operation(), other.target(), deletion(&other))).await.expect("other department deletion"), Receipt::Deleted);
                assert_eq!(Box::pin(delete(services.directory(), manager.clone(), operation(), group.target(), deletion(&group))).await.expect("unreferenced group deletion"), Receipt::Deleted);
                assert_eq!(Box::pin(delete(services.directory(), manager.clone(), operation(), alpha.target(), deletion(&alpha))).await.expect("second group deletion"), Receipt::Deleted);
                assert!(Box::pin(services.directory().list(&manager, Collection::Groups)).await.expect("empty groups").is_empty());
                assert!(Box::pin(services.directory().list(&manager, Collection::Departments)).await.expect("empty departments").is_empty());

                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                let columns = observer.client().query("SELECT attname,attnotnull FROM pg_catalog.pg_attribute WHERE attrelid='workforce.completions'::regclass AND attnum>0 AND NOT attisdropped ORDER BY attnum", &[]).await.expect("exact completion columns");
                assert_eq!(columns.iter().map(|column| column.get::<_, &str>("attname")).collect::<std::collections::HashSet<_>>(), std::collections::HashSet::from([
                    "family", "operation_id", "action", "target", "input", "subject", "person_id", "revision_id", "created_at", "expires_at", "seal", "seal_key_id", "seal_version",
                ]));
                assert!(columns.iter().all(|column| column.get::<_, bool>("attnotnull") == (column.get::<_, &str>("attname") != "expires_at")));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=$1", &[&creation]).await.expect("one creation completion").get::<_, i64>(0), 1);
                assert_eq!(Seed::input(observer.client(), services.directory().app.keys(), super::COMPLETIONS, creation).await.expect("authenticated canonical input").as_slice(), r#"{"name":"Café","enabled":false,"index":4}"#.as_bytes());
                observer.terminate().await.expect("observer ended");
                stop(context, services, supplier).await;
            })).await.expect("bounded CRUD and replay acceptance");
        }));
    }

    async fn check_updates(
        services: &Services,
        manager: &Actor,
        department: &Item,
        child: &Item,
        department_creation_operation: operation::Id,
    ) -> (operation::Id, Update, Item) {
        let mut replacement = department.clone();
        let Item::Department(value) = &mut replacement else {
            panic!("department")
        };
        value.name = "Updated".parse().expect("name");
        value.enabled = false;
        value.index = 9.try_into().expect("index");
        let update_operation = operation();
        let update = modification(department, &replacement);
        let updated = present(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                update_operation,
                department.target(),
                update.clone(),
            ))
            .await
            .expect("complete conditional update"),
        );
        assert_eq!(updated, replacement);
        let noop = present(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                operation(),
                department.target(),
                modification(department, &replacement),
            ))
            .await
            .expect("current target state is a completed no-op even with an old expected"),
        );
        assert_eq!(noop, replacement);
        assert!(matches!(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                operation(),
                department.target(),
                modification(department, child)
            ))
            .await,
            Err(Error::Rejected(Rejection::Input))
        ));
        assert_eq!(
            present(
                Box::pin(update_request(
                    services.directory(),
                    manager.clone(),
                    update_operation,
                    department.target(),
                    update.clone()
                ))
                .await
                .expect("matching original update")
            ),
            replacement
        );

        let mut later = replacement.clone();
        let Item::Department(value) = &mut later else {
            panic!("department")
        };
        value.name = "Later".parse().expect("name");
        let later = present(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                operation(),
                department.target(),
                modification(&replacement, &later),
            ))
            .await
            .expect("later current state"),
        );
        assert_eq!(
            present(
                Box::pin(update_request(
                    services.directory(),
                    manager.clone(),
                    update_operation,
                    department.target(),
                    update.clone()
                ))
                .await
                .expect("original completion returns the current visible item")
            ),
            later
        );
        assert!(matches!(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                update_operation,
                department.target(),
                modification(department, child)
            ))
            .await,
            Err(Error::Rejected(Rejection::Input))
        ));
        assert!(
            matches!(
                Box::pin(delete(
                    services.directory(),
                    manager.clone(),
                    department_creation_operation,
                    department.target(),
                    deletion(&later)
                ))
                .await,
                Err(Error::Mismatch)
            ),
            "action changes cannot reuse a creation operation"
        );

        let mut changed_expected = update.clone();
        let Item::Department(value) = &mut changed_expected.expected else {
            panic!("department")
        };
        value.index = 17.try_into().expect("index");
        assert!(matches!(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                update_operation,
                department.target(),
                changed_expected
            ))
            .await,
            Err(Error::Mismatch)
        ));
        assert!(matches!(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                update_operation,
                child.target(),
                modification(child, child)
            ))
            .await,
            Err(Error::Mismatch)
        ));
        let restored = present(
            Box::pin(update_request(
                services.directory(),
                manager.clone(),
                operation(),
                department.target(),
                modification(&later, department),
            ))
            .await
            .expect("another operation restores the old expected"),
        );
        assert_eq!(restored, *department);
        assert_eq!(
            present(
                Box::pin(update_request(
                    services.directory(),
                    manager.clone(),
                    update_operation,
                    department.target(),
                    update.clone()
                ))
                .await
                .expect("retained completion does not apply twice")
            ),
            restored
        );
        (update_operation, update, restored)
    }

    #[test]
    fn concurrent_creates_and_renames_have_exactly_one_winner() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let parent = create(&services, &manager, operation(), group_creation("Parent", true, 0)).await;
                let Item::Group(parent_group) = &parent else { panic!("group") };
                let operations = [operation(), operation()];
                let gate = database.connections(&database.user).recover().await.expect("runtime name-scope gate");
                gate.client().batch_execute("BEGIN; SELECT workforce.lock_directory()").await.expect("hold scope before concurrent service calls");
                let (_, first, second) = tokio::join!(
                    release_directory_waiters(gate.client(), 2),
                    Box::pin(create_request(services.directory(), manager.clone(), operations[0], Collection::Groups, group_creation("Shared", true, 0))),
                    Box::pin(create_request(services.directory(), manager.clone(), operations[1], Collection::Departments, department_creation("Shared", true, 0, parent_group.id(), None)))
                );
                assert!(matches!((&first, &second), (Ok(_), Err(Error::Rejected(Rejection::NameInUse))) | (Err(Error::Rejected(Rejection::NameInUse)), Ok(_))), "one cross-table creation wins: {first:?} {second:?}");
                gate.terminate().await.expect("gate ended");
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM workforce.groups WHERE name='Shared')+(SELECT count(*) FROM workforce.departments WHERE name='Shared')", &[]).await.expect("one stored winner").get::<_, i64>(0), 1);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=ANY($1::uuid[])", &[&operations.as_slice()]).await.expect("only the winner completes").get::<_, i64>(0), 1);

                let left = create(&services, &manager, operation(), group_creation("Left", true, 0)).await;
                let right = create(&services, &manager, operation(), department_creation("Right", true, 0, parent_group.id(), None)).await;
                let mut left_replacement = left.clone();
                let Item::Group(value) = &mut left_replacement else { panic!("group") };
                value.name = "Rename".parse().expect("name");
                let mut right_replacement = right.clone();
                let Item::Department(value) = &mut right_replacement else { panic!("department") };
                value.name = "Rename".parse().expect("name");
                let operations = [operation(), operation()];
                let gate = database.connections(&database.user).recover().await.expect("runtime name-scope gate");
                gate.client().batch_execute("BEGIN; SELECT workforce.lock_directory()").await.expect("hold scope before concurrent service calls");
                let (_, first, second) = tokio::join!(
                    release_directory_waiters(gate.client(), 2),
                    Box::pin(update_request(services.directory(), manager.clone(), operations[0], left.target(), modification(&left, &left_replacement))),
                    Box::pin(update_request(services.directory(), manager.clone(), operations[1], right.target(), modification(&right, &right_replacement)))
                );
                gate.terminate().await.expect("rename gate ended");
                assert!(matches!((&first, &second), (Ok(_), Err(Error::Rejected(Rejection::NameInUse))) | (Err(Error::Rejected(Rejection::NameInUse)), Ok(_))), "one cross-table rename wins");
                assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM workforce.groups WHERE name='Rename')+(SELECT count(*) FROM workforce.departments WHERE name='Rename')", &[]).await.expect("one renamed winner").get::<_, i64>(0), 1);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=ANY($1::uuid[])", &[&operations.as_slice()]).await.expect("only one rename completes").get::<_, i64>(0), 1);
                observer.terminate().await.expect("observer ended");
                stop(context, services, supplier).await;
            })).await.expect("bounded name races");
        }));
    }

    #[test]
    fn department_cycles_subtree_moves_and_concurrent_children_are_atomic() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(45),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let first = create(
                        &services,
                        &manager,
                        operation(),
                        group_creation("First", true, 0),
                    )
                    .await;
                    let second = create(
                        &services,
                        &manager,
                        operation(),
                        group_creation("Second", true, 0),
                    )
                    .await;
                    let Item::Group(first_group) = &first else {
                        panic!("first group")
                    };
                    let Item::Group(second_group) = &second else {
                        panic!("second group")
                    };
                    let root = create(
                        &services,
                        &manager,
                        operation(),
                        department_creation("Root", true, 0, first_group.id(), None),
                    )
                    .await;
                    let child = create(
                        &services,
                        &manager,
                        operation(),
                        department_creation(
                            "Child",
                            true,
                            0,
                            first_group.id(),
                            Some(root.target().into_department()),
                        ),
                    )
                    .await;
                    let grandchild = create(
                        &services,
                        &manager,
                        operation(),
                        department_creation(
                            "Grandchild",
                            true,
                            0,
                            first_group.id(),
                            Some(child.target().into_department()),
                        ),
                    )
                    .await;
                    let mut cyclic = root.clone();
                    let Item::Department(value) = &mut cyclic else {
                        panic!("root")
                    };
                    value.parent_id = Some(grandchild.target().into_department());
                    assert!(matches!(
                        Box::pin(update_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            root.target(),
                            modification(&root, &cyclic)
                        ))
                        .await,
                        Err(Error::Rejected(Rejection::InvalidParent))
                    ));
                    let Item::Department(value) = &mut cyclic else {
                        panic!("root")
                    };
                    value.parent_id = Some(root.target().into_department());
                    assert!(matches!(
                        Box::pin(update_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            root.target(),
                            modification(&root, &cyclic)
                        ))
                        .await,
                        Err(Error::Rejected(Rejection::InvalidParent))
                    ));
                    let mut moved = root.clone();
                    let Item::Department(value) = &mut moved else {
                        panic!("root")
                    };
                    value.group_id = second_group.id();
                    value.parent_id = None;
                    let moved = present(
                        Box::pin(update_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            root.target(),
                            modification(&root, &moved),
                        ))
                        .await
                        .expect("whole-subtree cross-group move"),
                    );
                    let departments =
                        Box::pin(services.directory().list(&manager, Collection::Departments))
                            .await
                            .expect("moved subtree");
                    assert_eq!(departments.len(), 3);
                    for item in departments {
                        let Item::Department(item) = item else {
                            panic!("department")
                        };
                        assert_eq!(item.group_id(), second_group.id());
                    }
                    let Item::Department(moved_root) = &moved else {
                        panic!("moved root")
                    };
                    assert_eq!(moved_root.parent_id(), None);

                    let Item::Department(root_value) = &moved else {
                        panic!("root")
                    };
                    let mut returned = moved.clone();
                    let Item::Department(value) = &mut returned else {
                        panic!("root")
                    };
                    value.group_id = first_group.id();
                    let create_operation = operation();
                    let move_operation = operation();
                    let gate = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("runtime name-scope gate");
                    gate.client()
                        .batch_execute("BEGIN; SELECT workforce.lock_directory()")
                        .await
                        .expect("hold scope before concurrent service calls");
                    let (_, new_child, move_result) = tokio::join!(
                        release_directory_waiters(gate.client(), 2),
                        Box::pin(create_request(
                            services.directory(),
                            manager.clone(),
                            create_operation,
                            Collection::Departments,
                            department_creation(
                                "Concurrent",
                                true,
                                0,
                                root_value.group_id(),
                                Some(grandchild.target().into_department())
                            )
                        )),
                        Box::pin(update_request(
                            services.directory(),
                            manager.clone(),
                            move_operation,
                            moved.target(),
                            modification(&moved, &returned)
                        ))
                    );
                    gate.terminate().await.expect("tree gate ended");
                    let moved_back =
                        present(move_result.expect("move always has a valid current expected"));
                    let Item::Department(root_value) = &moved_back else {
                        panic!("returned root")
                    };
                    assert_eq!(root_value.group_id(), first_group.id());
                    let departments =
                        Box::pin(services.directory().list(&manager, Collection::Departments))
                            .await
                            .expect("coordinated final tree");
                    match new_child {
                        Ok(Receipt::Present(_)) => {
                            assert_eq!(
                                departments.len(),
                                4,
                                "child creation before the move is included in that move"
                            );
                            for item in departments {
                                let Item::Department(item) = item else {
                                    panic!("department")
                                };
                                assert_eq!(item.group_id(), first_group.id());
                            }
                        }
                        Err(Error::Rejected(Rejection::InvalidParent)) => assert_eq!(
                            departments.len(),
                            3,
                            "child creation after the move sees the new parent group"
                        ),
                        other => panic!("coordinated child outcome: {other:?}"),
                    }
                    stop(context, services, supplier).await;
                }),
            )
            .await
            .expect("bounded tree and child coordination");
        }));
    }

    #[test]
    fn group_updates_and_department_reference_rejections_preserve_completions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(45),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let manager =
                        Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                    let group = create(
                        &services,
                        &manager,
                        operation(),
                        group_creation("First", true, 0),
                    )
                    .await;
                    let other = create(
                        &services,
                        &manager,
                        operation(),
                        group_creation("Second", true, 0),
                    )
                    .await;
                    let Item::Group(first) = &group else {
                        panic!("group")
                    };
                    let Item::Group(second) = &other else {
                        panic!("group")
                    };
                    let parent = create(
                        &services,
                        &manager,
                        operation(),
                        department_creation("Parent", true, 0, first.id(), None),
                    )
                    .await;
                    for (group_id, parent_id, absent) in [
                        (group::Id::from(Uuid::new_v4()), None, true),
                        (first.id(), Some(department::Id::from(Uuid::new_v4())), true),
                        (second.id(), Some(parent.target().into_department()), false),
                    ] {
                        let result = Box::pin(create_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Collection::Departments,
                            department_creation("Rejected", true, 0, group_id, parent_id),
                        ))
                        .await;
                        assert!(if absent {
                            matches!(result, Err(Error::Rejected(Rejection::Absent)))
                        } else {
                            matches!(result, Err(Error::Rejected(Rejection::InvalidParent)))
                        });
                    }
                    let mut replacement = group.clone();
                    let Item::Group(value) = &mut replacement else {
                        panic!("group")
                    };
                    value.name = "Renamed".parse().expect("name");
                    value.enabled = false;
                    value.index = 20.try_into().expect("index");
                    let original = operation();
                    assert_eq!(
                        present(
                            Box::pin(update_request(
                                services.directory(),
                                manager.clone(),
                                original,
                                group.target(),
                                modification(&group, &replacement)
                            ))
                            .await
                            .expect("group update")
                        ),
                        replacement
                    );
                    assert_eq!(
                        Box::pin(services.directory().list(&manager, Collection::Groups))
                            .await
                            .expect("sorted groups"),
                        vec![other, replacement.clone()]
                    );
                    assert_eq!(
                        present(
                            Box::pin(update_request(
                                services.directory(),
                                manager.clone(),
                                original,
                                group.target(),
                                modification(&group, &replacement)
                            ))
                            .await
                            .expect("group replay")
                        ),
                        replacement
                    );
                    stop(context, services, supplier).await;
                }),
            )
            .await
            .expect("bounded group and reference acceptance");
        }));
    }

    #[test]
    fn unknown_commit_and_cancelled_wait_retain_the_original_work() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::UnknownCommit, Fault::DelayedCommit] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(authentication::proxied(&database, &proxy), &supplier)).await;
                    let institution = Box::pin(actor(&database, &services, &mut supplier)).await;
                    let manager = Box::pin(manager(&database, &services, &mut supplier, &institution)).await;
                    let original = operation();
                    let cancelled = matches!(fault, Fault::DelayedCommit);
                    proxy.arm(fault);
                    let waiter = tokio::spawn({
                        let service = services.directory().clone(); let actor = manager.clone();
                        async move { service.create(actor, original, Collection::Groups, group_creation("Original", true, 0)).await }
                    });
                    timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual directory commit fault");
                    if cancelled {
                        waiter.abort(); assert!(waiter.await.expect_err("waiter cancelled").is_cancelled());
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "work survives cancellation of its waiter");
                        proxy.release(); services.drain().await.expect("original work completes");
                    } else {
                        proxy.release();
                        assert!(matches!(waiter.await.expect("waiter"), Err(Error::Unknown(_))));
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("context stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    assert_eq!(observer.client().query_one("SELECT (SELECT count(*) FROM workforce.groups WHERE operation_id=$1), (SELECT count(*) FROM workforce.completions WHERE operation_id=$1)", &[&original]).await.expect("original facts before replay").get::<_, i64>(0), 1);
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=$1", &[&original]).await.expect("original completion before replay").get::<_, i64>(0), 1);
                    proxy.stop().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    Box::pin(create_request(services.directory(), manager, original, Collection::Groups, group_creation("Original", true, 0))).await.expect("replay after restart");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.groups", &[]).await.expect("only one item").get::<_, i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    stop(context, services, supplier).await;
                })).await.expect("bounded unknown commit scenario");
            }
        }));
    }

    #[test]
    fn commit_faults_recover_completions_and_startup_verifies_stored_directories() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = Box::pin(start(authentication::proxied(&database, &proxy), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let original = operation();
                proxy.arm(Fault::LostCommit);
                let created = present(Box::pin(create_request(services.directory(), manager.clone(), original, Collection::Groups, group_creation("Recovered", true, 0))).await.expect("lost commit response recovers the original completion"));
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual commit fault");
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.groups WHERE operation_id=$1", &[&original]).await.expect("one recovered item").get::<_, i64>(0), 1);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=$1", &[&original]).await.expect("one recovered completion").get::<_, i64>(0), 1);
                assert_eq!(present(Box::pin(create_request(services.directory(), manager.clone(), original, Collection::Groups, group_creation("Recovered", true, 0))).await.expect("replay after recovery")), created);
                let rejected = operation();
                proxy.arm(Fault::RolledBack);
                assert!(matches!(Box::pin(create_request(services.directory(), manager.clone(), rejected, Collection::Groups, group_creation("Rolled back", true, 0))).await, Err(Error::Unavailable(_))));
                timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual rollback");
                assert!(!observer.client().query_one("SELECT EXISTS (SELECT 1 FROM workforce.completions WHERE operation_id=$1)", &[&rejected]).await.expect("no completion after rollback").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;

                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.ready().await.expect("valid directories and completions survive restart");
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                owner.client().batch_execute("BEGIN; ALTER TABLE workforce.groups DROP CONSTRAINT groups_name_check; SET LOCAL session_replication_role=replica; UPDATE workforce.groups SET name=' damaged '; COMMIT").await.expect("stored noncanonical name");
                owner.terminate().await.expect("owner ended");
                let context = Context::new(database.connections(&database.user)).await.expect("verification context");
                let barrier = Barrier::begin(context.clone()).await.expect("verification barrier");
                let result = Services::start(
                    barrier,
                    authentication::inventory(authentication::keys()),
                    Settings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings"),
                    supplier.client(),
                    authentication::unused_client().await,
                ).await;
                assert!(matches!(result, Err(crate::startup::Error::Directory(_))), "startup rejects a noncanonical stored directory name");
                context.stop().await.expect("verification context stopped");
                supplier.stop().await;
            })).await.expect("bounded recovery and startup verification");
        }));
    }

    #[test]
    fn rules_expose_missing_validate_lineage_and_preserve_later_references() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let initial = services
                    .directory()
                    .list_rules(&manager)
                    .await
                    .expect("missing rules are explicit during initialization");
                assert!(initial.rules.is_empty());
                assert_eq!(initial.missing, Subtype::ALL);
                assert!(matches!(
                    services.directory().list_rules(&institution).await,
                    Err(Error::Denied)
                ));

                let group = create(
                    &services,
                    &manager,
                    operation(),
                    group_creation("Rule group", true, 0),
                )
                .await;
                let Item::Group(group_value) = &group else {
                    panic!("rule group")
                };
                let root = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation("Rule root", true, 0, group_value.id(), None),
                )
                .await;
                let child = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation(
                        "Rule child",
                        true,
                        0,
                        group_value.id(),
                        Some(root.target().into_department()),
                    ),
                )
                .await;
                let other = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation("Rule other", true, 0, group_value.id(), None),
                )
                .await;
                let child_id = child.target().into_department();
                let other_id = other.target().into_department();
                let missing = rule_value(Subtype::Clinical, child_id, true);
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Subtype::Clinical,
                            rule_modification(&missing, &missing),
                        )
                        .await,
                    Err(Error::Rejected(Rejection::Absent))
                ));
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Subtype::Clinical,
                            rule_creation(department::Id::from(Uuid::new_v4()), true),
                        )
                        .await,
                    Err(Error::Rejected(Rejection::Absent))
                ));
                let group_as_department = serde_json::from_value(json!(group_value.id()))
                    .expect("group UUID cannot become a department reference");
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Subtype::Clinical,
                            rule_creation(group_as_department, true),
                        )
                        .await,
                    Err(Error::Rejected(Rejection::Absent))
                ));

                let create_operation = operation();
                let create_input = rule_creation(child_id, false);
                let created = configure(
                    &services,
                    &manager,
                    create_operation,
                    Subtype::Internship,
                    create_input.clone(),
                )
                .await;
                assert!(created.created());
                assert_eq!(created.rule(), &rule_value(Subtype::Internship, child_id, false));
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Subtype::Internship,
                            create_input.clone(),
                        )
                        .await,
                    Err(Error::Rejected(Rejection::Changed))
                ));
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            create_operation,
                            Subtype::Internship,
                            rule_creation(child_id, true),
                        )
                        .await,
                    Err(Error::Mismatch)
                ));

                let update_operation = operation();
                let updated_value = rule_value(Subtype::Internship, other_id, true);
                let update_input = rule_modification(created.rule(), &updated_value);
                let updated = configure(
                    &services,
                    &manager,
                    update_operation,
                    Subtype::Internship,
                    update_input.clone(),
                )
                .await;
                assert!(!updated.created());
                assert_eq!(updated.rule(), &updated_value);
                assert!(matches!(
                    configure_request(services.directory(), manager.clone(), create_operation,
                        Subtype::Clinical, rule_creation(child_id, false)).await,
                    Err(Error::Mismatch)
                ), "the original target binds the subtype even when the body is unchanged");
                assert!(matches!(
                    configure_request(services.directory(), manager.clone(), create_operation,
                        Subtype::Internship, update_input.clone()).await,
                    Err(Error::Mismatch)
                ), "creation cannot replay as an update");
                let stale_replacement = rule_value(Subtype::Internship, child_id, true);
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Subtype::Internship,
                            rule_modification(created.rule(), &stale_replacement),
                        )
                        .await,
                    Err(Error::Rejected(Rejection::Changed))
                ));
                let mismatched_replacement = rule_value(Subtype::Internship, other_id, false);
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            update_operation,
                            Subtype::Internship,
                            rule_modification(created.rule(), &mismatched_replacement),
                        )
                        .await,
                    Err(Error::Mismatch)
                ));
                let replayed_update = configure(
                    &services,
                    &manager,
                    update_operation,
                    Subtype::Internship,
                    update_input,
                )
                .await;
                assert!(!replayed_update.created());
                assert_eq!(replayed_update.rule(), updated.rule());
                let replayed_create = configure(
                    &services,
                    &manager,
                    create_operation,
                    Subtype::Internship,
                    create_input,
                )
                .await;
                assert!(replayed_create.created());
                assert_eq!(replayed_create.rule(), updated.rule());

                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("rule completion observer");
                let row = observer
                    .client()
                    .query_one(
                        "SELECT action, target FROM workforce.completions WHERE operation_id = $1",
                        &[&create_operation],
                    )
                    .await
                    .expect("rule creation completion");
                assert_eq!(row.get::<_, &str>("action"), "create");
                assert_eq!(row.get::<_, &str>("target"), "902");
                assert_eq!(Seed::input(observer.client(), services.directory().app.keys(), super::COMPLETIONS, create_operation).await.expect("authenticated rule creation input").as_slice(), format!(r#"{{"departmentId":"{child_id}","position":false}}"#).as_bytes());
                let row = observer
                    .client()
                    .query_one(
                        "SELECT action, target FROM workforce.completions WHERE operation_id = $1",
                        &[&update_operation],
                    )
                    .await
                    .expect("rule update completion");
                assert_eq!(row.get::<_, &str>("action"), "update");
                assert_eq!(row.get::<_, &str>("target"), "902");
                assert_eq!(Seed::input(observer.client(), services.directory().app.keys(), super::COMPLETIONS, update_operation).await.expect("authenticated complete rule update input").as_slice(), format!(r#"{{"expected":{{"id":902,"departmentId":"{child_id}","position":false}},"replacement":{{"id":902,"departmentId":"{other_id}","position":true}}}}"#).as_bytes());
                observer.terminate().await.expect("observer ended");
                assert!(matches!(
                    delete(
                        services.directory(),
                        manager.clone(),
                        operation(),
                        other.target(),
                        deletion(&other),
                    )
                    .await,
                    Err(Error::Rejected(Rejection::InUse))
                ));

                let disabled_department = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation("Disabled default", false, 0, group_value.id(), None),
                )
                .await;
                let disabled_root = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation("Disabled ancestor", false, 0, group_value.id(), None),
                )
                .await;
                let enabled_descendant = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation(
                        "Enabled descendant",
                        true,
                        0,
                        group_value.id(),
                        Some(disabled_root.target().into_department()),
                    ),
                )
                .await;
                let disabled_group = create(
                    &services,
                    &manager,
                    operation(),
                    group_creation("Disabled rule group", false, 0),
                )
                .await;
                let Item::Group(disabled_group_value) = &disabled_group else {
                    panic!("disabled group")
                };
                let disabled_group_department = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation(
                        "Department in disabled group",
                        true,
                        0,
                        disabled_group_value.id(),
                        None,
                    ),
                )
                .await;
                for (subtype, item) in [
                    (Subtype::Clinical, disabled_department),
                    (Subtype::Nursing, enabled_descendant),
                    (Subtype::Technology, disabled_group_department),
                ] {
                    assert!(matches!(
                        configure_request(
                                services.directory(),
                                manager.clone(),
                                operation(),
                                subtype,
                                rule_creation(item.target().into_department(), true),
                            )
                            .await,
                        Err(Error::Rejected(Rejection::InvalidDepartment))
                    ));
                }

                let mut disabled_replacement = other.clone();
                let Item::Department(value) = &mut disabled_replacement else {
                    panic!("disable referenced department")
                };
                value.enabled = false;
                let disabled_other = present(
                    update_request(
                        services.directory(),
                        manager.clone(),
                        operation(),
                        other.target(),
                        modification(&other, &disabled_replacement),
                    )
                    .await
                    .expect("later disabling is allowed"),
                );
                let retained = services
                    .directory()
                    .list_rules(&manager)
                    .await
                    .expect("later disabling retains the original rule reference");
                assert_eq!(retained.rules, vec![updated_value.clone()]);
                let original_after_disabling = configure(&services, &manager, update_operation,
                    Subtype::Internship, rule_modification(created.rule(), &updated_value)).await;
                assert_eq!(original_after_disabling.rule(), &updated_value);
                assert!(matches!(
                    configure_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Subtype::Internship,
                            rule_modification(&updated_value, &updated_value),
                        )
                        .await,
                    Err(Error::Rejected(Rejection::InvalidDepartment))
                ));
                let mut enabled_replacement = disabled_other.clone();
                let Item::Department(value) = &mut enabled_replacement else {
                    panic!("reenable referenced department")
                };
                value.enabled = true;
                let enabled_other = present(
                    update_request(
                        services.directory(),
                        manager.clone(),
                        operation(),
                        disabled_other.target(),
                        modification(&disabled_other, &enabled_replacement),
                    )
                    .await
                    .expect("real service reenables the department"),
                );
                let second_group = create(
                    &services,
                    &manager,
                    operation(),
                    group_creation("Second rule group", true, 0),
                )
                .await;
                let Item::Group(second_group_value) = &second_group else {
                    panic!("second rule group")
                };
                let mut moved_replacement = enabled_other.clone();
                let Item::Department(value) = &mut moved_replacement else {
                    panic!("move referenced department")
                };
                value.group_id = second_group_value.id();
                value.parent_id = None;
                present(
                    update_request(
                        services.directory(),
                        manager.clone(),
                        operation(),
                        enabled_other.target(),
                        modification(&enabled_other, &moved_replacement),
                    )
                    .await
                    .expect("later move retains the rule reference"),
                );
                let retained = services
                    .directory()
                    .list_rules(&manager)
                    .await
                    .expect("rule remains readable after later directory edits");
                assert_eq!(retained.rules, vec![updated_value.clone()]);
                assert_eq!(retained.missing.len(), 15);
                let no_op = configure(
                    &services,
                    &manager,
                    operation(),
                    Subtype::Internship,
                    rule_modification(created.rule(), &updated_value),
                )
                .await;
                assert!(!no_op.created());
                assert_eq!(no_op.rule(), &updated_value);
                for subtype in Subtype::ALL {
                    if subtype != Subtype::Internship {
                        configure(&services, &manager, operation(), subtype,
                            rule_creation(other_id, true)).await;
                    }
                }
                let complete = services.directory().list_rules(&manager).await.expect("all fixed rules");
                assert!(complete.missing.is_empty());
                assert_eq!(complete.rules.iter().map(rule::Rule::id).collect::<Vec<_>>(), Subtype::ALL);
                assert!(complete.rules.iter().all(rule::Rule::position), "the switch is configured, including 902");
                assert_eq!(delete(services.directory(), manager.clone(), operation(), child.target(), deletion(&child))
                    .await.expect("a replaced default no longer protects its old department"), Receipt::Deleted);
                stop(context, services, supplier).await;
            }))
            .await
            .expect("bounded rule lifecycle acceptance");
        }));
    }

    #[test]
    fn rule_management_rechecks_exact_permission_membership_and_session() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let group = create(&services, &manager, operation(), group_creation("Authority", true, 0)).await;
                let Item::Group(group) = group else { panic!("group") };
                let department = create(&services, &manager, operation(), department_creation("Default", true, 0, group.id(), None)).await;
                let input = rule_creation(department.target().into_department(), false);
                assert!(matches!(configure_request(services.directory(), institution.clone(), operation(), Subtype::Internship, input.clone()).await, Err(Error::Denied)));
                let role = Box::pin(services.role().create_workforce(manager.clone(), operation(), serde_json::from_value(json!({"entry":"domain","name":"Rule reader","level":200,"permissions":["workforce.numbers"]})).expect("role input"))).await.expect("unrelated permission role");
                let grant = Box::pin(grant::tests::admit(&services, &manager, Entry::Domain, "13900000000", Kind::Custom)).await;
                assert!(matches!(services.directory().list_rules(&institution).await, Err(Error::Denied)));
                assert!(matches!(configure_request(services.directory(), institution.clone(), operation(), Subtype::Internship, input.clone()).await, Err(Error::Denied)));
                let role = Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Rule reader","level":200,"permissions":["workforce.directory"]})).expect("directory permission"))).await.expect("exact permission enabled");
                let original = operation();
                configure(&services, &institution, original, Subtype::Internship, input.clone()).await;
                let current = services.current().read(&institution).await.expect("target identity");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13900000000"}});
                let suspended = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(target.clone()).expect("pause input"))).await.expect("real member suspension");
                assert!(matches!(services.directory().list_rules(&institution).await, Err(Error::Denied)));
                assert!(matches!(configure_request(services.directory(), institution.clone(), original, Subtype::Internship, input.clone()).await, Err(Error::Denied)), "replay requires current unsuspended membership");
                Box::pin(services.grant().resume(manager.clone(), operation(), suspended.id(), serde_json::from_value(target).expect("resume input"))).await.expect("real member resumption");
                configure(&services, &institution, original, Subtype::Internship, input.clone()).await;
                Box::pin(services.role().update_workforce(manager.clone(), operation(), role.id(), serde_json::from_value(json!({"entry":"domain","changeId":role.change_id(),"name":"Rule reader","level":200,"permissions":["workforce.numbers"]})).expect("remove permission"))).await.expect("directory permission removed");
                assert!(matches!(services.directory().list_rules(&institution).await, Err(Error::Denied)));
                assert!(matches!(configure_request(services.directory(), institution.clone(), original, Subtype::Internship, input.clone()).await, Err(Error::Denied)));
                Box::pin(services.revocation().revoke(institution.clone(), operation(), crate::identity::revocation::Scope::Current)).await.expect("revoke current session");
                assert!(matches!(services.directory().list_rules(&institution).await.expect_err("invalid session"), Error::Unauthenticated));
                assert!(matches!(configure_request(services.directory(), institution, original, Subtype::Internship, input).await.expect_err("invalid writer"), Error::Unauthenticated));
                stop(context, services, supplier).await;
            })).await.expect("bounded rule authority checks");
        }));
    }

    #[test]
    fn concurrent_rule_creations_and_directory_edits_follow_the_scope_lock() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (move_tree, rule_first) in [(false, false), (false, true), (true, false), (true, true)] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let group = create(&services, &manager, operation(), group_creation("Enabled", true, 0)).await;
                    let Item::Group(group) = group else { panic!("enabled group") };
                    let disabled = create(&services, &manager, operation(), group_creation("Disabled", false, 0)).await;
                    let Item::Group(disabled) = disabled else { panic!("disabled group") };
                    let root = create(&services, &manager, operation(), department_creation("Root", true, 0, group.id(), None)).await;
                    let child = create(&services, &manager, operation(), department_creation("Child", true, 0, group.id(), Some(root.target().into_department()))).await;
                    let child_id = child.target().into_department();
                    let input = rule_creation(child_id, true);
                    let gate = database.connections(&database.user).recover().await.expect("runtime coordination gate");
                    gate.client().batch_execute("BEGIN; SELECT workforce.lock_directory()").await.expect("hold directory scope");
                    let operations = [operation(), operation()];
                    let (_, first, second) = tokio::join!(
                        release_directory_waiters(gate.client(), 2),
                        configure_request(services.directory(), manager.clone(), operations[0], Subtype::Clinical, input.clone()),
                        configure_request(services.directory(), manager.clone(), operations[1], Subtype::Clinical, input.clone())
                    );
                    assert!(matches!((&first, &second), (Ok(_), Err(Error::Rejected(Rejection::Changed))) | (Err(Error::Rejected(Rejection::Changed)), Ok(_))));
                    assert_eq!(gate.client().query_one("SELECT count(*) FROM workforce.completions WHERE operation_id=ANY($1::uuid[])", &[&operations.as_slice()])
                        .await.expect("only the winning creation completes").get::<_, i64>(0), 1);

                    let mut replacement = root.clone();
                    let Item::Department(value) = &mut replacement else { panic!("root") };
                    if move_tree { value.group_id = disabled.id(); } else { value.enabled = false; }
                    let original = operation();
                    gate.client().batch_execute("BEGIN; SELECT workforce.lock_directory()").await.expect("serialize configuration with directory edit");
                    let (_, configured, edited) = tokio::join!(
                        release_directory_waiters(gate.client(), 2),
                        async {
                            if !rule_first { wait_directory_waiters(gate.client(), 1).await; }
                            configure_request(services.directory(), manager.clone(), original, Subtype::Internship, input.clone()).await
                        },
                        async {
                            if rule_first { wait_directory_waiters(gate.client(), 1).await; }
                            update_request(services.directory(), manager.clone(), operation(), root.target(), modification(&root, &replacement)).await
                        }
                    );
                    edited.expect("a later directory edit remains allowed");
                    if rule_first {
                        assert_eq!(configured.expect("configuration precedes disabling").rule().department_id(), child_id);
                        configure(&services, &manager, original, Subtype::Internship, input.clone()).await;
                    } else {
                        assert!(matches!(configured, Err(Error::Rejected(Rejection::InvalidDepartment))), "configuration sees the committed disabled lineage");
                    }
                    let row = gate.client().query_one("SELECT (SELECT count(*) FROM workforce.rules WHERE id=902), (SELECT count(*) FROM workforce.completions WHERE operation_id=$1)", &[&original])
                        .await.expect("configuration and completion agree");
                    assert_eq!(row.get::<_, i64>(0), i64::from(rule_first));
                    assert_eq!(row.get::<_, i64>(1), i64::from(rule_first));
                    gate.terminate().await.expect("gate ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let retained = services.directory().list_rules(&manager).await.expect("startup accepts references under a disabled lineage");
                    assert!(retained.rules.iter().all(|rule| rule.department_id() == child_id));
                    assert_eq!(retained.rules.len(), if rule_first { 2 } else { 1 });
                    stop(context, services, supplier).await;
                })).await.expect("bounded rule and directory race");
            }
        }));
    }

    #[test]
    fn rule_commit_faults_recover_original_and_startup_checks_references() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(
                        authentication::proxied(&database, &proxy),
                        &supplier,
                    ))
                    .await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let group = create(
                        &services,
                        &manager,
                        operation(),
                        group_creation("Fault rule group", true, 0),
                    )
                    .await;
                    let Item::Group(group_value) = &group else {
                        panic!("fault rule group")
                    };
                    let department = create(
                        &services,
                        &manager,
                        operation(),
                        department_creation(
                            "Fault rule department",
                            true,
                            0,
                            group_value.id(),
                            None,
                        ),
                    )
                    .await;
                    let department_id = department.target().into_department();
                    let original = operation();
                    let input = rule_creation(department_id, false);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    proxy.arm(fault);
                    let result = configure_request(
                            services.directory(),
                            manager.clone(),
                            original,
                            Subtype::Internship,
                            input.clone(),
                        )
                        .await;
                    if rolled_back {
                        assert!(matches!(result, Err(Error::Unavailable(_))));
                    } else if unknown {
                        assert!(matches!(result, Err(Error::Unknown(_))));
                    } else {
                        result.expect("lost response recovers the original rule");
                    }
                    proxy.release();
                    timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("actual rule commit fault");
                    services.close();
                    services.drain().await.expect("rule work drained");
                    context.stop().await.expect("context stopped");
                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("independent durable observer");
                    observer
                        .client()
                        .batch_execute("BEGIN")
                        .await
                        .expect("durable observation transaction");
                    observer
                        .client()
                        .query_one(
                            "SELECT id FROM workforce.departments WHERE id = $1 FOR UPDATE",
                            &[&department_id],
                        )
                        .await
                        .expect("original transaction released its department row before counting");
                    let expected = if rolled_back { 0_i64 } else { 1_i64 };
                    let row = observer
                        .client()
                        .query_one(
                            "SELECT (SELECT count(*) FROM workforce.rules WHERE id = 902), (SELECT count(*) FROM workforce.completions WHERE operation_id = $1)",
                            &[&original],
                        )
                        .await
                        .expect("durable rule and completion after original transaction");
                    assert_eq!(row.get::<_, i64>(0), expected);
                    assert_eq!(row.get::<_, i64>(1), expected);
                    observer
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .expect("durable observation ended");
                    proxy.stop().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    services
                        .ready()
                        .await
                        .expect("stored rules and completions pass startup verification");
                    let replayed = configure(
                        &services,
                        &manager,
                        original,
                        Subtype::Internship,
                        input,
                    )
                    .await;
                    assert!(replayed.created());
                    assert_eq!(
                        observer
                            .client()
                            .query_one("SELECT count(*) FROM workforce.rules WHERE id = 902", &[])
                            .await
                            .expect("only one rule after replay")
                            .get::<_, i64>(0),
                        1
                    );
                    observer.terminate().await.expect("observer ended");
                    stop(context, services, supplier).await;
                }))
                .await
                .expect("bounded rule commit fault scenario");
            }

            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let group = create(
                    &services,
                    &manager,
                    operation(),
                    group_creation("Verified rule group", true, 0),
                )
                .await;
                let Item::Group(group_value) = &group else {
                    panic!("verified rule group")
                };
                let department = create(
                    &services,
                    &manager,
                    operation(),
                    department_creation(
                        "Verified rule department",
                        true,
                        0,
                        group_value.id(),
                        None,
                    ),
                )
                .await;
                configure(
                    &services,
                    &manager,
                    operation(),
                    Subtype::Internship,
                    rule_creation(department.target().into_department(), false),
                )
                .await;
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");
                let owner = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("isolated damage owner");
                owner
                    .client()
                    .batch_execute(
                        "BEGIN; ALTER TABLE workforce.rules DROP CONSTRAINT rules_department_id_fkey; SET LOCAL session_replication_role = replica; UPDATE workforce.rules SET department_id = gen_random_uuid(); COMMIT",
                    )
                    .await
                    .expect("inject a broken stored rule reference");
                owner.terminate().await.expect("owner ended");
                let context = Context::new(database.connections(&database.user))
                    .await
                    .expect("verification context");
                let barrier = Barrier::begin(context.clone())
                    .await
                    .expect("verification barrier");
                let result = Services::start(
                    barrier,
                    authentication::inventory(authentication::keys()),
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
                .await;
                assert!(
                    matches!(result, Err(crate::startup::Error::Directory(_))),
                    "startup rejects a rule without its real referenced department"
                );
                context.stop().await.expect("verification context stopped");
                supplier.stop().await;
            }))
            .await
            .expect("bounded damaged rule startup verification");
        }));
    }

    #[test]
    fn position_crud_and_independent_name_scope() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(45),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;

                    for (collection, input) in [
                        (
                            Collection::Positions,
                            group_creation("Wrong owner", true, 0),
                        ),
                        (
                            Collection::Groups,
                            position_creation("Wrong owner", true, 0),
                        ),
                    ] {
                        assert!(matches!(
                            services
                                .directory()
                                .create(manager.clone(), operation(), collection, input)
                                .await,
                            Err(Error::Rejected(Rejection::Input))
                        ));
                    }

                    // 1. Create a group and department
                    let group = create(
                        &services,
                        &manager,
                        operation(),
                        group_creation("CommonName", true, 0),
                    )
                    .await;
                    let Item::Group(group_val) = &group else {
                        panic!("group")
                    };
                    let _department = create(
                        &services,
                        &manager,
                        operation(),
                        department_creation("DeptName", true, 0, group_val.id(), None),
                    )
                    .await;

                    // 2. Independent name scope: a position can share names with a group or department!
                    let pos_group_name = create(
                        &services,
                        &manager,
                        operation(),
                        position_creation("CommonName", true, 1),
                    )
                    .await;
                    let pos_dept_name = create(
                        &services,
                        &manager,
                        operation(),
                        position_creation("DeptName", true, 2),
                    )
                    .await;

                    // 3. But two positions cannot share a name (even disabled)
                    let doctor_op = operation();
                    let doctor = create(
                        &services,
                        &manager,
                        doctor_op,
                        position_creation("Doctor", false, 3),
                    )
                    .await;
                    let Item::Position(doctor_val) = &doctor else {
                        panic!("position")
                    };
                    assert_eq!(doctor_val.name().as_str(), "Doctor");
                    assert!(!doctor_val.enabled());

                    // Disabled positions still occupy name in the position scope
                    assert!(matches!(
                        Box::pin(create_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            Collection::Positions,
                            position_creation("Doctor", true, 0),
                        ))
                        .await,
                        Err(Error::Rejected(Rejection::NameInUse))
                    ));

                    // 4. Name normalization and rules
                    let nfc_pos = create(
                        &services,
                        &manager,
                        operation(),
                        position_creation("  Physician\u{301}  ", true, 0),
                    )
                    .await;
                    let Item::Position(nfc_val) = &nfc_pos else {
                        panic!("position")
                    };
                    assert_eq!(nfc_val.name().as_str(), "Physiciań");

                    // Invalid names are rejected with InputError / Error::Rejected(Rejection::Input)
                    for invalid in ["", " ", "a\0b", "\tLead", "Lead\n", &"字".repeat(65)] {
                        assert!(
                            serde_json::from_value::<position::Creation>(
                                json!({"name":invalid,"enabled":true,"index":0})
                            )
                            .is_err()
                        );
                    }

                    // 5. Read / List positions (ordered by index, then id)
                    let positions =
                        Box::pin(services.directory().list(&manager, Collection::Positions))
                            .await
                            .expect("positions list");
                    assert_eq!(positions.len(), 4);
                    assert_eq!(positions[0], nfc_pos, "lowest index 0 comes first");
                    assert_eq!(positions[1], pos_group_name, "index 1");
                    assert_eq!(positions[2], pos_dept_name, "index 2");
                    assert_eq!(positions[3], doctor, "index 3");

                    // 6. Update position
                    let mut replacement = doctor.clone();
                    let Item::Position(val) = &mut replacement else {
                        panic!("position")
                    };
                    val.name = "Chief Doctor".parse().expect("name");
                    val.enabled = true;
                    val.index = 0.try_into().expect("index");
                    let update_op = operation();
                    let updated = Box::pin(update_request(
                        services.directory(),
                        manager.clone(),
                        update_op,
                        doctor.target(),
                        modification(&doctor, &replacement),
                    ))
                    .await
                    .expect("position update");
                    assert_eq!(updated, Receipt::Present(replacement.clone()));

                    // Idempotent update returns current
                    let idempotent = Box::pin(update_request(
                        services.directory(),
                        manager.clone(),
                        operation(),
                        doctor.target(),
                        modification(&replacement, &replacement),
                    ))
                    .await
                    .expect("idempotent update");
                    assert_eq!(idempotent, Receipt::Present(replacement.clone()));

                    // Stale update returns Changed
                    let mut stale_replacement = replacement.clone();
                    let Item::Position(val) = &mut stale_replacement else {
                        panic!("position")
                    };
                    val.name = "Stale Name".parse().expect("name");
                    assert!(matches!(
                        Box::pin(update_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            doctor.target(),
                            modification(&doctor, &stale_replacement),
                        ))
                        .await,
                        Err(Error::Rejected(Rejection::Changed))
                    ));

                    // Rename collision with another position returns NameInUse
                    let mut collides = pos_group_name.clone();
                    let Item::Position(val) = &mut collides else {
                        panic!("position")
                    };
                    val.name = "Chief Doctor".parse().expect("name");
                    assert!(matches!(
                        Box::pin(update_request(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            pos_group_name.target(),
                            modification(&pos_group_name, &collides),
                        ))
                        .await,
                        Err(Error::Rejected(Rejection::NameInUse))
                    ));

                    // 7. Delete position
                    let delete_op = operation();
                    assert_eq!(
                        Box::pin(delete(
                            services.directory(),
                            manager.clone(),
                            delete_op,
                            doctor.target(),
                            deletion(&replacement),
                        ))
                        .await
                        .expect("position deletion"),
                        Receipt::Deleted
                    );

                    // Deleting already absent returns Absent
                    assert!(matches!(
                        Box::pin(delete(
                            services.directory(),
                            manager.clone(),
                            operation(),
                            doctor.target(),
                            deletion(&replacement),
                        ))
                        .await,
                        Err(Error::Rejected(Rejection::Absent))
                    ));

                    // 8. Replays
                    // Creation replay of deleted position returns Deleted
                    assert_eq!(
                        Box::pin(create_request(
                            services.directory(),
                            manager.clone(),
                            doctor_op,
                            Collection::Positions,
                            position_creation("Doctor", false, 3),
                        ))
                        .await
                        .expect("creation replay of deleted item"),
                        Receipt::Deleted
                    );

                    // Request mismatch (different input)
                    assert!(matches!(
                        Box::pin(create_request(
                            services.directory(),
                            manager.clone(),
                            doctor_op,
                            Collection::Positions,
                            position_creation("Different", false, 3),
                        ))
                        .await,
                        Err(Error::Mismatch)
                    ));

                    // Deletion replay returns Deleted
                    assert_eq!(
                        Box::pin(delete(
                            services.directory(),
                            manager.clone(),
                            delete_op,
                            doctor.target(),
                            deletion(&replacement),
                        ))
                        .await
                        .expect("deletion replay"),
                        Receipt::Deleted
                    );

                    // 9. Database completion record format
                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("observer");
                    let row = observer
                    .client()
                    .query_one(
                        "SELECT target, action FROM workforce.completions WHERE operation_id = $1",
                        &[&doctor_op],
                    )
                    .await
                    .expect("creation completion row");
                    assert_eq!(row.get::<_, String>("target"), "positions");
                    assert_eq!(row.get::<_, String>("action"), "create");
                    assert_eq!(
                        Seed::input(
                            observer.client(),
                            services.directory().app.keys(),
                            super::COMPLETIONS,
                            doctor_op
                        )
                        .await
                        .expect("authenticated position creation input")
                        .as_slice(),
                        r#"{"name":"Doctor","enabled":false,"index":3}"#.as_bytes()
                    );
                    observer.terminate().await.expect("observer ended");

                    stop(context, services, supplier).await;
                }),
            )
            .await
            .expect("bounded position CRUD and independent name scope");
        }));
    }

    #[test]
    fn position_concurrent_creates_and_renames() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;

                // Concurrent creates in position scope
                let operations = [operation(), operation()];
                let gate = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("gate");
                gate.client()
                    .batch_execute("BEGIN; SELECT workforce.lock_directory()")
                    .await
                    .expect("gate lock");
                let (_, first, second) = tokio::join!(
                    release_directory_waiters(gate.client(), 2),
                    Box::pin(create_request(
                        services.directory(),
                        manager.clone(),
                        operations[0],
                        Collection::Positions,
                        position_creation("Surgeon", true, 0),
                    )),
                    Box::pin(create_request(
                        services.directory(),
                        manager.clone(),
                        operations[1],
                        Collection::Positions,
                        position_creation("Surgeon", true, 0),
                    )),
                );
                gate.terminate().await.expect("gate ended");
                assert!(
                    matches!(
                        (&first, &second),
                        (Ok(_), Err(Error::Rejected(Rejection::NameInUse))) | (Err(Error::Rejected(Rejection::NameInUse)), Ok(_))
                    ),
                    "exactly one position creation wins: {first:?} {second:?}"
                );
                let observer = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("observer");
                assert_eq!(
                    observer
                        .client()
                        .query_one(
                            "SELECT count(*) FROM workforce.positions WHERE name = 'Surgeon'",
                            &[],
                        )
                        .await
                        .expect("one surgeon")
                        .get::<_, i64>(0),
                    1
                );
                assert_eq!(
                    observer
                        .client()
                        .query_one(
                            "SELECT count(*) FROM workforce.completions WHERE operation_id = ANY($1::uuid[])",
                            &[&operations.as_slice()],
                        )
                        .await
                        .expect("one completion")
                        .get::<_, i64>(0),
                    1
                );

                // Concurrent renames in position scope
                let pos_a = create(
                    &services,
                    &manager,
                    operation(),
                    position_creation("PositionA", true, 0),
                )
                .await;
                let pos_b = create(
                    &services,
                    &manager,
                    operation(),
                    position_creation("PositionB", true, 0),
                )
                .await;
                let mut repl_a = pos_a.clone();
                let Item::Position(val) = &mut repl_a else { panic!("position") };
                val.name = "Head Surgeon".parse().expect("name");
                let mut repl_b = pos_b.clone();
                let Item::Position(val) = &mut repl_b else { panic!("position") };
                val.name = "Head Surgeon".parse().expect("name");

                let operations = [operation(), operation()];
                let gate = database
                    .connections(&database.user)
                    .recover()
                    .await
                    .expect("gate");
                gate.client()
                    .batch_execute("BEGIN; SELECT workforce.lock_directory()")
                    .await
                    .expect("gate lock");
                let (_, first, second) = tokio::join!(
                    release_directory_waiters(gate.client(), 2),
                    Box::pin(update_request(
                        services.directory(),
                        manager.clone(),
                        operations[0],
                        pos_a.target(),
                        modification(&pos_a, &repl_a),
                    )),
                    Box::pin(update_request(
                        services.directory(),
                        manager.clone(),
                        operations[1],
                        pos_b.target(),
                        modification(&pos_b, &repl_b),
                    )),
                );
                gate.terminate().await.expect("gate ended");
                assert!(
                    matches!(
                        (&first, &second),
                        (Ok(_), Err(Error::Rejected(Rejection::NameInUse))) | (Err(Error::Rejected(Rejection::NameInUse)), Ok(_))
                    ),
                    "exactly one position rename wins"
                );
                assert_eq!(
                    observer
                        .client()
                        .query_one(
                            "SELECT count(*) FROM workforce.positions WHERE name = 'Head Surgeon'",
                            &[],
                        )
                        .await
                        .expect("one renamed position")
                        .get::<_, i64>(0),
                    1
                );
                observer.terminate().await.expect("observer ended");

                stop(context, services, supplier).await;
            }))
            .await
            .expect("bounded position races");
        }));
    }

    #[test]
    fn position_commit_faults_and_startup_verification() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(
                        authentication::proxied(&database, &proxy),
                        &supplier,
                    ))
                    .await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;

                    let original = operation();
                    let input = position_creation("Fault Position", true, 0);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    proxy.arm(fault);
                    let result = create_request(
                        services.directory(),
                        manager.clone(),
                        original,
                        Collection::Positions,
                        input.clone(),
                    )
                    .await;
                    if rolled_back {
                        assert!(matches!(result, Err(Error::Unavailable(_))));
                    } else if unknown {
                        assert!(matches!(result, Err(Error::Unknown(_))));
                    } else {
                        result.expect("lost response recovers the original position");
                    }
                    proxy.release();
                    timeout(Duration::from_secs(2), proxy.intercepted())
                        .await
                        .expect("actual position commit fault");
                    services.close();
                    services.drain().await.expect("position work drained");
                    context.stop().await.expect("context stopped");

                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("observer");
                    observer
                        .client()
                        .batch_execute("BEGIN")
                        .await
                        .expect("durable observation transaction");
                    // Lock row held by original transaction (manager account) before counting
                    observer
                        .client()
                        .query_one(
                            "SELECT id FROM identity.accounts WHERE id = $1 FOR UPDATE",
                            &[&manager.account_id()],
                        )
                        .await
                        .expect("original transaction released locks before counting");

                    let expected = if rolled_back { 0_i64 } else { 1_i64 };
                    let row = observer
                        .client()
                        .query_one(
                            "SELECT (SELECT count(*) FROM workforce.positions WHERE operation_id = $1), (SELECT count(*) FROM workforce.completions WHERE operation_id = $1)",
                            &[&original],
                        )
                        .await
                        .expect("durable position and completion");
                    assert_eq!(row.get::<_, i64>(0), expected);
                    assert_eq!(row.get::<_, i64>(1), expected);
                    observer
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .expect("durable observer committed");
                    observer.terminate().await.expect("observer ended");

                    if !rolled_back {
                        let (context, services) =
                            Box::pin(start(database.connections(&database.user), &supplier)).await;
                        let replayed = Box::pin(create_request(
                            services.directory(),
                            manager.clone(),
                            original,
                            Collection::Positions,
                            input,
                        ))
                        .await
                        .expect("replay after recovery");
                        assert!(matches!(replayed, Receipt::Present(_)));
                        stop(context, services, supplier).await;
                    } else {
                        supplier.stop().await;
                    }
                    proxy.stop().await;
                }))
                .await
                .expect("bounded position commit fault");
            }

            // Startup verification test
            timeout(Duration::from_secs(45), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                create(
                    &services,
                    &manager,
                    operation(),
                    position_creation("Valid Position", true, 0),
                )
                .await;
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");

                // Restart succeeds
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                services.ready().await.expect("positions survive restart");
                services.close();
                services.drain().await.expect("drained");
                context.stop().await.expect("stopped");

                // Inject damaged noncanonical position name
                let owner = database
                    .connections("postgres")
                    .recover()
                    .await
                    .expect("owner");
                owner
                    .client()
                    .batch_execute(
                        "BEGIN; ALTER TABLE workforce.positions DROP CONSTRAINT positions_name_check; SET LOCAL session_replication_role=replica; UPDATE workforce.positions SET name=' damaged position '; COMMIT",
                    )
                    .await
                    .expect("inject noncanonical position name");
                owner.terminate().await.expect("owner ended");

                let context = Context::new(database.connections(&database.user))
                    .await
                    .expect("context");
                let barrier = Barrier::begin(context.clone())
                    .await
                    .expect("barrier");
                let result = Services::start(
                    barrier,
                    authentication::inventory(authentication::keys()),
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
                .await;
                assert!(
                    matches!(result, Err(crate::startup::Error::Directory(_))),
                    "startup rejects noncanonical position name"
                );
                context.stop().await.expect("context stopped");
                supplier.stop().await;
            }))
            .await
            .expect("bounded position startup verification");
        }));
    }

    fn choice(position_id: position::Id, enabled: bool, index: i32) -> roster::Choice {
        roster::Choice {
            position_id,
            enabled,
            index: index.try_into().expect("roster index"),
        }
    }

    fn replacement(
        expected: &[roster::Entry],
        replacement: Vec<roster::Choice>,
    ) -> roster::Replace {
        roster::Replace {
            expected: expected.to_vec(),
            replacement,
        }
    }

    fn entries(receipt: roster::Receipt) -> Vec<roster::Entry> {
        let roster::Receipt::Present(entries) = receipt else {
            panic!("department position list")
        };
        entries
    }

    fn replace_request(
        service: &super::Service,
        actor: Actor,
        operation: operation::Id,
        department_id: department::Id,
        input: roster::Replace,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<roster::Receipt, Error>> + '_>>
    {
        Box::pin(service.replace_roster(actor, operation, department_id, input))
    }

    async fn roster_directory(
        services: &Services,
        manager: &Actor,
    ) -> (group::Id, department::Id, department::Id, Vec<position::Id>) {
        let Item::Group(group) = create(
            services,
            manager,
            operation(),
            group_creation("Roster group", true, 0),
        )
        .await
        else {
            panic!("group")
        };
        let mut departments = Vec::new();
        for name in ["Roster surgery", "Roster clinic"] {
            let Item::Department(department) = create(
                services,
                manager,
                operation(),
                department_creation(name, true, 0, group.id(), None),
            )
            .await
            else {
                panic!("department")
            };
            departments.push(department.id());
        }
        let mut positions = Vec::new();
        for (name, enabled) in [
            ("Roster lead", true),
            ("Roster nurse", true),
            ("Roster retired", false),
        ] {
            let Item::Position(position) = create(
                services,
                manager,
                operation(),
                position_creation(name, enabled, 0),
            )
            .await
            else {
                panic!("position")
            };
            positions.push(position.id());
        }
        (group.id(), departments[0], departments[1], positions)
    }

    #[test]
    fn rosters_replace_complete_lists_and_protect_their_references() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let directory = services.directory();
                let (group_id, surgery, clinic, positions) =
                    roster_directory(&services, &manager).await;
                let (lead, nurse, retired) = (positions[0], positions[1], positions[2]);
                assert!(directory.list_roster(&manager, surgery).await.expect("empty list").is_empty());
                let group_as_department: department::Id = serde_json::from_value(json!(group_id)).expect("same UUID");
                for absent in [department::Id::allocate(), group_as_department] {
                    assert!(matches!(directory.list_roster(&manager, absent).await, Err(Error::Rejected(Rejection::Absent))));
                    assert!(matches!(
                        replace_request(directory, manager.clone(), operation(), absent, replacement(&[], vec![])).await,
                        Err(Error::Rejected(Rejection::Absent))
                    ));
                }
                assert!(matches!(
                    directory.list_roster(&institution, surgery).await,
                    Err(Error::Denied)
                ));

                // A first complete list; list order does not change the canonical input.
                let original = operation();
                let first = replacement(&[], vec![choice(lead, true, 1), choice(nurse, true, 0)]);
                let configured = entries(
                    replace_request(directory, manager.clone(), original, surgery, first.clone())
                        .await
                        .expect("first list"),
                );
                assert_eq!(
                    configured.iter().map(|entry| (entry.position_id(), entry.index().into())).collect::<Vec<(position::Id, i32)>>(),
                    vec![(nurse, 0), (lead, 1)]
                );
                let reordered = replacement(&[], vec![choice(nurse, true, 0), choice(lead, true, 1)]);
                assert_eq!(
                    entries(replace_request(directory, manager.clone(), original, surgery, reordered).await.expect("replay")),
                    configured
                );
                assert!(matches!(
                    replace_request(directory, manager.clone(), original, surgery, replacement(&[], vec![choice(lead, true, 1)])).await,
                    Err(Error::Mismatch)
                ));
                assert!(matches!(
                    replace_request(directory, manager.clone(), original, clinic, first.clone()).await,
                    Err(Error::Mismatch)
                ));
                // Stale expected values conflict, while an already reached list only completes.
                assert!(matches!(
                    replace_request(directory, manager.clone(), operation(), surgery, replacement(&[], vec![choice(lead, false, 1)])).await,
                    Err(Error::Rejected(Rejection::Changed))
                ));
                let reached = operation();
                assert_eq!(
                    entries(replace_request(directory, manager.clone(), reached, surgery, first).await.expect("reached list")),
                    configured
                );
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let completions: i64 = observer.client()
                    .query_one("SELECT count(*) FROM workforce.completions WHERE operation_id = ANY($1) AND action = 'replace' AND target = $2", &[&vec![original, reached], &surgery.to_string()])
                    .await.expect("replace completions").get(0);
                assert_eq!(completions, 2);

                // Duplicate positions or entries are input errors before any lookup.
                for duplicate in [
                    replacement(&configured, vec![choice(lead, true, 0), choice(lead, false, 1)]),
                    replacement(&[configured[0].clone(), configured[0].clone()], vec![]),
                    roster::Replace {
                        expected: vec![configured[0].clone(), roster::Entry { position_id: retired, ..configured[0].clone() }],
                        replacement: vec![],
                    },
                ] {
                    assert!(matches!(
                        replace_request(directory, manager.clone(), operation(), surgery, duplicate).await,
                        Err(Error::Rejected(Rejection::Input))
                    ));
                }
                assert!(matches!(
                    replace_request(directory, manager.clone(), operation(), surgery, replacement(&configured, vec![choice(position::Id::allocate(), true, 0)])).await,
                    Err(Error::Rejected(Rejection::Absent))
                ));

                // Retained positions keep their entry; omitted ones are removed; disabled
                // positions may be configured. Lists are per department and not inherited.
                let changed = entries(
                    replace_request(directory, manager.clone(), operation(), surgery,
                        replacement(&configured, vec![choice(lead, false, 5), choice(retired, true, 0)]))
                    .await
                    .expect("changed list"),
                );
                let kept = configured.iter().find(|entry| entry.position_id() == lead).expect("lead entry");
                assert_eq!(changed.len(), 2);
                assert_eq!(changed[1].id(), kept.id());
                assert!(!changed[1].enabled());
                assert_eq!(changed[0].position_id(), retired);
                assert!(directory.list_roster(&manager, clinic).await.expect("clinic list").is_empty());
                let shared = entries(
                    replace_request(directory, manager.clone(), operation(), clinic, replacement(&[], vec![choice(lead, true, 0)]))
                        .await
                        .expect("same position in a second department"),
                );
                assert_ne!(shared[0].id(), kept.id());
                assert_eq!(directory.list_roster(&manager, surgery).await.expect("surgery list"), changed);

                // Referenced departments and positions cannot be deleted.
                let lead_item = super::database::find_position(observer.client(), lead).await.expect("lead").expect("present");
                let surgery_item = super::database::find_department(observer.client(), surgery).await.expect("surgery").expect("present");
                for (target, expected) in [(Target::Position(lead), &lead_item), (Target::Department(surgery), &surgery_item)] {
                    assert!(matches!(
                        delete(directory, manager.clone(), operation(), target, deletion(expected)).await,
                        Err(Error::Rejected(Rejection::InUse))
                    ));
                }

                // The runtime identity cannot rebind references or write without a completion.
                let runtime = database.connections(&database.user).recover().await.expect("runtime");
                for statement in [
                    "UPDATE workforce.rosters SET position_id = position_id",
                    "UPDATE workforce.rosters SET department_id = department_id",
                    "UPDATE workforce.rosters SET id = id",
                ] {
                    let error = runtime.client().execute(statement, &[]).await.expect_err("immutable references");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE), "{statement}");
                }
                for statement in [
                    "BEGIN; UPDATE workforce.rosters SET index = index + 1; COMMIT",
                    "BEGIN; DELETE FROM workforce.rosters; COMMIT",
                    "BEGIN; INSERT INTO workforce.rosters (id, department_id, position_id, enabled, index) SELECT gen_random_uuid(), id, (SELECT id FROM workforce.positions WHERE name = 'Roster nurse'), true, 0 FROM workforce.departments WHERE name = 'Roster clinic'; COMMIT",
                ] {
                    let error = runtime.client().batch_execute(statement).await.expect_err("roster write requires a completion");
                    assert_eq!(error.as_db_error().and_then(|error| error.constraint()), Some("rosters_completion_check"), "{statement}");
                    runtime.client().batch_execute("ROLLBACK").await.expect("reset runtime");
                }
                runtime.client().batch_execute("BEGIN").await.expect("unpaired completion transaction");
                Seed::insert(runtime.client(), directory.app.keys(), super::COMPLETIONS, operation(), original, ("replace", department::Id::allocate().to_string()), &json!({})).await.expect("sealed replacement completion before pairing check");
                let error = runtime.client().batch_execute("COMMIT").await.expect_err("replacement completion requires its department");
                assert_eq!(error.as_db_error().and_then(|error| error.constraint()), Some("completions_pairing_check"));
                runtime.client().batch_execute("ROLLBACK").await.expect("reset runtime");
                runtime.terminate().await.expect("runtime ended");

                // Clearing every list releases both references; a later replay reports deletion.
                let current = directory.list_roster(&manager, clinic).await.expect("clinic list");
                replace_request(directory, manager.clone(), operation(), clinic, replacement(&current, vec![])).await.expect("clear clinic");
                let clearing = operation();
                let cleared = replacement(&changed, vec![]);
                assert!(entries(replace_request(directory, manager.clone(), clearing, surgery, cleared.clone()).await.expect("clear surgery")).is_empty());
                delete(directory, manager.clone(), operation(), Target::Position(lead), deletion(&lead_item)).await.expect("unreferenced position");
                delete(directory, manager.clone(), operation(), Target::Department(surgery), deletion(&surgery_item)).await.expect("unreferenced department");
                assert!(matches!(
                    replace_request(directory, manager.clone(), clearing, surgery, cleared.clone()).await,
                    Ok(roster::Receipt::Deleted)
                ));
                assert!(matches!(
                    replace_request(directory, manager.clone(), operation(), surgery, cleared).await,
                    Err(Error::Rejected(Rejection::Absent))
                ));
                observer.terminate().await.expect("observer ended");
                stop(context, services, supplier).await;
            }))
            .await
            .expect("bounded roster replacement");
        }));
    }

    #[test]
    fn roster_commit_faults_recover_the_original_and_startup_checks_references() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(
                        authentication::proxied(&database, &proxy),
                        &supplier,
                    ))
                    .await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let (_, surgery, _, positions) = roster_directory(&services, &manager).await;
                    let original = operation();
                    let input = replacement(&[], vec![choice(positions[0], true, 0)]);
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    proxy.arm(fault);
                    let result = replace_request(services.directory(), manager.clone(), original, surgery, input.clone()).await;
                    if rolled_back {
                        assert!(matches!(result, Err(Error::Unavailable(_))));
                    } else if unknown {
                        assert!(matches!(result, Err(Error::Unknown(_))));
                    } else {
                        assert_eq!(entries(result.expect("lost response recovers the original list")).len(), 1);
                    }
                    proxy.release();
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual roster commit fault");
                    services.close();
                    services.drain().await.expect("roster work drained");
                    context.stop().await.expect("context stopped");

                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    observer.client().batch_execute("BEGIN").await.expect("durable observation");
                    // The original transaction holds the department row until it ends.
                    observer.client()
                        .query_one("SELECT id FROM workforce.departments WHERE id = $1 FOR UPDATE", &[&surgery])
                        .await
                        .expect("original transaction released the department");
                    let expected = if rolled_back { 0_i64 } else { 1_i64 };
                    let row = observer.client()
                        .query_one("SELECT (SELECT count(*) FROM workforce.rosters WHERE department_id = $1), (SELECT count(*) FROM workforce.completions WHERE operation_id = $2)", &[&surgery, &original])
                        .await
                        .expect("durable list and completion");
                    assert_eq!(row.get::<_, i64>(0), expected);
                    assert_eq!(row.get::<_, i64>(1), expected);
                    observer.client().batch_execute("COMMIT").await.expect("observer committed");
                    observer.terminate().await.expect("observer ended");

                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let replayed = entries(
                        replace_request(services.directory(), manager.clone(), original, surgery, input)
                            .await
                            .expect("replay or new application after recovery"),
                    );
                    assert_eq!(replayed.len(), 1);
                    stop(context, services, supplier).await;
                    proxy.stop().await;
                }))
                .await
                .expect("bounded roster commit fault");
            }

            for damage in [
                "DELETE FROM workforce.positions WHERE name = 'Roster lead'",
                "DELETE FROM workforce.departments WHERE name = 'Roster surgery'",
            ] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let (_, surgery, _, positions) = roster_directory(&services, &manager).await;
                    replace_request(services.directory(), manager.clone(), operation(), surgery, replacement(&[], vec![choice(positions[0], true, 0)]))
                        .await
                        .expect("configured list");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    let (context, services) =
                        Box::pin(start(database.connections(&database.user), &supplier)).await;
                    services.ready().await.expect("lists survive restart");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");

                    let owner = database.connections("postgres").recover().await.expect("owner");
                    owner.client()
                        .batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {damage}; COMMIT"))
                        .await
                        .expect("inject a lost roster reference");
                    owner.terminate().await.expect("owner ended");
                    let context = Context::new(database.connections(&database.user)).await.expect("context");
                    let barrier = Barrier::begin(context.clone()).await.expect("barrier");
                    let result = Services::start(
                        barrier,
                        authentication::inventory(authentication::keys()),
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
                    .await;
                    assert!(matches!(result, Err(crate::startup::Error::Directory(_))), "{damage}");
                    context.stop().await.expect("context stopped");
                    supplier.stop().await;
                }))
                .await
                .expect("bounded roster startup verification");
            }
        }));
    }

    fn category_creation(name: &str, enabled: bool) -> Creation {
        Creation::Category(category::Creation {
            name: name.parse().expect("category name"),
            enabled,
            index: 0.try_into().expect("index"),
        })
    }

    fn discipline_creation(name: &str, category_id: category::Id) -> Creation {
        Creation::Discipline(discipline::Creation {
            name: name.parse().expect("discipline name"),
            enabled: true,
            index: 0.try_into().expect("index"),
            category_id,
        })
    }

    fn title_creation(name: &str, discipline_id: discipline::Id) -> Creation {
        Creation::Title(title::Creation {
            name: name.parse().expect("title name"),
            enabled: true,
            index: 0.try_into().expect("index"),
            discipline_id,
        })
    }

    #[test]
    fn title_tree_shares_one_name_scope_and_keeps_real_parents() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let directory = services.directory();

                let category_operation = operation();
                let Item::Category(medicine) = create(&services, &manager, category_operation, category_creation("Medicine", true)).await else { panic!("category") };
                let Item::Category(nursing) = create(&services, &manager, operation(), category_creation("Nursing", false)).await else { panic!("category") };
                let Item::Discipline(surgery) = create(&services, &manager, operation(), discipline_creation("Surgery", medicine.id())).await else { panic!("discipline") };
                let title_operation = operation();
                let Item::Title(surgeon) = create(&services, &manager, title_operation, title_creation("Chief surgeon", surgery.id())).await else { panic!("title") };
                assert_eq!(surgeon.discipline_id(), surgery.id());

                // One name scope across all three levels, disabled items included; other scopes are independent.
                for input in [
                    discipline_creation("Medicine", medicine.id()),
                    discipline_creation("Nursing", medicine.id()),
                    title_creation("Surgery", surgery.id()),
                    category_creation("Chief surgeon", true),
                ] {
                    let collection = input.collection();
                    assert!(matches!(
                        create_request(directory, manager.clone(), operation(), collection, input).await,
                        Err(Error::Rejected(Rejection::NameInUse))
                    ));
                }
                create(&services, &manager, operation(), group_creation("Chief surgeon", true, 0)).await;
                create(&services, &manager, operation(), position_creation("Chief surgeon", true, 0)).await;

                // Parents must be real items of the parent collection.
                let medicine_as_discipline: discipline::Id = serde_json::from_value(json!(medicine.id())).expect("same UUID");
                for input in [
                    discipline_creation("Orphan", category::Id::allocate()),
                    title_creation("Orphan", medicine_as_discipline),
                ] {
                    let collection = input.collection();
                    assert!(matches!(
                        create_request(directory, manager.clone(), operation(), collection, input).await,
                        Err(Error::Rejected(Rejection::Absent))
                    ));
                }

                // Moves and renames use complete conditional updates.
                let moved = discipline::Item { category_id: nursing.id(), ..surgery.clone() };
                let receipt = update_request(directory, manager.clone(), operation(), Target::Discipline(surgery.id()), modification(&Item::Discipline(surgery.clone()), &Item::Discipline(moved.clone()))).await.expect("move discipline");
                assert_eq!(present(receipt), Item::Discipline(moved.clone()));
                let renamed = title::Item { name: "Medicine".parse().expect("name"), ..surgeon.clone() };
                assert!(matches!(
                    update_request(directory, manager.clone(), operation(), Target::Title(surgeon.id()), modification(&Item::Title(surgeon.clone()), &Item::Title(renamed))).await,
                    Err(Error::Rejected(Rejection::NameInUse))
                ));
                let orphaned = title::Item { discipline_id: discipline::Id::allocate(), ..surgeon.clone() };
                assert!(matches!(
                    update_request(directory, manager.clone(), operation(), Target::Title(surgeon.id()), modification(&Item::Title(surgeon.clone()), &Item::Title(orphaned))).await,
                    Err(Error::Rejected(Rejection::Absent))
                ));
                for (target, expected) in [
                    (Target::Category(nursing.id()), Item::Category(nursing.clone())),
                    (Target::Discipline(surgery.id()), Item::Discipline(moved.clone())),
                ] {
                    assert!(matches!(
                        delete(directory, manager.clone(), operation(), target, deletion(&expected)).await,
                        Err(Error::Rejected(Rejection::InUse))
                    ));
                }
                let mut listed = Vec::new();
                for collection in [Collection::Categories, Collection::Disciplines, Collection::Titles] {
                    listed.push(directory.list(&manager, collection).await.expect("title tree list").len());
                }
                assert_eq!(listed, vec![2, 1, 1]);

                // Runtime constraints protect the scope, the original creation and completions.
                let runtime = database.connections(&database.user).recover().await.expect("runtime");
                let error = runtime.client().execute("INSERT INTO workforce.categories (id, name, enabled, index, operation_id) VALUES (gen_random_uuid(), 'Chief surgeon', true, 0, gen_random_uuid())", &[]).await.expect_err("third scope");
                assert!(matches!(Error::from(error), Error::Rejected(Rejection::NameInUse)));
                let error = runtime.client().execute("INSERT INTO workforce.categories (id, name, enabled, index, operation_id) VALUES (gen_random_uuid(), 'Fresh', true, 0, $1)", &[&title_operation]).await.expect_err("cross-collection original creation");
                assert_eq!(error.as_db_error().and_then(|error| error.constraint()), Some("directory_operation_id_key"));
                for statement in ["UPDATE workforce.titles SET operation_id = gen_random_uuid()", "UPDATE workforce.titles SET id = id"] {
                    let error = runtime.client().execute(statement, &[]).await.expect_err("immutable identity");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE), "{statement}");
                }
                let error = runtime.client().batch_execute("BEGIN; UPDATE workforce.titles SET index = index + 1; COMMIT").await.expect_err("title write requires a completion");
                assert_eq!(error.as_db_error().and_then(|error| error.constraint()), Some("directory_completion_check"));
                runtime.client().batch_execute("ROLLBACK").await.expect("reset runtime");
                runtime.terminate().await.expect("runtime ended");

                // Removing the leaves first releases each parent; original creations replay as deleted.
                delete(directory, manager.clone(), operation(), Target::Title(surgeon.id()), deletion(&Item::Title(surgeon.clone()))).await.expect("delete title");
                delete(directory, manager.clone(), operation(), Target::Discipline(surgery.id()), deletion(&Item::Discipline(moved))).await.expect("delete discipline");
                delete(directory, manager.clone(), operation(), Target::Category(medicine.id()), deletion(&Item::Category(medicine.clone()))).await.expect("delete category");
                for (original, input) in [
                    (title_operation, title_creation("Chief surgeon", surgery.id())),
                    (category_operation, category_creation("Medicine", true)),
                ] {
                    let collection = input.collection();
                    assert!(matches!(
                        create_request(directory, manager.clone(), original, collection, input).await,
                        Ok(Receipt::Deleted)
                    ));
                }
                stop(context, services, supplier).await;
            }))
            .await
            .expect("bounded title tree");
        }));
    }

    #[test]
    fn title_commit_faults_recover_the_original_and_startup_checks_the_tree() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostCommit, Fault::RolledBack, Fault::UnknownCommit] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = Box::pin(start(authentication::proxied(&database, &proxy), &supplier)).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let Item::Category(category) = create(&services, &manager, operation(), category_creation("Fault category", true)).await else { panic!("category") };
                    let Item::Discipline(discipline) = create(&services, &manager, operation(), discipline_creation("Fault discipline", category.id())).await else { panic!("discipline") };
                    let original = operation();
                    let input = title_creation("Fault title", discipline.id());
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let unknown = matches!(fault, Fault::UnknownCommit);
                    proxy.arm(fault);
                    let result = create_request(services.directory(), manager.clone(), original, Collection::Titles, input.clone()).await;
                    if rolled_back {
                        assert!(matches!(result, Err(Error::Unavailable(_))));
                    } else if unknown {
                        assert!(matches!(result, Err(Error::Unknown(_))));
                    } else {
                        result.expect("lost response recovers the original title");
                    }
                    proxy.release();
                    timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("actual title commit fault");
                    services.close();
                    services.drain().await.expect("title work drained");
                    context.stop().await.expect("context stopped");

                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    observer.client().batch_execute("BEGIN").await.expect("durable observation");
                    // The original transaction holds its parent discipline row until it ends.
                    observer.client().query_one("SELECT id FROM workforce.disciplines WHERE id = $1 FOR UPDATE", &[&discipline.id()]).await.expect("original transaction released the discipline");
                    let expected = if rolled_back { 0_i64 } else { 1_i64 };
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM workforce.titles WHERE operation_id = $1), (SELECT count(*) FROM workforce.completions WHERE operation_id = $1)", &[&original]).await.expect("durable title and completion");
                    assert_eq!(row.get::<_, i64>(0), expected);
                    assert_eq!(row.get::<_, i64>(1), expected);
                    observer.client().batch_execute("COMMIT").await.expect("observer committed");
                    observer.terminate().await.expect("observer ended");

                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let replayed = create_request(services.directory(), manager.clone(), original, Collection::Titles, input).await.expect("replay or new creation after recovery");
                    assert!(matches!(replayed, Receipt::Present(Item::Title(_))));
                    stop(context, services, supplier).await;
                    proxy.stop().await;
                }))
                .await
                .expect("bounded title commit fault");
            }

            for damage in [
                "DELETE FROM workforce.disciplines",
                "UPDATE workforce.categories SET name = 'Startup title'",
                "UPDATE workforce.titles SET operation_id = (SELECT operation_id FROM workforce.categories)",
            ] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    let institution = actor(&database, &services, &mut supplier).await;
                    let manager = manager(&database, &services, &mut supplier, &institution).await;
                    let Item::Category(category) = create(&services, &manager, operation(), category_creation("Startup category", true)).await else { panic!("category") };
                    let Item::Discipline(discipline) = create(&services, &manager, operation(), discipline_creation("Startup discipline", category.id())).await else { panic!("discipline") };
                    create(&services, &manager, operation(), title_creation("Startup title", discipline.id())).await;
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    let (context, services) = Box::pin(start(database.connections(&database.user), &supplier)).await;
                    services.ready().await.expect("title tree survives restart");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");

                    let owner = database.connections("postgres").recover().await.expect("owner");
                    owner.client().batch_execute(&format!("BEGIN; SET LOCAL session_replication_role=replica; {damage}; COMMIT")).await.expect("damage the title tree");
                    owner.terminate().await.expect("owner ended");
                    let context = Context::new(database.connections(&database.user)).await.expect("context");
                    let barrier = Barrier::begin(context.clone()).await.expect("barrier");
                    let result = Services::start(
                        barrier,
                        authentication::inventory(authentication::keys()),
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
                    .await;
                    assert!(matches!(result, Err(crate::startup::Error::Directory(_))), "{damage}");
                    context.stop().await.expect("context stopped");
                    supplier.stop().await;
                }))
                .await
                .expect("bounded title startup verification");
            }
        }));
    }

    #[test]
    fn selection_shows_only_selectable_items_to_current_members() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) =
                    Box::pin(start(database.connections(&database.user), &supplier)).await;
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = manager(&database, &services, &mut supplier, &institution).await;
                let member = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                Box::pin(grant::tests::admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Default)).await;
                let outsider = Box::pin(register_numbered(&database, &services, &mut supplier, 11)).await;
                let directory = services.directory();
                assert!(matches!(directory.read_selection(&outsider).await, Err(Error::Denied)));
                assert!(matches!(directory.list(&member, Collection::Groups).await, Err(Error::Denied)), "the ordinary member holds no directory permission");

                let Item::Group(open) = create(&services, &manager, operation(), group_creation("Open group", true, 1)).await else { panic!("group") };
                let Item::Group(closed) = create(&services, &manager, operation(), group_creation("Closed group", false, 0)).await else { panic!("group") };
                let department = |name: &'static str, enabled: bool, index: i32, group: group::Id, parent: Option<department::Id>| {
                    let services = &services; let manager = &manager;
                    async move {
                        let Item::Department(item) = create(services, manager, operation(), department_creation(name, enabled, index, group, parent)).await else { panic!("department") };
                        item
                    }
                };
                let root = department("Selectable root", true, 2, open.id(), None).await;
                let child = department("Selectable child", true, 1, open.id(), Some(root.id())).await;
                department("Closed group department", true, 0, closed.id(), None).await;
                let paused = department("Disabled parent", false, 0, open.id(), None).await;
                department("Hidden child", true, 0, open.id(), Some(paused.id())).await;
                let later = department("Later disabled default", true, 3, open.id(), None).await;
                let Item::Position(lead) = create(&services, &manager, operation(), position_creation("Selectable lead", true, 0)).await else { panic!("position") };
                let Item::Position(retired) = create(&services, &manager, operation(), position_creation("Retired role", false, 0)).await else { panic!("position") };
                let root_list = entries(replace_request(directory, manager.clone(), operation(), root.id(), replacement(&[], vec![choice(lead.id(), true, 1), choice(retired.id(), true, 0)])).await.expect("root list"));
                replace_request(directory, manager.clone(), operation(), child.id(), replacement(&[], vec![choice(lead.id(), false, 0)])).await.expect("child list");
                replace_request(directory, manager.clone(), operation(), paused.id(), replacement(&[], vec![choice(lead.id(), true, 0)])).await.expect("hidden list");
                let Item::Category(open_category) = create(&services, &manager, operation(), category_creation("Selectable category", true)).await else { panic!("category") };
                let Item::Category(closed_category) = create(&services, &manager, operation(), category_creation("Closed category", false)).await else { panic!("category") };
                let Item::Discipline(open_discipline) = create(&services, &manager, operation(), discipline_creation("Selectable discipline", open_category.id())).await else { panic!("discipline") };
                let Item::Discipline(hidden_discipline) = create(&services, &manager, operation(), discipline_creation("Discipline under closed category", closed_category.id())).await else { panic!("discipline") };
                let Item::Discipline(paused_discipline) = create(&services, &manager, operation(), discipline_creation("Paused discipline", open_category.id())).await else { panic!("discipline") };
                let Item::Title(open_title) = create(&services, &manager, operation(), title_creation("Selectable title", open_discipline.id())).await else { panic!("title") };
                create(&services, &manager, operation(), title_creation("Title under closed category", hidden_discipline.id())).await;
                create(&services, &manager, operation(), title_creation("Title under paused discipline", paused_discipline.id())).await;
                let disabled = discipline::Item { enabled: false, ..paused_discipline.clone() };
                update_request(directory, manager.clone(), operation(), Target::Discipline(paused_discipline.id()), modification(&Item::Discipline(paused_discipline), &Item::Discipline(disabled))).await.expect("disable discipline");
                configure(&services, &manager, operation(), Subtype::Internship, rule_creation(root.id(), false)).await;
                configure(&services, &manager, operation(), Subtype::Clinical, rule_creation(later.id(), true)).await;
                let closed_later = department::Item { enabled: false, ..later.clone() };
                update_request(directory, manager.clone(), operation(), Target::Department(later.id()), modification(&Item::Department(later.clone()), &Item::Department(closed_later))).await.expect("disable later default");

                let selection = directory.read_selection(&member).await.expect("ordinary member selection");
                let lead_entry = root_list.iter().find(|entry| entry.position_id() == lead.id()).expect("lead entry");
                let missing: Vec<i16> = Subtype::ALL.into_iter().filter(|subtype| !matches!(subtype, Subtype::Internship | Subtype::Clinical)).map(i16::from).collect();
                assert_eq!(serde_json::to_value(&selection).expect("selection JSON"), json!({
                    "groups": [{"id": open.id(), "name": "Open group", "index": 1}],
                    "departments": [
                        {"id": child.id(), "name": "Selectable child", "index": 1, "groupId": open.id(), "parentId": root.id()},
                        {"id": root.id(), "name": "Selectable root", "index": 2, "groupId": open.id(), "parentId": null},
                    ],
                    "positions": [{"departmentId": root.id(), "entryId": lead_entry.id(), "positionId": lead.id(), "name": "Selectable lead", "index": 1}],
                    "categories": [{"id": open_category.id(), "name": "Selectable category", "index": 0}],
                    "disciplines": [{"id": open_discipline.id(), "name": "Selectable discipline", "index": 0, "categoryId": open_category.id()}],
                    "titles": [{"id": open_title.id(), "name": "Selectable title", "index": 0, "disciplineId": open_discipline.id()}],
                    "rules": [
                        {"id": 100, "departmentId": later.id(), "position": true, "selectable": false},
                        {"id": 902, "departmentId": root.id(), "position": false, "selectable": true},
                    ],
                    "missing": missing,
                }));
                assert_eq!(directory.read_selection(&manager).await.expect("manager selection"), selection);

                // A suspended Grant loses the applicant read until it is resumed.
                let identity = services.current().read(&member).await.expect("member identity");
                let target = json!({"target": {"accountId": identity.id(), "personId": identity.person_id(), "revisionId": identity.revision_id(), "mobile": mobile(10)}});
                let member_grant = services.grant().read_latest(&member).await.expect("member Grant");
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), member_grant.id(), serde_json::from_value(target.clone()).expect("confirmation"))).await.expect("suspend member");
                assert!(matches!(directory.read_selection(&member).await, Err(Error::Denied)));
                Box::pin(services.grant().resume(manager.clone(), operation(), pause.id(), serde_json::from_value(target).expect("confirmation"))).await.expect("resume member");
                directory.read_selection(&member).await.expect("resumed member selection");

                // Missing parents are damage, not a smaller selection.
                let owner = database.connections("postgres").recover().await.expect("owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; DELETE FROM workforce.groups WHERE name = 'Open group'; COMMIT").await.expect("damage the directory");
                owner.terminate().await.expect("owner ended");
                assert!(directory.registry().ready().is_ok());
                assert!(matches!(directory.read_selection(&member).await, Err(Error::Corrupt(_))));
                assert!(directory.registry().ready().is_err(), "damage closes admission");
                stop(context, services, supplier).await;
            }))
            .await
            .expect("bounded directory selection");
        }));
    }

    trait IntoDepartment {
        fn into_department(self) -> department::Id;
    }

    impl IntoDepartment for Target {
        fn into_department(self) -> department::Id {
            let Target::Department(id) = self else {
                panic!("department target")
            };
            id
        }
    }
}
