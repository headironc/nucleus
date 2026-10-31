//! Confirmed fixed badge applications: ordinary transactions and current self-state refresh.

mod database;
pub mod photo;
mod review;
mod settings;
mod value;

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};
use tokio_postgres::Client;

pub use self::{settings::Settings, value::*};
use crate::{
    access::{
        grant::{self, member},
        role,
    },
    database::{Query, read, transaction},
    history::{self, Read},
    identity::{
        account,
        current::{self, Actor},
        person,
    },
    kernel::App,
    pagination::Pagination,
    workforce::{directory, font, number},
};

#[derive(Clone)]
pub struct Service {
    app: App,
    current: current::Service,
    numbers: number::Service,
    roles: role::Reader,
    persons: person::Reader,
    settings: Option<Arc<Settings>>,
    // The owned permit moves into the blocking closure, including after a caller disconnects.
    processing: Arc<Semaphore>,
}

impl Service {
    pub(crate) fn prepare(
        app: App,
        current: current::Service,
        numbers: number::Service,
        keys: Arc<crate::identity::keys::Keys>,
        settings: Option<Arc<Settings>>,
    ) -> Self {
        Self {
            persons: person::Reader::new(
                account::Reader::new(app.clone(), Arc::clone(&keys)),
                keys,
            ),
            roles: role::Reader::new(app.clone()),
            app,
            current,
            numbers,
            settings,
            processing: Arc::new(Semaphore::new(2)),
        }
    }

    pub(crate) async fn register(client: &Client, settings: &Settings) -> Result<(), Error> {
        for &(id, kind, media_type, size) in &settings.resources {
            database::resource(client, id, kind, media_type, size).await?;
        }
        Ok(())
    }

    fn settings(&self) -> Result<&Settings, Error> {
        self.settings.as_deref().ok_or(Error::Configuration)
    }

    async fn identity(
        &self,
        client: &Client,
        actor: &Actor,
        read: Read,
    ) -> Result<(person::Id, person::name::Name, bool, DateTime<Utc>), Error> {
        let (person, name, now) = self.current.fetch_name(client, actor, read).await?;
        let roles = match read {
            Read::Snapshot(view) => self.roles.at(view),
            _ => self.roles.clone(),
        };
        let active = member::find_active(client, &roles, person, now)
            .await?
            .is_some();
        Ok((person, name, active, now))
    }

    async fn lock(&self, client: &Client, actor: &Actor) -> Result<(), Error> {
        transaction::shared(client, b"access/scope", b"workforce").await?;
        actor.lock_account(client).await?;
        let person = account::state::discover_owner(client, actor.account_id()).await?;
        // Directory writers resolve Person after taking their directory lock. This consumer
        // never changes Person, so a shared row lock avoids an opposite-order lock cycle.
        if let Some(person) = person {
            person::state::lock_shared(client, person).await?;
        }
        actor.lock_session(client).await?;
        member::lock_current(client, actor.account_id()).await?;
        client
            .query_one("SELECT workforce.lock_directory()", &[])
            .await?;
        Ok(())
    }

    pub async fn prepare_photo(
        &self,
        actor: &Actor,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<Photo, Error> {
        self.app.registry().ready()?;
        let settings = self.settings()?.clone();
        // Establish eligibility before expensive image processing; repeat it at acceptance.
        let prepared_person = self.require_active(actor).await?;
        let permit = Arc::clone(&self.processing)
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let resource_id = ResourceId::allocate();
        let (width, height, size) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let photo = photo::prepare(&media_type, &bytes)?;
            settings.photo_limits.publish(
                &settings.directory,
                prepared_person,
                resource_id,
                &photo.bytes,
            )?;
            Ok::<_, Error>((photo.width, photo.height, photo.bytes.len()))
        })
        .await
        .map_err(|_| Error::Worker)??;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            self.lock(client, actor).await?;
            let (person, _, active, _) = self.identity(client, actor, Read::Shared).await?;
            if !active || person != prepared_person {
                return Err(Error::Denied);
            }
            let id = PhotoId::allocate();
            database::resource(
                client,
                resource_id,
                "photo",
                "image/webp",
                i64::try_from(size).map_err(|_| Error::Configuration)?,
            )
            .await?;
            database::photo(client, id, person, resource_id, width, height).await?;
            Ok(Photo {
                photo_id: id,
                resource_id,
                width,
                height,
                media_type: "image/webp",
            })
        }
        .await;
        finish(self, lease, result, true).await
    }

    async fn require_active(&self, actor: &Actor) -> Result<person::Id, Error> {
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                let (person, _, active, _) =
                    self.identity(client, actor, Read::Snapshot(view)).await?;
                if !active {
                    return Err(Error::Denied);
                }
                Ok(person)
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        finish(self, lease, result, false).await
    }

    pub async fn template(&self, actor: &Actor) -> Result<TemplateChoices, Error> {
        self.app.registry().ready()?;
        self.require_active(actor).await?;
        let settings = self.settings()?;
        Ok(TemplateChoices {
            template: settings.template.clone(),
            pickups: settings.pickups.clone(),
        })
    }

    pub async fn preview(&self, actor: &Actor, input: PreviewInput) -> Result<Preview, Error> {
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let selections = &input.selections;
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                self.snapshot(client, actor, selections, Read::Snapshot(view))
                    .await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        let (_, snapshot, photo) = finish(self, lease, result, false).await?;
        Ok(Preview {
            kind: input.kind,
            snapshot,
            photo,
        })
    }

    async fn snapshot(
        &self,
        client: &Client,
        actor: &Actor,
        selections: &Selections,
        read: Read,
    ) -> Result<(person::Id, Snapshot, Photo), Error> {
        let settings = self.settings()?;
        let (person, name, active, now) = self.identity(client, actor, read).await?;
        if !active {
            return Err(Error::Denied);
        }
        let number = self
            .numbers
            .current_in(client, person, now, read)
            .await?
            .ok_or(Error::NumberRequired)?;
        let directory = directory::Service::selectable(client).await?;
        if !directory.missing().is_empty() {
            return Err(Error::Configuration);
        }
        let rule = directory
            .rules()
            .iter()
            .find(|rule| rule.id() == number.subtype())
            .ok_or(Error::Configuration)?;
        let department = directory
            .departments()
            .iter()
            .find(|department| department.id() == selections.department_id)
            .ok_or(Error::SelectionUnavailable)?;
        let position = match selections.roster_id {
            None => None,
            Some(id) => {
                if !rule.position() {
                    return Err(Error::SelectionUnavailable);
                }
                let position = directory
                    .positions()
                    .iter()
                    .find(|position| {
                        position.entry_id() == id
                            && position.department_id() == selections.department_id
                    })
                    .ok_or(Error::SelectionUnavailable)?;
                Some(Position {
                    roster_id: id,
                    name: position.name().as_str().to_owned(),
                })
            }
        };
        let title = match selections.title_id {
            None => None,
            Some(id) => {
                let title = directory
                    .titles()
                    .iter()
                    .find(|title| title.id() == id)
                    .ok_or(Error::SelectionUnavailable)?;
                Some(Title {
                    id,
                    name: title.name().as_str().to_owned(),
                })
            }
        };
        let photo = database::owned_photo(client, person, selections.photo_id)
            .await?
            .ok_or(Error::TargetUnavailable)?;
        if !settings.pickups.contains(&selections.pickup) {
            return Err(Error::SelectionUnavailable);
        }
        let printed_department = match &position {
            Some(position) => format!("{}{}", department.name().as_str(), position.name),
            None => department.name().as_str().to_owned(),
        };
        let printed_title = title
            .as_ref()
            .map_or_else(|| "无".to_owned(), |title| title.name.clone());
        let printed_number = format!(
            "{}{:02}{:02}",
            number.subtype().category(),
            number.year().rem_euclid(100),
            number.serial()
        );
        let text = TextLayout {
            name: settings.text(name.as_ref(), &settings.template.layout.name)?,
            number: settings.text(&printed_number, &settings.template.layout.number)?,
            department: settings.text(&printed_department, &settings.template.layout.department)?,
            title: settings.text(&printed_title, &settings.template.layout.title)?,
        };
        Ok((
            person,
            Snapshot {
                name: name.as_ref().to_owned(),
                number,
                printed_number,
                department: Department {
                    id: department.id(),
                    name: department.name().as_str().to_owned(),
                    printed: printed_department,
                },
                position,
                title,
                printed_title,
                photo_id: selections.photo_id,
                template: settings.template.clone(),
                pickup: selections.pickup.clone(),
                text,
            },
            photo,
        ))
    }

    pub async fn submit(&self, actor: &Actor, input: Input) -> Result<Application, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            self.lock(client, actor).await?;
            let (person, snapshot, _) = self
                .snapshot(client, actor, &input.selections, Read::Shared)
                .await?;
            if snapshot != input.expected {
                return Err(Error::Changed);
            }
            database::submit(client, person, input.kind, snapshot).await
        }
        .await;
        finish(self, lease, result, true).await
    }

    pub async fn list(&self, actor: &Actor, pagination: Pagination) -> Result<List, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                let (person, _, active, now) =
                    self.identity(client, actor, Read::Snapshot(view)).await?;
                database::list(
                    client,
                    person,
                    active,
                    pagination,
                    &self.persons.with_read(Read::Snapshot(view)),
                    now,
                )
                .await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        finish(self, lease, result, false).await
    }

    pub async fn detail(&self, actor: &Actor, id: Id) -> Result<Detail, Error> {
        self.app.registry().ready()?;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                let (person, _, active, now) =
                    self.identity(client, actor, Read::Snapshot(view)).await?;
                database::detail(
                    client,
                    person,
                    active,
                    id,
                    &self.persons.with_read(Read::Snapshot(view)),
                    now,
                )
                .await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        finish(self, lease, result, false).await
    }

    /// The connection's capacity also covers a file read that outlives its async caller.
    pub async fn resource(
        &self,
        actor: &Actor,
        id: ResourceId,
        capacity: Arc<OwnedSemaphorePermit>,
        head: bool,
    ) -> Result<Resource, Error> {
        self.app.registry().ready()?;
        let settings = self.settings()?.clone();
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                let (person, _, active, _) =
                    self.identity(client, actor, Read::Snapshot(view)).await?;
                if !active {
                    return Err(Error::TargetUnavailable);
                }
                let roles = self.roles.at(view);
                let manager = match Box::pin(member::authorize(
                    client,
                    &roles,
                    &self.current,
                    actor,
                    role::Code::WorkforceApproval,
                ))
                .await
                {
                    Ok(_) => true,
                    Err(grant::Error::Denied) => false,
                    Err(error) => return Err(Error::Grant(error)),
                };
                database::readable_resource(client, person, id, &self.settings()?.template, manager)
                    .await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        let (size, media_type, photo_person) = finish(self, lease, result, false).await?;
        let permit = if head {
            None
        } else {
            Some(
                Arc::clone(&self.processing)
                    .try_acquire_owned()
                    .map_err(|_| Error::Busy)?,
            )
        };
        let bytes = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _capacity = capacity;
            let person;
            let segments: &[&str] = if let Some(photo_person) = photo_person {
                person = photo_person.to_string();
                &["workforce", "photos", &person]
            } else {
                &["workforce"]
            };
            let file = settings
                .directory
                .open(segments, &id.to_string())?
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
            media_type,
            size,
        })
    }
}

pub struct Resource {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub size: u64,
}

/// A failed COMMIT stays unknown. The lease is never returned before an acknowledged end.
async fn finish<T>(
    service: &Service,
    lease: platform::database::Lease,
    result: Result<T, Error>,
    writing: bool,
) -> Result<T, Error> {
    if result.as_ref().is_err_and(Error::unreliable) {
        service.app.registry().close();
    }
    match result {
        Ok(value) => match lease.client().batch_execute("COMMIT").await {
            Ok(()) => {
                lease.release();
                Ok(value)
            }
            Err(error) if writing => Err(Error::Unknown(Query::from(error))),
            Err(error) => Err(Error::Query(Query::from(error))),
        },
        Err(error) => match lease.client().batch_execute("ROLLBACK").await {
            Ok(()) => {
                lease.release();
                Err(error)
            }
            Err(rollback) => Err(Error::Rollback {
                cause: Box::new(error),
                rollback: Query::from(rollback),
            }),
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("current identity failed")]
    Identity(#[from] current::Error),
    #[error("current participation failed")]
    Grant(#[from] grant::Error),
    #[error("current history failed")]
    History(#[from] history::Error),
    #[error("current number failed")]
    Number(#[from] number::Error),
    #[error("current directory failed")]
    Directory(#[from] directory::Error),
    #[error("current person failed")]
    Person(#[from] person::ReadError),
    #[error("database operation failed")]
    Query(#[from] Query),
    #[error("database connection failed")]
    Connection(#[from] platform::database::Error),
    #[error("transaction preparation failed")]
    Transaction(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("file operation failed")]
    File(#[from] platform::files::Error),
    #[error("font preparation failed")]
    Font(#[from] font::InputError),
    #[error("photo preparation failed")]
    Photo(#[from] photo::Error),
    #[error("application encoding failed")]
    Encoding(#[from] serde_json::Error),
    #[error("application processing is busy")]
    Busy,
    #[error("retained photograph capacity is exhausted")]
    PhotoCapacity,
    #[error("application processing failed")]
    Worker,
    #[error("application read timed out")]
    Timeout,
    #[error("application admission failed")]
    Admission(#[from] crate::runtime::work::Error),
    #[error("current participation is required")]
    Denied,
    #[error("a current number is required")]
    NumberRequired,
    #[error("the selected directory or pickup is unavailable")]
    SelectionUnavailable,
    #[error("the target is unavailable")]
    TargetUnavailable,
    #[error("the confirmed content changed")]
    Changed,
    #[error("the application already has a decision")]
    StateChanged,
    #[error("the applicant's current account and participation are required")]
    ApplicantUnavailable,
    #[error("the template has a missing glyph")]
    MissingGlyph,
    #[error("complete text does not fit the configured area")]
    TextOverflow,
    #[error("application configuration is invalid or missing")]
    Configuration,
    #[error("a retained file is missing")]
    MissingFile,
    #[error("application commit outcome is unknown")]
    Unknown(#[source] Query),
    #[error("application rollback failed")]
    Rollback { cause: Box<Error>, rollback: Query },
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Query(Query::from(error))
    }
}

impl From<transaction::Error> for Error {
    fn from(error: transaction::Error) -> Self {
        Self::Transaction(Box::new(error))
    }
}

impl Error {
    fn unreliable(&self) -> bool {
        match self {
            Self::Identity(error) => error.unreliable(),
            Self::Grant(error) => crate::runtime::work::Failure::unreliable(error),
            Self::History(error) => error.corrupt(),
            Self::Number(error) => error.is_corrupt(),
            Self::Directory(error) => error.is_corrupt(),
            Self::Person(error) => error.unreliable(),
            Self::Rollback { cause, .. } => cause.unreliable(),
            _ => false,
        }
    }
}

impl read::Failure for Error {
    fn changed(&self) -> bool {
        match self {
            Self::Number(error) => error.changed(),
            Self::Directory(error) => error.changed(),
            _ => read::changed(self),
        }
    }
}

/// Legal Latin-only fixture materials for application assembly tests, never production input.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::expect_used)]
pub fn fixture_settings(storage: platform::files::Settings) -> Settings {
    use std::io::Cursor;
    let mut background = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(85, 54)
        .write_to(&mut background, image::ImageFormat::Png)
        .expect("fixture background");
    Settings::new(
        storage,
        fixture_template(),
        "image/png",
        background.into_inner(),
        include_bytes!("../../../tests/fixtures/fonts/SourceSans3-Regular.ttf").to_vec(),
        vec!["Fixture reception".to_owned()],
        photo::Limits::new(100, 1 << 30, 1000, 10 << 30).expect("fixture capacity"),
    )
    .expect("legal fixture settings")
}

#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::expect_used)]
pub fn fixture_template() -> Template {
    let area = |y| TextArea {
        area: Rectangle {
            x: 20_000,
            y,
            width: 60_000,
            height: 10_000,
        },
        font_size: 2500,
        line_height: 3500,
        wrap: true,
    };
    Template {
        background_id: "40000000-0000-4000-8000-000000000001"
            .parse()
            .expect("fixture background id"),
        font_id: "40000000-0000-4000-8000-000000000002"
            .parse()
            .expect("fixture font id"),
        layout: Layout {
            width: 85_000,
            height: 54_000,
            photo: Rectangle {
                x: 1000,
                y: 1000,
                width: 12_000,
                height: 16_000,
            },
            name: area(1000),
            number: area(12000),
            department: area(23000),
            title: area(34000),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::{io::Cursor, sync::Arc, time::Duration};

    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};

    use super::*;
    use crate::{
        access::{
            Entry,
            appointment::tests::register,
            grant::tests::admit,
            role::{Kind as RoleKind, tests::actor},
        },
        database::Barrier,
        identity::{
            authentication::{self, Database, Supplier, server},
            operation,
        },
        runtime::Context,
        startup::{Services, Settings as ServicesSettings},
        workforce::directory::{Creation, Item, Receipt},
    };

    fn operation() -> operation::Id {
        uuid::Uuid::new_v4().to_string().parse().expect("operation")
    }

    fn read_capacity() -> Arc<OwnedSemaphorePermit> {
        Arc::new(
            Arc::new(Semaphore::new(1))
                .try_acquire_owned()
                .expect("transfer capacity"),
        )
    }

    fn application_services<'a>(
        context: &'a Context,
        settings: ServicesSettings,
        supplier: &'a Supplier,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Services> + 'a>> {
        Box::pin(async move {
            Box::pin(Services::start(
                Barrier::begin(context.clone()).await.expect("barrier"),
                authentication::inventory(authentication::keys()),
                settings,
                supplier.client(),
                authentication::unused_client().await,
            ))
            .await
            .expect("services")
        })
    }

    fn verify_inactive_applicant<'a>(
        services: &'a Services,
        database: &'a Database,
        supplier: &'a mut Supplier,
        institution: &'a Actor,
        applicant: &'a Actor,
        reviewer: &'a Actor,
        id: Id,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Actor> + 'a>> {
        Box::pin(async move {
            let candidate = Box::pin(
                services
                    .account()
                    .candidate(institution, "13800000000".parse().expect("mobile")),
            )
            .await
            .expect("current account candidate");
            let subject = candidate.identity();
            let suspension = Box::pin(services.suspension().suspend(institution.clone(),operation(),applicant.account_id(),serde_json::from_value(json!({"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000000"})).expect("suspension input"))).await.expect("suspend applicant account");
            let detail = Box::pin(services.application().managed_detail(reviewer, id))
                .await
                .expect("ineligible account remains visible to approval authority");
            assert!(!detail.current.account_active);
            assert!(
                detail.current.participation_active,
                "account eligibility is independent of the retained active grant"
            );
            assert!(matches!(
                Box::pin(
                    services
                        .application()
                        .review(reviewer, id, ReviewInput::Approve {})
                )
                .await,
                Err(Error::ApplicantUnavailable)
            ));
            Box::pin(
                services.application().review(
                    reviewer,
                    id,
                    serde_json::from_value(
                        json!({"decision":"reject","reason":"Account suspended"}),
                    )
                    .expect("reason"),
                ),
            )
            .await
            .expect("ineligible account may still be rejected");
            Box::pin(services.application().resource(
                reviewer,
                detail.application.photo.resource_id,
                read_capacity(),
                false,
            ))
            .await
            .expect("manager can still read referenced media");
            Box::pin(services.suspension().resume(institution.clone(),operation(),applicant.account_id(),serde_json::from_value(json!({"personId":subject.person_id(),"revisionId":subject.revision_id(),"mobile":"13800000000","suspensionId":suspension.id()})).expect("resumption input"))).await.expect("resume applicant account");
            let message = Box::pin(authentication::session::tests::message(
                database,
                services.authentication(),
                supplier,
                "13800000000",
            ))
            .await;
            let authentication::session::Receipt::Session(session) =
                Box::pin(services.authentication().create_session(
                    authentication::session::tests::input(
                        uuid::Uuid::new_v4(),
                        message,
                        "13800000000",
                        authentication::session::tests::CODE,
                    ),
                ))
                .await
                .expect("new session after account resumption")
            else {
                panic!("existing account session")
            };
            services
                .current()
                .authenticate(current::Proof::Cookie {
                    id: session.id(),
                    token: session.token().text().parse().expect("session proof"),
                })
                .await
                .expect("resumed authenticated applicant")
        })
    }

    fn verify_reviews<'a>(
        services: &'a Services,
        database: &'a Database,
        supplier: &'a mut Supplier,
        institution: &'a Actor,
        applicant: (&'a Actor, grant::Id),
        ids: &'a [Id],
        proxy: &'a fixture::database_proxy::Proxy,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = i64> + 'a>> {
        Box::pin(async move {
            let (applicant, applicant_grant) = applicant;
            let service = services.application();
            let page = || serde_json::from_value(json!({})).expect("management page");
            assert!(matches!(
                Box::pin(service.managed_list(institution, page())).await,
                Err(Error::Grant(grant::Error::Denied))
            ));
            assert!(matches!(
                Box::pin(service.managed_detail(institution, ids[0])).await,
                Err(Error::Grant(grant::Error::Denied))
            ));
            assert!(matches!(
                Box::pin(service.review(institution, ids[0], ReviewInput::Approve {})).await,
                Err(Error::Grant(grant::Error::Denied))
            ));
            let reviewer = Box::pin(crate::access::appointment::tests::register_numbered(
                database, services, supplier, 21,
            ))
            .await;
            let grant = Box::pin(admit(
                services,
                institution,
                Entry::Institution,
                &crate::access::appointment::tests::mobile(21),
                RoleKind::Full,
            ))
            .await;
            let observer = database
                .connections(&database.user)
                .recover()
                .await
                .expect("review observer");
            let owner = database
                .connections(&database.owner)
                .recover()
                .await
                .expect("fault fixture owner");
            let listing = Box::pin(service.managed_list(&reviewer, page()))
                .await
                .expect("current approval authority");
            assert_eq!(listing.count, 6);
            let detail = Box::pin(service.managed_detail(&reviewer, ids[0]))
                .await
                .expect("fixed and current details");
            assert!(detail.current.account_active && detail.current.participation_active);
            assert_eq!(detail.current.name, "Target Person");
            let fixed = detail.application.snapshot.clone();
            let resource = detail.application.photo.resource_id;
            let bytes = Box::pin(service.resource(&reviewer, resource, read_capacity(), false))
                .await
                .expect("manager reads a referenced applicant photo")
                .bytes;
            assert!(!bytes.is_empty());
            let draft = Box::pin(service.prepare_photo(&reviewer, "image/webp".into(), bytes))
                .await
                .expect("another person's unreferenced draft");
            assert!(
                matches!(
                    Box::pin(service.resource(
                        applicant,
                        draft.resource_id,
                        read_capacity(),
                        false
                    ))
                    .await,
                    Err(Error::TargetUnavailable)
                ),
                "approval authority never opens an arbitrary foreign draft"
            );
            assert!(matches!(
                Box::pin(service.resource(applicant, draft.resource_id, read_capacity(), true))
                    .await,
                Err(Error::TargetUnavailable)
            ));
            let approved = Box::pin(service.review(&reviewer, ids[1], ReviewInput::Approve {}))
                .await
                .expect("approve without generating an artifact");
            assert!(matches!(approved.decision, Decision::Approved { .. }));
            let rejected = Box::pin(
                service.review(
                    &reviewer,
                    ids[2],
                    serde_json::from_value(
                        json!({"decision":"reject","reason":"  Photo needs correction  "}),
                    )
                    .expect("reason"),
                ),
            )
            .await
            .expect("reject with a reason");
            let rejection = serde_json::to_value(rejected).expect("public decision");
            assert_eq!(rejection["rejectionReason"], "Photo needs correction");
            assert_eq!(rejection["status"], "rejected");
            let (first, second) = tokio::join!(
                Box::pin(service.review(&reviewer, ids[0], ReviewInput::Approve {})),
                Box::pin(
                    service.review(
                        &reviewer,
                        ids[0],
                        serde_json::from_value(
                            json!({"decision":"reject","reason":"Competing decision"})
                        )
                        .expect("competing reason")
                    )
                ),
            );
            assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
            assert!(matches!(
                (&first, &second),
                (Ok(_), Err(Error::StateChanged)) | (Err(Error::StateChanged), Ok(_))
            ));
            assert_eq!(observer.client().query_one("SELECT count(*) FROM audit.events WHERE target_id=$1 AND action IN ('workforce.application.approved','workforce.application.rejected')", &[&ids[0]]).await.expect("one review audit").get::<_,i64>(0), 1);
            owner.client().batch_execute("CREATE FUNCTION audit.reject_review_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture review audit rejection'; END $$; CREATE TRIGGER reject_review_fixture BEFORE INSERT ON audit.events FOR EACH ROW EXECUTE FUNCTION audit.reject_review_fixture()").await.expect("audit failure injection");
            assert!(
                Box::pin(service.review(&reviewer, ids[3], ReviewInput::Approve {}))
                    .await
                    .is_err()
            );
            assert!(matches!(
                Box::pin(service.managed_detail(&reviewer, ids[3]))
                    .await
                    .expect("refresh rolled back review")
                    .application
                    .decision,
                Decision::Submitted
            ));
            owner.client().batch_execute("DROP TRIGGER reject_review_fixture ON audit.events; DROP FUNCTION audit.reject_review_fixture()").await.expect("remove fixture failure");
            // Configuration changes and missing current numbers are advisory, never snapshot edits.
            let department = Box::pin(services.directory().list(applicant, directory::Collection::Departments)).await.expect("current department").into_iter().find(|item|matches!(item,Item::Department(department) if department.id()==fixed.department.id)).expect("retained department");
            let Item::Department(department) = department else {
                panic!("department")
            };
            let mut replacement =
                serde_json::to_value(&department).expect("current department JSON");
            replacement["enabled"] = json!(false);
            let update = serde_json::from_value::<directory::Update<directory::department::Item>>(
                json!({"expected":department,"replacement":replacement}),
            )
            .expect("explicit disable")
            .into_items();
            Box::pin(services.directory().update(
                applicant.clone(),
                operation(),
                directory::Target::Department(fixed.department.id),
                update,
            ))
            .await
            .expect("disabled directory through its real consumer");
            let changed = Box::pin(service.managed_detail(&reviewer, ids[3]))
                .await
                .expect("disabled directory shown separately");
            assert!(!changed.current.department.enabled);
            assert_eq!(changed.application.snapshot, fixed);
            proxy.arm(fixture::database_proxy::Fault::LostCommit);
            match Box::pin(service.review(&reviewer, ids[3], ReviewInput::Approve {})).await {
                Err(error) => assert!(
                    matches!(error, Error::Unknown(_)),
                    "lost review commit: {error:?}"
                ),
                Ok(_) => panic!("review acknowledged despite an armed lost-commit fault"),
            }
            let backend = proxy.intercepted().await;
            let durable = Box::pin(service.managed_detail(&reviewer, ids[3]))
                .await
                .expect("GET refresh after lost response");
            assert!(matches!(
                durable.application.decision,
                Decision::Approved { .. }
            ));
            assert_eq!(observer.client().query_one("SELECT count(*) FROM audit.events WHERE target_id=$1 AND action='workforce.application.approved'", &[&ids[3]]).await.expect("no automatic repeat").get::<_,i64>(0), 1);
            proxy.release();
            timeout(Duration::from_secs(3), async {
                let mut cadence = tokio::time::interval(Duration::from_millis(10));
                loop {
                    cadence.tick().await;
                    if observer
                        .client()
                        .query_one(
                            "SELECT NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1)",
                            &[&backend],
                        )
                        .await
                        .expect("original backend")
                        .get::<_, bool>(0)
                    {
                        break;
                    }
                }
            })
            .await
            .expect("unknown write backend ended");
            let own = serde_json::to_value(
                Box::pin(service.list(applicant, Pagination::new(1, 20).expect("page")))
                    .await
                    .expect("full decided history"),
            )
            .expect("history JSON");
            assert!(
                own["applications"]
                    .as_array()
                    .expect("history rows")
                    .iter()
                    .filter(|row| row["status"] != "submitted")
                    .all(|row| row["reviewerName"] == "H*")
            );
            assert!(
                own["applications"]
                    .as_array()
                    .expect("history rows")
                    .iter()
                    .any(|row| row["status"] == "rejected"
                        && row["rejectionReason"] == "Photo needs correction")
            );
            for sql in [
                "UPDATE workforce.applications SET status='submitted',reviewer_id=NULL,reviewed_at=NULL,rejection_reason=NULL WHERE status='approved'",
                "UPDATE workforce.applications SET reviewer_id=NULL WHERE status='rejected'",
                "UPDATE workforce.applications SET submitted_at=submitted_at + interval '1 second' WHERE status='submitted'",
                "UPDATE workforce.applications SET status='rejected',reviewer_id=person_id,reviewed_at=statement_timestamp(),rejection_reason=' ' WHERE status='submitted'",
            ] {
                assert!(
                    owner.client().batch_execute(sql).await.is_err(),
                    "database review protection: {sql}"
                );
            }
            let resumed = verify_inactive_applicant(
                services,
                database,
                supplier,
                institution,
                applicant,
                &reviewer,
                ids[4],
            )
            .await;
            let applicant = &resumed;
            let remaining = ids[5];
            Box::pin(
                services
                    .grant()
                    .terminate(applicant.clone(), operation(), applicant_grant),
            )
            .await
            .expect("real own service termination");
            assert!(
                matches!(
                    Box::pin(service.review(&reviewer, remaining, ReviewInput::Approve {})).await,
                    Err(Error::ApplicantUnavailable)
                ),
                "ended participation prohibits approval"
            );
            Box::pin(service.managed_detail(&reviewer, remaining))
                .await
                .expect("manager still sees an ineligible applicant");
            Box::pin(
                service.review(
                    &reviewer,
                    remaining,
                    serde_json::from_value(
                        json!({"decision":"reject","reason":"Participation ended"}),
                    )
                    .expect("reason"),
                ),
            )
            .await
            .expect("manager may still reject");
            assert!(
                matches!(
                    Box::pin(service.detail(applicant, remaining)).await,
                    Err(Error::TargetUnavailable)
                ),
                "restricted history conceals rejected decisions and snapshots"
            );
            let restricted = serde_json::to_value(
                Box::pin(service.list(applicant, Pagination::new(1, 20).expect("page")))
                    .await
                    .expect("restricted history"),
            )
            .expect("projection");
            let count:i64=observer.client().query_one("SELECT count(*) FROM workforce.applications WHERE person_id=(SELECT person_id FROM identity.accounts WHERE id=$1) AND status IN ('submitted','approved')",&[&applicant.account_id()]).await.expect("independent restricted count").get(0);
            assert_eq!(restricted["count"], count);
            assert!(
                restricted["applications"]
                    .as_array()
                    .expect("restricted rows")
                    .iter()
                    .all(|row| row.as_object().expect("summary").len() == 4)
            );
            let expected_cancellable = if first.is_ok() { 3 } else { 2 };
            assert_eq!(
                restricted["count"], expected_cancellable,
                "fixed reviews, two ineligible rejections, and the race's actual winner determine the exact count"
            );
            proxy.arm(fixture::database_proxy::Fault::StalledCurrent);
            let pending_review = {
                let service = service.clone();
                let actor = reviewer.clone();
                let id = ids[1];
                tokio::spawn(
                    async move { service.review(&actor, id, ReviewInput::Approve {}).await },
                )
            };
            let waiting = proxy.intercepted().await;
            Box::pin(
                services
                    .grant()
                    .terminate(reviewer.clone(), operation(), grant.id()),
            )
            .await
            .expect("revoke approval participation");
            proxy.release();
            assert!(matches!(
                timeout(Duration::from_secs(3), pending_review)
                    .await
                    .expect("review ended after revocation")
                    .expect("review task ended"),
                Err(Error::Grant(grant::Error::Denied))
            ));
            assert!(observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND state='active')",&[&waiting]).await.expect("original request finished").get::<_,bool>(0));
            assert!(matches!(
                Box::pin(service.managed_detail(&reviewer, ids[1])).await,
                Err(Error::Grant(grant::Error::Denied))
            ));
            assert!(matches!(
                Box::pin(service.review(&reviewer, ids[1], ReviewInput::Approve {})).await,
                Err(Error::Grant(grant::Error::Denied))
            ));
            assert!(matches!(
                Box::pin(service.resource(&reviewer, resource, read_capacity(), false)).await,
                Err(Error::TargetUnavailable)
            ));
            let candidate = Box::pin(
                services.account().candidate(
                    institution,
                    crate::access::appointment::tests::mobile(21)
                        .parse()
                        .expect("reviewer mobile"),
                ),
            )
            .await
            .expect("reviewer account candidate");
            Box::pin(services.cancellation().cancel(reviewer.clone(),operation(),serde_json::from_value(json!({"accountId":reviewer.account_id(),"personId":candidate.identity().person_id(),"revisionId":candidate.identity().revision_id()})).expect("self cancellation input"))).await.expect("cancel reviewer account while retaining the person");
            Box::pin(admit(
                services,
                institution,
                Entry::Institution,
                "13800000000",
                RoleKind::Full,
            ))
            .await;
            let retained = serde_json::to_value(
                Box::pin(service.list(applicant, Pagination::new(1, 20).expect("page")))
                    .await
                    .expect("decided history after reviewer cancellation"),
            )
            .expect("history JSON");
            assert_eq!(retained["access"], "full");
            assert_eq!(retained["count"], 6);
            assert!(
                retained["applications"]
                    .as_array()
                    .expect("retained rows")
                    .iter()
                    .all(|row| row["reviewerName"] == "H*")
            );
            let retained_detail = serde_json::to_value(
                Box::pin(service.managed_detail(applicant, ids[1]))
                    .await
                    .expect("retained reviewer in management detail"),
            )
            .expect("detail JSON");
            assert_eq!(retained_detail["application"]["reviewerName"], "H*");
            owner.terminate().await.expect("owner ended");
            observer.terminate().await.expect("observer ended");
            expected_cancellable
        })
    }

    async fn cancelled_resource_read(service: &Service, actor: &Actor, id: ResourceId) {
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let gate = tokio::task::spawn_blocking(move || {
            entered.send(()).expect("blocking worker observed");
            released
                .recv_timeout(Duration::from_secs(15))
                .expect("release blocking worker");
        });
        started.await.expect("single blocking worker held");
        let transfers = Arc::new(Semaphore::new(1));
        let capacity = Arc::new(
            Arc::clone(&transfers)
                .try_acquire_owned()
                .expect("connection transfer"),
        );
        let reading = {
            let service = service.clone();
            let actor = actor.clone();
            let capacity = Arc::clone(&capacity);
            tokio::spawn(async move { service.resource(&actor, id, capacity, false).await })
        };
        timeout(Duration::from_secs(5), async {
            while service.processing.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("file read queued with its real capacities");
        reading.abort();
        let Err(error) = reading.await else {
            panic!("caller exited");
        };
        assert!(error.is_cancelled());
        drop(capacity);
        assert_eq!(
            transfers.available_permits(),
            0,
            "a disconnected caller cannot release an unfinished file read"
        );
        release.send(()).expect("allow actual read");
        gate.await.expect("blocking gate ended");
        timeout(Duration::from_secs(5), async {
            while transfers.available_permits() != 1 || service.processing.available_permits() != 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual read releases transfer capacity");
        assert_eq!(service.processing.available_permits(), 2);
    }

    fn verify_private_photo<'a>(
        service: &'a Service,
        manager: &'a Actor,
        institution: &'a Actor,
        photo: &'a Photo,
        observer: &'a Client,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
        Box::pin(async move {
            assert!(matches!(
                service
                    .resource(institution, photo.resource_id, read_capacity(), false)
                    .await,
                Err(Error::TargetUnavailable)
            ));
            let original_file = service
                .resource(manager, photo.resource_id, read_capacity(), false)
                .await
                .expect("original private photo")
                .bytes;
            let processing = Arc::clone(&service.processing)
                .try_acquire_many_owned(2)
                .expect("occupy actual processing capacity");
            let metadata = service
                .resource(manager, photo.resource_id, read_capacity(), true)
                .await
                .expect("HEAD avoids occupied processing slots");
            assert!(metadata.bytes.is_empty());
            assert_eq!(
                metadata.size,
                u64::try_from(original_file.len()).expect("resource size")
            );
            assert_eq!(metadata.media_type, "image/webp");
            assert!(matches!(
                service
                    .resource(institution, photo.resource_id, read_capacity(), true)
                    .await,
                Err(Error::TargetUnavailable)
            ));
            drop(processing);
            let private = &service.settings().expect("configured storage").directory;
            let person: person::Id = observer
                .query_one(
                    "SELECT person_id FROM workforce.photos WHERE id=$1",
                    &[&photo.photo_id],
                )
                .await
                .expect("photo owner")
                .get(0);
            let person_path = person.to_string();
            let segments = ["workforce", "photos", &person_path];
            assert!(
                private
                    .publish(
                        &segments,
                        &photo.resource_id.to_string(),
                        &uuid::Uuid::new_v4().to_string(),
                        b"replacement bytes"
                    )
                    .is_err(),
                "a retained resource cannot be overwritten"
            );
            assert_eq!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), false)
                    .await
                    .expect("unchanged photo")
                    .bytes,
                original_file
            );
            assert!(
                matches!(
                    service
                        .prepare_photo(manager, "image/webp".to_owned(), original_file.clone())
                        .await,
                    Err(Error::PhotoCapacity)
                ),
                "new preparation stops at the retained-file limit"
            );
            assert_eq!(
                observer
                    .query_one("SELECT count(*) FROM workforce.photos", &[])
                    .await
                    .expect("no rejected photo metadata")
                    .get::<_, i64>(0),
                1
            );
            assert_eq!(
                observer
                    .query_one(
                        "SELECT count(*) FROM audit.events WHERE action='workforce.photo.prepared'",
                        &[]
                    )
                    .await
                    .expect("no rejected photo audit")
                    .get::<_, i64>(0),
                1
            );
            private
                .remove(&segments, &photo.resource_id.to_string())
                .expect("isolated missing-file injection");
            assert!(matches!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), false)
                    .await,
                Err(Error::MissingFile)
            ));
            assert!(matches!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), true)
                    .await,
                Err(Error::MissingFile)
            ));
            private
                .publish(
                    &segments,
                    &photo.resource_id.to_string(),
                    &uuid::Uuid::new_v4().to_string(),
                    b"wrong length",
                )
                .expect("isolated size mismatch");
            assert!(matches!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), true)
                    .await,
                Err(Error::File(_))
            ));
            private
                .remove(&segments, &photo.resource_id.to_string())
                .expect("remove malformed fixture");
            private
                .publish(
                    &segments,
                    &photo.resource_id.to_string(),
                    &uuid::Uuid::new_v4().to_string(),
                    &original_file,
                )
                .expect("restore retained fixture bytes");
        })
    }

    fn verify_submission_outcomes<'a>(
        services: &'a Services,
        manager: (&'a Actor, grant::Id),
        input: Input,
        photo: &'a Photo,
        proxy: &'a fixture::database_proxy::Proxy,
        clients: (&'a Client, &'a Client),
        first: Id,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
        Box::pin(async move {
            let (manager, grant) = manager;
            let (owner, observer) = clients;
            let service = services.application();
            let Input {
                kind,
                selections,
                expected,
            } = input;
            let Detail::Full { application } = service
                .detail(manager, first)
                .await
                .expect("retained snapshot")
            else {
                panic!("active detail")
            };
            assert_eq!(
                application.snapshot, expected,
                "configuration edits cannot rewrite an accepted print snapshot"
            );
            owner.batch_execute("CREATE FUNCTION audit.reject_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture audit rejection'; END $$; CREATE TRIGGER reject_fixture BEFORE INSERT ON audit.events FOR EACH ROW EXECUTE FUNCTION audit.reject_fixture()").await.expect("atomic rollback injection");
            assert!(
                service
                    .submit(
                        manager,
                        Input {
                            kind,
                            selections: selections.clone(),
                            expected: expected.clone()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                observer
                    .query_one("SELECT count(*) FROM workforce.applications", &[])
                    .await
                    .expect("application rolled back with audit")
                    .get::<_, i64>(0),
                5
            );
            owner.batch_execute("DROP TRIGGER reject_fixture ON audit.events; DROP FUNCTION audit.reject_fixture()").await.expect("remove fixture fault");
            proxy.arm(fixture::database_proxy::Fault::LostCommit);
            assert!(
                matches!(
                    service
                        .submit(
                            manager,
                            Input {
                                kind,
                                selections: selections.clone(),
                                expected: expected.clone()
                            }
                        )
                        .await,
                    Err(Error::Unknown(_))
                ),
                "a lost commit reply remains unknown"
            );
            let original_backend = proxy.intercepted().await;
            observer
                .batch_execute("BEGIN")
                .await
                .expect("independent outcome observation");
            observer.query_one("SELECT id FROM identity.persons WHERE id=(SELECT person_id FROM identity.accounts WHERE id=$1) FOR UPDATE",&[&manager.account_id()]).await.expect("original transaction released its participant");
            assert_eq!(
                observer
                    .query_one("SELECT count(*) FROM workforce.applications", &[])
                    .await
                    .expect("one durable result")
                    .get::<_, i64>(0),
                6
            );
            observer
                .batch_execute("COMMIT")
                .await
                .expect("observation ended");
            proxy.release();
            assert!(matches!(
                service
                    .list(manager, Pagination::new(1, 20).expect("page"))
                    .await
                    .expect("refresh after unknown"),
                List::Full { count: 6, .. }
            ));
            assert!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), false)
                    .await
                    .is_ok(),
                "unknown commit never deletes a referenced file"
            );
            timeout(Duration::from_secs(3),async {
                let mut cadence=tokio::time::interval(Duration::from_millis(10));
                loop {cadence.tick().await;if observer.query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&original_backend]).await.expect("original backend state").get::<_,bool>(0){break;}}
            }).await.expect("original backend ended before fixture cleanup");
            cancelled_resource_read(service, manager, photo.resource_id).await;
            Box::pin(
                services
                    .grant()
                    .terminate(manager.clone(), operation(), grant),
            )
            .await
            .expect("real own service termination");
            let restricted = serde_json::to_value(
                service
                    .list(manager, Pagination::new(1, 20).expect("page"))
                    .await
                    .expect("restricted history"),
            )
            .expect("projection");
            assert_eq!(restricted["access"], "restricted");
            assert_eq!(restricted["count"], 6);
            assert!(
                restricted["applications"]
                    .as_array()
                    .expect("summaries")
                    .iter()
                    .all(|item| item.as_object().expect("summary").len() == 4)
            );
            assert!(matches!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), false)
                    .await,
                Err(Error::TargetUnavailable)
            ));
            assert!(matches!(
                service
                    .resource(manager, photo.resource_id, read_capacity(), true)
                    .await,
                Err(Error::TargetUnavailable)
            ));
            assert!(matches!(
                service
                    .submit(
                        manager,
                        Input {
                            kind,
                            selections,
                            expected
                        }
                    )
                    .await,
                Err(Error::Denied)
            ));
        })
    }

    async fn create(services: &Services, actor: &Actor, creation: Creation) -> Item {
        let Receipt::Present(item) = services
            .directory()
            .create(actor.clone(), operation(), creation.collection(), creation)
            .await
            .expect("real directory creation")
        else {
            panic!("created directory")
        };
        item
    }

    async fn directory_waiter(
        client: &Client,
        user: &str,
        gate: i32,
        previous: Option<i32>,
    ) -> i32 {
        timeout(Duration::from_secs(3), async {
            let mut cadence = tokio::time::interval(Duration::from_millis(10));
            loop {
                cadence.tick().await;
                if let Some(row) = client.query_opt(
                    "SELECT pid FROM pg_catalog.pg_stat_activity WHERE datname=current_database() AND usename=$1 AND wait_event_type='Lock' AND query='SELECT workforce.lock_directory()' AND $2=ANY(pg_catalog.pg_blocking_pids(pid)) AND ($3::int IS NULL OR pid<>$3)",
                    &[&user, &gate, &previous],
                ).await.expect("specific directory blocker") {
                    break row.get(0);
                }
            }
        }).await.expect("writer waits for the controlled directory gate")
    }

    async fn verify_directory_conflicts(
        services: &Services,
        manager: &Actor,
        department: &directory::department::Item,
        entries: &[directory::roster::Entry],
        selections: &Selections,
        snapshot: &Snapshot,
        database: &Database,
    ) {
        let owner = database
            .connections(&database.owner)
            .recover()
            .await
            .expect("directory gate owner");
        let observer = database
            .connections(&database.user)
            .recover()
            .await
            .expect("directory observer");
        let person: person::Id = observer
            .client()
            .query_one(
                "SELECT person_id FROM identity.accounts WHERE id=$1",
                &[&manager.account_id()],
            )
            .await
            .expect("photo owner")
            .get(0);
        let other: person::Id = observer
            .client()
            .query_one(
                "SELECT id FROM identity.persons WHERE id<>$1 LIMIT 1",
                &[&person],
            )
            .await
            .expect("different fixture person")
            .get(0);
        let Item::Department(other_department)=Box::pin(create(services,manager,Creation::Department(serde_json::from_value(json!({"name":"Other department","enabled":true,"index":0,"groupId":department.group_id(),"parentId":null})).expect("different actual department")))).await else {panic!("department")};
        let original = serde_json::to_value(snapshot).expect("trusted snapshot");
        let mut wrong_role = original.clone();
        wrong_role["template"]["backgroundId"] = wrong_role["template"]["fontId"].clone();
        let mut wrong_department = original.clone();
        wrong_department["department"]["id"] = json!(other_department.id());
        for (person, snapshot, constraint) in [
            (
                other,
                original.clone(),
                "applications_photo_id_person_id_fkey",
            ),
            (
                person,
                wrong_role,
                "applications_background_id_background_kind_fkey",
            ),
            (
                person,
                wrong_department,
                "applications_roster_department_fkey",
            ),
        ] {
            let error=observer.client().execute("INSERT INTO workforce.applications (id,person_id,kind,snapshot) VALUES ($1,$2,'issuance',$3::text::jsonb)",&[&uuid::Uuid::new_v4(),&person,&snapshot.to_string()]).await.expect_err("runtime cannot bypass a snapshot reference");
            assert_eq!(
                error.code(),
                Some(&tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION)
            );
            assert_eq!(
                error.as_db_error().expect("constraint").constraint(),
                Some(constraint)
            );
        }
        let mut invalid_number = original;
        invalid_number["number"]["serial"] = json!(0);
        let error=observer.client().execute("INSERT INTO workforce.applications (id,person_id,kind,snapshot) VALUES ($1,$2,'issuance',$3::text::jsonb)",&[&uuid::Uuid::new_v4(),&person,&invalid_number.to_string()]).await.expect_err("nested number rules are database protected");
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::CHECK_VIOLATION)
        );
        let gate: i32 = owner
            .client()
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .expect("gate backend")
            .get(0);
        assert!(
            matches!(
                Box::pin(
                    services.directory().replace_roster(
                        manager.clone(),
                        operation(),
                        department.id(),
                        serde_json::from_value(json!({"expected":entries,"replacement":[]}))
                            .expect("remove used roster")
                    )
                )
                .await,
                Err(crate::kernel::Error::Rejected(directory::Rejection::InUse))
            ),
            "a snapshot's configured entry cannot disappear"
        );
        // The actual selected subtype rule wins the controlled race before submission.
        owner
            .client()
            .batch_execute("BEGIN; SELECT workforce.lock_directory()")
            .await
            .expect("rule gate");
        let rule_writer = {
            let services = services.clone();
            let manager = manager.clone();
            let department_id = department.id();
            tokio::spawn(async move {
                Box::pin(services.directory().configure_rule(manager,operation(),number::Subtype::ALL[0],serde_json::from_value(json!({"expected":{"id":100,"departmentId":department_id,"position":true},"replacement":{"id":100,"departmentId":department_id,"position":false}})).expect("selected rule disabled"))).await
            })
        };
        let first = directory_waiter(observer.client(), &database.user, gate, None).await;
        let submitter = {
            let service = services.application().clone();
            let manager = manager.clone();
            let selections = selections.clone();
            let expected = snapshot.clone();
            tokio::spawn(async move {
                service
                    .submit(
                        &manager,
                        Input {
                            kind: Kind::Issuance,
                            selections,
                            expected,
                        },
                    )
                    .await
            })
        };
        directory_waiter(observer.client(), &database.user, gate, Some(first)).await;
        owner
            .client()
            .batch_execute("COMMIT")
            .await
            .expect("rule wins");
        rule_writer
            .await
            .expect("rule writer ended")
            .expect("rule changed");
        assert!(
            matches!(
                submitter.await.expect("submitter ended"),
                Err(Error::SelectionUnavailable)
            ),
            "no application accepts a newly forbidden selected position"
        );
        assert_eq!(observer.client().query_one("SELECT count(*) FROM audit.events WHERE action='workforce.application.submitted'",&[]).await.expect("rejected race has no audit").get::<_,i64>(0),5);
        Box::pin(services.directory().configure_rule(manager.clone(),operation(),number::Subtype::ALL[0],serde_json::from_value(json!({"expected":{"id":100,"departmentId":department.id(),"position":false},"replacement":{"id":100,"departmentId":department.id(),"position":true}})).expect("restore fixture rule"))).await.expect("rule restored");
        let mut disabled = serde_json::to_value(department).expect("directory value");
        disabled["enabled"] = json!(false);
        let replacement = serde_json::from_value::<directory::Update<directory::department::Item>>(
            json!({"expected":department,"replacement":disabled}),
        )
        .expect("disable selected department")
        .into_items();
        Box::pin(services.directory().update(
            manager.clone(),
            operation(),
            directory::Target::Department(department.id()),
            replacement,
        ))
        .await
        .expect("department disabled");
        assert!(matches!(
            services
                .application()
                .submit(
                    manager,
                    Input {
                        kind: Kind::Issuance,
                        selections: selections.clone(),
                        expected: snapshot.clone()
                    }
                )
                .await,
            Err(Error::SelectionUnavailable)
        ));
        let replacement = serde_json::from_value::<directory::Update<directory::department::Item>>(
            json!({"expected":disabled,"replacement":department}),
        )
        .expect("restore selected department")
        .into_items();
        Box::pin(services.directory().update(
            manager.clone(),
            operation(),
            directory::Target::Department(department.id()),
            replacement,
        ))
        .await
        .expect("department enabled");
        owner.terminate().await.expect("gate owner stopped");
        observer
            .terminate()
            .await
            .expect("directory observer stopped");
    }

    #[test]
    fn cancelled_photo_wait_retains_its_budget_until_physical_publication_finishes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(60), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (root, mount, device) = server.files();
                let storage = platform::files::Settings::new(root, mount, device).expect("storage");
                let mut settings = fixture_settings(storage);
                settings.photo_limits = photo::Limits::new(1, 1 << 20, 1, 1 << 20).expect("capacity");
                let context = Context::new(database.connections(&database.user)).await.expect("context");
                let services = Box::pin(Services::start(
                    Barrier::begin(context.clone()).await.expect("barrier"),
                    authentication::inventory(authentication::keys()),
                    ServicesSettings::new(4, crate::identity::message::Limits::default(), authentication::session_settings(), crate::registration::Limits::default(), authentication::avatar_settings()).expect("settings").with_application(settings),
                    supplier.client(), authentication::unused_client().await,
                )).await.expect("services");
                let institution = actor(&database, &services, &mut supplier).await;
                let manager = Box::pin(register(&database, &services, &mut supplier)).await;
                Box::pin(admit(&services, &institution, Entry::Institution, "13800000000", RoleKind::Full)).await;
                let service = services.application();
                let held = service.settings().expect("settings").directory.exclusive(&["workforce", "photos"]).expect("pause actual publication");
                let mut image = Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(4, 5).write_to(&mut image, image::ImageFormat::Png).expect("image");
                let image = image.into_inner();
                let preparing = {
                    let service = service.clone();
                    let manager = manager.clone();
                    let image = image.clone();
                    tokio::spawn(async move { service.prepare_photo(&manager, "image/png".into(), image).await })
                };
                timeout(Duration::from_secs(5), async {
                    while service.processing.available_permits() != 1 { tokio::task::yield_now().await; }
                }).await.expect("worker owns the permit before disconnect");
                preparing.abort();
                let Err(error) = preparing.await else { panic!("caller exited"); };
                assert!(error.is_cancelled());
                assert_eq!(service.processing.available_permits(), 1, "caller exit cannot release the worker's permit");
                drop(held);
                timeout(Duration::from_secs(5), async {
                    while service.processing.available_permits() != 2 { tokio::task::yield_now().await; }
                }).await.expect("actual worker ended");
                let held = service.settings().expect("settings").directory.exclusive(&["workforce", "photos"]).expect("recount");
                assert_eq!(held.usage(&[]).expect("orphan counted").files, 1);
                drop(held);
                assert!(matches!(service.prepare_photo(&manager, "image/png".into(), image).await, Err(Error::PhotoCapacity)));
                let observer = database.connections(&database.user).recover().await.expect("observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.photos", &[]).await.expect("no accepted metadata").get::<_, i64>(0), 0);
                assert_eq!(observer.client().query_one("SELECT count(*) FROM audit.events WHERE action='workforce.photo.prepared'", &[]).await.expect("no success audit").get::<_, i64>(0), 0);
                observer.terminate().await.expect("observer ended");
                services.close();
                services.drain().await.expect("services drained");
                context.stop().await.expect("services stopped");
                supplier.stop().await;
            })).await.expect("bounded cancelled preparation");
        }));
    }

    #[test]
    fn fixed_submission_self_history_atomic_audit_and_directory_protection() {
        let server = server();
        // Queue a real resource read behind one held blocking worker for cancellation coverage.
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).max_blocking_threads(1).enable_all().build().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (root,mount,device) = server.files();
                let storage = platform::files::Settings::new(root,mount,device).expect("fixture storage");
                let mut background = Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(85,54).write_to(&mut background,image::ImageFormat::Png).expect("background");
                let mut template = fixture_template();
                template.font_id = ResourceId::allocate();
                let settings = Settings::new(storage,template,"image/png",background.into_inner(),include_bytes!("../../../tests/fixtures/fonts/NotoSansSC-Regular.otf").to_vec(),vec!["Fixture reception".to_owned()], photo::Limits::new(1, 1 << 30, 2, 2 << 30).expect("fixture capacity")).expect("CJK fixture");
                let proxy=fixture::database_proxy::Proxy::start(&server).await;
                let context = Context::new(authentication::proxied(&database,&proxy)).await.expect("context");
                let services = application_services(&context,ServicesSettings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings").with_application(settings),&supplier).await;
                let institution = actor(&database,&services,&mut supplier).await;
                let manager = Box::pin(register(&database,&services,&mut supplier)).await;
                let grant = Box::pin(admit(&services,&institution,Entry::Institution,"13800000000",RoleKind::Full)).await;
                Box::pin(services.number().allocate(manager.clone(),operation(),serde_json::from_value(json!({"subtype":100})).expect("number input"))).await.expect("first number");
                let Item::Group(group) = Box::pin(create(&services,&manager,Creation::Group(serde_json::from_value(json!({"name":"Group","enabled":true,"index":0})).expect("group")))).await else { panic!("group") };
                let Item::Department(department) = Box::pin(create(&services,&manager,Creation::Department(serde_json::from_value(json!({"name":"Department","enabled":true,"index":0,"groupId":group.id(),"parentId":null})).expect("department")))).await else { panic!("department") };
                let Item::Position(position) = Box::pin(create(&services,&manager,Creation::Position(serde_json::from_value(json!({"name":"Lead","enabled":true,"index":0})).expect("position")))).await else {panic!("position")};
                let roster = Box::pin(services.directory().replace_roster(manager.clone(),operation(),department.id(),serde_json::from_value(json!({"expected":[],"replacement":[{"positionId":position.id(),"enabled":true,"index":0}]})).expect("department position"))).await.expect("configured roster");
                let directory::roster::Receipt::Present(entries)=roster else {panic!("present department")};
                let mut image = Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(120,160).write_to(&mut image,image::ImageFormat::Png).expect("confirmed photograph");
                let photo = services.application().prepare_photo(&manager,"image/png".to_owned(),image.into_inner()).await.expect("prepared photo");
                let selections = Selections { department_id: department.id(), roster_id:Some(entries[0].id()),title_id:None,photo_id:photo.photo_id,pickup:"Fixture reception".to_owned() };
                assert!(matches!(services.application().preview(&manager,PreviewInput {kind:Kind::Issuance,selections:selections.clone()}).await,Err(Error::Configuration)),"all sixteen rules are necessary");
                for subtype in number::Subtype::ALL {
                    Box::pin(services.directory().configure_rule(manager.clone(),operation(),subtype,serde_json::from_value(json!({"departmentId":department.id(),"position":true})).expect("explicit rule"))).await.expect("real rule");
                }
                let preview = services.application().preview(&manager,PreviewInput {kind:Kind::Issuance,selections:selections.clone()}).await.expect("trusted preview");
                assert_eq!(preview.snapshot.name,"Target Person");
                assert_eq!(preview.snapshot.printed_title,"无");
                let mut forged = preview.snapshot.clone(); forged.name="Forged name".into();
                assert!(matches!(services.application().submit(&manager,Input {kind:Kind::Issuance,selections:selections.clone(),expected:forged}).await,Err(Error::Changed)));
                let observer = database.connections(&database.user).recover().await.expect("runtime observer");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.applications",&[]).await.expect("no mismatch write").get::<_,i64>(0),0);
                let mut ids=Vec::new();
                for kind in [Kind::Issuance,Kind::Duplication,Kind::Replacement,Kind::Issuance] {
                    let application=services.application().submit(&manager,Input {kind,selections:selections.clone(),expected:preview.snapshot.clone()}).await.expect("atomic application");
                    ids.push(application.id);
                    assert_eq!(application.snapshot,preview.snapshot);
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM audit.events WHERE action='workforce.application.submitted'",&[]).await.expect("audited submissions").get::<_,i64>(0),4);
                let List::Full {count,applications} = services.application().list(&manager,Pagination::new(1,20).expect("page")).await.expect("history") else {panic!("active full history")};
                assert_eq!(count,4); assert_eq!(applications.len(),4);
                assert!(matches!(services.application().detail(&institution,ids[0]).await,Err(Error::TargetUnavailable)));
                verify_private_photo(services.application(),&manager,&institution,&photo,observer.client()).await;
                let delete = serde_json::from_value::<directory::Delete<directory::department::Item>>(json!({"expected":department})).expect("confirmed department deletion").into_items();
                assert!(matches!(Box::pin(services.directory().delete(manager.clone(),operation(),directory::Target::Department(department.id()),delete)).await,Err(crate::kernel::Error::Rejected(directory::Rejection::InUse))));
                // A real runtime identity cannot rewrite snapshots, delete audit, or cross-bind a photo.
                for sql in ["UPDATE workforce.applications SET snapshot='{}'::jsonb", "DELETE FROM audit.events", "DELETE FROM workforce.resources"] {
                    assert!(observer.client().batch_execute(sql).await.is_err(),"append-only runtime: {sql}");
                }
                let owner = database.connections(&database.owner).recover().await.expect("isolated fault owner");
                let gate: i32 = owner.client().query_one("SELECT pg_backend_pid()",&[]).await.expect("gate backend").get(0);
                owner.client().batch_execute("BEGIN; SELECT workforce.lock_directory()").await.expect("controlled directory gate");
                let directory_writer={let services=services.clone();let manager=manager.clone();tokio::spawn(async move {
                    services.directory().create(manager,operation(),directory::Collection::Groups,Creation::Group(serde_json::from_value(json!({"name":"Concurrent directory","enabled":true,"index":0})).expect("group input"))).await
                })};
                let first = directory_waiter(observer.client(),&database.user,gate,None).await;
                let submitter={let service=services.application().clone();let manager=manager.clone();let selections=selections.clone();let expected=preview.snapshot.clone();tokio::spawn(async move {
                    service.submit(&manager,Input {kind:Kind::Issuance,selections,expected}).await
                })};
                let second = directory_waiter(observer.client(),&database.user,gate,Some(first)).await;
                assert_ne!(first,second,"independent application and directory backends");
                owner.client().batch_execute("COMMIT").await.expect("release directory gate");
                let (directory_result,submit_result)=tokio::join!(directory_writer,submitter);
                directory_result.expect("directory task ended").expect("directory maintenance does not deadlock with its own application");
                submit_result.expect("submit task ended").expect("both serialized outcomes remain available");
                Box::pin(verify_directory_conflicts(&services,&manager,&department,&entries,&selections,&preview.snapshot,&database)).await;
                verify_submission_outcomes(&services,(&manager,grant.id()),Input {kind:Kind::Issuance,selections,expected:preview.snapshot},&photo,&proxy,(owner.client(),observer.client()),ids[0]).await;
                owner.terminate().await.expect("owner stopped"); observer.terminate().await.expect("observer stopped");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("context stopped"); proxy.stop().await; supplier.stop().await;
            })).await.expect("bounded application acceptance");
        }));
    }

    #[test]
    fn reviews_preserve_current_authority_atomic_decisions_privacy_and_unknown_outcomes() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(90), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (root, mount, device) = server.files();
                let storage = platform::files::Settings::new(root, mount, device).expect("storage");
                let mut background = Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(85,54).write_to(&mut background,image::ImageFormat::Png).expect("background");
                let settings = Settings::new(storage, fixture_template(), "image/png", background.into_inner(), include_bytes!("../../../tests/fixtures/fonts/NotoSansSC-Regular.otf").to_vec(), vec!["Fixture reception".to_owned()], photo::Limits::new(1,1 << 30,2,2 << 30).expect("limits")).expect("settings");
                let proxy = fixture::database_proxy::Proxy::start(&server).await;
                let context = Context::new(authentication::proxied(&database,&proxy)).await.expect("context");
                let services = application_services(&context,ServicesSettings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("services settings").with_application(settings),&supplier).await;
                let institution = Box::pin(actor(&database,&services,&mut supplier)).await;
                let applicant = Box::pin(register(&database,&services,&mut supplier)).await;
                let grant = Box::pin(admit(&services,&institution,Entry::Institution,"13800000000",RoleKind::Full)).await;
                Box::pin(services.number().allocate(applicant.clone(),operation(),serde_json::from_value(json!({"subtype":100})).expect("number"))).await.expect("number allocation");
                let Item::Group(group) = Box::pin(create(&services,&applicant,Creation::Group(serde_json::from_value(json!({"name":"Group","enabled":true,"index":0})).expect("group")))).await else { panic!("group") };
                let Item::Department(department) = Box::pin(create(&services,&applicant,Creation::Department(serde_json::from_value(json!({"name":"Department","enabled":true,"index":0,"groupId":group.id(),"parentId":null})).expect("department")))).await else { panic!("department") };
                for subtype in number::Subtype::ALL {
                    Box::pin(services.directory().configure_rule(applicant.clone(),operation(),subtype,serde_json::from_value(json!({"departmentId":department.id(),"position":false})).expect("rule"))).await.expect("complete rules");
                }
                let mut image = Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(120,160).write_to(&mut image,image::ImageFormat::Png).expect("photo");
                let photo = Box::pin(services.application().prepare_photo(&applicant,"image/png".to_owned(),image.into_inner())).await.expect("photo preparation");
                let selections = Selections { department_id:department.id(),roster_id:None,title_id:None,photo_id:photo.photo_id,pickup:"Fixture reception".to_owned() };
                let preview = Box::pin(services.application().preview(&applicant,PreviewInput {kind:Kind::Issuance,selections:selections.clone()})).await.expect("preview");
                let mut ids = Vec::new();
                for _ in 0..6 {
                    let application = Box::pin(services.application().submit(&applicant,Input {kind:Kind::Issuance,selections:selections.clone(),expected:preview.snapshot.clone()})).await.expect("submitted fixture");
                    ids.push(application.id);
                }
                verify_reviews(&services,&database,&mut supplier,&institution,(&applicant,grant.id()),&ids,&proxy).await;
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("context stopped"); proxy.stop().await; supplier.stop().await;
            })).await.expect("bounded review acceptance");
        }));
    }
}
