# Buddy protocol

The host and Buddy talk in small protobuf messages called **envelopes**. There is one schema,
[`protocol/buddy.proto`](../protocol/buddy.proto); both sides are generated from it:

| Side | Generator | Where |
| --- | --- | --- |
| Rust host | prost, at build time (`build.rs`) | `OUT_DIR` |
| C app | nanopb 0.4.8 (`scripts/gen-protocol.sh`) | `protocol/generated/`, committed; CI fails on drift |

Fixtures in `protocol/fixtures/` are encoded by each side and decoded by the other, so the two
can never silently disagree.

## Transport

Envelopes ride inside the Flipper's own RPC as the payload of `AppDataExchangeRequest` — no
extra framing, checksum or fragmentation. The firmware delivers them to Buddy only while Buddy
was launched by the host with the `RPC` argument.

```mermaid
flowchart LR
    env["Envelope<br/>≤ 512 bytes"] --> dx["PB.Main<br/>AppDataExchangeRequest"] --> rpc["native RPC session"] --> link["BLE serial<br/>or USB CDC"]
```

## The envelope

```mermaid
classDiagram
    direction LR
    class Envelope {
        uint32 protocol_major = 1
        uint32 epoch ≠ 0
        uint32 sequence ≠ 0
        oneof payload
    }
    class Hello {
        Role role
        uint32 max_message_bytes
    }
    class Snapshot {
        Activity activity
        string session_label ≤24
        uint32 active_sessions
        uint32 age_seconds
        string detail ≤48
    }
    class ArmRequest {
        uint32 request_id ≠ 0
        string app_label ≤24
        string app_path ≤96
        string client_label ≤24
        uint32 max_actions ≠ 0
        uint32 expires_in_seconds ≠ 0
    }
    class ArmCancel {
        uint32 request_id
    }
    class Decision {
        uint32 request_id
        Verdict verdict
    }
    class Receipt {
        uint32 acknowledged_sequence
        ReceiptStatus status
        RejectReason reason
    }
    Envelope *-- Hello
    Envelope *-- Snapshot
    Envelope *-- ArmRequest
    Envelope *-- ArmCancel
    Envelope *-- Decision
    Envelope *-- Receipt
```

| Message | Direction | Meaning |
| --- | --- | --- |
| `Hello` | both | Role and the largest message each side accepts. The host's Hello sets the epoch. |
| `Snapshot` | host → Buddy | The complete status to display. Replaces the previous one; may be coalesced. |
| `ArmRequest` | host → Buddy | Ask the operator to approve one app. Its `request_id` never changes meaning. |
| `ArmCancel` | host → Buddy | Withdraw the pending request. |
| `Decision` | Buddy → host | The operator's physical approve or deny for `request_id`. |
| `Receipt` | Buddy → host | Buddy accepted or rejected a message, and why. (Buddy also understands a host receipt for its `Decision`, but the host does not send one yet, so Buddy shows a denial as "unconfirmed".) |

## A conversation

```mermaid
sequenceDiagram
    participant H as host (epoch 7)
    participant B as Buddy
    H->>B: Hello · role HOST · seq 1
    B-->>H: Receipt seq 1 accepted
    B-->>H: Hello · role BUDDY · max 512
    H->>B: Snapshot · Working · "flipperaki" · 1 session
    B-->>H: Receipt accepted
    H->>B: ArmRequest #3 · counter · /ext/apps/Tools/counter.fap
    B-->>H: Receipt accepted
    Note over B: operator holds OK
    B-->>H: Decision #3 · APPROVE
    H->>B: ArmRequest #4 · bad path
    B-->>H: Receipt rejected · MALFORMED
```

## Rules both sides enforce

| Rule | Value |
| --- | --- |
| Size | an encoded envelope is at most 512 bytes (largest possible today: 189) |
| Version | `protocol_major` must be 1; anything else → `VERSION` |
| Epoch | nonzero and equal to the current connection's; otherwise → `EPOCH` |
| Sequence | nonzero, chosen by the sender |
| Zero values | never authorize: `VERDICT_UNSPECIFIED` and unknown verdicts are denials |
| Text | printable ASCII within its bound; too long is **rejected, never truncated** — a permission prompt must show exactly what will run |
| App identity | `app_path` must be `/ext/apps/<category>/<name>.fap` with `name == app_label` |
| Decoding | into a fresh value, validated before anything live changes |
| Compatibility | add fields only; removed field numbers are reserved |

## Changing the schema

See [Changing the Buddy protocol](../CONTRIBUTING.md#changing-the-buddy-protocol).
