# Hardware acceptance checklist

Run on the real Mac, Momentum device and Claude client. Record for each run: date, Mac model and
macOS version, `uname -m`, Momentum version/build/API (Settings → About, or `flipperaki status`),
Claude Code version (`claude --version`), host commit, FAP sha256, and the observed result
(passed / failed / skipped / not run) with notes. Use only harmless apps.

Preparation

```sh
cargo build --release
scripts/build-fap.sh                      # note the printed sha256
# copy flipper/dist/flipperaki.fap to /ext/apps/Tools/ on the Flipper
target/release/flipperaki scan
target/release/flipperaki enroll ble --peripheral <id>
target/release/flipperaki daemon          # separate terminal; FLIPPERAKI_LOG=debug for detail
```

| # | Scenario | Steps | Pass when |
| --- | --- | --- | --- |
| 1 | BLE enrollment and reconnect | First connect (accept macOS pairing + Flipper passkey); stop/start daemon; toggle Flipper Bluetooth | Same peripheral reconnects; `status` shows a new epoch each time; control is `none` after reconnect |
| 2 | Real Claude status | `flipperaki install-hooks`; start a Claude Code session and send a prompt | Buddy shows working → turn finished; project name only; no prompt text |
| 3 | Physical approval | Ask Claude to open a harmless app; approve by holding OK on the Flipper | Grant appears in `status`; a second attempt approved via qFlipper remote/ASCII does **not** approve |
| 4 | Handoff loop | Buddy → app → observe → input → observe → finish(return_to_buddy) | Screenshots match the device; Buddy returns with current status |
| 5 | Leave running | finish(leave_running), then exit the app manually | App keeps running; Buddy returns after manual exit |
| 6 | Disconnects | Turn off Flipper BLE while idle / while prompt shown / during input | No hang; prompt cancelled; input result `unknown` or `not_sent`; nothing replayed after reconnect |
| 7 | Lock / pause | Lock the Flipper during control; `flipperaki pause` during control | Grant revoked immediately; reconnect/resume starts disarmed |
| 8 | USB parity | `enroll usb`, restart daemon, repeat 3–5 over cable | Same behavior; switching transport revokes control |
| 9 | Repeated handoffs | 10× open/finish cycle | No growing memory on the Flipper (Settings → Memory), no stuck loader |
| 10 | Real launch path | `claude mcp add flipperaki -- <abs path>/flipperaki mcp`; use from Claude Code (and Desktop if used) | Tools work when launched by Claude, not only from Terminal |

Bluetooth permission: macOS asks the process that opens CoreBluetooth (the daemon's terminal or
app bundle). Record which binary/terminal received the TCC prompt.
