//! Offline system-administrator affiliation correction through the application executor.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    access::{self, holder, role},
    center::affiliation::{COMPLETIONS, Code, Id, command, database, profile::Profile},
    history::{self, Change, Read, Verified},
    identity::{
        account, keys,
        mobile::Mobile,
        operation,
        person::{self, revision},
    },
    kernel::{
        self, App, Authorization, Command, Order, completion,
        error::{Fault, FaultKind, Stage},
        tx::Tx,
    },
};

/// The complete private request; every unknown field at either level is rejected.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    operator: Operator,
    target: Target,
    affiliation: Code,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Operator {
    mobile: Mobile,
    account_id: account::Id,
    revision_id: revision::Id,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    account_id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
    affiliation_id: Id,
}

/// The frozen canonical input; the field order is part of the completion binding.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Canonical<'a> {
    operator: CanonicalOperator<'a>,
    target: CanonicalTarget,
    affiliation: Code,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalOperator<'a> {
    mobile: &'a str,
    account_id: account::Id,
    revision_id: revision::Id,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalTarget {
    account_id: account::Id,
    person_id: person::Id,
    revision_id: revision::Id,
    affiliation_id: Id,
}

impl Input {
    pub fn masked(&self) -> String {
        format!(
            "{}****{} / {} / {}",
            &self.operator.mobile.as_ref()[..3],
            &self.operator.mobile.as_ref()[7..],
            self.target.person_id,
            self.target.affiliation_id
        )
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Operator,
    Target,
    TargetChanged,
    Unset,
    Unchanged,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Operator => kernel::Status::Unprocessable,
            Self::Target | Self::Unset => kernel::Status::NotFound,
            Self::TargetChanged | Self::Unchanged => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Operator => "operator_unavailable",
            Self::Target => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::Unset => "affiliation_unset",
            Self::Unchanged => "state_conflict",
        }
    }
}

impl From<Rejection> for Error {
    fn from(rejection: Rejection) -> Self {
        Self::Rejected(rejection)
    }
}

pub(crate) async fn run(
    app: App,
    keys: Arc<keys::Keys>,
    operation_id: operation::Id,
    input: Input,
    dry_run: bool,
) -> Result<Option<()>, Error> {
    let command = Write {
        accounts: account::Reader::new(app.clone(), Arc::clone(&keys)),
        roles: role::Reader::new(app.clone()),
        revisions: revision::Reader::new(app.clone()),
        app: app.clone(),
        keys,
        input,
        id: Id::allocate(),
        locked: Mutex::new(None),
    };
    if dry_run {
        app.dry_run(operation_id, command).await
    } else {
        app.command(operation_id, command).await.map(Some)
    }
}

struct Write {
    app: App,
    keys: Arc<keys::Keys>,
    accounts: account::Reader,
    roles: role::Reader,
    revisions: revision::Reader,
    input: Input,
    id: Id,
    locked: Mutex<Option<Locked>>,
}

#[derive(Clone, Copy)]
struct Locked {
    operator: Option<account::Id>,
    target: Option<account::Id>,
}

struct Facts {
    profile: Verified<Profile>,
    person_id: person::Id,
    #[cfg(test)]
    operator_person: person::Id,
    operator_id: revision::Id,
    operation_id: operation::Id,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    const ORDER: Order = Order::OriginalFirst;
    type Facts = Facts;
    type Change = Change<Profile>;
    type Output = ();
    type Rejection = Rejection;

    fn session(
        &self,
    ) -> Option<(
        &crate::identity::current::Service,
        &crate::identity::current::Actor,
    )> {
        None
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        completion::Request::system(
            "system_correct",
            self.input.target.person_id.to_string(),
            &Canonical {
                operator: CanonicalOperator {
                    mobile: self.input.operator.mobile.as_ref(),
                    account_id: self.input.operator.account_id,
                    revision_id: self.input.operator.revision_id,
                },
                target: CanonicalTarget {
                    account_id: self.input.target.account_id,
                    person_id: self.input.target.person_id,
                    revision_id: self.input.target.revision_id,
                    affiliation_id: self.input.target.affiliation_id,
                },
                affiliation: self.input.affiliation,
            },
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        tx.shared(b"access/scope", b"institution").await?;
        let hash = self
            .keys
            .mobile
            .hash(&self.input.operator.mobile)
            .map_err(crypto)?;
        let operator = account::state::discover_mobile(tx.client(), &hash).await?;
        let target =
            account::state::discover_person(tx.client(), self.input.target.person_id).await?;
        match (operator, target) {
            (Some(operator), Some(target)) => {
                account::state::lock_management(tx.client(), operator, target).await?
            }
            (Some(operator), None) => account::state::lock_shared(tx.client(), operator).await?,
            (None, Some(target)) => account::state::lock(tx.client(), target).await?,
            (None, None) => {}
        }
        database::lock_person(tx.client(), self.input.target.person_id).await?;
        // The locations selected the locks. A concurrent move makes this set stale.
        if operator != account::state::discover_mobile(tx.client(), &hash).await?
            || target
                != account::state::discover_person(tx.client(), self.input.target.person_id).await?
        {
            return Err(Error::lock_set_changed());
        }
        *self.locked.lock().map_err(|_| {
            Error::corrupt("system affiliation correction retains its lock identity")
        })? = Some(Locked { operator, target });
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        _: Option<crate::identity::current::database::State>,
    ) -> Result<Authorization, Error> {
        let locked = self.locked()?;
        let hash = self
            .keys
            .mobile
            .hash(&self.input.operator.mobile)
            .map_err(crypto)?;
        if account::state::discover_mobile(tx.client(), &hash).await? != locked.operator
            || account::state::discover_person(tx.client(), self.input.target.person_id).await?
                != locked.target
        {
            return Err(Error::lock_set_changed());
        }
        let operator = locked
            .operator
            .ok_or(Error::Rejected(Rejection::Operator))?;
        let now = tx.now(self.app.context()).await?;
        let subject = self
            .accounts
            .find_by_id(tx.client(), operator, &self.input.operator.mobile, now)
            .await?
            .ok_or(Error::Rejected(Rejection::Operator))?;
        if subject.suspension_id().is_some()
            || subject.id() != self.input.operator.account_id
            || subject.revision_id() != self.input.operator.revision_id
        {
            return Err(Error::Rejected(Rejection::Operator));
        }
        Ok(Authorization {
            person_id: subject.person_id(),
            revision_id: subject.revision_id(),
            now,
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let locked = self.locked()?;
        let target = locked.target.ok_or(Error::Rejected(Rejection::Target))?;
        let now = authority.now;
        let person_id = self.input.target.person_id;
        self.accounts
            .person_state(tx.client(), person_id, now)
            .await?
            .filter(|state| state.id() == target && state.current().is_some())
            .ok_or(Error::Rejected(Rejection::Target))?;
        let subject = self
            .accounts
            .find_current(tx.client(), target, now)
            .await?
            .ok_or(Error::Rejected(Rejection::Target))?;
        if subject.person_id() != person_id {
            return Err(Error::corrupt(
                "a correction target account retains its permanent person",
            ));
        }
        if subject.id() != self.input.target.account_id
            || subject.revision_id() != self.input.target.revision_id
        {
            return Err(Error::Rejected(Rejection::TargetChanged));
        }
        let holder = history::find::<holder::State>(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            Read::Locked,
            target,
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "a current correction target retains its holder",
        ))?;
        if !holder.active() || holder.person_id() != person_id {
            return Err(Error::corrupt(
                "a current correction target and its holder agree",
            ));
        }
        let role = self
            .roles
            .find(
                tx.client(),
                holder.role_id(),
                access::Scope::Institution,
                now,
            )
            .await?
            .ok_or(Error::corrupt(
                "a current correction target retains its institutional role",
            ))?;
        if role.kind() != role::Kind::Full {
            return Err(Error::Rejected(Rejection::Target));
        }
        let profile = history::find::<Profile>(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            Read::Locked,
            person_id,
            now,
        )
        .await?
        .ok_or(Error::Rejected(Rejection::Unset))?;
        if profile.affiliation_id() != self.input.target.affiliation_id {
            return Err(Error::Rejected(Rejection::TargetChanged));
        }
        let records =
            history::page::<Profile>(tx.client(), self.app.keys(), &profile, 1, 0, now).await?;
        let head = records
            .first()
            .ok_or(Error::corrupt("a filled profile retains its head record"))?;
        if head.code() == self.input.affiliation {
            return Err(Error::Rejected(Rejection::Unchanged));
        }
        Ok(Facts {
            profile,
            person_id,
            #[cfg(test)]
            operator_person: authority.person_id,
            operator_id: authority.revision_id,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Self::Change, Error> {
        command::advance(
            Some(facts.profile),
            facts.person_id,
            database::Data {
                id: self.id,
                code: self.input.affiliation,
                operator_id: facts.operator_id,
                #[cfg(test)]
                operator_person: facts.operator_person,
                created_at: now,
                operation_id: facts.operation_id,
            },
            now,
        )
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: Self::Change,
        pending: &mut Option<crate::history::Pending>,
    ) -> Result<(), Error> {
        *pending = Some(
            change
                .write(tx.client(), self.app.keys(), self.app.anchors())
                .await?,
        );
        Ok(())
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        let record = history::original::<database::Record>(
            tx.client(),
            self.app.keys(),
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "a system affiliation completion retains its original record",
        ))?;
        let completion =
            completion::original(tx.client(), self.app.keys(), COMPLETIONS, tx.operation_id())
                .await?
                .ok_or(Error::corrupt(
                    "a system affiliation replay retains its original completion",
                ))?;
        let operator = self
            .revisions
            .fetch_identity(tx.client(), record.operator_id())
            .await?;
        if record.person_id().to_string() != request.target()
            || operator.person_id() != request.person_id()
            || record.operator_id() != request.revision_id()
            || record.code() != self.input.affiliation
            || record.created_at() != completion.created_at()
        {
            return Err(Error::corrupt(
                "the original affiliation agrees with its authenticated completion",
            ));
        }
        self.app.anchors().confirm::<Profile>(&record)?;
        Ok(())
    }

    async fn absent(&self, tx: &Tx) -> Result<(), Error> {
        let now = tx.now(self.app.context()).await?;
        if history::original::<database::Record>(
            tx.client(),
            self.app.keys(),
            tx.operation_id(),
            now,
        )
        .await?
        .is_some()
        {
            return Err(Error::corrupt(
                "a retained affiliation keeps its original completion",
            ));
        }
        Ok(())
    }
}

impl Write {
    fn locked(&self) -> Result<Locked, Error> {
        self.locked
            .lock()
            .map_err(|_| Error::corrupt("system affiliation correction retains its lock identity"))?
            .ok_or_else(|| Error::corrupt("system affiliation correction resolves its locks"))
    }
}

fn crypto(error: platform::cryptography::Error) -> Error {
    Error::Unavailable(Fault::new(Stage::Lock, FaultKind::Crypto(error)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::time::timeout;
    use uuid::Uuid;

    use super::{Canonical, CanonicalOperator, CanonicalTarget, Error, Input, Rejection};
    use crate::{
        access::{
            appointment::tests::{register, register_numbered},
            role::tests::{actor, start},
        },
        center::affiliation::{self, Code, Id, command, database, profile},
        database::Barrier,
        history::{self, Seed},
        identity::{
            account,
            authentication::{self, Database, Supplier, server},
            current, operation,
            person::{self, revision},
        },
        kernel::{FaultKind, Stage},
        pagination::Pagination,
        startup::Administration,
    };

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    fn at() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000, 123_000).expect("fixed time")
    }

    fn input(
        mobile: &str,
        operator: (account::Id, revision::Id),
        target: (account::Id, person::Id, revision::Id, Id),
        code: &str,
    ) -> Input {
        serde_json::from_value(json!({
            "operator": {"mobile": mobile, "accountId": operator.0, "revisionId": operator.1},
            "target": {
                "accountId": target.0,
                "personId": target.1,
                "revisionId": target.2,
                "affiliationId": target.3,
            },
            "affiliation": code,
        }))
        .expect("system affiliation correction input")
    }

    fn fill(code: &str) -> affiliation::Input {
        serde_json::from_value(json!({"affiliation": code})).expect("first fill")
    }

    async fn facts(
        database: &Database,
        actor: &current::Actor,
    ) -> (account::Id, person::Id, revision::Id) {
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("independent actor facts");
        let row = observer
            .client()
            .query_one(
                "SELECT account.id, account.person_id, person.revision_id
                 FROM identity.sessions session
                 JOIN identity.accounts account ON account.id=session.account_id
                 JOIN identity.persons person ON person.id=account.person_id
                 WHERE session.id=$1",
                &[&actor.session_id()],
            )
            .await
            .expect("session actor facts");
        observer.terminate().await.expect("observer ended");
        (row.get(0), row.get(1), row.get(2))
    }

    async fn head(database: &Database, person_id: person::Id) -> (Id, i64, i64) {
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("independent profile facts");
        let row = observer
            .client()
            .query_one(
                "SELECT affiliation_id, sequence, version FROM center.profiles WHERE id=$1",
                &[&person_id],
            )
            .await
            .expect("retained profile");
        observer.terminate().await.expect("observer ended");
        (row.get(0), row.get(1), row.get(2))
    }

    async fn snapshot(client: &tokio_postgres::Client) -> Vec<(String, String)> {
        let mut result = Vec::new();
        let tables = client.query(
            "SELECT format('%I.%I', namespace.nspname, relation.relname) FROM pg_catalog.pg_class relation
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid=relation.relnamespace
             WHERE namespace.nspname IN ('identity','access','center','workforce') AND relation.relkind='r'
             ORDER BY namespace.nspname,relation.relname", &[],
        ).await.expect("business tables in the actual migrated database");
        for table in tables {
            let table: String = table.get(0);
            // PostgreSQL quotes catalog identifiers; command input never supplies these names.
            let sql = format!(
                "SELECT COALESCE(jsonb_agg(to_jsonb(record) ORDER BY to_jsonb(record))::text, '[]') FROM {table} record"
            );
            result.push((
                table,
                client
                    .query_one(&sql, &[])
                    .await
                    .expect("independent retained facts")
                    .get(0),
            ));
        }
        result
    }

    async fn start_administration(
        database: &Database,
    ) -> (Administration, crate::runtime::Context) {
        let context = crate::runtime::Context::new(database.connections(&database.user))
            .await
            .expect("maintenance instance");
        let barrier = Barrier::begin(context.clone())
            .await
            .expect("maintenance startup barrier");
        let administration = Administration::start(
            barrier,
            authentication::inventory(authentication::keys()),
            authentication::session_settings().app_id().clone(),
        )
        .await
        .expect("verified maintenance application");
        (administration, context)
    }

    async fn stop_administration(administration: Administration, context: crate::runtime::Context) {
        administration.close();
        administration
            .drain()
            .await
            .expect("maintenance work drained");
        context.stop().await.expect("maintenance instance released");
    }

    #[test]
    fn input_rejects_unknown_and_missing_fields_without_disclosing_the_value() {
        let account = account::Id::allocate();
        let person = person::Id::allocate();
        let revision = revision::Id::allocate();
        let affiliation = Id::allocate();
        let valid = json!({
            "operator": {"mobile": "13900000000", "accountId": account, "revisionId": revision},
            "target": {"accountId": account, "personId": person, "revisionId": revision, "affiliationId": affiliation},
            "affiliation": "13",
        });
        assert!(serde_json::from_value::<Input>(valid.clone()).is_ok());
        let mut top = valid.clone();
        top["extra"] = json!(true);
        let mut operator = valid.clone();
        operator["operator"]["extra"] = json!(true);
        let mut target = valid.clone();
        target["target"]["extra"] = json!(true);
        let mut missing = valid.clone();
        missing["target"]
            .as_object_mut()
            .expect("target object")
            .remove("affiliationId");
        let mut unset = valid.clone();
        unset["target"]["affiliationId"] = json!(null);
        let mut code = valid.clone();
        code["affiliation"] = json!("14");
        let mut uuid = valid.clone();
        uuid["target"]["accountId"] = json!("invalid");
        for invalid in [top, operator, target, missing, unset, code, uuid] {
            assert!(serde_json::from_value::<Input>(invalid).is_err());
        }
        let mut sensitive = valid.clone();
        sensitive["affiliation"] = json!("sensitive-marker");
        let error = serde_json::from_value::<Input>(sensitive)
            .err()
            .expect("invalid affiliation");
        assert!(!format!("{error:?} {error}").contains("sensitive-marker"));
        let masked = serde_json::from_value::<Input>(valid)
            .expect("valid input")
            .masked();
        assert!(masked.contains("139****0000"));
        assert!(!masked.contains("13900000000"));
        assert!(!masked.contains("\"affiliation\""));
    }

    #[test]
    fn canonical_input_freezes_every_field_in_order() {
        let account = account::Id::allocate();
        let person = person::Id::allocate();
        let revision = revision::Id::allocate();
        let affiliation = Id::allocate();
        let canonical = serde_json::to_string(&Canonical {
            operator: CanonicalOperator {
                mobile: "13900000000",
                account_id: account,
                revision_id: revision,
            },
            target: CanonicalTarget {
                account_id: account,
                person_id: person,
                revision_id: revision,
                affiliation_id: affiliation,
            },
            affiliation: Code::Unaffiliated,
        })
        .expect("canonical input");
        assert_eq!(
            canonical,
            format!(
                r#"{{"operator":{{"mobile":"13900000000","accountId":"{account}","revisionId":"{revision}"}},"target":{{"accountId":"{account}","personId":"{person}","revisionId":"{revision}","affiliationId":"{affiliation}"}},"affiliation":"13"}}"#
            )
        );
    }

    #[test]
    fn strict_time_rejects_equal_or_earlier_heads_and_accepts_later() {
        let inventory = authentication::inventory(authentication::keys());
        let person = person::Id::allocate();
        let state = Seed::state::<profile::Profile>(
            profile::Data {
                person_id: person,
                affiliation_id: Id::allocate(),
                created_at: at(),
            },
            1,
            1,
            history::Keys {
                data: &inventory.encryption,
                seals: &inventory.seal,
            },
        )
        .expect("verified pure-test state");
        let record = || database::Data {
            id: Id::allocate(),
            code: Code::Unaffiliated,
            operator_id: revision::Id::allocate(),
            #[cfg(test)]
            operator_person: person,
            created_at: at(),
            operation_id: operation(),
        };
        for now in [at(), at() - chrono::Duration::microseconds(1)] {
            let error = command::advance(Some(state.clone()), person, record(), now)
                .err()
                .expect("time does not advance");
            assert!(matches!(
                error,
                Error::Unavailable(fault) if fault.stage()==Stage::Time && matches!(fault.kind(), FaultKind::Integrity("affiliation time strictly advances the previous value"))
            ));
        }
        assert!(
            command::advance::<Rejection>(
                Some(state),
                person,
                record(),
                at() + chrono::Duration::microseconds(1)
            )
            .is_ok()
        );
        assert!(command::advance::<Rejection>(None, person, record(), at()).is_ok());
    }

    #[test]
    fn correction_requires_a_filled_system_administrator_and_replays_original_material() {
        let server = server();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Runtime::new()
                    .expect("runtime")
                    .block_on(Box::pin(async move {
                        timeout(Duration::from_secs(90), Box::pin(async {
                            let database = Database::initialized(Arc::clone(&server)).await;
                            let mut supplier = Supplier::new().await;
                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            let administrator = actor(&database, &services, &mut supplier).await;
                            let member = register(&database, &services, &mut supplier).await;
                            let ordinary =
                                register_numbered(&database, &services, &mut supplier, 2).await;
                            let (admin_account, admin_person, admin_revision) =
                                facts(&database, &administrator).await;
                            let (member_account, member_person, member_revision) =
                                facts(&database, &member).await;
                            let (ordinary_account, ordinary_person, ordinary_revision) =
                                facts(&database, &ordinary).await;
                            let keys =
                                Arc::new(authentication::inventory(authentication::keys()).identity);
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let observer = database
                                .connections(&database.user)
                                .recover()
                                .await
                                .expect("independent facts");
                            let before = snapshot(observer.client()).await;
                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let command = |account, revision, affiliation, code| {
                                input(
                                    "13900000000",
                                    (admin_account, admin_revision),
                                    (account, admin_person, revision, affiliation),
                                    code,
                                )
                            };
                            assert!(matches!(
                                administration
                                    .correct_affiliation(
                                        operation(),
                                        command(admin_account, admin_revision, Id::allocate(), "13"),
                                        false,
                                    )
                                    .await,
                                Err(Error::Rejected(Rejection::Unset))
                            ));
                            assert!(matches!(
                                administration
                                    .correct_affiliation(
                                        operation(),
                                        command(admin_account, admin_revision, Id::allocate(), "13"),
                                        true,
                                    )
                                    .await,
                                Err(Error::Rejected(Rejection::Unset))
                            ));
                            let ordinary_target = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    ordinary_account,
                                    ordinary_person,
                                    ordinary_revision,
                                    Id::allocate(),
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), ordinary_target, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Target))
                            ));
                            assert_eq!(
                                snapshot(observer.client()).await,
                                before,
                                "unset and ordinary targets write no business facts"
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            crate::access::appoint::run(
                                services.app().clone(),
                                Arc::clone(&keys),
                                operation(),
                                serde_json::from_value(json!({
                                    "operator": {"mobile": "13900000000"},
                                    "accountId": member_account,
                                    "revisionId": member_revision,
                                    "selection": {"action": "grant"},
                                }))
                                .expect("system role grant"),
                                false,
                            )
                            .await
                            .expect("second system administrator");
                            let seed_operation = operation();
                            services
                                .affiliation()
                                .create(administrator.clone(), seed_operation, fill("01"))
                                .await
                                .expect("original affiliation value");
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let (original_id, sequence, version) = head(&database, admin_person).await;
                            assert_eq!((sequence, version), (1, 1));
                            let before = snapshot(observer.client()).await;
                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let member_target = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    member_account,
                                    member_person,
                                    member_revision,
                                    Id::allocate(),
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), member_target, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Unset))
                            ));
                            let original_operation = operation();
                            let command = || {
                                input(
                                    "13900000000",
                                    (admin_account, admin_revision),
                                    (
                                        admin_account,
                                        admin_person,
                                        admin_revision,
                                        original_id,
                                    ),
                                    "13",
                                )
                            };
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), true)
                                    .await
                                    .expect("real dry-run decision")
                                    .is_none()
                            );
                            assert_eq!(
                                snapshot(observer.client()).await,
                                before,
                                "dry run writes no business facts"
                            );
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), false)
                                    .await
                                    .expect("committed system affiliation correction")
                                    .is_some()
                            );
                            let (corrected_id, sequence, version) =
                                head(&database, admin_person).await;
                            assert_ne!(corrected_id, original_id);
                            assert_eq!((sequence, version), (2, 2));
                            assert_eq!(
                                observer
                                    .client()
                                    .query_one(
                                        "SELECT (SELECT count(*) FROM center.affiliations),
                                                (SELECT count(*) FROM center.completions),
                                                (SELECT count(*) FROM center.affiliations WHERE operation_id=$1)",
                                        &[&seed_operation],
                                    )
                                    .await
                                    .expect("independent permanent facts")
                                    .get::<_, i64>(0),
                                2
                            );
                            let row = observer
                                .client()
                                .query_one(
                                    "SELECT affiliation.id, affiliation.person_id, affiliation.operator_id,
                                            affiliation.created_at, profile.affiliation_id,
                                            completion.subject, completion.action, completion.target,
                                            completion.person_id, completion.revision_id,
                                            completion.created_at, completion.expires_at
                                     FROM center.affiliations affiliation
                                     JOIN center.profiles profile ON profile.id=affiliation.person_id
                                     JOIN center.completions completion
                                        ON completion.operation_id=affiliation.operation_id
                                     WHERE affiliation.operation_id=$1",
                                    &[&original_operation],
                                )
                                .await
                                .expect("one atomic affiliation, profile and completion");
                            assert_eq!(row.get::<_, Id>(0), corrected_id);
                            assert_eq!(row.get::<_, person::Id>(1), admin_person);
                            assert_eq!(row.get::<_, revision::Id>(2), admin_revision);
                            assert_eq!(row.get::<_, Id>(4), corrected_id);
                            assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>(3), row.get::<_, chrono::DateTime<chrono::Utc>>(10));
                            assert_eq!(row.get::<_, String>(5), "system");
                            assert_eq!(row.get::<_, String>(6), "system_correct");
                            assert_eq!(row.get::<_, String>(7), admin_person.to_string());
                            assert_eq!(row.get::<_, person::Id>(8), admin_person);
                            assert_eq!(row.get::<_, revision::Id>(9), admin_revision);
                            assert_eq!(row.get::<_, Option<chrono::DateTime<chrono::Utc>>>(11), None);
                            assert_eq!(
                                observer
                                    .client()
                                    .query_one(
                                        "SELECT count(*) FROM center.affiliations WHERE operation_id=$1 AND person_id=$2 AND operator_id=$3",
                                        &[&seed_operation, &admin_person, &admin_revision],
                                    )
                                    .await
                                    .expect("retained original value")
                                    .get::<_, i64>(0),
                                1
                            );
                            let after = snapshot(observer.client()).await;
                            assert_eq!(before.len(), after.len());
                            for ((table, left), (after_table, right)) in before.iter().zip(&after) {
                                assert_eq!(table, after_table);
                                if !matches!(table.as_str(), "center.affiliations" | "center.profiles" | "center.completions") {
                                    assert_eq!(left, right, "{table} is unchanged");
                                }
                            }
                            assert_ne!(before, after, "the correction committed its business facts");

                            for dry_run in [false, true] {
                                assert!(
                                    administration
                                        .correct_affiliation(
                                            original_operation,
                                            command(),
                                            dry_run
                                        )
                                        .await
                                        .expect("original completion replay")
                                        .is_some()
                                );
                                assert_eq!(
                                    snapshot(observer.client()).await,
                                    after,
                                    "replay writes no facts"
                                );
                            }
                            for field in [
                                "operator.mobile",
                                "operator.accountId",
                                "operator.revisionId",
                                "target.accountId",
                                "target.personId",
                                "target.revisionId",
                                "target.affiliationId",
                                "affiliation",
                            ] {
                                let mut changed = command();
                                match field {
                                    "operator.mobile" => changed.operator.mobile = serde_json::from_value(json!("13700000000")).expect("different mobile"),
                                    "operator.accountId" => changed.operator.account_id = account::Id::allocate(),
                                    "operator.revisionId" => changed.operator.revision_id = revision::Id::allocate(),
                                    "target.accountId" => changed.target.account_id = account::Id::allocate(),
                                    "target.personId" => changed.target.person_id = person::Id::allocate(),
                                    "target.revisionId" => changed.target.revision_id = revision::Id::allocate(),
                                    "target.affiliationId" => changed.target.affiliation_id = Id::allocate(),
                                    "affiliation" => changed.affiliation = Code::ProbationaryParty,
                                    _ => unreachable!("closed canonical input fields"),
                                }
                                assert!(matches!(
                                    administration.correct_affiliation(original_operation, changed, false).await,
                                    Err(Error::Mismatch)
                                ), "{field} must mismatch before current-state checks");
                                assert_eq!(snapshot(observer.client()).await, after, "{field} writes nothing");
                            }
                            let stale = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    admin_account,
                                    admin_person,
                                    admin_revision,
                                    original_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), stale, false)
                                    .await,
                                Err(Error::Rejected(Rejection::TargetChanged))
                            ));
                            let unchanged = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    admin_account,
                                    admin_person,
                                    admin_revision,
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), unchanged, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Unchanged))
                            ));
                            let wrong_account = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    account::Id::allocate(),
                                    admin_person,
                                    admin_revision,
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), wrong_account, false)
                                    .await,
                                Err(Error::Rejected(Rejection::TargetChanged))
                            ));
                            let wrong_revision = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    admin_account,
                                    admin_person,
                                    revision::Id::allocate(),
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), wrong_revision, false)
                                    .await,
                                Err(Error::Rejected(Rejection::TargetChanged))
                            ));
                            let missing_person = input(
                                "13900000000",
                                (admin_account, admin_revision),
                                (
                                    account::Id::allocate(),
                                    person::Id::allocate(),
                                    admin_revision,
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), missing_person, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Target))
                            ));
                            let wrong_operator_account = input(
                                "13900000000",
                                (account::Id::allocate(), admin_revision),
                                (
                                    admin_account,
                                    admin_person,
                                    admin_revision,
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), wrong_operator_account, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Operator))
                            ));
                            let wrong_operator_revision = input(
                                "13900000000",
                                (admin_account, revision::Id::allocate()),
                                (
                                    admin_account,
                                    admin_person,
                                    admin_revision,
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), wrong_operator_revision, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Operator))
                            ));
                            let unknown_operator = input(
                                "13700000000",
                                (admin_account, admin_revision),
                                (
                                    admin_account,
                                    admin_person,
                                    admin_revision,
                                    corrected_id,
                                ),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), unknown_operator, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Operator))
                            ));
                            assert_eq!(
                                snapshot(observer.client()).await,
                                after,
                                "all refusals preserve the committed facts"
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            services.ready().await.expect("system correction survives startup");
                            let list = services
                                .affiliation()
                                .history(
                                    &administrator,
                                    Pagination::new(1, 20).expect("history page"),
                                )
                                .await
                                .expect("self history authenticates both sources");
                            assert_eq!(list.count(), 2);
                            assert_eq!(list.affiliations()[0].value, Code::Unaffiliated);
                            assert_eq!(list.affiliations()[1].value, Code::CommunistParty);
                            services.close();
                            services.drain().await.expect("final work drained");
                            context.stop().await.expect("final instance stopped");
                            let runtime = database
                                .connections(&database.user)
                                .recover()
                                .await
                                .expect("ordinary runtime writer");
                            for statement in [
                                "UPDATE center.affiliations SET created_at=created_at",
                                "DELETE FROM center.affiliations",
                                "UPDATE center.completions SET input=input",
                                "DELETE FROM center.completions",
                            ] {
                                assert_eq!(
                                    runtime
                                        .client()
                                        .execute(statement, &[])
                                        .await
                                        .expect_err("runtime cannot mutate permanent facts")
                                        .code(),
                                    Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
                                );
                            }
                            for (action, subject) in
                                [("correct", "system"), ("system_correct", "session")]
                            {
                                let insert = format!(
                                    "INSERT INTO center.completions \
                                     SELECT family, gen_random_uuid(), '{action}', \
                                            gen_random_uuid()::text, input, '{subject}', \
                                            person_id, revision_id, created_at, NULL, seal, \
                                            seal_key_id, seal_version \
                                     FROM center.completions LIMIT 1"
                                );
                                assert_eq!(
                                    runtime
                                        .client()
                                        .execute(&insert, &[])
                                        .await
                                        .expect_err("crossed subject and action are rejected")
                                        .code(),
                                    Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
                                );
                            }
                            runtime.terminate().await.expect("runtime writer ended");
                            observer.terminate().await.expect("observer ended");
                            assert!(
                                timeout(Duration::from_millis(30), supplier.request())
                                    .await
                                    .is_err()
                            );
                            supplier.stop().await;
                        }))
                        .await
                        .expect("system affiliation lifecycle deadline");
                    }));
            })
            .expect("test thread")
            .join()
            .expect("test thread result");
    }

    #[test]
    fn a_normal_operator_and_a_suspended_target_keep_the_original_completion() {
        let server = server();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Runtime::new()
                    .expect("runtime")
                    .block_on(Box::pin(async move {
                        timeout(Duration::from_secs(120), Box::pin(async {
                            let database = Database::initialized(Arc::clone(&server)).await;
                            let mut supplier = Supplier::new().await;
                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            let administrator = actor(&database, &services, &mut supplier).await;
                            let operator = register(&database, &services, &mut supplier).await;
                            let target =
                                register_numbered(&database, &services, &mut supplier, 2).await;
                            let manager =
                                register_numbered(&database, &services, &mut supplier, 3).await;
                            let (operator_account, operator_person, operator_revision) =
                                facts(&database, &operator).await;
                            let (target_account, target_person, target_revision) =
                                facts(&database, &target).await;
                            let (manager_account, _, manager_revision) =
                                facts(&database, &manager).await;
                            services
                                .affiliation()
                                .correct(
                                    administrator.clone(),
                                    operation(),
                                    target_person,
                                    serde_json::from_value(json!({
                                        "target": {
                                            "type": "account",
                                            "accountId": target_account,
                                            "personId": target_person,
                                            "revisionId": target_revision,
                                        },
                                        "affiliation": "01",
                                    }))
                                    .expect("management fill"),
                                )
                                .await
                                .expect("original target affiliation");
                            let (original_id, _, _) = head(&database, target_person).await;
                            crate::access::appoint::run(
                                services.app().clone(),
                                Arc::new(authentication::inventory(authentication::keys()).identity),
                                operation(),
                                serde_json::from_value(json!({
                                    "operator": {"mobile": "13900000000"},
                                    "accountId": manager_account,
                                    "revisionId": manager_revision,
                                    "selection": {"action": "grant"},
                                }))
                                .expect("manager role grant"),
                                false,
                            )
                            .await
                            .expect("independent manager");
                            let suspension = Box::pin(services.suspension().suspend(
                                administrator.clone(),
                                operation(),
                                target_account,
                                serde_json::from_value(json!({
                                    "personId": target_person,
                                    "revisionId": target_revision,
                                    "mobile": "13800000002",
                                }))
                                .expect("target suspension"),
                            ))
                            .await
                            .expect("ordinary target suspended before system appointment");
                            let suspension_id = suspension.id();
                            crate::access::appoint::run(
                                services.app().clone(),
                                Arc::new(authentication::inventory(authentication::keys()).identity),
                                operation(),
                                serde_json::from_value(json!({
                                    "operator": {"mobile": "13900000000"},
                                    "accountId": target_account,
                                    "revisionId": target_revision,
                                    "selection": {"action": "grant"},
                                }))
                                .expect("system role grant"),
                                false,
                            )
                            .await
                            .expect("suspended target is a system administrator");
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let observer = database
                                .connections(&database.user)
                                .recover()
                                .await
                                .expect("independent facts");
                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let command = || {
                                input(
                                    "13800000000",
                                    (operator_account, operator_revision),
                                    (
                                        target_account,
                                        target_person,
                                        target_revision,
                                        original_id,
                                    ),
                                    "13",
                                )
                            };
                            let original_operation = operation();
                            administration
                                .correct_affiliation(original_operation, command(), false)
                                .await
                                .expect("a separate normal operator corrects a suspended administrator")
                                .expect("committed correction");
                            let (corrected_id, sequence, version) = head(&database, target_person).await;
                            assert_eq!((sequence, version), (2, 2));
                            assert_ne!(corrected_id, original_id);
                            assert!(
                                observer
                                    .client()
                                    .query_one(
                                        "SELECT (SELECT operator_id=$1 FROM center.affiliations WHERE operation_id=$2)
                                                AND (SELECT suspension_id=$3 FROM identity.accounts WHERE id=$4)",
                                        &[&operator_revision, &original_operation, &suspension_id, &target_account],
                                    )
                                    .await
                                    .expect("fixed operator and preserved suspension")
                                    .get::<_, bool>(0)
                            );
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), false)
                                    .await
                                    .expect("original operator replay")
                                    .is_some()
                            );
                            let (later_id, _, _) = head(&database, target_person).await;
                            administration
                                .correct_affiliation(
                                    operation(),
                                    input(
                                        "13800000003",
                                        (manager_account, manager_revision),
                                        (target_account, target_person, target_revision, later_id),
                                        "02",
                                    ),
                                    false,
                                )
                                .await
                                .expect("a later affiliation value")
                                .expect("committed correction");
                            let (latest_id, _, _) = head(&database, target_person).await;
                            assert_ne!(latest_id, corrected_id);
                            let after = snapshot(observer.client()).await;
                            administration
                                .correct_affiliation(original_operation, command(), false)
                                .await
                                .expect("older completion survives a later correction")
                                .expect("original completion");
                            assert_eq!(head(&database, target_person).await.0, latest_id);
                            assert_eq!(snapshot(observer.client()).await, after);
                            assert_eq!(
                                observer
                                    .client()
                                    .query_one(
                                        "SELECT count(*) FROM center.affiliations",
                                        &[],
                                    )
                                    .await
                                    .expect("later value and replay")
                                    .get::<_, i64>(0),
                                3
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            let _renamed = Box::pin(services.correction().correct(
                                manager.clone(),
                                operation(),
                                serde_json::from_value(json!({
                                    "target": {"personId": operator_person, "revisionId": operator_revision},
                                    "name": "Renamed Operator",
                                    "idNumber": "00000020000229007X",
                                }))
                                .expect("operator rename"),
                            ))
                            .await
                            .expect("operator identity change");
                            let (_, _, renamed_revision) = facts(&database, &operator).await;
                            assert_ne!(renamed_revision, operator_revision);
                            let after_rename = snapshot(observer.client()).await;
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let (administration, maintenance) =
                                start_administration(&database).await;
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), true)
                                    .await
                                    .expect("replay after operator rename reports completion")
                                    .is_some()
                            );
                            let stale_operator = input(
                                "13800000000",
                                (operator_account, operator_revision),
                                (target_account, target_person, target_revision, latest_id),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), stale_operator, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Operator))
                            ));
                            assert_eq!(
                                snapshot(observer.client()).await,
                                after_rename,
                                "a stale operator revision writes nothing"
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            Box::pin(services.suspension().suspend(
                                manager.clone(),
                                operation(),
                                operator_account,
                                serde_json::from_value(json!({
                                    "personId": operator_person,
                                    "revisionId": renamed_revision,
                                    "mobile": "13800000000",
                                }))
                                .expect("operator suspension"),
                            ))
                            .await
                            .expect("actual operator suspension");
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let suspended_operator = input(
                                "13800000000",
                                (operator_account, renamed_revision),
                                (target_account, target_person, target_revision, latest_id),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), suspended_operator, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Operator))
                            ));
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), false)
                                    .await
                                    .expect("replay after operator suspension")
                                    .is_some()
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            Box::pin(services.cancellation().cancel_account(
                                manager.clone(),
                                operation(),
                                operator_account,
                                serde_json::from_value(json!({
                                    "personId": operator_person,
                                    "revisionId": renamed_revision,
                                    "mobile": "13800000000",
                                }))
                                .expect("operator cancellation"),
                            ))
                            .await
                            .expect("actual operator cancellation");
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let cancelled_operator = input(
                                "13800000000",
                                (operator_account, renamed_revision),
                                (target_account, target_person, target_revision, latest_id),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), cancelled_operator, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Operator))
                            ));
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), false)
                                    .await
                                    .expect("replay after operator cancellation")
                                    .is_some()
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            let role = Box::pin(services.role().create(
                                manager.clone(),
                                operation(),
                                serde_json::from_value(json!({
                                    "name": "Former administrator",
                                    "level": 2,
                                    "permissions": [],
                                }))
                                .expect("ordinary role"),
                            ))
                            .await
                            .expect("ordinary institutional role");
                            crate::access::appoint::run(
                                services.app().clone(),
                                Arc::new(authentication::inventory(authentication::keys()).identity),
                                operation(),
                                serde_json::from_value(json!({
                                    "operator": {"mobile": "13800000003"},
                                    "accountId": target_account,
                                    "revisionId": target_revision,
                                    "selection": {"action": "revoke", "roleId": role.id()},
                                }))
                                .expect("role replacement"),
                                false,
                            )
                            .await
                            .expect("target role change");
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let former = input(
                                "13800000003",
                                (manager_account, manager_revision),
                                (target_account, target_person, target_revision, latest_id),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), former, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Target))
                            ));
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), false)
                                    .await
                                    .expect("replay after target role change")
                                    .is_some()
                            );
                            stop_administration(administration, maintenance).await;

                            let (context, services) =
                                start(database.connections(&database.user), &supplier).await;
                            Box::pin(services.suspension().resume(
                                manager.clone(),
                                operation(),
                                target_account,
                                serde_json::from_value(json!({
                                    "personId": target_person,
                                    "revisionId": target_revision,
                                    "mobile": "13800000002",
                                    "suspensionId": suspension_id,
                                }))
                                .expect("target resumption"),
                            ))
                            .await
                            .expect("target resumption before cancellation");
                            Box::pin(services.cancellation().cancel_account(
                                manager.clone(),
                                operation(),
                                target_account,
                                serde_json::from_value(json!({
                                    "personId": target_person,
                                    "revisionId": target_revision,
                                    "mobile": "13800000002",
                                }))
                                .expect("target cancellation"),
                            ))
                            .await
                            .expect("actual target cancellation");
                            services.close();
                            services.drain().await.expect("online work drained");
                            context
                                .stop()
                                .await
                                .expect("online instance released for maintenance");

                            let (administration, maintenance) =
                                start_administration(&database).await;
                            let cancelled_target = input(
                                "13800000003",
                                (manager_account, manager_revision),
                                (target_account, target_person, target_revision, latest_id),
                                "13",
                            );
                            assert!(matches!(
                                administration
                                    .correct_affiliation(operation(), cancelled_target, false)
                                    .await,
                                Err(Error::Rejected(Rejection::Target))
                            ));
                            assert!(
                                administration
                                    .correct_affiliation(original_operation, command(), false)
                                    .await
                                    .expect("replay after target cancellation")
                                    .is_some()
                            );
                            assert_eq!(
                                observer
                                    .client()
                                    .query_one(
                                        "SELECT (SELECT count(*) FROM center.affiliations),
                                                (SELECT count(*) FROM center.completions WHERE action='system_correct')",
                                        &[],
                                    )
                                    .await
                                    .expect("replays never append")
                                    .get::<_, i64>(0),
                                3
                            );
                            stop_administration(administration, maintenance).await;
                            observer.terminate().await.expect("observer ended");
                            assert!(
                                timeout(Duration::from_millis(30), supplier.request())
                                    .await
                                    .is_err()
                            );
                            supplier.stop().await;
                        }))
                        .await
                        .expect("system affiliation operator and target lifecycle deadline");
                    }));
            })
            .expect("test thread")
            .join()
            .expect("test thread result");
    }

    #[test]
    fn system_correction_recovers_real_commit_faults_and_cancelled_waiters() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(Box::pin(async {
                for case in ["lost", "rollback", "late", "unknown", "cancelled"] {
                    timeout(Duration::from_secs(45), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start(
                            authentication::proxied(&database, &proxy),
                            &supplier,
                        )
                        .await;
                        let manager = actor(&database, &services, &mut supplier).await;
                        let (account, person, revision) = facts(&database, &manager).await;
                        let seed_operation = operation();
                        services
                            .affiliation()
                            .create(manager.clone(), seed_operation, fill("01"))
                            .await
                            .expect("original affiliation value");
                        let observer = database
                            .connections(&database.user)
                            .recover()
                            .await
                            .expect("independent observer");
                        let affiliation: Id = observer
                            .client()
                            .query_one(
                                "SELECT affiliation_id FROM center.profiles WHERE id=$1",
                                &[&person],
                            )
                            .await
                            .expect("original profile")
                            .get(0);
                        let unrelated = database
                            .connections(&database.user)
                            .recover()
                            .await
                            .expect("unrelated session");
                        unrelated
                            .client()
                            .batch_execute("BEGIN; SELECT pg_advisory_xact_lock(900123, 1)")
                            .await
                            .expect("unrelated idle transaction owns a distinct lock");
                        let original = operation();
                        let keys =
                            Arc::new(authentication::inventory(authentication::keys()).identity);
                        let command = |services: &crate::startup::Services,
                                       operation_id| {
                            let input = input(
                                "13900000000",
                                (account, revision),
                                (account, person, revision, affiliation),
                                "13",
                            );
                            super::run(
                                services.app().clone(),
                                Arc::clone(&keys),
                                operation_id,
                                input,
                                false,
                            )
                        };
                        proxy.arm(match case {
                            "lost" => Fault::LostCommit,
                            "rollback" => Fault::RolledBack,
                            "unknown" => Fault::UnknownCommit,
                            _ => Fault::DelayedCommit,
                        });
                        let waiter = tokio::spawn(command(&services, original));
                        let backend = timeout(Duration::from_secs(5), proxy.intercepted())
                            .await
                            .expect("actual business COMMIT interception");
                        if matches!(case, "late" | "cancelled") {
                            timeout(Duration::from_secs(5), async {
                                loop {
                                    let waiting: bool = observer
                                        .client()
                                        .query_one(
                                            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks waiting
                                             JOIN pg_catalog.pg_locks original ON original.locktype=waiting.locktype
                                                 AND original.database=waiting.database AND original.classid=waiting.classid
                                                 AND original.objid=waiting.objid AND original.objsubid=waiting.objsubid
                                             WHERE original.pid=$1 AND original.granted AND NOT waiting.granted
                                                 AND waiting.pid<>original.pid AND waiting.locktype='advisory'
                                                 AND waiting.objsubid=1 AND original.pid=ANY(pg_blocking_pids(waiting.pid)))",
                                            &[&backend],
                                        )
                                        .await
                                        .expect("exact original backend lock graph")
                                        .get(0);
                                    if waiting {
                                        break;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("recovery queues on the original operation");
                            assert!(!waiter.is_finished(), "late COMMIT cannot be inferred absent");
                        }
                        if case == "cancelled" {
                            waiter.abort();
                            assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                            proxy.release();
                        } else {
                            if matches!(case, "late" | "unknown") {
                                proxy.release();
                            }
                            let result = waiter.await.expect("command task");
                            match case {
                                "rollback" => assert!(matches!(result, Err(Error::Unavailable(_)))),
                                "unknown" => assert!(matches!(result, Err(Error::Unknown(_)))),
                                _ => {
                                    result
                                        .expect("original recovered completion")
                                        .expect("not a preview");
                                }
                            }
                            if case == "lost" {
                                proxy.release();
                            }
                        }
                        timeout(Duration::from_secs(5), async {
                            while observer
                                .client()
                                .query_one(
                                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                                    &[&backend],
                                )
                                .await
                                .expect("original backend termination")
                                .get::<_, bool>(0)
                            {
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("original operation backend ended before replay");
                        let row = observer
                            .client()
                            .query_one(
                                "SELECT (SELECT count(*) FROM center.affiliations WHERE operation_id=$1),
                                        (SELECT count(*) FROM center.completions WHERE operation_id=$1 AND action='system_correct'),
                                        (SELECT sequence FROM center.profiles WHERE id=$2),(SELECT version FROM center.profiles WHERE id=$2)",
                                &[&original, &person],
                            )
                            .await
                            .expect("independent effects before any retry");
                        let expected = if case == "rollback" { 0 } else { 1 };
                        for column in 0..2 {
                            assert_eq!(row.get::<_, i64>(column), expected, "{case} atomic effects");
                        }
                        assert_eq!(row.get::<_, i64>(2), 1 + expected, "{case} state sequence");
                        assert_eq!(row.get::<_, i64>(3), 1 + expected, "{case} state version");
                        unrelated
                            .client()
                            .batch_execute("SELECT 1; ROLLBACK")
                            .await
                            .expect("recovery preserves unrelated backend");
                        unrelated.terminate().await.expect("end unrelated transaction");
                        if !matches!(case, "cancelled" | "rollback") {
                            let list = services
                                .affiliation()
                                .history(&manager, Pagination::new(1, 20).expect("page"))
                                .await
                                .expect("shared pending state settles after original completion");
                            assert_eq!(list.affiliations()[0].value, Code::Unaffiliated);
                        }
                        let (context, services) = if case == "cancelled" {
                            services.close();
                            services.drain().await.expect("accepted work drained after release");
                            context.stop().await.expect("instance ended");
                            start(database.connections(&database.user), &supplier).await
                        } else {
                            (context, services)
                        };
                        command(&services, original)
                            .await
                            .expect("same input remains recoverable")
                            .expect("completion");
                        let row = observer
                            .client()
                            .query_one(
                                "SELECT affiliation.id, affiliation.operator_id,
                                        completion.created_at=affiliation.created_at,
                                        profile.affiliation_id=affiliation.id
                                 FROM center.affiliations affiliation
                                 JOIN center.completions completion
                                    ON completion.operation_id=affiliation.operation_id AND completion.action='system_correct'
                                 JOIN center.profiles profile ON profile.id=affiliation.person_id
                                 WHERE affiliation.operation_id=$1",
                                &[&original],
                            )
                            .await
                            .expect("one permanent affiliation after retry");
                        assert_eq!(row.get::<_, revision::Id>(1), revision);
                        assert!(row.get::<_, bool>(2) && row.get::<_, bool>(3));
                        observer.terminate().await.expect("observer ended");
                        services.close();
                        services.drain().await.expect("accepted work drained");
                        context.stop().await.expect("instance ended");
                        supplier.stop().await;
                        proxy.stop().await;
                    }))
                    .await
                    .expect("bounded system affiliation recovery scenario");
                }
            }));
    }

    #[test]
    fn recovery_rejects_damaged_system_facts_and_closes_admission() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(Box::pin(async {
                for case in ["operator", "target", "value"] {
                    timeout(Duration::from_secs(45), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) =
                            start(authentication::proxied(&database, &proxy), &supplier).await;
                        let administrator = actor(&database, &services, &mut supplier).await;
                        let (account, person, revision) = facts(&database, &administrator).await;
                        let other = register(&database, &services, &mut supplier).await;
                        let (_, other_person, other_revision) = facts(&database, &other).await;
                        services
                            .affiliation()
                            .create(administrator, operation(), fill("01"))
                            .await
                            .expect("original affiliation value");
                        let (affiliation, _, _) = head(&database, person).await;
                        let original = operation();
                        let keys =
                            Arc::new(authentication::inventory(authentication::keys()).identity);
                        let command = super::run(
                            services.app().clone(),
                            Arc::clone(&keys),
                            original,
                            input(
                                "13900000000",
                                (account, revision),
                                (account, person, revision, affiliation),
                                "13",
                            ),
                            false,
                        );
                        proxy.arm(Fault::DelayedCommit);
                        let waiter = tokio::spawn(command);
                        timeout(Duration::from_secs(5), proxy.intercepted())
                            .await
                            .expect("original correction COMMIT intercepted");
                        proxy.arm(Fault::StalledAffiliation);
                        proxy.release();
                        timeout(Duration::from_secs(5), proxy.intercepted())
                            .await
                            .expect("recovery paused before reading original facts");
                        let owner = database
                            .connections("postgres")
                            .recover()
                            .await
                            .expect("isolated fault owner");
                        let row = owner.client().query_one(
                            "SELECT person_id, operator_id FROM center.affiliations WHERE operation_id=$1",
                            &[&original],
                        ).await.expect("committed original before recovery");
                        assert_eq!(row.get::<_, person::Id>(0), person);
                        assert_eq!(row.get::<_, revision::Id>(1), revision);
                        owner.client().batch_execute(
                            "BEGIN; SET LOCAL session_replication_role=replica",
                        ).await.expect("controlled damage in disposable database");
                        match case {
                            "operator" => {
                                owner.client().execute(
                                    "UPDATE center.affiliations SET operator_id=$2 WHERE operation_id=$1",
                                    &[&original, &other_revision],
                                ).await.expect("substitute another existing operator");
                            }
                            "target" => {
                                owner.client().execute(
                                    "UPDATE center.affiliations SET person_id=$2 WHERE operation_id=$1",
                                    &[&original, &other_person],
                                ).await.expect("substitute another existing target");
                            }
                            "value" => {
                                owner.client().execute(
                                    "UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38)#1) WHERE operation_id=$1",
                                    &[&original],
                                ).await.expect("damage protected original value");
                            }
                            _ => unreachable!("closed damage cases"),
                        }
                        owner.client().batch_execute("COMMIT").await.expect("damage visible");
                        let damaged = snapshot(owner.client()).await;
                        proxy.release();
                        let result = waiter.await.expect("original command task");
                        assert!(matches!(result, Err(Error::Unknown(_))), "{case}: {result:?}");
                        assert!(services.ready().await.is_err(), "{case}: damaged recovery closes admission");
                        assert_eq!(snapshot(owner.client()).await, damaged, "recovery never repeats a write");
                        owner.terminate().await.expect("fault owner ended");
                        services.close();
                        services.drain().await.expect("accepted recovery drained");
                        context.stop().await.expect("instance ended");
                        proxy.stop().await;
                        supplier.stop().await;
                    }))
                    .await
                    .unwrap_or_else(|error| panic!("system recovery damage case {case}: {error}"));
                }
            }));
    }

    #[test]
    fn online_and_system_requests_share_the_original_operation_namespace() {
        let server = server();
        std::thread::Builder::new()
            .stack_size(24 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Runtime::new()
                    .expect("runtime")
                    .block_on(Box::pin(async move {
                        timeout(
                            Duration::from_secs(60),
                            Box::pin(async {
                                let database = Database::initialized(Arc::clone(&server)).await;
                                let mut supplier = Supplier::new().await;
                                let (context, services) =
                                    start(database.connections(&database.user), &supplier).await;
                                let administrator =
                                    actor(&database, &services, &mut supplier).await;
                                let (account, person, revision) =
                                    facts(&database, &administrator).await;
                                let online_operation = operation();
                                services
                                    .affiliation()
                                    .create(administrator.clone(), online_operation, fill("01"))
                                    .await
                                    .expect("online first fill");
                                let (online_id, _, _) = head(&database, person).await;
                                services.close();
                                services.drain().await.expect("online work drained");
                                context
                                    .stop()
                                    .await
                                    .expect("online instance released for maintenance");

                                let observer = database
                                    .connections(&database.user)
                                    .recover()
                                    .await
                                    .expect("independent facts");
                                let before = snapshot(observer.client()).await;
                                let (administration, maintenance) =
                                    start_administration(&database).await;
                                let collision = input(
                                    "13900000000",
                                    (account, revision),
                                    (account, person, revision, online_id),
                                    "13",
                                );
                                assert!(matches!(
                                    administration
                                        .correct_affiliation(online_operation, collision, false)
                                        .await,
                                    Err(Error::Mismatch)
                                ));
                                assert_eq!(
                                    snapshot(observer.client()).await,
                                    before,
                                    "an online original keeps its facts and namespace"
                                );
                                let system_operation = operation();
                                let correction = input(
                                    "13900000000",
                                    (account, revision),
                                    (account, person, revision, online_id),
                                    "13",
                                );
                                administration
                                    .correct_affiliation(system_operation, correction, false)
                                    .await
                                    .expect("system correction after the unrelated online original")
                                    .expect("committed correction");
                                let (system_id, _, _) = head(&database, person).await;
                                assert_ne!(system_id, online_id);
                                let committed = snapshot(observer.client()).await;
                                stop_administration(administration, maintenance).await;

                                let (context, services) =
                                    start(database.connections(&database.user), &supplier).await;
                                assert!(matches!(
                                    services
                                        .affiliation()
                                        .create(administrator.clone(), system_operation, fill("02"))
                                        .await,
                                    Err(affiliation::Error::Mismatch)
                                ));
                                assert_eq!(
                                    snapshot(observer.client()).await,
                                    committed,
                                    "a system original cannot be replayed as online work"
                                );
                                let list = services
                                    .affiliation()
                                    .history(&administrator, Pagination::new(1, 20).expect("page"))
                                    .await
                                    .expect("mixed source history");
                                assert_eq!(list.count(), 2);
                                assert_eq!(list.affiliations()[0].value, Code::Unaffiliated);
                                assert_eq!(list.affiliations()[1].value, Code::CommunistParty);
                                services.close();
                                services.drain().await.expect("online work drained");
                                context
                                    .stop()
                                    .await
                                    .expect("online instance released for maintenance");

                                let (administration, maintenance) =
                                    start_administration(&database).await;
                                let replay = input(
                                    "13900000000",
                                    (account, revision),
                                    (account, person, revision, online_id),
                                    "13",
                                );
                                assert!(
                                    administration
                                        .correct_affiliation(system_operation, replay, true)
                                        .await
                                        .expect("system replay after online refusals")
                                        .is_some()
                                );
                                assert_eq!(snapshot(observer.client()).await, committed);
                                stop_administration(administration, maintenance).await;
                                observer.terminate().await.expect("observer ended");
                                assert!(
                                    timeout(Duration::from_millis(30), supplier.request())
                                        .await
                                        .is_err()
                                );
                                supplier.stop().await;
                            }),
                        )
                        .await
                        .expect("namespace collision deadline");
                    }));
            })
            .expect("test thread")
            .join()
            .expect("test thread result");
    }

    #[test]
    fn missing_or_tampered_system_material_closes_startup() {
        let server = server();
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(Box::pin(async {
                for case in [
                    "missing_completion",
                    "crossed",
                    "input",
                    "seal",
                    "time",
                    "operator",
                    "target",
                    "value",
                ] {
                    timeout(Duration::from_secs(45), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let (context, services) =
                            start(database.connections(&database.user), &supplier).await;
                        let administrator = actor(&database, &services, &mut supplier).await;
                        let (account, person, revision) =
                            facts(&database, &administrator).await;
                        services
                            .affiliation()
                            .create(administrator.clone(), operation(), fill("01"))
                            .await
                            .expect("original affiliation value");
                        let (original_id, _, _) = head(&database, person).await;
                        services.close();
                        services.drain().await.expect("online work drained");
                        context
                            .stop()
                            .await
                            .expect("online instance released for maintenance");

                        let (administration, maintenance) = start_administration(&database).await;
                        let system_operation = operation();
                        administration
                            .correct_affiliation(
                                system_operation,
                                input(
                                    "13900000000",
                                    (account, revision),
                                    (account, person, revision, original_id),
                                    "13",
                                ),
                                false,
                            )
                            .await
                            .expect("committed system correction")
                            .expect("not a preview");
                        stop_administration(administration, maintenance).await;

                        let owner = database
                            .connections("postgres")
                            .recover()
                            .await
                            .expect("isolated damage owner");
                        owner
                            .client()
                            .batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
                            .await
                            .expect("explicit post-startup fault");
                        let damage = match case {
                            "missing_completion" => format!(
                                "DELETE FROM center.completions WHERE operation_id='{system_operation}'"
                            ),
                            "crossed" => format!(
                                "ALTER TABLE center.completions DROP CONSTRAINT completions_subject_check, DROP CONSTRAINT completions_subject_action_check; UPDATE center.completions SET subject='session' WHERE operation_id='{system_operation}'"
                            ),
                            "input" => format!("UPDATE center.completions SET input=set_byte(input,octet_length(input)-1,get_byte(input,octet_length(input)-1)#1) WHERE operation_id='{system_operation}'"),
                            "seal" => format!("UPDATE center.completions SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE operation_id='{system_operation}'"),
                            "time" => format!("UPDATE center.completions SET created_at=created_at-interval '1 second' WHERE operation_id='{system_operation}'"),
                            "operator" => format!("UPDATE center.affiliations SET operator_id=gen_random_uuid() WHERE operation_id='{system_operation}'"),
                            "target" => format!("UPDATE center.affiliations SET person_id=gen_random_uuid() WHERE operation_id='{system_operation}'"),
                            "value" => format!("UPDATE center.affiliations SET value=set_byte(value,38,get_byte(value,38)#1) WHERE operation_id='{system_operation}'"),
                            _ => unreachable!("closed damage cases"),
                        };
                        owner
                            .client()
                            .batch_execute(&damage)
                            .await
                            .expect("one exact isolated source mutation");
                        owner
                            .client()
                            .batch_execute("COMMIT")
                            .await
                            .expect("isolated fault established");
                        owner.terminate().await.expect("owner ended");

                        let context = crate::runtime::Context::new(
                            database.connections(&database.user),
                        )
                        .await
                        .expect("restart context");
                        let result = Box::pin(crate::startup::Services::start(
                            Barrier::begin(context.clone())
                                .await
                                .expect("restart barrier"),
                            authentication::inventory(authentication::keys()),
                            crate::startup::Settings::new(
                                4,
                                crate::identity::message::Limits::default(),
                                authentication::session_settings(),
                                crate::registration::Limits::default(),
                                authentication::avatar_settings(),
                            )
                            .expect("settings"),
                            supplier.client(),
                            authentication::unused_client().await,
                        ))
                        .await;
                        let Err(error) = result else {
                            panic!("{case}: damaged system material must close startup");
                        };
                        assert!(
                            matches!(error, crate::startup::Error::Affiliation(affiliation::Error::Corrupt(_))),
                            "{case}: damaged system material closes startup: {error:?}"
                        );
                        context.stop().await.expect("failed startup stopped");
                        supplier.stop().await;
                    }))
                    .await
                    .unwrap_or_else(|error| panic!("startup damage case {case}: {error}"));
                }
            }));
    }
}
