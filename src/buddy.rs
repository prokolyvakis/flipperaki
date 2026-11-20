//! The Buddy protocol boundary (host side) and Claude activity aggregation.
//!
//! Encoding and validation are pure: bytes in, typed values out. The daemon
//! decides what to send and when.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use prost::Message;
use thiserror::Error;

use crate::limits::{BUDDY_MESSAGE_MAX_BYTES, BUDDY_PROTOCOL_MAJOR, TRACKED_SESSIONS_MAX};

/// Generated from `protocol/buddy.proto`.
#[allow(clippy::all, missing_docs)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/flipperaki.buddy.rs"));
}

use pb::envelope::Payload;

/// Maximum characters of a display label (matches nanopb `max_size - 1`).
pub const LABEL_MAX_CHARS: usize = 24;
/// Maximum characters of a Snapshot detail line.
pub const DETAIL_MAX_CHARS: usize = 48;
/// Maximum characters of an exact app path in an ArmRequest.
pub const APP_PATH_MAX_CHARS: usize = 96;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuddyError {
    #[error("encoded message is {0} bytes, limit is {BUDDY_MESSAGE_MAX_BYTES}")]
    TooLarge(usize),
    #[error("message does not decode")]
    Malformed,
    #[error("unsupported protocol major version {0}")]
    Version(u32),
    #[error("message epoch {got} does not match connection epoch {expected}")]
    Epoch { expected: u32, got: u32 },
    #[error("field {0} is missing, zero or out of range")]
    Invalid(&'static str),
    #[error("label cannot be represented on the device without changing its meaning")]
    Unrepresentable,
    #[error("message type is not accepted from the Buddy app")]
    UnexpectedFromBuddy,
}

/// A validated message received from the Buddy app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromBuddy {
    Hello { max_message_bytes: u32 },
    Decision { request_id: u32, approved: bool },
    Receipt { sequence: u32, accepted: bool },
}

/// Decodes and validates one Buddy message. Unknown or default enum values
/// are rejected, so nothing here can turn into an approval by accident.
pub fn decode_from_buddy(bytes: &[u8], epoch: u32) -> Result<FromBuddy, BuddyError> {
    if bytes.len() > BUDDY_MESSAGE_MAX_BYTES {
        return Err(BuddyError::TooLarge(bytes.len()));
    }
    let envelope = pb::Envelope::decode(bytes).map_err(|_| BuddyError::Malformed)?;
    if envelope.protocol_major != BUDDY_PROTOCOL_MAJOR {
        return Err(BuddyError::Version(envelope.protocol_major));
    }
    if envelope.epoch != epoch {
        return Err(BuddyError::Epoch {
            expected: epoch,
            got: envelope.epoch,
        });
    }
    if envelope.sequence == 0 {
        return Err(BuddyError::Invalid("sequence"));
    }
    match envelope.payload {
        Some(Payload::Hello(hello)) => {
            if hello.role != pb::Role::Buddy as i32 {
                return Err(BuddyError::Invalid("role"));
            }
            Ok(FromBuddy::Hello {
                max_message_bytes: hello.max_message_bytes,
            })
        }
        Some(Payload::Decision(decision)) => {
            if decision.request_id == 0 {
                return Err(BuddyError::Invalid("request_id"));
            }
            let approved = match pb::Verdict::try_from(decision.verdict) {
                Ok(pb::Verdict::Approve) => true,
                Ok(pb::Verdict::Deny) => false,
                _ => return Err(BuddyError::Invalid("verdict")),
            };
            Ok(FromBuddy::Decision {
                request_id: decision.request_id,
                approved,
            })
        }
        Some(Payload::Receipt(receipt)) => {
            if receipt.acknowledged_sequence == 0 {
                return Err(BuddyError::Invalid("acknowledged_sequence"));
            }
            let accepted = match pb::ReceiptStatus::try_from(receipt.status) {
                Ok(pb::ReceiptStatus::Accepted) => true,
                Ok(pb::ReceiptStatus::Rejected) => false,
                _ => return Err(BuddyError::Invalid("status")),
            };
            Ok(FromBuddy::Receipt {
                sequence: receipt.acknowledged_sequence,
                accepted,
            })
        }
        Some(_) => Err(BuddyError::UnexpectedFromBuddy),
        None => Err(BuddyError::Invalid("payload")),
    }
}

/// Builds host-to-Buddy envelopes for one connection epoch.
#[derive(Debug)]
pub struct BuddyEncoder {
    epoch: u32,
    next_sequence: u32,
}

impl BuddyEncoder {
    pub fn new(epoch: u32) -> Self {
        Self {
            epoch,
            next_sequence: 1,
        }
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Encodes `payload`, returning the bytes and the sequence they carry.
    pub fn encode(&mut self, payload: Payload) -> Result<(Vec<u8>, u32), BuddyError> {
        validate_outgoing(&payload)?;
        let sequence = self.next_sequence;
        let envelope = pb::Envelope {
            protocol_major: BUDDY_PROTOCOL_MAJOR,
            epoch: self.epoch,
            sequence,
            payload: Some(payload),
        };
        let bytes = envelope.encode_to_vec();
        if bytes.len() > BUDDY_MESSAGE_MAX_BYTES {
            return Err(BuddyError::TooLarge(bytes.len()));
        }
        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        Ok((bytes, sequence))
    }
}

fn validate_outgoing(payload: &Payload) -> Result<(), BuddyError> {
    match payload {
        Payload::Snapshot(s) => {
            check_label(&s.session_label, LABEL_MAX_CHARS)?;
            check_label(&s.detail, DETAIL_MAX_CHARS)?;
            if s.activity == pb::Activity::Unspecified as i32 {
                return Err(BuddyError::Invalid("activity"));
            }
        }
        Payload::ArmRequest(r) => {
            // A permission's meaning is never truncated to fit: reject instead.
            check_label(&r.app_label, LABEL_MAX_CHARS)?;
            check_label(&r.client_label, LABEL_MAX_CHARS)?;
            check_label(&r.app_path, APP_PATH_MAX_CHARS)?;
            if r.request_id == 0 || r.max_actions == 0 || r.expires_in_seconds == 0 {
                return Err(BuddyError::Invalid("arm_request"));
            }
            if r.app_path.is_empty() || r.app_label.is_empty() {
                return Err(BuddyError::Invalid("app"));
            }
        }
        Payload::ArmCancel(c) if c.request_id == 0 => {
            return Err(BuddyError::Invalid("request_id"));
        }
        Payload::Hello(h) if h.role != pb::Role::Host as i32 => {
            return Err(BuddyError::Invalid("role"));
        }
        _ => {}
    }
    Ok(())
}

fn check_label(text: &str, max_chars: usize) -> Result<(), BuddyError> {
    let printable = text.chars().all(|c| c.is_ascii() && !c.is_ascii_control());
    if !printable || text.len() > max_chars {
        return Err(BuddyError::Unrepresentable);
    }
    Ok(())
}

/// Reduces untrusted text to a short printable ASCII display label.
/// For display-only fields; never use this on permission scopes.
pub fn display_label(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .take(max_chars)
        .collect()
}

/// Coarse Claude activity reported by hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Idle,
    Working,
    NeedsAttention,
    TurnFinished,
}

impl Activity {
    /// Higher wins when several sessions are aggregated into one display.
    fn priority(self) -> u8 {
        match self {
            Activity::NeedsAttention => 3,
            Activity::Working => 2,
            Activity::TurnFinished => 1,
            Activity::Idle => 0,
        }
    }

    fn to_pb(self) -> pb::Activity {
        match self {
            Activity::Idle => pb::Activity::Idle,
            Activity::Working => pb::Activity::Working,
            Activity::NeedsAttention => pb::Activity::NeedsAttention,
            Activity::TurnFinished => pb::Activity::TurnFinished,
        }
    }
}

/// One minimal activity report, already stripped of everything but the
/// fields Buddy needs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ActivityReport {
    pub session_key: String,
    pub label: String,
    /// `None` means the session ended.
    pub activity: Option<Activity>,
}

/// A session not heard from for this long is dropped from the display.
pub const SESSION_STALE_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
struct SessionActivity {
    label: String,
    activity: Activity,
    updated_at: Instant,
}

/// Aggregates hook reports from several Claude sessions into one snapshot.
#[derive(Debug, Default)]
pub struct ActivityBoard {
    sessions: HashMap<String, SessionActivity>,
}

impl ActivityBoard {
    pub fn apply(&mut self, report: ActivityReport, now: Instant) {
        self.sessions
            .retain(|_, s| now.saturating_duration_since(s.updated_at) < SESSION_STALE_AFTER);
        let Some(activity) = report.activity else {
            self.sessions.remove(&report.session_key);
            return;
        };
        if !self.sessions.contains_key(&report.session_key)
            && self.sessions.len() >= TRACKED_SESSIONS_MAX
        {
            // Evict the oldest session rather than grow without bound.
            if let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.updated_at)
                .map(|(k, _)| k.clone())
            {
                self.sessions.remove(&oldest);
            }
        }
        let label = display_label(&report.label, LABEL_MAX_CHARS);
        self.sessions.insert(
            report.session_key,
            SessionActivity {
                label,
                activity,
                updated_at: now,
            },
        );
    }

    /// The display state: the most urgent fresh session wins; ties go to the
    /// most recently updated one.
    pub fn snapshot(&self, now: Instant) -> pb::Snapshot {
        let fresh = self
            .sessions
            .values()
            .filter(|s| now.saturating_duration_since(s.updated_at) < SESSION_STALE_AFTER);
        let count = fresh.clone().count();
        let lead = fresh.max_by_key(|s| (s.activity.priority(), s.updated_at));
        match lead {
            None => pb::Snapshot {
                activity: pb::Activity::Idle as i32,
                session_label: String::new(),
                active_sessions: 0,
                age_seconds: 0,
                detail: "No Claude sessions".to_owned(),
            },
            Some(s) => pb::Snapshot {
                activity: s.activity.to_pb() as i32,
                session_label: s.label.clone(),
                active_sessions: u32::try_from(count).unwrap_or(u32::MAX),
                age_seconds: u32::try_from(now.saturating_duration_since(s.updated_at).as_secs())
                    .unwrap_or(u32::MAX),
                detail: String::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(epoch: u32, payload: Payload) -> Vec<u8> {
        pb::Envelope {
            protocol_major: 1,
            epoch,
            sequence: 7,
            payload: Some(payload),
        }
        .encode_to_vec()
    }

    #[test]
    fn approve_decision_decodes() {
        let bytes = envelope(
            3,
            Payload::Decision(pb::Decision {
                request_id: 9,
                verdict: pb::Verdict::Approve as i32,
            }),
        );
        assert_eq!(
            decode_from_buddy(&bytes, 3),
            Ok(FromBuddy::Decision {
                request_id: 9,
                approved: true
            })
        );
    }

    #[test]
    fn default_or_unknown_verdict_never_approves() {
        for verdict in [0, 99] {
            let bytes = envelope(
                3,
                Payload::Decision(pb::Decision {
                    request_id: 9,
                    verdict,
                }),
            );
            assert_eq!(
                decode_from_buddy(&bytes, 3),
                Err(BuddyError::Invalid("verdict"))
            );
        }
    }

    #[test]
    fn wrong_epoch_version_and_size_are_rejected() {
        let decision = Payload::Decision(pb::Decision {
            request_id: 1,
            verdict: 1,
        });
        assert_eq!(
            decode_from_buddy(&envelope(4, decision.clone()), 3),
            Err(BuddyError::Epoch {
                expected: 3,
                got: 4
            })
        );
        let v2 = pb::Envelope {
            protocol_major: 2,
            epoch: 3,
            sequence: 1,
            payload: Some(decision),
        }
        .encode_to_vec();
        assert_eq!(decode_from_buddy(&v2, 3), Err(BuddyError::Version(2)));
        assert_eq!(
            decode_from_buddy(&[0u8; 513], 3),
            Err(BuddyError::TooLarge(513))
        );
        assert_eq!(
            decode_from_buddy(&[0xff, 0xff], 3),
            Err(BuddyError::Malformed)
        );
    }

    #[test]
    fn buddy_cannot_send_host_messages() {
        let bytes = envelope(3, Payload::ArmCancel(pb::ArmCancel { request_id: 1 }));
        assert_eq!(
            decode_from_buddy(&bytes, 3),
            Err(BuddyError::UnexpectedFromBuddy)
        );
    }

    #[test]
    fn arm_request_is_never_truncated() {
        let mut encoder = BuddyEncoder::new(1);
        let request = pb::ArmRequest {
            request_id: 1,
            app_label: "A very long application name indeed".into(),
            app_path: "/ext/apps/Tools/x.fap".into(),
            client_label: "mcp-1".into(),
            max_actions: 10,
            expires_in_seconds: 30,
        };
        assert_eq!(
            encoder.encode(Payload::ArmRequest(request)),
            Err(BuddyError::Unrepresentable)
        );
    }

    #[test]
    fn largest_valid_envelope_fits_the_limit() {
        let mut encoder = BuddyEncoder::new(u32::MAX);
        let request = pb::ArmRequest {
            request_id: u32::MAX,
            app_label: "x".repeat(LABEL_MAX_CHARS),
            app_path: "p".repeat(APP_PATH_MAX_CHARS),
            client_label: "c".repeat(LABEL_MAX_CHARS),
            max_actions: u32::MAX,
            expires_in_seconds: u32::MAX,
        };
        let (bytes, sequence) = encoder.encode(Payload::ArmRequest(request)).expect("fits");
        assert!(bytes.len() <= BUDDY_MESSAGE_MAX_BYTES);
        assert_eq!(sequence, 1);
    }

    #[test]
    fn board_prefers_attention_and_drops_ended_sessions() {
        let t0 = Instant::now();
        let mut board = ActivityBoard::default();
        let report = |key: &str, activity| ActivityReport {
            session_key: key.into(),
            label: key.into(),
            activity,
        };
        board.apply(report("a", Some(Activity::Working)), t0);
        board.apply(report("b", Some(Activity::NeedsAttention)), t0);
        let snap = board.snapshot(t0);
        assert_eq!(snap.activity, pb::Activity::NeedsAttention as i32);
        assert_eq!(snap.session_label, "b");
        assert_eq!(snap.active_sessions, 2);

        board.apply(report("b", None), t0);
        assert_eq!(board.snapshot(t0).session_label, "a");
    }

    #[test]
    fn board_is_bounded_and_forgets_stale_sessions() {
        let t0 = Instant::now();
        let mut board = ActivityBoard::default();
        for i in 0..(TRACKED_SESSIONS_MAX + 5) {
            board.apply(
                ActivityReport {
                    session_key: i.to_string(),
                    label: "x".into(),
                    activity: Some(Activity::Idle),
                },
                t0 + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(board.sessions.len(), TRACKED_SESSIONS_MAX);
        let later = t0 + SESSION_STALE_AFTER + Duration::from_secs(1);
        assert_eq!(board.snapshot(later).active_sessions, 0);
    }

    #[test]
    fn display_label_strips_control_and_non_ascii() {
        assert_eq!(display_label("a\nb\u{0}cé\u{1b}[2Jd", 24), "abc[2Jd");
        assert_eq!(display_label(&"z".repeat(40), 24).len(), 24);
    }
}
