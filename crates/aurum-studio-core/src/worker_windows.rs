//! Windows-owned validation trees, assigned atomically at process creation.
//! Job ownership is lifecycle control, not a security sandbox.
use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "worker_ledger_windows.rs"]
mod ledger;
pub(crate) use ledger::StartupRegistration;

pub(super) struct RegisteredStartup<'a> {
    pub(super) lifetime: Option<&'a crate::candidate_workspace::LifetimeFence>,
    pub(super) requested_deadline: Option<Instant>,
    pub(super) registration: StartupRegistration,
}
struct RetainedStartup<'a> {
    lifetime: Option<&'a crate::candidate_workspace::LifetimeFence>,
    requested_deadline: Option<Instant>,
    registration: Option<StartupRegistration>,
}

#[path = "worker_binary_windows.rs"]
pub(crate) mod binary;
pub(crate) use binary::command::{CommandCapture, CommandSpawn};
pub(crate) use binary::{BinarySpawn, BinaryStream, StreamController};

type Handle = *mut c_void;

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: Handle,
    inherit: i32,
}
#[repr(C)]
struct StartupInfo {
    size: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    chars_x: u32,
    chars_y: u32,
    fill: u32,
    flags: u32,
    show: u16,
    reserved_bytes: u16,
    reserved_data: *mut u8,
    stdin: Handle,
    stdout: Handle,
    stderr: Handle,
}
#[repr(C)]
struct StartupInfoEx {
    startup: StartupInfo,
    attributes: Handle,
}
#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    pid: u32,
    tid: u32,
}
#[repr(C)]
struct BasicLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_processes: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}
#[repr(C)]
struct ExtendedLimits {
    basic: BasicLimits,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}
#[repr(C)]
struct Accounting {
    times: [i64; 4],
    page_faults: u32,
    total_processes: u32,
    active_processes: u32,
    terminated_processes: u32,
}
#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: Handle, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: i32, info: Handle, length: u32) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn QueryInformationJobObject(
        job: Handle,
        class: i32,
        info: Handle,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn CreatePipe(read: *mut Handle, write: *mut Handle, attributes: Handle, size: u32) -> i32;
    fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
    fn InitializeProcThreadAttributeList(
        list: Handle,
        count: u32,
        flags: u32,
        size: *mut usize,
    ) -> i32;
    fn UpdateProcThreadAttribute(
        list: Handle,
        flags: u32,
        key: usize,
        value: Handle,
        size: usize,
        previous: Handle,
        returned: *mut usize,
    ) -> i32;
    fn DeleteProcThreadAttributeList(list: Handle);
    fn CreateProcessW(
        application: *const u16,
        command: *mut u16,
        process_attributes: Handle,
        thread_attributes: Handle,
        inherit: i32,
        flags: u32,
        environment: Handle,
        directory: *const u16,
        startup: *const StartupInfoEx,
        information: *mut ProcessInformation,
    ) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn GetProcessTimes(
        process: Handle,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = text.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker text contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

// CRT argument quoting, not shell escaping. Always quote and double trailing
// backslashes and those preceding a quote. No batch or shell programs allowed.
fn quote(text: &OsStr) -> io::Result<Vec<u16>> {
    let text = wide(text)?;
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for &character in &text[..text.len() - 1] {
        if character == b'\\' as u16 {
            slashes += 1;
        } else {
            let count = if character == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            };
            result.extend(std::iter::repeat_n(b'\\' as u16, count));
            result.push(character);
            slashes = 0;
        }
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    Ok(result)
}

struct Attributes {
    storage: Vec<usize>,
}
impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // The sizing call is documented to fail with the required size.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 2, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        unsafe {
            check(InitializeProcThreadAttributeList(
                storage.as_mut_ptr().cast(),
                2,
                0,
                &mut bytes,
            ))?;
        }
        Ok(Self { storage })
    }
    fn pointer(&mut self) -> Handle {
        self.storage.as_mut_ptr().cast()
    }
    fn add(&mut self, key: usize, handles: &mut [Handle]) -> io::Result<()> {
        unsafe {
            check(UpdateProcThreadAttribute(
                self.pointer(),
                0,
                key,
                handles.as_mut_ptr().cast(),
                std::mem::size_of_val(handles),
                null_mut(),
                null_mut(),
            ))
        }
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.pointer());
        }
    }
}

fn pipe(parent_reads: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut attributes = SecurityAttributes {
        length: size_of::<SecurityAttributes>() as u32,
        descriptor: null_mut(),
        inherit: 1,
    };
    let (mut read, mut write) = (null_mut(), null_mut());
    unsafe {
        check(CreatePipe(
            &mut read,
            &mut write,
            (&mut attributes as *mut SecurityAttributes).cast(),
            0,
        ))?;
        let read = OwnedHandle::from_raw_handle(read);
        let write = OwnedHandle::from_raw_handle(write);
        let parent = if parent_reads {
            read.as_raw_handle()
        } else {
            write.as_raw_handle()
        };
        check(SetHandleInformation(parent, 1, 0))?;
        Ok((read, write))
    }
}

pub(super) struct Worker {
    process: OwnedHandle,
    job: Arc<OwnedHandle>,
    pid: u32,
    census: ledger::Census,
}
pub(super) struct PendingWorker {
    job: Arc<OwnedHandle>,
    census: ledger::Census,
}
pub(super) struct WorkerAdmission {
    pub(super) error: io::Error,
    pub(super) pending: Option<PendingWorker>,
}
impl From<io::Error> for WorkerAdmission {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            pending: None,
        }
    }
}
impl PendingWorker {
    pub(super) fn retire(&mut self) -> io::Result<()> {
        let deadline = self.census.request_stop();
        let _ = unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
        loop {
            if self.census.retained_admission_retired(&self.job)? {
                let driver = self.census.finish_driver_retained();
                // An admission error remains an operation error. Once native
                // no-spawn/all-birth retirement and driver join are proved, it
                // does not make the same objects live again.
                return if self.census.retained_driver_joined() {
                    Ok(())
                } else {
                    driver
                };
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "pending worker admission requires recovery",
                ));
            }
            self.census.wait_for_change(deadline);
        }
    }
}
impl Worker {
    /// Private native command capture preserves inherited environment semantics.
    /// Its finite output owner is distinct from accepted native gameplay.
    pub(crate) fn spawn_native_command_output(
        controller: &StreamController,
        request: CommandSpawn<'_>,
    ) -> io::Result<CommandCapture> {
        CommandCapture::spawn(controller, request)
    }
    /// Binary output has a distinct owned read/retirement path. Ordinary
    /// validation capture and accepted native gameplay keep their current API.
    #[cfg(test)]
    pub(crate) fn spawn_binary(
        controller: &StreamController,
        request: BinarySpawn<'_>,
    ) -> io::Result<BinaryStream> {
        BinaryStream::spawn(controller, request)
    }
    /// Curated environment for the trusted retained compiler caller.
    pub(crate) fn spawn_binary_cleared(
        controller: &StreamController,
        request: BinarySpawn<'_>,
    ) -> io::Result<BinaryStream> {
        BinaryStream::spawn_cleared(controller, request)
    }
    pub(crate) fn spawn_binary_cleared_retaining(
        controller: &StreamController,
        request: BinarySpawn<'_>,
        retained: Box<dyn Send>,
    ) -> io::Result<BinaryStream> {
        BinaryStream::spawn_cleared_retaining(controller, request, retained)
    }
    fn retire_binary(&mut self) -> io::Result<()> {
        let tree = self.terminate();
        let driver = self.census.finish_driver_retained();
        tree.and(driver)
    }
    pub(super) fn spawn(
        program: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
    ) -> io::Result<(Self, File, File)> {
        Self::spawn_inner(program, program, args, directory, overrides, None, None)
    }
    pub(super) fn spawn_with_argv0(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
    ) -> io::Result<(Self, File, File)> {
        if program == argv0 {
            Self::spawn(program, args, directory, overrides)
        } else {
            Self::spawn_inner(program, argv0, args, directory, overrides, None, None)
        }
    }
    pub(super) fn spawn_with_argv0_and_fence(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        requested_deadline: Option<Instant>,
    ) -> io::Result<(Self, File, File)> {
        if lifetime.is_none() && requested_deadline.is_none() {
            Self::spawn_with_argv0(program, argv0, args, directory, overrides)
        } else {
            Self::spawn_inner(
                program,
                argv0,
                args,
                directory,
                overrides,
                lifetime,
                requested_deadline,
            )
        }
    }
    fn spawn_inner(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        requested_deadline: Option<Instant>,
    ) -> io::Result<(Self, File, File)> {
        Self::spawn_inner_retained(
            program,
            argv0,
            args,
            directory,
            overrides,
            lifetime,
            requested_deadline,
        )
        .map_err(|failure| failure.error)
    }
    pub(super) fn spawn_inner_retained(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        requested_deadline: Option<Instant>,
    ) -> Result<(Self, File, File), WorkerAdmission> {
        Self::spawn_retained_environment(
            program,
            argv0,
            args,
            directory,
            ChildEnvironment::Inherit(overrides),
            lifetime,
            requested_deadline,
        )
    }
    pub(super) fn spawn_with_environment(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        environment_policy: ChildEnvironment<'_>,
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        requested_deadline: Option<Instant>,
    ) -> io::Result<(Self, File, File)> {
        Self::spawn_retained_environment(
            program,
            argv0,
            args,
            directory,
            environment_policy,
            lifetime,
            requested_deadline,
        )
        .map_err(|failure| failure.error)
    }
    fn spawn_retained_environment(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        environment_policy: ChildEnvironment<'_>,
        lifetime: Option<&crate::candidate_workspace::LifetimeFence>,
        requested_deadline: Option<Instant>,
    ) -> Result<(Self, File, File), WorkerAdmission> {
        Self::spawn_retained_environment_inner(
            program,
            argv0,
            args,
            directory,
            environment_policy,
            RetainedStartup {
                lifetime,
                requested_deadline,
                registration: None,
            },
        )
    }
    /// Owned-capture prerequisite only; no caller is adopted by this slice.
    fn spawn_retained_environment_registered(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        environment_policy: ChildEnvironment<'_>,
        startup: RegisteredStartup<'_>,
    ) -> Result<(Self, File, File), WorkerAdmission> {
        Self::spawn_retained_environment_inner(
            program,
            argv0,
            args,
            directory,
            environment_policy,
            RetainedStartup {
                lifetime: startup.lifetime,
                requested_deadline: startup.requested_deadline,
                registration: Some(startup.registration),
            },
        )
    }
    fn spawn_retained_environment_inner(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        environment_policy: ChildEnvironment<'_>,
        startup: RetainedStartup<'_>,
    ) -> Result<(Self, File, File), WorkerAdmission> {
        let RetainedStartup {
            lifetime,
            requested_deadline,
            registration,
        } = startup;
        #[cfg(test)]
        let overrides = environment_policy.overrides();
        let admitted = Instant::now();
        let context = crate::operation_context::OperationContext::current();
        let startup_deadline = requested_deadline
            .unwrap_or(admitted + Duration::from_secs(10))
            .min(admitted + Duration::from_secs(10));
        let startup_deadline = context.as_ref().map_or(startup_deadline, |context| {
            startup_deadline.min(context.deadline())
        });
        if Instant::now() >= startup_deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned worker admission deadline expired",
            )
            .into());
        }
        if [program, argv0].iter().any(|path| {
            !path.is_absolute()
                || !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "owned workers require an absolute .exe path",
            )
            .into());
        }
        let application = wide(program.as_os_str())?;
        let mut command = quote(argv0.as_os_str())?;
        for arg in args {
            command.push(b' ' as u16);
            command.extend(quote(OsStr::new(arg))?);
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "worker command is too long").into(),
            );
        }
        let directory = directory.map(|path| wide(path.as_os_str())).transpose()?;
        let inherited = if environment_policy.inherits() {
            std::env::vars_os().collect()
        } else {
            Vec::new()
        };
        let mut block = build_environment(inherited, environment_policy)?;
        if let Some(context) = &context {
            context.check()?;
        }
        unsafe {
            let raw_job = CreateJobObjectW(null_mut(), null());
            if raw_job.is_null() {
                return Err(io::Error::last_os_error().into());
            }
            let job = Arc::new(OwnedHandle::from_raw_handle(raw_job));
            let mut limits: ExtendedLimits = zeroed();
            limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            check(SetInformationJobObject(
                job.as_raw_handle(),
                9,
                (&mut limits as *mut ExtendedLimits).cast(),
                size_of::<ExtendedLimits>() as u32,
            ))?;
            let creation_job = job.clone();
            let creation_context = context.clone();
            let before_run_context = context.clone();
            #[cfg(test)]
            let creator_gate = overrides
                .iter()
                .find(|(key, _)| key == "AURUM_WORKER_CREATOR_GATE")
                .map(|(_, path)| std::path::PathBuf::from(path));
            let create = move || -> io::Result<ledger::Spawned> {
                #[cfg(test)]
                if let Some(gate) = creator_gate {
                    std::fs::write(gate.join("entered"), b"entered")?;
                    while !gate.join("release").is_file() {
                        std::thread::park_timeout(Duration::from_millis(10));
                    }
                }
                let (stdin, input) = pipe(false)?;
                let (stdout, output) = pipe(true)?;
                let (stderr, errors) = pipe(true)?;
                let mut inherited = [
                    stdin.as_raw_handle(),
                    output.as_raw_handle(),
                    errors.as_raw_handle(),
                ];
                let mut jobs = [creation_job.as_raw_handle()];
                let mut attributes = Attributes::new()?;
                attributes.add(0x20002, &mut inherited)?; // HANDLE_LIST
                attributes.add(0x2000d, &mut jobs)?; // JOB_LIST, Windows 10+
                let mut startup: StartupInfoEx = zeroed();
                startup.startup.size = size_of::<StartupInfoEx>() as u32;
                startup.startup.flags = 0x100; // STARTF_USESTDHANDLES
                startup.startup.stdin = inherited[0];
                startup.startup.stdout = inherited[1];
                startup.startup.stderr = inherited[2];
                startup.attributes = attributes.pointer();
                let mut information: ProcessInformation = zeroed();
                if let Some(context) = &creation_context {
                    context.check()?;
                }
                if Instant::now() >= startup_deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned worker deadline expired before native creation",
                    ));
                }
                check(CreateProcessW(
                    application.as_ptr(),
                    command.as_mut_ptr(),
                    null_mut(),
                    null_mut(),
                    1,
                    0x08000000 | 0x00080000 | 0x00000400 | 1, // NO_WINDOW, EXTENDED_STARTUPINFO_PRESENT, UNICODE_ENVIRONMENT, DEBUG_PROCESS
                    block.as_mut_ptr().cast(),
                    directory.as_ref().map_or(null(), |value| value.as_ptr()),
                    &startup,
                    &mut information,
                ))?;
                let process = OwnedHandle::from_raw_handle(information.process);
                let thread = OwnedHandle::from_raw_handle(information.thread);
                drop(thread);
                // Only the three explicit pipe ends are inherited. In particular,
                // the kill-on-close job handle stays exclusively with this host.
                drop(input);
                drop(stdin);
                drop(output);
                drop(errors);
                Ok(ledger::Spawned {
                    process,
                    pid: information.pid,
                    stdout: stdout.into(),
                    stderr: stderr.into(),
                })
            };
            let before_run = move || {
                // This capture exists only for initial admission. The standing
                // pump never enters or retains a completed operation scope.
                if let Some(context) = &before_run_context {
                    context.check()?;
                }
                if Instant::now() >= startup_deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned worker deadline expired before continuation",
                    ));
                }
                Ok(())
            };
            let started = if let Some(registration) = registration {
                ledger::Census::start_registered(
                    job.clone(),
                    startup_deadline,
                    create,
                    before_run,
                    lifetime,
                    registration,
                )
            } else {
                ledger::Census::start_retained(
                    job.clone(),
                    startup_deadline,
                    create,
                    before_run,
                    lifetime,
                )
            };
            let (census, spawned) = match started {
                Ok(started) => started,
                Err(failure) => {
                    if failure.error.kind() == io::ErrorKind::TimedOut {
                        if let Some(context) = &context {
                            context.mark_termination_unconfirmed();
                        }
                    }
                    return Err(WorkerAdmission {
                        error: failure.error,
                        pending: failure.census.map(|census| PendingWorker { job, census }),
                    });
                }
            };
            Ok((
                Self {
                    process: spawned.process,
                    job,
                    pid: spawned.pid,
                    census,
                },
                spawned.stdout,
                spawned.stderr,
            ))
        }
    }
    pub(super) fn pid(&self) -> u32 {
        self.pid
    }
    pub(super) fn creation_time(&self) -> io::Result<String> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        unsafe {
            check(GetProcessTimes(
                self.process.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            ))?;
        }
        let ticks = ((created.high as u64) << 32) | created.low as u64;
        Ok(format!("windows-filetime:{ticks}"))
    }
    pub(super) fn try_wait(&self) -> io::Result<Option<i32>> {
        self.census.failure()?;
        unsafe {
            match WaitForSingleObject(self.process.as_raw_handle(), 0) {
                0 => {
                    let mut code = 0;
                    check(GetExitCodeProcess(self.process.as_raw_handle(), &mut code))?;
                    Ok(Some(code as i32))
                }
                258 => Ok(None),
                u32::MAX => Err(io::Error::last_os_error()),
                _ => Err(io::Error::other("unexpected owned process wait status")),
            }
        }
    }
    pub(super) fn termination_confirmed(&self) -> io::Result<bool> {
        // The complete lifetime census includes early exits and late forks.
        // Every original birth object must signal, not only the root handle.
        Ok(self.census.confirmed(&self.job)? && self.try_wait()?.is_some())
    }
    pub(super) fn terminate(&self) -> io::Result<()> {
        let deadline = self.census.request_stop();
        if let Err(error) = unsafe { check(TerminateJobObject(self.job.as_raw_handle(), 1)) } {
            // A concurrent stop or natural exit can finish before this call.
            // An OS failure is only harmless if the same handles prove exit.
            return if self.termination_confirmed().unwrap_or(false) {
                Ok(())
            } else {
                Err(error)
            };
        }
        loop {
            if self.termination_confirmed()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "owned worker tree did not finish termination",
                ));
            }
            self.census.wait_for_change(deadline);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let retirement = self.terminate();
        let driver = self.census.finish_driver();
        if retirement.is_err() || driver.is_err() {
            if let Some(context) = crate::operation_context::OperationContext::current() {
                context.mark_termination_unconfirmed();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    const HELPER: &str = "worker_windows::tests::worker_tree_helper";
    #[test]
    fn worker_tree_helper() {
        let Some(directory) = std::env::var_os("AURUM_WORKER_FIXTURE") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        let role = std::env::var("AURUM_WORKER_ROLE").unwrap();
        if role == "handled-exception"
            || role == "unhandled-exception"
            || role == "handled-wx86-collision"
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            const EXCEPTION: u32 = 0xe042_4343;
            static HANDLED: AtomicBool = AtomicBool::new(false);
            #[repr(C)]
            struct ExceptionPointers {
                record: *const u32,
                context: Handle,
            }
            unsafe extern "system" fn handle(exception: *mut ExceptionPointers) -> i32 {
                if matches!(
                    unsafe { (*exception).record.read() },
                    EXCEPTION | 0x4000001f
                ) {
                    HANDLED.store(true, Ordering::Release);
                    -1 // EXCEPTION_CONTINUE_EXECUTION
                } else {
                    0
                }
            }
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn RaiseException(code: u32, flags: u32, count: u32, arguments: *const usize);
                fn AddVectoredExceptionHandler(
                    first: u32,
                    handler: unsafe extern "system" fn(*mut ExceptionPointers) -> i32,
                ) -> Handle;
                fn RemoveVectoredExceptionHandler(handler: Handle) -> u32;
                fn SetErrorMode(mode: u32) -> u32;
            }
            unsafe {
                SetErrorMode(3);
            }
            if role == "handled-exception" || role == "handled-wx86-collision" {
                let handler = unsafe { AddVectoredExceptionHandler(1, handle) };
                assert!(!handler.is_null());
                unsafe {
                    RaiseException(
                        if role == "handled-wx86-collision" {
                            0x4000001f
                        } else {
                            EXCEPTION
                        },
                        0,
                        0,
                        null(),
                    );
                }
                assert!(HANDLED.load(Ordering::Acquire));
                assert_ne!(unsafe { RemoveVectoredExceptionHandler(handler) }, 0);
                std::fs::write(directory.join("exception-handled"), "handled").unwrap();
                return;
            }
            std::fs::write(directory.join("exception-raised"), "raised").unwrap();
            unsafe {
                RaiseException(EXCEPTION, 0, 0, null());
            }
            panic!("unhandled exception was swallowed");
        }
        if role == "echo" {
            println!(
                "worker-fixture:{}",
                serde_json::json!({"args":std::env::args().collect::<Vec<_>>(), "cwd":std::env::current_dir().unwrap(), "value":std::env::var("AURUM_WORKER_VALUE").unwrap()})
            );
            return;
        }
        if role == "heap-child" {
            println!("heap-fixture:{}", std::env::var("_NO_DEBUG_HEAP").unwrap());
            return;
        }
        if role.starts_with("heap-host-") {
            let before = std::env::var_os("_NO_DEBUG_HEAP");
            let expected = match role.as_str() {
                "heap-host-default" => {
                    assert!(before.is_none());
                    "1"
                }
                "heap-host-ambient" => {
                    assert_eq!(before.as_deref(), Some(OsStr::new("0")));
                    "0"
                }
                "heap-host-explicit" => {
                    assert_eq!(before.as_deref(), Some(OsStr::new("1")));
                    "0"
                }
                _ => panic!("unknown heap fixture role"),
            };
            let mut command = crate::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", HELPER, "--nocapture"])
                .env("AURUM_WORKER_ROLE", "heap-child")
                .owned_tree();
            if role == "heap-host-explicit" {
                command = command.env("_nO_dEbUg_hEaP", "0");
            }
            let outcome = command.run(Duration::from_secs(8)).unwrap();
            assert!(outcome.success(), "{outcome:?}");
            assert!(outcome
                .stdout
                .lines()
                .any(|line| line == format!("heap-fixture:{expected}")));
            assert_eq!(std::env::var_os("_NO_DEBUG_HEAP"), before);
            std::fs::write(directory.join("heap-proof"), expected).unwrap();
            return;
        }
        if role == "host" {
            crate::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", HELPER, "--nocapture"])
                .env("AURUM_WORKER_ROLE", "root")
                .owned_tree()
                .run(Duration::from_secs(30))
                .unwrap();
            return;
        }
        if role == "short" {
            std::fs::write(directory.join("short.pid"), std::process::id().to_string()).unwrap();
            return;
        }
        if role == "root" {
            use std::os::windows::process::CommandExt;
            if std::env::var("AURUM_WORKER_SHORT_FIRST").as_deref() == Ok("1") {
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .creation_flags(0x08000000)
                    .args(["--exact", HELPER, "--nocapture"])
                    .env("AURUM_WORKER_ROLE", "short")
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            let broken_chain = std::env::var("AURUM_WORKER_CHAIN_BREAK").as_deref() == Ok("1");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .creation_flags(0x08000000 | if broken_chain { 3 } else { 0 })
                .args(["--exact", HELPER, "--nocapture"])
                .env("AURUM_WORKER_ROLE", "leaf")
                .stdin(std::process::Stdio::null())
                .spawn()
                .unwrap();
            std::fs::write(directory.join("root.pid"), std::process::id().to_string()).unwrap();
            std::fs::write(directory.join("leaf.pid"), child.id().to_string()).unwrap();
            // Reap on natural exit without preventing the root fixture from
            // finishing first. The OS job must still end the waiting tree.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            let started = Instant::now();
            while !broken_chain && !directory.join("leaf.ready").exists() {
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(10));
            }
            std::fs::write(directory.join("ready"), "ready").unwrap();
            if std::env::var("AURUM_WORKER_ROOT_EXIT").as_deref() == Ok("1") {
                return;
            }
            if std::env::var("AURUM_WORKER_BAD_STORAGE").as_deref() == Ok("1") {
                let mut nested = directory.clone();
                for _ in 0..34 {
                    nested.push("d");
                }
                std::fs::create_dir_all(nested).unwrap();
            }
        } else {
            assert_eq!(role, "leaf");
            std::fs::write(directory.join("leaf.ready"), "ready").unwrap();
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn complete_census_retains_short_exits_and_signals_each_birth() {
        let path = fixture();
        let fence = crate::candidate_workspace::LifetimeFence::default();
        let executable = std::env::current_exe().unwrap();
        let (worker, _stdout, _stderr) = Worker::spawn_with_argv0_and_fence(
            &executable,
            &executable,
            &["--exact".into(), HELPER.into(), "--nocapture".into()],
            None,
            &[
                ("AURUM_WORKER_FIXTURE".into(), path.to_string_lossy().into()),
                ("AURUM_WORKER_ROLE".into(), "root".into()),
                ("AURUM_WORKER_SHORT_FIRST".into(), "1".into()),
            ],
            Some(&fence),
            None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !path.join("ready").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let short = std::fs::read_to_string(path.join("short.pid"))
            .unwrap()
            .parse::<u32>()
            .unwrap();
        let before = worker.census.evidence(&worker.job).unwrap();
        assert!(before
            .members
            .iter()
            .any(|(pid, birth, signaled, exited)| *pid == short
                && *birth > 0
                && *signaled
                && *exited));
        assert!(fence.unresolved());
        worker.terminate().unwrap();
        assert!(!fence.unresolved());
        let after = worker.census.evidence(&worker.job).unwrap();
        assert_eq!(after.active, 0);
        assert_eq!(after.total as usize, after.members.len());
        assert!(after
            .members
            .iter()
            .all(|(_, birth, signaled, exited)| *birth > 0 && *signaled && *exited));
        drop(worker);
        gone(&path);
    }

    #[test]
    fn a_debug_chain_gap_refuses_retirement_success() {
        let path = fixture();
        let fence = crate::candidate_workspace::LifetimeFence::default();
        let executable = std::env::current_exe().unwrap();
        let (worker, _stdout, _stderr) = Worker::spawn_with_argv0_and_fence(
            &executable,
            &executable,
            &["--exact".into(), HELPER.into(), "--nocapture".into()],
            None,
            &[
                ("AURUM_WORKER_FIXTURE".into(), path.to_string_lossy().into()),
                ("AURUM_WORKER_ROLE".into(), "root".into()),
                ("AURUM_WORKER_CHAIN_BREAK".into(), "1".into()),
            ],
            Some(&fence),
            None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !path.join("ready").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(worker.terminate().is_err());
        assert!(!worker.termination_confirmed().unwrap_or(false));
        assert!(fence.unresolved());
        drop(worker);
        assert!(fence.unresolved());
        gone(&path);
    }
    #[test]
    fn affirmative_native_creation_failure_resolves_no_context_lifetime_claim() {
        let path = fixture();
        let program = path.join("not-an-executable.exe");
        std::fs::write(&program, b"invalid executable fixture").unwrap();
        let fence = crate::candidate_workspace::LifetimeFence::default();
        let error = Worker::spawn_with_argv0_and_fence(
            &program,
            &program,
            &[],
            None,
            &[],
            Some(&fence),
            None,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("process creation"));
        assert!(!fence.unresolved());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn expired_explicit_admission_refuses_before_native_creation() {
        let path = fixture();
        let exe = std::env::current_exe().unwrap();
        let fence = crate::candidate_workspace::LifetimeFence::default();
        let error = Worker::spawn_with_argv0_and_fence(
            &exe,
            &exe,
            &["--exact".into(), HELPER.into(), "--nocapture".into()],
            Some(&path),
            &[
                ("AURUM_WORKER_FIXTURE".into(), path.to_string_lossy().into()),
                ("AURUM_WORKER_ROLE".into(), "root".into()),
            ],
            Some(&fence),
            Some(Instant::now()),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(!fence.unresolved());
        assert!(!path.join("root.pid").exists());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn short_no_context_command_preserves_workspace_while_creator_is_unresolved() {
        assert!(crate::operation_context::OperationContext::current().is_none());
        let mut workspace = crate::candidate_workspace::Workspace::create().unwrap();
        let gate = workspace.path.join("creator-gate");
        std::fs::create_dir(&gate).unwrap();
        let protected = workspace.path.join("protected.bin");
        std::fs::write(&protected, b"protected source").unwrap();
        let before = crate::sha256_file(&protected).unwrap();
        let fence = workspace.lifetime.clone();
        let command = crate::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", workspace.path.to_string_lossy())
            .env("AURUM_WORKER_ROLE", "root")
            .env("AURUM_WORKER_CREATOR_GATE", gate.to_string_lossy())
            .lifetime_fence(&fence);
        let started = Instant::now();
        let running = std::thread::spawn(move || command.run(Duration::from_millis(100)));
        let entered = Instant::now() + Duration::from_secs(2);
        while !gate.join("entered").exists() {
            assert!(Instant::now() < entered);
            std::thread::yield_now();
        }
        let error = running.join().unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(7),
            "constructor reset the short command deadline"
        );
        assert!(fence.unresolved());
        assert!(workspace.clean().is_err());
        assert_eq!(crate::sha256_file(&protected).unwrap(), before);
        assert!(!workspace.path.join("root.pid").exists());
        std::fs::write(gate.join("release"), b"release").unwrap();
        let complete = Instant::now() + Duration::from_secs(2);
        while fence.unresolved() {
            assert!(Instant::now() < complete);
            std::thread::yield_now();
        }
        // Expired admission is checked again after the controlled creator delay.
        // The native no-spawn receipt resolves the fence without user code.
        assert!(!workspace.path.join("root.pid").exists());
        workspace.clean().unwrap();
    }
    #[test]
    fn first_chance_application_handler_and_unhandled_exception_keep_native_semantics() {
        for (role, code, marker) in [
            ("handled-exception", 0, "exception-handled"),
            ("handled-wx86-collision", 0, "exception-handled"),
            (
                "unhandled-exception",
                0xe042_4343_u32 as i32,
                "exception-raised",
            ),
        ] {
            let path = fixture();
            let result = crate::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", HELPER, "--nocapture"])
                .env("AURUM_WORKER_FIXTURE", path.to_string_lossy())
                .env("AURUM_WORKER_ROLE", role)
                .owned_tree()
                .run(Duration::from_secs(10))
                .unwrap();
            assert_eq!(result.code, Some(code), "{}", result.failure_detail());
            assert!(!result.timed_out);
            assert!(path.join(marker).exists());
            std::fs::remove_dir_all(path).unwrap();
        }
    }
    #[test]
    fn wow64_startup_handles_both_loader_breakpoints_and_retires_every_birth() {
        let path = fixture();
        let program = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("SysWOW64")
            .join("cmd.exe");
        assert!(program.is_file());
        let fence = crate::candidate_workspace::LifetimeFence::default();
        let (worker, _stdout, _stderr) = Worker::spawn_with_argv0_and_fence(
            &program,
            &program,
            &[],
            Some(&path),
            &[
                ("TEMP".into(), path.to_string_lossy().into()),
                ("TMP".into(), path.to_string_lossy().into()),
            ],
            Some(&fence),
            None,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let code = loop {
            if let Some(code) = worker.try_wait().unwrap() {
                break code;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(code, 0);
        worker.terminate().unwrap();
        assert!(!fence.unresolved());
        let evidence = worker.census.evidence(&worker.job).unwrap();
        assert_eq!(evidence.active, 0);
        assert_eq!(evidence.total as usize, evidence.members.len());
        assert!(evidence
            .members
            .iter()
            .all(|(_, birth, signal, _)| *birth > 0 && *signal));
        assert!(evidence
            .console_members
            .iter()
            .all(|(_, birth, signal)| *birth > 0 && *signal));
        eprintln!(
            "WOW64_RETIREMENT total={} retained={} console_helpers={} members={:?}",
            evidence.total,
            evidence.members.len(),
            evidence.console_members.len(),
            evidence.members
        );
        drop(worker);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn a_standing_worker_does_not_inherit_finished_operation_cancellation() {
        let path = fixture();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let (worker, _stdout, _stderr) = context
            .scope(|| {
                Worker::spawn(
                    &std::env::current_exe().unwrap(),
                    &["--exact".into(), HELPER.into(), "--nocapture".into()],
                    None,
                    &[
                        ("AURUM_WORKER_FIXTURE".into(), path.to_string_lossy().into()),
                        ("AURUM_WORKER_ROLE".into(), "root".into()),
                    ],
                )
            })
            .unwrap()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !path.join("ready").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        context.request_cancel().unwrap();
        assert!(worker.try_wait().unwrap().is_none());
        worker.terminate().unwrap();
        drop(worker);
        gone(&path);
    }
    fn fixture() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("aurum-worker-test-{}", crate::random::session_id()));
        std::fs::create_dir(&path).unwrap();
        path
    }
    fn heap_environment_fixture(role: &str, inherited: Option<(&str, &str)>) {
        use std::os::windows::process::CommandExt;
        let path = fixture();
        let before = std::env::var_os("_NO_DEBUG_HEAP");
        let mut host = std::process::Command::new(std::env::current_exe().unwrap());
        host.creation_flags(0x08000000)
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", &path)
            .env("AURUM_WORKER_ROLE", role)
            .env_remove("_NO_DEBUG_HEAP");
        if let Some((key, value)) = inherited {
            host.env(key, value);
        }
        let output = host.output().unwrap();
        assert!(
            output.status.success(),
            "host fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(path.join("heap-proof").is_file());
        assert_eq!(std::env::var_os("_NO_DEBUG_HEAP"), before);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn owned_child_defaults_to_normal_heap_without_mutating_parent() {
        heap_environment_fixture("heap-host-default", None);
    }
    #[test]
    fn owned_child_preserves_case_insensitive_explicit_and_ambient_heap_settings() {
        heap_environment_fixture("heap-host-ambient", Some(("_no_debug_heap", "0")));
        heap_environment_fixture("heap-host-explicit", Some(("_No_Debug_Heap", "1")));
    }
    fn ready(path: &Path, host: &mut std::process::Child) {
        let started = Instant::now();
        while !path.join("ready").exists() {
            if started.elapsed() >= Duration::from_secs(8) {
                let _ = host.kill();
                let _ = host.wait();
                panic!("worker tree did not become ready");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn gone(path: &Path) {
        for name in ["root.pid", "leaf.pid"] {
            let pid = std::fs::read_to_string(path.join(name))
                .unwrap()
                .parse()
                .unwrap();
            let started = Instant::now();
            while crate::ownership::inspect(pid).is_some() {
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "owned tree member survived: {name}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn confirmed_retirement_signals_the_original_process_handle() {
        let path = fixture();
        let executable = std::env::current_exe().unwrap();
        let host = crate::ownership::inspect(std::process::id()).unwrap();
        let (worker, _stdout, _stderr) = Worker::spawn(
            &executable,
            &["--exact".into(), HELPER.into(), "--nocapture".into()],
            None,
            &[
                (
                    "AURUM_WORKER_FIXTURE".into(),
                    path.to_string_lossy().into_owned(),
                ),
                ("AURUM_WORKER_ROLE".into(), "root".into()),
            ],
        )
        .unwrap();
        let started = Instant::now();
        while !path.join("ready").exists() {
            assert!(worker.try_wait().unwrap().is_none());
            assert!(started.elapsed() < Duration::from_secs(8));
            std::thread::sleep(Duration::from_millis(10));
        }
        let owner = crate::ownership::OwnershipRecord {
            session: crate::random::session_id(),
            executable,
            pid: worker.pid(),
            started: worker.creation_time().unwrap(),
            project: path.clone(),
            kind: crate::ownership::ProcessKind::Worker,
        };
        assert!(owner.describes(&crate::ownership::inspect(worker.pid()).unwrap()));
        assert!(!worker.termination_confirmed().unwrap());
        worker.terminate().unwrap();
        // Keep both owned handles open while checking retirement. A retained
        // process object is valid after exit and does not represent a live run.
        assert!(worker.try_wait().unwrap().is_some());
        assert!(worker.termination_confirmed().unwrap());
        assert!(crate::ownership::recorded_process_gone(&owner));
        assert_eq!(worker.creation_time().unwrap(), owner.started);
        worker.terminate().unwrap();
        assert_eq!(crate::ownership::inspect(std::process::id()).unwrap(), host);
        gone(&path);
    }
    #[test]
    fn host_crash_terminates_worker_and_descendants() {
        use std::os::windows::process::CommandExt;
        let path = fixture();
        let mut host = std::process::Command::new(std::env::current_exe().unwrap())
            .creation_flags(0x08000000)
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", &path)
            .env("AURUM_WORKER_ROLE", "host")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        ready(&path, &mut host);
        host.kill().unwrap();
        host.wait().unwrap();
        gone(&path);
    }
    #[test]
    fn a_completed_root_does_not_leave_a_pipe_holding_descendant() {
        let path = fixture();
        let started = Instant::now();
        let outcome = crate::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", path.to_string_lossy())
            .env("AURUM_WORKER_ROLE", "root")
            .env("AURUM_WORKER_ROOT_EXIT", "1")
            .owned_tree()
            .run(Duration::from_secs(10))
            .unwrap();
        assert!(outcome.success(), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(8));
        gone(&path);
    }
    #[test]
    fn a_deadline_terminates_the_entire_tree() {
        let path = fixture();
        let outcome = crate::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", path.to_string_lossy())
            .env("AURUM_WORKER_ROLE", "root")
            .owned_tree()
            .run(Duration::from_secs(3))
            .unwrap();
        assert!(outcome.timed_out);
        assert!(path.join("ready").exists());
        gone(&path);
    }
    #[test]
    fn invalid_owned_programs_are_refused_without_shell_fallback() {
        assert!(crate::Command::new("cmd")
            .owned_tree()
            .run(Duration::from_secs(1))
            .is_err());
        assert!(crate::Command::new("C:/worker.bat")
            .owned_tree()
            .run(Duration::from_secs(1))
            .is_err());
    }
    #[test]
    fn owned_capture_preserves_unicode_arguments_environment_and_directory() {
        let path = fixture().join("working space");
        std::fs::create_dir(&path).unwrap();
        let specimen = "quoted \"value\" Unicode \u{65e5} trailing\\";
        let outcome = crate::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--nocapture", "--skip", specimen])
            .directory(&path)
            .env("AURUM_WORKER_FIXTURE", path.to_string_lossy())
            .env("AURUM_WORKER_ROLE", "echo")
            .env("AURUM_WORKER_VALUE", specimen)
            .owned_tree()
            .run(Duration::from_secs(5))
            .unwrap();
        assert!(outcome.success(), "{outcome:?}");
        let line = outcome
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("worker-fixture:"))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["value"], specimen);
        assert_eq!(value["args"].as_array().unwrap().last().unwrap(), specimen);
        assert!(crate::ownership::paths_equal(
            Path::new(value["cwd"].as_str().unwrap()),
            &path
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn resolved_application_keeps_the_quoted_invoked_argument_name() {
        use std::io::Read;
        let path = fixture();
        let application = std::env::current_exe().unwrap();
        let invoked = path.join("cargo proxy \u{65e5}.exe");
        let (worker, mut stdout, mut stderr) = Worker::spawn_with_argv0(
            &application,
            &invoked,
            &["--exact".into(), HELPER.into(), "--nocapture".into()],
            None,
            &[
                (
                    "AURUM_WORKER_FIXTURE".into(),
                    path.to_string_lossy().into_owned(),
                ),
                ("AURUM_WORKER_ROLE".into(), "echo".into()),
                ("AURUM_WORKER_VALUE".into(), "argument-name-witness".into()),
            ],
        )
        .unwrap();
        let started = Instant::now();
        let code = loop {
            if let Some(code) = worker.try_wait().unwrap() {
                break code;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        };
        worker.terminate().unwrap();
        let mut output = String::new();
        let mut errors = String::new();
        stdout.read_to_string(&mut output).unwrap();
        stderr.read_to_string(&mut errors).unwrap();
        assert_eq!(code, 0, "{output}\n{errors}");
        let line = output
            .lines()
            .find_map(|line| line.strip_prefix("worker-fixture:"))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["args"][0], invoked.to_string_lossy().as_ref());
        assert!(worker.termination_confirmed().unwrap());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn observed_storage_failure_stops_the_entire_tree() {
        let path = fixture();
        let error = crate::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", path.to_string_lossy())
            .env("AURUM_WORKER_ROLE", "root")
            .env("AURUM_WORKER_BAD_STORAGE", "1")
            .candidate_storage(&path)
            .run(Duration::from_secs(10))
            .unwrap_err();
        assert!(error.to_string().contains("nesting"), "{error}");
        gone(&path);
    }
    #[test]
    fn quoting_keeps_empty_quotes_and_trailing_slashes() {
        let quoted = |text: &str| String::from_utf16(&quote(OsStr::new(text)).unwrap()).unwrap();
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("a b\\"), "\"a b\\\\\"");
        assert_eq!(quoted("a\\\"b"), "\"a\\\\\\\"b\"");
        assert!(quote(OsStr::new("bad\0text")).is_err());
    }
}

/// Trusted internal child policy, never a project request field.
#[derive(Clone, Copy)]
pub(super) enum ChildEnvironment<'a> {
    Inherit(&'a [(String, String)]),
    Cleared(&'a [(String, String)]),
}
impl<'a> ChildEnvironment<'a> {
    fn overrides(self) -> &'a [(String, String)] {
        match self {
            Self::Inherit(values) | Self::Cleared(values) => values,
        }
    }
    fn inherits(self) -> bool {
        matches!(self, Self::Inherit(_))
    }
}

fn build_environment(
    mut inherited: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    policy: ChildEnvironment<'_>,
) -> io::Result<Vec<u16>> {
    // The caller does not collect ambient entries for Cleared. Keep this
    // defensive discard so the serialized block cannot accept them either.
    if !policy.inherits() {
        inherited.clear();
    }
    for (key, value) in policy.overrides() {
        if key.is_empty() || key.contains(['=', '\0']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid worker environment key",
            ));
        }
        inherited.retain(|(existing, _)| !existing.to_string_lossy().eq_ignore_ascii_case(key));
        inherited.push((key.into(), value.into()));
    }
    // This is the existing child-only heap default. Inherit preserves an
    // ambient value; Cleared preserves only an explicit curated override.
    if !inherited
        .iter()
        .any(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("_NO_DEBUG_HEAP"))
    {
        inherited.push(("_NO_DEBUG_HEAP".into(), "1".into()));
    }
    inherited.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (key, value) in inherited {
        let mut entry = key;
        entry.push("=");
        entry.push(value);
        block.extend(wide(&entry)?);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(test)]
mod child_environment_tests {
    use super::*;

    fn parent_snapshot() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        [
            ("AURUM_FAKE_CREDENTIAL", "fixed-fake-credential"),
            ("CARGO_TARGET_DIR", "A:\\unrelated-parent-target"),
            ("AURUM_UNRELATED_PARENT_VAR", "parent-only"),
            ("_NO_DEBUG_HEAP", "0"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
    }
    fn pairs(block: &[u16]) -> Vec<(String, String)> {
        assert!(block.ends_with(&[0, 0]));
        block
            .split(|unit| *unit == 0)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                let entry = String::from_utf16(entry).unwrap();
                let (key, value) = entry.split_once('=').unwrap();
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }
    fn value<'a>(entries: &'a [(String, String)], name: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
    #[test]
    fn cleared_block_excludes_fake_credentials_cargo_target_and_unrelated_parent_values() {
        let parent = parent_snapshot();
        let preserved = parent.clone();
        let host_before: Vec<_> = [
            "_NO_DEBUG_HEAP",
            "CARGO_TARGET_DIR",
            "AURUM_FAKE_CREDENTIAL",
        ]
        .into_iter()
        .map(|name| (name, std::env::var_os(name)))
        .collect();
        let overrides = [
            ("SystemRoot".into(), "C:\\Windows".into()),
            ("PATH".into(), "A:\\trusted-rust;A:\\trusted-sdk".into()),
            ("TEMP".into(), "A:\\private-temp".into()),
            ("CARGO_HOME".into(), "A:\\private-cargo".into()),
            ("USERPROFILE".into(), "A:\\private-home".into()),
        ];
        let entries = pairs(
            &build_environment(parent.clone(), ChildEnvironment::Cleared(&overrides)).unwrap(),
        );
        assert_eq!(entries.len(), overrides.len() + 1);
        for forbidden in [
            "AURUM_FAKE_CREDENTIAL",
            "CARGO_TARGET_DIR",
            "AURUM_UNRELATED_PARENT_VAR",
        ] {
            assert!(value(&entries, forbidden).is_none());
        }
        for (key, expected) in &overrides {
            assert!(value(&entries, key) == Some(expected.as_str()));
        }
        assert!(value(&entries, "_NO_DEBUG_HEAP") == Some("1"));
        assert!(parent == preserved, "caller snapshot was mutated");
        for (name, before) in host_before {
            assert!(std::env::var_os(name) == before, "host environment changed");
        }
    }
    #[test]
    fn cleared_block_preserves_explicit_heap_zero_and_case_insensitive_overrides() {
        let overrides = [
            ("_no_debug_heap".into(), "0".into()),
            ("Path".into(), "discarded".into()),
            ("PATH".into(), "A:\\trusted-tools".into()),
        ];
        let entries = pairs(
            &build_environment(parent_snapshot(), ChildEnvironment::Cleared(&overrides)).unwrap(),
        );
        assert_eq!(entries.len(), 2);
        assert!(value(&entries, "_NO_DEBUG_HEAP") == Some("0"));
        assert!(value(&entries, "PATH") == Some("A:\\trusted-tools"));
    }
    #[test]
    fn inherited_block_preserves_existing_order_encoding_overrides_and_heap_value() {
        let parent = vec![
            ("PATH".into(), "old".into()),
            ("CARGO_TARGET_DIR".into(), "A:\\parent-target".into()),
            ("_NO_DEBUG_HEAP".into(), "0".into()),
            ("AURUM_FAKE_CREDENTIAL".into(), "fake=credential".into()),
        ];
        let overrides = [("Path".into(), "A:\\tools ü".into())];
        let block = build_environment(parent, ChildEnvironment::Inherit(&overrides)).unwrap();
        let expected: Vec<_> = [
            "AURUM_FAKE_CREDENTIAL=fake=credential",
            "CARGO_TARGET_DIR=A:\\parent-target",
            "Path=A:\\tools ü",
            "_NO_DEBUG_HEAP=0",
        ]
        .into_iter()
        .flat_map(|entry| entry.encode_utf16().chain(std::iter::once(0)))
        .chain(std::iter::once(0))
        .collect();
        assert!(block == expected, "default inherited block bytes changed");
    }
    #[test]
    fn cleared_block_rejects_invalid_keys_and_nul_values_before_creation() {
        for (key, value) in [
            ("", "x"),
            ("bad=name", "x"),
            ("bad\0name", "x"),
            ("VALID", "bad\0value"),
        ] {
            let overrides = [(key.to_owned(), value.to_owned())];
            let error =
                build_environment(Vec::new(), ChildEnvironment::Cleared(&overrides)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }
}
