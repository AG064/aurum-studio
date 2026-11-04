//! Owned asynchronous binary reads over the validation Worker's original pipes.
//! Client EOF/retirement does not establish Docker container retirement or grant
//! permission to publish. This module has no daemon or container operations.

use super::{PendingWorker, RegisteredStartup, StartupRegistration, Worker};
use crate::candidate_workspace::{LifetimeClaim, LifetimeFence};
use crate::operation_context::OperationContext;
use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[path = "worker_command_windows.rs"]
pub(crate) mod command;
#[cfg(test)]
#[path = "worker_fixture_diagnostic.rs"]
mod fixture_diagnostic;
#[cfg(test)]
#[path = "worker_real_cargo_diagnostic.rs"]
pub(crate) mod real_cargo_diagnostic;

const CHUNK: usize = 16 * 1024;
const HOST_SLOTS: usize = 2;
const MAX_STDOUT: u64 = 130 * 1024 * 1024;
const MAX_STDERR: u64 = 64 * 1024;
const POLL: Duration = Duration::from_millis(10);

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn WaitForSingleObject(handle: *mut c_void, millis: u32) -> u32;
    fn CancelSynchronousIo(thread: *mut c_void) -> i32;
    fn GetThreadId(thread: *mut c_void) -> u32;
    fn GetProcessIdOfThread(thread: *mut c_void) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn GetThreadTimes(
        thread: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetFileType(handle: *mut c_void) -> u32;
}

#[derive(Clone, Copy)]
pub(crate) struct StreamLimits {
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
}
impl StreamLimits {
    fn check(self) -> io::Result<Self> {
        if self.stdout_bytes > MAX_STDOUT || self.stderr_bytes > MAX_STDERR {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "binary stream limits exceed maxima",
            ))
        } else {
            Ok(self)
        }
    }
}
impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            stdout_bytes: MAX_STDOUT,
            stderr_bytes: MAX_STDERR,
        }
    }
}
pub(crate) struct StreamReceipt {
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
    pub(crate) stdout_eof: bool,
    pub(crate) stderr_eof: bool,
    pub(crate) root_exit_code: i32,
    pub(crate) owned_tree_retired: bool,
    pub(crate) owned_readers_joined: bool,
}
pub(crate) struct BinarySpawn<'a> {
    pub(crate) program: &'a Path,
    pub(crate) argv0: &'a Path,
    pub(crate) args: &'a [String],
    pub(crate) directory: Option<&'a Path>,
    pub(crate) overrides: &'a [(String, String)],
    pub(crate) lifetime: &'a LifetimeFence,
    pub(crate) deadline: Instant,
    pub(crate) limits: StreamLimits,
}

// There is only one host pool. Creating another controller cannot bypass its
// admission bound. Controller tokens are opaque Rust capabilities, never tool
// parameters. The runtime must retain/clone its controller across operations.
#[derive(Default)]
struct OwnerState {
    closed_at: Option<Instant>,
    parent: Option<Weak<crate::workspace_access::Broker>>,
}
struct Owner {
    state: Mutex<OwnerState>,
}
struct Admission {
    #[cfg(test)]
    fixture: Mutex<Option<fixture_diagnostic::Sink>>,
    #[cfg(test)]
    cargo_fixture: Mutex<Option<real_cargo_diagnostic::Sink>>,
    owner: Arc<Owner>,
    registration: StartupRegistration,
    cancelled: Arc<AtomicBool>,
    wake: Arc<Wake>,
    context: Option<OperationContext>,
    deadline: Instant,
    parent_lease: Option<Arc<crate::workspace_access::Broker>>,
}
struct Reservation {
    index: usize,
    admission: Arc<Admission>,
}
/// Orders one bounded result construction against owner close, not publication.
pub(crate) struct AcceptancePermit {
    guard: Option<MutexGuard<'static, [Entry; HOST_SLOTS]>>,
    parent_lease: Option<Arc<crate::workspace_access::Broker>>,
}
impl Drop for AcceptancePermit {
    fn drop(&mut self) {
        drop(self.guard.take());
        // Broker close/reap may reenter: release the parent after the pool lock.
        self.parent_lease.take();
    }
}
impl Admission {
    fn request_retirement(&self) -> Instant {
        let first = {
            let _pool = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
            self.owner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .closed_at
                .unwrap_or_else(Instant::now)
        };
        self.registration.request_stop_at(first)
    }
    fn acceptance(&self, deadline: Instant) -> io::Result<AcceptancePermit> {
        self.permit(deadline, false)
    }
    fn failed_delivery(&self, deadline: Instant) -> io::Result<AcceptancePermit> {
        self.permit(deadline, true)
    }
    fn permit(&self, deadline: Instant, expired_diagnostics: bool) -> io::Result<AcceptancePermit> {
        let parent_lease = self.parent_lease.clone();
        let guard = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        let binding_matches = match (&state.parent, &parent_lease) {
            (None, None) => true,
            (Some(bound), Some(parent)) => Weak::ptr_eq(bound, &Arc::downgrade(parent)),
            _ => false,
        };
        if state.closed_at.is_some()
            || !binding_matches
            || parent_lease
                .as_ref()
                .is_some_and(|parent| !parent.managed_output_is_open())
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "binary owner is closed",
            ));
        }
        drop(state);
        let deadline = self
            .registration
            .latched_deadline()
            .map_or(deadline.min(self.deadline), |retirement| {
                deadline.min(self.deadline).min(retirement)
            });
        if expired_diagnostics {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "binary admission was cancelled",
                ));
            }
            if let Some(context) = &self.context {
                context.check()?;
            }
            if Instant::now() < deadline {
                return Err(io::Error::other(
                    "failure diagnostics require original deadline expiry",
                ));
            }
        } else {
            check_control(&self.cancelled, self.context.as_ref(), deadline)?;
        }
        Ok(AcceptancePermit {
            guard: Some(guard),
            parent_lease,
        })
    }
    fn check(&self, deadline: Instant) -> io::Result<()> {
        drop(self.acceptance(deadline)?);
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct StreamController {
    owner: Arc<Owner>,
}
impl std::fmt::Debug for StreamController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamController")
            .finish_non_exhaustive()
    }
}
enum Entry {
    Empty,
    Reserved(Arc<Admission>),
    Reaping(Arc<Admission>),
    Retained(Arc<Admission>, Box<Resources>),
}
impl Entry {
    fn admission(&self) -> Option<&Arc<Admission>> {
        match self {
            Self::Empty => None,
            Self::Reserved(admission) | Self::Reaping(admission) | Self::Retained(admission, _) => {
                Some(admission)
            }
        }
    }
}
struct HostPool {
    entries: Mutex<[Entry; HOST_SLOTS]>,
}
fn pool() -> &'static HostPool {
    static POOL: OnceLock<HostPool> = OnceLock::new();
    POOL.get_or_init(|| HostPool {
        entries: Mutex::new(std::array::from_fn(|_| Entry::Empty)),
    })
}
impl StreamController {
    /// Private compiler factories require this exact live Broker binding.
    pub(crate) fn check_parent(
        &self,
        parent: &Arc<crate::workspace_access::Broker>,
    ) -> io::Result<()> {
        let state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed_at.is_some() || !parent.managed_output_is_open() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "managed compiler owner is closed",
            ));
        }
        if !state
            .parent
            .as_ref()
            .is_some_and(|bound| Weak::ptr_eq(bound, &Arc::downgrade(parent)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed compiler parent is unbound or foreign",
            ));
        }
        Ok(())
    }
    pub(crate) fn for_runtime() -> Self {
        Self {
            owner: Arc::new(Owner {
                state: Mutex::new(OwnerState::default()),
            }),
        }
    }
    /// Bind only a weak standing parent; admitted records hold the real Arc.
    pub(crate) fn bind_parent(
        &self,
        parent: &Arc<crate::workspace_access::Broker>,
    ) -> io::Result<()> {
        let entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed_at.is_some() || !parent.managed_output_is_open() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "binary owner is closed",
            ));
        }
        let expected = Arc::downgrade(parent);
        if let Some(bound) = &state.parent {
            if !Weak::ptr_eq(bound, &expected) {
                return Err(io::Error::other("binary owner belongs to another broker"));
            }
        } else {
            if entries.iter().any(|entry| {
                entry
                    .admission()
                    .is_some_and(|admission| Arc::ptr_eq(&admission.owner, &self.owner))
            }) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "binary parent binding requires no earlier admissions",
                ));
            }
            state.parent = Some(expected);
        }
        Ok(())
    }
    fn parent_lease(&self) -> io::Result<Option<Arc<crate::workspace_access::Broker>>> {
        let parent = {
            let _pool = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
            let state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed_at.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "binary owner is closed",
                ));
            }
            state.parent.clone()
        };
        // Upgrade/drop strong Broker references outside the pool critical section.
        parent
            .map(|parent| {
                parent
                    .upgrade()
                    .ok_or_else(|| io::Error::other("binary parent broker is unavailable"))
            })
            .transpose()
    }
    fn reserve(
        &self,
        context: Option<OperationContext>,
        deadline: Instant,
    ) -> io::Result<Reservation> {
        let parent_lease = self.parent_lease()?;
        let admission = Arc::new(Admission {
            #[cfg(test)]
            fixture: Mutex::new(None),
            #[cfg(test)]
            cargo_fixture: Mutex::new(None),
            owner: self.owner.clone(),
            registration: StartupRegistration::default(),
            cancelled: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Wake {
                generation: Mutex::new(0),
                changed: Condvar::new(),
            }),
            context,
            deadline,
            parent_lease,
        });
        let mut entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        let binding_matches = match (&state.parent, &admission.parent_lease) {
            (None, None) => true,
            (Some(bound), Some(parent)) => Weak::ptr_eq(bound, &Arc::downgrade(parent)),
            _ => false,
        };
        if state.closed_at.is_some()
            || !binding_matches
            || admission
                .parent_lease
                .as_ref()
                .is_some_and(|parent| !parent.managed_output_is_open())
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "binary owner is closed",
            ));
        }
        drop(state);
        let index = entries
            .iter()
            .position(|entry| matches!(entry, Entry::Empty))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "binary stream ownership pool is full",
                )
            })?;
        entries[index] = Entry::Reserved(admission.clone());
        Ok(Reservation { index, admission })
    }
    fn release(&self, index: usize) {
        let removed = {
            let mut entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
            let same = match &entries[index] {
                Entry::Reserved(admission) | Entry::Reaping(admission) => {
                    Arc::ptr_eq(&admission.owner, &self.owner)
                }
                _ => false,
            };
            assert!(same, "private stream reservation changed");
            std::mem::replace(&mut entries[index], Entry::Empty)
        };
        drop(removed);
    }
    fn retain(&self, index: usize, resources: Box<Resources>) {
        let removed = {
            let mut entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
            let same = match &entries[index] {
                Entry::Reserved(admission) | Entry::Reaping(admission) => {
                    Arc::ptr_eq(&admission.owner, &self.owner)
                        && Arc::ptr_eq(admission, &resources.admission)
                }
                _ => false,
            };
            assert!(same, "private stream reservation changed");
            let admission = resources.admission.clone();
            std::mem::replace(&mut entries[index], Entry::Retained(admission, resources))
        };
        drop(removed);
    }
    /// Close/reserve use one pool lock. Latches precede publication; original
    /// Shared signaling, wakes and any disposal occur after it.
    pub(crate) fn close_at(&self, requested: Instant) {
        let admissions = {
            let entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
            let mut state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
            let first = state
                .closed_at
                .map_or(requested, |stored| stored.min(requested));
            let admissions = entries
                .iter()
                .filter_map(|entry| entry.admission())
                .filter(|admission| Arc::ptr_eq(&admission.owner, &self.owner))
                .cloned()
                .collect::<Vec<_>>();
            for admission in &admissions {
                admission.registration.latch_stop_at(first);
                admission.cancelled.store(true, Ordering::Release);
            }
            state.closed_at = Some(first);
            admissions
        };
        #[cfg(test)]
        owner_tests::after_close_publish(self);
        for admission in admissions {
            let _ = admission.registration.propagate_stop();
            admission.wake.notify();
        }
    }
    /// Only positively retired resources leave the pool. A failed retry keeps
    /// the same handles, joins, allocation claim and original grace deadline.
    pub(crate) fn reap(&self) -> io::Result<usize> {
        reap(Some(&self.owner))
    }
    #[cfg(test)]
    pub(crate) fn retained(&self) -> usize {
        pool()
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|entry| {
                matches!(entry, Entry::Retained(admission, _) | Entry::Reaping(admission)
                    if Arc::ptr_eq(&admission.owner, &self.owner))
            })
            .count()
    }
}
fn reap(owner: Option<&Arc<Owner>>) -> io::Result<usize> {
    let mut incomplete = false;
    for index in 0..HOST_SLOTS {
        let taken = {
            let mut entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
            let eligible = matches!(&entries[index], Entry::Retained(key, _)
                if owner.is_none_or(|owner| Arc::ptr_eq(owner, &key.owner)));
            if !eligible {
                None
            } else {
                let Entry::Retained(key, resources) =
                    std::mem::replace(&mut entries[index], Entry::Empty)
                else {
                    unreachable!()
                };
                entries[index] = Entry::Reaping(key.clone());
                Some((
                    StreamController {
                        owner: key.owner.clone(),
                    },
                    resources,
                ))
            }
        };
        if let Some((controller, mut resources)) = taken {
            if resources.retire().is_ok() {
                controller.release(index);
                // Destructors, driver joins and context drops occur outside the
                // service lock; retained identities never become unreachable.
                drop(resources);
            } else {
                controller.retain(index, resources);
                incomplete = true;
            }
        }
    }
    let remaining = {
        let entries = pool().entries.lock().unwrap_or_else(|e| e.into_inner());
        let closed = owner.is_some_and(|owner| {
            owner
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .closed_at
                .is_some()
        });
        entries
            .iter()
            .filter(|entry| {
                entry.admission().is_some_and(|admission| {
                    owner.is_none_or(|owner| Arc::ptr_eq(owner, &admission.owner))
                        && (closed || matches!(entry, Entry::Retained(_, _) | Entry::Reaping(_)))
                })
            })
            .count()
    };
    if incomplete || remaining != 0 {
        Err(io::Error::other("binary stream recovery remains required"))
    } else {
        Ok(remaining)
    }
}

#[derive(Clone, Copy)]
enum End {
    Open,
    Eof,
    Stopped,
    Failed(io::ErrorKind),
    Limit,
}
struct Slot {
    bytes: [u8; CHUNK],
    at: usize,
    length: usize,
    total: u64,
    end: End,
}
struct Wake {
    generation: Mutex<u64>,
    changed: Condvar,
}
impl Wake {
    fn notify(&self) {
        let mut generation = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        *generation = generation.wrapping_add(1);
        self.changed.notify_all();
    }
    fn generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn wait(&self, observed: u64, deadline: Instant) {
        let generation = self.generation.lock().unwrap_or_else(|e| e.into_inner());
        if *generation == observed {
            let _guard = self.changed.wait_timeout(
                generation,
                POLL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
struct SharedPipe {
    #[cfg(test)]
    fixture: Option<fixture_diagnostic::PipeTap>,
    #[cfg(test)]
    cargo_fixture: Option<real_cargo_diagnostic::PipeTap>,
    slot: Mutex<Slot>,
    consumed: Condvar,
    stop: AtomicBool,
    wake: Arc<Wake>,
}
impl SharedPipe {
    fn end(&self, end: End) {
        #[cfg(test)]
        if let Some(tap) = &self.fixture {
            tap.sink.end(tap.pipe, end);
        }
        #[cfg(test)]
        if let Some(tap) = &self.cargo_fixture {
            tap.sink.end(tap.pipe, end);
        }
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).end = end;
        self.wake.notify();
    }
}
#[derive(Clone, Copy)]
struct ThreadIdentity {
    host: u32,
    tid: u32,
    created: u64,
}
fn thread_identity(thread: &JoinHandle<()>) -> io::Result<ThreadIdentity> {
    let raw = thread.as_raw_handle();
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    let host = unsafe { GetProcessIdOfThread(raw) };
    let tid = unsafe { GetThreadId(raw) };
    if host == 0
        || tid == 0
        || host != unsafe { GetCurrentProcessId() }
        || unsafe { GetThreadTimes(raw, &mut created, &mut exited, &mut kernel, &mut user) } == 0
    {
        return Err(io::Error::other("owned pipe thread identity unavailable"));
    }
    let created = ((created.high as u64) << 32) | created.low as u64;
    if created == 0 {
        return Err(io::Error::other("owned pipe thread creation unavailable"));
    }
    Ok(ThreadIdentity { host, tid, created })
}
struct PipeReader {
    shared: Arc<SharedPipe>,
    thread: Option<JoinHandle<()>>,
    identity: Option<ThreadIdentity>,
}
impl PipeReader {
    fn start(pipe: File, maximum: u64, wake: Arc<Wake>) -> io::Result<Self> {
        if unsafe { GetFileType(pipe.as_raw_handle()) } != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "owned output is not a pipe",
            ));
        }
        let shared = Arc::new(SharedPipe {
            #[cfg(test)]
            fixture: None,
            #[cfg(test)]
            cargo_fixture: None,
            slot: Mutex::new(Slot {
                bytes: [0; CHUNK],
                at: 0,
                length: 0,
                total: 0,
                end: End::Open,
            }),
            consumed: Condvar::new(),
            stop: AtomicBool::new(false),
            wake,
        });
        let child_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("aurum-owned-binary-pipe".into())
            .spawn(move || pump(pipe, maximum, &child_shared))?;
        // Keep the join even if identity capture fails. The caller refuses
        // delivery and performs/retains exact-object teardown.
        let identity = thread_identity(&thread).ok();
        Ok(Self {
            shared,
            thread: Some(thread),
            identity,
        })
    }
    #[cfg(test)]
    fn start_fixture(
        pipe: File,
        maximum: u64,
        wake: Arc<Wake>,
        tap: fixture_diagnostic::PipeTap,
    ) -> io::Result<Self> {
        if unsafe { GetFileType(pipe.as_raw_handle()) } != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "owned output is not a pipe",
            ));
        }
        let shared = Arc::new(SharedPipe {
            fixture: Some(tap),
            cargo_fixture: None,
            slot: Mutex::new(Slot {
                bytes: [0; CHUNK],
                at: 0,
                length: 0,
                total: 0,
                end: End::Open,
            }),
            consumed: Condvar::new(),
            stop: AtomicBool::new(false),
            wake,
        });
        let child_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("aurum-owned-binary-pipe".into())
            .spawn(move || pump(pipe, maximum, &child_shared))?;
        // Keep the join even if identity capture fails. The caller refuses
        // delivery and performs/retains exact-object teardown.
        let identity = thread_identity(&thread).ok();
        Ok(Self {
            shared,
            thread: Some(thread),
            identity,
        })
    }
    #[cfg(test)]
    fn start_cargo_fixture(
        pipe: File,
        maximum: u64,
        wake: Arc<Wake>,
        tap: real_cargo_diagnostic::PipeTap,
    ) -> io::Result<Self> {
        if unsafe { GetFileType(pipe.as_raw_handle()) } != 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "owned output is not a pipe",
            ));
        }
        let shared = Arc::new(SharedPipe {
            fixture: None,
            cargo_fixture: Some(tap),
            slot: Mutex::new(Slot {
                bytes: [0; CHUNK],
                at: 0,
                length: 0,
                total: 0,
                end: End::Open,
            }),
            consumed: Condvar::new(),
            stop: AtomicBool::new(false),
            wake,
        });
        let child_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("aurum-owned-binary-pipe".into())
            .spawn(move || pump(pipe, maximum, &child_shared))?;
        // Keep the join even if identity capture fails. The caller refuses
        // delivery and performs/retains exact-object teardown.
        let identity = thread_identity(&thread).ok();
        Ok(Self {
            shared,
            thread: Some(thread),
            identity,
        })
    }
    fn take(&self, output: Option<&mut [u8]>) -> io::Result<Option<usize>> {
        let mut slot = self.shared.slot.lock().unwrap_or_else(|e| e.into_inner());
        if slot.length != 0 {
            let count = output
                .as_ref()
                .map_or(slot.length, |bytes| slot.length.min(bytes.len()));
            if let Some(output) = output {
                output[..count].copy_from_slice(&slot.bytes[slot.at..slot.at + count]);
            }
            slot.at += count;
            slot.length -= count;
            #[cfg(test)]
            if let Some(tap) = &self.shared.fixture {
                tap.sink.consumed(tap.pipe, count);
            }
            #[cfg(test)]
            if let Some(tap) = &self.shared.cargo_fixture {
                tap.sink.consumed(tap.pipe, count);
            }
            if slot.length == 0 {
                slot.at = 0;
                self.shared.consumed.notify_all();
            }
            return Ok(Some(count));
        }
        match slot.end {
            End::Open => Ok(None),
            End::Eof => Ok(Some(0)),
            End::Stopped => Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "owned binary pipe was cancelled",
            )),
            End::Limit => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "owned binary pipe byte limit exceeded",
            )),
            End::Failed(kind) => Err(io::Error::new(kind, "owned binary pipe read failed")),
        }
    }
    fn stop(&self) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.consumed.notify_all();
        self.shared.wake.notify();
        let Some(thread) = &self.thread else {
            return Ok(());
        };
        if let Some(expected) = self.identity {
            let observed = thread_identity(thread)?;
            if observed.host != expected.host
                || observed.tid != expected.tid
                || observed.created != expected.created
            {
                return Err(io::Error::other("owned pipe thread identity changed"));
            }
        }
        if native_signal(thread)? {
            return Ok(());
        }
        if {
            let result = unsafe { CancelSynchronousIo(thread.as_raw_handle()) };
            #[cfg(test)]
            context_tests::record_cancel(result);
            result
        } == 0
            && io::Error::last_os_error().raw_os_error() != Some(1168)
        {
            return Err(io::Error::other("owned pipe cancellation unavailable"));
        }
        // ERROR_NOT_FOUND is not proof. A repeat covers read-submission races;
        // completion still requires the same thread signal and successful join.
        Ok(())
    }
    fn join_if_complete(&mut self) -> io::Result<bool> {
        let Some(thread) = &self.thread else {
            return Ok(true);
        };
        if !native_signal(thread)? {
            return Ok(false);
        }
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| io::Error::other("owned pipe reader panic"))?;
        Ok(true)
    }
    fn total(&self) -> u64 {
        self.shared
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .total
    }
}
fn native_signal(thread: &JoinHandle<()>) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(thread.as_raw_handle(), 0) } {
        0 => Ok(true),
        258 => Ok(false),
        _ => Err(io::Error::other("owned pipe thread wait unavailable")),
    }
}
fn pump(mut pipe: File, maximum: u64, shared: &SharedPipe) {
    // Both the blocking buffer and pipe belong to this retained thread, never
    // a caller's borrowed output slice. There is one fixed pending slot.
    let mut bytes = [0u8; CHUNK];
    #[cfg(test)]
    context_tests::before_read();
    #[cfg(test)]
    if let Some(tap) = &shared.fixture {
        tap.sink.before_read();
    }
    loop {
        let mut slot = shared.slot.lock().unwrap_or_else(|e| e.into_inner());
        while slot.length != 0 && !shared.stop.load(Ordering::Acquire) {
            slot = shared
                .consumed
                .wait(slot)
                .unwrap_or_else(|e| e.into_inner());
        }
        if shared.stop.load(Ordering::Acquire) {
            slot.end = End::Stopped;
            drop(slot);
            shared.wake.notify();
            return;
        }
        let cap = maximum
            .saturating_sub(slot.total)
            .saturating_add(1)
            .min(CHUNK as u64) as usize;
        drop(slot);
        let result = pipe.read(&mut bytes[..cap]);
        if shared.stop.load(Ordering::Acquire) {
            shared.end(End::Stopped);
            return;
        }
        let count = match result {
            Ok(0) => {
                shared.end(End::Eof);
                return;
            }
            #[cfg(not(test))]
            Ok(count) => count,
            #[cfg(test)]
            Ok(count) => {
                if let Some(tap) = &shared.fixture {
                    tap.sink.bytes(tap.pipe, &bytes[..count]);
                }
                if let Some(tap) = &shared.cargo_fixture {
                    tap.sink.bytes(tap.pipe, &bytes[..count]);
                }
                count
            }
            Err(error) if error.raw_os_error() == Some(109) => {
                shared.end(End::Eof);
                return;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                #[cfg(test)]
                if let Some(tap) = &shared.fixture {
                    tap.sink.origin(
                        fixture_diagnostic::Origin::Pipe(tap.pipe),
                        error.kind(),
                        error.raw_os_error(),
                    );
                }
                #[cfg(test)]
                if let Some(tap) = &shared.cargo_fixture {
                    tap.sink.origin(
                        real_cargo_diagnostic::Origin::Pipe(tap.pipe),
                        error.kind(),
                        error.raw_os_error(),
                    );
                }
                shared.end(End::Failed(error.kind()));
                return;
            }
        };
        let mut slot = shared.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.total += count as u64;
        if slot.total > maximum {
            slot.end = End::Limit;
            drop(slot);
            shared.wake.notify();
            return;
        }
        slot.bytes[..count].copy_from_slice(&bytes[..count]);
        slot.at = 0;
        slot.length = count;
        drop(slot);
        shared.wake.notify();
    }
}

struct Resources {
    // Opaque original caller resources, registered before native startup. They
    // follow this same Resources object through uncertain retirement/parking.
    retained: Option<Box<dyn Send>>,
    admission: Arc<Admission>,
    worker: Option<Worker>,
    pending: Option<PendingWorker>,
    stdout: Option<PipeReader>,
    stderr: Option<PipeReader>,
    wake: Arc<Wake>,
    cancelled: Arc<AtomicBool>,
    context: Option<OperationContext>,
    claim: Option<LifetimeClaim>,
    deadline: Instant,
    retirement_deadline: Option<Instant>,
    stdout_eof: bool,
    stderr_eof: bool,
    command_prefixes: Option<command::Prefixes>,
}
impl Resources {
    fn check(&self, deadline: Instant) -> io::Result<()> {
        self.admission.check(deadline.min(self.deadline))
    }
    fn request_retirement(&mut self) -> Instant {
        let requested = self.admission.request_retirement();
        let deadline = self
            .retirement_deadline
            .map_or(requested, |stored| stored.min(requested));
        self.retirement_deadline = Some(deadline);
        deadline
    }
    fn stop_readers(&self) {
        for reader in [&self.stdout, &self.stderr].into_iter().flatten() {
            let _ = reader.stop();
        }
    }
    fn retire(&mut self) -> io::Result<()> {
        self.request_retirement();
        self.stop_readers();
        let tree = if let Some(worker) = &mut self.worker {
            worker.retire_binary()
        } else if let Some(pending) = &mut self.pending {
            pending.retire()
        } else {
            Ok(())
        };
        let mut readers = false;
        loop {
            let deadline = self.request_retirement();
            self.stop_readers();
            let mut complete = true;
            for reader in [&mut self.stdout, &mut self.stderr].into_iter().flatten() {
                match reader.join_if_complete() {
                    Ok(done) => complete &= done,
                    Err(_) => complete = false,
                }
            }
            if complete {
                readers = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            let generation = self.wake.generation();
            self.wake.wait(generation, deadline);
        }
        if tree.is_ok() && readers {
            if let Some(mut claim) = self.claim.take() {
                claim.resolve();
            }
            Ok(())
        } else {
            if let Some(context) = &self.context {
                context.mark_termination_unconfirmed();
                context.mark_recovery_required();
            }
            Err(io::Error::other(
                "owned binary retirement requires recovery",
            ))
        }
    }
    fn drain_stderr(&mut self) -> io::Result<()> {
        if self.stderr_eof {
            return Ok(());
        }
        if let Some(reader) = &self.stderr {
            if reader.take(None)? == Some(0) {
                self.stderr_eof = true;
            }
        }
        Ok(())
    }
}

fn check_control(
    cancelled: &AtomicBool,
    context: Option<&OperationContext>,
    deadline: Instant,
) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "owned binary stream was cancelled",
        ));
    }
    if let Some(context) = context {
        context.check()?;
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "owned binary read deadline expired",
        ));
    }
    Ok(())
}

pub(crate) struct BinaryStream {
    #[cfg(test)]
    fixture: Option<fixture_diagnostic::Sink>,
    #[cfg(test)]
    cargo_fixture: Option<real_cargo_diagnostic::Sink>,
    controller: StreamController,
    reservation: usize,
    resources: Option<Box<Resources>>,
}
impl BinaryStream {
    #[cfg(test)]
    pub(crate) fn spawn(
        controller: &StreamController,
        request: BinarySpawn<'_>,
    ) -> io::Result<Self> {
        Self::spawn_environment(controller, request, false)
    }
    pub(super) fn spawn_cleared(
        controller: &StreamController,
        request: BinarySpawn<'_>,
    ) -> io::Result<Self> {
        Self::spawn_environment(controller, request, true)
    }
    pub(super) fn spawn_cleared_retaining(
        controller: &StreamController, request: BinarySpawn<'_>, retained: Box<dyn Send>,
    ) -> io::Result<Self> {
        Self::spawn_environment_retaining(controller, request, true, Some(retained))
    }
    fn spawn_environment(
        controller: &StreamController, request: BinarySpawn<'_>, clear_environment: bool,
    ) -> io::Result<Self> {
        Self::spawn_environment_retaining(controller, request, clear_environment, None)
    }
    #[cfg(not(test))]
    fn spawn_environment_retaining(
        controller: &StreamController,
        request: BinarySpawn<'_>,
        clear_environment: bool,
        retained: Option<Box<dyn Send>>,
    ) -> io::Result<Self> {
        let BinarySpawn {
            program,
            argv0,
            args,
            directory,
            overrides,
            lifetime,
            deadline,
            limits,
        } = request;
        let limits = limits.check()?;
        let context = OperationContext::current();
        if let Some(context) = &context {
            context.check()?;
        }
        let deadline = context
            .as_ref()
            .map_or(deadline, |context| deadline.min(context.deadline()));
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "binary worker deadline expired before admission",
            ));
        }
        let reserved = controller.reserve(context.clone(), deadline)?;
        let reservation = reserved.index;
        let admission = reserved.admission;
        let claim = match lifetime.claim() {
            Ok(claim) => claim,
            Err(error) => {
                controller.release(reservation);
                return Err(error);
            }
        };
        let wake = admission.wake.clone();
        let mut stream = Self {
            controller: controller.clone(),
            reservation,
            resources: Some(Box::new(Resources {
                retained,
                admission: admission.clone(),
                worker: None,
                pending: None,
                stdout: None,
                stderr: None,
                wake: wake.clone(),
                cancelled: admission.cancelled.clone(),
                context,
                claim: Some(claim),
                deadline,
                retirement_deadline: None,
                stdout_eof: false,
                stderr_eof: false,
                command_prefixes: None,
            })),
        };
        #[cfg(test)]
        owner_tests::before_registered_start(&admission);
        let spawned = Worker::spawn_retained_environment_registered(
            program,
            argv0,
            args,
            directory,
            if clear_environment {
                super::ChildEnvironment::Cleared(overrides)
            } else {
                super::ChildEnvironment::Inherit(overrides)
            },
            RegisteredStartup {
                lifetime: Some(lifetime),
                requested_deadline: Some(deadline),
                registration: admission.registration.clone(),
            },
        );
        let (worker, stdout, stderr) = match spawned {
            Ok(spawned) => spawned,
            Err(failure) => {
                stream.resources.as_mut().unwrap().pending = failure.pending;
                return Err(failure.error);
            }
        };
        let resources = stream.resources.as_mut().unwrap();
        resources.worker = Some(worker);
        resources.check(deadline)?;
        resources.stdout = Some(PipeReader::start(
            stdout,
            limits.stdout_bytes,
            wake.clone(),
        )?);
        resources.stderr = Some(PipeReader::start(stderr, limits.stderr_bytes, wake)?);
        if resources.stdout.as_ref().unwrap().identity.is_none()
            || resources.stderr.as_ref().unwrap().identity.is_none()
        {
            return Err(io::Error::other(
                "binary pipe original thread identity unavailable",
            ));
        }
        resources.check(deadline)?;
        Ok(stream)
    }
    #[cfg(test)]
    fn spawn_environment_retaining(
        controller: &StreamController,
        request: BinarySpawn<'_>,
        clear_environment: bool,
        retained: Option<Box<dyn Send>>,
    ) -> io::Result<Self> {
        Self::spawn_environment_fixture_retaining(controller, request, clear_environment, None, retained)
    }
    #[cfg(test)]
    fn spawn_environment_fixture(
        controller: &StreamController,
        request: BinarySpawn<'_>,
        clear_environment: bool,
        fixture: Option<fixture_diagnostic::Sink>,
    ) -> io::Result<Self> {
        Self::spawn_environment_fixture_retaining(controller, request, clear_environment, fixture, None)
    }
    #[cfg(test)]
    fn spawn_environment_fixture_retaining(
        controller: &StreamController, request: BinarySpawn<'_>, clear_environment: bool,
        fixture: Option<fixture_diagnostic::Sink>, retained: Option<Box<dyn Send>>,
    ) -> io::Result<Self> {
        let BinarySpawn {
            program,
            argv0,
            args,
            directory,
            overrides,
            lifetime,
            deadline,
            limits,
        } = request;
        let limits = limits.check()?;
        let context = OperationContext::current();
        if let Some(context) = &context {
            context.check()?;
        }
        let deadline = context
            .as_ref()
            .map_or(deadline, |context| deadline.min(context.deadline()));
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "binary worker deadline expired before admission",
            ));
        }
        let reserved = controller.reserve(context.clone(), deadline)?;
        let reservation = reserved.index;
        let admission = reserved.admission;
        *admission.fixture.lock().unwrap_or_else(|e| e.into_inner()) = fixture.clone();
        let claim = match lifetime.claim() {
            Ok(claim) => claim,
            Err(error) => {
                controller.release(reservation);
                return Err(error);
            }
        };
        let wake = admission.wake.clone();
        let mut stream = Self {
            fixture: fixture.clone(),
            cargo_fixture: None,
            controller: controller.clone(),
            reservation,
            resources: Some(Box::new(Resources {
                retained,
                admission: admission.clone(),
                worker: None,
                pending: None,
                stdout: None,
                stderr: None,
                wake: wake.clone(),
                cancelled: admission.cancelled.clone(),
                context,
                claim: Some(claim),
                deadline,
                retirement_deadline: None,
                stdout_eof: false,
                stderr_eof: false,
                command_prefixes: None,
            })),
        };
        #[cfg(test)]
        owner_tests::before_registered_start(&admission);
        let spawned = Worker::spawn_retained_environment_registered(
            program,
            argv0,
            args,
            directory,
            if clear_environment {
                super::ChildEnvironment::Cleared(overrides)
            } else {
                super::ChildEnvironment::Inherit(overrides)
            },
            RegisteredStartup {
                lifetime: Some(lifetime),
                requested_deadline: Some(deadline),
                registration: admission.registration.clone(),
            },
        );
        let (worker, stdout, stderr) = match spawned {
            Ok(spawned) => spawned,
            Err(failure) => {
                if let (Some(sink), Some(pending)) = (&fixture, failure.pending.as_ref()) {
                    if let Some((kind, raw, phase)) = pending.census.fixture_failure() {
                        sink.origin(fixture_diagnostic::Origin::Census(phase), kind, raw);
                    }
                }
                stream.resources.as_mut().unwrap().pending = failure.pending;
                return Err(failure.error);
            }
        };
        let resources = stream.resources.as_mut().unwrap();
        resources.worker = Some(worker);
        resources.check(deadline)?;
        resources.stdout = Some(if let Some(sink) = &fixture {
            PipeReader::start_fixture(
                stdout,
                limits.stdout_bytes,
                wake.clone(),
                fixture_diagnostic::PipeTap {
                    sink: sink.clone(),
                    pipe: fixture_diagnostic::Pipe::Stdout,
                },
            )?
        } else {
            PipeReader::start(stdout, limits.stdout_bytes, wake.clone())?
        });
        resources.stderr = Some(if let Some(sink) = &fixture {
            PipeReader::start_fixture(
                stderr,
                limits.stderr_bytes,
                wake,
                fixture_diagnostic::PipeTap {
                    sink: sink.clone(),
                    pipe: fixture_diagnostic::Pipe::Stderr,
                },
            )?
        } else {
            PipeReader::start(stderr, limits.stderr_bytes, wake)?
        });
        if resources.stdout.as_ref().unwrap().identity.is_none()
            || resources.stderr.as_ref().unwrap().identity.is_none()
        {
            return Err(io::Error::other(
                "binary pipe original thread identity unavailable",
            ));
        }
        resources.check(deadline)?;
        Ok(stream)
    }
    #[cfg(test)]
    fn spawn_cargo_fixture(
        controller: &StreamController,
        request: BinarySpawn<'_>,
        sink: real_cargo_diagnostic::Sink,
    ) -> io::Result<Self> {
        let BinarySpawn {
            program,
            argv0,
            args,
            directory,
            overrides,
            lifetime,
            deadline,
            limits,
        } = request;
        let limits = limits.check()?;
        let context = OperationContext::current();
        if let Some(context) = &context {
            context.check()?;
        }
        let deadline = context
            .as_ref()
            .map_or(deadline, |context| deadline.min(context.deadline()));
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "binary worker deadline expired before admission",
            ));
        }
        let reserved = controller.reserve(context.clone(), deadline)?;
        let reservation = reserved.index;
        let admission = reserved.admission;
        if let Err(error) = sink.admitted(&admission) {
            controller.release(reservation);
            return Err(error);
        }
        *admission
            .cargo_fixture
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(sink.clone());
        let claim = match lifetime.claim() {
            Ok(claim) => claim,
            Err(error) => {
                controller.release(reservation);
                return Err(error);
            }
        };
        let wake = admission.wake.clone();
        let mut stream = Self {
            fixture: None,
            cargo_fixture: Some(sink.clone()),
            controller: controller.clone(),
            reservation,
            resources: Some(Box::new(Resources {
                retained: None,
                admission: admission.clone(),
                worker: None,
                pending: None,
                stdout: None,
                stderr: None,
                wake: wake.clone(),
                cancelled: admission.cancelled.clone(),
                context,
                claim: Some(claim),
                deadline,
                retirement_deadline: None,
                stdout_eof: false,
                stderr_eof: false,
                command_prefixes: None,
            })),
        };
        #[cfg(test)]
        owner_tests::before_registered_start(&admission);
        let spawned = Worker::spawn_retained_environment_registered(
            program,
            argv0,
            args,
            directory,
            super::ChildEnvironment::Cleared(overrides),
            RegisteredStartup {
                lifetime: Some(lifetime),
                requested_deadline: Some(deadline),
                registration: admission.registration.clone(),
            },
        );
        let (worker, stdout, stderr) = match spawned {
            Ok(spawned) => spawned,
            Err(failure) => {
                if let Some(pending) = failure.pending.as_ref() {
                    if let Some((kind, raw, phase)) = pending.census.fixture_failure() {
                        sink.origin(real_cargo_diagnostic::Origin::Census(phase), kind, raw);
                    }
                }
                stream.resources.as_mut().unwrap().pending = failure.pending;
                return Err(failure.error);
            }
        };
        let resources = stream.resources.as_mut().unwrap();
        resources.worker = Some(worker);
        resources.check(deadline)?;
        resources.stdout = Some(PipeReader::start_cargo_fixture(
            stdout,
            limits.stdout_bytes,
            wake.clone(),
            real_cargo_diagnostic::PipeTap {
                sink: sink.clone(),
                pipe: real_cargo_diagnostic::Pipe::Stdout,
            },
        )?);
        resources.stderr = Some(PipeReader::start_cargo_fixture(
            stderr,
            limits.stderr_bytes,
            wake,
            real_cargo_diagnostic::PipeTap {
                sink: sink.clone(),
                pipe: real_cargo_diagnostic::Pipe::Stderr,
            },
        )?);
        if resources.stdout.as_ref().unwrap().identity.is_none()
            || resources.stderr.as_ref().unwrap().identity.is_none()
        {
            return Err(io::Error::other(
                "binary pipe original thread identity unavailable",
            ));
        }
        resources.check(deadline)?;
        Ok(stream)
    }
    #[cfg(test)]
    pub(crate) fn read_before(
        &mut self,
        output: &mut [u8],
        supplied: Instant,
    ) -> io::Result<usize> {
        self.read_with_check(output, supplied, &mut |_| Ok(()), false)
    }
    /// The trusted caller's observer runs synchronously under the same work
    /// deadline. It must bound its own filesystem operations and never commit.
    pub(crate) fn read_before_checked(
        &mut self,
        output: &mut [u8],
        supplied: Instant,
        mut check: impl FnMut(Instant) -> io::Result<()>,
    ) -> io::Result<usize> {
        self.read_with_check(output, supplied, &mut check, true)
    }
    fn read_with_check(
        &mut self,
        output: &mut [u8],
        supplied: Instant,
        check: &mut dyn FnMut(Instant) -> io::Result<()>,
        observed: bool,
    ) -> io::Result<usize> {
        if output.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "binary read buffer is empty",
            ));
        }
        let resources = self
            .resources
            .as_mut()
            .ok_or_else(|| io::Error::other("binary stream is already retired"))?;
        let deadline = supplied.min(resources.deadline);
        let result = (|| loop {
            resources.check(deadline)?;
            let checked = check(deadline);
            if observed {
                resources.check(deadline)?;
            }
            checked?;
            let generation = resources.wake.generation();
            resources.drain_stderr()?;
            let read = resources
                .stdout
                .as_ref()
                .unwrap()
                .take(Some(&mut *output))?;
            resources.check(deadline)?;
            if let Some(count) = read {
                if count == 0 {
                    resources.stdout_eof = true;
                }
                return Ok(count);
            }
            resources.wake.wait(generation, deadline);
        })();
        if result.is_err() {
            resources.stop_readers();
        }
        #[cfg(test)]
        if let Some(sink) = &self.fixture {
            sink.observed(resources, None);
            sink.api(fixture_diagnostic::Stage::Read, &result);
            sink.returned(&result);
        }
        #[cfg(test)]
        if let Some(sink) = &self.cargo_fixture {
            sink.observed(resources, None);
            sink.api(real_cargo_diagnostic::Stage::Read, &result);
            sink.returned(&result);
        }
        result
    }
    /// Finish proves the client only. The caller must separately prove daemon
    /// container retirement, artifact hashes and source/commit preconditions.
    /// Positively retired resources are released even if the work deadline
    /// expires during cleanup. Such completion cannot authorize acceptance.
    pub(crate) fn finish_checked(
        &mut self,
        mut check: impl FnMut(Instant) -> io::Result<()>,
    ) -> io::Result<StreamReceipt> {
        let result = self.finish_with_check(&mut check, true);
        #[cfg(test)]
        if let Some(sink) = &self.cargo_fixture {
            if let Some(resources) = &self.resources {
                sink.observed(resources, None);
            }
            sink.api(real_cargo_diagnostic::Stage::Finish, &result);
        }
        #[cfg(test)]
        if let Some(sink) = &self.fixture {
            if let Some(resources) = &self.resources {
                sink.observed(resources, None);
            }
            sink.api(fixture_diagnostic::Stage::Finish, &result);
        }
        if result.is_err() {
            if let Some(resources) = &self.resources {
                resources.stop_readers();
            }
        }
        result
    }
    fn finish_with_check(
        &mut self,
        check: &mut dyn FnMut(Instant) -> io::Result<()>,
        observed: bool,
    ) -> io::Result<StreamReceipt> {
        let resources = self
            .resources
            .as_mut()
            .ok_or_else(|| io::Error::other("binary stream is already retired"))?;
        if !resources.stdout_eof {
            return Err(io::Error::other("binary stdout EOF has not been observed"));
        }
        let code = loop {
            resources.check(resources.deadline)?;
            let checked = check(resources.deadline);
            if observed {
                resources.check(resources.deadline)?;
            }
            checked?;
            let generation = resources.wake.generation();
            resources.drain_stderr()?;
            let code = resources.worker.as_ref().unwrap().try_wait()?;
            if let Some(code) = code {
                if resources.stderr_eof {
                    break code;
                }
            }
            resources.wake.wait(generation, resources.deadline);
        };
        let receipt = StreamReceipt {
            stdout_bytes: resources.stdout.as_ref().unwrap().total(),
            stderr_bytes: resources.stderr.as_ref().unwrap().total(),
            stdout_eof: true,
            stderr_eof: resources.stderr_eof,
            root_exit_code: code,
            owned_tree_retired: true,
            owned_readers_joined: true,
        };
        #[cfg(test)]
        if let Some(sink) = &self.fixture {
            sink.observed(resources, Some(code));
        }
        #[cfg(test)]
        if let Some(sink) = &self.cargo_fixture {
            sink.observed(resources, Some(code));
        }
        resources.retire()?;
        #[cfg(test)]
        if let Some(sink) = &self.fixture {
            sink.completed(&receipt);
        }
        #[cfg(test)]
        if let Some(sink) = &self.cargo_fixture {
            sink.completed(&receipt);
        }
        let delivery_deadline = resources
            .retirement_deadline
            .map_or(resources.deadline, |retirement| {
                resources.deadline.min(retirement)
            });
        #[cfg(test)]
        owner_tests::after_retired(resources);
        #[cfg(test)]
        if observed {
            storage_tests::after_verified_retirement(delivery_deadline);
        }
        let approval = observed.then(|| {
            (
                resources.cancelled.clone(),
                resources.context.clone(),
                delivery_deadline,
            )
        });
        let admission = resources.admission.clone();
        let resources = self.resources.take().unwrap();
        self.controller.release(self.reservation);
        drop(resources);
        if let Some((cancelled, context, deadline)) = approval {
            check_control(&cancelled, context.as_ref(), deadline)?;
            let checked = check(deadline);
            check_control(&cancelled, context.as_ref(), deadline)?;
            checked?;
        }
        if code != 0 {
            return Err(io::Error::other("binary client exited unsuccessfully"));
        }
        let permit = admission.acceptance(delivery_deadline)?;
        let accepted = receipt;
        drop(permit);
        Ok(accepted)
    }
    /// Idempotent cancellation uses the original grace and objects. On error
    /// keep this stream alive or Drop will park it in the reachable host pool.
    pub(crate) fn retire(&mut self) -> io::Result<()> {
        let Some(resources) = self.resources.as_mut() else {
            #[cfg(test)]
            if let Some(sink) = &self.fixture {
                sink.cleanup(false, true);
            }
            #[cfg(test)]
            if let Some(sink) = &self.cargo_fixture {
                sink.cleanup(false, true);
            }
            return Ok(());
        };
        #[cfg(not(test))]
        resources.retire()?;
        #[cfg(test)]
        {
            let result = resources.retire();
            if let Some(sink) = &self.fixture {
                sink.observed(resources, None);
                sink.cleanup(true, result.is_ok());
            }
            if let Some(sink) = &self.cargo_fixture {
                sink.observed(resources, None);
                sink.cleanup(true, result.is_ok());
            }
            result?;
        }
        let resources = self.resources.take().unwrap();
        self.controller.release(self.reservation);
        drop(resources);
        Ok(())
    }
}
impl Drop for BinaryStream {
    fn drop(&mut self) {
        if let Some(mut resources) = self.resources.take() {
            let retired = resources.retire();
            #[cfg(test)]
            if let Some(sink) = &self.cargo_fixture {
                sink.observed(&resources, None);
                sink.cleanup(true, retired.is_ok());
            }
            if retired.is_ok() {
                self.controller.release(self.reservation);
                drop(resources);
            } else {
                self.controller.retain(self.reservation, resources);
            }
        }
    }
}

#[cfg(test)]
mod configfile_retention_tests {
    use super::*;
    struct DropWitness(Arc<AtomicBool>);
    impl Drop for DropWitness { fn drop(&mut self) { self.0.store(true,Ordering::Release); } }
    #[test]
    fn configfile_parser_retention_follows_original_pool_until_known_no_spawn_retirement() {
        // Actual pool/claim/retirement machinery, with affirmative no-spawn.
        // This does not simulate a live engine or claim process termination.
        let broker=Arc::new(crate::workspace_access::Broker::default());
        let controller=broker.managed_compiler_controller().unwrap();
        let deadline=Instant::now()+Duration::from_secs(3);
        let reserved=controller.reserve(None,deadline).unwrap();
        let fence=LifetimeFence::default();
        let dropped=Arc::new(AtomicBool::new(false));
        let resources=Box::new(Resources {
            retained:Some(Box::new(DropWitness(dropped.clone()))),admission:reserved.admission.clone(),
            worker:None,pending:None,stdout:None,stderr:None,wake:reserved.admission.wake.clone(),
            cancelled:reserved.admission.cancelled.clone(),context:None,claim:Some(fence.claim().unwrap()),
            deadline,retirement_deadline:None,stdout_eof:false,stderr_eof:false,command_prefixes:None,
        });
        controller.retain(reserved.index,resources);
        assert!(!dropped.load(Ordering::Acquire));
        assert!(fence.unresolved());
        controller.reap().unwrap();
        assert!(dropped.load(Ordering::Acquire));
        assert!(!fence.unresolved());
        assert_eq!(controller.retained(),0);
    }
}

#[cfg(test)]
#[path = "worker_binary_context_tests.rs"]
mod context_tests;

#[cfg(test)]
#[path = "worker_binary_storage_tests.rs"]
mod storage_tests;

#[cfg(test)]
#[path = "worker_owner_admission_tests.rs"]
mod owner_tests;
