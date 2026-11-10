//! The in-app update flow, as the page sees it.
//!
//! A check and an install each run on a thread of their own, and the page polls
//! [`Updates::snapshot`] for progress. Nothing here starts by itself: every
//! action is a request from the person using Studio.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aurum_studio_core::update::{self, Applied, Feed, Release, Stage};
use serde_json::{json, Value};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Extracting,
    Verifying,
    Installing,
    Installed,
    Failed,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Checking => "checking",
            Self::Current => "current",
            Self::Available => "available",
            Self::Downloading => "downloading",
            Self::Extracting => "extracting",
            Self::Verifying => "verifying",
            Self::Installing => "installing",
            Self::Installed => "installed",
            Self::Failed => "failed",
        }
    }

    fn working(self) -> bool {
        matches!(
            self,
            Self::Checking
                | Self::Downloading
                | Self::Extracting
                | Self::Verifying
                | Self::Installing
        )
    }
}

struct State {
    phase: Phase,
    release: Option<Release>,
    error: Option<String>,
    applied: Option<Applied>,
}

/// The update state of one running Studio.
pub(crate) struct Updates {
    state: Arc<Mutex<State>>,
    /// The installed folder, or why this Studio is not an installed copy.
    home: Result<PathBuf, String>,
}

impl Updates {
    pub(crate) fn new() -> Self {
        let home = std::env::current_exe()
            .map_err(|error| format!("could not locate Studio: {error}"))
            .and_then(|path| update::installation(&path));
        Self {
            state: Arc::new(Mutex::new(State {
                phase: Phase::Idle,
                release: None,
                error: None,
                applied: None,
            })),
            home,
        }
    }

    fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
        state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Whether a check or an install is running, which stops Studio closing.
    pub(crate) fn busy(&self) -> bool {
        Self::lock(&self.state).phase.working()
    }

    fn work_directory(&self) -> PathBuf {
        match &self.home {
            Ok(home) => home.join("updates"),
            Err(_) => std::env::temp_dir().join(format!("aurum-update-{}", std::process::id())),
        }
    }

    /// Everything the page shows.
    pub(crate) fn snapshot(&self) -> Value {
        let state = Self::lock(&self.state);
        let release = state.release.as_ref();
        let downloaded = match (state.phase, release) {
            (Phase::Downloading, Some(release)) => {
                std::fs::metadata(update::partial_archive(&self.work_directory(), release))
                    .map(|meta| meta.len())
                    .unwrap_or(0)
            }
            _ => 0,
        };
        json!({
            "ok": true,
            "current": VERSION,
            "phase": state.phase.name(),
            "error": state.error,
            "release": release.map(|release| json!({
                "tag": release.tag,
                "version": release.version,
                "title": release.title,
                "notes": release.notes,
                "page": release.page,
                "archive": release.archive,
                "archive_bytes": release.archive_bytes,
            })),
            "downloaded_bytes": downloaded,
            "installed": state.applied.as_ref().map(|applied| json!({
                "from": applied.from,
                "to": applied.to,
                "replaced": applied.replaced,
                "unchanged": applied.unchanged,
                "backups": applied.backups.display().to_string(),
            })),
            "platform_can_update": update::can_apply(),
            "installed_copy": self.home.is_ok(),
            "not_installed_reason": self.home.as_ref().err(),
            "source": match Feed::from_environment() {
                Ok(Feed::GitHub) | Err(_) => "github",
                Ok(Feed::Loopback(_)) => "loopback",
            },
        })
    }

    /// Look for a newer release.
    pub(crate) fn check(&self) -> Result<(), String> {
        {
            let mut state = Self::lock(&self.state);
            if state.phase.working() {
                return Err("An update step is already running".into());
            }
            state.phase = Phase::Checking;
            state.error = None;
            state.release = None;
            state.applied = None;
        }
        let state = Arc::clone(&self.state);
        let scratch = self.work_directory().join("check");
        std::thread::spawn(move || {
            let outcome = Feed::from_environment()
                .and_then(|feed| update::check_online(VERSION, &feed, &scratch));
            let _ = std::fs::remove_dir_all(&scratch);
            let mut state = Self::lock(&state);
            match outcome {
                Ok(Some(release)) => {
                    state.release = Some(release);
                    state.phase = Phase::Available;
                }
                Ok(None) => state.phase = Phase::Current,
                Err(error) => {
                    state.error = Some(error);
                    state.phase = Phase::Failed;
                }
            }
        });
        Ok(())
    }

    /// Download, check and install the release the last check found.
    pub(crate) fn install(&self) -> Result<(), String> {
        let home = self.home.clone()?;
        if !update::can_apply() {
            return Err("Updating from inside Studio is only available on Windows".into());
        }
        let release = {
            let mut state = Self::lock(&self.state);
            if state.phase != Phase::Available {
                return Err("Check for updates first".into());
            }
            let release = state.release.clone().ok_or("Check for updates first")?;
            state.phase = Phase::Downloading;
            state.error = None;
            release
        };
        let state = Arc::clone(&self.state);
        let work = self.work_directory();
        std::thread::spawn(move || {
            let outcome = (|| {
                let feed = Feed::from_environment()?;
                let staged = update::prepare(&release, VERSION, &feed, &work, &mut |stage| {
                    Self::lock(&state).phase = match stage {
                        Stage::Checksums | Stage::Download => Phase::Downloading,
                        Stage::Extract => Phase::Extracting,
                        Stage::Probe => Phase::Verifying,
                    };
                })?;
                Self::lock(&state).phase = Phase::Installing;
                update::apply(&home, &staged, VERSION)
            })();
            let _ = std::fs::remove_dir_all(&work);
            let mut state = Self::lock(&state);
            match outcome {
                Ok(applied) => {
                    state.applied = Some(applied);
                    state.phase = Phase::Installed;
                }
                Err(error) => {
                    state.error = Some(error);
                    state.phase = Phase::Failed;
                }
            }
        });
        Ok(())
    }

    /// Schedule the installed Studio to start once this one has stopped.
    pub(crate) fn schedule_restart(&self) -> Result<(), String> {
        let home = self.home.clone()?;
        if Self::lock(&self.state).phase != Phase::Installed {
            return Err("There is no installed update to restart into".into());
        }
        update::schedule_restart(&home)
            .map_err(|error| format!("could not schedule the restart: {error}"))
    }
}
