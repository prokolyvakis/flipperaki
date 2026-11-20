# Dependencies: reuse, patches and why

## flipper-companion-protocol `=0.8.2` (AGPL-3.0-or-later)

Used for: the generated `pb*` protobuf modules (from the vendored official
`flipperzero-protobuf` definitions) and `ScreenFrame`. Its approaches were
reused, not its code paths, for framing (`codec.rs`) and the USB CLI
handshake (`serial.rs`).

Not used: `RpcSession` (actor, ~1300 lines) and `FrameCodec`.

- `RpcSession` has no per-request deadline enforced by the executor, no
  typed outcome separating not-sent / acknowledged / rejected / unknown, no
  bound on pending requests, and broadcasts unsolicited frames into a lossy
  channel. Patching it would touch most of the file, so `src/rpc/session.rs`
  is a narrow executor written for these contracts instead.
- `FrameCodec` accepts 16 MiB frames (`MAX_FRAME_SIZE`). `src/rpc/codec.rs`
  is a ~60-line decoder for the same varint-delimited `PB.Main` format with
  our `RPC_FRAME_MAX_BYTES` (64 KiB) bound.
- `serial::start_session` returns its own `RpcSession` and may consume RPC
  bytes that follow the CLI echo; `src/transport/usb.rs` repeats its DTR /
  prompt / `start_rpc_session\r` / echo sequence, reading byte by byte.

License: AGPL-3.0-or-later is compatible with this project's AGPL-3.0-only
(we take it under version 3). Its protobuf definitions are GPL-3.0-or-later,
also compatible.

## btleplug 0.13.5 (BSD-3-Clause), vendored and patched

`third_party/btleplug`, wired through `[patch.crates-io]`. Upstream drops
`broadcast::Lagged` errors in `Peripheral::notifications` and
`Central::events`, so lost GATT indications or disconnect events go
unnoticed. The two-hunk patch ends those streams at the first lag; the BLE
transport treats a stream end as a transport failure. Details, upstream
lines and removal condition: `third_party/btleplug/PATCHES.md`.

## Feature changes

- `tokio-util`: added `io` (`StreamReader`, `SinkWriter`, `CopyToBytes`)
  to bridge BLE channels into one `AsyncRead + AsyncWrite` stream.
