//! Admission uses the shared executor and a permanent, authenticated original input.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    access::{
        self, Entry, appointment, assignment,
        capability::Scope,
        grant::{
            Confirmation, Error, Grant, Input, Service, Status, database, member, state, transition,
        },
        role,
    },
    history::{self, Change},
    identity::{
        account,
        current::{self, Actor},
        operation,
        person::{self, revision},
        suspension,
    },
    kernel::{self, Authorization, Command, completion, operation::Family, tx::Tx},
};

pub type CommandError = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Absent,
    TargetChanged,
    RoleChanged,
    AccountNotReady,
    AlreadyAuthorized,
    StateConflict,
    InvitationUnavailable,
    Expiry,
    Capacity,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Absent => kernel::Status::NotFound,
            Self::AccountNotReady | Self::Expiry | Self::Capacity => kernel::Status::Unprocessable,
            _ => kernel::Status::Conflict,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::Absent => "target_unavailable",
            Self::TargetChanged => "target_changed",
            Self::RoleChanged => "role_changed",
            Self::AccountNotReady => "account_not_ready",
            Self::AlreadyAuthorized => "already_authorized",
            Self::StateConflict => "state_conflict",
            Self::InvitationUnavailable => "invitation_unavailable",
            Self::Expiry => "expiry_passed",
            Self::Capacity => "capacity_below_usage",
        }
    }
}

impl From<Rejection> for CommandError {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

// Public Grant and invitation methods retain their established error contract.
impl From<CommandError> for Error {
    fn from(value: CommandError) -> Self {
        match value {
            CommandError::Rejected(rejection) => match rejection {
                Rejection::Absent => Self::Absent,
                Rejection::TargetChanged => Self::TargetChanged,
                Rejection::RoleChanged => Self::RoleChanged,
                Rejection::AccountNotReady => Self::AccountNotReady,
                Rejection::AlreadyAuthorized => Self::AlreadyAuthorized,
                Rejection::StateConflict => Self::StateConflict,
                Rejection::InvitationUnavailable => Self::InvitationUnavailable,
                Rejection::Expiry => Self::Expiry,
                Rejection::Capacity => Self::Capacity,
            },
            CommandError::Unauthenticated => Self::Current(current::Error::Invalid),
            CommandError::Denied => Self::Denied,
            CommandError::Mismatch => Self::Mismatch,
            CommandError::Busy => Self::Work(crate::runtime::work::Error::Busy),
            value => Self::Command(value),
        }
    }
}

fn accepts(action: &str, target: &str) -> bool {
    match action {
        "create" => Uuid::parse_str(target).is_ok_and(|id| id.to_string() == target),
        "accept" => target == "invitations",
        _ => false,
    }
}

pub(super) const COMPLETIONS: completion::Template =
    completion::Template::personal(Family::Grant, accepts);

pub(super) async fn create(
    service: &Service,
    actor: Actor,
    operation_id: operation::Id,
    input: Input,
) -> Result<(Grant, assignment::Assignment), Error> {
    Ok(service
        .app
        .command(
            operation_id,
            Create {
                service: service.clone(),
                actor,
                input,
                id: super::Id::allocate(),
                assignment_id: assignment::Id::allocate(),
            },
        )
        .await?)
}

struct Create {
    service: Service,
    actor: Actor,
    input: Input,
    id: super::Id,
    assignment_id: assignment::Id,
}

struct Target {
    subject: Option<revision::Identity>,
    suspended: bool,
    grant: Option<Confirmation>,
    selected: Option<role::Role>,
}

struct Facts {
    target: Option<Target>,
    authority: role::Role,
    operator: revision::Identity,
    operation_id: operation::Id,
}

struct Selected {
    change: Change<state::State>,
    grant: Grant,
    assignment: assignment::Assignment,
}

impl Command for Create {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = Selected;
    type Output = (Grant, assignment::Assignment);
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        #[derive(Serialize)]
        struct Input<'a> {
            entry: &'a str,
            account_id: account::Id,
            person_id: person::Id,
            revision_id: revision::Id,
            mobile: &'a str,
            assignment_id: appointment::Id,
            suspension_id: Option<suspension::Id>,
            grant: Option<Confirmation>,
            role_id: role::Id,
            change_id: role::change::Id,
        }
        let target = &self.input.target;
        completion::Request::new(
            "create",
            Uuid::from_bytes(*target.account_id.as_bytes()).to_string(),
            &Input {
                entry: self.input.entry.as_ref(),
                account_id: target.account_id,
                person_id: target.person_id,
                revision_id: target.revision_id,
                mobile: target.mobile.as_ref(),
                assignment_id: target.assignment_id,
                suspension_id: target.suspension_id,
                grant: target.grant,
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
        state::lock_operator(tx.client(), self.actor.account_id()).await?;
        role::database::lock_shared(tx.client(), self.input.role.id, Scope::Workforce).await?;
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, CommandError> {
        let subject = subject.ok_or(CommandError::corrupt("admission has a qualified session"))?;
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
                    role::Code::WorkforceAdmission,
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
        let mut target = None;
        if let Some(holder) =
            appointment::find_by_account(client, roles, self.input.target.account_id, now).await?
        {
            let grant = database::find_by_person(client, roles, holder.person_id(), now).await?;
            let target_role =
                member::find_by_person(client, roles, holder.person_id(), now).await?;
            let hidden = holder.person_id() == authority.person_id
                || match self.input.entry {
                    Entry::Institution => holder.role().level() >= manager.level(),
                    Entry::Domain => {
                        target_role.is_some_and(|member| member.role().level() >= manager.level())
                    }
                };
            if !hidden {
                let account = self
                    .service
                    .accounts
                    .find_by_id(
                        client,
                        self.input.target.account_id,
                        &self.input.target.mobile,
                        now,
                    )
                    .await?;
                let confirmed = account.as_ref().is_some_and(|account| {
                    account.person_id() == self.input.target.person_id
                        && account.revision_id() == self.input.target.revision_id
                        && holder.id() == self.input.target.assignment_id
                        && account.suspension_id() == self.input.target.suspension_id
                        && grant == self.input.target.grant
                });
                target = Some(Target {
                    subject: if confirmed {
                        Some(
                            self.service
                                .revisions
                                .fetch_identity(client, self.input.target.revision_id)
                                .await?,
                        )
                    } else {
                        None
                    },
                    suspended: account
                        .as_ref()
                        .is_some_and(|account| account.suspension_id().is_some()),
                    grant,
                    selected: if confirmed {
                        roles
                            .find(client, self.input.role.id, Scope::Workforce, now)
                            .await?
                    } else {
                        None
                    },
                });
            }
        }
        Ok(Facts {
            target,
            authority: manager,
            operator: self
                .service
                .revisions
                .fetch_identity(client, authority.revision_id)
                .await?,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        let target = facts.target.ok_or(Rejection::Absent)?;
        let subject = target.subject.ok_or(Rejection::TargetChanged)?;
        let role = target
            .selected
            .filter(|role| role.change_id() == self.input.role.change_id)
            .ok_or(Rejection::RoleChanged)?;
        if !self.input.entry.can_assign(&facts.authority, &role) {
            return Err(CommandError::Denied);
        }
        if target.suspended {
            return Err(Rejection::AccountNotReady.into());
        }
        if target
            .grant
            .is_some_and(|grant| grant.status != Status::Terminated)
        {
            return Err(Rejection::AlreadyAuthorized.into());
        }
        let change = transition::first(
            state::Admission {
                id: self.id,
                person_id: subject.person_id(),
                operator_id: facts.operator.revision_id(),
                revision_id: subject.revision_id(),
                entry: Some(self.input.entry),
                invitation_id: None,
                granted_at: now,
                operation_id: facts.operation_id,
            },
            &role,
            self.assignment_id,
        );
        let grant = Grant {
            id: self.id,
            subject,
            operator: facts.operator,
            granted_at: now,
            sequence: 1,
            operation_id: facts.operation_id,
            status: Status::Active,
        };
        let assignment = assignment::Assignment::first(self.assignment_id, &grant, role);
        Ok(Selected {
            change,
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
        Ok((selected.grant, selected.assignment))
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Self::Output, CommandError> {
        let original = database::original(
            tx.client(),
            &self.service.roles,
            tx.operation_id(),
            &self.service.revisions,
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "an admission completion retains its original grant and assignment",
        ))?;
        if original.0.subject().person_id() != self.input.target.person_id
            || original.0.subject().revision_id() != self.input.target.revision_id
            || original.0.operator().person_id() != request.person_id()
            || original.1.role().id() != self.input.role.id
            || original.1.role().change_id() != self.input.role.change_id
        {
            return Err(CommandError::corrupt(
                "the admission agrees with its completed identity and selection",
            ));
        }
        Ok(original)
    }
}
