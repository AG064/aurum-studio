//! Attended, session-only external dependency access. Ordinary authorization
//! may propose or consume a read grant, never construct an owner confirmation.
//! Same-user arbitrary native code is outside this API authority boundary.
#[cfg(any(windows, test))]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(any(windows, test))]
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(any(windows, test))]
use std::time::Duration;
use std::time::Instant;

#[cfg(any(windows, test))]
const FRAME_BYTES: usize = 16 * 1024;
#[cfg(any(windows, test))]
const PENDING_SECONDS: u64 = 90;
pub const OPERATIONS: &[&str] = &[
    "workspace_access_request",
    "workspace_access_status",
    "workspace_access_cancel",
    "external_workspace_validate",
];
pub fn supported(op: &str) -> bool {
    OPERATIONS.contains(&op)
}

fn fields(input: &Value, allowed: &[&str]) -> Result<(), String> {
    if input
        .as_object()
        .ok_or("workspace access request must be an object")?
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err("workspace access request contains an unsupported field".into());
    }
    Ok(())
}
pub(crate) fn preflight(input: &Value) -> Result<(), String> {
    match input["op"]
        .as_str()
        .ok_or("workspace access operation must be a string")?
    {
        "workspace_access_request" => {
            fields(input, &["op", "project", "mode"])?;
            if input["mode"] != "dependency_read" {
                return Err("external source publication is not implemented; only dependency_read intake is supported".into());
            }
        }
        "workspace_access_status" => fields(input, &["op", "project"])?,
        "workspace_access_cancel" => fields(input, &["op", "project", "request_id"])?,
        "external_workspace_validate" => {
            fields(
                input,
                &[
                    "op",
                    "project",
                    "root_ids",
                    "placements",
                    "workspace",
                    "member",
                    "compile_native",
                    "gdextension_manifest",
                ],
            )?;
            if input
                .get("compile_native")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err("compile_native must be a boolean".into());
            }
            if let Some(selection) = input.get("gdextension_manifest") {
                fields(selection, &["root_id", "path"])?;
                if selection.as_object().is_none_or(|row| row.len() != 2)
                    || input["compile_native"].as_bool() != Some(true)
                {
                    return Err(
                        "gdextension_manifest requires closed selection and compile_native:true"
                            .into(),
                    );
                }
                let id = selection["root_id"]
                    .as_str()
                    .ok_or("manifest root_id must be a string")?;
                let path = selection["path"]
                    .as_str()
                    .ok_or("manifest path must be a string")?;
                if !relative(path)
                    || !path.ends_with(".gdextension")
                    || (id != "project"
                        && !input["root_ids"]
                            .as_array()
                            .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(id))))
                {
                    return Err("invalid gdextension_manifest selected root or path".into());
                }
            }
            let ids = input["root_ids"]
                .as_array()
                .filter(|ids| !ids.is_empty() && ids.len() <= 7)
                .ok_or("root_ids must select 1..7 existing read grants")?;
            let mut seen = std::collections::BTreeSet::new();
            for id in ids {
                let id = id.as_str().ok_or("root_id must be a string")?;
                if !valid_id(id) || !seen.insert(id) {
                    return Err("invalid or repeated read grant identity".into());
                }
            }
            let placements = input["placements"]
                .as_array()
                .filter(|rows| rows.len() == ids.len() + 1)
                .ok_or("placements must contain project and each selected root exactly once")?;
            let mut roots = std::collections::BTreeSet::new();
            for row in placements {
                fields(row, &["root_id", "slot"])?;
                let id = row["root_id"]
                    .as_str()
                    .ok_or("placement root_id must be a string")?;
                let slot = row["slot"]
                    .as_str()
                    .ok_or("placement slot must be a string")?;
                if (id != "project" && !seen.contains(id)) || !roots.insert(id) || !relative(slot) {
                    return Err("invalid or undeclared root placement".into());
                }
            }
            for name in ["workspace", "member"] {
                let row = &input[name];
                fields(row, &["root_id", "path"])?;
                let id = row["root_id"]
                    .as_str()
                    .ok_or("manifest root_id must be a string")?;
                if !seen.contains(id)
                    || !relative(
                        row["path"]
                            .as_str()
                            .ok_or("manifest path must be a string")?,
                    )
                {
                    return Err(
                        "external manifest must use a selected root identity and relative path"
                            .into(),
                    );
                }
            }
        }
        _ => return Err("unknown workspace access operation".into()),
    }
    if input["op"] == "workspace_access_cancel"
        && !input["request_id"].as_str().is_some_and(valid_id)
    {
        return Err("invalid attended request identity".into());
    }
    Ok(())
}
fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.contains(':')
        && !value.contains('\\')
        && value.split('/').count() <= 24
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}
#[cfg(any(windows, test))]
fn write_frame(output: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "owner frame serialization refused")?;
    if bytes.is_empty() || bytes.len() > FRAME_BYTES {
        return Err("owner frame exceeds its bound".into());
    }
    output
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|()| output.write_all(&bytes))
        .and_then(|()| output.flush())
        .map_err(|_| "owned owner channel write failed".into())
}
#[cfg(any(windows, test))]
fn read_frame<T: for<'de> Deserialize<'de>>(input: &mut impl Read) -> Result<T, String> {
    let mut prefix = [0; 4];
    input
        .read_exact(&mut prefix)
        .map_err(|_| "owned owner channel closed")?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > FRAME_BYTES {
        return Err("owner frame exceeds its bound".into());
    }
    let mut body = vec![0; length];
    input
        .read_exact(&mut body)
        .map_err(|_| "owned owner frame incomplete")?;
    serde_json::from_slice(&body).map_err(|_| "owner frame is invalid".into())
}
#[cfg(windows)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    version: String,
    parent_pid: u32,
    parent_started: String,
    parent_sha256: String,
    helper_sha256: String,
    nonce: String,
}
#[cfg(windows)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selected {
    version: String,
    nonce: String,
    path: Option<String>,
}
#[cfg(windows)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Display {
    version: String,
    nonce: String,
    digest: String,
    project: String,
    canonical_path: String,
    permission: String,
}
#[cfg(any(windows, test))]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    version: String,
    nonce: String,
    digest: String,
    approved: bool,
}

// No Deserialize, public constructor, Clone or Debug for this issuance proof.
#[cfg(any(windows, test))]
pub(crate) struct OwnerConfirmation {
    digest: String,
}
#[cfg(any(windows, test))]
impl OwnerConfirmation {
    pub(crate) fn matches(&self, digest: &str) -> bool {
        self.digest == digest
    }
}
#[cfg(all(test, windows))]
pub(crate) fn simulated_private_confirmation_for_test(digest: &str) -> OwnerConfirmation {
    OwnerConfirmation {
        digest: digest.into(),
    }
}
#[cfg(any(windows, test))]
struct Challenge {
    nonce: Option<String>,
    digest: String,
    expires: Instant,
}
#[cfg(any(windows, test))]
impl Challenge {
    fn consume(
        &mut self,
        decision: Decision,
        cancelled: bool,
        now: Instant,
    ) -> Result<OwnerConfirmation, String> {
        // Every attempted decision consumes the challenge, including forged/replayed input.
        let expected = self
            .nonce
            .take()
            .ok_or("owner confirmation already consumed")?;
        if cancelled || now >= self.expires {
            return Err("owner confirmation expired or cancelled".into());
        }
        if decision.version != env!("CARGO_PKG_VERSION")
            || decision.nonce != expected
            || decision.digest != self.digest
        {
            return Err("owner confirmation binding mismatch".into());
        }
        if !decision.approved {
            return Err("owner cancelled external dependency access".into());
        }
        Ok(OwnerConfirmation {
            digest: self.digest.clone(),
        })
    }
}

#[cfg(any(windows, test))]
const PIPE_EOF_BYTES: usize = 32 * 1024;
/// Read at most32KiB plus exactly one overflow-detection byte. Raw bytes are
/// discarded; only actual EOF within the cap is affirmative closure evidence.
#[cfg(any(windows, test))]
fn read_pipe_eof_capped(pipe: &mut impl Read) -> bool {
    let mut bytes = [0; 4096];
    let mut total = 0;
    loop {
        let remaining = PIPE_EOF_BYTES - total;
        let requested = bytes.len().min(remaining + 1);
        match pipe.read(&mut bytes[..requested]) {
            Ok(0) => return true,
            Ok(count) if count <= remaining => total += count,
            Ok(_) => return false,
            Err(error) if error.raw_os_error() == Some(109) => return true,
            Err(_) => return false,
        }
    }
}

#[cfg(any(windows, test))]
const MAX_NORMAL_PIPE_WORKERS: usize = 6;
#[cfg(any(windows, test))]
const MAX_RETIREMENT_PIPE_WORKERS: usize = 2;
/// Only this helper's original pipe workers are tracked. This is not the
/// validation worker's complete descendant-process census.
#[cfg(any(windows, test))]
#[derive(Default)]
struct PipeWorkers {
    handles: Vec<std::thread::JoinHandle<()>>,
    normal_started: usize,
    retirement_started: usize,
    closing: bool,
    normal_retired: bool,
    unconfirmed: bool,
}
#[cfg(any(windows, test))]
impl PipeWorkers {
    fn start(&mut self, name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), ()> {
        if self.closing || self.unconfirmed || self.normal_started >= MAX_NORMAL_PIPE_WORKERS {
            return Err(());
        }
        self.normal_started += 1;
        self.spawn(name, work)
    }
    fn start_retirement(
        &mut self,
        name: &str,
        deadline: Instant,
        work: impl FnOnce() + Send + 'static,
    ) -> Result<(), ()> {
        if Instant::now() >= deadline {
            self.unconfirmed = true;
            return Err(());
        }
        if !self.closing
            || !self.normal_retired
            || self.unconfirmed
            || self.retirement_started >= MAX_RETIREMENT_PIPE_WORKERS
        {
            return Err(());
        }
        self.retirement_started += 1;
        self.spawn(name, work)
    }
    fn spawn(&mut self, name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), ()> {
        let handle = std::thread::Builder::new()
            .name(name.into())
            .spawn(work)
            .map_err(|_| ())?;
        self.handles.push(handle);
        Ok(())
    }
    fn join_ready(&mut self, deadline: Instant) {
        for index in (0..self.handles.len()).rev() {
            if Instant::now() >= deadline {
                self.unconfirmed = true;
                return;
            }
            match pipe_thread_ready(&self.handles[index]) {
                Ok(true) => {
                    if Instant::now() >= deadline {
                        self.unconfirmed = true;
                        return;
                    }
                    let handle = self.handles.swap_remove(index);
                    if handle.join().is_err() || Instant::now() >= deadline {
                        self.unconfirmed = true;
                    }
                }
                Ok(false) => {}
                Err(()) => self.unconfirmed = true,
            }
        }
    }
    fn complete(
        &mut self,
        deadline: Instant,
        cancel_pending: bool,
        cancel: impl Fn(&std::thread::JoinHandle<()>) -> Result<(), ()>,
    ) -> Result<(), ()> {
        self.closing = true;
        if self.unconfirmed || Instant::now() >= deadline {
            self.unconfirmed = true;
            return Err(());
        }
        self.join_ready(deadline);
        if cancel_pending {
            for handle in &self.handles {
                // Cancellation is a request, never completion evidence.
                if cancel(handle).is_err() {
                    self.unconfirmed = true;
                }
            }
        }
        while !self.handles.is_empty() && Instant::now() < deadline && !self.unconfirmed {
            self.join_ready(deadline);
            if !self.handles.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        self.join_ready(deadline);
        if !self.handles.is_empty() {
            // One bounded original-thread cancellation attempt at this boundary.
            // Do not retry retirement or permit a later helper admission.
            if !cancel_pending {
                for handle in &self.handles {
                    let _ = cancel(handle);
                }
            }
            self.unconfirmed = true;
        }
        if self.unconfirmed || Instant::now() >= deadline {
            self.unconfirmed = true;
            return Err(());
        }
        self.normal_retired = true;
        Ok(())
    }
}
#[cfg(any(windows, test))]
fn pipe_thread_ready(handle: &std::thread::JoinHandle<()>) -> Result<bool, ()> {
    #[cfg(windows)]
    {
        windows::pipe_thread_ready(handle)
    }
    #[cfg(not(windows))]
    {
        Ok(handle.is_finished())
    }
}
#[cfg(any(windows, test))]
impl Drop for PipeWorkers {
    fn drop(&mut self) {
        if !self.handles.is_empty() {
            // Retain original ownership objects until host exit. Dropping these
            // JoinHandles would detach still-blocked workers without evidence.
            std::mem::forget(std::mem::take(&mut self.handles));
        }
    }
}

#[derive(Default)]
pub(crate) struct Broker {
    closed: AtomicBool,
    unconfirmed: AtomicBool,
    state: Mutex<State>,
    #[cfg(windows)]
    active: Mutex<Option<std::thread::JoinHandle<()>>>,
    #[cfg(windows)]
    compiler: Mutex<Option<crate::worker_windows::StreamController>>,
}
#[derive(Default)]
struct State {
    pending: Option<Pending>,
    last: Option<(String, &'static str)>,
    #[cfg(windows)]
    grants: std::collections::BTreeMap<String, crate::external_workspace::VerifiedGrant>,
}
struct Pending {
    id: String,
    expires: Instant,
    cancel: Arc<AtomicBool>,
    phase: &'static str,
}
impl std::fmt::Debug for Broker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkspaceAccess { private authority: redacted }")
    }
}
impl Broker {
    #[cfg(windows)]
    pub(crate) fn mark_parser_unconfirmed(&self) {
        self.unconfirmed.store(true, Ordering::Release);
    }
    #[cfg(windows)]
    pub(crate) fn managed_output_is_open(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && !self.unconfirmed.load(Ordering::Acquire)
    }
    /// Private same-runtime caller supplies the actual parent Arc, never tool input.
    #[cfg(windows)]
    pub(crate) fn managed_compiler_controller(
        self: &Arc<Self>,
    ) -> Result<crate::worker_windows::StreamController, String> {
        self.check()?;
        let controller = {
            let mut slot = self
                .compiler
                .lock()
                .map_err(|_| "compiler owner unavailable")?;
            self.check()?;
            slot.get_or_insert_with(crate::worker_windows::StreamController::for_runtime)
                .clone()
        };
        controller
            .bind_parent(self)
            .map_err(|_| "compiler parent binding refused")?;
        self.check()?;
        Ok(controller)
    }
    #[cfg(all(windows, test))]
    pub(crate) fn insert_simulated_grants_for_test(
        &self,
        grants: Vec<(String, crate::external_workspace::VerifiedGrant)>,
    ) {
        self.state.lock().unwrap().grants.extend(grants);
    }
    pub(crate) fn status(&self) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "workspace access broker unavailable")?;
        #[cfg(windows)]
        let grants: Vec<_> = state
            .grants
            .values()
            .map(|grant| grant.public_metadata())
            .collect();
        #[cfg(not(windows))]
        let grants: Vec<Value> = Vec::new();
        let pending=state.pending.as_ref().map(|pending|json!({"request_id":pending.id,"state":pending.phase,
            "expires_in_seconds":pending.expires.saturating_duration_since(Instant::now()).as_secs()}));
        Ok(
            json!({"ok":true,"pending":pending,"last":state.last.as_ref().map(|(id,status)|json!({"request_id":id,"state":status})),
            "grants":grants,"permission":"dependency_read","source_publish":false,"session_only":true,
            "attended_approval_required":true,"ordinary_authorization_can_approve":false}),
        )
    }
    pub(crate) fn close(&self) {
        #[cfg(windows)]
        let first_close = Instant::now();
        self.closed.store(true, Ordering::Release);
        #[cfg(windows)]
        let compiler = match self.compiler.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => {
                self.unconfirmed.store(true, Ordering::Release);
                poisoned.into_inner().clone()
            }
        };
        #[cfg(windows)]
        if let Some(controller) = &compiler {
            controller.close_at(first_close);
        }
        if let Ok(state) = self.state.lock() {
            if let Some(pending) = &state.pending {
                pending.cancel.store(true, Ordering::Release);
            }
            #[cfg(windows)]
            {
                let mut state = state;
                state.grants.clear();
            }
        }
        #[cfg(windows)]
        if let Ok(mut slot) = self.active.lock() {
            if let Some(handle) = slot.take() {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !handle.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                if handle.is_finished() {
                    if handle.join().is_err() {
                        self.unconfirmed.store(true, Ordering::Release);
                    }
                } else {
                    // No guessed process kill or a successful cleanup claim.
                    // The original worker retains its child and file guards.
                    self.unconfirmed.store(true, Ordering::Release);
                    *slot = Some(handle);
                }
            }
        }
        #[cfg(windows)]
        if let Some(controller) = &compiler {
            if controller.reap().is_err() {
                self.unconfirmed.store(true, Ordering::Release);
            }
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            Err("workspace access controller closed".into())
        } else if self.unconfirmed.load(Ordering::Acquire) {
            Err("owned access helper retirement is unconfirmed; intake and staging refused".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn cancel(&self, id: &str) -> Result<Value, String> {
        self.check()?;
        let state = self
            .state
            .lock()
            .map_err(|_| "workspace access broker unavailable")?;
        let pending = state
            .pending
            .as_ref()
            .filter(|pending| pending.id == id)
            .ok_or("no matching pending owner request")?;
        pending.cancel.store(true, Ordering::Release);
        Ok(json!({"ok":true,"cancel_requested":true,"request_id":id,"approved":false}))
    }
    pub(crate) fn request(self: &Arc<Self>, root: &Path) -> Result<Value, String> {
        self.check()?;
        #[cfg(not(windows))]
        {
            let _ = root;
            Err("attended dependency access is currently Windows-only".into())
        }
        #[cfg(windows)]
        {
            // Check the immutable package before reserving a request or showing native UI.
            let helper = windows::OwnedHelper::prepare()?;
            let mut active = self
                .active
                .lock()
                .map_err(|_| "owner helper lifetime unavailable")?;
            if let Some(previous) = active.take() {
                if !previous.is_finished() {
                    *active = Some(previous);
                    return Err("previous owner helper retirement is pending".into());
                }
                if previous.join().is_err() {
                    self.unconfirmed.store(true, Ordering::Release);
                    return Err("previous owner helper retirement is unconfirmed".into());
                }
            }
            let id = windows::random_hex(16)?;
            let cancel = Arc::new(AtomicBool::new(false));
            let deadline = Instant::now() + Duration::from_secs(PENDING_SECONDS);
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "workspace access broker unavailable")?;
                if state.pending.is_some() {
                    return Err("another attended access request is pending".into());
                }
                if state.grants.len() >= 7 {
                    return Err("session external grant limit reached".into());
                }
                self.check()?;
                state.pending = Some(Pending {
                    id: id.clone(),
                    expires: deadline,
                    cancel: cancel.clone(),
                    phase: "selecting",
                });
            }
            let broker = Arc::clone(self);
            let project = root.to_path_buf();
            let request_id = id.clone();
            let worker = std::thread::Builder::new()
                .name("aurum-owner-intake".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        windows::attend(&broker, &project, &request_id, &cancel, deadline, helper)
                    }))
                    .unwrap_or(Err(windows::IntakeFailure::Unconfirmed));
                    if let Ok(mut state) = broker.state.lock() {
                        if state
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.id == request_id)
                        {
                            state.pending = None;
                            let status = match result {
                                Ok(grant)
                                    if !broker.closed.load(Ordering::Acquire)
                                        && !cancel.load(Ordering::Acquire)
                                        && Instant::now() < deadline =>
                                {
                                    state.grants.insert(request_id.clone(), grant);
                                    "granted"
                                }
                                Ok(_) if Instant::now() >= deadline => "expired",
                                Ok(_) => "cancelled",
                                Err(windows::IntakeFailure::Unconfirmed) => {
                                    broker.unconfirmed.store(true, Ordering::Release);
                                    state.grants.clear();
                                    "owned_helper_exit_unconfirmed"
                                }
                                Err(windows::IntakeFailure::Expired) => "expired",
                                Err(windows::IntakeFailure::Cancelled) => "cancelled",
                                Err(_) => "refused",
                            };
                            state.last = Some((request_id, status));
                        }
                    }
                })
                .map_err(|_| {
                    if let Ok(mut state) = self.state.lock() {
                        if state
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.id == id)
                        {
                            state.pending = None;
                        }
                    }
                    "owner intake thread could not start"
                })?;
            *active = Some(worker);
            Ok(
                json!({"ok":true,"request_id":id,"state":"selecting","permission":"dependency_read",
                "attended_approval_required":true,"approved":false,"source_publish":false}),
            )
        }
    }
    #[cfg(test)]
    pub(crate) fn validate(&self, root: &Path, input: &Value) -> Result<Value, String> {
        self.validate_with_parser(root, input, None)
    }
    pub(crate) fn validate_with_parser(
        &self,
        root: &Path,
        input: &Value,
        permission: Option<&crate::native_operations::ConfigFileParsePermission<'_>>,
    ) -> Result<Value, String> {
        self.check()?;
        preflight(input)?;
        if input.get("gdextension_manifest").is_some() {
            permission
                .ok_or("configfile_parser_execution_permission_required")?
                .check_request(self, input)?;
        }
        #[cfg(not(windows))]
        {
            let _ = (root, input);
            Err("confined external workspace staging is currently Windows-only".into())
        }
        #[cfg(windows)]
        {
            use crate::external_workspace::{
                Budget, ImportedProject, ManifestRef, Placement, Revision, RootId, Selection,
                StagingPlan,
            };
            let check = || {
                self.check()?;
                if let Some(context) = crate::operation_context::OperationContext::current() {
                    context.check().map_err(|e| e.to_string())?;
                }
                Ok(())
            };
            let grants = {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| "workspace access broker unavailable")?;
                input["root_ids"]
                    .as_array()
                    .ok_or("missing root identities")?
                    .iter()
                    .map(|id| {
                        let grant = state
                            .grants
                            .get(id.as_str().ok_or("invalid root identity")?)
                            .ok_or("read grant is absent; attended approval required")?;
                        if !grant.is_dependency_read() {
                            return Err("external_source_publish_unimplemented".into());
                        }
                        Ok(grant.clone())
                    })
                    .collect::<Result<Vec<_>, String>>()?
            };
            let compile_native = input["compile_native"].as_bool().unwrap_or(false);
            let parent_deadline =
                Instant::now() + Duration::from_secs(if compile_native { 180 } else { 60 });
            let parent_deadline = crate::operation_context::OperationContext::current()
                .map_or(parent_deadline, |context| {
                    parent_deadline.min(context.deadline())
                });
            let budget = Budget::clipped(parent_deadline, &check)?;
            let parser_owner = if input.get("gdextension_manifest").is_some() {
                let permission =
                    permission.ok_or("configfile_parser_execution_permission_required")?;
                permission.check_broker(self)?;
                let controller = permission.controller()?;
                let mut parent = crate::external_native::ParentDeadline::at(parent_deadline)?;
                parent.bind_runtime_read_owner(
                    permission.broker(),
                    crate::external_workspace::ProposalLimits::from_budget(&budget),
                )?;
                permission.check(&parent, &controller)?;
                Some((parent, controller))
            } else {
                None
            };
            let revision =
                Revision::new(&crate::external_workspace::current_revision(root, &budget)?)?;
            let project = ImportedProject::bind(root, revision.clone(), &budget)?;
            let placements = input["placements"]
                .as_array()
                .ok_or("missing placements")?
                .iter()
                .map(|row| {
                    Ok(Placement {
                        root_id: RootId::new(
                            row["root_id"]
                                .as_str()
                                .ok_or("invalid placement identity")?,
                        )?,
                        slot: row["slot"].as_str().ok_or("invalid placement slot")?.into(),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let reference = |key: &str| -> Result<ManifestRef, String> {
                Ok(ManifestRef {
                    root_id: RootId::new(
                        input[key]["root_id"]
                            .as_str()
                            .ok_or("invalid manifest identity")?,
                    )?,
                    path: input[key]["path"]
                        .as_str()
                        .ok_or("invalid manifest path")?
                        .into(),
                })
            };
            let plan = StagingPlan::resolve(
                &project,
                &grants,
                &placements,
                Selection {
                    workspace: reference("workspace")?,
                    member: reference("member")?,
                },
                &revision,
                &budget,
            )?;
            // The transport never supplies a destination. The recognized pool owns it.
            #[cfg(not(test))]
            let mut workspace = if compile_native {
                crate::candidate_workspace::Workspace::create_for_native_compile()?
            } else {
                crate::candidate_workspace::Workspace::create()?
            };
            #[cfg(test)]
            let mut workspace = if compile_native {
                crate::candidate_workspace::Workspace::for_test_project_native_compile(root)?
            } else {
                crate::candidate_workspace::Workspace::for_test_project(root)?
            };
            if let Some((parent, controller)) = &parser_owner {
                let permission =
                    permission.ok_or("configfile_parser_execution_permission_required")?;
                permission.check(parent, controller)?;
                let selection = reference("gdextension_manifest")?;
                let mut value = crate::external_workspace::validate_configfile_operation(
                    workspace, &plan, &selection, parent, controller, &budget, permission,
                )?;
                value["revision"] = json!(revision.as_str());
                return Ok(value);
            }
            let result = (|| {
                let staged = {
                    let envelope = workspace.source_envelope(budget.deadline())?;
                    plan.stage_owned(&envelope, &budget)?
                };
                let rows: Vec<_> = staged
                    .roots
                    .iter()
                    .map(|row| {
                        let slot=row.staged.strip_prefix(&staged.envelope).map_err(|_|"staged root escaped its recognized envelope")?;
                        if row.source.starts_with(&staged.envelope) {return Err("original root unexpectedly aliases its staged envelope".into());}
                        let permission=match row.mode {
                            crate::external_workspace::Mode::DependencyRead=>"dependency_read",
                            crate::external_workspace::Mode::SourcePublish if row.root_id.as_str()=="project"=>"project_source_read",
                            _=>return Err("external_source_publish_unimplemented".into()),
                        };
                        Ok(json!({"root_id":row.root_id.as_str(),"slot":slot.to_string_lossy().replace('\\',"/"),"permission":permission,"sha256":row.fingerprint.sha256,
                    "files":row.fingerprint.files,"bytes":row.fingerprint.bytes}))
                    })
                    .collect::<Result<Vec<_>,String>>()?;
                let cargo_manifest = staged
                    .cargo_manifest
                    .strip_prefix(&staged.envelope)
                    .map_err(|_| "staged manifest escaped its recognized envelope")?
                    .to_string_lossy()
                    .replace('\\', "/");
                check()?;
                if compile_native {
                    let controller = {
                        let mut slot = self
                            .compiler
                            .lock()
                            .map_err(|_| "external_compile_controller_unavailable")?;
                        self.check()?;
                        slot.get_or_insert_with(
                            crate::worker_windows::StreamController::for_runtime,
                        )
                        .clone()
                    };
                    let mut value = crate::external_native::validate(
                        &mut workspace,
                        &staged,
                        parent_deadline,
                        &controller,
                    )?;
                    self.check()?;
                    value["ok"] = json!(true);
                    value["roots"] = json!(rows);
                    value["revision"] = json!(revision.as_str());
                    value["cargo_manifest"] = json!(cargo_manifest);
                    value["candidate_lease_owned"] = json!(true);
                    return Ok(value);
                }
                Ok(
                    json!({"ok":true,"validation":"confined_source_staging","roots":rows,"revision":revision.as_str(),
                    "originals_mutated":staged.originals_mutated,"manifest_transformed":staged.manifest_transformed,
                    "cargo_manifest":cargo_manifest,"cargo_executed":false,"native_artifact_produced":false,"source_publish":false,"candidate_lease_owned":true}),
                )
            })();
            // A terminal command error does not prove worker or reader retirement.
            // Block subsequent intake/staging when exact owned cleanup is unknown.
            if workspace.lifetime.unresolved() {
                self.unconfirmed.store(true, Ordering::Release);
            }
            let cleanup = workspace.clean();
            match (result, cleanup) {
                (Ok(mut value), Ok(())) => {
                    value["private_workspace_cleaned"] = json!(true);
                    Ok(value)
                }
                (_, Err(_)) => Err(
                    "external validation cleanup unconfirmed; owned private workspace retained"
                        .into(),
                ),
                (Err(error), Ok(())) => Err(error),
            }
        }
    }
}

/// Used only by the attended packaged Studio helper. It verifies its original
/// parent and pipe before creating any dialog. No public operation calls it.
pub fn attended_owner_helper() -> Result<(), String> {
    #[cfg(windows)]
    {
        windows::helper_main()
    }
    #[cfg(not(windows))]
    {
        Err("attended dependency access is currently Windows-only".into())
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use crate::external_workspace::{
        Budget, ImportedProject, PreparedGrant, Revision, RootId, VerifiedGrant,
    };
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc;

    const PACKAGED_GUI_BASENAME: &str = "Aurum Studio.exe";

    fn require_packaged_gui_basename(current: &Path) -> Result<(), String> {
        if current.file_name().is_none_or(|name| {
            !name
                .to_string_lossy()
                .eq_ignore_ascii_case(PACKAGED_GUI_BASENAME)
        }) {
            return Err(format!(
                "attended helper is not packaged {PACKAGED_GUI_BASENAME}"
            ));
        }
        Ok(())
    }

    pub(super) enum IntakeFailure {
        Refused,
        Cancelled,
        Expired,
        Unconfirmed,
    }
    fn bounded_hash(path: &Path, deadline: Instant) -> Result<(File, String), String> {
        let mut file = crate::snapshot::open_checked_file(path, deadline)?;
        let length = file
            .metadata()
            .map_err(|_| "owner executable metadata unavailable")?
            .len();
        if length == 0 || length > 128 * 1024 * 1024 {
            return Err("owner executable exceeds its bound".into());
        }
        let mut hash = crate::hash::Sha256::new();
        let mut count = 0;
        let mut bytes = [0; 64 * 1024];
        loop {
            if Instant::now() >= deadline {
                return Err("owner executable hash deadline expired".into());
            }
            let n = file
                .read(&mut bytes)
                .map_err(|_| "owner executable read failed")?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > length {
                return Err("owner executable grew".into());
            }
            hash.update(&bytes[..n]);
        }
        if count != length
            || file
                .metadata()
                .map_err(|_| "owner executable metadata unavailable")?
                .len()
                != length
        {
            return Err("owner executable changed".into());
        }
        crate::snapshot::check_opened_path(&file, path, deadline)?;
        Ok((
            file,
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        ))
    }
    pub(super) fn random_hex(bytes: usize) -> Result<String, String> {
        let value = crate::random::bytes(bytes);
        if value.source != crate::random::Source::OperatingSystem {
            return Err("owner capability requires OS randomness".into());
        }
        Ok(value
            .bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    struct Package {
        helper: PathBuf,
        parent: PathBuf,
        parent_sha: String,
        helper_sha: String,
        _guards: Vec<File>,
    }
    impl Package {
        fn prepare(current: PathBuf, deadline: Instant) -> Result<Self, String> {
            let bin = current.parent().ok_or("owner package bin missing")?;
            if current
                .file_name()
                .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("aurum.exe"))
                || bin
                    .file_name()
                    .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("bin"))
            {
                return Err("attended access requires the complete packaged Studio layout".into());
            }
            let root = bin.parent().ok_or("owner package root missing")?;
            let helper = root.join(PACKAGED_GUI_BASENAME);
            let root_guard = crate::snapshot::open_checked_directory(root, deadline)?;
            let bin_guard = crate::snapshot::open_checked_directory(bin, deadline)?;
            let (parent_guard, parent_sha) = bounded_hash(&current, deadline)?;
            let (helper_guard, helper_sha) = bounded_hash(&helper, deadline)?;
            Ok(Self {
                helper,
                parent: current,
                parent_sha,
                helper_sha,
                _guards: vec![root_guard, bin_guard, parent_guard, helper_guard],
            })
        }
    }
    pub(super) struct OwnedHelper {
        package: Package,
        child: Option<Child>,
        input: Option<ChildStdin>,
        stderr_done: Option<mpsc::Receiver<Result<(), String>>>,
        output: Option<std::process::ChildStdout>,
        owner: Option<crate::ownership::LiveProcess>,
        stderr: Arc<std::sync::atomic::AtomicUsize>,
        retirement_unconfirmed: bool,
        pipes: PipeWorkers,
        spawned: bool,
        stdout_guard: Option<File>,
        stderr_guard: Option<File>,
        stdout_eof: Arc<AtomicBool>,
        stderr_eof: Arc<AtomicBool>,
    }
    impl OwnedHelper {
        pub(super) fn prepare() -> Result<Self, String> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let current = crate::project::clean_path(
                std::env::current_exe().map_err(|_| "owner parent unavailable")?,
            );
            let package = Package::prepare(current, deadline)?;
            Ok(Self {
                package,
                child: None,
                input: None,
                stderr_done: None,
                output: None,
                owner: None,
                stderr: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                retirement_unconfirmed: false,
                pipes: PipeWorkers::default(),
                spawned: false,
                stdout_guard: None,
                stderr_guard: None,
                stdout_eof: Arc::new(AtomicBool::new(false)),
                stderr_eof: Arc::new(AtomicBool::new(false)),
            })
        }
        fn start(
            &mut self,
            nonce: &str,
            cancel: &AtomicBool,
            deadline: Instant,
        ) -> Result<(), IntakeFailure> {
            use std::os::windows::process::CommandExt;
            let parent = crate::ownership::LiveProcess::current().ok_or(IntakeFailure::Refused)?;
            if !crate::ownership::paths_equal(&parent.executable, &self.package.parent) {
                return Err(IntakeFailure::Refused);
            }
            let child = Command::new(&self.package.helper)
                .arg("--internal-owner-access-helper")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x08000000)
                .spawn()
                .map_err(|_| IntakeFailure::Refused)?;
            self.child = Some(child);
            self.spawned = true;
            let child = self.child.as_mut().ok_or(IntakeFailure::Refused)?;
            let output = child.stdout.take().ok_or(IntakeFailure::Refused)?;
            let stderr = child.stderr.take().ok_or(IntakeFailure::Refused)?;
            self.stdout_guard = Some(
                duplicate_pipe(output.as_raw_handle()).map_err(|_| IntakeFailure::Unconfirmed)?,
            );
            self.stderr_guard = Some(
                duplicate_pipe(stderr.as_raw_handle()).map_err(|_| IntakeFailure::Unconfirmed)?,
            );
            self.input = child.stdin.take();
            self.output = Some(output);
            let live = self.live().map_err(|_| IntakeFailure::Refused)?;
            if !crate::ownership::paths_equal(&live.executable, &self.package.helper) {
                return Err(IntakeFailure::Refused);
            }
            self.owner = Some(live);
            let count = self.stderr.clone();
            let (done_send, done_receive) = mpsc::sync_channel(1);
            self.stderr_done = Some(done_receive);
            let stderr_eof = self.stderr_eof.clone();
            self.pipes
                .start("aurum-owner-stderr", move || {
                    let mut stderr = stderr;
                    let mut bytes = [0; 4096];
                    let result = loop {
                        match stderr.read(&mut bytes) {
                            Ok(0) => {
                                stderr_eof.store(true, Ordering::Release);
                                break Ok(());
                            }
                            Ok(n) => {
                                count.fetch_add(n, Ordering::AcqRel);
                                if count.load(Ordering::Acquire) > 32 * 1024 {
                                    break Err("owner diagnostic output exceeds its bound".into());
                                }
                            }
                            Err(_) => break Err("owner diagnostic channel failed".into()),
                        }
                    };
                    let _ = done_send.send(result);
                })
                .map_err(|_| IntakeFailure::Refused)?;
            self.send(
                &Hello {
                    version: env!("CARGO_PKG_VERSION").into(),
                    parent_pid: parent.pid,
                    parent_started: parent.started.ok_or(IntakeFailure::Refused)?,
                    parent_sha256: self.package.parent_sha.clone(),
                    helper_sha256: self.package.helper_sha.clone(),
                    nonce: nonce.into(),
                },
                cancel,
                deadline,
            )
        }
        fn send<T: Serialize>(
            &mut self,
            value: &T,
            cancel: &AtomicBool,
            deadline: Instant,
        ) -> Result<(), IntakeFailure> {
            let bytes = serde_json::to_vec(value).map_err(|_| IntakeFailure::Refused)?;
            if bytes.is_empty() || bytes.len() > FRAME_BYTES {
                return Err(IntakeFailure::Refused);
            }
            let mut input = self.input.take().ok_or(IntakeFailure::Refused)?;
            let (send, receive) = mpsc::sync_channel(1);
            self.pipes
                .start("aurum-owner-write", move || {
                    let result = input
                        .write_all(&(bytes.len() as u32).to_le_bytes())
                        .and_then(|()| input.write_all(&bytes))
                        .and_then(|()| input.flush());
                    let _ = send.send((result, input));
                })
                .map_err(|_| IntakeFailure::Refused)?;
            let (result, input) = wait_channel(&receive, cancel, deadline)?;
            self.input = Some(input);
            result.map_err(|_| IntakeFailure::Refused)
        }
        fn live(&self) -> Result<crate::ownership::LiveProcess, String> {
            let child = self.child.as_ref().ok_or("owner helper missing")?;
            process_identity(child.as_raw_handle(), child.id())
                .ok_or("owner helper identity unavailable".into())
        }
        fn verify(&self) -> Result<(), String> {
            let now = self.live()?;
            if self.owner.as_ref() != Some(&now) || self.stderr.load(Ordering::Acquire) > 32 * 1024
            {
                return Err("owner helper identity or output bound refused".into());
            }
            Ok(())
        }
        fn selected(
            &mut self,
            cancel: &AtomicBool,
            deadline: Instant,
        ) -> Result<Selected, IntakeFailure> {
            let mut output = self.output.take().ok_or(IntakeFailure::Refused)?;
            let (send, recv) = mpsc::sync_channel(1);
            self.pipes
                .start("aurum-owner-selection", move || {
                    let row = read_frame(&mut output);
                    let _ = send.send((row, output));
                })
                .map_err(|_| IntakeFailure::Refused)?;
            let (row, output) = wait_channel(&recv, cancel, deadline)?;
            self.output = Some(output);
            self.verify().map_err(|_| IntakeFailure::Refused)?;
            row.map_err(|_| IntakeFailure::Refused)
        }
        fn decision(
            &mut self,
            display: &Display,
            cancel: &AtomicBool,
            deadline: Instant,
        ) -> Result<Decision, IntakeFailure> {
            self.verify().map_err(|_| IntakeFailure::Refused)?;
            self.send(display, cancel, deadline)?;
            let mut output = self.output.take().ok_or(IntakeFailure::Refused)?;
            let (send, recv) = mpsc::sync_channel(1);
            self.pipes
                .start("aurum-owner-decision", move || {
                    let row = read_frame(&mut output);
                    let _ = send.send((row, output));
                })
                .map_err(|_| IntakeFailure::Refused)?;
            let (row, output) = wait_channel(&recv, cancel, deadline)?;
            self.output = Some(output);
            row.map_err(|_| IntakeFailure::Refused)
        }
        fn finish(&mut self, cancel: &AtomicBool, deadline: Instant) -> Result<(), IntakeFailure> {
            self.input.take();
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(IntakeFailure::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(IntakeFailure::Expired);
                }
                if self.stderr.load(Ordering::Acquire) != 0 {
                    return Err(IntakeFailure::Refused);
                }
                let status = self
                    .child
                    .as_mut()
                    .ok_or(IntakeFailure::Refused)?
                    .try_wait()
                    .map_err(|_| IntakeFailure::Unconfirmed)?;
                if let Some(status) = status {
                    if !status.success() {
                        return Err(IntakeFailure::Refused);
                    }
                    wait_channel(
                        self.stderr_done.as_ref().ok_or(IntakeFailure::Refused)?,
                        cancel,
                        deadline,
                    )?
                    .map_err(|_| IntakeFailure::Refused)?;
                    if self.stderr.load(Ordering::Acquire) != 0 {
                        return Err(IntakeFailure::Refused);
                    }
                    // Child::try_wait observes the original retained process handle.
                    let mut output = self.output.take().ok_or(IntakeFailure::Refused)?;
                    let (send, receive) = mpsc::sync_channel(1);
                    let eof = self.stdout_eof.clone();
                    self.pipes
                        .start("aurum-owner-eof", move || {
                            let mut extra = [0];
                            let result = output.read(&mut extra);
                            if matches!(result, Ok(0)) {
                                eof.store(true, Ordering::Release);
                            }
                            let _ = send.send(result);
                        })
                        .map_err(|_| IntakeFailure::Refused)?;
                    if wait_channel(&receive, cancel, deadline)?
                        .map_err(|_| IntakeFailure::Refused)?
                        != 0
                    {
                        return Err(IntakeFailure::Refused);
                    }
                    self.pipes
                        .complete(deadline, false, cancel_pipe_io)
                        .map_err(|_| IntakeFailure::Unconfirmed)?;
                    if !self.stdout_eof.load(Ordering::Acquire)
                        || !self.stderr_eof.load(Ordering::Acquire)
                    {
                        return Err(IntakeFailure::Unconfirmed);
                    }
                    self.stdout_guard.take();
                    self.stderr_guard.take();
                    self.child.take();
                    return Ok(());
                }
                self.verify().map_err(|_| IntakeFailure::Refused)?;
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        fn retire(&mut self) -> Result<(), IntakeFailure> {
            if self.retirement_unconfirmed {
                return Err(IntakeFailure::Unconfirmed);
            }
            let result = self.retire_original();
            if result.is_err() {
                self.retirement_unconfirmed = true;
            }
            result
        }

        fn retire_original(&mut self) -> Result<(), IntakeFailure> {
            // The old two-second root retirement budget now also covers all
            // owned I/O completion and EOF probes. No timeout broadening.
            let deadline = Instant::now() + Duration::from_secs(2);
            self.input.take();
            self.retire_root(deadline)?;
            if !self.spawned {
                return Ok(());
            }
            self.pipes
                .complete(deadline, true, cancel_pipe_io)
                .map_err(|_| IntakeFailure::Unconfirmed)?;
            self.output.take();
            self.stderr_done.take();
            for (name, guard, eof) in [
                (
                    "aurum-owner-stdout-close",
                    self.stdout_guard.as_ref(),
                    self.stdout_eof.clone(),
                ),
                (
                    "aurum-owner-stderr-close",
                    self.stderr_guard.as_ref(),
                    self.stderr_eof.clone(),
                ),
            ] {
                if eof.load(Ordering::Acquire) {
                    continue;
                }
                let guard = guard.ok_or(IntakeFailure::Unconfirmed)?;
                let pipe = duplicate_pipe(guard.as_raw_handle())
                    .map_err(|_| IntakeFailure::Unconfirmed)?;
                // Observe actual EOF only after earlier consumers were joined.
                // Raw private bytes are discarded, never retained or emitted.
                self.pipes
                    .start_retirement(name, deadline, move || drain_private_pipe(pipe, eof))
                    .map_err(|_| IntakeFailure::Unconfirmed)?;
            }
            // A still-open peer keeps EOF false. Cancel requests and root exit
            // cannot convert that state to confirmed pipe retirement.
            self.pipes
                .complete(deadline, false, cancel_pipe_io)
                .map_err(|_| IntakeFailure::Unconfirmed)?;
            if !self.stdout_eof.load(Ordering::Acquire) || !self.stderr_eof.load(Ordering::Acquire)
            {
                return Err(IntakeFailure::Unconfirmed);
            }
            self.stdout_guard.take();
            self.stderr_guard.take();
            self.child.take();
            Ok(())
        }
        fn retire_root(&mut self, deadline: Instant) -> Result<(), IntakeFailure> {
            let Some(child) = self.child.as_mut() else {
                return Ok(());
            };
            if child
                .try_wait()
                .map_err(|_| IntakeFailure::Unconfirmed)?
                .is_none()
            {
                if child.kill().is_err()
                    && child
                        .try_wait()
                        .map_err(|_| IntakeFailure::Unconfirmed)?
                        .is_none()
                {
                    return Err(IntakeFailure::Unconfirmed);
                }
                loop {
                    if child
                        .try_wait()
                        .map_err(|_| IntakeFailure::Unconfirmed)?
                        .is_some()
                    {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Err(IntakeFailure::Unconfirmed);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            // Keep the original Child handle until pipe retirement is proved.
            Ok(())
        }
    }
    impl Drop for OwnedHelper {
        fn drop(&mut self) {
            if self.retire().is_err() {
                // No retry after uncertainty, no detached blocking worker and
                // no complete-descendant-census claim. Retain this scope's
                // original objects, pipe handles and package guards until exit.
                if let Some(child) = self.child.take() {
                    std::mem::forget(child);
                }
                std::mem::forget(self.input.take());
                std::mem::forget(self.output.take());
                std::mem::forget(self.stdout_guard.take());
                std::mem::forget(self.stderr_guard.take());
                std::mem::forget(std::mem::take(&mut self.pipes));
                std::mem::forget(std::mem::take(&mut self.package._guards));
            }
        }
    }

    fn wait_channel<T>(
        receiver: &mpsc::Receiver<T>,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> Result<T, IntakeFailure> {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(IntakeFailure::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(IntakeFailure::Expired);
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(value) => return Ok(value),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(IntakeFailure::Refused),
                Err(_) => {}
            }
        }
    }
    pub(super) fn attend(
        broker: &Broker,
        root: &Path,
        id: &str,
        cancel: &AtomicBool,
        deadline: Instant,
        mut helper: OwnedHelper,
    ) -> Result<VerifiedGrant, IntakeFailure> {
        let result = (|| {
            let check = || {
                broker.check()?;
                if cancel.load(Ordering::Acquire) {
                    return Err("owner intake cancelled".into());
                }
                Ok(())
            };
            let budget =
                Budget::new(Duration::from_secs(60), &check).map_err(|_| IntakeFailure::Refused)?;
            let revision = Revision::new(
                &crate::external_workspace::current_revision(root, &budget)
                    .map_err(|_| IntakeFailure::Refused)?,
            )
            .map_err(|_| IntakeFailure::Refused)?;
            let project = ImportedProject::bind(root, revision.clone(), &budget)
                .map_err(|_| IntakeFailure::Refused)?;
            let nonce = random_hex(32).map_err(|_| IntakeFailure::Refused)?;
            helper.start(&nonce, cancel, deadline)?;
            let selected = helper.selected(cancel, deadline)?;
            if selected.version != env!("CARGO_PKG_VERSION") || selected.nonce != nonce {
                return Err(IntakeFailure::Refused);
            }
            let selected = selected.path.ok_or(IntakeFailure::Cancelled)?;
            if selected.len() > 4096 || selected.chars().any(char::is_control) {
                return Err(IntakeFailure::Refused);
            }
            // New short preparation budget follows selection; the whole pending deadline remains fixed.
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(30));
            let budget = Budget::new(remaining, &check).map_err(|_| IntakeFailure::Expired)?;
            let prepared = PreparedGrant::dependency_read(
                &project,
                RootId::new(id).map_err(|_| IntakeFailure::Refused)?,
                Path::new(&selected),
                &budget,
            )
            .map_err(|_| IntakeFailure::Refused)?;
            if let Ok(mut state) = broker.state.lock() {
                if let Some(pending) = state.pending.as_mut().filter(|pending| pending.id == id) {
                    pending.phase = "confirming";
                }
            }
            let display=Display {version:env!("CARGO_PKG_VERSION").into(),nonce:nonce.clone(),digest:prepared.digest(),
                project:root.to_string_lossy().into(),canonical_path:prepared.canonical_root().to_string_lossy().into(),
                permission:"Read public dependency source and copy it to a private validation workspace for this project revision. No source publication.".into()};
            let mut challenge = Challenge {
                nonce: Some(nonce),
                digest: display.digest.clone(),
                expires: deadline,
            };
            let decision = helper.decision(&display, cancel, deadline)?;
            let confirmation = challenge
                .consume(decision, cancel.load(Ordering::Acquire), Instant::now())
                .map_err(|_| IntakeFailure::Cancelled)?;
            helper.finish(cancel, deadline)?;
            // Rehash the imported source after attendance, then recheck retained exact directory identities.
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(30));
            let budget = Budget::new(remaining, &check).map_err(|_| IntakeFailure::Expired)?;
            let current = Revision::new(
                &crate::external_workspace::current_revision(root, &budget)
                    .map_err(|_| IntakeFailure::Refused)?,
            )
            .map_err(|_| IntakeFailure::Refused)?;
            prepared
                .finish(&confirmation, &current, &budget)
                .map_err(|_| IntakeFailure::Refused)
        })();
        if helper.retire().is_err() {
            return Err(IntakeFailure::Unconfirmed);
        }
        result
    }
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryFullProcessImageNameW(
            handle: *mut c_void,
            flags: u32,
            path: *mut u16,
            length: *mut u32,
        ) -> i32;
        fn GetProcessTimes(
            handle: *mut c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
        fn GetFileType(handle: *mut c_void) -> u32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn ExitProcess(code: u32) -> !;
        fn CancelSynchronousIo(thread: *mut c_void) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn DuplicateHandle(
            source_process: *mut c_void,
            source: *mut c_void,
            target_process: *mut c_void,
            target: *mut *mut c_void,
            access: u32,
            inherit: i32,
            options: u32,
        ) -> i32;
    }

    fn drain_private_pipe(mut pipe: File, eof: Arc<AtomicBool>) {
        if read_pipe_eof_capped(&mut pipe) {
            eof.store(true, Ordering::Release);
        }
    }

    pub(super) fn pipe_thread_ready(handle: &std::thread::JoinHandle<()>) -> Result<bool, ()> {
        match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
            0 => Ok(true),
            258 => Ok(false),
            _ => Err(()),
        }
    }
    fn cancel_pipe_io(handle: &std::thread::JoinHandle<()>) -> Result<(), ()> {
        if unsafe { CancelSynchronousIo(handle.as_raw_handle()) } != 0 {
            return Ok(());
        }
        // ERROR_NOT_FOUND means no cancellation was located; completion still
        // requires the retained thread's signaled object and successful join.
        if std::io::Error::last_os_error().raw_os_error() == Some(1168) {
            Ok(())
        } else {
            Err(())
        }
    }
    fn duplicate_pipe(handle: *mut c_void) -> Result<File, ()> {
        if unsafe { GetFileType(handle) } != 3 {
            return Err(());
        }
        let mut result = std::ptr::null_mut();
        let process = unsafe { GetCurrentProcess() };
        if unsafe { DuplicateHandle(process, handle, process, &mut result, 0, 0, 2) } == 0
            || result.is_null()
        {
            return Err(());
        }
        // Duplicate of the exact original read end, never a guessed pipe path.
        Ok(unsafe { File::from_raw_handle(result) })
    }

    fn process_identity(handle: *mut c_void, pid: u32) -> Option<crate::ownership::LiveProcess> {
        let mut path = [0u16; 32768];
        let mut size = path.len() as u32;
        let (mut created, mut exited, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        unsafe {
            if QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut size) == 0
                || size == 0
                || size as usize >= path.len()
                || GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) == 0
                || WaitForSingleObject(handle, 0) != 258
            {
                return None;
            }
        }
        Some(crate::ownership::LiveProcess {
            pid,
            executable: PathBuf::from(String::from_utf16(&path[..size as usize]).ok()?),
            started: Some(format!(
                "windows-filetime:{}",
                ((created.high as u64) << 32) | created.low as u64
            )),
        })
    }
    fn parent_pid() -> Option<u32> {
        #[repr(C)]
        struct Basic {
            exit: isize,
            peb: *mut c_void,
            affinity: usize,
            priority: isize,
            pid: usize,
            parent: usize,
        }
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn NtQueryInformationProcess(
                handle: *mut c_void,
                class: u32,
                info: *mut c_void,
                length: u32,
                returned: *mut u32,
            ) -> i32;
        }
        let mut info = std::mem::MaybeUninit::<Basic>::zeroed();
        let mut returned = 0;
        let code = unsafe {
            NtQueryInformationProcess(
                (-1isize) as *mut c_void,
                0,
                info.as_mut_ptr().cast(),
                std::mem::size_of::<Basic>() as u32,
                &mut returned,
            )
        };
        if code != 0 || returned as usize != std::mem::size_of::<Basic>() {
            return None;
        }
        u32::try_from(unsafe { info.assume_init() }.parent)
            .ok()
            .filter(|pid| *pid != 0)
    }
    pub(super) fn helper_main() -> Result<(), String> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        if unsafe { GetFileType(stdin.as_raw_handle()) } != 3
            || unsafe { GetFileType(stdout.as_raw_handle()) } != 3
        {
            return Err("attended helper requires original private pipes".into());
        }
        let mut input = stdin.lock();
        let mut output = stdout.lock();
        let hello: Hello = read_frame(&mut input)?;
        let parent = parent_pid().ok_or("attended helper parent unavailable")?;
        if hello.version != env!("CARGO_PKG_VERSION")
            || hello.parent_pid != parent
            || hello.nonce.len() != 64
            || !hello
                .nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("attended helper parent or challenge mismatch".into());
        }
        let current = crate::project::clean_path(
            std::env::current_exe().map_err(|_| "owner helper image unavailable")?,
        );
        require_packaged_gui_basename(&current)?;
        let root = current.parent().ok_or("owner helper package missing")?;
        let expected = root.join("bin/aurum.exe");
        let deadline = Instant::now() + Duration::from_secs(5);
        let _root = crate::snapshot::open_checked_directory(root, deadline)?;
        let _bin = crate::snapshot::open_checked_directory(&root.join("bin"), deadline)?;
        let (_parent, sha) = bounded_hash(&expected, deadline)?;
        let (_self, self_sha) = bounded_hash(&current, deadline)?;
        if self_sha != hello.helper_sha256 || sha != hello.parent_sha256 {
            return Err("attended helper package mismatch".into());
        }
        let handle = unsafe { OpenProcess(0x1000 | 0x0010_0000, 0, parent) };
        if handle.is_null() {
            return Err("attended helper parent handle unavailable".into());
        }
        let live = process_identity(handle, parent);
        if !live.is_some_and(|live| {
            crate::ownership::paths_equal(&live.executable, &expected)
                && live.started.as_deref() == Some(hello.parent_started.as_str())
        }) {
            unsafe {
                CloseHandle(handle);
            }
            return Err("attended helper parent identity mismatch".into());
        }
        // The bounded watcher terminates only this helper if the original parent
        // exits or attendance expires, including while a native dialog is modal.
        let raw = handle as usize;
        std::thread::spawn(move || {
            let handle = raw as *mut c_void;
            let result = unsafe { WaitForSingleObject(handle, (PENDING_SECONDS * 1000) as u32) };
            unsafe {
                CloseHandle(handle);
                ExitProcess(if result == 0 { 125 } else { 124 });
            }
        });
        let chosen = choose_folder()?;
        write_frame(
            &mut output,
            &Selected {
                version: env!("CARGO_PKG_VERSION").into(),
                nonce: hello.nonce.clone(),
                path: chosen.clone(),
            },
        )?;
        if chosen.is_none() {
            return Ok(());
        }
        let display: Display = read_frame(&mut input)?;
        if display.version != hello.version
            || display.nonce != hello.nonce
            || display.digest.len() != 64
            || !display
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || display.project.len() > 4096
            || display.canonical_path.len() > 4096
            || display.permission.len() > 512
            || [
                &display.project,
                &display.canonical_path,
                &display.permission,
            ]
            .iter()
            .any(|text| text.chars().any(char::is_control))
        {
            return Err("attended prepared display invalid".into());
        }
        let approved = confirm(&display);
        write_frame(
            &mut output,
            &Decision {
                version: hello.version,
                nonce: hello.nonce,
                digest: display.digest,
                approved,
            },
        )?;
        Ok(())
    }
    fn choose_folder() -> Result<Option<String>, String> {
        #[repr(C)]
        struct BrowseInfo {
            owner: *mut c_void,
            root: *mut c_void,
            display: *mut u16,
            title: *const u16,
            flags: u32,
            callback: Option<unsafe extern "system" fn(*mut c_void, u32, isize, isize) -> i32>,
            parameter: isize,
            image: i32,
        }
        #[link(name = "ole32")]
        unsafe extern "system" {
            fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
            fn CoUninitialize();
            fn CoTaskMemFree(memory: *mut c_void);
        }
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SHBrowseForFolderW(info: *const BrowseInfo) -> *mut c_void;
            fn SHGetPathFromIDListEx(
                pidl: *mut c_void,
                path: *mut u16,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        // Old tree dialog excludes the newer drag/drop and folder creation UI.
        // ABI and caller-owned path bounds: Microsoft shlobj_core documentation.
        let initialized = unsafe { CoInitializeEx(std::ptr::null_mut(), 2) };
        if initialized < 0 {
            return Err("owner folder picker initialization failed".into());
        }
        let title: Vec<u16> = "Choose dependency source to read for Aurum Studio"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut display = [0u16; 260];
        let info = BrowseInfo {
            owner: std::ptr::null_mut(),
            root: std::ptr::null_mut(),
            display: display.as_mut_ptr(),
            title: title.as_ptr(),
            flags: 0x1 | 0x200 | 0x400,
            callback: None,
            parameter: 0,
            image: 0,
        };
        let pidl = unsafe { SHBrowseForFolderW(&info) };
        if pidl.is_null() {
            unsafe {
                CoUninitialize();
            }
            return Ok(None);
        }
        let mut path = [0u16; 32768];
        let ok = unsafe { SHGetPathFromIDListEx(pidl, path.as_mut_ptr(), path.len() as u32, 0) };
        unsafe {
            CoTaskMemFree(pidl);
            CoUninitialize();
        }
        if ok == 0 {
            return Err("owner folder picker path unavailable".into());
        }
        let size = path
            .iter()
            .position(|c| *c == 0)
            .ok_or("owner folder picker path exceeded its bound")?;
        Ok(Some(
            String::from_utf16(&path[..size]).map_err(|_| "owner folder picker path invalid")?,
        ))
    }
    fn confirm(display: &Display) -> bool {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                window: *mut c_void,
                text: *const u16,
                title: *const u16,
                kind: u32,
            ) -> i32;
        }
        let text:Vec<u16>=format!("Project: {}\n\nDependency folder: {}\n\nPermission: {}\n\nAllow this access for the current Studio session?",
            display.project,display.canonical_path,display.permission).encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "Aurum Studio dependency access"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2, default is No.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                0x4 | 0x20 | 0x100,
            ) == 6
        }
    }

    #[cfg(test)]
    mod packaged_layout_tests {
        use super::*;

        static NEXT_PACKAGE: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);

        struct PackageFixture(PathBuf);
        impl PackageFixture {
            fn new() -> Self {
                let root = std::env::temp_dir().join(format!(
                    "aurum-owner-helper-package-{}-{}",
                    std::process::id(),
                    NEXT_PACKAGE.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir(&root).unwrap();
                std::fs::create_dir(root.join("bin")).unwrap();
                Self(root)
            }

            fn write(&self, name: &str, bytes: &[u8]) {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(self.0.join(name))
                    .unwrap();
                file.write_all(bytes).unwrap();
                file.sync_all().unwrap();
            }

            fn cli(&self) -> PathBuf {
                crate::project::clean_path(self.0.join("bin/aurum.exe"))
            }
        }

        impl Drop for PackageFixture {
            fn drop(&mut self) {
                for name in [
                    "bin/aurum.exe",
                    "Aurum Studio.exe",
                    "Studio.exe",
                    "aurum-studio.exe",
                    "renamed-gui.exe",
                ] {
                    let _ = std::fs::remove_file(self.0.join(name));
                }
                let _ = std::fs::remove_dir(self.0.join("bin"));
                let _ = std::fs::remove_dir(&self.0);
            }
        }

        #[test]
        fn packaged_gui_layout_resolves_and_pins_before_spawn() {
            let fixture = PackageFixture::new();
            let parent_bytes = b"packaged CLI fixture";
            let helper_bytes = b"packaged GUI fixture";
            fixture.write("bin/aurum.exe", parent_bytes);
            fixture.write("Aurum Studio.exe", helper_bytes);
            let helper = fixture.0.join("Aurum Studio.exe");
            let renamed = fixture.0.join("renamed-gui.exe");
            assert!(!fixture.0.join("Studio.exe").exists());

            let package =
                Package::prepare(fixture.cli(), Instant::now() + Duration::from_secs(5)).unwrap();
            assert_eq!(package.helper, helper);
            assert_eq!(package.parent, fixture.cli());
            assert_eq!(package.parent_sha, crate::sha256_hex(parent_bytes));
            assert_eq!(package.helper_sha, crate::sha256_hex(helper_bytes));
            require_packaged_gui_basename(&package.helper).unwrap();
            assert!(std::fs::rename(&helper, &renamed).is_err());

            drop(package);
            std::fs::rename(&helper, &renamed).unwrap();
            std::fs::rename(&renamed, &helper).unwrap();
        }

        #[test]
        fn packaged_gui_basename_and_missing_gui_refuse_legacy_aliases() {
            let fixture = PackageFixture::new();
            fixture.write("bin/aurum.exe", b"packaged CLI fixture");
            fixture.write("Studio.exe", b"legacy alias must not be used");
            fixture.write("aurum-studio.exe", b"Cargo basename must not be used");
            assert!(!fixture.0.join("Aurum Studio.exe").exists());
            assert!(
                Package::prepare(fixture.cli(), Instant::now() + Duration::from_secs(5)).is_err()
            );

            for name in [
                "Studio.exe",
                "aurum-studio.exe",
                "Aurum Studio.exe.bak",
                "other.exe",
            ] {
                assert_eq!(
                    require_packaged_gui_basename(&fixture.0.join(name)).unwrap_err(),
                    "attended helper is not packaged Aurum Studio.exe"
                );
            }
            require_packaged_gui_basename(&fixture.0.join("Aurum Studio.exe")).unwrap();
            require_packaged_gui_basename(&fixture.0.join("AURUM STUDIO.EXE")).unwrap();
            for parent in [
                fixture.0.join("bin/other.exe"),
                fixture.0.join("other/aurum.exe"),
            ] {
                assert!(Package::prepare(parent, Instant::now() + Duration::from_secs(5)).is_err());
            }
        }
    }

    #[cfg(test)]
    mod pipe_eof_tests {
        use super::*;
        fn pair() -> (File, File) {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn CreatePipe(
                    read: *mut *mut c_void,
                    write: *mut *mut c_void,
                    attributes: *mut c_void,
                    size: u32,
                ) -> i32;
            }
            let mut read = std::ptr::null_mut();
            let mut write = std::ptr::null_mut();
            assert_ne!(
                unsafe { CreatePipe(&mut read, &mut write, std::ptr::null_mut(), 4096) },
                0
            );
            (unsafe { File::from_raw_handle(read) }, unsafe {
                File::from_raw_handle(write)
            })
        }
        #[test]
        fn original_pipe_peer_requires_eof_not_just_completed_root_or_cancel_request() {
            let (read, write) = pair();
            let eof = Arc::new(AtomicBool::new(false));
            let observed = eof.clone();
            let mut pipes = PipeWorkers::default();
            pipes
                .complete(
                    Instant::now() + Duration::from_secs(1),
                    false,
                    cancel_pipe_io,
                )
                .unwrap();
            let (ready_send, ready) = mpsc::sync_channel(1);
            pipes
                .start_retirement(
                    "owner-peer-open-unit",
                    Instant::now() + Duration::from_secs(1),
                    move || {
                        ready_send.send(()).unwrap();
                        drain_private_pipe(read, observed);
                    },
                )
                .unwrap();
            ready.recv().unwrap();
            assert!(!eof.load(Ordering::Acquire));
            // Pretending a root has exited cannot complete an open original pipe.
            assert!(pipes.complete(Instant::now(), false, |_| Ok(())).is_err());
            assert!(pipes.unconfirmed);
            assert!(!eof.load(Ordering::Acquire));
            drop(write);
            pipes.handles.pop().unwrap().join().unwrap();
            assert!(eof.load(Ordering::Acquire));
            assert!(pipes
                .complete(
                    Instant::now() + Duration::from_secs(1),
                    false,
                    cancel_pipe_io
                )
                .is_err());
            let (read, write) = pair();
            drop(write);
            let eof = Arc::new(AtomicBool::new(false));
            let observed = eof.clone();
            let mut pipes = PipeWorkers::default();
            pipes
                .complete(
                    Instant::now() + Duration::from_secs(1),
                    false,
                    cancel_pipe_io,
                )
                .unwrap();
            pipes
                .start_retirement(
                    "owner-peer-closed-unit",
                    Instant::now() + Duration::from_secs(1),
                    move || drain_private_pipe(read, observed),
                )
                .unwrap();
            pipes
                .complete(
                    Instant::now() + Duration::from_secs(1),
                    false,
                    cancel_pipe_io,
                )
                .unwrap();
            assert!(eof.load(Ordering::Acquire));
            assert!(pipes.handles.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    fn configfile_parser_request() -> Value {
        let id = "a".repeat(32);
        json!({"op":"external_workspace_validate","root_ids":[id.clone()],
            "placements":[{"root_id":"project","slot":"app"},{"root_id":id.clone(),"slot":"engine"}],
            "workspace":{"root_id":id.clone(),"path":"Cargo.toml"},
            "member":{"root_id":id,"path":"native/Cargo.toml"},"compile_native":true,
            "gdextension_manifest":{"root_id":"project","path":"bin/checked.gdextension"}})
    }
    #[test]
    fn configfile_parser_selector_is_closed_and_does_not_widen_cargo_references() {
        let request = configfile_parser_request();
        preflight(&request).unwrap();
        let mut selected = request.clone();
        selected["gdextension_manifest"]["root_id"] = selected["root_ids"][0].clone();
        preflight(&selected).unwrap();
        for selection in [
            json!(null),
            json!([]),
            json!({}),
            json!({"root_id":"project","path":"bin/checked.gdextension","extra":true}),
            json!({"root_id":"b".repeat(32),"path":"bin/checked.gdextension"}),
            json!({"root_id":"project","path":"../checked.gdextension"}),
            json!({"root_id":"project","path":"A:/checked.gdextension"}),
            json!({"root_id":"project","path":"bin/checked.dll"}),
        ] {
            let mut invalid = request.clone();
            invalid["gdextension_manifest"] = selection;
            assert!(preflight(&invalid).is_err());
        }
        for value in [json!(false), json!(null), json!(1)] {
            let mut invalid = request.clone();
            invalid["compile_native"] = value;
            assert!(preflight(&invalid).is_err());
        }
        let mut cargo = request.clone();
        cargo["member"]["root_id"] = json!("project");
        assert!(preflight(&cargo).is_err());
    }
    #[test]
    fn configfile_parser_compatibility_entry_refuses_permission_before_source_io() {
        let broker = Broker::default();
        let missing = Path::new("absent-configfile-source-must-not-be-opened");
        assert_eq!(
            broker
                .validate(missing, &configfile_parser_request())
                .unwrap_err(),
            "configfile_parser_execution_permission_required"
        );
        assert!(!missing.exists());
    }
    #[cfg(windows)]
    #[test]
    fn configfile_parser_unconfirmed_peer_fence_blocks_further_staging() {
        let broker = Arc::new(Broker::default());
        broker.check().unwrap();
        broker.mark_parser_unconfirmed();
        assert!(broker
            .validate(
                Path::new("absent-parser-reentry"),
                &configfile_parser_request()
            )
            .unwrap_err()
            .contains("unconfirmed"));
        assert!(broker.managed_compiler_controller().is_err());
    }
    #[test]
    fn compile_native_is_only_an_explicit_boolean_validation_option() {
        let id = "0123456789abcdef0123456789abcdef";
        let request = json!({"op":"external_workspace_validate","root_ids":[id],
            "placements":[{"root_id":"project","slot":"app"},{"root_id":id,"slot":"engine"}],
            "workspace":{"root_id":id,"path":"Cargo.toml"},
            "member":{"root_id":id,"path":"native/Cargo.toml"}});
        preflight(&request).unwrap();
        for option in [json!(false), json!(true)] {
            let mut valid = request.clone();
            valid["compile_native"] = option;
            preflight(&valid).unwrap();
        }
        for option in [
            Value::Null,
            json!("PRIVATE_SENTINEL"),
            json!(1),
            json!({"approved":true}),
        ] {
            let mut invalid = request.clone();
            invalid["compile_native"] = option;
            assert_eq!(
                preflight(&invalid).unwrap_err(),
                "compile_native must be a boolean"
            );
        }
        assert!(preflight(
            &json!({"op":"workspace_access_request","mode":"dependency_read","compile_native":true})
        )
        .is_err());
        assert!(
            preflight(&json!({"op":"workspace_access_request","mode":"source_publish"})).is_err()
        );
    }
    use super::*;
    fn decision(nonce: &str, digest: &str, approved: bool) -> Decision {
        Decision {
            version: env!("CARGO_PKG_VERSION").into(),
            nonce: nonce.into(),
            digest: digest.into(),
            approved,
        }
    }
    fn challenge() -> Challenge {
        Challenge {
            nonce: Some("private-one-use".into()),
            digest: "prepared-identity-digest".into(),
            expires: Instant::now() + Duration::from_secs(1),
        }
    }
    #[test]
    fn ordinary_fields_cannot_supply_owner_authority() {
        for key in [
            "approved",
            "nonce",
            "owner_capability",
            "path",
            "directory_id",
            "digest",
        ] {
            let mut request = json!({"op":"workspace_access_request","mode":"dependency_read"});
            request[key] = json!(true);
            assert!(preflight(&request).is_err());
        }
        assert!(
            preflight(&json!({"op":"workspace_access_request","mode":"source_publish"})).is_err()
        );
        assert!(
            preflight(&json!({"op":"workspace_access_request","mode":"dependency_read"})).is_ok()
        );
    }
    #[test]
    fn forged_decision_consumes_and_cannot_be_replayed() {
        let mut pending = challenge();
        assert!(pending
            .consume(
                decision("foreign", "prepared-identity-digest", true),
                false,
                Instant::now()
            )
            .is_err());
        assert!(pending
            .consume(
                decision("private-one-use", "prepared-identity-digest", true),
                false,
                Instant::now()
            )
            .is_err());
        let mut pending = challenge();
        let confirmation = pending
            .consume(
                decision("private-one-use", "prepared-identity-digest", true),
                false,
                Instant::now(),
            )
            .unwrap();
        assert!(confirmation.matches("prepared-identity-digest"));
        assert!(!confirmation.matches("another-identity"));
        assert!(pending
            .consume(
                decision("private-one-use", "prepared-identity-digest", true),
                false,
                Instant::now()
            )
            .is_err());
    }
    #[test]
    fn cancel_expiry_and_owner_no_never_issue() {
        assert!(challenge()
            .consume(
                decision("private-one-use", "prepared-identity-digest", true),
                true,
                Instant::now()
            )
            .is_err());
        let mut pending = challenge();
        assert!(pending
            .consume(
                decision("private-one-use", "prepared-identity-digest", true),
                false,
                pending.expires
            )
            .is_err());
        assert!(challenge()
            .consume(
                decision("private-one-use", "prepared-identity-digest", false),
                false,
                Instant::now()
            )
            .is_err());
    }
    #[test]
    fn protocol_is_length_bounded_closed_and_does_not_echo_private_data() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &decision("private-one-use", "digest", true)).unwrap();
        let row: Decision = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(row.nonce, "private-one-use");
        let oversized = (FRAME_BYTES as u32 + 1).to_le_bytes();
        assert!(read_frame::<Decision>(&mut oversized.as_slice()).is_err());
        let body=br#"{"version":"0.4.1","nonce":"private-one-use","digest":"digest","approved":true,"extra":"secret-marker"}"#;
        let mut framed = (body.len() as u32).to_le_bytes().to_vec();
        framed.extend(body);
        let error = read_frame::<Decision>(&mut framed.as_slice())
            .err()
            .unwrap();
        assert!(!error.contains("private-one-use"));
        assert!(!error.contains("secret-marker"));
    }
    #[test]
    fn status_redacts_challenges_and_cancel_requires_exact_pending_identity() {
        let broker = Broker::default();
        let id = "a".repeat(32);
        let cancel = Arc::new(AtomicBool::new(false));
        broker.state.lock().unwrap().pending = Some(Pending {
            id: id.clone(),
            expires: Instant::now() + Duration::from_secs(PENDING_SECONDS),
            cancel: cancel.clone(),
            phase: "confirming",
        });
        let status = broker.status().unwrap().to_string();
        for denied in ["nonce", "digest", "directory_id", "capability", "volume"] {
            assert!(!status.contains(denied));
        }
        assert!(broker.cancel(&"b".repeat(32)).is_err());
        assert!(!cancel.load(Ordering::Acquire));
        broker.cancel(&id).unwrap();
        assert!(cancel.load(Ordering::Acquire));
        broker.close();
        assert!(broker.cancel(&id).is_err());
    }
    #[test]
    fn validation_requires_only_existing_ids_and_closed_relative_layout() {
        let id = "a".repeat(32);
        let valid = json!({"op":"external_workspace_validate","root_ids":[id],"placements":[{"root_id":"project","slot":"app"},{"root_id":id,"slot":"engine"}],
            "workspace":{"root_id":id,"path":"Cargo.toml"},"member":{"root_id":id,"path":"member/Cargo.toml"}});
        assert!(preflight(&valid).is_ok());
        for path in [
            "../Cargo.toml",
            "A:/Cargo.toml",
            "member\\Cargo.toml",
            ".aurum/../Cargo.toml",
        ] {
            let mut bad = valid.clone();
            bad["member"]["path"] = json!(path);
            assert!(preflight(&bad).is_err());
        }
        let mut bad = valid.clone();
        bad["root_ids"] = json!(["b".repeat(32)]);
        assert!(preflight(&bad).is_err());
        let mut bad = valid.clone();
        bad["source_publish"] = json!(true);
        assert!(preflight(&bad).is_err());
        let broker = Broker::default();
        assert!(broker
            .validate(Path::new("absent-fixture-root-must-not-be-read"), &valid)
            .is_err());
        #[cfg(windows)]
        assert_eq!(
            broker
                .validate(Path::new("absent-fixture-root-must-not-be-read"), &valid)
                .unwrap_err(),
            "read grant is absent; attended approval required"
        );
    }
    #[test]
    fn pipe_workers_join_original_threads_before_confirmed_retirement() {
        let mut pipes = PipeWorkers::default();
        let finished = Arc::new(AtomicBool::new(false));
        let observed = finished.clone();
        pipes
            .start("owner-pipe-completed-unit", move || {
                observed.store(true, Ordering::Release);
            })
            .unwrap();
        pipes
            .complete(Instant::now() + Duration::from_secs(1), false, |_| Ok(()))
            .unwrap();
        assert!(finished.load(Ordering::Acquire));
        assert!(pipes.handles.is_empty());
        assert!(pipes.normal_retired);
        assert!(pipes.start("late-normal-unit", || {}).is_err());
    }
    #[test]
    fn blocked_pipe_worker_refuses_completion_retry_and_broker_admission() {
        let mut pipes = PipeWorkers::default();
        let (ready_send, ready) = std::sync::mpsc::sync_channel(1);
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        pipes
            .start("owner-pipe-blocked-unit", move || {
                ready_send.send(()).unwrap();
                let _ = wait.recv();
            })
            .unwrap();
        ready.recv().unwrap();
        let requests = std::sync::atomic::AtomicUsize::new(0);
        assert!(pipes
            .complete(Instant::now() + Duration::from_secs(1), true, |_| {
                requests.fetch_add(1, Ordering::AcqRel);
                Ok(())
            })
            .is_err());
        assert_eq!(requests.load(Ordering::Acquire), 1);
        assert_eq!(pipes.handles.len(), 1);
        assert!(pipes.unconfirmed);
        assert!(pipes.start("refused-retry-unit", || {}).is_err());
        // Test-only release proves no unit worker is abandoned. It cannot clear
        // production's already-latched uncertainty or admit a repeated helper.
        release.send(()).unwrap();
        pipes.handles.pop().unwrap().join().unwrap();
        assert!(pipes
            .complete(Instant::now() + Duration::from_secs(1), true, |_| Ok(()))
            .is_err());
        let broker = Arc::new(Broker::default());
        broker.unconfirmed.store(true, Ordering::Release);
        assert!(broker
            .request(Path::new("absent-no-helper-start"))
            .unwrap_err()
            .contains("unconfirmed"));
        assert!(broker
            .validate(Path::new("absent-no-staging"), &json!({}))
            .unwrap_err()
            .contains("unconfirmed"));
        assert_eq!(requests.load(Ordering::Acquire), 1);
    }
    #[test]
    fn pipe_worker_panic_and_inaccessible_cancellation_are_unconfirmed() {
        let mut pipes = PipeWorkers::default();
        pipes
            .start("owner-pipe-panic-unit", || {
                panic!("controlled pipe worker unit failure")
            })
            .unwrap();
        assert!(pipes
            .complete(Instant::now() + Duration::from_secs(1), false, |_| Ok(()))
            .is_err());
        assert!(pipes.unconfirmed);
        assert!(pipes.handles.is_empty());
        let mut pipes = PipeWorkers::default();
        let (ready_send, ready) = std::sync::mpsc::sync_channel(1);
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        pipes
            .start("owner-pipe-access-unit", move || {
                ready_send.send(()).unwrap();
                let _ = wait.recv();
            })
            .unwrap();
        ready.recv().unwrap();
        assert!(pipes
            .complete(Instant::now() + Duration::from_secs(1), true, |_| Err(()))
            .is_err());
        assert!(pipes.unconfirmed);
        assert!(pipes
            .start_retirement(
                "forbidden-probe-unit",
                Instant::now() + Duration::from_secs(1),
                || {}
            )
            .is_err());
        release.send(()).unwrap();
        pipes.handles.pop().unwrap().join().unwrap();
    }
    #[test]
    fn pipe_worker_admission_has_closed_normal_and_probe_bounds() {
        let mut pipes = PipeWorkers::default();
        for _ in 0..MAX_NORMAL_PIPE_WORKERS {
            pipes.start("owner-pipe-limit-unit", || {}).unwrap();
        }
        assert!(pipes.start("extra-normal-unit", || {}).is_err());
        assert!(pipes
            .start_retirement(
                "early-probe-unit",
                Instant::now() + Duration::from_secs(1),
                || {}
            )
            .is_err());
        pipes
            .complete(Instant::now() + Duration::from_secs(1), false, |_| Ok(()))
            .unwrap();
        for _ in 0..MAX_RETIREMENT_PIPE_WORKERS {
            pipes
                .start_retirement(
                    "owner-probe-limit-unit",
                    Instant::now() + Duration::from_secs(1),
                    || {},
                )
                .unwrap();
        }
        assert!(pipes
            .start_retirement(
                "extra-probe-unit",
                Instant::now() + Duration::from_secs(1),
                || {}
            )
            .is_err());
        pipes
            .complete(Instant::now() + Duration::from_secs(1), false, |_| Ok(()))
            .unwrap();
        assert!(pipes.handles.is_empty());
        assert_eq!(pipes.normal_started, 6);
        assert_eq!(pipes.retirement_started, 2);
    }
    #[test]
    fn expired_deadline_refuses_even_already_ready_workers_and_probe_admission() {
        let mut pipes = PipeWorkers::default();
        pipes.start("owner-already-ready-unit", || {}).unwrap();
        let ready_deadline = Instant::now() + Duration::from_secs(1);
        while !pipe_thread_ready(&pipes.handles[0]).unwrap() {
            assert!(Instant::now() < ready_deadline);
            std::thread::yield_now();
        }
        assert!(pipes
            .complete(Instant::now() - Duration::from_secs(1), false, |_| Ok(()))
            .is_err());
        assert!(pipes.unconfirmed);
        assert!(!pipes.normal_retired);
        assert_eq!(pipes.handles.len(), 1);
        pipes.handles.pop().unwrap().join().unwrap();
        let mut pipes = PipeWorkers::default();
        pipes
            .complete(Instant::now() + Duration::from_secs(1), false, |_| Ok(()))
            .unwrap();
        assert!(pipes
            .start_retirement(
                "expired-probe-unit",
                Instant::now() - Duration::from_secs(1),
                || {}
            )
            .is_err());
        assert!(pipes.unconfirmed);
        assert_eq!(pipes.retirement_started, 0);
        assert!(pipes.handles.is_empty());
    }
    #[test]
    fn eof_probe_reads_exact_cap_plus_one_overflow_byte() {
        struct Counted {
            remaining: usize,
            consumed: usize,
            requests: Vec<usize>,
        }
        impl Read for Counted {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                self.requests.push(output.len());
                let count = output.len().min(self.remaining);
                output[..count].fill(0);
                self.remaining -= count;
                self.consumed += count;
                Ok(count)
            }
        }
        let mut exact = Counted {
            remaining: PIPE_EOF_BYTES,
            consumed: 0,
            requests: Vec::new(),
        };
        assert!(read_pipe_eof_capped(&mut exact));
        assert_eq!(exact.consumed, PIPE_EOF_BYTES);
        assert_eq!(exact.requests.last(), Some(&1));
        let mut overflow = Counted {
            remaining: PIPE_EOF_BYTES + 4096,
            consumed: 0,
            requests: Vec::new(),
        };
        assert!(!read_pipe_eof_capped(&mut overflow));
        assert_eq!(overflow.consumed, PIPE_EOF_BYTES + 1);
        assert_eq!(overflow.remaining, 4095);
        assert_eq!(overflow.requests.last(), Some(&1));
        assert!(overflow.requests.iter().all(|count| *count <= 4096));
    }
}
