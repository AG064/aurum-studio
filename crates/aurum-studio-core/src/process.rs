//! Running child processes, with capture and a bounded wait.
//!
//! Every external tool Studio touches — Cargo, Godot, a project's own
//! validation script — goes through here, so the behaviour that matters is in
//! one place: output is captured without deadlocking, a hung child is killed
//! rather than hanging Studio, and nothing is ever run through a shell.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

/// The result of running a child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The exit code, or `None` when the process was killed by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Whether the deadline was reached and the child was killed.
    pub timed_out: bool,
}

impl Outcome {
    pub fn success(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }

    /// The most useful line of failure text available.
    ///
    /// Prefers stderr because compilers and Godot both report there, and falls
    /// back to stdout so a tool that logs elsewhere is not reported as silent.
    pub fn failure_detail(&self) -> String {
        let source = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return match self.code {
                Some(code) => format!("exited with code {code} and produced no output"),
                None => "was terminated".to_string(),
            };
        }
        trimmed.to_string()
    }

    /// The last `count` lines of combined output, for logs.
    pub fn tail(&self, count: usize) -> Vec<String> {
        let mut lines: Vec<String> = self
            .stdout
            .lines()
            .chain(self.stderr.lines())
            .map(str::to_string)
            .collect();
        let start = lines.len().saturating_sub(count);
        lines.drain(..start);
        lines
    }
}

/// A command to run. Arguments are passed as a list, never as a shell string.
#[derive(Debug, Clone)]
pub struct Command {
    program: PathBuf,
    args: Vec<String>,
    directory: Option<PathBuf>,
    environment: Vec<(String, String)>,
    owned_tree: bool,
    storage_budget: Vec<PathBuf>,
    storage_budget_overflow: bool,
}

impl Command {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            directory: None,
            environment: Vec::new(),
            owned_tree: false,
            storage_budget: Vec::new(),
            storage_budget_overflow: false,
        }
    }

    pub fn arg(mut self, value: impl Into<String>) -> Self {
        self.args.push(value.into());
        self
    }

    /// Add an engine option before its user-argument separator, if present.
    /// Script argument count and order must remain unchanged by diagnostics.
    pub fn arg_before_separator(mut self, value: impl Into<String>) -> Self {
        let index = self
            .args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(self.args.len());
        self.args.insert(index, value.into());
        self
    }

    pub fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(values.into_iter().map(Into::into));
        self
    }

    pub fn directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.directory = Some(path.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// Candidate workers must not outlive their host or leave descendants.
    /// Currently qualified on Windows 10+ only. Ordinary games are unaffected.
    pub(crate) fn owned_tree(mut self) -> Self {
        self.owned_tree = true;
        self
    }
    pub(crate) fn candidate_storage(mut self, workspace: &Path) -> Self {
        let key = storage_root_key(workspace);
        if !self
            .storage_budget
            .iter()
            .any(|root| storage_root_key(root) == key)
        {
            if self.storage_budget.len() == 2 {
                self.storage_budget_overflow = true;
            } else {
                self.storage_budget.push(workspace.to_path_buf());
            }
        }
        self.owned_tree()
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn arguments(&self) -> &[String] {
        &self.args
    }

    /// A redacted description, safe to write to a log.
    pub fn describe(&self) -> String {
        let mut parts = vec![self.program.display().to_string()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }

    /// Run to completion, or kill the child at `timeout`.
    ///
    /// Output is drained on separate threads. Reading the pipes only after the
    /// child exits deadlocks as soon as a build produces more than a pipe
    /// buffer, which a real Cargo invocation always does.
    pub fn run(&self, timeout: Duration) -> std::io::Result<Outcome> {
        if self.storage_budget_overflow {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "owned commands support at most two distinct storage roots",
            ));
        }
        let operation = crate::operation_context::OperationContext::current();
        if let Some(context) = &operation {
            context.check()?;
        }
        let deadline = operation
            .as_ref()
            .map_or(Instant::now() + timeout, |context| {
                context.deadline().min(Instant::now() + timeout)
            });
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "command deadline expired before spawn",
            ));
        }
        // These bounded commands are transient. Accepted gameplay is launched
        // separately and is never placed in an operation's cancellation scope.
        let owned_tree = self.owned_tree || operation.is_some();
        let mut command = StdCommand::new(&self.program);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &self.directory {
            command.current_dir(directory);
        }
        for (key, value) in &self.environment {
            command.env(key, value);
        }

        let (mut child, stdout, stderr) = if owned_tree {
            #[cfg(windows)]
            {
                let program = if self.program.is_absolute() {
                    self.program.clone()
                } else if operation.is_some() {
                    self.program
                        .to_str()
                        .and_then(crate::toolchain::find_on_path)
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "background tool executable was not found",
                            )
                        })?
                } else {
                    self.program.clone()
                };
                let program = if operation.is_some() {
                    std::path::absolute(program).map(crate::project::clean_path)?
                } else {
                    program
                };
                let application = if operation.is_some() {
                    program.canonicalize().map(crate::project::clean_path)?
                } else {
                    program.clone()
                };
                check_before_spawn(operation.as_ref(), deadline)?;
                // Proxies such as rustup select Cargo from argv[0]. Resolve the
                // application for launch while preserving the invoked name.
                let (worker, stdout, stderr) = crate::worker_windows::Worker::spawn_with_argv0(
                    &application,
                    &program,
                    &self.args,
                    self.directory.as_deref(),
                    &self.environment,
                )?;
                (
                    Running::Owned(worker),
                    Some(Box::new(stdout) as Box<dyn Read + Send>),
                    Some(Box::new(stderr) as Box<dyn Read + Send>),
                )
            }
            #[cfg(not(windows))]
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "owned candidate workers are Windows-qualified only",
                ));
            }
        } else {
            check_before_spawn(operation.as_ref(), deadline)?;
            let mut child = command.spawn()?;
            let stdout = child
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
            let stderr = child
                .stderr
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>);
            (Running::Standard(child), stdout, stderr)
        };

        // Readers report over a channel rather than being joined. A killed
        // child can leave a grandchild holding the pipe open — `cargo` spawns
        // `rustc`, a shell spawns whatever it ran — and joining would then
        // block long past the deadline that was just enforced.
        let (sender, receiver) = std::sync::mpsc::channel();
        let collect = |mut pipe: Option<Box<dyn Read + Send>>, is_stdout: bool| {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                if let Some(pipe) = pipe.as_mut() {
                    let mut chunk = [0u8; 8192];
                    let mut truncated = false;
                    loop {
                        match pipe.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(count) => {
                                let keep = (2 * 1024 * 1024usize)
                                    .saturating_sub(buffer.len())
                                    .min(count);
                                buffer.extend_from_slice(&chunk[..keep]);
                                truncated |= keep < count;
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                                continue
                            }
                            Err(_) => break,
                        }
                    }
                    if truncated {
                        buffer.extend_from_slice(b"\nERROR: captured output exceeded 2 MiB; further output was discarded\n");
                    }
                }
                let _ = sender.send((is_stdout, buffer));
            })
        };
        collect(stdout, true);
        collect(stderr, false);

        let mut storage_check = Instant::now();
        let mut timed_out = false;
        let status = loop {
            if let Some(context) = &operation {
                if let Err(error) = context.check() {
                    stop_scoped(&mut child, operation.as_ref())?;
                    return Err(error);
                }
            }
            if Instant::now() >= deadline {
                stop_scoped(&mut child, operation.as_ref())?;
                timed_out = true;
                break None;
            }
            let waited = match child.try_wait() {
                Ok(waited) => waited,
                Err(error) => {
                    stop_scoped(&mut child, operation.as_ref())?;
                    return Err(error);
                }
            };
            match waited {
                Some(status) => break Some(status),
                None => {
                    if Instant::now() >= deadline {
                        stop_scoped(&mut child, operation.as_ref())?;
                        timed_out = true;
                        break None;
                    }
                    if Instant::now() >= storage_check {
                        if let Err(error) = inspect_storage(
                            &self.storage_budget,
                            crate::candidate_workspace::WORKSPACE_BYTES,
                            deadline,
                            operation.as_ref(),
                        ) {
                            stop_scoped(&mut child, operation.as_ref())?;
                            if Instant::now() >= deadline
                                && (error.kind() == std::io::ErrorKind::TimedOut
                                    || error.to_string().contains("elapsed inspection budget"))
                            {
                                timed_out = true;
                                break None;
                            }
                            return Err(error);
                        }
                        if Instant::now() >= deadline {
                            stop_scoped(&mut child, operation.as_ref())?;
                            timed_out = true;
                            break None;
                        }
                        storage_check = Instant::now() + Duration::from_secs(1);
                    }
                    std::thread::sleep(
                        Duration::from_millis(15)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
            }
        };
        // A completed worker may have spawned descendants holding the output
        // pipes. End the owned tree before waiting for readers to drain.
        if owned_tree {
            stop_scoped(&mut child, operation.as_ref())?;
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut received = 0;
        while received < 2 {
            let grace = operation.as_ref().map_or(CAPTURE_GRACE, |context| {
                CAPTURE_GRACE.min(context.deadline().saturating_duration_since(Instant::now()))
            });
            match receiver.recv_timeout(grace) {
                Ok((true, bytes)) => stdout = bytes,
                Ok((false, bytes)) => stderr = bytes,
                // Output is lost rather than the deadline being broken.
                Err(_) => break,
            }
            received += 1;
        }

        Ok(Outcome {
            code: status.flatten(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            timed_out,
        })
    }
}

enum Running {
    Standard(std::process::Child),
    #[cfg(windows)]
    Owned(crate::worker_windows::Worker),
}
fn storage_root_key(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // Do not resolve links while deduplicating: every distinct root still
    // goes through the scanner's reparse-point and filesystem checks.
    if cfg!(windows) && absolute.to_str().is_some() {
        PathBuf::from(
            crate::project::clean_path(absolute)
                .to_string_lossy()
                .to_lowercase(),
        )
    } else {
        absolute
    }
}
fn inspect_storage(
    roots: &[PathBuf],
    limit: u64,
    deadline: Instant,
    context: Option<&crate::operation_context::OperationContext>,
) -> std::io::Result<()> {
    let mut used = 0;
    for root in roots {
        if let Some(context) = context {
            context.check()?;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let bytes = crate::candidate_workspace::usage_with_budget(root, limit - used, remaining)
            .map_err(std::io::Error::other)?;
        used += bytes;
    }
    if let Some(context) = context {
        context.check()?;
    }
    if Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "command deadline expired during storage inspection",
        ));
    }
    Ok(())
}
fn check_before_spawn(
    context: Option<&crate::operation_context::OperationContext>,
    deadline: Instant,
) -> std::io::Result<()> {
    if let Some(context) = context {
        context.check()?;
    }
    if Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "command deadline expired before spawn",
        ));
    }
    Ok(())
}
fn stop_scoped(
    child: &mut Running,
    context: Option<&crate::operation_context::OperationContext>,
) -> std::io::Result<()> {
    let result = child.stop();
    if result.is_err() {
        if let Some(context) = context {
            context.mark_termination_unconfirmed();
        }
    }
    result
}
impl Running {
    fn try_wait(&mut self) -> std::io::Result<Option<Option<i32>>> {
        match self {
            Self::Standard(child) => Ok(child.try_wait()?.map(|status| status.code())),
            #[cfg(windows)]
            Self::Owned(worker) => Ok(worker.try_wait()?.map(Some)),
        }
    }
    fn stop(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => {
                let _ = child.kill();
                let _ = child.wait();
                Ok(())
            }
            #[cfg(windows)]
            Self::Owned(worker) => worker.terminate(),
        }
    }
}

/// How long a tool version probe is allowed to take.
///
/// Generous for a `--version` call, short enough that a tool which hangs on
/// startup does not stall `doctor`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for a finished child's output to drain before giving up.
const CAPTURE_GRACE: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;
    fn storage_fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aurum-process-storage-test-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        root
    }
    #[test]
    fn duplicate_storage_roots_keep_two_slots_and_excess_is_refused_before_spawn() {
        let root = storage_fixture();
        let first = root.join("first");
        let second = root.join("second");
        let command = Command::new("definitely-not-a-real-program-xyz")
            .candidate_storage(&first)
            .candidate_storage(&first.join("."))
            .candidate_storage(&second)
            .candidate_storage(&second);
        assert_eq!(command.storage_budget.len(), 2);
        assert!(!command.storage_budget_overflow);
        #[cfg(windows)]
        let command =
            command.candidate_storage(&PathBuf::from(first.to_string_lossy().to_uppercase()));
        assert!(!command.storage_budget_overflow);
        let error = command
            .candidate_storage(&root.join("third"))
            .run(Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("two distinct storage roots"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn storage_roots_share_one_byte_limit_and_expired_deadline() {
        let root = storage_fixture();
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("bytes"), [0u8; 50]).unwrap();
        std::fs::write(second.join("bytes"), [0u8; 50]).unwrap();
        let roots = [first, second];
        let deadline = Instant::now() + Duration::from_secs(5);
        inspect_storage(&roots[..1], 99, deadline, None).unwrap();
        inspect_storage(&roots[1..], 99, deadline, None).unwrap();
        assert!(inspect_storage(&roots, 99, deadline, None)
            .unwrap_err()
            .to_string()
            .contains("observed-byte budget"));
        inspect_storage(&roots, 100, deadline, None).unwrap();
        assert!(inspect_storage(&roots, 100, Instant::now(), None)
            .unwrap_err()
            .to_string()
            .contains("elapsed inspection budget"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn an_invalid_secondary_storage_root_is_never_skipped() {
        let root = storage_fixture();
        let first = root.join("first");
        let second = root.join("regular-file");
        std::fs::create_dir(&first).unwrap();
        std::fs::write(&second, "not a directory").unwrap();
        assert!(inspect_storage(
            &[first, second],
            100,
            Instant::now() + Duration::from_secs(5),
            None,
        )
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    fn outside_secondary_storage_stops_only_the_owned_tree(link: bool) {
        use std::os::windows::process::CommandExt;
        const HELPER: &str = "worker_windows::tests::worker_tree_helper";
        struct Unrelated(std::process::Child);
        impl Drop for Unrelated {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = storage_fixture();
        let first = root.join("first");
        let second = root.join("second");
        let unrelated_path = root.join("unrelated");
        let outside = root.join("outside");
        for path in [&first, &second, &unrelated_path, &outside] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(outside.join("keep"), "untouched").unwrap();
        let executable = std::env::current_exe().unwrap();
        let unrelated = Unrelated(
            StdCommand::new(&executable)
                .args(["--exact", HELPER, "--nocapture"])
                .env("AURUM_WORKER_FIXTURE", &unrelated_path)
                .env("AURUM_WORKER_ROLE", "leaf")
                .creation_flags(0x08000000)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let unrelated_identity = crate::ownership::inspect(unrelated.0.id()).unwrap();
        let command = Command::new(executable)
            .args(["--exact", HELPER, "--nocapture"])
            .env("AURUM_WORKER_FIXTURE", first.to_string_lossy())
            .env("AURUM_WORKER_ROLE", "root")
            .candidate_storage(&first)
            .candidate_storage(&second);
        let running = std::thread::spawn(move || command.run(Duration::from_secs(10)));
        let started = Instant::now();
        while !first.join("ready").exists() || !unrelated_path.join("leaf.ready").exists() {
            assert!(
                !running.is_finished(),
                "owned worker exited before readiness"
            );
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
        let owners: Vec<_> = ["root.pid", "leaf.pid"]
            .into_iter()
            .map(|filename| {
                let pid = std::fs::read_to_string(first.join(filename))
                    .unwrap()
                    .parse::<u32>()
                    .unwrap();
                let live = crate::ownership::inspect(pid).unwrap();
                crate::ownership::OwnershipRecord {
                    session: crate::random::session_id(),
                    executable: live.executable,
                    pid,
                    started: live.started.unwrap(),
                    project: first.clone(),
                    kind: crate::ownership::ProcessKind::Worker,
                }
            })
            .collect();
        let redirected = second.join("redirected");
        if link {
            let created = StdCommand::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&redirected)
                .arg(&outside)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(created.status.success(), "{created:?}");
        } else {
            use std::os::windows::io::AsRawHandle;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn DeviceIoControl(
                    handle: *mut std::ffi::c_void,
                    code: u32,
                    input: *mut std::ffi::c_void,
                    input_bytes: u32,
                    output: *mut std::ffi::c_void,
                    output_bytes: u32,
                    returned_bytes: *mut u32,
                    overlapped: *mut std::ffi::c_void,
                ) -> i32;
            }
            let file = std::fs::File::create(second.join("too-large")).unwrap();
            let mut returned = 0;
            // A sparse length proves the byte guard without allocating 1 GiB.
            assert_ne!(
                unsafe {
                    DeviceIoControl(
                        file.as_raw_handle(),
                        0x900c4, // FSCTL_SET_SPARSE
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            file.set_len(crate::candidate_workspace::WORKSPACE_BYTES + 1)
                .unwrap();
        }
        let error = running.join().unwrap().unwrap_err();
        let expected = if link { "link" } else { "observed-byte budget" };
        assert!(error.to_string().contains(expected), "{error}");
        for owner in owners {
            assert!(crate::ownership::recorded_process_gone(&owner));
        }
        assert_eq!(
            crate::ownership::inspect(unrelated.0.id()),
            Some(unrelated_identity)
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("keep")).unwrap(),
            "untouched"
        );
        if link {
            std::fs::remove_dir(redirected).unwrap();
        }
        drop(unrelated);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn oversize_secondary_storage_stops_owned_descendants_and_preserves_unrelated_process() {
        outside_secondary_storage_stops_only_the_owned_tree(false);
    }
    #[cfg(windows)]
    #[test]
    fn linked_secondary_storage_stops_owned_descendants_and_preserves_unrelated_process() {
        outside_secondary_storage_stops_only_the_owned_tree(true);
    }
    #[cfg(windows)]
    #[test]
    fn background_cargo_metadata_preserves_the_invoked_proxy_name() {
        let path = std::env::temp_dir().join(format!(
            "aurum-cargo-proxy-test-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname=\"cargo-proxy-witness\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[lib]\ncrate-type=[\"cdylib\"]\n",
        )
        .unwrap();
        std::fs::write(path.join("src/lib.rs"), "pub fn witness() {}\n").unwrap();
        let lock = "version=4\n\n[[package]]\nname=\"cargo-proxy-witness\"\nversion=\"0.1.0\"\n";
        std::fs::write(path.join("Cargo.lock"), lock).unwrap();
        let cargo = crate::toolchain::find_on_path("cargo").expect("Cargo must be installed");
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(10),
        );
        let outcome = context
            .scope(|| {
                Command::new(cargo)
                    .args([
                        "metadata",
                        "--format-version",
                        "1",
                        "--no-deps",
                        "--locked",
                        "--offline",
                    ])
                    .directory(&path)
                    .env("CARGO_TARGET_DIR", path.join("target").to_string_lossy())
                    .run(Duration::from_secs(10))
            })
            .unwrap()
            .unwrap();
        assert!(outcome.success(), "{outcome:?}");
        let metadata: serde_json::Value = serde_json::from_str(&outcome.stdout).unwrap();
        assert_eq!(metadata["packages"][0]["name"], "cargo-proxy-witness");
        assert_eq!(
            std::fs::read_to_string(path.join("Cargo.lock")).unwrap(),
            lock
        );
        assert!(crate::operation_context::OperationContext::current().is_none());
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn expired_commands_do_not_spawn_or_leave_a_scoped_context() {
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(5),
        );
        let error = context
            .scope(|| Command::new("definitely-not-a-real-program-xyz").run(Duration::ZERO))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(crate::operation_context::OperationContext::current().is_none());
    }
    #[cfg(not(windows))]
    #[test]
    fn background_tree_ownership_is_refused_on_unqualified_platforms() {
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(5),
        );
        let error = context
            .scope(|| echo_command("fixture").run(Duration::from_secs(1)))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        assert!(crate::operation_context::OperationContext::current().is_none());
    }

    #[test]
    fn diagnostic_engine_flags_do_not_become_script_arguments() {
        let command = Command::new("engine")
            .args([
                "--headless",
                "--script",
                "worker.gd",
                "--",
                "request.json",
                "response.json",
            ])
            .arg_before_separator("--verbose");
        assert_eq!(
            command.arguments(),
            &[
                "--headless",
                "--script",
                "worker.gd",
                "--verbose",
                "--",
                "request.json",
                "response.json"
            ]
        );
        assert_eq!(
            Command::new("engine")
                .arg("--editor")
                .arg_before_separator("--verbose")
                .arguments(),
            &["--editor", "--verbose"]
        );
    }

    /// A program that exists on every Windows machine and can echo.
    #[cfg(windows)]
    fn echo_command(text: &str) -> Command {
        Command::new("cmd").args(["/c", "echo", text])
    }

    #[cfg(not(windows))]
    fn echo_command(text: &str) -> Command {
        Command::new("echo").arg(text)
    }

    #[test]
    fn captures_stdout_and_a_zero_exit() {
        let outcome = echo_command("hello").run(PROBE_TIMEOUT).unwrap();
        assert!(outcome.success(), "{outcome:?}");
        assert_eq!(outcome.code, Some(0));
        assert!(outcome.stdout.contains("hello"), "{outcome:?}");
        assert!(!outcome.timed_out);
    }

    #[test]
    fn a_nonzero_exit_is_reported_with_its_output() {
        #[cfg(windows)]
        let command = Command::new("cmd").args(["/c", "echo problem 1>&2 & exit 3"]);
        #[cfg(not(windows))]
        let command = Command::new("sh").args(["-c", "echo problem 1>&2; exit 3"]);

        let outcome = command.run(PROBE_TIMEOUT).unwrap();
        assert!(!outcome.success());
        assert_eq!(outcome.code, Some(3));
        // stderr is preferred because that is where compilers report.
        assert!(outcome.failure_detail().contains("problem"), "{outcome:?}");
    }

    #[test]
    fn a_missing_program_is_an_io_error_not_a_panic() {
        let error = Command::new("definitely-not-a-real-program-xyz")
            .run(PROBE_TIMEOUT)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_hung_child_is_killed_at_the_deadline() {
        // A direct child, not a shell: a shell's grandchild survives the kill
        // and holds the pipe open, which tests process-tree ownership rather
        // than the deadline. That is a separate concern (see the note in
        // `run`), and conflating them hides which one broke.
        #[cfg(windows)]
        let command = Command::new("ping").args(["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let command = Command::new("sleep").arg("30");

        let started = Instant::now();
        let outcome = command.run(Duration::from_millis(400)).unwrap();
        assert!(outcome.timed_out, "the deadline should have been enforced");
        assert!(!outcome.success());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the kill took far longer than the deadline"
        );
    }

    #[test]
    fn large_output_does_not_deadlock() {
        // Well past a pipe buffer, which is where a read-after-wait design
        // would hang forever.
        #[cfg(windows)]
        let command = Command::new("cmd").args([
            "/c",
            "for /L %i in (1,1,4000) do @echo line %i padding-padding-padding-padding",
        ]);
        #[cfg(not(windows))]
        let command = Command::new("sh").args([
            "-c",
            "for i in $(seq 1 4000); do echo line $i padding-padding-padding; done",
        ]);

        let outcome = command.run(Duration::from_secs(60)).unwrap();
        assert!(outcome.success(), "{outcome:?}");
        assert!(
            outcome.stdout.lines().count() >= 4000,
            "captured {} lines",
            outcome.stdout.lines().count()
        );
    }

    #[test]
    fn failure_detail_handles_silent_failures() {
        let outcome = Outcome {
            code: Some(7),
            stdout: String::new(),
            stderr: "   \n".into(),
            timed_out: false,
        };
        assert!(outcome.failure_detail().contains("code 7"));

        let killed = Outcome {
            code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        };
        assert!(killed.failure_detail().contains("terminated"));
    }

    #[test]
    fn tail_keeps_the_end_of_combined_output() {
        let outcome = Outcome {
            code: Some(0),
            stdout: "a\nb\nc\n".into(),
            stderr: "d\ne\n".into(),
            timed_out: false,
        };
        assert_eq!(outcome.tail(2), vec!["d".to_string(), "e".to_string()]);
        assert_eq!(
            outcome.tail(99).len(),
            5,
            "asking for more returns all of it"
        );
    }

    #[test]
    fn describe_is_readable_and_shell_free() {
        let command = Command::new("cargo")
            .args(["build", "-p", "aurum-godot"])
            .env("AURUM_TOKEN", "secret");
        assert_eq!(command.describe(), "cargo build -p aurum-godot");
        // The environment never appears in the description, so a token cannot
        // leak into a log through it.
        assert!(!command.describe().contains("secret"));
    }

    #[test]
    fn arguments_are_passed_as_a_list_not_a_shell_string() {
        let command = echo_command("one two");
        // Two on Windows, where the helper carries `cmd /c` in front of the
        // program, and none elsewhere, where the program is the whole command.
        // Asserting the Windows count everywhere is how this test failed on
        // macOS while the behaviour it exists to check was perfectly fine.
        let expected = if cfg!(windows) { 3 } else { 1 };
        assert_eq!(command.arguments().len(), expected);
        // A shell would have split this into two arguments.
        let outcome = command.run(PROBE_TIMEOUT).unwrap();
        assert!(outcome.stdout.contains("one two"), "{outcome:?}");
    }
}
