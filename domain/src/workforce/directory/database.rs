use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};

use crate::{
    history::Keys,
    identity::operation,
    kernel::completion,
    workforce::{
        directory::{
            Collection, Error, Item, Name, Target, category, department, discipline, group,
            position, roster, rule, selection, title,
        },
        number::Subtype,
    },
};

pub(super) async fn lock_directory(client: &Client) -> Result<(), Error> {
    client
        .query_one("SELECT workforce.lock_directory()", &[])
        .await?;
    Ok(())
}

pub(super) async fn lock_target(client: &Client, target: Target) -> Result<(), Error> {
    match target {
        Target::Group(id) => {
            client
                .query_opt(
                    "SELECT id FROM workforce.groups WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await?;
        }
        Target::Department(id) => {
            client
                .query_opt(
                    "SELECT id FROM workforce.departments WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await?;
        }
        Target::Position(id) => {
            client
                .query_opt(
                    "SELECT id FROM workforce.positions WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await?;
        }
        Target::Category(id) => {
            client
                .query_opt(
                    "SELECT id FROM workforce.categories WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await?;
        }
        Target::Discipline(id) => {
            client
                .query_opt(
                    "SELECT id FROM workforce.disciplines WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await?;
        }
        Target::Title(id) => {
            client
                .query_opt(
                    "SELECT id FROM workforce.titles WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await?;
        }
    }
    Ok(())
}

/// Whether another position already holds this name.
pub(super) async fn position_name_taken(
    client: &Client,
    name: &Name,
    except: Option<position::Id>,
) -> Result<bool, Error> {
    client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM workforce.positions WHERE name = $1 AND ($2::uuid IS NULL OR id <> $2))",
            &[&name, &except],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)
}

/// Groups and departments share the first name scope.
pub(super) async fn name_taken(
    client: &Client,
    name: &Name,
    except: Option<Target>,
) -> Result<bool, Error> {
    let (group_id, department_id) = match except {
        Some(Target::Group(id)) => (Some(id), None),
        Some(Target::Department(id)) => (None, Some(id)),
        Some(
            Target::Position(_) | Target::Category(_) | Target::Discipline(_) | Target::Title(_),
        )
        | None => (None, None),
    };
    client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM workforce.groups WHERE name = $1 AND ($2::uuid IS NULL OR id <> $2)) OR EXISTS (SELECT 1 FROM workforce.departments WHERE name = $1 AND ($3::uuid IS NULL OR id <> $3))",
            &[&name, &group_id, &department_id],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)
}

/// Categories, disciplines and complete titles share the third name scope.
pub(super) async fn title_name_taken(
    client: &Client,
    name: &Name,
    except: Option<Target>,
) -> Result<bool, Error> {
    let (category_id, discipline_id, title_id) = match except {
        Some(Target::Category(id)) => (Some(id), None, None),
        Some(Target::Discipline(id)) => (None, Some(id), None),
        Some(Target::Title(id)) => (None, None, Some(id)),
        Some(Target::Group(_) | Target::Department(_) | Target::Position(_)) | None => {
            (None, None, None)
        }
    };
    client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM workforce.categories WHERE name = $1 AND ($2::uuid IS NULL OR id <> $2)) OR EXISTS (SELECT 1 FROM workforce.disciplines WHERE name = $1 AND ($3::uuid IS NULL OR id <> $3)) OR EXISTS (SELECT 1 FROM workforce.titles WHERE name = $1 AND ($4::uuid IS NULL OR id <> $4))",
            &[&name, &category_id, &discipline_id, &title_id],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)
}

/// The group and the parent's group that a department would belong to.
pub(super) struct Lineage {
    pub(super) group: bool,
    pub(super) parent: Parent,
}

pub(super) enum Parent {
    Top,
    Missing,
    In(group::Id),
}

pub(super) async fn fetch_lineage(
    client: &Client,
    group_id: group::Id,
    parent_id: Option<department::Id>,
) -> Result<Lineage, Error> {
    let group = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM workforce.groups WHERE id = $1)",
            &[&group_id],
        )
        .await?
        .try_get::<_, bool>(0)
        .map_err(Error::decode)?;
    let parent = match parent_id {
        None => Parent::Top,
        Some(parent_id) => match client
            .query_opt(
                "SELECT group_id FROM workforce.departments WHERE id = $1",
                &[&parent_id],
            )
            .await?
        {
            None => Parent::Missing,
            Some(row) => Parent::In(row.try_get("group_id").map_err(Error::decode)?),
        },
    };
    Ok(Lineage { group, parent })
}

/// Whether the parent is the department itself or one of its descendants.
pub(super) async fn is_descendant(
    client: &Client,
    id: department::Id,
    parent_id: department::Id,
) -> Result<bool, Error> {
    client
        .query_one(
            "WITH RECURSIVE subtree(id) AS (SELECT $1::uuid UNION ALL SELECT department.id FROM workforce.departments department JOIN subtree parent ON department.parent_id = parent.id) SELECT EXISTS (SELECT 1 FROM subtree WHERE id = $2)",
            &[&id, &parent_id],
        )
        .await?
        .try_get::<_, bool>(0)
        .map_err(Error::decode)
}

/// A rule's department: missing, present on a disabled path, or selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuleDepartment {
    Missing,
    Unselectable,
    Selectable,
}

pub(super) async fn find_rule_department(
    client: &Client,
    department_id: department::Id,
) -> Result<RuleDepartment, Error> {
    let present = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM workforce.departments WHERE id = $1)",
            &[&department_id],
        )
        .await?
        .try_get::<_, bool>(0)
        .map_err(Error::decode)?;
    if !present {
        return Ok(RuleDepartment::Missing);
    }
    let selectable = client
        .query_one(
            "WITH RECURSIVE lineage(id, parent_id, group_id, enabled) AS (SELECT department.id, department.parent_id, department.group_id, department.enabled FROM workforce.departments department WHERE department.id = $1 UNION SELECT department.id, department.parent_id, department.group_id, department.enabled FROM workforce.departments department JOIN lineage child ON department.id = child.parent_id) SELECT COALESCE(bool_and(lineage.enabled), false) AND COALESCE((SELECT directory_group.enabled FROM workforce.groups directory_group WHERE directory_group.id = (SELECT group_id FROM lineage LIMIT 1)), false) FROM lineage",
            &[&department_id],
        )
        .await?
        .try_get::<_, bool>(0)
        .map_err(Error::decode)?;
    Ok(if selectable {
        RuleDepartment::Selectable
    } else {
        RuleDepartment::Unselectable
    })
}

pub(super) async fn find_target(client: &Client, target: Target) -> Result<Option<Item>, Error> {
    match target {
        Target::Group(id) => find_group(client, id).await,
        Target::Department(id) => find_department(client, id).await,
        Target::Position(id) => find_position(client, id).await,
        Target::Category(id) => find_category(client, id).await,
        Target::Discipline(id) => find_discipline(client, id).await,
        Target::Title(id) => find_title(client, id).await,
    }
}

async fn find_category(client: &Client, id: category::Id) -> Result<Option<Item>, Error> {
    client
        .query_opt(
            "SELECT id, operation_id, name, enabled, index FROM workforce.categories WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|row| category::Item::try_from(row).map(Item::Category))
        .transpose()
}

async fn find_discipline(client: &Client, id: discipline::Id) -> Result<Option<Item>, Error> {
    client
        .query_opt(
            "SELECT id, operation_id, name, enabled, index, category_id FROM workforce.disciplines WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|row| discipline::Item::try_from(row).map(Item::Discipline))
        .transpose()
}

async fn find_title(client: &Client, id: title::Id) -> Result<Option<Item>, Error> {
    client
        .query_opt(
            "SELECT id, operation_id, name, enabled, index, discipline_id FROM workforce.titles WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|row| title::Item::try_from(row).map(Item::Title))
        .transpose()
}

pub(super) async fn find_group(client: &Client, id: group::Id) -> Result<Option<Item>, Error> {
    client
        .query_opt(
            "SELECT id, operation_id, name, enabled, index FROM workforce.groups WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|row| group::Item::try_from(row).map(Item::Group))
        .transpose()
}

pub(super) async fn find_position(
    client: &Client,
    id: position::Id,
) -> Result<Option<Item>, Error> {
    client
        .query_opt(
            "SELECT id, operation_id, name, enabled, index FROM workforce.positions WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|row| position::Item::try_from(row).map(Item::Position))
        .transpose()
}

pub(super) async fn find_department(
    client: &Client,
    id: department::Id,
) -> Result<Option<Item>, Error> {
    client
        .query_opt(
            "SELECT id, operation_id, name, enabled, index, group_id, parent_id FROM workforce.departments WHERE id = $1",
            &[&id],
        )
        .await?
        .map(|row| department::Item::try_from(row).map(Item::Department))
        .transpose()
}

pub(super) async fn find_creation(
    client: &Client,
    collection: Collection,
    operation_id: operation::Id,
) -> Result<Option<Item>, Error> {
    match collection {
        Collection::Groups => {
            let row = client
                .query_opt(
                    "SELECT id, operation_id, name, enabled, index FROM workforce.groups WHERE operation_id = $1",
                    &[&operation_id],
                )
                .await?;
            row.map(|row| group::Item::try_from(row).map(Item::Group))
                .transpose()
        }
        Collection::Departments => {
            let row = client
                .query_opt(
                    "SELECT id, operation_id, name, enabled, index, group_id, parent_id FROM workforce.departments WHERE operation_id = $1",
                    &[&operation_id],
                )
                .await?;
            row.map(|row| department::Item::try_from(row).map(Item::Department))
                .transpose()
        }
        Collection::Positions => {
            let row = client
                .query_opt(
                    "SELECT id, operation_id, name, enabled, index FROM workforce.positions WHERE operation_id = $1",
                    &[&operation_id],
                )
                .await?;
            row.map(|row| position::Item::try_from(row).map(Item::Position))
                .transpose()
        }
        Collection::Categories => client
            .query_opt(
                "SELECT id, operation_id, name, enabled, index FROM workforce.categories WHERE operation_id = $1",
                &[&operation_id],
            )
            .await?
            .map(|row| category::Item::try_from(row).map(Item::Category))
            .transpose(),
        Collection::Disciplines => client
            .query_opt(
                "SELECT id, operation_id, name, enabled, index, category_id FROM workforce.disciplines WHERE operation_id = $1",
                &[&operation_id],
            )
            .await?
            .map(|row| discipline::Item::try_from(row).map(Item::Discipline))
            .transpose(),
        Collection::Titles => client
            .query_opt(
                "SELECT id, operation_id, name, enabled, index, discipline_id FROM workforce.titles WHERE operation_id = $1",
                &[&operation_id],
            )
            .await?
            .map(|row| title::Item::try_from(row).map(Item::Title))
            .transpose(),
    }
}

pub(super) async fn insert(
    client: &Client,
    operation_id: operation::Id,
    creation: &crate::workforce::directory::Creation,
) -> Result<Item, Error> {
    match creation {
        crate::workforce::directory::Creation::Group(input) => {
            let id = group::Id::allocate();
            let row = client
                .query_one(
                    "INSERT INTO workforce.groups (id, name, enabled, index, operation_id) VALUES ($1, $2, $3, $4, $5) RETURNING id, operation_id, name, enabled, index",
                    &[&id, &input.name, &input.enabled, &input.index, &operation_id],
                )
                .await?;
            Ok(Item::Group(group::Item::try_from(row)?))
        }
        crate::workforce::directory::Creation::Department(input) => {
            let id = department::Id::allocate();
            let row = client
                .query_one(
                    "INSERT INTO workforce.departments (id, name, enabled, index, operation_id, group_id, parent_id) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id, operation_id, name, enabled, index, group_id, parent_id",
                    &[&id, &input.name, &input.enabled, &input.index, &operation_id, &input.group_id, &input.parent_id],
                )
                .await?;
            Ok(Item::Department(department::Item::try_from(row)?))
        }
        crate::workforce::directory::Creation::Position(input) => {
            let id = position::Id::allocate();
            let row = client
                .query_one(
                    "INSERT INTO workforce.positions (id, name, enabled, index, operation_id) VALUES ($1, $2, $3, $4, $5) RETURNING id, operation_id, name, enabled, index",
                    &[&id, &input.name, &input.enabled, &input.index, &operation_id],
                )
                .await?;
            Ok(Item::Position(position::Item::try_from(row)?))
        }
        crate::workforce::directory::Creation::Category(input) => {
            let row = client
                .query_one(
                    "INSERT INTO workforce.categories (id, name, enabled, index, operation_id) VALUES ($1, $2, $3, $4, $5) RETURNING id, operation_id, name, enabled, index",
                    &[&category::Id::allocate(), &input.name, &input.enabled, &input.index, &operation_id],
                )
                .await?;
            Ok(Item::Category(category::Item::try_from(row)?))
        }
        crate::workforce::directory::Creation::Discipline(input) => {
            let row = client
                .query_one(
                    "INSERT INTO workforce.disciplines (id, name, enabled, index, operation_id, category_id) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, operation_id, name, enabled, index, category_id",
                    &[&discipline::Id::allocate(), &input.name, &input.enabled, &input.index, &operation_id, &input.category_id],
                )
                .await?;
            Ok(Item::Discipline(discipline::Item::try_from(row)?))
        }
        crate::workforce::directory::Creation::Title(input) => {
            let row = client
                .query_one(
                    "INSERT INTO workforce.titles (id, name, enabled, index, operation_id, discipline_id) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, operation_id, name, enabled, index, discipline_id",
                    &[&title::Id::allocate(), &input.name, &input.enabled, &input.index, &operation_id, &input.discipline_id],
                )
                .await?;
            Ok(Item::Title(title::Item::try_from(row)?))
        }
    }
}

pub(super) async fn update(
    client: &Client,
    expected: &Item,
    replacement: &Item,
) -> Result<Item, Error> {
    match (expected, replacement) {
        (Item::Group(_), Item::Group(replacement)) => {
            let row = client
                .query_one(
                    "UPDATE workforce.groups SET name = $2, enabled = $3, index = $4 WHERE id = $1 RETURNING id, operation_id, name, enabled, index",
                    &[&replacement.id, &replacement.name, &replacement.enabled, &replacement.index],
                )
                .await?;
            Ok(Item::Group(group::Item::try_from(row)?))
        }
        (Item::Position(_), Item::Position(replacement)) => {
            let row = client
                .query_one(
                    "UPDATE workforce.positions SET name = $2, enabled = $3, index = $4 WHERE id = $1 RETURNING id, operation_id, name, enabled, index",
                    &[&replacement.id, &replacement.name, &replacement.enabled, &replacement.index],
                )
                .await?;
            Ok(Item::Position(position::Item::try_from(row)?))
        }
        (Item::Department(expected), Item::Department(replacement)) => {
            if expected.group_id != replacement.group_id {
                client
                    .execute(
                        "WITH RECURSIVE subtree(id) AS (SELECT $1::uuid UNION ALL SELECT department.id FROM workforce.departments department JOIN subtree parent ON department.parent_id = parent.id) UPDATE workforce.departments SET group_id = $2 WHERE id IN (SELECT id FROM subtree)",
                        &[&replacement.id, &replacement.group_id],
                    )
                    .await?;
            }
            let row = client
                .query_one(
                    "UPDATE workforce.departments SET name = $2, enabled = $3, index = $4, group_id = $5, parent_id = $6 WHERE id = $1 RETURNING id, operation_id, name, enabled, index, group_id, parent_id",
                    &[&replacement.id, &replacement.name, &replacement.enabled, &replacement.index, &replacement.group_id, &replacement.parent_id],
                )
                .await?;
            Ok(Item::Department(department::Item::try_from(row)?))
        }
        (Item::Category(_), Item::Category(replacement)) => {
            let row = client
                .query_one(
                    "UPDATE workforce.categories SET name = $2, enabled = $3, index = $4 WHERE id = $1 RETURNING id, operation_id, name, enabled, index",
                    &[&replacement.id, &replacement.name, &replacement.enabled, &replacement.index],
                )
                .await?;
            Ok(Item::Category(category::Item::try_from(row)?))
        }
        (Item::Discipline(_), Item::Discipline(replacement)) => {
            let row = client
                .query_one(
                    "UPDATE workforce.disciplines SET name = $2, enabled = $3, index = $4, category_id = $5 WHERE id = $1 RETURNING id, operation_id, name, enabled, index, category_id",
                    &[&replacement.id, &replacement.name, &replacement.enabled, &replacement.index, &replacement.category_id],
                )
                .await?;
            Ok(Item::Discipline(discipline::Item::try_from(row)?))
        }
        (Item::Title(_), Item::Title(replacement)) => {
            let row = client
                .query_one(
                    "UPDATE workforce.titles SET name = $2, enabled = $3, index = $4, discipline_id = $5 WHERE id = $1 RETURNING id, operation_id, name, enabled, index, discipline_id",
                    &[&replacement.id, &replacement.name, &replacement.enabled, &replacement.index, &replacement.discipline_id],
                )
                .await?;
            Ok(Item::Title(title::Item::try_from(row)?))
        }
        _ => Err(Error::corrupt(
            "directory replacement retains its collection",
        )),
    }
}

pub(super) async fn find_rule(
    client: &Client,
    subtype: Subtype,
) -> Result<Option<rule::Rule>, Error> {
    let code = i16::from(subtype);
    client
        .query_opt(
            "SELECT id, department_id, position FROM workforce.rules WHERE id = $1",
            &[&code],
        )
        .await?
        .map(rule::Rule::try_from)
        .transpose()
}

pub(super) async fn insert_rule(
    client: &Client,
    subtype: Subtype,
    input: &rule::Creation,
) -> Result<rule::Rule, Error> {
    let code = i16::from(subtype);
    let row = client
        .query_one(
            "INSERT INTO workforce.rules (id, department_id, position) VALUES ($1, $2, $3) RETURNING id, department_id, position",
            &[&code, &input.department_id, &input.position],
        )
        .await?;
    rule::Rule::try_from(row)
}

pub(super) async fn update_rule(
    client: &Client,
    replacement: &rule::Rule,
) -> Result<rule::Rule, Error> {
    let code = i16::from(replacement.id);
    let row = client
        .query_one(
            "UPDATE workforce.rules SET department_id = $2, position = $3 WHERE id = $1 RETURNING id, department_id, position",
            &[&code, &replacement.department_id, &replacement.position],
        )
        .await?;
    rule::Rule::try_from(row)
}

pub(super) async fn delete(client: &Client, target: Target) -> Result<(), Error> {
    let deleted = match target {
        Target::Group(id) => {
            client
                .execute("DELETE FROM workforce.groups WHERE id = $1", &[&id])
                .await?
        }
        Target::Department(id) => {
            client
                .execute("DELETE FROM workforce.departments WHERE id = $1", &[&id])
                .await?
        }
        Target::Position(id) => {
            client
                .execute("DELETE FROM workforce.positions WHERE id = $1", &[&id])
                .await?
        }
        Target::Category(id) => {
            client
                .execute("DELETE FROM workforce.categories WHERE id = $1", &[&id])
                .await?
        }
        Target::Discipline(id) => {
            client
                .execute("DELETE FROM workforce.disciplines WHERE id = $1", &[&id])
                .await?
        }
        Target::Title(id) => {
            client
                .execute("DELETE FROM workforce.titles WHERE id = $1", &[&id])
                .await?
        }
    };
    if deleted > 1 {
        return Err(Error::corrupt(
            "directory deletion affects at most one item",
        ));
    }
    Ok(())
}

/// Whether children, rules or department positions still reference the item.
pub(super) async fn referenced(client: &Client, expected: &Item) -> Result<bool, Error> {
    let referenced = match expected {
        Item::Group(item) => client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM workforce.departments WHERE group_id = $1)",
                &[&item.id],
            )
            .await?
            .try_get::<_, bool>(0)
            .map_err(Error::decode)?,
        Item::Department(item) => client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM workforce.departments WHERE parent_id = $1) OR EXISTS (SELECT 1 FROM workforce.rules WHERE department_id = $1) OR EXISTS (SELECT 1 FROM workforce.rosters WHERE department_id = $1) OR EXISTS (SELECT 1 FROM workforce.applications WHERE department_id = $1)",
                &[&item.id],
            )
            .await?
            .try_get::<_, bool>(0)
            .map_err(Error::decode)?,
        Item::Position(item) => client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM workforce.rosters WHERE position_id = $1)",
                &[&item.id],
            )
            .await?
            .try_get::<_, bool>(0)
            .map_err(Error::decode)?,
        Item::Category(item) => client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM workforce.disciplines WHERE category_id = $1)",
                &[&item.id],
            )
            .await?
            .try_get::<_, bool>(0)
            .map_err(Error::decode)?,
        Item::Discipline(item) => client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM workforce.titles WHERE discipline_id = $1)",
                &[&item.id],
            )
            .await?
            .try_get::<_, bool>(0)
            .map_err(Error::decode)?,
        Item::Title(item) => client.query_one(
            "SELECT EXISTS (SELECT 1 FROM workforce.applications WHERE title_id = $1)", &[&item.id]
        ).await?.try_get(0).map_err(Error::decode)?,
    };
    Ok(referenced)
}

pub(super) async fn fetch_list(
    client: &Client,
    collection: Collection,
) -> Result<Vec<Item>, Error> {
    match collection {
        Collection::Groups => fetch_groups(client).await,
        Collection::Departments => fetch_departments(client).await,
        Collection::Positions => fetch_positions(client).await,
        Collection::Categories => fetch_categories(client).await,
        Collection::Disciplines => fetch_disciplines(client).await,
        Collection::Titles => fetch_titles(client).await,
    }
}

async fn fetch_categories(client: &Client) -> Result<Vec<Item>, Error> {
    client
        .query(
            "SELECT id, operation_id, name, enabled, index FROM workforce.categories ORDER BY index, id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| category::Item::try_from(row).map(Item::Category))
        .collect()
}

async fn fetch_disciplines(client: &Client) -> Result<Vec<Item>, Error> {
    client
        .query(
            "SELECT id, operation_id, name, enabled, index, category_id FROM workforce.disciplines ORDER BY index, id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| discipline::Item::try_from(row).map(Item::Discipline))
        .collect()
}

async fn fetch_titles(client: &Client) -> Result<Vec<Item>, Error> {
    client
        .query(
            "SELECT id, operation_id, name, enabled, index, discipline_id FROM workforce.titles ORDER BY index, id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| title::Item::try_from(row).map(Item::Title))
        .collect()
}

async fn fetch_positions(client: &Client) -> Result<Vec<Item>, Error> {
    let rows = client
        .query(
            "SELECT id, operation_id, name, enabled, index FROM workforce.positions ORDER BY index, id",
            &[],
        )
        .await?;
    rows.into_iter()
        .map(|row| position::Item::try_from(row).map(Item::Position))
        .collect()
}

async fn fetch_groups(client: &Client) -> Result<Vec<Item>, Error> {
    let rows = client
        .query(
            "SELECT id, operation_id, name, enabled, index FROM workforce.groups ORDER BY index, id",
            &[],
        )
        .await?;
    rows.into_iter()
        .map(|row| group::Item::try_from(row).map(Item::Group))
        .collect()
}

async fn fetch_departments(client: &Client) -> Result<Vec<Item>, Error> {
    let rows = client
        .query(
            "SELECT id, operation_id, name, enabled, index, group_id, parent_id FROM workforce.departments ORDER BY index, id",
            &[],
        )
        .await?;
    rows.into_iter()
        .map(|row| department::Item::try_from(row).map(Item::Department))
        .collect()
}

async fn fetch_rules(client: &Client) -> Result<Vec<rule::Rule>, Error> {
    let rows = client
        .query(
            "SELECT id, department_id, position FROM workforce.rules ORDER BY id",
            &[],
        )
        .await?;
    rows.into_iter().map(rule::Rule::try_from).collect()
}

pub(super) async fn fetch_roster(
    client: &Client,
    department_id: department::Id,
) -> Result<Vec<roster::Entry>, Error> {
    client
        .query(
            "SELECT id, position_id, enabled, index FROM workforce.rosters WHERE department_id = $1 ORDER BY index, id",
            &[&department_id],
        )
        .await?
        .into_iter()
        .map(roster::Entry::try_from)
        .collect()
}

/// Whether every replacement position is a real position; disabled positions may be configured.
pub(super) async fn positions_present(
    client: &Client,
    replacement: &[roster::Choice],
) -> Result<bool, Error> {
    let positions: Vec<position::Id> = replacement
        .iter()
        .map(|choice| choice.position_id)
        .collect();
    let present: i64 = client
        .query_one(
            "SELECT count(*) FROM workforce.positions WHERE id = ANY($1)",
            &[&positions],
        )
        .await?
        .try_get(0)
        .map_err(Error::decode)?;
    Ok(usize::try_from(present).ok() == Some(positions.len()))
}

/// Applies the difference between the compared current list and the complete replacement.
/// Retained positions keep their entry ID; their department and position never change.
pub(super) async fn replace_roster(
    client: &Client,
    department_id: department::Id,
    current: &[roster::Entry],
    replacement: &[roster::Choice],
) -> Result<(), Error> {
    for entry in current {
        if replacement
            .iter()
            .all(|choice| choice.position_id != entry.position_id)
        {
            if client
                .query_one(
                    "SELECT EXISTS (SELECT 1 FROM workforce.applications WHERE roster_id=$1)",
                    &[&entry.id],
                )
                .await?
                .try_get::<_, bool>(0)
                .map_err(Error::decode)?
            {
                return Err(super::Rejection::InUse.into());
            }
            let deleted = client
                .execute("DELETE FROM workforce.rosters WHERE id = $1", &[&entry.id])
                .await?;
            if deleted != 1 {
                return Err(Error::corrupt(
                    "a compared department position remains until its replacement",
                ));
            }
        }
    }
    for choice in replacement {
        let changed = match current
            .iter()
            .find(|entry| entry.position_id == choice.position_id)
        {
            Some(entry) if entry.configures(choice) => 1,
            Some(entry) => {
                client
                    .execute(
                        "UPDATE workforce.rosters SET enabled = $2, index = $3 WHERE id = $1",
                        &[&entry.id, &choice.enabled, &choice.index],
                    )
                    .await?
            }
            None => {
                client
                    .execute(
                        "INSERT INTO workforce.rosters (id, department_id, position_id, enabled, index) VALUES ($1, $2, $3, $4, $5)",
                        &[&roster::Id::allocate(), &department_id, &choice.position_id, &choice.enabled, &choice.index],
                    )
                    .await?
            }
        };
        if changed != 1 {
            return Err(Error::corrupt(
                "a compared department position remains until its replacement",
            ));
        }
    }
    Ok(())
}

async fn fetch_rosters(client: &Client) -> Result<Vec<(department::Id, roster::Entry)>, Error> {
    client
        .query(
            "SELECT id, department_id, position_id, enabled, index FROM workforce.rosters ORDER BY department_id, index, id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| {
            let department_id = row.try_get("department_id").map_err(Error::decode)?;
            Ok((department_id, roster::Entry::try_from(row)?))
        })
        .collect()
}

pub(super) async fn fetch_rule_configuration(
    client: &Client,
) -> Result<rule::Configuration, Error> {
    Ok(rule::Configuration::new(fetch_rules(client).await?))
}

pub(super) async fn fetch_selection(client: &Client) -> Result<selection::Selection, Error> {
    let groups: Vec<group::Item> = fetch_groups(client)
        .await?
        .into_iter()
        .map(|item| {
            let Item::Group(item) = item else {
                unreachable!()
            };
            item
        })
        .collect();
    let departments: Vec<department::Item> = fetch_departments(client)
        .await?
        .into_iter()
        .map(|item| {
            let Item::Department(item) = item else {
                unreachable!()
            };
            item
        })
        .collect();
    let positions: Vec<position::Item> = fetch_positions(client)
        .await?
        .into_iter()
        .map(|item| {
            let Item::Position(item) = item else {
                unreachable!()
            };
            item
        })
        .collect();
    let categories: Vec<category::Item> = fetch_categories(client)
        .await?
        .into_iter()
        .map(|item| {
            let Item::Category(item) = item else {
                unreachable!()
            };
            item
        })
        .collect();
    let disciplines: Vec<discipline::Item> = fetch_disciplines(client)
        .await?
        .into_iter()
        .map(|item| {
            let Item::Discipline(item) = item else {
                unreachable!()
            };
            item
        })
        .collect();
    let titles: Vec<title::Item> = fetch_titles(client)
        .await?
        .into_iter()
        .map(|item| {
            let Item::Title(item) = item else {
                unreachable!()
            };
            item
        })
        .collect();
    let rosters = fetch_rosters(client).await?;
    let configuration = rule::Configuration::new(fetch_rules(client).await?);

    let groups_by_id: HashMap<group::Id, &group::Item> =
        groups.iter().map(|item| (item.id, item)).collect();
    let departments_by_id: HashMap<department::Id, &department::Item> =
        departments.iter().map(|item| (item.id, item)).collect();
    let positions_by_id: HashMap<position::Id, &position::Item> =
        positions.iter().map(|item| (item.id, item)).collect();
    let categories_by_id: HashMap<category::Id, &category::Item> =
        categories.iter().map(|item| (item.id, item)).collect();
    let disciplines_by_id: HashMap<discipline::Id, &discipline::Item> =
        disciplines.iter().map(|item| (item.id, item)).collect();

    let mut selectable_departments = HashSet::new();
    let mut selection_departments = Vec::new();
    for department in &departments {
        if department_selectable(department, &departments_by_id, &groups_by_id)? {
            selectable_departments.insert(department.id);
            selection_departments.push(selection::Department {
                id: department.id,
                name: department.name.clone(),
                index: department.index,
                group_id: department.group_id,
                parent_id: department.parent_id,
            });
        }
    }

    let mut selection_categories = Vec::new();
    for category in &categories {
        if category.enabled {
            selection_categories.push(selection::Category {
                id: category.id,
                name: category.name.clone(),
                index: category.index,
            });
        }
    }
    let mut selection_disciplines = Vec::new();
    for discipline in &disciplines {
        let category = categories_by_id
            .get(&discipline.category_id)
            .ok_or(Error::corrupt("every discipline retains its category"))?;
        if discipline.enabled && category.enabled {
            selection_disciplines.push(selection::Discipline {
                id: discipline.id,
                name: discipline.name.clone(),
                index: discipline.index,
                category_id: discipline.category_id,
            });
        }
    }
    let mut selection_titles = Vec::new();
    for title in &titles {
        let discipline = disciplines_by_id
            .get(&title.discipline_id)
            .ok_or(Error::corrupt("every title retains its discipline"))?;
        let category = categories_by_id
            .get(&discipline.category_id)
            .ok_or(Error::corrupt(
                "every title discipline retains its category",
            ))?;
        if title.enabled && discipline.enabled && category.enabled {
            selection_titles.push(selection::Title {
                id: title.id,
                name: title.name.clone(),
                index: title.index,
                discipline_id: title.discipline_id,
            });
        }
    }

    let mut selection_positions = Vec::new();
    for (department_id, entry) in rosters {
        if !departments_by_id.contains_key(&department_id) {
            return Err(Error::corrupt(
                "every department position retains its department",
            ));
        }
        let position = positions_by_id
            .get(&entry.position_id)
            .ok_or(Error::corrupt(
                "every department position retains its position",
            ))?;
        if selectable_departments.contains(&department_id) && entry.enabled && position.enabled {
            selection_positions.push(selection::Position {
                department_id,
                entry_id: entry.id,
                position_id: entry.position_id,
                name: position.name.clone(),
                index: entry.index,
            });
        }
    }

    let mut selection_rules = Vec::new();
    for rule in &configuration.rules {
        if !departments_by_id.contains_key(&rule.department_id) {
            return Err(Error::corrupt("every rule retains its real department"));
        }
        selection_rules.push(selection::Rule {
            id: rule.id,
            department_id: rule.department_id,
            position: rule.position,
            selectable: selectable_departments.contains(&rule.department_id),
        });
    }
    let selection_groups = groups
        .into_iter()
        .filter(|group| group.enabled)
        .map(|group| selection::Group {
            id: group.id,
            name: group.name,
            index: group.index,
        })
        .collect();
    Ok(selection::Selection {
        groups: selection_groups,
        departments: selection_departments,
        positions: selection_positions,
        categories: selection_categories,
        disciplines: selection_disciplines,
        titles: selection_titles,
        rules: selection_rules,
        missing: configuration.missing,
    })
}

fn department_selectable(
    department: &department::Item,
    departments: &HashMap<department::Id, &department::Item>,
    groups: &HashMap<group::Id, &group::Item>,
) -> Result<bool, Error> {
    let mut selectable = true;
    let mut seen = HashSet::new();
    let mut current = department;
    loop {
        if !seen.insert(current.id) {
            return Err(Error::corrupt("department ancestors do not form a cycle"));
        }
        let group = groups
            .get(&current.group_id)
            .ok_or(Error::corrupt("every department retains its direct group"))?;
        selectable &= current.enabled && group.enabled;
        let Some(parent_id) = current.parent_id else {
            return Ok(selectable);
        };
        let parent = departments
            .get(&parent_id)
            .ok_or(Error::corrupt("department parent exists in the same group"))?;
        if parent.group_id != current.group_id {
            return Err(Error::corrupt("department parent exists in the same group"));
        }
        current = parent;
    }
}

impl TryFrom<Row> for rule::Rule {
    type Error = Error;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        let code = row.try_get::<_, i16>("id").map_err(Error::decode)?;
        let id = match Subtype::try_from(code) {
            Ok(id) => id,
            Err(_) => {
                return Err(Error::corrupt("rule id is a fixed person subtype"));
            }
        };
        Ok(rule::Rule {
            id,
            department_id: row.try_get("department_id").map_err(Error::decode)?,
            position: row.try_get("position").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for roster::Entry {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(roster::Entry {
            id: row.try_get("id").map_err(Error::decode)?,
            position_id: row.try_get("position_id").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for category::Item {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(category::Item {
            id: row.try_get("id").map_err(Error::decode)?,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            name: row.try_get("name").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for discipline::Item {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(discipline::Item {
            id: row.try_get("id").map_err(Error::decode)?,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            name: row.try_get("name").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
            category_id: row.try_get("category_id").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for title::Item {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(title::Item {
            id: row.try_get("id").map_err(Error::decode)?,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            name: row.try_get("name").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
            discipline_id: row.try_get("discipline_id").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for group::Item {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(group::Item {
            id: row.try_get("id").map_err(Error::decode)?,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            name: row.try_get("name").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for position::Item {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(position::Item {
            id: row.try_get("id").map_err(Error::decode)?,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            name: row.try_get("name").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
        })
    }
}

impl TryFrom<Row> for department::Item {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        Ok(department::Item {
            id: row.try_get("id").map_err(Error::decode)?,
            operation_id: row.try_get("operation_id").map_err(Error::decode)?,
            name: row.try_get("name").map_err(Error::decode)?,
            enabled: row.try_get("enabled").map_err(Error::decode)?,
            index: row.try_get("index").map_err(Error::decode)?,
            group_id: row.try_get("group_id").map_err(Error::decode)?,
            parent_id: row.try_get("parent_id").map_err(Error::decode)?,
        })
    }
}

pub(super) fn rule_target(value: &str) -> Option<Subtype> {
    let subtype = Subtype::try_from(value.parse::<i16>().ok()?).ok()?;
    (i16::from(subtype).to_string() == value).then_some(subtype)
}

pub(super) async fn verify(
    client: &Client,
    keys: Keys<'_>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let groups = fetch_groups(client).await?;
    let departments = fetch_departments(client).await?;
    let positions = fetch_positions(client).await?;
    let rules = fetch_rules(client).await?;
    let rosters = fetch_rosters(client).await?;
    let categories = fetch_categories(client).await?;
    let disciplines = fetch_disciplines(client).await?;
    let titles = fetch_titles(client).await?;
    let mut names = HashSet::new();
    let mut operations = HashSet::new();
    for item in groups.iter().chain(departments.iter()) {
        if !names.insert(match item {
            Item::Group(item) => item.name.as_str(),
            Item::Department(item) => item.name.as_str(),
            _ => unreachable!(),
        }) || !operations.insert(item.operation_id())
        {
            return Err(Error::corrupt(
                "directory names and original creation operations are unique across both collections",
            ));
        }
    }
    let mut position_names = HashSet::new();
    for item in &positions {
        let Item::Position(item) = item else {
            unreachable!()
        };
        if !position_names.insert(item.name.as_str()) {
            return Err(Error::corrupt(
                "position names are unique across all positions",
            ));
        }
        if !operations.insert(item.operation_id()) {
            return Err(Error::corrupt(
                "directory original creation operations are unique",
            ));
        }
    }
    let mut title_names = HashSet::new();
    let mut category_ids = HashSet::new();
    let mut discipline_ids = HashSet::new();
    for item in categories.iter().chain(&disciplines).chain(&titles) {
        let name = match item {
            Item::Category(item) => {
                category_ids.insert(item.id);
                item.name.as_str()
            }
            Item::Discipline(item) => {
                discipline_ids.insert(item.id);
                item.name.as_str()
            }
            Item::Title(item) => item.name.as_str(),
            _ => unreachable!(),
        };
        if !title_names.insert(name) || !operations.insert(item.operation_id()) {
            return Err(Error::corrupt(
                "title tree names share one scope and original creations are unique",
            ));
        }
    }
    for item in disciplines.iter().chain(&titles) {
        let parented = match item {
            Item::Discipline(item) => category_ids.contains(&item.category_id),
            Item::Title(item) => discipline_ids.contains(&item.discipline_id),
            _ => unreachable!(),
        };
        if !parented {
            return Err(Error::corrupt(
                "every discipline retains its category and every title its discipline",
            ));
        }
    }
    let group_ids: HashSet<group::Id> = groups
        .iter()
        .map(|item| {
            let Item::Group(item) = item else {
                unreachable!()
            };
            item.id
        })
        .collect();
    let departments_by_id: HashMap<department::Id, &department::Item> = departments
        .iter()
        .map(|item| {
            let Item::Department(item) = item else {
                unreachable!()
            };
            (item.id, item)
        })
        .collect();
    for rule in &rules {
        if !departments_by_id.contains_key(&rule.department_id) {
            return Err(Error::corrupt("every rule retains its real department"));
        }
    }
    let position_ids: HashSet<position::Id> = positions
        .iter()
        .map(|item| {
            let Item::Position(item) = item else {
                unreachable!()
            };
            item.id
        })
        .collect();
    let mut entries = HashSet::new();
    let mut configured = HashSet::new();
    for (department_id, entry) in &rosters {
        if !departments_by_id.contains_key(department_id)
            || !position_ids.contains(&entry.position_id)
        {
            return Err(Error::corrupt(
                "every department position retains its real department and position",
            ));
        }
        if !entries.insert(entry.id) || !configured.insert((*department_id, entry.position_id)) {
            return Err(Error::corrupt(
                "department positions are unique by entry and by department and position",
            ));
        }
    }
    for department in departments_by_id.values() {
        if !group_ids.contains(&department.group_id) {
            return Err(Error::corrupt("every department retains its direct group"));
        }
        let mut seen = HashSet::new();
        let mut current = Some(*department);
        while let Some(item) = current {
            if !seen.insert(item.id) {
                return Err(Error::corrupt("department ancestors do not form a cycle"));
            }
            if let Some(parent_id) = item.parent_id {
                let parent = departments_by_id
                    .get(&parent_id)
                    .ok_or(Error::corrupt("department parent exists in the same group"))?;
                if parent.group_id != item.group_id {
                    return Err(Error::corrupt("department parent exists in the same group"));
                }
                current = Some(*parent);
            } else {
                current = None;
            }
        }
    }
    for completion in completion::fetch_all(client, keys, super::COMPLETIONS).await? {
        let (action, target) = (completion.action(), completion.target());
        if completion.created_at() > now {
            return Err(Error::corrupt(
                "completion time does not exceed the verified clock",
            ));
        }
        if let Some(subtype) = rule_target(target) {
            if !rules.iter().any(|rule| rule.id == subtype) {
                return Err(Error::corrupt(
                    "a rule completion retains its configured rule",
                ));
            }
            continue;
        }
        if action == "create" {
            let Ok(collection) = Collection::from_str(target) else {
                return Err(Error::corrupt(
                    "directory creation completion targets a delivered collection",
                ));
            };
            if groups
                .iter()
                .chain(departments.iter())
                .chain(positions.iter())
                .chain(categories.iter())
                .chain(disciplines.iter())
                .chain(titles.iter())
                .any(|item| {
                    item.operation_id() == completion.operation_id()
                        && item.collection() != collection
                })
            {
                return Err(Error::corrupt(
                    "an extant original creation belongs to its completed collection",
                ));
            }
        }
    }
    Ok(())
}
