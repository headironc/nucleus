//! Loopback TLS faults at deployment and domain transaction boundaries.

use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};

use rustls::{ClientConfig, RootCertStore, ServerConfig, crypto::ring};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
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
    RejectedBegin,
    DelayedRollback,
    UnknownCommit,
    LostReadCommit,
    RejectedRollback,
    StalledCurrent,
    LostClockCommit,
    RewoundCurrent,
    StalledRollback,
    DelayedKeyCommit,
    CommitTermination,
    RollbackTermination,
    MigrationCommitTermination,
    LostMigrationReadCommit,
    StalledReadCommit,
    StalledQuery,
    StalledRoleCount,
    StalledRolePage,
    ReadCommitTermination,
    DelayedReadCommit,
    DelayedStateQueries,
    StalledCorrectionCount,
    StalledCorrectionPage,
    StalledGrant,
    StalledAffiliation,
    StalledAffiliationCount,
    StalledAffiliationPage,
    StalledAffiliationRead,
    StalledPerson,
    StalledAccountIdentity,
    StalledIdentityRevisions,
    StalledDatabasePrerequisites,
    StalledNumberStates,
    StalledNumberHistoryCount,
    StalledRuntimeGrants,
    CorruptTimeTermination,
    NullCompletionRead,
    PausedReadEnd,
    StalledRoleStates,
    StalledRoleHistoryCount,
    StalledRoleOriginal,
    StalledAppointmentOriginal,
    StalledHolderStates,
    StalledClock,
    PausedAuthenticationSession,
    ExpiredInvitationCompletion,
    StalledAvatarUploads,
    PausedCommit,
}

struct Control {
    fault: AtomicU8,
    intercepted: Mutex<Option<i32>>,
    entered: Notify,
    release: Notify,
    stop: Notify,
    delayed_queries: AtomicUsize,
    time_queries: AtomicUsize,
}

impl Control {
    async fn pause(&self, backend: &OnceLock<i32>) -> io::Result<()> {
        // Register before publishing the interception; release may arrive immediately.
        let released = self.release.notified();
        self.intercept(backend)?;
        released.await;
        Ok(())
    }

    fn intercept(&self, backend: &OnceLock<i32>) -> io::Result<()> {
        let pid = backend.get().copied().ok_or(io::ErrorKind::InvalidData)?;
        *self.intercepted.lock().expect("interception state") = Some(pid);
        self.entered.notify_waiters();
        Ok(())
    }
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
            intercepted: Mutex::new(None),
            entered: Notify::new(),
            release: Notify::new(),
            stop: Notify::new(),
            delayed_queries: AtomicUsize::new(0),
            time_queries: AtomicUsize::new(0),
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
    pub fn time_queries(&self) -> usize {
        self.control.time_queries.load(Ordering::SeqCst)
    }
    pub fn authority(&self) -> CertificateDer<'static> {
        self.authority.clone()
    }

    pub fn arm(&self, fault: Fault) {
        *self.control.intercepted.lock().expect("interception state") = None;
        self.control.delayed_queries.store(0, Ordering::SeqCst);
        self.control.fault.store(
            match fault {
                Fault::LostCommit => 1,
                Fault::DelayedCommit => 2,
                Fault::RolledBack => 3,
                Fault::StalledTime => 4,
                Fault::StalledHistory => 5,
                Fault::RejectedBegin => 6,
                Fault::DelayedRollback => 7,
                Fault::UnknownCommit => 8,
                Fault::LostReadCommit => 9,
                Fault::RejectedRollback => 10,
                Fault::StalledCurrent => 11,
                Fault::LostClockCommit => 12,
                Fault::RewoundCurrent => 13,
                Fault::StalledRollback => 14,
                Fault::DelayedKeyCommit => 15,
                Fault::CommitTermination => 16,
                Fault::RollbackTermination => 17,
                Fault::MigrationCommitTermination => 18,
                Fault::LostMigrationReadCommit => 19,
                Fault::StalledReadCommit => 20,
                Fault::StalledQuery => 21,
                Fault::StalledRoleCount => 22,
                Fault::StalledRolePage => 23,
                Fault::ReadCommitTermination => 24,
                Fault::DelayedReadCommit => 25,
                Fault::DelayedStateQueries => 26,
                Fault::StalledCorrectionCount => 27,
                Fault::StalledCorrectionPage => 28,
                Fault::StalledGrant => 29,
                Fault::StalledAffiliation => 30,
                Fault::StalledAffiliationCount => 31,
                Fault::StalledAffiliationPage => 32,
                Fault::StalledAffiliationRead => 33,
                Fault::StalledPerson => 34,
                Fault::StalledAccountIdentity => 35,
                Fault::StalledIdentityRevisions => 36,
                Fault::StalledDatabasePrerequisites => 37,
                Fault::StalledNumberStates => 38,
                Fault::StalledNumberHistoryCount => 39,
                Fault::StalledRuntimeGrants => 40,
                Fault::CorruptTimeTermination => 41,
                Fault::NullCompletionRead => 42,
                Fault::PausedReadEnd => 43,
                Fault::StalledRoleStates => 44,
                Fault::StalledRoleHistoryCount => 45,
                Fault::StalledRoleOriginal => 46,
                Fault::StalledAppointmentOriginal => 47,
                Fault::StalledHolderStates => 48,
                Fault::StalledClock => 49,
                Fault::PausedAuthenticationSession => 50,
                Fault::ExpiredInvitationCompletion => 51,
                Fault::StalledAvatarUploads => 52,
                Fault::PausedCommit => 53,
            },
            Ordering::SeqCst,
        );
    }

    /// Return the backend PID of the connection that reached the armed fault.
    pub async fn intercepted(&self) -> i32 {
        loop {
            let notified = self.control.entered.notified();
            let option = *self.control.intercepted.lock().expect("interception state");
            if let Some(pid) = option {
                return pid;
            }
            notified.await;
        }
    }

    pub fn release(&self) {
        // An ended fault must not leave a permit for a later interception.
        self.control.release.notify_waiters();
    }

    pub fn delayed_queries(&self) -> usize {
        self.control.delayed_queries.load(Ordering::SeqCst)
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
    let closed = Notify::new();
    let stalled_query = AtomicBool::new(false);
    let resumed = Notify::new();
    let stalled_completion = AtomicBool::new(false);
    let termination_response = AtomicBool::new(false);
    let paused_read_end = AtomicBool::new(false);
    let paused_authentication_lookup = AtomicBool::new(false);
    let committing = AtomicBool::new(false);
    let backend_pid = OnceLock::new();
    let outgoing = async {
        let mut changed = false;
        let mut coordinated = false;
        let mut active = false;
        let mut clock = false;
        let mut keys = false;
        let mut migration = false;
        let mut history = false;
        let mut listing = false;
        let mut delayed = false;
        let mut invitation_written = false;
        loop {
            let mut message = frame(&mut input).await?;
            if message[0] == b'P'
                && control.fault.load(Ordering::SeqCst) == 51
                && message
                    .windows(b"UPDATE access.invitations SET".len())
                    .any(|bytes| bytes == b"UPDATE access.invitations SET")
            {
                invitation_written = true;
            }
            if invitation_written
                && message[0] == b'P'
                && message
                    .windows(b"SELECT clock_timestamp() AS observed_at".len())
                    .any(|bytes| bytes == b"SELECT clock_timestamp() AS observed_at")
                && control
                    .fault
                    .compare_exchange(51, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // Only the completion observation crosses the invitation's original expiry.
                let observed = b"clock_timestamp() AS observed_at";
                let offset = message
                    .windows(observed.len())
                    .position(|bytes| bytes == observed)
                    .ok_or(io::ErrorKind::InvalidData)?;
                message.splice(
                    offset..offset + observed.len(),
                    b"(clock_timestamp() + interval '8 days') AS observed_at"
                        .iter()
                        .copied(),
                );
                let length = u32::try_from(message.len() - 1)
                    .map_err(integer)?
                    .to_be_bytes();
                message[1..5].copy_from_slice(&length);
            }
            if message[0] == b'P'
                && message
                    .windows(b"WHERE state.authentication_id = $1\0".len())
                    .any(|bytes| bytes == b"WHERE state.authentication_id = $1\0")
                && control
                    .fault
                    .compare_exchange(50, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                paused_authentication_lookup.store(true, Ordering::SeqCst);
            }
            if message[0] == b'P'
                && message
                    .windows(b"SELECT clock_timestamp() AS observed_at".len())
                    .any(|bytes| bytes == b"SELECT clock_timestamp() AS observed_at")
            {
                control.time_queries.fetch_add(1, Ordering::SeqCst);
            }
            let role_states = message[0] == b'P'
                && message
                    .windows(b"SELECT state.id AS role_id, state.scope, state.name, state.level, state.permissions, state.active".len())
                    .any(|bytes| bytes == b"SELECT state.id AS role_id, state.scope, state.name, state.level, state.permissions, state.active")
                && message
                    .windows(b"FROM access.roles state\0".len())
                    .any(|bytes| bytes == b"FROM access.roles state\0");
            let role_history_count = message[0] == b'P'
                && message
                    .windows(b"SELECT count(*) AS total, count(*) FILTER (WHERE ($1::uuid IS NULL OR role_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)) AS selected FROM access.changes WHERE scope=$3\0".len())
                    .any(|bytes| bytes == b"SELECT count(*) AS total, count(*) FILTER (WHERE ($1::uuid IS NULL OR role_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)) AS selected FROM access.changes WHERE scope=$3\0");
            let appointment_history_count = message[0] == b'P'
                && message
                    .windows(b"SELECT count(*) AS total,count(*) FILTER (WHERE ($1::uuid IS NULL OR person_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)) AS selected FROM access.appointments\0".len())
                    .any(|bytes| bytes == b"SELECT count(*) AS total,count(*) FILTER (WHERE ($1::uuid IS NULL OR person_id=$1) AND ($2::uuid IS NULL OR operation_id=$2)) AS selected FROM access.appointments\0");
            let role_original = message[0] == b'P'
                && message
                    .windows(b"SELECT record.id,record.role_id,record.previous_id,record.sequence,".len())
                    .any(|bytes| bytes == b"SELECT record.id,record.role_id,record.previous_id,record.sequence,")
                && message
                    .windows(b"FROM access.changes record WHERE record.operation_id=$1 AND record.scope=$2 AND record.level BETWEEN 2 AND 999\0".len())
                    .any(|bytes| bytes == b"FROM access.changes record WHERE record.operation_id=$1 AND record.scope=$2 AND record.level BETWEEN 2 AND 999\0");
            let correction_count = control.fault.load(Ordering::SeqCst) == 27
                && message[0] == b'P'
                && message
                    .windows(b"FROM identity.persons state".len())
                    .any(|bytes| bytes == b"FROM identity.persons state");
            let person_history = message[0] == b'P'
                && message.windows(b"FROM identity.changes record WHERE record.person_id = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3".len())
                    .any(|bytes| bytes == b"FROM identity.changes record WHERE record.person_id = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3");
            if message[0] == b'P' {
                if (role_history_count
                    || appointment_history_count
                    || message
                        .windows(b"SELECT count(*) FROM (SELECT record.grant_id,".len())
                        .any(|bytes| bytes == b"SELECT count(*) FROM (SELECT record.grant_id,")
                    || person_history
                    || message
                        .windows(b"SELECT count(*) FROM access.grants state WHERE".len())
                        .any(|bytes| bytes == b"SELECT count(*) FROM access.grants state WHERE")
                    || message
                        .windows(b"SELECT count(*) FROM access.suspensions suspension WHERE".len())
                        .any(|bytes| {
                            bytes == b"SELECT count(*) FROM access.suspensions suspension WHERE"
                        })
                    || message
                        .windows(b"AS retained FROM history WHERE".len())
                        .any(|bytes| bytes == b"AS retained FROM history WHERE")
                    || message
                        .windows(b"AS retained FROM access.resumptions resumption LEFT JOIN".len())
                        .any(|bytes| {
                            bytes == b"AS retained FROM access.resumptions resumption LEFT JOIN"
                        })
                    || message
                        .windows(
                            b"SELECT count(*) FROM identity.suspensions WHERE account_id=$1\0".len(),
                        )
                        .any(|bytes| {
                            bytes
                                == b"SELECT count(*) FROM identity.suspensions WHERE account_id=$1\0"
                        })
                    || [
                        b"SELECT count(*) FROM access.assignments WHERE ($1".as_slice(),
                        b"SELECT count(*) FROM access.changes WHERE scope = $3",
                    ]
                    .iter()
                    .any(|pattern| {
                        message
                            .windows(pattern.len())
                            .any(|bytes| bytes == *pattern)
                    }))
                    && control
                        .fault
                        .compare_exchange(26, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    delayed = true;
                    control.intercept(&backend_pid)?;
                }
                if delayed {
                    // One delay per query Parse, not per Bind/Execute or response frame.
                    // Only this history transaction is affected, after its authorization.
                    tokio::time::sleep(Duration::from_millis(8)).await;
                    control.delayed_queries.fetch_add(1, Ordering::SeqCst);
                }
            }
            if message[0] == b'P'
                && control
                    .fault
                    .compare_exchange(21, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // Parse follows connection startup: the caller owns a live client and
                // driver, but this prerequisite query has not reached PostgreSQL.
                // Hold backend errors too, so its idle timeout cannot race the caller's deadline.
                stalled_query.store(true, Ordering::SeqCst);
                control.pause(&backend_pid).await?;
                stalled_query.store(false, Ordering::SeqCst);
                resumed.notify_one();
            }
            if message[0] == b'P'
                && message
                    .windows(b"SELECT clock_timestamp() AS observed_at".len())
                    .any(|bytes| bytes == b"SELECT clock_timestamp() AS observed_at")
                && control
                    .fault
                    .compare_exchange(13, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // Only the decisive query sees wall time behind the startup high water.
                let observed = b"clock_timestamp() AS observed_at";
                let offset = message
                    .windows(observed.len())
                    .position(|bytes| bytes == observed)
                    .ok_or(io::ErrorKind::InvalidData)?;
                message.splice(offset..offset + observed.len(), b"(SELECT observed_at - interval '1 microsecond' FROM security.clocks WHERE id) AS observed_at".iter().copied());
                let length = u32::try_from(message.len() - 1)
                    .map_err(integer)?
                    .to_be_bytes();
                message[1..5].copy_from_slice(&length);
                control.pause(&backend_pid).await?;
            }
            if message[0] == b'P'
                && message
                    .windows(b"SELECT clock_timestamp() AS observed_at".len())
                    .any(|bytes| bytes == b"SELECT clock_timestamp() AS observed_at")
                && control
                    .fault
                    .compare_exchange(41, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // Damage the decisive observation, then fail its connection termination.
                let observed = b"clock_timestamp() AS observed_at";
                let offset = message
                    .windows(observed.len())
                    .position(|bytes| bytes == observed)
                    .ok_or(io::ErrorKind::InvalidData)?;
                message.splice(offset..offset + observed.len(), b"(SELECT observed_at - interval '1 microsecond' FROM security.clocks WHERE id) AS observed_at".iter().copied());
                let length = u32::try_from(message.len() - 1)
                    .map_err(integer)?
                    .to_be_bytes();
                message[1..5].copy_from_slice(&length);
                termination_response.store(true, Ordering::SeqCst);
            }
            if message[0] == b'P'
                && (message
                    .windows(b"FROM testing.completions WHERE operation_id=$1".len())
                    .any(|bytes| bytes == b"FROM testing.completions WHERE operation_id=$1")
                    || message
                        .windows(b"FROM workforce.completions WHERE operation_id=$1".len())
                        .any(|bytes| bytes == b"FROM workforce.completions WHERE operation_id=$1"))
                && control
                    .fault
                    .compare_exchange(42, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // Preserve the original lookup and binding, but damage its mandatory seal.
                let prefix = b", seal, ";
                let offset = message
                    .windows(prefix.len())
                    .position(|bytes| bytes == prefix)
                    .ok_or(io::ErrorKind::InvalidData)?;
                message.splice(
                    offset..offset + prefix.len(),
                    b", NULL::bytea AS seal, ".iter().copied(),
                );
                let length = u32::try_from(message.len() - 1)
                    .map_err(integer)?
                    .to_be_bytes();
                message[1..5].copy_from_slice(&length);
                control.intercept(&backend_pid)?;
            }
            if message[0] == b'P'
                && message
                    .windows(b"UPDATE security.clocks".len())
                    .any(|bytes| bytes == b"UPDATE security.clocks")
            {
                clock = true;
                if control
                    .fault
                    .compare_exchange(49, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    stalled_query.store(true, Ordering::SeqCst);
                    control.pause(&backend_pid).await?;
                    stalled_query.store(false, Ordering::SeqCst);
                    resumed.notify_one();
                }
            }
            if message[0] == b'Q'
                && (message[5..].starts_with(b"BEGIN ISOLATION LEVEL")
                    || message[5..].starts_with(b"START TRANSACTION ISOLATION LEVEL")
                    || &message[5..] == b"START TRANSACTION\0")
            {
                coordinated = false;
                active = true;
                migration = false;
                history = false;
                listing = false;
            }
            if message[0] == b'Q'
                && &message[5..]
                    == b"SELECT pg_catalog.pg_advisory_xact_lock_shared(1314210636, 1)\0"
            {
                coordinated = true;
            }
            if message[0] == b'Q'
                && &message[5..] == b"SELECT pg_catalog.pg_advisory_xact_lock(1314210636, 1)\0"
            {
                keys = true;
                if control
                    .fault
                    .compare_exchange(37, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    control.pause(&backend_pid).await?;
                }
            }
            if message[0] == b'Q' && message[5..].starts_with(b"INSERT INTO deployment.migrations ")
            {
                changed = true;
                migration = true;
            }
            if message[0] == b'P'
                && message.windows(b"SELECT version, name, applied_on, checksum FROM deployment.migrations ORDER BY version ASC;".len())
                    .any(|bytes| bytes == b"SELECT version, name, applied_on, checksum FROM deployment.migrations ORDER BY version ASC;")
            {
                history = true;
            }
            if active
                && !changed
                && !clock
                && message[0] == b'Q'
                && matches!(&message[5..], b"COMMIT\0" | b"ROLLBACK\0")
                && control
                    .fault
                    .compare_exchange(43, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                paused_read_end.store(true, Ordering::SeqCst);
            }
            let termination_fault = match (message[0], &message[5..]) {
                (b'Q', b"COMMIT\0")
                    if !changed
                        && (coordinated || keys)
                        && control.fault.load(Ordering::SeqCst) == 24 =>
                {
                    24
                }
                (b'Q', b"COMMIT\0") if migration && control.fault.load(Ordering::SeqCst) == 18 => {
                    18
                }
                (b'Q', b"COMMIT\0") => 16,
                (b'Q', b"ROLLBACK\0") => 17,
                _ => 0,
            };
            if active
                && termination_fault != 0
                && control
                    .fault
                    .compare_exchange(termination_fault, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                termination_response.store(true, Ordering::SeqCst);
            }
            if active
                && message[0] == b'Q'
                && &message[5..] == b"ROLLBACK\0"
                && control
                    .fault
                    .compare_exchange(14, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // Hold the response after rollback, so PostgreSQL's idle transaction
                // timeout cannot race the client's rollback deadline.
                let released = control.release.notified();
                stalled_completion.store(true, Ordering::SeqCst);
                commands.write_all(&message).await?;
                commands.flush().await?;
                released.await;
                close.notify_one();
                return Ok(());
            }
            if active
                && message[0] == b'Q'
                && &message[5..] == b"ROLLBACK\0"
                && control
                    .fault
                    .compare_exchange(10, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                close.notify_one();
                control.intercept(&backend_pid)?;
                return Ok(());
            }
            if message[0] == b'Q'
                && (message[5..].starts_with(b"BEGIN ISOLATION LEVEL")
                    || message[5..].starts_with(b"START TRANSACTION ISOLATION LEVEL")
                    || &message[5..] == b"START TRANSACTION\0")
                // This fault proves a business transaction was not retried. Background
                // high-water persistence is a separate owner with its own commit fault.
                && &message[5..] != b"BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL statement_timeout = '3s'\0"
                && control
                    .fault
                    .compare_exchange(6, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                close.notify_one();
                control.intercept(&backend_pid)?;
                return Ok(());
            }
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
                        .any(|bytes| bytes == b"INSERT INTO identity.claims")
                    || message
                        .windows(b"INSERT INTO identity.authentications".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.authentications")
                    || message
                        .windows(b"INSERT INTO identity.persons".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.persons")
                    || message
                        .windows(b"INSERT INTO identity.attempts".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.attempts")
                    || message
                        .windows(b"INSERT INTO identity.revocations".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.revocations")
                    || message
                        .windows(b"INSERT INTO identity.suspensions".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.suspensions")
                    || message
                        .windows(b"INSERT INTO identity.resumptions".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.resumptions")
                    || message
                        .windows(b"INSERT INTO identity.corrections".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.corrections")
                    || message
                        .windows(b"INSERT INTO identity.transfers".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.transfers")
                    || message
                        .windows(b"INSERT INTO identity.rejections".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.rejections")
                    || message
                        .windows(b"INSERT INTO identity.uploads".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.uploads")
                    || message
                        .windows(b"INSERT INTO identity.publications".len())
                        .any(|bytes| bytes == b"INSERT INTO identity.publications")
                    || message
                        .windows(b"DELETE FROM identity.transfers".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.transfers")
                    || message
                        .windows(b"DELETE FROM identity.rejections".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.rejections")
                    || message
                        .windows(b"INSERT INTO access.changes".len())
                        .any(|bytes| bytes == b"INSERT INTO access.changes")
                    || message
                        .windows(b"INSERT INTO access.appointments".len())
                        .any(|bytes| bytes == b"INSERT INTO access.appointments")
                    || message
                        .windows(b"INSERT INTO access.assignments".len())
                        .any(|bytes| bytes == b"INSERT INTO access.assignments")
                    || message
                        .windows(b"INSERT INTO access.grants".len())
                        .any(|bytes| bytes == b"INSERT INTO access.grants")
                    || message
                        .windows(b"INSERT INTO access.terminations".len())
                        .any(|bytes| bytes == b"INSERT INTO access.terminations")
                    || message
                        .windows(b"INSERT INTO access.suspensions".len())
                        .any(|bytes| bytes == b"INSERT INTO access.suspensions")
                    || message
                        .windows(b"INSERT INTO access.resumptions".len())
                        .any(|bytes| bytes == b"INSERT INTO access.resumptions")
                    || message
                        .windows(b"INSERT INTO access.invitations".len())
                        .any(|bytes| bytes == b"INSERT INTO access.invitations")
                    || message
                        .windows(b"INSERT INTO access.withdrawals".len())
                        .any(|bytes| bytes == b"INSERT INTO access.withdrawals")
                    || message
                        .windows(b"INSERT INTO access.adjustments".len())
                        .any(|bytes| bytes == b"INSERT INTO access.adjustments")
                    || message
                        .windows(b"INSERT INTO workforce.allocations".len())
                        .any(|bytes| bytes == b"INSERT INTO workforce.allocations")
                    || message
                        .windows(b"INSERT INTO workforce.corrections".len())
                        .any(|bytes| bytes == b"INSERT INTO workforce.corrections")
                    || message
                        .windows(b"INSERT INTO workforce.recoveries".len())
                        .any(|bytes| bytes == b"INSERT INTO workforce.recoveries")
                    || message
                        .windows(b"INSERT INTO workforce.completions".len())
                        .any(|bytes| bytes == b"INSERT INTO workforce.completions")
                    || message
                        .windows(b"INSERT INTO content.completions".len())
                        .any(|bytes| bytes == b"INSERT INTO content.completions")
                    || message
                        .windows(b"INSERT INTO testing.completions".len())
                        .any(|bytes| bytes == b"INSERT INTO testing.completions")
                    || message
                        .windows(b"DELETE FROM workforce.completions".len())
                        .any(|bytes| bytes == b"DELETE FROM workforce.completions")
                    || message
                        .windows(b"DELETE FROM testing.completions".len())
                        .any(|bytes| bytes == b"DELETE FROM testing.completions")
                    || message
                        .windows(b"INSERT INTO center.affiliations".len())
                        .any(|bytes| bytes == b"INSERT INTO center.affiliations")
                    || message
                        .windows(b"DELETE FROM identity.attempts".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.attempts")
                    || message
                        .windows(b"DELETE FROM identity.handoffs".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.handoffs")
                    || message
                        .windows(b"DELETE FROM identity.registrations".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.registrations")
                    || message
                        .windows(b"DELETE FROM identity.avatars".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.avatars")
                    || message
                        .windows(b"DELETE FROM identity.authentications".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.authentications"))
                || message[0] == b'P'
                    && message
                        .windows(b"DELETE FROM identity.challenges".len())
                        .any(|bytes| bytes == b"DELETE FROM identity.challenges")
            {
                changed = true;
            }
            let role_count = message[0] == b'P'
                && (message
                    .windows(b"SELECT count(*) FROM access.roles WHERE scope = ".len())
                    .any(|bytes| bytes == b"SELECT count(*) FROM access.roles WHERE scope = ")
                    || message
                        .windows(
                            b"SELECT count(*) FROM access.roles role WHERE role.scope = ".len(),
                        )
                        .any(|bytes| {
                            bytes == b"SELECT count(*) FROM access.roles role WHERE role.scope = "
                        })
                    || message
                        .windows(b"SELECT count(*) FROM access.changes WHERE scope = ".len())
                        .any(|bytes| {
                            bytes == b"SELECT count(*) FROM access.changes WHERE scope = "
                        })
                    || appointment_history_count
                    || message
                        .windows(b"SELECT count(*) FROM (SELECT record.grant_id,".len())
                        .any(|bytes| bytes == b"SELECT count(*) FROM (SELECT record.grant_id,")
                    || message
                        .windows(b"SELECT count(*) FROM access.assignments WHERE".len())
                        .any(|bytes| bytes == b"SELECT count(*) FROM access.assignments WHERE")
                    || message
                        .windows(b"SELECT count(*) FROM access.grants state WHERE".len())
                        .any(|bytes| bytes == b"SELECT count(*) FROM access.grants state WHERE")
                    || message
                        .windows(b"SELECT count(*) FROM access.suspensions suspension WHERE".len())
                        .any(|bytes| {
                            bytes == b"SELECT count(*) FROM access.suspensions suspension WHERE"
                        })
                    || message
                        .windows(b"AS retained FROM history WHERE".len())
                        .any(|bytes| bytes == b"AS retained FROM history WHERE")
                    || message
                        .windows(b"AS retained FROM access.resumptions resumption LEFT JOIN".len())
                        .any(|bytes| {
                            bytes == b"AS retained FROM access.resumptions resumption LEFT JOIN"
                        })
                    || message
                        .windows(
                            b"SELECT count(*) FROM identity.suspensions WHERE account_id=$1\0".len(),
                        )
                        .any(|bytes| {
                            bytes
                                == b"SELECT count(*) FROM identity.suspensions WHERE account_id=$1\0"
                        }));
            let grant = message[0] == b'P'
                && (message.windows(b"FROM access.grants state WHERE state.person_id=$1 ORDER BY state.granted_at DESC,state.id DESC".len())
                    .any(|bytes| bytes == b"FROM access.grants state WHERE state.person_id=$1 ORDER BY state.granted_at DESC,state.id DESC")
                    || message.windows(b"FROM access.grants state WHERE state.person_id=$1 AND state.termination IS NULL".len())
                    .any(|bytes| bytes == b"FROM access.grants state WHERE state.person_id=$1 AND state.termination IS NULL"));
            let affiliation_state = message[0] == b'P'
                && message
                    .windows(b"FROM center.profiles state".len())
                    .any(|bytes| bytes == b"FROM center.profiles state");
            let affiliation_count = affiliation_state && control.fault.load(Ordering::SeqCst) == 31;
            let number_history_count = control.fault.load(Ordering::SeqCst) == 39
                && message[0] == b'P'
                && message
                    .windows(b"FROM workforce.numbers state".len())
                    .any(|bytes| bytes == b"FROM workforce.numbers state");
            if role_states
                || role_history_count
                || role_count
                || correction_count
                || grant
                || affiliation_count
                || number_history_count
            {
                listing = true;
            }
            let affiliation_read = affiliation_state && !affiliation_count;
            let person = message[0] == b'P'
                && message
                    .windows(b"FROM identity.accounts state WHERE state.person_id = $1\0".len())
                    .any(|bytes| {
                        bytes == b"FROM identity.accounts state WHERE state.person_id = $1\0"
                    });
            let account_identity = message[0] == b'P'
                && message
                    .windows(b"FROM identity.accounts state WHERE (state.mobile).hash = $1\0".len())
                    .any(|bytes| {
                        bytes == b"FROM identity.accounts state WHERE (state.mobile).hash = $1\0"
                    });
            let logout_original = message[0] == b'P'
                && message.windows(b"SELECT record.account_id,record.person_id,record.sequence,record.operation_id,record.happened_at,record.kind".len())
                    .any(|bytes| bytes == b"SELECT record.account_id,record.person_id,record.sequence,record.operation_id,record.happened_at,record.kind")
                && message.windows(b"WHERE record.kind='logout' AND record.account_id=$1 AND record.operation_id=$2".len())
                    .any(|bytes| bytes == b"WHERE record.kind='logout' AND record.account_id=$1 AND record.operation_id=$2");
            if affiliation_read || person || account_identity || logout_original {
                listing = true;
            }
            let stalled = if message[0] == b'Q'
                && message[5..].starts_with(b"DO $grants$")
            {
                40
            } else if affiliation_read {
                33
            } else if person {
                34
            } else if account_identity {
                35
            } else if message[0] == b'P'
                && message
                    .windows(b"SELECT id, person_id FROM identity.revisions WHERE ($1::uuid IS NULL OR id>$1) ORDER BY id LIMIT 128".len())
                    .any(|bytes| bytes == b"SELECT id, person_id FROM identity.revisions WHERE ($1::uuid IS NULL OR id>$1) ORDER BY id LIMIT 128")
            {
                36
            } else if control.fault.load(Ordering::SeqCst) == 38 && message[0] == b'P'
                && message
                    .windows(b"FROM workforce.numbers state".len())
                    .any(|bytes| bytes == b"FROM workforce.numbers state")
            {
                38
            } else if message[0] == b'P'
                && message
                    .windows(b"WHERE record.operation_id = $1".len())
                    .any(|bytes| bytes == b"WHERE record.operation_id = $1")
            {
                30
            } else if affiliation_count {
                31
            } else if message[0] == b'P'
                && message
                    .windows(b"FROM center.affiliations record WHERE record.person_id = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3".len())
                    .any(|bytes| bytes == b"FROM center.affiliations record WHERE record.person_id = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3")
            {
                32
            } else if grant {
                29
            } else if number_history_count {
                39
            } else if correction_count {
                27
            } else if message[0] == b'P'
                && message
                    .windows(b"FROM identity.changes record WHERE record.person_id = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3".len())
                    .any(|bytes| bytes == b"FROM identity.changes record WHERE record.person_id = $1 ORDER BY record.sequence DESC LIMIT $2 OFFSET $3")
            {
                28
            } else if role_states {
                44
            } else if role_history_count {
                45
            } else if role_original {
                46
            } else if message[0] == b'P'
                && message
                    .windows(b") record WHERE record.operation_id=$1 AND record.source IN ('session','system')\0".len())
                    .any(|bytes| bytes == b") record WHERE record.operation_id=$1 AND record.source IN ('session','system')\0")
            {
                47
            } else if message[0] == b'P'
                && message.windows(b"FROM access.holders state\0".len())
                    .any(|bytes| bytes == b"FROM access.holders state\0")
            {
                48
            } else if role_count {
                22
            } else if message[0] == b'P'
                && (message
                    .windows(b"FROM access.roles role".len())
                    .any(|bytes| bytes == b"FROM access.roles role")
                    || message
                        .windows(b"FROM access.changes change".len())
                        .any(|bytes| bytes == b"FROM access.changes change")
                    || message
                        .windows(b"FROM access.changes record WHERE record.scope=$5 AND ($1::uuid IS NULL OR record.role_id=$1) AND ($2::uuid IS NULL OR record.operation_id=$2) ORDER BY record.changed_at DESC, record.id DESC LIMIT $3 OFFSET $4\0".len())
                        .any(|bytes| bytes == b"FROM access.changes record WHERE record.scope=$5 AND ($1::uuid IS NULL OR record.role_id=$1) AND ($2::uuid IS NULL OR record.operation_id=$2) ORDER BY record.changed_at DESC, record.id DESC LIMIT $3 OFFSET $4\0")
                    || message
                        .windows(b") record WHERE record.source <> 'cancellation' AND".len())
                        .any(|bytes| bytes == b") record WHERE record.source <> 'cancellation' AND")
                    || message
                        .windows(b") record WHERE record.kind=$1 AND ($2::uuid IS NULL OR record.person_id=$2)".len())
                        .any(|bytes| bytes == b") record WHERE record.kind=$1 AND ($2::uuid IS NULL OR record.person_id=$2)")
                    || message
                        .windows(b"FROM access.assignments assignment WHERE ($1::uuid IS NULL".len())
                        .any(|bytes| bytes == b"FROM access.assignments assignment WHERE ($1::uuid IS NULL")
                    || message
                        .windows(b") record WHERE record.kind='assignment' AND ($1::uuid IS NULL".len())
                        .any(|bytes| bytes == b") record WHERE record.kind='assignment' AND ($1::uuid IS NULL")
                    || message
                        .windows(b"FROM access.grants state WHERE ($1::uuid IS NULL OR state.person_id=$1)".len())
                        .any(|bytes| bytes == b"FROM access.grants state WHERE ($1::uuid IS NULL OR state.person_id=$1)")
                    || message
                        .windows(b"WITH selected AS MATERIALIZED (SELECT id FROM access.grants WHERE".len())
                        .any(|bytes| bytes == b"WITH selected AS MATERIALIZED (SELECT id FROM access.grants WHERE")
                    || message
                        .windows(b"WITH selected AS MATERIALIZED (SELECT suspension.id FROM access.suspensions".len())
                        .any(|bytes| bytes == b"WITH selected AS MATERIALIZED (SELECT suspension.id FROM access.suspensions")
                    || message.windows(b"SELECT history.id, history.person_id".len()).any(|bytes| bytes == b"SELECT history.id, history.person_id")
                    || message
                        .windows(b"WITH selected AS MATERIALIZED (SELECT resumption.id FROM access.resumptions".len())
                        .any(|bytes| bytes == b"WITH selected AS MATERIALIZED (SELECT resumption.id FROM access.resumptions")
                    || message
                        .windows(b") record WHERE record.kind='suspension' AND record.account_id=$1 ORDER BY record.sequence DESC, record.id DESC LIMIT $2 OFFSET $3\0".len())
                        .any(|bytes| bytes == b") record WHERE record.kind='suspension' AND record.account_id=$1 ORDER BY record.sequence DESC, record.id DESC LIMIT $2 OFFSET $3\0"))
            {
                23
            } else if message[0] == b'P'
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
            } else if message[0] == b'P'
                && message
                    .windows(b"FROM identity.uploads WHERE $1::uuid IS NULL".len())
                    .any(|bytes| bytes == b"FROM identity.uploads WHERE $1::uuid IS NULL")
            {
                52
            } else if message[0] == b'P'
                && message
                    .windows(b"SELECT state.id,state.account_id,state.authentication_id,state.receipt_id,".len())
                    .any(|bytes| bytes == b"SELECT state.id,state.account_id,state.authentication_id,state.receipt_id,")
                && message
                    .windows(b"FROM identity.sessions state WHERE state.id = $1".len())
                    .any(|bytes| bytes == b"FROM identity.sessions state WHERE state.id = $1")
            {
                11
            } else {
                0
            };
            if stalled != 0
                && control
                    .fault
                    .compare_exchange(stalled, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                if matches!(stalled, 28 | 32..=36 | 38 | 39 | 44..=48 | 52) {
                    // Keep PostgreSQL's idle timeout from racing the read deadline.
                    stalled_query.store(true, Ordering::SeqCst);
                }
                control.pause(&backend_pid).await?;
                if matches!(stalled, 28 | 32..=36 | 38 | 39 | 44..=48 | 52) {
                    stalled_query.store(false, Ordering::SeqCst);
                    resumed.notify_one();
                }
            }
            if message[0] == b'Q' && &message[5..] == b"COMMIT\0" {
                if changed
                    && control
                        .fault
                        .compare_exchange(53, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    // Delay a write decision while preserving the caller's live connection.
                    control.pause(&backend_pid).await?;
                }
                let fault = control.fault.load(Ordering::SeqCst);
                if !changed
                    && (coordinated || listing || keys)
                    && control
                        .fault
                        .compare_exchange(20, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    // Complete the read-only transaction before holding its response.
                    let released = control.release.notified();
                    stalled_completion.store(true, Ordering::SeqCst);
                    commands.write_all(&message).await?;
                    commands.flush().await?;
                    released.await;
                    close.notify_one();
                    return Ok(());
                }
                // Proof preflight commits no coordinated business transaction.
                if (changed && matches!(fault, 1..=3 | 7 | 8)
                    || !changed && (coordinated || listing || keys) && fault == 9
                    || !changed && coordinated && fault == 25
                    || clock && fault == 12
                    || keys && fault == 15
                    || history && fault == 19)
                    && control
                        .fault
                        .compare_exchange(fault, 0, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    // Callers can release after observing the frontend failure.
                    let released = control.release.notified();
                    tokio::pin!(released);
                    if fault == 8 {
                        // Fail the independent recovery BEGIN while retaining the
                        // original backend's ability to commit after release.
                        control.fault.store(6, Ordering::SeqCst);
                    }
                    committing.store(!matches!(fault, 3 | 7), Ordering::SeqCst);
                    if fault != 7 {
                        close.notify_one();
                        // End frontend forwarding before PostgreSQL can reply to COMMIT.
                        // The single interception below must not race a late success frame.
                        closed.notified().await;
                    }
                    control.intercept(&backend_pid)?;
                    if matches!(fault, 2 | 7 | 8 | 15 | 25) {
                        released.as_mut().await;
                    }
                    if fault == 7 {
                        close.notify_one();
                        closed.notified().await;
                    }
                    if matches!(fault, 3 | 7) {
                        return Ok::<_, io::Error>(());
                    }
                    commands.write_all(&message).await?;
                    commands.flush().await?;
                    if matches!(fault, 1 | 9 | 12 | 19) {
                        released.await;
                    }
                    return Ok(());
                }
                changed = false;
                coordinated = false;
                active = false;
                clock = false;
                keys = false;
                migration = false;
                history = false;
                listing = false;
                delayed = false;
            }
            if message[0] == b'Q' && &message[5..] == b"ROLLBACK\0" {
                active = false;
                migration = false;
                history = false;
                listing = false;
                delayed = false;
            }
            commands.write_all(&message).await?;
            commands.flush().await?;
        }
    };
    let incoming = async {
        let mut completed = Vec::new();
        let mut disconnected = false;
        let mut commit_finished = false;
        loop {
            let message = {
                let message = frame(&mut replies);
                tokio::pin!(message);
                tokio::select! {
                    result = &mut message => result,
                    _ = close.notified(), if !disconnected => {
                        // End the frontend without cancelling a partially read backend frame.
                        let _ = output.shutdown().await;
                        closed.notify_one();
                        if !committing.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        disconnected = true;
                        message.await
                    }
                }
            };
            if disconnected {
                let Ok(message) = message else {
                    // A test may terminate the backend to force rollback. Let the
                    // outgoing fault consume its release before this relay ends.
                    return Ok(());
                };
                commit_finished |= message[0] == b'E'
                    || message[0] == b'C' && matches!(&message[5..], b"COMMIT\0" | b"ROLLBACK\0");
                if commit_finished && message[0] == b'Z' {
                    // COMMIT is complete; the retained backend can now end independently.
                    return Ok(());
                }
                continue;
            }
            while stalled_query.load(Ordering::SeqCst) {
                resumed.notified().await;
            }
            let message = message?;
            if paused_authentication_lookup.load(Ordering::SeqCst)
                && (matches!(message[0], b'D' | b'C') || !completed.is_empty())
            {
                completed.extend_from_slice(&message);
                if message[0] != b'Z' {
                    continue;
                }
                // The row has been read, but its caller has not authenticated it or taken a lock.
                control.pause(&backend_pid).await?;
                output.write_all(&completed).await?;
                output.flush().await?;
                completed.clear();
                paused_authentication_lookup.store(false, Ordering::SeqCst);
                continue;
            }
            if message[0] == b'K' {
                // BackendKeyData identifies this authenticated TLS connection.
                // Forward its cancellation secret unchanged without retaining it.
                let mut bytes = [0; 4];
                bytes.copy_from_slice(message.get(5..9).ok_or(io::ErrorKind::InvalidData)?);
                let pid = i32::from_be_bytes(bytes);
                if pid <= 0 || backend_pid.set(pid).is_err() {
                    return Err(io::ErrorKind::InvalidData.into());
                }
            }
            if paused_read_end.load(Ordering::SeqCst)
                && (message[0] == b'C' && matches!(&message[5..], b"COMMIT\0" | b"ROLLBACK\0")
                    || !completed.is_empty())
            {
                completed.extend_from_slice(&message);
                if message[0] != b'Z' {
                    continue;
                }
                // The database has ended the snapshot, but the caller has no receipt yet.
                control.pause(&backend_pid).await?;
                output.write_all(&completed).await?;
                output.flush().await?;
                completed.clear();
                paused_read_end.store(false, Ordering::SeqCst);
                continue;
            }
            if termination_response.load(Ordering::SeqCst)
                && (message[0] == b'C'
                    && matches!(&message[5..], b"COMMIT\0" | b"ROLLBACK\0" | b"SELECT 1\0")
                    || !completed.is_empty())
            {
                completed.extend_from_slice(&message);
                if message[0] != b'Z' {
                    continue;
                }
                // Preserve PostgreSQL's complete success response, then inject a synthetic FATAL.
                // A single small TLS write lets a current-thread driver process both before
                // the caller can observe success and request cancellation.
                let fields = b"SFATAL\0VFATAL\0C08006\0Mfictional-sensitive-termination-detail\0\0";
                completed.push(b'E');
                completed.extend_from_slice(
                    &u32::try_from(fields.len() + 4)
                        .map_err(integer)?
                        .to_be_bytes(),
                );
                completed.extend_from_slice(fields);
                output.write_all(&completed).await?;
                output.flush().await?;
                control.intercept(&backend_pid)?;
                return Ok(());
            }
            if stalled_completion.load(Ordering::SeqCst)
                && message[0] == b'C'
                && matches!(&message[5..], b"ROLLBACK\0" | b"COMMIT\0")
            {
                control.intercept(&backend_pid)?;
                close.notified().await;
                let result = output.shutdown().await;
                closed.notify_one();
                return result;
            }
            output.write_all(&message).await?;
            output.flush().await?;
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

fn integer(error: std::num::TryFromIntError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
