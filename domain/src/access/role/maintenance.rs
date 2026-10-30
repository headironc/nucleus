//! System role facts created by initialization.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::role::{Builtin, Code, Error, Id, Kind, Level, Name, Role, change, state},
    history::{self, Change, Draft, Kind as _},
    identity::{operation, person::revision},
};

pub(in crate::access) struct Creation {
    pub(in crate::access) id: Id,
    pub(in crate::access) change_id: change::Id,
    pub(in crate::access) system: Builtin,
    pub(in crate::access) operator_id: revision::Id,
    pub(in crate::access) changed_at: DateTime<Utc>,
    pub(in crate::access) operation_id: operation::Id,
}

pub(in crate::access) async fn confirm_initialization(
    client: &Client,
    roles: &super::Reader,
    operation_id: operation::Id,
    revision_id: revision::Id,
    at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let rows = client
        .query(
            &format!(
                "{} WHERE record.operation_id=$1 AND record.level IN (1,1000)",
                change::Record::SELECT
            ),
            &[&operation_id],
        )
        .await?;
    let mut seen = Vec::with_capacity(4);
    for row in rows {
        let record = history::record::<change::Record>(&row, roles.keys(), now)?;
        let builtin = record
            .initialization(operation_id, revision_id, at)
            .ok_or(Error::Facts {
                rule: "initialization retains its four original system role creations",
            })?;
        if seen.contains(&builtin) {
            return Err(Error::Facts {
                rule: "each initialized system role is unique",
            });
        }
        seen.push(builtin);
        roles.anchors().confirm::<state::State>(&record)?;
    }
    if seen.len() != Builtin::ALL.len() {
        return Err(Error::Facts {
            rule: "initialization retains all four system role creations",
        });
    }
    Ok(())
}

pub(in crate::access) fn create(input: Creation) -> Result<(Role, Change<state::State>), Error> {
    let scope = input.system.scope();
    let name = Name(input.system.name().to_owned());
    let level = Level(input.system.level());
    let mut permissions = if level.0 == 1000 {
        Code::all()
            .iter()
            .copied()
            .filter(|code| code.scope() == scope)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    permissions.sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
    let kind = Kind::new(scope, &name, level, &permissions)?;
    let record = change::Data {
        id: input.change_id,
        previous_id: None,
        scope,
        entry: None,
        action: change::Action::Create {
            permissions: permissions.clone(),
        },
        name: name.clone(),
        level,
        operator_id: input.operator_id,
        changed_at: input.changed_at,
        operation_id: input.operation_id,
    };
    let role = Role {
        id: input.id,
        name,
        level,
        kind,
        permissions,
        change_id: input.change_id,
    };
    let draft = Draft::<state::State>::create(state::Data {
        role: role.clone(),
        scope,
        active: true,
        changed_at: input.changed_at,
    });
    Ok((role, Change::new(draft, record)))
}
