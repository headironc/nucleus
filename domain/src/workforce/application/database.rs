//! Fixed SQL and decoding for the application owner's actual projections.

use tokio_postgres::{Client, Row};

use crate::{
    identity::person,
    pagination::Pagination,
    workforce::application::{
        Application, Detail, Error, Id, Kind, List, Photo, PhotoId, ResourceId, Snapshot, Status,
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
        if row.try_get::<_, &str>("status")? != "submitted" {
            return Err(Error::Configuration);
        }
        Ok(Self {
            id: row.try_get("id")?,
            kind,
            status: Status::Submitted,
            submitted_at: row.try_get("submitted_at")?,
        })
    }
}

impl TryFrom<Row> for Application {
    type Error = Error;
    fn try_from(row: Row) -> Result<Self, Error> {
        let snapshot = serde_json::from_str(row.try_get::<_, &str>("snapshot")?)?;
        let photo = Photo::try_from(&row)?;
        Ok(Self {
            summary: row.try_into()?,
            snapshot,
            photo,
        })
    }
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
    match detail(client, person, true, id).await? {
        Detail::Full { application } => Ok(*application),
        _ => Err(Error::Configuration),
    }
}

pub(super) async fn list(
    client: &Client,
    person: person::Id,
    active: bool,
    pagination: Pagination,
) -> Result<List, Error> {
    let count = client
        .query_one(
            "SELECT count(*) FROM workforce.applications WHERE person_id=$1",
            &[&person],
        )
        .await?
        .try_get(0)?;
    if active {
        let rows = client.query("SELECT application.id,application.kind,application.status,application.submitted_at,application.snapshot::text,photo.id AS photo_id,photo.resource_id,photo.width,photo.height FROM workforce.applications application JOIN workforce.photos photo ON photo.id=application.photo_id WHERE application.person_id=$1 ORDER BY application.submitted_at DESC,application.id DESC LIMIT $2 OFFSET $3", &[&person,&pagination.size(),&pagination.offset()]).await?;
        Ok(List::Full {
            count,
            applications: rows
                .into_iter()
                .map(Application::try_from)
                .collect::<Result<_, _>>()?,
        })
    } else {
        let rows = client.query("SELECT id,kind,status,submitted_at FROM workforce.applications WHERE person_id=$1 AND status='submitted' ORDER BY submitted_at DESC,id DESC LIMIT $2 OFFSET $3", &[&person,&pagination.size(),&pagination.offset()]).await?;
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
) -> Result<Detail, Error> {
    if active {
        let row = client.query_opt("SELECT application.id,application.kind,application.status,application.submitted_at,application.snapshot::text,photo.id AS photo_id,photo.resource_id,photo.width,photo.height FROM workforce.applications application JOIN workforce.photos photo ON photo.id=application.photo_id WHERE application.person_id=$1 AND application.id=$2", &[&person,&id]).await?.ok_or(Error::TargetUnavailable)?;
        Ok(Detail::Full {
            application: Box::new(row.try_into()?),
        })
    } else {
        let row = client.query_opt("SELECT id,kind,status,submitted_at FROM workforce.applications WHERE person_id=$1 AND id=$2 AND status='submitted'", &[&person,&id]).await?.ok_or(Error::TargetUnavailable)?;
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
) -> Result<(u64, String, Option<person::Id>), Error> {
    let row = client.query_opt("SELECT resource.size,resource.media_type,CASE WHEN resource.kind='photo' THEN $2::uuid END FROM workforce.resources resource WHERE resource.id=$1 AND (EXISTS (SELECT 1 FROM workforce.photos photo WHERE photo.resource_id=resource.id AND photo.person_id=$2) OR (resource.kind IN ('background','font') AND (resource.id=$3 OR resource.id=$4 OR EXISTS (SELECT 1 FROM workforce.applications application WHERE application.person_id=$2 AND (application.background_id=resource.id OR application.font_id=resource.id)))))", &[&id,&person,&template.background_id,&template.font_id]).await?.ok_or(Error::TargetUnavailable)?;
    Ok((
        u64::try_from(row.try_get::<_, i64>(0)?).map_err(|_| Error::Configuration)?,
        row.try_get(1)?,
        row.try_get(2)?,
    ))
}
