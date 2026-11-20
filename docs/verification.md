# Verification record

## Offline checks — 2026-11-20 ~12:05 EET

Host: macOS 26.6.2 (25G83), arm64. Rust 1.100.0. Tree: `be440b2` (`feat/flipperaki-rc1`), each
command run separately in the main checkout; exit status taken from the shell.

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all --check` | 0 | clean |
| `cargo clippy --all-targets --locked -- -D warnings` | 0 | clean |
| `cargo nextest run --all-targets --locked` | 0 | 80 passed, 0 skipped |
| `cargo test --doc --locked` | 0 | 0 doctests (none written) |
| `cargo deny check` | 0 | advisories/licenses/sources ok; duplicate-version warnings only |
| `scripts/gen-protocol.sh` then `git diff --exit-code -- protocol/generated` | 0, 0 | generated C up to date; max encoded Envelope 189 B ≤ 512 |
| `scripts/check-c.sh` | 0 | 36 Unity tests + fixture check under ASan/UBSan, `-Werror`; clang-tidy clean |
| `scripts/build-fap.sh` | 0 | APPCHK Target 7, API 87.1 (Momentum mntm-012 SDK, provisional) |
| `scripts/package-macos.sh` | 0 | ad-hoc signed `Flipperaki.app`, `codesign --verify --strict` ok |
| per-commit `cog verify` over `git rev-list d7d87e3..be440b2` | 0 | 28 commits conventional; `Initial commit` preserved |

Artifact checksums from that run (`dist/SHA256SUMS`):

```
b983a367d23b2777b287f0d1b257d9b38d34650224c7505aa397c71ab5575147  Flipperaki.app/Contents/MacOS/flipperaki
ff66b343a349929f30dca822de909fe227220c49d5fe97bf4a4198ea9353e96d  flipperaki.fap
```

The FAP hash is stable across rebuilds in one checkout but differs between checkouts (the build
embeds paths); compare hashes from the same build.

### What the 80 host tests cover

- RPC executor (fake stream): byte-boundary splits and concatenation (proptest), oversized and
  malformed length prefixes, expired queued mutation sends zero bytes, late reply never completes
  another request, stalled partial write fails the session, event-queue overflow fails the
  session, bounded multipart aggregate, one mutation in flight.
- Device ops: tap/hold sequences, stop at first failure, typed not-sent/rejected/unknown.
- Transports: USB port selection and CLI→RPC handshake on a fake stream; BLE UUIDs derived from
  firmware byte arrays, credit accounting, MTU chunking, notification-stream end = failure.
- Buddy protocol: default/unknown verdicts never approve, epoch/version/size, no truncation of
  permission text, Rust↔C fixtures in both directions.
- Grants: two clients cannot share; stale epoch, wrong request, expiry, budget, hold limit, pause.
- Daemon (fake Flipper over duplex, paused time): Buddy launch and handshake, status snapshots,
  full approved handoff with targets launched *without* the RPC argument, wrong/deny/unspecified
  decisions never launch (and withdraw the prompt), second client denied, input only while the
  loader is busy, lock and pause revoke, client departure cancels, reconnect starts disarmed with
  a new epoch.
- Hooks/IPC/setup: allowlist (no prompt or path leaks), 0600 socket, server-assigned identities,
  oversized IPC lines, reversible settings merge preserving permissions.

### Smoke test without a device

`target/debug/flipperaki daemon` in a temporary `FLIPPERAKI_HOME`: a second daemon refused
(exit 1); `status` reports disconnected; `hook` exits 0 with no stdout; `mcp` initializes, lists
exactly the six tools, and `flipper_input`/`flipper_open` return `no_grant`/`disconnected`
tool errors; prompt text from the hook appears in no log.

## Hardware run over USB — 2026-11-20 14:30–15:01 EET

Mac15,10, macOS 26.6.2 (25G83), arm64; Claude Code 2.1.311 (not used as a client in this run).
Flipper Zero "Ladramd", hardware target 7, Momentum `mntm-012` (commit `e1784e74`, API 87.1,
protobuf 0.25), connected by USB cable (`/dev/cu.usbmodemflip_Ladramd1`).
FAP `ff66b343…e96d` copied to `/ext/apps/Tools/flipperaki.fap` with the uFBT `storage.py send`.
Host: `target/release/flipperaki` built from `fca2803`, `enroll usb`, `daemon` with
`FLIPPERAKI_LOG=flipperaki=debug`. The six tools were driven through `flipperaki mcp` over stdio
by a scripted JSON-RPC client; the operator pressed the Flipper buttons.

| Checklist # | Observed | Result |
| --- | --- | --- |
| prep | `status` connected/usb; after the fix below `firmware: "mntm-012 (Momentum)"` | passed |
| Buddy launch | Buddy started with `RPC`, Hello↔Hello, Snapshot accepted; operator saw "Idle, 0 sessions" | passed |
| 3 physical approval | hold OK → `Decision{approved:true}` in 7–14 s, grant shown in `status` (3 runs) | passed |
| 3 deny | Back → `Decision{approved:false}`, `denied_by_operator`, nothing launched | passed |
| 3 no decision | request unanswered → `expired` at 30 s, `ArmCancel` accepted, control `none` | passed |
| 3 software approval refused | qFlipper remote/CLI `input send` while prompting | **not run** (USB port owned by the daemon) |
| 4 handoff loop | Counter launched without `RPC`; screenshot 0 → two Up taps → screenshot 2; inputs `acknowledged` | passed |
| 4 return_to_buddy | `not_sent` "the app is still open" (by design); after manual exit Buddy relaunched by itself | passed |
| 6 disconnect (USB, idle) | cable pulled: session failed `Device not configured`, no hang; reconnect epoch 2, control `none`; Buddy restored after Counter exit | passed |
| 6 disconnect during prompt / input | not run (the operator did not unplug during control) | not run |
| 7 pause | `pause` during control → next input and open `denied: paused`; `resume` leaves control `none` | passed |
| 7 lock | not run | not run |
| 1, 2, 5, 8 (BLE parts), 9, 10 | not run | not run |

Unexplained: of six prompts sent, one (seq 12) was accepted by Buddy (`Receipt accepted`) but the
operator reported no prompt on screen; it expired normally. Not reproduced on the next three
prompts; one earlier expiry was with nobody watching. Treat as open until seen again.

Fixed during the run (`fca2803`): devinfo multipart replies end with an `Empty` part and use
dotted keys, so `firmware` was always `null`.

## Not run

- **BLE**: pairing/TCC, BLE transport on device, BLE disconnects, transport switching: **not run**.
- Software-injected approval attempts, lock revocation, 10× handoff memory check, stack/heap
  headroom: **not run**.
- Real Claude Code hook delivery and Claude-launched MCP against a connected device: **not run**
  (hooks were not installed into the user's settings).
- CI on GitHub: **not run** (branch not pushed). Repository protection settings: **not changed**.
- Fuzzing, coverage reporting: **not run**.
