//! The supervisor: typed commands in, bounded events out.
//!
//! The design requires that "the UI thread never waits for builds, file
//! operations, or child processes", with "a supervisor worker owning
//! asynchronous work and sending bounded events". This is that worker.
//!
//! It exists ahead of any particular UI on purpose. Whatever the shell turns
//! out to be — a window, a page, a terminal — it needs the same two things: a
//! way to ask for work without blocking, and a stream of what happened. The CLI
//! and any future shell are adapters over this, not implementations of it.
//!
//! ## Why the queue is bounded, and what gets dropped
//!
//! A build produces far more log lines than a person can read, and an
//! unbounded queue would grow without limit if nobody is draining it. So the
//! queue has a capacity, and when it is full the **oldest droppable** event is
//! discarded.
//!
//! Not every event is droppable. Losing a log line costs a little history;
//! losing `BuildFinished` would leave a caller waiting forever for a result
//! that already happened, and losing `ProcessStopped` would leave a window
//! claiming something is running that is not. Events that carry state are
//! never dropped, and the consumer can ask how many were.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::build::{BuildError, BuildReport, BuildRequest, Profile};
use crate::build_queue::{BuildLock, LockError};
use crate::doctor::{Finding, Health, Report};
use crate::ownership::ProcessKind;
use crate::project::Project;
use crate::reload::Verdict;
use crate::supervise;

/// How many events are held before droppable ones start being discarded.
pub const DEFAULT_EVENT_CAPACITY: usize = 512;

/// What a caller can ask the supervisor to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Re-check the project and report health.
    Doctor,
    SelectProject(PathBuf),
    Develop {
        editor: bool,
    },
    EndDevelop,
    RestartEditor,
    /// Build and install the extension.
    Build {
        force: bool,
        release: bool,
    },
    /// Launch the editor.
    StartEditor,
    /// Launch the game.
    StartGame,
    /// Stop everything this session started.
    Stop {
        force: bool,
    },
    /// Stop the worker. Nothing after it is processed.
    Shutdown,
}

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A health report, with every finding behind the verdict.
    ///
    /// The findings travel with the verdict rather than being fetched
    /// separately, so a shell cannot end up showing a verdict whose reasons it
    /// never received. They arrive problems-first: see [`findings_worst_first`].
    Health {
        verdict: Health,
        summary: String,
        findings: Vec<Finding>,
    },
    BuildStarted {
        package: String,
        profile: &'static str,
    },
    BuildFinished {
        ok: bool,
        summary: String,
        installed_sha256: Option<String>,
    },
    ProcessStarted {
        kind: ProcessKind,
        pid: u32,
    },
    ProcessStopped {
        kind: ProcessKind,
        description: String,
    },
    /// A change was classified, with the reason a person needs to read.
    ChangeClassified {
        verdict: Verdict,
        reason: String,
    },
    Log(String),
    Error(String),
    /// The worker has stopped.
    Stopped,
}

impl Event {
    /// Whether losing this event would lose state rather than history.
    ///
    /// Log lines and change classifications are history: a caller that misses
    /// one is less informed but not stuck, and the next change re-reports the
    /// same verdict anyway. Everything else is a result or a transition.
    pub fn is_droppable(&self) -> bool {
        matches!(self, Self::Log(_) | Self::ChangeClassified { .. })
    }

    /// A short line for a log view.
    pub fn describe(&self) -> String {
        match self {
            Self::Health {
                verdict, summary, ..
            } => {
                format!("health: {} ({summary})", verdict.label())
            }
            Self::BuildStarted { package, profile } => format!("building {package} ({profile})"),
            Self::BuildFinished {
                ok,
                summary,
                installed_sha256,
            } => match (ok, installed_sha256) {
                (true, Some(hash)) => format!("build ok: {summary} [{hash}]"),
                (true, None) => format!("build ok: {summary}"),
                (false, _) => format!("build failed: {summary}"),
            },
            Self::ProcessStarted { kind, pid } => format!("{} started (pid {pid})", kind.label()),
            Self::ProcessStopped { kind, description } => {
                format!("{}: {description}", kind.label())
            }
            Self::ChangeClassified { verdict, reason } => {
                format!("{}: {reason}", verdict.label())
            }
            Self::Log(line) => line.clone(),
            Self::Error(message) => format!("error: {message}"),
            Self::Stopped => "supervisor stopped".to_string(),
        }
    }
}

/// A bounded, thread-safe event queue.
#[derive(Debug)]
struct Queue {
    events: Mutex<VecDeque<Event>>,
    /// Signalled whenever an event arrives or the queue closes, so a waiting
    /// consumer wakes instead of sleeping out its whole timeout.
    signal: Condvar,
    capacity: usize,
    dropped: Mutex<usize>,
    closed: AtomicBool,
}

impl Queue {
    fn new(capacity: usize) -> Self {
        Self {
            events: Mutex::new(VecDeque::new()),
            signal: Condvar::new(),
            // A caller passing zero gets a working queue rather than one that
            // discards everything it is given.
            capacity: capacity.max(1),
            dropped: Mutex::new(0),
            closed: AtomicBool::new(false),
        }
    }

    fn push(&self, event: Event) {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());

        if events.len() >= self.capacity {
            // Drop the oldest droppable event, wherever it sits. Dropping the
            // front outright could discard a result, leaving a caller waiting
            // for something that already happened.
            if let Some(index) = events.iter().position(Event::is_droppable) {
                events.remove(index);
                *self.dropped.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            }
            // Nothing droppable: the queue grows rather than losing a result.
            // Bounded in practice, because every non-droppable event
            // corresponds to a command the caller chose to send.
        }

        events.push_back(event);
        self.signal.notify_all();
    }

    fn pop(&self, timeout: Duration) -> Option<Event> {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        if events.is_empty() {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            let (guard, _) = self
                .signal
                .wait_timeout(events, timeout)
                .unwrap_or_else(|e| e.into_inner());
            events = guard;
        }
        events.pop_front()
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.signal.notify_all();
    }

    /// Take the dropped count, resetting it.
    fn take_dropped(&self) -> usize {
        let mut dropped = self.dropped.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *dropped)
    }
}

/// What the supervisor needs to do its job.
pub struct SupervisorConfig {
    pub project: Project,
    /// An explicit Godot path; when set it is authoritative.
    pub godot_hint: Option<PathBuf>,
    /// How long a build may take.
    pub build_timeout: Duration,
    /// How long to wait for a process to close politely.
    pub stop_timeout: Duration,
}

impl SupervisorConfig {
    /// Defaults for the two timeouts, matching the CLI.
    pub fn new(project: Project, godot_hint: Option<PathBuf>) -> Self {
        Self {
            project,
            godot_hint,
            build_timeout: Duration::from_secs(30 * 60),
            stop_timeout: Duration::from_secs(45),
        }
    }
}

/// A running supervisor worker.
pub struct Supervisor {
    commands: Sender<Command>,
    events: Arc<Queue>,
    worker: Mutex<Option<JoinHandle<()>>>,
    busy: Arc<AtomicBool>,
}

impl Supervisor {
    /// Start a worker for a project.
    pub fn start(config: SupervisorConfig) -> Self {
        Self::with_capacity(config, DEFAULT_EVENT_CAPACITY)
    }

    /// Start a worker with an explicit event capacity.
    pub fn with_capacity(config: SupervisorConfig, capacity: usize) -> Self {
        let (commands, inbox) = mpsc::channel::<Command>();
        let events = Arc::new(Queue::new(capacity));
        let worker_events = Arc::clone(&events);
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);

        let worker = std::thread::spawn(move || {
            run(config, inbox, &worker_events, &worker_busy);
            // The stop is announced before closing so a consumer waiting for
            // it wakes, sees it, and then sees the queue close.
            worker_events.push(Event::Stopped);
            worker_events.close();
        });

        Self {
            commands,
            events,
            worker: Mutex::new(Some(worker)),
            busy,
        }
    }

    /// Ask for something. Returns `false` once the worker has stopped, so a
    /// caller does not wait for an answer that will never come.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// The next event, waiting up to `timeout`.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.events.pop(timeout)
    }

    /// The next event if one is already waiting.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.pop(Duration::ZERO)
    }

    /// How many events were dropped to keep the queue bounded.
    pub fn dropped(&self) -> usize {
        self.events.take_dropped()
    }

    /// Ask the worker to stop and wait for it.
    pub fn shutdown(self) {
        self.stop_worker();
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn wait_for_shutdown(&self) {
        self.stop_worker();
    }

    fn stop_worker(&self) {
        // A send can fail because the worker already exited on its own, which
        // is not an error worth reporting: either way it is stopping.
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // Dropping without an explicit shutdown must not leave a thread
        // running against a project the caller has moved on from.
        self.stop_worker();
    }
}

/// The worker loop.
struct Busy<'a>(&'a AtomicBool);
impl<'a> Busy<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(flag)
    }
}
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn run(mut config: SupervisorConfig, inbox: Receiver<Command>, events: &Queue, busy: &AtomicBool) {
    let mut session: Option<crate::session::Session> = None;
    let mut watchers: Vec<crate::Watcher> = Vec::new();
    let mut debouncer = crate::Debouncer::new(crate::watch::DEFAULT_QUIET);

    // A closed inbox means the `Supervisor` was dropped without a shutdown,
    // which is a normal way to stop.
    //
    // Commands displaced while a burst of builds collapses are held here
    // rather than dropped, so coalescing never reorders or loses work.
    let mut held: VecDeque<Command> = VecDeque::new();

    loop {
        let command = match held.pop_front() {
            Some(command) => command,
            None => match inbox.recv_timeout(crate::watch::DEFAULT_INTERVAL) {
                Ok(command) => command,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    for watcher in &mut watchers {
                        debouncer.push(watcher.scan());
                    }
                    if let Some(batch) = debouncer.take_if_settled() {
                        let classification = watchers
                            .iter_mut()
                            .map(|watcher| watcher.classify(&batch))
                            .max_by_key(|c| c.verdict);
                        if let Some(classification) = classification {
                            events.push(Event::ChangeClassified {
                                verdict: classification.verdict,
                                reason: classification.reason,
                            });
                            if classification.verdict == Verdict::EditorRestart {
                                events.push(Event::Log("Native declarations changed. Save work and use Restart editor before installing this change.".into()));
                            } else if batch
                                .iter()
                                .any(|change| crate::reload::rebuild_required(&change.path))
                            {
                                held.push_back(Command::Build {
                                    force: false,
                                    release: false,
                                });
                            } else if classification.verdict != Verdict::NoAction {
                                let _busy = Busy::new(busy);
                                match crate::project_ops::execute(
                                    &config.project.root,
                                    &serde_json::json!({"op":"validate"}),
                                    false,
                                ) {
                                    Ok(result) => {
                                        if result["ok"] == true {
                                            events.push(Event::Log("Project validated".into()));
                                            restart_preview(&config, &session, events);
                                        } else {
                                            events.push(Event::Error(format!(
                                                "Validation failed: {result}"
                                            )));
                                        }
                                    }
                                    Err(message) => events.push(Event::Error(message)),
                                }
                            }
                        }
                    }
                    continue;
                }
            },
        };

        let _busy = Busy::new(busy);
        match command {
            Command::Shutdown => break,
            Command::SelectProject(root) => match Project::open(&root) {
                Ok(project) => {
                    watchers.clear();
                    debouncer = crate::Debouncer::new(crate::watch::DEFAULT_QUIET);
                    session = crate::session::Session::latest_for(&project.root);
                    config.project = project;
                    events.push(Event::Log(format!(
                        "Selected {}",
                        config.project.root.display()
                    )));
                }
                Err(error) => events.push(Event::Error(error.to_string())),
            },
            Command::Develop { editor } => {
                watchers.clear();
                let mut project_watcher = crate::Watcher::new(&config.project.root);
                project_watcher.scan();
                watchers.push(project_watcher);
                if let Some(engine) = config
                    .project
                    .config
                    .engine_path_hint
                    .as_ref()
                    .map(|hint| config.project.root.join(hint))
                {
                    if let Ok(engine) = engine.canonicalize().map(crate::project::clean_path) {
                        if !engine.starts_with(&config.project.root) {
                            let mut watcher = crate::Watcher::new(engine);
                            watcher.scan();
                            watchers.push(watcher);
                        }
                    }
                }
                held.push_back(Command::Build {
                    force: false,
                    release: false,
                });
                if editor {
                    held.push_back(Command::StartEditor);
                }
                events.push(Event::Log("Develop is watching project sources. Content validation and native rebuilds run automatically.".into()));
            }
            Command::EndDevelop => {
                watchers.clear();
                debouncer = crate::Debouncer::new(crate::watch::DEFAULT_QUIET);
                events.push(Event::Log("Development watch stopped".into()));
            }
            Command::RestartEditor => {
                let mut safe = true;
                if let Some(active) = &session {
                    for record in crate::OwnershipRecord::read_all(&active.ownership_directory())
                        .into_iter()
                        .filter(|r| r.kind == ProcessKind::Editor)
                    {
                        let outcome = supervise::terminate(&record, false, config.stop_timeout);
                        if !matches!(
                            outcome,
                            supervise::StopOutcome::Stopped | supervise::StopOutcome::NotRunning
                        ) {
                            safe = false;
                            events.push(Event::Error(
                                "Editor did not close. Save open work before restarting.".into(),
                            ));
                        }
                    }
                }
                if safe {
                    held.push_back(Command::Build {
                        force: false,
                        release: false,
                    });
                    held.push_back(Command::StartEditor);
                }
            }

            Command::Doctor => {
                let toolchain =
                    crate::toolchain::discover(&config.project, config.godot_hint.as_deref());
                let report = crate::doctor::diagnose(
                    &config.project,
                    &toolchain,
                    config.godot_hint.as_deref(),
                );
                events.push(Event::Health {
                    verdict: report.health(),
                    summary: format!(
                        "{} ok, {} warnings, {} blocked",
                        report.count(Health::Healthy),
                        report.count(Health::Warning),
                        report.count(Health::Blocked)
                    ),
                    findings: findings_worst_first(&report),
                });
            }

            Command::Build {
                force: mut latest_force,
                release: mut latest_release,
            } => {
                // Collapse a burst into the newest request. Pressing Build five
                // times should produce one build of the current source, not
                // five builds of states that no longer exist. Anything that is
                // not a build is set aside and runs afterwards, in order.
                let mut collapsed = 0usize;
                while let Ok(next) = inbox.try_recv() {
                    match next {
                        Command::Build { force, release } => {
                            latest_force = force;
                            latest_release = release;
                            collapsed += 1;
                        }
                        other => held.push_back(other),
                    }
                }
                if collapsed > 0 {
                    events.push(Event::Log(format!(
                        "{collapsed} earlier build request{} superseded",
                        if collapsed == 1 { " was" } else { "s were" }
                    )));
                }

                let Some(request) = build_request(&config.project, latest_release) else {
                    if config.project.config.rust_package.is_none()
                        && config.project.godot_project_dir().is_some()
                    {
                        match crate::project_ops::execute(
                            &config.project.root,
                            &serde_json::json!({"op":"validate"}),
                            false,
                        ) {
                            Ok(result) => events.push(Event::BuildFinished {
                                ok: result["ok"] == true,
                                summary: result.to_string(),
                                installed_sha256: None,
                            }),
                            Err(error) => events.push(Event::BuildFinished {
                                ok: false,
                                summary: error,
                                installed_sha256: None,
                            }),
                        }
                        continue;
                    }
                    events.push(Event::Error(
                        "the project does not say which crate builds the extension, or where \
                         the add-on lives"
                            .into(),
                    ));
                    continue;
                };

                // The lock is what actually enforces "one build writes the
                // artifact at a time". The worker thread only serializes the
                // commands inside *this* process, and a person can easily have
                // `aurum dev`, the shell, and a bare `aurum build` open at
                // once. Taken before the build starts and held until it ends;
                // a refused build reports rather than waits, so a caller is
                // never left watching nothing happen.
                let _lock = match BuildLock::try_acquire(&config.project.root) {
                    Ok(lock) => lock,
                    Err(LockError::Held { by }) => {
                        events.push(Event::BuildFinished {
                            ok: false,
                            summary: LockError::Held { by }.to_string(),
                            installed_sha256: None,
                        });
                        continue;
                    }
                    Err(LockError::Io(detail)) => {
                        events.push(Event::BuildFinished {
                            ok: false,
                            summary: detail,
                            installed_sha256: None,
                        });
                        continue;
                    }
                };

                events.push(Event::BuildStarted {
                    package: request.package.clone(),
                    profile: if request.profile.is_debug() {
                        "debug"
                    } else {
                        "release"
                    },
                });

                // A result is reported whatever happens. A caller waiting on
                // `BuildFinished` would otherwise wait forever.
                match crate::build::run(&request, latest_force, config.build_timeout) {
                    Ok(report) => {
                        push_build_success(events, &report);
                        if report.replaced {
                            restart_preview(&config, &session, events);
                        }
                    }
                    Err(error) => events.push(Event::BuildFinished {
                        ok: false,
                        summary: describe_build_error(&error),
                        installed_sha256: None,
                    }),
                }
            }

            Command::StartEditor => {
                start_process(&config, &mut session, events, ProcessKind::Editor)
            }
            Command::StartGame => start_process(&config, &mut session, events, ProcessKind::Game),

            Command::Stop { force } => match &session {
                Some(session) => {
                    for (kind, outcome) in
                        supervise::stop_session(session, force, config.stop_timeout)
                    {
                        events.push(Event::ProcessStopped {
                            kind,
                            description: outcome.describe(kind),
                        });
                    }
                }
                None => events.push(Event::Log("nothing is running".into())),
            },
        }
    }
}

fn start_process(
    config: &SupervisorConfig,
    session: &mut Option<crate::session::Session>,
    events: &Queue,
    kind: ProcessKind,
) {
    if session.is_none() {
        match crate::session::Session::create(&config.project.root) {
            Ok(created) => *session = Some(created),
            Err(error) => {
                events.push(Event::Error(format!("could not start a session: {error}")));
                return;
            }
        }
    }
    // `session` was just filled, so this cannot be `None`.
    let Some(active) = session.as_ref() else {
        return;
    };
    if let Some(record) = crate::OwnershipRecord::read_all(&active.ownership_directory())
        .into_iter()
        .find(|record| {
            record.kind == kind
                && crate::ownership::inspect(record.pid).is_some_and(|live| record.describes(&live))
        })
    {
        events.push(Event::Log(format!(
            "{} is already running (pid {})",
            kind.label(),
            record.pid
        )));
        return;
    }

    let Some(godot_project) = config.project.godot_project_dir().map(Path::to_path_buf) else {
        events.push(Event::Error(
            "no project.godot was found, so there is nothing to launch".into(),
        ));
        return;
    };
    let Some(godot) =
        crate::toolchain::discover_godot_to_launch(&config.project, config.godot_hint.as_deref())
    else {
        events.push(Event::Error(
            "Godot was not found; pass an explicit path if it is not on PATH".into(),
        ));
        return;
    };

    let mut request = supervise::LaunchRequest::godot(&godot, &godot_project, &config.project.root);
    request.kind = kind;
    if kind == ProcessKind::Editor {
        request.arguments.push("--editor".into());
    }
    request.environment = supervise::bridge_environment(active, None);

    match supervise::launch(&request, active) {
        Ok(launched) => events.push(Event::ProcessStarted {
            kind: launched.record.kind,
            pid: launched.pid(),
        }),
        Err(error) => events.push(Event::Error(error.to_string())),
    }
}

fn restart_preview(config: &SupervisorConfig, session: &Option<crate::Session>, events: &Queue) {
    let Some(session) = session else { return };
    let Some(record) = crate::OwnershipRecord::read_all(&session.ownership_directory())
        .into_iter()
        .find(|record| {
            record.kind == ProcessKind::Game
                && crate::ownership::inspect(record.pid).is_some_and(|live| record.describes(&live))
        })
    else {
        return;
    };
    let Some(root) = config.project.godot_project_dir() else {
        return;
    };
    let mut request =
        supervise::LaunchRequest::godot(&record.executable, root, &config.project.root);
    request.environment = supervise::bridge_environment(session, None);
    let mut game = crate::Game::new(request, session.clone());
    game.running = Some(record);
    let result = game.restart(false, config.stop_timeout);
    events.push(Event::Log(result.describe()));
}

fn build_request(project: &Project, release: bool) -> Option<BuildRequest> {
    let package = project.config.rust_package.clone()?;
    let addon = project.layout.addon_directory.clone()?;
    let cargo = crate::toolchain::find_on_path("cargo")?;
    let profile = if release {
        Profile::Release
    } else {
        Profile::Debug
    };
    let destination = addon
        .join("bin")
        .join(profile.installed_filename(&crate::build::library_name(&package)));
    let mut request = BuildRequest::new(
        project.build_workspace(),
        package,
        profile,
        destination,
        cargo,
    );
    request.locked = project.build_workspace().join("Cargo.lock").is_file();
    Some(request)
}

fn push_build_success(events: &Queue, report: &BuildReport) {
    events.push(Event::Log(report.summary()));
    events.push(Event::BuildFinished {
        ok: true,
        summary: report.summary(),
        installed_sha256: Some(report.installed.sha256.clone()),
    });
}

/// Every finding, problems first.
///
/// The rendered report keeps discovery order, because reading a fixed list is
/// how a person notices that a check they expected is missing. A panel that is
/// glanced at has the opposite need: whatever must be acted on should be the
/// first thing read, and healthy checks are reassurance that can wait behind
/// them. Problems come worst-first from [`Report::problems`]; the healthy ones
/// follow in the order they were checked.
fn findings_worst_first(report: &Report) -> Vec<Finding> {
    report
        .problems()
        .into_iter()
        .chain(
            report
                .findings
                .iter()
                .filter(|finding| finding.health == Health::Healthy),
        )
        .cloned()
        .collect()
}

fn describe_build_error(error: &BuildError) -> String {
    match error {
        // Cargo's own output is many lines; the first names the failure.
        BuildError::Cargo(detail) => detail
            .lines()
            .next()
            .unwrap_or("the build failed")
            .to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // -- the queue ---------------------------------------------------------

    fn log(index: usize) -> Event {
        Event::Log(format!("line {index}"))
    }

    #[test]
    fn log_lines_are_droppable_and_results_are_not() {
        assert!(Event::Log("x".into()).is_droppable());
        assert!(Event::ChangeClassified {
            verdict: Verdict::Reload,
            reason: "r".into()
        }
        .is_droppable());

        // Losing any of these would leave a caller waiting, or a view
        // claiming something that is no longer true.
        assert!(!Event::Stopped.is_droppable());
        assert!(!Event::BuildFinished {
            ok: true,
            summary: "s".into(),
            installed_sha256: None
        }
        .is_droppable());
        assert!(!Event::Error("x".into()).is_droppable());
        assert!(!Event::Health {
            verdict: Health::Healthy,
            summary: "s".into(),
            findings: Vec::new(),
        }
        .is_droppable());
        assert!(!Event::ProcessStopped {
            kind: ProcessKind::Editor,
            description: "d".into()
        }
        .is_droppable());
    }

    #[test]
    fn the_queue_holds_up_to_capacity() {
        let queue = Queue::new(4);
        for index in 0..4 {
            queue.push(log(index));
        }
        assert_eq!(queue.take_dropped(), 0);

        let mut count = 0;
        while queue.pop(Duration::ZERO).is_some() {
            count += 1;
        }
        assert_eq!(count, 4);
    }

    #[test]
    fn a_full_queue_drops_the_oldest_log_line() {
        let queue = Queue::new(3);
        for index in 0..3 {
            queue.push(log(index));
        }
        queue.push(log(99));

        assert_eq!(queue.take_dropped(), 1, "one line should have been dropped");
        let mut lines = Vec::new();
        while let Some(Event::Log(text)) = queue.pop(Duration::ZERO) {
            lines.push(text);
        }
        // The oldest goes; the newest survives.
        assert_eq!(lines, vec!["line 1", "line 2", "line 99"]);
    }

    #[test]
    fn a_result_is_never_dropped_even_when_the_queue_is_full_of_results() {
        // Every entry is a result, so there is nothing safe to discard. The
        // queue grows rather than losing one.
        let queue = Queue::new(2);
        queue.push(Event::Stopped);
        queue.push(Event::Error("one".into()));
        queue.push(Event::Error("two".into()));

        assert_eq!(queue.take_dropped(), 0);
        let mut count = 0;
        while queue.pop(Duration::ZERO).is_some() {
            count += 1;
        }
        assert_eq!(count, 3, "no result should be lost");
    }

    #[test]
    fn a_result_survives_a_flood_of_log_lines() {
        // The realistic case: a build emits hundreds of lines while one result
        // waits behind them.
        let queue = Queue::new(8);
        queue.push(Event::BuildStarted {
            package: "aurum-godot".into(),
            profile: "debug",
        });
        for index in 0..100 {
            queue.push(log(index));
        }
        queue.push(Event::BuildFinished {
            ok: true,
            summary: "done".into(),
            installed_sha256: Some("abc".into()),
        });

        let mut saw_finished = false;
        let mut saw_started = false;
        while let Some(item) = queue.pop(Duration::ZERO) {
            match item {
                Event::BuildFinished { .. } => saw_finished = true,
                Event::BuildStarted { .. } => saw_started = true,
                _ => {}
            }
        }
        assert!(saw_started, "the build start should survive");
        assert!(saw_finished, "the build result must survive");
        assert!(
            queue.take_dropped() > 0,
            "log lines should have been dropped, or the test proves nothing"
        );
    }

    #[test]
    fn order_is_preserved_for_what_survives() {
        let queue = Queue::new(64);
        let mut expected = Vec::new();
        for index in 0..5 {
            queue.push(log(index));
            expected.push(format!("line {index}"));
        }
        let mut lines = Vec::new();
        while let Some(Event::Log(text)) = queue.pop(Duration::ZERO) {
            lines.push(text);
        }
        assert_eq!(lines, expected);
    }

    #[test]
    fn popping_from_an_empty_queue_times_out() {
        let queue = Queue::new(4);
        let started = std::time::Instant::now();
        assert!(queue.pop(Duration::from_millis(80)).is_none());
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "an empty queue should wait, not spin"
        );
    }

    #[test]
    fn a_waiting_consumer_wakes_when_an_event_arrives() {
        let queue = Arc::new(Queue::new(4));
        let producer = Arc::clone(&queue);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            producer.push(Event::Stopped);
        });
        // Without the condvar this blocks for the full timeout and then
        // reports nothing, which is the bug this guards.
        assert_eq!(queue.pop(Duration::from_secs(5)), Some(Event::Stopped));
    }

    #[test]
    fn closing_the_queue_releases_a_waiting_consumer() {
        let queue = Arc::new(Queue::new(4));
        let closer = Arc::clone(&queue);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            closer.close();
        });
        let started = std::time::Instant::now();
        // `None` promptly rather than blocking for the full timeout: the
        // worker is gone, so no event can ever arrive.
        assert_eq!(queue.pop(Duration::from_secs(30)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_zero_capacity_queue_still_works() {
        let queue = Queue::new(0);
        queue.push(Event::Stopped);
        assert_eq!(queue.pop(Duration::ZERO), Some(Event::Stopped));
    }

    #[test]
    fn events_describe_themselves() {
        assert!(Event::Log("plain".into()).describe().contains("plain"));
        assert!(Event::Error("broke".into()).describe().contains("broke"));
        assert!(Event::BuildStarted {
            package: "p".into(),
            profile: "debug"
        }
        .describe()
        .contains("building p"));
        assert!(Event::BuildFinished {
            ok: false,
            summary: "the compiler said no".into(),
            installed_sha256: None
        }
        .describe()
        .contains("failed"));
        assert!(Event::ProcessStarted {
            kind: ProcessKind::Editor,
            pid: 7
        }
        .describe()
        .contains('7'));
        assert!(Event::ChangeClassified {
            verdict: Verdict::EditorRestart,
            reason: "why".into()
        }
        .describe()
        .contains("editor restart"));
    }

    // -- the worker --------------------------------------------------------

    fn unique_directory(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("aurum-supervisor-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp directory");
        path
    }

    /// A project that opens, so the worker has something real to act on.
    ///
    /// The add-on directory has to exist: discovery records it only when it is
    /// really there, so without it every build request is refused before it
    /// reaches the queue and these tests would assert nothing.
    fn temporary_project(tag: &str) -> Project {
        let root = unique_directory(tag);
        std::fs::write(
            root.join("aurum.toml"),
            "schema_version = 1\nname = \"supervisor-test\"\n\
             rust_package = \"aurum-godot\"\naddon_destination = \"godot/addons/aurum\"\n",
        )
        .expect("write aurum.toml");
        std::fs::write(root.join("project.godot"), "config_version=5\n")
            .expect("write project.godot");
        std::fs::create_dir_all(root.join("godot").join("addons").join("aurum").join("bin"))
            .expect("create the add-on directory");
        Project::open(&root).expect("the temporary project should open")
    }

    /// Collect events until one satisfies `wanted`, or the deadline passes.
    fn wait_for(supervisor: &Supervisor, wanted: impl Fn(&Event) -> bool) -> Option<Event> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            match supervisor.recv_timeout(Duration::from_millis(500)) {
                Some(event) if wanted(&event) => return Some(event),
                Some(_) => continue,
                None => continue,
            }
        }
        None
    }

    #[test]
    fn a_doctor_command_produces_a_health_event() {
        let project = temporary_project("doctor");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        assert!(supervisor.send(Command::Doctor));
        let event = wait_for(&supervisor, |e| matches!(e, Event::Health { .. }))
            .expect("doctor should report health");
        match event {
            Event::Health {
                summary, findings, ..
            } => {
                assert!(
                    summary.contains("ok"),
                    "the summary should count findings, got '{summary}'"
                );
                // The verdict is useless without its reasons, so the reasons
                // travel with it rather than being asked for again.
                assert!(
                    findings.iter().any(|finding| finding.id == "config"),
                    "every check should reach the shell, got {findings:?}"
                );
                assert_problems_come_first(&findings);
            }
            other => panic!("expected health, got {other:?}"),
        }

        supervisor.shutdown();
    }

    /// Assert that no healthy check is listed before a problem.
    fn assert_problems_come_first(findings: &[Finding]) {
        let first_healthy = findings
            .iter()
            .position(|finding| finding.health == Health::Healthy);
        let last_problem = findings
            .iter()
            .rposition(|finding| finding.health != Health::Healthy);
        if let (Some(first_healthy), Some(last_problem)) = (first_healthy, last_problem) {
            assert!(
                last_problem < first_healthy,
                "a problem below the healthy checks is one nobody reads: {findings:?}"
            );
        }
    }

    #[test]
    fn findings_are_ordered_worst_first_behind_the_verdict() {
        let finding = |id: &'static str, health: Health| Finding {
            id,
            health,
            summary: format!("{id} summary"),
            evidence: None,
            remedy: None,
        };
        let report = Report {
            project_root: PathBuf::from("/project"),
            name: "test".into(),
            // Discovery order: the healthy checks are found first, which is
            // exactly the order a panel must not use.
            findings: vec![
                finding("config", Health::Healthy),
                finding("cargo", Health::Healthy),
                finding("addon", Health::Warning),
                finding("cargo_manifest", Health::Blocked),
                finding("rustc", Health::Healthy),
            ],
        };

        let ordered: Vec<&str> = findings_worst_first(&report)
            .iter()
            .map(|finding| finding.id)
            .collect();
        assert_eq!(
            ordered,
            vec!["cargo_manifest", "addon", "config", "cargo", "rustc"],
            "the blocked check should lead, and healthy checks keep their order"
        );
    }

    #[test]
    fn a_build_without_a_package_reports_an_error_rather_than_waiting() {
        // No `rust_package` means no build can be attempted. The command must
        // still answer, because a caller is blocked on a result either way.
        let root = unique_directory("nopackage");
        std::fs::write(
            root.join("aurum.toml"),
            "schema_version = 1\nname = \"bare\"\n",
        )
        .expect("write aurum.toml");
        let project = Project::open(&root).expect("the project should open");

        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));
        assert!(supervisor.send(Command::Build {
            force: false,
            release: false
        }));

        let event = wait_for(&supervisor, |e| matches!(e, Event::Error(_)))
            .expect("an unbuildable project should report an error");
        match event {
            Event::Error(message) => assert!(
                message.contains("which crate") || message.contains("add-on"),
                "the error should say what is missing, got '{message}'"
            ),
            other => panic!("expected an error, got {other:?}"),
        }

        supervisor.shutdown();
    }

    #[test]
    fn stopping_with_nothing_running_is_not_an_error() {
        let project = temporary_project("stopempty");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        assert!(supervisor.send(Command::Stop { force: false }));
        let event = wait_for(&supervisor, |e| matches!(e, Event::Log(_)))
            .expect("stopping an idle session should say so");
        assert!(event.describe().contains("nothing is running"));

        supervisor.shutdown();
    }

    #[test]
    fn a_run_of_commands_is_answered_in_order() {
        let project = temporary_project("ordered");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        assert!(supervisor.send(Command::Doctor));
        assert!(supervisor.send(Command::Stop { force: false }));

        let mut saw_health = false;
        let mut saw_idle = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !(saw_health && saw_idle) && std::time::Instant::now() < deadline {
            match supervisor.recv_timeout(Duration::from_millis(500)) {
                Some(Event::Health { .. }) => {
                    assert!(
                        !saw_idle,
                        "health was answered after the stop that followed it"
                    );
                    saw_health = true;
                }
                Some(Event::Log(text)) if text.contains("nothing is running") => {
                    assert!(
                        saw_health,
                        "the stop was answered before the doctor before it"
                    );
                    saw_idle = true;
                }
                _ => {}
            }
        }
        assert!(saw_health && saw_idle, "both commands should be answered");

        supervisor.shutdown();
    }

    #[test]
    fn shutdown_announces_itself_and_closes_the_queue() {
        let project = temporary_project("shutdown");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        // The worker is idle, so this is processed at once.
        supervisor.shutdown();

        // The supervisor has been consumed, so assert on a fresh one that the
        // terminal event is observable to a live consumer.
        let project = temporary_project("shutdown-live");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));
        assert!(supervisor.send(Command::Shutdown));
        let event = wait_for(&supervisor, |e| matches!(e, Event::Stopped))
            .expect("shutdown should be announced");
        assert_eq!(event, Event::Stopped);
        // And the queue closes behind it, so a waiting consumer is released.
        assert!(supervisor.recv_timeout(Duration::from_secs(10)).is_none());
    }

    #[test]
    fn sending_after_shutdown_is_refused_rather_than_silently_lost() {
        let project = temporary_project("after");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        assert!(supervisor.send(Command::Shutdown));
        // Wait for the worker to actually finish.
        let _ = wait_for(&supervisor, |e| matches!(e, Event::Stopped));
        let _ = supervisor.recv_timeout(Duration::from_secs(10));

        let mut accepted = 0;
        for _ in 0..50 {
            if supervisor.send(Command::Doctor) {
                accepted += 1;
            }
            if accepted > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            accepted, 0,
            "a stopped supervisor must not accept work it cannot do"
        );
    }

    #[test]
    fn a_burst_of_build_requests_does_not_become_a_burst_of_builds() {
        // Pressing Build ten times must not queue ten builds of states that no
        // longer exist. This is the integration half of `BuildQueue`'s unit
        // tests: the property asserted is the one that matters — fewer builds
        // ran than were asked for — rather than an exact count, because how
        // many requests are in the channel at the moment the worker drains is
        // genuinely a race, and pinning it would make the test flaky rather
        // than strict.
        let project = temporary_project("coalesce");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        let sent = 10;
        for _ in 0..sent {
            assert!(supervisor.send(Command::Build {
                force: false,
                release: false
            }));
        }

        // Collect until the supervisor goes quiet.
        let mut started = 0;
        let mut finished = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut quiet_since = std::time::Instant::now();
        while std::time::Instant::now() < deadline {
            match supervisor.recv_timeout(Duration::from_millis(250)) {
                Some(Event::BuildStarted { .. }) => {
                    started += 1;
                    quiet_since = std::time::Instant::now();
                }
                Some(Event::BuildFinished { .. }) => {
                    finished += 1;
                    quiet_since = std::time::Instant::now();
                }
                Some(_) => {}
                // A running compiler can be quiet while it still owes a result.
                // Only treat silence as idle after every started build finished.
                None if started == finished && quiet_since.elapsed() > Duration::from_secs(3) => {
                    break
                }
                None => {}
            }
        }

        assert!(started >= 1, "at least one build should have run");
        assert!(
            started < sent,
            "coalescing should mean fewer builds ({started}) than requests ({sent})"
        );
        assert_eq!(
            started, finished,
            "every build that started must report a result; a caller waiting on \
             one would otherwise wait forever"
        );

        supervisor.shutdown();
    }

    #[test]
    fn a_build_refuses_rather_than_racing_another_process() {
        // The cross-process guarantee, seen from the supervisor. Held here with
        // the same operating-system mechanism a second Studio would use, and
        // verified for real against a second process before this test existed.
        let project = temporary_project("locked");
        let _held = crate::build_queue::BuildLock::try_acquire(&project.root)
            .expect("the test should be able to take the lock");

        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));
        assert!(supervisor.send(Command::Build {
            force: false,
            release: false
        }));

        let event = wait_for(&supervisor, |e| matches!(e, Event::BuildFinished { .. }))
            .expect("a refused build must still report a result");
        match event {
            Event::BuildFinished {
                ok,
                summary,
                installed_sha256,
            } => {
                assert!(!ok, "the build should not have been attempted");
                assert!(
                    summary.contains("already building"),
                    "the refusal should say why, got '{summary}'"
                );
                assert!(installed_sha256.is_none());
            }
            other => panic!("expected a build result, got {other:?}"),
        }

        // And no build was started, which is the point of refusing. Checked
        // over a short fixed window rather than with `wait_for`, which would
        // sit out its whole deadline waiting for an event that is correctly
        // never coming.
        let mut started = false;
        let settle = std::time::Instant::now() + Duration::from_millis(1500);
        while std::time::Instant::now() < settle {
            if matches!(
                supervisor.recv_timeout(Duration::from_millis(200)),
                Some(Event::BuildStarted { .. })
            ) {
                started = true;
                break;
            }
        }
        assert!(!started, "a refused build must not have started cargo");

        supervisor.shutdown();
    }

    #[test]
    fn a_command_sent_during_a_build_burst_is_not_lost() {
        // Coalescing sets aside anything that is not a build. It must come
        // back, and in order, rather than being swallowed by the collapse.
        let project = temporary_project("burst");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));

        assert!(supervisor.send(Command::Build {
            force: false,
            release: false
        }));
        assert!(supervisor.send(Command::Doctor));
        assert!(supervisor.send(Command::Build {
            force: false,
            release: false
        }));

        let health = wait_for(&supervisor, |e| matches!(e, Event::Health { .. }));
        assert!(
            health.is_some(),
            "the doctor between two builds must still be answered"
        );

        supervisor.shutdown();
    }

    #[test]
    fn dropping_a_supervisor_stops_its_worker() {
        // Without the `Drop` impl this test would finish with a thread still
        // running against a deleted project directory.
        let project = temporary_project("dropped");
        let supervisor = Supervisor::start(SupervisorConfig::new(project, None));
        assert!(supervisor.send(Command::Doctor));
        drop(supervisor);
    }

    #[test]
    fn the_dropped_count_is_reported_once() {
        let queue = Queue::new(2);
        for index in 0..10 {
            queue.push(log(index));
        }
        let first = queue.take_dropped();
        assert!(first > 0);
        assert_eq!(queue.take_dropped(), 0, "the count should reset when read");
    }
}
