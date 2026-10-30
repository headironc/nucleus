//! Admission under one explicit authority, with recovery of its permanent paired facts.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::time::Instant;
use tokio_postgres::Client;
use utoipa::ToSchema;

use crate::{
    access::{
        appointment,
        assignment::Assignment,
        grant::{Confirmation, Entry, Error, Grant, Service, database},
        role,
    },
    database::transaction,
    identity::{
        account,
        current::Actor,
        mobile::Mobile,
        operation,
        person::{self, revision},
        suspension,
    },
};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = grant::Target)]
pub struct Target {
    pub(super) account_id: account::Id,
    pub(super) person_id: person::Id,
    pub(super) revision_id: revision::Id,
    #[schema(value_type = String, pattern = "^1[0-9]{10}$")]
    pub(super) mobile: Mobile,
    pub(super) assignment_id: appointment::Id,
    pub(super) suspension_id: Option<suspension::Id>,
    pub(super) grant: Option<Confirmation>,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = grant::Input)]
pub struct Input {
    pub(super) entry: Entry,
    pub(super) target: Target,
    pub(super) role: role::Selection,
}

impl Service {
    pub async fn create(
        &self,
        actor: Actor,
        operation_id: operation::Id,
        input: Input,
    ) -> Result<(Grant, Assignment), Error> {
        super::command::create(self, actor, operation_id, input).await
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
                entry.permission(),
            )
            .await?),
            Entry::Domain => {
                self.authorize_member(client, roles, actor, entry.permission())
                    .await
            }
        }
    }

    pub(super) async fn authorize_target(
        client: &Client,
        roles: &role::Reader,
        entry: Entry,
        manager: &role::Role,
        person_id: person::Id,
        target: &appointment::database::Current,
        now: DateTime<Utc>,
    ) -> Result<(), Error> {
        if target.person_id() == person_id {
            return Err(Error::Absent);
        }
        match entry {
            Entry::Institution if target.role().level() >= manager.level() => Err(Error::Absent),
            Entry::Domain => {
                if let Some(role) =
                    database::find_role_by_person(client, roles, target.person_id(), now).await?
                    && role.level() >= manager.level()
                {
                    return Err(Error::Absent);
                }
                Ok(())
            }
            Entry::Institution => Ok(()),
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
