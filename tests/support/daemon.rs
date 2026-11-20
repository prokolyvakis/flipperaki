//! An auto-answering fake Flipper running Buddy, driven by the real daemon
//! loop through `daemon::serve_with`. Every host request is recorded.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use flipperaki::broker::ClientId;
use flipperaki::buddy::pb as buddy;
use flipperaki::config::Config;
use flipperaki::daemon;
use flipperaki::ipc::{Inbound, Request, Response, StatusReport};
use flipperaki::rpc::{RpcCodec, encode_frame, pb, pb_app, pb_desktop, pb_gui, pb_property};
use flipperaki::transport::BoxedStream;
use futures::StreamExt;
use prost::Message;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::codec::FramedRead;

use super::Content;

pub const BUDDY_PATH: &str = flipperaki::config::DEFAULT_BUDDY_PATH;
pub const CLOCK: &str = "/ext/apps/Tools/clock.fap";
/// Longest a test waits (virtual time) for the daemon to do something.
const PATIENCE: Duration = Duration::from_secs(20);

/// What the fake device does on its own. Tests flip these to script faults.
#[derive(Debug, Clone)]
pub struct Behaviour {
    /// An app holds the loader (AppStart sets it, AppExit clears it).
    pub loader_locked: bool,
    /// Report AppState STARTED/CLOSED after AppStart/AppExit.
    pub announce_app_state: bool,
    /// Buddy answers the host Hello with its own Hello.
    pub buddy_hello: bool,
    /// Redraw right after the Press is acknowledged, not on Release, and ack
    /// the Release slowly: the frame lands while the input is still in flight
    /// and the screen then stays static (seen over BLE).
    pub redraw_on_press: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            loader_locked: false,
            announce_app_state: true,
            buddy_hello: true,
            redraw_on_press: false,
        }
    }
}

enum Command {
    Emit(Content),
    Close,
}

/// The test's view of one fake device session.
pub struct Fake {
    pub behaviour: Arc<Mutex<Behaviour>>,
    commands: mpsc::UnboundedSender<Command>,
    traffic: mpsc::UnboundedReceiver<Content>,
    /// Every host request taken from `traffic` so far, in order.
    pub seen: Vec<Content>,
}

impl Fake {
    /// Waits for the next host request matching `wanted`, skipping others.
    pub async fn expect(&mut self, what: &str, wanted: impl Fn(&Content) -> bool) -> Content {
        let found = timeout(PATIENCE, async {
            loop {
                let content = self.traffic.recv().await.expect("fake running");
                self.seen.push(content.clone());
                if wanted(&content) {
                    return content;
                }
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("host never sent {what}; saw {:#?}", self.seen))
    }

    /// Records everything the host has sent so far.
    pub fn drain(&mut self) -> &[Content] {
        while let Ok(content) = self.traffic.try_recv() {
            self.seen.push(content);
        }
        &self.seen
    }

    pub fn count(&mut self, wanted: impl Fn(&Content) -> bool) -> usize {
        self.drain().iter().filter(|c| wanted(c)).count()
    }

    /// Sends an unsolicited frame (`command_id == 0`) to the host.
    pub fn emit(&self, content: Content) {
        let _ = self.commands.send(Command::Emit(content));
    }

    /// Buddy sends an envelope to the host.
    pub fn buddy_says(&self, epoch: u32, payload: buddy::envelope::Payload) {
        self.emit(Content::AppDataExchangeRequest(
            pb_app::DataExchangeRequest {
                data: envelope(epoch, payload),
            },
        ));
    }

    pub fn decide(&self, epoch: u32, request_id: u32, verdict: i32) {
        self.buddy_says(
            epoch,
            buddy::envelope::Payload::Decision(buddy::Decision {
                request_id,
                verdict,
            }),
        );
    }

    pub fn lock(&self, locked: bool) {
        self.emit(Content::DesktopStatus(pb_desktop::Status { locked }));
    }

    /// Drops the device end of the pipe, as if the link died.
    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
    }

    pub fn set(&self, change: impl FnOnce(&mut Behaviour)) {
        change(&mut self.behaviour.lock().unwrap());
    }

    /// Waits for the host's next Buddy envelope.
    pub async fn next_buddy(&mut self) -> buddy::Envelope {
        let Content::AppDataExchangeRequest(request) = self
            .expect("a Buddy message", |c| {
                matches!(c, Content::AppDataExchangeRequest(_))
            })
            .await
        else {
            unreachable!()
        };
        buddy::Envelope::decode(request.data.as_slice()).expect("host envelope decodes")
    }

    /// Waits for Buddy to be launched and greeted; returns the epoch.
    pub async fn buddy_up(&mut self) -> u32 {
        self.expect("AppStart(Buddy)", |c| is_app_start(c, BUDDY_PATH))
            .await;
        let hello = self.next_buddy().await;
        assert!(matches!(
            hello.payload,
            Some(buddy::envelope::Payload::Hello(_))
        ));
        hello.epoch
    }

    /// Waits for an ArmRequest.
    pub async fn arm_request(&mut self) -> buddy::ArmRequest {
        loop {
            if let Some(buddy::envelope::Payload::ArmRequest(request)) =
                self.next_buddy().await.payload
            {
                return request;
            }
        }
    }

    /// Waits for an ArmCancel; returns the request id it withdraws.
    pub async fn arm_cancel(&mut self) -> u32 {
        loop {
            if let Some(buddy::envelope::Payload::ArmCancel(cancel)) =
                self.next_buddy().await.payload
            {
                return cancel.request_id;
            }
        }
    }
}

pub fn envelope(epoch: u32, payload: buddy::envelope::Payload) -> Vec<u8> {
    buddy::Envelope {
        protocol_major: 1,
        epoch,
        sequence: 1,
        payload: Some(payload),
    }
    .encode_to_vec()
}

pub fn is_app_start(content: &Content, name: &str) -> bool {
    matches!(content, Content::AppStartRequest(r) if r.name == name)
}

pub fn is_input(content: &Content) -> bool {
    matches!(content, Content::GuiSendInputEventRequest(_))
}

fn screen() -> Content {
    Content::GuiScreenFrame(pb_gui::ScreenFrame {
        data: vec![0xA5; 1024],
        ..Default::default()
    })
}

/// Serves one session on `stream` until the host hangs up or `Close`.
async fn run_fake(
    stream: tokio::io::DuplexStream,
    behaviour: Arc<Mutex<Behaviour>>,
    commands_tx: mpsc::UnboundedSender<Command>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    traffic: mpsc::UnboundedSender<Content>,
) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = FramedRead::new(reader, RpcCodec::default());
    let mut streaming = false;
    // Frames arrive a moment after the action, like on the device.
    let frame_soon = || {
        let commands = commands_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let _ = commands.send(Command::Emit(screen()));
        });
    };
    loop {
        let (id, content) = tokio::select! {
            frame = reader.next() => match frame {
                Some(Ok(pb::Main { command_id, content: Some(content), .. })) => (command_id, content),
                _ => return,
            },
            command = commands.recv() => match command {
                Some(Command::Emit(content)) => {
                    if write(&mut writer, 0, 0, content).await.is_err() {
                        return;
                    }
                    continue;
                }
                Some(Command::Close) | None => return,
            },
        };
        let _ = traffic.send(content.clone());
        let mut after = Vec::new();
        let ok = Content::Empty(pb::Empty {});
        let mut status = 0;
        let mut slow_ack = false;
        let reply = {
            let mut b = behaviour.lock().unwrap();
            match &content {
                Content::PropertyGetRequest(_) => {
                    Content::PropertyGetResponse(pb_property::GetResponse {
                        key: "firmware.version".into(),
                        value: "fake".into(),
                    })
                }
                // `ERROR` means the desktop is unlocked.
                Content::DesktopIsLockedRequest(_) => {
                    status = pb::CommandStatus::Error as i32;
                    ok
                }
                Content::AppLockStatusRequest(_) => {
                    Content::AppLockStatusResponse(pb_app::LockStatusResponse {
                        locked: b.loader_locked,
                    })
                }
                Content::AppStartRequest(_) => {
                    b.loader_locked = true;
                    if b.announce_app_state {
                        after.push(app_state(pb_app::AppState::AppStarted));
                    }
                    ok
                }
                Content::AppExitRequest(_) => {
                    b.loader_locked = false;
                    if b.announce_app_state {
                        after.push(app_state(pb_app::AppState::AppClosed));
                    }
                    ok
                }
                Content::AppDataExchangeRequest(request) => {
                    let envelope = buddy::Envelope::decode(request.data.as_slice());
                    if let Ok(buddy::Envelope {
                        epoch,
                        payload: Some(buddy::envelope::Payload::Hello(_)),
                        ..
                    }) = envelope
                        && b.buddy_hello
                    {
                        let hello = buddy::envelope::Payload::Hello(buddy::Hello {
                            role: buddy::Role::Buddy as i32,
                            max_message_bytes: 512,
                        });
                        after.push(Content::AppDataExchangeRequest(
                            pb_app::DataExchangeRequest {
                                data: self::envelope(epoch, hello),
                            },
                        ));
                    }
                    ok
                }
                Content::GuiStartScreenStreamRequest(_) => {
                    streaming = true;
                    frame_soon();
                    ok
                }
                Content::GuiStopScreenStreamRequest(_) => {
                    streaming = false;
                    ok
                }
                Content::GuiSendInputEventRequest(event) => {
                    let press = event.r#type == pb_gui::InputType::Press as i32;
                    let release = event.r#type == pb_gui::InputType::Release as i32;
                    if streaming && b.redraw_on_press && press {
                        after.push(screen());
                    } else if b.redraw_on_press && release {
                        slow_ack = true;
                    } else if streaming && !b.redraw_on_press && release {
                        frame_soon();
                    }
                    ok
                }
                _ => ok,
            }
        };
        if slow_ack {
            tokio::time::sleep(Duration::from_millis(60)).await; // BLE round trip
        }
        if write(&mut writer, id, status, reply).await.is_err() {
            return;
        }
        for content in after {
            if write(&mut writer, 0, 0, content).await.is_err() {
                return;
            }
        }
    }
}

fn app_state(state: pb_app::AppState) -> Content {
    Content::AppStateResponse(pb_app::AppStateResponse {
        state: state as i32,
    })
}

async fn write(
    writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
    command_id: u32,
    command_status: i32,
    content: Content,
) -> std::io::Result<()> {
    let message = pb::Main {
        command_id,
        command_status,
        has_next: false,
        content: Some(content),
    };
    writer
        .write_all(&encode_frame(&message, usize::MAX).expect("encodable"))
        .await
}

/// The real daemon loop plus a fresh fake device per connection attempt.
pub struct Harness {
    inbound: mpsc::Sender<Inbound>,
    fakes: mpsc::UnboundedReceiver<Fake>,
}

pub const ALICE: ClientId = ClientId(1);
pub const BOB: ClientId = ClientId(2);

impl Harness {
    pub fn start() -> Self {
        Self::start_with(Behaviour::default())
    }

    /// Every device session starts with `initial` behaviour.
    pub fn start_with(initial: Behaviour) -> Self {
        let (inbound, inbound_rx) = mpsc::channel(32);
        let (fakes_tx, fakes) = mpsc::unbounded_channel();
        let connect = move || {
            let (host, device) = tokio::io::duplex(64 * 1024);
            let behaviour = Arc::new(Mutex::new(initial.clone()));
            let (commands_tx, commands) = mpsc::unbounded_channel();
            let (traffic_tx, traffic) = mpsc::unbounded_channel();
            tokio::spawn(run_fake(
                device,
                behaviour.clone(),
                commands_tx.clone(),
                commands,
                traffic_tx,
            ));
            let _ = fakes_tx.send(Fake {
                behaviour,
                commands: commands_tx,
                traffic,
                seen: Vec::new(),
            });
            async move { Ok(Box::new(host) as BoxedStream) }
        };
        tokio::spawn(daemon::serve_with(Config::default(), connect, inbound_rx));
        Self { inbound, fakes }
    }

    /// The next device session the daemon opens.
    pub async fn next_fake(&mut self) -> Fake {
        timeout(PATIENCE, self.fakes.recv())
            .await
            .expect("daemon connects")
            .expect("daemon running")
    }

    /// Starts a call whose reply may come later (e.g. `Open`).
    pub async fn submit(&self, client: ClientId, request: Request) -> oneshot::Receiver<Response> {
        let (reply, answer) = oneshot::channel();
        self.inbound
            .send(Inbound::Call {
                client,
                request,
                reply,
            })
            .await
            .expect("daemon running");
        answer
    }

    pub async fn call(&self, client: ClientId, request: Request) -> Response {
        let answer = self.submit(client, request).await;
        timeout(PATIENCE, answer)
            .await
            .expect("daemon replies")
            .expect("reply sent")
    }

    pub async fn status(&self, client: ClientId) -> StatusReport {
        match self.call(client, Request::Status).await {
            Ok(flipperaki::ipc::Reply::Status(report)) => report,
            other => panic!("unexpected {other:?}"),
        }
    }

    pub async fn gone(&self, client: ClientId) {
        self.inbound
            .send(Inbound::Gone { client })
            .await
            .expect("daemon running");
    }
}
