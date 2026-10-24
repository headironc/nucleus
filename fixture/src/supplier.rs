//! Loopback TLS transport for fictional supplier requests.

use std::{net::SocketAddr, sync::Arc, time::Instant};

use rcgen::generate_simple_self_signed;
use rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::TlsAcceptor;

pub struct Supplier {
    address: SocketAddr,
    authority: Vec<u8>,
    requests: mpsc::UnboundedReceiver<Request>,
    stopping: Arc<Notify>,
    task: JoinHandle<()>,
}

pub struct Request {
    pub bytes: Vec<u8>,
    /// Recorded before queueing, independently of when the test receives this request.
    pub received_at: Instant,
    pub reply: oneshot::Sender<Vec<u8>>,
}

impl Supplier {
    pub async fn start() -> Self {
        let certificate = generate_simple_self_signed(vec!["dypnsapi.aliyuncs.com".into()])
            .expect("fictional supplier certificate");
        let authority = certificate.cert.der().to_vec();
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring TLS")
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.cert.der().clone()],
                    PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
                )
                .expect("fictional supplier TLS");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("loopback address");
        let (sender, requests) = mpsc::unbounded_channel();
        let stopping = Arc::new(Notify::new());
        let stopped = Arc::clone(&stopping);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut option = None;
            loop {
                tokio::select! {
                    () = stopped.notified() => break,
                    Some(result) = connections.join_next(), if !connections.is_empty() => {
                        if let Err(error) = result {
                            option = Some(error);
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("fixture accept");
                        let acceptor = acceptor.clone();
                        let sender = sender.clone();
                        connections.spawn(async move {
                            let mut socket = acceptor.accept(socket).await.expect("fixture TLS");
                            let mut bytes = Vec::new();
                            let mut buffer = [0; 4096];
                            loop {
                                let size = socket.read(&mut buffer).await.expect("request read");
                                assert!(size > 0 && bytes.len() < 32768, "bounded fixture request");
                                bytes.extend_from_slice(&buffer[..size]);
                                if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            let received_at = Instant::now();
                            let (reply, response) = oneshot::channel();
                            if sender.send(Request { bytes, received_at, reply }).is_err() {
                                return;
                            }
                            if let Ok(bytes) = response.await {
                                let _ = socket.write_all(&bytes).await;
                                let _ = socket.shutdown().await;
                            }
                        });
                    }
                }
            }
            drop(listener);
            connections.abort_all();
            while let Some(result) = connections.join_next().await {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    option.get_or_insert(error);
                }
            }
            if let Some(error) = option {
                panic!("supplier connection task failed: {error}");
            }
        });
        Self {
            address,
            authority,
            requests,
            stopping,
            task,
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn authority(&self) -> &[u8] {
        &self.authority
    }
    pub async fn request(&mut self) -> Request {
        self.requests.recv().await.expect("supplier request")
    }

    /// Close the listener and wait for every accepted connection to end.
    pub async fn stop(mut self) {
        self.stopping.notify_one();
        (&mut self.task).await.expect("supplier stopped");
    }
}

impl Drop for Supplier {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use rustls::{ClientConfig, RootCertStore, pki_types::CertificateDer};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
        time::timeout,
    };
    use tokio_rustls::{TlsConnector, client::TlsStream};

    use crate::supplier::Supplier;

    async fn connect(supplier: &Supplier) -> TlsStream<TcpStream> {
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(supplier.authority().to_vec()))
            .expect("fixture authority");
        let connector = TlsConnector::from(Arc::new(
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ));
        connector
            .connect(
                "dypnsapi.aliyuncs.com".try_into().expect("supplier name"),
                TcpStream::connect(supplier.address())
                    .await
                    .expect("connect"),
            )
            .await
            .expect("fixture TLS")
    }

    #[tokio::test]
    async fn observes_another_request_while_a_reply_and_a_tls_handshake_are_pending() {
        timeout(Duration::from_secs(5), async {
            let mut supplier = Supplier::start().await;
            let mut first = connect(&supplier).await;
            first
                .write_all(b"POST /first HTTP/1.1\r\n\r\n")
                .await
                .expect("first request");
            let first_request = supplier.request().await;
            let mut stalled = TcpStream::connect(supplier.address())
                .await
                .expect("pending TLS");
            let mut second = connect(&supplier).await;
            let sent_at = std::time::Instant::now();
            second
                .write_all(b"POST /second HTTP/1.1\r\n\r\n")
                .await
                .expect("second request");
            let second_request = supplier.request().await;
            assert_eq!(first_request.bytes, b"POST /first HTTP/1.1\r\n\r\n");
            assert_eq!(second_request.bytes, b"POST /second HTTP/1.1\r\n\r\n");
            assert!(second_request.received_at >= sent_at);
            assert!(second_request.received_at <= std::time::Instant::now());
            assert!(!first_request.reply.is_closed());
            let response = b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
            second_request
                .reply
                .send(response.to_vec())
                .expect("release second reply first");
            let mut bytes = Vec::new();
            second
                .read_to_end(&mut bytes)
                .await
                .expect("second response");
            assert_eq!(bytes, response);
            assert!(!first_request.reply.is_closed());
            first_request
                .reply
                .send(response.to_vec())
                .expect("release first reply");
            bytes.clear();
            first.read_to_end(&mut bytes).await.expect("first response");
            assert_eq!(bytes, response);
            supplier.stop().await;
            assert_eq!(stalled.read(&mut [0]).await.expect("stalled TLS closed"), 0);
        })
        .await
        .expect("concurrent supplier observations bounded");
    }

    #[tokio::test]
    async fn stop_ends_connections_waiting_for_tls_headers_and_replies() {
        timeout(Duration::from_secs(5), async {
            let mut supplier = Supplier::start().await;
            let address = supplier.address();
            let mut first = connect(&supplier).await;
            first
                .write_all(b"POST /held HTTP/1.1\r\n\r\n")
                .await
                .expect("held request");
            let request = supplier.request().await;
            let mut handshake = TcpStream::connect(address)
                .await
                .expect("pending handshake");
            // Completing this later handshake also lets the listener accept the earlier socket.
            let mut headers = connect(&supplier).await;
            headers
                .write_all(b"POST /partial HTTP/1.1\r\n")
                .await
                .expect("partial headers");
            supplier.stop().await;
            assert!(request.reply.is_closed(), "response owner has ended");
            for mut socket in [first, headers] {
                let mut bytes = Vec::new();
                let _ = socket.read_to_end(&mut bytes).await;
                assert!(bytes.is_empty(), "stop does not invent a response");
            }
            assert_eq!(
                handshake.read(&mut [0]).await.expect("pending TLS closed"),
                0
            );
            assert!(
                TcpStream::connect(address).await.is_err(),
                "listener closed"
            );
        })
        .await
        .expect("supplier shutdown bounded");
    }

    #[tokio::test]
    async fn connection_failure_is_reported_after_other_connections_end() {
        timeout(Duration::from_secs(5), async {
            let mut supplier = Supplier::start().await;
            let mut held = connect(&supplier).await;
            held.write_all(b"POST /held HTTP/1.1\r\n\r\n")
                .await
                .expect("held request");
            let request = supplier.request().await;
            let mut broken = connect(&supplier).await;
            broken
                .shutdown()
                .await
                .expect("close before complete headers");
            while !supplier.task.is_finished() {
                tokio::task::yield_now().await;
            }
            assert!(
                request.reply.is_closed(),
                "other connection ended before failure delivery"
            );
            let failure = tokio::spawn(supplier.stop())
                .await
                .expect_err("connection failure remains observable");
            assert!(failure.is_panic());
        })
        .await
        .expect("supplier failure handling bounded");
    }
}
