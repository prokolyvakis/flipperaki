//! Claude Code hook payloads reduced to minimal activity reports.
//!
//! Hook input can contain prompts, tool arguments and transcript paths. Only
//! the allowlisted fields below are ever deserialized; everything else is
//! ignored before it can be logged or forwarded.

use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::buddy::{Activity, ActivityReport, LABEL_MAX_CHARS, display_label};
use crate::limits::HOOK_INPUT_MAX_BYTES;

/// Longest session id accepted from a hook (Claude uses UUIDs).
const SESSION_ID_MAX_CHARS: usize = 128;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HookError {
    #[error("hook payload exceeds {HOOK_INPUT_MAX_BYTES} bytes")]
    TooLarge,
    #[error("hook payload is not a supported Claude Code hook object")]
    Malformed,
    #[error("hook event is not used by Flipperaki")]
    Ignored,
}

/// The only hook fields Flipperaki reads.
#[derive(Debug, Deserialize)]
struct AllowlistedHook {
    session_id: String,
    hook_event_name: String,
    #[serde(default)]
    cwd: Option<String>,
}

/// Parses one hook payload into an activity report.
pub fn parse_hook(input: &[u8]) -> Result<ActivityReport, HookError> {
    if input.len() > HOOK_INPUT_MAX_BYTES {
        return Err(HookError::TooLarge);
    }
    let hook: AllowlistedHook = serde_json::from_slice(input).map_err(|_| HookError::Malformed)?;
    if hook.session_id.is_empty() || hook.session_id.len() > SESSION_ID_MAX_CHARS {
        return Err(HookError::Malformed);
    }
    let activity = match hook.hook_event_name.as_str() {
        "SessionStart" => Some(Activity::Idle),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PreCompact" => Some(Activity::Working),
        "Notification" => Some(Activity::NeedsAttention),
        // A finished turn is not proof the user's task succeeded.
        "Stop" => Some(Activity::TurnFinished),
        "SessionEnd" => None,
        _ => return Err(HookError::Ignored),
    };
    // Only the project directory's final component is shown, never the path.
    let label = hook
        .cwd
        .as_deref()
        .and_then(|cwd| Path::new(cwd).file_name())
        .map(|name| display_label(&name.to_string_lossy(), LABEL_MAX_CHARS))
        .unwrap_or_default();
    Ok(ActivityReport {
        session_key: hook.session_id,
        label,
        activity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_events_and_keeps_only_the_directory_name() {
        let input = br#"{"session_id":"s1","hook_event_name":"UserPromptSubmit",
            "cwd":"/Users/me/secret/flipperaki","prompt":"top secret prompt",
            "transcript_path":"/Users/me/.claude/t.jsonl"}"#;
        let report = parse_hook(input).expect("valid hook");
        assert_eq!(report.activity, Some(Activity::Working));
        assert_eq!(report.label, "flipperaki");
        let debug = format!("{report:?}");
        assert!(!debug.contains("secret prompt") && !debug.contains("/Users"));
    }

    #[test]
    fn stop_is_turn_finished_and_session_end_clears() {
        let stop = br#"{"session_id":"s","hook_event_name":"Stop"}"#;
        assert_eq!(
            parse_hook(stop).unwrap().activity,
            Some(Activity::TurnFinished)
        );
        let end = br#"{"session_id":"s","hook_event_name":"SessionEnd"}"#;
        assert_eq!(parse_hook(end).unwrap().activity, None);
    }

    #[test]
    fn rejects_oversized_malformed_and_unknown_input() {
        assert_eq!(
            parse_hook(&vec![b' '; HOOK_INPUT_MAX_BYTES + 1]),
            Err(HookError::TooLarge)
        );
        assert_eq!(parse_hook(b"not json"), Err(HookError::Malformed));
        assert_eq!(
            parse_hook(br#"{"session_id":"","hook_event_name":"Stop"}"#),
            Err(HookError::Malformed)
        );
        assert_eq!(
            parse_hook(br#"{"session_id":"s","hook_event_name":"SomethingNew"}"#),
            Err(HookError::Ignored)
        );
    }
}
