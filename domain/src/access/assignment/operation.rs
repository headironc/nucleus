//! Replace a member's role through the Grant state transition and shared executor.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_postgres::Client;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::{
        self, Entry, appointment,
        assignment::{Assignment, Error, Id, Service, database},
        capability::Scope,
        grant::{self, member, state, transition},
        role,
    },
    database::transaction,
    history::{self, Change, State as _, Verified},
    identity::{
        account,
        current::{self, Actor},
        mobile::Mobile,
        operation,
        person::{self, revision},
    },
    kernel::{self, Authorization, Command, completion, operation::Family, tx::Tx},
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = assignment::Target)]
pub struct Target {
    pub(super) account_id: account::Id,
    pub(super) person_id: person::Id,
    pub(super) revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    pub(super) mobile: Mobile,
    pub(super) grant_id: grant::Id,
    pub(super) assignment_id: Id,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = assignment::Input)]
pub struct Input {
    pub(super) entry: Entry,
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
            _ => kernel::Status::Conflict,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::RoleChanged => "role_changed",
            Self::Unchanged => "role_unchanged",
        }
    }
}
impl From<Rejection> for CommandError {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}
impl From<CommandError> for Error {
    fn from(value: CommandError) -> Self {
        match value {
            CommandError::Rejected(value) => match value {
                Rejection::Absent => Self::Absent,
                Rejection::TargetChanged => Self::TargetChanged,
                Rejection::RoleChanged => Self::RoleChanged,
                Rejection::Unchanged => Self::Unchanged,
            },
            CommandError::Denied => Self::Denied,
            CommandError::Unauthenticated => Self::Current(current::Error::Invalid),
            CommandError::Mismatch => Self::Mismatch,
            CommandError::Busy => Self::Work(crate::runtime::work::Error::Busy),
            value => Self::Command(value),
        }
    }
}

fn accepts(action: &str, target: &str) -> bool {
    action == "assign" && Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target)
}
pub(in crate::access) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::WorkforceAssignment, accepts);

impl Service {
    pub async fn assign(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Assignment, Error> {
        Ok(self
            .app
            .command(
                operation_id,
                Assign {
                    service: self.clone(),
                    actor,
                    input,
                    id: Id::allocate(),
                },
            )
            .await?)
    }

    pub(super) async fn authorize(
        &self,
        client: &Client,
        roles: &role::Reader,
        actor: &Actor,
        entry: Entry,
    ) -> Result<(role::Role, person::Id, revision::Id, DateTime<Utc>), Error> {
        match entry {
            Entry::Institution => Ok(crate::access::authorize(
                client,
                roles,
                &self.current,
                actor,
                role::Code::InstitutionWorkforce,
            )
            .await?),
            Entry::Domain => Ok(grant::member::authorize(
                client,
                roles,
                &self.current,
                actor,
                role::Code::WorkforceAssignments,
            )
            .await?),
        }
    }

    pub(super) async fn rollback(
        lease: platform::database::Lease,
        cause: Error,
        deadline: Instant,
    ) -> Error {
        let (known, cleanup) =
            transaction::rollback(cause, deadline, lease.client().batch_execute("ROLLBACK"))
                .await
                .recycle(lease)
                .await;
        match known.end {
            Ok(()) => known.value,
            Err(rollback) => Error::Rollback {
                cause: Box::new(known.value),
                rollback,
            },
        }
        .combine(cleanup)
    }
}

struct Assign {
    service: Service,
    actor: Actor,
    input: Input,
    id: Id,
}
struct Facts {
    state: Verified<state::State>,
    before: role::Role,
    selected: role::Role,
    manager: role::Role,
    subject: revision::Identity,
    operator: revision::Identity,
    operation_id: operation::Id,
}
struct Selected {
    change: Change<state::State>,
    assignment: Assignment,
}

impl Command for Assign {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = Assignment;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        #[derive(Serialize)]
        struct Request<'a> {
            entry: &'a str,
            account_id: account::Id,
            person_id: person::Id,
            revision_id: revision::Id,
            mobile: &'a str,
            grant_id: grant::Id,
            assignment_id: Id,
            role_id: role::Id,
            change_id: role::change::Id,
        }
        let target = &self.input.target;
        completion::Request::new(
            "assign",
            Uuid::from_bytes(*target.grant_id.as_bytes()).to_string(),
            &Request {
                entry: self.input.entry.as_ref(),
                account_id: target.account_id,
                person_id: target.person_id,
                revision_id: target.revision_id,
                mobile: target.mobile.as_ref(),
                grant_id: target.grant_id,
                assignment_id: target.assignment_id,
                role_id: self.input.role.id,
                change_id: self.input.role.change_id,
            },
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        tx.shared(b"access/scope", b"institution").await?;
        tx.shared(b"access/scope", b"workforce").await?;
        self.actor
            .lock_accounts(tx.client(), self.input.target.account_id)
            .await?;
        self.actor.lock_session(tx.client()).await?;
        state::lock_management(
            tx.client(),
            self.actor.account_id(),
            self.input.target.grant_id,
        )
        .await?;
        role::database::lock_shared(tx.client(), self.input.role.id, Scope::Workforce).await?;
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, CommandError> {
        let subject = subject.ok_or(CommandError::corrupt("assignment has a qualified session"))?;
        let (_, person_id, revision_id, now) = match self.input.entry {
            Entry::Institution => {
                access::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    role::Code::InstitutionWorkforce,
                )
                .await?
            }
            Entry::Domain => {
                member::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    role::Code::WorkforceAssignments,
                )
                .await?
            }
        };
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, CommandError> {
        let client = tx.client();
        let roles = &self.service.roles;
        let now = authority.now;
        let manager = match self.input.entry {
            Entry::Institution => {
                appointment::find_by_account(client, roles, self.actor.account_id(), now)
                    .await?
                    .ok_or(CommandError::corrupt(
                        "an institutional manager retains its holder",
                    ))?
                    .role()
                    .clone()
            }
            Entry::Domain => member::find_by_person(client, roles, authority.person_id, now)
                .await?
                .ok_or(CommandError::corrupt(
                    "a Workforce manager retains its grant",
                ))?
                .into_role(),
        };
        let member = Service::fetch_target(
            client,
            roles,
            self.input.entry,
            &manager,
            authority.person_id,
            self.input.target.account_id,
            now,
        )
        .await
        .map_err(|error| match error {
            Error::Absent => Rejection::Absent.into(),
            error => CommandError::from(error),
        })?;
        let target = &self.input.target;
        let identity = self
            .service
            .accounts
            .find_by_id(client, target.account_id, &target.mobile, now)
            .await?
            .ok_or(Rejection::TargetChanged)?;
        if identity.person_id() != target.person_id
            || identity.revision_id() != target.revision_id
            || member.id() != target.grant_id
            || member.assignment_id() != target.assignment_id
        {
            return Err(Rejection::TargetChanged.into());
        }
        let selected = roles
            .find(client, self.input.role.id, Scope::Workforce, now)
            .await?
            .ok_or(Rejection::RoleChanged)?;
        let state = state::find(client, roles, member.id(), history::Read::Locked, now)
            .await?
            .ok_or(CommandError::corrupt(
                "a current assignment retains its grant",
            ))?;
        Ok(Facts {
            state,
            before: member.into_role(),
            selected,
            manager,
            operation_id: tx.operation_id(),
            subject: self
                .service
                .revisions
                .fetch_identity(client, target.revision_id)
                .await?,
            operator: self
                .service
                .revisions
                .fetch_identity(client, authority.revision_id)
                .await?,
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        if facts.selected.change_id() != self.input.role.change_id {
            return Err(Rejection::RoleChanged.into());
        }
        if !self.input.entry.can_assign(&facts.manager, &facts.selected) {
            return Err(CommandError::Denied);
        }
        if facts.before.id() == facts.selected.id() {
            return Err(Rejection::Unchanged.into());
        }
        let change = transition::assign(
            &facts.state,
            &facts.before,
            &facts.selected,
            transition::Assignment {
                id: self.id,
                entry: self.input.entry,
                operator_id: facts.operator.revision_id(),
                revision_id: facts.subject.revision_id(),
            },
            facts.operation_id,
            now,
        )?;
        let assignment = Assignment {
            id: self.id,
            grant_id: facts.state.id(),
            subject: facts.subject,
            operator: facts.operator,
            role: facts.selected,
            sequence: facts.state.sequence() + 1,
            assigned_at: now,
            operation_id: facts.operation_id,
        };
        Ok(Selected { change, assignment })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Assignment, CommandError> {
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
        Ok(selected.assignment)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Assignment, CommandError> {
        let record = database::original(tx.client(), &self.service.roles, tx.operation_id(), now)
            .await?
            .ok_or(CommandError::corrupt(
                "an assignment completion retains its original record",
            ))?;
        let grant::record::Event::Assignment {
            previous_id,
            change_id,
            entry,
            revision_id,
            ..
        } = record.event()
        else {
            return Err(CommandError::corrupt(
                "an assignment completion names an assignment",
            ));
        };
        if *previous_id != Some(self.input.target.assignment_id)
            || *change_id != self.input.role.change_id
            || *entry != Some(self.input.entry)
            || *revision_id != self.input.target.revision_id
            || record.person_id() != self.input.target.person_id
            || record.grant_id() != self.input.target.grant_id
            || record.role_id() != self.input.role.id
        {
            return Err(CommandError::corrupt(
                "an assignment retains its completed target and selection",
            ));
        }
        let assignment = database::deliver(
            tx.client(),
            &self.service.roles,
            &record,
            &self.service.revisions,
            now,
        )
        .await?;
        if assignment.operator().person_id() != request.person_id() {
            return Err(CommandError::corrupt(
                "an assignment retains its completed operator",
            ));
        }
        self.service
            .app
            .anchors()
            .confirm::<state::State>(&record)?;
        Ok(assignment)
    }
}
