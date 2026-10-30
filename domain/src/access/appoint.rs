//! System appointments use the same Holder transition and executor as online assignments.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    access::{appointment, capability::Scope, holder, role},
    history::{self, Change, Pending, Read, Verified},
    identity::{account, keys, operation, person::revision},
    kernel::{self, App, Authorization, Command, Order, completion, tx::Tx},
};

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Selection {
    Grant,
    Revoke { role_id: role::Id },
}

impl Selection {
    fn action(&self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke { .. } => "revoke",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    operator: Operator,
    account_id: account::Id,
    revision_id: revision::Id,
    selection: Selection,
}

/// A mobile-only operator file; it attributes the action and proves nothing by itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Operator {
    mobile: crate::identity::mobile::Mobile,
}

impl Operator {
    pub(crate) fn mobile(&self) -> &crate::identity::mobile::Mobile {
        &self.mobile
    }

    pub(crate) fn masked(&self) -> String {
        format!(
            "{}****{}",
            &self.mobile.as_ref()[..3],
            &self.mobile.as_ref()[7..]
        )
    }
}

/// A committed or pending system appointment before the executor reports its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completion {
    operation_id: operation::Id,
    completed_at: DateTime<Utc>,
}

impl Completion {
    fn new(operation_id: operation::Id, completed_at: DateTime<Utc>) -> Self {
        Self {
            operation_id,
            completed_at,
        }
    }

    pub fn operation_id(&self) -> operation::Id {
        self.operation_id
    }

    pub fn completed_at(&self) -> DateTime<Utc> {
        self.completed_at
    }
}

impl Input {
    pub fn masked(&self) -> String {
        format!(
            "{} / {} / {}",
            self.operator.masked(),
            self.account_id,
            self.selection.action()
        )
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    Operator,
    Target,
    Changed,
    Role,
    Conflict,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        match self {
            Self::Target => kernel::Status::NotFound,
            Self::Changed | Self::Conflict => kernel::Status::Conflict,
            Self::Operator | Self::Role => kernel::Status::Unprocessable,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::Operator => "operator_unavailable",
            Self::Target => "target_unavailable",
            Self::Changed => "target_changed",
            Self::Role => "role_unavailable",
            Self::Conflict => "state_conflict",
        }
    }
}

pub(crate) async fn run(
    app: App,
    keys: Arc<keys::Keys>,
    operation_id: operation::Id,
    input: Input,
    dry_run: bool,
) -> Result<Option<Completion>, Error> {
    let command = Write {
        app: app.clone(),
        input,
        id: appointment::Id::allocate(),
        accounts: account::Reader::new(app.clone(), Arc::clone(&keys)),
        revisions: revision::Reader::new(app.clone()),
        roles: role::Reader::new(app.clone()),
        keys,
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
    revisions: revision::Reader,
    roles: role::Reader,
    input: Input,
    id: appointment::Id,
}

struct Target {
    subject: account::Subject,
    holder: Verified<holder::State>,
    role: role::Role,
}

struct Facts {
    target: Option<Target>,
    selected: Option<role::Role>,
    operator_id: revision::Id,
    operation_id: operation::Id,
}

struct Selected {
    change: Change<holder::State>,
    completion: Completion,
}

impl Command for Write {
    const COMPLETIONS: completion::Template = appointment::COMPLETIONS;
    const ORDER: Order = Order::OriginalFirst;
    type Facts = Facts;
    type Change = Selected;
    type Output = Completion;
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
        #[derive(Serialize)]
        struct Input<'a> {
            operator: &'a str,
            account_id: account::Id,
            revision_id: revision::Id,
            selection: Selection,
        }
        completion::Request::system(
            self.input.selection.action(),
            self.input.account_id.to_string(),
            &Input {
                operator: self.input.operator.mobile().as_ref(),
                account_id: self.input.account_id,
                revision_id: self.input.revision_id,
                selection: self.input.selection,
            },
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        tx.shared(b"access/scope", b"institution").await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        _: Option<crate::identity::current::database::State>,
    ) -> Result<Authorization, Error> {
        let mobile = self.input.operator.mobile();
        let hash = self
            .keys
            .mobile
            .hash(mobile)
            .map_err(account::ReadError::from)?;
        let operator_id = account::state::discover_mobile(tx.client(), &hash)
            .await?
            .ok_or(Error::Rejected(Rejection::Operator))?;
        // The lookup selects the lock identity. Confirmation below rejects a moved mobile.
        tx.client()
            .query_one(
                "SELECT identity.lock_accounts($1,$2)",
                &[&operator_id, &self.input.account_id],
            )
            .await
            .map_err(Error::query)?;
        let now = tx.now(self.app.context()).await?;
        let operator = self
            .accounts
            .find_by_id(tx.client(), operator_id, mobile, now)
            .await?
            .ok_or(Error::Rejected(Rejection::Operator))?;
        if operator.suspension_id().is_some() {
            return Err(Error::Rejected(Rejection::Operator));
        }
        let holder = history::find::<holder::State>(
            tx.client(),
            self.app.keys(),
            self.app.anchors(),
            Read::Locked,
            operator_id,
            now,
        )
        .await?
        .ok_or(Error::corrupt("a normal operator retains its holder"))?;
        if !holder.active() || holder.person_id() != operator.person_id() {
            return Err(Error::corrupt("a normal operator and its holder agree"));
        }
        Ok(Authorization {
            person_id: operator.person_id(),
            revision_id: operator.revision_id(),
            now,
        })
    }

    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        let now = authority.now;
        let target = match self
            .accounts
            .find_current(tx.client(), self.input.account_id, now)
            .await?
        {
            None => None,
            Some(subject) => {
                let holder = history::find::<holder::State>(
                    tx.client(),
                    self.app.keys(),
                    self.app.anchors(),
                    Read::Locked,
                    self.input.account_id,
                    now,
                )
                .await?
                .ok_or(Error::corrupt("a current target retains its holder"))?;
                if !holder.active() || holder.person_id() != subject.person_id() {
                    return Err(Error::corrupt("a current account and its holder agree"));
                }
                let role = self
                    .roles
                    .find(tx.client(), holder.role_id(), Scope::Institution, now)
                    .await?
                    .ok_or(Error::corrupt("a current holder retains its active role"))?;
                Some(Target {
                    subject,
                    holder,
                    role,
                })
            }
        };
        let selected = match self.input.selection {
            Selection::Grant => {
                Some(role::fetch_administrator(tx.client(), &self.roles, now).await?)
            }
            Selection::Revoke { role_id } => {
                self.roles
                    .find(tx.client(), role_id, Scope::Institution, now)
                    .await?
            }
        };
        Ok(Facts {
            target,
            selected,
            operator_id: authority.revision_id,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, Error> {
        let target = facts.target.ok_or(Error::Rejected(Rejection::Target))?;
        if target.subject.revision_id() != self.input.revision_id {
            return Err(Error::Rejected(Rejection::Changed));
        }
        let role = facts.selected.ok_or(Error::Rejected(Rejection::Role))?;
        match self.input.selection {
            Selection::Grant if target.role.kind() == role::Kind::Full => {
                return Err(Error::Rejected(Rejection::Conflict));
            }
            Selection::Revoke { .. } if target.role.kind() != role::Kind::Full => {
                return Err(Error::Rejected(Rejection::Conflict));
            }
            Selection::Revoke { .. } if role.kind() == role::Kind::Full => {
                return Err(Error::Rejected(Rejection::Role));
            }
            _ => {}
        }
        let change = holder::replace(
            &target.holder,
            &role,
            holder::Assignment {
                id: self.id,
                previous_id: Some(target.holder.assignment_id()),
                change_id: role.change_id(),
                operator_id: facts.operator_id,
                revision_id: target.subject.revision_id(),
                source: holder::Source::System,
            },
            facts.operation_id,
            now,
        )?;
        Ok(Selected {
            change,
            completion: Completion::new(facts.operation_id, now),
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<Pending>,
    ) -> Result<Completion, Error> {
        *pending = Some(
            selected
                .change
                .write(tx.client(), self.app.keys(), self.app.anchors())
                .await?,
        );
        Ok(selected.completion)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Completion, Error> {
        let record =
            appointment::database::original(tx.client(), &self.roles, tx.operation_id(), now)
                .await?
                .ok_or(Error::corrupt(
                    "system appointment retains its original record",
                ))?;
        let assignment = record
            .assignment()
            .ok_or(Error::corrupt("system appointment refers to an assignment"))?;
        if assignment.source != holder::Source::System
            || record.holder_id() != self.input.account_id
            || assignment.revision_id != self.input.revision_id
        {
            return Err(Error::corrupt(
                "system appointment retains its confirmed target and source",
            ));
        }
        let output =
            appointment::database::deliver(tx.client(), &self.roles, &record, &self.revisions, now)
                .await?;
        let selected = match self.input.selection {
            Selection::Grant => output.role().kind() == role::Kind::Full,
            Selection::Revoke { role_id } => {
                output.role().id() == role_id && output.role().kind() != role::Kind::Full
            }
        };
        if !selected || output.operator().person_id() != request.person_id() {
            return Err(Error::corrupt(
                "system appointment retains its selection and operator",
            ));
        }
        self.app.anchors().confirm::<holder::State>(&record)?;
        Ok(Completion::new(tx.operation_id(), output.assigned_at()))
    }
}
