//! Framing: arbitrary splits and concatenations decode identically; bad
//! length prefixes are rejected.

use bytes::BytesMut;
use flipperaki::limits::RPC_FRAME_MAX_BYTES;
use flipperaki::rpc::{FrameError, RpcCodec, encode_frame, pb, pb_system};
use proptest::prelude::*;
use tokio_util::codec::Decoder;

fn message(command_id: u32, data: Vec<u8>, has_next: bool) -> pb::Main {
    pb::Main {
        command_id,
        command_status: 0,
        has_next,
        content: Some(pb::main::Content::SystemPingResponse(
            pb_system::PingResponse { data },
        )),
    }
}

fn messages() -> impl Strategy<Value = Vec<pb::Main>> {
    let one = (
        any::<u32>(),
        prop::collection::vec(any::<u8>(), 0..300),
        any::<bool>(),
    )
        .prop_map(|(id, data, has_next)| message(id, data, has_next));
    prop::collection::vec(one, 1..8)
}

proptest! {
    #[test]
    fn decodes_any_split_of_concatenated_frames(
        originals in messages(),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12),
    ) {
        let stream: Vec<u8> = originals
            .iter()
            .flat_map(|m| encode_frame(m, RPC_FRAME_MAX_BYTES).unwrap())
            .collect();
        let mut cuts: Vec<usize> = cuts.iter().map(|i| i.index(stream.len() + 1)).collect();
        cuts.push(stream.len());
        cuts.sort_unstable();

        let mut codec = RpcCodec::default();
        let mut buffer = BytesMut::new();
        let mut decoded = Vec::new();
        let mut start = 0;
        for cut in cuts {
            buffer.extend_from_slice(&stream[start..cut]);
            start = cut;
            while let Some(message) = codec.decode(&mut buffer).unwrap() {
                decoded.push(message);
            }
        }
        prop_assert_eq!(decoded, originals);
        prop_assert!(buffer.is_empty());
    }

    #[test]
    fn rejects_any_length_above_the_limit(length in (RPC_FRAME_MAX_BYTES + 1)..=u32::MAX as usize) {
        let mut buffer = BytesMut::new();
        prost::encoding::encode_varint(length as u64, &mut buffer);
        let result = RpcCodec::default().decode(&mut buffer);
        prop_assert!(matches!(result, Err(FrameError::TooLarge(_))), "{result:?}");
    }
}

#[test]
fn rejects_unterminated_varint() {
    let mut buffer = BytesMut::from(&[0x80u8; 4][..]);
    assert_eq!(RpcCodec::default().decode(&mut buffer), Ok(None));
    let mut buffer = BytesMut::from(&[0x80u8; 5][..]);
    assert_eq!(
        RpcCodec::default().decode(&mut buffer),
        Err(FrameError::InvalidLength)
    );
}

#[test]
fn rejects_malformed_body() {
    let mut buffer = BytesMut::from(&[2u8, 0xFF, 0xFF][..]);
    assert!(matches!(
        RpcCodec::default().decode(&mut buffer),
        Err(FrameError::Malformed(_))
    ));
}

#[test]
fn refuses_to_encode_oversized_frames() {
    let big = message(1, vec![0; RPC_FRAME_MAX_BYTES], false);
    assert!(matches!(
        encode_frame(&big, RPC_FRAME_MAX_BYTES),
        Err(FrameError::TooLarge(_))
    ));
}
