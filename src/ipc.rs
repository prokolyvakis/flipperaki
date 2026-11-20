//! Private local IPC between the daemon (sole device owner) and its clients:
//! the MCP server, the hook command and operator commands.
//!
//! Newline-delimited JSON over a Unix socket in a 0700 directory. Each
//! connection gets a server-assigned [`ClientId`]; nothing a client sends can
//! choose or change its identity.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{Framed, LinesCodec};

use crate::broker::{ClientId, Denial, InputStep};
use crate::buddy::ActivityReport;
use crate::limits::IPC_LINE_MAX_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Activity { report: ActivityReport },
    Status,
    Apps,
    Open { app: String },
    Observe,
    Input { steps: Vec<InputStep> },
    Finish { mode: FinishMode },
    Pause { paused: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FinishMode {
    /// Stop agent input and leave the target app running.
    LeaveRunning,
    /// Close the target through a supported path and relaunch Buddy.
    ReturnToBuddy,
}

/// How far a device mutation got. Never upgraded by guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "delivery", content = "detail")]
pub enum Delivery {
    NotSent(String),
    Acknowledged,
    Rejected(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// PNG of the 128x64 screen, base64.
    pub png_base64: String,
    pub epoch: u32,
    pub sequence: u64,
    pub age_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// Exact identifier to pass to `flipper_open`.
    pub id: String,
    pub name: String,
    /// `builtin` (verified catalog) or `installed` (discovered FAP).
    pub source: String,
    /// Workflows that were actually tested on hardware, if any.
    pub tested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub connection: String,
    pub transport: Option<String>,
    pub epoch: Option<u32>,
    pub locked: Option<bool>,
    pub paused: bool,
    pub foreground: String,
    pub control: String,
    pub you_own_control: bool,
    pub firmware: Option<String>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Reply {
    Done,
    Status(StatusReport),
    Apps {
        apps: Vec<AppEntry>,
    },
    Opened {
        delivery: Delivery,
        observation: Option<Observation>,
        next_step: String,
    },
    Observed {
        observation: Observation,
    },
    InputResult {
        delivery: Delivery,
        steps_delivered: u32,
        remaining_actions: u32,
        observation: Option<Observation>,
        next_step: String,
    },
    Finished {
        delivery: Delivery,
        next_step: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case", tag = "error", content = "detail")]
pub enum Failure {
    #[error("{0}")]
    Denied(Denial),
    #[error("device: {0}")]
    Device(String),
    #[error("invalid request: {0}")]
    Invalid(String),
}

pub type Response = Result<Reply, Failure>;

/// One request delivered to the daemon's broker loop.
#[derive(Debug)]
pub enum Inbound {
    Call {
        client: ClientId,
        request: Request,
        reply: oneshot::Sender<Response>,
    },
    /// The connection closed; any workflow it owns must be revoked.
    Gone { client: ClientId },
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("the Flipperaki daemon is not running ({0})")]
    NotRunning(std::io::Error),
    #[error("ipc I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipc framing: {0}")]
    Framing(#[from] tokio_util::codec::LinesCodecError),
    #[error("ipc payload: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the daemon closed the connection")]
    Closed,
}

/// The private per-user runtime directory, created 0700.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("FLIPPERAKI_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("Library/Application Support/flipperaki"))
        })
        .ok_or_else(|| std::io::Error::other("HOME is not set"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&base)?;
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
    Ok(base)
}

pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("daemon.sock")
}

/// Accepts connections forever, forwarding requests into `inbound`.
pub async fn serve(listener: UnixListener, inbound: mpsc::Sender<Inbound>) -> std::io::Result<()> {
    let next_id = Arc::new(AtomicU64::new(1));
    loop {
        let (stream, _) = listener.accept().await?;
        let client = ClientId(next_id.fetch_add(1, Ordering::Relaxed));
        tokio::spawn(serve_client(stream, client, inbound.clone()));
    }
}

/// Binds the socket with owner-only permissions, replacing a stale file.
/// The caller must already hold the daemon lock.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn serve_client(stream: UnixStream, client: ClientId, inbound: mpsc::Sender<Inbound>) {
    let mut lines = Framed::new(stream, LinesCodec::new_with_max_length(IPC_LINE_MAX_BYTES));
    while let Some(line) = lines.next().await {
        let response = match line {
            Err(error) => {
                // Oversized or broken framing: answer once, then drop the client.
                tracing::warn!(client = client.0, %error, "ipc framing error");
                let _ = send(&mut lines, &Err(Failure::Invalid("bad framing".into()))).await;
                break;
            }
            Ok(line) => match serde_json::from_str::<Request>(&line) {
                Err(_) => Err(Failure::Invalid("unrecognized request".into())),
                Ok(request) => {
                    let (reply, answer) = oneshot::channel();
                    if inbound
                        .send(Inbound::Call {
                            client,
                            request,
                            reply,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    answer
                        .await
                        .unwrap_or_else(|_| Err(Failure::Device("daemon stopping".into())))
                }
            },
        };
        if send(&mut lines, &response).await.is_err() {
            break;
        }
    }
    let _ = inbound.send(Inbound::Gone { client }).await;
}

async fn send(
    lines: &mut Framed<UnixStream, LinesCodec>,
    response: &Response,
) -> Result<(), IpcError> {
    lines.send(serde_json::to_string(response)?).await?;
    Ok(())
}

/// One long-lived client connection. Its identity is the connection itself.
pub struct IpcClient {
    lines: Framed<UnixStream, LinesCodec>,
}

impl IpcClient {
    pub async fn connect(path: &Path) -> Result<Self, IpcError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(IpcError::NotRunning)?;
        Ok(Self {
            lines: Framed::new(stream, LinesCodec::new_with_max_length(IPC_LINE_MAX_BYTES)),
        })
    }

    pub async fn call(&mut self, request: &Request) -> Result<Response, IpcError> {
        self.lines.send(serde_json::to_string(request)?).await?;
        let line = self.lines.next().await.ok_or(IpcError::Closed)??;
        Ok(serde_json::from_str(&line)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    async fn echo_daemon(mut inbound: mpsc::Receiver<Inbound>, gone: mpsc::Sender<ClientId>) {
        while let Some(message) = inbound.recv().await {
            match message {
                Inbound::Call { client, reply, .. } => {
                    let _ = reply.send(Ok(Reply::Finished {
                        delivery: Delivery::Acknowledged,
                        next_step: client.0.to_string(),
                    }));
                }
                Inbound::Gone { client } => {
                    let _ = gone.send(client).await;
                }
            }
        }
    }

    #[tokio::test]
    async fn clients_get_distinct_server_assigned_ids_and_departures_are_reported() {
        let dir = std::env::temp_dir().join(format!("flipperaki-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = socket_path(&dir);
        let listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let (tx, rx) = mpsc::channel(8);
        let (gone_tx, mut gone_rx) = mpsc::channel(8);
        tokio::spawn(serve(listener, tx));
        tokio::spawn(echo_daemon(rx, gone_tx));

        let mut a = IpcClient::connect(&path).await.unwrap();
        let mut b = IpcClient::connect(&path).await.unwrap();
        let id = |r: Response| match r {
            Ok(Reply::Finished { next_step, .. }) => next_step,
            other => panic!("unexpected {other:?}"),
        };
        let a_id = id(a.call(&Request::Status).await.unwrap());
        let b_id = id(b.call(&Request::Status).await.unwrap());
        assert_ne!(a_id, b_id);
        assert_eq!(a_id, id(a.call(&Request::Apps).await.unwrap()));

        drop(a);
        assert_eq!(gone_rx.recv().await.unwrap().0.to_string(), a_id);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn oversized_lines_are_rejected_and_the_client_dropped() {
        let dir = std::env::temp_dir().join(format!("flipperaki-ipc-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = socket_path(&dir);
        let (tx, rx) = mpsc::channel(8);
        let (gone_tx, mut gone_rx) = mpsc::channel(8);
        tokio::spawn(serve(bind(&path).unwrap(), tx));
        tokio::spawn(echo_daemon(rx, gone_tx));

        let mut raw = UnixStream::connect(&path).await.unwrap();
        raw.write_all(&vec![b'x'; IPC_LINE_MAX_BYTES + 10])
            .await
            .unwrap();
        raw.write_all(b"\n").await.unwrap();
        assert!(gone_rx.recv().await.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
