//! Authorized column creation and permanent original results.

pub mod change;
mod command;
mod navigation;
mod state;
#[cfg(test)]
mod tests;
mod value;

pub(crate) use change::TABLE as CHANGES;
use chrono::{DateTime, Utc};
pub(crate) use navigation::{RECORDS as APPENDS, TABLE as NAVIGATIONS};
pub(crate) use state::TABLE as COLUMNS;
use tokio_postgres::Client;
pub use value::{Column, Creation, Description, Id, InputError, Key, Name};

use crate::{
    access::role,
    history::{self, Read, Record as _, State as _, Verified},
    identity::{
        current::{self, Actor},
        operation,
        person::revision,
    },
    kernel::{
        self, App,
        completion::{self, Template},
        operation::Family,
    },
};

const COMPLETIONS: Template = Template::new(Family::ContentColumn, |action, target| {
    action == "create" && target.parse::<Key>().is_ok()
});

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    roles: role::Reader,
    revisions: revision::Reader,
}

impl Service {
    pub(crate) fn prepare(app: App, current: current::Service) -> Self {
        Self {
            roles: role::Reader::new(app.clone()),
            revisions: revision::Reader::new(app.clone()),
            app,
            current,
        }
    }

    pub async fn create(
        &self,
        actor: Actor,
        operation: operation::Id,
        input: Creation,
    ) -> Result<change::Change, Error> {
        self.app
            .command(
                operation,
                command::Create {
                    service: self.clone(),
                    actor,
                    input,
                    id: Id::allocate(),
                    change_id: change::Id::allocate(),
                    navigation_id: uuid::Uuid::new_v4(),
                    append_id: uuid::Uuid::new_v4(),
                },
            )
            .await
    }
}

pub type Error = kernel::Error<Rejection>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    KeyInUse,
    NameInUse,
    Limit,
}

impl kernel::Rejection for Rejection {
    fn status(&self) -> kernel::Status {
        kernel::Status::Conflict
    }
    fn code(&self) -> &'static str {
        match self {
            Self::KeyInUse => "column_key_in_use",
            Self::NameInUse => "name_in_use",
            Self::Limit => "column_limit_exceeded",
        }
    }
}

impl From<Rejection> for Error {
    fn from(value: Rejection) -> Self {
        Self::Rejected(value)
    }
}

struct Collection {
    navigation: Option<Verified<navigation::Navigation>>,
    columns: Vec<Verified<state::ColumnState>>,
}

impl Collection {
    async fn load(
        client: &Client,
        service: &Service,
        read: Read,
        now: DateTime<Utc>,
    ) -> Result<Self, Error> {
        let mut navigation = history::all::<navigation::Navigation>(
            client,
            service.app.keys(),
            service.app.anchors(),
            read,
            now,
        )
        .await?;
        if navigation.len() > 1 {
            return Err(Error::corrupt("column navigation is a singleton"));
        }
        let columns = history::all::<state::ColumnState>(
            client,
            service.app.keys(),
            service.app.anchors(),
            read,
            now,
        )
        .await?;
        let value = Self {
            navigation: navigation.pop(),
            columns,
        };
        value.check()?;
        Ok(value)
    }

    fn check(&self) -> Result<(), Error> {
        let order = self
            .navigation
            .as_ref()
            .map_or(&[][..], |value| value.order());
        if self.columns.len() != order.len()
            || order
                .iter()
                .any(|id| !self.columns.iter().any(|column| column.id() == *id))
        {
            return Err(Error::corrupt(
                "column navigation retains every current column exactly once",
            ));
        }
        Ok(())
    }
}

pub(crate) async fn verify(client: &Client, app: &App, now: DateTime<Utc>) -> Result<(), Error> {
    let columns = history::inspect::<state::ColumnState>(client, app.keys(), now).await?;
    let mut navigations =
        history::inspect::<navigation::Navigation>(client, app.keys(), now).await?;
    if navigations.len() > 1 {
        return Err(Error::corrupt("column navigation is a singleton"));
    }
    let collection = Collection {
        navigation: navigations.pop(),
        columns,
    };
    collection.check()?;
    let completions = completion::fetch_all(client, app.keys(), COMPLETIONS).await?;
    let mut operations = std::collections::HashSet::new();
    for completion in &completions {
        completion.check_time(now)?;
        let input: Creation = completion.input(app.keys())?;
        let record =
            history::original::<change::Record>(client, app.keys(), completion.operation_id(), now)
                .await?
                .ok_or(Error::corrupt("a column completion retains its creation"))?;
        if record.input() != &input
            || record.digest() != input.digest()?
            || completion.target() != input.key.as_str()
            || record.operator_id() != completion.revision_id()
            || record.created_at() != completion.created_at()
        {
            return Err(Error::corrupt(
                "column creation agrees with its original completion",
            ));
        }
        operations.insert(completion.operation_id());
    }
    for column in &collection.columns {
        let records =
            history::page::<state::ColumnState>(client, app.keys(), column, 1, 0, now).await?;
        for record in records {
            if !operations.remove(&record.operation_id()) {
                return Err(Error::corrupt("a column creation retains its completion"));
            }
        }
    }
    if !operations.is_empty() {
        return Err(Error::corrupt("a column completion has a retained state"));
    }
    navigation::verify_sources(client).await?;
    history::initialize(app.anchors(), collection.columns)?;
    history::initialize(
        app.anchors(),
        collection.navigation.into_iter().collect::<Vec<_>>(),
    )?;
    Ok(())
}
