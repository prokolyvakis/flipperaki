//! Named device operations on top of one [`RpcSession`].
//!
//! Each operation gets one deadline at admission (`RPC_OPERATION_TIMEOUT`,
//! plus the hold time for `hold`). Multi-step operations stop at the first
//! step that is not acknowledged and report that step's outcome.

use std::time::Duration;

use tokio::time::{Instant, sleep};

use crate::limits::{BUDDY_MESSAGE_MAX_BYTES, INPUT_HOLD_MAX, RPC_OPERATION_TIMEOUT};
use crate::rpc::{
    Delivery, Reason, Reply, RequestKind, RpcSession, SessionState, Stage, pb, pb_app, pb_desktop,
    pb_gui, pb_property, pb_storage, pb_system,
};

use pb::main::Content;

const PING_PAYLOAD: &[u8] = b"flipperaki";

/// A device operation did not succeed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("not sent: {0}")]
    NotSent(Reason),
    #[error("rejected by firmware with status {status}")]
    Rejected { status: i32 },
    /// The device may have acted. Do not retry a mutation.
    #[error("outcome unknown ({stage:?}): {reason}")]
    Unknown { reason: Reason, stage: Stage },
    #[error("unexpected reply from device")]
    UnexpectedReply,
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
}

/// Flipper hardware buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
}

impl From<Button> for pb_gui::InputKey {
    fn from(button: Button) -> Self {
        match button {
            Button::Up => Self::Up,
            Button::Down => Self::Down,
            Button::Left => Self::Left,
            Button::Right => Self::Right,
            Button::Ok => Self::Ok,
            Button::Back => Self::Back,
        }
    }
}

/// Key/value pairs of a property group, in device order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Properties(pub Vec<(String, String)>);

impl Properties {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u32,
}

/// The device behind one session. A transport change means a new `Device`.
#[derive(Debug, Clone)]
pub struct Device {
    session: RpcSession,
}

impl Device {
    #[must_use]
    pub fn new(session: RpcSession) -> Self {
        Self { session }
    }

    #[must_use]
    pub fn session(&self) -> &RpcSession {
        &self.session
    }

    pub async fn ping(&self) -> Result<(), DeviceError> {
        let request = Content::SystemPingRequest(pb_system::PingRequest {
            data: PING_PAYLOAD.to_vec(),
        });
        match self.query(request).await?.0.as_slice() {
            [Content::SystemPingResponse(reply)] if reply.data == PING_PAYLOAD => Ok(()),
            _ => Err(DeviceError::UnexpectedReply),
        }
    }

    /// The `devinfo` property group (firmware version, build, hardware).
    pub async fn device_info(&self) -> Result<Properties, DeviceError> {
        let request = Content::PropertyGetRequest(pb_property::GetRequest {
            key: "devinfo".to_owned(),
        });
        self.query(request)
            .await?
            .0
            .into_iter()
            .map(|part| match part {
                Content::PropertyGetResponse(pair) => Ok((pair.key, pair.value)),
                _ => Err(DeviceError::UnexpectedReply),
            })
            .collect::<Result<_, _>>()
            .map(Properties)
    }

    /// Whether the desktop is PIN-locked. The firmware answers in the
    /// status: `OK` means locked, `ERROR` means unlocked.
    pub async fn lock_status(&self) -> Result<bool, DeviceError> {
        let request = Content::DesktopIsLockedRequest(pb_desktop::IsLockedRequest {});
        match self.call(request, RequestKind::Query, deadline()).await {
            Ok(_) => Ok(true),
            Err(DeviceError::Rejected { status }) if status == pb::CommandStatus::Error as i32 => {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Lock changes then arrive as `DeviceEvent::Lock`.
    pub async fn subscribe_desktop_status(&self) -> Result<(), DeviceError> {
        let request = Content::DesktopStatusSubscribeRequest(pb_desktop::StatusSubscribeRequest {});
        self.query(request).await.map(drop)
    }

    /// Frames then arrive as `DeviceEvent::Screen`.
    pub async fn start_screen_stream(&self) -> Result<(), DeviceError> {
        let request = Content::GuiStartScreenStreamRequest(pb_gui::StartScreenStreamRequest {});
        self.query(request).await.map(drop)
    }

    pub async fn stop_screen_stream(&self) -> Result<(), DeviceError> {
        let request = Content::GuiStopScreenStreamRequest(pb_gui::StopScreenStreamRequest {});
        self.query(request).await.map(drop)
    }

    /// Press, short, release.
    pub async fn tap(&self, button: Button) -> Result<(), DeviceError> {
        let deadline = deadline();
        for kind in [
            pb_gui::InputType::Press,
            pb_gui::InputType::Short,
            pb_gui::InputType::Release,
        ] {
            self.input(button, kind, deadline).await?;
        }
        Ok(())
    }

    /// Press, wait `duration`, long, release. The release is only sent while
    /// this session is still open; a failure ends the sequence.
    pub async fn hold(&self, button: Button, duration: Duration) -> Result<(), DeviceError> {
        if duration > INPUT_HOLD_MAX {
            return Err(DeviceError::InvalidArgument("hold exceeds INPUT_HOLD_MAX"));
        }
        let deadline = deadline() + duration;
        self.input(button, pb_gui::InputType::Press, deadline)
            .await?;
        let mut state = self.session.state();
        tokio::select! {
            () = sleep(duration) => {}
            ended = state.wait_for(|state| *state != SessionState::Open) => {
                let reason = match ended.as_deref() {
                    Ok(SessionState::Failed(failure)) => Reason::Failed(failure.clone()),
                    _ => Reason::Closed,
                };
                return Err(DeviceError::NotSent(reason));
            }
        }
        self.input(button, pb_gui::InputType::Long, deadline)
            .await?;
        self.input(button, pb_gui::InputType::Release, deadline)
            .await
    }

    pub async fn app_start(&self, name: &str, args: &str) -> Result<(), DeviceError> {
        let request = Content::AppStartRequest(pb_app::StartRequest {
            name: name.to_owned(),
            args: args.to_owned(),
        });
        self.mutate(request).await
    }

    pub async fn app_exit(&self) -> Result<(), DeviceError> {
        self.mutate(Content::AppExitRequest(pb_app::AppExitRequest {}))
            .await
    }

    /// Whether an application holds the loader lock.
    pub async fn app_lock_status(&self) -> Result<bool, DeviceError> {
        let request = Content::AppLockStatusRequest(pb_app::LockStatusRequest {});
        match self.query(request).await?.0.as_slice() {
            [Content::AppLockStatusResponse(reply)] => Ok(reply.locked),
            _ => Err(DeviceError::UnexpectedReply),
        }
    }

    /// Sends bytes to the running FAP.
    pub async fn app_data_exchange(&self, data: &[u8]) -> Result<(), DeviceError> {
        if data.len() > BUDDY_MESSAGE_MAX_BYTES {
            return Err(DeviceError::InvalidArgument("app data exceeds 512 bytes"));
        }
        let request = Content::AppDataExchangeRequest(pb_app::DataExchangeRequest {
            data: data.to_vec(),
        });
        self.mutate(request).await
    }

    /// Directory listing, e.g. `/ext/apps` for app discovery. Total size is
    /// bounded by the session's multipart response limit.
    pub async fn storage_list(&self, path: &str) -> Result<Vec<StorageEntry>, DeviceError> {
        let request = Content::StorageListRequest(pb_storage::ListRequest {
            path: path.to_owned(),
            include_md5: false,
            filter_max_size: 0,
        });
        let mut entries = Vec::new();
        for part in self.query(request).await?.0 {
            let Content::StorageListResponse(list) = part else {
                return Err(DeviceError::UnexpectedReply);
            };
            entries.extend(list.file.into_iter().map(|file| StorageEntry {
                kind: match file.r#type() {
                    pb_storage::file::FileType::Dir => EntryKind::Dir,
                    pb_storage::file::FileType::File => EntryKind::File,
                },
                name: file.name,
                size: file.size,
            }));
        }
        Ok(entries)
    }

    async fn input(
        &self,
        button: Button,
        kind: pb_gui::InputType,
        deadline: Instant,
    ) -> Result<(), DeviceError> {
        let request = Content::GuiSendInputEventRequest(pb_gui::SendInputEventRequest {
            key: pb_gui::InputKey::from(button) as i32,
            r#type: kind as i32,
        });
        self.call(request, RequestKind::Mutation, deadline)
            .await
            .map(drop)
    }

    async fn query(&self, content: Content) -> Result<Reply, DeviceError> {
        self.call(content, RequestKind::Query, deadline()).await
    }

    async fn mutate(&self, content: Content) -> Result<(), DeviceError> {
        self.call(content, RequestKind::Mutation, deadline())
            .await
            .map(drop)
    }

    async fn call(
        &self,
        content: Content,
        kind: RequestKind,
        deadline: Instant,
    ) -> Result<Reply, DeviceError> {
        match self.session.request(content, kind, deadline).await {
            Delivery::Acknowledged(reply) => Ok(reply),
            Delivery::NotSent(reason) => Err(DeviceError::NotSent(reason)),
            Delivery::Rejected { status } => Err(DeviceError::Rejected { status }),
            Delivery::Unknown { reason, stage } => Err(DeviceError::Unknown { reason, stage }),
        }
    }
}

fn deadline() -> Instant {
    Instant::now() + RPC_OPERATION_TIMEOUT
}
