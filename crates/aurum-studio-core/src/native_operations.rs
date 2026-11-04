//! Persistent protocol-session ownership for native project operations.
#[cfg(windows)]
use crate::Project;
#[cfg(windows)]
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const OPERATIONS: &[&str] = &[
    "native_start",
    "native_recover",
    "native_status",
    "native_inspect",
    "runtime_context",
    "native_apply",
    "native_input",
    "native_checkpoint",
    "native_replace",
    "native_stop",
];
pub fn supported(op: &str) -> bool {
    OPERATIONS.contains(&op)
}

#[derive(Default)]
pub struct NativeRuntime {
    controller_root: Option<PathBuf>,
    jobs: Mutex<BTreeMap<PathBuf, Arc<crate::jobs::JobManager>>>,
    workspace_access: Arc<crate::workspace_access::Broker>,
    #[cfg(windows)]
    projects: Mutex<BTreeMap<PathBuf, Arc<Controller>>>,
    closed: AtomicBool,
}
/// Issued only by the authenticated write-capable workspace action. This is
/// neither deserializable nor constructible from a request or an image token.
pub(crate) struct ConfigFileParsePermission<'a> {
    runtime: &'a NativeRuntime,
    root: PathBuf,
    manifest_root: String,
    manifest_path: String,
}
impl ConfigFileParsePermission<'_> {
    pub(crate) fn broker(&self) -> &Arc<crate::workspace_access::Broker> {
        &self.runtime.workspace_access
    }
    pub(crate) fn check_broker(
        &self,
        broker: &crate::workspace_access::Broker,
    ) -> Result<(), String> {
        if self.runtime.closed.load(Ordering::Acquire)
            || !self.runtime.is_controller_for(&self.root)
            || !std::ptr::eq(Arc::as_ptr(self.broker()), broker)
        {
            return Err("configfile_parser_original_runtime_permission_required".into());
        }
        Ok(())
    }
    pub(crate) fn check_manifest(&self, root: &str, path: &str) -> Result<(), String> {
        if root != self.manifest_root || path != self.manifest_path {
            return Err("configfile_parser_permitted_selection_mismatch".into());
        }
        Ok(())
    }
    pub(crate) fn check_request(
        &self,
        broker: &crate::workspace_access::Broker,
        input: &Value,
    ) -> Result<(), String> {
        self.check_broker(broker)?;
        if input["compile_native"].as_bool() != Some(true) {
            return Err("configfile_parser_native_compile_required".into());
        }
        self.check_manifest(
            input["gdextension_manifest"]["root_id"]
                .as_str()
                .ok_or("configfile_parser_selection")?,
            input["gdextension_manifest"]["path"]
                .as_str()
                .ok_or("configfile_parser_selection")?,
        )
    }
    #[cfg(windows)]
    pub(crate) fn controller(&self) -> Result<crate::worker_windows::StreamController, String> {
        self.check_broker(self.broker())?;
        self.runtime.managed_native_output(&self.root)
    }
    #[cfg(windows)]
    pub(crate) fn mark_unconfirmed(&self) {
        self.broker().mark_parser_unconfirmed();
    }
    #[cfg(windows)]
    pub(crate) fn check(
        &self,
        parent: &crate::external_native::ParentDeadline,
        controller: &crate::worker_windows::StreamController,
    ) -> Result<(), String> {
        self.check_broker(self.broker())?;
        parent.check_read_controller(controller)?;
        self.runtime
            .check_managed_native_output(&self.root, controller)
    }
    #[cfg(windows)]
    pub(crate) fn hold_engine<'a>(
        &'a self,
        parent: &'a crate::external_native::ParentDeadline,
        controller: &'a crate::worker_windows::StreamController,
    ) -> Result<crate::external_native::HeldConfigFileEngine<'a>, String> {
        self.check(parent, controller)?;
        crate::external_native::HeldConfigFileEngine::hold_configured(
            self.runtime,
            &self.root,
            parent,
            controller,
        )
    }
}
impl std::fmt::Debug for NativeRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeRuntime { credentials: redacted }")
    }
}
#[cfg(windows)]
#[derive(Default)]
struct Controller {
    preview: Mutex<Option<crate::native_preview::NativePreview>>,
    status: Mutex<Value>,
    owner: Mutex<Option<crate::ownership::OwnershipRecord>>,
    // The OS lease is separate from content locks, so gameplay never blocks edits.
    lease: Mutex<Option<std::fs::File>>,
    busy: AtomicBool,
}
#[cfg(windows)]
fn validate_mutation(input: &Value, operation: &str) -> Result<usize, String> {
    let (field, maximum) = if operation == "native_apply" {
        ("properties", 128)
    } else {
        ("input_actions", 32)
    };
    let rows = input[field]
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= maximum)
        .ok_or("native mutation requires a bounded non-empty array")?;
    for row in rows {
        if !row.is_object() {
            return Err("native mutation entries must be objects".into());
        }
        if operation == "native_apply" {
            let path = row["path"]
                .as_str()
                .ok_or("native property path must be a string")?;
            if path.chars().count() > 512
                || path.starts_with('/')
                || path.contains(':')
                || path.split('/').any(|part| part == "..")
            {
                return Err("native property path is invalid".into());
            }
            if !row["property"]
                .as_str()
                .is_some_and(|name| !name.is_empty() && name.chars().count() <= 256)
                || row.get("value").is_none()
            {
                return Err("native property name or value is missing or invalid".into());
            }
        } else if !row["action"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name.chars().count() <= 128)
            || !row["pressed"].is_boolean()
        {
            return Err("native input action or pressed state is invalid".into());
        }
    }
    Ok(rows.len())
}
#[cfg(windows)]
fn execute_mutation(
    preview: &crate::native_preview::NativePreview,
    input: &Value,
    operation: &str,
) -> Result<Value, String> {
    let count = validate_mutation(input, operation)?;
    if preview.status()["running"] != true {
        return Err("native preview is not running".into());
    }
    let (request, count_key) = if operation == "native_apply" {
        (
            serde_json::json!({"op":"apply","changes":input["properties"]}),
            "applied",
        )
    } else {
        (
            serde_json::json!({"op":"input","events":input["input_actions"]}),
            "events",
        )
    };
    let context = crate::operation_context::OperationContext::current();
    if let Some(context) = &context {
        context.check().map_err(|error| error.to_string())?;
        context.phase(crate::operation_context::Phase::Publishing);
    }
    let guard = context
        .as_ref()
        .map(|context| context.commit_barrier())
        .transpose()
        .map_err(|error| error.to_string())?;
    let result = preview.request(&request, std::time::Duration::from_secs(5))?;
    if result["ok"] == true && result[count_key].as_u64() == Some(count as u64) {
        if let Some(guard) = guard {
            guard.finish();
        }
    } else if let Some(context) = context {
        // A response/timeout alone cannot certify that live script callbacks
        // left no effects. Keep the operation outcome explicitly unverified.
        context.mark_recovery_required();
    }
    if result["ok"] == true && result[count_key].as_u64() != Some(count as u64) {
        return Err("native mutation response did not confirm the complete batch".into());
    }
    Ok(result)
}
impl NativeRuntime {
    /// IO ownership only. Source/publication authority remains separately checked.
    #[cfg(windows)]
    pub(crate) fn managed_native_output(
        &self,
        root: &Path,
    ) -> Result<crate::worker_windows::StreamController, String> {
        if self.closed.load(Ordering::Acquire) || !self.is_controller_for(root) {
            return Err("managed output requires the same live project controller".into());
        }
        let controller = self.workspace_access.managed_compiler_controller()?;
        self.check_managed_native_output(root, &controller)?;
        Ok(controller)
    }
    #[cfg(windows)]
    pub(crate) fn check_managed_native_output(
        &self,
        root: &Path,
        controller: &crate::worker_windows::StreamController,
    ) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) || !self.is_controller_for(root) {
            return Err("managed output requires the same live project controller".into());
        }
        controller
            .check_parent(&self.workspace_access)
            .map_err(|error| error.to_string())
    }
    /// Test-only borrow from the actual issuer. Never adopt a caller's broker.
    #[cfg(all(windows, test))]
    pub(crate) fn original_managed_output_for_test(
        &self,
        root: &Path,
    ) -> Result<
        (
            &Arc<crate::workspace_access::Broker>,
            crate::worker_windows::StreamController,
        ),
        String,
    > {
        let controller = self.managed_native_output(root)?;
        self.check_managed_native_output(root, &controller)?;
        Ok((&self.workspace_access, controller))
    }
    pub(crate) fn workspace_access(
        &self,
        root: &Path,
        input: &Value,
        write_authorized: bool,
    ) -> Result<Value, String> {
        if !write_authorized && input["op"] != "workspace_access_status" {
            return Err("workspace action requires write permission".into());
        }
        crate::workspace_access::preflight(input)?;
        if self.closed.load(Ordering::Acquire) || !self.is_controller_for(root) {
            return Err("workspace access requires the same live project controller".into());
        }
        match input["op"]
            .as_str()
            .ok_or("invalid workspace access operation")?
        {
            "workspace_access_request" => {
                crate::Project::open(root).map_err(|_| "imported project unavailable")?;
                self.workspace_access.request(root)
            }
            "workspace_access_status" => self.workspace_access.status(),
            "workspace_access_cancel" => self.workspace_access.cancel(
                input["request_id"]
                    .as_str()
                    .ok_or("invalid request identity")?,
            ),
            "external_workspace_validate" => {
                let permission = if let Some(selection) = input.get("gdextension_manifest") {
                    Some(ConfigFileParsePermission {
                        runtime: self,
                        root: root.to_path_buf(),
                        manifest_root: selection["root_id"]
                            .as_str()
                            .ok_or("configfile_parser_selection")?
                            .to_owned(),
                        manifest_path: selection["path"]
                            .as_str()
                            .ok_or("configfile_parser_selection")?
                            .to_owned(),
                    })
                } else {
                    None
                };
                self.workspace_access
                    .validate_with_parser(root, input, permission.as_ref())
            }
            _ => Err("unknown workspace access operation".into()),
        }
    }

    pub fn controller(root: &Path) -> Self {
        let mut runtime = Self::default();
        runtime.controller_root = Some(root.to_path_buf());
        runtime
    }
    pub fn is_controller_for(&self, root: &Path) -> bool {
        self.controller_root
            .as_ref()
            .is_some_and(|owned| crate::ownership::paths_equal(owned, root))
    }
    pub fn job_manager(
        &self,
        root: &Path,
        create: bool,
    ) -> Result<Option<Arc<crate::jobs::JobManager>>, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("project controller is shutting down".into());
        }
        if !self.is_controller_for(root) {
            return Err("background jobs require their project controller".into());
        }
        let root = root
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|error| error.to_string())?;
        let mut managers = self
            .jobs
            .lock()
            .map_err(|_| "job controller is unavailable")?;
        if let Some(manager) = managers.get(&root) {
            return Ok(Some(Arc::clone(manager)));
        }
        if !create {
            return Ok(None);
        }
        let directory = crate::files::confined(&root, ".aurum/background-jobs")?;
        crate::diagnostics::checked_path(&directory).map_err(|error| error.to_string())?;
        let manager = Arc::new(crate::jobs::JobManager::new(Some(directory))?);
        if self.closed.load(Ordering::Acquire) {
            manager.shutdown();
            return Err("project controller closed while preparing jobs".into());
        }
        managers.insert(root, Arc::clone(&manager));
        Ok(Some(manager))
    }
    pub fn knows_project(&self, root: &Path) -> bool {
        #[cfg(not(windows))]
        {
            let _ = root;
            false
        }
        #[cfg(windows)]
        {
            root.canonicalize().ok().is_some_and(|root| {
                self.projects.lock().is_ok_and(|projects| {
                    projects.contains_key(&PathBuf::from(
                        crate::project::clean_path(root)
                            .to_string_lossy()
                            .to_lowercase(),
                    ))
                })
            })
        }
    }
    pub fn execute(&self, root: &Path, input: &Value, read_only: bool) -> Result<Value, String> {
        let op = input["op"]
            .as_str()
            .ok_or("native operation must be a string")?;
        if !supported(op) {
            return Err("unknown native operation".into());
        }
        if read_only && !matches!(op, "native_status" | "native_inspect" | "runtime_context") {
            return Err(format!("{op} requires write permission"));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err("native runtime is shutting down".into());
        }
        let context_input = if op == "runtime_context" {
            Some(crate::runtime_context::operation_request(input)?)
        } else {
            None
        };
        #[cfg(not(windows))]
        {
            if let Some(input) = context_input.as_ref() {
                return crate::runtime_context::inspect(
                    root,
                    input,
                    crate::runtime_context::Session::Unavailable,
                );
            }
            let _ = root;
            Err("managed native sessions are currently Windows-qualified".into())
        }
        #[cfg(windows)]
        {
            let root = root
                .canonicalize()
                .map(crate::project::clean_path)
                .map_err(|error| error.to_string())?;
            let key = PathBuf::from(root.to_string_lossy().to_lowercase());
            let controller = {
                let mut projects = self
                    .projects
                    .lock()
                    .map_err(|_| "native project registry is unavailable")?;
                if let Some(controller) = projects.get(&key) {
                    Some(controller.clone())
                } else if matches!(op, "native_start" | "native_recover") {
                    if projects.len() >= 16 {
                        return Err(
                            "native runtime project registry reached its 16-project bound".into(),
                        );
                    }
                    let controller = Arc::new(Controller::default());
                    projects.insert(key, controller.clone());
                    Some(controller)
                } else {
                    None
                }
            };
            let Some(controller) = controller else {
                if let Some(input) = context_input.as_ref() {
                    return crate::runtime_context::inspect(
                        &root,
                        input,
                        crate::runtime_context::Session::Unavailable,
                    );
                }
                if op == "native_status" {
                    return Ok(json!({"ok":true,"running":false,"busy":false,"host_owned":true}));
                }
                return Err("no native preview is running in this host session".into());
            };
            if op == "native_status" {
                let mut status = controller
                    .status
                    .lock()
                    .map_err(|_| "native status is unavailable")?
                    .clone();
                if !status.is_object() {
                    status = json!({});
                }
                let owner = controller
                    .owner
                    .lock()
                    .map_err(|_| "native ownership is unavailable")?
                    .clone();
                status["ok"] = json!(true);
                status["busy"] = json!(controller.busy.load(Ordering::Acquire));
                status["running"] = json!(owner
                    .is_some_and(|owner| crate::ownership::inspect(owner.pid)
                        .is_some_and(|live| owner.describes(&live))));
                return Ok(status);
            }
            struct Busy<'a>(&'a AtomicBool);
            impl Drop for Busy<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            if let Some(input) = context_input.as_ref() {
                let mut slot = match controller.preview.try_lock() {
                    Ok(slot) => slot,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        return crate::runtime_context::inspect(
                            &root,
                            input,
                            crate::runtime_context::Session::Busy,
                        )
                    }
                    Err(_) => return Err("native preview lock is unavailable".into()),
                };
                let Some(preview) = slot.as_ref() else {
                    return crate::runtime_context::inspect(
                        &root,
                        input,
                        crate::runtime_context::Session::Unavailable,
                    );
                };
                let preparation = preview.context_preparation();
                controller.busy.store(true, Ordering::Release);
                let _busy = Busy(&controller.busy);
                let query =
                    |request: &Value, budget: std::time::Duration| preview.request(request, budget);
                let result = crate::runtime_context::inspect(
                    &root,
                    input,
                    crate::runtime_context::Session::Ready {
                        preparation: &preparation,
                        query: &query,
                    },
                );
                if self.closed.load(Ordering::Acquire) {
                    slot.take();
                    controller
                        .lease
                        .lock()
                        .map_err(|_| "native host lease is unavailable")?
                        .take();
                    return Err("native runtime shut down during context inspection".into());
                }
                return result;
            }
            let mut slot = controller.preview.try_lock().map_err(|_| {
                "native preview has an active operation; query native_status and retry"
            })?;
            controller.busy.store(true, Ordering::Release);
            let _busy = Busy(&controller.busy);
            let result = (|| -> Result<Value, String> {
                match op {
                    "native_start" | "native_recover" => {
                        if slot
                            .as_ref()
                            .is_some_and(|preview| preview.status()["running"] == true)
                        {
                            return Err("native preview already runs; use native_replace for compatible changes".into());
                        }
                        slot.take();
                        let mut lease = controller
                            .lease
                            .lock()
                            .map_err(|_| "native host lease is unavailable")?;
                        if lease.is_none() {
                            *lease = Some(host_lease(&root)?);
                        }
                        let rendered = input
                            .get("rendered")
                            .map(|value| value.as_bool().ok_or("rendered must be a boolean"))
                            .transpose()?
                            .unwrap_or(false);
                        let project = Project::open(&root).map_err(|error| error.to_string())?;
                        if op == "native_recover" {
                            let (preview, receipt) =
                                crate::native_preview::NativePreview::recover_with_runtime(
                                    &project,
                                    Some(self),
                                )?;
                            if self.closed.load(Ordering::Acquire) {
                                return Err("native host closed during recovery".into());
                            }
                            *slot = Some(preview);
                            return Ok(receipt);
                        }
                        let preview = crate::native_preview::NativePreview::prepare_with_runtime(
                            &project,
                            !rendered,
                            Some(self),
                        )?;
                        if self.closed.load(Ordering::Acquire) {
                            return Err("native host closed during preparation".into());
                        }
                        let _content = crate::content_lock::ContentLock::shared(&root)?;
                        if !preview.source_still_current(&project) {
                            return Err(
                                "source changed during preparation; private run was not activated"
                                    .into(),
                            );
                        }
                        let context = crate::operation_context::OperationContext::current();
                        if let Some(context) = &context {
                            context.phase(crate::operation_context::Phase::Activating);
                        }
                        let guard = context
                            .as_ref()
                            .map(|context| context.commit_barrier())
                            .transpose()
                            .map_err(|error| error.to_string())?;
                        let activated = preview.activate()?;
                        if activated["ok"] != true {
                            return Err("native preview activation failed".into());
                        }
                        let mut result = preview.status();
                        result["ok"] = json!(true);
                        result["started"] = json!(true);
                        *slot = Some(preview);
                        if let Some(guard) = guard {
                            guard.finish();
                        }
                        Ok(result)
                    }
                    "native_stop" => {
                        let Some(preview) = slot.as_mut() else {
                            controller
                                .lease
                                .lock()
                                .map_err(|_| "native lease is unavailable")?
                                .take();
                            return Ok(json!({"ok":true,"stopped":false}));
                        };
                        let context = crate::operation_context::OperationContext::current();
                        if let Some(context) = &context {
                            context.check().map_err(|error| error.to_string())?;
                            context.phase(crate::operation_context::Phase::Cleanup);
                        }
                        let guard = context
                            .as_ref()
                            .map(|context| context.commit_barrier())
                            .transpose()
                            .map_err(|error| error.to_string())?;
                        preview.stop()?;
                        if let Some(guard) = guard {
                            guard.finish();
                        }
                        slot.take();
                        controller
                            .lease
                            .lock()
                            .map_err(|_| "native lease is unavailable")?
                            .take();
                        Ok(json!({"ok":true,"stopped":true}))
                    }
                    _ => {
                        let preview = slot
                            .as_mut()
                            .ok_or("no native preview is running in this host")?;
                        match op {
                            "native_inspect" => preview.request(&json!({"op":"inspect","path":input.get("node_path").cloned().unwrap_or(json!(".")),"include_state":input.get("include_state").cloned().unwrap_or(json!(false))}),std::time::Duration::from_secs(5)),
                            "native_apply" | "native_input" => execute_mutation(preview, input, op),
                            "native_checkpoint" => preview.checkpoint(&Project::open(&root).map_err(|error|error.to_string())?),
                            "native_replace" => preview.replace_with_runtime(&Project::open(&root).map_err(|error|error.to_string())?, Some(self)),
                            _ => Err("unknown native operation".into()),
                        }
                    }
                }
            })();
            if self.closed.load(Ordering::Acquire) {
                slot.take();
                controller
                    .lease
                    .lock()
                    .map_err(|_| "native lease is unavailable")?
                    .take();
            }
            if let Some(preview) = slot.as_ref() {
                *controller
                    .status
                    .lock()
                    .map_err(|_| "native status is unavailable")? = preview.status();
                *controller
                    .owner
                    .lock()
                    .map_err(|_| "native ownership is unavailable")? = Some(preview.owner());
            } else {
                *controller
                    .status
                    .lock()
                    .map_err(|_| "native status is unavailable")? = json!({"host_owned":true});
                *controller
                    .owner
                    .lock()
                    .map_err(|_| "native ownership is unavailable")? = None;
            }
            result
        }
    }
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.workspace_access.close();
        let managers = self
            .jobs
            .lock()
            .map(|jobs| jobs.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for manager in managers {
            manager.shutdown();
        }
        #[cfg(windows)]
        if let Ok(projects) = self.projects.lock() {
            for controller in projects.values() {
                if let Ok(mut slot) = controller.preview.try_lock() {
                    slot.take();
                }
            }
        }
    }
}
impl Drop for NativeRuntime {
    fn drop(&mut self) {
        self.close();
    }
}
#[cfg(windows)]
fn host_lease(root: &Path) -> Result<std::fs::File, String> {
    let directory = crate::files::confined(root, ".aurum/native")?;
    crate::diagnostics::checked_path(&directory).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join("game.lock");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.try_lock()
        .map_err(|_| "another Aurum host owns this native project; connect to that host session")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(windows)]
    #[test]
    fn malformed_short_mutations_fail_before_any_commit_barrier() {
        let context = crate::operation_context::OperationContext::new(
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        );
        context
            .scope(|| {
                assert!(validate_mutation(&json!({"properties":[]}), "native_apply").is_err());
                assert!(validate_mutation(
                    &json!({"properties":[{"path":"../foreign","property":"speed","value":2}]}),
                    "native_apply"
                )
                .is_err());
                assert!(validate_mutation(
                    &json!({"input_actions":[{"action":"jump","pressed":"yes"}]}),
                    "native_input"
                )
                .is_err());
                assert_eq!(
                    validate_mutation(
                        &json!({"properties":[{"path":".","property":"speed","value":2}]}),
                        "native_apply"
                    )
                    .unwrap(),
                    1
                );
                assert_eq!(
                    validate_mutation(
                        &json!({"input_actions":[{"action":"jump","pressed":true}]}),
                        "native_input"
                    )
                    .unwrap(),
                    1
                );
            })
            .unwrap();
        assert!(!context.committed());
        assert!(context.commit_status_verified());
    }
    #[test]
    fn native_permissions_are_checked_before_creating_state() {
        let root = std::env::temp_dir().join(format!(
            "aurum-native-permission-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let runtime = NativeRuntime::default();
        assert!(runtime
            .execute(&root, &json!({"op":"native_start"}), true)
            .unwrap_err()
            .contains("permission"));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        assert!(!runtime.knows_project(&root));
        runtime.close();
        assert!(runtime
            .execute(&root, &json!({"op":"native_status"}), true)
            .is_err());
        std::fs::remove_dir(&root).unwrap();
    }
    #[test]
    fn configfile_parser_read_only_and_foreign_broker_refuse_before_allocation() {
        let root =
            std::env::temp_dir().join(format!("absent-parser-{}", crate::random::session_id()));
        let runtime = NativeRuntime::controller(&root);
        let input = json!({"op":"external_workspace_validate","gdextension_manifest":{}});
        assert!(
            crate::project_ops::execute_in_runtime(&root, &input, true, &runtime)
                .unwrap_err()
                .contains("write permission")
        );
        assert!(runtime
            .workspace_access(&root, &input, false)
            .unwrap_err()
            .contains("write permission"));
        let permission = ConfigFileParsePermission {
            runtime: &runtime,
            root: root.clone(),
            manifest_root: "project".into(),
            manifest_path: "bin/checked.gdextension".into(),
        };
        let foreign = crate::workspace_access::Broker::default();
        assert!(permission.check_broker(&foreign).is_err());
        permission.check_broker(&runtime.workspace_access).unwrap();
        assert!(permission
            .check_manifest("project", "bin/other.gdextension")
            .is_err());
        runtime.close();
        assert!(permission.check_broker(&runtime.workspace_access).is_err());
        assert!(!root.exists());
    }
    #[cfg(windows)]
    #[test]
    fn original_managed_output_test_borrow_uses_original_issuer_and_preserves_root() {
        let root = std::env::temp_dir().join(format!(
            "original-managed-output-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let sentinel = root.join("unchanged.bin");
        std::fs::write(&sentinel, b"actual original runtime IO owner, no process").unwrap();
        let before = std::fs::read(&sentinel).unwrap();
        let runtime = NativeRuntime::controller(&root);
        assert!(runtime
            .original_managed_output_for_test(&root.join("foreign"))
            .is_err());
        let (broker, controller) = runtime.original_managed_output_for_test(&root).unwrap();
        assert!(Arc::ptr_eq(broker, &runtime.workspace_access));
        controller.check_parent(broker).unwrap();
        runtime
            .check_managed_native_output(&root, &controller)
            .unwrap();
        assert_eq!(controller.retained(), 0);
        runtime.close();
        assert!(!broker.managed_output_is_open());
        assert!(runtime.original_managed_output_for_test(&root).is_err());
        assert_eq!(controller.retained(), 0);
        assert_eq!(std::fs::read(&sentinel).unwrap(), before);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        drop(controller);
        drop(runtime);
        std::fs::remove_file(sentinel).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn absent_status_does_not_allocate_a_controller_or_project_state() {
        let root = std::env::temp_dir().join(format!(
            "aurum-native-status-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let runtime = NativeRuntime::default();
        assert_eq!(
            runtime
                .execute(&root, &json!({"op":"native_status"}), true)
                .unwrap()["running"],
            false
        );
        assert!(!runtime.knows_project(&root));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir(&root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn runtime_context_distinguishes_absent_controller_and_busy_without_creating_state() {
        let root = std::env::temp_dir().join(format!(
            "aurum-runtime-context-routing-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().map(crate::project::clean_path).unwrap();
        let runtime = NativeRuntime::default();
        let request = json!({"op":"runtime_context","project":root});
        assert_eq!(
            runtime.execute(&root, &request, true).unwrap()["state"],
            "unavailable"
        );
        assert!(!runtime.knows_project(&root));
        let controller = Arc::new(Controller::default());
        runtime.projects.lock().unwrap().insert(
            PathBuf::from(root.to_string_lossy().to_lowercase()),
            controller.clone(),
        );
        let held = controller.preview.lock().unwrap();
        assert_eq!(
            runtime.execute(&root, &request, true).unwrap()["state"],
            "busy"
        );
        drop(held);
        assert_eq!(
            runtime.execute(&root, &request, true).unwrap()["state"],
            "unavailable"
        );
        assert!(runtime
            .execute(
                &root,
                &json!({"op":"runtime_context","checkpoint":true}),
                true
            )
            .is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        runtime.close();
        std::fs::remove_dir(&root).unwrap();
    }
}
