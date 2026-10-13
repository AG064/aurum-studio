//! Host-owned native previews over private, fingerprinted source revisions.
//! Holding this object keeps its source lease and Windows job alive. Dropping
//! it terminates only that job before releasing the private copy.
use crate::candidate_workspace::{self, Workspace};
use crate::ownership::{OwnershipRecord, ProcessKind};
use crate::runtime_channel::Channel;
use crate::{files, snapshot, Project, Session};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    version: u8,
    pid: u32,
    port: u16,
    scene: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovery {
    version: u8,
    replacement_id: String,
    phase: String,
    previous: OwnershipRecord,
    replacement: OwnershipRecord,
    host: OwnershipRecord,
    source_sha256: String,
    checkpoint: Value,
    headless: bool,
    #[serde(default)]
    previous_termination_confirmed: Option<bool>,
}
fn host_identity(project: &Project) -> Result<OwnershipRecord, String> {
    let live = crate::ownership::inspect(std::process::id())
        .ok_or("native host identity is unavailable")?;
    Ok(OwnershipRecord {
        session: crate::random::session_id(),
        executable: live.executable,
        pid: live.pid,
        started: live
            .started
            .ok_or("native host creation time unavailable")?,
        project: project.root.clone(),
        kind: ProcessKind::Worker,
    })
}

// Neither the channel nor its token may be serialized or printed by Debug.
pub struct NativePreview {
    worker: Arc<crate::worker_windows::Worker>,
    monitor_stop: Arc<AtomicBool>,
    workspace: Option<Workspace>,
    channel: Channel,
    source_sha256: String,
    prepared_sha256: String,
    headless: bool,
    owner: OwnershipRecord,
    session: Session,
    source: PathBuf,
    userdata: crate::native_userdata::UserdataRun,
    dependencies: Value,
}
impl NativePreview {
    /// Stage, import and boot while paused, without changing working content.
    /// Self-contained scripts and privately staged Rust/GDExtension projects.
    pub fn prepare(project: &Project, headless: bool) -> Result<Self, String> {
        Self::prepare_from(project, headless, None)
    }
    fn prepare_from(
        project: &Project,
        headless: bool,
        parent: Option<&crate::native_userdata::UserdataRun>,
    ) -> Result<Self, String> {
        let (mut workspace, candidate, source_sha256, engine) = {
            let _lease = crate::content_lock::ContentLock::shared(&project.root)?;
            crate::transactions::ensure_writable(&project.root)?;
            let project = Project::open(&project.root).map_err(|error| error.to_string())?;
            let inventory = snapshot::inventory(&project.root)?;
            if project.config.validation_command.is_some() {
                return Err("private native preview does not stage custom validators".into());
            }
            project.confined_build_workspace()?;
            if inventory.iter().map(|(_, bytes)| bytes).sum::<u64>()
                > crate::candidates::MAX_SOURCE_BYTES
            {
                return Err("private native source exceeds the 512 MiB copy budget".into());
            }
            let engine = crate::project_ops::engine_binary(&project)?
                .canonicalize()
                .map(crate::project::clean_path)
                .map_err(|error| error.to_string())?;
            let source_sha256 = snapshot::fingerprint_all(&project.root)?;
            let workspace = Workspace::create_native()?;
            let source = workspace.path.join("source");
            snapshot::copy(&project.root, &source)?;
            if snapshot::fingerprint_all(&source)? != source_sha256
                || snapshot::fingerprint_all(&project.root)? != source_sha256
            {
                return Err(
                    "source changed during private native preparation; retry from fresh content"
                        .into(),
                );
            }
            let candidate = Project::open(&source).map_err(|error| error.to_string())?;
            (workspace, candidate, source_sha256, engine)
        };
        let godot = candidate
            .godot_project_dir()
            .ok_or("private native preview requires project.godot")?;
        if godot.join("addons/aurum_live").exists() {
            return Err("addons/aurum_live is reserved for private managed snapshots".into());
        }
        files::write_atomic(
            &godot.join("addons/aurum_live/runtime.gd"),
            include_bytes!("runtime_bridge.gd"),
        )
        .map_err(|error| error.to_string())?;
        files::write_atomic(
            &godot.join("addons/aurum_live/native_dependency_plan.gd"),
            include_bytes!("native_dependency_plan.gd"),
        )
        .map_err(|error| error.to_string())?;
        let setup = workspace.path.join("setup.gd");
        files::write_atomic(&setup, include_bytes!("native_preview_setup.gd"))
            .map_err(|error| error.to_string())?;
        let validation_data = workspace.path.join("validation-userdata");
        std::fs::create_dir(&validation_data).map_err(|error| error.to_string())?;
        let dependencies = crate::native_dependencies::prepare(
            &candidate,
            &workspace.path,
            &engine,
            &validation_data,
        )?;
        let environment: Vec<(String, String)> = [
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
        ]
        .into_iter()
        .map(|key| (key.into(), validation_data.to_string_lossy().into_owned()))
        .collect();
        let mut setup_command = crate::Command::new(&engine)
            .args(["--headless", "--path"])
            .arg(godot.to_string_lossy())
            .arg("--script")
            .arg(setup.to_string_lossy())
            .candidate_storage(&workspace.path);
        for (key, value) in &environment {
            setup_command = setup_command.env(key, value);
        }
        let prepared = crate::native_runtime::run(&setup_command, Duration::from_secs(30))
            .map_err(|error| error.to_string())?;
        if !prepared.success() || prepared.stderr.contains("ERROR:") {
            return Err("private native setup failed; working content was not changed".into());
        }
        let validation = crate::project_ops::validate_with_context(
            &candidate,
            Some(&crate::project_ops::RuntimeContext {
                engine: engine.clone(),
                userdata: validation_data,
            }),
        )?;
        if validation["ok"] != true {
            return Err(
                "private native validation failed; the existing game was not touched".into(),
            );
        }
        candidate_workspace::usage(&workspace.path, candidate_workspace::WORKSPACE_BYTES)?;
        let prepared_sha256 = snapshot::fingerprint_all(&candidate.root)?;
        let userdata = crate::native_userdata::UserdataRun::stage(&project.root, parent)?;
        let mut random = crate::random::bytes(32);
        if random.source != crate::random::Source::OperatingSystem {
            return Err("native preview requires operating-system token generation".into());
        }
        let token: String = random
            .bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        random.bytes.fill(0);
        let log_root = files::confined(&project.root, ".aurum/native/sessions")?;
        crate::diagnostics::checked_path(&log_root).map_err(|error| error.to_string())?;
        let mut session =
            Session::create_in(&log_root, &project.root).map_err(|error| error.to_string())?;
        session.token = token.clone();
        session.write_token().map_err(|error| error.to_string())?;
        let ready_path = workspace.path.join("ready.json");
        let mut overrides: Vec<(String, String)> = [
            "APPDATA",
            "LOCALAPPDATA",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
        ]
        .into_iter()
        .map(|key| (key.into(), userdata.path.to_string_lossy().into_owned()))
        .collect();
        overrides.extend([
            (
                "AURUM_NATIVE_PROJECT_ROOT".into(),
                candidate.root.to_string_lossy().replace('\\', "/"),
            ),
            ("AURUM_NATIVE_TOKEN".into(), token.clone()),
            (
                "AURUM_NATIVE_READY".into(),
                ready_path.to_string_lossy().into_owned(),
            ),
            ("AURUM_NATIVE_BOOT_PAUSED".into(), "1".into()),
            (
                "AURUM_NATIVE_BOOT_HIDDEN".into(),
                if headless { "0" } else { "1" }.into(),
            ),
        ]);
        let mut args = vec!["--path".into(), godot.to_string_lossy().into_owned()];
        if headless {
            args.insert(0, "--headless".into());
        }
        let (worker, stdout, stderr) =
            crate::worker_windows::Worker::spawn(&engine, &args, Some(&candidate.root), &overrides)
                .map_err(|error| error.to_string())?;
        let worker = Arc::new(worker);
        for pipe in [stdout, stderr] {
            let session = session.clone();
            std::thread::spawn(move || crate::diagnostic_relay::drain(pipe, &session));
        }
        let result = (|| -> Result<_, String> {
            let deadline = Instant::now() + Duration::from_secs(15);
            let ready = loop {
                if Instant::now() >= deadline {
                    return Err(
                        "prepared native game did not become ready within 15 seconds".into(),
                    );
                }
                if let Ok(metadata) = std::fs::metadata(&ready_path) {
                    if !metadata.is_file() || metadata.len() > 4096 {
                        return Err("native readiness exceeds supported bounds".into());
                    }
                    let bytes = std::fs::read(&ready_path).map_err(|error| error.to_string())?;
                    if bytes.len() > 4096 {
                        return Err("native readiness exceeds supported bounds".into());
                    }
                    if Instant::now() >= deadline {
                        return Err(
                            "prepared native game did not become ready within 15 seconds".into(),
                        );
                    }
                    break serde_json::from_slice::<Ready>(&bytes)
                        .map_err(|_| "invalid native readiness record")?;
                }
                if worker
                    .try_wait()
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    return Err("prepared native game exited before readiness".into());
                }
                candidate_workspace::usage_with_budget(
                    &workspace.path,
                    candidate_workspace::WORKSPACE_BYTES,
                    deadline.saturating_duration_since(Instant::now()),
                )?;
                if Instant::now() >= deadline {
                    return Err(
                        "prepared native game did not become ready within 15 seconds".into(),
                    );
                }
                std::thread::sleep(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            };
            if ready.version != 1
                || ready.pid != worker.pid()
                || ready.port == 0
                || !ready.scene.starts_with("res://")
            {
                return Err("native readiness identity or scene is invalid".into());
            }
            let live = crate::ownership::inspect(worker.pid())
                .ok_or("prepared native process is not running")?;
            let owner = OwnershipRecord {
                session: session.id.clone(),
                executable: engine,
                pid: live.pid,
                started: worker.creation_time().map_err(|error| error.to_string())?,
                project: project.root.clone(),
                kind: ProcessKind::Game,
            };
            if !owner.describes(&live) {
                return Err("native process identity does not match its prepared launch".into());
            }
            let channel = Channel::new(ready.port, token, owner.clone())?;
            let checkpoint =
                channel.request(&json!({"op":"checkpoint"}), Duration::from_secs(5))?;
            if checkpoint["ok"] != true || checkpoint["checkpoint"]["paused"] != true {
                return Err("prepared native game did not boot paused".into());
            }
            if dependencies["native"] == true {
                let manifests: Vec<_> = dependencies["manifests"]
                    .as_array()
                    .ok_or("missing prepared dependency manifests")?
                    .iter()
                    .map(|row| row["manifest"].clone())
                    .collect();
                let actual = channel.request(
                    &json!({"op":"dependencies","manifests":manifests}),
                    Duration::from_secs(5),
                )?;
                crate::native_dependencies::verify(&dependencies, &actual)?;
            }
            owner
                .write(&session.ownership_directory())
                .map_err(|error| error.to_string())?;
            Ok((channel, owner))
        })();
        match result {
            Ok((channel, owner)) => {
                let monitor_stop = Arc::new(AtomicBool::new(false));
                let stopping = monitor_stop.clone();
                let watched = worker.clone();
                let path = workspace.path.clone();
                let data_path = userdata.path.clone();
                let log = session.clone();
                std::thread::spawn(move || {
                    while !stopping.load(Ordering::Acquire) {
                        if !watched.try_wait().is_ok_and(|status| status.is_none()) {
                            break;
                        }
                        if candidate_workspace::usage(&path, candidate_workspace::WORKSPACE_BYTES)
                            .is_err()
                            || crate::native_userdata::observed_ok(&data_path).is_err()
                        {
                            let _ = watched.terminate();
                            crate::diagnostics::report(log.log(
                                "native preview stopped after its observed storage budget failed",
                            ));
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                });
                Ok(Self {
                    worker,
                    monitor_stop,
                    source: candidate.root,
                    workspace: Some(workspace),
                    channel,
                    source_sha256,
                    prepared_sha256,
                    headless,
                    owner,
                    session,
                    userdata,
                    dependencies,
                })
            }
            Err(error) => {
                let retirement = worker.terminate();
                drop(worker);
                if let Err(retirement) = retirement {
                    // Retain the marked copy and its allocation lease until
                    // host exit rather than removing files under a live job.
                    std::mem::forget(workspace);
                    return Err(format!("{error}; native retirement was not confirmed: {retirement}; private workspace retained"));
                }
                match workspace.clean() {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!(
                        "{error}; native workspace cleanup failed: {cleanup}"
                    )),
                }
            }
        }
    }
    pub fn request(&self, input: &Value, budget: Duration) -> Result<Value, String> {
        self.channel.request(input, budget)
    }
    pub fn status(&self) -> Value {
        json!({"pid":self.owner.pid,"running":self.worker.try_wait().is_ok_and(|status|status.is_none()),"source_sha256":self.source_sha256,
            "source_private":true,"source":self.source,"session":self.session.id,"host_owned":true,"execution_sandboxed":false,"native_dependencies":self.dependencies})
    }
    pub(crate) fn owner(&self) -> OwnershipRecord {
        self.owner.clone()
    }
    pub fn source_still_current(&self, project: &Project) -> bool {
        snapshot::fingerprint_all(&project.root).is_ok_and(|hash| hash == self.source_sha256)
    }
    pub fn activate(&self) -> Result<Value, String> {
        let result = self.request(
            &json!({"op":"resume","paused":false,"render_loop_enabled":true,"show_window":true}),
            Duration::from_secs(5),
        )?;
        if result["ok"] == true {
            self.userdata.publish()?;
        }
        Ok(result)
    }
    pub fn checkpoint(&self, project: &Project) -> Result<Value, String> {
        if !crate::ownership::paths_equal(&self.owner.project, &project.root) {
            return Err("native checkpoint must use the owned project".into());
        }
        let mut captured = self.request(&json!({"op":"checkpoint"}), Duration::from_secs(5))?;
        if captured["ok"] != true || captured["complete"] != true {
            captured["evidence_persisted"] = json!(false);
            return Ok(captured);
        }
        if snapshot::fingerprint_all(&self.source)? != self.prepared_sha256 {
            return Err(
                "private native source changed; checkpoint recovery cannot assert its revision"
                    .into(),
            );
        }
        let path = files::confined(&project.root, ".aurum/native/replacement-latest.json")?;
        let record = Recovery {
            version: 1,
            replacement_id: crate::random::session_id(),
            phase: "checkpointed".into(),
            previous: self.owner.clone(),
            replacement: self.owner.clone(),
            host: host_identity(project)?,
            source_sha256: self.source_sha256.clone(),
            checkpoint: captured["checkpoint"].clone(),
            headless: self.headless,
            previous_termination_confirmed: None,
        };
        persist_handoff(
            &path,
            &serde_json::to_value(record).map_err(|error| error.to_string())?,
        )?;
        captured["evidence"] = json!(path);
        captured["evidence_persisted"] = json!(true);
        Ok(captured)
    }
    /// Prepare and prove restoration before terminating the witnessed old job.
    /// The previous preview remains owned until the new one is active and ready.
    pub fn replace(&mut self, project: &Project) -> Result<Value, String> {
        if !crate::ownership::paths_equal(&self.owner.project, &project.root) {
            return Err("native replacement must use the original project".into());
        }
        let mut replacement = Self::prepare_from(project, self.headless, Some(&self.userdata))?;
        let _lease = crate::content_lock::ContentLock::shared(&project.root)?;
        crate::transactions::ensure_writable(&project.root)?;
        if !replacement.source_still_current(project) {
            return Err("source changed during replacement preparation".into());
        }
        let started = Instant::now();
        let freeze_id = crate::random::session_id();
        let frozen = self.request(
            &json!({"op":"checkpoint","freeze":true,"freeze_id":freeze_id}),
            Duration::from_secs(5),
        );
        let result = (|| -> Result<_, String> {
            let frozen = frozen?;
            if !replacement.userdata.seed_unchanged()? {
                return Err("native saved data changed during preparation; the original data remains current".into());
            }
            if frozen["ok"] != true || frozen["complete"] != true {
                return Err("native replacement requires a complete checkpoint".into());
            }
            let lease_ms = frozen["freeze_lease_ms"]
                .as_u64()
                .ok_or("missing native freeze lease")?;
            let budget = Duration::from_millis(lease_ms.saturating_sub(500).min(15_000));
            let deadline = started + budget;
            let remaining = || {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    Err("native replacement exceeded its freeze lease budget".to_string())
                } else {
                    Ok(remaining)
                }
            };
            let checkpoint = &frozen["checkpoint"];
            if checkpoint["version"] != 1
                || !checkpoint["paused"].is_boolean()
                || !checkpoint["render_loop_enabled"].is_boolean()
            {
                return Err("invalid native checkpoint control state".into());
            }
            let baseline = replacement.request(&json!({"op":"checkpoint"}), remaining()?)?;
            let mut held = checkpoint.clone();
            held["paused"] = json!(true);
            held["render_loop_enabled"] = json!(false);
            let restored =
                replacement.request(&json!({"op":"restore","checkpoint":held}), remaining()?)?;
            if restored["ok"] != true || restored["complete"] != true {
                return Err("replacement restoration was rejected or incomplete".into());
            }
            let verified = replacement.request(&json!({"op":"checkpoint"}), remaining()?)?;
            if verified["ok"] != true
                || verified["complete"] != true
                || !checkpoint_preserved(
                    checkpoint,
                    &baseline["checkpoint"],
                    &verified["checkpoint"],
                )?
            {
                let keys = checkpoint["custom"]
                    .as_object()
                    .map(|values| {
                        values
                            .iter()
                            .filter(|(key, value)| {
                                verified["checkpoint"]["custom"].get(*key) != Some(*value)
                            })
                            .take(8)
                            .map(|(key, _)| key.chars().take(64).collect::<String>())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                return Err(format!("replacement state verification did not match the checkpoint; differing_fields={keys:?}"));
            }
            if !replacement.source_still_current(project)
                || snapshot::fingerprint_all(&replacement.source)? != replacement.prepared_sha256
            {
                return Err("working or private source changed during native handoff".into());
            }
            remaining()?;
            let receipt_path =
                files::confined(&project.root, ".aurum/native/replacement-latest.json")?;
            let mut journal = serde_json::to_value(Recovery {
                version: 1,
                replacement_id: crate::random::session_id(),
                phase: "restored".into(),
                previous: self.owner.clone(),
                replacement: replacement.owner.clone(),
                host: host_identity(project)?,
                source_sha256: replacement.source_sha256.clone(),
                checkpoint: checkpoint.clone(),
                headless: self.headless,
                previous_termination_confirmed: None,
            })
            .map_err(|error| error.to_string())?;
            persist_handoff(&receipt_path, &journal)?;
            let activated = replacement.request(
                &json!({"op":"resume","paused":checkpoint["paused"],
                "render_loop_enabled":checkpoint["render_loop_enabled"],"show_window":true}),
                remaining()?,
            )?;
            if activated["ok"] != true {
                return Err("replacement activation was rejected".into());
            }
            if replacement.request(&json!({"op":"inspect","path":"."}), remaining()?)?["ok"] != true
            {
                return Err("activated native scene was not ready".into());
            }
            journal["phase"] = json!("activated");
            persist_handoff(&receipt_path, &journal)?;
            remaining()?;
            if !replacement.userdata.seed_unchanged()? {
                return Err(
                    "native saved data changed during handoff; the original data remains current"
                        .into(),
                );
            }
            replacement.userdata.publish()?;
            Ok((journal, receipt_path, restored))
        })();
        let (mut journal, receipt_path, restored) = match result {
            Ok(result) => result,
            Err(error) => {
                let resumed = self
                    .request(
                        &json!({"op":"release_freeze","freeze_id":freeze_id}),
                        Duration::from_secs(5),
                    )
                    .is_ok_and(|value| value["ok"] == true);
                return Err(format!("{error}; previous_game_resumed={resumed}"));
            }
        };
        let previous_pid = self.owner.pid;
        // This handle owns exactly the original job, not a PID lookup or editor.
        // If termination/cleanup is delayed, retain the verified active successor
        // and report the uncertainty instead of destroying the only proven run.
        self.monitor_stop.store(true, Ordering::Release);
        let retirement_error = self.worker.terminate().err().map(|error| error.to_string());
        let retired = retirement_error.is_none();
        if retired {
            let _ = self.owner.remove(&self.session.ownership_directory());
        }
        std::mem::swap(self, &mut replacement);
        journal["phase"] = json!("committed");
        journal["previous_termination_confirmed"] = json!(retired);
        let evidence_persisted = persist_handoff(&receipt_path, &journal).is_ok();
        Ok(
            json!({"ok":true,"replaced":true,"previous_pid":previous_pid,"pid":self.owner.pid,
            "state_preserved":true,"restoration":restored,"previous_termination_confirmed":retired,
            "previous_termination_error":retirement_error,
            "evidence":receipt_path,"evidence_persisted":evidence_persisted,"source_sha256":self.source_sha256,
            "scope":"host-owned native replacement; custom hooks declare state completeness"}),
        )
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.monitor_stop.store(true, Ordering::Release);
        self.worker.terminate().map_err(|error| error.to_string())?;
        let ownership = self
            .owner
            .remove(&self.session.ownership_directory())
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|error| format!("native ownership cleanup failed: {error}"));
        let workspace = self.workspace.as_mut().map_or(Ok(()), Workspace::clean);
        match (ownership, workspace) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(ownership), Err(workspace)) => Err(format!("{ownership}; {workspace}")),
        }
    }
    /// Resume a checkpoint after the recorded host and both jobs have ended.
    /// No recorded PID is stopped, and newer working source is never overwritten.
    pub fn recover(project: &Project) -> Result<(Self, Value), String> {
        let path = files::confined(&project.root, ".aurum/native/replacement-latest.json")?;
        crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 + 65536 {
            return Err("native recovery journal exceeds its bound".into());
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .map_err(|error| error.to_string())?
            .take(1024 * 1024 + 65537)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 1024 * 1024 + 65536 {
            return Err("native recovery journal exceeds its bound".into());
        }
        let mut recovery: Recovery = serde_json::from_slice(&bytes)
            .map_err(|_| "native recovery journal schema is invalid")?;
        if recovery.version != 1
            || !matches!(
                recovery.phase.as_str(),
                "restored" | "activated" | "committed" | "recovered" | "checkpointed"
            )
            || recovery.replacement_id.len() != 32
            || !recovery
                .replacement_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || recovery.source_sha256.len() != 64
            || !recovery
                .source_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || recovery.checkpoint["version"] != 1
            || !recovery.checkpoint["paused"].is_boolean()
            || !recovery.checkpoint["render_loop_enabled"].is_boolean()
            || recovery.previous.kind != ProcessKind::Game
            || recovery.replacement.kind != ProcessKind::Game
            || recovery.host.kind != ProcessKind::Worker
        {
            return Err("native recovery journal identity or version is invalid".into());
        }
        for record in [&recovery.host, &recovery.previous, &recovery.replacement] {
            if !crate::ownership::paths_equal(&record.project, &project.root)
                || !crate::ownership::recorded_process_gone(record)
            {
                return Err(
                    "native recovery refused a live, inaccessible or foreign recorded process"
                        .into(),
                );
            }
        }
        if snapshot::fingerprint_all(&project.root)? != recovery.source_sha256 {
            return Err(
                "working source differs from the verified native revision; recovery preserves it"
                    .into(),
            );
        }
        let preview = Self::prepare(project, recovery.headless)?;
        let baseline = preview.request(&json!({"op":"checkpoint"}), Duration::from_secs(5))?;
        let mut held = recovery.checkpoint.clone();
        held["paused"] = json!(true);
        held["render_loop_enabled"] = json!(false);
        let restored = preview.request(
            &json!({"op":"restore","checkpoint":held}),
            Duration::from_secs(5),
        )?;
        if restored["ok"] != true || restored["complete"] != true {
            return Err("native recovery restoration was rejected or incomplete".into());
        }
        let actual = preview.request(&json!({"op":"checkpoint"}), Duration::from_secs(5))?;
        if actual["ok"] != true
            || actual["complete"] != true
            || !checkpoint_preserved(
                &recovery.checkpoint,
                &baseline["checkpoint"],
                &actual["checkpoint"],
            )?
        {
            return Err("native recovered state did not match its checkpoint".into());
        }
        let _lease = crate::content_lock::ContentLock::shared(&project.root)?;
        if !preview.source_still_current(project) {
            return Err("source changed during native recovery; working edits preserved".into());
        }
        let resumed=preview.request(&json!({"op":"resume","paused":recovery.checkpoint["paused"],"render_loop_enabled":recovery.checkpoint["render_loop_enabled"],"show_window":true}),Duration::from_secs(5))?;
        if resumed["ok"] != true {
            return Err("native recovery activation failed".into());
        }
        preview.userdata.publish()?;
        recovery.phase = "recovered".into();
        recovery.host = host_identity(project)?;
        recovery.previous = recovery.replacement;
        recovery.replacement = preview.owner.clone();
        let persisted = persist_handoff(
            &path,
            &serde_json::to_value(&recovery).map_err(|error| error.to_string())?,
        )
        .is_ok();
        let receipt = json!({"ok":true,"recovered":true,"pid":preview.owner.pid,"state_preserved":true,"evidence":path,"evidence_persisted":persisted});
        Ok((preview, receipt))
    }
}
fn persist_handoff(path: &std::path::Path, value: &Value) -> Result<(), String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > 1024 * 1024 + 65536 {
        return Err("native recovery receipt exceeds its byte bound".into());
    }
    files::write_atomic(path, &bytes).map_err(|error| error.to_string())
}
fn checkpoint_preserved(old: &Value, baseline: &Value, actual: &Value) -> Result<bool, String> {
    if old["scene"] != actual["scene"] || old["version"] != actual["version"] {
        return Ok(false);
    }
    if old.get("custom").is_some() {
        return Ok(old["custom"] == actual["custom"]);
    }
    let nodes = old["nodes"]
        .as_array()
        .ok_or("invalid property checkpoint")?;
    let next = actual["nodes"]
        .as_array()
        .ok_or("invalid verified property checkpoint")?;
    let defaults = baseline["nodes"]
        .as_array()
        .ok_or("invalid property baseline")?;
    for node in nodes {
        let Some(actual) = next
            .iter()
            .find(|next| next["path"] == node["path"] && next["class"] == node["class"])
        else {
            return Ok(false);
        };
        let new_defaults = defaults.iter().find(|next| next["path"] == node["path"]);
        for (name, value) in node["values"]
            .as_object()
            .ok_or("invalid property values")?
        {
            let expected = if node["defaults"].get(name) == Some(value) {
                new_defaults
                    .and_then(|node| node["defaults"].get(name))
                    .unwrap_or(value)
            } else {
                value
            };
            if actual["values"].get(name) != Some(expected) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
impl Drop for NativePreview {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            crate::diagnostics::report(
                self.session
                    .log(&format!("native preview cleanup failed: {error}")),
            );
            if !self.worker.termination_confirmed().unwrap_or(false) {
                // Keep the private source and its lease until host exit. The
                // next allocator can then recover this marked workspace.
                if let Some(workspace) = self.workspace.take() {
                    std::mem::forget(workspace);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires checksum-pinned Godot; explicit Windows native acceptance"]
    fn actual_private_native_preparation() {
        let engine = std::env::var_os("AURUM_GODOT").expect("AURUM_GODOT is required");
        assert!(std::path::Path::new(&engine).is_file());
        let root = std::env::temp_dir().join(format!(
            "aurum-native-private-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("aurum.toml"),
            "schema_version=1\nname=\"native-private\"\nmodules=[]\n",
        )
        .unwrap();
        std::fs::write(root.join("project.godot"),"config_version=5\n[application]\nrun/main_scene=\"res://main.tscn\"\n[rendering]\nrenderer/rendering_method=\"gl_compatibility\"\n").unwrap();
        std::fs::write(root.join("main.tscn"),"[gd_scene load_steps=2 format=3]\n[ext_resource type=\"Script\" path=\"res://main.gd\" id=\"1\"]\n[node name=\"Mission\" type=\"Node3D\"]\nscript=ExtResource(\"1\")\n").unwrap();
        let original = "extends Node3D\n@export var speed: float = 4.0\nvar inventory = [\"cell\",\"tool\"]\nvar enemy_ids = [\"guard-4\",\"guard-9\"]\nvar cooldowns = {\"dash\": 1.5}\nvar mission_step = 3\nfunc aurum_capture_state():\n\treturn {\"schema\":1,\"position\":position,\"inventory\":inventory,\"enemy_ids\":enemy_ids,\"cooldowns\":cooldowns,\"mission_step\":mission_step}\nfunc aurum_restore_state(state):\n\tif not state is Dictionary or state.get(\"schema\") != 1 or not state.get(\"position\") is Vector3: return false\n\tposition = state.position\n\tinventory = state.inventory.duplicate()\n\tenemy_ids = state.enemy_ids.duplicate()\n\tcooldowns = state.cooldowns.duplicate()\n\tmission_step = int(state.mission_step)\n\treturn true\n";
        let original = format!("{original}\n@export var saved_value: String = \"\"\nfunc _ready():\n\tvar file = FileAccess.open(\"user://persistent.dat\", FileAccess.READ)\n\tif file != null: saved_value = file.get_as_text()\n\telse:\n\t\tfile = FileAccess.open(\"user://persistent.dat\", FileAccess.WRITE)\n\t\tfile.store_string(\"saved-progress\")\n\t\tfile.close()\n\t\tsaved_value = \"saved-progress\"\n");
        std::fs::write(root.join("main.gd"), &original).unwrap();
        std::fs::write(root.join(".env"), "private-sentinel").unwrap();
        let project = Project::open(&root).unwrap();
        let before = snapshot::fingerprint_all(&root).unwrap();
        let mut preview = NativePreview::prepare(&project, true).unwrap();
        assert_eq!(before, snapshot::fingerprint_all(&root).unwrap());
        assert!(!preview.source.join(".env").exists());
        assert!(!root.join(".godot").exists());
        assert!(preview.source_still_current(&project));
        let budget = Duration::from_secs(5);
        assert_eq!(
            preview
                .request(&json!({"op":"checkpoint"}), budget)
                .unwrap()["checkpoint"]["paused"],
            true
        );
        assert_eq!(preview.activate().unwrap()["paused"], false);
        assert_eq!(preview.request(&json!({"op":"apply","changes":[{"path":".","property":"position","value":{"__aurum_type":"Vector3","value":[5,2,-8]}}]}),budget).unwrap()["ok"],true);
        let captured = preview
            .request(&json!({"op":"checkpoint"}), budget)
            .unwrap();
        std::fs::write(root.join("main.gd"), "extends Node3D\nvar =\n").unwrap();
        assert!(NativePreview::prepare(&project, true).is_err());
        assert_eq!(
            preview
                .request(&json!({"op":"checkpoint"}), budget)
                .unwrap()["checkpoint"]["custom"],
            captured["checkpoint"]["custom"]
        );
        assert_eq!(preview.status()["running"], true);
        std::fs::write(
            root.join("main.gd"),
            original.replace("return true\n", "return false\n"),
        )
        .unwrap();
        let old_pid = preview.owner.pid;
        assert!(preview.replace(&project).is_err());
        let after_rejection = preview
            .request(&json!({"op":"checkpoint"}), budget)
            .unwrap();
        assert_eq!(after_rejection["checkpoint"]["paused"], false);
        assert_eq!(
            after_rejection["checkpoint"]["custom"],
            captured["checkpoint"]["custom"]
        );
        assert_eq!(preview.owner.pid, old_pid);
        std::fs::write(
            root.join("main.gd"),
            original.replace("speed: float = 4.0", "speed: float = 6.0"),
        )
        .unwrap();
        let previous_owner = preview.owner.clone();
        let replaced = preview.replace(&project).unwrap();
        assert_eq!(replaced["state_preserved"], true);
        assert_eq!(replaced["previous_termination_confirmed"], true);
        assert_ne!(preview.owner.pid, old_pid);
        assert!(crate::ownership::recorded_process_gone(&previous_owner));
        let after_replacement = preview
            .request(&json!({"op":"checkpoint"}), budget)
            .unwrap();
        assert_eq!(
            after_replacement["checkpoint"]["custom"],
            captured["checkpoint"]["custom"]
        );
        let properties = preview.request(&json!({"op":"inspect"}), budget).unwrap();
        assert_eq!(
            properties["properties"]
                .as_array()
                .unwrap()
                .iter()
                .find(|value| value["name"] == "speed")
                .unwrap()["value"],
            6.0
        );
        let journal =
            std::fs::read_to_string(root.join(".aurum/native/replacement-latest.json")).unwrap();
        assert!(!journal.contains(&preview.session.token));
        let data_path = preview.userdata.path.clone();
        let data_id = preview.userdata.id.clone();
        let source = preview.source.clone();
        let pid = preview.owner.pid;
        preview.stop().unwrap();
        assert!(crate::ownership::inspect(pid).is_none());
        assert!(!source.exists());
        assert!(data_path.exists());
        assert_eq!(
            crate::native_userdata::list(&root).unwrap()["current"],
            data_id
        );
        drop(preview);
        let restarted = NativePreview::prepare(&project, true).unwrap();
        let resumed_saved = restarted.request(&json!({"op":"inspect"}), budget).unwrap();
        assert_eq!(
            resumed_saved["properties"]
                .as_array()
                .unwrap()
                .iter()
                .find(|value| value["name"] == "saved_value")
                .unwrap()["value"],
            "saved-progress"
        );
        std::fs::write(root.join("evidence.json"),serde_json::to_vec_pretty(&json!({"ok":true,"pid":pid,"source_unchanged":true,"bad_preparation_preserved_old":true,"restore_rejection_preserved_old":true,"replacement":replaced,"captured":captured,"restored":after_replacement})).unwrap()).unwrap();
        println!(
            "native preparation evidence: {}",
            root.join("evidence.json").display()
        );
    }
}
