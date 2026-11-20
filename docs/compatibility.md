# Compatibility

## Buddy FAP SDK pin (PROVISIONAL)

Provisional until the user's device firmware is recorded. The FAP only loads on
firmware whose API major matches and whose minor is >= the SDK's.

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
