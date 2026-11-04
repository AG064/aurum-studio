//! One discoverable native controller per project, shared by CLI/MCP/Studio.
//! Tokens live in a private file, never endpoint metadata or transport errors.
use crate::ownership::{OwnershipRecord, ProcessKind};
use crate::{files, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn request_window(op: &str, native_candidate: bool) -> Duration {
    if native_candidate {
        Duration::from_secs(600)
    } else if matches!(op, "native_start" | "native_replace" | "native_recover") {
        Duration::from_secs(240)
    } else {
        Duration::from_secs(15)
    }
}
struct RequestBudget {
    started: Instant,
    deadline: Instant,
    context: Option<crate::operation_context::OperationContext>,
}
impl RequestBudget {
    fn at(
        started: Instant,
        window: Duration,
        context: Option<crate::operation_context::OperationContext>,
    ) -> Result<Self, String> {
        let deadline = context.as_ref().map_or(started + window, |context| {
            context.deadline().min(started + window)
        });
        let budget = Self {
            started,
            deadline,
            context,
        };
        budget.remaining_at(started, Duration::MAX)?;
        Ok(budget)
    }
    fn remaining_at(&self, now: Instant, cap: Duration) -> Result<Duration, String> {
        if let Some(context) = &self.context {
            context.check().map_err(|error| error.to_string())?;
        }
        let left = self.deadline.saturating_duration_since(now).min(cap);
        if left.is_zero() {
            return Err("native controller caller deadline expired".into());
        }
        Ok(left)
    }
    fn remaining(&self, cap: Duration) -> Result<Duration, String> {
        self.remaining_at(Instant::now(), cap)
    }
    fn check(&self) -> Result<(), String> {
        self.remaining(Duration::MAX).map(|_| ())
    }
    fn clip(&self, cap: Duration) -> Self {
        self.clip_at(Instant::now(), cap)
    }
    fn clip_at(&self, now: Instant, cap: Duration) -> Self {
        Self {
            started: self.started,
            deadline: self.deadline.min(now + cap),
            context: self.context.clone(),
        }
    }
    fn restrict_window(&self, window: Duration) -> Self {
        Self {
            started: self.started,
            deadline: self.deadline.min(self.started + window),
            context: self.context.clone(),
        }
    }
}
fn write_before<W: Write>(
    stream: &mut W,
    bytes: &[u8],
    budget: &RequestBudget,
    mut now: impl FnMut() -> Instant,
    mut timeout: impl FnMut(&mut W, Duration) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = budget.remaining_at(now(), Duration::MAX)?;
        timeout(stream, remaining).map_err(|_| "native controller write timeout failed")?;
        budget.remaining_at(now(), Duration::MAX)?;
        let result = stream.write(&bytes[offset..]);
        match result {
            Ok(0) => return Err("native controller request write returned zero".into()),
            Ok(count) if count <= bytes.len() - offset => {
                budget.remaining_at(now(), Duration::MAX)?;
                offset += count;
            }
            Ok(_) => return Err("native controller request write count is invalid".into()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                budget.remaining_at(now(), Duration::MAX)?;
                continue;
            }
            Err(_) => return Err("native controller request write failed".into()),
        }
    }
    budget.remaining_at(now(), Duration::MAX).map(|_| ())
}
fn read_ready_before<R: Read>(
    stream: &mut R,
    budget: &RequestBudget,
    mut now: impl FnMut() -> Instant,
    mut timeout: impl FnMut(&mut R, Duration) -> std::io::Result<()>,
) -> Result<(), String> {
    // Read exactly the negotiated interim reply, leaving the final response
    // untouched. A different reply never permits sending the operation body.
    for expected in crate::native_request_control::READY {
        loop {
            let remaining = budget.remaining_at(now(), Duration::from_secs(1))?;
            timeout(stream, remaining).map_err(|_| "native controller ready timeout failed")?;
            budget.remaining_at(now(), Duration::MAX)?;
            let mut byte = [0];
            match stream.read(&mut byte) {
                Ok(0) => return Err("native controller closed before ready".into()),
                Ok(1) if byte[0] == *expected => {
                    budget.remaining_at(now(), Duration::MAX)?;
                    break;
                }
                Ok(_) => return Err("native controller returned an unsupported ready reply".into()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    budget.remaining_at(now(), Duration::MAX)?;
                }
                Err(_) => return Err("native controller ready read failed".into()),
            }
        }
    }
    budget.remaining_at(now(), Duration::MAX).map(|_| ())
}
struct Delivery<'a> {
    budget: &'a RequestBudget,
    attempted: bool,
}
impl Delivery<'_> {
    fn begin(&mut self) -> Result<(), String> {
        self.budget.check()?;
        self.attempted = true;
        Ok(())
    }
}
fn deliver_once<T>(
    budget: &RequestBudget,
    work: impl FnOnce(&mut Delivery<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let mut delivery = Delivery {
        budget,
        attempted: false,
    };
    let result = work(&mut delivery).and_then(|value| budget.check().map(|_| value));
    if result.is_err() && delivery.attempted {
        if let Some(context) = &budget.context {
            context.mark_recovery_required();
        }
    }
    result
}
fn launch_eligible(existing: bool, op: &str, native_candidate: bool) -> bool {
    !existing
        && (matches!(
            op,
            "native_start" | "native_recover" | "job_submit" | "job_reconnect"
        ) || (native_candidate && matches!(op, "changes_validate" | "changes_apply")))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    version: u8,
    port: u16,
    owner: OwnershipRecord,
}
fn directory(root: &Path) -> Result<PathBuf, String> {
    let path = files::confined(root, ".aurum/native")?;
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    Ok(path)
}
fn metadata(root: &Path) -> Result<Option<Endpoint>, String> {
    let path = directory(root)?.join("host.json");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    if !path.exists() {
        return Ok(None);
    }
    let meta = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if !meta.is_file() || meta.len() > 8192 {
        return Err("native host metadata exceeds its bound".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 8192 {
        return Err("native host metadata exceeds its bound".into());
    }
    let endpoint: Endpoint =
        serde_json::from_slice(&bytes).map_err(|_| "invalid native host metadata")?;
    if endpoint.version != 1
        || endpoint.port == 0
        || endpoint.owner.kind != ProcessKind::Worker
        || !crate::ownership::paths_equal(&endpoint.owner.project, root)
    {
        return Err("native host metadata identity is invalid".into());
    }
    if !crate::ownership::inspect(endpoint.owner.pid)
        .is_some_and(|live| endpoint.owner.describes(&live))
    {
        #[cfg(windows)]
        if !crate::ownership::recorded_process_gone(&endpoint.owner) {
            return Err(
                "native controller identity cannot be verified; refusing another host".into(),
            );
        }
        return Ok(None);
    }
    Ok(Some(endpoint))
}
fn token(root: &Path) -> Result<String, String> {
    let path = directory(root)?.join("host.token");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let meta = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err("native host credential file exceeds its bound".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let value = String::from_utf8(bytes).map_err(|_| "native host credential is invalid")?;
    if value.is_empty()
        || value.len() > 4096
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("native host credential is invalid".into());
    }
    Ok(value)
}
#[cfg(windows)]
fn listener_owned(port: u16, pid: u32) -> bool {
    use std::ffi::c_void;
    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn GetExtendedTcpTable(
            table: *mut c_void,
            size: *mut u32,
            ordered: i32,
            family: u32,
            class: i32,
            reserved: u32,
        ) -> u32;
    }
    let mut size = 0;
    unsafe {
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, 2, 3, 0);
    }
    if !(4..=1024 * 1024).contains(&size) {
        return false;
    }
    let mut table = vec![0u32; (size as usize).div_ceil(4)];
    if unsafe { GetExtendedTcpTable(table.as_mut_ptr().cast(), &mut size, 0, 2, 3, 0) } != 0 {
        return false;
    }
    let count = table[0] as usize;
    if count > table.len().saturating_sub(1) / 6 {
        return false;
    }
    (0..count).any(|index| {
        let row = &table[1 + index * 6..1 + (index + 1) * 6];
        row[0] == 2
            && row[1] == u32::from_ne_bytes([127, 0, 0, 1])
            && u16::from_be(row[2] as u16) == port
            && row[5] == pid
    })
}
fn registration_lock(root: &Path) -> Result<File, String> {
    // All clients of this project must resolve the same lease, including
    // clients launched with a different TEMP or TMPDIR environment.
    let path = directory(root)?.join("host.lock");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(path.parent().ok_or("missing host lock parent")?)
        .map_err(|error| error.to_string())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.try_lock().map_err(|_| {
        "an Aurum Studio host already owns this project; connect to the existing host"
    })?;
    Ok(file)
}
pub struct Registration {
    _lease: File,
    root: PathBuf,
    port: u16,
}
impl Registration {
    pub fn create(root: &Path, port: u16, secret: &str) -> Result<Self, String> {
        if port == 0 {
            return Err("native controller requires a bound port".into());
        }
        if secret.is_empty()
            || secret.len() > 4096
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("native host credential is invalid".into());
        }
        let lease = registration_lock(root)?;
        let directory = directory(root)?;
        let live = crate::ownership::inspect(std::process::id())
            .ok_or("native controller process identity is unavailable")?;
        let owner = OwnershipRecord {
            session: crate::random::session_id(),
            executable: live.executable,
            pid: live.pid,
            started: live
                .started
                .ok_or("native controller creation time is unavailable")?,
            project: root.to_path_buf(),
            kind: ProcessKind::Worker,
        };
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        let path = directory.join("host.token");
        crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        file.write_all(secret.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        files::write_atomic(
            &directory.join("host.json"),
            &serde_json::to_vec(&Endpoint {
                version: 1,
                port,
                owner,
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            _lease: lease,
            root: root.to_path_buf(),
            port,
        })
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(Some(endpoint)) = metadata(&self.root) {
            if endpoint.owner.pid == std::process::id() && endpoint.port == self.port {
                if let Ok(directory) = directory(&self.root) {
                    let _ = std::fs::remove_file(directory.join("host.json"));
                    let _ = std::fs::remove_file(directory.join("host.token"));
                }
            }
        }
        // Explicitly release the lease before closing the descriptor. Unix
        // children can briefly retain its open file description while another
        // thread is spawning, even though the descriptor is close-on-exec.
        let _ = self._lease.unlock();
    }
}
pub fn route(root: &Path, input: &Value) -> Result<Value, String> {
    let started = Instant::now();
    let hint = input["op"].as_str().unwrap_or("");
    // Native-candidate classification may only shorten this initial ceiling.
    let budget = RequestBudget::at(
        started,
        request_window(hint, matches!(hint, "changes_validate" | "changes_apply")),
        crate::operation_context::OperationContext::current(),
    )?;
    budget.check()?;
    let context_request = (input["op"] == "runtime_context")
        .then(|| crate::runtime_context::operation_request(input))
        .transpose()?;
    budget.check()?;
    if !cfg!(windows) {
        if let Some(input) = context_request.as_ref() {
            return crate::runtime_context::inspect(
                root,
                input,
                crate::runtime_context::Session::Unavailable,
            );
        }
        return Err("managed native sessions are currently Windows-qualified".into());
    }
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())?;
    budget.check()?;
    let op = input["op"]
        .as_str()
        .ok_or("native operation must be a string")?;
    let jobs = crate::project_jobs::supported(op);
    let mut request = input.clone();
    request["project"] = json!(root);
    if jobs {
        crate::jobs::validate_request_size(&request)?;
    }
    let body = serde_json::to_vec(&request).map_err(|_| "native request encoding failed")?;
    budget.check()?;
    if body.len() > 2 * 1024 * 1024 {
        return Err("native routed request exceeds 2 MiB".into());
    }
    let native_candidate = if matches!(op, "changes_validate" | "changes_apply") {
        budget.check()?;
        let project = Project::open(&root).map_err(|error| error.to_string())?;
        budget.check()?;
        if project.config.rust_package.is_some() {
            if project.config.validation_command.is_some() {
                return Err(
                    "native candidates require copied source without custom validators".into(),
                );
            }
            project.confined_build_workspace()?;
            budget.check()?;
            if project.godot_project_dir().is_none() {
                return Err("native candidate validation requires project.godot".into());
            }
            let checked = crate::revisions::check(&root, input)?;
            budget.check()?;
            if checked["ready"] != true {
                return Ok(checked);
            }
            true
        } else {
            false
        }
    } else {
        false
    };
    let budget = budget.restrict_window(request_window(op, native_candidate));
    budget.check()?;
    let mut endpoint = metadata(&root)?;
    budget.check()?;
    let may_launch = launch_eligible(endpoint.is_some(), op, native_candidate);
    if endpoint.is_none() {
        if jobs && !matches!(op, "job_submit" | "job_reconnect") {
            return Ok(
                json!({"ok":true,"host_available":false,"jobs_available":false,"reconnect_required":true}),
            );
        }
        if op == "native_status" {
            return Ok(json!({"ok":true,"running":false,"host_available":false}));
        }
        if op == "runtime_context" {
            return crate::runtime_context::inspect(
                &root,
                context_request
                    .as_ref()
                    .ok_or("runtime context request is unavailable")?,
                crate::runtime_context::Session::Unavailable,
            );
        }
        if matches!(op, "native_stop" | "native_host_stop") {
            return Ok(json!({"ok":true,"stopped":false,"host_available":false}));
        }
        if !may_launch {
            return Err("no native controller is running; start a native session first".into());
        }
        Project::open(&root).map_err(|error| error.to_string())?;
        budget.check()?;
        launch(&root)?;
        budget.check()?;
        let startup = budget.clip(Duration::from_secs(20));
        while endpoint.is_none() {
            std::thread::sleep(startup.remaining(Duration::from_millis(50))?);
            startup.check()?;
            endpoint = metadata(&root)?;
            startup.check()?;
        }
    }
    let endpoint = endpoint.ok_or("native controller unavailable")?;
    #[cfg(windows)]
    if !listener_owned(endpoint.port, endpoint.owner.pid) {
        return Err("native controller port does not belong to its recorded process".into());
    }
    let secret = token(&root)?;
    budget.check()?;
    let controlled = op != "native_host_stop";
    let supports_control = verify_scope_with_budget(
        &root,
        &endpoint,
        &secret,
        &budget.clip(Duration::from_secs(3)),
    )?;
    if controlled && !supports_control {
        return Err("native controller does not support bounded request control".into());
    }
    deliver_once(&budget, |delivery| {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
        let mut stream =
            TcpStream::connect_timeout(&address, budget.remaining(Duration::from_secs(2))?)
                .map_err(|_| "native controller connection failed")?;
        let path = if op == "native_host_stop" {
            "/api/stop"
        } else {
            "/api/project"
        };
        let control_header = if controlled {
            format!(
                "X-Aurum-Request-Control: {}\r\nExpect: 100-continue\r\n",
                crate::native_request_control::PROTOCOL
            )
        } else {
            String::new()
        };
        let body_len = if controlled {
            crate::native_request_control::body_len(body.len())?
        } else {
            body.len()
        };
        let header=format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Aurum-Token: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{control_header}Connection: close\r\n\r\n",endpoint.port,secret,body_len);
        delivery.begin()?;
        write_before(
            &mut stream,
            header.as_bytes(),
            &budget,
            Instant::now,
            |stream, remaining| stream.set_write_timeout(Some(remaining)),
        )?;
        let controlled_body = if controlled {
            read_ready_before(&mut stream, &budget, Instant::now, |stream, remaining| {
                stream.set_read_timeout(Some(remaining))
            })?;
            // The receiver established its anchor before sending ready. Sample
            // this same restricted budget now, without creating a new window.
            Some(crate::native_request_control::encode(
                &body,
                budget.remaining(Duration::MAX)?,
            )?)
        } else {
            None
        };
        write_before(
            &mut stream,
            controlled_body.as_deref().unwrap_or(&body),
            &budget,
            Instant::now,
            |stream, remaining| stream.set_write_timeout(Some(remaining)),
        )?;
        budget.check()?;
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            let remaining = budget.remaining(Duration::from_secs(1))?;
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|_| "native controller read timeout failed")?;
            let count = match stream.read(&mut chunk) {
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    budget.check()?;
                    continue; // Same socket/request and absolute end, never resend.
                }
                Err(_) => return Err("native controller response failed".into()),
            };
            budget.check()?;
            if count == 0 {
                break;
            }
            if bytes.len() + count > 4 * 1024 * 1024 {
                return Err("native controller response exceeds 4 MiB".into());
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let boundary = bytes
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or("native controller returned invalid HTTP")?;
        if boundary > 16384 {
            return Err("native controller response headers exceed their bound".into());
        }
        let header = std::str::from_utf8(&bytes[..boundary])
            .map_err(|_| "invalid native controller headers")?;
        let status = header
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or("invalid native controller response status")?;
        let value: Value = serde_json::from_slice(&bytes[boundary + 4..])
            .map_err(|_| "native controller returned invalid JSON")?;
        if op != "native_host_stop"
            && !crate::ownership::inspect(endpoint.owner.pid)
                .is_some_and(|live| endpoint.owner.describes(&live))
        {
            return Err("native controller identity changed during request".into());
        }
        if status != 200 || value["ok"] == false {
            return Err(value["error"]
                .as_str()
                .unwrap_or("native controller operation failed")
                .replace(&secret, "[redacted]"));
        }
        Ok(value)
    })
}
fn verify_scope(root: &Path, endpoint: &Endpoint, secret: &str) -> Result<(), String> {
    let budget = RequestBudget::at(
        Instant::now(),
        Duration::from_secs(3),
        crate::operation_context::OperationContext::current(),
    )?;
    verify_scope_with_budget(root, endpoint, secret, &budget).map(|_| ())
}
fn verify_scope_with_budget(
    root: &Path,
    endpoint: &Endpoint,
    secret: &str,
    budget: &RequestBudget,
) -> Result<bool, String> {
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port)),
        budget.remaining(Duration::from_secs(2))?,
    )
    .map_err(|_| "native controller scope probe failed")?;
    stream
        .set_read_timeout(Some(budget.remaining(Duration::MAX)?))
        .map_err(|_| "native controller scope timeout failed")?;
    let header=format!("GET /api/native_host HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Aurum-Token: {}\r\nConnection: close\r\n\r\n",endpoint.port,secret);
    write_before(
        &mut stream,
        header.as_bytes(),
        budget,
        Instant::now,
        |stream, remaining| stream.set_write_timeout(Some(remaining)),
    )?;
    budget.check()?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let remaining = budget.remaining(Duration::MAX)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| "native scope timeout failed")?;
        let count = stream
            .read(&mut chunk)
            .map_err(|_| "native controller scope response failed")?;
        budget.check()?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > 65536 {
            return Err("native controller scope response exceeds its bound".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let boundary = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("invalid native controller scope response")?;
    let header = std::str::from_utf8(&bytes[..boundary])
        .map_err(|_| "invalid native controller scope header")?;
    if !header.starts_with("HTTP/1.1 200 ") {
        return Err("native controller scope authentication failed".into());
    }
    let value: Value = serde_json::from_slice(&bytes[boundary + 4..])
        .map_err(|_| "invalid native controller scope JSON")?;
    if value["protocol"] != "aurum-native-host/1"
        || value["pid"] != endpoint.owner.pid
        || value["port"] != endpoint.port
        || !value["project"]
            .as_str()
            .is_some_and(|project| crate::ownership::paths_equal(Path::new(project), root))
    {
        return Err("native controller serves a different project or protocol".into());
    }
    budget.check()?;
    Ok(value["request_control"] == crate::native_request_control::PROTOCOL)
}
pub fn existing_url(root: &Path) -> Result<Option<String>, String> {
    let Some(endpoint) = metadata(root)? else {
        return Ok(None);
    };
    #[cfg(windows)]
    if !listener_owned(endpoint.port, endpoint.owner.pid) {
        return Err("existing Studio port ownership is invalid".into());
    }
    let secret = token(root)?;
    verify_scope(root, &endpoint, &secret)?;
    Ok(Some(format!(
        "http://127.0.0.1:{}/?t={}",
        endpoint.port, secret
    )))
}
fn launch(root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    crate::diagnostic_relay::confine_standard_handles()
        .map_err(|_| "could not confine launching client handles")?;
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let executable = if current
        .file_stem()
        .is_some_and(|name| name.eq_ignore_ascii_case("aurum"))
    {
        current
    } else {
        current
            .parent()
            .ok_or("Aurum executable directory unavailable")?
            .join(if cfg!(windows) { "aurum.exe" } else { "aurum" })
    };
    if !executable.is_file() {
        return Err(
            "Aurum CLI is required to start the shared controller; start Studio explicitly".into(),
        );
    }
    let mut command = std::process::Command::new(executable);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .arg("studio")
        .arg(root)
        .args(["--no-open", "--json", "--port", "0"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "could not launch the shared native controller")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
#[path = "native_request_caller_tests.rs"]
mod request_control_tests;

#[cfg(test)]
mod tests {
    use super::*;
    enum WriteStep {
        Part(usize, u64),
        Interrupt,
        Timeout,
        Zero,
        Cancel(usize),
        Terminal { zero: bool, cancel: bool },
    }
    struct ScriptWriter {
        steps: std::collections::VecDeque<WriteStep>,
        bytes: Vec<u8>,
        inputs: Vec<Vec<u8>>,
        now: std::rc::Rc<std::cell::Cell<Instant>>,
        context: Option<crate::operation_context::OperationContext>,
    }
    impl Write for ScriptWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.inputs.push(bytes.to_vec());
            match self.steps.pop_front().expect("unexpected extra write") {
                WriteStep::Part(count, seconds) => {
                    self.bytes.extend_from_slice(&bytes[..count]);
                    self.now.set(self.now.get() + Duration::from_secs(seconds));
                    Ok(count)
                }
                WriteStep::Cancel(count) => {
                    self.bytes.extend_from_slice(&bytes[..count]);
                    self.context.as_ref().unwrap().request_cancel().unwrap();
                    Ok(count)
                }
                WriteStep::Interrupt => {
                    self.now.set(self.now.get() + Duration::from_secs(1));
                    Err(std::io::ErrorKind::Interrupted.into())
                }
                WriteStep::Timeout => Err(std::io::ErrorKind::TimedOut.into()),
                WriteStep::Zero => Ok(0),
                WriteStep::Terminal { zero, cancel } => {
                    if cancel {
                        self.context.as_ref().unwrap().request_cancel().unwrap();
                    } else {
                        self.now.set(self.now.get() + Duration::from_secs(21));
                    }
                    if zero {
                        Ok(0)
                    } else {
                        Err(std::io::ErrorKind::BrokenPipe.into())
                    }
                }
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn scripted(start: Instant, steps: Vec<WriteStep>) -> ScriptWriter {
        ScriptWriter {
            steps: steps.into(),
            bytes: Vec::new(),
            inputs: Vec::new(),
            now: std::rc::Rc::new(std::cell::Cell::new(start)),
            context: None,
        }
    }
    #[test]
    fn partial_writer_recomputes_timeout_and_never_duplicates_accepted_prefix() {
        let start = Instant::now();
        let budget = RequestBudget::at(start, Duration::from_secs(20), None).unwrap();
        let mut writer = scripted(
            start,
            vec![
                WriteStep::Part(2, 2),
                WriteStep::Interrupt,
                WriteStep::Part(1, 3),
                WriteStep::Part(3, 1),
            ],
        );
        let clock = writer.now.clone();
        let mut timeouts = Vec::new();
        write_before(
            &mut writer,
            b"abcdef",
            &budget,
            || clock.get(),
            |_, value| {
                timeouts.push(value);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(writer.bytes, b"abcdef");
        assert_eq!(
            writer.inputs,
            [
                b"abcdef".to_vec(),
                b"cdef".to_vec(),
                b"cdef".to_vec(),
                b"def".to_vec()
            ]
        );
        assert_eq!(timeouts, [20, 18, 17, 14].map(Duration::from_secs));
    }
    #[test]
    fn writer_expiry_after_partial_progress_stops_suffix_and_next_frame() {
        let start = Instant::now();
        let budget = RequestBudget::at(start, Duration::from_secs(5), None).unwrap();
        let mut writer = scripted(start, vec![WriteStep::Part(2, 6), WriteStep::Part(2, 0)]);
        let clock = writer.now.clone();
        let result: Result<(), String> = deliver_once(&budget, |delivery| {
            delivery.begin()?;
            write_before(&mut writer, b"abcd", &budget, || clock.get(), |_, _| Ok(()))?;
            write_before(&mut writer, b"body", &budget, || clock.get(), |_, _| Ok(()))
        });
        assert!(result.is_err());
        assert_eq!(writer.bytes, b"ab");
        assert_eq!(writer.inputs.len(), 1);
    }
    #[test]
    fn writer_cancellation_after_partial_progress_does_not_send_suffix() {
        let start = Instant::now();
        let context =
            crate::operation_context::OperationContext::new(start + Duration::from_secs(20));
        let budget =
            RequestBudget::at(start, Duration::from_secs(20), Some(context.clone())).unwrap();
        let mut writer = scripted(start, vec![WriteStep::Cancel(1), WriteStep::Part(2, 0)]);
        writer.context = Some(context.clone());
        let clock = writer.now.clone();
        let result: Result<(), String> = deliver_once(&budget, |delivery| {
            delivery.begin()?;
            write_before(&mut writer, b"abc", &budget, || clock.get(), |_, _| Ok(()))
        });
        assert!(result.is_err());
        assert_eq!(writer.bytes, b"a");
        assert_eq!(writer.inputs.len(), 1);
        assert!(context.recovery_required());
    }
    #[test]
    fn writer_zero_timeout_and_setup_expiry_refuse_without_redelivery() {
        for step in [WriteStep::Zero, WriteStep::Timeout] {
            let start = Instant::now();
            let budget = RequestBudget::at(start, Duration::from_secs(20), None).unwrap();
            let mut writer = scripted(start, vec![step]);
            let clock = writer.now.clone();
            assert!(
                write_before(&mut writer, b"abc", &budget, || clock.get(), |_, _| Ok(())).is_err()
            );
            assert!(writer.bytes.is_empty());
            assert_eq!(writer.inputs.len(), 1);
        }
        let start = Instant::now();
        let budget = RequestBudget::at(start, Duration::from_secs(20), None).unwrap();
        let mut writer = scripted(start, vec![WriteStep::Part(3, 0)]);
        let clock = writer.now.clone();
        let setter_clock = writer.now.clone();
        assert!(write_before(
            &mut writer,
            b"abc",
            &budget,
            || clock.get(),
            |_, _| {
                setter_clock.set(start + Duration::from_secs(21));
                Ok(())
            }
        )
        .is_err());
        assert!(writer.inputs.is_empty());
    }
    #[test]
    fn terminal_write_negative_survives_later_expiry_or_cancellation() {
        for zero in [false, true] {
            for cancel in [false, true] {
                let start = Instant::now();
                let context = crate::operation_context::OperationContext::new(
                    start + Duration::from_secs(20),
                );
                let budget =
                    RequestBudget::at(start, Duration::from_secs(20), Some(context.clone()))
                        .unwrap();
                let mut writer = scripted(start, vec![WriteStep::Terminal { zero, cancel }]);
                writer.context = Some(context.clone());
                let clock = writer.now.clone();
                let result: Result<(), String> = deliver_once(&budget, |delivery| {
                    delivery.begin()?;
                    write_before(
                        &mut writer,
                        b"header",
                        &budget,
                        || clock.get(),
                        |_, _| Ok(()),
                    )?;
                    write_before(&mut writer, b"body", &budget, || clock.get(), |_, _| Ok(()))
                });
                assert_eq!(
                    result.unwrap_err(),
                    if zero {
                        "native controller request write returned zero"
                    } else {
                        "native controller request write failed"
                    }
                );
                assert!(context.recovery_required());
                assert_eq!(writer.inputs, [b"header".to_vec()]);
                assert!(writer.bytes.is_empty());
                assert!(writer.steps.is_empty());
            }
        }
    }
    #[test]
    fn request_classification_keeps_original_start_and_cannot_renew_after_preflight() {
        let start = Instant::now();
        let initial = RequestBudget::at(start, Duration::from_secs(600), None).unwrap();
        let ordinary = initial.restrict_window(Duration::from_secs(15));
        assert_eq!(ordinary.started, start);
        assert_eq!(ordinary.deadline, start + Duration::from_secs(15));
        assert!(ordinary
            .remaining_at(start + Duration::from_secs(20), Duration::MAX)
            .is_err());
        let native = initial.restrict_window(request_window("changes_validate", true));
        assert_eq!(native.started, start);
        assert_eq!(native.deadline, initial.deadline);
        assert_eq!(
            native
                .remaining_at(start + Duration::from_secs(20), Duration::MAX)
                .unwrap(),
            Duration::from_secs(580)
        );
    }
    #[test]
    fn fresh_candidate_bootstrap_is_explicit_and_existing_hosts_never_relaunch() {
        for op in ["changes_validate", "changes_apply"] {
            assert!(launch_eligible(false, op, true));
            assert!(!launch_eligible(true, op, true));
            assert!(!launch_eligible(false, op, false));
        }
        for op in [
            "native_status",
            "native_stop",
            "native_host_stop",
            "runtime_context",
            "malformed",
        ] {
            assert!(!launch_eligible(false, op, false));
            assert!(!launch_eligible(false, op, true));
        }
        assert!(launch_eligible(false, "native_start", false));
        assert!(launch_eligible(false, "job_submit", false));
    }
    #[test]
    fn caller_budget_consumes_bootstrap_and_preserves_original_parent_expiry() {
        let start = Instant::now();
        let context =
            crate::operation_context::OperationContext::new(start + Duration::from_secs(30));
        let budget = RequestBudget::at(
            start,
            request_window("changes_apply", true),
            Some(context.clone()),
        )
        .unwrap();
        assert_eq!(budget.deadline, context.deadline());
        assert_eq!(
            budget
                .remaining_at(start + Duration::from_secs(20), Duration::MAX)
                .unwrap(),
            Duration::from_secs(10)
        );
        let scope = budget.clip_at(start + Duration::from_secs(28), Duration::from_secs(3));
        assert_eq!(scope.deadline, budget.deadline);
        assert!(scope
            .remaining_at(start + Duration::from_secs(30), Duration::MAX)
            .is_err());
        assert_eq!(budget.deadline, start + Duration::from_secs(30));
        assert!(request_window("changes_apply", true) >= Duration::from_secs(300));
        assert_eq!(
            request_window("native_status", false),
            Duration::from_secs(15)
        );
    }
    #[test]
    fn cancellation_before_send_prevents_delivery_and_does_not_claim_remote_work() {
        let start = Instant::now();
        let context =
            crate::operation_context::OperationContext::new(start + Duration::from_secs(30));
        let budget =
            RequestBudget::at(start, Duration::from_secs(30), Some(context.clone())).unwrap();
        context.request_cancel().unwrap();
        let mut writes = 0;
        let result: Result<(), String> = deliver_once(&budget, |delivery| {
            delivery.begin()?;
            writes += 1;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(writes, 0);
        assert!(!context.recovery_required());
    }
    #[test]
    fn expired_original_budget_refuses_before_first_write() {
        let budget = RequestBudget {
            started: Instant::now() - Duration::from_secs(2),
            deadline: Instant::now() - Duration::from_secs(1),
            context: None,
        };
        let mut writes = 0;
        let result: Result<(), String> = deliver_once(&budget, |delivery| {
            delivery.begin()?;
            writes += 1;
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(writes, 0);
    }
    #[test]
    fn uncertain_delivery_preserves_failure_and_never_retries() {
        let start = Instant::now();
        let context =
            crate::operation_context::OperationContext::new(start + Duration::from_secs(30));
        let budget =
            RequestBudget::at(start, Duration::from_secs(30), Some(context.clone())).unwrap();
        let mut attempts = 0;
        let result: Result<(), String> = deliver_once(&budget, |delivery| {
            delivery.begin()?;
            attempts += 1;
            Err("injected partial request failure".into())
        });
        assert_eq!(result.unwrap_err(), "injected partial request failure");
        assert_eq!(attempts, 1);
        assert!(context.recovery_required());
        // This local uncertainty flag is not remote cancellation or retirement.
    }
    use std::net::TcpListener;

    pub(super) struct Fixture(pub(super) PathBuf);
    impl Fixture {
        pub(super) fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("aurum-host-test-{}", crate::random::session_id()));
            std::fs::create_dir(&path).unwrap();
            Self(crate::project::clean_path(path.canonicalize().unwrap()))
        }
        fn metadata_path(&self) -> PathBuf {
            self.0.join(".aurum/native/host.json")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn registration_is_exclusive_and_keeps_credentials_out_of_metadata() {
        let fixture = Fixture::new();
        let registration = Registration::create(&fixture.0, 12345, "private_test_secret").unwrap();
        assert!(fixture.0.join(".aurum/native/host.lock").is_file());
        let bytes = std::fs::read(fixture.metadata_path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("private_test_secret"));
        assert_eq!(metadata(&fixture.0).unwrap().unwrap().port, 12345);
        assert_eq!(token(&fixture.0).unwrap(), "private_test_secret");
        assert!(Registration::create(&fixture.0, 23456, "another_secret").is_err());
        drop(registration);
        assert!(!fixture.metadata_path().exists());
        assert!(!fixture.0.join(".aurum/native/host.token").exists());
        assert!(Registration::create(&fixture.0, 23456, "another_secret").is_ok());
    }

    #[test]
    fn closing_registration_releases_a_retained_descriptor_lease() {
        let fixture = Fixture::new();
        let registration = Registration::create(&fixture.0, 12345, "lease_test_secret").unwrap();
        let retained = registration._lease.try_clone().unwrap();
        drop(registration);
        let next = Registration::create(&fixture.0, 23456, "next_secret").unwrap();
        drop(next);
        drop(retained);
    }

    #[test]
    fn invalid_registration_creates_no_project_state() {
        let fixture = Fixture::new();
        assert!(Registration::create(&fixture.0, 0, "secret").is_err());
        assert!(Registration::create(&fixture.0, 12345, "header\r\ninjection").is_err());
        assert!(!fixture.0.join(".aurum").exists());
    }

    #[test]
    fn malformed_or_foreign_metadata_is_refused_without_mutation() {
        let fixture = Fixture::new();
        let _registration = Registration::create(&fixture.0, 12345, "test_secret").unwrap();
        let original = std::fs::read(fixture.metadata_path()).unwrap();
        let mut value: Value = serde_json::from_slice(&original).unwrap();
        for field in ["version", "port", "owner"] {
            let mut changed = value.clone();
            changed[field] = Value::Null;
            let bytes = serde_json::to_vec(&changed).unwrap();
            std::fs::write(fixture.metadata_path(), &bytes).unwrap();
            assert!(metadata(&fixture.0).is_err());
            assert_eq!(std::fs::read(fixture.metadata_path()).unwrap(), bytes);
        }
        value["owner"]["project"] = json!(fixture.0.join("different"));
        std::fs::write(fixture.metadata_path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(metadata(&fixture.0).is_err());
        std::fs::write(fixture.metadata_path(), vec![b' '; 8193]).unwrap();
        assert!(metadata(&fixture.0).is_err());
        std::fs::write(fixture.metadata_path(), original).unwrap();
    }

    #[test]
    fn credential_validation_does_not_echo_secret_content() {
        let fixture = Fixture::new();
        let _registration = Registration::create(&fixture.0, 12345, "valid_secret").unwrap();
        let path = fixture.0.join(".aurum/native/host.token");
        for bytes in [b"sensitive\r\nheader".to_vec(), vec![b'a'; 4097], vec![255]] {
            std::fs::write(&path, bytes).unwrap();
            let error = token(&fixture.0).unwrap_err();
            assert!(!error.contains("sensitive"));
            assert!(error.len() < 100);
        }
    }

    #[test]
    fn scope_probe_requires_matching_project_pid_port_and_protocol() {
        let fixture = Fixture::new();
        for mismatch in ["none", "project", "pid", "port", "protocol"] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let registration = Registration::create(&fixture.0, port, "scope_test_secret").unwrap();
            let endpoint = metadata(&fixture.0).unwrap().unwrap();
            let mut value = json!({"project":fixture.0,"pid":endpoint.owner.pid,"port":port,"protocol":"aurum-native-host/1"});
            if mismatch != "none" {
                value[mismatch] = json!("incorrect");
            }
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 4096);
                }
                assert!(String::from_utf8(request)
                    .unwrap()
                    .contains("X-Aurum-Token: scope_test_secret"));
                let body = serde_json::to_vec(&value).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            });
            assert_eq!(
                verify_scope(&fixture.0, &endpoint, "scope_test_secret").is_ok(),
                mismatch == "none"
            );
            worker.join().unwrap();
            drop(registration);
        }
    }

    #[cfg(windows)]
    #[test]
    fn live_unverifiable_identity_is_not_an_absent_host() {
        let fixture = Fixture::new();
        let _registration = Registration::create(&fixture.0, 12345, "test_secret").unwrap();
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(fixture.metadata_path()).unwrap()).unwrap();
        value["owner"]["executable"] = json!(fixture.0.join("unrelated.exe"));
        std::fs::write(fixture.metadata_path(), serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(metadata(&fixture.0)
            .err()
            .unwrap()
            .contains("cannot be verified"));
    }

    #[cfg(windows)]
    #[test]
    fn listener_ownership_rejects_another_pid() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(listener_owned(port, std::process::id()));
        assert!(!listener_owned(port, u32::MAX));
    }

    #[cfg(windows)]
    #[test]
    fn missing_status_and_stop_do_not_start_or_write_a_host() {
        let fixture = Fixture::new();
        assert_eq!(
            route(&fixture.0, &json!({"op":"native_status"})).unwrap()["running"],
            false
        );
        assert_eq!(
            route(&fixture.0, &json!({"op":"native_host_stop"})).unwrap()["stopped"],
            false
        );
        assert!(!fixture.0.join(".aurum").exists());
    }

    #[cfg(unix)]
    #[test]
    fn registration_restricts_existing_credential_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let directory = fixture.0.join(".aurum/native");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("host.token");
        std::fs::write(&path, "old_secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _registration = Registration::create(&fixture.0, 12345, "new_secret").unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
