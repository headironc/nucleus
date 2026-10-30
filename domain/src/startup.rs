//! No online service leaves this owner before every retained fact and the barrier are confirmed.

use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};

use crate::{
    access::{appoint, appointment, assignment, grant, initialize, role},
    account, cancellation,
    center::affiliation,
    content::column,
    correction,
    database::{Barrier, verification},
    identity::{
        self, authentication, avatar, challenge::client, current, message, revocation, transfer,
        website,
    },
    kernel::App,
    keys::{self, Keys},
    registration,
    runtime::{Context, registry::Registry, work::Work},
    suspension,
    workforce::{directory, number},
};

#[derive(Clone)]
pub struct Services {
    authentication: authentication::Service,
    registration: registration::Service,
    current: current::Service,
    revocation: revocation::Service,
    transfer: transfer::Service,
    role: role::Service,
    appointment: appointment::Service,
    assignment: assignment::Service,
    grant: grant::Service,
    account: account::Service,
    suspension: suspension::Service,
    correction: correction::Service,
    cancellation: cancellation::Service,
    avatar: avatar::Service,
    affiliation: affiliation::Service,
    column: column::Service,
    directory: directory::Service,
    number: number::Service,
    context: Context,
    registry: Arc<Registry>,
    work: Arc<Work>,
    app: App,
    avatar_retention: avatar::retention::Settings,
}

pub struct Settings {
    capacity: NonZeroUsize,
    authentication: authentication::Settings,
    registration: registration::Limits,
    avatar: avatar::link::Link,
    avatar_retention: avatar::retention::Settings,
    avatar_storage: Option<avatar::storage::Settings>,
    avatar_limits: avatar::upload::Limits,
}

impl Settings {
    pub fn new(
        capacity: usize,
        message: message::Limits,
        session: authentication::session::settings::Settings,
        registration: registration::Limits,
        avatar: avatar::link::Link,
    ) -> Result<Self, CapacityError> {
        Ok(Self {
            capacity: NonZeroUsize::new(capacity).ok_or(CapacityError)?,
            authentication: authentication::Settings::new(message, session),
            registration,
            avatar,
            avatar_retention: avatar::retention::Settings::default(),
            avatar_storage: None,
            avatar_limits: avatar::upload::Limits::default(),
        })
    }

    pub fn with_avatar_retention(mut self, settings: avatar::retention::Settings) -> Self {
        self.avatar_retention = settings;
        self
    }

    pub fn with_avatar_storage(mut self, settings: Option<avatar::storage::Settings>) -> Self {
        self.avatar_storage = settings;
        self
    }

    pub fn with_avatar_limits(mut self, settings: avatar::upload::Limits) -> Self {
        self.avatar_limits = settings;
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("shared request work capacity must be positive")]
pub struct CapacityError;

/// The same verified application, without HTTP or supplier clients.
pub struct Administration {
    app: App,
    keys: Arc<identity::keys::Keys>,
}

impl Administration {
    pub async fn start(
        barrier: Barrier,
        keys: Keys,
        app_id: website::AppId,
    ) -> Result<Self, Error> {
        let metadata = keys.metadata().clone();
        let context = barrier.context().clone();
        // A maintenance process accepts exactly its one command and any original-result lookup.
        let registry = Arc::new(Registry::new(1, context.admission()));
        let app = App::new(
            context,
            registry,
            Arc::new(Work::default()),
            Arc::new(keys.encryption),
            Arc::new(keys.seal),
        );
        let identity = Arc::new(keys.identity);
        let result = async {
            verify(
                &barrier,
                &app,
                &metadata,
                &identity,
                &keys.invitation,
                &app_id,
            )
            .await?;
            verification::Stage::BarrierConfirmation
                .query(Duration::from_secs(10), barrier.confirm())
                .await??;
            Ok::<_, Error>(())
        }
        .await;
        let ended = barrier.terminate().await;
        match (result, ended) {
            (Ok(()), Ok(())) => Ok(Self {
                app,
                keys: identity,
            }),
            (Ok(()), Err(error)) => Err(error.into()),
            (Err(cause), Ok(())) => Err(cause),
            (Err(cause), Err(termination)) => Err(Error::Combined {
                cause: Box::new(cause),
                termination,
            }),
        }
    }

    pub async fn initialize(
        &self,
        operation_id: identity::operation::Id,
        input: initialize::Operator,
        dry_run: bool,
    ) -> Result<Option<initialize::Receipt>, initialize::Error> {
        initialize::run(
            self.app.clone(),
            Arc::clone(&self.keys),
            operation_id,
            input,
            dry_run,
        )
        .await
    }

    pub async fn appoint(
        &self,
        operation_id: identity::operation::Id,
        input: appoint::Input,
        dry_run: bool,
    ) -> Result<Option<appoint::Completion>, appoint::Error> {
        appoint::run(
            self.app.clone(),
            Arc::clone(&self.keys),
            operation_id,
            input,
            dry_run,
        )
        .await
    }

    pub async fn correct(
        &self,
        operation_id: identity::operation::Id,
        input: identity::correction::admin::Input,
        dry_run: bool,
    ) -> Result<Option<identity::correction::Correction>, identity::correction::admin::Error> {
        identity::correction::admin::run(
            self.app.clone(),
            Arc::clone(&self.keys),
            operation_id,
            input,
            dry_run,
        )
        .await
    }

    pub async fn correct_affiliation(
        &self,
        operation_id: identity::operation::Id,
        input: affiliation::admin::Input,
        dry_run: bool,
    ) -> Result<Option<()>, affiliation::admin::Error> {
        affiliation::admin::run(
            self.app.clone(),
            Arc::clone(&self.keys),
            operation_id,
            input,
            dry_run,
        )
        .await
    }

    pub async fn confirm(&self) -> Result<(), Error> {
        if let Err(error) = self.app.context().heartbeat().await {
            self.close();
            return Err(error.into());
        }
        self.app.work().ready(self.app.registry()).await?;
        Ok(())
    }

    pub fn close(&self) {
        self.app.registry().close();
    }

    pub async fn drain(&self) -> Result<(), Error> {
        self.app.work().drain(self.app.registry()).await?;
        Ok(())
    }
}

async fn verify(
    barrier: &Barrier,
    app: &App,
    metadata: &keys::metadata::Metadata,
    keys: &Arc<identity::keys::Keys>,
    invitations: &grant::invitation::keys::Keys,
    app_id: &website::AppId,
) -> Result<chrono::DateTime<chrono::Utc>, Error> {
    let client = barrier.client();
    verification::Stage::Keys
        .batch(
            Duration::from_secs(10),
            keys::database::verify(metadata, client),
        )
        .await??;
    avatar::Defaults::verify()?;
    let now = verification::Stage::TrustedTime.runtime(
        verification::Waiting::Query,
        app.context().now(client).await,
    )?;
    identity::verification::start(client, app, keys, now).await?;
    let roles = role::Reader::new(app.clone());
    let revisions = identity::person::revision::Reader::new(app.clone());
    role::verify(client, app, &roles, &revisions, now).await?;
    grant::verify(client, app, &revisions, now).await?;
    verification::Stage::Appointments
        .batch(
            Duration::from_secs(10),
            appointment::verify(client, app, &roles, &revisions, now),
        )
        .await??;
    crate::access::verification::verify(client, app, now).await?;
    initialize::verify(client, app, now).await?;
    authentication::verify(client, app, keys, app_id, now).await?;
    grant::invitation::verify(client, &roles, invitations, &revisions, now).await?;
    registration::verify(client, app, keys, app_id, now).await?;
    revocation::verify(client, app, now).await?;
    transfer::verify(client, app, keys, now).await?;
    suspension::verify(client, app, now).await?;
    affiliation::verify(client, app, now).await?;
    verification::Stage::HistoryRecords
        .query(Duration::from_secs(10), column::verify(client, app, now))
        .await??;
    verification::Stage::Directories
        .batch(
            Duration::from_secs(10),
            directory::verify(client, app.keys(), now),
        )
        .await??;
    number::verify(client, app, &revisions, now).await?;
    avatar::verify(client, app, now)
        .await
        .map_err(Error::AvatarFacts)?;
    Ok(now)
}

impl Services {
    #[cfg(test)]
    pub(crate) fn app(&self) -> &App {
        &self.app
    }

    pub async fn start(
        barrier: Barrier,
        keys: Keys,
        settings: Settings,
        supplier: client::Client,
        message: message::client::Client,
    ) -> Result<Self, Error> {
        let metadata = keys.metadata().clone();
        let encryption = Arc::new(keys.encryption);
        let seals = Arc::new(keys.seal);
        let invitations = keys.invitation;
        let keys = Arc::new(keys.identity);
        let context = barrier.context().clone();
        let registry = Arc::new(Registry::new(settings.capacity.get(), context.admission()));
        let work = Arc::new(Work::default());
        let app = App::new(
            context.clone(),
            Arc::clone(&registry),
            Arc::clone(&work),
            encryption,
            Arc::clone(&seals),
        );
        let app_id = settings.authentication.session().app_id().clone();
        let registration = registration::Service::prepare(
            app.clone(),
            Arc::clone(&keys),
            settings.authentication.session().issuance().clone(),
            settings.registration,
        );
        let result = async {
            let avatar = avatar::Service::prepare(
                app.clone(),
                context.clone(),
                Arc::clone(&keys),
                settings.avatar,
                Arc::clone(&registry),
                settings.avatar_storage,
                settings.avatar_limits,
            )?;
            let now = verify(&barrier, &app, &metadata, &keys, &invitations, &app_id).await?;
            avatar
                .verify_files(barrier.client())
                .await
                .map_err(Error::AvatarFacts)?;
            registration::ready(barrier.client(), &app, now).await?;
            verification::Stage::BarrierConfirmation
                .query(Duration::from_secs(10), barrier.confirm())
                .await??;
            // Recovery transactions need the committed generation and shared barrier.
            // Request services remain unavailable until their file effects are settled.
            avatar::retention::Retention::new(
                app.clone(),
                avatar.clone(),
                settings.avatar_retention,
            )
            .restore()
            .await
            .map_err(Error::AvatarFacts)?;
            let current =
                current::Service::prepare(app.clone(), Arc::clone(&keys), app_id, avatar.clone());
            let authentication = authentication::Service::prepare(
                app.clone(),
                Arc::clone(&keys),
                settings.authentication,
                current.clone(),
                authentication::Suppliers {
                    challenge: supplier,
                    message,
                },
            );
            let revocation = revocation::Service::prepare(app.clone(), current.clone());
            let transfer =
                transfer::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let role = role::Service::prepare(
                app.clone(),
                current.clone(),
                crate::identity::person::revision::Reader::new(app.clone()),
            );
            let affiliation =
                affiliation::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let directory = directory::Service::prepare(app.clone(), current.clone());
            let column = column::Service::prepare(app.clone(), current.clone());
            let number = number::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let appointment =
                appointment::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let account = account::Service::prepare(
                app.clone(),
                current.clone(),
                Arc::clone(&keys),
                avatar.clone(),
            );
            let assignment =
                assignment::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let grant = grant::Service::prepare(
                app.clone(),
                current.clone(),
                Arc::clone(&keys),
                invitations,
            );
            let suspension =
                suspension::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let correction =
                correction::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            let cancellation =
                cancellation::Service::prepare(app.clone(), current.clone(), Arc::clone(&keys));
            Ok::<_, Error>((
                authentication,
                current,
                avatar,
                revocation,
                role,
                appointment,
                assignment,
                account,
                grant,
                suspension,
                correction,
                cancellation,
                transfer,
                affiliation,
                column,
                directory,
                number,
            ))
        }
        .await;
        let ended = barrier.terminate().await;
        match result {
            Ok((
                authentication,
                current,
                avatar,
                revocation,
                role,
                appointment,
                assignment,
                account,
                grant,
                suspension,
                correction,
                cancellation,
                transfer,
                affiliation,
                column,
                directory,
                number,
            )) => {
                ended?;
                Ok(Self {
                    authentication,
                    registration,
                    current,
                    revocation,
                    role,
                    appointment,
                    assignment,
                    account,
                    grant,
                    suspension,
                    correction,
                    cancellation,
                    transfer,
                    avatar,
                    affiliation,
                    column,
                    directory,
                    number,
                    context,
                    registry,
                    work,
                    app,
                    avatar_retention: settings.avatar_retention,
                })
            }
            Err(cause) => match ended {
                Ok(()) => Err(cause),
                Err(termination) => Err(Error::Combined {
                    cause: Box::new(cause),
                    termination,
                }),
            },
        }
    }

    pub fn authentication(&self) -> &authentication::Service {
        &self.authentication
    }
    pub fn registration(&self) -> &registration::Service {
        &self.registration
    }
    pub fn current(&self) -> &current::Service {
        &self.current
    }
    pub fn revocation(&self) -> &revocation::Service {
        &self.revocation
    }
    pub fn transfer(&self) -> &transfer::Service {
        &self.transfer
    }
    pub fn role(&self) -> &role::Service {
        &self.role
    }
    pub fn appointment(&self) -> &appointment::Service {
        &self.appointment
    }
    pub fn assignment(&self) -> &assignment::Service {
        &self.assignment
    }
    pub fn grant(&self) -> &grant::Service {
        &self.grant
    }
    pub fn suspension(&self) -> &suspension::Service {
        &self.suspension
    }
    pub fn account(&self) -> &account::Service {
        &self.account
    }
    pub fn correction(&self) -> &correction::Service {
        &self.correction
    }
    pub fn cancellation(&self) -> &cancellation::Service {
        &self.cancellation
    }
    pub fn avatar(&self) -> &avatar::Service {
        &self.avatar
    }

    pub fn avatar_retention(&self) -> avatar::retention::Retention {
        avatar::retention::Retention::new(
            self.app.clone(),
            self.avatar.clone(),
            self.avatar_retention,
        )
    }
    pub fn affiliation(&self) -> &affiliation::Service {
        &self.affiliation
    }

    pub fn directory(&self) -> &directory::Service {
        &self.directory
    }

    pub fn column(&self) -> &column::Service {
        &self.column
    }

    pub fn number(&self) -> &number::Service {
        &self.number
    }

    pub fn close(&self) {
        self.registry.close();
    }

    /// Whether shared admission is still open; a lost instance session closes it.
    pub fn admitted(&self) -> bool {
        self.registry.ready().is_ok()
    }

    /// Before handing out or announcing the application, confirm on the instance session
    /// itself that the lock is still held and admission is open. Startup scans run
    /// without the clock, so a session lost meanwhile is otherwise noticed only later.
    pub async fn confirm(&self) -> Result<(), Error> {
        if let Err(error) = self.context.heartbeat().await {
            self.registry.close();
            return Err(error.into());
        }
        self.work.ready(&self.registry).await?;
        Ok(())
    }

    pub async fn ready(&self) -> Result<(), Error> {
        self.context.read_time().await?;
        self.work.ready(&self.registry).await?;
        Ok(())
    }

    pub async fn retry_after(&self, retry_at: DateTime<Utc>) -> Result<Duration, Error> {
        Ok((retry_at - self.context.read_time().await?)
            .to_std()
            .unwrap_or(Duration::ZERO))
    }

    pub async fn drain(&self) -> Result<(), Error> {
        self.work.drain(&self.registry).await?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Initialization(#[from] initialize::Error),
    #[error("{0}")]
    Role(#[from] role::CommandError),
    #[error("{0}")]
    Affiliation(#[from] affiliation::Error),
    #[error("column verification failed")]
    Column(#[from] column::Error),
    #[error("{0}")]
    Directory(#[from] directory::Error),
    #[error("{0}")]
    Number(#[from] number::Error),
    #[error("{0}")]
    Suspension(#[from] crate::suspension::Error),
    #[error("{0}")]
    Avatar(#[from] avatar::Error),
    #[error("avatar retained-fact verification failed: {0}")]
    AvatarFacts(crate::kernel::Error<std::convert::Infallible>),
    #[error("{0}")]
    Metadata(#[from] keys::database::Error),
    #[error("{0}")]
    Access(#[from] crate::access::Error),
    #[error("{0}")]
    Appointment(#[from] appointment::Error),
    #[error("{0}")]
    AppointmentCommand(#[from] appointment::CommandError),
    #[error("{0}")]
    Assignment(#[from] assignment::Error),
    #[error("{0}")]
    Grant(#[from] grant::Error),
    #[error("{0}")]
    Identity(#[from] crate::identity::verification::Error),
    #[error("{0}")]
    Runtime(#[from] crate::runtime::Error),
    #[error("{0}")]
    Work(#[from] crate::runtime::work::Error),
    #[error("{0}")]
    Authentication(#[from] authentication::Error),
    #[error("{0}")]
    Registration(#[from] registration::Error),
    #[error("{0}")]
    Revocation(#[from] revocation::Error),
    #[error("{0}")]
    Transfer(#[from] transfer::Error),
    #[error("{0}")]
    Database(#[from] crate::database::Error),
    #[error("{0}")]
    Connection(#[from] platform::database::Error),
    #[error("{0}")]
    Timeout(#[from] crate::database::verification::Timeout),
    #[error("startup verification and barrier termination failed")]
    Combined {
        cause: Box<Error>,
        termination: platform::database::Error,
    },
}
