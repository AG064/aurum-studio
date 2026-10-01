//! Restarting the game without restarting the editor.
//!
//! Three of the four verdicts cost no process. `NoAction` costs nothing,
//! `Reload` is Godot's own business, and an `EditorRestart` is the user's
//! decision — the design is explicit that Studio asks before taking one, and
//! never takes it while work is unsaved. The fourth, `GameplayRestart`, is the
//! one a session acts on by itself: a change that invalidates live scene
//! instances is answered by stopping the game this session launched and
//! starting it again.
//!
//! Nothing reachable from here can touch an editor, and that is the point of
//! the module. "The editor stays alive through a gameplay restart" is a
//! property of the types rather than a promise in a comment, because no
//! function below is ever given an editor process to stop.

use std::time::Duration;

use crate::ownership::{OwnershipRecord, ProcessKind};
use crate::reload::{Classification, Verdict};
use crate::session::Session;
use crate::supervise::{launch, terminate, LaunchError, LaunchRequest, StopOutcome};

/// The game a session launched, and how to start it again.
#[derive(Debug, Clone)]
pub struct Game {
    /// How it is launched. Every restart reuses this exactly, so a game that
    /// started with the bridge environment starts with it again.
    pub request: LaunchRequest,
    /// The session that owns it, and therefore the one `aurum stop` consults.
    pub session: Session,
    /// The process believed to be running, if any.
    ///
    /// A record is a claim rather than a fact: the process may have exited, and
    /// its identifier may since belong to something else entirely. Every stop
    /// re-checks the record against the live process, which is what makes a
    /// stale record harmless and a recycled identifier survivable.
    pub running: Option<OwnershipRecord>,
    /// In-memory launch witness. On-disk ownership claims cannot replace it.
    last_owned: Option<OwnershipRecord>,
}

impl Game {
    /// Supervise a game launched from `request` under `session`.
    ///
    /// The kind is set here rather than left to the caller: a game recorded as
    /// anything else would be ordered wrongly when a session is stopped, and a
    /// stop that reaches an editor before a game lets the editor relaunch one.
    pub fn new(mut request: LaunchRequest, session: Session) -> Self {
        request.kind = ProcessKind::Game;
        Self {
            request,
            session,
            running: None,
            last_owned: None,
        }
    }

    /// Launch the game, recording ownership in the session.
    pub fn start(&mut self) -> Result<u32, LaunchError> {
        let launched = launch(&self.request, &self.session)?;
        let pid = launched.pid();
        self.last_owned = Some(launched.record.clone());
        self.running = Some(launched.record);
        Ok(pid)
    }

    /// Stop the running game and start it again.
    ///
    /// `force` terminates a game that will not close when asked, which is only
    /// safe because a game holds no unsaved work; callers are expected to ask
    /// the user before setting it, exactly as `aurum stop` does.
    ///
    /// A game that survives the polite request is reported rather than replaced
    /// by force regardless: two games running over one project is a worse
    /// outcome than one stale one, so nothing is started until the old process
    /// is known to be gone.
    pub fn restart(&mut self, force: bool, timeout: Duration) -> Restart {
        let previous = match self.running.take() {
            Some(record) => record,
            None => return self.start_again(),
        };

        match terminate(&previous, force, timeout) {
            StopOutcome::Stopped => {
                let stopped = previous.pid;
                // The record describes a process that is gone, so leaving it
                // behind would have `aurum stop` report a game that no longer
                // exists.
                let _ = previous.remove(&self.session.ownership_directory());
                match self.start() {
                    Ok(started) => Restart::Restarted { stopped, started },
                    Err(error) => Restart::Failed {
                        detail: error.to_string(),
                    },
                }
            }
            // The record was stale: the game had already exited on its own, so
            // nothing was stopped and nothing was replaced. Reporting a restart
            // here would claim a stop that never happened, which is the kind of
            // small lie that makes a log untrustworthy.
            StopOutcome::NotRunning => {
                let _ = previous.remove(&self.session.ownership_directory());
                self.start_again()
            }
            // The record still describes something, and it is not ours to
            // stop. Keeping it means a later change re-checks the same claim
            // rather than silently forgetting the process.
            StopOutcome::Refused(reason) => {
                // A different creation time proves our witnessed launch exited.
                // Never signal the recycled PID. Unknown or edited identities still refuse.
                if self.last_owned.as_ref().is_some_and(|owned| {
                    crate::ownership::inspect(previous.pid)
                        .is_some_and(|live| witnessed_exit(&previous, owned, &live))
                }) {
                    let _ = previous.remove(&self.session.ownership_directory());
                    return self.start_again();
                }
                let pid = previous.pid;
                self.running = Some(previous);
                Restart::Refused { pid, reason }
            }
            StopOutcome::StillRunning => {
                let pid = previous.pid;
                self.running = Some(previous);
                Restart::StillRunning { pid }
            }
        }
    }

    fn start_again(&mut self) -> Restart {
        match self.start() {
            Ok(pid) => Restart::Started { pid },
            Err(error) => Restart::Failed {
                detail: error.to_string(),
            },
        }
    }
}

fn witnessed_exit(
    previous: &OwnershipRecord,
    owned: &OwnershipRecord,
    live: &crate::ownership::LiveProcess,
) -> bool {
    previous == owned
        && live.pid == owned.pid
        && live
            .started
            .as_ref()
            .is_some_and(|started| started != &owned.started)
}

/// What happened to the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// A game is running that was not before: either none was recorded, or the
    /// one that was had already exited.
    Started { pid: u32 },
    /// The old game closed and a new one took its place.
    Restarted { stopped: u32, started: u32 },
    /// The old game did not close when asked, so it was not replaced.
    StillRunning { pid: u32 },
    /// The recorded process is not the one this session launched, so it was
    /// left alone and nothing was started over it.
    Refused { pid: u32, reason: String },
    /// The old game closed but the replacement could not start.
    Failed { detail: String },
}

impl Restart {
    /// Whether the game is now running under an identifier that was not
    /// running before.
    pub fn restarted(&self) -> bool {
        matches!(self, Self::Started { .. } | Self::Restarted { .. })
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Started { pid } => format!("started the game (pid {pid}); none was running"),
            Self::Restarted { stopped, started } => {
                format!("game restarted: pid {stopped} stopped, pid {started} started")
            }
            Self::StillRunning { pid } => format!(
                "the game (pid {pid}) did not close when asked, so it was not replaced; \
                 close it and the next change will start a new one"
            ),
            Self::Refused { pid, reason } => {
                format!("left pid {pid} alone and started nothing: {reason}")
            }
            Self::Failed { detail } => {
                format!("the old game closed but the new one did not start: {detail}")
            }
        }
    }
}

/// What a classified change asked of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// The verdict asks nothing of any process.
    Nothing,
    /// What the supervised game did about it.
    Game(Restart),
    /// The change needs the running game restarted, but this session has none
    /// running, so there is nothing to restart.
    NoGame,
    /// Native registration changed. An editor restart is the user's decision,
    /// so the reason is reported and nothing is restarted.
    EditorRestartRequired { reason: String },
}

impl Response {
    /// Whether a process was started as a result.
    pub fn restarted(&self) -> bool {
        matches!(self, Self::Game(restart) if restart.restarted())
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Nothing => "no restart was needed".to_string(),
            Self::Game(restart) => restart.describe(),
            Self::NoGame => {
                "the change needs the running game restarted, but this session has none running"
                    .to_string()
            }
            Self::EditorRestartRequired { reason } => {
                format!("editor restart required: {reason}; nothing was restarted automatically")
            }
        }
    }
}

/// Act on a classified change.
///
/// A gameplay restart is taken; an editor restart is only ever reported, with
/// the specific reason the classifier found. A caller with no game supervised
/// gets [`Response::NoGame`] rather than silence, so a verdict is never
/// mistaken for an action.
pub fn respond(
    classification: &Classification,
    game: Option<&mut Game>,
    force: bool,
    timeout: Duration,
) -> Response {
    match classification.verdict {
        Verdict::GameplayRestart => match game {
            Some(game) => Response::Game(game.restart(force, timeout)),
            None => Response::NoGame,
        },
        Verdict::EditorRestart => Response::EditorRestartRequired {
            reason: classification.reason.clone(),
        },
        Verdict::NoAction | Verdict::Reload => Response::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::inspect;
    use crate::reload::{classify_change, classify_path};
    use std::path::{Path, PathBuf};

    /// A stand-in for a long-running program: something every Windows machine
    /// has and that does not exit on its own.
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

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-gameplay-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request(project: &Path, kind: ProcessKind) -> LaunchRequest {
        let (executable, arguments) = sleeper(300);
        LaunchRequest {
            executable,
            project: project.to_path_buf(),
            working_directory: None,
            kind,
            arguments,
            environment: Vec::new(),
        }
    }

    /// One session, an editor in it, and a supervised game — the shape the dev
    /// loop really has.
    fn scene(tag: &str) -> (PathBuf, Session, OwnershipRecord, Game) {
        let root = temp_root(tag);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session = Session::create_in(&root.join("state"), &project).unwrap();
        let editor = launch(&request(&project, ProcessKind::Editor), &session)
            .expect("the stand-in editor should launch");
        let mut game = Game::new(request(&project, ProcessKind::Worker), session.clone());
        game.start().expect("the stand-in game should launch");
        (root, session, editor.record, game)
    }

    fn cleanup(editor: &OwnershipRecord, game: &mut Game, root: &Path) {
        let _ = terminate(editor, true, Duration::from_secs(30));
        if let Some(record) = game.running.clone() {
            let _ = terminate(&record, true, Duration::from_secs(30));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_gameplay_restart_replaces_the_game_and_leaves_the_editor_alone() {
        let (root, _session, editor, mut game) = scene("gameplay");
        let first = game.running.as_ref().unwrap().pid;

        // A project settings change is the classifier's own example of a
        // change that invalidates a running game.
        let classification = classify_path(Path::new("godot/project.godot"));
        assert_eq!(classification.verdict, Verdict::GameplayRestart);

        let response = respond(
            &classification,
            Some(&mut game),
            true,
            Duration::from_secs(30),
        );
        match &response {
            Response::Game(Restart::Restarted { stopped, started }) => {
                assert_eq!(
                    *stopped, first,
                    "the game that stopped should be the old one"
                );
                assert_ne!(*started, first, "the replacement should be a new process");
            }
            other => panic!("expected the game to be replaced, got {other:?}"),
        }
        assert!(response.restarted());

        // The session recognises the replacement it recorded, so a later stop
        // reaches the new process rather than a stale record.
        let replacement = game.running.as_ref().expect("a game should be recorded");
        let replacement_live = inspect(replacement.pid).expect("the replacement should be running");
        assert!(replacement.describes(&replacement_live), "replacement identity mismatch: recorded={replacement:?}, observed={replacement_live:?}");
        assert!(inspect(first).is_none(), "the old game should be gone");

        // The editor is the point of the whole exercise: same process, still
        // described by its record.
        let live = inspect(editor.pid).expect("the editor should still be running");
        assert!(
            editor.describes(&live),
            "the editor identity changed: recorded={editor:?}, observed={live:?}"
        );

        cleanup(&editor, &mut game, &root);
    }

    #[test]
    fn an_editor_restart_verdict_restarts_nothing_and_reports_its_reason() {
        let (root, _session, editor, mut game) = scene("editor-restart");
        let first = game.running.as_ref().unwrap().pid;

        let source = "impl X { #[func] fn go(&self) {} }";
        let classification =
            classify_change(Path::new("crates/aurum-godot/src/lib.rs"), Some(source));
        assert_eq!(classification.verdict, Verdict::EditorRestart);

        let response = respond(
            &classification,
            Some(&mut game),
            true,
            Duration::from_secs(30),
        );
        match &response {
            Response::EditorRestartRequired { reason } => {
                assert!(
                    reason.contains("native registration"),
                    "the reason should be the classifier's own: {reason}"
                );
            }
            other => panic!("expected a reported editor restart, got {other:?}"),
        }
        assert!(
            response.describe().contains("nothing was restarted"),
            "the report must not pretend a restart happened: {}",
            response.describe()
        );
        assert!(!response.restarted());

        // Neither process moved.
        assert_eq!(game.running.as_ref().unwrap().pid, first);
        assert!(inspect(first).is_some(), "the game should still be running");
        let live = inspect(editor.pid).expect("the editor should still be running");
        assert!(
            editor.describes(&live),
            "editor identity changed without a restart: recorded={editor:?}, observed={live:?}"
        );

        cleanup(&editor, &mut game, &root);
    }

    #[test]
    fn a_process_this_session_did_not_launch_is_never_touched() {
        let root = temp_root("foreign");
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session = Session::create_in(&root.join("state"), &project).unwrap();

        // A record claiming this test's own process: same identifier, but a
        // different executable and start time, which is what a recycled
        // identifier looks like from the inside.
        let me = inspect(std::process::id()).unwrap();
        let mut game = Game::new(request(&project, ProcessKind::Game), session);
        game.running = Some(OwnershipRecord {
            session: game.session.id.clone(),
            executable: PathBuf::from("A:/not-this-session's-game.exe"),
            pid: me.pid,
            started: "1999-01-01T00:00:00Z".into(),
            project: project.clone(),
            kind: ProcessKind::Game,
        });

        let classification = classify_path(Path::new("godot/project.godot"));
        let response = respond(
            &classification,
            Some(&mut game),
            true,
            Duration::from_millis(500),
        );
        match &response {
            Response::Game(Restart::Refused { pid, reason }) => {
                assert_eq!(*pid, me.pid);
                assert!(
                    reason.contains("not-this-session's-game")
                        || reason.contains("different process")
                        || reason.contains("reused"),
                    "the refusal should name the mismatch: {reason}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }

        // The refusal is the point: the process is alive, and no second game
        // was started over the one that was left alone.
        assert!(inspect(std::process::id()).is_some());
        assert!(
            game.running.is_some(),
            "a refused record should be kept, not forgotten"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_polite_restart_never_runs_two_games_at_once() {
        let (root, _session, editor, mut game) = scene("polite");
        let first = game.running.as_ref().unwrap().pid;

        // `ping` has no window to close, so the request may be declined. What
        // must never happen is a replacement starting over a live game.
        let classification = classify_path(Path::new("godot/project.godot"));
        match respond(
            &classification,
            Some(&mut game),
            false,
            Duration::from_millis(500),
        ) {
            Response::Game(Restart::Restarted { stopped, started }) => {
                assert_eq!(stopped, first);
                assert!(inspect(first).is_none());
                assert_ne!(started, first);
            }
            Response::Game(Restart::StillRunning { pid }) => {
                assert_eq!(pid, first, "the surviving game should be the old one");
                assert!(inspect(first).is_some());
                assert_eq!(
                    game.running.as_ref().unwrap().pid,
                    first,
                    "the record should still describe the surviving game"
                );
            }
            other => panic!("unexpected outcome {other:?}"),
        }

        cleanup(&editor, &mut game, &root);
    }

    #[test]
    fn a_reload_verdict_asks_nothing_of_the_game() {
        let (root, _session, editor, mut game) = scene("reload");
        let first = game.running.as_ref().unwrap().pid;

        let classification = classify_path(Path::new("godot/scripts/aurum_runtime.gd"));
        let response = respond(
            &classification,
            Some(&mut game),
            true,
            Duration::from_secs(30),
        );
        assert_eq!(response, Response::Nothing);
        assert_eq!(game.running.as_ref().unwrap().pid, first);

        cleanup(&editor, &mut game, &root);
    }

    #[test]
    fn a_gameplay_restart_without_a_game_says_so_rather_than_pretending() {
        let classification = classify_path(Path::new("godot/project.godot"));
        let response = respond(&classification, None, false, Duration::from_millis(50));

        assert_eq!(response, Response::NoGame);
        assert!(!response.restarted());
        assert!(
            response.describe().contains("none running"),
            "the report should say there was nothing to restart: {}",
            response.describe()
        );
    }

    #[test]
    fn a_restart_after_the_game_already_exited_starts_a_fresh_one() {
        let (root, _session, editor, mut game) = scene("exited");
        let first_record = game.running.as_ref().unwrap().clone();
        let first = first_record.pid;
        let _ = terminate(
            game.running.as_ref().unwrap(),
            true,
            Duration::from_secs(30),
        );

        let classification = classify_path(Path::new("godot/project.godot"));
        let response = respond(
            &classification,
            Some(&mut game),
            false,
            Duration::from_secs(30),
        );
        match &response {
            Response::Game(Restart::Started { pid }) => {
                assert_eq!(*pid, game.running.as_ref().unwrap().pid);
                assert_ne!(game.running.as_ref().unwrap().started, first_record.started);
            }
            other => panic!("expected a fresh start, got {other:?}"),
        }
        assert!(
            !game
                .session
                .ownership_directory()
                .join(format!("game-{first}.json"))
                .exists()
                || game.running.as_ref().unwrap().pid == first,
            "the record of the process that exited should be cleared"
        );

        cleanup(&editor, &mut game, &root);
    }

    #[test]
    fn witnessed_pid_reuse_is_distinct_from_unknown_or_tampered_identity() {
        let owned = OwnershipRecord {
            session: "s".into(),
            executable: "game".into(),
            pid: 42,
            started: "first".into(),
            project: "project".into(),
            kind: ProcessKind::Game,
        };
        let mut live = crate::ownership::LiveProcess {
            pid: 42,
            executable: "unrelated".into(),
            started: Some("second".into()),
        };
        assert!(witnessed_exit(&owned, &owned, &live));
        live.started = None;
        assert!(!witnessed_exit(&owned, &owned, &live));
        live.started = Some("first".into());
        assert!(!witnessed_exit(&owned, &owned, &live));
        live.started = Some("second".into());
        let mut edited = owned.clone();
        edited.executable = "forged".into();
        assert!(!witnessed_exit(&edited, &owned, &live));
    }

    #[test]
    fn restarts_and_responses_describe_themselves() {
        assert!(Restart::Started { pid: 7 }.describe().contains('7'));
        assert!(Restart::Restarted {
            stopped: 1,
            started: 2
        }
        .describe()
        .contains("pid 1 stopped, pid 2 started"));
        assert!(Restart::StillRunning { pid: 3 }
            .describe()
            .contains("did not close"));
        assert!(Restart::Refused {
            pid: 4,
            reason: "not ours".into()
        }
        .describe()
        .contains("not ours"));
        assert!(Restart::Failed {
            detail: "gone".into()
        }
        .describe()
        .contains("gone"));

        assert!(Restart::Started { pid: 1 }.restarted());
        assert!(!Restart::StillRunning { pid: 1 }.restarted());

        assert!(Response::Nothing.describe().contains("no restart"));
        assert!(Response::Game(Restart::Started { pid: 9 })
            .describe()
            .contains('9'));
        assert!(Response::NoGame.describe().contains("none running"));
        assert!(Response::EditorRestartRequired {
            reason: "why".into()
        }
        .describe()
        .contains("why"));
    }
}
