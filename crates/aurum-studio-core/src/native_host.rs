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
    }
}
pub fn route(root: &Path, input: &Value) -> Result<Value, String> {
    if !cfg!(windows) {
        return Err("managed native sessions are currently Windows-qualified".into());
    }
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())?;
    let op = input["op"]
        .as_str()
        .ok_or("native operation must be a string")?;
    let mut endpoint = metadata(&root)?;
    if endpoint.is_none() {
        if op == "native_status" {
            return Ok(json!({"ok":true,"running":false,"host_available":false}));
        }
        if matches!(op, "native_stop" | "native_host_stop") {
            return Ok(json!({"ok":true,"stopped":false,"host_available":false}));
        }
        if !matches!(op, "native_start" | "native_recover") {
            return Err("no native controller is running; start a native session first".into());
        }
        Project::open(&root).map_err(|error| error.to_string())?;
        launch(&root)?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while endpoint.is_none() {
            if Instant::now() >= deadline {
                return Err("native controller did not become ready within 20 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(50));
            endpoint = metadata(&root)?;
        }
    }
    let endpoint = endpoint.ok_or("native controller unavailable")?;
    #[cfg(windows)]
    if !listener_owned(endpoint.port, endpoint.owner.pid) {
        return Err("native controller port does not belong to its recorded process".into());
    }
    let secret = token(&root)?;
    verify_scope(&root, &endpoint, &secret)?;
    let mut request = input.clone();
    request["project"] = json!(root);
    let body = serde_json::to_vec(&request).map_err(|_| "native request encoding failed")?;
    if body.len() > 2 * 1024 * 1024 {
        return Err("native routed request exceeds 2 MiB".into());
    }
    let budget = if matches!(op, "native_start" | "native_replace" | "native_recover") {
        Duration::from_secs(240)
    } else {
        Duration::from_secs(15)
    };
    let deadline = Instant::now() + budget;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|_| "native controller connection failed")?;
    stream
        .set_write_timeout(Some(budget))
        .map_err(|_| "native controller write timeout failed")?;
    let path = if op == "native_host_stop" {
        "/api/stop"
    } else {
        "/api/project"
    };
    let header=format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Aurum-Token: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",endpoint.port,secret,body.len());
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|_| "native controller request failed")?;
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("native controller operation exceeded its deadline".into());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| "native controller read timeout failed")?;
        let count = stream
            .read(&mut chunk)
            .map_err(|_| "native controller response failed")?;
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
    let header =
        std::str::from_utf8(&bytes[..boundary]).map_err(|_| "invalid native controller headers")?;
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
}
fn verify_scope(root: &Path, endpoint: &Endpoint, secret: &str) -> Result<(), String> {
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port)),
        Duration::from_secs(2),
    )
    .map_err(|_| "native controller scope probe failed")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|_| "native controller scope timeout failed")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|_| "native controller scope timeout failed")?;
    let header=format!("GET /api/native_host HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Aurum-Token: {}\r\nConnection: close\r\n\r\n",endpoint.port,secret);
    stream
        .write_all(header.as_bytes())
        .map_err(|_| "native controller scope request failed")?;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("native controller scope deadline exceeded".into());
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| "native scope timeout failed")?;
        let count = stream
            .read(&mut chunk)
            .map_err(|_| "native controller scope response failed")?;
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
    Ok(())
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
mod tests {
    use super::*;
    use std::net::TcpListener;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
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
