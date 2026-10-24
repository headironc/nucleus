//! Current values, candidates and history are authenticated within one kernel snapshot.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{
        appointment,
        grant::{self, member},
        role,
    },
    history::{self, Read, Record as _, State as _, View},
    identity::{account, current::Actor, mobile::Mobile, person},
    kernel::Query,
    pagination::Pagination,
    workforce::number::{
        CorrectionHolder, Error, HistoryItem, HistoryKind, HistoryList, Number, Rejection,
        Revision, Service, command,
        record::Event,
        recovery::RecoveryId,
        revision,
        state::{self, State},
    },
};

async fn authorize(
    service: &Service,
    client: &Client,
    actor: &Actor,
    view: View,
) -> Result<DateTime<Utc>, Error> {
    let (_, _, _, now) = member::authorize(
        client,
        &service.roles.at(view),
        &service.current,
        actor,
        role::Code::WorkforceNumbers,
    )
    .await?;
    Ok(now)
}

pub(super) struct Current<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
}

impl Query for Current<'_> {
    type Authorized = (person::Id, DateTime<Utc>);
    type Output = Option<Revision>;
    type Rejection = Rejection;
    async fn authorize(&self, client: &Client, view: View) -> Result<Self::Authorized, Error> {
        let (person, _, now) = self
            .service
            .current
            .at(view)
            .fetch_person(client, self.actor)
            .await?;
        Ok((person, now))
    }
    async fn read(
        &self,
        client: &Client,
        (person, now): Self::Authorized,
        view: View,
    ) -> Result<Self::Output, Error> {
        Ok(history::find::<State>(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Snapshot(view),
            person,
            now,
        )
        .await?
        .and_then(|state| state.current()))
    }
}

pub(super) enum CandidateKind {
    Allocation,
    Correction,
    Recovery,
}

pub(super) struct Candidate<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) mobile: &'a Mobile,
    pub(super) kind: CandidateKind,
}

impl Query for Candidate<'_> {
    type Authorized = DateTime<Utc>;
    type Output = (account::Subject, Option<grant::Id>, Option<Revision>);
    type Rejection = Rejection;
    async fn authorize(&self, client: &Client, view: View) -> Result<DateTime<Utc>, Error> {
        authorize(self.service, client, self.actor, view).await
    }
    async fn read(
        &self,
        client: &Client,
        now: DateTime<Utc>,
        view: View,
    ) -> Result<Self::Output, Error> {
        let roles = self.service.roles.at(view);
        let accounts = self.service.accounts.with_read(Read::Snapshot(view));
        let account = accounts
            .find_by_mobile(client, self.mobile, now)
            .await?
            .ok_or(Rejection::Absent)?;
        let target = appointment::find_by_account(client, &roles, account, now)
            .await?
            .ok_or(Rejection::Absent)?;
        let person = target.person_id();
        let grant = if matches!(self.kind, CandidateKind::Allocation) {
            let (_, member) = command::target(client, &roles, person, now).await?;
            match command::qualify(client, &roles, self.actor, account, &member, now).await {
                Err(Error::Denied) => return Err(Rejection::Absent.into()),
                result => result?,
            }
            Some(member.id())
        } else {
            match command::manageable(client, &roles, self.actor, person, Some(account), now).await
            {
                Err(Error::Denied) => return Err(Rejection::Absent.into()),
                result => result?,
            }
            None
        };
        let state = history::find::<State>(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Snapshot(view),
            person,
            now,
        )
        .await?;
        let number = state.as_ref().and_then(|state| state.current());
        match self.kind {
            CandidateKind::Allocation if state.is_some() => return Err(Rejection::Conflict.into()),
            CandidateKind::Correction if number.is_none() => {
                return Err(Rejection::NumberAbsent.into());
            }
            _ => {}
        }
        let identity = accounts
            .find_by_id(client, account, self.mobile, now)
            .await?
            .ok_or(Rejection::Absent)?;
        Ok((identity, grant, number))
    }
}

pub(super) struct Holder<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) number: Number,
}

impl Query for Holder<'_> {
    type Authorized = DateTime<Utc>;
    type Output = CorrectionHolder;
    type Rejection = Rejection;
    async fn authorize(&self, client: &Client, view: View) -> Result<DateTime<Utc>, Error> {
        authorize(self.service, client, self.actor, view).await
    }
    async fn read(
        &self,
        client: &Client,
        now: DateTime<Utc>,
        view: View,
    ) -> Result<CorrectionHolder, Error> {
        let state = state::holder(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Snapshot(view),
            self.number,
            now,
        )
        .await?
        .filter(|state| state.revision().number() == self.number)
        .ok_or(Rejection::Absent)?;
        let accounts = self.service.accounts.with_read(Read::Snapshot(view));
        let account = accounts.find_by_person(client, state.id(), now).await?;
        match command::manageable(
            client,
            &self.service.roles.at(view),
            self.actor,
            state.id(),
            account,
            now,
        )
        .await
        {
            Err(Error::Denied) => return Err(Rejection::Absent.into()),
            result => result?,
        }
        let identity = person::Reader::new(accounts, std::sync::Arc::clone(&self.service.keys))
            .with_read(Read::Snapshot(view))
            .find_identity(client, state.id(), now)
            .await?
            .ok_or_else(|| Error::corrupt("a number holder retains its personal identity"))?;
        Ok(CorrectionHolder {
            identity,
            number: state.revision(),
        })
    }
}

pub(super) struct History<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) person: person::Id,
    pub(super) pagination: Pagination,
}

impl Query for History<'_> {
    type Authorized = DateTime<Utc>;
    type Output = HistoryList;
    type Rejection = Rejection;
    async fn authorize(&self, client: &Client, view: View) -> Result<DateTime<Utc>, Error> {
        authorize(self.service, client, self.actor, view).await
    }
    async fn read(
        &self,
        client: &Client,
        now: DateTime<Utc>,
        view: View,
    ) -> Result<HistoryList, Error> {
        let state = history::find::<State>(
            client,
            self.service.app.keys(),
            self.service.app.anchors(),
            Read::Snapshot(view),
            self.person,
            now,
        )
        .await?;
        let exists = person::state::exists(client, self.person).await?;
        if !exists {
            return Err(if state.is_some() {
                Error::corrupt("number history retains its person")
            } else {
                Rejection::Absent.into()
            });
        }
        let Some(state) = state else {
            return Ok(HistoryList {
                count: 0,
                revisions: vec![],
            });
        };
        let records = history::page::<State>(
            client,
            self.service.app.keys(),
            &state,
            self.pagination.size(),
            self.pagination.offset(),
            now,
        )
        .await?;
        let operators = records
            .iter()
            .map(|record| record.operator_id())
            .collect::<Vec<_>>();
        let identities = self
            .service
            .revisions
            .fetch_identities(client, &operators)
            .await?;
        let mut revisions = Vec::with_capacity(records.len());
        for record in records {
            let number = command::fixed(self.service, client, &record, now).await?;
            let kind = match record.event() {
                Event::Allocation { source, reason } => HistoryKind::Allocation {
                    source: *source,
                    number,
                    reason: reason.as_ref().map(|reason| reason.as_ref().to_owned()),
                },
                Event::Correction { before_id, reason } => {
                    let before = history::find_value::<revision::Value>(
                        client,
                        self.service.app.keys(),
                        *before_id,
                        now,
                    )
                    .await?
                    .ok_or_else(|| Error::corrupt("a correction retains its previous value"))?;
                    if before.person_id() != self.person {
                        return Err(Error::corrupt("a correction's values belong to its person"));
                    }
                    HistoryKind::Correction {
                        before: before.revision(),
                        after: number,
                        reason: reason.as_ref().to_owned(),
                    }
                }
                Event::Recovery {
                    origin_id, reason, ..
                } => HistoryKind::Recovery {
                    number,
                    origin: *origin_id,
                    reason: reason.as_ref().to_owned(),
                },
                Event::Revocation {
                    recovery_id,
                    reason,
                } => HistoryKind::Revocation {
                    number,
                    recovery: RecoveryId::from_record(*recovery_id),
                    reason: reason.as_ref().to_owned(),
                },
            };
            // Identity is an immutable value; repeated operators retain the same fixed revision.
            let operator = identities
                .get(&record.operator_id())
                .cloned()
                .ok_or(person::revision::ReadError::Missing)?;
            revisions.push(HistoryItem {
                id: super::history::Id::from_record(record.id()),
                kind,
                time: record.time(),
                sequence: record.sequence(),
                operation_id: record.operation_id(),
                operator,
            });
        }
        Ok(HistoryList {
            count: state.sequence(),
            revisions,
        })
    }
}
