# Implementation status

Branch `feat/flipperaki-rc1` (local, not pushed). Started 2026-11-20 11:22 EET from
`d7d87e3 Initial commit`.

**Delivery state: `IMPLEMENTED_HARDWARE_UNVERIFIED`.** Code, builds and offline tests exist for
both requirements; no Flipper was connected and Bluetooth was off on the Mac during this
session, so no device or real-client gate has been exercised.

## Backlog

| Step | State | Notes |
| --- | --- | --- |
| P0 Baseline | done | Toolchain pins, AGENTS.md, cog, cargo-deny, CI (`.github/workflows/ci.yml`). Device firmware not yet recorded. |
| P1 Shared contract | done | `protocol/buddy.proto`, nanopb options, generated C, Rust↔C fixtures both directions. |
| P2 RPC hardening | done (offline) | Own narrow executor in `src/rpc/` reusing `flipper-companion-protocol` schema types; deadlines owned by the executor, bounded pending/multipart/events, no mid-frame abandonment, one mutation in flight. |
| P3 Native BLE | implemented, unverified | btleplug 0.13.5 vendored with a 2-hunk patch so notification loss ends the stream; flow-control credits; enrollment by CoreBluetooth id. Pairing/TCC untested. |
| P4 Buddy FAP | implemented, unverified on device | Builds for Momentum mntm-012 / API 87.1. ASCII consumed, hardware origin + GPIO pin agreement, released-keys arming, immutable request binding, SessionClose priority path. |
| P5 Observation and input | implemented, unverified | Screen stream to latest-frame slot, PNG observations with epoch/sequence/age, tap/hold with stop-at-first-failure, typed delivery. |
| P6 Authorization and handoff | implemented, unverified | Pure grant decisions + daemon handoff (Buddy exit → closed → loader free → normal launch → fresh frame); leave-running; Buddy auto-restored when the loader is free. |
| P7 Six MCP tools | done (offline smoke) | rmcp stdio; tools listed and denials verified against a running daemon without a device. |
| P8 Claude Code hooks | done (offline smoke) | Allowlisted fields; reversible settings merge with backup. Not yet installed into the user's real settings. |
| P9 USB parity | implemented, unverified | Same session/broker; VID/PID discovery; CLI → RPC handshake. |
| P10 Operations | partial | status/pause/resume/enroll/scan/install-hooks/uninstall-hooks; ad-hoc app bundle. No launchd agent yet (run the daemon manually). |
| P11 Release validation | not started | Needs hardware: see `docs/hardware-checklist.md`. |

## Accepted deviations

- **RPC executor instead of patching the upstream actor.** `flipper-companion-protocol` 0.8.2's
  `RpcSession` lacks executor-owned deadlines, aggregate bounds and backpressure-safe writes.
  Patching its 1300-line actor was larger than a narrow executor over its generated schema; only
  one session implementation is used (`src/rpc/session.rs`). Recorded in `docs/dependencies.md`.
- **Rust Buddy bindings generated at build time** (prost in `build.rs`) from the same schema; C
  bindings are committed. CI checks C regeneration produces no diff.
- **No builtin app catalog.** Nothing has been verified on hardware, so `flipper_apps` lists only
  discovered FAPs under `/ext/apps/<category>/` and marks all `tested: false`.
- **Observation requires an active grant.** Screenshots go to the Claude service, so the screen is
  only streamed while the operator has approved a workflow.
- **Return-to-Buddy never sends Back.** If the app is still open, `flipper_finish` reports
  `not_sent` and asks the operator to exit it; Buddy relaunches automatically once the loader is free.
- **Pause is processed between device operations**, not mid-sequence; a sequence is at most three
  taps (≤1.5 s holds), and each step checks the session.
- **License metadata** `AGPL-3.0-only` was chosen for `Cargo.toml`; the `LICENSE` text is the
  standard AGPL-3.0 and does not itself state "or later". Change it if you prefer `-or-later`.

## Known limitations

- Global input bus: the broker checks the loader is still busy before each input but cannot prove
  which app is in the foreground.
- A callback the firmware already started can still race a *local* Buddy exit; mitigated with a
  100 ms grace period (firmware offers no barrier).
- Ad-hoc signed bundle only; no notarization, no Intel build.
- Claude Desktop chat activity is not visible (MCP only); Desktop Hardware Buddy (NUS) mode is
  not implemented.

## Next action

Connect the Flipper (USB first is fine for diagnosis), record the installed Momentum version, and
run `docs/hardware-checklist.md` scenarios 1–4 over BLE. If the device firmware is not API 87.x,
re-pin the SDK in `scripts/build-fap.sh` before installing the FAP.
