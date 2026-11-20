//! A scripted fake Flipper on the far end of a `tokio::io::duplex` pipe.
#![allow(dead_code)]

use std::time::Duration;

use flipperaki::rpc::{
    DeviceEvent, RpcCodec, RpcSession, SessionLimits, encode_frame, pb, pb_gui, pb_system,
};
use futures::StreamExt;
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout};
use tokio_util::codec::FramedRead;

pub use pb::main::Content;

pub struct FakeFlipper {
    reader: FramedRead<ReadHalf<DuplexStream>, RpcCodec>,
    writer: WriteHalf<DuplexStream>,
}

/// A session wired to a fake Flipper through a pipe of `pipe_bytes` capacity.
pub fn connect(
    limits: SessionLimits,
    pipe_bytes: usize,
) -> (RpcSession, mpsc::Receiver<DeviceEvent>, FakeFlipper) {
    let (host, device) = tokio::io::duplex(pipe_bytes);
    let (session, events) = RpcSession::spawn(host, limits);
    let (reader, writer) = tokio::io::split(device);
    let fake = FakeFlipper {
        reader: FramedRead::new(reader, RpcCodec::default()),
        writer,
    };
    (session, events, fake)
}

pub fn start() -> (RpcSession, mpsc::Receiver<DeviceEvent>, FakeFlipper) {
    connect(SessionLimits::default(), 64 * 1024)
}

pub fn after(duration: Duration) -> Instant {
    Instant::now() + duration
}

pub fn ping(data: &[u8]) -> Content {
    Content::SystemPingRequest(pb_system::PingRequest {
        data: data.to_vec(),
    })
}

pub fn input(key: pb_gui::InputKey, kind: pb_gui::InputType) -> Content {
    Content::GuiSendInputEventRequest(pb_gui::SendInputEventRequest {
        key: key as i32,
        r#type: kind as i32,
    })
}

impl FakeFlipper {
    /// Next frame the host wrote. Panics if none arrives.
    pub async fn recv(&mut self) -> pb::Main {
        match self.reader.next().await {
            Some(Ok(message)) => message,
            other => panic!("expected a frame from the host, got {other:?}"),
        }
    }

    /// `None` if the host writes nothing within `wait`.
    pub async fn try_recv(&mut self, wait: Duration) -> Option<pb::Main> {
        timeout(wait, self.reader.next())
            .await
            .ok()
            .flatten()
            .and_then(Result::ok)
    }

    pub async fn send(&mut self, command_id: u32, status: i32, has_next: bool, content: Content) {
        let message = pb::Main {
            command_id,
            command_status: status,
            has_next,
            content: Some(content),
        };
        let frame = encode_frame(&message, usize::MAX).expect("encodable");
        self.writer.write_all(&frame).await.expect("pipe open");
    }

    pub async fn reply(&mut self, command_id: u32, content: Content) {
        self.send(command_id, 0, false, content).await;
    }

    pub async fn ack(&mut self, command_id: u32) {
        self.reply(command_id, Content::Empty(pb::Empty {})).await;
    }

    pub async fn reject(&mut self, command_id: u32, status: i32) {
        self.send(command_id, status, false, Content::Empty(pb::Empty {}))
            .await;
    }

    pub async fn raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.expect("pipe open");
    }
}

pub mod daemon;
