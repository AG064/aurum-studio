//! Source-only fixture on reviewed owner02. Compilation and execution are held.
use super::super::ledger::StartupDriverState;
use super::*;
use crate::candidate_workspace::Workspace;
use std::fs;
use std::io::Write;
use std::path::{Component, Prefix};

const CHILD: &str = "worker_windows::binary::owner_tests::fixture_child";
static SERIAL: Mutex<()> = Mutex::new(());
static SELECTED: Mutex<Option<Arc<Hooks>>> = Mutex::new(None);
#[derive(Default)]
struct Gate(Mutex<(bool, bool)>, Condvar);
impl Gate {
    fn wait(&self, entered: bool) {
        self.wait_until(entered, Instant::now() + Duration::from_secs(10));
    }
    fn wait_until(&self, entered: bool, end: Instant) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if entered {
            state.0 = true;
            self.1.notify_all();
        }
        while if entered { !state.1 } else { !state.0 } {
            if Instant::now() >= end {
                drop(state);
                panic!("test barrier deadline");
            }
            state = self
                .1
                .wait_timeout(state, end.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    fn release(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
        self.1.notify_all();
    }
}
// This guard must live inside the scoped closure, before any participating peer.
struct ReleaseGates<'a>(&'a Gate, &'a Gate);
impl Drop for ReleaseGates<'_> {
    fn drop(&mut self) {
        self.0.release();
        self.1.release();
    }
}
#[test]
fn gate_timeout_preserves_primary_panic_and_cleanup_recovers_poison() {
    let gate = Gate::default();
    let timeout = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        gate.wait_until(false, Instant::now());
    }))
    .unwrap_err();
    assert_eq!(
        timeout.downcast_ref::<&str>(),
        Some(&"test barrier deadline")
    );
    assert!(
        !gate.0.is_poisoned(),
        "timeout must unlock before panicking"
    );
    let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = gate.0.lock().unwrap();
        panic!("intentional gate poison");
    }))
    .unwrap_err();
    assert_eq!(
        poison.downcast_ref::<&str>(),
        Some(&"intentional gate poison")
    );
    assert!(gate.0.is_poisoned());
    gate.release();
    gate.wait(true);
}
#[test]
fn scope_failure_releases_blocked_peer_before_automatic_join() {
    let start = Gate::default();
    let close = Gate::default();
    let completed = AtomicBool::new(false);
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        thread::scope(|scope| {
            let _release_on_failure = ReleaseGates(&start, &close);
            let _peer = scope.spawn(|| {
                start.wait(true);
                completed.store(true, Ordering::Release);
            });
            start.wait(false);
            panic!("primary scoped failure");
        });
    }))
    .unwrap_err();
    assert_eq!(
        failure.downcast_ref::<&str>(),
        Some(&"primary scoped failure")
    );
    assert!(completed.load(Ordering::Acquire));
    assert!(start.0.lock().unwrap().1 && close.0.lock().unwrap().1);
}
struct Retired {
    deadline: Instant,
    joined: bool,
    eof: (bool, bool),
}
struct Hooks {
    owner: Weak<Owner>,
    pause: bool,
    start: Gate,
    close: Gate,
    admission: Mutex<Option<Arc<Admission>>>,
    retired: Mutex<Option<Retired>>,
}
struct Selected(Arc<Hooks>);
impl Selected {
    fn new(controller: &StreamController, pause: bool) -> Self {
        let hooks = Arc::new(Hooks {
            owner: Arc::downgrade(&controller.owner),
            pause,
            start: Gate::default(),
            close: Gate::default(),
            admission: Mutex::new(None),
            retired: Mutex::new(None),
        });
        assert!(SELECTED.lock().unwrap().replace(hooks.clone()).is_none());
        Self(hooks)
    }
    fn admission(&self) -> Arc<Admission> {
        self.0.admission.lock().unwrap().as_ref().unwrap().clone()
    }
}
impl Drop for Selected {
    fn drop(&mut self) {
        self.0.start.release();
        self.0.close.release();
        *SELECTED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
fn selected(owner: &Arc<Owner>) -> Option<Arc<Hooks>> {
    SELECTED
        .lock()
        .unwrap()
        .clone()
        .filter(|h| Weak::ptr_eq(&h.owner, &Arc::downgrade(owner)))
}
pub(super) fn before_registered_start(admission: &Arc<Admission>) {
    if let Some(h) = selected(&admission.owner) {
        *h.admission.lock().unwrap() = Some(admission.clone());
        if h.pause {
            h.start.wait(true);
        }
    }
}
pub(super) fn after_close_publish(controller: &StreamController) {
    if let Some(h) = selected(&controller.owner) {
        if h.pause {
            h.close.wait(true);
        }
    }
}
pub(super) fn after_retired(resources: &Resources) {
    if let Some(h) = selected(&resources.admission.owner) {
        let control = resources.admission.registration.control().unwrap();
        let (total, active, _, _) = control.original_job_for_test().unwrap();
        assert!(total > 0);
        assert_eq!(active, 0);
        assert_eq!(control.state().driver, StartupDriverState::Joined);
        let deadline = control
            .state()
            .retirement_deadline
            .unwrap()
            .min(resources.retirement_deadline.unwrap())
            .min(resources.deadline);
        let joined = [&resources.stdout, &resources.stderr]
            .into_iter()
            .all(|reader| {
                reader
                    .as_ref()
                    .is_some_and(|r| r.thread.is_none() && r.identity.is_some())
            });
        *h.retired.lock().unwrap() = Some(Retired {
            deadline,
            joined,
            eof: (resources.stdout_eof, resources.stderr_eof),
        });
    }
}
struct Fixture {
    workspace: Workspace,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap();
        assert_a(&root);
        assert_a(&std::env::current_exe().unwrap());
        let root = root.join(format!("aurum-owner-tests-{}", crate::random::session_id()));
        fs::create_dir(&root).unwrap();
        Self {
            workspace: Workspace::for_test_project(&root).unwrap(),
        }
    }
    fn clean(&mut self) {
        assert!(!self.workspace.lifetime.unresolved());
        self.workspace.clean().unwrap();
    }
}
fn assert_a(path: &Path) {
    assert!(path.is_absolute() && path.components().nth(1) == Some(Component::RootDir));
    assert!(
        matches!(path.components().next(), Some(Component::Prefix(p))
        if matches!(p.kind(), Prefix::Disk(d) | Prefix::VerbatimDisk(d)
            if d.eq_ignore_ascii_case(&b'A')))
    );
}
fn spawn(f: &Fixture, c: &StreamController, deadline: Instant) -> io::Result<BinaryStream> {
    let exe = std::env::current_exe()?;
    Worker::spawn_binary_cleared(
        c,
        BinarySpawn {
            program: &exe,
            argv0: &exe,
            args: &["--exact".into(), CHILD.into(), "--nocapture".into()],
            directory: Some(&f.workspace.path),
            overrides: &overrides(f),
            lifetime: &f.workspace.lifetime,
            deadline,
            limits: StreamLimits {
                stdout_bytes: 4096,
                stderr_bytes: 4096,
            },
        },
    )
}
fn overrides(f: &Fixture) -> Vec<(String, String)> {
    let root = f.workspace.path.to_string_lossy().into_owned();
    let mut values = vec![
        ("AURUM_OWNER_TEST_CHILD".into(), "emit".into()),
        ("SystemRoot".into(), std::env::var("SystemRoot").unwrap()),
    ];
    values.extend(
        ["TEMP", "TMP", "USERPROFILE", "HOME"]
            .into_iter()
            .map(|k| (k.to_owned(), root.clone())),
    );
    values
}
fn command(f: &Fixture, c: &StreamController, deadline: Instant) -> command::CommandCapture {
    let exe = std::env::current_exe().unwrap();
    command::CommandCapture::spawn(
        c,
        command::CommandSpawn {
            program: &exe,
            argv0: &exe,
            args: &["--exact".into(), CHILD.into(), "--nocapture".into()],
            directory: Some(&f.workspace.path),
            overrides: &overrides(f),
            lifetime: &f.workspace.lifetime,
            deadline,
        },
    )
    .unwrap()
}
fn drain(stream: &mut BinaryStream, deadline: Instant) {
    let mut bytes = 0;
    loop {
        let n = stream
            .read_before_checked(&mut [0u8; 256], deadline, |_| Ok(()))
            .unwrap();
        if n == 0 {
            break;
        }
        bytes += n;
        assert!(bytes <= 4096);
    }
}

#[test]
fn close_published_before_registration_prevents_original_driver_launch() {
    let _serial = SERIAL.lock().unwrap();
    let mut f = Fixture::new();
    let c = StreamController::for_runtime();
    let h = Selected::new(&c, true);
    // Deliberately already expired: binding may not grant a fresh five seconds.
    let first = Instant::now() - Duration::from_secs(6);
    let work = Instant::now() + Duration::from_secs(30);
    thread::scope(|scope| {
        let _release_on_failure = ReleaseGates(&h.0.start, &h.0.close);
        let started = scope.spawn(|| spawn(&f, &c, work).is_err());
        h.0.start.wait(false);
        let closing = scope.spawn(|| c.close_at(first));
        h.0.close.wait(false); // owner closed; propagation still held
        h.0.start.release();
        let refused = started.join().unwrap();
        let control = h.admission().registration.control().unwrap();
        let state = control.state();
        let native = control.original_job_for_test().unwrap();
        h.0.close.release();
        closing.join().unwrap();
        assert!(refused);
        assert_eq!(state.driver, StartupDriverState::NeverLaunched);
        assert_eq!(native, (0, 0, true, true));
        assert_eq!(
            state.retirement_deadline,
            Some(first + Duration::from_secs(5))
        );
        assert!(state.retirement_deadline.unwrap() < Instant::now());
    });
    assert_eq!(c.reap().unwrap(), 0);
    f.clean();
}
#[test]
fn raw_finish_rejects_final_observer_crossing_original_retirement_bound() {
    let _serial = SERIAL.lock().unwrap();
    let mut f = Fixture::new();
    let c = StreamController::for_runtime();
    let h = Selected::new(&c, false);
    let work = Instant::now() + Duration::from_secs(30);
    let mut stream = spawn(&f, &c, work).unwrap();
    drain(&mut stream, work);
    let mut crossed = false;
    let result = stream.finish_checked(|_| {
        if let Some(r) = h.0.retired.lock().unwrap().as_ref() {
            assert!(r.joined && r.eof == (true, true) && r.deadline < work);
            let wait = Mutex::new(());
            let changed = Condvar::new();
            let mut guard = wait.lock().unwrap();
            while Instant::now() < r.deadline {
                guard = changed
                    .wait_timeout(guard, r.deadline.saturating_duration_since(Instant::now()))
                    .unwrap()
                    .0;
            }
            assert!(Instant::now() < work);
            assert!(c.owner.state.lock().unwrap().closed_at.is_none());
            crossed = true;
        }
        Ok(()) // Deliberately cooperative; production must reject the late return.
    });
    assert!(crossed);
    assert!(result.is_err());
    assert_eq!(c.reap().unwrap(), 0);
    f.clean();
}
#[test]
fn old_clone_cannot_reserve_or_enter_registered_start_after_close() {
    let _serial = SERIAL.lock().unwrap();
    let mut f = Fixture::new();
    let c = StreamController::for_runtime();
    let old = c.clone();
    let h = Selected::new(&c, false);
    c.close_at(Instant::now());
    let result = spawn(&f, &old, Instant::now() + Duration::from_secs(10));
    assert!(matches!(result, Err(e) if e.kind() == io::ErrorKind::Interrupted));
    assert!(h.0.admission.lock().unwrap().is_none());
    assert_eq!(c.reap().unwrap(), 0);
    f.clean();
}
#[test]
fn closed_reap_counts_collector_owned_reserved_entry() {
    let _serial = SERIAL.lock().unwrap();
    let c = StreamController::for_runtime();
    let r = c
        .reserve(None, Instant::now() + Duration::from_secs(10))
        .unwrap();
    c.close_at(Instant::now());
    assert!(c.reap().is_err());
    c.release(r.index);
    drop(r);
    assert_eq!(c.reap().unwrap(), 0);
}
#[test]
fn pure_latched_deadline_clips_acceptance_before_control_binding_or_propagation() {
    let _serial = SERIAL.lock().unwrap();
    let c = StreamController::for_runtime();
    let work = Instant::now() + Duration::from_secs(20);
    let r = c.reserve(None, work).unwrap();
    let first = Instant::now() - Duration::from_secs(6);
    let bound = first + Duration::from_secs(5);
    assert_eq!(r.admission.registration.latch_stop_at(first), bound);
    assert_eq!(
        r.admission.registration.latch_stop_at(Instant::now()),
        bound
    );
    assert_eq!(r.admission.registration.latched_deadline(), Some(bound));
    assert!(r.admission.registration.control().is_none());
    assert!(c.owner.state.lock().unwrap().closed_at.is_none());
    assert!(!r.admission.cancelled.load(Ordering::Acquire) && Instant::now() < work);
    assert_eq!(
        r.admission.acceptance(work).err().unwrap().kind(),
        io::ErrorKind::TimedOut
    );
    c.release(r.index);
    drop(r);
}
#[test]
fn reservation_keeps_exact_parent_and_foreign_binding_is_refused_without_cycle() {
    let _serial = SERIAL.lock().unwrap();
    let parent = Arc::new(crate::workspace_access::Broker::default());
    let weak = Arc::downgrade(&parent);
    let c = parent.managed_compiler_controller().unwrap();
    let r = c
        .reserve(None, Instant::now() + Duration::from_secs(10))
        .unwrap();
    assert!(Arc::ptr_eq(
        r.admission.parent_lease.as_ref().unwrap(),
        &parent
    ));
    assert!(c
        .bind_parent(&Arc::new(crate::workspace_access::Broker::default()))
        .is_err());
    drop(parent);
    assert!(weak.upgrade().is_some());
    c.release(r.index);
    drop(r);
    assert!(weak.upgrade().is_none());
}
#[test]
fn post_conversion_result_gate_refuses_owner_close() {
    let _serial = SERIAL.lock().unwrap();
    let mut f = Fixture::new();
    let c = StreamController::for_runtime();
    let result = command(&f, &c, Instant::now() + Duration::from_secs(15))
        .collect_checked(|_| Ok(()))
        .unwrap();
    let converted = (
        String::from_utf8_lossy(&result.stdout).into_owned(),
        String::from_utf8_lossy(&result.stderr).into_owned(),
    );
    assert!(!converted.0.is_empty() && !converted.1.is_empty());
    c.close_at(Instant::now());
    assert!(result.acceptance_gate().is_err());
    f.clean();
}
#[test]
fn positive_retirement_recovers_both_committed_slots_without_eof_promotion() {
    let _serial = SERIAL.lock().unwrap();
    let mut f = Fixture::new();
    let c = StreamController::for_runtime();
    let work = Instant::now() + Duration::from_secs(15);
    let h = Selected::new(&c, false);
    let mut capture = command(&f, &c, work);
    let committed = loop {
        let r = capture.resources_for_test();
        let generation = r.wake.generation();
        let slots: Vec<_> = [&r.stdout, &r.stderr]
            .into_iter()
            .map(|reader| {
                let slot = reader.as_ref().unwrap().shared.slot.lock().unwrap();
                slot.bytes[slot.at..slot.at + slot.length].to_vec()
            })
            .collect();
        if slots.iter().all(|bytes| !bytes.is_empty()) {
            break slots;
        }
        assert!(Instant::now() < work);
        r.wake.wait(generation, work);
    };
    assert!(!capture.resources_for_test().stdout_eof && !capture.resources_for_test().stderr_eof);
    let recovered = capture.retire_diagnostics().unwrap();
    assert_eq!(recovered.stdout, committed[0]);
    assert_eq!(recovered.stderr, committed[1]);
    let retired = h.0.retired.lock().unwrap();
    let retired = retired.as_ref().unwrap();
    assert!(retired.joined);
    assert_eq!(retired.eof, (false, false));
    assert!(recovered.timeout_delivery_gate().is_err());
    assert_eq!(c.reap().unwrap(), 0);
    f.clean();
}
#[test]
fn fixture_child() {
    let Some(role) = std::env::var_os("AURUM_OWNER_TEST_CHILD") else {
        return;
    };
    assert_eq!(role, "emit");
    std::io::stdout().write_all(b"owner-out-\xff").unwrap();
    std::io::stderr().write_all(b"owner-err-\xfe").unwrap();
    std::io::stdout().flush().unwrap();
    std::io::stderr().flush().unwrap();
    std::process::exit(0);
}
