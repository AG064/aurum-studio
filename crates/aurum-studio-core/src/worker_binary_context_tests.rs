//! Unrun integration tests for the actual source16 context and workspace APIs.
//! Include only in the separately reviewed binary-stream candidate's cfg(test).
use super::{BinarySpawn, BinaryStream, StreamController, StreamLimits, Worker};
use crate::candidate_workspace::Workspace;
use crate::operation_context::{OperationContext, StopReason};
use std::fs;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::{Component, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CHILD_TEST: &str = "worker_windows::binary::context_tests::fixture_child";
static SERIAL: Mutex<()> = Mutex::new(());
static READ_HOLD: Mutex<Option<Arc<Hold>>> = Mutex::new(None);
static CANCEL_SUCCESSES: AtomicUsize = AtomicUsize::new(0);
struct Hold {
    entered: AtomicUsize,
    released: AtomicBool,
}
struct HoldGuard(Arc<Hold>);
impl HoldGuard {
    fn new() -> Self {
        let hold = Arc::new(Hold {
            entered: AtomicUsize::new(0),
            released: AtomicBool::new(false),
        });
        let mut selected = READ_HOLD.lock().unwrap();
        assert!(selected.is_none());
        *selected = Some(hold.clone());
        Self(hold)
    }
    fn entered(&self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.0.entered.load(Ordering::Acquire) != 2 {
            assert!(
                Instant::now() < deadline,
                "original readers did not enter test gate"
            );
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }
    fn release(&self) {
        self.0.released.store(true, Ordering::Release);
    }
}
impl Drop for HoldGuard {
    fn drop(&mut self) {
        self.release();
        *READ_HOLD.lock().unwrap() = None;
    }
}
/// Test-only hook before the first ReadFile; no parent environment mutation.
pub(super) fn before_read() {
    let hold = READ_HOLD.lock().unwrap().clone();
    if let Some(hold) = hold {
        hold.entered.fetch_add(1, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !hold.released.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }
}
/// Call immediately after the real API result; AtomicUsize preserves LastError.
pub(super) fn record_cancel(result: i32) {
    if result != 0 {
        CANCEL_SUCCESSES.fetch_add(1, Ordering::Relaxed);
    }
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetThreadIOPendingFlag(thread: *mut std::ffi::c_void, pending: *mut i32) -> i32;
    fn GetProcessId(process: *mut std::ffi::c_void) -> u32;
    fn GetProcessTimes(
        process: *mut std::ffi::c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}
#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
fn original_process(child: &std::process::Child) -> (u32, u64, u32) {
    let handle = child.as_raw_handle();
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    assert_ne!(
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) },
        0
    );
    let birth = (u64::from(created.high) << 32) | u64::from(created.low);
    let pid = unsafe { GetProcessId(handle) };
    assert_eq!(pid, child.id());
    assert_ne!(birth, 0);
    (pid, birth, unsafe { super::WaitForSingleObject(handle, 0) })
}
struct Fixture {
    root: PathBuf,
    workspace: Workspace,
}
fn temporary_root() -> PathBuf {
    let temporary = std::env::temp_dir().canonicalize().unwrap();
    let a_disk = matches!(
        temporary.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(drive) | Prefix::VerbatimDisk(drive)
                if drive.eq_ignore_ascii_case(&b'A'))
    );
    assert!(
        temporary.is_absolute()
            && a_disk
            && temporary.components().nth(1) == Some(Component::RootDir),
        "absolute A-only gate required"
    );
    temporary
}
impl Fixture {
    fn new() -> Self {
        let temporary = temporary_root();
        let root = temporary.join(format!(
            "aurum-binary-context-{}",
            crate::random::session_id()
        ));
        fs::create_dir(&root).unwrap();
        let workspace = Workspace::for_test_project(&root).unwrap();
        fs::write(workspace.path.join("protected"), b"retained private source").unwrap();
        Self { root, workspace }
    }
}
fn spawn(
    fixture: &Fixture,
    controller: &StreamController,
    duration: Duration,
    extra: &[(String, String)],
) -> io::Result<BinaryStream> {
    let executable = std::env::current_exe()?;
    let args = [
        "--exact".to_owned(),
        CHILD_TEST.to_owned(),
        "--nocapture".to_owned(),
    ];
    let mut overrides = vec![("AURUM_BINARY_CONTEXT_CHILD".to_owned(), "idle".to_owned())];
    overrides.extend_from_slice(extra);
    Worker::spawn_binary(
        controller,
        BinarySpawn {
            program: &executable,
            argv0: &executable,
            args: &args,
            directory: Some(&fixture.workspace.path),
            overrides: &overrides,
            lifetime: &fixture.workspace.lifetime,
            deadline: Instant::now() + duration,
            limits: StreamLimits {
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
        },
    )
}
fn wait_pending(stream: &BinaryStream) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let resources = stream.resources.as_ref().unwrap();
        let mut all = true;
        for reader in [&resources.stdout, &resources.stderr].into_iter().flatten() {
            // Test harness banner must be consumed before stdout can block.
            let _ = reader.take(None).unwrap();
            let thread = reader.thread.as_ref().unwrap();
            let mut pending = 0;
            assert_ne!(
                unsafe { GetThreadIOPendingFlag(thread.as_raw_handle(), &mut pending) },
                0
            );
            all &= pending != 0;
        }
        if all {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native pending reads not observed"
        );
        std::thread::park_timeout(Duration::from_millis(1));
    }
}
fn reap(controller: &StreamController) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if controller.reap().is_ok() && controller.retained() == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "original retained resources not reaped"
        );
        std::thread::park_timeout(Duration::from_millis(1));
    }
}
fn protected(workspace: &Workspace) {
    assert_eq!(
        fs::read(workspace.path.join("protected")).unwrap(),
        b"retained private source"
    );
}

#[test]
fn actual_context_cancel_is_captured_and_cancels_pending_native_io() {
    let _serial = SERIAL.lock().unwrap();
    assert!(OperationContext::current().is_none());
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(10));
    let mut stream = context
        .scope(|| spawn(&fixture, &controller, Duration::from_secs(15), &[]))
        .unwrap()
        .unwrap();
    assert_eq!(
        stream.resources.as_ref().unwrap().deadline,
        context.deadline()
    );
    assert!(OperationContext::current().is_none());
    wait_pending(&stream);
    let cancellations = CANCEL_SUCCESSES.load(Ordering::Relaxed);
    assert!(context.request_cancel().unwrap());
    assert_eq!(context.stop_reason(), Some(StopReason::Cancelled));
    assert_eq!(
        stream
            .read_before(&mut [0u8; 7], Instant::now() + Duration::from_secs(3))
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(CANCEL_SUCCESSES.load(Ordering::Relaxed) > cancellations);
    stream.retire().unwrap();
    assert!(!fixture.workspace.lifetime.unresolved());
    assert_eq!(controller.retained(), 0);
    assert!(!context.termination_unconfirmed());
    assert!(!context.recovery_required());
    assert!(!context.committed());
    protected(&fixture.workspace);
    fixture.workspace.clean().unwrap();
}

#[test]
fn actual_context_deadline_clips_stream_and_retains_original_objects_until_joined() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(1));
    let mut stream = context
        .scope(|| spawn(&fixture, &controller, Duration::from_secs(10), &[]))
        .unwrap()
        .unwrap();
    assert_eq!(
        stream.resources.as_ref().unwrap().deadline,
        context.deadline()
    );
    wait_pending(&stream);
    let supplied_deadline = Instant::now() + Duration::from_secs(5);
    let mut harness_bytes = 0usize;
    let error = loop {
        match stream.read_before(&mut [0u8; 7], supplied_deadline) {
            Ok(0) => panic!("unexpected EOF before absolute deadline"),
            Ok(count) => {
                harness_bytes += count;
                assert!(harness_bytes <= 4096, "test harness output exceeded bound");
            }
            Err(error) => break error,
        }
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(context.stop_reason(), Some(StopReason::Deadline));
    stream.retire().unwrap();
    assert!(!fixture.workspace.lifetime.unresolved());
    assert!(!context.termination_unconfirmed());
    assert!(!context.committed());
    protected(&fixture.workspace);
    fixture.workspace.clean().unwrap();
}

#[test]
fn no_context_uncertain_retirement_blocks_exact_workspace_clean_until_verified_reap() {
    let _serial = SERIAL.lock().unwrap();
    assert!(OperationContext::current().is_none());
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let hold = HoldGuard::new();
    let mut stream = spawn(&fixture, &controller, Duration::from_secs(10), &[]).unwrap();
    hold.entered();
    assert!(stream.retire().is_err());
    assert!(fixture.workspace.lifetime.unresolved());
    assert!(fixture.workspace.clean().is_err());
    protected(&fixture.workspace);
    drop(stream);
    assert_eq!(controller.retained(), 1);
    hold.release();
    reap(&controller);
    assert!(!fixture.workspace.lifetime.unresolved());
    protected(&fixture.workspace);
    let path = fixture.workspace.path.clone();
    fixture.workspace.clean().unwrap();
    assert!(!path.exists());
}

#[test]
fn actual_context_uncertainty_stays_recovery_required_after_scope_exit() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(30));
    let hold = HoldGuard::new();
    let mut stream = context
        .scope(|| spawn(&fixture, &controller, Duration::from_secs(15), &[]))
        .unwrap()
        .unwrap();
    hold.entered();
    context
        .scope(|| {
            assert!(context.request_cancel().unwrap());
            assert_eq!(
                stream
                    .read_before(&mut [0u8; 7], Instant::now() + Duration::from_secs(3))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Interrupted
            );
            assert!(stream.retire().is_err());
            assert!(context.termination_unconfirmed());
            assert!(context.recovery_required());
            assert!(fixture.workspace.clean().is_err());
        })
        .unwrap();
    assert!(OperationContext::current().is_none());
    assert!(fixture.workspace.clean().is_err());
    protected(&fixture.workspace);
    drop(stream);
    assert_eq!(controller.retained(), 1);
    hold.release();
    reap(&controller);
    assert!(!fixture.workspace.lifetime.unresolved());
    // A later positive reap does not retroactively certify the failed job.
    assert!(context.termination_unconfirmed() && context.recovery_required());
    assert!(!context.committed());
    fixture.workspace.clean().unwrap();
}

#[test]
fn actual_context_partial_admission_preserves_original_pending_and_workspace() {
    let _serial = SERIAL.lock().unwrap();
    let mut fixture = Fixture::new();
    let controller = StreamController::for_runtime();
    let gate = fixture.root.join("creator");
    fs::create_dir(&gate).unwrap();
    let context = OperationContext::new(Instant::now() + Duration::from_millis(100));
    let result = context
        .scope(|| {
            spawn(
                &fixture,
                &controller,
                Duration::from_secs(10),
                &[(
                    "AURUM_WORKER_CREATOR_GATE".into(),
                    gate.to_string_lossy().into_owned(),
                )],
            )
        })
        .unwrap();
    match result {
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::TimedOut),
        Ok(_) => panic!("held creator unexpectedly admitted"),
    }
    assert!(gate.join("entered").is_file());
    assert_eq!(controller.retained(), 1);
    assert!(context.termination_unconfirmed() && context.recovery_required());
    assert!(fixture.workspace.clean().is_err());
    protected(&fixture.workspace);
    fs::write(gate.join("release"), b"release").unwrap();
    reap(&controller);
    assert!(!fixture.workspace.lifetime.unresolved());
    assert!(!context.committed());
    fixture.workspace.clean().unwrap();
}

#[test]
fn actual_workspace_drop_preserves_uncertain_allocation_and_lease_until_host_exit() {
    let _serial = SERIAL.lock().unwrap();
    let temporary = temporary_root();
    let project = temporary.join(format!("aurum-binary-drop-{}", crate::random::session_id()));
    fs::create_dir(&project).unwrap();
    let mut host = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("AURUM_BINARY_CONTEXT_CHILD", "drop")
        .env("AURUM_BINARY_CONTEXT_PROJECT", &project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .current_dir(&project)
        .spawn()
        .unwrap();
    let original = original_process(&host);
    assert_eq!(original.2, 258);
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(status) = host.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            let _ = host.kill();
            let _ = host.wait();
            panic!("drop proof host deadline");
        }
        std::thread::park_timeout(Duration::from_millis(1));
    }
    let retired = original_process(&host);
    assert_eq!((retired.0, retired.1), (original.0, original.1));
    assert_eq!(retired.2, 0);
    let path_text = fs::read_to_string(project.join("retained-path")).unwrap();
    assert!(path_text.len() <= 4096);
    let retained = PathBuf::from(path_text);
    assert_eq!(
        fs::read(retained.join("protected")).unwrap(),
        b"retained private source"
    );
    // The recognized allocator owns recovery; no arbitrary deletion of the receipt path.
    let mut recovered = Workspace::for_test_project(&project).unwrap();
    assert_eq!(recovered.recovered, 1);
    assert!(!retained.exists());
    recovered.clean().unwrap();
}

#[test]
fn fixture_child() {
    let Some(role) = std::env::var_os("AURUM_BINARY_CONTEXT_CHILD") else {
        return;
    };
    match role.to_str().unwrap() {
        "idle" => std::thread::park_timeout(Duration::from_secs(20)),
        "drop" => {
            assert!(OperationContext::current().is_none());
            let project = PathBuf::from(std::env::var_os("AURUM_BINARY_CONTEXT_PROJECT").unwrap());
            let workspace = Workspace::for_test_project(&project).unwrap();
            fs::write(workspace.path.join("protected"), b"retained private source").unwrap();
            let path = workspace.path.clone();
            let fixture = Fixture {
                root: project.clone(),
                workspace,
            };
            let controller = StreamController::for_runtime();
            let hold = HoldGuard::new();
            let mut stream = spawn(&fixture, &controller, Duration::from_secs(10), &[]).unwrap();
            hold.entered();
            assert!(stream.retire().is_err());
            let evidence = stream
                .resources
                .as_ref()
                .unwrap()
                .worker
                .as_ref()
                .unwrap()
                .census
                .evidence(
                    &stream
                        .resources
                        .as_ref()
                        .unwrap()
                        .worker
                        .as_ref()
                        .unwrap()
                        .job,
                )
                .unwrap();
            assert_eq!(evidence.active, 0);
            assert_eq!(evidence.total as usize, evidence.members.len());
            assert!(evidence.members.iter().all(|(_, _, signal, _)| *signal));
            drop(stream);
            drop(fixture);
            assert!(path.exists());
            assert_eq!(
                fs::read(path.join("protected")).unwrap(),
                b"retained private source"
            );
            assert!(Workspace::for_test_project(&project).unwrap().recovered == 0);
            fs::write(
                project.join("retained-path"),
                path.to_string_lossy().as_bytes(),
            )
            .unwrap();
            hold.release();
            assert_eq!(controller.retained(), 1);
        }
        _ => panic!("closed fixture role refused"),
    }
}
