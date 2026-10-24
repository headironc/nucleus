//! Authorization, sealed state and complete history windows share the executor's snapshot.

use chrono::{DateTime, Utc};
use tokio_postgres::Client;

use crate::{
    access::{self, role},
    center::affiliation::{
        Affiliation, Code, Error, List, Rejection, Service, database, management, profile::Profile,
    },
    history::{self, Read, State as _, Verified, View},
    identity::{current::Actor, person},
    kernel::Query,
    pagination::Pagination,
};

pub(super) struct Authorized {
    person: person::Id,
    now: DateTime<Utc>,
}

pub(super) struct Values<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
}

impl Query for Values<'_> {
    type Authorized = ();
    type Output = &'static [Code];
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<(), Error> {
        self.service
            .current
            .at(view)
            .fetch_person(client, self.actor)
            .await?;
        Ok(())
    }

    async fn read(&self, _: &Client, (): (), _: View) -> Result<Self::Output, Error> {
        Ok(Code::all())
    }
}

async fn authorize(
    service: &Service,
    client: &Client,
    actor: &Actor,
    target: Option<person::Id>,
    view: View,
) -> Result<Authorized, Error> {
    match target {
        None => {
            let (person, _, now) = service.current.at(view).fetch_person(client, actor).await?;
            Ok(Authorized { person, now })
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
            Ok(Authorized { person, now })
        }
    }
}

async fn profile(
    service: &Service,
    client: &Client,
    authority: &Authorized,
    view: View,
) -> Result<Option<Verified<Profile>>, Error> {
    let value = history::find(
        client,
        service.app.keys(),
        service.app.anchors(),
        Read::Snapshot(view),
        authority.person,
        authority.now,
    )
    .await?;
    if !database::person_exists(client, authority.person).await? {
        return Err(if value.is_some() {
            Error::corrupt("a profile retains its permanent person")
        } else {
            Rejection::Absent.into()
        });
    }
    Ok(value)
}

pub(super) struct Current<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) target: Option<person::Id>,
}

impl Query for Current<'_> {
    type Authorized = Authorized;
    type Output = Affiliation;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Authorized, Error> {
        authorize(self.service, client, self.actor, self.target, view).await
    }

    async fn read(
        &self,
        client: &Client,
        authority: Authorized,
        view: View,
    ) -> Result<Affiliation, Error> {
        let profile = profile(self.service, client, &authority, view)
            .await?
            .ok_or(Rejection::Unset)?;
        let rows = history::page::<Profile>(
            client,
            self.service.app.keys(),
            &profile,
            1,
            0,
            authority.now,
        )
        .await?;
        Ok(rows
            .first()
            .ok_or(Error::corrupt("a filled profile retains its head record"))?
            .affiliation())
    }
}

pub(super) struct History<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) pagination: Pagination,
}

impl Query for History<'_> {
    type Authorized = Authorized;
    type Output = List;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Authorized, Error> {
        authorize(self.service, client, self.actor, None, view).await
    }

    async fn read(
        &self,
        client: &Client,
        authority: Authorized,
        view: View,
    ) -> Result<List, Error> {
        let Some(profile) = profile(self.service, client, &authority, view).await? else {
            return Ok(List {
                count: 0,
                affiliations: Vec::new(),
            });
        };
        let records = history::page::<Profile>(
            client,
            self.service.app.keys(),
            &profile,
            self.pagination.size(),
            self.pagination.offset(),
            authority.now,
        )
        .await?;
        Ok(List {
            count: profile.sequence(),
            affiliations: records.iter().map(|record| record.affiliation()).collect(),
        })
    }
}

pub(super) struct ManagementHistory<'a> {
    pub(super) service: &'a Service,
    pub(super) actor: &'a Actor,
    pub(super) person: person::Id,
    pub(super) pagination: Pagination,
}

impl Query for ManagementHistory<'_> {
    type Authorized = Authorized;
    type Output = management::List;
    type Rejection = Rejection;

    async fn authorize(&self, client: &Client, view: View) -> Result<Authorized, Error> {
        authorize(self.service, client, self.actor, Some(self.person), view).await
    }

    async fn read(
        &self,
        client: &Client,
        authority: Authorized,
        view: View,
    ) -> Result<management::List, Error> {
        let Some(profile) = profile(self.service, client, &authority, view).await? else {
            return Ok(management::List {
                count: 0,
                affiliations: Vec::new(),
            });
        };
        let records = history::page::<Profile>(
            client,
            self.service.app.keys(),
            &profile,
            self.pagination.size(),
            self.pagination.offset(),
            authority.now,
        )
        .await?;
        let ids = records
            .iter()
            .map(|record| record.operator_id())
            .collect::<Vec<_>>();
        let identities = self
            .service
            .revisions
            .fetch_identities(client, &ids)
            .await?;
        let mut affiliations = Vec::with_capacity(records.len());
        for record in records {
            let operator = identities
                .get(&record.operator_id())
                .ok_or(person::revision::ReadError::Missing)?;
            affiliations.push(management::Affiliation {
                value: record.code(),
                created_at: record.created_at(),
                operator: operator.clone(),
            });
        }
        Ok(management::List {
            count: profile.sequence(),
            affiliations,
        })
    }
}
