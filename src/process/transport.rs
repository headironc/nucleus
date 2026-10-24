//! HTTP route assembly and transport-task lifetime under the process supervisor.

use std::{future::IntoFuture, io, net::SocketAddr};

use domain::identity::authentication;
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::{health, identity, listener, process::RunError, router, source};

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
        authentication: authentication::Service,
        settings: identity::authentication::Settings,
        source: source::Settings,
    ) -> Result<Self, RunError> {
        let (failure, failures) = mpsc::channel(1);
        let listener = listener::Listener::bind(address, failure)
            .await
            .map_err(RunError::Listener)?;
        let admission = identity::admission::Admission::new().map_err(RunError::Admission)?;
        let routes =
            identity::authentication::router(authentication.clone(), settings, source, admission)
                .merge(health::router(authentication));
        let router = router::new(routes)?;
        let address = axum::serve::Listener::local_addr(&listener).map_err(RunError::Listener)?;
        let (shutdown, mut shutting_down) = watch::channel(false);
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
