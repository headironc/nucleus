use std::{
    future::pending,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    time::{Sleep, sleep},
};

/// Bound transport resources independently of C1's accepted work capacity.
pub(crate) struct Listener {
    socket: TcpListener,
    capacity: Arc<Semaphore>,
    failure: mpsc::Sender<io::Error>,
}

impl Listener {
    pub(crate) async fn bind(
        address: SocketAddr,
        failure: mpsc::Sender<io::Error>,
    ) -> io::Result<Self> {
        Ok(Self {
            socket: TcpListener::bind(address).await?,
            capacity: Arc::new(Semaphore::new(128)),
            failure,
        })
    }
}

impl axum::serve::Listener for Listener {
    type Io = Connection;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Connection, SocketAddr) {
        let permit = match Arc::clone(&self.capacity).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return pending().await,
        };
        match self.socket.accept().await {
            Ok((socket, address)) => (
                Connection {
                    socket,
                    _permit: permit,
                    deadline: Box::pin(sleep(Duration::from_secs(60))),
                },
                address,
            ),
            Err(error) => {
                let _ = self.failure.send(error).await;
                pending().await
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

pub(crate) struct Connection {
    socket: TcpStream,
    _permit: OwnedSemaphorePermit,
    deadline: Pin<Box<Sleep>>,
}

impl Connection {
    fn expired(&mut self, context: &mut Context<'_>) -> bool {
        std::future::Future::poll(self.deadline.as_mut(), context).is_ready()
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.expired(context) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Pin::new(&mut self.socket).poll_read(context, buffer)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.expired(context) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        Pin::new(&mut self.socket).poll_write(context, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(context)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(context)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{io, time::Duration};

    use axum::serve::Listener;
    use tokio::{
        io::AsyncReadExt,
        net::TcpStream,
        sync::mpsc,
        time::{Instant, timeout},
    };

    #[tokio::test]
    async fn bounds_connections_and_releases_capacity_on_drop() {
        let (failure, _failures) = mpsc::channel(1);
        let mut listener = super::Listener::bind("127.0.0.1:0".parse().expect("address"), failure)
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("address");
        let mut clients = Vec::new();
        let mut connections = Vec::new();
        for _ in 0..128 {
            let (client, (connection, _)) =
                tokio::join!(TcpStream::connect(address), listener.accept());
            clients.push(client.expect("fixture connection"));
            connections.push(connection);
        }
        let extra = TcpStream::connect(address)
            .await
            .expect("pending connection");
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        drop(connections.pop());
        let (mut connection, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("released capacity");
        connection.deadline.as_mut().reset(Instant::now());
        let error = timeout(Duration::from_secs(1), connection.read_u8())
            .await
            .expect("transport deadline")
            .expect_err("expired connection");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(extra);
    }
}
