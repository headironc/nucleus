//! Closed, run-bound private control channel for the native harness.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    time::timeout,
};
use uuid::Uuid;

use super::relay::Case;

const LINE_LIMIT: usize = 64 * 1024;
pub(super) const COMMAND_LIMIT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Command {
    Hello {
        run: Uuid,
        client_revision: String,
        client_build: String,
    },
    Arm {
        run: Uuid,
        case: Case,
        operation: Option<Uuid>,
        actor: Option<Uuid>,
        target: Option<Uuid>,
        registration: Option<Uuid>,
    },
    Snapshot {
        run: Uuid,
    },
    Checkpoint {
        run: Uuid,
        case: Case,
        operation: Option<Uuid>,
        actor: Option<Uuid>,
        target: Option<Uuid>,
        later_session: Option<Uuid>,
        foreign_session: Option<Uuid>,
    },
    Finish {
        run: Uuid,
        #[serde(default)]
        observations: Observations,
    },
    Abort {
        run: Uuid,
        reason: AbortReason,
    },
}

impl Command {
    pub(super) fn run(&self) -> Uuid {
        match self {
            Self::Hello { run, .. }
            | Self::Arm { run, .. }
            | Self::Snapshot { run }
            | Self::Checkpoint { run, .. }
            | Self::Finish { run, .. }
            | Self::Abort { run, .. } => *run,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observations {
    #[serde(default)]
    pub(super) avatar_read: bool,
    #[serde(default)]
    pub(super) registration_recovered: bool,
    #[serde(default)]
    pub(super) current_503_retained: bool,
    #[serde(default)]
    pub(super) later_login_used: bool,
    #[serde(default)]
    pub(super) other_account_isolated: bool,
    #[serde(default)]
    pub(super) revocation_reads_read_only: bool,
    #[serde(default)]
    pub(super) registration_retry_no_extra_exchange: bool,
}

impl Observations {
    pub(super) fn evidence(&self) -> Value {
        json!({
            "avatarRead": self.avatar_read,
            "registrationRecovered": self.registration_recovered,
            "current503Retained": self.current_503_retained,
            "laterLoginUsed": self.later_login_used,
            "otherAccountIsolated": self.other_account_isolated,
            "revocationReadsReadOnly": self.revocation_reads_read_only,
            "registrationRetryNoExtraExchange": self.registration_retry_no_extra_exchange,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AbortReason {
    ClientFailure,
    ClientTimeout,
    ControlFailure,
    OperatorStop,
}

impl AbortReason {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ClientFailure => "client_failure",
            Self::ClientTimeout => "client_timeout",
            Self::ControlFailure => "control_failure",
            Self::OperatorStop => "operator_stop",
        }
    }
}

pub(super) enum ReadError {
    Disconnected,
    Timeout,
    Malformed,
    Io(String),
}

pub(super) struct Control {
    path: PathBuf,
    listener: UnixListener,
    reader: Option<BufReader<UnixStream>>,
}

impl Control {
    pub(super) fn bind(path: &Path) -> Result<Self, String> {
        if path.exists() {
            return Err("private control path already exists".into());
        }
        let listener = UnixListener::bind(path)
            .map_err(|error| format!("private control bind failed: {error}"))?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("private control permissions failed: {error}"))?;
        Ok(Self {
            path: path.to_path_buf(),
            listener,
            reader: None,
        })
    }

    pub(super) async fn accept(&mut self, budget: Duration) -> Result<(), ReadError> {
        let (stream, _) = timeout(budget, self.listener.accept())
            .await
            .map_err(|_| ReadError::Timeout)?
            .map_err(|error| ReadError::Io(format!("private control accept failed: {error}")))?;
        self.reader = Some(BufReader::new(stream));
        Ok(())
    }

    pub(super) async fn read(&mut self) -> Result<Command, ReadError> {
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| ReadError::Io("private control is not connected".into()))?;
        let line = timeout(COMMAND_LIMIT, read_line(reader))
            .await
            .map_err(|_| ReadError::Timeout)??;
        let line = std::str::from_utf8(&line).map_err(|_| ReadError::Malformed)?;
        let line = line.trim();
        if line.is_empty() {
            return Err(ReadError::Malformed);
        }
        serde_json::from_str(line).map_err(|_| ReadError::Malformed)
    }

    pub(super) async fn respond(&mut self, value: &Value) -> Result<(), String> {
        let Some(reader) = self.reader.as_mut() else {
            return Err("private control is not connected".into());
        };
        let mut line = serde_json::to_vec(value)
            .map_err(|_| "private control response serialization failed".to_owned())?;
        line.push(b'\n');
        timeout(Duration::from_secs(5), reader.get_mut().write_all(&line))
            .await
            .map_err(|_| "private control response write timed out".to_owned())?
            .map_err(|_| "private control response write failed".to_owned())
    }

    pub(super) async fn reject(&mut self, code: &str) -> Result<(), String> {
        self.respond(&json!({"ok": false, "code": code})).await
    }

    pub(super) async fn close(mut self) -> Result<(), String> {
        if let Some(reader) = self.reader.take() {
            let mut stream = reader.into_inner();
            timeout(Duration::from_secs(5), stream.shutdown())
                .await
                .map_err(|_| "private control shutdown timed out")?
                .map_err(|_| "private control shutdown failed")?;
        }
        drop(self.listener);
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("private control cleanup failed: {error}")),
        }
    }
}

async fn read_line(reader: &mut BufReader<UnixStream>) -> Result<Vec<u8>, ReadError> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| ReadError::Io("private control read failed".into()))?;
        if available.is_empty() {
            return if line.is_empty() {
                Err(ReadError::Disconnected)
            } else {
                Err(ReadError::Malformed)
            };
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            if line.len() + position > LINE_LIMIT {
                return Err(ReadError::Malformed);
            }
            line.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            return Ok(line);
        }
        if line.len() + available.len() > LINE_LIMIT {
            return Err(ReadError::Malformed);
        }
        line.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}
