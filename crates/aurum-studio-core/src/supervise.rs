//! Launching and stopping the processes a session owns.
//!
//! Two rules shape everything here.
//!
//! **Only what Studio launched is ever stopped.** A stop request checks the
//! ownership record against the live process first, and refuses on any
//! mismatch, so a recycled identifier cannot turn `aurum stop` into a way to
//! kill something unrelated.
//!
//! **Unsaved work is never discarded.** A normal stop asks the process to
//! close and waits. If it is still running afterwards, that is reported and
//! the decision goes back to the user rather than escalating to a kill on its
//! own.

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::ownership::{inspect, LiveProcess, OwnershipRecord, ProcessKind};
use crate::process::Command;
use crate::session::Session;

/// What to launch.
///
/// Arguments are stored complete rather than assembled at launch time. The
/// first draft prepended Godot's `--path` inside the command builder, which
/// silently made the type Godot-only: any other executable received an
/// argument it did not understand.
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    /// The executable to run.
    pub executable: PathBuf,
    /// The project root this launch belongs to, recorded for ownership.
    pub project: PathBuf,
    /// Where to run it, when that matters.
    pub working_directory: Option<PathBuf>,
    pub kind: ProcessKind,
    /// The complete argument list, without a shell.
    pub arguments: Vec<String>,
    /// Environment entries, used for the bridge configuration.
    pub environment: Vec<(String, String)>,
}

impl LaunchRequest {
    /// Launch Godot against a project directory.
    pub fn godot(
        executable: impl Into<PathBuf>,
        project_directory: impl Into<PathBuf>,
        project: impl Into<PathBuf>,
    ) -> Self {
        let project_directory = project_directory.into();
        Self {
            executable: executable.into(),
            project: project.into(),
            working_directory: project_directory.parent().map(Path::to_path_buf),
            kind: ProcessKind::Editor,
            arguments: vec![
                "--path".to_string(),
                project_directory.display().to_string(),
            ],
            environment: Vec::new(),
        }
    }

    /// Launch any executable with the arguments given, for helpers and tests.
    pub fn plain(executable: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            project: project.into(),
            working_directory: None,
            kind: ProcessKind::Worker,
            arguments: Vec::new(),
            environment: Vec::new(),
        }
    }

    /// The argument list, without a shell.
    pub fn command_line(&self) -> Vec<String> {
        self.arguments.clone()
    }

    /// A description with any environment values removed.
    ///
    /// The environment carries the bridge token, so it is never rendered.
    pub fn describe(&self) -> String {
        format!(
            "{} {}",
            self.executable.display(),
            self.command_line().join(" ")
        )
    }
}

/// Why a launch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The executable could not be started.
    Spawn(String),
    /// It started but could not be described, so ownership could not be
    /// recorded and it must not be supervised.
    Unidentifiable(u32),
    Io(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(m) => write!(f, "could not start the process: {m}"),
            Self::Unidentifiable(pid) => write!(
                f,
                "started process {pid} but the system would not describe it, so it cannot be \
                 supervised safely"
            ),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// A running process Studio owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub record: OwnershipRecord,
}

impl Launched {
    pub fn pid(&self) -> u32 {
        self.record.pid
    }
}

/// Launch a process and record ownership of it.
pub fn launch(request: &LaunchRequest, session: &Session) -> Result<Launched, LaunchError> {
    let log_path = session.log_path();

    // Child output goes to the session log rather than a pipe: a long-running
    // editor would otherwise fill a pipe buffer and block, and the log is
    // where a user looks anyway.
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| LaunchError::Io(format!("could not open '{}': {e}", log_path.display())))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| LaunchError::Io(format!("could not duplicate the log handle: {e}")))?;

    let mut command = std::process::Command::new(&request.executable);
    command
        .args(&request.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(directory) = &request.working_directory {
        command.current_dir(directory);
    }
    for (key, value) in &request.environment {
        command.env(key, value);
    }

    let child: Child = command
        .spawn()
        .map_err(|e| LaunchError::Spawn(format!("{}: {e}", request.executable.display())))?;
    let pid = child.id();

    // The process identifier alone is not enough to supervise safely, so a
    // process the system will not describe is reported rather than adopted.
    //
    // Retried, because a freshly spawned process is not immediately visible to
    // the description query: asking once and giving up would fail every real
    // launch on timing alone.
    let parent_image = std::env::current_exe().ok().map(normalize_image);
    let requested_image = resolve_requested_image(request);
    let live = describe_after_start(
        pid,
        IDENTIFY_TIMEOUT,
        parent_image.as_deref(),
        requested_image.as_deref(),
    )
    .ok_or(LaunchError::Unidentifiable(pid))?;
    let started = live
        .started
        .clone()
        .ok_or(LaunchError::Unidentifiable(pid))?;

    let record = OwnershipRecord {
        session: session.id.clone(),
        executable: live.executable.clone(),
        pid,
        started,
        project: request.project.clone(),
        kind: request.kind,
    };
    session
        .log(&format!(
            "launched {} pid {} ({})",
            request.kind.label(),
            pid,
            request.describe()
        ))
        .map_err(|e| LaunchError::Io(e.to_string()))?;
    record
        .write(&session.ownership_directory())
        .map_err(|e| LaunchError::Io(format!("could not record ownership: {e}")))?;

    Ok(Launched { record })
}

/// What a stop attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// The process is gone.
    Stopped,
    /// It was not running to begin with.
    NotRunning,
    /// It is still running: the request was polite and it did not comply.
    StillRunning,
    /// The process is not the one the record describes, so it was left alone.
    Refused(String),
}

impl StopOutcome {
    pub fn stopped(&self) -> bool {
        matches!(self, Self::Stopped | Self::NotRunning)
    }

    pub fn describe(&self, kind: ProcessKind) -> String {
        match self {
            Self::Stopped => format!("stopped the {}", kind.label()),
            Self::NotRunning => format!("the {} was not running", kind.label()),
            Self::StillRunning => format!(
                "the {} is still running; it did not close when asked",
                kind.label()
            ),
            Self::Refused(reason) => format!("left the process alone: {reason}"),
        }
    }
}

/// Ask a process to close, then wait.
///
/// `force` escalates to an immediate termination, which discards unsaved work,
/// so callers are expected to ask the user before setting it.
pub fn terminate(record: &OwnershipRecord, force: bool, timeout: Duration) -> StopOutcome {
    let Some(live) = inspect(record.pid) else {
        return StopOutcome::NotRunning;
    };

    // The safety check: everything must still match.
    if !record.describes(&live) {
        return StopOutcome::Refused(describe_mismatch(record, &live));
    }

    // A refusal must mean "this is not our process". A process that is simply
    // refusing to close is a different answer, and conflating them would hide
    // the distinction that matters.
    match request_close(record.pid, force) {
        CloseRequest::Requested => {
            if wait_for_exit(record.pid, timeout) {
                StopOutcome::Stopped
            } else {
                StopOutcome::StillRunning
            }
        }
        // It is alive and declined to close. Reported as such rather than as a
        // refusal, so the caller can offer to force it.
        //
        // Windows only, because only Windows has this answer. `taskkill`
        // distinguishes "delivered" from "the process will not close without
        // being forced", and that distinction is worth carrying to the caller.
        // A Unix signal has no such reply: it is delivered or it is not, and
        // whether it was honoured is decided by waiting, which is the
        // `Requested` arm above. Gating the variant is not tidiness — an
        // unconstructed variant is a dead-code error under `-D warnings`, so
        // leaving it unconditional fails the build on Linux and macOS.
        #[cfg(windows)]
        CloseRequest::Declined(reason) => {
            if force {
                StopOutcome::Refused(reason)
            } else {
                StopOutcome::StillRunning
            }
        }
        CloseRequest::Failed(error) => {
            StopOutcome::Refused(format!("the stop request failed: {error}"))
        }
    }
}

/// What asking a process to close did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CloseRequest {
    /// The request was delivered; whether it is honoured is a separate
    /// question, answered by waiting.
    Requested,
    /// The process is alive and will not close without being forced.
    #[cfg(windows)]
    Declined(String),
    /// The request could not be delivered.
    Failed(String),
}

/// Explain which field failed to match, so a refusal is diagnosable.
fn describe_mismatch(record: &OwnershipRecord, live: &LiveProcess) -> String {
    if record.pid != live.pid {
        return format!("identifier {} is now a different process", record.pid);
    }
    if !crate::ownership::paths_equal(&record.executable, &live.executable) {
        return format!(
            "identifier {} now belongs to '{}', not '{}'",
            record.pid,
            live.executable.display(),
            record.executable.display()
        );
    }
    match &live.started {
        None => format!(
            "the system would not report a start time for {}, so ownership cannot be proven",
            record.pid
        ),
        Some(started) => format!(
            "identifier {} has been reused (started {started}, recorded {})",
            record.pid, record.started
        ),
    }
}

#[cfg(windows)]
fn request_close(pid: u32, force: bool) -> CloseRequest {
    // Without /F this posts a close request and lets the process decide, which
    // is what preserves unsaved work.
    let mut arguments = vec!["/PID".to_string(), pid.to_string(), "/T".to_string()];
    if force {
        arguments.push("/F".to_string());
    }
    let Ok(outcome) = Command::new("taskkill")
        .args(arguments)
        .run(Duration::from_secs(30))
    else {
        return CloseRequest::Failed("taskkill could not be run".to_string());
    };

    // A process that has already exited is not a failure.
    if outcome.success() || outcome.stderr.contains("not found") {
        return CloseRequest::Requested;
    }
    // taskkill's own words for "it is alive and would not close politely".
    if outcome.stderr.contains("can only be terminated forcefully") {
        return CloseRequest::Declined(
            "the process is still running and will not close on request".to_string(),
        );
    }
    CloseRequest::Failed(outcome.failure_detail())
}

#[cfg(not(windows))]
fn request_close(pid: u32, force: bool) -> CloseRequest {
    let signal = if force { "-KILL" } else { "-TERM" };
    let Ok(outcome) = Command::new("kill")
        .args([signal, &pid.to_string()])
        .run(Duration::from_secs(30))
    else {
        return CloseRequest::Failed("kill could not be run".to_string());
    };
    if outcome.success() {
        CloseRequest::Requested
    } else {
        CloseRequest::Failed(outcome.failure_detail())
    }
}

/// How long to wait for a spawned process to become describable.
pub const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait for the system to be able to describe a process, and require a start
/// time rather than settling for a name.
fn normalize_image(path: PathBuf) -> PathBuf {
    crate::project::clean_path(path.canonicalize().unwrap_or(path))
}

fn resolve_requested_image(request: &LaunchRequest) -> Option<PathBuf> {
    if request.executable.is_absolute() {
        return Some(normalize_image(request.executable.clone()));
    }
    if request.executable.components().count() == 1 {
        return crate::toolchain::find_on_path(request.executable.to_str()?).map(normalize_image);
    }
    let base = request
        .working_directory
        .clone()
        .or_else(|| std::env::current_dir().ok())?;
    Some(normalize_image(base.join(&request.executable)))
}

fn identity_is_ready(
    live: &LiveProcess,
    parent_image: Option<&Path>,
    requested_image: Option<&Path>,
) -> bool {
    if live.started.is_none() {
        return false;
    }
    if parent_image.is_none() {
        return requested_image
            .is_some_and(|requested| crate::ownership::paths_equal(requested, &live.executable));
    }
    // A new Linux child can still expose its inherited parent image through
    // /proc immediately after spawn returns. Do not record that transient
    // identity unless the caller deliberately launched the same executable.
    let inherited =
        parent_image.is_some_and(|parent| crate::ownership::paths_equal(parent, &live.executable));
    let self_launch = parent_image
        .zip(requested_image)
        .is_some_and(|(parent, requested)| crate::ownership::paths_equal(parent, requested));
    !inherited || self_launch
}

fn describe_after_start(
    pid: u32,
    timeout: Duration,
    parent_image: Option<&Path>,
    requested_image: Option<&Path>,
) -> Option<LiveProcess> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(live) = inspect(pid) {
            if identity_is_ready(&live, parent_image, requested_image) {
                return Some(live);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll until the process is gone.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if inspect(pid).is_none() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stop every process a session recorded, worst-first is not meaningful here:
/// games go before editors, so a closing editor does not relaunch one.
pub fn stop_session(
    session: &Session,
    force: bool,
    timeout: Duration,
) -> Vec<(ProcessKind, StopOutcome)> {
    let mut records = OwnershipRecord::read_all(&session.ownership_directory());
    records.sort_by_key(|record| match record.kind {
        ProcessKind::Game => 0,
        ProcessKind::Worker => 1,
        ProcessKind::Editor => 2,
    });

    let mut outcomes = Vec::with_capacity(records.len());
    for record in records {
        let outcome = terminate(&record, force, timeout);
        // A record for a process that is gone is stale, not an error.
        if outcome.stopped() {
            let _ = record.remove(&session.ownership_directory());
        }
        let _ = session.log(&format!(
            "{}: {}",
            record.kind.label(),
            outcome.describe(record.kind)
        ));
        outcomes.push((record.kind, outcome));
    }
    outcomes
}

/// A launch request's environment, including the bridge token.
///
/// Kept separate from [`LaunchRequest::describe`] so a token cannot reach a log
/// through a rendering path.
pub fn bridge_environment(session: &Session, port: Option<u16>) -> Vec<(String, String)> {
    let mut environment = vec![
        ("AURUM_SESSION".to_string(), session.id.clone()),
        ("AURUM_BRIDGE_TOKEN".to_string(), session.token.clone()),
        ("AURUM_BRIDGE_VERSION".to_string(), "1".to_string()),
    ];
    // The port is only meaningful once a listener exists, so it is omitted
    // rather than published as zero.
    if let Some(port) = port {
        environment.push(("AURUM_BRIDGE_PORT".to_string(), port.to_string()));
    }
    environment
}

/// A path that is a directory, or `None`.
pub fn existing_directory(path: &Path) -> Option<PathBuf> {
    path.is_dir().then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_parent_image_is_not_recorded_as_the_launched_program() {
        let parent = Path::new("/fixtures/aurum-test");
        let child = Path::new("/fixtures/sleep");
        let mut live = LiveProcess {
            pid: 4915,
            executable: parent.into(),
            started: Some("10002".into()),
        };
        assert!(!identity_is_ready(&live, Some(parent), Some(child)));
        assert!(!identity_is_ready(&live, Some(parent), None));
        live.executable = child.into();
        assert!(identity_is_ready(&live, Some(parent), Some(child)));
        live.started = None;
        assert!(!identity_is_ready(&live, Some(parent), Some(child)));
    }

    #[test]
    fn deliberate_self_launch_and_missing_parent_identity_are_explicit() {
        let executable = Path::new("/fixtures/aurum");
        let live = LiveProcess {
            pid: 10,
            executable: executable.into(),
            started: Some("t".into()),
        };
        assert!(identity_is_ready(&live, Some(executable), Some(executable)));
        assert!(identity_is_ready(&live, None, Some(executable)));
        assert!(!identity_is_ready(&live, None, None));
        assert!(!identity_is_ready(
            &live,
            None,
            Some(Path::new("/fixtures/other"))
        ));
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-supervise-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stand-in for a long-running editor: something that exists on every
    /// Windows machine and does not exit on its own.
    #[cfg(windows)]
    fn sleeper(seconds: u32) -> (PathBuf, Vec<String>) {
        (
            PathBuf::from("ping"),
            vec!["-n".into(), seconds.to_string(), "127.0.0.1".into()],
        )
    }

    #[cfg(not(windows))]
    fn sleeper(seconds: u32) -> (PathBuf, Vec<String>) {
        (PathBuf::from("sleep"), vec![seconds.to_string()])
    }

    fn sleeper_request(project: &Path) -> LaunchRequest {
        let (executable, arguments) = sleeper(120);
        LaunchRequest {
            executable,
            project: project.to_path_buf(),
            working_directory: None,
            kind: ProcessKind::Editor,
            arguments,
            environment: Vec::new(),
        }
    }

    #[test]
    fn a_launch_records_enough_to_recognise_the_process() {
        let root = temp_root("launch");
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session = Session::create_in(&root.join("state"), &project).unwrap();

        let launched = launch(&sleeper_request(&project), &session).unwrap();
        assert_eq!(launched.record.kind, ProcessKind::Editor);
        assert_eq!(launched.record.project, project);
        assert!(
            !launched.record.started.is_empty(),
            "a start time is required"
        );
        assert!(launched.record.executable.is_file());

        // The record is on disk, so a later command can find it.
        let records = OwnershipRecord::read_all(&session.ownership_directory());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pid, launched.pid());

        // And it still describes the process, which is what stop relies on.
        let live = inspect(launched.pid()).unwrap();
        assert!(launched.record.describes(&live));

        let outcome = terminate(&launched.record, true, Duration::from_secs(30));
        assert_eq!(outcome, StopOutcome::Stopped);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_polite_stop_closes_the_process() {
        let root = temp_root("polite");
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session = Session::create_in(&root.join("state"), &project).unwrap();

        let launched = launch(&sleeper_request(&project), &session).unwrap();
        // `ping` has no window to close, so a polite request may not be
        // honoured; either outcome is acceptable as long as it is reported
        // truthfully and the process is left alone on refusal.
        let outcome = terminate(&launched.record, false, Duration::from_millis(500));
        assert!(
            matches!(outcome, StopOutcome::Stopped | StopOutcome::StillRunning),
            "unexpected outcome {outcome:?}"
        );

        let _ = terminate(&launched.record, true, Duration::from_secs(30));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stopping_something_that_is_not_running_reports_so() {
        let record = OwnershipRecord {
            session: "s".into(),
            executable: PathBuf::from("A:/gone.exe"),
            // A process identifier that cannot be live.
            pid: u32::MAX - 3,
            started: "never".into(),
            project: PathBuf::from("A:/p"),
            kind: ProcessKind::Editor,
        };
        assert_eq!(
            terminate(&record, true, Duration::from_millis(200)),
            StopOutcome::NotRunning
        );
    }

    #[test]
    fn a_reused_identifier_is_refused_rather_than_killed() {
        // The record claims our own identifier but a different executable and
        // start time, which is exactly what a recycled identifier looks like.
        let me = inspect(std::process::id()).unwrap();
        let record = OwnershipRecord {
            session: "s".into(),
            executable: PathBuf::from("A:/something-else.exe"),
            pid: me.pid,
            started: "an hour ago".into(),
            project: PathBuf::from("A:/p"),
            kind: ProcessKind::Editor,
        };

        let outcome = terminate(&record, true, Duration::from_millis(200));
        match outcome {
            StopOutcome::Refused(reason) => {
                assert!(
                    reason.contains("something-else") || reason.contains("different process"),
                    "the refusal should name the mismatch: {reason}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // The refusal is the point: this process is still alive.
        assert!(inspect(std::process::id()).is_some());
    }

    #[test]
    fn a_record_with_the_wrong_start_time_is_refused() {
        let me = inspect(std::process::id()).unwrap();
        let record = OwnershipRecord {
            session: "s".into(),
            executable: me.executable.clone(),
            pid: me.pid,
            started: "1999-01-01T00:00:00Z".into(),
            project: PathBuf::from("A:/p"),
            kind: ProcessKind::Editor,
        };
        match terminate(&record, true, Duration::from_millis(200)) {
            StopOutcome::Refused(reason) => {
                assert!(
                    reason.contains("reused"),
                    "expected a reuse message: {reason}"
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn stopping_a_session_removes_the_records_it_cleared() {
        let root = temp_root("session-stop");
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session = Session::create_in(&root.join("state"), &project).unwrap();

        let launched = launch(&sleeper_request(&project), &session).unwrap();
        assert_eq!(
            OwnershipRecord::read_all(&session.ownership_directory()).len(),
            1
        );

        let outcomes = stop_session(&session, true, Duration::from_secs(30));
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].1.stopped(), "{:?}", outcomes[0]);
        assert_eq!(
            OwnershipRecord::read_all(&session.ownership_directory()).len(),
            0,
            "a cleared record should not be left behind"
        );
        let _ = inspect(launched.pid());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_command_line_passes_the_project_path_without_a_shell() {
        let request = LaunchRequest::godot("A:/godot.exe", "A:/project/godot", "A:/project");
        let line = request.command_line();
        assert_eq!(line[0], "--path");
        assert_eq!(line[1], "A:/project/godot");
        // A path containing a space must survive as one argument.
        let spaced = LaunchRequest::godot("g.exe", "A:/my project/godot", "A:/my project");
        assert_eq!(spaced.command_line()[1], "A:/my project/godot");
    }

    #[test]
    fn a_launch_description_never_contains_the_environment() {
        let mut request = LaunchRequest::godot("A:/godot.exe", "A:/p", "A:/p");
        request.environment = vec![("AURUM_BRIDGE_TOKEN".into(), "supersecret".into())];
        assert!(
            !request.describe().contains("supersecret"),
            "the token must not be renderable"
        );
    }

    #[test]
    fn the_bridge_environment_carries_the_session_and_token() {
        let root = temp_root("env");
        let session = Session::create_in(&root, Path::new("A:/p")).unwrap();
        let environment = bridge_environment(&session, Some(6570));

        let lookup = |key: &str| {
            environment
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            lookup("AURUM_SESSION").as_deref(),
            Some(session.id.as_str())
        );
        assert_eq!(
            lookup("AURUM_BRIDGE_TOKEN").as_deref(),
            Some(session.token.as_str())
        );
        assert_eq!(lookup("AURUM_BRIDGE_PORT").as_deref(), Some("6570"));
        assert_eq!(lookup("AURUM_BRIDGE_VERSION").as_deref(), Some("1"));

        // With no listener there is no port to publish.
        let without = bridge_environment(&session, None);
        assert!(without.iter().all(|(k, _)| k != "AURUM_BRIDGE_PORT"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_executable_is_reported_not_adopted() {
        let root = temp_root("missing-exe");
        let session = Session::create_in(&root.join("state"), Path::new("A:/p")).unwrap();
        let request = LaunchRequest::godot(
            "definitely-not-a-real-program-xyz",
            root.clone(),
            root.clone(),
        );

        match launch(&request, &session) {
            Err(LaunchError::Spawn(_)) => {}
            other => panic!("expected a spawn failure, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stop_outcomes_describe_themselves() {
        assert!(StopOutcome::Stopped.stopped());
        assert!(StopOutcome::NotRunning.stopped());
        assert!(!StopOutcome::StillRunning.stopped());
        assert!(!StopOutcome::Refused("x".into()).stopped());

        assert!(StopOutcome::Refused("why".into())
            .describe(ProcessKind::Editor)
            .contains("why"));
        assert!(StopOutcome::StillRunning
            .describe(ProcessKind::Game)
            .contains("still running"));
    }

    #[test]
    fn existing_directory_only_accepts_directories() {
        let root = temp_root("dirs");
        assert_eq!(existing_directory(&root), Some(root.clone()));
        let file = root.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(existing_directory(&file), None);
        assert_eq!(existing_directory(&root.join("nope")), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
