//! The creator permanently withdraws a live invitation through the shared executor.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{Expiry, Id, command, database, record, state, transition};
use crate::{
    access::grant::{CommandError, Error, Rejection, Service},
    history::{self, Change},
    identity::{
        current::{self, Actor},
        operation,
        person::revision,
    },
    kernel::{Authorization, Command, completion, operation::Family, tx::Tx},
};

pub(super) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::InvitationWithdrawal, |action, target| {
        action == "withdraw" && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
    });

/// A withdrawal shares its invitation's ID and is never reversed.
pub struct Withdrawal {
    id: Id,
    withdrawn_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl Withdrawal {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn withdrawn_at(&self) -> DateTime<Utc> {
        self.withdrawn_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

impl Service {
    pub async fn withdraw_invitation(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
    ) -> Result<Withdrawal, Error> {
        Ok(self
            .app
            .command(
                operation_id,
                Withdraw {
                    service: self.clone(),
                    actor,
                    id,
                },
            )
            .await?)
    }
}

struct Withdraw {
    service: Service,
    actor: Actor,
    id: Id,
}

struct Facts {
    invitation: database::Fact,
    operator_id: revision::Id,
    operation_id: operation::Id,
}

struct Selected {
    change: Change<state::State>,
    expiry: Expiry,
    output: Withdrawal,
}

impl Command for Withdraw {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Withdrawal;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        completion::Request::new("withdraw", self.id.0.to_string(), &(), authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        command::lock_management(&self.actor, tx, self.id).await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, CommandError> {
        command::authorize(&self.service, tx, subject).await
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, CommandError> {
        let invitation = database::find_by_creator(
            tx.client(),
            &self.service.roles,
            self.id,
            authority.person_id,
            crate::history::Read::Locked,
            authority.now,
        )
        .await?
        .ok_or(Rejection::Absent)?;
        invitation.open(&self.service.invitations)?;
        Ok(Facts {
            invitation,
            operator_id: authority.revision_id,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        if !facts.invitation.manageable() {
            return Err(Rejection::StateConflict.into());
        }
        Ok(Selected {
            change: transition::withdraw(
                &facts.invitation.state,
                facts.operator_id,
                facts.operation_id,
                now,
            )?,
            expiry: facts.invitation.state.expires_at(),
            output: Withdrawal {
                id: self.id,
                withdrawn_at: now,
                operation_id: facts.operation_id,
            },
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Withdrawal, CommandError> {
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
        if tx.now(&self.service.context).await? >= DateTime::<Utc>::from(selected.expiry) {
            return Err(Rejection::StateConflict.into());
        }
        Ok(selected.output)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Withdrawal, CommandError> {
        let original = record::original(
            tx.client(),
            &self.service.roles,
            Family::InvitationWithdrawal,
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "withdrawal completion retains its original record",
        ))?;
        if original.person_id() != request.person_id()
            || original.invitation_id() != self.id
            || !matches!(original.event(), record::Event::Withdrawal)
        {
            return Err(CommandError::corrupt(
                "withdrawal record agrees with its completed input",
            ));
        }
        Ok(Withdrawal {
            id: original.invitation_id(),
            withdrawn_at: original.happened_at(),
            operation_id: tx.operation_id(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        access::{
            Entry,
            appointment::tests::{mobile, register_numbered},
            grant::{
                Error,
                invitation::{
                    Id, Reason,
                    tests::{
                        Choice, body, creator, input, later, operation, reasons, workforce_role,
                    },
                },
                tests::admit,
            },
            role::{self, Kind, tests::start},
        },
        identity::authentication::{Database, Supplier, server},
        startup::Services,
    };

    #[test]
    fn the_creator_withdraws_a_usable_invitation_once_and_permanently() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let peer = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Full)).await;
                let invited = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Invited","level":100,"permissions":[]})).expect("invited definition"))).await.expect("custom selected role");
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&Choice::from(&invited), 2, later())))).await.expect("usable invitation");
                let other = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&workforce_role(&services, &member, Kind::Default).await, 1, later())))).await.expect("another invitation");
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(peer.clone(), operation(), created.id())).await, Err(Error::Absent)), "another creator cannot withdraw");
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(member.clone(), operation(), Id::allocate())).await, Err(Error::Absent)));
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(institution.clone(), operation(), created.id())).await, Err(Error::Denied)), "institutional authority is not a member");
                let original = operation();
                let withdrawal = Box::pin(services.grant().withdraw_invitation(member.clone(), original, created.id())).await.expect("creator withdraws");
                assert_eq!(withdrawal.id(), created.id());
                assert_eq!(withdrawal.operation_id(), original);
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let current = services.current().read(&member).await.expect("creator identity");
                let row = observer.client().query_one("SELECT operator_id, withdrawn_at FROM access.withdrawals WHERE id = $1 AND operation_id = $2", &[&created.id(), &original]).await.expect("one permanent withdrawal before replay");
                assert_eq!(row.get::<_, crate::identity::person::revision::Id>("operator_id"), current.revision_id());
                assert_eq!(row.get::<_, chrono::DateTime<chrono::Utc>>("withdrawn_at"), withdrawal.withdrawn_at());
                let replay = Box::pin(services.grant().withdraw_invitation(member.clone(), original, created.id())).await.expect("original withdrawal replays");
                assert_eq!(replay.withdrawn_at(), withdrawal.withdrawn_at());
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(member.clone(), original, other.id())).await, Err(Error::Mismatch)), "the operation is bound to its invitation");
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(member.clone(), operation(), created.id())).await, Err(Error::StateConflict)), "withdrawal is permanent and happens once");
                assert_eq!(reasons(&observer, &services, created.id()).await, ["withdrawn"]);
                let read = services.grant().read_invitation(&member, created.id()).await.expect("creator still reads it");
                assert_eq!(read.reasons(), [Reason::Withdrawn]);
                assert!(read.code().is_none(), "a withdrawn code is never copied again");
                Box::pin(services.role().delete_workforce(institution.clone(), operation(), invited.id(), serde_json::from_value::<role::workforce::Delete>(json!({"entry":"institution","changeId":invited.change_id()})).expect("deletion"))).await.expect("a withdrawn invitation no longer keeps its role");
                assert!(services.grant().read_invitation(&member, other.id()).await.expect("unrelated invitation").code().is_some());
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.withdrawals", &[]).await.expect("one withdrawal").get::<_, i64>(0), 1);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.grant().withdraw_invitation(member, original, created.id())).await.expect("restart retains the withdrawal").withdrawn_at(), withdrawal.withdrawn_at());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded withdrawal");
        }));
    }

    #[test]
    fn withdrawal_recovers_its_original_result_across_commit_faults() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cancelled"] {
                timeout(Duration::from_secs(45), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (_, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                    let default = workforce_role(&services, &member, Kind::Default).await;
                    let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("usable invitation");
                    let operation_id = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let service = services.grant().clone(); let actor = member.clone(); let id = created.id();
                    let waiter = tokio::spawn(async move { service.withdraw_invitation(actor, operation_id, id).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("original commit intercepted");
                    if case == "cancelled" {
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close();
                        assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err(), "a cancelled wait keeps accepted work until completion");
                        proxy.release(); services.drain().await.expect("accepted work drained");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded outcome").expect("waiter");
                        match case {
                            "rollback" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage() == crate::kernel::Stage::Commit)),
                            "unknown" => assert!(matches!(result, Err(Error::Command(crate::kernel::Error::Unknown(_))))),
                            _ => assert_eq!(result.expect("recovered original").id(), created.id()),
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the actor's account lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.accounts WHERE id = $1 FOR UPDATE", &[&member.account_id()]).await.expect("original backend released the actor's account lock");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.withdrawals WHERE operation_id = $1", &[&operation_id]).await.expect("durable state before replay").get::<_, i64>(0), i64::from(case != "rollback"));
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    Box::pin(services.grant().withdraw_invitation(member, operation_id, created.id())).await.expect("original result or explicit retry after proven rollback");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.withdrawals", &[]).await.expect("one withdrawal after replay").get::<_, i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded recovery case");
            }
        }));
    }

    #[test]
    fn runtime_sql_cannot_forge_or_rewrite_withdrawals() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, member) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let default = workforce_role(&services, &member, Kind::Default).await;
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("usable invitation");
                let creator = services.current().read(&member).await.expect("creator");
                let outsider = services.current().read(&institution).await.expect("other person");
                let runtime = database.connections(&database.user).recover().await.expect("runtime identity");
                for case in ["valid", "other operator", "missing state", "missing completion"] {
                    runtime.client().batch_execute("BEGIN").await.expect("fixture transaction");
                    let original = crate::access::grant::invitation::tests::state(runtime.client(), &services, created.id()).await;
                    let operator = if case == "other operator" { outsider.revision_id() } else { creator.revision_id() };
                    let operation_id = operation();
                    let now = original.changed_at();
                    let change = crate::access::grant::invitation::transition::withdraw(&original, operator, operation_id, now).expect("equal-time transition");
                    let (state, record) = crate::history::Seed::change(change, services.app().keys()).expect("sealed fixture");
                    crate::history::Seed::persist_record(runtime.client(), services.app().keys(), &record).await.expect("record insertion");
                    if case != "missing state" {
                        crate::history::Seed::persist_state(runtime.client(), &state, Some(&original)).await.expect("state advancement");
                    }
                    if case != "missing completion" {
                        crate::access::grant::tests::fixture_completion(runtime.client(), services.grant(), super::COMPLETIONS, "withdraw", uuid::Uuid::from_bytes(*created.id().as_bytes()).to_string(), operator, operation_id, now).await;
                    }
                    let result = runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await;
                    if case == "valid" {
                        result.expect("complete same-time control");
                    } else {
                        let error = result.expect_err(case);
                        assert_eq!(error.code().expect("constraint").code(), if case == "missing completion" { "23503" } else { "23514" }, "{case}");
                    }
                    runtime.client().batch_execute("ROLLBACK").await.expect("discard fixture");
                }
                Box::pin(services.grant().withdraw_invitation(member.clone(), operation(), created.id())).await.expect("real withdrawal");
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(member, operation(), created.id())).await, Err(Error::StateConflict)));
                for statement in ["UPDATE access.withdrawals SET withdrawn_at = withdrawn_at", "DELETE FROM access.withdrawals"] {
                    assert_eq!(runtime.client().batch_execute(statement).await.expect_err(statement).code().expect("privilege").code(), "42501", "{statement}");
                }
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                assert_eq!(owner.client().batch_execute("DELETE FROM access.withdrawals").await.expect_err("immutable").code().expect("trigger").code(), "23514");
                runtime.terminate().await.expect("runtime ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let expires_at: chrono::DateTime<chrono::Utc> = owner.client().query_one("SELECT expires_at FROM access.invitations WHERE id = $1", &[&created.id()]).await.expect("original expiry").get(0);
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE access.invitations SET expires_at = created_at + interval '1 microsecond'; COMMIT").await.expect("withdrawal after expiry");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "startup rejects a withdrawal of an already expired invitation");
                context.stop().await.expect("failed startup context stopped");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated repair");
                owner.client().execute("UPDATE access.invitations SET expires_at = $1", &[&expires_at]).await.expect("restore expiry");
                owner.client().batch_execute("COMMIT").await.expect("repair committed");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE access.withdrawals SET withdrawn_at = (SELECT created_at - interval '1 second' FROM access.invitations WHERE id = access.withdrawals.id); COMMIT").await.expect("backdated withdrawal");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "startup rejects a withdrawal before its invitation");
                context.stop().await.expect("failed startup context stopped");
                owner.terminate().await.expect("owner ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded database protection");
        }));
    }

    #[test]
    fn withdrawal_rejects_a_permanent_loss_after_authority_returns() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = crate::access::role::tests::actor(&database, &services, &mut supplier).await;
                let member = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                let inviter = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("inviter definition"))).await.expect("custom admission role");
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                let default = workforce_role(&services, &institution, Kind::Default).await;
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("usable invitation");
                let removed = Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":inviter.change_id(),"name":"Inviter","level":500,"permissions":[]})).expect("removed authority"))).await.expect("creator loses admission");
                Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":removed.change_id(),"name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("restored authority"))).await.expect("creator regains admission");
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(member, operation(), created.id())).await, Err(Error::StateConflict)), "the service also sees the permanent loss");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded early-time protection");
        }));
    }

    #[test]
    fn retained_withdrawals_survive_later_suspension_but_reject_timestamp_tampering() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = crate::access::role::tests::actor(&database, &services, &mut supplier).await;
                let member = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("inviter definition"))).await.expect("custom admission role");
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                let manager = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Full)).await;
                let default = workforce_role(&services, &institution, Kind::Default).await;
                let withdrawn = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("invitation to withdraw");
                let withdrawal = Box::pin(services.grant().withdraw_invitation(member.clone(), operation(), withdrawn.id())).await.expect("real withdrawal");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                let original = crate::access::grant::invitation::tests::state(owner.client(), &services, withdrawn.id()).await;
                assert!(crate::access::grant::invitation::transition::withdraw(&original, original.operator_id(), operation(), original.expires_at().into()).is_err(), "expiry is exclusive");
                let grant = services.grant().read_latest(&member).await.expect("creator grant");
                let current = services.current().read(&member).await.expect("creator identity");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13800000000"}});
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(target).expect("pause confirmation"))).await.expect("real Grant suspension after the withdrawal");
                assert!(pause.suspended_at() > withdrawal.withdrawn_at());
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("later suspension preserves the original completed withdrawal");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated damage");
                owner.client().execute("UPDATE access.withdrawals SET withdrawn_at = $2 WHERE id = $1", &[&withdrawn.id(), &(pause.suspended_at() + chrono::Duration::microseconds(1))]).await.expect("withdrawal moved into the suspension");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "startup rejects a changed sealed withdrawal timestamp");
                context.stop().await.expect("failed startup context stopped");
                owner.terminate().await.expect("owner ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded retained withdrawal rules");
        }));
    }

    #[test]
    fn creation_and_withdrawal_require_current_unsuspended_authority() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = crate::access::role::tests::actor(&database, &services, &mut supplier).await;
                let member = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("inviter definition"))).await.expect("custom admission role");
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                let manager = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Full)).await;
                let default = workforce_role(&services, &institution, Kind::Default).await;
                let created = Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await.expect("usable invitation");
                let grant = services.grant().read_latest(&member).await.expect("creator grant");
                let current = services.current().read(&member).await.expect("creator identity");
                let target = json!({"target":{"accountId":current.id(),"personId":current.person_id(),"revisionId":current.revision_id(),"mobile":"13800000000"}});
                let pause = Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), serde_json::from_value(target.clone()).expect("pause confirmation"))).await.expect("real Grant suspension");
                assert!(matches!(Box::pin(services.grant().withdraw_invitation(member.clone(), operation(), created.id())).await, Err(Error::Denied)));
                assert!(matches!(Box::pin(services.grant().create_invitation(member.clone(), operation(), input(&body(&default, 1, later())))).await, Err(Error::Denied)));
                Box::pin(services.grant().resume(manager, operation(), pause.id(), serde_json::from_value(target).expect("resume confirmation"))).await.expect("real resumption");
                Box::pin(services.grant().withdraw_invitation(member, operation(), created.id())).await.expect("a fresh withdrawal after resumption");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded early-start protection");
        }));
    }

    #[test]
    fn held_role_changes_after_withdrawal_preserve_the_invitation_and_original_result() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let institution = crate::access::role::tests::actor(&database, &services, &mut supplier).await;
                let member = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                let held = Box::pin(services.role().create_workforce(institution.clone(), operation(),
                    serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("custom held definition"))).await.expect("real custom admission role");
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                assert_eq!(services.grant().read_role(&member).await.expect("current held role").id(), held.id());
                let default = workforce_role(&services, &institution, Kind::Default).await;
                let original_creation = operation();
                let value = body(&default, 1, later());
                let created = Box::pin(services.grant().create_invitation(member.clone(), original_creation, input(&value))).await.expect("usable invitation");
                assert!(created.reasons().is_empty());
                assert!(created.code().is_some());
                let original_withdrawal = operation();
                let withdrawal = Box::pin(services.grant().withdraw_invitation(member.clone(), original_withdrawal, created.id())).await.expect("real withdrawal");
                let changed = Box::pin(services.role().update_workforce(institution, operation(), held.id(),
                    serde_json::from_value(json!({"entry":"institution","changeId":held.change_id(),"name":"Later","level":500,"permissions":["workforce.admission"]})).expect("later held definition"))).await.expect("normal role API follows withdrawal");
                assert_eq!(changed.id(), held.id());
                assert_ne!(changed.change_id(), held.change_id());
                let observer = database.connections(&database.user).recover().await.expect("independent observer");
                let changed_at: chrono::DateTime<chrono::Utc> = observer.client().query_one("SELECT changed_at FROM access.changes WHERE id=$1", &[&changed.change_id()]).await.expect("actual committed role time").get(0);
                assert!(changed_at > withdrawal.withdrawn_at());
                assert_eq!(services.grant().read_role(&member).await.expect("new live definition").change_id(), changed.change_id());
                let read = services.grant().read_invitation(&member, created.id()).await.expect("withdrawn invitation remains readable");
                assert_eq!(read.reasons(), [Reason::Withdrawn]);
                assert!(read.code().is_none(), "later role changes do not revive a withdrawn code");
                assert_eq!(reasons(&observer, &services, created.id()).await, ["withdrawn"]);
                let creation = Box::pin(services.grant().create_invitation(member.clone(), original_creation, input(&value))).await.expect("original creation remains available");
                assert_eq!(creation.id(), created.id());
                assert_eq!(creation.created_at(), created.created_at());
                assert_eq!(creation.reasons(), [Reason::Withdrawn]);
                assert!(creation.code().is_none());
                let replay = Box::pin(services.grant().withdraw_invitation(member, original_withdrawal, created.id())).await.expect("original withdrawal after role modification");
                assert_eq!(replay.id(), withdrawal.id());
                assert_eq!(replay.operation_id(), original_withdrawal);
                assert_eq!(replay.withdrawn_at(), withdrawal.withdrawn_at());
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.withdrawals WHERE id=$1", &[&created.id()]).await.expect("single permanent withdrawal").get::<_, i64>(0), 1);
                observer.terminate().await.expect("observer ended");
                services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded withdrawal and later definition");
        }));
    }
}
