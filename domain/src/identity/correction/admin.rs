//! Offline system-administrator correction through the application executor.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{
    access::{holder, role},
    history::{self, Pending, Read, Verified},
    identity::{
        account::{self, state::State},
        correction::{self, Canonical, CanonicalOperator, CanonicalTarget, Correction, Id, Source},
        keys,
        mobile::Mobile,
        operation,
        person::{self, name::Name, number::IdNumber, revision},
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
    name: Name,
    id_number: IdNumber,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operator {
    mobile: Mobile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    person_id: person::Id,
    revision_id: revision::Id,
}

impl Input {
    pub fn masked(&self) -> String {
        format!(
            "{}****{} / {} / {}",
            &self.operator.mobile.as_ref()[..3],
            &self.operator.mobile.as_ref()[7..],
            self.target.person_id,
            self.target.revision_id
        )
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Operator,
    Target,
    TargetChanged,
    Unchanged,
    IdNumberTaken,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Operator => kernel::Status::Unprocessable,
            Self::Target => kernel::Status::NotFound,
            Self::TargetChanged | Self::Unchanged | Self::IdNumberTaken => kernel::Status::Conflict,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Operator => "operator_unavailable",
            Self::Target => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::Unchanged => "state_conflict",
            Self::IdNumberTaken => "identity_number_taken",
        }
    }
}

pub(crate) async fn run(
    app: App,
    keys: Arc<keys::Keys>,
    operation_id: operation::Id,
    input: Input,
    dry_run: bool,
) -> Result<Option<Correction>, Error> {
    let command = Write {
        app: app.clone(),
        keys: Arc::clone(&keys),
        accounts: account::Reader::new(app.clone(), keys),
        roles: role::Reader::new(app.clone()),
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
    target: account::Id,
    account: Verified<State>,
    person: Verified<person::state::State>,
    before: revision::Profile,
    holder: Verified<holder::State>,
    operator_id: revision::Id,
    operation_id: operation::Id,
}

struct Selected {
    id: Id,
    corrected_at: DateTime<Utc>,
    target: account::Id,
    account: Verified<State>,
    person: Verified<person::state::State>,
    before: revision::Profile,
    holder: Verified<holder::State>,
    operator_id: revision::Id,
    operation_id: operation::Id,
    name: Name,
    id_number: IdNumber,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = correction::COMPLETIONS;
    const ORDER: Order = Order::OriginalFirst;
    type Facts = Facts;
    type Change = Selected;
    type Output = Correction;
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
            "correct",
            self.input.target.person_id.to_string(),
            &Canonical {
                operator: CanonicalOperator {
                    mobile: self.input.operator.mobile.as_ref(),
                },
                target: CanonicalTarget {
                    person_id: self.input.target.person_id,
                    revision_id: self.input.target.revision_id,
                },
                name: &self.input.name,
                id_number: &self.input.id_number,
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
        correction::lock_person(tx.client(), self.input.target.person_id).await?;
        kernel::tx::object(
            tx.client(),
            b"identity/id_number",
            self.input.id_number.as_ref().as_bytes(),
        )
        .await?;
        // The locations selected the locks. A concurrent move makes this set stale.
        if operator != account::state::discover_mobile(tx.client(), &hash).await?
            || target
                != account::state::discover_person(tx.client(), self.input.target.person_id).await?
        {
            return Err(Error::lock_set_changed());
        }
        *self
            .locked
            .lock()
            .map_err(|_| Error::corrupt("system correction retains its lock identity"))? =
            Some(Locked { operator, target });
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
        if subject.suspension_id().is_some() {
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
        let account = self
            .accounts
            .person_state(tx.client(), self.input.target.person_id, now)
            .await?
            .filter(|state| state.id() == target && state.current().is_some())
            .ok_or(Error::Rejected(Rejection::Target))?;
        let subject = self
            .accounts
            .find_current(tx.client(), target, now)
            .await?
            .ok_or(Error::Rejected(Rejection::Target))?;
        if subject.person_id() != self.input.target.person_id {
            return Err(Error::corrupt(
                "a correction target account retains its permanent person",
            ));
        }
        if subject.revision_id() != self.input.target.revision_id {
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
        if !holder.active() || holder.person_id() != self.input.target.person_id {
            return Err(Error::corrupt(
                "a current correction target and its holder agree",
            ));
        }
        let role = self
            .roles
            .find(
                tx.client(),
                holder.role_id(),
                crate::access::Scope::Institution,
                now,
            )
            .await?
            .ok_or(Error::corrupt(
                "a current correction target retains its institutional role",
            ))?;
        if role.kind() != role::Kind::Full {
            return Err(Error::Rejected(Rejection::Target));
        }
        let before = revision::fetch(tx.client(), self.app.keys(), subject.revision_id())
            .await
            .map_err(correction::Error::from)?;
        if before.person_id() != self.input.target.person_id
            || before.revision_id() != subject.revision_id()
        {
            return Err(Error::corrupt(
                "a correction target revision retains its authenticated values",
            ));
        }
        let person = person::state::find(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            Read::Locked,
            self.input.target.person_id,
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "a correction target retains its permanent person state",
        ))?;
        if person.revision_id() != before.revision_id() {
            return Err(Error::corrupt(
                "a correction target retains its current person revision",
            ));
        }
        if self
            .accounts
            .number_occupied(
                tx.client(),
                &self.input.id_number,
                self.input.target.person_id,
                now,
            )
            .await?
        {
            return Err(Error::Rejected(Rejection::IdNumberTaken));
        }
        Ok(Facts {
            target,
            account,
            person,
            before,
            holder,
            operator_id: authority.revision_id,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, Error> {
        if facts.before.name().as_ref() == self.input.name.as_ref()
            && facts.before.id_number().as_ref() == self.input.id_number.as_ref()
        {
            return Err(Error::Rejected(Rejection::Unchanged));
        }
        Ok(Selected {
            id: self.id,
            corrected_at: now,
            target: facts.target,
            account: facts.account,
            person: facts.person,
            before: facts.before,
            holder: facts.holder,
            operator_id: facts.operator_id,
            operation_id: facts.operation_id,
            name: self.input.name.clone(),
            id_number: self.input.id_number.clone(),
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<Pending>,
    ) -> Result<Correction, Error> {
        if selected.target != selected.account.id() {
            return Err(Error::corrupt(
                "a correction retains its locked target account",
            ));
        }
        let correction = correction::write(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            &self.keys.number,
            &selected.person,
            &selected.before,
            Some(&selected.account),
            pending,
            correction::New {
                id: selected.id,
                person_id: self.input.target.person_id,
                operator_id: selected.operator_id,
                appointment_id: Some(selected.holder.assignment_id()),
                name: &selected.name,
                id_number: &selected.id_number,
                operation_id: selected.operation_id,
                corrected_at: selected.corrected_at,
            },
        )
        .await
        .map_err(write_error)?;
        Ok(correction)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Correction, Error> {
        let original =
            correction::find_by_operation(tx.client(), self.app.keys(), tx.operation_id(), now)
                .await?
                .ok_or(Error::corrupt(
                    "a system completion retains its original correction",
                ))?;
        if original.source() != Source::System
            || !original.matches_system(
                self.input.target.person_id,
                self.input.target.revision_id,
                request.person_id(),
                request.revision_id(),
                &self.input.name,
                &self.input.id_number,
            )
        {
            return Err(Error::corrupt(
                "a system correction retains its original permanent facts",
            ));
        }
        Ok(original.into_correction())
    }

    async fn absent(&self, tx: &Tx) -> Result<(), Error> {
        correction::absent(tx, &self.app, Source::System).await
    }
}

impl Write {
    fn locked(&self) -> Result<Locked, Error> {
        self.locked
            .lock()
            .map_err(|_| Error::corrupt("system correction retains its lock identity"))?
            .ok_or_else(|| Error::corrupt("system correction resolves its locks"))
    }
}

fn crypto(error: platform::cryptography::Error) -> Error {
    Error::Unavailable(Fault::new(Stage::Lock, FaultKind::Crypto(error)))
}

fn write_error(error: correction::Error) -> Error {
    match error {
        correction::Error::IdNumberTaken => Error::Rejected(Rejection::IdNumberTaken),
        error => error.into(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::time::timeout;
    use uuid::Uuid;

    use super::{Error, Input, Rejection};
    use crate::{
        access::{
            appointment,
            appointment::tests::{register, register_numbered},
            role::tests::{actor, start},
        },
        identity::{
            account,
            authentication::{self, Database, Supplier, server},
            correction, current, operation,
            person::{self, number::IdNumber, revision},
        },
        pagination::Pagination,
        startup::Administration,
    };

    fn input(
        operator: &str,
        person: person::Id,
        revision: revision::Id,
        name: &str,
        number: &str,
    ) -> Input {
        serde_json::from_value(json!({
            "operator": {"mobile": operator},
            "target": {"personId": person, "revisionId": revision},
            "name": name,
            "idNumber": number,
        }))
        .expect("system correction input")
    }

    fn operation() -> operation::Id {
        Uuid::new_v4().to_string().parse().expect("operation")
    }

    #[test]
    fn system_correction_recovers_real_commit_faults_and_cancelled_waiters() {
        use fixture::database_proxy::{Fault, Proxy};

        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "late", "unknown", "cancelled"] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let (person, revision, _) = facts(&database, &manager).await;
                    services.current().read(&manager).await.expect("warm shared account anchor");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let unrelated = database.connections(&database.user).recover().await.expect("unrelated session");
                    unrelated.client().batch_execute("BEGIN; SELECT pg_advisory_xact_lock(900123, 1)").await.expect("unrelated idle transaction owns a distinct lock");
                    let original = operation();
                    let keys = Arc::new(authentication::inventory(authentication::keys()).identity);
                    let command = || super::run(
                        services.app().clone(), Arc::clone(&keys), original,
                        input("13900000000", person, revision, "Recovered Administrator", "000000200002290029"), false,
                    );
                    proxy.arm(match case {
                        "lost" => Fault::LostCommit,
                        "rollback" => Fault::RolledBack,
                        "unknown" => Fault::UnknownCommit,
                        _ => Fault::DelayedCommit,
                    });
                    let waiter = tokio::spawn(command());
                    let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("actual business COMMIT interception");
                    if matches!(case, "late" | "cancelled") {
                        timeout(Duration::from_secs(5), async {
                            loop {
                                let waiting: bool = observer.client().query_one(
                                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks waiting
                                     JOIN pg_catalog.pg_locks original ON original.locktype=waiting.locktype
                                         AND original.database=waiting.database AND original.classid=waiting.classid
                                         AND original.objid=waiting.objid AND original.objsubid=waiting.objsubid
                                     WHERE original.pid=$1 AND original.granted AND NOT waiting.granted
                                         AND waiting.pid<>original.pid AND waiting.locktype='advisory'
                                         AND waiting.objsubid=1 AND original.pid=ANY(pg_blocking_pids(waiting.pid)))",
                                    &[&backend],
                                ).await.expect("exact original backend lock graph").get(0);
                                if waiting { break; }
                                tokio::task::yield_now().await;
                            }
                        }).await.expect("recovery queues on the original operation");
                        assert!(!waiter.is_finished(), "late COMMIT cannot be inferred absent");
                    }
                    if case == "cancelled" {
                        waiter.abort();
                        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        proxy.release();
                    } else {
                        if matches!(case, "late" | "unknown") { proxy.release(); }
                        let result = waiter.await.expect("command task");
                        match case {
                            "rollback" => assert!(matches!(result, Err(Error::Unavailable(_)))),
                            "unknown" => assert!(matches!(result, Err(Error::Unknown(_)))),
                            _ => { result.expect("original recovered completion").expect("not a preview"); }
                        }
                        if case == "lost" { proxy.release(); }
                    }
                    timeout(Duration::from_secs(5), async {
                        while observer.client().query_one(
                            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)", &[&backend],
                        ).await.expect("original backend termination").get::<_, bool>(0) {
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("original operation backend ended before replay");
                    let row = observer.client().query_one(
                        "SELECT (SELECT count(*) FROM identity.corrections WHERE operation_id=$1),
                                (SELECT count(*) FROM identity.completions WHERE operation_id=$1 AND family='system_correction'),
                                (SELECT count(*) FROM identity.projections WHERE operation_id=$1)", &[&original],
                    ).await.expect("independent effects before any retry");
                    let expected = if case == "rollback" { 0 } else { 1 };
                    for column in 0..3 { assert_eq!(row.get::<_, i64>(column), expected, "{case} atomic effects"); }
                    unrelated.client().batch_execute("SELECT 1; ROLLBACK").await.expect("recovery preserves unrelated backend");
                    unrelated.terminate().await.expect("end unrelated transaction");
                    if case != "rollback" {
                        let current = services.current().read(&manager).await.expect("shared pending account settles after original completion");
                        assert_eq!(current.name().as_ref(), "Recovered Administrator");
                    }
                    let completed = command().await.expect("same input remains recoverable").expect("completion");
                    let row = observer.client().query_one(
                        "SELECT correction.id, correction.operator_id, completion.created_at=correction.corrected_at,
                                person.revision_id=correction.id
                         FROM identity.corrections correction JOIN identity.completions completion
                            ON completion.operation_id=correction.operation_id AND completion.family='system_correction'
                         JOIN identity.persons person ON person.id=correction.person_id WHERE correction.operation_id=$1", &[&original],
                    ).await.expect("one permanent correction after retry");
                    assert_eq!(row.get::<_, correction::Id>(0), completed.id());
                    assert_eq!(row.get::<_, revision::Id>(1), revision);
                    assert!(row.get::<_, bool>(2) && row.get::<_, bool>(3));
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("accepted work drained");
                    context.stop().await.expect("instance ended");
                    supplier.stop().await; proxy.stop().await;
                })).await.expect("bounded system correction recovery scenario");
            }
        }));
    }

    #[test]
    fn sources_survive_role_changes_and_hidden_history_damage_is_rejected() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            for (route, page) in [("self", 1), ("self", 3), ("management", 1), ("management", 3)] {
                timeout(Duration::from_secs(40), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let original = operation();
                    services.revocation().revoke(manager, original, crate::identity::revocation::Scope::Current)
                        .await.expect("another identity family uses the same UUID");
                    let manager = actor(&database, &services, &mut supplier).await;
                    let operator = register(&database, &services, &mut supplier).await;
                    let (person, revision, account) = facts(&database, &manager).await;
                    let (operator_person, operator_revision, operator_account) = facts(&database, &operator).await;
                    let keys = Arc::new(authentication::inventory(authentication::keys()).identity);
                    let system = super::run(
                        services.app().clone(), Arc::clone(&keys), original,
                        input("13800000000", person, revision, "System Administrator", "000000200002290010"), false,
                    ).await.expect("unrelated family UUID does not collide").expect("system completion");
                    let observer = database.connections(&database.user).recover().await.expect("independent facts");
                    assert_eq!(observer.client().query_one(
                        "SELECT count(*) FROM identity.completions WHERE operation_id=$1", &[&original],
                    ).await.expect("separate family completions").get::<_, i64>(0), 2);
                    let role = Box::pin(services.role().create(manager.clone(), operation(),
                        serde_json::from_value(json!({"name":"Former administrator", "level":2, "permissions":[]})).expect("ordinary role"),
                    )).await.expect("ordinary institutional role");
                    for (mobile, target, confirmed, selection) in [
                        ("13900000000", operator_account, operator_revision, json!({"action":"grant"})),
                        ("13800000000", account, system.id().revision_id(), json!({"action":"revoke", "roleId":role.id()})),
                    ] {
                        crate::access::appoint::run(services.app().clone(), Arc::clone(&keys), operation(),
                            serde_json::from_value(json!({"operator":{"mobile":mobile}, "accountId":target, "revisionId":confirmed, "selection":selection})).expect("system appointment input"), false,
                        ).await.expect("real role replacement");
                    }
                    if route == "management" {
                        services.cancellation().cancel(manager.clone(), operation(),
                            serde_json::from_value(json!({"accountId":account, "personId":person, "revisionId":system.id().revision_id()})).expect("current self cancellation"),
                        ).await.expect("former administrator cancellation retains system lineage");
                    }
                    let candidate = services.correction().candidate_by_person(&operator, person).await.expect("former administrator is manageable with or without a current account");
                    let online_operation = operation();
                    let online_input = || serde_json::from_value(json!({"target":candidate.target(), "name":"Online Administrator", "idNumber":"000000200002290010"})).expect("online correction");
                    let online = Box::pin(services.correction().correct(operator.clone(), online_operation, online_input())).await.expect("online correction after system role ends");
                    assert!(matches!(Box::pin(services.correction().correct(operator.clone(), original, online_input())).await, Err(crate::kernel::Error::Mismatch)), "system source cannot be replayed as online correction");
                    assert!(matches!(super::run(services.app().clone(), Arc::clone(&keys), online_operation,
                        input("13800000000", person, system.id().revision_id(), "Online Administrator", "000000200002290010"), false,
                    ).await, Err(Error::Mismatch)), "authenticated online source cannot be replayed as system correction");
                    let history = services.correction().history_by_person(&operator, person, Pagination::new(1, 20).expect("history page")).await.expect("mixed source history");
                    assert_eq!(history.count(), 2);
                    assert_eq!(history.corrections()[0].id(), online.correction().id());
                    assert_eq!(history.corrections()[1].id(), system.id());
                    assert_eq!(history.corrections()[1].after().id_number().as_ref(), "000000200002290010");
                    assert_eq!(history.corrections()[1].operator().person_id(), operator_person);
                    assert_eq!(history.corrections()[1].operator().name().as_ref(), "Target Person");
                    let renamed = super::run(services.app().clone(), Arc::clone(&keys), operation(),
                        input("13800000000", operator_person, operator_revision, "Operator Later", "00000020000229007X"), false,
                    ).await.expect("operator later name").expect("operator completion");
                    assert_ne!(renamed.id().revision_id(), operator_revision);
                    let history = services.correction().history_by_person(&operator, person, Pagination::new(1, 20).expect("history page")).await.expect("original operator retained");
                    assert_eq!(history.corrections()[1].operator().name().as_ref(), "Target Person");
                    assert_eq!(super::run(services.app().clone(), Arc::clone(&keys), original,
                        input("13800000000", person, revision, "System Administrator", "000000200002290010"), false,
                    ).await.expect("original survives target role and operator rename").expect("original completion").id(), system.id());

                    // Corrupt only an older system source. Page 1 contains the healthy online
                    // correction; page 3 is empty. Both still disclose the authenticated count.
                    let owner = database.connections("postgres").recover().await.expect("isolated corruption owner");
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicit fixture damage");
                    owner.client().execute("DELETE FROM identity.completions WHERE family='system_correction' AND operation_id=$1", &[&original]).await.expect("remove older source completion");
                    owner.client().batch_execute("COMMIT").await.expect("damage committed");
                    let pagination = Pagination::new(page, 1).expect("off-page or empty history");
                    let failed = if route == "self" {
                        services.correction().history(&manager, pagination).await.map(|_| ())
                    } else {
                        services.correction().history_by_person(&operator, person, pagination).await.map(|_| ())
                    };
                    assert!(matches!(failed, Err(crate::correction::Error::Fact(_))), "hidden source damage fails the complete history");
                    assert!(services.app().registry().ready().is_err(), "source corruption closes admission");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.corrections WHERE person_id=$1", &[&person]).await.expect("failed read preserves facts").get::<_, i64>(0), 2);
                    owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("work drained"); context.stop().await.expect("instance ended"); supplier.stop().await;
                })).await.expect("bounded mixed source scenario");
            }
        }));
    }

    fn numbered(index: u16) -> IdNumber {
        "0123456789X"
            .chars()
            .find_map(|suffix| format!("00000020000229{index:03}{suffix}").parse().ok())
            .expect("valid fixture identity number")
    }

    #[test]
    fn runtime_cannot_commit_a_system_correction_without_its_completion() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(20), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let (person, revision, _) = facts(&database, &manager).await;
                let writer = database.connections(&database.user).recover().await.expect("ordinary runtime writer");
                let before = revision::fetch(writer.client(), services.app().keys(), revision).await.expect("actual initial profile");
                let appointment: appointment::Id = writer.client().query_one(
                    "SELECT assignment_id FROM access.holders WHERE person_id=$1", &[&person],
                ).await.expect("fixed current appointment").get(0);
                let id = correction::Id::allocate();
                let original = operation();
                writer.client().batch_execute("BEGIN").await.expect("isolated runtime transaction");
                let now = context.sample().await.expect("trusted direct-write time after transaction start");
                let keys = authentication::inventory(authentication::keys());
                let state = crate::identity::person::state::find(
                    writer.client(),
                    services.app().keys(),
                    services.app().anchors(),
                    crate::history::Read::Locked,
                    person,
                    now,
                ).await.expect("person state").expect("permanent person state");
                let mut pending = None;
                correction::write(writer.client(), services.app().keys(), services.app().anchors(), &keys.identity.number, &state, &before, None, &mut pending,
                    correction::New {
                        id, person_id:person, operator_id:revision, appointment_id:Some(appointment),
                        name:&"Incomplete Administrator".parse().expect("name"), id_number:before.id_number(),
                        operation_id:original, corrected_at:now,
                    },
                ).await.expect("complete name-only correction facts before deferred completion check");
                let error = writer.client().batch_execute("COMMIT").await.expect_err("system completion is mandatory in the same transaction");
                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                pending.expect("registered correction candidate").rolled_back().expect("confirmed failed commit");
                let row = writer.client().query_one(
                    "SELECT (SELECT count(*) FROM identity.corrections WHERE operation_id=$1),
                            (SELECT count(*) FROM identity.revisions WHERE id=$2),
                            (SELECT revision_id=$3 AND sequence=1 FROM identity.persons WHERE id=$4)",
                    &[&original, &id.revision_id(), &revision, &person],
                ).await.expect("failed commit rolls back all business facts");
                assert_eq!(row.get::<_, i64>(0), 0); assert_eq!(row.get::<_, i64>(1), 0); assert!(row.get::<_, bool>(2));
                writer.terminate().await.expect("writer ended");
                services.close(); services.drain().await.expect("work drained"); context.stop().await.expect("instance ended"); supplier.stop().await;
            })).await.expect("bounded deferred system pairing");
        }));
    }

    #[test]
    fn operator_mobile_changes_and_suspension_preserve_original_attribution() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(40), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let mut sms = fixture::supplier::Supplier::start().await;
                let (context, services) = crate::identity::transfer::tests::start(&database, &supplier, &sms).await;
                let manager = actor(&database, &services, &mut supplier).await;
                let operator = Box::pin(register(&database, &services, &mut supplier)).await;
                let (person, revision, _) = facts(&database, &manager).await;
                let (operator_person, operator_revision, operator_account) = facts(&database, &operator).await;
                let keys = Arc::new(authentication::inventory(authentication::keys()).identity);
                let original = operation();
                let corrected = super::run(services.app().clone(), Arc::clone(&keys), original,
                    input("13800000000", person, revision, "Mobile Attribution", "000000200002290010"), false,
                ).await.expect("normal operator correction").expect("completion");
                let transfer = services.account().create_transfer(manager.clone(), operation(), operator_account,
                    serde_json::from_value(json!({"personId":operator_person,"revisionId":operator_revision,"mobile":"13800000000","verified":true})).expect("verified assisted transfer"),
                ).await.expect("actual transfer establishment");
                let (_, message, code) = crate::identity::transfer::tests::bind_to(
                    &database, &services, &manager, transfer.id(), &mut supplier, &mut sms, "13600000000",
                ).await;
                services.transfer().complete_assisted(manager.clone(), operator_account,
                    crate::identity::transfer::completion::Input::new(operation(), "13600000000".parse().expect("new mobile"), message.id(), code.parse().expect("fixture code")),
                ).await.expect("actual operator mobile replacement");
                let observer = database.connections(&database.user).recover().await.expect("independent mobile observer");
                let hash = keys.mobile.hash(&"13600000000".parse().expect("new mobile")).expect("mobile hash");
                assert!(observer.client().query_one("SELECT (mobile).hash=$2 FROM identity.accounts WHERE id=$1", &[&operator_account, &hash]).await.expect("new current mobile before replay").get::<_, bool>(0));
                assert_eq!(super::run(services.app().clone(), Arc::clone(&keys), original,
                    input("13800000000", person, revision, "Mobile Attribution", "000000200002290010"), false,
                ).await.expect("old attribution remains replayable").expect("original completion").id(), corrected.id());
                assert!(matches!(super::run(services.app().clone(), Arc::clone(&keys), original,
                    input("13600000000", person, revision, "Mobile Attribution", "000000200002290010"), false,
                ).await, Err(Error::Mismatch)));
                assert!(matches!(super::run(services.app().clone(), Arc::clone(&keys), operation(),
                    input("13800000000", person, corrected.id().revision_id(), "Stale Mobile", "000000200002290010"), false,
                ).await, Err(Error::Rejected(Rejection::Operator))));
                let current = super::run(services.app().clone(), Arc::clone(&keys), operation(),
                    input("13600000000", person, corrected.id().revision_id(), "Current Mobile", "000000200002290010"), false,
                ).await.expect("new command resolves the current mobile").expect("new completion");
                let suspended = services.suspension().suspend(manager.clone(), operation(), operator_account,
                    serde_json::from_value(json!({"personId":operator_person,"revisionId":operator_revision,"mobile":"13600000000"})).expect("current suspension confirmation"),
                ).await.expect("actual operator suspension");
                assert!(matches!(super::run(services.app().clone(), Arc::clone(&keys), operation(),
                    input("13600000000", person, current.id().revision_id(), "Unavailable Operator", "000000200002290010"), false,
                ).await, Err(Error::Rejected(Rejection::Operator))));
                crate::access::appoint::run(services.app().clone(), Arc::clone(&keys), operation(),
                    serde_json::from_value(json!({"operator":{"mobile":"13900000000"},"accountId":operator_account,"revisionId":operator_revision,"selection":{"action":"grant"}})).expect("grant paused target system role"), false,
                ).await.expect("target suspension does not remove institutional role eligibility");
                super::run(services.app().clone(), Arc::clone(&keys), operation(),
                    input("13900000000", operator_person, operator_revision, "Suspended Administrator", "00000020000229007X"), false,
                ).await.expect("another normal operator can correct the suspended system target");
                assert!(observer.client().query_one("SELECT suspension_id=$2 FROM identity.accounts WHERE id=$1", &[&operator_account, &suspended.id()]).await.expect("correction preserves suspension").get::<_, bool>(0));
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("work drained"); context.stop().await.expect("instance ended"); supplier.stop().await; sms.stop().await;
            })).await.expect("bounded mobile attribution and suspension lifecycle");
        }));
    }

    async fn facts(
        database: &Database,
        actor: &current::Actor,
    ) -> (person::Id, revision::Id, account::Id) {
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("independent actor facts");
        let row = observer
            .client()
            .query_one(
                "SELECT account.person_id, person.revision_id, account.id
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

    async fn start_administration(
        database: &Database,
    ) -> (Administration, crate::runtime::Context) {
        let context = crate::runtime::Context::new(database.connections(&database.user))
            .await
            .expect("maintenance instance");
        let barrier = crate::database::Barrier::begin(context.clone())
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

    #[test]
    fn input_rejects_unknown_and_missing_fields_without_disclosing_secrets() {
        let person = person::Id::allocate();
        let revision = revision::Id::allocate();
        let valid = json!({
            "operator": {"mobile": "13800000000"},
            "target": {"personId": person, "revisionId": revision},
            "name": "Masked Person",
            "idNumber": "000000200002290010",
        });
        assert!(serde_json::from_value::<Input>(valid.clone()).is_ok());
        for invalid in [
            json!({"operator": {"mobile": "13800000000"}, "target": {"personId": person, "revisionId": revision}, "name": "Masked Person", "idNumber": "000000200002290010", "extra": true}),
            json!({"operator": {"mobile": "13800000000", "extra": true}, "target": {"personId": person, "revisionId": revision}, "name": "Masked Person", "idNumber": "000000200002290010"}),
            json!({"operator": {"mobile": "13800000000"}, "target": {"personId": person, "revisionId": revision, "extra": true}, "name": "Masked Person", "idNumber": "000000200002290010"}),
            json!({"operator": {"mobile": "13800000000"}, "target": {"personId": person, "revisionId": revision}, "name": "Masked Person"}),
            json!({"target": {"personId": person, "revisionId": revision}, "name": "Masked Person", "idNumber": "000000200002290010"}),
        ] {
            assert!(serde_json::from_value::<Input>(invalid).is_err());
        }
        let masked = serde_json::from_value::<Input>(valid)
            .expect("valid input")
            .masked();
        assert!(masked.contains("138****0000"));
        assert!(!masked.contains("000000200002290010"));
        assert!(!masked.contains("Masked Person"));
    }

    #[test]
    fn system_correction_previews_commits_replays_and_rejects_changed_input() {
        let server = server();
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Runtime::new()
                    .expect("runtime")
                    .block_on(Box::pin(async move {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let operator = register(&database, &services, &mut supplier).await;
                    let occupied =
                        register_numbered(&database, &services, &mut supplier, 2).await;
                    let (target_person, target_revision, _) = facts(&database, &manager).await;
                    let (operator_person, operator_revision, operator_account) =
                        facts(&database, &operator).await;
                    facts(&database, &occupied).await;
                    services.close();
                    services.drain().await.expect("online work drained");
                    context
                        .stop()
                        .await
                        .expect("online instance released for maintenance");

                    let (administration, maintenance) = start_administration(&database).await;
                    let original_operation = operation();
                    let name = "System Corrected";
                    let number = "000000200002290010";
                    assert!(
                        administration
                            .correct(
                                original_operation,
                                input(
                                    "13800000000",
                                    target_person,
                                    target_revision,
                                    name,
                                    number
                                ),
                                true,
                            )
                            .await
                            .expect("real dry-run decision")
                            .is_none()
                    );
                    let observer = database
                        .connections(&database.user)
                        .recover()
                        .await
                        .expect("independent facts");
                    let preview = observer
                        .client()
                        .query_one(
                            "SELECT (SELECT sequence AS person_sequence FROM identity.persons WHERE id=$1) AS sequence,
                                (SELECT count(*) FROM identity.corrections) AS corrections,
                                (SELECT count(*) FROM identity.completions WHERE family='system_correction') AS completions",
                            &[&target_person],
                        )
                        .await
                        .expect("preview snapshot");
                    assert_eq!(preview.get::<_, i64>("sequence"), 1);
                    assert_eq!(preview.get::<_, i64>("corrections"), 0);
                    assert_eq!(preview.get::<_, i64>("completions"), 0);

                    let completed = administration
                        .correct(
                            original_operation,
                            input(
                                "13800000000",
                                target_person,
                                target_revision,
                                name,
                                number,
                            ),
                            false,
                        )
                        .await
                        .expect("committed system correction")
                        .expect("not a preview");
                    let correction = completed.id();

                    let root = authentication::inventory(authentication::keys());
                    let digest = root
                        .identity
                        .number
                        .hash(&number.parse::<IdNumber>().expect("canonical number"))
                        .expect("identity number digest");
                    let row = observer
                        .client()
                        .query_one(
                            "SELECT correction.id, correction.operator_id, correction.appointment_id,
                                correction.transaction_id = completion.transaction_id AS same_transaction,
                                completion.subject, completion.family,
                                completion.expires_at,
                                person.revision_id, person.sequence AS person_sequence, account.id_number,
                                holder.assignment_id, completion.created_at
                             FROM identity.corrections correction
                             JOIN identity.completions completion
                                ON completion.operation_id=correction.operation_id
                                AND completion.family='system_correction'
                             JOIN identity.persons person ON person.id=correction.person_id
                             JOIN identity.accounts account ON account.person_id=person.id
                             JOIN access.holders holder ON holder.person_id=person.id
                             WHERE correction.operation_id=$1",
                            &[&original_operation],
                        )
                        .await
                        .expect("one atomic correction, projection and completion");
                    assert_eq!(row.get::<_, correction::Id>("id"), correction);
                    assert_eq!(row.get::<_, revision::Id>("operator_id"), operator_revision);
                    assert_eq!(row.get::<_, String>("subject"), "system");
                    assert_eq!(row.get::<_, String>("family"), "system_correction");
                    assert_eq!(row.get::<_, Option<chrono::DateTime<chrono::Utc>>>("expires_at"), None);
                    assert!(row.get::<_, bool>("same_transaction"));
                    assert_eq!(
                        row.get::<_, revision::Id>("revision_id"),
                        correction.revision_id()
                    );
                    assert_eq!(row.get::<_, i64>("person_sequence"), 2);
                    assert_eq!(row.get::<_, person::number::hash::Hash>("id_number"), digest);
                    assert_eq!(
                        row.get::<_, appointment::Id>("appointment_id"),
                        row.get::<_, appointment::Id>("assignment_id")
                    );
                    assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>("created_at"), completed.corrected_at());

                    assert_eq!(administration.correct(original_operation,
                        input("13800000000", target_person, target_revision, name, number), true,
                    ).await.expect("dry-run replay after independent commit evidence")
                        .expect("matching original reports completion").id(), correction);

                    let current = correction.revision_id();
                    assert!(matches!(
                        administration
                            .correct(
                                operation(),
                                input(
                                    "13800000000",
                                    target_person,
                                    target_revision,
                                    name,
                                    number
                                ),
                                false
                            )
                            .await,
                        Err(Error::Rejected(Rejection::TargetChanged))
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                operation(),
                                input("13800000000", target_person, current, name, number),
                                false
                            )
                            .await,
                        Err(Error::Rejected(Rejection::Unchanged))
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                operation(),
                                input(
                                    "13900000000",
                                    operator_person,
                                    operator_revision,
                                    name,
                                    number
                                ),
                                false
                            )
                            .await,
                        Err(Error::Rejected(Rejection::Target))
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                operation(),
                                input(
                                    "13800000000",
                                    target_person,
                                    current,
                                    name,
                                    numbered(2).as_ref()
                                ),
                                false
                            )
                            .await,
                        Err(Error::Rejected(Rejection::IdNumberTaken))
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                operation(),
                                input(
                                    "13700000000",
                                    target_person,
                                    current,
                                    "Unknown Operator",
                                    "000000200002290029"
                                ),
                                false
                            )
                            .await,
                        Err(Error::Rejected(Rejection::Operator))
                    ));
                    administration.close();
                    administration.drain().await.expect("maintenance work drained");
                    maintenance
                        .stop()
                        .await
                        .expect("maintenance instance released");

                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("system correction survives startup");
                    let history = services
                        .correction()
                        .history(&manager, Pagination::new(1, 100).expect("page"))
                        .await
                        .expect("self history authenticates a system source");
                    assert_eq!(history.count(), 1);
                    assert_eq!(history.corrections()[0].after().name().as_ref(), name);
                    assert_eq!(
                        history.corrections()[0].after().suffix(),
                        &number[number.len() - 4..]
                    );
                    let candidate = services
                        .correction()
                        .candidate_by_person(&manager, operator_person)
                        .await
                        .expect("operator correction candidate");
                    let renamed = Box::pin(services.correction().correct(
                        manager.clone(),
                        operation(),
                        serde_json::from_value(json!({
                            "target": candidate.target(),
                            "name": "Renamed Operator",
                            "idNumber": "00000020000229007X",
                        }))
                        .expect("name-only correction"),
                    ))
                    .await
                    .expect("real operator rename");
                    let management = services
                        .correction()
                        .history_by_person(
                            &manager,
                            operator_person,
                            Pagination::new(1, 100).expect("page"),
                        )
                        .await
                        .expect("management history authenticates its source");
                    assert_eq!(management.count(), 1);
                    assert_eq!(
                        management.corrections()[0].after().name().as_ref(),
                        "Renamed Operator"
                    );
                    services
                        .cancellation()
                        .cancel_account(
                            manager.clone(),
                            operation(),
                            operator_account,
                            serde_json::from_value(json!({
                                "personId": operator_person,
                                "revisionId": renamed.correction().id().revision_id(),
                                "mobile": "13800000000",
                            }))
                            .expect("operator cancellation"),
                        )
                        .await
                        .expect("real operator cancellation");
                    services.close();
                    services.drain().await.expect("later operator changes drained");
                    context
                        .stop()
                        .await
                        .expect("online instance released for replay");

                    let (administration, maintenance) = start_administration(&database).await;
                    assert_eq!(
                        administration
                            .correct(
                                original_operation,
                                input(
                                    "13800000000",
                                    target_person,
                                    target_revision,
                                    name,
                                    number
                                ),
                                false,
                            )
                            .await
                            .expect("system replay after operator rename and cancellation")
                            .expect("retained original")
                            .id(),
                        correction
                    );
                    assert!(matches!(
                        administration
                            .correct(
                                original_operation,
                                input("13900000000", target_person, target_revision, name, number),
                                false
                            )
                            .await,
                        Err(Error::Mismatch)
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                original_operation,
                                input(
                                    "13800000000",
                                    target_person,
                                    target_revision,
                                    "Different Value",
                                    number
                                ),
                                false
                            )
                            .await,
                        Err(Error::Mismatch)
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                original_operation,
                                input(
                                    "13800000000",
                                    Uuid::new_v4().to_string().parse().expect("other person"),
                                    target_revision,
                                    name,
                                    number
                                ),
                                false
                            )
                            .await,
                        Err(Error::Mismatch)
                    ));
                    assert!(matches!(
                        administration
                            .correct(
                                original_operation,
                                input(
                                    "13800000000",
                                    target_person,
                                    Uuid::new_v4().to_string().parse().expect("other revision"),
                                    name,
                                    number
                                ),
                                false
                            )
                            .await,
                        Err(Error::Mismatch)
                    ));
                    administration.close();
                    administration.drain().await.expect("replay work drained");
                    maintenance
                        .stop()
                        .await
                        .expect("replay instance released");

                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("replay left complete history");
                    assert_eq!(
                        services
                            .correction()
                            .history(&manager, Pagination::new(1, 100).expect("page"))
                            .await
                            .expect("history after replay")
                            .count(),
                        1
                    );
                    services.close();
                    services.drain().await.expect("final work drained");
                    context.stop().await.expect("final instance stopped");
                    observer.terminate().await.expect("observer ended");
                    assert!(
                        timeout(Duration::from_millis(30), supplier.request())
                            .await
                            .is_err()
                    );
                    supplier.stop().await;
                }))
                .await
                        .expect("system correction lifecycle deadline");
                    }));
            })
            .expect("test thread")
            .join()
            .expect("test thread result");
    }

    #[test]
    fn missing_or_tampered_system_completion_closes_startup() {
        let server = server();
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(Box::pin(async {
                for case in ["missing", "target", "input", "time", "seal", "family", "appointment", "definition"] {
                    timeout(Duration::from_secs(30), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let (context, services) =
                            start(database.connections(&database.user), &supplier).await;
                        let manager = actor(&database, &services, &mut supplier).await;
                        let operator = register(&database, &services, &mut supplier).await;
                        let (target_person, target_revision, _) = facts(&database, &manager).await;
                        facts(&database, &operator).await;
                        services.close();
                        services.drain().await.expect("online work drained");
                        context
                            .stop()
                            .await
                            .expect("online instance released for maintenance");

                        let (administration, maintenance) = start_administration(&database).await;
                        let original_operation = operation();
                        administration
                            .correct(
                                original_operation,
                                input(
                                    "13800000000",
                                    target_person,
                                    target_revision,
                                    "Tamper Target",
                                    "000000200002290010",
                                ),
                                false,
                            )
                            .await
                            .expect("committed system correction")
                            .expect("not a preview");
                        administration.close();
                        administration.drain().await.expect("maintenance work drained");
                        maintenance
                            .stop()
                            .await
                            .expect("maintenance instance released");

                        let runtime = database
                            .connections(&database.user)
                            .recover()
                            .await
                            .expect("ordinary runtime connection");
                        let denied = runtime
                            .client()
                            .execute(
                                "DELETE FROM identity.completions WHERE operation_id=$1",
                                &[&original_operation],
                            )
                            .await
                            .expect_err("ordinary runtime cannot delete a permanent completion");
                        assert_eq!(
                            denied.code(),
                            Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
                        );
                        runtime.terminate().await.expect("runtime connection ended");

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
                        let mutation = match case {
                            "missing" => "DELETE FROM identity.completions WHERE operation_id=$1",
                            "target" => "UPDATE identity.completions SET target=gen_random_uuid()::text WHERE operation_id=$1",
                            "input" => "UPDATE identity.completions SET input=set_byte(input,octet_length(input)-1,get_byte(input,octet_length(input)-1)#1) WHERE operation_id=$1",
                            "time" => "UPDATE identity.completions SET created_at=created_at+interval '1 second' WHERE operation_id=$1",
                            "seal" => "UPDATE identity.completions SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE operation_id=$1",
                            "family" => "UPDATE identity.completions SET family='logout',action='logout',subject='session' WHERE operation_id=$1",
                            "appointment" => "UPDATE identity.corrections SET appointment_id=NULL WHERE operation_id=$1",
                            "definition" => "UPDATE access.appointments SET change_id=gen_random_uuid() WHERE id=(SELECT appointment_id FROM identity.corrections WHERE operation_id=$1)",
                            _ => unreachable!(),
                        };
                        assert_eq!(owner.client().execute(mutation, &[&original_operation]).await.expect("one exact isolated source mutation"), 1);
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
                            crate::database::Barrier::begin(context.clone())
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
                        assert!(
                            result.is_err(),
                            "{case}: damaged system source closes admission"
                        );
                        context.stop().await.expect("damaged instance stopped");
                        supplier.stop().await;
                    }))
                    .await
                    .expect("system completion tamper deadline");
                }
            }));
    }
}
