//! Current institutional authority and authenticated permanent appointment queries.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{
        appointment::{Appointment, CommandError, Error, Filter, Id, List},
        capability::Scope,
        holder, role,
    },
    history::{self, Kind as _, Record as _, State as _, Verified},
    identity::{
        account, operation,
        person::{self, revision},
    },
    kernel::completion,
};

pub(super) async fn verify(
    client: &Client,
    roles: &role::Reader,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<(), CommandError> {
    for state in roles.holders(client, now).await? {
        if state.active()
            && roles
                .find(client, state.role_id(), Scope::Institution, now)
                .await?
                .is_none()
        {
            return Err(CommandError::corrupt(
                "a live holder retains an active institutional role",
            ));
        }
    }
    let completions =
        completion::fetch_all(client, roles.keys(), super::operation::COMPLETIONS).await?;
    let count = holder::replacements(client).await?;
    if usize::try_from(count).ok() != Some(completions.len()) {
        return Err(CommandError::corrupt(
            "every replacement retains exactly one completion",
        ));
    }
    for completion in completions {
        let record = original(client, roles, completion.operation_id(), now)
            .await?
            .ok_or(CommandError::corrupt(
                "an appointment completion retains its original record",
            ))?;
        let assignment = record.assignment().ok_or(CommandError::corrupt(
            "a replacement completion refers to an appointment",
        ))?;
        let output = deliver(client, roles, &record, revisions, now).await?;
        let action = match assignment.source {
            holder::Source::Session => "assign",
            holder::Source::System if output.role.kind() == role::Kind::Full => "grant",
            holder::Source::System => "revoke",
            holder::Source::Initialization | holder::Source::Registration => {
                return Err(CommandError::corrupt(
                    "replacement completion retains its declared source",
                ));
            }
        };
        if completion.action() != action
            || completion.target()
                != uuid::Uuid::from_bytes(*record.holder_id().as_bytes()).to_string()
            || completion.created_at() != record.assigned_at()
            || completion.person_id() != output.operator.person_id()
            || completion.revision_id() != assignment.operator_id
        {
            return Err(CommandError::corrupt(
                "an appointment and its completion retain the same original binding",
            ));
        }
    }
    Ok(())
}

pub(crate) struct Current {
    pub(super) id: Id,
    pub(super) person_id: person::Id,
    pub(super) role: role::Role,
}

impl Current {
    pub(crate) fn id(&self) -> Id {
        self.id
    }
    pub(crate) fn person_id(&self) -> person::Id {
        self.person_id
    }
    pub(crate) fn role(&self) -> &role::Role {
        &self.role
    }
    pub(crate) fn into_role(self) -> role::Role {
        self.role
    }
}

pub(crate) async fn find_by_account(
    client: &Client,
    roles: &role::Reader,
    id: account::Id,
    now: DateTime<Utc>,
) -> Result<Option<Current>, Error> {
    let Some(holder) = roles.holder(client, id, now).await? else {
        return Ok(None);
    };
    if !holder.active() {
        return Ok(None);
    }
    let role = role::database::find_by_id(client, roles, holder.role_id(), Scope::Institution, now)
        .await?
        .ok_or(Error::Facts {
            rule: "current holder has a complete institutional role",
        })?;
    Ok(Some(Current {
        id: holder.assignment_id(),
        person_id: holder.person_id(),
        role,
    }))
}

pub(in crate::access) async fn original(
    client: &Client,
    roles: &role::Reader,
    operation_id: operation::Id,
    now: DateTime<Utc>,
) -> Result<Option<Verified<holder::Record>>, Error> {
    let row = client
        .query_opt(
            &format!(
                "{} WHERE record.operation_id=$1 AND record.source IN ('session','system')",
                holder::Record::SELECT
            ),
            &[&operation_id],
        )
        .await?;
    row.as_ref()
        .map(|row| history::record(row, roles.keys(), now).map_err(Error::from))
        .transpose()
}

pub(in crate::access) async fn deliver(
    client: &Client,
    roles: &role::Reader,
    record: &Verified<holder::Record>,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<Appointment, Error> {
    let assignment = record.assignment().ok_or(Error::Facts {
        rule: "an appointment result retains an assignment record",
    })?;
    let role = role::database::definitions(
        client,
        roles.keys(),
        &[(record.role_id(), assignment.change_id)],
        Scope::Institution,
        now,
    )
    .await?
    .pop()
    .ok_or(Error::Facts {
        rule: "an appointment retains its selected role definition",
    })?;
    let subject = revisions
        .fetch_identity(client, assignment.revision_id)
        .await?;
    if subject.person_id() != record.person_id() {
        return Err(Error::Facts {
            rule: "an appointment retains the holder's fixed identity",
        });
    }
    let operator = revisions
        .fetch_identity(client, assignment.operator_id)
        .await?;
    Ok(Appointment {
        id: assignment.id,
        subject,
        operator,
        role,
        sequence: record.sequence(),
        assigned_at: record.assigned_at(),
        operation_id: record.operation_id(),
    })
}

pub(super) async fn fetch_list(
    client: &Client,
    roles: &role::Reader,
    filter: &Filter,
    revisions: &revision::Reader,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let states = roles.holders(client, now).await?;
    holder::verify_cancellations(client, roles.keys(), &states, now).await?;
    let expected = states.iter().try_fold(0_i64, |sum, state| {
        sum.checked_add(state.sequence() - i64::from(!state.active()))
            .ok_or(Error::Facts {
                rule: "appointment count is representable",
            })
    })?;
    let (total, count) = holder::counts(client, filter.person_id, filter.operation_id).await?;
    if total != expected {
        return Err(Error::Facts {
            rule: "retained holders account for all permanent appointments",
        });
    }
    let order = if filter.person_id.is_some() {
        "record.sequence DESC,record.assignment_id DESC"
    } else {
        "record.happened_at DESC,record.assignment_id DESC"
    };
    let rows = client.query(
        &format!("{} WHERE record.source <> 'cancellation' AND ($1::uuid IS NULL OR record.person_id=$1) AND ($2::uuid IS NULL OR record.operation_id=$2) ORDER BY {order} LIMIT $3 OFFSET $4", holder::Record::SELECT),
        &[&filter.person_id, &filter.operation_id, &filter.pagination.size(), &filter.pagination.offset()],
    ).await?;
    let mut records = Vec::with_capacity(rows.len());
    let mut selections = Vec::with_capacity(rows.len());
    let mut ids = Vec::with_capacity(rows.len() * 2);
    for row in &rows {
        let record = history::record::<holder::Record>(row, roles.keys(), now)?;
        let state = states
            .iter()
            .find(|state| state.account_id() == record.holder_id())
            .ok_or(Error::Facts {
                rule: "appointment history retains its holder",
            })?;
        if record.person_id() != state.person_id()
            || record.sequence() > state.sequence() - i64::from(!state.active())
            || (record.sequence() == state.sequence() && !state.has_head(&record))
        {
            return Err(Error::Facts {
                rule: "appointment history agrees with the authenticated holder",
            });
        }
        let assignment = record.assignment().ok_or(Error::Facts {
            rule: "the appointment page contains assignment records",
        })?;
        selections.push((record.role_id(), assignment.change_id));
        ids.extend([assignment.revision_id, assignment.operator_id]);
        records.push(record);
    }
    let definitions =
        role::database::definitions(client, roles.keys(), &selections, Scope::Institution, now)
            .await?;
    let identities = revisions.fetch_identities(client, &ids).await?;
    let mut appointments = Vec::with_capacity(records.len());
    for (record, role) in records.into_iter().zip(definitions) {
        let assignment = record.assignment().ok_or(Error::Facts {
            rule: "a selected appointment retains its assignment",
        })?;
        let subject = identities
            .get(&assignment.revision_id)
            .ok_or(revision::ReadError::Missing)?
            .clone();
        if subject.person_id() != record.person_id() {
            return Err(Error::Facts {
                rule: "appointment history retains the holder's fixed identity",
            });
        }
        let operator = identities
            .get(&assignment.operator_id)
            .ok_or(revision::ReadError::Missing)?
            .clone();
        appointments.push(Appointment {
            id: assignment.id,
            subject,
            operator,
            role,
            sequence: record.sequence(),
            assigned_at: record.assigned_at(),
            operation_id: record.operation_id(),
        });
    }
    Ok(List {
        count,
        appointments,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;

    use crate::{
        access::{
            appointment::{Error, Failure, Filter, tests::register},
            role::{
                self,
                tests::{actor, start},
            },
        },
        history,
        identity::authentication::{Database, Supplier, server},
        keys::seal,
        pagination::Pagination,
        runtime::work,
    };

    #[test]
    fn assignment_history_authenticates_cancelled_holder_heads() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for case in ["retained", "missing", "seal", "cause"] {
                timeout(Duration::from_secs(20), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let mut supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    let manager = actor(&database, &services, &mut supplier).await;
                    let _target = register(&database, &services, &mut supplier).await;
                    let target = services.appointment().candidate(
                        &manager,
                        "13800000000".parse().expect("target mobile"),
                    ).await.expect("current target confirmation");
                    let account = target.identity().id();
                    let person = target.identity().person_id();
                    let cancelled = Box::pin(services.cancellation().cancel_account(
                        manager.clone(),
                        Uuid::new_v4().to_string().parse().expect("cancellation operation"),
                        account,
                        serde_json::from_value(json!({
                            "personId": person,
                            "revisionId": target.identity().revision_id(),
                            "mobile": "13800000000",
                        })).expect("cancellation confirmation"),
                    )).await.expect("real target cancellation");

                    let owner = database.connections("postgres").recover().await.expect("isolated fixture owner");
                    let projection = owner.client().query_one(
                        "SELECT sequence FROM access.projections WHERE holder_id=$1",
                        &[&account],
                    ).await.expect("committed cancellation projection");
                    assert_eq!(projection.get::<_, i64>("sequence"), 2);
                    assert!(owner.client().query_one(
                        "SELECT EXISTS(SELECT 1 FROM identity.accounts account
                            JOIN identity.creations creation ON creation.id=account.id AND creation.person_id=account.person_id
                            JOIN identity.persons person ON person.id=account.person_id
                            JOIN identity.cancellations cancellation ON cancellation.id=account.cancellation_id AND cancellation.account_id=account.id AND cancellation.person_id=person.id
                            JOIN identity.projections projection ON projection.id=cancellation.id AND projection.cause='cancellation' AND projection.account_id=account.id AND projection.person_id=person.id
                            JOIN access.holders holder ON holder.id=account.id AND holder.person_id=person.id
                            JOIN access.projections holder_projection ON holder_projection.id=cancellation.id AND holder_projection.holder_id=holder.id AND holder_projection.person_id=person.id
                            WHERE account.id=$1 AND person.id=$2 AND cancellation.id=$3
                                AND account.mobile IS NULL AND account.id_number IS NULL AND account.suspension_id IS NULL
                                AND account.created_at=creation.created_at AND account.sequence=projection.sequence
                                AND account.changed_at=cancellation.cancelled_at AND projection.projected_at=cancellation.cancelled_at
                                AND projection.operation_id=cancellation.operation_id AND person.sequence=cancellation.sequence
                                AND holder.cancellation_id=cancellation.id AND holder.sequence=holder_projection.sequence
                                AND holder.assignment_id=holder_projection.assignment_id AND holder.role_id=holder_projection.role_id
                                AND holder.changed_at=cancellation.cancelled_at AND holder_projection.projected_at=cancellation.cancelled_at
                                AND holder_projection.operation_id=cancellation.operation_id)",
                        &[&account, &person, &cancelled.id()],
                    ).await.expect("independent Account tombstone, Person cancellation and Holder projection").get::<_, bool>(0));

                    if case != "retained" {
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica")
                            .await.expect("damage only this disposable database");
                        let affected = match case {
                            "missing" => owner.client().execute(
                                "DELETE FROM access.projections WHERE holder_id=$1", &[&account],
                            ).await.expect("remove the permanent projection"),
                            "seal" => owner.client().execute(
                                "UPDATE access.projections SET seal=set_byte(seal,0,get_byte(seal,0)#1) WHERE holder_id=$1", &[&account],
                            ).await.expect("damage the projection seal"),
                            "cause" => owner.client().execute(
                                "UPDATE access.projections SET transaction_id=pg_catalog.pg_current_xact_id() WHERE holder_id=$1", &[&account],
                            ).await.expect("break the projection's original transaction binding"),
                            _ => unreachable!(),
                        };
                        assert_eq!(affected, 1);
                        owner.client().batch_execute("COMMIT").await.expect("committed fixture damage");
                    }

                    services.role().read_current(&manager).await.expect("current authority only needs its live holder and role");
                    assert!(services.appointment().registry.ready().is_ok());
                    let unmatched = Uuid::new_v4().to_string().parse().expect("unmatched operation");
                    let filter = Filter::new(
                        Pagination::new(1, 20).expect("page"),
                        Some(person),
                        (case != "retained").then_some(unmatched),
                    );
                    let result = services.appointment().list(&manager, filter).await;
                    if case == "retained" {
                        let page = result.expect("a cancelled holder retains readable assignment history");
                        assert_eq!(page.count(), 1);
                        assert_eq!(page.appointments().len(), 1);
                        assert_eq!(page.appointments()[0].id(), target.assignment_id());
                        assert_eq!(page.appointments()[0].sequence(), 1);
                        assert!(services.appointment().registry.ready().is_ok());
                    } else {
                        let error = result.err().expect("the filtered empty page still requires its tombstone head");
                        assert_eq!(error.failure(), Failure::Unavailable, "{case}: {error:?}");
                        assert!(work::Failure::unreliable(&error), "{case}: {error:?}");
                        if case == "seal" {
                            assert!(matches!(error, Error::Role(role::Error::History(history::Error::Seal(seal::Error::Mismatch)))));
                        } else {
                            assert!(matches!(error, Error::Role(role::Error::History(history::Error::Integrity(_)))));
                        }
                        assert!(services.appointment().registry.ready().is_err());
                    }
                    assert_eq!(owner.client().query_one(
                        "SELECT count(*) FROM access.appointments WHERE person_id=$1", &[&person],
                    ).await.expect("history reads preserve permanent appointments").get::<_, i64>(0), 1);
                    owner.terminate().await.expect("fixture owner ended");
                    services.close();
                    services.drain().await.expect("services drained");
                    context.stop().await.expect("context stopped");
                    assert!(timeout(Duration::from_millis(30), supplier.request()).await.is_err(), "no supplier requests after the real login and registration");
                    supplier.stop().await;
                })).await.expect("bounded cancellation history case");
            }
        }));
    }
}
