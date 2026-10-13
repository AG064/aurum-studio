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
    job: OwnedHandle,
    pid: u32,
}
impl Worker {
    pub(super) fn spawn(
        program: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
    ) -> io::Result<(Self, File, File)> {
        Self::spawn_with_argv0(program, program, args, directory, overrides)
    }
    pub(super) fn spawn_with_argv0(
        program: &Path,
        argv0: &Path,
        args: &[String],
        directory: Option<&Path>,
        overrides: &[(String, String)],
    ) -> io::Result<(Self, File, File)> {
        if [program, argv0].iter().any(|path| {
            !path.is_absolute()
                || !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "owned workers require an absolute .exe path",
            ));
        }
        let application = wide(program.as_os_str())?;
        let mut command = quote(argv0.as_os_str())?;
        for arg in args {
            command.push(b' ' as u16);
            command.extend(quote(OsStr::new(arg))?);
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker command is too long",
            ));
        }
        let directory = directory.map(|path| wide(path.as_os_str())).transpose()?;
        let mut environment: Vec<_> = std::env::vars_os().collect();
        for (key, value) in overrides {
            if key.is_empty() || key.contains(['=', '\0']) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid worker environment key",
                ));
            }
            environment
                .retain(|(existing, _)| !existing.to_string_lossy().eq_ignore_ascii_case(key));
            environment.push((key.into(), value.into()));
        }
        environment.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_uppercase());
        let mut block = Vec::new();
        for (key, value) in environment {
            let mut entry = key;
            entry.push("=");
            entry.push(value);
            block.extend(wide(&entry)?);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        unsafe {
            let raw_job = CreateJobObjectW(null_mut(), null());
            if raw_job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = OwnedHandle::from_raw_handle(raw_job);
            let mut limits: ExtendedLimits = zeroed();
            limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            check(SetInformationJobObject(
                job.as_raw_handle(),
                9,
                (&mut limits as *mut ExtendedLimits).cast(),
                size_of::<ExtendedLimits>() as u32,
            ))?;
            let (stdin, input) = pipe(false)?;
            let (stdout, output) = pipe(true)?;
            let (stderr, errors) = pipe(true)?;
            let mut inherited = [
                stdin.as_raw_handle(),
                output.as_raw_handle(),
                errors.as_raw_handle(),
            ];
            let mut jobs = [job.as_raw_handle()];
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
            check(CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null_mut(),
                null_mut(),
                1,
                0x08000000 | 0x00080000 | 0x00000400, // NO_WINDOW, EXTENDED_STARTUPINFO_PRESENT, UNICODE_ENVIRONMENT
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
            Ok((
                Self {
                    process,
                    job,
                    pid: information.pid,
                },
                stdout.into(),
                stderr.into(),
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
        let mut accounting: Accounting = unsafe { zeroed() };
        unsafe {
            check(QueryInformationJobObject(
                self.job.as_raw_handle(),
                1,
                (&mut accounting as *mut Accounting).cast(),
                size_of::<Accounting>() as u32,
                null_mut(),
            ))?;
        }
        // Job accounting can reach zero before the process handle signals
        // completed teardown. Both are required for confirmed retirement.
        Ok(accounting.active_processes == 0 && self.try_wait()?.is_some())
    }
    pub(super) fn terminate(&self) -> io::Result<()> {
        if let Err(error) = unsafe { check(TerminateJobObject(self.job.as_raw_handle(), 1)) } {
            // A concurrent stop or natural exit can finish before this call.
            // An OS failure is only harmless if the same handles prove exit.
            return if self.termination_confirmed().unwrap_or(false) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let started = std::time::Instant::now();
        loop {
            if self.termination_confirmed()? {
                return Ok(());
            }
            if started.elapsed() > std::time::Duration::from_secs(5) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "owned worker tree did not finish termination",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
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
        if role == "echo" {
            println!(
                "worker-fixture:{}",
                serde_json::json!({"args":std::env::args().collect::<Vec<_>>(), "cwd":std::env::current_dir().unwrap(), "value":std::env::var("AURUM_WORKER_VALUE").unwrap()})
            );
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
        if role == "root" {
            use std::os::windows::process::CommandExt;
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .creation_flags(0x08000000)
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
            while !directory.join("leaf.ready").exists() {
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
    fn fixture() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("aurum-worker-test-{}", crate::random::session_id()));
        std::fs::create_dir(&path).unwrap();
        path
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
