# btleplug 0.13.5 — local patch

Source: crates.io `btleplug` 0.13.5 (upstream git sha
`1af8299c291f24fca68dfb6b7ef1b5c0475892e1`, see `.cargo_vcs_info.json`),
copied verbatim except for the two hunks below. License: BSD-3-Clause
(`LICENSE.md`). Wired in through `[patch.crates-io]` in the root
`Cargo.toml`.

## Loss of notifications and adapter events is silent upstream

Upstream files and lines (0.13.5):

- `src/common/util.rs:18` — `notifications_stream_from_broadcast_receiver`
- `src/common/adapter_manager.rs:58` — `AdapterManager::event_stream`

Both wrap a `tokio::sync::broadcast::Receiver` in `BroadcastStream` and
apply `.filter_map(|x| async move { x.ok() })`. When the consumer falls
behind (the CoreBluetooth notification channel holds only 16 items,
`src/corebluetooth/peripheral.rs:111`), `BroadcastStream` yields
`Err(Lagged(n))`, which this filter discards: the stream simply continues
with a gap. For a byte-stream protocol (Flipper serial over GATT
indications) that is silent data corruption, and a lost
`DeviceDisconnected` event is a missed disconnect.

## Patch

Insert `.take_while(|x| ready(x.is_ok()))` before the `filter_map`, so the
stream **ends** at the first `Lagged`. Flipperaki treats the end of either
stream as a transport failure and fails the RPC session. No API change;
consumers that tolerate gaps now see an early end instead.

## Removal condition

Drop the vendored copy and the `[patch.crates-io]` entry once upstream
btleplug surfaces lag (an error item or a terminated stream) from
`Peripheral::notifications` and `Central::events`.
