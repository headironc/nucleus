use std::{
    future::{Future, pending},
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch},
    time::{Instant, Sleep, sleep, sleep_until},
};

/// Bound transport resources independently of C1's accepted work capacity.
pub(crate) struct Listener {
    socket: TcpListener,
    capacity: Arc<Semaphore>,
    failure: mpsc::Sender<io::Error>,
    stopping: watch::Receiver<bool>,
}

impl Listener {
    pub(crate) async fn bind(
        address: SocketAddr,
        failure: mpsc::Sender<io::Error>,
        stopping: watch::Receiver<bool>,
    ) -> io::Result<Self> {
        Ok(Self {
            socket: TcpListener::bind(address).await?,
            capacity: Arc::new(Semaphore::new(128)),
            failure,
            stopping,
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
                    transmission: Transmission::new(self.stopping.clone()),
                    stopped: Box::pin({
                        let mut stopping = self.stopping.clone();
                        async move {
                            let _ = stopping.wait_for(|stopped| *stopped).await;
                        }
                    }),
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
    transmission: Transmission,
    stopped: Pin<Box<dyn Future<Output = ()> + Send>>,
}

/// An image closes its HTTP/1 connection; the permit outlives Hyper's copied body buffers.
#[derive(Clone)]
pub(crate) struct Transmission {
    state: Arc<Mutex<State>>,
    stopping: watch::Receiver<bool>,
}

enum State {
    Idle,
    Image {
        permit: Arc<OwnedSemaphorePermit>,
        deadline: Pin<Box<Sleep>>,
        prepared: bool,
    },
    Closed,
}

impl Transmission {
    pub(crate) fn new(stopping: watch::Receiver<bool>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::Idle)),
            stopping,
        }
    }

    pub(crate) fn begin(&self, permit: OwnedSemaphorePermit) -> Result<Instant, io::Error> {
        let mut state = self.state.lock().map_err(poisoned)?;
        if self.stopping() || !matches!(*state, State::Idle) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        *state = State::Image {
            permit: Arc::new(permit),
            deadline: Box::pin(sleep_until(deadline)),
            prepared: false,
        };
        Ok(deadline)
    }

    /// The content read keeps capacity even if its peer disconnects before it returns.
    pub(crate) fn hold_read(&self) -> io::Result<Arc<OwnedSemaphorePermit>> {
        let state = self.state.lock().map_err(poisoned)?;
        match &*state {
            State::Image { permit, .. } => Ok(Arc::clone(permit)),
            _ => Err(io::ErrorKind::ConnectionAborted.into()),
        }
    }

    /// Finish the bounded read before shutdown may interrupt this response's byte stream.
    pub(crate) fn respond(&self) -> io::Result<()> {
        let mut state = self.state.lock().map_err(poisoned)?;
        let State::Image { prepared, .. } = &mut *state else {
            return Err(io::ErrorKind::ConnectionAborted.into());
        };
        *prepared = true;
        Ok(())
    }

    fn stopping(&self) -> bool {
        *self.stopping.borrow() || self.stopping.has_changed().is_err()
    }

    fn check(
        &self,
        context: &mut Context<'_>,
        stopped: Pin<&mut (dyn Future<Output = ()> + Send)>,
    ) -> io::Result<()> {
        let mut state = self.state.lock().map_err(poisoned)?;
        match &mut *state {
            State::Idle => Ok(()),
            State::Image {
                deadline, prepared, ..
            } => {
                if *prepared && (self.stopping() || stopped.poll(context).is_ready()) {
                    return Err(io::ErrorKind::ConnectionAborted.into());
                }
                if Instant::now() >= deadline.deadline()
                    || deadline.as_mut().poll(context).is_ready()
                {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                Ok(())
            }
            State::Closed => Err(io::ErrorKind::ConnectionAborted.into()),
        }
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::ErrorKind::Other.into()
}

impl Drop for Connection {
    fn drop(&mut self) {
        // A handler that finishes after disconnection cannot register another transfer.
        match self.transmission.state.lock() {
            Ok(mut state) => *state = State::Closed,
            Err(error) => *error.into_inner() = State::Closed,
        }
    }
}

impl Connection {
    fn check(&mut self, context: &mut Context<'_>) -> io::Result<()> {
        self.transmission.check(context, self.stopped.as_mut())?;
        if self.deadline.as_mut().poll(context).is_ready() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
    pub(crate) fn transmission(&self) -> Transmission {
        self.transmission.clone()
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.check(context)?;
        Pin::new(&mut self.socket).poll_read(context, buffer)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.check(context)?;
        Pin::new(&mut self.socket).poll_write(context, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(context)?;
        Pin::new(&mut self.socket).poll_flush(context)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.check(context)?;
        Pin::new(&mut self.socket).poll_shutdown(context)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{io, sync::Arc, time::Duration};

    use axum::serve::Listener;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpSocket, TcpStream},
        sync::{Semaphore, mpsc, watch},
        time::{Instant, timeout},
    };

    #[tokio::test]
    async fn image_permits_cover_blocked_socket_writes_and_cannot_outlive_connection_closure() {
        let (failure, _failures) = mpsc::channel(1);
        let (_stop, stopping) = watch::channel(false);
        let mut listener =
            super::Listener::bind("127.0.0.1:0".parse().expect("address"), failure, stopping)
                .await
                .expect("listener");
        let address = listener.local_addr().expect("address");
        let capacity = Arc::new(Semaphore::new(8));
        let mut clients = Vec::new();
        let mut connections = Vec::new();
        for _ in 0..8 {
            let socket = TcpSocket::new_v4().expect("client socket");
            socket
                .set_recv_buffer_size(1024)
                .expect("bounded receive window");
            let (client, (connection, _)) =
                tokio::join!(socket.connect(address), listener.accept());
            clients.push(client.expect("client"));
            let deadline = connection
                .transmission
                .begin(
                    Arc::clone(&capacity)
                        .try_acquire_owned()
                        .expect("available image slot"),
                )
                .expect("connection owns transfer");
            assert!(deadline > Instant::now() + Duration::from_secs(29));
            connections.push(connection);
        }
        assert!(
            Arc::clone(&capacity).try_acquire_owned().is_err(),
            "ninth transfer cannot queue"
        );
        let mut connection = connections.pop().expect("last transfer");
        let late = connection.transmission();
        let reading = late.hold_read().expect("accepted content read");
        // A real TCP receive window saturates. Cancelling the write waiter does not finish IO ownership.
        let bytes = vec![42; 8 * 1024 * 1024];
        assert!(
            timeout(Duration::from_millis(100), connection.write_all(&bytes))
                .await
                .is_err()
        );
        assert_eq!(capacity.available_permits(), 0);
        if let super::State::Image { deadline, .. } = &mut *connection
            .transmission
            .state
            .lock()
            .expect("transfer state")
        {
            deadline.as_mut().reset(Instant::now());
        } else {
            panic!("original transfer must remain registered");
        }
        let error = timeout(Duration::from_secs(1), connection.flush())
            .await
            .expect("flush is bounded without polling a body")
            .expect_err("image deadline closes IO");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            capacity.available_permits(),
            0,
            "timing out a caller does not release the connection permit"
        );
        drop(connection);
        assert_eq!(
            capacity.available_permits(),
            0,
            "a disconnected peer cannot release unfinished content work"
        );
        drop(reading);
        assert_eq!(capacity.available_permits(), 1);
        assert!(
            late.begin(
                Arc::clone(&capacity)
                    .try_acquire_owned()
                    .expect("released slot")
            )
            .is_err(),
            "closed connection rejects late handler registration"
        );
        assert_eq!(capacity.available_permits(), 1);
        drop(connections);
        assert_eq!(capacity.available_permits(), 8);
    }

    #[tokio::test]
    async fn stopping_wakes_blocked_image_io_without_releasing_its_connection_permit() {
        let (failure, _failures) = mpsc::channel(1);
        let (stop, stopping) = watch::channel(false);
        let mut listener =
            super::Listener::bind("127.0.0.1:0".parse().expect("address"), failure, stopping)
                .await
                .expect("listener");
        let socket = TcpSocket::new_v4().expect("client socket");
        socket
            .set_recv_buffer_size(1024)
            .expect("small receive window");
        let (client, (mut connection, _)) = tokio::join!(
            socket.connect(listener.local_addr().expect("address")),
            listener.accept(),
        );
        let _client = client.expect("client");
        let capacity = Arc::new(Semaphore::new(8));
        let transmission = connection.transmission();
        let deadline = transmission
            .begin(Arc::clone(&capacity).try_acquire_owned().expect("slot"))
            .expect("register image");
        transmission.respond().expect("bounded read completed");
        let bytes = vec![42; 8 * 1024 * 1024];
        assert!(
            timeout(Duration::from_millis(100), connection.write_all(&bytes))
                .await
                .is_err(),
            "the real TCP send buffer is saturated while the client does not read"
        );
        let (blocked, observed) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            use std::{future::Future, task::Poll};
            let mut writing = Box::pin(connection.write_all(&bytes));
            std::future::poll_fn(|context| {
                assert!(writing.as_mut().poll(context).is_pending());
                Poll::Ready(())
            })
            .await;
            blocked.send(()).expect("observer awaits the blocked IO");
            let result = writing.await;
            (connection, result)
        });
        observed.await.expect("write registered its wakeup");
        assert!(deadline > Instant::now() + Duration::from_secs(29));
        stop.send(true).expect("stop transport");
        assert_eq!(
            capacity.available_permits(),
            7,
            "the stop signal cannot release ownership"
        );
        let (connection, result) = timeout(Duration::from_secs(1), task)
            .await
            .expect("stop wakes pending socket IO")
            .expect("writer task");
        assert_eq!(
            result.expect_err("stream interrupted").kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(capacity.available_permits(), 7);
        drop(connection);
        assert_eq!(capacity.available_permits(), 8);
        let fresh = super::Transmission::new(stop.subscribe());
        assert_eq!(
            fresh
                .begin(Arc::clone(&capacity).try_acquire_owned().expect("slot"))
                .expect_err("stopping forbids new registration")
                .kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(capacity.available_permits(), 8);
    }

    #[tokio::test]
    async fn stopping_preserves_an_images_pending_read_and_unrelated_connections() {
        let (failure, _failures) = mpsc::channel(1);
        let (stop, stopping) = watch::channel(false);
        let mut listener =
            super::Listener::bind("127.0.0.1:0".parse().expect("address"), failure, stopping)
                .await
                .expect("listener");
        let (client, (mut connection, _)) = tokio::join!(
            TcpStream::connect(listener.local_addr().expect("address")),
            listener.accept(),
        );
        let mut client = client.expect("client");
        let capacity = Arc::new(Semaphore::new(8));
        let transmission = connection.transmission();
        transmission
            .begin(Arc::clone(&capacity).try_acquire_owned().expect("slot"))
            .expect("register image before shutdown");
        stop.send(true).expect("stop transport");
        client.write_all(b"a").await.expect("peer data");
        assert_eq!(
            connection
                .read_u8()
                .await
                .expect("read may finish preparing its response"),
            b'a'
        );
        transmission
            .respond()
            .expect("preparation completed after stop");
        assert_eq!(
            connection
                .flush()
                .await
                .expect_err("prepared image IO ends")
                .kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(capacity.available_permits(), 7);
        drop(connection);
        assert_eq!(capacity.available_permits(), 8);

        let (client, (mut connection, _)) = tokio::join!(
            TcpStream::connect(listener.local_addr().expect("address")),
            listener.accept(),
        );
        let mut client = client.expect("ordinary connection");
        client.write_all(b"b").await.expect("peer data");
        assert_eq!(
            connection
                .read_u8()
                .await
                .expect("Hyper owns ordinary graceful shutdown"),
            b'b'
        );
        assert!(
            connection
                .transmission()
                .begin(Arc::clone(&capacity).try_acquire_owned().expect("slot"))
                .is_err()
        );
    }

    #[tokio::test]
    async fn graceful_http_shutdown_waits_for_the_image_connection_after_the_handler_returns() {
        use std::future::IntoFuture;

        use axum::{Router, extract::ConnectInfo, http::header, routing::get};
        use tokio::sync::oneshot;

        let (failure, _failures) = mpsc::channel(1);
        let (_stop, stopping) = watch::channel(false);
        let listener =
            super::Listener::bind("127.0.0.1:0".parse().expect("address"), failure, stopping)
                .await
                .expect("listener");
        let address = listener.local_addr().expect("address");
        let capacity = Arc::new(Semaphore::new(8));
        let (entered, mut started) = mpsc::channel(1);
        let slots = Arc::clone(&capacity);
        let router = Router::new().route(
            "/image",
            get(move |ConnectInfo(peer): ConnectInfo<crate::source::Peer>| {
                let slots = Arc::clone(&slots);
                let entered = entered.clone();
                async move {
                    let transmission = peer.transmission().clone();
                    transmission
                        .begin(slots.try_acquire_owned().expect("image permit"))
                        .expect("connection registration");
                    entered
                        .send(transmission)
                        .await
                        .expect("test observes registered work");
                    // Exercise Hyper's real copied-buffer path with a saturated TCP window.
                    (
                        [(header::CONNECTION, "close")],
                        vec![42_u8; 8 * 1024 * 1024],
                    )
                }
            }),
        );
        let (stop, stopped) = oneshot::channel();
        let mut server = tokio::spawn(
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<crate::source::Peer>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .into_future(),
        );
        let socket = TcpSocket::new_v4().expect("client socket");
        socket
            .set_recv_buffer_size(1024)
            .expect("small receive window");
        let mut client = socket.connect(address).await.expect("client");
        client
            .write_all(b"GET /image HTTP/1.1\r\nHost: fixture\r\n\r\n")
            .await
            .expect("request");
        let transmission = timeout(Duration::from_secs(1), started.recv())
            .await
            .expect("handler reached")
            .expect("registered transmission");
        stop.send(()).expect("graceful stop");
        assert!(
            timeout(Duration::from_millis(100), &mut server)
                .await
                .is_err(),
            "handler completion does not complete the HTTP connection"
        );
        assert_eq!(
            capacity.available_permits(),
            7,
            "Hyper's copied body buffer still owns its transfer permit"
        );
        if let super::State::Image { deadline, .. } =
            &mut *transmission.state.lock().expect("transfer state")
        {
            deadline.as_mut().reset(Instant::now());
        } else {
            panic!("HTTP transfer remains registered until its connection ends");
        }
        timeout(Duration::from_secs(1), server)
            .await
            .expect("image deadline permits graceful completion")
            .expect("HTTP task")
            .expect("HTTP server");
        assert_eq!(capacity.available_permits(), 8);
        assert!(
            transmission
                .begin(
                    Arc::clone(&capacity)
                        .try_acquire_owned()
                        .expect("reusable slot")
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn bounds_connections_and_releases_capacity_on_drop() {
        let (failure, _failures) = mpsc::channel(1);
        let (_stop, stopping) = watch::channel(false);
        let mut listener =
            super::Listener::bind("127.0.0.1:0".parse().expect("address"), failure, stopping)
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
