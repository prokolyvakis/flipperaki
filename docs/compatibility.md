# Compatibility

## Buddy FAP SDK pin

Matches the user's device (recorded 2026-11-20 over USB `info device`: `firmware.version
mntm-012`, commit `e1784e74`, build date 31-12-2025, API 87.1, hardware target 7, protobuf
0.25). The FAP only loads on firmware whose API major matches and whose minor is >= the SDK's.

| Item | Value |
|------|-------|
| Firmware | Momentum release `mntm-012` (release channel of `https://up.momentum-fw.dev/firmware/directory.json`) |
| SDK archive | `https://up.momentum-fw.dev/builds/firmware/mntm-012/flipper-z-f7-sdk-mntm-012.zip` |
| SDK sha256 | `c90530c3e0aa7bc4356b8521df20d23d7b4a5435ab58a2530ffb696200408cc2` (matches the index entry) |
| Target / API | f7, API 87.1 (`targets/f7/api_symbols.csv`; uFBT APPCHK reports `Target: 7, API: 87.1`) |
| Firmware source checked | `Next-Flip/Momentum-Firmware` tag `mntm-012` = `e1784e7418d8b074e971983ceb6fef0f37e52ae4` |
| uFBT | 0.2.6 (PyPI), state in `./.ufbt`, toolchain version 39 |
| nanopb | generator 0.4.8 (`scripts/gen-protocol.sh`); the SDK ships and exports nanopb-0.4.8 (`pb_decode`, `pb_encode`, stream helpers), so the FAP links the firmware's copy |

`scripts/build-fap.sh` downloads the archive, verifies the sha256 and deploys it
with `ufbt update --local`.

## SDK facts the FAP relies on

Verified against the pinned SDK headers and the `mntm-012` firmware source:

- `view_set_ascii_callback` exists and is exported. Unconsumed ASCII is turned
  into InputEvents with `sequence_source` 0 (HARDWARE) by
  `view_dispatcher_handle_ascii`, so the Buddy view consumes all ASCII.
- `InputEvent.sequence_source`: `INPUT_SEQUENCE_SOURCE_HARDWARE` (0) is set by
  the input service GPIO path; RPC GUI input (`rpc_gui.c`) sets
  `INPUT_SEQUENCE_SOURCE_SOFTWARE` (1).
- RPC app lifetime (`rpc_app.c`, `rpc.c`): see the comment block at the top of
  `flipper/buddy_rpc.c`.
- Furi tick rate is 1000 Hz; the app checks this at startup.
- CLI `input send` (`input_cli.c`, `fake_input`) publishes InputEvents without
  setting `sequence_source`, so they can read as HARDWARE (0). Buddy therefore
  also samples the button GPIO (`input_pins`, pressed when `read ^ inverted`)
  and requires every event to agree with the physical pin.

## Host

| Item | Value |
|------|-------|
| Tested build host | macOS 26.6.2 (25G83), arm64 (Apple Silicon) |
| Rust | 1.100.0 (`rust-toolchain.toml`) |
| Packaging | `scripts/package-macos.sh` → ad-hoc signed `Flipperaki.app` (bundle id `io.github.prokolyvakis.flipperaki`); not notarized |
| Intel Macs | not built or tested |

## Apps

No app workflow has been tested on hardware yet. `flipper_apps` marks every
entry `tested: false` until a workflow is recorded in `docs/verification.md`.
Apps that switch the BLE or USB profile (e.g. Bluetooth HID remotes) are
expected to end the session; that is a transport boundary, not a bug to work
around.
