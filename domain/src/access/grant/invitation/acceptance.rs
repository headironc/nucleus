//! A code holder confirms a role definition and obtains one permanent admission.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::timeout_at;
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{
        assignment::{self, Assignment},
        capability::Scope,
        grant::{
            self, CommandError, Error, Grant, Rejection, Service, Status,
            invitation::{Code, Expiry, database, record},
            member, state, transition,
        },
        role,
    },
    history::{self, Change},
    identity::{account, current::Actor, operation, person::revision},
    kernel::{
        Authorization, Command, completion,
        tx::{self, Tx},
    },
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = invitation::acceptance::Search)]
pub struct Search {
    #[schema(value_type = String, pattern = "^[A-Za-z0-9_-]{43}$")]
    code: Code,
}

#[derive(Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = invitation::acceptance::Input)]
pub struct Input {
    #[schema(value_type = String, pattern = "^[A-Za-z0-9_-]{43}$")]
    pub(super) code: Code,
    pub(super) change_id: role::change::Id,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = invitation::acceptance::Role)]
pub struct Role {
    name: role::Name,
    level: role::Level,
    change_id: role::change::Id,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = invitation::acceptance::Candidate)]
pub struct Candidate {
    role: Role,
    creator_name: String,
    expires_at: DateTime<Utc>,
}

impl Service {
    pub async fn invitation_candidate(
        &self,
        actor: &Actor,
        search: Search,
    ) -> Result<Candidate, Error> {
        self.registry.ready()?;
        let lease = self.context.connections().lease().await?;
        let budget = self.context.read_budget();
        let deadline = budget.deadline();
        let search = &search;
        let result = timeout_at(deadline, async {
            let client = lease.client();
            budget
                .run(client, self.roles.anchors(), |view| {
                    let roles = self.roles.at(view);
                    async move {
                        let (_, _, now) = self.current.at(view).fetch_person(client, actor).await?;
                        let fact = database::find_by_code(
                            client,
                            &roles,
                            &self.invitations,
                            &search.code,
                            now,
                        )
                        .await?
                        .ok_or(Error::InvitationUnavailable)?;
                        let (selected, operator_id) = self
                            .acceptable_invitation(client, &roles, &fact, &search.code, now)
                            .await?;
                        let creator = self.revisions.fetch_identity(client, operator_id).await?;
                        Ok(Candidate {
                            role: Role {
                                name: selected.name().clone(),
                                level: selected.level(),
                                change_id: selected.change_id(),
                            },
                            creator_name: creator.name().to_owned(),
                            expires_at: fact.current_expiry.into(),
                        })
                    }
                })
                .await
        })
        .await
        .map_err(Error::from)
        .and_then(std::convert::identity);
        self.finish(lease, deadline, result).await
    }

    pub async fn accept_invitation(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<(Grant, Assignment), Error> {
        Ok(self
            .app
            .command(
                operation_id,
                Accept {
                    service: self.clone(),
                    actor,
                    input,
                    id: grant::Id::allocate(),
                    assignment_id: assignment::Id::allocate(),
                    locked: Mutex::new(None),
                },
            )
            .await?)
    }

    async fn acceptable_invitation(
        &self,
        client: &Client,
        roles: &role::Reader,
        fact: &database::Fact,
        code: &Code,
        now: DateTime<Utc>,
    ) -> Result<(role::Role, revision::Id), Error> {
        if !fact.open(&self.invitations)?.matches(code) {
            return Err(Error::Facts {
                rule: "located invitation authenticates the submitted code",
            });
        }
        // Validate retained Grant history before interpreting any current unavailability.
        member::find_by_person(client, roles, fact.person_id, now).await?;
        if !fact.available() {
            return Err(Error::InvitationUnavailable);
        }
        let selected =
            role::database::find_by_id(client, roles, fact.role_id, Scope::Workforce, now)
                .await?
                .ok_or(Error::Facts {
                    rule: "available invitation retains its selected role",
                })?;
        let creator = crate::identity::person::state::find(
            client,
            self.app.keys(),
            self.app.anchors(),
            roles.read()?,
            fact.person_id,
            now,
        )
        .await?
        .ok_or(Error::Facts {
            rule: "an invitation retains its creator's permanent person",
        })?;
        Ok((selected, creator.revision_id()))
    }
}

struct Accept {
    service: Service,
    actor: Actor,
    input: Input,
    id: grant::Id,
    assignment_id: assignment::Id,
    locked: Mutex<Option<database::Hint>>,
}

struct Facts {
    invitation: history::Verified<super::state::State>,
    selected: role::Role,
    subject: revision::Identity,
    operator: revision::Identity,
    authorized: bool,
    operation_id: operation::Id,
}

struct Selected {
    change: Change<state::State>,
    invitation: Change<super::state::State>,
    expiry: Expiry,
    grant: Grant,
    assignment: Assignment,
}

impl Command for Accept {
    const COMPLETIONS: completion::Template = grant::command::COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = (Grant, Assignment);
    type Rejection = Rejection;

    fn session(&self) -> Option<(&crate::identity::current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        completion::Request::new("accept", "invitations".to_owned(), &self.input, authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        tx.shared(b"access/scope", b"institution").await?;
        tx.shared(b"access/scope", b"workforce").await?;
        let hint = database::discover_code(
            tx.client(),
            &self.service.invitations.lookup,
            &self.input.code,
        )
        .await?;
        if let Some(hint) = hint {
            match account::state::discover_person(tx.client(), hint.person_id).await? {
                Some(creator) => self.actor.lock_as_target(tx.client(), creator).await?,
                None => self.actor.lock_account(tx.client()).await?,
            }
            self.actor.lock_session(tx.client()).await?;
            state::lock_shared(tx.client(), hint.grant_id).await?;
            tx::object(tx.client(), b"access/invitation", hint.id.as_bytes()).await?;
        } else {
            self.actor.lock_account(tx.client()).await?;
            self.actor.lock_session(tx.client()).await?;
        }
        *self
            .locked
            .lock()
            .map_err(|_| CommandError::corrupt("acceptance retains its lock binding"))? = hint;
        Ok(())
    }

    async fn authorize(
        &self,
        _: &Tx,
        subject: Option<crate::identity::current::database::State>,
    ) -> Result<Authorization, CommandError> {
        let subject = subject.ok_or(CommandError::corrupt(
            "invitation acceptance has a qualified session",
        ))?;
        Ok(Authorization {
            person_id: subject.person_id(),
            revision_id: subject.revision_id(),
            now: subject.observed_at(),
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, CommandError> {
        let client = tx.client();
        let fact = database::find_by_code(
            client,
            &self.service.roles,
            &self.service.invitations,
            &self.input.code,
            authority.now,
        )
        .await?
        .ok_or(Rejection::InvitationUnavailable)?;
        let locked = *self
            .locked
            .lock()
            .map_err(|_| CommandError::corrupt("acceptance retains its lock binding"))?;
        if !locked.is_some_and(|hint| {
            hint.id == fact.state.id()
                && hint.person_id == fact.person_id
                && hint.grant_id == fact.grant_id
        }) {
            return Err(CommandError::lock_set_changed());
        }
        // The object lock makes the database observation decisive for a preceding pending commit.
        let invitation = super::state::find(
            client,
            &self.service.roles,
            fact.state.id(),
            history::Read::Locked,
            authority.now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "a locked invitation retains its state",
        ))?;
        let (selected, operator_id) = self
            .service
            .acceptable_invitation(
                client,
                &self.service.roles,
                &fact,
                &self.input.code,
                authority.now,
            )
            .await
            .map_err(|error| match error {
                Error::InvitationUnavailable => Rejection::InvitationUnavailable.into(),
                error => CommandError::from(error),
            })?;
        let authorized = state::current(
            client,
            &self.service.roles,
            authority.person_id,
            self.service.roles.read()?,
            authority.now,
        )
        .await?
        .is_some();
        Ok(Facts {
            invitation,
            selected,
            authorized,
            operation_id: tx.operation_id(),
            subject: self
                .service
                .revisions
                .fetch_identity(client, authority.revision_id)
                .await?,
            operator: self
                .service
                .revisions
                .fetch_identity(client, operator_id)
                .await?,
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        if facts.selected.change_id() != self.input.change_id {
            return Err(Rejection::RoleChanged.into());
        }
        if facts.authorized {
            return Err(Rejection::AlreadyAuthorized.into());
        }
        let change = transition::first(
            state::Admission {
                id: self.id,
                person_id: facts.subject.person_id(),
                operator_id: facts.operator.revision_id(),
                revision_id: facts.subject.revision_id(),
                entry: None,
                invitation_id: Some(facts.invitation.id()),
                granted_at: now,
                operation_id: facts.operation_id,
            },
            &facts.selected,
            self.assignment_id,
        );
        let invitation = super::transition::accept(
            &facts.invitation,
            self.id,
            facts.operator.revision_id(),
            facts.operation_id,
            now,
        )?;
        let grant = Grant {
            id: self.id,
            subject: facts.subject,
            operator: facts.operator,
            granted_at: now,
            sequence: 1,
            operation_id: facts.operation_id,
            status: Status::Active,
        };
        let assignment = Assignment::first(self.assignment_id, &grant, facts.selected);
        Ok(Selected {
            change,
            invitation,
            expiry: facts.invitation.expires_at(),
            assignment,
            grant,
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Self::Output, CommandError> {
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
        let invitation = selected
            .invitation
            .write(
                tx.client(),
                self.service.app.keys(),
                self.service.app.anchors(),
            )
            .await?;
        match pending {
            Some(pending) => pending.extend(invitation)?,
            None => *pending = Some(invitation),
        }
        if tx.now(&self.service.context).await? >= DateTime::<Utc>::from(selected.expiry) {
            return Err(Rejection::InvitationUnavailable.into());
        }
        Ok((selected.grant, selected.assignment))
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Self::Output, CommandError> {
        let original = grant::database::original(
            tx.client(),
            &self.service.roles,
            tx.operation_id(),
            &self.service.revisions,
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "an acceptance completion retains its original grant",
        ))?;
        let state = state::find(
            tx.client(),
            &self.service.roles,
            original.0.id(),
            history::Read::Shared,
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "an acceptance retains its authenticated grant",
        ))?;
        let id = state.invitation_id().ok_or(CommandError::corrupt(
            "an acceptance fixes its source invitation",
        ))?;
        let acceptance = record::original(
            tx.client(),
            &self.service.roles,
            crate::kernel::operation::Family::Grant,
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "an invitation grant retains its acceptance",
        ))?;
        record::check_acceptance_pair(tx.client(), state.id()).await?;
        if acceptance.invitation_id() != id
            || !matches!(acceptance.event(), record::Event::Acceptance { grant_id } if *grant_id == state.id())
            || original.0.subject().person_id() != request.person_id()
            || original.1.role().change_id() != self.input.change_id
        {
            return Err(CommandError::corrupt(
                "an acceptance agrees with its completed subject and selection",
            ));
        }
        let fact = database::find_by_id(tx.client(), &self.service.roles, id, now)
            .await?
            .ok_or(CommandError::corrupt(
                "an acceptance retains its original invitation",
            ))?;
        if !fact
            .open(&self.service.invitations)?
            .matches(&self.input.code)
        {
            return Err(CommandError::corrupt(
                "an acceptance retains its completed code",
            ));
        }
        Ok(original)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use super::{Input, Search};
    use crate::{
        access::{
            appointment::tests::register_numbered,
            grant::{
                Error, Status,
                invitation::tests::{body, creator, input, later, operation, text, workforce_role},
            },
            role::{Kind, tests::start},
        },
        identity::authentication::{Database, Supplier, server},
    };

    fn command(code: &str, change_id: crate::access::role::change::Id) -> Input {
        serde_json::from_value(json!({"code":code,"changeId":change_id})).expect("confirmed code")
    }

    fn search(code: &str) -> Search {
        serde_json::from_value(json!({"code":code})).expect("code search")
    }

    #[test]
    fn candidate_and_acceptance_preserve_disclosure_pairing_history_and_replays() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let expires_at = later();
                let invitation = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, expires_at)))).await.expect("invitation");
                let code = text(&invitation);
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let holder_identity = Box::pin(services.current().read(&holder)).await.expect("holder identity");
                let creator_identity = Box::pin(services.current().read(&manager)).await.expect("creator identity");
                let before: i64 = observer.client().query_one("SELECT sequence FROM identity.persons WHERE id=$1", &[&holder_identity.person_id()]).await.expect("sequence before").get(0);
                for _ in 0..2 {
                    let candidate = Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await.expect("candidate");
                    assert_eq!(serde_json::to_value(candidate).expect("candidate shape"), json!({"role":{"name":"普通成员","level":1,"changeId":selected.change_id()},"creatorName":"T*","expiresAt":expires_at}));
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.acceptances", &[]).await.expect("candidate consumes nothing").get::<_,i64>(0), 0);
                let wrong = serde_json::from_value(json!(Uuid::new_v4())).expect("other change");
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&code, wrong))).await, Err(Error::RoleChanged)));
                let original = operation();
                let (grant, assignment) = Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, selected.change_id()))).await.expect("accepted");
                assert_eq!(grant.status(), Status::Active);
                assert_eq!(grant.sequence(), 1);
                assert_eq!(grant.subject().revision_id(), holder_identity.revision_id());
                assert_eq!(grant.operator().revision_id(), creator_identity.revision_id());
                assert_eq!(assignment.role().id(), selected.id());
                let row = observer.client().query_one("SELECT admission.person_id, admission.revision_id, admission.operator_id, admission.sequence, admission.entry, admission.invitation_id=acceptance.invitation_id AND EXISTS(SELECT 1 FROM access.completions completion WHERE completion.family='grant' AND completion.operation_id=admission.operation_id AND completion.action='accept') AS completed, acceptance.invitation_id, assignment.id AS assignment_id, assignment.role_id, assignment.sequence = admission.sequence AND assignment.operation_id = admission.operation_id AND assignment.assigned_at = admission.granted_at AND assignment.operator_id = admission.operator_id AND assignment.revision_id = admission.revision_id AS paired, admission.assignment_id = assignment.id AND admission.role_id = assignment.role_id AND admission.termination IS NULL AS current, person.sequence FROM access.grants admission JOIN access.acceptances acceptance ON acceptance.id=admission.id JOIN access.assignments assignment ON assignment.grant_id=admission.id JOIN identity.persons person ON person.id=admission.person_id WHERE admission.operation_id=$1", &[&original]).await.expect("independent complete admission");
                assert_eq!(row.get::<_,crate::identity::person::Id>("person_id"), holder_identity.person_id());
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("revision_id"), holder_identity.revision_id());
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"), creator_identity.revision_id());
                assert_eq!(row.get::<_,crate::access::grant::invitation::Id>("invitation_id"), invitation.id());
                assert_eq!(row.get::<_,crate::access::assignment::Id>("assignment_id"), assignment.id());
                assert_eq!(row.get::<_,crate::access::role::Id>("role_id"), selected.id());
                assert_eq!(row.get::<_,i64>("sequence"), before);
                assert_eq!(row.get::<_,String>("entry"), "invitation");
                assert!(row.get::<_,bool>("paired") && row.get::<_,bool>("current") && row.get::<_,bool>("completed"));
                assert!(matches!(Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await, Err(Error::InvitationUnavailable)));
                let replay = Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, selected.change_id()))).await.expect("full code replay");
                assert_eq!(replay.0.id(), grant.id()); assert_eq!(replay.1.id(), assignment.id());
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, wrong))).await, Err(Error::Mismatch)));
                let unknown = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), original, command(unknown, selected.change_id()))).await, Err(Error::Mismatch)));
                let extra = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 2, later())))).await.expect("available code");
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&text(&extra), selected.change_id()))).await, Err(Error::AlreadyAuthorized)));
                let candidate = Box::pin(services.grant().suspension_candidate(&manager, "13900000000".parse().expect("holder mobile"))).await.expect("suspension target");
                let target = serde_json::from_value(json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13900000000"}})).expect("target");
                Box::pin(services.grant().suspend(manager.clone(), operation(), grant.id(), target)).await.expect("suspended grant");
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&text(&extra), selected.change_id()))).await, Err(Error::AlreadyAuthorized)));
                Box::pin(services.grant().withdraw_invitation(manager.clone(), operation(), invitation.id())).await.expect("full invitation may be withdrawn");
                assert_eq!(Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, selected.change_id()))).await.expect("withdrawn replay").0.id(), grant.id());
                let history = Box::pin(services.grant().list(&manager, serde_json::from_value(json!({})).expect("history filter"))).await.expect("accepted grant history");
                assert_eq!(history.count(), 2);
                let assignments = Box::pin(services.assignment().list(&manager, serde_json::from_value(json!({})).expect("assignment history filter"))).await.expect("accepted first assignment history");
                assert_eq!(assignments.count(), 2);
                Box::pin(services.grant().terminate(holder.clone(), operation(), grant.id())).await.expect("holder exits while suspended");
                assert!(matches!(Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await, Err(Error::InvitationUnavailable)));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.acceptances WHERE invitation_id=$1", &[&invitation.id()]).await.expect("exit never refunds").get::<_,i64>(0),1);
                let joined = Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&text(&extra), selected.change_id()))).await.expect("new admission uses another cumulative unit");
                assert_ne!(joined.0.id(), grant.id());
                assert_eq!(Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, selected.change_id()))).await.expect("replay preserves later membership").0.status(), Status::Terminated);
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                assert_eq!(Box::pin(services.grant().read_latest(&holder)).await.expect("startup and current read accept new source").id(), joined.0.id());
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                observer.terminate().await.expect("observer end"); supplier.stop().await;
            })).await.expect("bounded acceptance");
        }));
    }

    #[test]
    fn the_last_capacity_unit_admits_exactly_one_concurrent_holder() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (first, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let second = Box::pin(register_numbered(&database, &services, &mut supplier, 21)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let invitation = Box::pin(services.grant().create_invitation(manager, operation(), input(&body(&selected, 1, later())))).await.expect("last unit");
                let gate = Arc::new(tokio::sync::Barrier::new(3));
                let mut tasks = Vec::new();
                for holder in [first, second] {
                    let gate = Arc::clone(&gate); let service = services.grant().clone();
                    let input = command(&text(&invitation), selected.change_id());
                    tasks.push(tokio::spawn(async move { gate.wait().await; service.accept_invitation(holder, operation(), input).await }));
                }
                gate.wait().await;
                let mut successes = 0; let mut rejected = 0;
                for task in tasks {
                    match task.await.expect("holder task") {
                        Ok(_) => successes += 1,
                        Err(Error::InvitationUnavailable) => rejected += 1,
                        Err(error) => panic!("unexpected race result: {error:?}"),
                    }
                }
                assert_eq!((successes,rejected),(1,1));
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.acceptances WHERE invitation_id=$1), (SELECT count(*) FROM access.grants WHERE entry='invitation'), (SELECT count(*) FROM access.assignments assignment JOIN access.acceptances acceptance ON acceptance.id=assignment.grant_id) AS assignments", &[&invitation.id()]).await.expect("one lasting result");
                assert_eq!(row.get::<_,i64>(0),1); assert_eq!(row.get::<_,i64>(1),1); assert_eq!(row.get::<_,i64>(2),1);
                observer.terminate().await.expect("observer end");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); supplier.stop().await;
            })).await.expect("bounded race");
        }));
    }

    #[test]
    fn one_holder_accepting_two_creators_concurrently_waits_instead_of_deadlocking() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(
                Duration::from_secs(90),
                Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) =
                        start(database.connections(&database.user), &supplier).await;
                    let (holder, manager) =
                        Box::pin(creator(&database, &services, &mut supplier)).await;
                    let peer =
                        Box::pin(register_numbered(&database, &services, &mut supplier, 24)).await;
                    Box::pin(crate::access::grant::tests::admit(
                        &services,
                        &holder,
                        crate::access::Entry::Institution,
                        "13800000024",
                        Kind::Full,
                    ))
                    .await;
                    let selected = workforce_role(&services, &manager, Kind::Default).await;
                    // The trigger locks the creator shared and the holder exclusive; both
                    // acceptances must take the same modes or one of them deadlocks.
                    for _ in 0..5 {
                        let mut tasks = Vec::new();
                        let gate = Arc::new(tokio::sync::Barrier::new(3));
                        for creator in [&manager, &peer] {
                            let invitation = Box::pin(services.grant().create_invitation(
                                creator.clone(),
                                operation(),
                                input(&body(&selected, 1, later())),
                            ))
                            .await
                            .expect("invitation");
                            let gate = Arc::clone(&gate);
                            let service = services.grant().clone();
                            let holder = holder.clone();
                            let input = command(&text(&invitation), selected.change_id());
                            tasks.push(tokio::spawn(async move {
                                gate.wait().await;
                                service.accept_invitation(holder, operation(), input).await
                            }));
                        }
                        gate.wait().await;
                        let mut accepted = Vec::new();
                        let mut rejected = 0;
                        for task in tasks {
                            match task.await.expect("acceptance task") {
                                Ok((grant, _)) => accepted.push(grant),
                                Err(Error::AlreadyAuthorized) => rejected += 1,
                                Err(error) => {
                                    panic!("concurrent acceptance must wait, not fail: {error:?}")
                                }
                            }
                        }
                        assert_eq!((accepted.len(), rejected), (1, 1));
                        Box::pin(services.grant().terminate(
                            holder.clone(),
                            operation(),
                            accepted[0].id(),
                        ))
                        .await
                        .expect("holder exits before the next round");
                    }
                    services.close();
                    services.drain().await.expect("drain");
                    context.stop().await.expect("stop");
                    supplier.stop().await;
                }),
            )
            .await
            .expect("bounded concurrent acceptances");
        }));
    }

    #[test]
    fn candidates_uniformly_hide_unknown_and_every_unusable_code() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["unknown", "expired", "withdrawn", "full", "admission_lost", "account_suspended"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                    let selected = workforce_role(&services, &manager, Kind::Default).await;
                    let expires = if case == "expired" { chrono::SubsecRound::trunc_subsecs(chrono::Utc::now() + chrono::Duration::seconds(3), 6) } else { later() };
                    let invitation = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, expires)))).await.expect("invitation");
                    let mut code = text(&invitation);
                    match case {
                        "unknown" => code = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
                        "expired" => tokio::time::sleep((expires - chrono::Utc::now()).to_std().expect("future expiry") + Duration::from_millis(1)).await,
                        "withdrawn" => { Box::pin(services.grant().withdraw_invitation(manager.clone(), operation(), invitation.id())).await.expect("withdrawn"); }
                        "full" => { Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&code, selected.change_id()))).await.expect("filled"); }
                        "admission_lost" => {
                            let candidate = Box::pin(services.assignment().candidate(&holder, crate::access::Entry::Institution, "13800000000".parse().expect("mobile"), crate::pagination::Pagination::new(1, 100).expect("page"))).await.expect("member confirmation");
                            let identity = candidate.identity();
                            Box::pin(services.assignment().assign(holder.clone(), operation(), serde_json::from_value(json!({"entry":"institution","target":{"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000","grantId":candidate.grant_id(),"assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("remove authority"))).await.expect("creator loses admission");
                        }
                        "account_suspended" => {
                            let identity = Box::pin(services.current().read(&manager)).await.expect("creator identity");
                            Box::pin(services.suspension().suspend(holder.clone(), operation(), identity.id(), serde_json::from_value(json!({"personId":identity.person_id(),"revisionId":identity.revision_id(),"mobile":"13800000000"})).expect("suspension"))).await.expect("creator suspended");
                        }
                        _ => unreachable!(),
                    }
                    assert!(matches!(Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await, Err(Error::InvitationUnavailable)), "{case}");
                    assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&code, selected.change_id()))).await, Err(Error::InvitationUnavailable)), "{case}");
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.acceptances", &[]).await.expect("no capacity consumed by rejects").get::<_,i64>(0),i64::from(case=="full"));
                    observer.terminate().await.expect("observer end");
                    services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); supplier.stop().await;
                })).await.expect("bounded unavailable case");
            }
        }));
    }

    #[test]
    fn role_definition_changes_require_new_confirmation_and_expiry_keeps_original_result() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = Box::pin(services.role().create_workforce(holder.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Invited","level":100,"permissions":[]})).expect("custom role"))).await.expect("selected role");
                let expires = chrono::SubsecRound::trunc_subsecs(chrono::Utc::now() + chrono::Duration::seconds(4), 6);
                let invitation = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&crate::access::grant::invitation::tests::Choice::from(&selected), 2, expires)))).await.expect("invitation");
                let code = text(&invitation);
                let candidate = Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await.expect("original candidate");
                assert_eq!(candidate.role.change_id, selected.change_id());
                let changed = Box::pin(services.role().update_workforce(holder.clone(), operation(), selected.id(), serde_json::from_value(json!({"entry":"institution","changeId":selected.change_id(),"name":"Revised","level":101,"permissions":[]})).expect("changed definition"))).await.expect("role changed");
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&code, candidate.role.change_id))).await, Err(Error::RoleChanged)));
                let candidate = Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await.expect("new candidate");
                assert_eq!(candidate.role.change_id, changed.change_id());
                let original = operation();
                let (grant, _) = Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, changed.change_id()))).await.expect("confirmed new definition");
                tokio::time::sleep((expires - chrono::Utc::now()).to_std().expect("future expiry") + Duration::from_millis(1)).await;
                assert!(matches!(Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await, Err(Error::InvitationUnavailable)));
                assert_eq!(Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, changed.change_id()))).await.expect("expired replay").0.id(), grant.id());
                assert!(matches!(Box::pin(services.grant().accept_invitation(holder, original, command(&code, selected.change_id()))).await, Err(Error::Mismatch)));
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("restart drain"); context.stop().await.expect("restart stop"); supplier.stop().await;
            })).await.expect("bounded confirmation and expiry");
        }));
    }

    #[test]
    fn acceptance_fixes_current_identity_versions_and_cancellation_never_refunds() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (institution, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let holder = Box::pin(register_numbered(&database, &services, &mut supplier, 23)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let invitation = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, later())))).await.expect("invitation before correction");
                for (actor, name) in [(&manager, "Changed Creator"), (&holder, "Changed Holder")] {
                    let previous = Box::pin(services.current().read(actor)).await.expect("previous version");
                    let candidate = Box::pin(services.correction().candidate_by_person(&institution, previous.person_id())).await.expect("correction confirmation");
                    let input = serde_json::from_value(json!({"target":candidate.target(),"name":name,"idNumber":candidate.id_number()})).expect("corrected name");
                    Box::pin(services.correction().correct(institution.clone(), operation(), input)).await.expect("real correction");
                    assert_ne!(Box::pin(services.current().read(actor)).await.expect("current version").revision_id(), previous.revision_id());
                }
                let manager_identity = Box::pin(services.current().read(&manager)).await.expect("current creator");
                let holder_identity = Box::pin(services.current().read(&holder)).await.expect("current holder");
                let code = text(&invitation);
                assert_eq!(Box::pin(services.grant().invitation_candidate(&holder, search(&code))).await.expect("current masked name").creator_name, "C*");
                let original = operation();
                let (grant, assignment) = Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, selected.change_id()))).await.expect("accepted after both corrections");
                assert_eq!(grant.subject().revision_id(), holder_identity.revision_id());
                assert_eq!(grant.operator().revision_id(), manager_identity.revision_id());
                let observer = database.connections(&database.user).recover().await.expect("observer");
                let row = observer.client().query_one("SELECT admission.revision_id, admission.operator_id, assignment.revision_id=admission.revision_id AND assignment.operator_id=admission.operator_id AS paired FROM access.grants admission JOIN access.assignments assignment ON assignment.id=$2 WHERE admission.id=$1", &[&grant.id(),&assignment.id()]).await.expect("independent current versions");
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("revision_id"), holder_identity.revision_id());
                assert_eq!(row.get::<_,crate::identity::person::revision::Id>("operator_id"), manager_identity.revision_id());
                assert!(row.get::<_,bool>("paired"));
                let cancel = |identity: &crate::identity::current::Account| serde_json::from_value(json!({"accountId":identity.id(),"personId":identity.person_id(),"revisionId":identity.revision_id()})).expect("cancellation confirmation");
                Box::pin(services.cancellation().cancel(manager, operation(), cancel(&manager_identity))).await.expect("creator cancellation");
                assert_eq!(Box::pin(services.grant().read_latest(&holder)).await.expect("accepted grant survives creator cancellation").status(), Status::Active);
                assert_eq!(Box::pin(services.grant().accept_invitation(holder.clone(), original, command(&code, selected.change_id()))).await.expect("replay after creator cancellation").0.id(), grant.id());
                Box::pin(services.cancellation().cancel(holder, operation(), cancel(&holder_identity))).await.expect("holder cancellation");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.acceptances WHERE invitation_id=$1", &[&invitation.id()]).await.expect("cancellation never refunds capacity").get::<_,i64>(0), 1);
                assert!(matches!(Box::pin(services.grant().invitation_candidate(&institution, search(&code))).await, Err(Error::InvitationUnavailable)));
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                services.close(); services.drain().await.expect("restart drain"); context.stop().await.expect("restart stop");
                observer.terminate().await.expect("observer ended"); supplier.stop().await;
            })).await.expect("bounded versions and cancellation");
        }));
    }

    async fn sql_admission(
        client: &tokio_postgres::Client,
        service: &crate::access::grant::Service,
        person: crate::identity::person::Id,
        operator: crate::identity::person::revision::Id,
        role: crate::access::role::Id,
        invitation: crate::access::grant::invitation::Id,
    ) -> crate::history::Verified<crate::access::grant::state::State> {
        crate::access::grant::tests::seed_admission(
            client,
            service,
            person,
            operator,
            role,
            None,
            Some(invitation),
        )
        .await
    }

    #[test]
    fn runtime_and_startup_enforce_acceptance_pairing_capacity_and_immutability() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                let selected = workforce_role(&services, &manager, Kind::Default).await;
                let other = Box::pin(services.role().create_workforce(holder.clone(), operation(), serde_json::from_value(json!({"entry":"institution","name":"Other","level":100,"permissions":[]})).expect("other assignable role"))).await.expect("other role");
                let invitation = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&selected, 1, later())))).await.expect("invitation");
                let person = Box::pin(services.current().read(&holder)).await.expect("holder").person_id();
                let operator = Box::pin(services.current().read(&manager)).await.expect("creator").revision_id();
                let peer = Box::pin(register_numbered(&database, &services, &mut supplier, 24)).await;
                Box::pin(crate::access::grant::tests::admit(&services, &holder, crate::access::Entry::Institution, "13800000024", Kind::Full)).await;
                let other_operator = Box::pin(services.current().read(&peer)).await.expect("another qualified creator").revision_id();
                let runtime = database.connections(&database.user).recover().await.expect("runtime");
                for case in ["valid", "missing acceptance", "wrong role", "wrong creator"] {
                    runtime.client().batch_execute("BEGIN; UPDATE security.clocks SET observed_at=clock_timestamp() WHERE id").await.expect("fresh trusted transaction");
                    let role = if case=="wrong role" { other.id() } else { selected.id() };
                    let responsible = if case=="wrong creator" { other_operator } else { operator };
                    let admission = sql_admission(runtime.client(), services.grant(), person, responsible, role, invitation.id()).await;
                    if case!="missing acceptance" {
                        let original = crate::access::grant::invitation::tests::state(runtime.client(), &services, invitation.id()).await;
                        let change = crate::access::grant::invitation::transition::accept(&original, admission.id(), responsible, admission.operation_id(), admission.granted_at()).expect("consume one slot");
                        crate::history::Seed::write(runtime.client(), services.app().keys(), change).await.expect("paired invitation advancement");
                    }
                    let result = runtime.client().batch_execute("SET CONSTRAINTS ALL IMMEDIATE").await;
                    if case=="valid" { result.expect("valid control"); } else { assert_eq!(result.expect_err(case).code().expect("pairing constraint").code(),if case=="missing acceptance" { "23503" } else { "23514" }, "{case}"); }
                    runtime.client().batch_execute("ROLLBACK").await.expect("rollback test facts");
                }
                let accepted = Box::pin(services.grant().accept_invitation(holder.clone(), operation(), command(&text(&invitation), selected.change_id()))).await.expect("permanent acceptance");
                let full = crate::access::grant::invitation::tests::state(runtime.client(), &services, invitation.id()).await;
                assert!(crate::access::grant::invitation::transition::accept(&full, crate::access::grant::Id::allocate(), operator, operation(), full.changed_at()).is_err(), "a full invitation cannot consume another slot");
                for statement in ["UPDATE access.acceptances SET invitation_id=invitation_id", "DELETE FROM access.acceptances"] {
                    assert_eq!(runtime.client().batch_execute(statement).await.expect_err("runtime cannot mutate").code().expect("privilege").code(),"42501");
                }
                let owner = database.connections("postgres").recover().await.expect("isolated owner");
                for statement in ["UPDATE access.acceptances SET invitation_id=invitation_id", "DELETE FROM access.acceptances"] {
                    assert_eq!(owner.client().batch_execute(statement).await.expect_err("immutable even for owner").code().expect("trigger").code(),"23514");
                }
                let other_invitation = Box::pin(services.grant().create_invitation(manager.clone(), operation(), input(&body(&crate::access::grant::invitation::tests::Choice::from(&other), 2, later())))).await.expect("different role invitation");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated corruption");
                owner.client().execute("UPDATE access.acceptances SET invitation_id=$1 WHERE id=$2", &[&other_invitation.id(),&accepted.0.id()]).await.expect("wrong permanent pairing");
                owner.client().batch_execute("COMMIT").await.expect("corruption committed");
                let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                let result = Box::pin(crate::startup::Services::start(
                    crate::database::Barrier::begin(context.clone()).await.expect("startup barrier"),
                    crate::identity::authentication::inventory(crate::identity::authentication::keys()),
                    crate::startup::Settings::new(4, crate::identity::message::Limits::default(), crate::identity::authentication::session_settings(), crate::registration::Limits::default(), crate::identity::authentication::avatar_settings()).expect("settings"),
                    supplier.client(), crate::identity::authentication::unused_client().await,
                )).await;
                assert!(result.is_err(), "startup rejects a mismatched permanent acceptance");
                context.stop().await.expect("failed context stopped");
                owner.terminate().await.expect("owner end"); runtime.terminate().await.expect("runtime end"); supplier.stop().await;
            })).await.expect("bounded database protections");
        }));
    }

    #[test]
    fn acceptance_recovers_original_facts_across_commit_faults() {
        use fixture::database_proxy::{Fault, Proxy};

        use crate::identity::authentication;

        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["lost", "rollback", "unknown", "cancelled"] {
                timeout(Duration::from_secs(60), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let (holder, manager) = Box::pin(creator(&database, &services, &mut supplier)).await;
                    let selected = workforce_role(&services, &manager, Kind::Default).await;
                    let invitation = Box::pin(services.grant().create_invitation(manager, operation(), input(&body(&selected, 1, later())))).await.expect("invitation");
                    let code = text(&invitation); let original = operation();
                    proxy.arm(match case { "lost" => Fault::LostCommit, "rollback" => Fault::RolledBack, "unknown" => Fault::UnknownCommit, "cancelled" => Fault::DelayedCommit, _ => unreachable!() });
                    let service = services.grant().clone(); let actor = holder.clone(); let input = command(&code, selected.change_id());
                    let waiter = tokio::spawn(async move { service.accept_invitation(actor, original, input).await });
                    timeout(Duration::from_secs(3), proxy.intercepted()).await.expect("commit intercepted");
                    if case == "cancelled" {
                        waiter.abort(); assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
                        services.close(); assert!(timeout(Duration::from_millis(50), services.drain()).await.is_err());
                        proxy.release(); services.drain().await.expect("work drained");
                    } else {
                        proxy.release();
                        let result = timeout(Duration::from_secs(15), waiter).await.expect("bounded recovery").expect("waiter");
                        match case { "rollback" => assert!(matches!(result,Err(Error::Command(crate::kernel::Error::Unavailable(ref fault))) if fault.stage()==crate::kernel::Stage::Commit)), "unknown" => assert!(matches!(result,Err(Error::Command(crate::kernel::Error::Unknown(_))))), _ => { result.expect("recovered success"); } }
                        services.close(); services.drain().await.expect("drain");
                    }
                    context.stop().await.expect("stop");
                    let observer = database.connections(&database.user).recover().await.expect("observer");
                    // An unknown outcome concludes once the recovery BEGIN fails; release only lets the
                    // proxy forward the original COMMIT. Cross the holder's account lock before reading.
                    observer.client().batch_execute("BEGIN").await.expect("observation transaction");
                    observer.client().query_one("SELECT id FROM identity.accounts WHERE id=$1 FOR UPDATE", &[&holder.account_id()]).await.expect("original backend released the holder's account lock");
                    let count = observer.client().query_one("SELECT count(*) FROM access.grants admission JOIN access.acceptances acceptance ON acceptance.id=admission.id JOIN access.assignments assignment ON assignment.grant_id=admission.id WHERE admission.operation_id=$1 AND admission.sequence=1 AND admission.assignment_id=assignment.id AND admission.termination IS NULL", &[&original]).await.expect("independent persistence before replay").get::<_,i64>(0);
                    assert_eq!(count,i64::from(case!="rollback"));
                    observer.client().batch_execute("COMMIT").await.expect("observation complete");
                    proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    Box::pin(services.grant().accept_invitation(holder, original, command(&code, selected.change_id()))).await.expect("replay or explicit retry after proven rollback");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.acceptances", &[]).await.expect("one unit").get::<_,i64>(0),1);
                    observer.terminate().await.expect("observer end");
                    services.close(); services.drain().await.expect("drain"); context.stop().await.expect("stop"); supplier.stop().await;
                })).await.expect("bounded fault case");
            }
        }));
    }
}
