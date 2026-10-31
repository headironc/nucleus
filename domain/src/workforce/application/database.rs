//! Fixed SQL and decoding for the application owner's actual projections.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};

use crate::{
    identity::person,
    pagination::Pagination,
    workforce::application::{
        Application, Approval, Decision, Detail, DirectoryValue, Error, Id, Kind, List,
        ManagedList, ManagedSummary, ManagementQuery, Photo, PhotoId, ResourceId, Snapshot, Status,
        Summary, Template,
    },
};

pub(super) async fn resource(
    client: &Client,
    id: ResourceId,
    kind: &str,
    media_type: &str,
    size: i64,
) -> Result<(), Error> {
    client.execute("INSERT INTO workforce.resources (id,kind,media_type,size) VALUES ($1,$2,$3,$4) ON CONFLICT (id) DO NOTHING", &[&id,&kind,&media_type,&size]).await?;
    let row = client
        .query_one(
            "SELECT kind,media_type,size FROM workforce.resources WHERE id=$1",
            &[&id],
        )
        .await?;
    if row.try_get::<_, &str>(0)? != kind
        || row.try_get::<_, &str>(1)? != media_type
        || row.try_get::<_, i64>(2)? != size
    {
        return Err(Error::Configuration);
    }
    Ok(())
}

pub(super) async fn photo(
    client: &Client,
    id: PhotoId,
    person: person::Id,
    resource: ResourceId,
    width: u32,
    height: u32,
) -> Result<(), Error> {
    client.execute("INSERT INTO workforce.photos (id,person_id,resource_id,width,height) VALUES ($1,$2,$3,$4,$5)",
        &[&id,&person,&resource,&i32::try_from(width).map_err(|_| Error::Configuration)?,&i32::try_from(height).map_err(|_| Error::Configuration)?]).await?;
    client.execute("INSERT INTO audit.events (actor_id,action,target_type,target_id,details) VALUES ($1,'workforce.photo.prepared','workforce.photo',$2,'{}'::jsonb)", &[&person,&id]).await?;
    Ok(())
}

pub(super) async fn owned_photo(
    client: &Client,
    person: person::Id,
    id: PhotoId,
) -> Result<Option<Photo>, Error> {
    client.query_opt("SELECT id AS photo_id,resource_id,width,height FROM workforce.photos WHERE id=$1 AND person_id=$2", &[&id,&person]).await?
        .map(|row| Photo::try_from(&row)).transpose()
}

impl TryFrom<&Row> for Photo {
    type Error = Error;
    fn try_from(row: &Row) -> Result<Self, Error> {
        Ok(Self {
            photo_id: row.try_get("photo_id")?,
            resource_id: row.try_get("resource_id")?,
            width: u32::try_from(row.try_get::<_, i32>("width")?)
                .map_err(|_| Error::Configuration)?,
            height: u32::try_from(row.try_get::<_, i32>("height")?)
                .map_err(|_| Error::Configuration)?,
            media_type: "image/webp",
        })
    }
}

fn kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Issuance => "issuance",
        Kind::Duplication => "duplication",
        Kind::Replacement => "replacement",
    }
}

impl TryFrom<Row> for Summary {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        let kind = match row.try_get::<_, &str>("kind")? {
            "issuance" => Kind::Issuance,
            "duplication" => Kind::Duplication,
            "replacement" => Kind::Replacement,
            _ => return Err(Error::Configuration),
        };
        let status = match row.try_get::<_, &str>("status")? {
            "submitted" => Status::Submitted,
            "approved" => Status::Approved,
            "rejected" => Status::Rejected,
            "cancelled" => Status::Cancelled,
            _ => return Err(Error::Configuration),
        };
        Ok(Self {
            id: row.try_get("id")?,
            kind,
            status,
            submitted_at: row.try_get("submitted_at")?,
        })
    }
}

impl TryFrom<(Row, Option<person::revision::Identity>)> for Application {
    type Error = Error;
    fn try_from((row, identity): (Row, Option<person::revision::Identity>)) -> Result<Self, Error> {
        let snapshot = serde_json::from_str(row.try_get::<_, &str>("snapshot")?)?;
        let photo = Photo::try_from(&row)?;
        let reviewer = row.try_get("reviewer_id")?;
        let reviewed = row.try_get("reviewed_at")?;
        let reason: Option<String> = row.try_get("rejection_reason")?;
        let summary = Summary::try_from(row)?;
        let decision = match (summary.status, reviewer, reviewed, reason, identity) {
            (Status::Submitted, None, None, None, None) => Decision::Submitted,
            (Status::Approved, Some(reviewer_id), Some(reviewed_at), None, Some(identity))
                if reviewed_at >= summary.submitted_at && identity.person_id() == reviewer_id =>
            {
                Decision::Approved {
                    approval: Approval {
                        reviewer_id,
                        reviewer_name: identity.name().to_owned(),
                        reviewed_at,
                    },
                }
            }
            (Status::Cancelled, None, None, None, None) => Decision::Cancelled { approval: None },
            (Status::Cancelled, Some(reviewer_id), Some(reviewed_at), None, Some(identity))
                if reviewed_at >= summary.submitted_at && identity.person_id() == reviewer_id =>
            {
                Decision::Cancelled {
                    approval: Some(Approval {
                        reviewer_id,
                        reviewer_name: identity.name().to_owned(),
                        reviewed_at,
                    }),
                }
            }
            (
                Status::Rejected,
                Some(reviewer_id),
                Some(reviewed_at),
                Some(reason),
                Some(identity),
            ) if reviewed_at >= summary.submitted_at && identity.person_id() == reviewer_id => {
                Decision::Rejected {
                    reviewer_id,
                    reviewer_name: identity.name().to_owned(),
                    reviewed_at,
                    rejection_reason: match reason.try_into() {
                        Ok(reason) => reason,
                        Err(_) => return Err(Error::Configuration),
                    },
                }
            }
            _ => return Err(Error::Configuration),
        };
        Ok(Self {
            id: summary.id,
            kind: summary.kind,
            submitted_at: summary.submitted_at,
            decision,
            snapshot,
            photo,
        })
    }
}

async fn application(
    client: &Client,
    row: Row,
    persons: &person::Reader,
    now: DateTime<Utc>,
) -> Result<Application, Error> {
    let reviewer: Option<person::Id> = row.try_get("reviewer_id")?;
    let identity = match reviewer {
        Some(id) => Some(
            persons
                .find_identity(client, id, now)
                .await?
                .ok_or(Error::Configuration)?,
        ),
        None => None,
    };
    (row, identity).try_into()
}

pub(super) async fn submit(
    client: &Client,
    person: person::Id,
    kind_value: Kind,
    snapshot: Snapshot,
) -> Result<Application, Error> {
    let id = Id::allocate();
    let snapshot = serde_json::to_string(&snapshot)?;
    client.execute("INSERT INTO workforce.applications (id,person_id,kind,snapshot) VALUES ($1,$2,$3,$4::text::jsonb)", &[&id,&person,&kind(kind_value),&snapshot]).await?;
    client.execute("INSERT INTO audit.events (actor_id,action,target_type,target_id,details) VALUES ($1,'workforce.application.submitted','workforce.application',$2,'{}'::jsonb)", &[&person,&id]).await?;
    (application_row(client, id).await?, None).try_into()
}

pub(super) async fn list(
    client: &Client,
    person: person::Id,
    active: bool,
    pagination: Pagination,
    persons: &person::Reader,
    now: DateTime<Utc>,
) -> Result<List, Error> {
    let count = client
        .query_one(
            "SELECT count(*) FROM workforce.applications WHERE person_id=$1 AND ($2 OR status IN ('submitted','approved'))",
            &[&person, &active],
        )
        .await?
        .try_get(0)?;
    if active {
        let rows = client.query("SELECT application.id,application.kind,application.status,application.submitted_at,application.reviewer_id,application.reviewed_at,application.rejection_reason,application.snapshot::text,photo.id AS photo_id,photo.resource_id,photo.width,photo.height FROM workforce.applications application JOIN workforce.photos photo ON photo.id=application.photo_id WHERE application.person_id=$1 ORDER BY application.submitted_at DESC,application.id DESC LIMIT $2 OFFSET $3", &[&person,&pagination.size(),&pagination.offset()]).await?;
        let mut applications = Vec::with_capacity(rows.len());
        for row in rows {
            applications.push(application(client, row, persons, now).await?);
        }
        Ok(List::Full {
            count,
            applications,
        })
    } else {
        let rows = client.query("SELECT id,kind,status,submitted_at FROM workforce.applications WHERE person_id=$1 AND status IN ('submitted','approved') ORDER BY submitted_at DESC,id DESC LIMIT $2 OFFSET $3", &[&person,&pagination.size(),&pagination.offset()]).await?;
        Ok(List::Restricted {
            count,
            applications: rows
                .into_iter()
                .map(Summary::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

pub(super) async fn detail(
    client: &Client,
    person: person::Id,
    active: bool,
    id: Id,
    persons: &person::Reader,
    now: DateTime<Utc>,
) -> Result<Detail, Error> {
    if active {
        let row = client.query_opt("SELECT application.id,application.kind,application.status,application.submitted_at,application.reviewer_id,application.reviewed_at,application.rejection_reason,application.snapshot::text,photo.id AS photo_id,photo.resource_id,photo.width,photo.height FROM workforce.applications application JOIN workforce.photos photo ON photo.id=application.photo_id WHERE application.person_id=$1 AND application.id=$2", &[&person,&id]).await?.ok_or(Error::TargetUnavailable)?;
        Ok(Detail::Full {
            application: Box::new(application(client, row, persons, now).await?),
        })
    } else {
        let row = client.query_opt("SELECT id,kind,status,submitted_at FROM workforce.applications WHERE person_id=$1 AND id=$2 AND status IN ('submitted','approved')", &[&person,&id]).await?.ok_or(Error::TargetUnavailable)?;
        Ok(Detail::Restricted {
            application: row.try_into()?,
        })
    }
}

pub(super) async fn readable_resource(
    client: &Client,
    person: person::Id,
    id: ResourceId,
    template: &Template,
    manager: bool,
) -> Result<(u64, String, Option<person::Id>), Error> {
    let row = client.query_opt("SELECT resource.size,resource.media_type,(SELECT photo.person_id FROM workforce.photos photo WHERE photo.resource_id=resource.id) FROM workforce.resources resource WHERE resource.id=$1 AND (EXISTS (SELECT 1 FROM workforce.photos photo WHERE photo.resource_id=resource.id AND photo.person_id=$2) OR (resource.kind IN ('background','font') AND (resource.id=$3 OR resource.id=$4 OR EXISTS (SELECT 1 FROM workforce.applications application WHERE application.person_id=$2 AND (application.background_id=resource.id OR application.font_id=resource.id)))) OR ($5 AND EXISTS (SELECT 1 FROM workforce.applications application WHERE application.photo_id IN (SELECT photo.id FROM workforce.photos photo WHERE photo.resource_id=resource.id) OR application.background_id=resource.id OR application.font_id=resource.id)))", &[&id,&person,&template.background_id,&template.font_id,&manager]).await?.ok_or(Error::TargetUnavailable)?;
    Ok((
        u64::try_from(row.try_get::<_, i64>(0)?).map_err(|_| Error::Configuration)?,
        row.try_get(1)?,
        row.try_get(2)?,
    ))
}

pub(super) async fn managed_list(
    client: &Client,
    query: ManagementQuery,
) -> Result<ManagedList, Error> {
    let status = match query.status {
        Status::Submitted => "submitted",
        Status::Approved => "approved",
        Status::Rejected => "rejected",
        Status::Cancelled => "cancelled",
    };
    let count = client
        .query_one(
            "SELECT count(*) FROM workforce.applications WHERE status=$1",
            &[&status],
        )
        .await?
        .try_get(0)?;
    let rows = client.query("SELECT id,kind,status,submitted_at,snapshot::text FROM workforce.applications WHERE status=$1 ORDER BY submitted_at DESC,id DESC LIMIT $2 OFFSET $3", &[&status,&query.pagination.size(),&query.pagination.offset()]).await?;
    let applications = rows
        .into_iter()
        .map(|row| {
            let snapshot: Snapshot = serde_json::from_str(row.try_get::<_, &str>("snapshot")?)?;
            Ok(ManagedSummary {
                summary: row.try_into()?,
                name: snapshot.name,
                printed_number: snapshot.printed_number,
                department: snapshot.department,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(ManagedList {
        count,
        applications,
    })
}

pub(super) async fn owner(client: &Client, id: Id) -> Result<person::Id, Error> {
    client
        .query_opt(
            "SELECT person_id FROM workforce.applications WHERE id=$1",
            &[&id],
        )
        .await?
        .ok_or(Error::TargetUnavailable)?
        .try_get(0)
        .map_err(Error::from)
}

async fn application_row(client: &Client, id: Id) -> Result<Row, Error> {
    let row = client.query_opt("SELECT application.id,application.kind,application.status,application.submitted_at,application.reviewer_id,application.reviewed_at,application.rejection_reason,application.snapshot::text,photo.id AS photo_id,photo.resource_id,photo.width,photo.height FROM workforce.applications application JOIN workforce.photos photo ON photo.id=application.photo_id WHERE application.id=$1", &[&id]).await?.ok_or(Error::TargetUnavailable)?;
    Ok(row)
}

pub(super) async fn managed_application(
    client: &Client,
    id: Id,
    persons: &person::Reader,
    now: DateTime<Utc>,
) -> Result<Application, Error> {
    application(client, application_row(client, id).await?, persons, now).await
}

pub(super) async fn current_directory(
    client: &Client,
    snapshot: &Snapshot,
) -> Result<
    (
        DirectoryValue,
        Option<DirectoryValue>,
        Option<DirectoryValue>,
    ),
    Error,
> {
    let choices = crate::workforce::directory::Service::selectable(client).await?;
    let row = client
        .query_one(
            "SELECT name FROM workforce.departments WHERE id=$1",
            &[&snapshot.department.id],
        )
        .await?;
    let department = DirectoryValue {
        name: row
            .try_get::<_, crate::workforce::directory::Name>(0)?
            .as_str()
            .to_owned(),
        enabled: choices
            .departments()
            .iter()
            .any(|department| department.id() == snapshot.department.id),
    };
    let position = if let Some(position) = &snapshot.position {
        let row = client.query_one("SELECT position.name FROM workforce.rosters roster JOIN workforce.positions position ON position.id=roster.position_id WHERE roster.id=$1", &[&position.roster_id]).await?;
        Some(DirectoryValue {
            name: row
                .try_get::<_, crate::workforce::directory::Name>(0)?
                .as_str()
                .to_owned(),
            enabled: choices
                .positions()
                .iter()
                .any(|entry| entry.entry_id() == position.roster_id),
        })
    } else {
        None
    };
    let title = if let Some(title) = &snapshot.title {
        let row = client
            .query_one(
                "SELECT name FROM workforce.titles WHERE id=$1",
                &[&title.id],
            )
            .await?;
        Some(DirectoryValue {
            name: row
                .try_get::<_, crate::workforce::directory::Name>(0)?
                .as_str()
                .to_owned(),
            enabled: choices.titles().iter().any(|entry| entry.id() == title.id),
        })
    } else {
        None
    };
    Ok((department, position, title))
}

pub(super) async fn review(
    client: &Client,
    id: Id,
    reviewer: person::Id,
    input: super::ReviewInput,
    persons: &person::Reader,
    now: DateTime<Utc>,
) -> Result<Application, Error> {
    let (status, reason) = match input {
        super::ReviewInput::Approve {} => ("approved", None),
        super::ReviewInput::Reject { reason } => ("rejected", Some(reason)),
    };
    let changed = client.execute("UPDATE workforce.applications SET status=$2,reviewer_id=$3,reviewed_at=statement_timestamp(),rejection_reason=$4 WHERE id=$1 AND status='submitted'", &[&id,&status,&reviewer,&reason]).await?;
    if changed != 1 {
        return Err(Error::StateChanged);
    }
    client.execute("INSERT INTO audit.events (actor_id,occurred_at,action,target_type,target_id,details) SELECT reviewer_id,reviewed_at,'workforce.application.' || status,'workforce.application',id,jsonb_build_object('previousStatus','submitted','status',status,'rejectionReason',rejection_reason) FROM workforce.applications WHERE id=$1", &[&id]).await?;
    managed_application(client, id, persons, now).await
}

pub(super) async fn cancel(
    client: &Client,
    id: Id,
    operator: person::Id,
    input: super::CancellationInput,
) -> Result<Id, Error> {
    let (expected, own, mode, reason) = match input {
        super::CancellationInput::SelfService { expected_status } => {
            (expected_status, true, "self", None)
        }
        super::CancellationInput::Manager {
            expected_status,
            reason,
        } => (expected_status, false, "manager", Some(reason)),
    };
    let expected = match expected {
        super::CancellableStatus::Submitted => "submitted",
        super::CancellableStatus::Approved => "approved",
    };
    let changed = client
        .execute(
            "UPDATE workforce.applications SET status='cancelled' WHERE id=$1 AND status=$2 AND (NOT $3 OR person_id=$4)",
            &[&id, &expected, &own, &operator],
        )
        .await?;
    if changed != 1 {
        let visible = client
            .query_opt(
                "SELECT id FROM workforce.applications WHERE id=$1 AND (NOT $2 OR person_id=$3)",
                &[&id, &own, &operator],
            )
            .await?
            .is_some();
        return Err(if visible {
            Error::StateChanged
        } else {
            Error::TargetUnavailable
        });
    }
    client.execute(
        "INSERT INTO audit.events (actor_id,action,target_type,target_id,details) VALUES ($1,'workforce.application.cancelled','workforce.application',$2,jsonb_build_object('previousStatus',$3::text,'status','cancelled','mode',$4::text) || CASE WHEN $5::text IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('reason',$5::text) END)",
        &[&operator, &id, &expected, &mode, &reason],
    ).await?;
    Ok(id)
}
