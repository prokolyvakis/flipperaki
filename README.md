# Flipperaki

*φλιπεράκι* — a Bluetooth-first Claude desk buddy and MCP controller for Flipper Zero.
Monitor Claude's activity and operate Momentum apps over BLE or USB. Built with Rust and C.

- **Buddy**: Claude Code activity (working, needs attention, turn finished) shown quietly on the Flipper.
- **Controller**: Claude opens an installed app, sees the screen, presses buttons and finishes — only after you approve on the Flipper itself.

> Status: release candidate in development. See `docs/implementation-status.md` and
> `docs/verification.md` for what has actually been tested on hardware.

## How it works

One host daemon owns the single native Flipper RPC connection (Bluetooth by default, USB as an
alternative). Claude Code hooks and the MCP server are thin clients of that daemon. A small C app
(`flipperaki.fap`) on the Flipper shows status and asks for a physical decision before Claude can
control any app. See `docs/architecture.md`.

## Requirements

- macOS (Apple Silicon tested), Flipper Zero on Momentum firmware (see `docs/compatibility.md`).
- Rust 1.100.0 (`rust-toolchain.toml`), `protoc`.
- For the FAP: Python via `uv`, uFBT (project-local), Momentum SDK matching your device.

## Build

```sh
scripts/package-macos.sh         # host: dist/Flipperaki.app (ad-hoc signed) + dist/SHA256SUMS
scripts/build-fap.sh             # FAP: flipper/dist/flipperaki.fap
```

Use the binary inside the bundle (`dist/Flipperaki.app/Contents/MacOS/flipperaki`, called
`flipperaki` below) so macOS grants Bluetooth to one stable app identity.

## Set up

1. Copy `flipperaki.fap` to `/ext/apps/Tools/` on the Flipper (qFlipper or SD card).
2. Choose the device:
   ```sh
   flipperaki scan                                   # list BLE peripherals and USB ports
   flipperaki enroll ble --peripheral <id>           # or: flipperaki enroll usb
   ```
   The first BLE connection triggers macOS pairing; confirm the passkey shown on the Flipper.
3. Run the daemon (keep it running): `open dist/Flipperaki.app --args daemon`. Launching through
   `open` makes Flipperaki (not your terminal) the app macOS asks for Bluetooth permission; allow
   it. For logs, run `flipperaki daemon` in a terminal instead (the terminal then needs Bluetooth
   permission). Stop it with `pkill -f "flipperaki daemon"`.
4. Claude Code activity: `flipperaki install-hooks` (backs up `~/.claude/settings.json` once and
   only adds Flipperaki entries).
5. MCP: `claude mcp add flipperaki -- /path/to/flipperaki mcp` (or add the same command to
   Claude Desktop's MCP config).

## Use

Ask Claude e.g. "open the Clock app on my Flipper and show me the screen". Claude calls
`flipper_open`; Buddy shows the request; **hold OK on the Flipper to approve** (Back denies).
Claude then observes and presses buttons, and finishes by leaving the app running or returning
to Buddy.

Buddy lights the screen when a request arrives. Hold Back on Buddy to put it away: it stays
closed while you use the Flipper and reopens by itself only when Claude asks for control.

`flipperaki pause` revokes control immediately; `flipperaki resume` allows new requests (each
still needs approval on the device) and brings Buddy back. `flipperaki status` shows the current state.

## Privacy

Buddy receives only a coarse state, a short project-directory name and a session count — never
prompts, tool arguments, paths or transcripts. Screenshots requested through MCP are sent to the
Claude service. Everything else stays on your Mac; logs go to stderr and are redacted.

## Remove

```sh
flipperaki uninstall-hooks
claude mcp remove flipperaki
rm -rf ~/Library/Application\ Support/flipperaki
```
Delete `/ext/apps/Tools/flipperaki.fap` from the Flipper and unpair it in macOS Bluetooth settings.

## License

AGPL-3.0 (see `LICENSE`). Third-party notices: `docs/dependencies.md`.
