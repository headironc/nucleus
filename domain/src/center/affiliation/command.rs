//! Ordinary first-fill and correction transactions. No operation registration or replay.

use std::time::Duration;

use chrono::{DateTime, Utc};
use platform::database::Lease;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Client;

use crate::{
    access::{self, appointment, role},
    center::affiliation::{
        Error, Id, Rejection, Request, Service, database, management::Confirmation, query,
    },
    database::transaction,
    history::Read,
    identity::{account, current::Actor, person},
    kernel::{Authorization, Fault, FaultKind, Stage},
};

pub(super) async fn run(service: &Service, actor: Actor, request: Request) -> Result<(), Error> {
    let result = async {
        service.app.registry().ready()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let lease = match timeout_at(deadline, service.app.context().connections().lease()).await {
            Ok(result) => result.map_err(Error::connection)?,
            Err(error) => return Error::elapsed(error),
        };
        let result = timeout_at(deadline, async {
            service.app.registry().ready()?;
            transaction::begin_current(service.app.context(), lease.client()).await?;
            write(service, lease.client(), &actor, &request).await
        })
        .await
        .unwrap_or_else(Error::elapsed);
        if result.as_ref().is_err_and(Error::is_corrupt) {
            service.app.registry().close();
        }
        finish(lease, deadline, result, false).await
    }
    .await;
    if let Err(error) = &result
        && error.fault().is_some()
    {
        let kind = if matches!(error, Error::Unknown(_)) {
            "unknown"
        } else {
            "unavailable"
        };
        tracing::error!(stage = "affiliation_write", kind, error = ?error);
    }
    result
}

async fn write(
    service: &Service,
    client: &Client,
    actor: &Actor,
    request: &Request,
) -> Result<(), Error> {
    match request {
        Request::Fill(_) => database::lock(client, actor).await?,
        Request::Correct { input } => {
            transaction::shared(client, b"access/scope", b"institution").await?;
            let target = input.target.person_id();
            let located = account::state::discover_person(client, target).await?;
            match located {
                Some(account) => {
                    database::lock_accounts(client, actor.account_id(), account).await?
                }
                None => actor.lock_account(client).await?,
            }
            let actor_person = account::state::discover_owner(client, actor.account_id()).await?;
            person::state::lock_participants(client, actor_person, &[target]).await?;
            if located != account::state::discover_person(client, target).await? {
                return Err(Error::lock_set_changed());
            }
        }
    }
    actor.lock_session(client).await?;
    let subject = service
        .current
        .fetch_state(client, actor, Read::Shared)
        .await?;
    let authority = match request {
        Request::Fill(_) => Authorization {
            person_id: subject.person_id(),
            revision_id: subject.revision_id(),
            now: subject.observed_at(),
        },
        Request::Correct { input } => {
            let (role, person_id, revision_id, now) = access::authorize_subject(
                client,
                &service.roles,
                subject,
                role::Code::InstitutionAffiliationsCorrect,
            )
            .await?;
            let target = input.target.person_id();
            let account = account::state::located(
                client,
                service.roles.keys(),
                service.roles.anchors(),
                service.roles.read()?,
                account::state::Location::Person(target),
                now,
            )
            .await?
            .filter(|account| account.current().is_some())
            .map(|account| account.id());
            let target_level = match account {
                Some(account) => Some(
                    appointment::find_by_account(client, &service.roles, account, now)
                        .await?
                        .ok_or(Error::corrupt(
                            "a target account retains its institutional assignment",
                        ))?
                        .role()
                        .level(),
                ),
                None => None,
            };
            qualify(person_id == target, role.level(), target_level)?;
            let identity = service
                .persons
                .with_read(service.roles.read()?)
                .find_identity(client, target, now)
                .await?;
            let identity_matches = identity
                .is_some_and(|identity| identity.revision_id() == input.target.revision_id());
            let account_matches = match (&input.target, account) {
                (
                    Confirmation::Account {
                        account_id, mobile, ..
                    },
                    Some(current),
                ) if *account_id == current => match mobile {
                    Some(mobile) => service
                        .accounts
                        .find_by_id(client, current, mobile, now)
                        .await?
                        .is_some(),
                    None => true,
                },
                (Confirmation::Person { .. }, None) => true,
                _ => false,
            };
            if !identity_matches || !account_matches {
                return Err(Rejection::TargetChanged.into());
            }
            Authorization {
                person_id,
                revision_id,
                now,
            }
        }
    };
    let person_id = match request {
        Request::Fill(_) => authority.person_id,
        Request::Correct { input } => input.target.person_id(),
    };
    let latest = database::find_latest(client, person_id, authority.now).await?;
    if matches!(request, Request::Fill(_)) && latest.is_some() {
        return Err(Rejection::Conflict.into());
    }
    advancing(
        latest.as_ref().map(database::Record::created_at),
        authority.now,
    )?;
    let id = Id::allocate();
    database::insert(
        client,
        service.app.keys().data,
        person_id,
        authority.revision_id,
        id,
        request.code(),
        authority.now,
    )
    .await?;
    database::audit(client, authority.person_id, request.action(), id).await
}

fn qualify(
    same_person: bool,
    level: role::Level,
    target_level: Option<role::Level>,
) -> Result<(), Rejection> {
    if same_person || target_level.is_some_and(|target| target >= level) {
        return Err(Rejection::Absent);
    }
    Ok(())
}

pub(super) fn advancing<R>(
    latest: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), crate::kernel::Error<R>> {
    if latest.is_some_and(|latest| now <= latest) {
        return Err(crate::kernel::Error::Unavailable(Fault::new(
            Stage::Time,
            FaultKind::Integrity("affiliation time strictly advances the previous value"),
        )));
    }
    Ok(())
}

/// Only an acknowledged transaction end permits reuse. COMMIT errors remain unknown.
pub(super) async fn finish<R>(
    lease: Lease,
    deadline: Instant,
    result: Result<(), crate::kernel::Error<R>>,
    dry_run: bool,
) -> Result<(), crate::kernel::Error<R>> {
    match result {
        Ok(()) if !dry_run => {
            let completion = transaction::commit(
                transaction::Outcome::Pending(()),
                deadline,
                lease.client().batch_execute("COMMIT"),
            )
            .await;
            let (completion, cleanup) = completion.recycle(lease).await;
            match completion {
                transaction::Completion::Known(_) => {
                    if let Err(error) = cleanup {
                        tracing::error!(stage = "affiliation_commit_cleanup", outcome = "committed", error = ?error);
                    }
                    Ok(())
                }
                transaction::Completion::Pending { error, .. } => {
                    Err(crate::kernel::Error::Unknown(
                        query::end(error).then_result(cleanup.map_err(query::termination)),
                    ))
                }
            }
        }
        Ok(()) => {
            let known =
                transaction::rollback((), deadline, lease.client().batch_execute("ROLLBACK")).await;
            let (known, cleanup) = known.recycle(lease).await;
            match known.end {
                Ok(()) => cleanup.map_err(crate::kernel::Error::connection),
                Err(error) => Err(crate::kernel::Error::Unavailable(
                    query::end(error).then_result(cleanup.map_err(query::termination)),
                )),
            }
        }
        Err(error) => Err(query::discard(error, lease).await),
    }
}
