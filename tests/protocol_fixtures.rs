//! Cross-language Buddy fixtures. `protocol/fixtures/rust/` is encoded here
//! and decoded by the C tests; `protocol/fixtures/c/` is encoded by C and
//! decoded here. Semantics are compared, not serializer byte order.
//!
//! Regenerate the Rust-side files with `FLIPPERAKI_WRITE_FIXTURES=1 cargo test`.

use std::path::{Path, PathBuf};

use flipperaki::buddy::pb::{self, envelope::Payload};
use flipperaki::buddy::{BuddyError, FromBuddy, decode_from_buddy};
use prost::Message;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("protocol/fixtures")
}

fn envelope(sequence: u32, payload: Payload) -> pb::Envelope {
    pb::Envelope {
        protocol_major: 1,
        epoch: 7,
        sequence,
        payload: Some(payload),
    }
}

fn rust_fixtures() -> Vec<(&'static str, pb::Envelope)> {
    vec![
        (
            "hello_host",
            envelope(
                1,
                Payload::Hello(pb::Hello {
                    role: pb::Role::Host as i32,
                    max_message_bytes: 512,
                }),
            ),
        ),
        (
            "snapshot_working",
            envelope(
                2,
                Payload::Snapshot(pb::Snapshot {
                    activity: pb::Activity::Working as i32,
                    session_label: "flipperaki".into(),
                    active_sessions: 2,
                    age_seconds: 5,
                    detail: String::new(),
                }),
            ),
        ),
        (
            "arm_request",
            envelope(
                3,
                Payload::ArmRequest(pb::ArmRequest {
                    request_id: 42,
                    app_label: "clock".into(),
                    app_path: "/ext/apps/Tools/clock.fap".into(),
                    client_label: "client 1".into(),
                    max_actions: 40,
                    expires_in_seconds: 30,
                }),
            ),
        ),
        (
            "arm_cancel",
            envelope(4, Payload::ArmCancel(pb::ArmCancel { request_id: 42 })),
        ),
        (
            "invalid_snapshot_unspecified_activity",
            envelope(
                5,
                Payload::Snapshot(pb::Snapshot {
                    activity: 0,
                    ..Default::default()
                }),
            ),
        ),
    ]
}

#[test]
fn rust_fixtures_are_committed_and_current() {
    let dir = fixtures().join("rust");
    for (name, envelope) in rust_fixtures() {
        let path = dir.join(format!("{name}.bin"));
        if std::env::var_os("FLIPPERAKI_WRITE_FIXTURES").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, envelope.encode_to_vec()).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(bytes.len() <= 512, "{name} exceeds the message limit");
        assert_eq!(
            pb::Envelope::decode(bytes.as_slice()).unwrap(),
            envelope,
            "{name} changed"
        );
    }
}

/// Files the C encoder writes, with the meaning the host must decode.
#[test]
fn c_encoded_fixtures_decode_with_the_same_meaning() {
    let dir = fixtures().join("c");
    if !dir.exists() {
        panic!(
            "{} missing: run scripts/check-c.sh to produce C fixtures",
            dir.display()
        );
    }
    let read = |name: &str| std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
    assert_eq!(
        decode_from_buddy(&read("decision_approve"), 7),
        Ok(FromBuddy::Decision {
            request_id: 42,
            approved: true
        })
    );
    assert_eq!(
        decode_from_buddy(&read("decision_deny"), 7),
        Ok(FromBuddy::Decision {
            request_id: 42,
            approved: false
        })
    );
    assert_eq!(
        decode_from_buddy(&read("hello_buddy"), 7),
        Ok(FromBuddy::Hello {
            max_message_bytes: 512
        })
    );
    assert_eq!(
        decode_from_buddy(&read("decision_unspecified"), 7),
        Err(BuddyError::Invalid("verdict"))
    );
}
