//! Disposable candidate construction and Godot validation, never publication.
use crate::{files, snapshot, Project};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RECEIPT_BYTES: usize = 64 * 1024;

struct Workspace(PathBuf);
impl Workspace {
    fn create() -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("aurum-candidate-{}", crate::random::session_id()));
        crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        Ok(Self(crate::project::clean_path(
            path.canonicalize().map_err(|error| error.to_string())?,
        )))
    }
    fn clean(&self) -> Result<(), String> {
        crate::diagnostics::checked_path(&self.0).map_err(|error| error.to_string())?;
        let resolved =
            crate::project::clean_path(self.0.canonicalize().map_err(|error| error.to_string())?);
        if resolved != self.0
            || !self
                .0
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("aurum-candidate-"))
        {
            return Err("candidate cleanup refused an unexpected path".into());
        }
        // std removes directory links themselves, not their targets. The root
        // itself must still be the exact directory this operation created.
        std::fs::remove_dir_all(&self.0).map_err(|error| error.to_string())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if self.0.exists() && self.clean().is_err() {
            eprintln!(
                "aurum candidates: private workspace cleanup failed; no source content was removed"
            );
        }
    }
}

fn script_project(project: &Project, inventory: &[(String, u64)]) -> Result<(), String> {
    if project.config.rust_package.is_some()
        || project.config.engine_path_hint.is_some()
        || project.config.validation_command.is_some()
        || inventory
            .iter()
            .any(|(path, _)| path.to_ascii_lowercase().ends_with(".gdextension"))
    {
        return Err("candidate validation currently supports self-contained script projects; native/external dependencies and custom validators need explicit staging support".into());
    }
    if project.godot_project_dir().is_none() {
        return Err("candidate validation requires project.godot".into());
    }
    Ok(())
}

fn text_summary(value: &Value) -> Value {
    let lines = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(8)
            .map(receipt_line)
            .collect()
    };
    let resources = value.get("resource_validation");
    json!({"ok":value.get("ok").and_then(Value::as_bool).unwrap_or(false),
        "exit_code":value.get("exit_code"),"timed_out":value.get("timed_out"),
        "errors":lines("errors"),"log":lines("log"),
        "resources_ok":resources.and_then(|value|value.get("ok")),
        "resources_checked":resources.and_then(|value|value.get("checked_resources")).and_then(Value::as_u64),
        "resources_error":resources.and_then(|value|value.get("error")).and_then(Value::as_str).map(receipt_line),
        "first_import_shutdown_retry":value.get("first_import_shutdown_retry").and_then(Value::as_bool).unwrap_or(false)})
}

fn receipt_line(text: &str) -> String {
    let safe = crate::diagnostics::text_line(text, &[]);
    if safe.len() <= 1024 {
        return safe;
    }
    let mut end = 1008;
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [truncated]", &safe[..end])
}

fn receipt_path(project: &Project) -> Result<PathBuf, String> {
    let path = files::confined(&project.root, ".aurum/checks/candidate-latest.json")?;
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    Ok(path)
}

/// Last completed receipt only, not a fresh validation or source-state assertion.
pub fn last(project: &Project) -> Result<Value, String> {
    let path = receipt_path(project)?;
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"present":false}))
        }
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() || metadata.len() > MAX_RECEIPT_BYTES as u64 => {
            return Err("candidate receipt exceeds supported bounds".into())
        }
        Ok(_) => {}
    }
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err("candidate receipt exceeds supported bounds".into());
    }
    let receipt: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    Ok(
        json!({"present":true,"receipt":receipt,"freshness":"stored evidence only; source state is not rechecked"}),
    )
}

pub fn validate(project: &Project, input: &Value) -> Result<Value, String> {
    if !cfg!(windows) {
        return Err("candidate runtime validation is currently Windows-qualified; other platforms need private-userdata acceptance".into());
    }
    validate_with(project, input, |candidate, userdata| {
        let engine = crate::project_ops::engine_binary(project)?
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|error| error.to_string())?;
        let context = crate::project_ops::RuntimeContext {
            engine,
            userdata: userdata.to_path_buf(),
        };
        crate::project_ops::validate_with_context(candidate, Some(&context))
    })
}

fn validate_with(
    project: &Project,
    input: &Value,
    validator: impl FnOnce(&Project, &Path) -> Result<Value, String>,
) -> Result<Value, String> {
    let started = Instant::now();
    let evidence = receipt_path(project)?;
    let mut workspace = None;
    let prepared = (|| -> Result<_, String> {
        let _content = crate::content_lock::ContentLock::shared(&project.root)?;
        // Reopen under the snapshot lease instead of using a stale configuration.
        let source = Project::open(&project.root).map_err(|error| error.to_string())?;
        let checked = crate::revisions::check(&source.root, input)?;
        if checked["ready"] != true {
            return Ok(Err(checked));
        }
        let entries = snapshot::inventory(&source.root)?;
        script_project(&source, &entries)?;
        let bytes: u64 = entries.iter().map(|(_, bytes)| bytes).sum();
        if bytes.saturating_add(checked["text_bytes"].as_u64().unwrap_or(0)) > MAX_SOURCE_BYTES {
            return Err("candidate source exceeds the 512 MiB snapshot budget".into());
        }
        for change in checked["changes"]
            .as_array()
            .ok_or("missing checked changes")?
        {
            if !snapshot::includes(change["path"].as_str().ok_or("missing checked path")?) {
                return Err("candidate changes cannot target excluded source/state paths".into());
            }
        }
        let source_sha = snapshot::fingerprint_all(&source.root)?;
        workspace = Some(Workspace::create()?);
        let private = workspace.as_ref().ok_or("missing workspace")?;
        let directory = private.0.join("source");
        snapshot::copy(&source.root, &directory)?;
        if snapshot::fingerprint_all(&directory)? != source_sha
            || snapshot::fingerprint_all(&source.root)? != source_sha
        {
            return Err(
                "source changed while copying the candidate; retry with fresh hashes".into(),
            );
        }
        let copied = crate::revisions::check(&directory, input)?;
        if copied["ready"] != true {
            return Err("copied candidate does not match the checked preconditions".into());
        }
        let changes: Vec<crate::revisions::Change> =
            serde_json::from_value(input["changes"].clone()).map_err(|error| error.to_string())?;
        for change in changes {
            let path = crate::project_ops::managed_path(&directory, &change.path)?;
            match change.action {
                crate::revisions::Action::Create | crate::revisions::Action::Replace => {
                    files::write_atomic(
                        &path,
                        change
                            .text
                            .as_deref()
                            .ok_or("missing change text")?
                            .as_bytes(),
                    )
                    .map_err(|error| error.to_string())?;
                }
                crate::revisions::Action::Delete => {
                    std::fs::remove_file(&path).map_err(|error| error.to_string())?
                }
            }
        }
        let candidate = Project::open(&directory).map_err(|error| error.to_string())?;
        script_project(&candidate, &snapshot::inventory(&directory)?)?;
        let prepared_sha = snapshot::fingerprint_all(&directory)?;
        Ok(Ok((candidate, source_sha, prepared_sha, checked, bytes)))
    })()?;
    let (candidate, source_sha, prepared_sha, checked, bytes) = match prepared {
        Ok(prepared) => prepared,
        Err(conflicts) => {
            return Ok(
                json!({"ok":false,"applied":false,"validated":false,"phase":"preflight","conflicts":conflicts["conflicts"]}),
            )
        }
    };
    let private = workspace.as_ref().ok_or("missing candidate workspace")?;
    let userdata = private.0.join("userdata");
    std::fs::create_dir(&userdata).map_err(|error| error.to_string())?;
    // The original content lease ended before invoking any compiler/runtime.
    let result = validator(&candidate, &userdata);
    let validation = match result {
        Ok(result) => text_summary(&result),
        Err(error) => json!({"ok":false,"error":receipt_line(&error)}),
    };
    let source_current =
        snapshot::fingerprint_all(&project.root).is_ok_and(|hash| hash == source_sha);
    let candidate_sha = snapshot::fingerprint_all(&candidate.root)?;
    let mut intact = true;
    for change in checked["changes"]
        .as_array()
        .ok_or("missing checked changes")?
    {
        let path = crate::project_ops::managed_path(
            &candidate.root,
            change["path"].as_str().ok_or("missing checked path")?,
        )?;
        let expected = change["proposed_sha256"].as_str();
        let actual = if path.exists() {
            Some(crate::sha256_file(&path).map_err(|error| error.to_string())?)
        } else {
            None
        };
        intact &= actual.as_deref() == expected;
    }
    let validated = validation["ok"] == true;
    let mut receipt = json!({"version":1,"check_id":crate::random::session_id(),
        "timestamp_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "ok":validated && source_current && intact,"validated":validated,"applied":false,"gameplay_tested":false,
        "source_still_current":source_current,"requested_changes_intact":intact,
        "source_sha256":source_sha,"prepared_sha256":prepared_sha,"candidate_sha256":candidate_sha,
        "validation_changed_snapshot":prepared_sha != candidate_sha,"validation":validation,
        "files_changed":checked["changes"].as_array().map(Vec::len),"source_bytes":bytes,
        "duration_ms":started.elapsed().as_millis() as u64,"candidate_retained":false,
        "userdata_redirected":true,"execution_sandboxed":false,
        "scope":"script/resource validation of a disposable copy; no publication, native build or gameplay proof"});
    match private.clean() {
        Ok(()) => receipt["cleanup_ok"] = json!(true),
        Err(error) => {
            receipt["cleanup_ok"] = json!(false);
            receipt["candidate_retained"] = json!(true);
            receipt["ok"] = json!(false);
            receipt["cleanup_error"] = json!(crate::diagnostics::text_line(&error, &[]));
            receipt["cleanup_path"] = json!(private.0);
        }
    }
    receipt["evidence"] = json!(evidence);
    receipt["evidence_persisted"] = json!(true);
    let encoded = serde_json::to_vec(&receipt).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_RECEIPT_BYTES {
        return Err("candidate receipt exceeded its bounded schema".into());
    }
    match files::write_atomic(&evidence, &encoded) {
        Ok(()) => receipt["evidence_persisted"] = json!(true),
        Err(_) => receipt["evidence_persisted"] = json!(false),
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(Project);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "aurum-candidate-test-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("aurum.toml"),
                "schema_version=1\nname=\"candidate-test\"\nmodules=[]\n",
            )
            .unwrap();
            std::fs::write(
                root.join("project.godot"),
                "[application]\nconfig/name=\"Candidate fixture\"\n",
            )
            .unwrap();
            std::fs::write(root.join("main.gd"), "extends Node\n").unwrap();
            Self(Project::open(&root).unwrap())
        }
        fn changes(&self) -> Value {
            json!({"changes":[{"path":"main.gd","action":"replace","expected_sha256":crate::sha256_hex(b"extends Node\n"),"text":"extends Node\nvar changed = 1\n"}]})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.root);
        }
    }

    #[test]
    fn candidate_changes_are_private_and_workspace_is_removed() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join(".env"), "private").unwrap();
        let before = snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let mut private_root = None;
        let result = validate_with(&fixture.0, &fixture.changes(), |candidate, userdata| {
            private_root = Some(candidate.root.clone());
            assert!(userdata.starts_with(candidate.root.parent().unwrap()));
            assert!(!candidate.root.join(".env").exists());
            assert!(std::fs::read_to_string(candidate.root.join("main.gd"))
                .unwrap()
                .contains("changed"));
            assert!(!std::fs::read_to_string(fixture.0.root.join("main.gd"))
                .unwrap()
                .contains("changed"));
            assert!(crate::content_lock::ContentLock::exclusive(&fixture.0.root).is_ok());
            Ok(json!({"ok":true}))
        })
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["applied"], false);
        assert_eq!(snapshot::fingerprint_all(&fixture.0.root).unwrap(), before);
        assert!(!private_root.unwrap().exists());
        assert_eq!(
            last(&fixture.0).unwrap()["receipt"]["check_id"],
            result["check_id"]
        );
    }

    #[test]
    fn failed_validator_and_conflicts_never_publish() {
        let fixture = Fixture::new();
        let before = snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let result = validate_with(&fixture.0, &fixture.changes(), |_, _| {
            Err("parser failed".into())
        })
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["cleanup_ok"], true);
        let mut request = fixture.changes();
        request["changes"][0]["expected_sha256"] = json!("0".repeat(64));
        let conflicts = validate_with(&fixture.0, &request, |_, _| {
            panic!("conflict must not run a validator")
        })
        .unwrap();
        assert_eq!(conflicts["phase"], "preflight");
        assert_eq!(snapshot::fingerprint_all(&fixture.0.root).unwrap(), before);
    }

    #[test]
    fn external_edits_during_validation_make_the_receipt_stale() {
        let fixture = Fixture::new();
        let result = validate_with(&fixture.0, &fixture.changes(), |_, _| {
            std::fs::write(fixture.0.root.join("other.gd"), "external edit").unwrap();
            Ok(json!({"ok":true}))
        })
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["source_still_current"], false);
        assert_eq!(
            std::fs::read_to_string(fixture.0.root.join("other.gd")).unwrap(),
            "external edit"
        );
    }

    #[test]
    fn a_validator_cannot_silently_replace_requested_content() {
        let fixture = Fixture::new();
        let result = validate_with(&fixture.0, &fixture.changes(), |candidate, _| {
            std::fs::write(candidate.root.join("main.gd"), "different content").unwrap();
            Ok(json!({"ok":true}))
        })
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["requested_changes_intact"], false);
    }

    #[test]
    fn create_replace_delete_are_applied_only_to_the_private_copy() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("delete.gd"), "old").unwrap();
        let mut request = fixture.changes();
        request["changes"].as_array_mut().unwrap().extend([
            json!({"path":"new.gd","action":"create","expected_sha256":"","text":"extends Node\n"}),
            json!({"path":"delete.gd","action":"delete","expected_sha256":crate::sha256_hex(b"old")}),
        ]);
        let result = validate_with(&fixture.0, &request, |candidate, _| {
            assert!(!candidate.root.join("delete.gd").exists());
            assert!(candidate.root.join("new.gd").exists());
            Ok(json!({"ok":true}))
        })
        .unwrap();
        assert_eq!(result["ok"], true);
        assert!(fixture.0.root.join("delete.gd").exists());
        assert!(!fixture.0.root.join("new.gd").exists());
    }

    #[test]
    fn deleting_a_private_directory_never_follows_a_nested_link() {
        let workspace = Workspace::create().unwrap();
        let outside =
            std::env::temp_dir().join(format!("aurum-outside-{}", crate::random::session_id()));
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), "untouched").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, workspace.0.join("link")).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let status = std::process::Command::new("cmd")
                .creation_flags(0x08000000)
                .args(["/C", "mklink", "/J"])
                .arg(workspace.0.join("link"))
                .arg(&outside)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        }
        workspace.clean().unwrap();
        assert_eq!(
            std::fs::read_to_string(outside.join("keep")).unwrap(),
            "untouched"
        );
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn receipts_stay_bounded_when_tools_emit_long_errors() {
        let fixture = Fixture::new();
        let long = "long error ".repeat(2000);
        let result = validate_with(&fixture.0, &fixture.changes(), |_, _| {
            Ok(json!({"ok":false,"errors":vec![long.clone();100],"log":vec![long;100]}))
        })
        .unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() < MAX_RECEIPT_BYTES);
        assert_eq!(result["ok"], false);
        assert_eq!(last(&fixture.0).unwrap()["present"], true);
    }

    #[test]
    fn unsupported_dependencies_and_excluded_changes_are_refused() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("native.gdextension"), "native").unwrap();
        assert!(validate_with(&fixture.0, &fixture.changes(), |_, _| panic!(
            "unsupported dependencies"
        ))
        .is_err());
        std::fs::remove_file(fixture.0.root.join("native.gdextension")).unwrap();
        let request = json!({"changes":[{"path":".gitignore","action":"create","expected_sha256":"","text":"dist"}]});
        assert!(validate_with(&fixture.0, &request, |_, _| panic!("excluded changes")).is_err());
    }
}
