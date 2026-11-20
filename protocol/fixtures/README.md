# Cross-language Buddy fixtures

Each `.bin` file is one encoded `flipperaki.buddy.Envelope`, without any length
prefix. Tests compare decoded semantics, not bytes across languages.
Unless noted, envelopes have `protocol_major=1` and `epoch=7`.

## `c/`: encoded by C (nanopb 0.4.8)

Written by `flipper/tests/build/buddy_fixtures --write protocol/fixtures/c`;
`scripts/check-c.sh` fails if they differ from what the C encoder produces now.

| File | Meaning | Expected on decode |
|------|---------|--------------------|
| `hello_buddy.bin` | seq 1, Hello role=BUDDY, max_message_bytes=512 | valid |
| `decision_approve.bin` | seq 2, Decision request_id=42 verdict=APPROVE | valid |
| `decision_deny.bin` | seq 2, Decision request_id=42 verdict=DENY | valid |
| `decision_unspecified.bin` | seq 2, Decision request_id=42 verdict=UNSPECIFIED | decodes; must never authorize |
| `invalid_version.bin` | as decision_deny, protocol_major=2 | reject: version |
| `invalid_truncated.bin` | decision_deny cut 3 bytes short | reject: does not decode |
| `invalid_label_too_long.bin` | seq 2, Snapshot WORKING, session_label of 30 bytes (bound 24) | reject: exceeds schema bound |
| `invalid_oversized.bin` | hello_buddy plus an unknown field 100, 513 bytes total | reject: above the 512-byte limit |

## `rust/`: encoded by the Rust host (owned by the host side; do not regenerate here)

| File | Meaning | Expected on decode |
|------|---------|--------------------|
| `hello_host.bin` | seq 1, Hello role=HOST, max_message_bytes=512 | valid |
| `snapshot_working.bin` | seq 2, Snapshot WORKING, label "flipperaki", sessions 2, age 5, detail "" | valid |
| `arm_request.bin` | seq 3, ArmRequest id 42, "clock", "/ext/apps/Tools/clock.fap", "client 1", max_actions 40, expires 30 | valid |
| `arm_cancel.bin` | seq 4, ArmCancel id 42 | valid |
| `invalid_snapshot_unspecified_activity.bin` | seq 5, Snapshot activity UNSPECIFIED | decodes; rejected by validation |
