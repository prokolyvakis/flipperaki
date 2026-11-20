//! RPC session executor.
//!
//! Three tasks own the connection: a reader that continuously drains and
//! decodes frames, a writer that owns the frame being written, and the
//! executor that admits requests, enforces deadlines, routes replies by
//! `command_id` and turns unsolicited frames into [`DeviceEvent`]s. Callers
//! only hold channels, so dropping a caller's future never cancels a write.

use std::collections::{HashMap, VecDeque};

use futures::StreamExt;
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_util::codec::FramedRead;
use tokio_util::sync::CancellationToken;

use crate::limits::{
    BUDDY_MESSAGE_MAX_BYTES, RPC_EVENT_QUEUE, RPC_FRAME_MAX_BYTES, RPC_PENDING_MAX,
    RPC_RESPONSE_MAX_BYTES,
};
use crate::rpc::{FrameError, RpcCodec, ScreenFrame, encode_frame, pb, pb_app};

/// Resource bounds of one session; `Default` takes them from `crate::limits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    pub frame_max_bytes: usize,
    pub response_max_bytes: usize,
    /// Admitted requests not yet resolved (queued + in flight). Together with
    /// `frame_max_bytes` this also bounds the outgoing bytes held.
    pub pending_max: usize,
    pub event_queue: usize,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            frame_max_bytes: RPC_FRAME_MAX_BYTES,
            response_max_bytes: RPC_RESPONSE_MAX_BYTES,
            pending_max: RPC_PENDING_MAX,
            event_queue: RPC_EVENT_QUEUE,
        }
    }
}

/// Whether a request changes device state. At most one mutation is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Query,
    Mutation,
}

/// Why a session stopped working. Terminal: a new session is required.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("transport closed by the device")]
    Eof,
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("device event queue overflowed")]
    EventOverflow,
    #[error("multipart response exceeded {0} bytes")]
    ResponseTooLarge(usize),
    #[error("a frame could not be written before its deadline")]
    WriteStalled,
    #[error("command ids exhausted")]
    IdsExhausted,
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Open,
    Closed,
    Failed(Failure),
}

/// Why a request was not sent or why its outcome is unknown.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Reason {
    #[error("deadline expired")]
    Deadline,
    #[error("too many requests pending")]
    Busy,
    #[error("request frame too large")]
    TooLarge,
    #[error("session closed")]
    Closed,
    #[error("session failed: {0}")]
    Failed(Failure),
}

/// How far an uncertain request got. `Written` means the transport accepted
/// the whole frame, which is not a firmware acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Writing,
    Written,
}

/// Every reply part's content, in arrival order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply(pub Vec<pb::main::Content>);

/// The typed outcome of one request.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum Delivery {
    /// No byte of the request reached the transport.
    NotSent(Reason),
    /// The firmware answered `OK` (all parts of a multipart reply).
    Acknowledged(Reply),
    /// The firmware answered with a non-OK `PB.CommandStatus`.
    Rejected { status: i32 },
    /// Bytes may have reached the device but no answer arrived. Never replay.
    Unknown { reason: Reason, stage: Stage },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Started,
    Closed,
}

/// Unsolicited device output (`command_id == 0`) and session failure.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceEvent {
    Screen {
        frame: ScreenFrame,
        received_at: Instant,
        seq: u64,
    },
    /// A data exchange the FAP sent to the host (at most 512 bytes).
    AppData(Vec<u8>),
    AppState(AppState),
    Lock {
        locked: bool,
    },
    Failed(Failure),
}

/// Handle to one RPC session. Cheap to clone; all clones share the session.
#[derive(Debug, Clone)]
pub struct RpcSession {
    commands: mpsc::Sender<Admit>,
    state: watch::Receiver<SessionState>,
    cancel: CancellationToken,
}

#[derive(Debug)]
struct Admit {
    content: pb::main::Content,
    kind: RequestKind,
    deadline: Instant,
    reply: oneshot::Sender<Delivery>,
}

impl RpcSession {
    /// Starts the reader, writer and executor tasks on `stream`.
    pub fn spawn<S>(stream: S, limits: SessionLimits) -> (Self, mpsc::Receiver<DeviceEvent>)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read_half, write_half) = tokio::io::split(stream);
        let (commands, command_rx) = mpsc::channel(limits.pending_max.max(1));
        let (events, event_rx) = mpsc::channel(limits.event_queue.max(1));
        let (state_tx, state) = watch::channel(SessionState::Open);
        let (frames_tx, frames) = mpsc::channel(16);
        let (writer, writer_rx) = mpsc::channel(1);
        let (written_tx, written) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let tasks = [
            tokio::spawn(read_loop(
                read_half,
                RpcCodec::new(limits.frame_max_bytes),
                frames_tx,
            )),
            tokio::spawn(write_loop(write_half, writer_rx, written_tx)),
        ];
        let executor = Executor {
            limits,
            next_id: 1,
            queue: VecDeque::new(),
            pending: HashMap::new(),
            writing: None,
            mutation: None,
            events,
            screen_seq: 0,
            state: state_tx,
            writer,
        };
        tokio::spawn(executor.run(command_rx, frames, written, cancel.clone(), tasks));
        (
            Self {
                commands,
                state,
                cancel,
            },
            event_rx,
        )
    }

    /// Admits one request. The executor enforces `deadline` whether or not
    /// the returned future is polled to completion.
    pub async fn request(
        &self,
        content: pb::main::Content,
        kind: RequestKind,
        deadline: Instant,
    ) -> Delivery {
        let (reply, outcome) = oneshot::channel();
        let admit = Admit {
            content,
            kind,
            deadline,
            reply,
        };
        if let Err(error) = self.commands.try_send(admit) {
            return Delivery::NotSent(match error {
                mpsc::error::TrySendError::Full(_) => Reason::Busy,
                mpsc::error::TrySendError::Closed(_) => self.end_reason(),
            });
        }
        outcome.await.unwrap_or_else(|_| Delivery::Unknown {
            reason: self.end_reason(),
            stage: Stage::Writing,
        })
    }

    /// Closes the session: queued requests are not sent, in-flight ones
    /// become `Unknown`. Returns once the executor has stopped.
    pub async fn close(&self) {
        self.cancel.cancel();
        let mut state = self.state.clone();
        let _ = state.wait_for(|state| *state != SessionState::Open).await;
    }

    /// The session state; changes once, from `Open` to `Closed` or `Failed`.
    #[must_use]
    pub fn state(&self) -> watch::Receiver<SessionState> {
        self.state.clone()
    }

    fn end_reason(&self) -> Reason {
        match &*self.state.borrow() {
            SessionState::Failed(failure) => Reason::Failed(failure.clone()),
            SessionState::Open | SessionState::Closed => Reason::Closed,
        }
    }
}

type Incoming = Result<(Instant, pb::Main), Failure>;

async fn read_loop<R: AsyncRead>(reader: R, codec: RpcCodec, frames: mpsc::Sender<Incoming>) {
    let mut reader = std::pin::pin!(FramedRead::new(reader, codec));
    loop {
        let item = match reader.next().await {
            Some(Ok(message)) => Ok((Instant::now(), message)),
            Some(Err(error)) => Err(Failure::Frame(error)),
            None => Err(Failure::Eof),
        };
        let terminal = item.is_err();
        if frames.send(item).await.is_err() || terminal {
            return;
        }
    }
}

async fn write_loop<W: AsyncWrite>(
    writer: W,
    mut frames: mpsc::Receiver<(u32, Vec<u8>)>,
    written: mpsc::Sender<Result<u32, Failure>>,
) {
    let mut writer = std::pin::pin!(writer);
    while let Some((id, frame)) = frames.recv().await {
        let result = match writer.write_all(&frame).await {
            Ok(()) => writer.flush().await,
            Err(error) => Err(error),
        };
        let report = result
            .map(|()| id)
            .map_err(|error| Failure::Frame(error.into()));
        let failed = report.is_err();
        if written.send(report).await.is_err() || failed {
            return;
        }
    }
}

struct Queued {
    id: u32,
    frame: Vec<u8>,
    kind: RequestKind,
    deadline: Instant,
    reply: oneshot::Sender<Delivery>,
}

struct Pending {
    deadline: Instant,
    reply: oneshot::Sender<Delivery>,
    stage: Stage,
    parts: Vec<pb::main::Content>,
    bytes: usize,
}

struct Executor {
    limits: SessionLimits,
    next_id: u32,
    queue: VecDeque<Queued>,
    pending: HashMap<u32, Pending>,
    writing: Option<u32>,
    mutation: Option<u32>,
    events: mpsc::Sender<DeviceEvent>,
    screen_seq: u64,
    state: watch::Sender<SessionState>,
    writer: mpsc::Sender<(u32, Vec<u8>)>,
}

impl Executor {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Admit>,
        mut frames: mpsc::Receiver<Incoming>,
        mut written: mpsc::Receiver<Result<u32, Failure>>,
        cancel: CancellationToken,
        tasks: [JoinHandle<()>; 2],
    ) {
        let end = loop {
            if let Err(failure) = self.dispatch() {
                break Some(failure);
            }
            let deadline = self.earliest_deadline();
            let step = tokio::select! {
                biased;
                () = cancel.cancelled() => break None,
                frame = frames.recv() => match frame {
                    Some(Ok((at, message))) => self.route(at, message),
                    Some(Err(failure)) => Err(failure),
                    None => Err(Failure::Eof),
                },
                report = written.recv() => match report {
                    Some(Ok(id)) => {
                        self.written(id);
                        Ok(())
                    }
                    Some(Err(failure)) => Err(failure),
                    None => Err(Failure::Eof),
                },
                command = commands.recv() => match command {
                    Some(admit) => self.admit(admit),
                    None => break None,
                },
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.expire(Instant::now())
                }
            };
            if let Err(failure) = step {
                break Some(failure);
            }
        };
        for task in tasks {
            task.abort();
        }
        self.shutdown(end);
    }

    fn admit(&mut self, admit: Admit) -> Result<(), Failure> {
        let kind = if is_app_transaction(&admit.content) {
            RequestKind::Mutation
        } else {
            admit.kind
        };
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(Failure::IdsExhausted)?;
        let message = pb::Main {
            command_id: id,
            command_status: 0,
            has_next: false,
            content: Some(admit.content),
        };
        let refusal = if admit.deadline <= Instant::now() {
            Some(Reason::Deadline)
        } else if self.queue.len() + self.pending.len() >= self.limits.pending_max {
            Some(Reason::Busy)
        } else {
            None
        };
        let frame = match (refusal, encode_frame(&message, self.limits.frame_max_bytes)) {
            (None, Ok(frame)) => frame,
            (refusal, _) => {
                let reason = refusal.unwrap_or(Reason::TooLarge);
                let _ = admit.reply.send(Delivery::NotSent(reason));
                return Ok(());
            }
        };
        self.queue.push_back(Queued {
            id,
            frame,
            kind,
            deadline: admit.deadline,
            reply: admit.reply,
        });
        Ok(())
    }

    /// Hands the first eligible queued frame to the idle writer.
    fn dispatch(&mut self) -> Result<(), Failure> {
        if self.writing.is_some() {
            return Ok(());
        }
        let mutation_busy = self.mutation.is_some();
        let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.kind == RequestKind::Query || !mutation_busy)
        else {
            return Ok(());
        };
        let Some(queued) = self.queue.remove(index) else {
            return Ok(());
        };
        if queued.deadline <= Instant::now() {
            let _ = queued.reply.send(Delivery::NotSent(Reason::Deadline));
            return Ok(());
        }
        self.writer
            .try_send((queued.id, queued.frame))
            .map_err(|_| Failure::Eof)?;
        self.writing = Some(queued.id);
        if queued.kind == RequestKind::Mutation {
            self.mutation = Some(queued.id);
        }
        self.pending.insert(
            queued.id,
            Pending {
                deadline: queued.deadline,
                reply: queued.reply,
                stage: Stage::Writing,
                parts: Vec::new(),
                bytes: 0,
            },
        );
        Ok(())
    }

    fn written(&mut self, id: u32) {
        self.writing = None;
        if let Some(pending) = self.pending.get_mut(&id) {
            pending.stage = Stage::Written;
        }
    }

    fn route(&mut self, received_at: Instant, message: pb::Main) -> Result<(), Failure> {
        let id = message.command_id;
        if id == 0 {
            return self.unsolicited(received_at, message);
        }
        let Some(pending) = self.pending.get_mut(&id) else {
            tracing::debug!(command_id = id, "ignoring late or unknown reply");
            return Ok(());
        };
        if message.command_status != 0 {
            self.finish(
                id,
                Delivery::Rejected {
                    status: message.command_status,
                },
            );
            return Ok(());
        }
        pending.bytes += message.encoded_len();
        if pending.bytes > self.limits.response_max_bytes {
            return Err(Failure::ResponseTooLarge(self.limits.response_max_bytes));
        }
        pending.parts.extend(message.content);
        if !message.has_next {
            let parts = std::mem::take(&mut pending.parts);
            self.finish(id, Delivery::Acknowledged(Reply(parts)));
        }
        Ok(())
    }

    fn unsolicited(&mut self, received_at: Instant, message: pb::Main) -> Result<(), Failure> {
        use pb::main::Content;
        let event = match message.content {
            Some(Content::GuiScreenFrame(_)) => {
                let Some(frame) = ScreenFrame::from_message(&message) else {
                    return Ok(());
                };
                self.screen_seq += 1;
                DeviceEvent::Screen {
                    frame,
                    received_at,
                    seq: self.screen_seq,
                }
            }
            Some(Content::AppDataExchangeRequest(request)) => {
                if request.data.len() > BUDDY_MESSAGE_MAX_BYTES {
                    return Err(Failure::Protocol("app data exchange exceeds 512 bytes"));
                }
                DeviceEvent::AppData(request.data)
            }
            Some(Content::AppStateResponse(response)) => {
                DeviceEvent::AppState(match response.state() {
                    pb_app::AppState::AppStarted => AppState::Started,
                    pb_app::AppState::AppClosed => AppState::Closed,
                })
            }
            Some(Content::DesktopStatus(status)) => DeviceEvent::Lock {
                locked: status.locked,
            },
            _ => {
                tracing::debug!("ignoring unsolicited frame");
                return Ok(());
            }
        };
        match self.events.try_send(event) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(Failure::EventOverflow),
        }
    }

    fn finish(&mut self, id: u32, delivery: Delivery) {
        if self.mutation == Some(id) {
            self.mutation = None;
        }
        if let Some(pending) = self.pending.remove(&id) {
            let _ = pending.reply.send(delivery);
        }
    }

    fn earliest_deadline(&self) -> Option<Instant> {
        let queued = self.queue.iter().map(|queued| queued.deadline);
        let pending = self.pending.values().map(|pending| pending.deadline);
        queued.chain(pending).min()
    }

    fn expire(&mut self, now: Instant) -> Result<(), Failure> {
        let (expired, kept) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|queued| queued.deadline <= now);
        self.queue = kept;
        for queued in Vec::from(expired) {
            let _ = queued.reply.send(Delivery::NotSent(Reason::Deadline));
        }
        let expired: Vec<(u32, Stage)> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|(id, pending)| (*id, pending.stage))
            .collect();
        let mut stalled = false;
        for (id, stage) in expired {
            stalled |= stage == Stage::Writing;
            let reason = Reason::Deadline;
            self.finish(id, Delivery::Unknown { reason, stage });
        }
        // The writer cannot abandon a frame mid-way, so the session must go.
        if stalled {
            return Err(Failure::WriteStalled);
        }
        Ok(())
    }

    fn shutdown(mut self, failure: Option<Failure>) {
        let (state, reason) = match failure {
            Some(failure) => {
                tracing::warn!(%failure, "rpc session failed");
                let _ = self.events.try_send(DeviceEvent::Failed(failure.clone()));
                (
                    SessionState::Failed(failure.clone()),
                    Reason::Failed(failure),
                )
            }
            None => (SessionState::Closed, Reason::Closed),
        };
        for queued in self.queue.drain(..) {
            let _ = queued.reply.send(Delivery::NotSent(reason.clone()));
        }
        for (_, pending) in self.pending.drain() {
            let _ = pending.reply.send(Delivery::Unknown {
                reason: reason.clone(),
                stage: pending.stage,
            });
        }
        self.state.send_replace(state);
    }
}

/// App RPC transactions are always serialized like mutations.
fn is_app_transaction(content: &pb::main::Content) -> bool {
    use pb::main::Content;
    matches!(
        content,
        Content::AppStartRequest(_)
            | Content::AppExitRequest(_)
            | Content::AppDataExchangeRequest(_)
            | Content::AppButtonPressRequest(_)
            | Content::AppButtonReleaseRequest(_)
            | Content::AppButtonPressReleaseRequest(_)
    )
}
