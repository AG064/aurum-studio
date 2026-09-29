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
}

impl Command {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            directory: None,
            environment: Vec::new(),
        }
    }

    pub fn arg(mut self, value: impl Into<String>) -> Self {
        self.args.push(value.into());
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

        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

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
        collect(stdout.map(|s| Box::new(s) as Box<dyn Read + Send>), true);
        collect(stderr.map(|s| Box::new(s) as Box<dyn Read + Send>), false);

        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait()? {
                Some(status) => break Some(status),
                None => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            }
        };

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut received = 0;
        while received < 2 {
            match receiver.recv_timeout(CAPTURE_GRACE) {
                Ok((true, bytes)) => stdout = bytes,
                Ok((false, bytes)) => stderr = bytes,
                // Output is lost rather than the deadline being broken.
                Err(_) => break,
            }
            received += 1;
        }

        Ok(Outcome {
            code: status.and_then(|s| s.code()),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            timed_out,
        })
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
