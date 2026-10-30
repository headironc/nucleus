//! Current approval authority, fixed applications and one conditional decision.

use super::*;

impl Service {
    async fn manager(
        &self,
        client: &Client,
        actor: &Actor,
        read: Read,
    ) -> Result<(person::Id, DateTime<Utc>), Error> {
        let roles = match read {
            Read::Snapshot(view) => self.roles.at(view),
            _ => self.roles.clone(),
        };
        let (_, person, _, now) = member::authorize(
            client,
            &roles,
            &self.current,
            actor,
            role::Code::WorkforceApproval,
        )
        .await?;
        Ok((person, now))
    }

    pub async fn managed_list(
        &self,
        actor: &Actor,
        query: ManagementQuery,
    ) -> Result<ManagedList, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                Box::pin(self.manager(client, actor, Read::Snapshot(view))).await?;
                database::managed_list(client, query).await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        finish(self, lease, result, false).await
    }

    async fn applicant(
        &self,
        client: &Client,
        person: person::Id,
        read: Read,
        now: DateTime<Utc>,
    ) -> Result<(bool, bool), Error> {
        let accounts = account::state::for_persons(
            client,
            self.app.keys(),
            self.app.anchors(),
            read,
            &[person],
            now,
        )
        .await?;
        let account_active = accounts
            .first()
            .and_then(Option::as_ref)
            .and_then(|account| account.current())
            .is_some_and(|account| account.suspension_id().is_none());
        let roles = match read {
            Read::Snapshot(view) => self.roles.at(view),
            _ => self.roles.clone(),
        };
        let participation_active = member::find_active(client, &roles, person, now)
            .await?
            .is_some();
        Ok((account_active, participation_active))
    }

    pub async fn managed_detail(&self, actor: &Actor, id: Id) -> Result<ManagedDetail, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                let read = Read::Snapshot(view);
                let (_, now) = Box::pin(self.manager(client, actor, read)).await?;
                let persons = self.persons.with_read(read);
                let application = database::managed_application(client, id, &persons, now).await?;
                let person = database::owner(client, id).await?;
                let profile = persons
                    .profile(client, person, now)
                    .await?
                    .ok_or(Error::Configuration)?;
                let (account_active, participation_active) =
                    Box::pin(self.applicant(client, person, read, now)).await?;
                let number = self.numbers.current_in(client, person, now, read).await?;
                let (department, position, title) =
                    database::current_directory(client, &application.snapshot).await?;
                Ok(ManagedDetail {
                    application,
                    current: Current {
                        name: profile.name().as_ref().to_owned(),
                        number,
                        account_active,
                        participation_active,
                        department,
                        position,
                        title,
                    },
                })
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        finish(self, lease, result, false).await
    }

    pub async fn review(
        &self,
        actor: &Actor,
        id: Id,
        input: ReviewInput,
    ) -> Result<Application, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            transaction::shared(client, b"access/scope", b"workforce").await?;
            Box::pin(self.manager(client, actor, Read::Shared)).await?;
            // Discover only immutable lock participants; authority is checked after locking.
            let target = database::owner(client, id).await?;
            account::state::lock_participants(client, actor.account_id(), &[target]).await?;
            let operator = account::state::discover_owner(client, actor.account_id())
                .await?
                .ok_or(current::Error::Invalid)?;
            let mut persons = vec![operator, target];
            persons.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            persons.dedup();
            for person in persons {
                person::state::lock_shared(client, person).await?;
            }
            actor.lock_session(client).await?;
            member::lock_current(client, actor.account_id()).await?;
            if let Some(account) = account::state::discover_person(client, target).await? {
                member::lock_current(client, account).await?;
            }
            let (reviewer, now) = Box::pin(self.manager(client, actor, Read::Shared)).await?;
            if matches!(input, ReviewInput::Approve {}) {
                let (account, participation) =
                    Box::pin(self.applicant(client, target, Read::Shared, now)).await?;
                if !account || !participation {
                    return Err(Error::ApplicantUnavailable);
                }
            }
            database::review(
                client,
                id,
                reviewer,
                input,
                &self.persons.with_read(Read::Shared),
                now,
            )
            .await
        }
        .await;
        finish(self, lease, result, true).await
    }
}
