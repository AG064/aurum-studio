//! Disposable candidate validation and guarded source publication.
use crate::candidate_workspace::Workspace;
use crate::{files, snapshot, Project};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RECEIPT_BYTES: usize = 64 * 1024;

fn operation_phase(phase: crate::operation_context::Phase) {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.phase(phase);
    }
}
fn operation_check() -> Result<(), String> {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.check().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn self_contained_project(project: &Project, inventory: &[(String, u64)]) -> Result<(), String> {
    if project.config.validation_command.is_some() {
        return Err("candidate validation requires copied source; custom validators need explicit staging support".into());
    }
    project.confined_build_workspace()?;
    if project.godot_project_dir().is_none() {
        return Err("candidate validation requires project.godot".into());
    }
    if !cfg!(windows)
        && (project.config.rust_package.is_some()
            || inventory
                .iter()
                .any(|(path, _)| path.to_ascii_lowercase().ends_with(".gdextension")))
    {
        return Err(
            "native candidate dependencies require Windows-qualified private staging".into(),
        );
    }
    Ok(())
}

#[cfg(windows)]
fn verify_native_dependencies(
    candidate: &Project,
    context: &crate::project_ops::RuntimeContext,
    expected: &Value,
) -> Result<Value, String> {
    use std::io::Read;
    use std::time::Duration;

    let workspace = context
        .userdata
        .parent()
        .ok_or("candidate workspace is unavailable")?;
    let directory = workspace.join("dependency-verification");
    // A previous or candidate-created result must never qualify this inspection.
    std::fs::create_dir(&directory).map_err(|error| error.to_string())?;
    let script = directory.join("verify.gd");
    let helper = directory.join("plan.gd");
    let input = directory.join("input.json");
    let output = directory.join("output.json");
    let clean = |path: &Path| path.to_string_lossy().replace('\\', "/");
    let godot = candidate
        .godot_project_dir()
        .ok_or("native dependencies require project.godot")?;
    let manifests = expected["manifests"]
        .as_array()
        .filter(|rows| rows.len() <= 16)
        .ok_or("invalid prepared native manifests")?;
    let script_text = b"extends SceneTree\nfunc _init():\n\tvar args = OS.get_cmdline_user_args()\n\tif args.size() != 3:\n\t\tquit(1)\n\t\treturn\n\tvar input = JSON.parse_string(FileAccess.get_file_as_string(args[0]))\n\tvar helper = load(args[2]).new()\n\tvar result = helper.plan(input.project, input.godot, input.manifests, true)\n\tvar file = FileAccess.open(args[1], FileAccess.WRITE)\n\tif file == null:\n\t\tquit(1)\n\t\treturn\n\tfile.store_string(JSON.stringify(result))\n\tfile.close()\n\tquit(0 if result.get(\"ok\", false) else 1)\n";
    files::write_atomic(&script, script_text).map_err(|error| error.to_string())?;
    files::write_atomic(&helper, include_bytes!("native_dependency_plan.gd"))
        .map_err(|error| error.to_string())?;
    files::write_atomic(
        &input,
        &serde_json::to_vec(
            &json!({"project":clean(&candidate.root),"godot":clean(godot),
            "manifests":manifests.iter().map(|row|row["manifest"].clone()).collect::<Vec<_>>() }),
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let mut command = crate::Command::new(&context.engine)
        .args(["--headless", "--path"])
        .arg(clean(godot))
        .arg("--script")
        .arg(clean(&script))
        .arg("--")
        .arg(clean(&input))
        .arg(clean(&output))
        .arg(clean(&helper))
        .candidate_storage(workspace);
    for key in [
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
    ] {
        command = command.env(key, clean(&context.userdata));
    }
    let result = crate::native_runtime::run(&command, Duration::from_secs(30))
        .map_err(|error| error.to_string())?;
    if !result.success() || result.stderr.contains("ERROR:") {
        return Err("private native dependency verification failed".into());
    }
    crate::diagnostics::checked_path(&output).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    std::fs::File::open(&output)
        .map_err(|error| error.to_string())?
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 256 * 1024 {
        return Err("native dependency verification exceeds 256 KiB".into());
    }
    let actual: Value =
        serde_json::from_slice(&bytes).map_err(|_| "invalid native dependency verification")?;
    crate::native_dependencies::verify(expected, &actual)?;
    Ok(json!({"ok":true,"manifests":manifests.len(),
        "libraries":manifests.iter().map(|row|json!({
            "path":Path::new(row["library"].as_str().unwrap_or_default())
                .strip_prefix(&candidate.root).ok().map(|path|receipt_line(&clean(path))),
            "sha256":row["sha256"],"loaded":true,
            "dependencies":row["dependency_hashes"].as_object().map(serde_json::Map::len)
        })).collect::<Vec<_>>() }))
}

struct Validation {
    outcome: Value,
    artifacts: Vec<crate::transactions::GeneratedArtifact>,
}
impl From<Value> for Validation {
    fn from(outcome: Value) -> Self {
        Self {
            outcome,
            artifacts: Vec::new(),
        }
    }
}
fn runtime_validate(
    original: &Project,
    candidate: &Project,
    userdata: &Path,
) -> Result<Validation, String> {
    operation_phase(crate::operation_context::Phase::Dependencies);
    operation_check()?;
    self_contained_project(candidate, &snapshot::inventory(&candidate.root)?)?;
    let engine = crate::project_ops::engine_binary(original)?
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())?;
    let context = crate::project_ops::RuntimeContext {
        engine,
        userdata: userdata.to_path_buf(),
    };
    #[cfg(windows)]
    let mut artifacts = Vec::new();
    #[cfg(not(windows))]
    let artifacts: Vec<crate::transactions::GeneratedArtifact> = Vec::new();
    #[cfg(windows)]
    let dependencies = crate::native_dependencies::prepare(
        candidate,
        userdata
            .parent()
            .ok_or("candidate workspace is unavailable")?,
        &context.engine,
        userdata,
    )?;
    #[cfg(not(windows))]
    let dependencies = json!({"native":false});
    #[cfg(windows)]
    if let Some(compiled) = dependencies
        .get("compiled")
        .filter(|value| value.is_object())
    {
        let library = Path::new(
            compiled["library"]
                .as_str()
                .ok_or("compiled native library is unavailable")?,
        );
        let path = library
            .strip_prefix(&candidate.root)
            .map_err(|_| "compiled library escaped candidate")?
            .to_str()
            .ok_or("compiled library path must be UTF-8")?
            .replace('\\', "/");
        artifacts.push(crate::transactions::GeneratedArtifact {
            path,
            sha256: compiled["sha256"]
                .as_str()
                .ok_or("compiled library hash is unavailable")?
                .into(),
        });
    }
    operation_phase(crate::operation_context::Phase::Importing);
    operation_check()?;
    let mut outcome = crate::project_ops::validate_with_context(candidate, Some(&context))?;
    operation_phase(crate::operation_context::Phase::Validating);
    operation_check()?;
    #[cfg(windows)]
    if outcome["ok"] == true && dependencies["native"] == true {
        outcome["native_dependencies"] =
            verify_native_dependencies(candidate, &context, &dependencies)?;
    }
    outcome["native_build"] = json!(dependencies["compiled"].is_object());
    outcome["native_artifacts"] = json!(artifacts
        .iter()
        .map(|artifact| json!({"path":artifact.path,"sha256":artifact.sha256}))
        .collect::<Vec<_>>());
    Ok(Validation { outcome, artifacts })
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
    run_with_validation(
        project,
        input,
        |candidate, userdata| runtime_validate(project, candidate, userdata),
        false,
    )
}

pub fn apply(project: &Project, input: &Value) -> Result<Value, String> {
    if !cfg!(windows) {
        return Err("candidate publication is currently Windows-qualified".into());
    }
    run_with_validation(
        project,
        input,
        |candidate, userdata| runtime_validate(project, candidate, userdata),
        true,
    )
}

#[cfg(test)]
fn validate_with(
    project: &Project,
    input: &Value,
    validator: impl FnOnce(&Project, &Path) -> Result<Value, String>,
) -> Result<Value, String> {
    run_with(project, input, validator, false)
}

#[cfg(test)]
fn run_with(
    project: &Project,
    input: &Value,
    validator: impl FnOnce(&Project, &Path) -> Result<Value, String>,
    publish: bool,
) -> Result<Value, String> {
    run_with_validation(
        project,
        input,
        |candidate, userdata| validator(candidate, userdata).map(Validation::from),
        publish,
    )
}
fn run_with_validation(
    project: &Project,
    input: &Value,
    validator: impl FnOnce(&Project, &Path) -> Result<Validation, String>,
    publish: bool,
) -> Result<Value, String> {
    let started = Instant::now();
    operation_phase(crate::operation_context::Phase::Staging);
    operation_check()?;
    let evidence = receipt_path(project)?;
    let mut workspace = None;
    let prepared = (|| -> Result<_, String> {
        let _content = crate::content_lock::ContentLock::shared(&project.root)?;
        crate::transactions::ensure_writable(&project.root)?;
        // Reopen under the snapshot lease instead of using a stale configuration.
        let source = Project::open(&project.root).map_err(|error| error.to_string())?;
        let checked = crate::revisions::check(&source.root, input)?;
        if checked["ready"] != true {
            return Ok(Err(checked));
        }
        let entries = snapshot::inventory(&source.root)?;
        self_contained_project(&source, &entries)?;
        let bytes: u64 = entries.iter().map(|(_, bytes)| bytes).sum();
        if bytes.saturating_add(checked["text_bytes"].as_u64().unwrap_or(0)) > MAX_SOURCE_BYTES {
            return Err("candidate source exceeds the 512 MiB snapshot budget".into());
        }
        for change in checked["changes"]
            .as_array()
            .ok_or("missing checked changes")?
        {
            operation_check()?;
            if !snapshot::includes(change["path"].as_str().ok_or("missing checked path")?) {
                return Err("candidate changes cannot target excluded source/state paths".into());
            }
        }
        let source_sha = snapshot::fingerprint_all(&source.root)?;
        operation_check()?;
        #[cfg(not(test))]
        {
            workspace = Some(Workspace::create()?);
        }
        #[cfg(test)]
        {
            workspace = Some(Workspace::for_test_project(&project.root)?);
        }
        let private = workspace.as_ref().ok_or("missing workspace")?;
        let directory = private.path.join("source");
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
            operation_check()?;
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
        self_contained_project(&candidate, &snapshot::inventory(&directory)?)?;
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
    let private = workspace.as_mut().ok_or("missing candidate workspace")?;
    let userdata = private.path.join("userdata");
    std::fs::create_dir(&userdata).map_err(|error| error.to_string())?;
    // The original content lease ended before invoking any compiler/runtime.
    operation_phase(crate::operation_context::Phase::Validating);
    operation_check()?;
    let result = validator(&candidate, &userdata);
    operation_check()?;
    let storage_budget = crate::operation_context::OperationContext::current()
        .map(|context| context.budget(Duration::from_secs(2)))
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or(Duration::from_secs(2));
    let storage = crate::candidate_workspace::usage_with_budget(
        &private.path,
        crate::candidate_workspace::WORKSPACE_BYTES,
        storage_budget,
    );
    operation_check()?;
    let storage_ok = storage.is_ok();
    let result = result.and_then(|result| storage.map(|_| result));
    let (validation, artifacts) = match result {
        Ok(result) => {
            let mut summary = text_summary(&result.outcome);
            let mut artifact_evidence = Vec::new();
            if result.artifacts.len() > 1 {
                summary["ok"] = json!(false);
                summary["error"] =
                    json!("native source revision accepts at most one generated library");
            }
            for artifact in &result.artifacts {
                operation_check()?;
                match artifact.verified_path(&candidate.root) {
                    Ok(_) if artifact_evidence.is_empty() => {
                        artifact_evidence
                            .push(json!({"path":artifact.path,"sha256":artifact.sha256}));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        summary["ok"] = json!(false);
                        summary["error"] = json!(receipt_line(&error));
                    }
                }
            }
            summary["native_build"] = json!(result.outcome["native_build"] == true);
            summary["native_artifacts"] = json!(artifact_evidence);
            summary["native_dependencies"] = result.outcome["native_dependencies"].clone();
            (summary, result.artifacts)
        }
        Err(error) => (json!({"ok":false,"error":receipt_line(&error)}), Vec::new()),
    };
    let source_current =
        snapshot::fingerprint_all(&project.root).is_ok_and(|hash| hash == source_sha);
    let candidate_sha = if storage_ok {
        Some(snapshot::fingerprint_all(&candidate.root)?)
    } else {
        None
    };
    let mut intact = storage_ok;
    for change in checked["changes"]
        .as_array()
        .ok_or("missing checked changes")?
    {
        operation_check()?;
        if !storage_ok {
            break;
        }
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
        "validation_changed_snapshot":candidate_sha.as_ref().map(|hash| &prepared_sha != hash),"validation":validation,
        "files_changed":checked["changes"].as_array().map(Vec::len),"source_bytes":bytes,
        "duration_ms":started.elapsed().as_millis() as u64,"candidate_retained":false,
        "userdata_redirected":true,"execution_sandboxed":false,
        "workspace_reserved_bytes":crate::candidate_workspace::WORKSPACE_BYTES,
        "workspace_recovered":private.recovered,
        "workspace_storage_ok":storage_ok,
        "native_build":validation["native_build"],"native_artifacts":validation["native_artifacts"],"native_artifacts_published":false,
        "native_dependencies":validation["native_dependencies"],
        "scope":"private source/resource validation; native builds when configured; no gameplay proof"});
    if publish && receipt["ok"] == true {
        operation_phase(crate::operation_context::Phase::Publishing);
        operation_check()?;
        let publication = match crate::transactions::publish_candidate_with_artifacts(
            project,
            &candidate,
            &source_sha,
            candidate_sha
                .as_deref()
                .ok_or("candidate has no checked fingerprint")?,
            &checked,
            &artifacts,
        ) {
            Ok(publication) => publication,
            Err(error) => json!({"ok":false,"applied":false,"error":receipt_line(&error)}),
        };
        receipt["ok"] = publication["ok"].clone();
        receipt["applied"] = publication.get("applied").cloned().unwrap_or(json!(false));
        receipt["publication"] = publication;
        receipt["scope"] =
            json!("validated journaled source publication; compiled artifacts remain private; no gameplay proof");
    }
    operation_phase(crate::operation_context::Phase::Cleanup);
    match private.clean() {
        Ok(()) => receipt["cleanup_ok"] = json!(true),
        Err(error) => {
            receipt["cleanup_ok"] = json!(false);
            receipt["candidate_retained"] = json!(true);
            receipt["ok"] = json!(false);
            receipt["cleanup_error"] = json!(crate::diagnostics::text_line(&error, &[]));
            receipt["cleanup_path"] = json!(private.path);
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
    fn cancellation_after_private_validation_never_publishes_source() {
        let fixture = Fixture::new();
        let before = snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let mut private_root = None;
        let result = context
            .scope(|| {
                run_with(
                    &fixture.0,
                    &fixture.changes(),
                    |candidate, _| {
                        private_root = Some(candidate.root.clone());
                        assert_eq!(
                            context.progress().phase,
                            crate::operation_context::Phase::Validating
                        );
                        context.request_cancel().unwrap();
                        Ok(json!({"ok":true}))
                    },
                    true,
                )
            })
            .unwrap();
        assert!(result.is_err());
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert!(!private_root.unwrap().exists());
        assert_eq!(snapshot::fingerprint_all(&fixture.0.root).unwrap(), before);
        assert!(!fixture.0.root.join(".aurum/revisions/active.json").exists());
    }

    #[test]
    fn private_validation_and_receipt_writes_do_not_commit_the_job() {
        let fixture = Fixture::new();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let result = context
            .scope(|| {
                validate_with(&fixture.0, &fixture.changes(), |_, _| {
                    assert!(!context.committed());
                    Ok(json!({"ok":true}))
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["evidence_persisted"], true);
        assert_eq!(result["applied"], false);
        assert!(!context.committed());
        assert_eq!(
            context.progress().phase,
            crate::operation_context::Phase::Cleanup
        );
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
    fn contained_cargo_hint_is_rebound_to_the_private_copy() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.0.root.join("rust")).unwrap();
        std::fs::write(fixture.0.root.join("rust/Cargo.toml"), "[workspace]\n").unwrap();
        let config = std::fs::read_to_string(fixture.0.root.join("aurum.toml")).unwrap();
        std::fs::write(
            fixture.0.root.join("aurum.toml"),
            format!("{config}\n[engine]\npath_hint=\"rust\"\n"),
        )
        .unwrap();
        let original = Project::open(&fixture.0.root).unwrap();
        let before = snapshot::fingerprint_all(&original.root).unwrap();
        let result = validate_with(&original, &fixture.changes(), |candidate, _| {
            assert_eq!(
                candidate.confined_build_workspace().unwrap(),
                candidate.root.join("rust")
            );
            assert_ne!(
                candidate.confined_build_workspace().unwrap(),
                original.build_workspace()
            );
            assert_eq!(
                std::fs::read(candidate.root.join("rust/Cargo.toml")).unwrap(),
                b"[workspace]\n"
            );
            Ok(json!({"ok":true}))
        })
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["applied"], false);
        assert_eq!(snapshot::fingerprint_all(&original.root).unwrap(), before);
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
    fn native_source_publication_keeps_the_working_library_and_records_source_only_undo() {
        let fixture = Fixture::new();
        let library = format!("native.{}", crate::build::dynamic_library_extension());
        std::fs::write(fixture.0.root.join(&library), b"working library").unwrap();
        let mut private_root = None;
        let result = run_with_validation(
            &fixture.0,
            &fixture.changes(),
            |candidate, _| {
                private_root = Some(candidate.root.clone());
                std::fs::write(candidate.root.join(&library), b"compiled library").unwrap();
                Ok(Validation {
                    outcome: json!({"ok":true,"native_build":true}),
                    artifacts: vec![crate::transactions::GeneratedArtifact {
                        path: library.clone(),
                        sha256: crate::sha256_hex(b"compiled library"),
                    }],
                })
            },
            true,
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["applied"], true);
        assert_eq!(result["native_build"], true);
        assert_eq!(result["native_artifacts_published"], false);
        assert_eq!(result["native_artifacts"][0]["path"], library);
        assert_eq!(
            result["native_artifacts"][0]["sha256"],
            crate::sha256_hex(b"compiled library")
        );
        assert!(!private_root.unwrap().exists());
        assert_eq!(
            std::fs::read(fixture.0.root.join(&library)).unwrap(),
            b"working library"
        );
        let revision = result["publication"]["revision_id"].as_str().unwrap();
        let history =
            crate::transactions::history(&fixture.0.root, &json!({"revision_id":revision}))
                .unwrap();
        assert_eq!(history["revisions"][0]["files_changed"], 1);
        assert_eq!(history["revisions"][0]["files"][0]["path"], "main.gd");
        crate::transactions::undo(&fixture.0, revision).unwrap();
        assert_eq!(
            std::fs::read(fixture.0.root.join("main.gd")).unwrap(),
            b"extends Node\n"
        );
        assert_eq!(
            std::fs::read(fixture.0.root.join(&library)).unwrap(),
            b"working library"
        );
    }

    #[test]
    fn changed_generated_library_refuses_publication_and_cleans_the_workspace() {
        let fixture = Fixture::new();
        let before = snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let library = format!("native.{}", crate::build::dynamic_library_extension());
        let mut private_root = None;
        let result = run_with_validation(
            &fixture.0,
            &fixture.changes(),
            |candidate, _| {
                private_root = Some(candidate.root.clone());
                std::fs::write(candidate.root.join(&library), b"changed after compilation")
                    .unwrap();
                Ok(Validation {
                    outcome: json!({"ok":true,"native_build":true}),
                    artifacts: vec![crate::transactions::GeneratedArtifact {
                        path: library.clone(),
                        sha256: crate::sha256_hex(b"compiled library"),
                    }],
                })
            },
            true,
        )
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["validated"], false);
        assert_eq!(result["applied"], false);
        assert_eq!(result["cleanup_ok"], true);
        assert!(result["native_artifacts"].as_array().unwrap().is_empty());
        assert!(!private_root.unwrap().exists());
        assert_eq!(snapshot::fingerprint_all(&fixture.0.root).unwrap(), before);
        assert!(!fixture.0.root.join(".aurum/revisions").exists());
    }

    #[cfg(windows)]
    #[test]
    fn native_dependency_inspection_refuses_a_preexisting_result_directory() {
        let fixture = Fixture::new();
        let directory = fixture.0.root.join("dependency-verification");
        std::fs::create_dir(&directory).unwrap();
        let output = directory.join("output.json");
        std::fs::write(&output, b"stale verification sentinel").unwrap();
        let context = crate::project_ops::RuntimeContext {
            engine: fixture.0.root.join("missing-engine.exe"),
            userdata: fixture.0.root.join("userdata"),
        };
        assert!(
            verify_native_dependencies(&fixture.0, &context, &json!({"manifests":[]})).is_err()
        );
        assert_eq!(
            std::fs::read(output).unwrap(),
            b"stale verification sentinel"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn native_dependencies_require_windows_staging_but_script_validation_is_preserved() {
        let fixture = Fixture::new();
        assert_eq!(
            validate_with(&fixture.0, &fixture.changes(), |_, _| Ok(
                json!({"ok":true})
            ))
            .unwrap()["ok"],
            true
        );
        let manifest = fixture.0.root.join("native.gdextension");
        std::fs::write(&manifest, b"native").unwrap();
        assert!(validate_with(&fixture.0, &fixture.changes(), |_, _| {
            panic!("unsupported native candidate must not reach runtime")
        })
        .is_err());
        std::fs::remove_file(manifest).unwrap();
        let config = std::fs::read_to_string(fixture.0.root.join("aurum.toml")).unwrap();
        std::fs::write(
            fixture.0.root.join("aurum.toml"),
            format!("{config}rust_package=\"native\"\n"),
        )
        .unwrap();
        let native = Project::open(&fixture.0.root).unwrap();
        assert!(runtime_validate(&native, &native, &native.root.join("userdata")).is_err());
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
        let mut workspace = Workspace::create().unwrap();
        let outside =
            std::env::temp_dir().join(format!("aurum-outside-{}", crate::random::session_id()));
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), "untouched").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, workspace.path.join("link")).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let status = std::process::Command::new("cmd")
                .creation_flags(0x08000000)
                .args(["/C", "mklink", "/J"])
                .arg(workspace.path.join("link"))
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
        let config = std::fs::read_to_string(fixture.0.root.join("aurum.toml")).unwrap();
        std::fs::write(
            fixture.0.root.join("aurum.toml"),
            format!("{config}\n[engine]\npath_hint=\"../external\"\n"),
        )
        .unwrap();
        assert!(validate_with(&fixture.0, &fixture.changes(), |_, _| panic!(
            "unsupported dependencies"
        ))
        .is_err());
        std::fs::write(fixture.0.root.join("aurum.toml"), config).unwrap();
        let request = json!({"changes":[{"path":".gitignore","action":"create","expected_sha256":"","text":"dist"}]});
        assert!(validate_with(&fixture.0, &request, |_, _| panic!("excluded changes")).is_err());
    }
}
