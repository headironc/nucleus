//! Role commands decide from sealed state and share transaction and recovery ownership.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{
        self, Entry, appointment,
        capability::Scope,
        grant,
        role::{
            CommandError, Id, Kind, Level, Name, Permissions, Rejection, Role, Service, change,
            database, state, workforce,
        },
    },
    history::{self, Change, Draft, Read, Verified},
    identity::{current::Actor, operation, person::revision},
    kernel::{
        Authorization, Command, completion,
        operation::Family,
        tx::{self, Tx},
    },
};

#[derive(Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = role::Input)]
pub struct Input {
    pub(super) name: Name,
    pub(super) level: Level,
    pub(super) permissions: Permissions,
}

impl Input {
    pub fn new(name: Name, level: Level, permissions: Permissions) -> Self {
        Self {
            name,
            level,
            permissions,
        }
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = role::Update)]
pub struct Update {
    change_id: change::Id,
    name: Name,
    level: Level,
    permissions: Permissions,
}

impl Update {
    pub fn new(change_id: change::Id, input: Input) -> Self {
        Self {
            change_id,
            name: input.name,
            level: input.level,
            permissions: input.permissions,
        }
    }
}

#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = role::Delete)]
pub struct Delete {
    change_id: change::Id,
}

impl Delete {
    pub fn new(change_id: change::Id) -> Self {
        Self { change_id }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Action {
    Create(Input),
    CreateWorkforce(workforce::Input),
    Update {
        id: Id,
        change_id: change::Id,
        input: Input,
    },
    UpdateWorkforce {
        id: Id,
        change_id: change::Id,
        input: workforce::Input,
    },
    Delete {
        id: Id,
        change_id: change::Id,
    },
    DeleteWorkforce {
        id: Id,
        change_id: change::Id,
        entry: Entry,
    },
}

impl Action {
    pub(super) fn request(
        &self,
        authority: &Authorization,
    ) -> Result<completion::Request, CommandError> {
        #[derive(Serialize)]
        struct Input<'a> {
            entry: Option<&'a str>,
            previous_id: Option<change::Id>,
            definition: Option<Definition<'a>>,
        }
        let entry = self.entry();
        completion::Request::new(
            self.name(),
            self.id()
                .map_or_else(|| "roles".to_owned(), |id| id.0.to_string()),
            &Input {
                entry: entry.as_ref().map(AsRef::as_ref),
                previous_id: self.previous(),
                definition: self.definition(),
            },
            authority,
        )
    }

    pub(super) fn scope(&self) -> Scope {
        match self {
            Self::CreateWorkforce(_)
            | Self::UpdateWorkforce { .. }
            | Self::DeleteWorkforce { .. } => Scope::Workforce,
            _ => Scope::Institution,
        }
    }
}

pub(super) enum Receipt {
    Role(Role),
    Change(change::Change),
}

#[derive(Serialize)]
pub(super) struct Definition<'a> {
    pub(super) name: &'a Name,
    pub(super) level: Level,
    pub(super) permissions: &'a [super::Code],
}

impl Action {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::Create(_) | Self::CreateWorkforce(_) => "create",
            Self::Update { .. } | Self::UpdateWorkforce { .. } => "update",
            Self::Delete { .. } | Self::DeleteWorkforce { .. } => "delete",
        }
    }

    pub(super) fn id(&self) -> Option<Id> {
        match self {
            Self::Create(_) | Self::CreateWorkforce(_) => None,
            Self::Update { id, .. }
            | Self::UpdateWorkforce { id, .. }
            | Self::Delete { id, .. }
            | Self::DeleteWorkforce { id, .. } => Some(*id),
        }
    }

    pub(super) fn previous(&self) -> Option<change::Id> {
        match self {
            Self::Create(_) | Self::CreateWorkforce(_) => None,
            Self::Update { change_id, .. }
            | Self::UpdateWorkforce { change_id, .. }
            | Self::Delete { change_id, .. }
            | Self::DeleteWorkforce { change_id, .. } => Some(*change_id),
        }
    }

    pub(super) fn entry(&self) -> Option<Entry> {
        match self {
            Self::CreateWorkforce(input) | Self::UpdateWorkforce { input, .. } => Some(input.entry),
            Self::DeleteWorkforce { entry, .. } => Some(*entry),
            _ => None,
        }
    }

    pub(super) fn definition(&self) -> Option<Definition<'_>> {
        match self {
            Self::Create(input) | Self::Update { input, .. } => Some(Definition {
                name: &input.name,
                level: input.level,
                permissions: &input.permissions.0,
            }),
            Self::CreateWorkforce(input) | Self::UpdateWorkforce { input, .. } => {
                Some(Definition {
                    name: &input.name,
                    level: input.level,
                    permissions: input.permissions.values(),
                })
            }
            Self::Delete { .. } | Self::DeleteWorkforce { .. } => None,
        }
    }
}

fn accepts(action: &str, target: &str) -> bool {
    match action {
        "create" => target == "roles",
        "update" | "delete" => target
            .parse::<Id>()
            .is_ok_and(|id| id.0.to_string() == target),
        _ => false,
    }
}

pub(super) const INSTITUTION: completion::Template =
    completion::Template::personal(Family::InstitutionRole, accepts);
pub(super) const WORKFORCE: completion::Template =
    completion::Template::personal(Family::WorkforceRole, accepts);

pub(crate) async fn verify(
    client: &Client,
    app: &crate::kernel::App,
    roles: &crate::access::role::Reader,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), CommandError> {
    history::start::<state::State>(client, app.keys(), app.anchors(), now).await?;
    let states = roles.all(client, now).await?;
    completion::verify_role_families(client).await?;
    for (scope, template) in [
        (Scope::Institution, INSTITUTION),
        (Scope::Workforce, WORKFORCE),
    ] {
        let completions = completion::fetch_all(client, app.keys(), template).await?;
        let expected = states
            .iter()
            .filter(|state| state.scope() == scope && state.role().kind() == Kind::Custom)
            .try_fold(0_i64, |sum, state| {
                sum.checked_add(history::State::sequence(&**state))
                    .ok_or(CommandError::corrupt("role history count is representable"))
            })?;
        if i64::try_from(completions.len()).ok() != Some(expected) {
            return Err(CommandError::corrupt(
                "every permanent online role change retains its completion",
            ));
        }
        for completion in completions {
            let original = change::original(client, roles, completion.operation_id(), scope, now)
                .await?
                .ok_or(CommandError::corrupt(
                    "role completion retains its original change",
                ))?;
            if !original.completed_by(&completion) {
                return Err(CommandError::corrupt(
                    "role completion and original change agree",
                ));
            }
            let operator = revisions
                .fetch_identity(client, original.operator_id())
                .await?;
            if operator.person_id() != completion.person_id() {
                return Err(CommandError::corrupt(
                    "role completion retains the original operator",
                ));
            }
        }
    }
    Ok(())
}

impl Service {
    pub async fn create(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<Role, CommandError> {
        match self.run(actor, operation_id, Action::Create(input)).await? {
            Receipt::Role(role) => Ok(role),
            Receipt::Change(_) => Err(CommandError::corrupt(
                "a role definition operation returns its definition",
            )),
        }
    }

    pub async fn update(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
        input: Update,
    ) -> Result<Role, CommandError> {
        let action = Action::Update {
            id,
            change_id: input.change_id,
            input: Input::new(input.name, input.level, input.permissions),
        };
        match self.run(actor, operation_id, action).await? {
            Receipt::Role(role) => Ok(role),
            Receipt::Change(_) => Err(CommandError::corrupt(
                "a role definition operation returns its definition",
            )),
        }
    }

    pub async fn delete(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        id: Id,
        input: Delete,
    ) -> Result<change::Change, CommandError> {
        match self
            .run(
                actor,
                operation_id,
                Action::Delete {
                    id,
                    change_id: input.change_id,
                },
            )
            .await?
        {
            Receipt::Change(change) => Ok(change),
            Receipt::Role(_) => Err(CommandError::corrupt(
                "a role deletion returns its terminal change",
            )),
        }
    }

    pub(super) async fn run(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        action: Action,
    ) -> Result<Receipt, CommandError> {
        let role_id = action.id().unwrap_or_else(Id::allocate);
        let change_id = change::Id::allocate();
        match action.scope() {
            Scope::Institution => {
                self.app
                    .command(
                        operation_id,
                        Write::<false> {
                            service: self.clone(),
                            actor,
                            action,
                            role_id,
                            change_id,
                        },
                    )
                    .await
            }
            Scope::Workforce => {
                self.app
                    .command(
                        operation_id,
                        Write::<true> {
                            service: self.clone(),
                            actor,
                            action,
                            role_id,
                            change_id,
                        },
                    )
                    .await
            }
        }
    }
}

struct Write<const WORKFORCE: bool> {
    service: Service,
    actor: Actor,
    action: Action,
    role_id: Id,
    change_id: change::Id,
}

struct Facts {
    target: Option<Verified<state::State>>,
    grants: Vec<(Verified<grant::state::State>, uuid::Uuid)>,
    authority: Role,
    holder_levels: Vec<Level>,
    in_use: bool,
    name_taken: bool,
    operator: revision::Identity,
    operation_id: operation::Id,
}

struct Selected {
    change: Change<state::State>,
    grants: Vec<Change<grant::state::State>>,
    output: Receipt,
}

impl<const WORKFORCE: bool> Write<WORKFORCE> {
    async fn authority(
        &self,
        client: &Client,
        authorization: &Authorization,
    ) -> Result<Role, CommandError> {
        if self.action.entry() == Some(Entry::Domain) {
            Ok(grant::member::find_by_person(
                client,
                &self.service.roles,
                authorization.person_id,
                authorization.now,
            )
            .await?
            .ok_or(CommandError::corrupt(
                "authorized member retains its locked role",
            ))?
            .into_role())
        } else {
            Ok(appointment::find_by_account(
                client,
                &self.service.roles,
                self.actor.account_id(),
                authorization.now,
            )
            .await?
            .ok_or(CommandError::corrupt(
                "authorized account retains its locked role",
            ))?
            .role()
            .clone())
        }
    }
}

impl<const WORKFORCE: bool> Command for Write<WORKFORCE> {
    const COMPLETIONS: completion::Template = if WORKFORCE {
        self::WORKFORCE
    } else {
        INSTITUTION
    };
    type Facts = Facts;
    type Change = Selected;
    type Output = Receipt;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&crate::identity::current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }

    fn request(&self, authority: &Authorization) -> Result<completion::Request, CommandError> {
        self.action.request(authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), CommandError> {
        if self.action.scope() == Scope::Workforce {
            if self.action.entry() == Some(Entry::Institution) {
                tx.shared(b"access/scope", b"institution").await?;
            }
            tx::object(tx.client(), b"access/scope", b"workforce").await?;
        } else {
            tx::object(tx.client(), b"access/scope", b"institution").await?;
        }
        self.actor.lock_account(tx.client()).await?;
        self.actor.lock_session(tx.client()).await?;
        if let Some(id) = self.action.id() {
            if matches!(self.action, Action::UpdateWorkforce { .. })
                && self.action.entry() == Some(Entry::Institution)
            {
                database::lock_holders(tx.client(), id, self.actor.account_id()).await?;
            }
            if self.action.scope() == Scope::Workforce {
                grant::state::lock_role(tx.client(), self.actor.account_id(), id).await?;
            }
            database::lock(tx.client(), id, self.action.scope()).await?;
        } else if self.action.entry() == Some(Entry::Domain) {
            grant::database::lock_operator(tx.client(), self.actor.account_id()).await?;
        }
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<crate::identity::current::database::State>,
    ) -> Result<Authorization, CommandError> {
        let subject = subject
            .ok_or_else(|| CommandError::corrupt("a role command has a qualified session"))?;
        let (_, person_id, revision_id, now) = match self.action.entry() {
            Some(Entry::Domain) => {
                grant::member::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    super::Code::WorkforceRoles,
                )
                .await?
            }
            Some(Entry::Institution) => {
                access::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    super::Code::InstitutionWorkforce,
                )
                .await?
            }
            None => {
                access::authorize_subject(
                    tx.client(),
                    &self.service.roles,
                    subject,
                    super::Code::InstitutionRoles,
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

    async fn load(&self, tx: &Tx, authorization: &Authorization) -> Result<Facts, CommandError> {
        let client = tx.client();
        let target = match self.action.id() {
            Some(id) => {
                history::find::<state::State>(
                    client,
                    self.service.app.keys(),
                    self.service.app.anchors(),
                    Read::Locked,
                    id,
                    authorization.now,
                )
                .await?
            }
            None => None,
        };
        let authority = self.authority(client, authorization).await?;
        let holder_levels = if matches!(self.action, Action::UpdateWorkforce { .. })
            && self.action.entry() == Some(Entry::Institution)
        {
            database::holder_levels(client, &self.service.roles, self.role_id, authorization.now)
                .await?
        } else {
            Vec::new()
        };
        let in_use = if self.action.definition().is_none() {
            database::in_use(client, &self.service.roles, self.role_id, authorization.now).await?
        } else {
            false
        };
        let name_taken = match self.action.definition() {
            Some(definition) => self
                .service
                .roles
                .all(client, authorization.now)
                .await?
                .iter()
                .any(|state| {
                    state.active()
                        && state.scope() == self.action.scope()
                        && state.role().id() != self.role_id
                        && state.role().name() == definition.name
                }),
            None => false,
        };
        let operator = self
            .service
            .revisions
            .fetch_identity(client, authorization.revision_id)
            .await?;
        Ok(Facts {
            target,
            grants: if self.action.scope() == Scope::Workforce && self.action.id().is_some() {
                grant::state::holders(
                    client,
                    &self.service.roles,
                    self.role_id,
                    Read::Locked,
                    authorization.now,
                )
                .await?
                .into_iter()
                .map(|state| (state, uuid::Uuid::new_v4()))
                .collect()
            } else {
                Vec::new()
            },
            authority,
            holder_levels,
            in_use,
            name_taken,
            operator,
            operation_id: tx.operation_id(),
        })
    }

    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<Selected, CommandError> {
        transition(&self.action, self.role_id, self.change_id, facts, now)
    }

    async fn apply(
        &self,
        tx: &Tx,
        selected: Selected,
        pending: &mut Option<history::Pending>,
    ) -> Result<Receipt, CommandError> {
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
        for change in selected.grants {
            let candidate = change
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await?;
            match pending {
                Some(pending) => pending.extend(candidate)?,
                None => *pending = Some(candidate),
            }
        }
        Ok(selected.output)
    }

    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<Receipt, CommandError> {
        let record = change::original(
            tx.client(),
            &self.service.roles,
            tx.operation_id(),
            self.action.scope(),
            now,
        )
        .await?
        .ok_or(CommandError::corrupt(
            "a role completion retains its original change",
        ))?;
        let operator = self
            .service
            .revisions
            .fetch_identity(tx.client(), record.operator_id())
            .await?;
        if operator.person_id() != request.person_id() || !record.agrees(&self.action) {
            return Err(CommandError::corrupt(
                "the original role record agrees with its authenticated completion",
            ));
        }
        self.service
            .app
            .anchors()
            .confirm::<state::State>(&record)?;
        if self.action.scope() == Scope::Workforce {
            grant::record::confirm_losses(
                tx.client(),
                &self.service.roles,
                record.id(),
                tx.operation_id(),
                now,
            )
            .await?;
        }
        if self.action.definition().is_some() {
            Ok(Receipt::Role(record.definition()?))
        } else {
            Ok(Receipt::Change(
                record.deliver(tx.client(), &self.service.revisions).await?,
            ))
        }
    }
}

fn transition(
    action: &Action,
    role_id: Id,
    change_id: change::Id,
    facts: Facts,
    now: DateTime<Utc>,
) -> Result<Selected, CommandError> {
    let cross_scope = action.entry() == Some(Entry::Institution);
    let target = match action.id() {
        Some(_) => {
            let target = facts
                .target
                .as_ref()
                .filter(|state| state.active() && state.scope() == action.scope())
                .ok_or(Rejection::Absent)?;
            if Some(target.role().change_id()) != action.previous() {
                return Err(Rejection::Changed.into());
            }
            Some(target)
        }
        None => None,
    };
    let (role, active, recorded, output) = match action.definition() {
        Some(definition) => {
            if let Some(target) = target {
                if cross_scope
                    && facts
                        .holder_levels
                        .iter()
                        .any(|level| *level >= facts.authority.level())
                {
                    return Err(CommandError::Denied);
                }
                check_update(
                    target.role(),
                    &facts.authority,
                    definition.name,
                    definition.level,
                    definition.permissions,
                    cross_scope,
                )?;
            } else if !(2..=999).contains(&definition.level.0)
                || (!cross_scope
                    && (definition.level >= facts.authority.level()
                        || definition
                            .permissions
                            .iter()
                            .any(|code| !facts.authority.permissions().contains(code))))
            {
                return Err(CommandError::Denied);
            }
            if facts.name_taken {
                return Err(Rejection::NameTaken.into());
            }
            let role = Role {
                id: role_id,
                name: definition.name.clone(),
                level: definition.level,
                kind: Kind::Custom,
                permissions: definition.permissions.to_vec(),
                change_id,
            };
            let recorded = if target.is_some() {
                change::Action::Update {
                    permissions: role.permissions.clone(),
                }
            } else {
                change::Action::Create {
                    permissions: role.permissions.clone(),
                }
            };
            let output = Receipt::Role(role.clone());
            (role, true, recorded, output)
        }
        None => {
            let target = target.ok_or(CommandError::corrupt("deletion has a selected role"))?;
            if target.role().kind() != Kind::Custom
                || (!cross_scope && target.role().level() >= facts.authority.level())
            {
                return Err(CommandError::Denied);
            }
            if facts.in_use {
                return Err(Rejection::InUse.into());
            }
            let output = Receipt::Change(change::Change::deleted(
                change_id,
                target.role(),
                facts.operator.clone(),
                now,
                facts.operation_id,
            ));
            let role = Role {
                change_id,
                permissions: Vec::new(),
                ..target.role().clone()
            };
            (role, false, change::Action::Delete, output)
        }
    };
    if target.is_some_and(|target| now <= target.changed_at()) {
        return Err(CommandError::corrupt(
            "role change time follows the previous definition",
        ));
    }
    let record = change::Data {
        id: change_id,
        previous_id: action.previous(),
        scope: action.scope(),
        entry: action.entry(),
        action: recorded,
        name: role.name.clone(),
        level: role.level,
        operator_id: facts.operator.revision_id(),
        changed_at: now,
        operation_id: facts.operation_id,
    };
    let grants = if action.scope() == Scope::Workforce {
        match target {
            Some(target) if grant::transition::loses_admission(target.role(), &role) => facts
                .grants
                .iter()
                .map(|(grant, id)| {
                    grant::transition::lose(
                        grant,
                        target.role(),
                        &role,
                        *id,
                        facts.operation_id,
                        now,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let data = state::Data {
        role,
        scope: action.scope(),
        active,
        changed_at: now,
    };
    let draft = match target {
        Some(target) => Draft::advance(target, data)?,
        None => Draft::create(data),
    };
    Ok(Selected {
        change: Change::new(draft, record),
        grants,
        output,
    })
}

fn check_update(
    target: &Role,
    authority: &Role,
    name: &Name,
    level: Level,
    permissions: &[super::Code],
    cross_scope: bool,
) -> Result<(), CommandError> {
    if target.kind() != Kind::Custom
        || !(2..=999).contains(&level.0)
        || (!cross_scope
            && (target.level() >= authority.level()
                || level >= authority.level()
                || permissions.iter().any(|code| {
                    !target.permissions().contains(code) && !authority.permissions().contains(code)
                })))
    {
        return Err(CommandError::Denied);
    }
    if target.name() == name && target.level() == level && target.permissions() == permissions {
        return Err(Rejection::Unchanged.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{error, sync::Arc, time::Duration};

    use fixture::database_proxy::{Fault, Proxy};
    use tokio::{runtime::Runtime, time::timeout};
    use tokio_postgres::types::{FromSql, Type};
    use uuid::Uuid;

    use crate::{
        access::role::{
            Code, Delete, Error, Input, InputError, Kind, Level, Name, Rejection, Update,
            operation::Action,
            tests::{actor, assign_role, insert_role, start},
        },
        identity::{
            authentication::{
                self, Database, Supplier, server,
                session::{
                    Receipt,
                    tests::{CODE, message},
                },
            },
            current,
            person::revision,
        },
        kernel,
        runtime::work::Failure,
    };

    #[test]
    fn original_role_changes_reject_missing_operator_revisions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let mut failures = Vec::new();
            for action in ["create", "update", "delete"] {
                for recovering in [false, true] {
                    timeout(Duration::from_secs(30), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let proxy = Proxy::start(&server).await;
                        let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                        let manager = actor(&database, &services, &mut supplier).await;
                        let operator = services.current().read(&manager).await.expect("qualified original operator");
                        let input = || serde_json::from_value(serde_json::json!({"name":"Retained definition","level":20,"permissions":[]})).expect("role input");
                        let command = if action == "create" {
                            Action::Create(input())
                        } else {
                            let role = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("setup operation"), serde_json::from_value(serde_json::json!({"name":"Before change","level":10,"permissions":[]})).expect("initial definition"))).await.expect("actual initial role");
                            if action == "update" { Action::Update { id: role.id(), change_id: role.change_id(), input: input() } }
                            else { Action::Delete { id: role.id(), change_id: role.change_id() } }
                        };
                        let operation_id = Uuid::new_v4().to_string().parse().expect("operation");
                        let result = if recovering {
                            Box::pin(crate::recovery::after_revision_loss(&database, &proxy, &manager,
                                services.role().run(manager.clone(), operation_id, command.clone()))).await
                        } else {
                            Box::pin(services.role().run(manager.clone(), operation_id, command.clone())).await.expect("actual committed change");
                            let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated missing reference");
                            assert_eq!(owner.client().execute("DELETE FROM identity.revisions WHERE id=$1", &[&operator.revision_id()]).await.expect("remove the exact operator revision"), 1);
                            owner.client().batch_execute("COMMIT").await.expect("damage committed");
                            owner.terminate().await.expect("owner ended");
                            Box::pin(services.role().run(manager.clone(), operation_id, command.clone())).await
                        };
                        let classified = match &result {
                            Err(kernel::Error::Unknown(fault)) if recovering => fault.later().any(|(stage, cause)| stage == kernel::Stage::Recovery && matches!(cause, kernel::FaultKind::Revision(revision::ReadError::Missing))),
                            Err(error) if !recovering => error.is_corrupt(),
                            _ => false,
                        };
                        let closed = services.ready().await.is_err();
                        if !classified || !closed {
                            failures.push(format!("{action}, recovering={recovering}, closed={closed}: {:?}", result.as_ref().err()));
                        }
                        if closed {
                            assert!(matches!(Box::pin(services.role().run(manager, operation_id, command)).await,
                                Err(error) if error.is_closed()), "later work is not accepted");
                        }
                        services.close(); services.drain().await.expect("drained");
                        let observer = database.connections(&database.user).recover().await.expect("independent durable facts");
                        let row = observer.client().query_one("SELECT change.action, (SELECT count(*) FROM access.changes WHERE role_id=change.role_id) AS changes, (SELECT count(*) FROM access.roles WHERE id=change.role_id AND change_id=change.id) AS current, NOT EXISTS(SELECT 1 FROM identity.revisions WHERE id=change.operator_id) AS missing FROM access.changes change WHERE change.operation_id=$1", &[&operation_id]).await.expect("retained original is not an absent operation");
                        assert_eq!(row.get::<_, String>("action"), action);
                        assert_eq!(row.get::<_, i64>("changes"), if action == "create" { 1 } else { 2 });
                        assert_eq!(row.get::<_, i64>("current"), 1, "deletion retains its sealed tombstone");
                        assert!(row.get::<_, bool>("missing"));
                        observer.terminate().await.expect("observer ended");
                        context.stop().await.expect("stopped"); proxy.stop().await;
                        assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                    })).await.expect("damaged original role change deadline");
                }
            }
            assert!(failures.is_empty(), "damaged originals were not contained: {failures:?}");
        }));
    }

    #[test]
    fn accepted_role_changes_recover_after_session_revocation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            let mut failures = Vec::new();
            for action in ["create", "update", "delete"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context, services) = start(authentication::proxied(&database, &proxy), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let operator = services.current().read(&manager).await.expect("qualified operator");
                    let input = || serde_json::from_value(serde_json::json!({"name":"Recovered definition","level":20,"permissions":[]})).expect("role input");
                    let command = if action == "create" {
                        Action::Create(input())
                    } else {
                        let role = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("setup operation"), serde_json::from_value(serde_json::json!({"name":"Before recovery","level":10,"permissions":[]})).expect("initial definition"))).await.expect("actual initial role");
                        if action == "update" { Action::Update { id: role.id(), change_id: role.change_id(), input: input() } }
                        else { Action::Delete { id: role.id(), change_id: role.change_id() } }
                    };
                    let operation = Uuid::new_v4().to_string().parse().expect("operation");
                    let result = Box::pin(crate::recovery::after_revocation(&database, &proxy, services.revocation(), &manager, services.role().run(manager.clone(), operation, command.clone()))).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent facts");
                    let row = observer.client().query_one("SELECT change.id,change.role_id,change.action,operator.person_id,role.active,role.permissions FROM access.changes change JOIN identity.revisions operator ON operator.id=change.operator_id JOIN access.roles role ON role.id=change.role_id AND role.change_id=change.id WHERE change.operation_id=$1", &[&operation]).await.expect("one permanent original change and retained state");
                    assert_eq!(row.get::<_,String>("action"), action);
                    assert_eq!(row.get::<_,crate::identity::person::Id>("person_id"), operator.person_id());
                    assert_eq!(row.get::<_,bool>("active"), action != "delete");
                    assert!(row.get::<_,Vec<String>>("permissions").is_empty());
                    match result {
                        Ok(super::Receipt::Role(role)) => { assert_eq!(role.id(), row.get("role_id")); assert_eq!(role.change_id(), row.get("id")); }
                        Ok(super::Receipt::Change(change)) => { assert_eq!(action, "delete"); assert_eq!(change.id(), row.get("id")); }
                        Err(error) => failures.push(format!("{action}: {error:?}")),
                    }
                    assert!(matches!(Box::pin(services.role().run(manager, operation, command)).await, Err(kernel::Error::Unauthenticated)), "a new request still requires the current session");
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1", &[&operation]).await.expect("no replay writes").get::<_,i64>(0), 1);
                    services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped"); proxy.stop().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("permanent history survives restart");
                    services.close(); services.drain().await.expect("restart drained"); context.stop().await.expect("restart stopped");
                    observer.terminate().await.expect("observer ended");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
                })).await.expect("role recovery after session loss deadline");
            }
            assert!(failures.is_empty(), "accepted changes lost their results: {failures:?}");
        }));
    }

    #[test]
    fn deletion_recovery_preserves_terminal_facts_absence_and_actual_cleanup_failures() {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(Box::pin(async {
            for fault in [Fault::LostReadCommit,Fault::ReadCommitTermination,Fault::StalledRoleOriginal,Fault::RolledBack] {
            timeout(Duration::from_secs(20),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;let mut supplier=Supplier::new().await;let proxy=Proxy::start(&server).await;
                let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;let actor=actor(&database,&services,&mut supplier).await;
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Original deletion","level":200,"permissions":["institution.accounts"]})).expect("input");
                let created=Box::pin(services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("creation"),input)).await.expect("actual target");
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                let operation=Uuid::nil().to_string().parse().expect("fixed original operation");let id=created.id();let confirmation=created.change_id();
                let rolled_back=matches!(fault,Fault::RolledBack);let incomplete=matches!(fault,Fault::StalledRoleOriginal);let lost=matches!(fault,Fault::LostReadCommit);
                proxy.arm(if rolled_back {Fault::RolledBack}else{Fault::DelayedCommit});
                let service=services.role().clone();let current=actor.clone();
                let task=tokio::spawn(async move {service.delete(current,operation,id,Delete::new(confirmation)).await});
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("actual terminal write reached COMMIT");
                let mut backends=Vec::<i32>::new();
                if !rolled_back {
                    let row=timeout(Duration::from_secs(3),async {loop {
                        if let Some(row)=observer.client().query_opt("SELECT holder.pid,waiter.pid FROM pg_locks waiter JOIN pg_locks holder USING(locktype,database,classid,objid,objsubid) WHERE waiter.locktype='advisory' AND NOT waiter.granted AND holder.granted AND waiter.mode='ExclusiveLock' AND holder.mode='ExclusiveLock' AND waiter.classid=602503669::oid AND waiter.objid=1284585832::oid AND waiter.objsubid=1 AND waiter.database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND holder.pid=ANY(pg_blocking_pids(waiter.pid))",&[]).await.expect("independent exact Role operation wait") {break row;}tokio::task::yield_now().await;
                    }}).await.expect("readback waits behind the original backend");backends=vec![row.get(0),row.get(1)];
                    assert!(observer.client().query_one("SELECT EXISTS(SELECT 1 FROM access.roles WHERE id=$1) AND NOT EXISTS(SELECT 1 FROM access.changes WHERE operation_id=$2)",&[&id,&operation]).await.expect("uncommitted deletion remains invisible").get::<_,bool>(0));
                    proxy.arm(fault);proxy.release();timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("readback fault hit actual consumer");
                }
                let result=timeout(Duration::from_secs(13),async {tokio::pin!(task);loop {tokio::select! {
                    result=&mut task=>break result.expect("accepted deletion task"),
                    request=timeout(Duration::from_secs(4),supplier.request())=>assert!(request.is_err(),"no Siteverify while deletion settles"),
                }}}).await.expect("original recovery deadline");
                let row=observer.client().query_one("SELECT active,(SELECT count(*) FROM access.permissions WHERE role_id=$1) AS permissions,(SELECT count(*) FROM access.changes WHERE role_id=$1 AND action='delete') AS deleted,(SELECT count(*) FROM access.entries entry JOIN access.changes change ON change.id=entry.change_id WHERE change.role_id=$1 AND change.action='delete') AS entries FROM access.roles WHERE id=$1",&[&id]).await.expect("independent retained state and complete effects before any replay");
                assert_eq!(row.get::<_,bool>("active"),rolled_back);assert_eq!(row.get::<_,i64>("permissions"),i64::from(rolled_back));assert_eq!(row.get::<_,i64>("deleted"),i64::from(!rolled_back));assert_eq!(row.get::<_,i64>("entries"),0);
                if rolled_back {assert!(matches!(result,Err(kernel::Error::Unavailable(ref fault)) if fault.stage()==kernel::Stage::Commit),"{:?}",result.as_ref().err());}
                else if incomplete {assert!(matches!(&result,Err(kernel::Error::Unknown(fault)) if fault.later().any(|(stage,cause)| stage==kernel::Stage::Recovery && matches!(cause,kernel::FaultKind::Timeout(_)))),"{:?}",result.as_ref().err());services.role().app.registry().ready().expect("ordinary read deadline does not close acceptance");}
                else {assert!(result.is_ok(),"{:?}",result.as_ref().err());}
                proxy.release();
                timeout(Duration::from_secs(3),async {loop {
                    if observer.client().query_one("SELECT NOT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=ANY($1::int[]))",&[&backends]).await.expect("actual backend termination").get::<_,bool>(0) {break;}tokio::task::yield_now().await;
                }}).await.expect("old drivers and actual backends ended");
                if !rolled_back {
                    if lost {proxy.arm(Fault::LostReadCommit);}
                    let replay=Box::pin(services.role().delete(actor,operation,id,Delete::new(confirmation))).await.expect("complete first original read remains known even if its COMMIT reply is lost");
                    if let Ok(original)=result {assert_eq!(replay.id(),original.id());}
                    assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&operation]).await.expect("no replay writes").get::<_,i64>(0),1);
                }
                services.close();services.drain().await.expect("drained");observer.terminate().await.expect("observer ended");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;proxy.stop().await;
            })).await.expect("bounded deletion recovery case");
            }
        }));
    }

    #[test]
    fn runtime_role_tombstones_and_immutable_facts_use_table_permissions() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let input = |name: &str| serde_json::from_value(serde_json::json!({"name":name,"level":200,"permissions":["institution.accounts"]})).expect("role input");
                    let created = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("creation"), input("Retained role"))).await.expect("actual role creation");
                    let updated = Box::pin(services.role().update(manager.clone(), Uuid::new_v4().to_string().parse().expect("modification"), created.id(), Update::new(created.change_id(), input("Updated role")))).await.expect("actual role modification");
                    let deleted = Box::pin(services.role().delete(manager, Uuid::new_v4().to_string().parse().expect("deletion"), updated.id(), Delete::new(updated.change_id()))).await.expect("actual role deletion");
                    let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let row = observer.client().query_one("SELECT active,permissions,change_id,sequence,version,(SELECT count(*) FROM access.changes WHERE role_id=role.id) AS records FROM access.roles role WHERE id=$1", &[&created.id()]).await.expect("permanent terminal state");
                    assert!(!row.get::<_, bool>("active"));
                    assert!(row.get::<_, Vec<String>>("permissions").is_empty());
                    assert_eq!(row.get::<_, crate::access::role::change::Id>("change_id"), deleted.id());
                    assert_eq!(row.get::<_, i64>("sequence"), 3);
                    assert_eq!(row.get::<_, i64>("version"), 3);
                    assert_eq!(row.get::<_, i64>("records"), 3);
                    for (table, required, forbidden) in [
                        ("access.roles", &["SELECT", "INSERT", "UPDATE"][..], &["DELETE"][..]),
                        ("access.changes", &["SELECT", "INSERT"][..], &["UPDATE", "DELETE"][..]),
                        ("access.completions", &["SELECT", "INSERT"][..], &["UPDATE", "DELETE"][..]),
                        ("access.permissions", &["SELECT"][..], &["INSERT", "UPDATE", "DELETE"][..]),
                        ("access.entries", &["SELECT"][..], &["INSERT", "UPDATE", "DELETE"][..]),
                    ] {
                        for (privileges, expected) in [(required, true), (forbidden, false)] {
                            for privilege in privileges {
                                assert_eq!(writer.client().query_one("SELECT has_table_privilege(current_user,$1,$2)", &[&table, privilege]).await.expect("effective runtime table ACL").get::<_, bool>(0), expected, "{table}: {privilege}");
                            }
                        }
                    }
                    let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("complete terminal facts").get(0);
                    for statement in [
                        "DELETE FROM access.roles",
                        "UPDATE access.changes SET name='Mutation'",
                        "DELETE FROM access.changes",
                        "UPDATE access.completions SET target='Mutation'",
                        "DELETE FROM access.completions",
                        "INSERT INTO access.permissions(role_id,scope,capability_id) SELECT id,scope,'institution.roles' FROM access.roles",
                        "UPDATE access.permissions SET capability_id='institution.roles'",
                        "DELETE FROM access.permissions",
                        "INSERT INTO access.entries(change_id,scope,capability_id) SELECT id,scope,'institution.roles' FROM access.changes",
                        "UPDATE access.entries SET capability_id='institution.roles'",
                        "DELETE FROM access.entries",
                    ] {
                        writer.client().batch_execute("BEGIN").await.expect("isolated forbidden mutation");
                        let error = writer.client().batch_execute(statement).await.expect_err("runtime cannot mutate permanent facts or derived views");
                        // PostgreSQL may reject a derived view during rewrite before checking its ACL.
                        assert!(matches!(error.code(), Some(code) if code == &tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE || code == &tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE), "{statement}: {error:?}");
                        writer.client().batch_execute("ROLLBACK").await.expect("failed mutation ended");
                        assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("independent complete rollback").get::<_, String>(0), before, "{statement}");
                    }
                    writer.terminate().await.expect("writer ended");
                    observer.terminate().await.expect("observer ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                })).await.expect("bounded runtime ACL and tombstone checks");
            }));
    }

    #[test]
    fn deletion_rejects_corrupt_fixed_operators_and_closes_accepted_writes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for corruption in ["authentication","structure","canonical"] {
            timeout(Duration::from_secs(15),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Fixed operator","level":200,"permissions":[]})).expect("definition");
                let created=Box::pin(services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("creation"),input)).await.expect("actual target");
                let observer=database.connections(&database.user).recover().await.expect("observer");
                let ordinary=observer.client().query_one("SELECT 1/0",&[]).await.expect_err("ordinary query failure");
                assert!(!Error::Revision(revision::ReadError::from(ordinary)).unreliable());
                let owner=database.connections("postgres").recover().await.expect("isolated damaged version fixture");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("privileged restore only");
                if corruption=="canonical" {
                    struct Input(revision::Id);
                    impl platform::cryptography::envelope::Binding for Input {
                        fn domain(&self)->&[u8] {b"nucleus/identity/envelope/v1"}
                        fn identity(&self)->&[u8] {self.0.as_bytes()}
                        fn field(&self)->&[u8] {b"revision.id_number"}
                    }
                    impl platform::cryptography::envelope::Input for Input {
                        fn plaintext(&self)->&[u8] {b"00000020000229007x"}
                    }
                    let id=owner.client().query_one("SELECT revision_id FROM identity.persons",&[]).await.expect("actual fixed version").get(0);
                    let envelope=authentication::keys().encryption.seal(&Input(id)).expect("authentic but noncanonical stored plaintext");
                    owner.client().execute("UPDATE identity.revisions SET id_number=$1",&[&envelope.as_ref()]).await.expect("privileged invalid plaintext fixture");
                } else {
                    let offset=if corruption=="structure" {0}else{54};
                    owner.client().execute("UPDATE identity.revisions SET id_number=set_byte(id_number,$1,get_byte(id_number,$1)#255)",&[&offset]).await.expect("valid name but damaged stored identity number");
                }
                owner.client().batch_execute("COMMIT").await.expect("damage visible");owner.terminate().await.expect("owner ended");
                let statement="SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
                let before:String=observer.client().query_one(statement,&[]).await.expect("independent graph before failed deletion").get(0);
                let operation=Uuid::new_v4().to_string().parse().expect("deletion");
                let error=Box::pin(services.role().delete(actor.clone(),operation,created.id(),Delete::new(created.change_id()))).await.err().expect("full historical identity must validate before commit");
                let kernel::Error::Corrupt(fault)=&error else {panic!("damaged fixed identity rejects before writing: {error:?}")};
                if corruption=="structure" {assert!(matches!(fault.kind(),kernel::FaultKind::Identity(crate::identity::current::Error::Decode(_))),"{error:?}");}
                else if corruption=="canonical" {assert!(matches!(fault.kind(),kernel::FaultKind::Identity(crate::identity::current::Error::Revision(revision::Error::Canonical))),"{error:?}");}
                else {assert!(matches!(fault.kind(),kernel::FaultKind::Identity(crate::identity::current::Error::Revision(revision::Error::Encryption(_)))),"{error:?}");}
                assert!(error.unreliable());assert!(services.role().app.registry().ready().is_err());
                let after:String=observer.client().query_one(statement,&[]).await.expect("independent graph before any retry").get(0);assert_eq!(after,before);
                assert!(matches!(Box::pin(services.role().delete(actor,operation,created.id(),Delete::new(created.change_id()))).await,Err(error) if error.is_closed()));
                services.close();services.drain().await.expect("drained");observer.terminate().await.expect("observer ended");context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded fixed version failure");
            }
        }));
    }

    #[test]
    fn deletion_preserves_original_operations_without_current_definitions_or_holders() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(25),Box::pin(async {
                let database=Database::initialized(server).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let creation=Uuid::new_v4().to_string().parse().expect("creation");
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Original","level":200,"permissions":["institution.accounts"]})).expect("definition");
                let created=Box::pin(services.role().create(actor.clone(),creation,input.clone())).await.expect("creation");
                let modification=Uuid::new_v4().to_string().parse().expect("modification");
                let replacement:Input=serde_json::from_value(serde_json::json!({"name":"Reusable","level":201,"permissions":[]})).expect("replacement");
                let updated=Box::pin(services.role().update(actor.clone(),modification,created.id(),Update::new(created.change_id(),replacement.clone()))).await.expect("modification");
                let operation=Uuid::new_v4().to_string().parse().expect("deletion");
                assert!(matches!(Box::pin(services.role().delete(actor.clone(),operation,created.id(),Delete::new(created.change_id()))).await,Err(kernel::Error::Rejected(Rejection::Changed))));
                let id=message(&database,services.authentication(),&mut supplier,"13800138000").await;
                let Receipt::Registration(credential)=Box::pin(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(),id,"13800138000",CODE))).await.expect("R1") else {panic!("registration credential")};
                let issued=Box::pin(services.registration().register_input_for_test(crate::registration::Input::new(Uuid::new_v4().to_string().parse().expect("registration"),credential.id(),credential.token().text().parse().expect("token"),"Other Operator".parse().expect("name"),"00000020000229007X".parse().expect("number")))).await.expect("R2");
                let other=services.current().authenticate(current::Proof::Cookie {id:issued.id(),token:issued.token().text().parse().expect("token")}).await.expect("ordinary actor");
                assert!(matches!(Box::pin(services.role().delete(other.clone(),operation,created.id(),Delete::new(updated.change_id()))).await,Err(kernel::Error::Denied)));
                let default=services.role().read_current(&other).await.expect("initial default role");
                let confirmation=|candidate:&crate::access::appointment::Candidate,selected:&crate::access::role::Role| -> crate::access::appointment::Input {
                    serde_json::from_value(serde_json::json!({"target":{"accountId":candidate.identity().id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id(),"mobile":"13800138000","assignmentId":candidate.assignment_id()},"role":{"id":selected.id(),"changeId":selected.change_id()}})).expect("confirmed appointment input")
                };
                let candidate=services.appointment().candidate(&actor,"13800138000".parse().expect("mobile")).await.expect("current target");
                Box::pin(services.appointment().assign(actor.clone(),Uuid::new_v4().to_string().parse().expect("appointment operation"),confirmation(&candidate,&updated))).await.expect("real appointment retains its completion");
                assert!(matches!(Box::pin(services.role().delete(actor.clone(),operation,created.id(),Delete::new(updated.change_id()))).await,Err(kernel::Error::Rejected(Rejection::InUse))));
                let observer=database.connections(&database.user).recover().await.expect("independent runtime observer");
                let statement="SELECT jsonb_build_object('role',(SELECT to_jsonb(role) FROM access.roles role WHERE id=$1),'changes',(SELECT jsonb_agg(to_jsonb(change) ORDER BY changed_at,id) FROM access.changes change WHERE role_id=$1),'entries',(SELECT jsonb_agg(to_jsonb(entry) ORDER BY change_id,capability_id) FROM access.entries entry JOIN access.changes change ON change.id=entry.change_id WHERE change.role_id=$1),'permissions',(SELECT jsonb_agg(to_jsonb(permission) ORDER BY capability_id) FROM access.permissions permission WHERE role_id=$1))::text";
                let before:String=observer.client().query_one(statement,&[&created.id()]).await.expect("holder rejection preserved graph").get(0);
                let writer=database.connections(&database.user).recover().await.expect("direct runtime writer");
                writer.client().batch_execute("BEGIN").await.expect("direct deletion");
                let error=writer.client().execute("DELETE FROM access.roles WHERE id=$1",&[&created.id()]).await.expect_err("runtime credentials cannot erase a role");
                let failure=error.as_db_error().expect("database failure");
                assert_eq!(failure.code(),&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE);
                writer.client().batch_execute("ROLLBACK").await.expect("no terminal candidate retained");
                assert_eq!(observer.client().query_one(statement,&[&created.id()]).await.expect("independent failed group").get::<_,String>(0),before);
                writer.terminate().await.expect("writer ended");
                let candidate=services.appointment().candidate(&actor,"13800138000".parse().expect("mobile")).await.expect("fresh target");
                Box::pin(services.appointment().assign(actor.clone(),Uuid::new_v4().to_string().parse().expect("default appointment operation"),confirmation(&candidate,&default))).await.expect("real removal from the role retains its completion");
                let deleted=Box::pin(services.role().delete(actor.clone(),operation,created.id(),Delete::new(updated.change_id()))).await.expect("terminal deletion");
                assert!(matches!(deleted.action(),crate::access::role::change::Action::Delete));
                assert_eq!(deleted.name(),updated.name()); assert_eq!(deleted.level(),updated.level());
                let row=observer.client().query_one("SELECT NOT active AND change_id=$2 AS deleted,NOT EXISTS(SELECT 1 FROM access.permissions WHERE role_id=$1) AS empty,(SELECT count(*) FROM access.changes WHERE role_id=$1) AS changes,(SELECT count(*) FROM access.entries WHERE change_id=$2) AS entries,clock_timestamp() AS now FROM access.roles WHERE id=$1",&[&created.id(),&deleted.id()]).await.expect("independent terminal graph before any replay");
                assert!(row.get::<_,bool>("deleted"));assert!(row.get::<_,bool>("empty"));assert_eq!(row.get::<_,i64>("changes"),3);assert_eq!(row.get::<_,i64>("entries"),0);
                crate::access::verification::verify(observer.client(), services.app(),row.get("now")).await.expect("startup accepts permanent terminal facts");
                let saved:String=observer.client().query_one(statement,&[&created.id()]).await.expect("complete permanent snapshot").get(0);
                let repeated=Box::pin(services.role().delete(actor.clone(),operation,created.id(),Delete::new(updated.change_id()))).await.expect("original deletion");
                assert_eq!(repeated.id(),deleted.id());assert_eq!(repeated.operator().revision_id(),deleted.operator().revision_id());
                assert_eq!(Box::pin(services.role().create(actor.clone(),creation,input)).await.expect("old creation after deletion").change_id(),created.change_id());
                assert_eq!(Box::pin(services.role().update(actor.clone(),modification,created.id(),Update::new(created.change_id(),replacement.clone()))).await.expect("old modification after deletion").change_id(),updated.change_id());
                assert!(matches!(Box::pin(services.role().create(actor.clone(),operation,replacement.clone())).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().delete(actor.clone(),operation,crate::access::role::Id::allocate(),Delete::new(updated.change_id()))).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().delete(actor.clone(),Uuid::new_v4().to_string().parse().expect("missing operation"),created.id(),Delete::new(updated.change_id()))).await,Err(kernel::Error::Rejected(Rejection::Absent))));
                let new=Box::pin(services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("new creation"),replacement)).await.expect("name can be reused");
                assert_ne!(new.id(),created.id());
                let list=services.role().changes(&actor,crate::access::role::change::Filter::new(crate::pagination::Pagination::new(1,100).expect("page"),Some(created.id()),None)).await.expect("permanent original history");
                assert_eq!(list.count(),3);assert_eq!(list.changes()[0].id(),deleted.id());
                assert_eq!(observer.client().query_one(statement,&[&created.id()]).await.expect("replays preserve deleted graph").get::<_,String>(0),saved);
                services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                assert_eq!(Box::pin(services.role().delete(actor,operation,created.id(),Delete::new(updated.change_id()))).await.expect("original deletion after restart").id(),deleted.id());
                services.close();services.drain().await.expect("drained again");observer.terminate().await.expect("observer ended");context.stop().await.expect("restarted context ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded complete deletion lifecycle");
        }));
    }

    #[test]
    fn original_modification_authenticates_its_record_and_history_authenticates_predecessors() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for predecessor in [false, true] {
            timeout(Duration::from_secs(20), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let definition: Input = serde_json::from_value(serde_json::json!({"name":"Original","level":200,"permissions":["institution.accounts"]})).expect("original definition");
                let created = services.role().create(actor.clone(), Uuid::new_v4().to_string().parse().expect("creation"), definition.clone()).await.expect("real creation");
                let operation = Uuid::new_v4().to_string().parse().expect("modification");
                let changed: Input = serde_json::from_value(serde_json::json!({"name":"Modified","level":200,"permissions":["institution.accounts"]})).expect("changed definition");
                let updated = services.role().update(actor.clone(), operation, created.id(), Update::new(created.change_id(), changed.clone())).await.expect("real modification");
                let owner = database.connections("postgres").recover().await.expect("isolated privileged restore");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("explicitly bypass immutable history only in this fixture");
                let damaged = if predecessor { created.change_id() } else { updated.change_id() };
                owner.client().execute("UPDATE access.changes SET name='Tampered' WHERE id=$1", &[&damaged]).await.expect("damage the record while retaining its original seal");
                owner.client().batch_execute("COMMIT").await.expect("damaged history visible");
                owner.terminate().await.expect("restore connection ended");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                let statement = "SELECT jsonb_build_object('role',(SELECT to_jsonb(role) FROM access.roles role WHERE id=$1),'changes',(SELECT jsonb_agg(to_jsonb(change) ORDER BY changed_at,id) FROM access.changes change WHERE role_id=$1),'entries',(SELECT jsonb_agg(to_jsonb(entry) ORDER BY change_id,capability_id) FROM access.entries entry JOIN access.changes change ON change.id=entry.change_id WHERE change.role_id=$1),'permissions',(SELECT jsonb_agg(to_jsonb(permission) ORDER BY capability_id) FROM access.permissions permission WHERE role_id=$1))::text";
                let before: String = observer.client().query_one(statement, &[&created.id()]).await.expect("independent complete graph before replay").get(0);
                services.role().list(&actor, crate::pagination::Pagination::new(1,100).expect("page")).await.expect("current definitions depend on their sealed states");
                let replay = services.role().update(actor.clone(), operation, created.id(), Update::new(created.change_id(), changed.clone())).await;
                let error = if predecessor {
                    assert_eq!(replay.expect("original delivery authenticates its own record and current anchor").change_id(), updated.change_id());
                    services.role().changes(&actor, crate::access::role::change::Filter::new(crate::pagination::Pagination::new(1,100).expect("page"), Some(created.id()), Some(operation))).await.err().expect("filtered history still authenticates the earlier record")
                } else { replay.err().expect("matching original request cannot deliver a damaged record") };
                assert!(error.is_corrupt(), "{error:?}");
                assert!(error.unreliable());
                assert!(services.role().app.registry().ready().is_err());
                assert!(matches!(services.role().update(actor, operation, created.id(), Update::new(created.change_id(), changed)).await, Err(error) if error.is_closed()), "later original work is rejected before reading damaged facts again");
                let after: String = observer.client().query_one(statement, &[&created.id()]).await.expect("rejection made no partial repairs or new facts").get(0);
                assert_eq!(after, before);
                services.close(); services.drain().await.expect("drained"); observer.terminate().await.expect("observer ended"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded original-result corruption scenario");
            }
        }));
    }

    #[test]
    fn modification_keeps_original_definitions_and_only_requires_added_permissions() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(20), Box::pin(async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let original: Input = serde_json::from_value(serde_json::json!({"name":"Original","level":200,"permissions":["institution.accounts"]})).expect("definition");
                let creation = Uuid::new_v4().to_string().parse().expect("operation");
                let created = Box::pin(services.role().create(actor.clone(), creation, original.clone())).await.expect("real creation");
                let owner = database.connections("postgres").recover().await.expect("isolated assignment fixture");
                let maintainer = insert_role(owner.client(), services.role(), &actor, "Maintainer", 500, &["institution.roles"]).await;
                assign_role(owner.client(), services.role(), &actor, maintainer).await;
                owner.terminate().await.expect("fixture ended");
                let observer = database.connections(&database.user).recover().await.expect("independent runtime observer");
                let sequence: i64 = observer.client().query_one("SELECT sequence AS person_sequence FROM identity.persons", &[]).await.expect("personal sequence").get(0);
                let operation = Uuid::new_v4().to_string().parse().expect("modification");
                let definition: Input = serde_json::from_value(serde_json::json!({"name":"Revised","level":201,"permissions":["institution.accounts"]})).expect("retained permission is not a new grant");
                let input = Update::new(created.change_id(), definition.clone());
                let updated = Box::pin(services.role().update(actor.clone(), operation, created.id(), input.clone())).await.expect("modification");
                let row = observer.client().query_one("SELECT role.change_id,role.name,role.level,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id) AS permissions,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=role.change_id ORDER BY capability_id) AS entries,(SELECT count(*) FROM access.changes WHERE role_id=role.id) AS changes,(SELECT sequence AS person_sequence FROM identity.persons) AS sequence FROM access.roles role WHERE id=$1", &[&created.id()]).await.expect("independent complete graph before replay");
                assert_eq!(row.get::<_, crate::access::role::change::Id>(0), updated.change_id());
                assert_eq!(row.get::<_, String>(1), "Revised");
                assert_eq!(row.get::<_, i16>(2), 201);
                assert_eq!(row.get::<_, Vec<String>>(3), ["institution.accounts"]);
                assert_eq!(row.get::<_, Vec<String>>(3), row.get::<_, Vec<String>>(4));
                assert_eq!(row.get::<_, i64>(5), 2);
                assert_eq!(row.get::<_, i64>(6), sequence);
                assert_eq!(Box::pin(services.role().update(actor.clone(),operation,created.id(),input.clone())).await.expect("original modification").change_id(),updated.change_id());
                assert_eq!(Box::pin(services.role().create(actor.clone(),creation,original.clone())).await.expect("original creation after modification").change_id(),created.change_id());
                assert!(matches!(Box::pin(services.role().update(actor.clone(),creation,created.id(),input.clone())).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().create(actor.clone(),operation,definition.clone())).await,Err(kernel::Error::Mismatch)));
                assert!(matches!(Box::pin(services.role().update(actor.clone(),Uuid::new_v4().to_string().parse().expect("operation"),created.id(),input)).await,Err(kernel::Error::Rejected(Rejection::Changed))));
                let expanded:Input=serde_json::from_value(serde_json::json!({"name":"Expanded","level":201,"permissions":["institution.accounts","institution.workforce"]})).expect("well typed unowned permission");
                assert!(matches!(Box::pin(services.role().update(actor.clone(),Uuid::new_v4().to_string().parse().expect("operation"),created.id(),Update::new(updated.change_id(),expanded))).await,Err(kernel::Error::Denied)));
                assert!(matches!(Box::pin(services.role().update(actor.clone(),Uuid::new_v4().to_string().parse().expect("operation"),created.id(),Update::new(updated.change_id(),definition))).await,Err(kernel::Error::Rejected(Rejection::Unchanged))));
                let changed_back=Box::pin(services.role().update(actor.clone(),Uuid::new_v4().to_string().parse().expect("operation"),created.id(),Update::new(updated.change_id(),original.clone()))).await.expect("definition can change back with fresh confirmation");
                assert_ne!(changed_back.change_id(),created.change_id());
                assert!(matches!(Box::pin(services.role().update(actor.clone(),Uuid::new_v4().to_string().parse().expect("operation"),created.id(),Update::new(created.change_id(),original))).await,Err(kernel::Error::Rejected(Rejection::Changed))));
                let row=observer.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1", &[&created.id()]).await.expect("all rejections have no partial history");
                assert_eq!(row.get::<_,i64>(0),3);
                assert_eq!(Box::pin(services.role().update(actor.clone(),operation,created.id(),Update::new(created.change_id(),serde_json::from_value(serde_json::json!({"name":"Revised","level":201,"permissions":["institution.accounts"]})).expect("original input")))).await.expect("permanent predecessor survives later updates").change_id(),updated.change_id());
                let owner=database.connections("postgres").recover().await.expect("isolated higher role fixtures");
                let equal=insert_role(owner.client(), services.role(),&actor,"Equal",500,&[]).await;
                let higher=insert_role(owner.client(), services.role(),&actor,"Higher",600,&[]).await;
                let assignment=insert_role(owner.client(), services.role(),&actor,"Assignments only",500,&["institution.assignments"]).await;
                let before:i64=observer.client().query_one("SELECT count(*) FROM access.changes",&[]).await.expect("facts before deletion refusals").get(0);
                let rows=observer.client().query("SELECT id,change_id FROM access.roles WHERE scope='institution' AND (level IN (1,1000) OR id=ANY($1))",&[&vec![equal,higher]]).await.expect("system, equal and higher targets");
                assert_eq!(rows.len(),4);
                for row in rows {
                    assert!(matches!(Box::pin(services.role().delete(actor.clone(),Uuid::new_v4().to_string().parse().expect("deletion"),row.get("id"),Delete::new(row.get("change_id")))).await,Err(kernel::Error::Denied)),"system and rank checks precede holder checks");
                }
                let row=observer.client().query_one("SELECT id,change_id FROM access.roles WHERE scope='workforce' AND level=1",&[]).await.expect("other scope target");
                assert!(matches!(Box::pin(services.role().delete(actor.clone(),Uuid::new_v4().to_string().parse().expect("deletion"),row.get("id"),Delete::new(row.get("change_id")))).await,Err(kernel::Error::Rejected(Rejection::Absent))));
                assign_role(owner.client(), services.role(),&actor,assignment).await;
                assert!(matches!(Box::pin(services.role().delete(actor,Uuid::new_v4().to_string().parse().expect("deletion"),created.id(),Delete::new(changed_back.change_id()))).await,Err(kernel::Error::Denied)));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes",&[]).await.expect("none of these refusals creates history").get::<_,i64>(0),before);
                owner.terminate().await.expect("fixture ended");
                services.close(); services.drain().await.expect("drained"); observer.terminate().await.expect("observer ended"); context.stop().await.expect("stopped");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            })).await.expect("bounded modification scenario");
        }));
    }

    #[test]
    fn concurrent_operations_recheck_confirmation_identity_and_time_after_locks() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for deletion in [false,true] {
            timeout(Duration::from_secs(20),Box::pin(async {
                let database=Database::initialized(Arc::clone(&server)).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let definition:Input=serde_json::from_value(serde_json::json!({"name":"Concurrent target","level":200,"permissions":[]})).expect("definition");
                let target=services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("creation"),definition).await.expect("target");
                let definition:Input=serde_json::from_value(serde_json::json!({"name":"Concurrent replacement","level":201,"permissions":[]})).expect("replacement");
                let operations=[Uuid::new_v4().to_string().parse().expect("first"),Uuid::new_v4().to_string().parse().expect("second")];
                let (first,second)=tokio::join!(services.role().update(actor.clone(),operations[0],target.id(),Update::new(target.change_id(),definition.clone())),services.role().update(actor.clone(),operations[1],target.id(),Update::new(target.change_id(),definition)));
                let updated=match (first,second) {(Ok(role),Err(kernel::Error::Rejected(Rejection::Changed)))|(Err(kernel::Error::Rejected(Rejection::Changed)),Ok(role))=>role,_=>panic!("one old confirmation admits exactly one update")};
                let owner=database.connections("postgres").recover().await.expect("isolated lock owner");
                let observer=database.connections(&database.user).recover().await.expect("independent observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1",&[&target.id()]).await.expect("independent single successor before replay").get::<_,i64>(0),2);
                let row=observer.client().query_one("SELECT changed_at,operator_id FROM access.changes WHERE id=$1",&[&updated.change_id()]).await.expect("original accepted time");
                let accepted:chrono::DateTime<chrono::Utc>=row.get(0);let operator:revision::Id=row.get(1);
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Later","level":202,"permissions":[]})).expect("later definition");
                let later = accepted + chrono::TimeDelta::microseconds(1);
                for (now, allowed) in [(accepted, false), (later, true)] {
                    let facts=super::Facts {
                        target: services.role().roles.find_state(observer.client(),target.id(),accepted).await.expect("accepted state"),
                        authority: services.role().roles.all(observer.client(),accepted).await.expect("verified authority").into_iter().find(|state| state.scope()==crate::access::capability::Scope::Institution && state.role().kind()==Kind::Full).expect("full institution authority").role().clone(),
                        grants: Vec::new(), holder_levels: Vec::new(), in_use: false, name_taken: false,
                        operator: services.role().revisions.fetch_identity(observer.client(),operator).await.expect("fixed operator"),
                        operation_id: Uuid::new_v4().to_string().parse().expect("operation"),
                    };
                    let action = if deletion { Action::Delete { id: target.id(), change_id: updated.change_id() } }
                        else { Action::Update { id: target.id(), change_id: updated.change_id(), input: input.clone() } };
                    let result = super::transition(&action,target.id(),crate::access::role::change::Id::allocate(),facts,now);
                    if allowed { assert!(result.is_ok(), "the next definition follows the role's own previous definition"); }
                    else {
                        let error = result.err().expect("unreliable persisted time cannot become a new change");
                        assert!(error.is_corrupt()); assert!(error.unreliable());
                    }
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1",&[&target.id()]).await.expect("pure decisions write no facts").get::<_,i64>(0),2);
                let expiry:chrono::DateTime<chrono::Utc>=observer.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("original expiry").get(0);
                let elapsed=expiry-database.now().await-chrono::TimeDelta::seconds(3);
                crate::registration::tests::age_facts(&database,&services,elapsed).await;
                owner.client().batch_execute("BEGIN").await.expect("target gate");
                owner.client().query_one("SELECT id FROM access.roles WHERE id=$1 FOR UPDATE",&[&target.id()]).await.expect("hold exact target");
                let blocker:i32=owner.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("blocker").get(0);
                let service=services.role().clone();let current=actor.clone();let command=Update::new(updated.change_id(),input.clone());let id=target.id();
                let confirmation=updated.change_id();
                let task=tokio::spawn(async move {let operation=Uuid::new_v4().to_string().parse().expect("operation");if deletion {service.delete(current,operation,id,Delete::new(confirmation)).await.map(|_|())}else{service.update(current,operation,id,command).await.map(|_|())}});
                timeout(Duration::from_secs(3),async {loop {if observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT id FROM access.roles%' AND $1::int=ANY(pg_blocking_pids(pid)))",&[&blocker]).await.expect("exact target lock wait after first authorization").get::<_,bool>(0) {break;}tokio::task::yield_now().await;}}).await.expect("waiting update reached target lock");
                assert!(observer.client().query_one("SELECT expires_at>clock_timestamp() FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("credential remains valid when the target lock blocks").get::<_,bool>(0));
                // Account and Session are already locked; let the authenticated lifetime actually end.
                timeout(Duration::from_secs(4),async {loop {
                    if observer.client().query_one("SELECT expires_at<=clock_timestamp() FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("real expiry while the target remains locked").get::<_,bool>(0) {break;}
                    tokio::task::yield_now().await;
                }}).await.expect("bounded session expiry");
                owner.client().batch_execute("ROLLBACK").await.expect("release target");
                assert!(matches!(task.await.expect("work ended"),Err(kernel::Error::Unauthenticated)));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1",&[&target.id()]).await.expect("no expired write").get::<_,i64>(0),2);
                crate::registration::tests::age_facts(&database,&services,-elapsed).await;
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("damage target time");owner.client().execute("UPDATE access.roles SET changed_at=clock_timestamp()+interval '1 hour' WHERE id=$1",&[&target.id()]).await.expect("future persisted target");owner.client().batch_execute("COMMIT").await.expect("future time visible");
                let operation=Uuid::new_v4().to_string().parse().expect("operation");
                let option=if deletion {Box::pin(services.role().delete(actor,operation,target.id(),Delete::new(updated.change_id()))).await.err()}else{Box::pin(services.role().update(actor,operation,target.id(),Update::new(updated.change_id(),input))).await.err()};
                let error=option.expect("unreliable target time cannot become ordinary query failure");
                assert!(error.is_corrupt());assert!(error.unreliable());assert!(services.role().app.registry().ready().is_err());
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE role_id=$1",&[&target.id()]).await.expect("no new facts after unreliable time").get::<_,i64>(0),2);
                owner.terminate().await.expect("gate ended");observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded concurrency and qualification");
            }
        }));
    }

    #[test]
    fn modification_and_deletion_cannot_both_accept_the_same_confirmation() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(15),Box::pin(async {
                let database=Database::initialized(server).await;let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;let actor=actor(&database,&services,&mut supplier).await;
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Race","level":200,"permissions":[]})).expect("definition");
                let target=Box::pin(services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("creation"),input)).await.expect("target");
                let owner=database.connections("postgres").recover().await.expect("target gate");let observer=database.connections(&database.user).recover().await.expect("observer");
                owner.client().batch_execute("BEGIN").await.expect("gate transaction");owner.client().query_one("SELECT id FROM access.roles WHERE id=$1 FOR UPDATE",&[&target.id()]).await.expect("hold actual target");
                let service=services.role().clone();let current=actor.clone();let id=target.id();let confirmation=target.change_id();
                let update=tokio::spawn(async move {let input:Input=serde_json::from_value(serde_json::json!({"name":"Race modified","level":201,"permissions":[]})).expect("replacement");service.update(current,Uuid::new_v4().to_string().parse().expect("modification"),id,Update::new(confirmation,input)).await});
                let service=services.role().clone();let current=actor.clone();
                let delete=tokio::spawn(async move {service.delete(current,Uuid::new_v4().to_string().parse().expect("deletion"),id,Delete::new(confirmation)).await});
                timeout(Duration::from_secs(3),async {loop {
                    let count:i64=observer.client().query_one("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND usename=$1 AND wait_event_type='Lock' AND (query LIKE 'SELECT id FROM access.roles%' OR query LIKE 'SELECT pg_catalog.pg_advisory_xact_lock%')",&[&database.user]).await.expect("both workers reached actual database lock waits").get(0);
                    if count==2 {break;}tokio::task::yield_now().await;
                }}).await.expect("explicit contention at target and scope");
                owner.client().batch_execute("ROLLBACK").await.expect("release gate");
                let (update,delete)=(update.await.expect("update worker"),delete.await.expect("delete worker"));
                match (&update,&delete) {(Ok(_),Err(kernel::Error::Rejected(Rejection::Changed)))|(Err(kernel::Error::Rejected(Rejection::Absent)),Ok(_))=>{},_=>panic!("one confirmation admits one action: update {:?}, delete {:?}",update.as_ref().err(),delete.as_ref().err())}
                let row=observer.client().query_one("SELECT (SELECT count(*) FROM access.changes WHERE role_id=$1) AS changes,active FROM access.roles WHERE id=$1",&[&id]).await.expect("independent retained final state before any replay");
                assert_eq!(row.get::<_,i64>("changes"),2);assert_eq!(row.get::<_,bool>("active"),update.is_ok());
                owner.terminate().await.expect("owner ended");observer.terminate().await.expect("observer ended");services.close();services.drain().await.expect("drained");context.stop().await.expect("context ended");assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err());supplier.stop().await;
            })).await.expect("bounded action race");
        }));
    }

    #[test]
    fn typed_input_and_stored_names_preserve_different_normalization_boundaries() {
        for value in [
            serde_json::json!({"name":" ","level":100,"permissions":[]}),
            serde_json::json!({"name":"a\u{0000}b","level":100,"permissions":[]}),
            serde_json::json!({"name":"字".repeat(65),"level":100,"permissions":[]}),
            serde_json::json!({"name":"Role","level":0,"permissions":[]}),
            serde_json::json!({"name":"Role","level":1001,"permissions":[]}),
            serde_json::json!({"name":"Role","level":1.5,"permissions":[]}),
            serde_json::json!({"name":"Role","level":100,"permissions":["institution.roles","institution.roles"]}),
            serde_json::json!({"name":"Role","level":100,"permissions":["workforce.roles"]}),
            serde_json::json!({"name":"Role","level":100,"permissions":["Institution.Roles"]}),
            serde_json::json!({"name":"Role","level":100,"permissions":[],"kind":"custom"}),
        ] {
            assert!(serde_json::from_value::<Input>(value).is_err());
        }
        for level in [1, 1000] {
            assert!(
                serde_json::from_value::<Input>(
                    serde_json::json!({"name":"字".repeat(64),"level":level,"permissions":[]})
                )
                .is_ok(),
                "system level requires business authorization"
            );
        }
        for text in ["Cafe\u{301}", " Role "] {
            assert!(
                Name::from_sql(&Type::TEXT, text.as_bytes()).is_err(),
                "stored definitions never normalize damaged values"
            );
        }
    }

    #[test]
    fn stored_value_decode_failure_keeps_its_column_and_closes_accepted_writes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15),async {
                let database=Database::initialized(server).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let writer=database.connections(&database.user).recover().await.expect("runtime writer");
                let query:Error=writer.client().query_one("SELECT 1/0",&[]).await.expect_err("ordinary statement failure").into();
                assert!(!query.unreliable(),"ordinary SQL errors do not invalidate facts");
                let row=writer.client().query_one("SELECT 1001::smallint AS level",&[]).await.expect("isolated malformed stored value projection");
                let level=Error::decode(row.try_get::<_,Level>("level").expect_err("stored level guard"));
                assert!(level.unreliable());
                assert!(level.to_string().contains("error deserializing column 0"),"driver keeps the offending column index");
                let operation=Uuid::new_v4().to_string().parse().expect("operation");
                let input:Input=serde_json::from_value(serde_json::json!({"name":"\u{0323}\u{1ad0}","level":200,"permissions":[]})).expect("current normalized input");
                let created=services.role().create(actor.clone(),operation,input.clone()).await.expect("actual canonical definition");
                let change=created.change_id();
                let owner=database.connections("postgres").recover().await.expect("isolated damage owner");
                owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated recorded value damage");
                owner.client().execute("UPDATE access.changes SET name=$1 WHERE id=$2",&[&"\u{1ad0}\u{0323}",&change]).await.expect("database Unicode 16 permits the obsolete normalization");
                owner.client().batch_execute("COMMIT").await.expect("damage committed");
                owner.terminate().await.expect("damage owner ended");
                let before:String=writer.client().query_one("SELECT row_to_json(change)::text FROM access.changes change WHERE id=$1",&[&change]).await.expect("independent original before replay").get(0);
                let result=services.role().create(actor.clone(),operation,input.clone()).await;
                let Err(kernel::Error::Corrupt(fault))=&result else {panic!("original column decode failure: {:?}",result.as_ref().err())};
                let kernel::FaultKind::History(crate::history::Error::Decode(query))=fault.kind() else {panic!("typed history decoding cause: {fault:?}")};
                assert!(query.to_string().contains("error deserializing column 7"),"{query}");
                let mut option=error::Error::source(query);
                let mut classified=false;
                while let Some(cause)=option {
                    classified|=matches!(cause.downcast_ref::<InputError>(),Some(InputError::Name));
                    option=cause.source();
                }
                assert!(classified,"owned safe error is retained behind the driver column");
                assert!(matches!(services.role().create(actor,operation,input).await,Err(error) if error.is_closed()));
                let after:String=writer.client().query_one("SELECT row_to_json(change)::text FROM access.changes change WHERE id=$1",&[&change]).await.expect("no attempt repaired or changed original facts").get(0);
                assert_eq!(after,before);
                services.close(); services.drain().await.expect("drained"); writer.terminate().await.expect("writer ended"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            }).await.expect("bounded stored-fact failure");
        });
    }

    #[test]
    #[tracing_test::traced_test]
    fn commit_recovery_keeps_complete_roles_and_real_cleanup_causes() {
        let server = server();
        tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
            timeout(Duration::from_secs(75), async {
                for (fault,modification) in [(Fault::LostReadCommit,false), (Fault::StalledReadCommit,false), (Fault::ReadCommitTermination,false), (Fault::StalledRoleOriginal,false), (Fault::ReadCommitTermination,true), (Fault::StalledReadCommit,true)] {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let proxy = Proxy::start(&server).await;
                    let (context,services) = start(authentication::proxied(&database,&proxy),&supplier).await;
                    let actor = actor(&database,&services,&mut supplier).await;
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let input: Input = serde_json::from_value(serde_json::json!({"name":"private-role-input","level":200,"permissions":["institution.accounts"]})).expect("input");
                    let operation = Uuid::nil().to_string().parse().expect("fixed original operation");
                    let action=if modification {
                        let previous:Input=serde_json::from_value(serde_json::json!({"name":"Before modification","level":200,"permissions":[]})).expect("initial definition");
                        let role=services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("creation"),previous).await.expect("original target");
                        Action::Update {id:role.id(),change_id:role.change_id(),input:input.clone()}
                    } else {Action::Create(input.clone())};
                    proxy.arm(Fault::DelayedCommit);
                    let service = services.role().clone(); let current = actor.clone(); let command = input.clone();
                    let invocation=action.clone();
                    let task = tokio::spawn(async move { match invocation { Action::Create(_)=>service.create(current,operation,command).await, Action::Update{id,change_id,..}=>service.update(current,operation,id,Update::new(change_id,command)).await, Action::Delete{..}|Action::CreateWorkforce(_)|Action::UpdateWorkforce{..}|Action::DeleteWorkforce{..}=>unreachable!("institutional definition cases") } });
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("role INSERT transaction reached COMMIT");
                    let (blocker,recovery) = timeout(Duration::from_secs(3),async {
                        loop {
                            let option = observer.client().query_opt("SELECT holder.pid AS blocker, waiter.pid AS recovery FROM pg_catalog.pg_locks waiter JOIN pg_catalog.pg_locks holder ON holder.locktype=waiter.locktype AND holder.database=waiter.database AND holder.classid=waiter.classid AND holder.objid=waiter.objid AND holder.objsubid=waiter.objsubid AND holder.granted WHERE waiter.locktype='advisory' AND NOT waiter.granted AND waiter.mode='ExclusiveLock' AND waiter.classid=602503669::oid AND waiter.objid=1284585832::oid AND waiter.objsubid=1 AND waiter.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=current_database()) AND holder.pid=ANY(pg_catalog.pg_blocking_pids(waiter.pid))",&[]).await.expect("independently derived LP32/SHA256 original role lock");
                            if let Some(row)=option { break (row.get::<_,i32>("blocker"),row.get::<_,i32>("recovery")); }
                            tokio::task::yield_now().await;
                        }
                    }).await.expect("recovery waits for this exact original transaction");
                    let visible: i64 = observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&operation]).await.expect("write has not committed").get(0);
                    assert_eq!(visible,0);
                    let incomplete=matches!(fault,Fault::StalledRoleOriginal);
                    proxy.arm(fault); proxy.release();
                    timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("selected recovery fault actually intercepted");
                    let result = timeout(Duration::from_secs(13),async {
                        tokio::pin!(task);
                        loop {
                            tokio::select! {
                                result=&mut task=>break result.expect("accepted worker"),
                                request=timeout(Duration::from_secs(4),supplier.request())=>assert!(request.is_err(),"role work never calls Siteverify"),
                            }
                        }
                    }).await.expect("original ten-second recovery deadline");
                    if incomplete {
                        assert!(matches!(&result,Err(kernel::Error::Unknown(fault)) if fault.later().any(|(stage,cause)| stage==kernel::Stage::Recovery && matches!(cause,kernel::FaultKind::Timeout(_)))),"{:?}",result.as_ref().err());
                        services.role().app.registry().ready().expect("an operation read timeout does not revoke the global clock");
                    } else { assert!(result.is_ok(),"complete original facts survive cleanup: {:?}",result.as_ref().err()); }
                    let row = observer.client().query_one("SELECT role.id,role.change_id,change.name,ARRAY(SELECT capability_id FROM access.permissions WHERE role_id=role.id ORDER BY capability_id) AS permissions,ARRAY(SELECT capability_id FROM access.entries WHERE change_id=change.id ORDER BY capability_id) AS entries FROM access.changes change JOIN access.roles role ON role.change_id=change.id WHERE change.operation_id=$1",&[&operation]).await.expect("independent full committed graph before replay");
                    assert_eq!(row.get::<_,String>("name"),"private-role-input");
                    assert_eq!(row.get::<_,Vec<String>>("permissions"),["institution.accounts"]);
                    assert_eq!(row.get::<_,Vec<String>>("entries"),["institution.accounts"]);
                    proxy.release();
                    timeout(Duration::from_secs(3),async {
                        loop {
                            let count:i64=observer.client().query_one("SELECT count(*) FROM pg_stat_activity WHERE pid=ANY($1::int[])",&[&vec![blocker,recovery]]).await.expect("actual original and recovery backends").get(0);
                            if count==0 {break;} tokio::task::yield_now().await;
                        }
                    }).await.expect("both actual drivers and remote backends ended");
                    let replay=match action {Action::Create(_)=>services.role().create(actor,operation,input).await, Action::Update{id,change_id,..}=>services.role().update(actor,operation,id,Update::new(change_id,input)).await, Action::Delete{..}|Action::CreateWorkforce(_)|Action::UpdateWorkforce{..}|Action::DeleteWorkforce{..}=>unreachable!("institutional definition cases")}.expect("same original after recovery");
                    assert_eq!(replay.id(),row.get("id")); assert_eq!(replay.change_id(),row.get("change_id"));
                    services.close(); services.drain().await.expect("drained"); observer.terminate().await.expect("observer ended"); context.stop().await.expect("context ended");
                    assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
                }
            }).await.expect("bounded recovery cases");
        });
        logs_assert(|lines| {
            let events: Vec<_> = lines
                .iter()
                .filter(|line| {
                    line.contains("stage=\"completion_recovery\"")
                        || line.contains("stage=\"command\"")
                })
                .collect();
            assert_eq!(events.len(), 6, "one final diagnostic per affected work");
            assert_eq!(
                events
                    .iter()
                    .filter(|line| line.contains("outcome=\"committed\""))
                    .count(),
                5
            );
            assert!(
                events
                    .iter()
                    .any(|line| line.contains("stage: Commit, kind: Query(connection closed)")),
                "actual lost COMMIT reply"
            );
            assert!(
                events
                    .iter()
                    .any(|line| line.contains("(Commit, Timeout(Elapsed")),
                "actual recovery timeout"
            );
            assert!(
                events
                    .iter()
                    .any(|line| line.contains("outcome=\"committed\"")
                        && line.contains(
                            "(Termination, Connection(database driver connection failed))"
                        )),
                "successful COMMIT retains the actual driver failure"
            );
            assert!(
                events
                    .iter()
                    .all(|line| !line.contains("private-role-input")
                        && !line.contains("13900000000")
                        && !line.contains("SELECT "))
            );
            Ok(())
        });
    }

    #[test]
    fn runtime_role_pairs_and_operation_families_preserve_complete_facts() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let operation = Uuid::new_v4().to_string().parse().expect("shared operation UUID");
                    let institution = Box::pin(services.role().create(manager.clone(), operation, serde_json::from_value(serde_json::json!({"name":"Independent scopes","level":200,"permissions":["institution.accounts"]})).expect("institution input"))).await.expect("actual institution role");
                    let workforce = Box::pin(services.role().create_workforce(manager, operation, serde_json::from_value(serde_json::json!({"entry":"institution","name":"Independent scopes","level":200,"permissions":["workforce.roles"]})).expect("workforce input"))).await.expect("actual independent Workforce family");
                    assert_ne!(institution.id(), workforce.id());
                    let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.changes WHERE operation_id=$1) AS records,(SELECT count(*) FROM access.completions WHERE operation_id=$1) AS completions", &[&operation]).await.expect("both committed families");
                    assert_eq!(row.get::<_, i64>("records"), 2);
                    assert_eq!(row.get::<_, i64>("completions"), 2);
                    // PostgreSQL 18 uses Unicode 16; the Rust owner uses Unicode 17.
                    let newer = "\u{1ad0}\u{0323}";
                    let canonical = "\u{0323}\u{1ad0}";
                    assert!(observer.client().query_one("SELECT $1::text IS NFC NORMALIZED", &[&newer]).await.expect("older Unicode database observation").get::<_, bool>(0));
                    assert!(Name::from_sql(&Type::TEXT, newer.as_bytes()).is_err(), "database NFC does not replace the Name owner");
                    assert!(Name::from_sql(&Type::TEXT, canonical.as_bytes()).is_ok());
                    let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("both complete families").get(0);
                    // Copies carry every required field. Their seals are not valid for changed data;
                    // these are isolated SQL structure counterexamples and are always rolled back.
                    let candidate_operation = Uuid::new_v4();
                    let institution_id = Uuid::from_bytes(*institution.id().as_bytes());
                    let institution_change = Uuid::from_bytes(*institution.change_id().as_bytes());
                    let record = format!("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT '00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000002',NULL,1,scope,entry,action,'Candidate',level,permissions,operator_id,changed_at,'{candidate_operation}',seal,seal_key_id,seal_version FROM access.changes WHERE id='{}'", institution_change);
                    let state = format!("INSERT INTO access.roles(id,scope,name,level,change_id,permissions,active,changed_at,sequence,version,seal,seal_key_id,seal_version) SELECT '00000000-0000-4000-8000-000000000002',scope,'Candidate',level,'00000000-0000-4000-8000-000000000001',permissions,active,changed_at,sequence,version,seal,seal_key_id,seal_version FROM access.roles WHERE id='{}'", institution_id);
                    let completion = format!("INSERT INTO access.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version) SELECT family,'{candidate_operation}',action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version FROM access.completions WHERE family='institution_role' AND operation_id='{operation}'");
                    let missing_completion = format!("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT gen_random_uuid(),role_id,id,sequence+1,scope,entry,'update',name,level,permissions,operator_id,changed_at,'{candidate_operation}',seal,seal_key_id,seal_version FROM access.changes WHERE id='{}'", institution_change);
                    for (name, statement, code, constraint) in [
                        ("record without owning state", format!("{completion}; {record}; SET CONSTRAINTS access.changes_role_id_scope_fkey IMMEDIATE"), "23503", Some("changes_role_id_scope_fkey")),
                        ("state without record", format!("{state}; SET CONSTRAINTS access.roles_records_check IMMEDIATE"), "23514", Some("roles_records_check")),
                        ("record without its family completion", format!("{missing_completion}; COMMIT"), "23503", Some("changes_family_operation_id_fkey")),
                        ("same-scope original operation", format!("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT gen_random_uuid(),gen_random_uuid(),NULL,1,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version FROM access.changes WHERE id='{}'", institution_change), "23505", Some("changes_institution_operation_id_key")),
                        ("same-family completion", format!("INSERT INTO access.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version) SELECT family,operation_id,action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version FROM access.completions WHERE family='institution_role' AND operation_id='{operation}'"), "23505", Some("completions_pkey")),
                    ] {
                        writer.client().batch_execute("BEGIN").await.expect("isolated structure counterexample");
                        let error = writer.client().batch_execute(&statement).await.expect_err(name);
                        assert_eq!(error.code().map(tokio_postgres::error::SqlState::code), Some(code), "{name}: {error:?}");
                        if let Some(constraint) = constraint {
                            assert_eq!(error.as_db_error().expect("database constraint").constraint(), Some(constraint), "{name}");
                        }
                        writer.client().batch_execute("ROLLBACK").await.expect("counterexample rolled back");
                        assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("no partial family or pair").get::<_, String>(0), before, "{name}");
                    }
                    for (permissions, valid) in [
                        ("ARRAY[]::text[]", true),
                        ("ARRAY[NULL]::text[]", false),
                        ("ARRAY[['institution.accounts']]::text[]", false),
                        ("'[0:0]={institution.accounts}'::text[]", false),
                    ] {
                        for (statement, constraint, source) in [
                            (format!("UPDATE access.roles SET permissions={permissions} WHERE id=$1"), "roles_permissions_check", institution_id),
                            (format!("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT gen_random_uuid(),gen_random_uuid(),NULL,1,scope,entry,action,name,level,{permissions},operator_id,changed_at,gen_random_uuid(),seal,seal_key_id,seal_version FROM access.changes WHERE id=$1"), "changes_permissions_check", institution_change),
                        ] {
                            writer.client().batch_execute("BEGIN").await.expect("isolated permission array shape");
                            let result = writer.client().execute(&statement, &[&source]).await;
                            if valid {
                                assert_eq!(result.expect("empty permission array satisfies the row CHECK"), 1);
                            } else {
                                let error = result.expect_err("malformed permission array");
                                assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION), "{constraint}: {permissions}");
                                assert_eq!(error.as_db_error().expect("permission shape CHECK").constraint(), Some(constraint));
                            }
                            writer.client().batch_execute("ROLLBACK").await.expect("permission shape attempt rolled back");
                            assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("independent complete rollback after array shape check").get::<_, String>(0), before, "{constraint}: {permissions}");
                        }
                    }
                    for name in ["".into(), "字".repeat(65), "Cafe\u{301}".into(), "Interior\u{001f}control".into(), "Interior\u{0085}control".into(), "\u{00a0}Leading".into(), "Trailing\u{3000}".into(), "\u{2007}Leading".into(), "Trailing\u{202f}".into()] {
                        writer.client().batch_execute("BEGIN").await.expect("isolated row-local name check");
                        let error = writer.client().execute("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT gen_random_uuid(),gen_random_uuid(),NULL,1,scope,entry,action,$1,level,permissions,operator_id,changed_at,gen_random_uuid(),seal,seal_key_id,seal_version FROM access.changes WHERE id=$2", &[&name, &institution.change_id()]).await.expect_err("noncanonical stored name");
                        assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                        assert_eq!(error.as_db_error().expect("row-local constraint").constraint(), Some("changes_name_check"));
                        writer.client().batch_execute("ROLLBACK").await.expect("invalid row rolled back");
                        assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("unchanged complete facts").get::<_, String>(0), before);
                    }
                    writer.terminate().await.expect("writer ended");
                    observer.terminate().await.expect("observer ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                })).await.expect("bounded structural pair and family checks");
            }));
    }

    #[test]
    fn runtime_role_heads_require_matching_objects_and_contiguous_records() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let input = |name: &str| serde_json::from_value(serde_json::json!({"name":name,"level":200,"permissions":[]})).expect("role input");
                    let created = Box::pin(services.role().create(manager.clone(), Uuid::new_v4().to_string().parse().expect("creation"), input("Original"))).await.expect("actual first record");
                    let current = Box::pin(services.role().update(manager.clone(), Uuid::new_v4().to_string().parse().expect("modification"), created.id(), Update::new(created.change_id(), input("Current")))).await.expect("actual contiguous successor");
                    let other = Box::pin(services.role().create(manager, Uuid::new_v4().to_string().parse().expect("other creation"), input("Other object"))).await.expect("actual separate object");
                    let writer = database.connections(&database.user).recover().await.expect("runtime writer");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let rows = observer.client().query("SELECT id,previous_id,sequence FROM access.changes WHERE role_id=$1 ORDER BY sequence", &[&current.id()]).await.expect("actual committed object sequence");
                    assert_eq!(rows.len(), 2);
                    assert_eq!(rows[0].get::<_, Option<crate::access::role::change::Id>>("previous_id"), None);
                    assert_eq!(rows[1].get::<_, Option<crate::access::role::change::Id>>("previous_id"), Some(created.change_id()));
                    assert_eq!(rows[1].get::<_, i64>("sequence"), 2);
                    let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("complete authenticated seeds").get(0);
                    let candidate = Uuid::new_v4();
                    // The copied seals deliberately authenticate only their original data. Every
                    // candidate below is a SQL structure counterexample and is rolled back.
                    let current_id = Uuid::from_bytes(*current.id().as_bytes());
                    let current_change = Uuid::from_bytes(*current.change_id().as_bytes());
                    let other_change = Uuid::from_bytes(*other.change_id().as_bytes());
                    let record = |previous: crate::access::role::change::Id, sequence| {
                        let previous = Uuid::from_bytes(*previous.as_bytes());
                        format!("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT '{candidate}',role_id,'{previous}',{sequence},scope,entry,action,name,level,permissions,operator_id,changed_at,gen_random_uuid(),seal,seal_key_id,seal_version FROM access.changes WHERE id='{}'", current_change)
                    };
                    let point = |sequence| format!("UPDATE access.roles SET change_id='{candidate}',sequence={sequence},version=version+1 WHERE id='{}'", current_id);
                    for (name, statement, code, constraint) in [
                        ("predecessor belongs to another object", record(other.change_id(), 3), "23503", Some("changes_previous_id_role_id_scope_fkey")),
                        ("duplicate object sequence", record(created.change_id(), 2), "23505", Some("changes_role_id_sequence_key")),
                        ("state skips a newly allocated record", format!("{}; {}; SET CONSTRAINTS access.roles_records_check IMMEDIATE", record(current.change_id(), 4), point(4)), "23514", Some("roles_records_check")),
                        ("head count differs from its record", format!("{}; {}; SET CONSTRAINTS access.roles_change_id_id_scope_sequence_changed_at_fkey IMMEDIATE", record(current.change_id(), 3), point(4)), "23503", Some("roles_change_id_id_scope_sequence_changed_at_fkey")),
                        ("head belongs to another object", format!("UPDATE access.roles SET change_id='{}',sequence=1,version=version+1 WHERE id='{}'; SET CONSTRAINTS access.roles_change_id_id_scope_sequence_changed_at_fkey IMMEDIATE", other_change, current_id), "23503", Some("roles_change_id_id_scope_sequence_changed_at_fkey")),
                        ("head record is absent", format!("{}; SET CONSTRAINTS access.roles_change_id_id_scope_sequence_changed_at_fkey IMMEDIATE", point(3)), "23503", Some("roles_change_id_id_scope_sequence_changed_at_fkey")),
                        ("head time differs from its record", format!("{}; {}; UPDATE access.roles SET changed_at=changed_at+interval '1 microsecond' WHERE id='{}'; SET CONSTRAINTS access.roles_change_id_id_scope_sequence_changed_at_fkey IMMEDIATE", record(current.change_id(), 3), point(3), current_id), "23503", Some("roles_change_id_id_scope_sequence_changed_at_fkey")),
                        ("version does not advance", format!("{}; UPDATE access.roles SET change_id='{candidate}',sequence=3 WHERE id='{}'; SET CONSTRAINTS access.roles_records_check IMMEDIATE", record(current.change_id(), 3), current_id), "23514", Some("roles_records_check")),
                        ("version skips a write", format!("{}; UPDATE access.roles SET change_id='{candidate}',sequence=3,version=version+2 WHERE id='{}'; SET CONSTRAINTS access.roles_records_check IMMEDIATE", record(current.change_id(), 3), current_id), "23514", Some("roles_records_check")),
                        ("state sequence does not advance", format!("UPDATE access.roles SET version=version+1 WHERE id='{}'; SET CONSTRAINTS access.roles_records_check IMMEDIATE", current_id), "23514", Some("roles_records_check")),
                        ("state object identity changes", format!("UPDATE access.roles SET id=gen_random_uuid(),version=version+1 WHERE id='{}'; SET CONSTRAINTS access.roles_records_check IMMEDIATE", current_id), "23514", Some("roles_records_check")),
                    ] {
                        writer.client().batch_execute("BEGIN").await.expect("isolated structural counterexample");
                        let error = writer.client().batch_execute(&statement).await.expect_err(name);
                        assert_eq!(error.code().map(tokio_postgres::error::SqlState::code), Some(code), "{name}: {error:?}");
                        if let Some(constraint) = constraint {
                            assert_eq!(error.as_db_error().expect("structural constraint").constraint(), Some(constraint), "{name}");
                        }
                        writer.client().batch_execute("ROLLBACK").await.expect("counterexample rolled back");
                        assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("independent no partial head or history").get::<_, String>(0), before, "{name}");
                    }
                    writer.terminate().await.expect("writer ended");
                    observer.terminate().await.expect("observer ended");
                    services.close();
                    services.drain().await.expect("drained");
                    context.stop().await.expect("stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                })).await.expect("bounded structural head checks");
            }));
    }

    #[test]
    fn original_person_survives_later_identity_and_role_changes_but_current_permission_is_required()
    {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20),async {
                let database=Database::initialized(server).await;
                let mut supplier=Supplier::new().await;
                let (context,services)=start(database.connections(&database.user),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let owner=database.connections("postgres").recover().await.expect("isolated future lifecycle owner");
                let observer=database.connections(&database.user).recover().await.expect("independent runtime observer");
                let operation=Uuid::new_v4().to_string().parse().expect("operation");
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Original","level":500,"permissions":["institution.accounts"]})).expect("input");
                let created=services.role().create(actor.clone(),operation,input.clone()).await.expect("initial authorized definition");
                let modification=Uuid::new_v4().to_string().parse().expect("modification");
                let changed:Input=serde_json::from_value(serde_json::json!({"name":"Modified","level":499,"permissions":["institution.accounts"]})).expect("modified definition");
                let modified=services.role().update(actor.clone(),modification,created.id(),Update::new(created.change_id(),changed.clone())).await.expect("legitimate modification before later identity and deletion");
                let row=observer.client().query_one("SELECT revision.person_id,change.operator_id FROM access.changes change JOIN identity.revisions revision ON revision.id=change.operator_id WHERE change.id=$1",&[&created.change_id()]).await.expect("original operator before later changes");
                let person=row.get::<_,crate::identity::person::Id>("person_id");
                let original_revision=row.get::<_,revision::Id>("operator_id");
                let reader=insert_role(owner.client(), services.role(),&actor,"Restricted maintainer",300,&["institution.roles"]).await;
                let assignments=insert_role(owner.client(), services.role(),&actor,"Assignments only",200,&["institution.assignments"]).await;
                crate::correction::tests::rename_administrator(
                    &services, person, original_revision, "Later Operator", "000000200002290010",
                ).await;
                services.role().delete(actor.clone(), Uuid::new_v4().to_string().parse().expect("later deletion"), created.id(), Delete::new(modified.change_id())).await.expect("legitimate later deletion");
                assign_role(owner.client(), services.role(),&actor,reader).await;
                let row=observer.client().query_one("SELECT change.operator_id,NOT role.active AS deleted FROM access.changes change JOIN access.roles role ON role.id=change.role_id WHERE change.id=$1",&[&created.change_id()]).await.expect("independent old definition and current tombstone before replay");
                assert_eq!(row.get::<_,revision::Id>("operator_id"),original_revision); assert!(row.get::<_,bool>("deleted"));
                let replay=services.role().create(actor.clone(),operation,input.clone()).await.expect("same person retains original result before new level and subset rules");
                assert_eq!(replay.id(),created.id()); assert_eq!(replay.change_id(),created.change_id()); assert_eq!(replay.permissions(),created.permissions());
                let replay=services.role().update(actor.clone(),modification,created.id(),Update::new(created.change_id(),changed)).await.expect("original modification survives a later legitimate deletion and identity revision");
                assert_eq!(replay.change_id(),modified.change_id()); assert_eq!(replay.name(),modified.name()); assert_eq!(replay.level(),modified.level()); assert_eq!(replay.permissions(),modified.permissions());
                for (level,permissions) in [(300,vec!["institution.roles"]),(200,vec!["institution.accounts"])] {
                    let next:Input=serde_json::from_value(serde_json::json!({"name":"Denied","level":level,"permissions":permissions})).expect("typed input");
                    assert!(matches!(services.role().create(actor.clone(),Uuid::new_v4().to_string().parse().expect("operation"),next).await,Err(kernel::Error::Denied)));
                }
                assign_role(owner.client(), services.role(),&actor,assignments).await;
                assert!(matches!(services.role().create(actor.clone(),operation,input).await,Err(kernel::Error::Denied)),"maintenance permission precedes original result");
                assign_role(owner.client(), services.role(),&actor,reader).await;
                let expiry:chrono::DateTime<chrono::Utc>=observer.client().query_one("SELECT expires_at FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("original expiry").get(0);
                crate::registration::tests::age_facts(&database,&services,expiry-database.now().await-chrono::TimeDelta::seconds(3)).await;
                owner.client().batch_execute("BEGIN").await.expect("hold account before decisive qualification");
                let blocker:i32=owner.client().query_one("SELECT pg_backend_pid(),account.id FROM identity.accounts account JOIN identity.sessions session ON session.account_id=account.id WHERE session.id=$1 FOR UPDATE OF account",&[&actor.session_id()]).await.expect("exclusive account gate").get(0);
                let next_operation=Uuid::new_v4().to_string().parse().expect("operation");
                let next:Input=serde_json::from_value(serde_json::json!({"name":"Locked","level":200,"permissions":[]})).expect("input");
                let service=services.role().clone(); let current=actor.clone();
                let task=tokio::spawn(async move {service.create(current,next_operation,next).await});
                timeout(Duration::from_secs(3),async {loop {
                    let waiting:bool=observer.client().query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND query='SELECT id FROM identity.accounts WHERE id=$1 FOR SHARE' AND wait_event_type='Lock' AND $1=ANY(pg_blocking_pids(pid)))",&[&blocker]).await.expect("actual account lock wait").get(0);
                    if waiting {break;} tokio::task::yield_now().await;
                }}).await.expect("write waits before qualification");
                assert!(observer.client().query_one("SELECT expires_at>clock_timestamp() FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("credential remains valid when the Account lock blocks").get::<_,bool>(0));
                timeout(Duration::from_secs(4),async {loop {
                    if observer.client().query_one("SELECT expires_at<=clock_timestamp() FROM identity.sessions WHERE id=$1",&[&actor.session_id()]).await.expect("real expiry before decisive qualification").get::<_,bool>(0) {break;}
                    tokio::task::yield_now().await;
                }}).await.expect("bounded session expiry");
                owner.client().batch_execute("COMMIT").await.expect("release changed qualification");
                assert!(matches!(task.await.expect("worker"),Err(kernel::Error::Unauthenticated)));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&next_operation]).await.expect("no partial acceptance").get::<_,i64>(0),0);
                services.close(); services.drain().await.expect("drained"); owner.terminate().await.expect("owner ended"); observer.terminate().await.expect("observer ended"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await;
            }).await.expect("bounded role qualification");
        });
    }

    #[test]
    fn competing_requests_and_confirmed_rollback_do_not_duplicate_original_facts() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20),async {
                let database=Database::initialized(server.clone()).await;
                let mut supplier=Supplier::new().await;
                let proxy=Proxy::start(&server).await;
                let (context,services)=start(authentication::proxied(&database,&proxy),&supplier).await;
                let actor=actor(&database,&services,&mut supplier).await;
                let observer=database.connections(&database.user).recover().await.expect("observer");
                let input:Input=serde_json::from_value(serde_json::json!({"name":"Atomic role","level":200,"permissions":[]})).expect("input");
                let operation=Uuid::new_v4().to_string().parse().expect("operation");
                proxy.arm(Fault::RolledBack);
                let result=services.role().create(actor.clone(),operation,input.clone()).await;
                assert!(matches!(result,Err(kernel::Error::Unavailable(ref fault)) if fault.stage()==kernel::Stage::Commit),"confirmed missing original is not unknown");
                timeout(Duration::from_secs(2),proxy.intercepted()).await.expect("actual first role COMMIT rolled back");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&operation]).await.expect("independent absence before retry").get::<_,i64>(0),0);
                let (first,second)=tokio::join!(services.role().create(actor.clone(),operation,input.clone()),services.role().create(actor.clone(),operation,input.clone()));
                let first=first.expect("first accepted caller"); let second=second.expect("matching concurrent caller");
                assert_eq!(first.id(),second.id()); assert_eq!(first.change_id(),second.change_id());
                let original_id=first.id();
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=$1",&[&operation]).await.expect("one original acceptance").get::<_,i64>(0),1);
                let competing:Input=serde_json::from_value(serde_json::json!({"name":"Concurrent name","level":200,"permissions":[]})).expect("input");
                let operations=[Uuid::new_v4().to_string().parse().expect("operation"),Uuid::new_v4().to_string().parse().expect("operation")];
                let (first,second)=tokio::join!(services.role().create(actor.clone(),operations[0],competing.clone()),services.role().create(actor.clone(),operations[1],competing));
                assert!(matches!((&first,&second),(Ok(_),Err(kernel::Error::Rejected(Rejection::NameTaken)))|(Err(kernel::Error::Rejected(Rejection::NameTaken)),Ok(_))));
                assert_eq!(observer.client().query_one("SELECT count(*) FROM access.changes WHERE operation_id=ANY($1::uuid[])",&[&operations.as_slice()]).await.expect("losing name transaction has no permanent orphan").get::<_,i64>(0),1);
                proxy.arm(Fault::StalledReadCommit);
                let service=services.role().clone(); let current=actor.clone(); let command=input.clone();
                let replay=tokio::spawn(async move {service.create(current,operation,command).await});
                timeout(Duration::from_secs(3),proxy.intercepted()).await.expect("original definition read completed before COMMIT response");
                proxy.arm(Fault::RejectedBegin); proxy.release();
                let repeated=replay.await.expect("replay caller").expect("known original does not enter a second recovery");
                assert_eq!(repeated.id(),original_id);
                // The armed BEGIN must remain untouched: the original read never needs a second transaction.
                assert!(timeout(Duration::from_millis(30),proxy.intercepted()).await.is_err());
                services.close(); services.drain().await.expect("work drained"); observer.terminate().await.expect("observer ended"); context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30),supplier.request()).await.is_err()); supplier.stop().await; proxy.stop().await;
            }).await.expect("bounded competing work");
        });
    }

    #[test]
    fn runtime_role_advancement_requires_records_from_its_own_transaction() {
        const SNAPSHOT: &str = "SELECT jsonb_build_array((SELECT jsonb_agg(role ORDER BY id) FROM access.roles role),(SELECT jsonb_agg(change ORDER BY id) FROM access.changes change),(SELECT jsonb_agg(completion ORDER BY family,operation_id) FROM access.completions completion),(SELECT jsonb_agg(entry ORDER BY change_id,capability_id) FROM access.entries entry),(SELECT jsonb_agg(permission ORDER BY role_id,capability_id) FROM access.permissions permission))::text";
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(server).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let operation = Uuid::new_v4().to_string().parse().expect("actual operation");
                    let created = Box::pin(services.role().create(manager, operation, serde_json::from_value(serde_json::json!({"name":"Transaction ownership","level":200,"permissions":[]})).expect("role input"))).await.expect("actual authenticated seed");
                    services.close();
                    services.drain().await.expect("drained before raw SQL counterexample");
                    context.stop().await.expect("stopped before copied seals are stored");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err());
                    supplier.stop().await;
                    let first = database.connections(&database.user).recover().await.expect("record writer");
                    let second = database.connections(&database.user).recover().await.expect("state writer");
                    let observer = database.connections(&database.user).recover().await.expect("independent observer");
                    let original: String = observer.client().query_one("SELECT row_to_json(role)::text FROM access.roles role WHERE id=$1", &[&created.id()]).await.expect("original state").get(0);
                    let record = Uuid::new_v4();
                    let candidate_operation = Uuid::new_v4();
                    // Only SQL structure is under test. These copied seals are deliberately
                    // unauthenticated for the new IDs; no application is started or queried again.
                    first.client().batch_execute("BEGIN").await.expect("separate record transaction");
                    assert_eq!(first.client().execute("INSERT INTO access.completions(family,operation_id,action,target,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version) SELECT family,$1,'update',$2::uuid::text,input,subject,person_id,revision_id,created_at,expires_at,seal,seal_key_id,seal_version FROM access.completions WHERE family='institution_role' AND operation_id=$3", &[&candidate_operation, &created.id(), &operation]).await.expect("complete copied completion"), 1);
                    assert_eq!(first.client().execute("INSERT INTO access.changes(id,role_id,previous_id,sequence,scope,entry,action,name,level,permissions,operator_id,changed_at,operation_id,seal,seal_key_id,seal_version) SELECT $1,role_id,id,sequence+1,scope,entry,'update',name,level,permissions,operator_id,changed_at,$2,seal,seal_key_id,seal_version FROM access.changes WHERE id=$3", &[&record, &candidate_operation, &created.change_id()]).await.expect("complete copied record for existing object"), 1);
                    assert!(first.client().query_one("SELECT xmin=pg_current_xact_id()::text::xid FROM access.changes WHERE id=$1", &[&record]).await.expect("record belongs to its inserting transaction").get::<_, bool>(0));
                    first.client().batch_execute("COMMIT").await.expect("record append alone has no reverse state trigger");
                    assert_eq!(observer.client().query_one("SELECT row_to_json(role)::text FROM access.roles role WHERE id=$1", &[&created.id()]).await.expect("record append did not advance state").get::<_, String>(0), original);
                    let before: String = observer.client().query_one(SNAPSHOT, &[]).await.expect("independent complete committed SQL fixture").get(0);
                    second.client().batch_execute("BEGIN").await.expect("later state transaction");
                    assert!(!second.client().query_one("SELECT xmin=pg_current_xact_id()::text::xid FROM access.changes WHERE id=$1", &[&record]).await.expect("record is visible but belongs to another transaction").get::<_, bool>(0));
                    assert_eq!(second.client().execute("UPDATE access.roles SET change_id=$2,sequence=sequence+1,version=version+1 WHERE id=$1", &[&created.id(), &record]).await.expect("head FK and row shape permit the candidate until deferred validation"), 1);
                    let error = second.client().batch_execute("COMMIT").await.expect_err("state cannot consume an earlier transaction's record");
                    assert_eq!(error.code(), Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION));
                    assert_eq!(error.as_db_error().expect("state structural constraint").constraint(), Some("roles_records_check"));
                    second.client().batch_execute("ROLLBACK").await.expect("failed state attempt ended");
                    assert_eq!(observer.client().query_one(SNAPSHOT, &[]).await.expect("independent rollback includes every role fact").get::<_, String>(0), before);
                    first.terminate().await.expect("record writer ended");
                    second.terminate().await.expect("state writer ended");
                    observer.terminate().await.expect("observer ended");
                })).await.expect("bounded transaction ownership check");
            }));
    }

    #[test]
    fn creation_commits_complete_definition_and_replays_its_own_operation_family() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(20), async {
                let database = Database::initialized(server).await;
                let mut supplier = Supplier::new().await;
                let (context, services) = start(database.connections(&database.user), &supplier).await;
                let actor = actor(&database, &services, &mut supplier).await;
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                let row = observer.client().query_one("SELECT appointment.operation_id, person.sequence AS person_sequence FROM access.appointments appointment JOIN identity.persons person ON person.id=appointment.person_id WHERE appointment.source='initialization'", &[]).await.expect("original initialization");
                let operation = row.get("operation_id");
                let sequence: i64 = row.get("person_sequence");
                let input: Input = serde_json::from_value(serde_json::json!({"name":"  Cafe\u{301}  ","level":300,"permissions":["institution.roles","institution.accounts"]})).expect("canonical typed input");
                let created = services.role().create(actor.clone(), operation, input.clone()).await.expect("online family may reuse initialization UUID");
                assert_eq!(created.kind(), Kind::Custom);
                assert_eq!(i16::from(created.level()), 300);
                assert_eq!(created.name(), &"Café".parse::<Name>().expect("expected normalized name"));
                assert_eq!(created.permissions(), [Code::InstitutionAccounts, Code::InstitutionRoles]);
                let original: String = observer.client().query_one("SELECT jsonb_build_object('role',to_jsonb(role),'change',to_jsonb(change),'permissions',(SELECT jsonb_agg(permission ORDER BY capability_id) FROM access.permissions permission WHERE role_id=role.id),'entries',(SELECT jsonb_agg(entry ORDER BY capability_id) FROM access.entries entry WHERE change_id=change.id))::text FROM access.roles role JOIN access.changes change ON change.id=role.change_id WHERE role.id=$1", &[&created.id()]).await.expect("independent complete committed graph before replay").get(0);
                let row = observer.client().query_one("SELECT (SELECT count(*) FROM access.changes WHERE operation_id=$1) AS changes, (SELECT sequence AS person_sequence FROM identity.persons LIMIT 1) AS sequence", &[&operation]).await.expect("separate operation families");
                assert_eq!(row.get::<_, i64>("changes"), 5);
                assert_eq!(row.get::<_, i64>("sequence"), sequence);
                let replay: Input = serde_json::from_value(serde_json::json!({"name":"Café","level":300,"permissions":["institution.accounts","institution.roles"]})).expect("same canonical input");
                let repeated = services.role().create(actor.clone(), operation, replay).await.expect("original definition");
                assert_eq!((repeated.id(), repeated.change_id()), (created.id(), created.change_id()));
                let different: Input = serde_json::from_value(serde_json::json!({"name":"Changed","level":300,"permissions":[]})).expect("valid different input");
                assert!(matches!(services.role().create(actor.clone(), operation, different).await, Err(kernel::Error::Mismatch)));
                let another = Uuid::new_v4().to_string().parse().expect("different operation");
                assert!(matches!(services.role().create(actor, another, input.clone()).await, Err(kernel::Error::Rejected(Rejection::NameTaken))));
                let id=message(&database,services.authentication(),&mut supplier,"13800138000").await;
                let Receipt::Registration(credential)=Box::pin(services.authentication().create_session(authentication::session::tests::input(Uuid::new_v4(),id,"13800138000",CODE))).await.expect("new R1 identity") else {panic!("unregistered person")};
                let issued=Box::pin(services.registration().register_input_for_test(crate::registration::Input::new(Uuid::new_v4().to_string().parse().expect("operation"),credential.id(),credential.token().text().parse().expect("token"),"Other Operator".parse().expect("name"),"00000020000229007X".parse().expect("number")))).await.expect("actual other account");
                let other=services.current().authenticate(current::Proof::Cookie {id:issued.id(),token:issued.token().text().parse().expect("token")}).await.expect("other current actor");
                let owner=database.connections("postgres").recover().await.expect("isolated later assignment owner");
                assign_role(owner.client(), services.role(),&other,created.id()).await;
                assert!(matches!(services.role().create(other,operation,input).await,Err(kernel::Error::Mismatch)),"maintenance permission never rebinds another person's original operation");
                owner.terminate().await.expect("owner ended");
                let after: String = observer.client().query_one("SELECT jsonb_build_object('role',to_jsonb(role),'change',to_jsonb(change),'permissions',(SELECT jsonb_agg(permission ORDER BY capability_id) FROM access.permissions permission WHERE role_id=role.id),'entries',(SELECT jsonb_agg(entry ORDER BY capability_id) FROM access.entries entry WHERE change_id=change.id))::text FROM access.roles role JOIN access.changes change ON change.id=role.change_id WHERE role.id=$1", &[&created.id()]).await.expect("original facts unchanged").get(0);
                assert_eq!(after, original);
                services.close();
                services.drain().await.expect("work drained");
                observer.terminate().await.expect("observer ended");
                context.stop().await.expect("context ended");
                assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no supplier work during creation, replay or drain");
                supplier.stop().await;
            }).await.expect("bounded creation");
        });
    }
}
