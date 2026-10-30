//! Confirmed fixed badge applications: ordinary transactions and current self-state refresh.

mod database;
pub mod photo;
mod settings;
mod value;

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use tokio::{sync::Semaphore, time::timeout};
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
    settings: Option<Arc<Settings>>,
    // The owned permit moves into the blocking closure, including after a caller disconnects.
    processing: Arc<Semaphore>,
}

impl Service {
    pub(crate) fn prepare(
        app: App,
        current: current::Service,
        numbers: number::Service,
        settings: Option<Arc<Settings>>,
    ) -> Self {
        Self {
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
        self.require_active(actor).await?;
        let permit = Arc::clone(&self.processing)
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let resource_id = ResourceId::allocate();
        let photo = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let photo = photo::prepare(&media_type, &bytes)?;
            settings::publish(&settings.directory, resource_id, &photo.bytes)?;
            Ok::<_, Error>(photo)
        })
        .await
        .map_err(|_| Error::Worker)??;
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let result = async {
            transaction::begin_current(self.app.context(), client).await?;
            self.lock(client, actor).await?;
            let (person, _, active, _) = self.identity(client, actor, Read::Shared).await?;
            if !active {
                return Err(Error::Denied);
            }
            let id = PhotoId::allocate();
            database::resource(
                client,
                resource_id,
                "photo",
                "image/webp",
                i64::try_from(photo.bytes.len()).map_err(|_| Error::Configuration)?,
            )
            .await?;
            database::photo(client, id, person, resource_id, photo.width, photo.height).await?;
            Ok(Photo {
                photo_id: id,
                resource_id,
                width: photo.width,
                height: photo.height,
                media_type: "image/webp",
            })
        }
        .await;
        finish(self, lease, result, true).await
    }

    async fn require_active(&self, actor: &Actor) -> Result<(), Error> {
        let lease = self.app.context().connections().lease().await?;
        let client = lease.client();
        let budget = self.app.context().read_budget();
        let result = timeout(
            Duration::from_secs(3),
            budget.run(client, self.app.anchors(), |view| async move {
                let (_, _, active, _) = self.identity(client, actor, Read::Snapshot(view)).await?;
                if !active {
                    return Err(Error::Denied);
                }
                Ok(())
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
                let (person, _, active, _) =
                    self.identity(client, actor, Read::Snapshot(view)).await?;
                database::list(client, person, active, pagination).await
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
                let (person, _, active, _) =
                    self.identity(client, actor, Read::Snapshot(view)).await?;
                database::detail(client, person, active, id).await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        finish(self, lease, result, false).await
    }

    pub async fn resource(&self, actor: &Actor, id: ResourceId) -> Result<Resource, Error> {
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
                database::readable_resource(client, person, id, &self.settings()?.template).await
            }),
        )
        .await
        .map_err(|_| Error::Timeout)?;
        let (size, media_type) = finish(self, lease, result, false).await?;
        let permit = Arc::clone(&self.processing)
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let bytes = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            settings
                .directory
                .open(&["workforce"], &id.to_string())?
                .ok_or(Error::MissingFile)?
                .read(size)
                .map_err(Error::from)
        })
        .await
        .map_err(|_| Error::Worker)??;
        Ok(Resource { bytes, media_type })
    }
}

pub struct Resource {
    pub bytes: Vec<u8>,
    pub media_type: String,
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
        let mut disabled = serde_json::to_value(&department).expect("directory value");
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
                    &manager,
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
    fn fixed_submission_self_history_atomic_audit_and_directory_protection() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            timeout(Duration::from_secs(120), Box::pin(async {
                let database = Database::initialized(Arc::clone(&server)).await;
                let mut supplier = Supplier::new().await;
                let (root,mount,device) = server.files();
                let storage = platform::files::Settings::new(root,mount,device).expect("fixture storage");
                let mut background = Cursor::new(Vec::new());
                image::DynamicImage::new_rgb8(85,54).write_to(&mut background,image::ImageFormat::Png).expect("background");
                let mut template = fixture_template();
                template.font_id = ResourceId::allocate();
                let settings = Settings::new(storage,template,"image/png",background.into_inner(),include_bytes!("../../../tests/fixtures/fonts/NotoSansSC-Regular.otf").to_vec(),vec!["Fixture reception".to_owned()]).expect("CJK fixture");
                let proxy=fixture::database_proxy::Proxy::start(&server).await;
                let context = Context::new(authentication::proxied(&database,&proxy)).await.expect("context");
                let services = Box::pin(Services::start(Barrier::begin(context.clone()).await.expect("barrier"),authentication::inventory(authentication::keys()),ServicesSettings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings").with_application(settings),supplier.client(),authentication::unused_client().await)).await.expect("services");
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
                    ids.push(application.summary.id);
                    assert_eq!(application.snapshot,preview.snapshot);
                }
                assert_eq!(observer.client().query_one("SELECT count(*) FROM audit.events WHERE action='workforce.application.submitted'",&[]).await.expect("audited submissions").get::<_,i64>(0),4);
                let List::Full {count,applications} = services.application().list(&manager,Pagination::new(1,20).expect("page")).await.expect("history") else {panic!("active full history")};
                assert_eq!(count,4); assert_eq!(applications.len(),4);
                assert!(matches!(services.application().detail(&institution,ids[0]).await,Err(Error::TargetUnavailable)));
                assert!(matches!(services.application().resource(&institution,photo.resource_id).await,Err(Error::TargetUnavailable)));
                let original_file=services.application().resource(&manager,photo.resource_id).await.expect("original private photo").bytes;
                let private=&services.application().settings().expect("configured storage").directory;
                assert!(settings::publish(private,photo.resource_id,b"replacement bytes").is_err(),"a retained resource cannot be overwritten");
                assert_eq!(services.application().resource(&manager,photo.resource_id).await.expect("unchanged photo").bytes,original_file);
                private.remove(&["workforce"],&photo.resource_id.to_string()).expect("isolated missing-file injection");
                assert!(matches!(services.application().resource(&manager,photo.resource_id).await,Err(Error::MissingFile)));
                settings::publish(private,photo.resource_id,&original_file).expect("restore retained fixture bytes");
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
                let Detail::Full {application}=services.application().detail(&manager,ids[0]).await.expect("retained snapshot") else {panic!("active detail")};
                assert_eq!(application.snapshot,preview.snapshot,"configuration edits cannot rewrite an accepted print snapshot");
                owner.client().batch_execute("CREATE FUNCTION audit.reject_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'fixture audit rejection'; END $$; CREATE TRIGGER reject_fixture BEFORE INSERT ON audit.events FOR EACH ROW EXECUTE FUNCTION audit.reject_fixture()").await.expect("atomic rollback injection");
                assert!(services.application().submit(&manager,Input {kind:Kind::Issuance,selections:selections.clone(),expected:preview.snapshot.clone()}).await.is_err());
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.applications",&[]).await.expect("application rolled back with audit").get::<_,i64>(0),5);
                owner.client().batch_execute("DROP TRIGGER reject_fixture ON audit.events; DROP FUNCTION audit.reject_fixture()").await.expect("remove fixture fault");
                proxy.arm(fixture::database_proxy::Fault::LostCommit);
                assert!(matches!(services.application().submit(&manager,Input {kind:Kind::Issuance,selections:selections.clone(),expected:preview.snapshot.clone()}).await,Err(Error::Unknown(_))),"a lost commit reply remains unknown");
                let original_backend=proxy.intercepted().await;
                observer.client().batch_execute("BEGIN").await.expect("independent outcome observation");
                observer.client().query_one("SELECT id FROM identity.persons WHERE id=(SELECT person_id FROM identity.accounts WHERE id=$1) FOR UPDATE",&[&manager.account_id()]).await.expect("original transaction released its participant");
                assert_eq!(observer.client().query_one("SELECT count(*) FROM workforce.applications",&[]).await.expect("one durable result").get::<_,i64>(0),6);
                observer.client().batch_execute("COMMIT").await.expect("observation ended");
                proxy.release();
                assert!(matches!(services.application().list(&manager,Pagination::new(1,20).expect("page")).await.expect("refresh after unknown"),List::Full {count:6,..}));
                assert!(services.application().resource(&manager,photo.resource_id).await.is_ok(),"unknown commit never deletes a referenced file");
                timeout(Duration::from_secs(3),async {
                    let mut cadence=tokio::time::interval(Duration::from_millis(10));
                    loop {cadence.tick().await;if observer.client().query_one("SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",&[&original_backend]).await.expect("original backend state").get::<_,bool>(0){break;}}
                }).await.expect("original backend ended before fixture cleanup");
                Box::pin(services.grant().terminate(manager.clone(),operation(),grant.id())).await.expect("real own service termination");
                let restricted=serde_json::to_value(services.application().list(&manager,Pagination::new(1,20).expect("page")).await.expect("restricted history")).expect("projection");
                assert_eq!(restricted["access"],"restricted");
                assert_eq!(restricted["count"],6);
                assert!(restricted["applications"].as_array().expect("summaries").iter().all(|item|item.as_object().expect("summary").len()==4));
                assert!(matches!(services.application().resource(&manager,photo.resource_id).await,Err(Error::TargetUnavailable)));
                assert!(matches!(services.application().submit(&manager,Input {kind:Kind::Issuance,selections,expected:preview.snapshot}).await,Err(Error::Denied)));
                owner.terminate().await.expect("owner stopped"); observer.terminate().await.expect("observer stopped");
                services.close(); services.drain().await.expect("drain"); context.stop().await.expect("context stopped"); proxy.stop().await; supplier.stop().await;
            })).await.expect("bounded application acceptance");
        }));
    }
}
