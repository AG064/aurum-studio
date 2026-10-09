//! Read-only change-set preconditions. This does not publish or validate a revision.
use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

pub const MAX_FILES: usize = 64;
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Action {
    Create,
    Replace,
    Delete,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Change {
    pub path: String,
    pub action: Action,
    pub expected_sha256: String,
    pub text: Option<String>,
}

/// Check all preconditions without creating locks, logs or staging directories.
/// The result remains subject to later edits and must be rechecked at publication.
pub fn check(root: &Path, input: &Value) -> Result<Value, String> {
    let entries = input
        .get("changes")
        .and_then(Value::as_array)
        .ok_or("'changes' must be an array")?;
    if entries.is_empty() || entries.len() > MAX_FILES {
        return Err("'changes' must contain 1..64 entries".into());
    }
    let changes: Vec<Change> =
        serde_json::from_value(Value::Array(entries.clone())).map_err(|error| error.to_string())?;
    let mut normalized = HashSet::new();
    let mut text_bytes = 0usize;
    let mut prepared = Vec::new();
    // Resolve and bound the whole request before reading any source content.
    for change in changes {
        if change.path.is_empty() || change.path.len() > 4096 {
            return Err("change paths must contain 1..4096 UTF-8 bytes".into());
        }
        #[cfg(windows)]
        for part in change
            .path
            .strip_prefix("res://")
            .unwrap_or(&change.path)
            .split(['/', '\\'])
            .filter(|part| !part.is_empty() && *part != ".")
        {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'));
            if part.ends_with(['.', ' ']) || device {
                return Err(
                    "change paths cannot use Windows device names or trailing dots/spaces".into(),
                );
            }
        }
        let path = crate::project_ops::managed_path(root, &change.path)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let key = if cfg!(windows) {
            relative.to_lowercase()
        } else {
            relative.clone()
        };
        if !normalized.insert(key) {
            return Err("duplicate normalized change path".into());
        }
        match change.action {
            Action::Create if !change.expected_sha256.is_empty() => {
                return Err("create requires an empty expected_sha256 (destination absent)".into())
            }
            Action::Replace | Action::Delete
                if change.expected_sha256.len() != 64
                    || !change
                        .expected_sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit()) =>
            {
                return Err("replace/delete require a preceding SHA-256 hash".into())
            }
            _ => {}
        }
        match (&change.action, &change.text) {
            (Action::Delete, Some(_)) => return Err("delete cannot include text".into()),
            (Action::Create | Action::Replace, None) => {
                return Err("create/replace require text".into())
            }
            _ => {}
        }
        if let Some(text) = &change.text {
            if text.len() > MAX_FILE_BYTES {
                return Err("change text exceeds 2 MiB".into());
            }
            text_bytes = text_bytes
                .checked_add(text.len())
                .ok_or("change text size overflow")?;
            if text_bytes > MAX_TEXT_BYTES {
                return Err("change set text exceeds 8 MiB".into());
            }
        }
        prepared.push((change, path, relative));
    }
    let mut checked = Vec::new();
    let mut conflicts = Vec::new();
    for (change, path, relative) in prepared {
        let actual = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
                    return Err(
                        "change targets must be regular UTF-8 files of at most 2 MiB".into(),
                    );
                }
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                File::open(&path)
                    .map_err(|error| error.to_string())?
                    .take(MAX_FILE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() > MAX_FILE_BYTES || std::str::from_utf8(&bytes).is_err() {
                    return Err("change targets must be UTF-8 files of at most 2 MiB".into());
                }
                Some(crate::sha256_hex(&bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let reason = match &change.action {
            Action::Create if actual.is_some() => Some("destination_exists"),
            Action::Replace | Action::Delete if actual.is_none() => Some("source_missing"),
            Action::Replace | Action::Delete
                if actual
                    .as_deref()
                    .is_some_and(|hash| !hash.eq_ignore_ascii_case(&change.expected_sha256)) =>
            {
                Some("stale_hash")
            }
            _ => None,
        };
        if let Some(reason) = reason {
            conflicts.push(json!({"path":relative,"reason":reason,"actual_sha256":actual}));
        }
        let action = match change.action {
            Action::Create => "create",
            Action::Replace => "replace",
            Action::Delete => "delete",
        };
        checked.push(
            json!({"path":relative,"action":action,"actual_sha256":actual,
            "proposed_sha256":change.text.as_ref().map(|text|crate::sha256_hex(text.as_bytes()))}),
        );
    }
    Ok(
        json!({"ready":conflicts.is_empty(),"applied":false,"validated":false,
        "changes":checked,"conflicts":conflicts,"text_bytes":text_bytes,
        "limits":{"files":MAX_FILES,"file_bytes":MAX_FILE_BYTES,"text_bytes":MAX_TEXT_BYTES},
        "scope":"preconditions only; recheck before publication; no Godot/native validation or filesystem isolation"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "aurum-revision-check-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(crate::project::clean_path(root.canonicalize().unwrap()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn create(path: &str) -> Value {
        json!({"path":path,"action":"create","expected_sha256":"","text":"new"})
    }

    #[test]
    fn linked_creates_are_checked_without_creating_any_state() {
        let root = Fixture::new();
        let result = check(
            &root.0,
            &json!({"changes":[create("scenes/main.tscn"),create("scripts/main.gd")]}),
        )
        .unwrap();
        assert_eq!(result["ready"], true);
        assert_eq!(result["applied"], false);
        assert_eq!(result["validated"], false);
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[test]
    fn all_conflicts_are_reported_and_originals_preserved() {
        let root = Fixture::new();
        std::fs::write(root.0.join("existing.gd"), "original").unwrap();
        let result = check(&root.0, &json!({"changes":[create("existing.gd"),
            {"path":"missing.gd","action":"delete","expected_sha256":"0".repeat(64)},
            {"path":"existing.gd2","action":"replace","expected_sha256":"0".repeat(64),"text":"new"}]})).unwrap();
        assert_eq!(result["ready"], false);
        assert_eq!(result["conflicts"].as_array().unwrap().len(), 3);
        let stale = check(&root.0, &json!({"changes":[{"path":"existing.gd","action":"replace","expected_sha256":"0".repeat(64),"text":"new"}]})).unwrap();
        assert_eq!(stale["conflicts"][0]["reason"], "stale_hash");
        assert_eq!(
            std::fs::read_to_string(root.0.join("existing.gd")).unwrap(),
            "original"
        );
        assert!(!root.0.join(".aurum").exists());
    }

    #[test]
    fn exact_hashes_accept_replace_and_delete_without_applying_them() {
        let root = Fixture::new();
        std::fs::write(root.0.join("replace.gd"), "original").unwrap();
        std::fs::write(root.0.join("delete.gd"), "original").unwrap();
        let hash = crate::sha256_hex(b"original");
        let result = check(&root.0, &json!({"changes":[{"path":"replace.gd","action":"replace","expected_sha256":hash.to_uppercase(),"text":"new"},
            {"path":"delete.gd","action":"delete","expected_sha256":hash}]})).unwrap();
        assert_eq!(result["ready"], true);
        assert_eq!(
            std::fs::read_to_string(root.0.join("replace.gd")).unwrap(),
            "original"
        );
        assert!(root.0.join("delete.gd").exists());
    }

    #[test]
    fn invalid_paths_duplicates_and_secret_state_are_rejected() {
        let root = Fixture::new();
        for path in [
            "../escape",
            ".aurum/state",
            ".git/config",
            ".env",
            "file.gd:stream",
            "",
        ] {
            assert!(check(&root.0, &json!({"changes":[create(path)]})).is_err());
        }
        assert!(check(
            &root.0,
            &json!({"changes":[create("same.gd"),create("./same.gd")]})
        )
        .is_err());
        assert!(check(
            &root.0,
            &json!({"changes":[create("same.gd"),create("res://same.gd")]})
        )
        .is_err());
        #[cfg(windows)]
        assert!(check(
            &root.0,
            &json!({"changes":[create("same.gd"),create("SAME.gd")]})
        )
        .is_err());
        #[cfg(windows)]
        for path in ["file.gd.", "file.gd ", "NUL.gd", "COM1"] {
            assert!(check(&root.0, &json!({"changes":[create(path)]})).is_err());
        }
        assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[test]
    fn schema_and_size_bounds_are_enforced_before_reading_content() {
        let root = Fixture::new();
        for changes in [
            json!([]),
            json!([{"path":"a","action":"create","expected_sha256":"not-empty","text":"x"}]),
            json!([{"path":"a","action":"replace","expected_sha256":"","text":"x"}]),
            json!([{"path":"a","action":"delete","expected_sha256":"0".repeat(64),"text":"x"}]),
            json!([{"path":"a","action":"create","expected_sha256":"","extra":true}]),
            json!([{"path":"a","action":"create","expected_sha256":"","text":"x".repeat(MAX_FILE_BYTES+1)}]),
        ] {
            assert!(check(&root.0, &json!({"changes":changes})).is_err());
        }
        assert!(check(&root.0, &json!({"changes":(0..65).map(|index|create(&format!("{index}.gd"))).collect::<Vec<_>>()})).is_err());
        assert!(check(&root.0, &json!({"changes":(0..5).map(|index|json!({"path":format!("{index}.gd"),"action":"create","expected_sha256":"","text":"x".repeat(MAX_FILE_BYTES)})).collect::<Vec<_>>()})).is_err());
    }
}
