//! Private Windows birth-object census. This is lifecycle control, not isolation.
use super::{check, Accounting, FileTime, Handle, QueryInformationJobObject};
use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_BIRTHS: usize = 4096;
const EVENT_WAIT: u32 = 20;
const STOP_GRACE: Duration = Duration::from_secs(5);
const QUERY_SYNC: u32 = 0x1000 | 0x0010_0000;

#[repr(C)]
#[derive(Clone, Copy)]
struct ExceptionRecord {
    code: u32,
    flags: u32,
    record: *mut ExceptionRecord,
    address: Handle,
    parameters: u32,
    information: [usize; 15],
}
#[repr(C)]
#[derive(Clone, Copy)]
struct ExceptionInfo {
    record: ExceptionRecord,
    first_chance: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CreateInfo {
    file: Handle,
    process: Handle,
    thread: Handle,
    image: Handle,
    debug_offset: u32,
    debug_size: u32,
    tls: Handle,
    entry: Handle,
    name: Handle,
    unicode: u16,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct LoadInfo {
    file: Handle,
    image: Handle,
    offset: u32,
    size: u32,
    name: Handle,
    unicode: u16,
}
#[repr(C)]
union EventData {
    exception: ExceptionInfo,
    create: CreateInfo,
    load: LoadInfo,
}
#[repr(C)]
struct DebugEvent {
    code: u32,
    pid: u32,
    tid: u32,
    data: EventData,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn WaitForDebugEvent(event: *mut DebugEvent, milliseconds: u32) -> i32;
    fn ContinueDebugEvent(pid: u32, tid: u32, status: u32) -> i32;
    fn DebugSetProcessKillOnExit(kill: i32) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn GetProcessId(process: Handle) -> u32;
    fn DuplicateHandle(
        source_process: Handle,
        source: Handle,
        target_process: Handle,
        target: *mut Handle,
        access: u32,
        inherit: i32,
        flags: u32,
    ) -> i32;
    fn GetProcessTimes(
        process: Handle,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn IsProcessInJob(process: Handle, job: Handle, result: *mut i32) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn IsWow64Process(process: Handle, wow64: *mut i32) -> i32;
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        name: *mut u16,
        length: *mut u32,
    ) -> i32;
    fn GetSystemWindowsDirectoryW(name: *mut u16, capacity: u32) -> u32;
    fn CompareStringOrdinal(
        left: *const u16,
        left_len: i32,
        right: *const u16,
        right_len: i32,
        ignore_case: i32,
    ) -> i32;
    fn GetModuleHandleW(name: *const u16) -> Handle;
    fn GetProcAddress(module: Handle, name: *const u8) -> Handle;
}
fn same_object(left: Handle, right: Handle) -> io::Result<bool> {
    type Compare = unsafe extern "system" fn(Handle, Handle) -> i32;
    static COMPARATOR: std::sync::OnceLock<Option<Compare>> = std::sync::OnceLock::new();
    let compare = COMPARATOR
        .get_or_init(|| {
            let name: Vec<u16> = "kernelbase.dll\0".encode_utf16().collect();
            let module = unsafe { GetModuleHandleW(name.as_ptr()) };
            if module.is_null() {
                return None;
            }
            let function =
                unsafe { GetProcAddress(module, c"CompareObjectHandles".as_ptr().cast()) };
            if function.is_null() {
                None
            } else {
                Some(unsafe { std::mem::transmute::<Handle, Compare>(function) })
            }
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "retained object comparator is unavailable",
            )
        })?;
    Ok(unsafe { compare(left, right) } != 0)
}

pub(super) struct Spawned {
    pub process: OwnedHandle,
    pub pid: u32,
    pub stdout: File,
    pub stderr: File,
}
struct Birth {
    pid: u32,
    created: u64,
    handle: OwnedHandle,
    exit_continued: bool,
    debug_birth: bool,
    system_console: bool,
    wow64: bool,
}
#[derive(Clone)]
struct Failure {
    kind: io::ErrorKind,
    raw: Option<i32>,
    phase: &'static str,
}
impl Failure {
    fn from(error: io::Error, phase: &'static str) -> Self {
        Self {
            kind: error.kind(),
            raw: error.raw_os_error(),
            phase,
        }
    }
    fn error(&self) -> io::Error {
        // Never retain OS descriptions containing application paths or input.
        io::Error::new(
            self.kind,
            format!(
                "owned worker retirement evidence unavailable: {} (os={:?})",
                self.phase, self.raw
            ),
        )
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StartupDriverState {
    #[default]
    Preparing,
    LaunchPending,
    Bound,
    NeverLaunched,
    Joined,
    Detached,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct StartupRetirementState {
    pub(crate) driver: StartupDriverState,
    pub(crate) retirement_deadline: Option<Instant>,
}
#[derive(Default)]
struct State {
    births: Vec<Birth>,
    root: Option<(u32, u64)>,
    retiring: bool,
    retirement_deadline: Option<Instant>,
    acknowledged: bool,
    done: bool,
    failure: Option<Failure>,
    claim: Option<crate::candidate_workspace::LifetimeClaim>,
    no_spawn_proved: bool,
    driver: StartupDriverState,
}
impl State {
    fn retire(&mut self, now: Instant) -> Instant {
        self.retiring = true;
        *self.retirement_deadline.get_or_insert(now + STOP_GRACE)
    }
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}
impl Shared {
    fn no_spawn(&self, job: &OwnedHandle) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        // The creator returned before a successful native creation, or the
        // creator thread never started. Zero active count alone is insufficient.
        let info = accounting(job)?;
        if info.total_processes != 0
            || info.active_processes != 0
            || state.root.is_some()
            || !state.births.is_empty()
        {
            return Err(io::Error::other("native no-spawn accounting disagrees"));
        }
        if let Some(mut claim) = state.claim.take() {
            claim.resolve();
        }
        state.no_spawn_proved = true;
        Ok(())
    }
    fn failure(&self, error: io::Error, phase: &'static str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .failure
            .get_or_insert_with(|| Failure::from(error, phase));
        state.retire(Instant::now());
        self.changed.notify_all();
    }
    fn finish(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .done = true;
        self.changed.notify_all();
    }
}

pub(super) struct Census {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
/// Cancellation/status only; the original Census remains the driver-handle owner.
/// The job lease keeps this exact native parent alive during pending startup.
#[derive(Clone)]
pub(crate) struct StartupRetirementControl {
    shared: Arc<Shared>,
    job: Arc<OwnedHandle>,
}
impl StartupRetirementControl {
    #[cfg(test)]
    pub(crate) fn original_job_for_test(&self) -> io::Result<(u32, u32, bool, bool)> {
        let info = accounting(&self.job)?;
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        Ok((
            info.total_processes,
            info.active_processes,
            state.no_spawn_proved,
            state.claim.is_none(),
        ))
    }

    /// Apply the first close time even if registration was delayed past expiry.
    /// An already earlier census deadline wins; no new grace begins here.
    pub(crate) fn request_stop_at(&self, first_request: Instant) -> Instant {
        let deadline = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let stored = state.retire(first_request);
            let deadline = stored.min(first_request + STOP_GRACE);
            state.retirement_deadline = Some(deadline);
            deadline
        };
        self.shared.changed.notify_all();
        deadline
    }
    /// These facts are not a no-spawn, joined-driver or retired-tree receipt.
    #[cfg(test)]
    pub(crate) fn state(&self) -> StartupRetirementState {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        StartupRetirementState {
            driver: state.driver,
            retirement_deadline: state.retirement_deadline,
        }
    }
}
#[derive(Default)]
struct RegistrationState {
    first_request: Option<Instant>,
    control: Option<StartupRetirementControl>,
    launch_pending: bool,
}
/// Per-admission latch, not a controller or a native retirement receipt.
#[derive(Clone, Default)]
pub(crate) struct StartupRegistration {
    state: Arc<Mutex<RegistrationState>>,
}
impl StartupRegistration {
    /// Pure latch: no Shared lock, native call, wake or resource disposal.
    /// Owner close may call this under the pool gate.
    pub(crate) fn latch_stop_at(&self, first_request: Instant) -> Instant {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let first = state
            .first_request
            .map_or(first_request, |stored| stored.min(first_request));
        state.first_request = Some(first);
        first + STOP_GRACE
    }
    /// Read only the original latch; never wait for Shared under an owner gate.
    pub(crate) fn latched_deadline(&self) -> Option<Instant> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .first_request
            .map(|first| first + STOP_GRACE)
    }
    /// Signal the exact original control outside owner/pool/compiler locks.
    pub(crate) fn propagate_stop(&self) -> Option<Instant> {
        let (first_request, control) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.first_request, state.control.clone())
        };
        first_request.map(|first| {
            control.map_or(first + STOP_GRACE, |control| control.request_stop_at(first))
        })
    }
    pub(crate) fn request_stop_at(&self, first_request: Instant) -> Instant {
        let deadline = self.latch_stop_at(first_request);
        self.propagate_stop().unwrap_or(deadline)
    }
    /// This transition and owner close use the same pure latch mutex.
    /// Winning it records startup in flight before the original driver is bound.
    fn begin_launch(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.first_request.is_some() || state.launch_pending {
            false
        } else {
            state.launch_pending = true;
            true
        }
    }
    #[cfg(test)]
    pub(crate) fn control(&self) -> Option<StartupRetirementControl> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .control
            .clone()
    }
    fn bind(&self, control: StartupRetirementControl) -> io::Result<()> {
        let (first_request, binding_error) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let binding_error = state.control.as_ref().map(|bound| {
                if Arc::ptr_eq(&bound.shared, &control.shared)
                    && Arc::ptr_eq(&bound.job, &control.job)
                {
                    "startup registration is already bound"
                } else {
                    "startup registration already belongs to another census"
                }
            });
            if binding_error.is_none() {
                state.control = Some(control.clone());
            }
            (state.first_request, binding_error)
        };
        // Preserve the saved time in this original pending census even on refusal.
        // This requests cancellation only; it never transfers a foreign control.
        if let Some(first_request) = first_request {
            control.request_stop_at(first_request);
        }
        match binding_error {
            None => Ok(()),
            Some(message) => Err(io::Error::other(message)),
        }
    }
}
pub(super) struct AdmissionFailure {
    pub(super) error: io::Error,
    pub(super) census: Option<Census>,
}
impl From<io::Error> for AdmissionFailure {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            census: None,
        }
    }
}
#[cfg(test)]
pub(super) struct Evidence {
    pub active: u32,
    pub total: u32,
    pub members: Vec<(u32, u64, bool, bool)>,
    pub console_members: Vec<(u32, u64, bool)>,
}
fn accounting(job: &OwnedHandle) -> io::Result<Accounting> {
    let mut result: Accounting = unsafe { zeroed() };
    unsafe {
        check(QueryInformationJobObject(
            job.as_raw_handle(),
            1,
            (&mut result as *mut Accounting).cast(),
            size_of::<Accounting>() as u32,
            null_mut(),
        ))?;
    }
    Ok(result)
}
fn creation(handle: &OwnedHandle) -> io::Result<u64> {
    let (mut c, mut e, mut k, mut u) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    unsafe {
        check(GetProcessTimes(
            handle.as_raw_handle(),
            &mut c,
            &mut e,
            &mut k,
            &mut u,
        ))?;
    }
    let ticks = ((c.high as u64) << 32) | c.low as u64;
    if ticks == 0 {
        Err(io::Error::other("zero process creation identity"))
    } else {
        Ok(ticks)
    }
}
fn signal(handle: &OwnedHandle) -> io::Result<bool> {
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        0 => Ok(true),
        258 => Ok(false),
        u32::MAX => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other("unexpected process signal status")),
    }
}
fn wow64(handle: &OwnedHandle) -> io::Result<bool> {
    let mut value = 0;
    unsafe {
        check(IsWow64Process(handle.as_raw_handle(), &mut value))?;
    }
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::other("process architecture evidence is invalid")),
    }
}
fn birth(event: &DebugEvent, job: &OwnedHandle) -> io::Result<Birth> {
    let mut raw = null_mut();
    unsafe {
        check(DuplicateHandle(
            GetCurrentProcess(),
            event.data.create.process,
            GetCurrentProcess(),
            &mut raw,
            QUERY_SYNC,
            0,
            0,
        ))?;
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let created = creation(&handle)?;
    let mut belongs = 0;
    unsafe {
        check(IsProcessInJob(
            handle.as_raw_handle(),
            job.as_raw_handle(),
            &mut belongs,
        ))?;
    }
    if belongs == 0 || unsafe { GetProcessId(handle.as_raw_handle()) } != event.pid {
        return Err(io::Error::other("process birth did not bind the owned Job"));
    }
    let wow64 = wow64(&handle)?;
    Ok(Birth {
        pid: event.pid,
        created,
        handle,
        exit_continued: false,
        debug_birth: true,
        system_console: false,
        wow64,
    })
}
fn normalized_native_path(mut path: Vec<u16>) -> Vec<u16> {
    for unit in &mut path {
        if *unit == b'/' as u16 {
            *unit = b'\\' as u16;
        }
    }
    let prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
    if path.starts_with(&prefix) {
        path.drain(..prefix.len());
    }
    path
}
fn system_console_image(handle: &OwnedHandle) -> io::Result<bool> {
    const PATH_UNITS: usize = 4096;
    let mut image = [0u16; PATH_UNITS];
    let mut image_len = PATH_UNITS as u32;
    unsafe {
        check(QueryFullProcessImageNameW(
            handle.as_raw_handle(),
            0,
            image.as_mut_ptr(),
            &mut image_len,
        ))?;
    }
    if image_len == 0 || image_len as usize >= PATH_UNITS {
        return Err(io::Error::other("process image identity exceeds bound"));
    }
    let image = normalized_native_path(image[..image_len as usize].to_vec());
    let mut windows = [0u16; PATH_UNITS];
    let length = unsafe { GetSystemWindowsDirectoryW(windows.as_mut_ptr(), PATH_UNITS as u32) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= PATH_UNITS - 32 {
        return Err(io::Error::other("system image identity exceeds bound"));
    }
    let windows = normalized_native_path(windows[..length as usize].to_vec());
    system_console_path_matches(&image, &windows)
}
fn system_console_path_matches(image: &[u16], windows: &[u16]) -> io::Result<bool> {
    for relative in ["System32\\conhost.exe", "SysWOW64\\conhost.exe"] {
        let mut expected = windows.to_vec();
        if expected.last() != Some(&(b'\\' as u16)) {
            expected.push(b'\\' as u16);
        }
        expected.extend(relative.encode_utf16());
        let equal = unsafe {
            CompareStringOrdinal(
                image.as_ptr(),
                image.len() as i32,
                expected.as_ptr(),
                expected.len() as i32,
                1,
            )
        };
        if equal == 0 {
            return Err(io::Error::last_os_error());
        }
        if equal == 2 {
            return Ok(true);
        }
    }
    Ok(false)
}
fn discover_job_members(job: &OwnedHandle, shared: &Shared) -> io::Result<()> {
    #[repr(C)]
    struct Pids {
        assigned: u32,
        count: u32,
        ids: [usize; MAX_BIRTHS],
    }
    let info = accounting(job)?;
    {
        let state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if counts_represented(info.total_processes, state.births.len())? {
            // Each lifetime count already has an exact retained object. A
            // current PID-list row may be absent/stale during a debug birth or
            // teardown; reopening it supplies no stronger evidence.
            return Ok(());
        }
    }
    let mut list: Box<Pids> = Box::new(unsafe { zeroed() });
    unsafe {
        check(QueryInformationJobObject(
            job.as_raw_handle(),
            3,
            (&mut *list as *mut Pids).cast(),
            size_of::<Pids>() as u32,
            null_mut(),
        ))?;
    }
    let returned = membership_list_count(list.assigned, list.count)?;
    let mut pids = BTreeSet::new();
    for value in &list.ids[..returned] {
        let pid =
            u32::try_from(*value).map_err(|_| io::Error::other("invalid Job member identifier"))?;
        if pid == 0 || !pids.insert(pid) {
            return Err(io::Error::other("duplicate Job member identifier"));
        }
        {
            let state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state
                .retirement_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "worker member capture deadline expired",
                ));
            }
        }
        let raw = unsafe { OpenProcess(QUERY_SYNC, 0, pid) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let created = creation(&handle)?;
        let mut belongs = 0;
        unsafe {
            check(IsProcessInJob(
                handle.as_raw_handle(),
                job.as_raw_handle(),
                &mut belongs,
            ))?;
        }
        if belongs == 0 || unsafe { GetProcessId(handle.as_raw_handle()) } != pid {
            return Err(io::Error::other("member handle did not bind the owned Job"));
        }
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(old) = state
            .births
            .iter()
            .find(|old| (old.pid, old.created) == (pid, created))
        {
            if !same_object(old.handle.as_raw_handle(), handle.as_raw_handle())? {
                return Err(io::Error::other(
                    "member identity did not bind the retained object",
                ));
            }
            continue;
        }
        if state.births.len() >= MAX_BIRTHS {
            return Err(io::Error::other(
                "worker retained object census exceeds bound",
            ));
        }
        // Query-only capture establishes object ownership, not permission to
        // signal a PID. A queued debug CREATE may still bind this object later.
        let console = system_console_image(&handle)?;
        let wow64 = wow64(&handle)?;
        state.births.push(Birth {
            pid,
            created,
            handle,
            exit_continued: false,
            debug_birth: false,
            system_console: console,
            wow64,
        });
        if state
            .retirement_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "worker member capture deadline expired",
            ));
        }
        shared.changed.notify_all();
    }
    let current = accounting(job)?;
    let state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // A gap here can be a birth whose debug CREATE event is still queued, so it is not a
    // failure. `complete_members` confirms retirement only once every count is retained.
    counts_represented(current.total_processes, state.births.len())?;
    Ok(())
}
fn counts_represented(total: u32, retained: usize) -> io::Result<bool> {
    if total as usize > MAX_BIRTHS || retained > MAX_BIRTHS {
        return Err(io::Error::other("worker lifetime census exceeds bound"));
    }
    Ok(total as usize == retained)
}
fn membership_list_count(assigned: u32, returned: u32) -> io::Result<usize> {
    if assigned as usize > MAX_BIRTHS || returned > assigned {
        return Err(io::Error::other(
            "worker membership census is invalid or exceeds bound",
        ));
    }
    Ok(returned as usize)
}
fn complete(state: &State, info: &Accounting) -> io::Result<bool> {
    if let Some(failure) = &state.failure {
        return Err(failure.error());
    }
    complete_members(state, info)
}
fn complete_members(state: &State, info: &Accounting) -> io::Result<bool> {
    if info.active_processes != 0 || info.total_processes as usize != state.births.len() {
        return Ok(false);
    }
    let Some(root) = state.root else {
        return Ok(false);
    };
    if !state
        .births
        .iter()
        .any(|birth| (birth.pid, birth.created) == root)
    {
        return Ok(false);
    }
    for member in &state.births {
        // Zero active accounting can precede queued CREATE/EXIT continuations.
        // A still-unsignaled discovered application may yet receive its birth
        // event, so do not classify the missing event as a broken chain early.
        if !signal(&member.handle)? {
            return Ok(false);
        }
        if !member.debug_birth && !member.system_console {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "owned application debug birth was not observed",
            ));
        }
        if member.debug_birth && !member.exit_continued {
            return Ok(false);
        }
    }
    Ok(true)
}
fn close_image(event: &DebugEvent) {
    let handle = unsafe {
        match event.code {
            3 => event.data.create.file,
            6 => event.data.load.file,
            _ => null_mut(),
        }
    };
    if !handle.is_null() {
        unsafe {
            CloseHandle(handle);
        }
    }
}
fn exception_status(
    pid: u32,
    code: u32,
    first_chance: u32,
    wow64: bool,
    seen: &mut BTreeSet<(u32, u32)>,
) -> u32 {
    // A WOW64 birth reports both the native and x86 loader breakpoint. Consume
    // each once, only on first chance. Application/second-chance exceptions
    // retain their native handler and exit behavior.
    if first_chance != 0
        && (code == 0x80000003 || (code == 0x4000001f && wow64))
        && seen.insert((pid, code))
    {
        0x00010002 // DBG_CONTINUE
    } else {
        0x80010001
    } // DBG_EXCEPTION_NOT_HANDLED
}

impl Census {
    #[cfg(test)]
    pub(super) fn fixture_failure(&self) -> Option<(io::ErrorKind, Option<i32>, &'static str)> {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .failure
            .as_ref()
            .map(|failure| (failure.kind, failure.raw, failure.phase))
    }
    #[cfg(test)]
    pub(super) fn evidence(&self, job: &OwnedHandle) -> io::Result<Evidence> {
        let info = accounting(job)?;
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let members = state
            .births
            .iter()
            .map(|member| {
                Ok((
                    member.pid,
                    member.created,
                    signal(&member.handle)?,
                    member.exit_continued,
                ))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let console_members = state
            .births
            .iter()
            .filter(|member| member.system_console && !member.debug_birth)
            .map(|member| Ok((member.pid, member.created, signal(&member.handle)?)))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Evidence {
            active: info.active_processes,
            total: info.total_processes,
            members,
            console_members,
        })
    }
    pub(super) fn start_retained(
        job: Arc<OwnedHandle>,
        deadline: Instant,
        create: impl FnOnce() -> io::Result<Spawned> + Send + 'static,
        before_run: impl FnOnce() -> io::Result<()> + Send + 'static,
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
    ) -> Result<(Self, Spawned), AdmissionFailure> {
        Self::start_inner(job, deadline, create, before_run, lifetime, None)
    }
    /// Bind the per-admission latch before any driver launch.
    pub(super) fn start_registered(
        job: Arc<OwnedHandle>,
        deadline: Instant,
        create: impl FnOnce() -> io::Result<Spawned> + Send + 'static,
        before_run: impl FnOnce() -> io::Result<()> + Send + 'static,
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        registration: StartupRegistration,
    ) -> Result<(Self, Spawned), AdmissionFailure> {
        Self::start_inner(
            job,
            deadline,
            create,
            before_run,
            lifetime,
            Some(registration),
        )
    }
    fn start_inner(
        job: Arc<OwnedHandle>,
        deadline: Instant,
        create: impl FnOnce() -> io::Result<Spawned> + Send + 'static,
        before_run: impl FnOnce() -> io::Result<()> + Send + 'static,
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        registration: Option<StartupRegistration>,
    ) -> Result<(Self, Spawned), AdmissionFailure> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                claim: lifetime.map(|fence| fence.claim()).transpose()?,
                ..State::default()
            }),
            changed: Condvar::new(),
        });
        let registered = registration.is_some();
        if let Some(registration) = registration.as_ref() {
            let control = StartupRetirementControl {
                shared: shared.clone(),
                job: job.clone(),
            };
            if let Err(error) = registration.bind(control) {
                shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .driver = StartupDriverState::NeverLaunched;
                let error = shared.no_spawn(&job).err().unwrap_or(error);
                return Err(AdmissionFailure {
                    error,
                    census: Some(Self {
                        shared,
                        thread: None,
                    }),
                });
            }
        }
        // A close latched before this indivisible transition forbids launch even
        // if its post-pool propagation is paused. Earlier launch remains in flight.
        let launch_permitted = registration
            .as_ref()
            .is_none_or(|registration| registration.begin_launch());
        if !launch_permitted {
            if let Some(registration) = &registration {
                let _ = registration.propagate_stop();
            }
        }
        let launch_refused = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if !launch_permitted || (registered && state.retiring) {
                state.driver = StartupDriverState::NeverLaunched;
                true
            } else {
                state.driver = StartupDriverState::LaunchPending;
                false
            }
        };
        if launch_refused {
            let error = shared.no_spawn(&job).err().unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Interrupted,
                    "worker startup closed before driver launch",
                )
            });
            return Err(AdmissionFailure {
                error,
                census: Some(Self {
                    shared,
                    thread: None,
                }),
            });
        }
        let worker_shared = shared.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let driver_job = job.clone();
        let thread = thread::Builder::new()
            .name("aurum-worker-lifetime".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pump(
                        &driver_job,
                        &worker_shared,
                        deadline,
                        create,
                        before_run,
                        &sender,
                    )
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        worker_shared.failure(error, "debug pump");
                        unsafe {
                            TerminateJobObject(driver_job.as_raw_handle(), 1);
                        }
                    }
                    Err(_) => {
                        worker_shared
                            .failure(io::Error::other("worker monitor panic"), "debug pump panic");
                        unsafe {
                            TerminateJobObject(driver_job.as_raw_handle(), 1);
                        }
                    }
                }
                worker_shared.finish();
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => {
                shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .driver = StartupDriverState::NeverLaunched;
                let error = shared.no_spawn(&job).err().unwrap_or(error);
                return Err(AdmissionFailure {
                    error,
                    census: Some(Self {
                        shared,
                        thread: None,
                    }),
                });
            }
        };
        let census = Self {
            shared,
            thread: Some(thread),
        };
        // The original JoinHandle is bound before status can report Bound.
        census
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .driver = StartupDriverState::Bound;
        census.shared.changed.notify_all();
        match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(spawned)) => {
                let mut state = census
                    .shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if Instant::now() >= deadline || state.retiring || state.failure.is_some() {
                    state.retire(Instant::now());
                    census.shared.changed.notify_all();
                    drop(state);
                    return Err(AdmissionFailure {
                        error: io::Error::new(
                            io::ErrorKind::TimedOut,
                            "owned worker initial birth deadline expired",
                        ),
                        census: Some(census),
                    });
                }
                state.acknowledged = true;
                census.shared.changed.notify_all();
                drop(state);
                Ok((census, spawned))
            }
            Ok(Err(failure)) => Err(AdmissionFailure {
                error: failure.error(),
                census: Some(census),
            }),
            Err(_) => {
                census.request_stop();
                Err(AdmissionFailure {
                    error: io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned worker initial birth was not acknowledged",
                    ),
                    census: Some(census),
                })
            }
        }
    }
    pub(super) fn request_stop(&self) -> Instant {
        let deadline = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retire(Instant::now());
        self.shared.changed.notify_all();
        deadline
    }
    pub(super) fn failure(&self) -> io::Result<()> {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match &state.failure {
            Some(failure) => Err(failure.error()),
            None => Ok(()),
        }
    }
    /// Pending admission may retire after a creation/continuation error. It
    /// still needs either affirmative no-spawn accounting, or every counted
    /// original birth object signaled with its debug continuation complete.
    pub(super) fn retained_admission_retired(&self, job: &OwnedHandle) -> io::Result<bool> {
        let info = accounting(job)?;
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let confirmed = if state.no_spawn_proved {
            info.total_processes == 0
                && info.active_processes == 0
                && state.root.is_none()
                && state.births.is_empty()
        } else {
            complete_members(&state, &info)?
        };
        if confirmed {
            if let Some(mut claim) = state.claim.take() {
                claim.resolve();
            }
        }
        Ok(confirmed)
    }
    pub(super) fn retained_driver_joined(&self) -> bool {
        self.thread.is_none()
            && matches!(
                self.shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .driver,
                StartupDriverState::NeverLaunched | StartupDriverState::Joined
            )
    }
    pub(super) fn confirmed(&self, job: &OwnedHandle) -> io::Result<bool> {
        let info = accounting(job)?;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let confirmed = complete(&state, &info)?;
        if confirmed {
            if let Some(mut claim) = state.claim.take() {
                claim.resolve();
            }
        }
        Ok(confirmed)
    }
    pub(super) fn wait_for_change(&self, deadline: Instant) {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _guard = self.shared.changed.wait_timeout(
            state,
            Duration::from_millis(EVENT_WAIT as u64)
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    /// The streaming owner retains this census on error. Do not release its
    /// original driver thread handle until the native object has signaled.
    pub(super) fn finish_driver_retained(&mut self) -> io::Result<()> {
        if self.thread.is_none() {
            return if self.retained_driver_joined() {
                self.failure()
            } else {
                Err(io::Error::other(
                    "original worker driver binding or join is unconfirmed",
                ))
            };
        }
        let deadline = self.request_stop();
        loop {
            let done = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .done;
            let signal =
                unsafe { WaitForSingleObject(self.thread.as_ref().unwrap().as_raw_handle(), 0) };
            if signal != 0 && signal != 258 {
                return Err(io::Error::other("retained worker driver wait failed"));
            }
            if done && signal == 0 {
                let joined = self.thread.take().unwrap().join();
                self.shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .driver = StartupDriverState::Joined;
                self.shared.changed.notify_all();
                joined.map_err(|_| io::Error::other("worker monitor panic"))?;
                return self.failure();
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "retained worker driver did not finish",
                ));
            }
            self.wait_for_change(deadline);
        }
    }
    pub(super) fn finish_driver(&mut self) -> io::Result<()> {
        if self.thread.is_none() {
            return self.failure();
        }
        let deadline = self.request_stop();
        loop {
            let done = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .done;
            if done {
                if let Some(thread) = self.thread.take() {
                    let joined = thread.join();
                    self.shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .driver = StartupDriverState::Joined;
                    self.shared.changed.notify_all();
                    joined.map_err(|_| io::Error::other("worker monitor panic"))?;
                }
                return self.failure();
            }
            if Instant::now() >= deadline {
                // Detach only after the single shared grace expires. The driver
                // retains the Job and never continues an unacknowledged birth.
                self.thread.take();
                self.shared
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .driver = StartupDriverState::Detached;
                self.shared.failure(
                    io::Error::new(io::ErrorKind::TimedOut, "worker monitor did not finish"),
                    "monitor retirement deadline",
                );
                return self.failure();
            }
            self.wait_for_change(deadline);
        }
    }
}
impl Drop for Census {
    fn drop(&mut self) {
        if self.finish_driver().is_err() {
            if let Some(context) = crate::operation_context::OperationContext::current() {
                context.mark_termination_unconfirmed();
            }
        }
    }
}

fn pump(
    job: &OwnedHandle,
    shared: &Shared,
    startup_deadline: Instant,
    create: impl FnOnce() -> io::Result<Spawned>,
    before_run: impl FnOnce() -> io::Result<()>,
    sender: &mpsc::SyncSender<Result<Spawned, Failure>>,
) -> io::Result<()> {
    if shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .retiring
        || Instant::now() >= startup_deadline
    {
        shared.no_spawn(job)?;
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "worker creation deadline expired",
        ));
    }
    let mut spawned = match create() {
        Ok(spawned) => Some(spawned),
        Err(error) => {
            // The creation closure's final fallible native call is CreateProcess;
            // it cannot return an error after successfully creating a process.
            shared.no_spawn(job)?;
            let failure = Failure::from(error, "process creation");
            let _ = sender.send(Err(failure));
            return Ok(());
        }
    };
    unsafe {
        check(DebugSetProcessKillOnExit(1))?;
    }
    let root = spawned
        .as_ref()
        .ok_or_else(|| io::Error::other("missing root"))?;
    let root_identity = (root.pid, creation(&root.process)?);
    shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .root = Some(root_identity);
    let mut before_run = Some(before_run);
    let mut first_break = BTreeSet::new();
    let mut termination_requested = false;
    loop {
        let retiring = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retiring;
        if retiring && !termination_requested {
            unsafe {
                check(TerminateJobObject(job.as_raw_handle(), 1))?;
            }
            termination_requested = true;
        }
        let mut event: DebugEvent = unsafe { zeroed() };
        if unsafe { WaitForDebugEvent(&mut event, EVENT_WAIT) } != 0 {
            let mut continuation = 0x00010002;
            let mut exit = None;
            if event.code == 3 {
                let member = birth(&event, job);
                match member {
                    Ok(member) => {
                        let mut state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        let previous = state
                            .births
                            .iter()
                            .position(|old| (old.pid, old.created) == (member.pid, member.created));
                        let duplicate = match previous {
                            Some(index) => {
                                state.births[index].debug_birth
                                    || !same_object(
                                        state.births[index].handle.as_raw_handle(),
                                        member.handle.as_raw_handle(),
                                    )?
                            }
                            None => false,
                        };
                        if (event.pid == root_identity.0 && member.created != root_identity.1)
                            || duplicate
                            || (previous.is_none() && state.births.len() >= MAX_BIRTHS)
                        {
                            drop(state);
                            shared.failure(
                                io::Error::other("worker birth census bound or duplicate"),
                                "birth census",
                            );
                        } else {
                            if let Some(index) = previous {
                                state.births[index].debug_birth = true;
                            } else {
                                state.births.push(member);
                            }
                            shared.changed.notify_all();
                        }
                    }
                    Err(error) => shared.failure(error, "birth identity or Job membership"),
                }
                if event.pid == root_identity.0 && spawned.is_some() {
                    let mut state = shared
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if state.failure.is_none() && !state.retiring {
                        drop(state);
                        if sender
                            .send(Ok(spawned
                                .take()
                                .ok_or_else(|| io::Error::other("missing root transfer"))?))
                            .is_err()
                        {
                            shared.failure(
                                io::Error::other("worker owner closed startup handoff"),
                                "startup handoff",
                            );
                        }
                        state = shared
                            .state
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        while !state.acknowledged
                            && !state.retiring
                            && Instant::now() < startup_deadline
                        {
                            let (next, _) = shared
                                .changed
                                .wait_timeout(
                                    state,
                                    startup_deadline.saturating_duration_since(Instant::now()),
                                )
                                .unwrap_or_else(|error| error.into_inner());
                            state = next;
                        }
                        if !state.acknowledged {
                            state.retire(Instant::now());
                            shared.changed.notify_all();
                        }
                    }
                    drop(state);
                    // No user code is continued until both the owner handoff
                    // and its current admission check have succeeded. This
                    // context is discarded before the standing pump begins.
                    if let Some(check_before_run) = before_run.take() {
                        if check_before_run().is_err() {
                            shared
                                .state
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .retire(Instant::now());
                            shared.changed.notify_all();
                        }
                    }
                }
                if shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .retiring
                {
                    unsafe {
                        check(TerminateJobObject(job.as_raw_handle(), 1))?;
                    }
                    termination_requested = true;
                }
            } else if event.code == 5 {
                let state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                exit = state
                    .births
                    .iter()
                    .position(|member| member.pid == event.pid && !member.exit_continued);
                if exit.is_none() {
                    drop(state);
                    shared.failure(
                        io::Error::other("worker exit has no birth identity"),
                        "exit census",
                    );
                }
            } else if event.code == 1 {
                let exception = unsafe { event.data.exception };
                let state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let member = state
                    .births
                    .iter()
                    .find(|member| {
                        member.pid == event.pid && member.debug_birth && !member.exit_continued
                    })
                    .ok_or_else(|| io::Error::other("exception has no retained debug birth"))?;
                let wow64 = member.wow64;
                drop(state);
                continuation = exception_status(
                    event.pid,
                    exception.record.code,
                    exception.first_chance,
                    wow64,
                    &mut first_break,
                );
            } else if event.code == 9 {
                shared.failure(
                    io::Error::other("worker debugger reported RIP"),
                    "debug event",
                );
            }
            close_image(&event);
            if let Err(error) = discover_job_members(job, shared) {
                shared.failure(error, "owned member capture");
            }
            unsafe {
                check(ContinueDebugEvent(event.pid, event.tid, continuation))?;
            }
            if let Some(index) = exit {
                shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .births[index]
                    .exit_continued = true;
                first_break.retain(|(pid, _)| *pid != event.pid);
            }
            shared.changed.notify_all();
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(121) {
                return Err(error);
            }
        }
        let info = accounting(job)?;
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.failure.is_none() && complete(&state, &info)? {
            if let Some(mut claim) = state.claim.take() {
                claim.resolve();
            }
            return Ok(());
        }
        if state
            .retirement_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned worker complete birth census did not retire",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_debug_event_abi_matches_host_architecture() {
        assert_eq!(
            size_of::<DebugEvent>(),
            if cfg!(target_pointer_width = "64") {
                176
            } else {
                96
            }
        );
        assert_eq!(size_of::<Accounting>(), 48);
    }
    #[test]
    fn loader_breakpoints_are_first_chance_and_independent_for_native_and_wow64() {
        let mut seen = BTreeSet::new();
        assert_eq!(
            exception_status(6, 0x4000001f, 1, false, &mut seen),
            0x80010001
        );
        assert!(seen.is_empty());
        assert_eq!(
            exception_status(7, 0x80000003, 0, true, &mut seen),
            0x80010001
        );
        assert!(seen.is_empty());
        assert_eq!(
            exception_status(7, 0x80000003, 1, true, &mut seen),
            0x00010002
        );
        assert_eq!(
            exception_status(7, 0x4000001f, 1, true, &mut seen),
            0x00010002
        );
        assert_eq!(
            exception_status(7, 0x80000003, 1, true, &mut seen),
            0x80010001
        );
        assert_eq!(
            exception_status(7, 0x4000001f, 0, true, &mut seen),
            0x80010001
        );
        assert_eq!(
            exception_status(7, 0xe0424343, 1, true, &mut seen),
            0x80010001
        );
        assert_eq!(
            exception_status(8, 0x80000003, 1, true, &mut seen),
            0x00010002
        );
        seen.retain(|(pid, _)| *pid != 7);
        assert_eq!(
            exception_status(7, 0x80000003, 1, true, &mut seen),
            0x00010002
        );
    }
    #[test]
    fn console_helper_requires_the_exact_system_image_path() {
        let windows: Vec<_> = "C:\\Windows".encode_utf16().collect();
        for path in [
            "c:\\windows\\System32\\ConHost.exe",
            "C:\\Windows\\SysWOW64\\conhost.exe",
        ] {
            assert!(system_console_path_matches(
                &path.encode_utf16().collect::<Vec<_>>(),
                &windows
            )
            .unwrap());
        }
        for path in [
            "C:\\untrusted\\conhost.exe",
            "C:\\Windows\\System32\\conhost.exe.extra",
            "C:\\Windows\\System32\\child\\conhost.exe",
            "C:\\Windows\\System32\\..\\conhost.exe",
        ] {
            assert!(!system_console_path_matches(
                &path.encode_utf16().collect::<Vec<_>>(),
                &windows
            )
            .unwrap());
        }
    }
    #[test]
    fn partial_membership_requires_every_lifetime_object_to_be_retained() {
        assert_eq!(membership_list_count(4, 3).unwrap(), 3);
        assert!(counts_represented(4, 4).unwrap());
        assert!(!counts_represented(4, 3).unwrap());
        assert!(!counts_represented(5, 4).unwrap());
        assert!(!counts_represented(4, 5).unwrap());
        assert!(membership_list_count(3, 4).is_err());
        assert!(membership_list_count(MAX_BIRTHS as u32 + 1, 1).is_err());
        assert!(counts_represented(MAX_BIRTHS as u32 + 1, 0).is_err());
        assert!(counts_represented(0, MAX_BIRTHS + 1).is_err());
    }
    #[test]
    fn an_empty_or_unknown_census_never_confirms() {
        let info: Accounting = unsafe { zeroed() };
        assert!(!complete(&State::default(), &info).unwrap());
        let state = State {
            failure: Some(Failure::from(io::Error::from_raw_os_error(5), "query")),
            ..State::default()
        };
        assert!(complete(&state, &info).is_err());
    }
    #[test]
    fn repeated_retirement_never_extends_the_shared_deadline() {
        let mut state = State::default();
        let start = Instant::now();
        assert_eq!(state.retire(start), start + STOP_GRACE);
        assert_eq!(state.retire(start + STOP_GRACE), start + STOP_GRACE);
        assert_eq!(state.retire(start + STOP_GRACE * 2), start + STOP_GRACE);
    }
    #[test]
    fn unacknowledged_creator_retains_workspace_without_context_until_no_spawn_proof() {
        assert!(crate::operation_context::OperationContext::current().is_none());
        let mut workspace = crate::candidate_workspace::Workspace::create().unwrap();
        let fence = workspace.lifetime.clone();
        let raw = unsafe { super::super::CreateJobObjectW(null_mut(), std::ptr::null()) };
        assert!(!raw.is_null());
        let job = Arc::new(unsafe { OwnedHandle::from_raw_handle(raw) });
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let tracked = fence.clone();
        let start = Instant::now();
        let caller_job = job.clone();
        let caller = thread::spawn(move || {
            Census::start_retained(
                caller_job,
                Instant::now() + Duration::from_millis(200),
                move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Err(io::Error::from_raw_os_error(2))
                },
                || Ok(()),
                Some(&tracked),
            )
            .err()
            .unwrap()
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(fence.unresolved());
        assert!(workspace.clean().is_err());
        let mut result = caller.join().unwrap();
        assert_eq!(result.error.kind(), io::ErrorKind::TimedOut);
        let mut census = result
            .census
            .take()
            .expect("original pending census retained");
        assert!(
            start.elapsed() < Duration::from_secs(7),
            "cleanup grace was multiplied"
        );
        assert!(fence.unresolved());
        assert!(workspace.clean().is_err());
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let no_spawn = census.retained_admission_retired(&job).unwrap();
            let signal =
                unsafe { WaitForSingleObject(census.thread.as_ref().unwrap().as_raw_handle(), 0) };
            assert!(
                matches!(signal, 0 | 258),
                "original driver signal unavailable"
            );
            if no_spawn && signal == 0 {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        // The operation error remains, but the same driver must join before
        // its retained resources can be released.
        let _admission_error = census.finish_driver_retained();
        assert!(census.retained_driver_joined());
        assert!(!fence.unresolved());
        // The creator's affirmative failure plus total=0 accounting resolved
        // the claim. This says nothing about post-host-crash process retirement.
        workspace.clean().unwrap();
    }
}
