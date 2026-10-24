//! Append-only changes to a creator's invitation capacity and expiry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql};
use utoipa::ToSchema;
use uuid::Uuid;

use super::{Capacity, Expiry, Id as InvitationId, command, database, record, state, transition};
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
    completion::Template::personal(Family::InvitationAdjustment, |action, target| {
        action == "adjust" && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
    });

/// A permanent adjustment identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = invitation::adjustment::Id, value_type = Uuid)]
pub struct Id(Uuid);

impl Id {
    pub(super) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = invitation::adjustment::Terms)]
pub struct Terms {
    pub(super) capacity: Capacity,
    pub(super) expires_at: Expiry,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = invitation::adjustment::Input)]
pub struct Input {
    pub(super) expected: Terms,
    pub(super) capacity: Capacity,
    pub(super) expires_at: Expiry,
}

/// The original completed adjustment, independent of subsequent changes.
pub struct Adjustment {
    id: Id,
    invitation_id: InvitationId,
    capacity: Capacity,
    expires_at: Expiry,
    adjusted_at: DateTime<Utc>,
    operation_id: operation::Id,
}

impl Adjustment {
    pub fn id(&self) -> Id {
        self.id
    }
    pub fn invitation_id(&self) -> InvitationId {
        self.invitation_id
    }
    pub fn capacity(&self) -> Capacity {
        self.capacity
    }
    pub fn expires_at(&self) -> Expiry {
        self.expires_at
    }
    pub fn adjusted_at(&self) -> DateTime<Utc> {
        self.adjusted_at
    }
    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }
}

impl Service {
    pub async fn adjust_invitation(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: InvitationId,
        input: Input,
    ) -> Result<Adjustment, Error> {
        Ok(self
            .app
            .command(
                operation_id,
                Adjust {
                    service: self.clone(),
                    actor,
                    invitation_id: id,
                    input,
                    id: Id(Uuid::new_v4()),
                },
            )
            .await?)
    }
}

struct Adjust {
    service: Service,
    actor: Actor,
    invitation_id: InvitationId,
    input: Input,
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
    output: Adjustment,
}

impl Command for Adjust {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Adjustment;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        completion::Request::new(
            "adjust",
            self.invitation_id.0.to_string(),
            &self.input,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        command::lock_management(&self.actor, tx, self.invitation_id).await
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
            self.invitation_id,
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
        let original = &facts.invitation.state;
        if self.input.expected
            != (Terms {
                capacity: original.capacity(),
                expires_at: original.expires_at(),
            })
        {
            return Err(Rejection::TargetChanged.into());
        }
        if (self.input.capacity, self.input.expires_at)
            == (original.capacity(), original.expires_at())
        {
            return Err(Rejection::StateConflict.into());
        }
        if i64::from(self.input.capacity) < original.used() {
            return Err(Rejection::Capacity.into());
        }
        if DateTime::<Utc>::from(self.input.expires_at) <= now {
            return Err(Rejection::Expiry.into());
        }
        Ok(Selected {
            change: transition::adjust(
                original,
                transition::Adjustment {
                    id: self.id,
                    capacity: self.input.capacity,
                    expires_at: self.input.expires_at,
                },
                facts.operator_id,
                facts.operation_id,
                now,
            )?,
            expiry: original.expires_at(),
            output: Adjustment {
                id: self.id,
                invitation_id: self.invitation_id,
                capacity: self.input.capacity,
                expires_at: self.input.expires_at,
                adjusted_at: now,
                operation_id: facts.operation_id,
            },
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Adjustment, CommandError> {
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
    ) -> Result<Adjustment, CommandError> {
        let original = record::original(
            tx.client(),
            &self.service.roles,
            Family::InvitationAdjustment,
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "adjustment completion retains its original record",
        ))?;
        let record::Event::Adjustment { id } = original.event() else {
            return Err(CommandError::corrupt(
                "adjustment completion identifies an adjustment",
            ));
        };
        if original.person_id() != request.person_id()
            || original.invitation_id() != self.invitation_id
            || original.capacity() != self.input.capacity
            || original.expires_at() != self.input.expires_at
        {
            return Err(CommandError::corrupt(
                "adjustment record agrees with its completed input",
            ));
        }
        Ok(Adjustment {
            id: *id,
            invitation_id: original.invitation_id(),
            capacity: original.capacity(),
            expires_at: original.expires_at(),
            adjusted_at: original.happened_at(),
            operation_id: tx.operation_id(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::{DateTime, Utc};
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        access::{
            Entry,
            appointment::tests::{mobile, register_numbered},
            grant::{
                Error,
                invitation::{
                    self, Reason,
                    adjustment::Input,
                    tests::{body, creator, input, later, operation, text, workforce_role},
                },
                tests::admit,
            },
            role::{Kind, tests::start},
        },
        identity::authentication::{Database, Supplier, server},
    };

    #[test]
    fn adjustment_recovers_its_original_result_across_commit_faults() {
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
                    let expiry = created.expires_at();
                    let operation_id = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let service = services.grant().clone(); let actor = member.clone(); let id = created.id();
                    let waiter = tokio::spawn(async move { service.adjust_invitation(actor, operation_id, id, command(1, expiry, 2, expiry)).await });
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
                            _ => assert_eq!(result.expect("recovered original").invitation_id(), created.id()),
                        }
                        services.close(); services.drain().await.expect("drained");
                    }
                    context.stop().await.expect("stopped");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the actor's account lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.accounts WHERE id = $1 FOR UPDATE", &[&member.account_id()]).await.expect("original backend released the actor's account lock");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.adjustments WHERE operation_id = $1", &[&operation_id]).await.expect("durable state before replay").get::<_, i64>(0), i64::from(case != "rollback"));
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    Box::pin(services.grant().adjust_invitation(member, operation_id, created.id(), command(1, expiry, 2, expiry))).await.expect("original result or explicit retry after proven rollback");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.adjustments", &[]).await.expect("one adjustment after replay").get::<_, i64>(0), 1);
                    observer.terminate().await.expect("observer ended");
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("bounded recovery case");
            }
        }));
    }

    fn command(
        expected_capacity: i64,
        expected_expiry: DateTime<Utc>,
        capacity: i64,
        expires_at: DateTime<Utc>,
    ) -> Input {
        serde_json::from_value(json!({"expected":{"capacity":expected_capacity,"expiresAt":expected_expiry},"capacity":capacity,"expiresAt":expires_at})).expect("adjustment input")
    }

    #[test]
    fn adjustments_change_current_terms_and_preserve_original_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later(); let extended = expiry + chrono::Duration::days(1);
                let original_creation = operation();
                let created = Box::pin(services.grant().create_invitation(manager.clone(), original_creation, input(&body(&selected, 1, expiry)))).await.expect("invitation");
                let code = text(&created); let id = created.id(); let original = operation();
                let first = Box::pin(services.grant().adjust_invitation(manager.clone(), original, id, command(1, expiry, 2, expiry))).await.expect("capacity only");
                let second = Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), id, command(2, expiry, 2, extended))).await.expect("expiry only");
                let third = Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), id, command(2, extended, 3, expiry))).await.expect("both");
                assert!(first.adjusted_at() < second.adjusted_at() && second.adjusted_at() < third.adjusted_at());
                let read = services.grant().read_invitation(&manager, id).await.expect("current terms");
                assert_eq!(i64::from(read.capacity()), 3); assert_eq!(read.expires_at(), expiry); assert_eq!(text(&read), code);
                let replay = Box::pin(services.grant().adjust_invitation(manager.clone(), original, id, command(1, expiry, 2, expiry))).await.expect("original adjustment");
                assert_eq!(replay.id(), first.id()); assert_eq!(i64::from(replay.capacity()), 2);
                for (target, input) in [
                    (id, command(2, expiry, 2, expiry)), (id, command(1, extended, 2, expiry)),
                    (id, command(1, expiry, 3, expiry)), (id, command(1, expiry, 2, extended)),
                    (invitation::Id::allocate(), command(1, expiry, 2, expiry)),
                ] {
                    assert!(matches!(Box::pin(services.grant().adjust_invitation(manager.clone(), original, target, input)).await, Err(Error::Mismatch)));
                }
                let replay = Box::pin(services.grant().create_invitation(manager.clone(), original_creation, input(&body(&selected, 1, expiry)))).await.expect("creation matches original input");
                assert_eq!(i64::from(replay.capacity()), 3);
                let candidate = services.grant().invitation_candidate(&holder, serde_json::from_value(json!({"code":code})).expect("candidate")).await.expect("adjusted candidate");
                assert_eq!(serde_json::to_value(candidate).expect("candidate projection")["expiresAt"], json!(expiry));
                let accepted_operation = operation();
                Box::pin(services.grant().accept_invitation(holder.clone(), accepted_operation, serde_json::from_value(json!({"code":code,"changeId":selected.change_id()})).expect("accept"))).await.expect("first unit");
                let peer = Box::pin(register_numbered(&database, &services, &mut supplier, 21)).await;
                Box::pin(services.grant().accept_invitation(peer.clone(), operation(), serde_json::from_value(json!({"code":code,"changeId":selected.change_id()})).expect("accept"))).await.expect("expanded second unit");
                assert!(matches!(Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), id, command(3, expiry, 1, expiry))).await, Err(Error::Capacity)));
                Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), id, command(3, expiry, 2, extended))).await.expect("capacity equal to cumulative usage");
                assert_eq!(services.grant().read_invitation(&manager, id).await.expect("full").reasons(), [Reason::CapacityReached]);
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT capacity, expires_at, (SELECT count(*) FROM access.adjustments WHERE invitation_id=$1), (SELECT count(*) FROM access.acceptances WHERE invitation_id=$1) FROM access.invitations WHERE id=$1", &[&id]).await.expect("independent facts");
                assert_eq!(row.get::<_, i64>(0), 1); assert_eq!(row.get::<_, DateTime<Utc>>(1), expiry); assert_eq!(row.get::<_, i64>(2), 4); assert_eq!(row.get::<_, i64>(3), 2);
                Box::pin(services.grant().withdraw_invitation(manager.clone(), operation(), id)).await.expect("withdraw adjusted code");
                assert_eq!(Box::pin(services.grant().adjust_invitation(manager.clone(), original, id, command(1, expiry, 2, expiry))).await.expect("replay after withdrawal").id(), first.id());
                Box::pin(services.grant().accept_invitation(holder, accepted_operation, serde_json::from_value(json!({"code":code,"changeId":selected.change_id()})).expect("accept replay"))).await.expect("old acceptance remains valid");
                observer.terminate().await.expect("observer end");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.grant().adjust_invitation(manager, original, id, command(1, expiry, 2, expiry))).await.expect("restart replay").id(), first.id());
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); supplier.stop().await;
            })).await.expect("bounded adjustments");
        }));
    }

    #[test]
    fn adjustment_rejects_wrong_authority_targets_and_values() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(60),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let (institution, manager) =
                        Box::pin(creator(&database, &services, &mut supplier)).await;
                    let peer =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                    Box::pin(admit(
                        &services,
                        &institution,
                        Entry::Institution,
                        &mobile(10),
                        Kind::Full,
                    ))
                    .await;
                    let selected = workforce_role(&services, &manager, Kind::Default).await;
                    let expiry = later();
                    let created = Box::pin(services.grant().create_invitation(
                        manager.clone(),
                        operation(),
                        input(&body(&selected, 2, expiry)),
                    ))
                    .await
                    .expect("invitation");
                    let id = created.id();
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            institution.clone(),
                            operation(),
                            id,
                            command(2, expiry, 3, expiry)
                        ))
                        .await,
                        Err(Error::Denied)
                    ));
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            peer,
                            operation(),
                            id,
                            command(2, expiry, 3, expiry)
                        ))
                        .await,
                        Err(Error::Absent)
                    ));
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            manager.clone(),
                            operation(),
                            invitation::Id::allocate(),
                            command(2, expiry, 3, expiry)
                        ))
                        .await,
                        Err(Error::Absent)
                    ));
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            manager.clone(),
                            operation(),
                            id,
                            command(1, expiry, 3, expiry)
                        ))
                        .await,
                        Err(Error::TargetChanged)
                    ));
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            manager.clone(),
                            operation(),
                            id,
                            command(2, expiry, 2, expiry)
                        ))
                        .await,
                        Err(Error::StateConflict)
                    ));
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            manager.clone(),
                            operation(),
                            id,
                            command(2, expiry, 3, created.created_at())
                        ))
                        .await,
                        Err(Error::Expiry)
                    ));
                    Box::pin(services.grant().withdraw_invitation(
                        manager.clone(),
                        operation(),
                        id,
                    ))
                    .await
                    .expect("withdraw");
                    assert!(matches!(
                        Box::pin(services.grant().adjust_invitation(
                            manager,
                            operation(),
                            id,
                            command(2, expiry, 3, expiry)
                        ))
                        .await,
                        Err(Error::StateConflict)
                    ));
                    services.close();
                    services.drain().await.expect("drain");
                    context.stop().await.expect("stop");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded rejection cases");
        }));
    }

    #[test]
    fn adjustment_serializes_with_the_last_acceptance_and_withdrawal() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["adjust_accept", "accept_adjust", "adjust_withdraw", "withdraw_adjust"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                    let peer = Box::pin(register_numbered(&database, &services, &mut supplier, 21)).await;
                    let selected = workforce_role(&services, &manager, Kind::Default).await;
                    let expiry = later();
                    let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 2, expiry)))).await.expect("two units");
                    let id = created.id(); let code = text(&created); let change = selected.change_id();
                    Box::pin(services.grant().accept_invitation(holder, operation(), serde_json::from_value(json!({"code":code,"changeId":change})).expect("first unit"))).await.expect("one unit remains");
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    proxy.arm(Fault::DelayedCommit);
                    let service = services.grant().clone(); let actor = manager.clone(); let accepting = peer.clone(); let original_code = code.clone();
                    let first = tokio::spawn(async move {
                        match case {
                            "accept_adjust" => service.accept_invitation(accepting, operation(), serde_json::from_value(json!({"code":original_code,"changeId":change})).expect("acceptance")).await.map(|_| ()),
                            "withdraw_adjust" => service.withdraw_invitation(actor, operation(), id).await.map(|_| ()),
                            _ => service.adjust_invitation(actor, operation(), id, command(2, expiry, 1, expiry)).await.map(|_| ()),
                        }
                    });
                    let blocker = timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("first operation holds locks at commit");
                    let service = services.grant().clone(); let actor = manager.clone();
                    let second = tokio::spawn(async move {
                        match case {
                            "adjust_accept" => service.accept_invitation(peer, operation(), serde_json::from_value(json!({"code":code,"changeId":change})).expect("acceptance")).await.map(|_| ()),
                            "adjust_withdraw" => service.withdraw_invitation(actor, operation(), id).await.map(|_| ()),
                            _ => service.adjust_invitation(actor, operation(), id, command(2, expiry, 1, expiry)).await.map(|_| ()),
                        }
                    });
                    timeout(Duration::from_secs(3), async {
                        loop {
                            if observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)))", &[&blocker]).await.expect("actual wait graph").get::<_, bool>(0) { break; }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("second operation waits for the first");
                    proxy.release(); first.await.expect("first task").expect("first operation succeeds");
                    let result = second.await.expect("second task");
                    match case {
                        "adjust_accept" => assert!(matches!(result, Err(Error::InvitationUnavailable))),
                        "accept_adjust" => assert!(matches!(result, Err(Error::Capacity))),
                        "withdraw_adjust" => assert!(matches!(result, Err(Error::StateConflict))),
                        _ => result.expect("withdraw follows adjustment"),
                    }
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.acceptances WHERE invitation_id=$1), (SELECT current_capacity FROM access.invitations WHERE id=$1), (SELECT count(*) FROM access.adjustments WHERE invitation_id=$1), (SELECT count(*) FROM access.withdrawals WHERE id=$1)", &[&id]).await.expect("serialized durable facts");
                    assert_eq!(row.get::<_, i64>(0), if case=="accept_adjust" {2} else {1});
                    assert!(row.get::<_, i64>(0) <= row.get::<_, i64>(1));
                    assert_eq!(row.get::<_, i64>(2), i64::from(case.starts_with("adjust_")));
                    assert_eq!(row.get::<_, i64>(3), i64::from(case.contains("withdraw")));
                    observer.terminate().await.expect("observer end");
                    services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); proxy.stop().await; supplier.stop().await;
                })).await.expect("bounded serialization case");
            }
        }));
    }

    #[test]
    fn adjustment_never_revives_expired_or_permanently_unavailable_codes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["expired", "admission_lost", "grant_terminated", "account_cancelled", "account_suspended", "grant_suspended", "role_unassignable"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let institution = crate::access::role::tests::actor(&database, &services, &mut supplier).await;
                    let manager = Box::pin(crate::access::appointment::tests::register(&database, &services, &mut supplier)).await;
                    let inviter = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("role"))).await.expect("admission role");
                    Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                    let supervisor = Box::pin(register_numbered(&database, &services, &mut supplier, 10)).await;
                    Box::pin(admit(&services, &institution, Entry::Institution, &mobile(10), Kind::Full)).await;
                    let selected = workforce_role(&services, &institution, Kind::Default).await;
                    let selected = if case == "role_unassignable" {
                        let role = Box::pin(services.role().create_workforce(institution.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Selected","level":2,"permissions":[]})).expect("selected role"))).await.expect("selected role");
                        invitation::tests::Choice::from(&role)
                    } else { selected };
                    let expiry = if case == "expired" { chrono::SubsecRound::trunc_subsecs(Utc::now() + chrono::Duration::seconds(2), 6) } else { later() };
                    let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, expiry)))).await.expect("invitation");
                    let id = created.id();
                    match case {
                        "expired" => tokio::time::sleep((expiry - Utc::now()).to_std().expect("future expiry") + Duration::from_millis(1)).await,
                        "admission_lost" => {
                            let removed = Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":inviter.change_id(),"name":"Inviter","level":500,"permissions":[]})).expect("remove"))).await.expect("lost authority");
                            assert!(matches!(Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), id, command(1, expiry, 2, expiry))).await, Err(Error::Denied)));
                            Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":removed.change_id(),"name":"Inviter","level":500,"permissions":["workforce.admission"]})).expect("restore"))).await.expect("regained authority");
                        }
                        "grant_terminated" => {
                            let grant = services.grant().read_latest(&manager).await.expect("current grant");
                            Box::pin(services.grant().terminate(manager.clone(), operation(), grant.id())).await.expect("exit");
                            Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", Kind::Custom)).await;
                        }
                        "account_cancelled" => {
                            let identity = services.current().read(&manager).await.expect("creator");
                            Box::pin(services.cancellation().cancel(manager.clone(), operation(), serde_json::from_value(json!({"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id()})).expect("cancel confirmation"))).await.expect("creator cancels");
                        }
                        "account_suspended" => {
                            let identity = services.current().read(&manager).await.expect("creator");
                            Box::pin(services.suspension().suspend(institution.clone(), operation(), identity.id(), serde_json::from_value(json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000"})).expect("suspend"))).await.expect("account paused");
                        }
                        "grant_suspended" => {
                            let candidate = services.grant().suspension_candidate(&supervisor, "13800000000".parse().expect("mobile")).await.expect("candidate");
                            let identity = candidate.identity();
                            Box::pin(services.grant().suspend(supervisor, operation(), candidate.grant_id(), serde_json::from_value(json!({"target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000"}})).expect("suspend"))).await.expect("grant paused");
                        }
                        "role_unassignable" => {
                            Box::pin(services.role().update_workforce(institution.clone(), operation(), inviter.id(), serde_json::from_value(json!({"entry":"institution","changeId":inviter.change_id(),"name":"Inviter","level":2,"permissions":["workforce.admission"]})).expect("lower authority"))).await.expect("role changed");
                        }
                        _ => unreachable!(),
                    }
                    let result = Box::pin(services.grant().adjust_invitation(manager, operation(), id, command(1, expiry, 2, later()))).await;
                    match case {
                        "account_cancelled" | "account_suspended" => assert!(matches!(result, Err(Error::Current(crate::identity::current::Error::Invalid))), "{case}"),
                        "grant_suspended" => assert!(matches!(result, Err(Error::Denied))),
                        "role_unassignable" => { result.expect("temporary role restriction still allows adjustment"); }
                        _ => assert!(matches!(result, Err(Error::StateConflict)), "{case}"),
                    }
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    assert!(crate::access::grant::invitation::tests::reasons(&observer, &services, id).await.contains(&case.to_owned()));
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.adjustments", &[]).await.expect("only permitted change persists").get::<_, i64>(0), i64::from(case=="role_unassignable"));
                    observer.terminate().await.expect("observer end");
                    services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); supplier.stop().await;
                })).await.expect("bounded availability case");
            }
        }));
    }

    #[test]
    fn runtime_sql_and_startup_enforce_adjustment_history() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let peer = Box::pin(register_numbered(&database, &services, &mut supplier, 21)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later();
                let created = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 2, expiry)))).await.expect("invitation");
                let id = created.id(); let code = text(&created);
                let revision = services.current().read(&manager).await.expect("creator").revision_id();
                let other = services.current().read(&holder).await.expect("other person").revision_id();
                for actor in [holder, peer] {
                    Box::pin(services.grant().accept_invitation(actor, operation(), serde_json::from_value(json!({"code":code,"changeId":selected.change_id()})).expect("accept"))).await.expect("fill capacity through service");
                }
                let runtime = database.connections(&database.user).recover().await.expect("runtime identity");
                for case in ["valid", "other operator", "missing record", "sequence gap", "missing completion"] {
                    runtime.client().batch_execute("BEGIN").await.expect("fixture transaction");
                    let original = invitation::tests::state(runtime.client(), &services, id).await;
                    let mut data = original.next(original.changed_at()).expect("same-time next state");
                    data.capacity = serde_json::from_value(json!(3)).expect("capacity");
                    let operation_id = operation();
                    let operator = if case == "other operator" { other } else { revision };
                    let sequence = crate::history::State::sequence(&*original) + if case == "sequence gap" { 2 } else { 1 };
                    let record = crate::history::Seed::record::<invitation::record::Record>(
                        id, sequence, invitation::record::Data::from_state(&data, operation_id, operator,
                            invitation::record::Event::Adjustment { id: super::Id(uuid::Uuid::new_v4()) }),
                        services.app().keys(),
                    ).expect("sealed record");
                    let state = crate::history::Seed::state::<invitation::state::State>(
                        data, sequence, crate::history::State::version(&*original) + 1, services.app().keys(),
                    ).expect("sealed state");
                    if case != "missing record" {
                        crate::history::Seed::persist_record(runtime.client(), services.app().keys(), &record).await.expect("record");
                    }
                    crate::history::Seed::persist_state(runtime.client(), &state, Some(&original)).await.expect("state");
                    if case != "missing completion" {
                        crate::access::grant::tests::fixture_completion(runtime.client(), services.grant(), super::COMPLETIONS, "adjust", uuid::Uuid::from_bytes(*id.as_bytes()).to_string(), operator, operation_id, original.changed_at()).await;
                    }
                    let result = runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await;
                    if case == "valid" { result.expect("equal-time complete control"); }
                    else { assert_eq!(result.expect_err(case).code().expect("constraint").code(), if case == "missing completion" { "23503" } else { "23514" }, "{case}"); }
                    runtime.client().batch_execute("ROLLBACK").await.expect("discard fixture");
                }
                Box::pin(services.grant().adjust_invitation(manager.clone(), operation(), id, command(2, expiry, 3, expiry))).await.expect("real adjustment");
                for statement in ["UPDATE access.adjustments SET capacity=capacity", "DELETE FROM access.adjustments"] {
                    assert_eq!(runtime.client().batch_execute(statement).await.expect_err("runtime immutable").code().expect("privilege").code(), "42501");
                }
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                assert_eq!(owner.client().batch_execute("DELETE FROM access.adjustments").await.expect_err("owner immutable").code().expect("trigger").code(), "23514");
                Box::pin(services.grant().withdraw_invitation(manager.clone(), operation(), id)).await.expect("withdraw");
                assert!(matches!(Box::pin(services.grant().adjust_invitation(manager, operation(), id, command(3, expiry, 4, expiry))).await, Err(Error::StateConflict)), "withdrawal cannot be revived");
                runtime.terminate().await.expect("runtime end");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica; UPDATE access.adjustments SET capacity=2; COMMIT").await.expect("isolated corrupt unchanged adjustment");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "startup rejects invalid retained adjustments");
                context.stop().await.expect("failed startup stopped"); owner.terminate().await.expect("owner end"); supplier.stop().await;
            })).await.expect("bounded SQL and startup");
        }));
    }

    #[test]
    fn startup_rejects_a_rewritten_adjustment_timestamp() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expiry = later();
                let created = Box::pin(services.grant().create_invitation(
                    manager.clone(), operation(), input(&body(&selected, 2, expiry)),
                )).await.expect("invitation");
                let accepted = Box::pin(services.grant().accept_invitation(
                    holder, operation(), serde_json::from_value(json!({"code":text(&created),"changeId":selected.change_id()})).expect("acceptance input"),
                )).await.expect("permanent acceptance");
                let adjusted = Box::pin(services.grant().adjust_invitation(
                    manager, operation(), created.id(), command(2, expiry, 3, expiry),
                )).await.expect("later adjustment");
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated retained damage");
                assert_eq!(owner.client().execute(
                    "UPDATE access.adjustments SET adjusted_at=(SELECT granted_at FROM access.grants WHERE id=$1) WHERE id=$2",
                    &[&accepted.0.id(), &adjusted.id()],
                ).await.expect("equal-time adjustment damage"), 1);
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                assert!(owner.client().query_one(
                    "SELECT admission.granted_at = adjustment.adjusted_at AND acceptance.invitation_id = adjustment.invitation_id
                    FROM access.acceptances acceptance JOIN access.grants admission ON admission.id = acceptance.id
                    CROSS JOIN access.adjustments adjustment WHERE acceptance.id=$1 AND adjustment.id=$2",
                    &[&accepted.0.id(), &adjusted.id()],
                ).await.expect("same invitation and instant").get::<_, bool>(0));
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(matches!(result, Err(crate::startup::Error::Grant(Error::Command(crate::kernel::Error::Corrupt(_))))), "changed record time fails seal verification: {:?}", result.as_ref().err());
                context.stop().await.expect("failed startup stopped"); owner.terminate().await.expect("owner end"); supplier.stop().await;
            })).await.expect("bounded timestamp integrity check");
        }));
    }

    #[test]
    fn extended_expiry_and_capacity_apply_to_reads_acceptance_and_withdrawal() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(60),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let (holder, manager) =
                        Box::pin(creator(&database, &services, &mut supplier)).await;
                    let peer =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 21)).await;
                    let selected = workforce_role(&services, &manager, Kind::Default).await;
                    let expiry = chrono::SubsecRound::trunc_subsecs(
                        Utc::now() + chrono::Duration::seconds(2),
                        6,
                    );
                    let extended = later();
                    let created = Box::pin(services.grant().create_invitation(
                        manager.clone(),
                        operation(),
                        input(&body(&selected, 1, expiry)),
                    ))
                    .await
                    .expect("short invitation");
                    let id = created.id();
                    let code = text(&created);
                    Box::pin(services.grant().adjust_invitation(
                        manager.clone(),
                        operation(),
                        id,
                        command(1, expiry, 1, extended),
                    ))
                    .await
                    .expect("extend before expiry");
                    tokio::time::sleep(
                        (expiry - Utc::now())
                            .to_std()
                            .expect("original future expiry")
                            + Duration::from_millis(1),
                    )
                    .await;
                    let candidate = services
                        .grant()
                        .invitation_candidate(
                            &holder,
                            serde_json::from_value(json!({"code":code})).expect("candidate"),
                        )
                        .await
                        .expect("extended candidate");
                    assert_eq!(
                        serde_json::to_value(candidate).expect("candidate")["expiresAt"],
                        json!(extended)
                    );
                    Box::pin(
                        services.grant().accept_invitation(
                            holder,
                            operation(),
                            serde_json::from_value(
                                json!({"code":code,"changeId":selected.change_id()}),
                            )
                            .expect("accept"),
                        ),
                    )
                    .await
                    .expect("accept after initial expiry");
                    assert_eq!(
                        services
                            .grant()
                            .read_invitation(&manager, id)
                            .await
                            .expect("full")
                            .reasons(),
                        [Reason::CapacityReached]
                    );
                    Box::pin(services.grant().adjust_invitation(
                        manager.clone(),
                        operation(),
                        id,
                        command(1, extended, 2, extended),
                    ))
                    .await
                    .expect("expand full code");
                    assert!(
                        services
                            .grant()
                            .read_invitation(&manager, id)
                            .await
                            .expect("available again")
                            .reasons()
                            .is_empty()
                    );
                    Box::pin(
                        services.grant().accept_invitation(
                            peer,
                            operation(),
                            serde_json::from_value(
                                json!({"code":code,"changeId":selected.change_id()}),
                            )
                            .expect("accept"),
                        ),
                    )
                    .await
                    .expect("new unit");
                    Box::pin(
                        services
                            .grant()
                            .withdraw_invitation(manager, operation(), id),
                    )
                    .await
                    .expect("withdraw after initial expiry");
                    services.close();
                    services.drain().await.expect("drain");
                    context.stop().await.expect("stop");
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    services.close();
                    services.drain().await.expect("drain");
                    context.stop().await.expect("stop");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded adjusted consumers");
        }));
    }
}
