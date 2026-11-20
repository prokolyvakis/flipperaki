//! Varint length-delimited `PB.Main` framing with our own frame bound.
//!
//! Same wire format as `flipper_companion_protocol::FrameCodec`, but that codec
//! accepts 16 MiB frames; ours rejects anything above `RPC_FRAME_MAX_BYTES`.

use bytes::{Buf, BytesMut};
use prost::Message;
use tokio_util::codec::Decoder;

use crate::limits::RPC_FRAME_MAX_BYTES;
use crate::rpc::pb;

/// Longest varint prefix we accept; 64 KiB needs three bytes.
const PREFIX_MAX_BYTES: usize = 5;

/// A frame could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the frame limit")]
    TooLarge(usize),
    #[error("invalid varint length prefix")]
    InvalidLength,
    #[error("malformed protobuf frame: {0}")]
    Malformed(String),
    #[error("transport i/o error: {0}")]
    Io(String),
}

impl From<std::io::Error> for FrameError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Incremental decoder for device-to-host frames.
#[derive(Debug, Clone, Copy)]
pub struct RpcCodec {
    max_frame: usize,
}

impl RpcCodec {
    #[must_use]
    pub fn new(max_frame: usize) -> Self {
        Self { max_frame }
    }
}

impl Default for RpcCodec {
    fn default() -> Self {
        Self::new(RPC_FRAME_MAX_BYTES)
    }
}

impl Decoder for RpcCodec {
    type Item = pb::Main;
    type Error = FrameError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<pb::Main>, FrameError> {
        let mut length = 0usize;
        let mut prefix = None;
        for (index, &byte) in src.iter().take(PREFIX_MAX_BYTES).enumerate() {
            length |= usize::from(byte & 0x7F) << (7 * index);
            // A partial value only grows, so it can be rejected early.
            if length > self.max_frame {
                return Err(FrameError::TooLarge(length));
            }
            if byte & 0x80 == 0 {
                prefix = Some(index + 1);
                break;
            }
        }
        let Some(prefix) = prefix else {
            return if src.len() >= PREFIX_MAX_BYTES {
                Err(FrameError::InvalidLength)
            } else {
                Ok(None)
            };
        };
        if src.len() < prefix + length {
            src.reserve(prefix + length - src.len());
            return Ok(None);
        }
        src.advance(prefix);
        let body = src.split_to(length);
        pb::Main::decode(body)
            .map(Some)
            .map_err(|error| FrameError::Malformed(error.to_string()))
    }
}

/// Encodes one host-to-device frame, refusing frames above `max_frame`.
pub fn encode_frame(message: &pb::Main, max_frame: usize) -> Result<Vec<u8>, FrameError> {
    let length = message.encoded_len();
    if length > max_frame {
        return Err(FrameError::TooLarge(length));
    }
    Ok(message.encode_length_delimited_to_vec())
}
