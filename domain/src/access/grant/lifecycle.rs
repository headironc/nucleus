//! Shared qualification and recovery for one Grant's suspension and end lifecycle.

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    access::{
        grant::{
            self, CommandError, Error, Rejection, Service, member,
            record::{Event, Record},
            state, suspension,
            termination::Mode,
            transition,
        },
        role,
    },
    history::{self, Change, Kind as _, Record as _, State as _, Verified},
    identity::{
        account,
        current::{self, Actor},
        operation,
        person::{self, revision},
    },
    kernel::{Authorization, completion, tx::Tx},
};

pub(super) enum Action {
    Suspend {
        grant_id: grant::Id,
        id: suspension::Id,
        target: member::Target,
    },
    Resume {
        id: suspension::Id,
        target: member::Target,
    },
    Terminate {
        grant_id: grant::Id,
        target: Option<member::Target>,
    },
}

impl Action {
    fn target(&self) -> Option<&member::Target> {
        match self {
            Self::Suspend { target, .. } | Self::Resume { target, .. } => Some(target),
            Self::Terminate { target, .. } => target.as_ref(),
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Self::Suspend { .. } => "suspend",
            Self::Resume { .. } => "resume",
            Self::Terminate {
                target: Some(_), ..
            } => "terminate",
            Self::Terminate { target: None, .. } => "leave",
        }
    }
    fn code(&self) -> role::Code {
        match self {
            Self::Suspend { .. } | Self::Resume { .. } => role::Code::WorkforceSuspension,
            Self::Terminate { .. } => role::Code::WorkforceTermination,
        }
    }
    fn path(&self) -> String {
        let bytes = match self {
            Self::Suspend { grant_id, .. } | Self::Terminate { grant_id, .. } => {
                grant_id.as_bytes()
            }
            Self::Resume { id, .. } => id.as_bytes(),
        };
        Uuid::from_bytes(*bytes).to_string()
    }
}

pub(super) enum Kind {
    Suspension(suspension::Id),
    Resumption(suspension::Id),
    Termination(Mode),
}

pub(super) struct Fact {
    pub(super) kind: Kind,
    pub(super) grant_id: grant::Id,
    pub(super) subject: revision::Identity,
    pub(super) operator: revision::Identity,
    pub(super) happened_at: DateTime<Utc>,
    pub(super) sequence: i64,
    pub(super) operation_id: operation::Id,
}

fn identities(record: &Record) -> Result<(Kind, revision::Id, revision::Id), Error> {
    Ok(match record.event() {
        Event::Suspension {
            id,
            operator_id,
            revision_id,
        } => (Kind::Suspension(*id), *operator_id, *revision_id),
        Event::Resumption {
            id,
            operator_id,
            revision_id,
        } => (Kind::Resumption(*id), *operator_id, *revision_id),
        Event::Termination {
            mode,
            operator_id,
            revision_id,
            ..
        } => (Kind::Termination(*mode), *operator_id, *revision_id),
        _ => {
            return Err(Error::Facts {
                rule: "a lifecycle result retains its lifecycle record",
            });
        }
    })
}

fn project(
    record: &Record,
    subject: revision::Identity,
    operator: revision::Identity,
) -> Result<Fact, Error> {
    if subject.person_id() != record.person_id() {
        return Err(Error::Facts {
            rule: "a lifecycle record retains its subject identity",
        });
    }
    Ok(Fact {
        kind: identities(record)?.0,
        grant_id: record.grant_id(),
        subject,
        operator,
        happened_at: record.happened_at(),
        sequence: record.sequence(),
        operation_id: record.operation_id(),
    })
}

pub(super) async fn page(
    service: &Service,
    client: &Client,
    roles: &role::Reader,
    filter: &grant::Filter,
    kind: &'static str,
    now: DateTime<Utc>,
) -> Result<(i64, Vec<Fact>), Error> {
    let predicate = "record.kind=$1 AND ($2::uuid IS NULL OR record.person_id=$2) AND ($3::uuid IS NULL OR record.grant_id=$3) AND ($4::uuid IS NULL OR record.operation_id=$4)";
    let count = client
        .query_one(
            &format!(
                "SELECT count(*) FROM ({}) record WHERE {predicate}",
                Record::SELECT
            ),
            &[
                &kind,
                &filter.person_id,
                &filter.grant_id,
                &filter.operation_id,
            ],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)?;
    let rows = client.query(&format!("{} WHERE {predicate} ORDER BY record.happened_at DESC,record.id DESC LIMIT $5 OFFSET $6", Record::SELECT), &[&kind, &filter.person_id, &filter.grant_id, &filter.operation_id, &filter.pagination.size(), &filter.pagination.offset()]).await?;
    let records = rows
        .iter()
        .map(|row| history::record::<Record>(row, roles.keys(), now))
        .collect::<Result<Vec<_>, _>>()?;
    let persons = records
        .iter()
        .map(|record| record.person_id())
        .collect::<Vec<_>>();
    if kind == "termination" {
        let cancelled = records
            .iter()
            .filter(|record| {
                matches!(
                    record.event(),
                    Event::Termination {
                        mode: Mode::Cancellation,
                        ..
                    }
                )
            })
            .map(|record| record.person_id())
            .collect::<Vec<_>>();
        crate::identity::cancellation::check_retained(
            client,
            &service.app,
            roles.read()?,
            &persons,
            &cancelled,
            now,
        )
        .await?;
    }
    let ids = records
        .iter()
        .map(|record| identities(record).map(|(_, operator, subject)| [operator, subject]))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let fixed = service.revisions.fetch_identities(client, &ids).await?;
    let facts = records
        .iter()
        .map(|record| {
            let (_, operator, subject) = identities(record)?;
            project(
                record,
                fixed
                    .get(&subject)
                    .ok_or(revision::ReadError::Missing)?
                    .clone(),
                fixed
                    .get(&operator)
                    .ok_or(revision::ReadError::Missing)?
                    .clone(),
            )
        })
        .collect::<Result<_, _>>()?;
    Ok((count, facts))
}

pub(super) struct Command {
    pub(super) service: Service,
    pub(super) actor: Actor,
    pub(super) action: Action,
}

pub(super) struct Facts {
    state: Verified<state::State>,
    subject: revision::Identity,
    operator: revision::Identity,
    operation_id: operation::Id,
}

pub(super) struct Selected {
    change: Change<state::State>,
    fact: Fact,
}

impl Command {
    pub(super) fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    pub(super) fn request(
        &self,
        authority: &Authorization,
    ) -> Result<completion::Request, CommandError> {
        #[derive(Serialize)]
        struct Target<'a> {
            account_id: account::Id,
            person_id: person::Id,
            revision_id: revision::Id,
            mobile: &'a str,
        }
        let target = self.action.target().map(|target| Target {
            account_id: target.account_id,
            person_id: target.person_id,
            revision_id: target.revision_id,
            mobile: target.mobile.as_ref(),
        });
        completion::Request::new(self.action.label(), self.action.path(), &target, authority)
    }

    pub(super) async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        tx.shared(b"access/scope", b"workforce").await?;
        match self.action.target() {
            Some(target) => {
                self.actor
                    .lock_accounts(tx.client(), target.account_id)
                    .await?
            }
            None => self.actor.lock_account(tx.client()).await?,
        }
        self.actor.lock_session(tx.client()).await?;
        let id = match &self.action {
            Action::Suspend { grant_id, .. } | Action::Terminate { grant_id, .. } => {
                Some(*grant_id)
            }
            Action::Resume { id, .. } => super::record::suspension_grant(tx.client(), *id).await?,
        };
        if let Some(id) = id {
            state::lock_management(tx.client(), self.actor.account_id(), id).await?;
        } else {
            state::lock_operator(tx.client(), self.actor.account_id()).await?;
        }
        Ok(())
    }

    pub(super) async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, CommandError> {
        let subject = subject.ok_or(CommandError::corrupt(
            "a lifecycle command has a qualified session",
        ))?;
        if self.action.target().is_none() {
            return Ok(Authorization {
                person_id: subject.person_id(),
                revision_id: subject.revision_id(),
                now: subject.observed_at(),
            });
        }
        let (_, person_id, revision_id, now) = member::authorize_subject(
            tx.client(),
            &self.service.roles,
            subject,
            self.action.code(),
        )
        .await?;
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }

    pub(super) async fn load(
        &self,
        tx: &Tx,
        authority: &Authorization,
    ) -> Result<Facts, CommandError> {
        let client = tx.client();
        let roles = &self.service.roles;
        let now = authority.now;
        let (state, revision_id) = if let Some(target) = self.action.target() {
            let manager = member::find_by_person(client, roles, authority.person_id, now)
                .await?
                .ok_or(CommandError::corrupt(
                    "a lifecycle manager retains its current Grant",
                ))?
                .into_role();
            let member = Service::fetch_managed_member(
                client,
                roles,
                &manager,
                authority.person_id,
                target.account_id,
                now,
            )
            .await
            .map_err(|error| match error {
                Error::Absent => Rejection::Absent.into(),
                error => CommandError::from(error),
            })?;
            let identity = self
                .service
                .accounts
                .find_by_id(client, target.account_id, &target.mobile, now)
                .await?
                .ok_or(Rejection::TargetChanged)?;
            if identity.person_id() != target.person_id
                || identity.revision_id() != target.revision_id
                || member.person_id() != target.person_id
            {
                return Err(Rejection::TargetChanged.into());
            }
            if let Action::Suspend { grant_id, .. } | Action::Terminate { grant_id, .. } =
                &self.action
                && member.id() != *grant_id
            {
                return Err(Rejection::TargetChanged.into());
            }
            let state = state::find(client, roles, member.id(), history::Read::Locked, now)
                .await?
                .ok_or(CommandError::corrupt("a managed member retains its Grant"))?;
            (state, target.revision_id)
        } else {
            let Action::Terminate { grant_id, .. } = &self.action else {
                return Err(CommandError::corrupt(
                    "only self termination omits a managed target",
                ));
            };
            let state = state::find(client, roles, *grant_id, history::Read::Locked, now)
                .await?
                .ok_or(Rejection::Absent)?;
            if state.person_id() != authority.person_id {
                return Err(Rejection::Absent.into());
            }
            (state, authority.revision_id)
        };
        Ok(Facts {
            state,
            subject: self
                .service
                .revisions
                .fetch_identity(client, revision_id)
                .await?,
            operator: self
                .service
                .revisions
                .fetch_identity(client, authority.revision_id)
                .await?,
            operation_id: tx.operation_id(),
        })
    }

    pub(super) fn decide(
        &self,
        facts: Facts,
        now: DateTime<Utc>,
    ) -> Result<Selected, CommandError> {
        if !facts.state.unfinished() {
            return Err(Rejection::StateConflict.into());
        }
        let operator = facts.operator.revision_id();
        let subject = facts.subject.revision_id();
        let (change, kind) = match &self.action {
            Action::Suspend { id, .. } => {
                if facts.state.suspension_id().is_some() {
                    return Err(Rejection::StateConflict.into());
                }
                (
                    transition::suspend(
                        &facts.state,
                        *id,
                        operator,
                        subject,
                        facts.operation_id,
                        now,
                    )?,
                    Kind::Suspension(*id),
                )
            }
            Action::Resume { id, .. } => {
                if facts.state.suspension_id() != Some(*id) {
                    return Err(Rejection::StateConflict.into());
                }
                (
                    transition::resume(
                        &facts.state,
                        *id,
                        operator,
                        subject,
                        facts.operation_id,
                        now,
                    )?,
                    Kind::Resumption(*id),
                )
            }
            Action::Terminate { target, .. } => {
                let mode = if target.is_some() {
                    Mode::Manager
                } else {
                    Mode::Current
                };
                (
                    transition::terminate(
                        &facts.state,
                        mode,
                        operator,
                        subject,
                        facts.operation_id,
                        None,
                        now,
                    )?,
                    Kind::Termination(mode),
                )
            }
        };
        Ok(Selected {
            change,
            fact: Fact {
                kind,
                grant_id: facts.state.id(),
                subject: facts.subject,
                operator: facts.operator,
                happened_at: now,
                sequence: facts.state.sequence() + 1,
                operation_id: facts.operation_id,
            },
        })
    }

    pub(super) async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Fact, CommandError> {
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
        Ok(selected.fact)
    }

    pub(super) async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Fact, CommandError> {
        let kind = match self.action {
            Action::Suspend { .. } => "suspension",
            Action::Resume { .. } => "resumption",
            Action::Terminate { .. } => "termination",
        };
        let row = tx
            .client()
            .query_opt(
                &format!(
                    "{} WHERE record.kind=$1 AND record.operation_id=$2",
                    Record::SELECT
                ),
                &[&kind, &tx.operation_id()],
            )
            .await
            .map_err(CommandError::query)?
            .ok_or(CommandError::corrupt(
                "a lifecycle completion retains its original record",
            ))?;
        let record = history::record::<Record>(&row, self.service.app.keys(), now)?;
        let (kind, operator, subject) = identities(&record)?;
        let fact = project(
            &record,
            self.service
                .revisions
                .fetch_identity(tx.client(), subject)
                .await?,
            self.service
                .revisions
                .fetch_identity(tx.client(), operator)
                .await?,
        )?;
        let matches = match (&self.action, kind) {
            (Action::Suspend { grant_id, .. }, Kind::Suspension(_)) => *grant_id == fact.grant_id,
            (Action::Resume { id, .. }, Kind::Resumption(original)) => *id == original,
            (Action::Terminate { grant_id, target }, Kind::Termination(mode)) => {
                *grant_id == fact.grant_id
                    && mode
                        == if target.is_some() {
                            Mode::Manager
                        } else {
                            Mode::Current
                        }
            }
            _ => false,
        };
        if !matches
            || fact.operator.person_id() != request.person_id()
            || self.action.target().is_some_and(|target| {
                target.person_id != fact.subject.person_id()
                    || target.revision_id != fact.subject.revision_id()
            })
        {
            return Err(CommandError::corrupt(
                "a lifecycle completion agrees with its fixed identities, action and target",
            ));
        }
        self.service
            .app
            .anchors()
            .confirm::<state::State>(&record)?;
        Ok(fact)
    }
}
