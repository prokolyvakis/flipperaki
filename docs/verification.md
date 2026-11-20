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

## Not run

- **Every hardware scenario** in `docs/hardware-checklist.md` (no Flipper connected; Mac Bluetooth
  was off). BLE pairing/TCC, USB handshake on real firmware, Buddy on device, physical approval,
  app handoff, stack/heap headroom: **not run**.
- Real Claude Code hook delivery and Claude-launched MCP against a connected device: **not run**
  (hooks were not installed into the user's settings).
- CI on GitHub: **not run** (branch not pushed). Repository protection settings: **not changed**.
- Fuzzing, coverage reporting: **not run**.
