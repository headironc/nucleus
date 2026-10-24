//! Institutional replacement uses the shared command pipeline and permanent completion.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::{
        appointment::{Appointment, Id, Service, database},
        capability::Scope,
        holder, role,
    },
    history::{self, Change, Read, State as _, Verified},
    identity::{
        account,
        current::Actor,
        mobile::Mobile,
        operation,
        person::{self, revision},
    },
    kernel::{self, Authorization, Command, completion, operation::Family, tx::Tx},
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = appointment::Target)]
pub struct Target {
    pub(super) account_id: account::Id,
    pub(super) person_id: person::Id,
    pub(super) revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    pub(super) mobile: Mobile,
    pub(super) assignment_id: Id,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = appointment::Input)]
pub struct Input {
    pub(super) target: Target,
    pub(super) role: role::Selection,
}

pub type CommandError = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    TargetChanged,
    RoleChanged,
    Unchanged,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent => kernel::Status::NotFound,
            Self::TargetChanged | Self::RoleChanged | Self::Unchanged => kernel::Status::Conflict,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::RoleChanged => "role_changed",
            Self::Unchanged => "state_conflict",
        }
    }
}

impl From<Rejection> for CommandError {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

fn accepts(action: &str, target: &str) -> bool {
    matches!(action, "assign" | "grant" | "revoke")
        && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
}

pub(in crate::access) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::InstitutionAppointment, accepts);

impl Service {
    pub async fn assign(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Appointment, CommandError> {
        self.app
            .command(
                operation_id,
                Write {
                    service: self.clone(),
                    actor,
                    input,
                    id: Id::allocate(),
                },
            )
            .await
    }
}

struct Write {
    service: Service,
    actor: Actor,
    input: Input,
    id: Id,
}

struct TargetFacts {
    holder: Verified<holder::State>,
    role: role::Role,
    subject: Option<revision::Identity>,
    selected: Option<role::Role>,
}

struct Facts {
    target: Option<TargetFacts>,
    authority: role::Role,
    operator: revision::Identity,
    operation_id: operation::Id,
}

struct Selected {
    change: Change<holder::State>,
    output: Appointment,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Appointment;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&crate::identity::current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        #[derive(Serialize)]
        struct Input<'a> {
            account_id: account::Id,
            person_id: person::Id,
            revision_id: revision::Id,
            mobile: &'a str,
            assignment_id: Id,
            role_id: role::Id,
            change_id: role::change::Id,
        }
        let target = &self.input.target;
        completion::Request::new(
            "assign",
            Uuid::from_bytes(*target.account_id.as_bytes()).to_string(),
            &Input {
                account_id: target.account_id,
                person_id: target.person_id,
                revision_id: target.revision_id,
                mobile: target.mobile.as_ref(),
                assignment_id: target.assignment_id,
                role_id: self.input.role.id,
                change_id: self.input.role.change_id,
            },
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        tx.shared(b"access/scope", b"institution").await?;
        self.actor
            .lock_accounts(tx.client(), self.input.target.account_id)
            .await?;
        self.actor.lock_session(tx.client()).await?;
        role::database::lock_shared(tx.client(), self.input.role.id, Scope::Institution).await?;
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<crate::identity::current::database::State>,
    ) -> Result<Authorization, CommandError> {
        let subject = subject.ok_or_else(|| {
            CommandError::corrupt("an appointment command has a qualified session")
        })?;
        let (_, person_id, revision_id, now) = crate::access::authorize_subject(
            tx.client(),
            &self.service.roles,
            subject,
            role::Code::InstitutionAssignments,
        )
        .await?;
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }

    async fn load(&self, tx: &Tx, authorization: &Authorization) -> Result<Facts, CommandError> {
        let client = tx.client();
        let now = authorization.now;
        let authority =
            database::find_by_account(client, &self.service.roles, self.actor.account_id(), now)
                .await?
                .ok_or(CommandError::corrupt(
                    "an authorized account retains its holder",
                ))?
                .into_role();
        let original = history::find::<holder::State>(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Locked,
            self.input.target.account_id,
            now,
        )
        .await?;
        let mut target = None;
        if let Some(original) = original.filter(|holder| holder.active()) {
            let role = self
                .service
                .roles
                .find(client, original.role_id(), Scope::Institution, now)
                .await?
                .ok_or(CommandError::corrupt(
                    "a live holder retains its active role",
                ))?;
            // Hidden targets need no identity decryption or selected-role lookup.
            let visible =
                original.person_id() != authorization.person_id && role.level() < authority.level();
            let subject = if visible {
                self.service
                    .accounts
                    .find_by_id(
                        client,
                        self.input.target.account_id,
                        &self.input.target.mobile,
                        now,
                    )
                    .await?
            } else {
                None
            };
            let confirmed = subject.as_ref().is_some_and(|subject| {
                original.person_id() == self.input.target.person_id
                    && subject.person_id() == original.person_id()
                    && subject.revision_id() == self.input.target.revision_id
                    && original.assignment_id() == self.input.target.assignment_id
            });
            let (subject, selected) = if confirmed {
                (
                    Some(
                        self.service
                            .revisions
                            .fetch_identity(client, self.input.target.revision_id)
                            .await?,
                    ),
                    self.service
                        .roles
                        .find(client, self.input.role.id, Scope::Institution, now)
                        .await?,
                )
            } else {
                (None, None)
            };
            target = Some(TargetFacts {
                holder: original,
                role,
                subject,
                selected,
            });
        }
        let operator = self
            .service
            .revisions
            .fetch_identity(client, authorization.revision_id)
            .await?;
        Ok(Facts {
            target,
            authority,
            operator,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        let target = facts.target.ok_or(Rejection::Absent)?;
        if target.holder.person_id() == facts.operator.person_id()
            || target.role.level() >= facts.authority.level()
        {
            return Err(Rejection::Absent.into());
        }
        let subject = target.subject.ok_or(Rejection::TargetChanged)?;
        if target.holder.person_id() != self.input.target.person_id
            || subject.person_id() != self.input.target.person_id
            || target.holder.assignment_id() != self.input.target.assignment_id
        {
            return Err(Rejection::TargetChanged.into());
        }
        let selected = target
            .selected
            .filter(|role| role.change_id() == self.input.role.change_id)
            .ok_or(Rejection::RoleChanged)?;
        if selected.level() >= facts.authority.level()
            || selected
                .permissions()
                .iter()
                .any(|code| !facts.authority.permissions().contains(code))
        {
            return Err(CommandError::Denied);
        }
        if selected.id() == target.role.id() {
            return Err(Rejection::Unchanged.into());
        }
        let change = holder::replace(
            &target.holder,
            &selected,
            holder::Assignment {
                id: self.id,
                previous_id: Some(self.input.target.assignment_id),
                change_id: selected.change_id(),
                operator_id: facts.operator.revision_id(),
                revision_id: subject.revision_id(),
                source: holder::Source::Session,
            },
            facts.operation_id,
            now,
        )?;
        Ok(Selected {
            change,
            output: Appointment {
                id: self.id,
                subject,
                operator: facts.operator,
                role: selected,
                sequence: target.holder.sequence() + 1,
                assigned_at: now,
                operation_id: facts.operation_id,
            },
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Appointment, CommandError> {
        *pending = Some(
            selected
                .change
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await?,
        );
        Ok(selected.output)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Appointment, CommandError> {
        let record = database::original(tx.client(), &self.service.roles, tx.operation_id(), now)
            .await?
            .ok_or(CommandError::corrupt(
                "an appointment completion retains its original record",
            ))?;
        let assignment = record.assignment().ok_or(CommandError::corrupt(
            "an appointment completion refers to an assignment",
        ))?;
        if record.holder_id() != self.input.target.account_id
            || record.person_id() != self.input.target.person_id
            || record.role_id() != self.input.role.id
            || assignment.previous_id != Some(self.input.target.assignment_id)
            || assignment.change_id != self.input.role.change_id
            || assignment.revision_id != self.input.target.revision_id
            || assignment.source != holder::Source::Session
        {
            return Err(CommandError::corrupt(
                "the original appointment agrees with its completed input",
            ));
        }
        let output = database::deliver(
            tx.client(),
            &self.service.roles,
            &record,
            &self.service.revisions,
            now,
        )
        .await?;
        if output.operator.person_id() != request.person_id() {
            return Err(CommandError::corrupt(
                "the original appointment retains its completed operator",
            ));
        }
        self.service
            .app
            .anchors()
            .confirm::<holder::State>(&record)?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::{sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use serde_json::json;
    use tokio::time::timeout;
    use uuid::Uuid;

    use crate::{
        access::{
            appointment::{
                CommandError as Error,
                tests::{command, register},
            },
            role::tests::{actor, start},
        },
        identity::authentication::{self, Database, Supplier, server},
        kernel::error::{FaultKind, Stage},
    };

    #[test]
    fn accepted_appointments_recover_after_session_revocation() {
        let server = server();
        tokio::runtime::Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(30), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let proxy = Proxy::start(&server).await;
                let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                let manager = actor(&database, &services, &mut supplier).await;
                register(&database, &services, &mut supplier).await;
                let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("setup operation"), serde_json::from_value(json!({"name":"Recovered appointment","level":20,"permissions":[]})).expect("role"))).await.expect("actual role");
                let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("target confirmation");
                let operation = Uuid::new_v4().to_string().parse().expect("operation");
                let result = Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, services.appointment().assign(manager.clone(), operation, command(&candidate, &selected)))).await;
                let observer = database.connections(&database.user).recover().await.expect("independent facts");
                let row = observer.client().query_one("SELECT appointment.id,appointment.role_id,appointment.sequence,person.sequence AS person_sequence FROM access.appointments appointment JOIN identity.persons person ON person.id=appointment.person_id JOIN access.holders holder ON holder.assignment_id=appointment.id WHERE appointment.operation_id=$1 AND appointment.person_id=$2", &[&operation,&candidate.identity().person_id()]).await.expect("one original appointment is the current holder");
                assert_eq!(row.get::<_,crate::access::role::Id>("role_id"), selected.id());
                assert_eq!((row.get::<_,i64>("sequence"), row.get::<_,i64>("person_sequence")), (2,1));
                if let Ok(original) = &result { assert_eq!(original.id(), row.get("id")); }
                let Err(error) = Box::pin(services.appointment().assign(manager, operation, command(&candidate, &selected))).await else { panic!("new request must be refused"); };
                assert!(matches!(error, Error::Unauthenticated));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE operation_id=$1", &[&operation]).await.expect("no replay writes").get::<_,i64>(0), 1);
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.ready().await.expect("history survives restart");
                services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                observer.terminate().await.expect("observer ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                assert!(result.is_ok(), "accepted appointment lost its result: {:?}", result.as_ref().err());
            })).await.expect("appointment recovery after session loss deadline");
        }));
    }

    #[test]
    #[tracing_test::traced_test]
    fn recovery_keeps_original_appointment_and_cleanup_without_repeating_effects() {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostReadCommit, Fault::StalledReadCommit, Fault::ReadCommitTermination, Fault::StalledAppointmentOriginal, Fault::RolledBack] {
                timeout(Duration::from_secs(25), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    register(&database, &services, &mut supplier).await;
                    let selected = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("create"), serde_json::from_value(json!({"name":"Recovery selected","level":20,"permissions":[]})).expect("definition"))).await.expect("role");
                    let candidate = services.appointment().candidate(&manager, "13800000000".parse().expect("mobile")).await.expect("confirmation");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let operation = Uuid::nil().to_string().parse().expect("fixed appointment operation");
                    let rolled_back = matches!(fault, Fault::RolledBack);
                    let cancelled = matches!(fault, Fault::StalledReadCommit);
                    let incomplete = matches!(fault, Fault::StalledAppointmentOriginal);
                    proxy.arm(if rolled_back { Fault::RolledBack } else { Fault::DelayedCommit });
                    let service = services.appointment().clone(); let current = manager.clone(); let input = command(&candidate, &selected);
                    let mut task = tokio::spawn(async move { service.assign(current, operation, input).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("appointment INSERT consumer reached COMMIT");
                    let mut backends = Vec::<i32>::new();
                    if !rolled_back {
                        backends = timeout(Duration::from_secs(3), async { loop {
                            if let Some(row) = observer.client().query_opt("SELECT holder.pid, waiter.pid FROM pg_catalog.pg_locks waiter JOIN pg_catalog.pg_locks holder USING(locktype,database,classid,objid,objsubid) WHERE waiter.locktype='advisory' AND NOT waiter.granted AND holder.granted AND waiter.mode='ExclusiveLock' AND holder.mode='ExclusiveLock' AND waiter.classid=2289506566::oid AND waiter.objid=3145120939::oid AND waiter.objsubid=1 AND waiter.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND holder.pid=ANY(pg_catalog.pg_blocking_pids(waiter.pid))", &[]).await.expect("independent LP32/SHA256 appointment operation key") { break vec![row.get(0),row.get(1)]; }
                            tokio::task::yield_now().await;
                        }}).await.expect("recovery waiting on the actual original backend");
                        assert_eq!(observer.client().query_one("SELECT count(*) FROM access.appointments WHERE person_id=$1", &[&candidate.identity().person_id()]).await.expect("pending write invisible").get::<_,i64>(0), 1);
                        proxy.arm(fault); proxy.release();
                        timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("actual readback cleanup or required read intercepted");
                    }
                    if cancelled {
                        task.abort(); assert!(task.await.err().expect("waiter cancelled").is_cancelled());
                        services.close();
                        assert!(timeout(Duration::from_millis(30), services.drain()).await.is_err(), "owned work outlives its HTTP-style waiter");
                        timeout(Duration::from_secs(13), async { loop { tokio::select! {
                            result=services.drain()=>{result.expect("known result cleanup drained"); break;},
                            request=timeout(Duration::from_secs(4),supplier.request())=>assert!(request.is_err(),"no supplier during drain"),
                        }}}).await.expect("original absolute cleanup budget");
                    } else {
                        let result = timeout(Duration::from_secs(13), async { loop { tokio::select! {
                            result=&mut task=>break result.expect("accepted appointment work"),
                            request=timeout(Duration::from_secs(4),supplier.request())=>assert!(request.is_err(),"no supplier during recovery"),
                        }}}).await.expect("bounded recovery");
                        if rolled_back { assert!(matches!(result, Err(Error::Unavailable(_))), "{:?}", result.as_ref().err()); }
                        else if incomplete {
                            let Err(Error::Unknown(fault)) = result else { panic!("necessary read cannot become known"); };
                            assert!(fault.later().any(|(stage, kind)| stage == Stage::Recovery && matches!(kind, FaultKind::Timeout(_))), "{fault:?}");
                            services.appointment().registry.ready().expect("ordinary incomplete query affects only the original operation");
                        } else { assert!(result.is_ok(), "{:?}", result.as_ref().err()); }
                    }
                    let row = observer.client().query_one("SELECT person.sequence AS person_sequence, holder.assignment_id, holder.role_id, holder.sequence, holder.version, (SELECT count(*) FROM access.appointments WHERE person_id=person.id) AS count, (SELECT octet_length(seal) FROM access.appointments WHERE id=holder.assignment_id) AS size, (SELECT count(*) FROM access.completions WHERE family='institution_appointment' AND operation_id=$2) AS completions FROM identity.persons person JOIN access.holders holder ON holder.person_id=person.id WHERE person.id=$1", &[&candidate.identity().person_id(), &operation]).await.expect("independent atomic graph before any replay");
                    assert_eq!(row.get::<_,i64>("person_sequence"), 1);
                    assert_eq!(row.get::<_,i64>("sequence"), if rolled_back {1}else{2});
                    assert_eq!(row.get::<_,i64>("version"), if rolled_back {1}else{2});
                    assert_eq!(row.get::<_,i64>("count"), if rolled_back {1}else{2});
                    assert_eq!(row.get::<_,crate::access::role::Id>("role_id"), if rolled_back {candidate.role().id()}else{selected.id()});
                    assert_eq!(row.get::<_,i32>("size"), 32);
                    assert_eq!(row.get::<_,i64>("completions"), if rolled_back {0}else{1});
                    proxy.release();
                    timeout(Duration::from_secs(3), async { loop {
                        if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=ANY($1::int[]))", &[&backends]).await.expect("actual backend termination").get::<_,bool>(0) {break;}
                        tokio::task::yield_now().await;
                    }}).await.expect("original and recovery backends ended");
                    if !cancelled && !rolled_back {
                        proxy.arm(Fault::LostReadCommit);
                        let replay=Box::pin(services.appointment().assign(manager,operation,command(&candidate,&selected))).await.expect("first complete original read survives lost COMMIT response");
                        assert_eq!(replay.id(),row.get("assignment_id")); assert_eq!(replay.role().change_id(),selected.change_id());
                    }
                    services.close();services.drain().await.expect("drained");observer.terminate().await.expect("observer ended");context.stop().await.expect("context ended");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err(),"monitor through replay and drain");supplier.stop().await;proxy.stop().await;
                })).await.expect("recovery case bound");
            }
        }));
        logs_assert(|lines| {
            let events = lines
                .iter()
                .filter(|line| {
                    line.contains("completion_recovery") || line.contains("completion_replay")
                })
                .collect::<Vec<_>>();
            assert!(
                !events.is_empty(),
                "known outcomes retain cleanup diagnostics"
            );
            assert!(
                events.iter().any(|line| line.contains("connection closed")),
                "real Query cause"
            );
            assert!(
                events.iter().any(|line| line.contains("Timeout(Elapsed")),
                "real cleanup deadline"
            );
            assert!(
                events
                    .iter()
                    .any(|line| line.contains("database driver connection failed")),
                "real driver cause"
            );
            for event in events {
                for secret in [
                    "13800000000",
                    "13900000000",
                    "Recovery selected",
                    "Target Person",
                    "00000020000229007X",
                    "fictional detail",
                ] {
                    assert!(!event.contains(secret), "safe final diagnostic");
                }
            }
            Ok(())
        });
    }
}
