#![cfg_attr(windows, windows_subsystem = "windows")]

mod studio_browser;

use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const STARTUP_SECONDS: u64 = 15;
const OUTPUT_BYTES: usize = 64 * 1024;
const LINE_BYTES: usize = 4096;
const RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
enum Failure {
    Arguments,
    Platform,
    Layout,
    Runtime,
    Version,
    Spawn,
    Output,
    Deadline,
    Backend,
    Identity,
    Browser,
    BrowserConfiguration,
    Cleanup,
    BrowserUnverified,
    Receipt,
}
impl Failure {
    fn code(self) -> &'static str {
        match self {
            Self::Arguments => "invalid_arguments",
            Self::Platform => "unsupported_platform",
            Self::Layout => "missing_or_unsafe_cli_layout",
            Self::Runtime => "missing_or_unsafe_bundled_runtime",
            Self::Version => "cli_launcher_version_mismatch",
            Self::Spawn => "cli_start_failed",
            Self::Output => "startup_output_invalid_or_exceeded_limit",
            Self::Deadline => "startup_deadline_expired",
            Self::Backend => "studio_backend_exited",
            Self::Identity => "studio_readiness_identity_unverified",
            Self::Browser => "default_browser_failed",
            Self::BrowserConfiguration => "browser_configuration_invalid",
            Self::Cleanup => "owned_startup_exit_unverified",
            Self::BrowserUnverified => "browser_delivery_unverified",
            Self::Receipt => "readiness_receipt_publication_failed",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::Arguments => "Use no arguments to open Studio, or --check, --version, --help.",
            Self::Platform => "This Studio launcher is supported on Windows. Use the Aurum CLI on this platform.",
            Self::Layout => "Aurum Studio cannot find its adjacent bin\\aurum.exe. Extract the complete package into a new writable local folder.",
            Self::Runtime => "The bundled full Godot runtime is missing or inaccessible. Extract the complete Studio package again.",
            Self::Version => "The launcher and CLI versions do not match. Keep all files from the same Studio release together.",
            Self::Spawn => "Windows could not start the adjacent Aurum CLI. Check that the package files are accessible.",
            Self::Output => "Studio returned invalid or excessive startup output. Check the complete package before trying again.",
            Self::Deadline => "Studio did not become ready within 15 seconds. Check the package and project registry before trying again.",
            Self::Backend => "Studio exited before becoming ready. Check the package and project registry. launcher-status.json contains a compact failure category.",
            Self::Identity => "Studio readiness could not be verified. Close an older Studio version if it is already running, then try again.",
            Self::Browser => "Windows could not open the default browser. Check the default HTTP browser application, then try again.",
            Self::BrowserConfiguration => "Check AURUM_STUDIO_BROWSER_EXECUTABLE and AURUM_STUDIO_BROWSER_PROFILE. Set both to an absolute ordinary .exe and an existing dedicated Chromium profile directory, or remove both settings.",
            Self::Cleanup => "Studio startup failed, and its owned process exit could not be verified. Check launcher-status.json before trying again.",
            Self::BrowserUnverified => "Windows did not acknowledge the browser request within the startup budget. A browser window may already have opened. The request helper was retired.",
            Self::Receipt => "Studio could not publish its bounded readiness receipt. Check that the package folder is writable. A refused replacement preserves the previous receipt.",
        }
    }
}

struct Layout {
    root: PathBuf,
    cli: PathBuf,
    runtime: PathBuf,
    _guards: Vec<File>,
}
fn check_deadline(deadline: Instant) -> Result<(), Failure> {
    if Instant::now() >= deadline {
        Err(Failure::Deadline)
    } else {
        Ok(())
    }
}
fn ordinary(path: &Path, directory: bool) -> Result<File, Failure> {
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|_| Failure::Layout)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(Failure::Layout);
            }
        }
        if metadata.file_type().is_symlink() {
            return Err(Failure::Layout);
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .share_mode(1)
            .custom_flags(0x00200000 | if directory { 0x02000000 } else { 0 });
    }
    let file = options.open(path).map_err(|_| Failure::Layout)?;
    let metadata = file.metadata().map_err(|_| Failure::Layout)?;
    if metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
        return Err(Failure::Layout);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFinalPathNameByHandleW(
                handle: *mut std::ffi::c_void,
                name: *mut u16,
                size: u32,
                flags: u32,
            ) -> u32;
        }
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(Failure::Layout);
        }
        let mut name = [0u16; 32768];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                name.as_mut_ptr(),
                name.len() as u32,
                0,
            )
        };
        if length == 0 || length as usize >= name.len() {
            return Err(Failure::Layout);
        }
        let actual = String::from_utf16(&name[..length as usize]).map_err(|_| Failure::Layout)?;
        let expected = path.to_string_lossy().replace('/', "\\");
        let actual = actual.strip_prefix(r"\\?\").unwrap_or(&actual);
        let expected = expected.strip_prefix(r"\\?\").unwrap_or(&expected);
        if !local_root(Path::new(actual)) || !actual.eq_ignore_ascii_case(expected) {
            return Err(Failure::Layout);
        }
    }
    Ok(file)
}
fn local_root(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let ordinary_drive = text.as_bytes().get(1) == Some(&b':')
        && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && text
            .as_bytes()
            .get(2)
            .is_some_and(|byte| matches!(*byte, b'\\' | b'/'))
        && !text.chars().any(char::is_control);
    if !ordinary_drive {
        return false;
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        let volume: Vec<u16> = format!("{}:\\", text.as_bytes()[0] as char)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        matches!(unsafe { GetDriveTypeW(volume.as_ptr()) }, 2 | 3 | 6)
    }
    #[cfg(not(windows))]
    ordinary_drive
}
fn layout(deadline: Instant) -> Result<Layout, Failure> {
    check_deadline(deadline)?;
    let executable = std::env::current_exe().map_err(|_| Failure::Layout)?;
    let root = executable.parent().ok_or(Failure::Layout)?.to_path_buf();
    if !local_root(&root) {
        return Err(Failure::Layout);
    }
    let mut guards = vec![ordinary(&root, true)?];
    guards.push(ordinary(&root.join("bin"), true)?);
    let cli = root.join("bin/aurum.exe");
    let cli_file = ordinary(&cli, false)?;
    if !(1..=128 * 1024 * 1024).contains(&cli_file.metadata().map_err(|_| Failure::Layout)?.len()) {
        return Err(Failure::Layout);
    }
    guards.push(cli_file);
    let runtime_directory = root.join("runtime");
    guards.push(ordinary(&runtime_directory, true).map_err(|_| Failure::Runtime)?);
    let runtime = runtime_directory.join("godot.exe");
    let runtime_file = ordinary(&runtime, false).map_err(|_| Failure::Runtime)?;
    if !(1024 * 1024..=512 * 1024 * 1024)
        .contains(&runtime_file.metadata().map_err(|_| Failure::Runtime)?.len())
    {
        return Err(Failure::Runtime);
    }
    guards.push(runtime_file);
    check_deadline(deadline)?;
    Ok(Layout {
        root,
        cli,
        runtime,
        _guards: guards,
    })
}
fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
// Credentials are deliberately not Debug/Serialize and never enter receipts.
struct Endpoint {
    url: String,
    token: String,
    port: u16,
    reused: bool,
}
fn endpoint(bytes: &[u8]) -> Result<Endpoint, Failure> {
    if bytes.len() > LINE_BYTES {
        return Err(Failure::Output);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Failure::Output)?;
    let object = value.as_object().ok_or(Failure::Output)?;
    let reused = match object.get("reused") {
        Some(value) => value.as_bool().ok_or(Failure::Identity)?,
        None => false,
    };
    let expected: &[&str] = if reused {
        &["url", "reused"]
    } else {
        &["url", "port", "host"]
    };
    if object.len() != expected.len() || object.keys().any(|key| !expected.contains(&key.as_str()))
    {
        return Err(Failure::Identity);
    }
    let url = value["url"]
        .as_str()
        .filter(|url| url.len() <= 200)
        .ok_or(Failure::Identity)?;
    let tail = url
        .strip_prefix("http://127.0.0.1:")
        .ok_or(Failure::Identity)?;
    let (port, token) = tail.split_once("/?t=").ok_or(Failure::Identity)?;
    if !port.bytes().all(|byte| byte.is_ascii_digit())
        || token.len() != 64
        || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Failure::Identity);
    }
    let port: u16 = port.parse().map_err(|_| Failure::Identity)?;
    if port == 0
        || (!reused
            && (value["host"] != "127.0.0.1" || value["port"].as_u64() != Some(port as u64)))
    {
        return Err(Failure::Identity);
    }
    Ok(Endpoint {
        url: url.to_string(),
        token: token.to_string(),
        port,
        reused,
    })
}
fn read_line(
    mut pipe: impl Read + Send + 'static,
    capture: Arc<AtomicBool>,
    sender: mpsc::SyncSender<Result<Vec<u8>, Failure>>,
) {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut total = 0usize;
        let mut buffer = [0u8; 4096];
        let mut emitted = false;
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            if !capture.load(Ordering::Relaxed) || emitted {
                continue;
            }
            total = total.saturating_add(read);
            if total > OUTPUT_BYTES {
                let _ = sender.try_send(Err(Failure::Output));
                emitted = true;
                continue;
            }
            for byte in &buffer[..read] {
                if *byte == b'\n' {
                    let _ = sender.try_send(Ok(std::mem::take(&mut bytes)));
                    emitted = true;
                    break;
                }
                if bytes.len() >= LINE_BYTES {
                    let _ = sender.try_send(Err(Failure::Output));
                    emitted = true;
                    break;
                }
                bytes.push(*byte);
            }
        }
        if !emitted && capture.load(Ordering::Relaxed) {
            let _ = sender.try_send(Err(Failure::Output));
        }
    });
}
fn stderr_drain(mut pipe: impl Read + Send + 'static, count: Arc<AtomicUsize>) {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            let read = match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let _ = count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(read).min(OUTPUT_BYTES))
            });
        }
    });
}
fn terminate_owned(child: &mut Child) -> bool {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    false
}
fn probe_version(layout: &Layout, deadline: Instant) -> Result<(), Failure> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(3));
    let mut child = command(&layout.cli)
        .arg("--version")
        .current_dir(&layout.root)
        .spawn()
        .map_err(|_| Failure::Spawn)?;
    let capture = Arc::new(AtomicBool::new(true));
    let (sender, receiver) = mpsc::sync_channel(1);
    read_line(
        child.stdout.take().ok_or(Failure::Spawn)?,
        capture.clone(),
        sender,
    );
    stderr_drain(
        child.stderr.take().ok_or(Failure::Spawn)?,
        Arc::new(AtomicUsize::new(0)),
    );
    let result = (|| {
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Version)??;
        if String::from_utf8(line)
            .map_err(|_| Failure::Version)?
            .trim()
            != format!("aurum {VERSION}")
        {
            return Err(Failure::Version);
        }
        loop {
            check_deadline(deadline)?;
            if let Some(status) = child.try_wait().map_err(|_| Failure::Version)? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(Failure::Version)
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    capture.store(false, Ordering::Relaxed);
    if result.is_err() && !terminate_owned(&mut child) {
        return Err(Failure::Cleanup);
    }
    result
}
#[cfg(windows)]
fn listener_pid(port: u16) -> Option<u32> {
    #[link(name = "iphlpapi")]
    unsafe extern "system" {
        fn GetExtendedTcpTable(
            table: *mut std::ffi::c_void,
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
        return None;
    }
    let mut table = vec![0u32; (size as usize).div_ceil(4)];
    if unsafe { GetExtendedTcpTable(table.as_mut_ptr().cast(), &mut size, 0, 2, 3, 0) } != 0 {
        return None;
    }
    let count = table[0] as usize;
    if count > table.len().saturating_sub(1) / 6 {
        return None;
    }
    (0..count).find_map(|index| {
        let row = &table[1 + index * 6..1 + (index + 1) * 6];
        (row[0] == 2
            && row[1] == u32::from_ne_bytes([127, 0, 0, 1])
            && u16::from_be(row[2] as u16) == port)
            .then_some(row[5])
    })
}
#[cfg(not(windows))]
fn listener_pid(_: u16) -> Option<u32> {
    None
}
fn request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    deadline: Instant,
) -> Result<Value, Failure> {
    check_deadline(deadline)?;
    if endpoint.url != format!("http://127.0.0.1:{}/?t={}", endpoint.port, endpoint.token) {
        return Err(Failure::Identity);
    }
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(2));
    let mut stream = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, endpoint.port).into(),
        remaining,
    )
    .map_err(|_| Failure::Identity)?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| Failure::Identity)?;
    stream
        .set_write_timeout(Some(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(2)),
        ))
        .map_err(|_| Failure::Identity)?;
    let outgoing = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Aurum-Token: {}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", endpoint.port, endpoint.token);
    let mut written = 0;
    while written < outgoing.len() {
        check_deadline(deadline)?;
        stream
            .set_write_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2)),
            ))
            .map_err(|_| Failure::Identity)?;
        let count = stream.write(&outgoing.as_bytes()[written..]).map_err(|_| {
            if Instant::now() >= deadline {
                Failure::Deadline
            } else {
                Failure::Identity
            }
        })?;
        if count == 0 {
            return Err(Failure::Identity);
        }
        written += count;
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        check_deadline(deadline)?;
        stream
            .set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2)),
            ))
            .map_err(|_| {
                if Instant::now() >= deadline {
                    Failure::Deadline
                } else {
                    Failure::Identity
                }
            })?;
        let limit = buffer
            .len()
            .min(RESPONSE_BYTES.saturating_sub(bytes.len()).saturating_add(1));
        let read = stream.read(&mut buffer[..limit]).map_err(|_| {
            if Instant::now() >= deadline {
                Failure::Deadline
            } else {
                Failure::Identity
            }
        })?;
        if read == 0 {
            break;
        }
        if bytes.len() + read > RESPONSE_BYTES {
            return Err(Failure::Identity);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    check_deadline(deadline)?;
    let split = bytes
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .ok_or(Failure::Identity)?;
    if !bytes.starts_with(b"HTTP/1.1 200 ") {
        return Err(Failure::Identity);
    }
    serde_json::from_slice(&bytes[split + 4..]).map_err(|_| Failure::Identity)
}
struct ConfiguredBrowser {
    #[cfg(windows)]
    choice: studio_browser::BrowserChoice,
    _guards: [File; 2],
}

fn browser_path(path: &Path, directory: bool) -> Result<File, Failure> {
    let guard = ordinary(path, directory).map_err(|_| Failure::BrowserConfiguration)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        for ancestor in path.ancestors() {
            let metadata =
                std::fs::symlink_metadata(ancestor).map_err(|_| Failure::BrowserConfiguration)?;
            if metadata.file_attributes() & 0x440 != 0 {
                return Err(Failure::BrowserConfiguration);
            }
        }
        if guard
            .metadata()
            .map_err(|_| Failure::BrowserConfiguration)?
            .file_attributes()
            & 0x440
            != 0
        {
            return Err(Failure::BrowserConfiguration);
        }
    }
    Ok(guard)
}

fn browser_configuration() -> Result<Option<ConfiguredBrowser>, Failure> {
    let executable = std::env::var_os(studio_browser::EXECUTABLE_VARIABLE);
    let profile = std::env::var_os(studio_browser::PROFILE_VARIABLE);
    let Some(choice) = studio_browser::parse(executable.as_deref(), profile.as_deref())
        .map_err(|_| Failure::BrowserConfiguration)?
    else {
        return Ok(None);
    };
    let guards = [
        browser_path(choice.executable(), false)?,
        browser_path(choice.profile(), true)?,
    ];
    Ok(Some(ConfiguredBrowser {
        #[cfg(windows)]
        choice,
        _guards: guards,
    }))
}

#[cfg(windows)]
fn browser_api(endpoint: &Endpoint, deadline: Instant) -> Result<(), Failure> {
    let configuration = browser_configuration()?;
    check_deadline(deadline)?;
    if let Some(configuration) = configuration {
        // This acknowledges a request, as ShellExecute does. It does not claim browser ownership.
        return command(configuration.choice.executable())
            .args(configuration.choice.arguments(&endpoint.url))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|_| Failure::Browser);
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            window: *mut std::ffi::c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }
    let url: Vec<u16> = endpoint.url.encode_utf16().chain(Some(0)).collect();
    let open: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    if unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            open.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    } <= 32
    {
        Err(Failure::Browser)
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn browser_api(_: &Endpoint, _: Instant) -> Result<(), Failure> {
    Err(Failure::Browser)
}
#[cfg(windows)]
fn helper_parent() -> Option<u32> {
    #[repr(C)]
    struct Entry {
        size: u32,
        usage: u32,
        pid: u32,
        heap: usize,
        module: u32,
        threads: u32,
        parent: u32,
        priority: i32,
        flags: u32,
        name: [u16; 260],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
        fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut Entry) -> i32;
        fn Process32NextW(snapshot: *mut std::ffi::c_void, entry: *mut Entry) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn QueryFullProcessImageNameW(
            process: *mut std::ffi::c_void,
            flags: u32,
            name: *mut u16,
            length: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    let snapshot = unsafe { CreateToolhelp32Snapshot(2, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return None;
    }
    let mut entry = Entry {
        size: std::mem::size_of::<Entry>() as u32,
        usage: 0,
        pid: 0,
        heap: 0,
        module: 0,
        threads: 0,
        parent: 0,
        priority: 0,
        flags: 0,
        name: [0; 260],
    };
    let mut available = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    let mut parent = None;
    for _ in 0..65536 {
        if !available {
            break;
        }
        if entry.pid == std::process::id() {
            parent = Some(entry.parent);
            break;
        }
        available = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    let parent = parent?;
    let process = unsafe { OpenProcess(0x1000, 0, parent) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 32768];
    let mut length = buffer.len() as u32;
    let success =
        unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } != 0;
    unsafe {
        CloseHandle(process);
    }
    if !success || length as usize >= buffer.len() {
        return None;
    }
    let actual = String::from_utf16(&buffer[..length as usize]).ok()?;
    let expected = std::env::current_exe()
        .ok()?
        .to_string_lossy()
        .replace('/', "\\");
    let expected = expected.strip_prefix(r"\\?\").unwrap_or(&expected);
    actual.eq_ignore_ascii_case(expected).then_some(parent)
}
#[cfg(not(windows))]
fn helper_parent() -> Option<u32> {
    None
}
fn helper_request(value: Value, parent: u32) -> Result<(Endpoint, u32), Failure> {
    let mut object = value.as_object().cloned().ok_or(Failure::Identity)?;
    if object.len() != 6
        || object.remove("version") != Some(json!(VERSION))
        || object.remove("parent_pid") != Some(json!(parent))
    {
        return Err(Failure::Identity);
    }
    let owner = object
        .remove("expected_pid")
        .and_then(|value| value.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or(Failure::Identity)?;
    let endpoint =
        endpoint(&serde_json::to_vec(&Value::Object(object)).map_err(|_| Failure::Identity)?)?;
    Ok((endpoint, owner))
}
fn browser_helper() -> Result<(), Failure> {
    let parent = helper_parent().ok_or(Failure::Arguments)?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take((LINE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Identity)?;
    if bytes.len() > LINE_BYTES {
        return Err(Failure::Output);
    }
    let (endpoint, owner) = helper_request(
        serde_json::from_slice(&bytes).map_err(|_| Failure::Identity)?,
        parent,
    )?;
    let deadline = Instant::now() + Duration::from_secs(2);
    if listener_pid(endpoint.port) != Some(owner) {
        return Err(Failure::Identity);
    }
    let state = request(&endpoint, "GET", "/api/state", deadline)?;
    if state["studio"] != VERSION || listener_pid(endpoint.port) != Some(owner) {
        return Err(Failure::Identity);
    }
    check_deadline(deadline)?;
    browser_api(&endpoint, deadline)
}
fn browser(endpoint: &Endpoint, owner: u32, deadline: Instant) -> Result<(), Failure> {
    check_deadline(deadline)?;
    // Fail closed before creating the helper; --check and other non-launch modes never reach here.
    let _configuration = browser_configuration()?;
    check_deadline(deadline)?;
    let executable = std::env::current_exe().map_err(|_| Failure::Browser)?;
    let mut launch = command(&executable);
    launch
        .arg("--internal-browser-helper")
        .stdin(Stdio::piped());
    let mut child = launch.spawn().map_err(|_| Failure::Browser)?;
    let capture = Arc::new(AtomicBool::new(true));
    let (sender, receiver) = mpsc::sync_channel(1);
    read_line(
        child.stdout.take().ok_or(Failure::Browser)?,
        capture.clone(),
        sender,
    );
    stderr_drain(
        child.stderr.take().ok_or(Failure::Browser)?,
        Arc::new(AtomicUsize::new(0)),
    );
    let result = (|| {
        let bytes=serde_json::to_vec(&json!({"url":endpoint.url,"port":endpoint.port,"host":"127.0.0.1","version":VERSION,"parent_pid":std::process::id(),"expected_pid":owner})).map_err(|_|Failure::Browser)?;
        if bytes.len() > LINE_BYTES {
            return Err(Failure::Browser);
        }
        child
            .stdin
            .take()
            .ok_or(Failure::Browser)?
            .write_all(&bytes)
            .map_err(|_| Failure::Browser)?;
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::BrowserUnverified)??;
        let value: Value = serde_json::from_slice(&line).map_err(|_| Failure::BrowserUnverified)?;
        if value.as_object().is_none_or(|object| object.len() != 1) || value["ok"] != true {
            return Err(Failure::Browser);
        }
        loop {
            if Instant::now() >= deadline {
                return Err(Failure::BrowserUnverified);
            }
            if let Some(status) = child.try_wait().map_err(|_| Failure::BrowserUnverified)? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(Failure::Browser)
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    capture.store(false, Ordering::Relaxed);
    if result.is_err() && !terminate_owned(&mut child) {
        return Err(Failure::Cleanup);
    }
    result
}
#[cfg(windows)]
fn held_path(file: &File, expected: &Path, directory: bool) -> bool {
    use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(
            handle: *mut std::ffi::c_void,
            name: *mut u16,
            size: u32,
            flags: u32,
        ) -> u32;
    }
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.file_attributes() & 0x400 != 0
        || metadata.is_dir() != directory
        || (!directory && !metadata.is_file())
    {
        return false;
    }
    let mut buffer = [0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if length == 0 || length as usize >= buffer.len() {
        return false;
    }
    let Ok(actual) = String::from_utf16(&buffer[..length as usize]) else {
        return false;
    };
    let actual = actual.strip_prefix(r"\\?\").unwrap_or(&actual);
    let expected = expected.to_string_lossy().replace('/', "\\");
    let expected = expected.strip_prefix(r"\\?\").unwrap_or(&expected);
    local_root(Path::new(actual)) && actual.eq_ignore_ascii_case(expected)
}
#[cfg(all(windows, target_pointer_width = "64"))]
fn publish_receipt(
    root: &Path,
    guard: &File,
    bytes: &[u8],
    stage_id: u32,
    deadline: Instant,
) -> Result<(), Failure> {
    use std::os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    };
    #[repr(C)]
    struct IoStatus {
        status: isize,
        information: usize,
    }
    #[repr(C)]
    struct RenameInfo {
        replace: u8,
        directory: *mut std::ffi::c_void,
        length: u32,
        first: u16,
    }
    const _: () = {
        assert!(std::mem::size_of::<IoStatus>() == 16 && std::mem::align_of::<IoStatus>() == 8);
        assert!(std::mem::size_of::<RenameInfo>() == 24 && std::mem::align_of::<RenameInfo>() == 8);
        assert!(
            std::mem::offset_of!(RenameInfo, directory) == 8
                && std::mem::offset_of!(RenameInfo, length) == 16
                && std::mem::offset_of!(RenameInfo, first) == 20
        );
    };
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSetInformationFile(
            file: *mut std::ffi::c_void,
            io: *mut IoStatus,
            information: *mut std::ffi::c_void,
            length: u32,
            kind: u32,
        ) -> i32;
    }
    let deadline = deadline.min(Instant::now() + Duration::from_millis(500));
    if bytes.len() > 4096 || !held_path(guard, root, true) {
        return Err(Failure::Receipt);
    }
    let target = root.join("launcher-status.json");
    match std::fs::symlink_metadata(&target) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_attributes() & 0x400 != 0
                || metadata.len() > 4096
                || metadata.permissions().readonly() =>
        {
            return Err(Failure::Receipt)
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Failure::Receipt),
    }
    let mut leftovers = 0;
    for (index, entry) in std::fs::read_dir(root)
        .map_err(|_| Failure::Receipt)?
        .enumerate()
    {
        check_deadline(deadline)?;
        if index >= 256 {
            return Err(Failure::Receipt);
        }
        let entry = entry.map_err(|_| Failure::Receipt)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("launcher-status.") && name.ends_with(".tmp") {
            leftovers += 1;
            let metadata = std::fs::symlink_metadata(entry.path()).map_err(|_| Failure::Receipt)?;
            if leftovers >= 32
                || !metadata.is_file()
                || metadata.file_attributes() & 0x400 != 0
                || metadata.len() > 4096
            {
                return Err(Failure::Receipt);
            }
        }
    }
    let staging = root.join(format!("launcher-status.{stage_id}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(0x40010080)
        .share_mode(1)
        .custom_flags(0x00200000)
        .open(&staging)
        .map_err(|_| Failure::Receipt)?;
    if !held_path(&file, &staging, false) {
        return Err(Failure::Receipt);
    }
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Failure::Receipt)?;
    let name: Vec<u16> = "launcher-status.json".encode_utf16().collect();
    let length = std::mem::size_of::<RenameInfo>() + name.len() * 2;
    let mut packet = vec![0u64; length.div_ceil(8)];
    unsafe {
        packet.as_mut_ptr().cast::<RenameInfo>().write(RenameInfo {
            replace: 1,
            directory: std::ptr::null_mut(),
            length: (name.len() * 2) as u32,
            first: 0,
        });
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            packet
                .as_mut_ptr()
                .cast::<u8>()
                .add(std::mem::offset_of!(RenameInfo, first))
                .cast::<u16>(),
            name.len(),
        );
    }
    loop {
        check_deadline(deadline)?;
        if !held_path(guard, root, true)
            || !held_path(&file, &staging, false)
            || file.metadata().map_err(|_| Failure::Receipt)?.len() != bytes.len() as u64
        {
            return Err(Failure::Receipt);
        }
        let mut io = IoStatus {
            status: 0,
            information: 0,
        };
        let status = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut io,
                packet.as_mut_ptr().cast(),
                length as u32,
                10,
            )
        };
        if status == 0 {
            if !held_path(guard, root, true) || !held_path(&file, &target, false) {
                return Err(Failure::Receipt);
            }
            return Ok(());
        }
        // Synchronous source handle; accept only exact STATUS_SUCCESS.
        // Pending/uncertain status preserves the private stage and old target.
        if !matches!(status as u32, 0xc0000022 | 0xc0000043 | 0xc0000054) {
            return Err(Failure::Receipt);
        }
        if Instant::now() + Duration::from_millis(10) >= deadline {
            if held_path(guard, root, true) && held_path(&file, &staging, false) {
                let deleting = 1u8;
                let mut io = IoStatus {
                    status: 0,
                    information: 0,
                };
                // Delete-on-close uses only our retained create_new handle.
                let _ = unsafe {
                    NtSetInformationFile(
                        file.as_raw_handle(),
                        &mut io,
                        (&deleting as *const u8).cast_mut().cast(),
                        1,
                        13,
                    )
                };
            }
            return Err(Failure::Receipt);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn receipt(layout: &Layout, value: &Value, deadline: Instant) -> Result<(), Failure> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| Failure::Receipt)?;
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        publish_receipt(
            &layout.root,
            layout._guards.first().ok_or(Failure::Receipt)?,
            &bytes,
            std::process::id(),
            deadline,
        )
    }
    #[cfg(not(all(windows, target_pointer_width = "64")))]
    {
        let _ = (layout, bytes, deadline);
        Err(Failure::Receipt)
    }
}
fn launch(layout: &Layout, deadline: Instant) -> Result<(), Failure> {
    let started = Instant::now();
    let mut child = command(&layout.cli)
        .args(["studio", "--json", "--port", "0", "--godot"])
        .arg(&layout.runtime)
        .env("AURUM_STUDIO_HOME", &layout.root)
        .current_dir(&layout.root)
        .spawn()
        .map_err(|_| Failure::Spawn)?;
    let child_pid = child.id();
    let capture = Arc::new(AtomicBool::new(true));
    let stderr = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::sync_channel(1);
    read_line(
        child.stdout.take().ok_or(Failure::Spawn)?,
        capture.clone(),
        sender,
    );
    stderr_drain(child.stderr.take().ok_or(Failure::Spawn)?, stderr.clone());
    let mut ready: Option<Endpoint> = None;
    let mut browser_requested = false;
    let result = (|| {
        let line = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| Failure::Deadline)??;
        let endpoint = endpoint(&line)?;
        let owner = listener_pid(endpoint.port).ok_or(Failure::Identity)?;
        if !endpoint.reused && owner != child_pid {
            return Err(Failure::Identity);
        }
        if endpoint.reused {
            loop {
                check_deadline(deadline)?;
                if let Some(status) = child.try_wait().map_err(|_| Failure::Backend)? {
                    if !status.success() {
                        return Err(Failure::Backend);
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        } else if child.try_wait().map_err(|_| Failure::Backend)?.is_some() {
            return Err(Failure::Backend);
        }
        let state = request(&endpoint, "GET", "/api/state", deadline)?;
        if state["studio"] != VERSION
            || listener_pid(endpoint.port) != Some(owner)
            || (!endpoint.reused && child.try_wait().map_err(|_| Failure::Backend)?.is_some())
        {
            return Err(Failure::Identity);
        }
        ready = Some(endpoint);
        browser(ready.as_ref().ok_or(Failure::Identity)?, owner, deadline)?;
        browser_requested = true;
        check_deadline(deadline)?;
        if receipt(
            layout,
            &json!({"ok":true,"launcher_version":VERSION,"cli_version":VERSION,"stage":"browser_requested","launcher_pid":std::process::id(),"child_pid":child_pid,"listener_pid":owner,"reused":ready.as_ref().is_some_and(|endpoint|endpoint.reused),"elapsed_ms":started.elapsed().as_millis(),"stderr_bytes_capped":stderr.load(Ordering::Relaxed),"raw_output_retained":false,"credentials_retained":false}),
            deadline,
        ).is_err() {
            // A diagnostic sidecar cannot undo a verified healthy startup.
            // This warning contains no child output, endpoint or credentials.
            eprintln!("Aurum Studio is ready; its readiness receipt could not be published. The previous receipt remains available if replacement was refused.");
            show_receipt_warning();
        }
        Ok(())
    })();
    capture.store(false, Ordering::Relaxed);
    if let Err(failure) = result {
        let failure = if !matches!(failure, Failure::BrowserUnverified | Failure::Cleanup)
            && Instant::now() >= deadline
        {
            Failure::Deadline
        } else {
            failure
        };
        if let Some(endpoint) = ready
            .as_ref()
            .filter(|endpoint| !endpoint.reused && listener_pid(endpoint.port) == Some(child_pid))
        {
            let _ = request(
                endpoint,
                "POST",
                "/api/stop",
                Instant::now() + Duration::from_secs(2),
            );
        }
        let stopped = terminate_owned(&mut child);
        let _ = receipt(
            layout,
            &json!({"ok":false,"launcher_version":VERSION,"stage":"startup_refused","reason":failure.code(),"browser_effect":if browser_requested{"confirmed"}else if matches!(failure,Failure::BrowserUnverified|Failure::Cleanup){"unverified"}else{"not_confirmed"},"launcher_pid":std::process::id(),"child_pid":child_pid,"owned_child_exit_verified":stopped,"stderr_bytes_capped":stderr.load(Ordering::Relaxed),"raw_output_retained":false,"credentials_retained":false}),
            Instant::now() + Duration::from_millis(500),
        );
        return Err(if stopped { failure } else { Failure::Cleanup });
    }
    if ready.as_ref().is_some_and(|endpoint| endpoint.reused) {
        return Ok(());
    }
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        _ => Err(Failure::Backend),
    }
}
#[cfg(windows)]
fn show_error(failure: Failure) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    let text: Vec<u16> = failure.message().encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "Aurum Studio".encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x10);
    }
}
#[cfg(windows)]
fn show_receipt_warning() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    let text: Vec<u16> = "Aurum Studio is ready, but its startup diagnostics could not be saved. Studio and its browser remain running. A refused replacement preserves the previous diagnostic receipt."
        .encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "Aurum Studio".encode_utf16().chain(Some(0)).collect();
    // User acknowledgement is outside the mandatory startup deadline. No
    // readiness reclassification or host rollback follows this warning.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0x30);
    }
}
#[cfg(not(windows))]
fn show_receipt_warning() {}
#[cfg(not(windows))]
fn show_error(failure: Failure) {
    eprintln!("{}", failure.message());
}
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--internal-owner-access-helper" {
        // No diagnostics serialize the private protocol or owner capability.
        return ExitCode::from(
            if aurum_studio_core::workspace_access::attended_owner_helper().is_ok() {
                0
            } else {
                1
            },
        );
    }
    if args.len() == 1 && args[0] == "--internal-browser-helper" {
        let success = browser_helper().is_ok();
        println!("{}", json!({"ok":success}));
        return ExitCode::from(if success { 0 } else { 1 });
    }
    if args.len() == 1 && args[0] == "--version" {
        println!("aurum-studio {VERSION}");
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && args[0] == "--help" {
        println!("Aurum Studio {VERSION}\nOpen without arguments to launch Studio. --check verifies the adjacent package without launching Studio; --version prints the launcher version.");
        return ExitCode::SUCCESS;
    }
    let check = args.len() == 1 && args[0] == "--check";
    if !args.is_empty() && !check {
        eprintln!("{}", Failure::Arguments.message());
        return ExitCode::from(64);
    }
    let deadline = Instant::now() + Duration::from_secs(STARTUP_SECONDS);
    let result = if !cfg!(windows) {
        Err(Failure::Platform)
    } else {
        layout(deadline).and_then(|layout| {
            probe_version(&layout, deadline)?;
            if check {
                Ok(())
            } else {
                launch(&layout, deadline)
            }
        })
    };
    if check {
        println!(
            "{}",
            match result {
                Ok(()) =>
                    json!({"ok":true,"launcher_version":VERSION,"cli_version":VERSION,"runtime_present":true,"runtime_version":"unverified","launch_performed":false}),
                Err(failure) =>
                    json!({"ok":false,"launcher_version":VERSION,"reason":failure.code(),"launch_performed":false}),
            }
        );
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            if !check {
                show_error(failure);
            }
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Value {
        json!({"url":format!("http://127.0.0.1:12345/?t={}","a".repeat(64)),"port":12345,"host":"127.0.0.1"})
    }
    #[test]
    fn readiness_is_closed_local_typed_and_bounded() {
        assert!(endpoint(&serde_json::to_vec(&valid()).unwrap()).is_ok());
        for (field, value) in [
            ("host", json!("remote.invalid")),
            ("port", json!("12345")),
            ("extra", json!(true)),
            ("reused", json!("true")),
        ] {
            let mut input = valid();
            input[field] = value;
            assert!(endpoint(&serde_json::to_vec(&input).unwrap()).is_err());
        }
        for url in [
            "https://127.0.0.1:12345/?t=abc",
            "http://remote.invalid:12345/?t=abc",
            "http://127.0.0.1:0/?t=abc",
            "http://127.0.0.1:12345/?t=abc&x=1",
        ] {
            let mut input = valid();
            input["url"] = json!(url);
            assert!(endpoint(&serde_json::to_vec(&input).unwrap()).is_err());
        }
        assert!(endpoint(&vec![b' '; LINE_BYTES + 1]).is_err());
        let reuse = json!({"url":valid()["url"],"reused":true});
        assert!(
            endpoint(&serde_json::to_vec(&reuse).unwrap())
                .unwrap()
                .reused
        );
    }
    #[test]
    fn pipe_reader_rejects_oversize_line_without_returning_body() {
        let (sender, receiver) = mpsc::sync_channel(1);
        read_line(
            std::io::Cursor::new(vec![b'x'; LINE_BYTES + 1]),
            Arc::new(AtomicBool::new(true)),
            sender,
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(Failure::Output)
        ));
    }
    #[test]
    fn local_root_refuses_network_and_relative_locations() {
        let local = if cfg!(windows) {
            std::env::current_dir().unwrap()
        } else {
            PathBuf::from("A:/Studio with spaces")
        };
        assert!(local_root(&local));
        let text = local.to_string_lossy();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        assert!(local_root(Path::new(&format!(r"\\?\{text}"))));
        assert!(!local_root(Path::new(r"\\server\Studio")));
        assert!(!local_root(Path::new(r"\\?\UNC\server\Studio")));
        assert!(!local_root(Path::new("Studio")));
    }
    #[test]
    fn private_browser_frame_requires_typed_binding_version_and_parent() {
        let mut input = valid();
        input["version"] = json!(VERSION);
        input["parent_pid"] = json!(10);
        input["expected_pid"] = json!(20);
        assert!(helper_request(input.clone(), 10).is_ok());
        for (field, value) in [
            ("version", json!("0.0.0")),
            ("parent_pid", json!(11)),
            ("expected_pid", json!("20")),
            ("extra", json!(true)),
        ] {
            let mut rejected = input.clone();
            rejected[field] = value;
            assert!(helper_request(rejected, 10).is_err());
        }
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn receipt_writer_helper() {
        let Some(base) = std::env::var_os("AURUM_RECEIPT_TEST_BASE") else {
            return;
        };
        let base = PathBuf::from(base);
        let root = base.join("source with spaces ü");
        let guard = ordinary(&root, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        std::fs::write(base.join(format!("ready.{}", std::process::id())), b"ready").unwrap();
        while !base.join("go").is_file() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let bytes =
            serde_json::to_vec(&json!({"writer_pid":std::process::id(),"test_only":true})).unwrap();
        publish_receipt(&root, &guard, &bytes, std::process::id(), deadline).unwrap();
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    fn create_receipt_test_junction(
        base: &Path,
        cwd: &Path,
        link: &Path,
        outside: &Path,
    ) -> Result<(), u32> {
        use std::os::windows::{
            fs::MetadataExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        };
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateFileW(
                path: *const u16,
                access: u32,
                share: u32,
                security: *mut std::ffi::c_void,
                disposition: u32,
                flags: u32,
                template: *mut std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            fn DeviceIoControl(
                handle: *mut std::ffi::c_void,
                code: u32,
                input: *mut std::ffi::c_void,
                input_bytes: u32,
                output: *mut std::ffi::c_void,
                output_bytes: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        let error = || std::io::Error::last_os_error().raw_os_error().unwrap_or(1) as u32;
        if link != base.join("redirected/launcher-status.json")
            || outside != base.join("outside")
            || cwd != base.join("unrelated cwd")
        {
            return Err(87);
        }
        let normalized = base.to_string_lossy().replace('/', "\\");
        let normalized = normalized.strip_prefix(r"\\?\").unwrap_or(&normalized);
        let temporary = std::env::temp_dir().to_string_lossy().replace('/', "\\");
        let temporary = temporary.strip_prefix(r"\\?\").unwrap_or(&temporary);
        let temporary = format!("{}\\", temporary.trim_end_matches('\\')).to_ascii_lowercase();
        if !normalized.to_ascii_lowercase().starts_with(&temporary)
            || normalized.len() > 1024
            || !local_root(base)
        {
            return Err(123);
        }
        let _base_guard = ordinary(base, true).map_err(|_| 123u32)?;
        let _outside_guard = ordinary(outside, true).map_err(|_| 123u32)?;
        let _cwd_guard = ordinary(cwd, true).map_err(|_| 123u32)?;
        std::fs::create_dir(link).map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        let path = link.to_string_lossy().replace('/', "\\");
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                0x40000080,
                0,
                std::ptr::null_mut(),
                3,
                0x02200000,
                std::ptr::null_mut(),
            )
        };
        if raw.is_null() || raw as isize == -1 {
            return Err(error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // Mount-point ABI: tag/length/reserved (8 bytes), four name-offset/
        // length u16 fields (8 bytes), then two terminated UTF-16 names.
        let printed = outside.to_string_lossy().replace('/', "\\");
        let printed = printed.strip_prefix(r"\\?\").unwrap_or(&printed);
        if printed.len() > 1024 || printed.chars().any(char::is_control) {
            return Err(123);
        }
        let substitute: Vec<u16> = format!(r"\??\{printed}").encode_utf16().collect();
        let print_name: Vec<u16> = printed.encode_utf16().collect();
        let names = (substitute.len() + 1 + print_name.len() + 1) * 2;
        let data_length = 8 + names;
        if 8 + data_length > 16 * 1024 || data_length > u16::MAX as usize {
            return Err(87);
        }
        let mut buffer = vec![0u8; 8 + data_length];
        buffer[..4].copy_from_slice(&0xa0000003u32.to_le_bytes());
        buffer[4..6].copy_from_slice(&(data_length as u16).to_le_bytes());
        buffer[10..12].copy_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
        buffer[12..14].copy_from_slice(&(((substitute.len() + 1) * 2) as u16).to_le_bytes());
        buffer[14..16].copy_from_slice(&((print_name.len() * 2) as u16).to_le_bytes());
        for (index, unit) in substitute
            .iter()
            .chain(std::iter::once(&0))
            .chain(print_name.iter())
            .chain(std::iter::once(&0))
            .enumerate()
        {
            buffer[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let mut returned = 0u32;
        if unsafe {
            DeviceIoControl(
                handle.as_raw_handle(),
                0x000900a4,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(error());
        }
        drop(handle);
        let metadata = std::fs::symlink_metadata(link)
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        if metadata.file_attributes() & 0x410 != 0x410 {
            return Err(4390);
        }
        let resolved = std::fs::read_link(link)
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        if resolved.as_os_str().len() > 1024 {
            return Err(123);
        }
        let resolved = resolved
            .canonicalize()
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        let expected = outside
            .canonicalize()
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        if resolved != expected {
            return Err(4390);
        }
        Ok(())
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn receipt_atomic_concurrency_refusal_and_redirected_target_are_contained() {
        use std::os::windows::{fs::OpenOptionsExt, process::CommandExt};
        let base = std::env::temp_dir().join(format!(
            "aurum-receipt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("source with spaces ü");
        let cwd = base.join("unrelated cwd");
        let outside = base.join("outside");
        for path in [&root, &cwd, &outside] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(root.join("source-sentinel"), b"source unchanged").unwrap();
        std::fs::write(cwd.join("cwd-sentinel"), b"cwd unchanged").unwrap();
        std::fs::write(outside.join("outside-sentinel"), b"outside unchanged").unwrap();
        let guard = ordinary(&root, true).unwrap();
        let mut children: Vec<_> = (0..2)
            .map(|_| {
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tests::receipt_writer_helper", "--nocapture"])
                    .env("AURUM_RECEIPT_TEST_BASE", &base)
                    .current_dir(&cwd)
                    .creation_flags(0x08000000)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let identifiers: Vec<_> = children.iter().map(Child::id).collect();
        let allowlist = json!({"root":root,"cwd":cwd,"target":root.join("launcher-status.json"),"stages":identifiers.iter().map(|pid|root.join(format!("launcher-status.{pid}.tmp"))).collect::<Vec<_>>()});
        std::fs::write(
            base.join("allowed-paths.json"),
            serde_json::to_vec_pretty(&allowlist).unwrap(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !identifiers
            .iter()
            .all(|pid| base.join(format!("ready.{pid}")).is_file())
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::write(base.join("go"), b"go").unwrap();
        for child in &mut children {
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("owned receipt writer deadline; evidence {}", base.display());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let target = root.join("launcher-status.json");
        let bytes = std::fs::read(&target).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(identifiers.contains(&(value["writer_pid"].as_u64().unwrap() as u32)));
        assert!(!cwd.join("launcher-status.json").exists());
        assert!(identifiers
            .iter()
            .all(|pid| !root.join(format!("launcher-status.{pid}.tmp")).exists()));
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&target)
            .unwrap();
        assert!(publish_receipt(
            &root,
            &guard,
            b"{}",
            std::process::id(),
            Instant::now() + Duration::from_millis(100)
        )
        .is_err());
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        drop(locked);
        let redirected = base.join("redirected");
        std::fs::create_dir(&redirected).unwrap();
        let link = redirected.join("launcher-status.json");
        let allowlist = json!({"base":base,"cwd":cwd,"outside":outside,"fresh_link_directory":link,
            "source_sentinel":root.join("source-sentinel"),"cwd_sentinel":cwd.join("cwd-sentinel"),"outside_sentinel":outside.join("outside-sentinel")});
        std::fs::write(
            base.join("junction-allowed-paths.json"),
            serde_json::to_vec_pretty(&allowlist).unwrap(),
        )
        .unwrap();
        let setup_result = create_receipt_test_junction(&base, &cwd, &link, &outside);
        let setup = json!({"win32_error":setup_result.as_ref().err().copied().unwrap_or(0),
            "link":link,"outside":outside,"api":"CreateFileW/DeviceIoControl","fsctl":0x000900a4u32,
            "tag":0xa0000003u32,"reparse_header_bytes":8,"mount_point_names_offset":16,"buffer_limit_bytes":16*1024});
        std::fs::write(
            base.join("junction-setup.json"),
            serde_json::to_vec_pretty(&setup).unwrap(),
        )
        .unwrap();
        assert!(
            setup_result.is_ok(),
            "junction fixture failed; evidence {}",
            base.display()
        );
        let redirected_guard = ordinary(&redirected, true).unwrap();
        assert!(publish_receipt(
            &redirected,
            &redirected_guard,
            b"{}",
            std::process::id(),
            Instant::now() + Duration::from_millis(100)
        )
        .is_err());
        assert_eq!(
            std::fs::read(root.join("source-sentinel")).unwrap(),
            b"source unchanged"
        );
        assert_eq!(
            std::fs::read(cwd.join("cwd-sentinel")).unwrap(),
            b"cwd unchanged"
        );
        assert_eq!(
            std::fs::read(outside.join("outside-sentinel")).unwrap(),
            b"outside unchanged"
        );
        // Retain the bounded fixture/allowlist for source and cwd inspection.
    }
}
