//! Reversible installation of the Flipperaki entries in Claude Code settings.
//!
//! Only Flipperaki's own hook entries are added or removed; every other key
//! is preserved, and the original file is copied aside before the first edit.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Hook events Flipperaki listens to (see `hooks::parse_hook`).
pub const HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PreCompact",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// Marks entries this tool owns, so removal never touches anything else.
const MARKER: &str = "flipperaki' hook";

fn is_ours(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks
            .iter()
            .any(|h| h["command"].as_str().is_some_and(|c| c.ends_with(MARKER)))
    })
}

/// Returns `settings` with Flipperaki hook entries added (idempotent).
pub fn with_hooks(mut settings: Value, executable: &Path) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    let command = format!(
        "'{}' hook",
        executable.display().to_string().replace('\'', r"'\''")
    );
    let hooks = settings
        .as_object_mut()
        .expect("object ensured above")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    for event in HOOK_EVENTS {
        let groups = hooks
            .as_object_mut()
            .expect("object ensured above")
            .entry(event)
            .or_insert_with(|| json!([]));
        if !groups.is_array() {
            *groups = json!([]);
        }
        let groups = groups.as_array_mut().expect("array ensured above");
        groups.retain(|g| !is_ours(g));
        // A short timeout: a missing Flipper must never slow Claude down.
        groups.push(json!({ "hooks": [{ "type": "command", "command": command, "timeout": 2 }] }));
    }
    settings
}

/// Returns `settings` with only Flipperaki hook entries removed.
pub fn without_hooks(mut settings: Value) -> Value {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return settings;
    };
    for groups in hooks.values_mut() {
        if let Some(groups) = groups.as_array_mut() {
            groups.retain(|g| !is_ours(g));
        }
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    settings
}

pub fn default_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude/settings.json"))
}

/// Applies `edit` to the settings file, keeping `<file>.flipperaki-backup`
/// from before the first Flipperaki edit.
pub fn edit_settings_file(path: &Path, edit: impl FnOnce(Value) -> Value) -> anyhow::Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("{} is not valid JSON ({e}); not modified", path.display())
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e.into()),
    };
    let backup = path.with_extension("json.flipperaki-backup");
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup)?;
    }
    let updated = edit(current);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The replacement keeps the original's mode (0600 for a new file) and is
    // created exclusively, so the rename never widens access to the settings.
    let mode = std::fs::metadata(path).map_or(0o600, |m| m.permissions().mode() & 0o777);
    let temp = path.with_extension("json.flipperaki-tmp");
    let _ = std::fs::remove_file(&temp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)?;
    file.write_all((serde_json::to_string_pretty(&updated)? + "\n").as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_is_idempotent_and_removal_restores_other_entries() {
        let original = json!({
            "model": "opus",
            "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ] }
        });
        let exe = Path::new("/Applications/Flipperaki.app/Contents/MacOS/flipperaki");
        let once = with_hooks(original.clone(), exe);
        let twice = with_hooks(once.clone(), exe);
        assert_eq!(once, twice);
        assert_eq!(once["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(once["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(without_hooks(twice), original);
    }

    #[test]
    fn executable_paths_with_quotes_are_shell_safe() {
        let settings = with_hooks(json!({}), Path::new("/tmp/it's here/flipperaki"));
        let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(command, r"'/tmp/it'\''s here/flipperaki' hook");
    }

    #[test]
    fn invalid_settings_file_is_left_untouched() {
        let dir = std::env::temp_dir().join(format!("flipperaki-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(edit_settings_file(&path, |v| v).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");

        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        edit_settings_file(&path, |v| with_hooks(v, Path::new("/x/flipperaki"))).unwrap();
        let backup =
            std::fs::read_to_string(path.with_extension("json.flipperaki-backup")).unwrap();
        assert_eq!(backup, r#"{"a":1}"#);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rewrite must not widen permissions");
        edit_settings_file(&path, without_hooks).unwrap();
        let restored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored, json!({"a":1}));
        std::fs::remove_dir_all(&dir).ok();
    }
}
