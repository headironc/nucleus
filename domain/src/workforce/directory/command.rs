//! Directory configuration commands: locks, decisive facts, pure decisions, writes and replays.

use chrono::{DateTime, Utc};

use crate::{
    access::{grant::member, role},
    identity::current::{self, Actor},
    kernel::{
        Authorization, Command,
        completion::{Request, Template},
        tx::Tx,
    },
    workforce::{
        directory::{
            self, Creation, Error, Item, Receipt, Rejection, Target,
            database::{self, Lineage, Parent, RuleDepartment},
            department, roster, rule,
        },
        number::Subtype,
    },
};

/// The acting session whose current Workforce membership must hold `workforce.directory`.
pub(super) struct Session {
    pub(super) actor: Actor,
    pub(super) current: current::Service,
    pub(super) roles: role::Reader,
}

impl Session {
    /// Scope, account, membership and directory locks precede each command's own targets.
    async fn lock(&self, tx: &Tx, targets: &[Target]) -> Result<(), Error> {
        tx.shared(b"access/scope", b"workforce").await?;
        self.actor.lock_account(tx.client()).await?;
        self.actor.lock_session(tx.client()).await?;
        member::lock_current(tx.client(), self.actor.account_id()).await?;
        database::lock_directory(tx.client()).await?;
        for target in targets {
            database::lock_target(tx.client(), *target).await?;
        }
        Ok(())
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject =
            subject.ok_or_else(|| Error::corrupt("a directory command has a qualified session"))?;
        let (_, person_id, revision_id, now) = member::authorize_subject(
            tx.client(),
            &self.roles,
            subject,
            role::Code::WorkforceDirectory,
        )
        .await?;
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }
}

/// The parent facts of a department, discipline or title.
pub(super) enum Ancestry {
    None,
    Department(Lineage),
    /// Whether the required category or discipline exists.
    Title(bool),
}

async fn ancestry(tx: &Tx, item: &ParentOf) -> Result<Ancestry, Error> {
    Ok(match *item {
        ParentOf::None => Ancestry::None,
        ParentOf::Department {
            group_id,
            parent_id,
        } => Ancestry::Department(database::fetch_lineage(tx.client(), group_id, parent_id).await?),
        ParentOf::Title(parent) => {
            Ancestry::Title(database::find_target(tx.client(), parent).await?.is_some())
        }
    })
}

/// Which parent a creation or replacement names.
enum ParentOf {
    None,
    Department {
        group_id: directory::group::Id,
        parent_id: Option<department::Id>,
    },
    Title(Target),
}

impl ParentOf {
    fn of_creation(input: &Creation) -> Self {
        match input {
            Creation::Department(input) => Self::Department {
                group_id: input.group_id,
                parent_id: input.parent_id,
            },
            Creation::Discipline(input) => Self::Title(Target::Category(input.category_id)),
            Creation::Title(input) => Self::Title(Target::Discipline(input.discipline_id)),
            Creation::Group(_) | Creation::Position(_) | Creation::Category(_) => Self::None,
        }
    }

    fn of_item(item: &Item) -> Self {
        match item {
            Item::Department(item) => Self::Department {
                group_id: item.group_id,
                parent_id: item.parent_id,
            },
            Item::Discipline(item) => Self::Title(Target::Category(item.category_id)),
            Item::Title(item) => Self::Title(Target::Discipline(item.discipline_id)),
            Item::Group(_) | Item::Position(_) | Item::Category(_) => Self::None,
        }
    }
}

/// A real group, and a parent that exists in that same group.
fn check_lineage(lineage: &Lineage, group_id: directory::group::Id) -> Result<(), Rejection> {
    if !lineage.group {
        return Err(Rejection::Absent);
    }
    match lineage.parent {
        Parent::Top => Ok(()),
        Parent::Missing => Err(Rejection::Absent),
        Parent::In(parent_group) if parent_group != group_id => Err(Rejection::InvalidParent),
        Parent::In(_) => Ok(()),
    }
}

fn check_ancestry(
    ancestry: &Ancestry,
    group_id: Option<directory::group::Id>,
) -> Result<(), Rejection> {
    match (ancestry, group_id) {
        (Ancestry::Department(lineage), Some(group_id)) => check_lineage(lineage, group_id),
        (Ancestry::Title(false), _) => Err(Rejection::Absent),
        _ => Ok(()),
    }
}

/// The item that currently answers a create, update or delete replay, if it still exists.
fn receipt(option: Option<Item>) -> Receipt {
    match option {
        Some(item) => Receipt::Present(item),
        None => Receipt::Deleted,
    }
}

async fn name_taken(tx: &Tx, item: NameOf<'_>) -> Result<bool, Error> {
    match item {
        NameOf::Directory(name, except) => database::name_taken(tx.client(), name, except).await,
        NameOf::Position(name, except) => {
            database::position_name_taken(tx.client(), name, except).await
        }
        NameOf::Title(name, except) => database::title_name_taken(tx.client(), name, except).await,
    }
}

/// A name in its scope, excluding the item being renamed.
enum NameOf<'a> {
    Directory(&'a directory::Name, Option<Target>),
    Position(&'a directory::Name, Option<directory::position::Id>),
    Title(&'a directory::Name, Option<Target>),
}

impl<'a> NameOf<'a> {
    fn of_creation(input: &'a Creation) -> Self {
        match input {
            Creation::Group(input) => Self::Directory(&input.name, None),
            Creation::Department(input) => Self::Directory(&input.name, None),
            Creation::Position(input) => Self::Position(&input.name, None),
            Creation::Category(input) => Self::Title(&input.name, None),
            Creation::Discipline(input) => Self::Title(&input.name, None),
            Creation::Title(input) => Self::Title(&input.name, None),
        }
    }

    fn of_item(item: &'a Item, target: Target) -> Self {
        match item {
            Item::Group(item) => Self::Directory(&item.name, Some(target)),
            Item::Department(item) => Self::Directory(&item.name, Some(target)),
            Item::Position(item) => Self::Position(&item.name, Some(item.id)),
            Item::Category(item) => Self::Title(&item.name, Some(target)),
            Item::Discipline(item) => Self::Title(&item.name, Some(target)),
            Item::Title(item) => Self::Title(&item.name, Some(target)),
        }
    }
}

/// Creates one item in its collection.
pub(super) struct Create {
    pub(super) session: Session,
    pub(super) input: Creation,
}

pub(super) struct CreationFacts {
    name_taken: bool,
    ancestry: Ancestry,
}

impl Command for Create {
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.session.current, &self.session.actor))
    }

    const COMPLETIONS: Template = directory::COMPLETIONS;
    type Facts = CreationFacts;
    type Change = ();
    type Output = Receipt;
    type Rejection = Rejection;

    fn request(&self, authority: &Authorization) -> Result<Request, Error> {
        Request::new(
            "create",
            self.input.collection().as_str().to_owned(),
            &self.input,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        let targets = match &self.input {
            Creation::Department(input) => std::iter::once(Target::Group(input.group_id))
                .chain(input.parent_id.map(Target::Department))
                .collect(),
            Creation::Discipline(input) => vec![Target::Category(input.category_id)],
            Creation::Title(input) => vec![Target::Discipline(input.discipline_id)],
            Creation::Group(_) | Creation::Position(_) | Creation::Category(_) => Vec::new(),
        };
        self.session.lock(tx, &targets).await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        self.session.authorize(tx, subject).await
    }

    async fn load(&self, tx: &Tx, _: &Authorization) -> Result<CreationFacts, Error> {
        Ok(CreationFacts {
            name_taken: name_taken(tx, NameOf::of_creation(&self.input)).await?,
            ancestry: ancestry(tx, &ParentOf::of_creation(&self.input)).await?,
        })
    }

    fn decide(&self, facts: CreationFacts, _: DateTime<Utc>) -> Result<(), Error> {
        if facts.name_taken {
            return Err(Rejection::NameInUse.into());
        }
        let group_id = match &self.input {
            Creation::Department(input) => Some(input.group_id),
            _ => None,
        };
        Ok(check_ancestry(&facts.ancestry, group_id)?)
    }

    async fn apply(
        &self,
        tx: &Tx,
        (): (),
        _: &mut Option<crate::history::Pending>,
    ) -> Result<Receipt, Error> {
        Ok(Receipt::Present(
            database::insert(tx.client(), tx.operation_id(), &self.input).await?,
        ))
    }

    async fn replay(&self, tx: &Tx, _: &Request, _: DateTime<Utc>) -> Result<Receipt, Error> {
        Ok(receipt(
            database::find_creation(tx.client(), self.input.collection(), tx.operation_id())
                .await?,
        ))
    }
}

/// Replaces one item after comparing its complete expected value.
pub(super) struct Update {
    pub(super) session: Session,
    pub(super) target: Target,
    pub(super) input: directory::Update,
}

pub(super) struct UpdateFacts {
    current: Option<Item>,
    name_taken: bool,
    ancestry: Ancestry,
    /// The replacement parent is the department itself or one of its descendants.
    descendant: bool,
}

pub(super) enum Modification {
    /// The current value already equals the replacement; only the completion is appended.
    Reached(Item),
    Replace,
}

impl Command for Update {
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.session.current, &self.session.actor))
    }

    const COMPLETIONS: Template = directory::COMPLETIONS;
    type Facts = UpdateFacts;
    type Change = Modification;
    type Output = Receipt;
    type Rejection = Rejection;

    fn request(&self, authority: &Authorization) -> Result<Request, Error> {
        Request::new("update", self.target.canonical(), &self.input, authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        self.session.lock(tx, &[self.target]).await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        self.session.authorize(tx, subject).await
    }

    async fn load(&self, tx: &Tx, _: &Authorization) -> Result<UpdateFacts, Error> {
        let replacement = self.input.replacement();
        let descendant = match replacement {
            Item::Department(item) => match item.parent_id {
                Some(parent_id) => database::is_descendant(tx.client(), item.id, parent_id).await?,
                None => false,
            },
            _ => false,
        };
        Ok(UpdateFacts {
            current: database::find_target(tx.client(), self.target).await?,
            name_taken: name_taken(tx, NameOf::of_item(replacement, self.target)).await?,
            ancestry: ancestry(tx, &ParentOf::of_item(replacement)).await?,
            descendant,
        })
    }

    fn decide(&self, facts: UpdateFacts, _: DateTime<Utc>) -> Result<Modification, Error> {
        let current = facts.current.ok_or(Rejection::Absent)?;
        if &current == self.input.replacement() {
            return Ok(Modification::Reached(current));
        }
        if &current != self.input.expected() {
            return Err(Rejection::Changed.into());
        }
        if facts.name_taken {
            return Err(Rejection::NameInUse.into());
        }
        match self.input.replacement() {
            Item::Department(item) => {
                check_ancestry(&facts.ancestry, Some(item.group_id))?;
                if item.parent_id == Some(item.id) || facts.descendant {
                    return Err(Rejection::InvalidParent.into());
                }
            }
            _ => check_ancestry(&facts.ancestry, None)?,
        }
        Ok(Modification::Replace)
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: Modification,
        _: &mut Option<crate::history::Pending>,
    ) -> Result<Receipt, Error> {
        Ok(Receipt::Present(match change {
            Modification::Reached(item) => item,
            Modification::Replace => {
                database::update(tx.client(), self.input.expected(), self.input.replacement())
                    .await?
            }
        }))
    }

    async fn replay(&self, tx: &Tx, _: &Request, _: DateTime<Utc>) -> Result<Receipt, Error> {
        Ok(receipt(
            database::find_target(tx.client(), self.target).await?,
        ))
    }
}

/// Deletes one unreferenced item after comparing its complete expected value.
pub(super) struct Delete {
    pub(super) session: Session,
    pub(super) target: Target,
    pub(super) input: directory::Delete,
}

pub(super) struct DeletionFacts {
    current: Option<Item>,
    referenced: bool,
}

impl Command for Delete {
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.session.current, &self.session.actor))
    }

    const COMPLETIONS: Template = directory::COMPLETIONS;
    type Facts = DeletionFacts;
    type Change = ();
    type Output = Receipt;
    type Rejection = Rejection;

    fn request(&self, authority: &Authorization) -> Result<Request, Error> {
        Request::new("delete", self.target.canonical(), &self.input, authority)
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        self.session.lock(tx, &[self.target]).await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        self.session.authorize(tx, subject).await
    }

    async fn load(&self, tx: &Tx, _: &Authorization) -> Result<DeletionFacts, Error> {
        let current = database::find_target(tx.client(), self.target).await?;
        let referenced = match &current {
            Some(item) => database::referenced(tx.client(), item).await?,
            None => false,
        };
        Ok(DeletionFacts {
            current,
            referenced,
        })
    }

    /// Like Role deletion, a new request cannot delete an already absent item.
    fn decide(&self, facts: DeletionFacts, _: DateTime<Utc>) -> Result<(), Error> {
        let current = facts.current.ok_or(Rejection::Absent)?;
        if &current != self.input.expected() {
            return Err(Rejection::Changed.into());
        }
        if facts.referenced {
            return Err(Rejection::InUse.into());
        }
        Ok(())
    }

    async fn apply(
        &self,
        tx: &Tx,
        (): (),
        _: &mut Option<crate::history::Pending>,
    ) -> Result<Receipt, Error> {
        database::delete(tx.client(), self.target).await?;
        Ok(Receipt::Deleted)
    }

    async fn replay(&self, tx: &Tx, _: &Request, _: DateTime<Utc>) -> Result<Receipt, Error> {
        Ok(receipt(
            database::find_target(tx.client(), self.target).await?,
        ))
    }
}

fn check_rule_department(department: RuleDepartment) -> Result<(), Rejection> {
    match department {
        RuleDepartment::Missing => Err(Rejection::Absent),
        RuleDepartment::Unselectable => Err(Rejection::InvalidDepartment),
        RuleDepartment::Selectable => Ok(()),
    }
}

async fn replay_rule(tx: &Tx, subtype: Subtype, created: bool) -> Result<rule::Receipt, Error> {
    let rule = database::find_rule(tx.client(), subtype)
        .await?
        .ok_or(Error::corrupt(
            "an original rule completion retains its rule",
        ))?;
    Ok(rule::Receipt { rule, created })
}

/// Creates the missing rule of one subtype.
pub(super) struct CreateRule {
    pub(super) session: Session,
    pub(super) subtype: Subtype,
    pub(super) input: rule::Creation,
}

pub(super) struct RuleCreationFacts {
    configured: bool,
    department: RuleDepartment,
}

impl Command for CreateRule {
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.session.current, &self.session.actor))
    }

    const COMPLETIONS: Template = directory::COMPLETIONS;
    type Facts = RuleCreationFacts;
    type Change = ();
    type Output = rule::Receipt;
    type Rejection = Rejection;

    fn request(&self, authority: &Authorization) -> Result<Request, Error> {
        Request::new(
            "create",
            i16::from(self.subtype).to_string(),
            &self.input,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        self.session
            .lock(tx, &[Target::Department(self.input.department_id)])
            .await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        self.session.authorize(tx, subject).await
    }

    async fn load(&self, tx: &Tx, _: &Authorization) -> Result<RuleCreationFacts, Error> {
        Ok(RuleCreationFacts {
            configured: database::find_rule(tx.client(), self.subtype)
                .await?
                .is_some(),
            department: database::find_rule_department(tx.client(), self.input.department_id)
                .await?,
        })
    }

    fn decide(&self, facts: RuleCreationFacts, _: DateTime<Utc>) -> Result<(), Error> {
        if facts.configured {
            return Err(Rejection::Changed.into());
        }
        Ok(check_rule_department(facts.department)?)
    }

    async fn apply(
        &self,
        tx: &Tx,
        (): (),
        _: &mut Option<crate::history::Pending>,
    ) -> Result<rule::Receipt, Error> {
        Ok(rule::Receipt {
            rule: database::insert_rule(tx.client(), self.subtype, &self.input).await?,
            created: true,
        })
    }

    async fn replay(&self, tx: &Tx, _: &Request, _: DateTime<Utc>) -> Result<rule::Receipt, Error> {
        replay_rule(tx, self.subtype, true).await
    }
}

/// Replaces one configured rule after comparing its complete expected value.
pub(super) struct UpdateRule {
    pub(super) session: Session,
    pub(super) subtype: Subtype,
    pub(super) input: rule::Update,
}

pub(super) struct RuleUpdateFacts {
    current: Option<rule::Rule>,
    department: RuleDepartment,
}

pub(super) enum RuleChange {
    Reached(rule::Rule),
    Replace,
}

impl Command for UpdateRule {
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.session.current, &self.session.actor))
    }

    const COMPLETIONS: Template = directory::COMPLETIONS;
    type Facts = RuleUpdateFacts;
    type Change = RuleChange;
    type Output = rule::Receipt;
    type Rejection = Rejection;

    fn request(&self, authority: &Authorization) -> Result<Request, Error> {
        Request::new(
            "update",
            i16::from(self.subtype).to_string(),
            &self.input,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        self.session
            .lock(
                tx,
                &[Target::Department(self.input.replacement.department_id)],
            )
            .await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        self.session.authorize(tx, subject).await
    }

    async fn load(&self, tx: &Tx, _: &Authorization) -> Result<RuleUpdateFacts, Error> {
        Ok(RuleUpdateFacts {
            current: database::find_rule(tx.client(), self.subtype).await?,
            department: database::find_rule_department(
                tx.client(),
                self.input.replacement.department_id,
            )
            .await?,
        })
    }

    /// Both a reached and a new replacement recheck the replacement department.
    fn decide(&self, facts: RuleUpdateFacts, _: DateTime<Utc>) -> Result<RuleChange, Error> {
        let current = facts.current.ok_or(Rejection::Absent)?;
        if current == self.input.replacement {
            check_rule_department(facts.department)?;
            return Ok(RuleChange::Reached(current));
        }
        if current != self.input.expected {
            return Err(Rejection::Changed.into());
        }
        check_rule_department(facts.department)?;
        Ok(RuleChange::Replace)
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: RuleChange,
        _: &mut Option<crate::history::Pending>,
    ) -> Result<rule::Receipt, Error> {
        let rule = match change {
            RuleChange::Reached(rule) => rule,
            RuleChange::Replace => {
                database::update_rule(tx.client(), &self.input.replacement).await?
            }
        };
        Ok(rule::Receipt {
            rule,
            created: false,
        })
    }

    async fn replay(&self, tx: &Tx, _: &Request, _: DateTime<Utc>) -> Result<rule::Receipt, Error> {
        replay_rule(tx, self.subtype, false).await
    }
}

/// Replaces one department's complete position list after comparing the complete current list.
pub(super) struct ReplaceRoster {
    pub(super) session: Session,
    pub(super) department_id: department::Id,
    pub(super) input: roster::Replace,
}

pub(super) struct RosterFacts {
    department: bool,
    /// The current list ordered by position, comparable with the canonical input.
    current: Vec<roster::Entry>,
    positions: bool,
}

pub(super) struct RosterChange {
    reached: bool,
    current: Vec<roster::Entry>,
}

impl Command for ReplaceRoster {
    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.session.current, &self.session.actor))
    }

    const COMPLETIONS: Template = directory::COMPLETIONS;
    type Facts = RosterFacts;
    type Change = RosterChange;
    type Output = roster::Receipt;
    type Rejection = Rejection;

    fn request(&self, authority: &Authorization) -> Result<Request, Error> {
        Request::new(
            "replace",
            self.department_id.to_string(),
            &self.input,
            authority,
        )
    }

    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        self.session
            .lock(tx, &[Target::Department(self.department_id)])
            .await
    }

    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        self.session.authorize(tx, subject).await
    }

    async fn load(&self, tx: &Tx, _: &Authorization) -> Result<RosterFacts, Error> {
        let mut current = database::fetch_roster(tx.client(), self.department_id).await?;
        current.sort_by_key(|entry| entry.position_id);
        Ok(RosterFacts {
            department: database::find_department(tx.client(), self.department_id)
                .await?
                .is_some(),
            current,
            positions: database::positions_present(tx.client(), &self.input.replacement).await?,
        })
    }

    fn decide(&self, facts: RosterFacts, _: DateTime<Utc>) -> Result<RosterChange, Error> {
        if !facts.department {
            return Err(Rejection::Absent.into());
        }
        let reached = facts.current.len() == self.input.replacement.len()
            && facts
                .current
                .iter()
                .zip(&self.input.replacement)
                .all(|(entry, choice)| entry.configures(choice));
        if !reached {
            if facts.current != self.input.expected {
                return Err(Rejection::Changed.into());
            }
            if !facts.positions {
                return Err(Rejection::Absent.into());
            }
        }
        Ok(RosterChange {
            reached,
            current: facts.current,
        })
    }

    async fn apply(
        &self,
        tx: &Tx,
        change: RosterChange,
        _: &mut Option<crate::history::Pending>,
    ) -> Result<roster::Receipt, Error> {
        if !change.reached {
            database::replace_roster(
                tx.client(),
                self.department_id,
                &change.current,
                &self.input.replacement,
            )
            .await?;
        }
        Ok(roster::Receipt::Present(
            database::fetch_roster(tx.client(), self.department_id).await?,
        ))
    }

    async fn replay(
        &self,
        tx: &Tx,
        _: &Request,
        _: DateTime<Utc>,
    ) -> Result<roster::Receipt, Error> {
        if database::find_department(tx.client(), self.department_id)
            .await?
            .is_none()
        {
            return Ok(roster::Receipt::Deleted);
        }
        Ok(roster::Receipt::Present(
            database::fetch_roster(tx.client(), self.department_id).await?,
        ))
    }
}
