# Buddy protocol

Schema: `protocol/buddy.proto` (single source). Rust bindings: prost via `build.rs`. C bindings: nanopb 0.4.8 via `scripts/gen-protocol.sh`, committed in `protocol/generated/`.

Carried as the payload of native Flipper RPC `AppDataExchangeRequest` — no extra framing, CRC or fragmentation.

| Rule | Value |
| --- | --- |
| Max encoded `Envelope` | 512 bytes, checked by both sides |
| `protocol_major` | 1; any other value rejected |
| `epoch` | host connection epoch, nonzero; mismatches rejected |
| `sequence` | sender-local, nonzero |
| Zero enum values | never authorize; `VERDICT_UNSPECIFIED`, unknown verdicts rejected |
| Labels | printable ASCII, ≤24 chars (detail ≤48, app path ≤96); permission text is rejected, never truncated |
| Compatibility | additive fields only; removed numbers reserved |

Direction:

| Message | Direction | Notes |
| --- | --- | --- |
| `Hello` | both | role + max message bytes |
| `Snapshot` | host → Buddy | complete replaceable display state; may be coalesced |
| `ArmRequest` | host → Buddy | one pending request; immutable id |
| `ArmCancel` | host → Buddy | withdraws the pending request |
| `Decision` | Buddy → host | physical approve/deny for `request_id` |
| `Receipt` | both | application-level acceptance of a sequence |

Decoding always targets a fresh value; validation precedes commit, so a bad message never partially replaces a live prompt.
