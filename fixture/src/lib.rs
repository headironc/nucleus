//! Shared disposable PostgreSQL for workspace tests.

#![allow(clippy::expect_used)]

pub mod database_proxy;
pub mod supplier;

use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Seek, SeekFrom},
    mem::ManuallyDrop,
    net::{SocketAddr, TcpStream},
    ops::Deref,
    os::unix::fs::{MetadataExt as _, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use postgresql_embedded::{VersionReq, blocking::PostgreSQL};
use rcgen::generate_simple_self_signed;
use rustls_pki_types::CertificateDer;
use tempfile::TempDir;

pub const PASSWORD: &str = "fictional-deployment-password";

const VERSION: &str = include_str!("../postgresql.version");
const STARTUP_LOG_LIMIT: u64 = 16 * 1024;

fn installation() -> PathBuf {
    let version = VERSION.trim();
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/postgresql")
        .join(version);
    let output = Command::new(directory.join("bin/postgres"))
        .arg("--version")
        .output()
        .expect("run `bun fixture/postgresql.ts` before database tests");
    assert!(
        output.status.success(),
        "fixture PostgreSQL must run with the host libraries"
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("PostgreSQL version output")
            .trim(),
        format!("postgres (PostgreSQL) {version}")
    );
    directory
}

pub struct Server {
    postgres: PostgreSQL,
    certificate: CertificateDer<'static>,
    directory: TempDir,
}

/// A server whose foreground supervisor must explicitly confirm stop.
/// Unwinding preserves the cluster for that supervisor's identity-checked recovery.
pub struct Supervised {
    server: ManuallyDrop<Server>,
}

impl Supervised {
    pub fn start() -> Self {
        let mut owner = Self {
            server: ManuallyDrop::new(Server::prepare()),
        };
        if owner.server.postgres.start().is_err() {
            panic!(
                "supervised PostgreSQL startup failed; retained {}",
                owner.server.directory.path().display()
            );
        }
        owner
    }

    pub fn stop(self) -> Result<(), String> {
        ManuallyDrop::into_inner(self.server).stop()
    }

    /// Keep the cluster when protected work has not confirmed termination.
    pub fn retain(self) -> PathBuf {
        self.server.directory.path().to_path_buf()
    }
}

impl Deref for Supervised {
    type Target = Server;
    fn deref(&self) -> &Self::Target {
        &self.server
    }
}

fn owned_postgres(directory: &Path, executable: &Path) -> io::Result<bool> {
    let owner = directory.metadata()?.uid();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.uid() != owner {
            continue;
        }
        let actual = match fs::read_link(entry.path().join("exe")) {
            Ok(actual) => actual,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if actual != executable {
            continue;
        }
        let cwd = match fs::read_link(entry.path().join("cwd")) {
            Ok(cwd) => cwd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if cwd.starts_with(directory) {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Server {
    /// Start an isolated server; the test suite owns sharing and preparation.
    pub fn start() -> Self {
        Self::prepare().launch()
    }

    fn prepare() -> Self {
        let installation_dir = installation();
        let directory = TempDir::new().expect("fixture directory");
        let path = directory.path();
        let certificate =
            generate_simple_self_signed(vec!["127.0.0.1".into()]).expect("certificate");
        fs::write(path.join("server.crt"), certificate.cert.pem()).expect("certificate file");
        fs::write(
            path.join("server.key"),
            certificate.signing_key.serialize_pem(),
        )
        .expect("key file");
        fs::set_permissions(path.join("server.key"), fs::Permissions::from_mode(0o600))
            .expect("permissions");
        let configuration = HashMap::from([
            ("listen_addresses".into(), "127.0.0.1".into()),
            ("ssl".into(), "on".into()),
            (
                "ssl_cert_file".into(),
                path.join("server.crt").to_string_lossy().into_owned(),
            ),
            (
                "ssl_key_file".into(),
                path.join("server.key").to_string_lossy().into_owned(),
            ),
            (
                "unix_socket_directories".into(),
                path.to_string_lossy().into_owned(),
            ),
        ]);
        let mut postgres = PostgreSQL::new(postgresql_embedded::Settings {
            releases_url: String::new(),
            version: VersionReq::parse(&format!("={}.0", VERSION.trim())).expect("version"),
            installation_dir,
            password_file: path.join("password"),
            data_dir: path.join("data"),
            host: "127.0.0.1".into(),
            port: 0,
            username: "postgres".into(),
            password: PASSWORD.into(),
            temporary: true,
            timeout: Some(Duration::from_secs(30)),
            configuration,
            trust_installation_dir: true,
            socket_dir: None,
        });
        postgres.setup().expect("isolated PostgreSQL setup");
        fs::write(
            postgres.settings().data_dir.join("pg_hba.conf"),
            "hostssl all all 127.0.0.1/32 scram-sha-256\nhost all all 127.0.0.1/32 reject\n",
        )
        .expect("TLS policy");
        Self {
            postgres,
            certificate: certificate.cert.der().clone(),
            directory,
        }
    }

    fn launch(mut self) -> Self {
        if let Err(error) = self.postgres.start() {
            // Capture diagnostics before unwinding removes the temporary cluster.
            let settings = self.postgres.settings();
            let log_path = settings.data_dir.join("start.log");
            let log = match startup_log(&log_path) {
                Ok(log) => log,
                Err(error) => format!("[startup log unavailable: {error:?}]"),
            };
            let error = format!("{error:?}").replace(PASSWORD, "[redacted]");
            panic!(
                "isolated PostgreSQL start failed\nversion: {}\naddress: {}:{}\ncluster: {}\nsource: {error}\n{} (up to {STARTUP_LOG_LIMIT} trailing bytes):\n{log}",
                VERSION.trim(),
                settings.host,
                settings.port,
                settings.data_dir.display(),
                log_path.display(),
            );
        }
        self
    }

    /// A private directory and its actual filesystem identity, removed with this server.
    pub fn files(&self) -> (PathBuf, PathBuf, u64) {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(self.directory.path())
            .expect("private file directory")
            .keep();
        let device = root.metadata().expect("file directory device").dev();
        let mut mount = root.clone();
        while let Some(parent) = mount.parent() {
            if parent.metadata().expect("mount ancestor").dev() != device {
                break;
            }
            mount = parent.to_path_buf();
        }
        (root, mount, device)
    }

    pub fn port(&self) -> u16 {
        self.postgres.settings().port
    }

    pub fn certificate(&self) -> &CertificateDer<'static> {
        &self.certificate
    }

    pub fn authority(&self) -> PathBuf {
        self.directory.path().join("server.crt")
    }

    /// Stop this fixture's owned server and verify that its listener and directory ended.
    ///
    /// `Drop` keeps the existing convenience for shared tests, but it cannot report a failed
    /// stop or prove that a listener was left behind. External supervisors use this readback.
    pub fn stop(self) -> Result<(), String> {
        let Self {
            postgres,
            certificate: _certificate,
            directory,
        } = self;
        let path = directory.path().to_path_buf();
        let address = SocketAddr::from(([127, 0, 0, 1], postgres.settings().port));
        let result = (|| {
            let executable = postgres
                .settings()
                .installation_dir
                .join("bin/postgres")
                .canonicalize()
                .map_err(|_| "PostgreSQL executable identity failed")?;
            let pid = fs::read_to_string(postgres.settings().data_dir.join("postmaster.pid"))
                .map_err(|_| "PostgreSQL PID identity failed")?;
            let pid: u32 = pid
                .lines()
                .next()
                .ok_or("PostgreSQL PID missing")?
                .parse()
                .map_err(|_| "PostgreSQL PID invalid")?;
            let process = PathBuf::from(format!("/proc/{pid}"));
            if fs::read_link(process.join("exe"))
                .map_err(|_| "PostgreSQL process identity failed")?
                != executable
                || fs::read_link(process.join("cwd"))
                    .map_err(|_| "PostgreSQL data identity failed")?
                    != postgres.settings().data_dir
            {
                return Err("PostgreSQL owner identity changed");
            }
            postgres.stop().map_err(|_| "PostgreSQL stop failed")?;
            if postgres.status() != postgresql_embedded::Status::Stopped {
                return Err("PostgreSQL retained a running status");
            }
            if process.exists()
                || owned_postgres(&path, &executable)
                    .map_err(|_| "PostgreSQL process readback failed")?
            {
                return Err("PostgreSQL processes remain in the owned directory");
            }
            match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => Ok(()),
                _ => Err("PostgreSQL listener termination is unconfirmed"),
            }
        })();
        if let Err(error) = result {
            // The library's destructor retries stop and deletes data without reporting errors.
            // Unknown termination must preserve both the live owner and recovery material.
            std::mem::forget(postgres);
            let retained = directory.keep();
            return Err(format!("{error}; retained {}", retained.display()));
        }
        drop(postgres);
        directory
            .close()
            .map_err(|_| format!("PostgreSQL directory cleanup failed: {}", path.display()))?;
        Ok(())
    }

    fn key(&self) -> Vec<u8> {
        use rustls_pki_types::{PrivateKeyDer, pem::PemObject};
        PrivateKeyDer::from_pem_file(self.directory.path().join("server.key"))
            .expect("this fixture's generated key")
            .secret_der()
            .to_vec()
    }
}

fn startup_log(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let offset = file.metadata()?.len().saturating_sub(STARTUP_LOG_LIMIT);
    let incomplete = if offset > 0 {
        file.seek(SeekFrom::Start(offset - 1))?;
        let mut previous = [0];
        file.read_exact(&mut previous)?;
        previous[0] != b'\n'
    } else {
        false
    };
    let mut bytes = Vec::new();
    file.take(STARTUP_LOG_LIMIT).read_to_end(&mut bytes)?;
    let mut log = String::new();
    let mut tail = bytes.as_slice();
    if offset > 0 {
        log.push_str("[earlier startup output omitted]\n");
    }
    if incomplete {
        // Omit the partial first line, including any credential cut by the byte limit.
        tail = match tail.iter().position(|byte| *byte == b'\n') {
            Some(end) => &tail[end + 1..],
            None => &[],
        };
    }
    log.push_str(&String::from_utf8_lossy(tail).replace(PASSWORD, "[redacted]"));
    Ok(log)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{SocketAddr, TcpStream},
        panic::{AssertUnwindSafe, catch_unwind},
        process::Command,
        time::Duration,
    };

    use tempfile::TempDir;

    use crate::{PASSWORD, Server, installation, startup_log};

    #[test]
    fn failed_start_reports_the_cause_before_removing_the_cluster() {
        let server = Server::prepare();
        let directory = server.directory.path().to_path_buf();
        fs::write(
            server
                .postgres
                .settings()
                .data_dir
                .join("postgresql.auto.conf"),
            format!("lc_messages = 'C'\nshared_buffers = '{PASSWORD}'\n"),
        )
        .expect("invalid configuration in this test's cluster");

        let panic = catch_unwind(AssertUnwindSafe(move || {
            server.launch();
        }))
        .expect_err("PostgreSQL must reject the invalid shared_buffers value");
        let message = panic.downcast_ref::<String>().expect("startup diagnostic");
        assert!(
            message.contains("DatabaseStartError"),
            "original error: {message}"
        );
        assert!(
            message.contains("invalid value for parameter") && message.contains("shared_buffers"),
            "PostgreSQL's actual failure: {message}"
        );
        assert!(message.contains("version: 18.4"));
        assert!(message.contains(&directory.join("data/start.log").display().to_string()));
        assert!(message.contains("[redacted]"));
        assert!(!message.contains(PASSWORD));
        let address: SocketAddr = message
            .lines()
            .find_map(|line| line.strip_prefix("address: "))
            .expect("selected address")
            .parse()
            .expect("loopback socket address");
        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0);
        assert!(
            !directory.exists(),
            "failed startup removes its temporary files"
        );
        assert!(TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn unavailable_startup_logs_preserve_the_original_failure() {
        for missing in [true, false] {
            let server = Server::prepare();
            let directory = server.directory.path().to_path_buf();
            if missing {
                fs::rename(
                    &server.postgres.settings().data_dir,
                    directory.join("removed-data"),
                )
                .expect("move only this test's cluster out of the startup path");
            } else {
                fs::create_dir(server.postgres.settings().data_dir.join("start.log"))
                    .expect("make the startup log unreadable as a file");
            }
            let panic = catch_unwind(AssertUnwindSafe(move || {
                server.launch();
            }))
            .expect_err("the prepared cluster cannot start");
            let message = panic.downcast_ref::<String>().expect("startup diagnostic");
            assert!(
                message.contains("DatabaseStartError"),
                "original error: {message}"
            );
            assert!(
                message.contains("startup log unavailable:"),
                "log error: {message}"
            );
            assert!(
                !directory.exists(),
                "failed startup still removes its files"
            );
        }
    }

    #[test]
    fn startup_log_keeps_a_bounded_tail_without_partial_credentials() {
        let directory = TempDir::new().expect("log directory");
        let path = directory.path().join("start.log");
        // Discard a partial first line and tolerate non-UTF-8 process output.
        let mut bytes = format!(
            "discarded header\n{}{PASSWORD}\nFATAL: invalid value {PASSWORD}\nfinal cause: ",
            "x".repeat(16 * 1024)
        )
        .into_bytes();
        bytes.push(0xff);
        fs::write(&path, bytes).expect("oversized startup log with non-UTF-8 output");
        assert_eq!(
            startup_log(&path).expect("bounded startup diagnostic"),
            "[earlier startup output omitted]\nFATAL: invalid value [redacted]\nfinal cause: \u{fffd}"
        );

        fs::write(&path, "x".repeat(32 * 1024)).expect("oversized single line");
        assert_eq!(
            startup_log(&path).expect("omit an incomplete oversized line"),
            "[earlier startup output omitted]\n"
        );

        let suffix = format!("\n{}", "x".repeat(16 * 1024 - PASSWORD.len() / 2 - 1));
        fs::write(&path, format!("discarded header\n{PASSWORD}{suffix}"))
            .expect("place the byte limit inside the password");
        let log = startup_log(&path).expect("omit the cut credential");
        assert!(!log.contains(&PASSWORD[PASSWORD.len() / 2..]));
        assert!(log.ends_with(&suffix));
    }

    #[test]
    fn startup_log_distinguishes_complete_and_partial_boundary_lines() {
        let directory = TempDir::new().expect("log directory");
        let path = directory.path().join("start.log");
        let cause = "FATAL: could not open configuration file\n";
        for (prefix, partial) in [
            ("", ""),
            ("\n", ""),
            ("discarded line\n", ""),
            ("discarded line\r\n", ""),
            ("discarded", "\n"),
            ("discarded", "\r\n"),
            ("discarded", " partial line\n"),
        ] {
            let retained = format!(
                "{cause}{}",
                "x".repeat(16 * 1024 - partial.len() - cause.len())
            );
            fs::write(&path, format!("{prefix}{partial}{retained}"))
                .expect("place the retained window at the chosen line boundary");
            let expected = if prefix.is_empty() {
                retained
            } else {
                format!("[earlier startup output omitted]\n{retained}")
            };
            assert!(
                startup_log(&path).expect("startup diagnostic") == expected,
                "keep the complete cause after prefix {prefix:?} and partial line {partial:?}"
            );
        }
    }

    #[test]
    fn pinned_server_requires_tls_and_removes_its_temporary_cluster() {
        let server = Server::start();
        let directory = server.directory.path().to_path_buf();
        let address = SocketAddr::from(([127, 0, 0, 1], server.port()));
        let connection = format!(
            "host=127.0.0.1 port={} user=postgres dbname=postgres sslmode=verify-full sslrootcert={} connect_timeout=5",
            server.port(),
            server.authority().display()
        );
        let execute = |connection: &str, sql: &str| {
            Command::new(installation().join("bin/psql"))
                .env_clear()
                .env("PGPASSWORD", PASSWORD)
                .args([
                    "-X",
                    "-At",
                    "-v",
                    "ON_ERROR_STOP=1",
                    "--dbname",
                    connection,
                    "--command",
                    sql,
                ])
                .output()
                .expect("fixture psql")
        };
        let output = execute(
            &connection,
            "SHOW server_version_num; SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
        );
        assert!(output.status.success(), "verified TLS connection");
        assert_eq!(
            String::from_utf8(output.stdout)
                .expect("query result")
                .trim(),
            "180004\nt"
        );
        assert!(
            execute(&connection, "CREATE DATABASE fixture_version")
                .status
                .success()
        );
        assert!(
            execute(
                &connection.replace("dbname=postgres", "dbname=fixture_version"),
                "SELECT 1"
            )
            .status
            .success()
        );
        assert!(
            !execute(
                &connection.replace("sslmode=verify-full", "sslmode=disable"),
                "SELECT 1"
            )
            .status
            .success()
        );
        drop(server);
        assert!(
            !directory.exists(),
            "fixture data is removed after shutdown"
        );
        assert!(TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err());
    }
}
