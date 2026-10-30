//! HTTP route assembly and transport-task lifetime under the process supervisor.

use std::{future::IntoFuture, io, net::SocketAddr};

use domain::startup::Services;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{
    access, center, health, identity, listener, process::RunError, router, source, workforce,
};

pub(super) struct Transport {
    address: SocketAddr,
    task: JoinHandle<io::Result<()>>,
    failures: mpsc::Receiver<io::Error>,
    shutdown: watch::Sender<bool>,
    joined: bool,
}

impl Transport {
    pub(super) async fn bind(
        address: SocketAddr,
        services: Services,
        settings: identity::Settings,
        source: source::Settings,
    ) -> Result<Self, RunError> {
        let (failure, failures) = mpsc::channel(1);
        let (shutdown, mut shutting_down) = watch::channel(false);
        let listener = listener::Listener::bind(address, failure, shutting_down.clone())
            .await
            .map_err(RunError::Listener)?;
        let admission = identity::admission::Admission::new().map_err(RunError::Admission)?;
        let authentication =
            identity::Context::new(services.clone(), settings, source, admission.clone());
        let routes = identity::router(authentication.clone())
            .merge(center::router(
                services.affiliation().clone(),
                authentication.clone(),
            ))
            .merge(workforce::router(
                services.number().clone(),
                authentication.clone(),
            ))
            .merge(workforce::directory::router(
                services.directory().clone(),
                authentication.clone(),
            ))
            .merge(workforce::application::router(
                services.application().clone(),
                authentication.clone(),
                admission,
            ))
            .merge(access::router(
                services.role().clone(),
                services.appointment().clone(),
                services.grant().clone(),
                services.assignment().clone(),
                authentication,
            ));
        let router = router::new(routes, health::router(services))?;
        let address = axum::serve::Listener::local_addr(&listener).map_err(RunError::Listener)?;
        let task = tokio::spawn(
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<source::Peer>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutting_down.wait_for(|stopped| *stopped).await;
            })
            .into_future(),
        );
        Ok(Self {
            address,
            task,
            failures,
            shutdown,
            joined: false,
        })
    }

    pub(super) async fn wait(
        &mut self,
        stopped: impl std::future::Future<Output = Result<(), RunError>>,
        ready: impl FnOnce(SocketAddr) -> Result<(), RunError>,
    ) -> Result<(), RunError> {
        tokio::pin!(stopped);
        tokio::select! {
            biased;
            result = &mut stopped => return result,
            result = async { ready(self.address) } => result?,
        }
        tracing::info!(stage = "ready", "online service is ready");
        tokio::select! {
            biased;
            result = &mut stopped => result,
            option = self.failures.recv() => Err(match option {
                Some(error) => RunError::Listener(error),
                None => RunError::Ended,
            }),
            result = &mut self.task => {
                self.joined = true;
                let result = result.map_err(Error::from)?;
                result.map_err(Error::from)?;
                Err(RunError::Ended)
            },
        }
    }

    pub(super) fn close(&self) {
        self.shutdown.send_replace(true);
    }

    /// Complete HTTP readers before the application closes its shared database.
    pub(super) async fn drain(&mut self) -> Result<(), Error> {
        self.close();
        if self.joined {
            return Ok(());
        }
        let result = (&mut self.task).await;
        self.joined = true;
        result??;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP transport failed")]
    Transport(#[from] io::Error),
    #[error("HTTP transport task failed")]
    Task {
        task_id: tokio::task::Id,
        panicked: bool,
        cancelled: bool,
    },
}

impl From<tokio::task::JoinError> for Error {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Task {
            task_id: error.id(),
            panicked: error.is_panic(),
            cancelled: error.is_cancelled(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{future::IntoFuture, sync::Arc, time::Duration};

    use axum::{Router, extract::ConnectInfo, http::header, routing::get};
    use tokio::{
        io::AsyncWriteExt,
        net::TcpSocket,
        runtime::Runtime,
        sync::{Semaphore, mpsc, watch},
        time::{Instant, timeout},
    };

    use crate::{
        listener,
        process::{self, Phase, retention, tests},
        source,
    };

    #[test]
    fn stopping_an_image_leaves_time_for_application_and_database_cleanup() {
        let server = tests::server();
        Runtime::new().expect("runtime").block_on(async {
            timeout(Duration::from_secs(15), async {
                let name = tests::database(&server).await;
                let supplier = tests::Supplier::new().await;
                let mut application = tests::start(&server, &name, 0, 8, &supplier).await;
                assert_eq!(
                    application.settings.stop,
                    Duration::from_secs(process::DEFAULT_STOP_SECONDS)
                );
                let context = application.context.clone();
                let connections = context.connections();
                let database = connections
                    .acquire()
                    .await
                    .expect("owned online connection");
                let (failure, failures) = mpsc::channel(1);
                let (shutdown, mut shutting_down) = watch::channel(false);
                let listener = listener::Listener::bind(
                    "127.0.0.1:0".parse().expect("address"),
                    failure,
                    shutting_down.clone(),
                )
                .await
                .expect("listener");
                let address = axum::serve::Listener::local_addr(&listener).expect("address");
                let capacity = Arc::new(Semaphore::new(8));
                let slots = Arc::clone(&capacity);
                let (entered, mut started) = mpsc::channel(1);
                let router = Router::new().route(
                    "/image",
                    get(move |ConnectInfo(peer): ConnectInfo<source::Peer>| {
                        let slots = Arc::clone(&slots);
                        let entered = entered.clone();
                        async move {
                            let transmission = peer.transmission();
                            let deadline = transmission
                                .begin(slots.try_acquire_owned().expect("image slot"))
                                .expect("registered image");
                            transmission
                                .respond()
                                .expect("bounded preparation complete");
                            entered.send(deadline).await.expect("observed image");
                            // Large test content exercises the same copied buffers as frozen HTTP content.
                            (
                                [(header::CONNECTION, "close")],
                                vec![42_u8; 8 * 1024 * 1024],
                            )
                        }
                    }),
                );
                let task = tokio::spawn(
                    axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<source::Peer>(),
                    )
                    .with_graceful_shutdown(async move {
                        let _ = shutting_down.wait_for(|stopped| *stopped).await;
                    })
                    .into_future(),
                );
                application.phase = Phase::Running {
                    transport: super::Transport {
                        address,
                        task,
                        failures,
                        shutdown,
                        joined: false,
                    },
                    retention: retention::Retention::start(application.services.clone()),
                };
                let socket = TcpSocket::new_v4().expect("client socket");
                socket
                    .set_recv_buffer_size(1024)
                    .expect("small receive window");
                let mut client = socket.connect(address).await.expect("client");
                client
                    .write_all(b"GET /image HTTP/1.1\r\nHost: fixture\r\n\r\n")
                    .await
                    .expect("request");
                let deadline = timeout(Duration::from_secs(1), started.recv())
                    .await
                    .expect("handler reached")
                    .expect("original image deadline");
                assert!(deadline > Instant::now() + Duration::from_secs(29));
                assert_eq!(capacity.available_permits(), 7);
                timeout(Duration::from_secs(3), application.stop())
                    .await
                    .expect("image cannot consume the default thirty-second stop budget")
                    .expect("complete transport, retention, work and database cleanup");
                assert!(matches!(application.phase, Phase::Stopped));
                assert_eq!(capacity.available_permits(), 8);
                assert!(
                    database.client().is_closed(),
                    "the database driver ended, not merely the HTTP waiter"
                );
                assert!(connections.acquire().await.is_err());
                assert!(
                    tests::instance_released(&server, &name).await,
                    "the completed drain ended the instance session"
                );
                drop(application);
            })
            .await
            .expect("application image shutdown test budget");
        });
    }
}
