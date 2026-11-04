//! Source-only tests. Run serially in a fresh A-only test assembly.
use super::*;
use crate::candidate_workspace::Workspace;
use std::fs;
use std::path::{Component, PathBuf, Prefix};
use std::sync::atomic::AtomicUsize;

const CHILD: &str = "worker_windows::binary::storage_tests::fixture_child";
static SERIAL: Mutex<()> = Mutex::new(());
static LATE_RELEASE: Mutex<Option<thread::ThreadId>> = Mutex::new(None);
static LATE_RELEASE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub(super) fn after_verified_retirement(deadline: Instant) {
    if LATE_RELEASE.lock().unwrap().as_ref() == Some(&thread::current().id()) {
        LATE_RELEASE_CALLS.fetch_add(1, Ordering::Relaxed);
        while Instant::now() < deadline {
            thread::yield_now();
        }
    }
}
struct LateRelease;
impl LateRelease {
    fn new() -> Self {
        let mut selected = LATE_RELEASE.lock().unwrap();
        assert!(selected.is_none());
        *selected = Some(thread::current().id());
        Self
    }
}
impl Drop for LateRelease {
    fn drop(&mut self) {
        *LATE_RELEASE.lock().unwrap() = None;
    }
}
struct Fixture {
    workspace: Workspace,
}
impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir().canonicalize().unwrap();
        assert!(temporary.is_absolute());
        assert_eq!(temporary.components().nth(1), Some(Component::RootDir));
        assert!(
            matches!(temporary.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(drive) | Prefix::VerbatimDisk(drive)
                if drive.eq_ignore_ascii_case(&b'A')))
        );
        let root = temporary.join(format!(
            "aurum-storage-check-{}",
            crate::random::session_id()
        ));
        fs::create_dir(&root).unwrap();
        let workspace = Workspace::for_test_project(&root).unwrap();
        fs::write(workspace.path.join("protected"), b"original private source").unwrap();
        Self { workspace }
    }
    fn clean(&mut self) {
        assert!(!self.workspace.lifetime.unresolved());
        assert_eq!(
            fs::read(self.workspace.path.join("protected")).unwrap(),
            b"original private source"
        );
        self.workspace.clean().unwrap();
    }
}
fn spawn(
    fixture: &Fixture,
    controller: &StreamController,
    role: &str,
    deadline: Instant,
) -> BinaryStream {
    let executable = std::env::current_exe().unwrap();
    Worker::spawn_binary_cleared(
        controller,
        BinarySpawn {
            program: &executable,
            argv0: &executable,
            args: &["--exact".into(), CHILD.into(), "--nocapture".into()],
            directory: Some(&fixture.workspace.path),
            overrides: &[
                ("AURUM_STORAGE_CHILD".into(), role.into()),
                (
                    "TEMP".into(),
                    std::env::temp_dir().to_string_lossy().into_owned(),
                ),
                ("SystemRoot".into(), std::env::var("SystemRoot").unwrap()),
            ],
            lifetime: &fixture.workspace.lifetime,
            deadline,
            limits: StreamLimits {
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
        },
    )
    .unwrap()
}
fn drain_checked(
    stream: &mut BinaryStream,
    deadline: Instant,
    mut check: impl FnMut(Instant) -> io::Result<()>,
) {
    let mut bytes = 0usize;
    loop {
        let count = stream
            .read_before_checked(&mut [0u8; 31], deadline, &mut check)
            .unwrap();
        if count == 0 {
            return;
        }
        bytes += count;
        assert!(bytes <= 4096, "bounded test-harness output exceeded");
    }
}

#[test]
fn checked_read_failure_stops_readers_without_replacing_the_original_retirement_path() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let absolute = Instant::now() + Duration::from_secs(5);
    let mut stream = spawn(&fixture, &controller, "idle", absolute);
    let mut callbacks = 0;
    let error = stream
        .read_before_checked(&mut [0u8; 7], absolute, |received| {
            callbacks += 1;
            assert_eq!(received, absolute);
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "checked storage refusal",
            ))
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(callbacks, 1);
    assert!(fixture.workspace.lifetime.unresolved());
    assert!(stream
        .resources
        .as_ref()
        .unwrap()
        .stdout
        .as_ref()
        .unwrap()
        .shared
        .stop
        .load(Ordering::Acquire));
    assert!(stream
        .resources
        .as_ref()
        .unwrap()
        .stderr
        .as_ref()
        .unwrap()
        .shared
        .stop
        .load(Ordering::Acquire));
    stream.retire().unwrap();
    assert_eq!(controller.retained(), 0);
    fixture.clean();
}
#[test]
fn checked_callback_cancellation_is_observed_before_returning_available_bytes() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let absolute = Instant::now() + Duration::from_secs(5);
    let context = OperationContext::new(absolute);
    let mut stream = context
        .scope(|| spawn(&fixture, &controller, "idle", absolute))
        .unwrap();
    let error = stream
        .read_before_checked(&mut [0u8; 7], absolute, |received| {
            assert_eq!(received, absolute);
            context.request_cancel().unwrap();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    stream.retire().unwrap();
    assert!(!context.termination_unconfirmed());
    assert!(!context.committed());
    fixture.clean();
}
#[test]
fn checked_observer_overrun_cannot_return_bytes_or_reset_the_work_deadline() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let absolute = Instant::now() + Duration::from_secs(1);
    let mut stream = spawn(&fixture, &controller, "idle", absolute);
    let error = stream
        .read_before_checked(
            &mut [0u8; 7],
            Instant::now() + Duration::from_secs(10),
            |received| {
                assert_eq!(received, absolute);
                while Instant::now() < received {
                    thread::yield_now();
                }
                Ok(())
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    stream.retire().unwrap();
    fixture.clean();
}
#[test]
fn checked_storage_scan_rejects_genuine_sparse_excess_and_retires_owned_worker() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let path = fixture.workspace.path.join("actual-sparse-excess");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            handle: *mut c_void,
            code: u32,
            input: *mut c_void,
            input_bytes: u32,
            output: *mut c_void,
            output_bytes: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn GetFileInformationByHandleEx(
            handle: *mut c_void,
            kind: u32,
            data: *mut c_void,
            bytes: u32,
        ) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    struct StandardInfo {
        allocation: i64,
        end: i64,
        links: u32,
        delete_pending: u8,
        directory: u8,
    }
    let mut returned = 0;
    assert_ne!(
        unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                0x000900c4,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        },
        0
    );
    file.set_len(crate::candidate_workspace::WORKSPACE_BYTES + 1)
        .unwrap();
    use std::os::windows::fs::MetadataExt;
    assert_ne!(file.metadata().unwrap().file_attributes() & 0x200, 0);
    let mut physical = StandardInfo::default();
    assert_ne!(
        unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                1,
                (&mut physical as *mut StandardInfo).cast(),
                std::mem::size_of::<StandardInfo>() as u32,
            )
        },
        0
    );
    assert!(physical.allocation >= 0 && physical.allocation <= 65536);
    drop(file);
    let absolute = Instant::now() + Duration::from_secs(5);
    let mut stream = spawn(&fixture, &controller, "idle", absolute);
    let error = stream
        .read_before_checked(&mut [0u8; 7], absolute, |deadline| {
            let refusal = crate::candidate_workspace::usage_with_budget(
                &fixture.workspace.path,
                crate::candidate_workspace::WORKSPACE_BYTES,
                deadline.saturating_duration_since(Instant::now()),
            )
            .expect_err("genuine excess unexpectedly passed storage inspection");
            assert!(
                refusal.contains("1 GiB observed-byte budget"),
                "different storage guard refused"
            );
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "observed storage exceeded",
            ))
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    stream.retire().unwrap();
    fixture.clean();
}
#[test]
fn checked_finish_callback_failure_stops_readers_and_requires_caller_retirement() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let absolute = Instant::now() + Duration::from_secs(5);
    let mut stream = spawn(&fixture, &controller, "success", absolute);
    drain_checked(&mut stream, absolute, |received| {
        assert_eq!(received, absolute);
        Ok(())
    });
    let error = stream
        .finish_checked(|received| {
            assert_eq!(received, absolute);
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "finish observation refused",
            ))
        })
        .err()
        .expect("finish observer unexpectedly accepted");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(stream.resources.is_some());
    stream.retire().unwrap();
    fixture.clean();
}
#[test]
fn checked_finish_releases_verified_resources_before_rejecting_late_cleanup() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let absolute = Instant::now() + Duration::from_secs(2);
    let context = OperationContext::new(absolute);
    let mut stream = context
        .scope(|| spawn(&fixture, &controller, "success", absolute))
        .unwrap();
    drain_checked(&mut stream, absolute, |_| Ok(()));
    let _late = LateRelease::new();
    let before = LATE_RELEASE_CALLS.load(Ordering::Relaxed);
    let error = stream
        .finish_checked(|received| {
            assert_eq!(received, absolute);
            Ok(())
        })
        .err()
        .expect("late cleanup unexpectedly accepted");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(LATE_RELEASE_CALLS.load(Ordering::Relaxed), before + 1);
    assert!(stream.resources.is_none());
    assert_eq!(controller.retained(), 0);
    assert!(!fixture.workspace.lifetime.unresolved());
    assert!(!context.termination_unconfirmed());
    assert!(!context.recovery_required());
    assert!(!context.committed());
    assert!(pool()
        .entries
        .lock()
        .unwrap()
        .iter()
        .all(|entry| matches!(entry, Entry::Empty)));
    fixture.clean();
}
#[test]
fn successful_checked_callbacks_keep_eof_receipt_deadline_and_context_intact() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let absolute = Instant::now() + Duration::from_secs(5);
    let context = OperationContext::new(absolute);
    let mut stream = context
        .scope(|| spawn(&fixture, &controller, "success", absolute))
        .unwrap();
    let mut calls = 0;
    drain_checked(&mut stream, absolute, |received| {
        calls += 1;
        assert_eq!(received, absolute);
        Ok(())
    });
    let receipt = stream
        .finish_checked(|received| {
            calls += 1;
            assert_eq!(received, absolute);
            Ok(())
        })
        .unwrap();
    assert!(calls >= 3);
    assert!(receipt.stdout_eof && receipt.stderr_eof);
    assert_eq!(receipt.root_exit_code, 0);
    assert!(receipt.owned_tree_retired && receipt.owned_readers_joined);
    assert!(!context.cancellation_requested());
    assert!(!context.termination_unconfirmed());
    fixture.clean();
}

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount64() -> u64;
    fn GetProcessId(process: *mut c_void) -> u32;
    fn GetProcessTimes(
        process: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn QueryFullProcessImageNameW(
        process: *mut c_void,
        flags: u32,
        path: *mut u16,
        count: *mut u32,
    ) -> i32;
}
fn host_identity(host: &std::process::Child) -> io::Result<(u32, u64, u32)> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    let handle = host.as_raw_handle();
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let pid = unsafe { GetProcessId(handle) };
    if pid == 0 {
        return Err(io::Error::last_os_error());
    }
    let birth = (u64::from(created.high) << 32) | u64::from(created.low);
    if pid != host.id() || birth == 0 {
        return Err(io::Error::other("original host handle identity mismatch"));
    }
    let signal = unsafe { WaitForSingleObject(handle, 0) };
    if signal == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    if signal != 0 && signal != 258 {
        return Err(io::Error::other("unexpected original host handle signal"));
    }
    Ok((pid, birth, signal))
}
struct HostBinding {
    handle: *mut c_void,
    pid: u32,
    birth: u64,
    image: String,
}
fn bind_live_host(host: &std::process::Child) -> io::Result<HostBinding> {
    // Cache image binding while the ready/continue handshake keeps this host live.
    let identity = host_identity(host)?;
    if identity.2 != 258 {
        return Err(io::Error::other("host exited before live image binding"));
    }
    let handle = host.as_raw_handle();
    let mut path = [0u16; 2048];
    let mut count = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let image = String::from_utf16(&path[..count as usize])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !image.eq_ignore_ascii_case(&std::env::current_exe()?.to_string_lossy()) {
        return Err(io::Error::other(
            "live host image does not match the test EXE",
        ));
    }
    if host_identity(host)? != identity {
        return Err(io::Error::other("host changed during live image binding"));
    }
    Ok(HostBinding {
        handle,
        pid: identity.0,
        birth: identity.1,
        image,
    })
}
fn host_deadline_failure(
    host: &mut std::process::Child,
    original: (u32, u64, u32),
    claim: &mut crate::candidate_workspace::LifetimeClaim,
) -> ! {
    // A kill request is not exit evidence, and cleanup cannot add deadline grace.
    let killed = host.kill();
    let identity = host_identity(host);
    let waited = host.try_wait();
    let confirmed = identity
        .as_ref()
        .is_ok_and(|value| (value.0, value.1, value.2) == (original.0, original.1, 0))
        && matches!(waited.as_ref(), Ok(Some(_)));
    if confirmed {
        claim.resolve();
    }
    panic!(
        "closed environment host deadline: kill={killed:?}, identity={identity:?}, wait={waited:?}, retirement_confirmed={confirmed}"
    );
}
#[test]
fn actual_child_env_isolation_and_default_inheritance_preserve_host_context() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let before = [
        "AURUM_FAKE_CREDENTIAL",
        "CARGO_TARGET_DIR",
        "AURUM_UNRELATED_PARENT_VAR",
        "_NO_DEBUG_HEAP",
    ]
    .map(std::env::var_os);
    let mut host_claim = fixture.workspace.lifetime.claim().unwrap();
    // The closed host is deliberately not debug-owned. Its inner probe is
    // debug-owned; nested debugger-chain behavior is tested elsewhere.
    let deadline_tick = unsafe { GetTickCount64() }.checked_add(25_000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture"])
        .env("AURUM_STORAGE_CHILD", "env-host")
        .env("AURUM_STORAGE_ENV_ROOT", &fixture.workspace.path)
        .env(
            "AURUM_STORAGE_HOST_DEADLINE_TICK",
            deadline_tick.to_string(),
        )
        .env("AURUM_FAKE_CREDENTIAL", "fixed-fake-credential")
        .env("CARGO_TARGET_DIR", "A:/unrelated-parent-target")
        .env("AURUM_UNRELATED_PARENT_VAR", "parent-only")
        .env("_NO_DEBUG_HEAP", "0")
        .current_dir(&fixture.workspace.path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let original = host_identity(&host).unwrap();
    let ready = fixture.workspace.path.join("env-host-ready");
    loop {
        if Instant::now() >= deadline {
            host_deadline_failure(&mut host, original, &mut host_claim);
        }
        match fs::read(&ready) {
            Ok(bytes) => {
                assert_eq!(bytes, b"ready for live image binding");
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("closed host ready marker: {error}"),
        }
        assert!(
            host.try_wait().unwrap().is_none(),
            "host exited before ready"
        );
        thread::yield_now();
    }
    let binding = bind_live_host(&host).unwrap();
    assert_eq!((binding.pid, binding.birth), (original.0, original.1));
    if Instant::now() >= deadline {
        host_deadline_failure(&mut host, original, &mut host_claim);
    }
    fs::write(
        fixture.workspace.path.join("env-host-continue"),
        b"continue after live image binding",
    )
    .unwrap();
    let status = loop {
        if Instant::now() >= deadline {
            host_deadline_failure(&mut host, original, &mut host_claim);
        }
        if let Some(status) = host.try_wait().unwrap() {
            break status;
        }
        thread::yield_now();
    };
    let retired = host_identity(&host).unwrap();
    assert_eq!(host.as_raw_handle(), binding.handle);
    assert_eq!((retired.0, retired.1), (binding.pid, binding.birth));
    assert_eq!(retired.2, 0);
    assert!(status.success());
    assert!(binding
        .image
        .eq_ignore_ascii_case(&std::env::current_exe().unwrap().to_string_lossy()));
    assert_eq!(
        fs::read(fixture.workspace.path.join("env-host-verified")).unwrap(),
        b"three original owned probes verified"
    );
    let after = [
        "AURUM_FAKE_CREDENTIAL",
        "CARGO_TARGET_DIR",
        "AURUM_UNRELATED_PARENT_VAR",
        "_NO_DEBUG_HEAP",
    ]
    .map(std::env::var_os);
    assert!(before == after, "root environment changed");
    host_claim.resolve();
    fixture.clean();
}
fn environment_host() {
    let root = PathBuf::from(std::env::var_os("AURUM_STORAGE_ENV_ROOT").unwrap());
    let deadline_tick = std::env::var("AURUM_STORAGE_HOST_DEADLINE_TICK")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    fs::write(root.join("env-host-ready"), b"ready for live image binding").unwrap();
    let release = root.join("env-host-continue");
    loop {
        assert!(
            unsafe { GetTickCount64() } < deadline_tick,
            "closed host handshake deadline"
        );
        match fs::read(&release) {
            Ok(bytes) => {
                assert_eq!(bytes, b"continue after live image binding");
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("closed host continue marker: {error}"),
        }
        thread::yield_now();
    }
    let started = Instant::now();
    let remaining =
        Duration::from_millis(deadline_tick.saturating_sub(unsafe { GetTickCount64() }));
    assert!(!remaining.is_zero());
    let context = OperationContext::new(started + remaining.min(Duration::from_secs(20)));
    let controller = StreamController::for_runtime();
    let executable = std::env::current_exe().unwrap();
    let before = [
        "AURUM_FAKE_CREDENTIAL",
        "CARGO_TARGET_DIR",
        "AURUM_UNRELATED_PARENT_VAR",
        "_NO_DEBUG_HEAP",
    ]
    .map(std::env::var_os);
    for mode in 0..3 {
        assert!(
            unsafe { GetTickCount64() } < deadline_tick,
            "closed host work deadline"
        );
        let record = root.join(format!("probe-{mode}.json"));
        let profile = root.join(format!("profile-{mode}"));
        fs::create_dir(&profile).unwrap();
        let marker = profile.to_string_lossy().into_owned();
        let mut overrides = vec![
            ("AURUM_STORAGE_CHILD".into(), "probe".into()),
            (
                "AURUM_STORAGE_PROBE_FILE".into(),
                record.to_string_lossy().into_owned(),
            ),
            ("AURUM_STORAGE_EXPECT_PRIVATE".into(), marker.clone()),
            (
                "AURUM_STORAGE_EXPECT_SYSTEMROOT".into(),
                std::env::var("SystemRoot").unwrap(),
            ),
            ("SystemRoot".into(), std::env::var("SystemRoot").unwrap()),
            ("PATH".into(), marker.clone()),
            ("USERPROFILE".into(), marker.clone()),
            ("HOME".into(), marker.clone()),
            ("TEMP".into(), marker.clone()),
            ("TMP".into(), marker.clone()),
        ];
        if mode == 1 {
            overrides.push(("_no_debug_heap".into(), "0".into()));
            overrides.push(("CARGO_TARGET_DIR".into(), marker.clone()));
        }
        let fence = LifetimeFence::default();
        let deadline = context
            .deadline()
            .min(Instant::now() + Duration::from_secs(5));
        let args = ["--exact".into(), CHILD.into(), "--nocapture".into()];
        let request = BinarySpawn {
            program: &executable,
            argv0: &executable,
            args: &args,
            directory: Some(&root),
            overrides: &overrides,
            lifetime: &fence,
            deadline,
            limits: StreamLimits {
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
        };
        let mut stream = context
            .scope(|| {
                if mode == 2 {
                    Worker::spawn_binary(&controller, request)
                } else {
                    Worker::spawn_binary_cleared(&controller, request)
                }
            })
            .unwrap()
            .unwrap();
        drain_checked(&mut stream, deadline, |_| Ok(()));
        let receipt = stream.finish_checked(|_| Ok(())).unwrap();
        assert!(receipt.owned_tree_retired && receipt.owned_readers_joined);
        assert_eq!(controller.retained(), 0);
        assert!(!fence.unresolved());
        // No environment output is trusted before original owned retirement.
        let metadata = fs::symlink_metadata(&record).unwrap();
        assert!(metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 4096);
        let file = crate::snapshot::open_checked_file(&record, context.deadline()).unwrap();
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes).unwrap();
        assert!(bytes.len() <= 4096);
        let data: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(data["private_values_match"], true);
        assert_eq!(data["systemroot_matches"], true);
        assert_eq!(data["heap"], if mode == 0 { "1" } else { "0" });
        assert_eq!(data["fake_credential_present"], mode == 2);
        assert_eq!(data["unrelated_parent_present"], mode == 2);
        assert_eq!(data["parent_cargo_target_present"], mode == 2);
        assert_eq!(data["private_cargo_target_present"], mode == 1);
    }
    let after = [
        "AURUM_FAKE_CREDENTIAL",
        "CARGO_TARGET_DIR",
        "AURUM_UNRELATED_PARENT_VAR",
        "_NO_DEBUG_HEAP",
    ]
    .map(std::env::var_os);
    assert!(before == after, "dedicated host environment changed");
    assert!(
        !context.cancellation_requested()
            && !context.termination_unconfirmed()
            && !context.committed()
    );
    fs::write(
        root.join("env-host-verified"),
        b"three original owned probes verified",
    )
    .unwrap();
}
fn environment_probe() {
    let private = std::env::var("AURUM_STORAGE_EXPECT_PRIVATE").unwrap();
    let private_values_match = ["PATH", "USERPROFILE", "HOME", "TEMP", "TMP"]
        .into_iter()
        .all(|key| std::env::var(key).as_deref() == Ok(private.as_str()));
    let data = serde_json::json!({
        "private_values_match": private_values_match,
        "systemroot_matches": std::env::var("SystemRoot").ok() == std::env::var("AURUM_STORAGE_EXPECT_SYSTEMROOT").ok(),
        "fake_credential_present": std::env::var("AURUM_FAKE_CREDENTIAL").as_deref() == Ok("fixed-fake-credential"),
        "unrelated_parent_present": std::env::var("AURUM_UNRELATED_PARENT_VAR").as_deref() == Ok("parent-only"),
        "parent_cargo_target_present": std::env::var("CARGO_TARGET_DIR").as_deref() == Ok("A:/unrelated-parent-target"),
        "private_cargo_target_present": std::env::var("CARGO_TARGET_DIR").as_deref() == Ok(private.as_str()),
        "heap": std::env::var("_NO_DEBUG_HEAP").unwrap(),
    });
    let path = PathBuf::from(std::env::var_os("AURUM_STORAGE_PROBE_FILE").unwrap());
    let encoded = serde_json::to_vec(&data).unwrap();
    assert!(encoded.len() <= 4096);
    fs::write(path, encoded).unwrap();
}
#[test]
fn fixture_child() {
    let Some(role) = std::env::var_os("AURUM_STORAGE_CHILD") else {
        return;
    };
    match role.to_str().unwrap() {
        "idle" => thread::park_timeout(Duration::from_secs(20)),
        "success" => {}
        "env-host" => environment_host(),
        "probe" => environment_probe(),
        _ => panic!("closed storage fixture role refused"),
    }
}
