use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    access::{self, role},
    content::column::{
        COMPLETIONS, Collection, Creation, Error, Id, Rejection, Service, change, navigation, state,
    },
    history::{self, Change, Draft, Read, State as _},
    identity::{
        current::{self, Actor},
        operation,
    },
    kernel::{
        Authorization, Command, completion,
        tx::{self, Tx},
    },
};

pub(super) struct Create {
    pub(super) service: Service,
    pub(super) actor: Actor,
    pub(super) input: Creation,
    pub(super) id: Id,
    pub(super) change_id: change::Id,
    pub(super) navigation_id: Uuid,
    pub(super) append_id: Uuid,
}

pub(super) struct Facts {
    collection: Collection,
    operator: crate::identity::person::revision::Identity,
    authority: Authorization,
    operation: operation::Id,
}

pub(super) struct CreationChange {
    column: Change<state::ColumnState>,
    navigation: Change<navigation::Navigation>,
    result: change::Change,
}

impl Command for Create {
    const COMPLETIONS: completion::Template = COMPLETIONS;
    type Facts = Facts;
    type Change = CreationChange;
    type Output = change::Change;
    type Rejection = Rejection;

    fn session(&self) -> Option<(&current::Service, &Actor)> {
        Some((&self.service.current, &self.actor))
    }
    fn request(&self, authority: &Authorization) -> Result<completion::Request, Error> {
        completion::Request::new(
            "create",
            self.input.key.as_str().to_owned(),
            &self.input,
            authority,
        )
    }
    async fn locks(&self, tx: &Tx) -> Result<(), Error> {
        tx.shared(b"access/scope", b"institution").await?;
        self.actor.lock_account(tx.client()).await?;
        self.actor.lock_session(tx.client()).await?;
        tx::object(tx.client(), b"content/columns", b"navigation").await?;
        Ok(())
    }
    async fn authorize(
        &self,
        tx: &Tx,
        subject: Option<current::database::State>,
    ) -> Result<Authorization, Error> {
        let subject = subject.ok_or(Error::corrupt(
            "a column command has a qualified website session",
        ))?;
        let (_, person_id, revision_id, now) = access::authorize_subject(
            tx.client(),
            &self.service.roles,
            subject,
            role::Code::InstitutionArticles,
        )
        .await?;
        Ok(Authorization {
            person_id,
            revision_id,
            now,
        })
    }
    async fn load(&self, tx: &Tx, authority: &Authorization) -> Result<Facts, Error> {
        Ok(Facts {
            collection: Collection::load(tx.client(), &self.service, Read::Locked, authority.now)
                .await?,
            operator: self
                .service
                .revisions
                .fetch_identity(tx.client(), authority.revision_id)
                .await?,
            authority: *authority,
            operation: tx.operation_id(),
        })
    }
    fn decide(&self, facts: Facts, now: DateTime<Utc>) -> Result<CreationChange, Error> {
        if facts
            .collection
            .columns
            .iter()
            .any(|column| column.input().key == self.input.key)
        {
            return Err(Rejection::KeyInUse.into());
        }
        if facts
            .collection
            .columns
            .iter()
            .any(|column| column.input().name == self.input.name)
        {
            return Err(Rejection::NameInUse.into());
        }
        if facts.collection.columns.len() >= 100 {
            return Err(Rejection::Limit.into());
        }
        if facts
            .collection
            .navigation
            .as_ref()
            .is_some_and(|navigation| navigation.changed_at() > now)
        {
            return Err(Error::corrupt(
                "column creation time does not precede navigation",
            ));
        }
        let index = i32::try_from(facts.collection.columns.len())
            .map_err(|_| Error::corrupt("column count is representable"))?;
        let record = change::Data {
            id: self.change_id,
            input: self.input.clone(),
            index,
            operator_id: facts.authority.revision_id,
            created_at: now,
            operation_id: facts.operation,
            digest: self.input.digest()?,
        };
        let column = Change::new(
            Draft::<state::ColumnState>::create(state::Data {
                id: self.id,
                input: self.input.clone(),
                change_id: self.change_id,
                created_at: now,
            }),
            record,
        );
        let mut order = facts
            .collection
            .navigation
            .as_ref()
            .map_or_else(Vec::new, |navigation| navigation.order().to_vec());
        order.push(self.id);
        let append_id = self.append_id;
        let data = navigation::Data {
            id: facts
                .collection
                .navigation
                .as_ref()
                .map_or(self.navigation_id, |navigation| navigation.id()),
            order: order.clone(),
            append_id,
            changed_at: now,
        };
        let navigation = Change::new(
            match &facts.collection.navigation {
                Some(original) => Draft::advance(original, data)?,
                None => Draft::create(data),
            },
            navigation::AppendData {
                id: append_id,
                column_id: self.id,
                change_id: self.change_id,
                order,
                created_at: now,
                operation_id: facts.operation,
            },
        );
        Ok(CreationChange {
            column,
            navigation,
            result: change::Change {
                id: self.change_id,
                column: super::Column {
                    id: self.id,
                    key: self.input.key.clone(),
                    name: self.input.name.clone(),
                    description: self.input.description.clone(),
                    enabled: self.input.enabled,
                    index,
                    change_id: self.change_id,
                },
                operator: facts.operator,
                created_at: now,
                operation_id: facts.operation,
            },
        })
    }
    async fn apply(
        &self,
        tx: &Tx,
        change: CreationChange,
        pending: &mut Option<history::Pending>,
    ) -> Result<change::Change, Error> {
        history::retain(
            pending,
            change
                .column
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await
                .map_err(classify)?,
        )?;
        history::retain(
            pending,
            change
                .navigation
                .write(
                    tx.client(),
                    self.service.app.keys(),
                    self.service.app.anchors(),
                )
                .await?,
        )?;
        Ok(change.result)
    }
    async fn replay(
        &self,
        tx: &Tx,
        request: &completion::Request,
        now: DateTime<Utc>,
    ) -> Result<change::Change, Error> {
        let record = change::original(tx.client(), &self.service, tx.operation_id(), now).await?;
        let append = history::original::<navigation::Append>(
            tx.client(),
            self.service.app.keys(),
            tx.operation_id(),
            now,
        )
        .await?
        .ok_or(Error::corrupt(
            "column creation retains its navigation append",
        ))?;
        if record.input() != &self.input
            || record.digest() != self.input.digest()?
            || request.target() != record.input().key.as_str()
            || append.change_id() != record.id()
            || append.column_id() != record.column_id()
            || append.created_at() != record.created_at()
        {
            return Err(Error::corrupt(
                "the original column result agrees with its completion and append",
            ));
        }
        let operator = self
            .service
            .revisions
            .fetch_identity(tx.client(), record.operator_id())
            .await?;
        self.service
            .app
            .anchors()
            .confirm::<state::ColumnState>(&record)?;
        self.service
            .app
            .anchors()
            .confirm::<navigation::Navigation>(&append)?;
        Ok(record.result(operator))
    }
}

fn classify(error: history::Error) -> Error {
    if let history::Error::Query(query) = &error {
        for (constraint, rejection) in [
            ("columns_key_key", Rejection::KeyInUse),
            ("columns_name_key", Rejection::NameInUse),
        ] {
            if query.is_constraint(
                &tokio_postgres::error::SqlState::UNIQUE_VIOLATION,
                "content",
                "columns",
                constraint,
            ) {
                return rejection.into();
            }
        }
    }
    error.into()
}
