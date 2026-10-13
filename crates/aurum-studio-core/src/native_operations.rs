//! Persistent protocol-session ownership for native project operations.
#[cfg(windows)]
use crate::Project;
#[cfg(windows)]
use serde_json::json;
use serde_json::Value;
#[cfg(windows)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(windows)]
use std::sync::{Arc, Mutex};

pub const OPERATIONS: &[&str] = &[
    "native_start",
    "native_recover",
    "native_status",
    "native_inspect",
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
    #[cfg(windows)]
    projects: Mutex<BTreeMap<PathBuf, Arc<Controller>>>,
    closed: AtomicBool,
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
impl NativeRuntime {
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
        if read_only && !matches!(op, "native_status" | "native_inspect") {
            return Err(format!("{op} requires write permission"));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err("native runtime is shutting down".into());
        }
        #[cfg(not(windows))]
        {
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
            let mut slot = controller.preview.try_lock().map_err(|_| {
                "native preview has an active operation; query native_status and retry"
            })?;
            controller.busy.store(true, Ordering::Release);
            struct Busy<'a>(&'a AtomicBool);
            impl Drop for Busy<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
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
                                crate::native_preview::NativePreview::recover(&project)?;
                            if self.closed.load(Ordering::Acquire) {
                                return Err("native host closed during recovery".into());
                            }
                            *slot = Some(preview);
                            return Ok(receipt);
                        }
                        let preview =
                            crate::native_preview::NativePreview::prepare(&project, !rendered)?;
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
                        let activated = preview.activate()?;
                        if activated["ok"] != true {
                            return Err("native preview activation failed".into());
                        }
                        let mut result = preview.status();
                        result["ok"] = json!(true);
                        result["started"] = json!(true);
                        *slot = Some(preview);
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
                        preview.stop()?;
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
                            "native_apply" => preview.request(&json!({"op":"apply","changes":input.get("properties").ok_or("properties are required")?}),std::time::Duration::from_secs(5)),
                            "native_input" => preview.request(&json!({"op":"input","events":input.get("input_actions").ok_or("input_actions are required")?}),std::time::Duration::from_secs(5)),
                            "native_checkpoint" => preview.checkpoint(&Project::open(&root).map_err(|error|error.to_string())?),
                            "native_replace" => preview.replace(&Project::open(&root).map_err(|error|error.to_string())?),
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
}
