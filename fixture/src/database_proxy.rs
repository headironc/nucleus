//! Loopback TLS faults at C1 commit, clock and retained-history boundaries.

use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

use rustls::{ClientConfig, RootCertStore, ServerConfig, crypto::ring};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::Server;

pub enum Fault {
    LostCommit,
    DelayedCommit,
    RolledBack,
    StalledTime,
    StalledHistory,
}

struct Control {
    fault: AtomicU8,
    intercepted: AtomicBool,
    entered: Notify,
    release: Notify,
    stop: Notify,
}

pub struct Proxy {
    port: u16,
    authority: CertificateDer<'static>,
    control: Arc<Control>,
    task: JoinHandle<()>,
}

impl Proxy {
    pub async fn start(server: &Server) -> Self {
        let authority = server.certificate().clone();
        // Identical disposable certificates preserve SCRAM channel binding on both legs.
        let acceptor = TlsAcceptor::from(Arc::new(
            ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_no_client_auth()
                .with_single_cert(
                    vec![authority.clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(server.key())),
                )
                .expect("proxy TLS"),
        ));
        let mut roots = RootCertStore::empty();
        roots.add(authority.clone()).expect("fixture authority");
        let connector = TlsConnector::from(Arc::new(
            ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy listener");
        let port = listener.local_addr().expect("proxy address").port();
        let backend = SocketAddr::from(([127, 0, 0, 1], server.port()));
        let control = Arc::new(Control {
            fault: AtomicU8::new(0),
            intercepted: AtomicBool::new(false),
            entered: Notify::new(),
            release: Notify::new(),
            stop: Notify::new(),
        });
        let shared = Arc::clone(&control);
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    _ = shared.stop.notified() => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let (acceptor, connector, control) = (acceptor.clone(), connector.clone(), Arc::clone(&shared));
                        clients.spawn(async move { let _ = relay(socket, backend, acceptor, connector, control).await; });
                    }
                    Some(_) = clients.join_next(), if !clients.is_empty() => {},
                }
            }
            clients.shutdown().await;
        });
        Self {
            port,
            authority,
            control,
            task,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn authority(&self) -> CertificateDer<'static> {
        self.authority.clone()
    }

    pub fn arm(&self, fault: Fault) {
        self.control.intercepted.store(false, Ordering::SeqCst);
        self.control.fault.store(
            match fault {
                Fault::LostCommit => 1,
                Fault::DelayedCommit => 2,
                Fault::RolledBack => 3,
                Fault::StalledTime => 4,
                Fault::StalledHistory => 5,
            },
            Ordering::SeqCst,
        );
    }

    pub async fn intercepted(&self) {
        loop {
            let notified = self.control.entered.notified();
            if self.control.intercepted.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub fn release(&self) {
        self.control.release.notify_one();
    }

    pub async fn stop(mut self) {
        self.control.stop.notify_one();
        (&mut self.task).await.expect("proxy ended");
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay(
    mut socket: TcpStream,
    address: SocketAddr,
    acceptor: TlsAcceptor,
    connector: TlsConnector,
    control: Arc<Control>,
) -> io::Result<()> {
    // Forwarded protocol frames must not accumulate Nagle/delayed-ACK latency.
    socket.set_nodelay(true)?;
    let mut negotiation = [0; 8];
    socket.read_exact(&mut negotiation).await?;
    if negotiation != [0, 0, 0, 8, 4, 210, 22, 47] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    socket.write_all(b"S").await?;
    let mut frontend = acceptor.accept(socket).await?;
    let mut backend = TcpStream::connect(address).await?;
    backend.set_nodelay(true)?;
    backend.write_all(&negotiation).await?;
    let mut accepted = [0];
    backend.read_exact(&mut accepted).await?;
    if accepted != *b"S" {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut backend = connector
        .connect(ServerName::IpAddress(address.ip().into()), backend)
        .await?;
    let mut length = [0; 4];
    frontend.read_exact(&mut length).await?;
    let size = usize::try_from(u32::from_be_bytes(length)).map_err(integer)?;
    if !(4..=65_536).contains(&size) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut startup = vec![0; size - 4];
    frontend.read_exact(&mut startup).await?;
    backend.write_all(&length).await?;
    backend.write_all(&startup).await?;
    backend.flush().await?;
    let (mut input, mut output) = tokio::io::split(frontend);
    let (mut replies, mut commands) = tokio::io::split(backend);
    let close = Notify::new();
    let outgoing = async {
        let mut changed = false;
        loop {
            let message = frame(&mut input).await?;
            if message[0] == b'P'
                && (message
                    .windows(b"INSERT INTO identity.challenges".len())
                    .any(|bytes| bytes == b"INSERT INTO identity.challenges")
                    || message
                        .windows(b"UPDATE identity.challenges".len())
                        .any(|bytes| bytes == b"UPDATE identity.challenges")
                    || message
                        .windows(b"INSERT INTO identity.messages".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.messages")
                    || message
                        .windows(b"UPDATE identity.messages".len())
                        .any(|bytes| bytes == b"UPDATE identity.messages")
                    || message
                        .windows(b"INSERT INTO identity.claims".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.claims"))
                || message[0] == b'P'
                    && message
                        .windows(b"DELETE FROM identity.challenges".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.challenges")
            {
                changed = true;
            }
            let stalled = if message[0] == b'P'
                && message
                    .windows(b"SELECT clock_timestamp()".len())
                    .any(|bytes| bytes == b"SELECT clock_timestamp()")
            {
                4
            } else if message[0] == b'P'
                && message
                    .windows(b"FETCH FORWARD 128 FROM challenges".len())
                    .any(|bytes| bytes == b"FETCH FORWARD 128 FROM challenges")
            {
                5
            } else {
                0
            };
            if stalled != 0
                && control
                    .fault
                    .compare_exchange(stalled, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                control.intercepted.store(true, Ordering::SeqCst);
                control.entered.notify_waiters();
                control.release.notified().await;
            }
            if changed && message[0] == b'Q' && &message[5..] == b"COMMIT\0" {
                let fault = control.fault.load(Ordering::SeqCst);
                if matches!(fault, 1..=3)
                    && control
                        .fault
                        .compare_exchange(fault, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    close.notify_one();
                    control.intercepted.store(true, Ordering::SeqCst);
                    control.entered.notify_waiters();
                    if fault == 2 {
                        control.release.notified().await;
                    }
                    if fault == 3 {
                        return Ok::<_, io::Error>(());
                    }
                    commands.write_all(&message).await?;
                    commands.flush().await?;
                    if fault == 1 {
                        control.release.notified().await;
                    }
                    return Ok(());
                }
                changed = false;
            }
            commands.write_all(&message).await?;
            commands.flush().await?;
        }
    };
    let incoming = async {
        tokio::select! {
            result = forward(&mut replies, &mut output) => result,
            _ = close.notified() => output.shutdown().await,
        }
    };
    tokio::try_join!(outgoing, incoming)?;
    Ok(())
}

async fn frame(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut header = [0; 5];
    stream.read_exact(&mut header).await?;
    let size = usize::try_from(u32::from_be_bytes([
        header[1], header[2], header[3], header[4],
    ]))
    .map_err(integer)?;
    if !(4..=1_048_576).contains(&size) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut message = vec![0; size + 1];
    message[..5].copy_from_slice(&header);
    stream.read_exact(&mut message[5..]).await?;
    Ok(message)
}

async fn forward(
    input: &mut (impl AsyncRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
) -> io::Result<()> {
    loop {
        output.write_all(&frame(input).await?).await?;
        output.flush().await?;
    }
}

fn integer(error: std::num::TryFromIntError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
