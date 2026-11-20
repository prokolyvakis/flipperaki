# Architecture

```
Claude Code hooks ──┐
Claude MCP (stdio) ─┼─ private Unix socket ─▶ flipperaki daemon ─▶ native RPC (BLE | USB) ─▶ Momentum
operator CLI ───────┘   (0700 dir, 0600)       (sole device owner)                            ├─ Buddy FAP (status + physical approval)
                                                                                                └─ target app (screen + buttons)
```

One Rust package, one binary:

| Mode | Role |
| --- | --- |
| `daemon` | Owns the single device connection, Buddy state and control grants. Process lock prevents a second owner. |
| `mcp` | Thin stdio MCP server (six tools) forwarding to the daemon over IPC. stdout = MCP only. |
| `hook` | Reads one Claude Code hook payload (allowlisted fields), forwards activity, exits 0 within 500 ms, prints nothing. |
| `status` / `pause` / `resume` / `enroll` / `scan` / `install-hooks` / `uninstall-hooks` | Operator commands. |

## Modules

| Path | Responsibility |
| --- | --- |
| `src/limits.rs` | Every resource bound. |
| `src/buddy.rs` | Buddy envelope encode/validate (host side); `ActivityBoard` status aggregation. |
| `src/hooks.rs` | Allowlisted hook parsing → `ActivityReport`. |
| `src/broker.rs` | Pure control decisions: `begin_request`, `resolve_decision`, `authorize_input`, `check_owner`. |
| `src/daemon.rs` | Imperative broker loop: connection lifecycle, epochs, handoff, I/O. |
| `src/ipc.rs` | Bounded newline-JSON IPC, server-assigned `ClientId`, client-gone notifications. |
| `src/mcp.rs` | rmcp tool surface. |
| `src/setup.rs` | Reversible Claude settings merge. |
| `src/config.rs` | Enrolled device config. |
| `src/rpc/` | Narrow native RPC executor (deadlines, bounds, routing, cancellation). |
| `src/transport/` | BLE (btleplug/CoreBluetooth) and USB (tokio-serial) byte streams. |
| `src/device.rs` | Named device operations. |
| `flipper/` | Buddy FAP: portable core, nanopb boundary, RPC adapter, view. |

## Control lifecycle

1. `flipper_open(app)` → broker admits one `PendingRequest` (client, epoch, app, 30 s expiry) and sends `ArmRequest` to Buddy.
2. The operator approves with a fresh **hardware-origin** gesture on Buddy (ASCII and software input are consumed/rejected).
3. Buddy returns `Decision`; broker validates request id, epoch, expiry → `ControlGrant` (5 min, 40 actions).
4. Broker exits Buddy, waits for app closure and loader availability, launches the target normally, starts the screen stream, returns a fresh observation.
5. `flipper_input` (≤3 steps, holds ≤1.5 s) is authorized against the grant before any byte is sent; results report `delivery` separately from observation.
6. `flipper_finish(leave_running | return_to_buddy)` ends the grant.

Pause, lock, disconnect, transport change, client departure and expiry revoke the grant and discard queued input. Reconnect starts disarmed. Resume restores no grant.

## Honest boundaries

- GUI input goes to the device's global input bus; the loader lock is not a foreground-identity API. The broker stops on unexpected observations but cannot detect every manual change.
- An RPC acknowledgement means the event was published, not that the app reached a goal. Unknown outcomes are reported as `unknown` and never replayed.
- Pause stops future broker actions; it cannot stop an operation already running inside an app.
- Screenshots returned through MCP are sent to the Claude service.
- Same-user processes can reach the socket; physical approval and the narrow tool set are the protection, not a host sandbox.
- Claude Code hooks report Code activity only; ordinary Claude Desktop chat activity is not visible through MCP. The Desktop Hardware Buddy (NUS) protocol is a different, unimplemented mode.
