//! Explicit generation and current-authority access to permanently adopted badge files.

use super::*;

pub(super) struct Candidate {
    application: Id,
    badge: Badge,
    size: usize,
}

impl Service {
    pub async fn render(&self, actor: &Actor, id: Id) -> Result<Badge, Error> {
        let candidate = Box::pin(self.prepare_badge(actor, id)).await?;
        Box::pin(self.adopt_badge(actor, candidate)).await
    }

    pub(super) async fn prepare_badge(&self, actor: &Actor, id: Id) -> Result<Candidate, Error> {
        self.app.registry().ready()?;
        let settings = self.settings()?.clone();
        // Read only the retained snapshot; release the transaction before blocking work.
        let detail = Box::pin(self.managed_detail(actor, id)).await?;
        if !matches!(detail.application.decision, Decision::Approved { .. }) {
            return Err(Error::StateChanged);
        }
        if !detail.current.account_active || !detail.current.participation_active {
            return Err(Error::ApplicantUnavailable);
        }
        let application = detail.application;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            let mut resources = Vec::new();
            for resource in [
                application.snapshot.template.background_id,
                application.snapshot.template.font_id,
                application.photo.resource_id,
            ] {
                let row = client
                    .query_one(
                        "SELECT size FROM workforce.resources WHERE id=$1",
                        &[&resource],
                    )
                    .await?;
                resources.push(
                    u64::try_from(row.try_get::<_, i64>(0)?).map_err(|_| Error::Configuration)?,
                );
            }
            Ok((database::owner(client, id).await?, resources))
        }
        .await;
        let (owner, sizes) = finish(self, lease, result, false).await?;
        let permit = Arc::clone(&self.processing)
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let badge = Badge {
            id: BadgeId::allocate(),
        };
        let badge_id = badge.id;
        let generated = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let read = |segments: &[&str], id: ResourceId, size| {
                settings
                    .directory
                    .open(segments, &id.to_string())?
                    .ok_or(Error::MissingFile)?
                    .read(size)
                    .map_err(Error::from)
            };
            let background = read(
                &["workforce"],
                application.snapshot.template.background_id,
                sizes[0],
            )?;
            let font = read(
                &["workforce"],
                application.snapshot.template.font_id,
                sizes[1],
            )?;
            let photo = read(
                &["workforce", "photos", &owner.to_string()],
                application.photo.resource_id,
                sizes[2],
            )?;
            let bytes = render::encode(
                &application.snapshot,
                settings.resolution,
                &background,
                &photo,
                font,
            )?;
            settings.badge_limits.publish(
                &settings.directory,
                "badges",
                owner,
                &badge_id.to_string(),
                &bytes,
            )?;
            Ok::<_, Error>(bytes.len())
        })
        .await
        .map_err(|_| Error::Worker)
        .and_then(|result| result);
        let size = match generated {
            Ok(size) => size,
            Err(error) => {
                let code = match error {
                    Error::MissingGlyph => "missing_glyph",
                    Error::TextOverflow => "text_overflow",
                    _ => "dependency_unavailable",
                };
                if let Err(persist) = self.render_failure(actor, id, code).await {
                    tracing::error!(stage="badge_render_error", error=?persist);
                }
                return Err(error);
            }
        };
        Ok(Candidate {
            application: id,
            badge,
            size,
        })
    }

    pub(super) async fn adopt_badge(
        &self,
        actor: &Actor,
        candidate: Candidate,
    ) -> Result<Badge, Error> {
        let Candidate {
            application: id,
            badge,
            size,
        } = candidate;
        let badge_id = badge.id;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            let (target, operator, now) = Box::pin(self.lock_managed(client, actor, id)).await?;
            let (account, participation) = Box::pin(self.applicant(client, target, Read::Shared, now)).await?;
            if !account || !participation { return Err(Error::ApplicantUnavailable); }
            if client.execute("UPDATE workforce.applications SET status='ready',render_error=NULL WHERE id=$1 AND status='approved'", &[&id]).await? != 1 { return Err(Error::StateChanged); }
            let size = i64::try_from(size).map_err(|_| Error::Configuration)?;
            client.execute("INSERT INTO workforce.badges (id,application_id,generated_by,size) VALUES ($1,$2,$3,$4)", &[&badge_id,&id,&operator,&size]).await?;
            client.execute("INSERT INTO audit.events (actor_id,action,target_type,target_id,details) VALUES ($1,'workforce.application.ready','workforce.application',$2,jsonb_build_object('badgeId',$3::uuid))", &[&operator,&id,&badge_id]).await?;
            Ok(badge)
        }.await;
        finish(self, lease, result, true).await
    }

    async fn render_failure(&self, actor: &Actor, id: Id, code: &str) -> Result<(), Error> {
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            Box::pin(self.lock_managed(client, actor, id)).await?;
            client.execute("UPDATE workforce.applications SET render_error=$2 WHERE id=$1 AND status='approved'", &[&id,&code]).await?;
            Ok(())
        }.await;
        finish(self, lease, result, true).await
    }

    pub async fn badge(
        &self,
        actor: &Actor,
        id: BadgeId,
        capacity: Arc<OwnedSemaphorePermit>,
        head: bool,
    ) -> Result<Resource, Error> {
        self.app.registry().ready()?;
        let settings = self.settings()?.clone();
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = timeout(Duration::from_secs(3), self.app.context().read_budget().run(client, self.app.anchors(), |view| async move {
            let (person, _, active, _) = self.identity(client, actor, Read::Snapshot(view)).await?;
            if !active { return Err(Error::TargetUnavailable); }
            let manager = match Box::pin(self.manager(client, actor, Read::Snapshot(view))).await {
                Ok(_) => true, Err(Error::Grant(grant::Error::Denied)) => false, Err(error) => return Err(error),
            };
            let row = client.query_opt("SELECT badge.size,application.person_id FROM workforce.badges badge JOIN workforce.applications application ON application.id=badge.application_id WHERE badge.id=$1 AND ($2 OR application.person_id=$3)", &[&id,&manager,&person]).await?.ok_or(Error::TargetUnavailable)?;
            Ok((u64::try_from(row.try_get::<_, i64>(0)?).map_err(|_| Error::Configuration)?, row.try_get::<_, person::Id>(1)?))
        })).await.map_err(|_| Error::Timeout)?;
        let (size, owner) = finish(self, lease, result, false).await?;
        let bytes = tokio::task::spawn_blocking(move || {
            let _capacity = capacity;
            let file = settings
                .directory
                .open(
                    &["workforce", "badges", &owner.to_string()],
                    &id.to_string(),
                )?
                .ok_or(Error::MissingFile)?;
            if head {
                file.verify_size(size)?;
                Ok(Vec::new())
            } else {
                file.read(size).map_err(Error::from)
            }
        })
        .await
        .map_err(|_| Error::Worker)??;
        Ok(Resource {
            bytes,
            media_type: "image/webp".to_owned(),
            size,
        })
    }
}
