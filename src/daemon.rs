//! The daemon: sole owner of the device connection and of control state.
//!
//! One loop owns all mutable state. Pure decisions come from `broker` and
//! `buddy`; this module applies them and performs the device I/O. A per-
//! connection pump drains device events continuously so screen streaming can
//! never overflow the RPC session while the loop awaits a device operation.

use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use tokio::sync::{mpsc, oneshot, watch};

use crate::broker::{
    self, AppTarget, Button, ClientId, ConnectionEpoch, Control, ControlGrant, InputStep, RequestId,
};
use crate::buddy::{ActivityBoard, BuddyEncoder, FromBuddy, decode_from_buddy, pb as buddy_pb};
use crate::config::{Config, TransportKind};
use crate::device::{self, Device, DeviceError, EntryKind};
use crate::ipc::{
    self, AppEntry, Delivery, Failure, FinishMode, Inbound, Observation, Reply, Request, Response,
    StatusReport,
};
use crate::limits::BUDDY_MESSAGE_MAX_BYTES;
use crate::rpc::{AppState, DeviceEvent, RpcSession, ScreenFrame, SessionLimits};
use crate::transport::{self, BoxedStream, Transport, ble::BlePeripheralId};

/// How often the loop checks expiry, reconnects and restores Buddy.
const TICK: Duration = Duration::from_millis(500);
/// Pause between connection attempts.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);
/// Waiting for an app to report start/close.
const APP_TRANSITION_TIMEOUT: Duration = Duration::from_secs(4);
/// Waiting for a screen frame newer than an action.
const FRESH_FRAME_TIMEOUT: Duration = Duration::from_secs(2);
/// Let the UI react before taking the post-input observation.
const INPUT_SETTLE: Duration = Duration::from_millis(250);
/// Upper bound on discovered FAPs returned by `flipper_apps`.
const APPS_MAX: usize = 200;
/// Where installed FAPs live.
const APPS_ROOT: &str = "/ext/apps";

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("daemon.lock"))?;
    // Never seize the device from another owner; just refuse to start.
    lock.try_lock()
        .map_err(|_| anyhow::anyhow!("another flipperaki daemon is already running"))?;
    let config = Config::load(dir)?;
    let listener = ipc::bind(&ipc::socket_path(dir))?;
    let (inbound_tx, inbound) = mpsc::channel(32);
    tokio::spawn(async move {
        if let Err(error) = ipc::serve(listener, inbound_tx).await {
            tracing::error!(%error, "ipc listener stopped");
        }
    });
    tracing::info!(transport = ?config.transport, "daemon started");
    let open_config = config.clone();
    let connect = move || open_configured(open_config.clone());
    serve_with(config, connect, inbound).await;
    drop(lock);
    Ok(())
}

/// Runs the daemon loop until `inbound` closes, opening each device session
/// with `connect`. `run` passes the configured transport; tests pass a fake.
pub async fn serve_with<F, Fut>(config: Config, mut connect: F, inbound: mpsc::Receiver<Inbound>)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<BoxedStream>> + Send + 'static,
{
    let connector: Connector = Box::new(move || Box::pin(connect()));
    Daemon::new(config, connector).run(inbound).await;
}

/// Opens the configured transport.
async fn open_configured(config: Config) -> anyhow::Result<BoxedStream> {
    let transport = match config.transport {
        TransportKind::Usb => Transport::Usb,
        TransportKind::Ble => {
            Transport::Ble(BlePeripheralId(config.ble_peripheral_id.ok_or_else(
                || anyhow::anyhow!("no BLE device enrolled; run `flipperaki enroll`"),
            )?))
        }
    };
    Ok(transport::open(&transport).await?)
}

/// Opens one device byte stream per connection attempt.
type Connector =
    Box<dyn FnMut() -> futures::future::BoxFuture<'static, anyhow::Result<BoxedStream>> + Send>;

/// The daemon's clock. Tokio's, so paused-time tests can drive expiry.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// Lists Flipper BLE peripherals and USB serial ports for enrollment.
pub async fn scan() -> anyhow::Result<()> {
    let ports = tokio_serial::available_ports().unwrap_or_default();
    match transport::usb::select_port(&ports) {
        Some(port) => eprintln!("usb: {port}"),
        None => eprintln!("usb: no Flipper serial port"),
    }
    for found in transport::ble::scan(BLE_SCAN_WINDOW).await? {
        let name = found.name.as_deref().unwrap_or("?");
        eprintln!("ble: {name}  peripheral id: {}", found.id.0);
    }
    Ok(())
}

/// How long `flipperaki scan` listens for advertisements.
const BLE_SCAN_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Foreground {
    Unknown,
    Buddy { ready: bool },
    Target(AppTarget),
}

#[derive(Debug, Clone)]
struct Frame {
    screen: ScreenFrame,
    received_at: Instant,
    seq: u64,
}

struct Connection {
    epoch: ConnectionEpoch,
    transport: TransportKind,
    device: Device,
    session: RpcSession,
    events: mpsc::Receiver<DeviceEvent>,
    frames: watch::Receiver<Option<Frame>>,
    buddy: BuddyEncoder,
    foreground: Foreground,
    locked: Option<bool>,
    firmware: Option<String>,
    /// Set while the screen stream runs: frames received since then show the
    /// target. The firmware sends a frame on every redraw, so the latest one is
    /// the current screen however old it is.
    screen_since: Option<Instant>,
}

struct OpenWaiter {
    client: ClientId,
    reply: oneshot::Sender<Response>,
}

struct Daemon {
    config: Config,
    connector: Connector,
    board: ActivityBoard,
    board_dirty: bool,
    control: Control,
    paused: bool,
    conn: Option<Connection>,
    last_epoch: u32,
    last_request: u32,
    next_connect: Instant,
    waiter: Option<OpenWaiter>,
}

impl Daemon {
    fn new(config: Config, connector: Connector) -> Self {
        Self {
            config,
            connector,
            board: ActivityBoard::default(),
            board_dirty: true,
            control: Control::None,
            paused: false,
            conn: None,
            last_epoch: 0,
            last_request: 0,
            next_connect: now(),
            waiter: None,
        }
    }

    async fn run(mut self, mut inbound: mpsc::Receiver<Inbound>) {
        let mut tick = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                message = inbound.recv() => match message {
                    Some(Inbound::Call { client, request, reply }) => {
                        self.handle(client, request, reply).await;
                    }
                    Some(Inbound::Gone { client }) => self.client_gone(client).await,
                    None => break,
                },
                event = next_event(&mut self.conn) => self.on_event(event).await,
                _ = tick.tick() => self.on_tick().await,
            }
        }
        self.disconnect("daemon stopping").await;
    }

    fn epoch(&self) -> Option<ConnectionEpoch> {
        self.conn.as_ref().map(|c| c.epoch)
    }

    // ----- connection lifecycle -------------------------------------------

    async fn on_tick(&mut self) {
        let now = now();
        self.expire(now).await;
        if self.conn.is_none() && now >= self.next_connect {
            self.next_connect = now + RECONNECT_BACKOFF;
            if let Err(error) = self.connect().await {
                tracing::info!(%error, "device not connected");
            }
        }
        self.restore_buddy_when_free().await;
        self.push_snapshot().await;
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        let stream = (self.connector)().await?;
        let (session, raw_events) = RpcSession::spawn(stream, SessionLimits::default());
        let (frames_tx, frames) = watch::channel(None);
        let (events_tx, events) = mpsc::channel(32);
        tokio::spawn(pump_events(raw_events, frames_tx, events_tx));

        self.last_epoch = self.last_epoch.wrapping_add(1).max(1);
        let epoch = ConnectionEpoch(self.last_epoch);
        let device = Device::new(session.clone());
        let info = device.device_info().await;
        if let Err(error) = &info {
            tracing::warn!(%error, "firmware version unavailable");
        }
        let firmware = info.ok().map(|info| {
            let field = |key| info.get(key).unwrap_or("?").to_owned();
            format!(
                "{} ({})",
                field("firmware.version"),
                field("firmware.origin.fork")
            )
        });
        let locked = device.lock_status().await.ok();
        if let Err(error) = device.subscribe_desktop_status().await {
            tracing::warn!(%error, "lock changes will not be observed");
        }
        tracing::info!(epoch = epoch.0, transport = ?self.config.transport, "device connected");
        // A new connection always starts disarmed.
        self.control = Control::None;
        self.conn = Some(Connection {
            epoch,
            transport: self.config.transport,
            device,
            session,
            events,
            frames,
            buddy: BuddyEncoder::new(epoch.0),
            foreground: Foreground::Unknown,
            locked,
            firmware,
            screen_since: None,
        });
        self.board_dirty = true;
        Ok(())
    }

    async fn disconnect(&mut self, why: &str) {
        let Some(conn) = self.conn.take() else { return };
        tracing::warn!(epoch = conn.epoch.0, reason = why, "device session ended");
        conn.session.close().await;
        self.revoke(why);
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter
                .reply
                .send(Err(Failure::Device(format!("connection lost: {why}"))));
        }
        self.next_connect = now() + RECONNECT_BACKOFF;
    }

    /// Ends any pending request or grant. Queued input lives only inside a
    /// single request handler, so nothing else needs discarding.
    fn revoke(&mut self, why: &str) {
        if !matches!(self.control, Control::None) {
            tracing::info!(reason = why, "control revoked");
        }
        self.control = Control::None;
    }

    async fn on_event(&mut self, event: Option<DeviceEvent>) {
        match event {
            None => self.disconnect("event stream ended").await,
            Some(DeviceEvent::Failed(failure)) => self.disconnect(&failure.to_string()).await,
            Some(DeviceEvent::Lock { locked }) => {
                if let Some(conn) = self.conn.as_mut() {
                    conn.locked = Some(locked);
                }
                if locked {
                    self.cancel_pending().await;
                    self.revoke("device locked");
                }
            }
            Some(DeviceEvent::AppState(state)) => self.on_app_state(state),
            Some(DeviceEvent::AppData(bytes)) => self.on_buddy_message(&bytes).await,
            // Screens are routed to the frame slot by the pump.
            Some(DeviceEvent::Screen { .. }) => {}
        }
    }

    fn on_app_state(&mut self, state: AppState) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        if state == AppState::Closed && matches!(conn.foreground, Foreground::Target(_)) {
            // The approved app is gone; input would reach whatever is now on screen.
            conn.foreground = Foreground::Unknown;
            self.revoke("target app closed");
            return;
        }
        if state == AppState::Closed && matches!(conn.foreground, Foreground::Buddy { .. }) {
            conn.foreground = Foreground::Unknown;
            // Buddy closing (e.g. operator pressed Back) withdraws its prompt.
            if matches!(self.control, Control::Pending(_)) {
                self.control = Control::None;
                if let Some(waiter) = self.waiter.take() {
                    let _ = waiter.reply.send(Err(Failure::Device(
                        "Buddy closed before a decision".into(),
                    )));
                }
            }
        }
    }

    async fn on_buddy_message(&mut self, bytes: &[u8]) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let message = match decode_from_buddy(bytes, conn.epoch.0) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "rejected Buddy message");
                return;
            }
        };
        tracing::debug!(?message, "from Buddy");
        match message {
            FromBuddy::Hello { max_message_bytes } => {
                if usize::try_from(max_message_bytes).unwrap_or(0) < BUDDY_MESSAGE_MAX_BYTES {
                    tracing::warn!(
                        max_message_bytes,
                        "Buddy accepts smaller messages than required"
                    );
                    return;
                }
                conn.foreground = Foreground::Buddy { ready: true };
                self.board_dirty = true;
                // Re-show a request that is still pending (e.g. after Buddy restarted).
                if let Control::Pending(pending) = self.control.clone() {
                    self.send_arm_request(&pending.app, pending.id).await;
                }
            }
            FromBuddy::Receipt { sequence, accepted } => {
                if !accepted {
                    tracing::warn!(sequence, "Buddy rejected a message");
                }
            }
            FromBuddy::Decision {
                request_id,
                approved,
            } => {
                let Control::Pending(pending) = self.control.clone() else {
                    tracing::warn!(request_id, "decision without a pending request");
                    return;
                };
                let epoch = conn.epoch;
                match broker::resolve_decision(
                    &pending,
                    RequestId(request_id),
                    approved,
                    epoch,
                    now(),
                ) {
                    Ok(grant) => {
                        self.control = Control::Granted(grant.clone());
                        let response = self.hand_off(&grant).await;
                        let handed_off = matches!(
                            &response,
                            Ok(Reply::Opened {
                                delivery: Delivery::Acknowledged,
                                ..
                            })
                        );
                        if !handed_off {
                            // Never leave a grant behind a failed or uncertain handoff.
                            self.revoke("handoff did not complete");
                        }
                        self.reply_waiter(grant.client, response);
                    }
                    Err(denial) => {
                        self.reply_waiter(pending.client, Err(Failure::Denied(denial)));
                        if denial == broker::Denial::DeniedByOperator {
                            self.control = Control::None;
                        } else {
                            // A mismatched or late decision: clear the prompt on Buddy
                            // too, so no stale request stays on screen.
                            self.cancel_pending().await;
                        }
                    }
                }
            }
        }
    }

    fn reply_waiter(&mut self, client: ClientId, response: Response) {
        if self.waiter.as_ref().is_some_and(|w| w.client == client)
            && let Some(waiter) = self.waiter.take()
        {
            let _ = waiter.reply.send(response);
        }
    }

    async fn expire(&mut self, now: Instant) {
        match &self.control {
            Control::Pending(p) if now >= p.expires_at => {
                let client = p.client;
                self.cancel_pending().await;
                self.reply_waiter(client, Err(Failure::Denied(broker::Denial::Expired)));
            }
            Control::Granted(g) if now >= g.expires_at => self.revoke("grant expired"),
            _ => {}
        }
    }

    async fn cancel_pending(&mut self) {
        let Control::Pending(pending) = self.control.clone() else {
            return;
        };
        self.control = Control::None;
        let payload = buddy_pb::envelope::Payload::ArmCancel(buddy_pb::ArmCancel {
            request_id: pending.id.0,
        });
        self.send_to_buddy(payload).await;
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter
                .reply
                .send(Err(Failure::Denied(broker::Denial::Expired)));
        }
    }

    async fn client_gone(&mut self, client: ClientId) {
        match &self.control {
            Control::Pending(p) if p.client == client => self.cancel_pending().await,
            // The app keeps running; Buddy returns once it is closed.
            Control::Granted(g) if g.client == client => self.revoke("client disconnected"),
            _ => {}
        }
    }

    // ----- Buddy ----------------------------------------------------------

    async fn launch_buddy(&mut self) {
        let path = self.config.buddy_path.clone();
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        // The RPC startup argument is passed only to Buddy, never to targets.
        match conn.device.app_start(&path, "RPC").await {
            Ok(()) => {
                // Buddy registers its RPC callback after starting; send only then.
                if !wait_for_app_state(conn, AppState::Started).await {
                    tracing::info!("Buddy did not report starting");
                    return;
                }
                conn.foreground = Foreground::Buddy { ready: false };
                // Hello carries the epoch so Buddy can bind decisions to it.
                let hello = buddy_pb::envelope::Payload::Hello(buddy_pb::Hello {
                    role: buddy_pb::Role::Host as i32,
                    max_message_bytes: BUDDY_MESSAGE_MAX_BYTES as u32,
                });
                self.send_to_buddy(hello).await;
                self.board_dirty = true;
            }
            Err(error) => tracing::info!(%error, "Buddy launch not acknowledged"),
        }
    }

    /// Relaunches Buddy whenever nothing holds control and the loader is free:
    /// after a return-to-Buddy finish, a manual app exit, or a reconnect.
    async fn restore_buddy_when_free(&mut self) {
        if !matches!(self.control, Control::None) {
            return;
        }
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        if matches!(conn.foreground, Foreground::Buddy { .. }) || conn.locked == Some(true) {
            return;
        }
        if conn.screen_since.take().is_some() {
            let _ = conn.device.stop_screen_stream().await;
        }
        match conn.device.app_lock_status().await {
            Ok(false) => self.launch_buddy().await,
            Ok(true) => {}
            Err(error) => tracing::debug!(%error, "loader status unavailable"),
        }
    }

    async fn push_snapshot(&mut self) {
        if !self.board_dirty {
            return;
        }
        let ready = matches!(
            self.conn.as_ref().map(|c| &c.foreground),
            Some(Foreground::Buddy { ready: true })
        );
        if !ready {
            return; // Updates stay coalesced on the host until Buddy returns.
        }
        self.board_dirty = false;
        let snapshot = self.board.snapshot(now());
        self.send_to_buddy(buddy_pb::envelope::Payload::Snapshot(snapshot))
            .await;
    }

    async fn send_arm_request(&mut self, app: &AppTarget, id: RequestId) {
        let client_label = match &self.control {
            Control::Pending(p) => format!("client {}", p.client.0),
            _ => return,
        };
        let request = buddy_pb::ArmRequest {
            request_id: id.0,
            app_label: app.label.clone(),
            app_path: app.path.clone(),
            client_label,
            max_actions: crate::limits::GRANT_ACTION_BUDGET,
            expires_in_seconds: crate::limits::ARM_REQUEST_TTL.as_secs() as u32,
        };
        self.send_to_buddy(buddy_pb::envelope::Payload::ArmRequest(request))
            .await;
    }

    async fn send_to_buddy(&mut self, payload: buddy_pb::envelope::Payload) -> bool {
        let Some(conn) = self.conn.as_mut() else {
            return false;
        };
        let kind = payload_kind(&payload);
        let (bytes, sequence) = match conn.buddy.encode(payload) {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::warn!(%error, "Buddy message not encodable");
                return false;
            }
        };
        match conn.device.app_data_exchange(&bytes).await {
            Ok(()) => {
                tracing::debug!(kind, sequence, "to Buddy");
                true
            }
            Err(error) => {
                tracing::info!(%error, "Buddy message not acknowledged");
                false
            }
        }
    }

    // ----- requests -------------------------------------------------------

    async fn handle(
        &mut self,
        client: ClientId,
        request: Request,
        reply: oneshot::Sender<Response>,
    ) {
        let response = match request {
            Request::Activity { report } => {
                self.board.apply(report, now());
                self.board_dirty = true;
                self.push_snapshot().await;
                Ok(Reply::Done)
            }
            Request::Status => Ok(Reply::Status(self.status(client))),
            Request::Pause { paused } => {
                self.paused = paused;
                if paused {
                    self.cancel_pending().await;
                    self.revoke("paused by operator");
                }
                Ok(Reply::Done)
            }
            Request::Apps => self.apps().await,
            Request::Open { app } => {
                // Replied later, once the physical decision arrives.
                self.open(client, app, reply).await;
                return;
            }
            Request::Observe => self.observe(client).await,
            Request::Input { steps } => self.input(client, &steps).await,
            Request::Finish { mode } => self.finish(client, mode).await,
        };
        let _ = reply.send(response);
    }

    fn status(&self, client: ClientId) -> StatusReport {
        let now = now();
        let (control, owned) = match &self.control {
            Control::None => ("none".to_owned(), false),
            Control::Pending(p) => (
                format!("awaiting physical approval for {}", p.app.label),
                p.client == client,
            ),
            Control::Granted(g) => (
                format!(
                    "controlling {} ({} actions left)",
                    g.app.label, g.remaining_actions
                ),
                g.client == client && now < g.expires_at,
            ),
        };
        let conn = self.conn.as_ref();
        StatusReport {
            connection: if conn.is_some() { "connected" } else { "disconnected" }.to_owned(),
            transport: conn.map(|c| format!("{:?}", c.transport).to_lowercase()),
            epoch: conn.map(|c| c.epoch.0),
            locked: conn.and_then(|c| c.locked),
            paused: self.paused,
            foreground: match conn.map(|c| &c.foreground) {
                None => "none".into(),
                Some(Foreground::Unknown) => "unknown".into(),
                Some(Foreground::Buddy { .. }) => "buddy".into(),
                Some(Foreground::Target(app)) => format!("app: {}", app.label),
            },
            control,
            you_own_control: owned,
            firmware: conn.and_then(|c| c.firmware.clone()),
            limitations: vec![
                "Input goes to the device's global input bus; foreground identity is inferred, not guaranteed.".into(),
                "An acknowledged input means it was delivered, not that the app reached a goal; observe after acting.".into(),
                "Apps that switch the Bluetooth or USB profile can end the session.".into(),
            ],
        }
    }

    async fn apps(&mut self) -> Response {
        let Some(conn) = self.conn.as_ref() else {
            return Err(Failure::Denied(broker::Denial::Disconnected));
        };
        let mut apps = Vec::new();
        let categories = conn
            .device
            .storage_list(APPS_ROOT)
            .await
            .map_err(|e| Failure::Device(e.to_string()))?;
        for category in categories.into_iter().filter(|e| e.kind == EntryKind::Dir) {
            let dir = format!("{APPS_ROOT}/{}", category.name);
            let Ok(entries) = conn.device.storage_list(&dir).await else {
                continue;
            };
            for entry in entries
                .into_iter()
                .filter(|e| e.kind == EntryKind::File && e.name.ends_with(".fap"))
            {
                let path = format!("{dir}/{}", entry.name);
                if path == self.config.buddy_path || apps.len() >= APPS_MAX {
                    continue;
                }
                let name = entry.name.trim_end_matches(".fap").to_owned();
                apps.push(AppEntry {
                    id: path,
                    name,
                    source: "installed".into(),
                    tested: false,
                });
            }
        }
        Ok(Reply::Apps { apps })
    }

    async fn open(&mut self, client: ClientId, app: String, reply: oneshot::Sender<Response>) {
        let target = match app_target(&app) {
            Ok(target) => target,
            Err(failure) => {
                let _ = reply.send(Err(failure));
                return;
            }
        };
        let buddy_ready = matches!(
            self.conn.as_ref().map(|c| &c.foreground),
            Some(Foreground::Buddy { ready: true })
        );
        self.last_request = self.last_request.wrapping_add(1).max(1);
        let id = RequestId(self.last_request);
        let admitted = broker::begin_request(
            &self.control,
            client,
            self.epoch(),
            self.paused,
            target.clone(),
            id,
            now(),
        );
        let pending = match admitted {
            Ok(pending) => pending,
            Err(denial) => {
                let _ = reply.send(Err(Failure::Denied(denial)));
                return;
            }
        };
        if !buddy_ready {
            let _ = reply.send(Err(Failure::Device(
                "Buddy is not on screen; close the current app on the Flipper and retry".into(),
            )));
            return;
        }
        self.control = Control::Pending(pending);
        self.waiter = Some(OpenWaiter { client, reply });
        self.send_arm_request(&target, id).await;
    }

    /// Buddy -> target: exit Buddy, wait for the loader, launch the target
    /// normally, start the screen stream and return a fresh observation.
    async fn hand_off(&mut self, grant: &ControlGrant) -> Response {
        let Some(conn) = self.conn.as_mut() else {
            return Err(Failure::Denied(broker::Denial::Disconnected));
        };
        if let Err(error) = conn.device.app_exit().await {
            return Err(Failure::Device(format!("Buddy exit: {error}")));
        }
        if !wait_for_app_state(conn, AppState::Closed).await {
            return Err(Failure::Device("Buddy did not report closing".into()));
        }
        conn.foreground = Foreground::Unknown;
        if !wait_for_loader_free(&conn.device).await {
            return Err(Failure::Device("loader still busy after Buddy exit".into()));
        }
        let started_at = now();
        let launch = conn.device.app_start(&grant.app.path, "").await;
        let delivery = to_delivery(&launch);
        if launch.is_err() {
            return Ok(Reply::Opened {
                delivery,
                observation: None,
                next_step: "launch failed; call flipper_finish(return_to_buddy)".into(),
            });
        }
        conn.foreground = Foreground::Target(grant.app.clone());
        let streaming =
            conn.screen_since.is_some() || conn.device.start_screen_stream().await.is_ok();
        conn.screen_since = streaming.then_some(started_at);
        let observation = fresh_observation(conn, started_at).await;
        let next_step = if observation.is_some() {
            "observe the screenshot, then flipper_input"
        } else {
            "call flipper_observe"
        };
        Ok(Reply::Opened {
            delivery,
            observation,
            next_step: next_step.into(),
        })
    }

    async fn observe(&mut self, client: ClientId) -> Response {
        let now = now();
        broker::check_owner(&self.control, client, self.epoch(), now).map_err(Failure::Denied)?;
        let conn = self
            .conn
            .as_mut()
            .ok_or(Failure::Denied(broker::Denial::Disconnected))?;
        let since = conn
            .screen_since
            .ok_or_else(|| Failure::Device("screen stream is not running".into()))?;
        let observation = fresh_observation(conn, since)
            .await
            .ok_or_else(|| Failure::Device("no screen frame since the app started".into()))?;
        Ok(Reply::Observed { observation })
    }

    async fn input(&mut self, client: ClientId, steps: &[InputStep]) -> Response {
        let now = now();
        let grant =
            broker::authorize_input(&self.control, client, self.epoch(), self.paused, now, steps)
                .map_err(Failure::Denied)?;
        let remaining_actions = grant.remaining_actions;
        // Commit the reduced budget before any byte is sent.
        self.control = Control::Granted(grant);
        let conn = self
            .conn
            .as_mut()
            .ok_or(Failure::Denied(broker::Denial::Disconnected))?;
        if !matches!(conn.foreground, Foreground::Target(_)) {
            return Err(Failure::Device(
                "the approved app is not in the foreground".into(),
            ));
        }
        // Ordinary apps send no close notice, so confirm an app still holds the
        // loader before every input. This cannot prove *which* app it is.
        let app_running =
            conn.locked != Some(true) && matches!(conn.device.app_lock_status().await, Ok(true));
        if !app_running {
            conn.foreground = Foreground::Unknown;
            self.revoke("device locked or approved app exited");
            return Err(Failure::Device(
                "the device is locked or the approved app has exited; control revoked".into(),
            ));
        }
        let conn = self
            .conn
            .as_mut()
            .ok_or(Failure::Denied(broker::Denial::Disconnected))?;
        // A redraw can reach us before the last step is acknowledged (seen over
        // BLE), and a static screen sends no further frame, so count from here.
        let sent_at = self::now();
        let mut delivered = 0u32;
        let mut last = Delivery::Acknowledged;
        for step in steps {
            let outcome = match *step {
                InputStep::Tap { button } => conn.device.tap(device_button(button)).await,
                InputStep::Hold { button, millis } => {
                    conn.device
                        .hold(
                            device_button(button),
                            Duration::from_millis(u64::from(millis)),
                        )
                        .await
                }
            };
            last = to_delivery(&outcome);
            if outcome.is_err() {
                break; // Stop at the first failure; never replay.
            }
            delivered += 1;
        }
        tokio::time::sleep(INPUT_SETTLE).await;
        let observation = fresh_observation(conn, sent_at).await;
        let next_step = match &last {
            Delivery::Acknowledged if observation.is_some() => {
                "check the screenshot before the next input"
            }
            Delivery::Acknowledged => "call flipper_observe before the next input",
            Delivery::Unknown(_) => {
                "outcome unknown: observe before doing anything else; do not repeat"
            }
            _ => "input stopped; observe and decide",
        };
        Ok(Reply::InputResult {
            delivery: last,
            steps_delivered: delivered,
            remaining_actions,
            observation,
            next_step: next_step.into(),
        })
    }

    async fn finish(&mut self, client: ClientId, mode: FinishMode) -> Response {
        let now = now();
        broker::check_owner(&self.control, client, self.epoch(), now).map_err(Failure::Denied)?;
        self.revoke("finished by client");
        let conn = self
            .conn
            .as_mut()
            .ok_or(Failure::Denied(broker::Denial::Disconnected))?;
        if conn.screen_since.take().is_some() {
            let _ = conn.device.stop_screen_stream().await;
        }
        match mode {
            FinishMode::LeaveRunning => Ok(Reply::Finished {
                delivery: Delivery::Acknowledged,
                next_step: "app left running; Buddy returns after it is closed on the device".into(),
            }),
            FinishMode::ReturnToBuddy => match conn.device.app_lock_status().await {
                Ok(false) => {
                    self.launch_buddy().await;
                    Ok(Reply::Finished {
                        delivery: Delivery::Acknowledged,
                        next_step: "Buddy relaunched".into(),
                    })
                }
                // No universal close exists for ordinary apps; never spam Back.
                Ok(true) => Ok(Reply::Finished {
                    delivery: Delivery::NotSent("the app is still open".into()),
                    next_step: "ask the operator to exit the app on the Flipper; Buddy returns automatically".into(),
                }),
                Err(error) => Err(Failure::Device(error.to_string())),
            },
        }
    }
}

/// Validates a `flipper_open` argument: an exact installed FAP path.
fn app_target(app: &str) -> Result<AppTarget, Failure> {
    let invalid = || Failure::Invalid("app must be an id returned by flipper_apps".into());
    // Exactly /ext/apps/<category>/<name>.fap — the same shape Buddy accepts.
    let rest = app
        .strip_prefix(APPS_ROOT)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(invalid)?;
    let (category, file) = rest.split_once('/').ok_or_else(invalid)?;
    let stem = file.strip_suffix(".fap").ok_or_else(invalid)?;
    let valid = !category.is_empty()
        && !category.starts_with('.')
        && !stem.is_empty()
        && !file.contains('/')
        && !app.contains("..")
        && app.len() <= crate::buddy::APP_PATH_MAX_CHARS
        && app.chars().all(|c| c.is_ascii_graphic() || c == ' ');
    if !valid {
        return Err(invalid());
    }
    let label: String = stem.chars().take(crate::buddy::LABEL_MAX_CHARS).collect();
    if label.len() < stem.len() {
        // The approval prompt must show the real name, so refuse rather than truncate.
        return Err(Failure::Invalid(
            "app name too long to show on the approval prompt".into(),
        ));
    }
    Ok(AppTarget {
        path: app.to_owned(),
        label,
    })
}

fn device_button(button: Button) -> device::Button {
    match button {
        Button::Up => device::Button::Up,
        Button::Down => device::Button::Down,
        Button::Left => device::Button::Left,
        Button::Right => device::Button::Right,
        Button::Ok => device::Button::Ok,
        Button::Back => device::Button::Back,
    }
}

fn to_delivery(outcome: &Result<(), DeviceError>) -> Delivery {
    match outcome {
        Ok(()) => Delivery::Acknowledged,
        Err(DeviceError::NotSent(reason)) => Delivery::NotSent(reason.to_string()),
        Err(DeviceError::InvalidArgument(why)) => Delivery::NotSent((*why).to_owned()),
        Err(DeviceError::Rejected { status }) => {
            Delivery::Rejected(format!("firmware status {status}"))
        }
        // A reply we cannot interpret proves nothing about what happened.
        Err(error @ (DeviceError::Unknown { .. } | DeviceError::UnexpectedReply)) => {
            Delivery::Unknown(error.to_string())
        }
    }
}

async fn next_event(conn: &mut Option<Connection>) -> Option<DeviceEvent> {
    match conn {
        Some(conn) => conn.events.recv().await,
        None => std::future::pending().await,
    }
}

/// Drains the session's events continuously: screens replace the latest-frame
/// slot, everything else goes to the daemon loop.
async fn pump_events(
    mut raw: mpsc::Receiver<DeviceEvent>,
    frames: watch::Sender<Option<Frame>>,
    events: mpsc::Sender<DeviceEvent>,
) {
    while let Some(event) = raw.recv().await {
        match event {
            DeviceEvent::Screen {
                frame,
                received_at,
                seq,
            } => {
                tracing::trace!(seq, "screen frame");
                frames.send_replace(Some(Frame {
                    screen: frame,
                    received_at: received_at.into_std(),
                    seq,
                }));
            }
            other => {
                if events.send(other).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Waits for the device to report `wanted`. Only used during app transitions,
/// when no Buddy message is expected; lock changes are still recorded and a
/// session failure ends the wait (the session state reports it to the loop).
async fn wait_for_app_state(conn: &mut Connection, wanted: AppState) -> bool {
    let until = tokio::time::Instant::now() + APP_TRANSITION_TIMEOUT;
    loop {
        match tokio::time::timeout_at(until, conn.events.recv()).await {
            Ok(Some(DeviceEvent::AppState(state))) if state == wanted => return true,
            Ok(Some(DeviceEvent::Lock { locked })) => {
                conn.locked = Some(locked);
                if locked {
                    return false; // The caller fails the transition, which revokes.
                }
            }
            Ok(Some(DeviceEvent::Failed(_))) | Ok(None) | Err(_) => return false,
            Ok(Some(_)) => {}
        }
    }
}

async fn wait_for_loader_free(device: &Device) -> bool {
    let until = now() + APP_TRANSITION_TIMEOUT;
    while now() < until {
        if let Ok(false) = device.app_lock_status().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// A screen frame received after `since`, as an MCP observation.
async fn fresh_observation(conn: &mut Connection, since: Instant) -> Option<Observation> {
    let wait = conn
        .frames
        .wait_for(|f| f.as_ref().is_some_and(|f| f.received_at >= since));
    let frame = tokio::time::timeout(FRESH_FRAME_TIMEOUT, wait)
        .await
        .ok()?
        .ok()?
        .clone()?;
    let png = screen_png(&frame.screen).ok()?;
    Some(Observation {
        png_base64: base64::engine::general_purpose::STANDARD.encode(png),
        epoch: conn.epoch.0,
        sequence: frame.seq,
        age_ms: u64::try_from(
            now()
                .saturating_duration_since(frame.received_at)
                .as_millis(),
        )
        .unwrap_or(u64::MAX),
    })
}

/// Each device pixel becomes a square of this many image pixels, so the
/// 128x64 screen is legible to a vision model.
const PNG_SCALE: usize = 4;

/// Encodes the 1-bit screen as a scaled grayscale PNG (lit pixels dark, as on
/// the device's display).
fn screen_png(frame: &ScreenFrame) -> anyhow::Result<Vec<u8>> {
    let (width, height) = (
        flipper_companion_protocol::WIDTH * PNG_SCALE,
        flipper_companion_protocol::HEIGHT * PNG_SCALE,
    );
    let mut pixels = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let lit = frame.pixel(x / PNG_SCALE, y / PNG_SCALE);
            pixels.push(if lit { 0x10 } else { 0xF0 });
        }
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, u32::try_from(width)?, u32::try_from(height)?);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(png)
}

fn payload_kind(payload: &buddy_pb::envelope::Payload) -> &'static str {
    use buddy_pb::envelope::Payload;
    match payload {
        Payload::Hello(_) => "hello",
        Payload::Snapshot(_) => "snapshot",
        Payload::ArmRequest(_) => "arm_request",
        Payload::Decision(_) => "decision",
        Payload::Receipt(_) => "receipt",
        Payload::ArmCancel(_) => "arm_cancel",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_accepts_only_exact_installed_fap_paths() {
        assert!(app_target("/ext/apps/Tools/clock.fap").is_ok());
        for bad in [
            "Clock",
            "/ext/apps/../int/x.fap",
            "/ext/apps/Tools/x.fap\n",
            "/int/apps/x.fap",
            "/ext/apps/Tools/averyveryveryverylongappname.fap",
            "/ext/apps/clock.fap",
            "/ext/apps/Tools/x/clock.fap",
            "/ext/apps//clock.fap",
            "/ext/apps/./clock.fap",
            "/ext/appsX/Tools/clock.fap",
            "/ext/apps/Tools/.fap",
        ] {
            assert!(app_target(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
