//! Authorization and direct value queries share one bounded read-only snapshot.

use std::{future::Future, pin::Pin};

use chrono::{DateTime, Utc};
use platform::database::Lease;
use tokio::time::timeout_at;
use tokio_postgres::Client;

use crate::{
    access::{self, role},
    center::affiliation::{Affiliation, Error, Rejection, Service, database},
    database::transaction,
    history::View,
    identity::{current::Actor, person},
    kernel::{Fault, FaultKind, Stage},
};

pub(super) struct Authorized {
    pub(super) person: person::Id,
    pub(super) now: DateTime<Utc>,
}

pub(super) async fn snapshot<T: Send + 'static>(
    service: &Service,
    query: impl for<'a> Fn(
        &'a Client,
        View,
    ) -> Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>
    + Send
    + Sync,
) -> Result<T, Error> {
    service.app.registry().ready()?;
    let budget = service.app.context().read_budget();
    let deadline = budget.deadline();
    let lease = match timeout_at(deadline, service.app.context().connections().lease()).await {
        Ok(result) => result.map_err(Error::connection)?,
        Err(error) => return Error::elapsed(error),
    };
    if let Err(error) = service.app.registry().ready() {
        lease.release();
        return Err(error.into());
    }
    let result = timeout_at(
        deadline,
        budget.run(lease.client(), service.app.anchors(), |view| {
            query(lease.client(), view)
        }),
    )
    .await
    .unwrap_or_else(Error::elapsed);
    if result.as_ref().is_err_and(Error::is_corrupt) {
        service.app.registry().close();
    }
    match result {
        Ok(value) => {
            let known =
                transaction::confirm(value, deadline, lease.client().batch_execute("COMMIT")).await;
            let (known, cleanup) = known.recycle(lease).await;
            match known.end {
                Ok(()) => cleanup.map_err(Error::connection).map(|()| known.value),
                Err(error) => Err(Error::Unavailable(
                    end(error).then_result(cleanup.map_err(termination)),
                )),
            }
        }
        Err(error) if error.is_rejection() => {
            let known =
                transaction::rollback((), deadline, lease.client().batch_execute("ROLLBACK")).await;
            let (known, cleanup) = known.recycle(lease).await;
            if let Err(error) = known.end {
                tracing::error!(stage = "affiliation_read_rollback", error = ?end(error));
            }
            if let Err(error) = cleanup {
                tracing::error!(stage = "affiliation_read_termination", error = ?error);
            }
            Err(error)
        }
        Err(error) => Err(discard(error, lease).await),
    }
}

pub(super) async fn discard<R>(
    error: crate::kernel::Error<R>,
    lease: Lease,
) -> crate::kernel::Error<R> {
    let cleanup = lease.discard().await.map_err(termination);
    match error {
        crate::kernel::Error::Unavailable(fault) => {
            crate::kernel::Error::Unavailable(fault.then_result(cleanup))
        }
        crate::kernel::Error::Unknown(fault) => {
            crate::kernel::Error::Unknown(fault.then_result(cleanup))
        }
        crate::kernel::Error::Corrupt(fault) => {
            crate::kernel::Error::Corrupt(fault.then_result(cleanup))
        }
        error => {
            if let Err(fault) = cleanup {
                tracing::error!(stage = "affiliation_termination", error = ?fault);
            }
            error
        }
    }
}

pub(super) fn termination(error: platform::database::Error) -> Fault {
    Fault::new(Stage::Termination, FaultKind::Connection(error))
}

pub(super) fn end(error: transaction::EndError) -> Fault {
    match error {
        transaction::EndError::Commit(error) => Fault::new(Stage::Commit, FaultKind::Query(error)),
        transaction::EndError::CommitTimeout(error) => {
            Fault::new(Stage::Commit, FaultKind::Timeout(error))
        }
        transaction::EndError::Rollback(error) => {
            Fault::new(Stage::Rollback, FaultKind::Query(error))
        }
        transaction::EndError::RollbackTimeout(error) => {
            Fault::new(Stage::Rollback, FaultKind::Timeout(error))
        }
    }
}

pub(super) async fn authorize(
    service: &Service,
    client: &Client,
    actor: &Actor,
    target: Option<person::Id>,
    view: View,
) -> Result<Authorized, Error> {
    let authority = match target {
        None => {
            let (person, _, now) = service.current.at(view).fetch_person(client, actor).await?;
            Authorized { person, now }
        }
        Some(person) => {
            let (_, operator, _, now) = access::authorize(
                client,
                &service.roles.at(view),
                &service.current,
                actor,
                role::Code::InstitutionAffiliationsRead,
            )
            .await?;
            if person == operator {
                return Err(Rejection::Absent.into());
            }
            Authorized { person, now }
        }
    };
    if !database::person_exists(client, authority.person).await? {
        return Err(Rejection::Absent.into());
    }
    Ok(authority)
}

pub(super) async fn current(
    service: &Service,
    client: &Client,
    actor: &Actor,
    target: Option<person::Id>,
    view: View,
) -> Result<Affiliation, Error> {
    let authority = authorize(service, client, actor, target, view).await?;
    let record = database::find_latest(client, authority.person, authority.now)
        .await?
        .ok_or(Rejection::Unset)?;
    service
        .revisions
        .fetch_identity(client, record.operator_id())
        .await?;
    record.affiliation(service.app.keys().data)
}
