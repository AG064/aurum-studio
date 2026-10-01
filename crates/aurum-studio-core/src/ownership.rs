//! Process ownership records.
//!
//! Studio launches Godot and its own helpers, and later needs to stop them.
//! The danger is a recycled process identifier: a PID recorded an hour ago can
//! belong to something entirely unrelated by the time `aurum stop` runs.
//!
//! So a record carries enough to *prove* identity — executable path, process
//! start time, and the project it was launched for — and stopping requires all
//! of them to still match. Where a field cannot be established, the answer is
//! no rather than a guess: refusing to stop is recoverable, killing the wrong
//! process is not.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// Windows uses process handles and Linux reads /proc. Only macOS shells out.
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::process::Command;

/// What a recorded process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    /// The Godot editor.
    Editor,
    /// A game launched by the editor bridge.
    Game,
    /// A long-running helper Studio supervises.
    Worker,
}

impl ProcessKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Game => "game",
            Self::Worker => "worker",
        }
    }
}

/// Everything needed to recognise a process again later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipRecord {
    /// The session that launched it.
    pub session: String,
    /// The executable that was launched.
    pub executable: PathBuf,
    pub pid: u32,
    /// The process start time, as the platform reported it.
    ///
    /// This is the field that defeats PID reuse: two processes can share an
    /// identifier, never a start time.
    pub started: String,
    /// The project it was launched against.
    pub project: PathBuf,
    pub kind: ProcessKind,
}

impl OwnershipRecord {
    /// Write the record into a session directory.
    pub fn write(&self, directory: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(directory)?;
        let path = self.path(directory);
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// Where this record lives in a session directory.
    pub fn path(&self, directory: &Path) -> PathBuf {
        directory.join(format!("{}-{}.json", self.kind.label(), self.pid))
    }

    /// Forget the record, once the process it describes is gone.
    ///
    /// A record left behind for a dead process is not dangerous — the
    /// ownership check refuses to act on it — but it makes every later report
    /// about the session mention a process that no longer exists.
    pub fn remove(&self, directory: &Path) -> std::io::Result<()> {
        std::fs::remove_file(self.path(directory))
    }

    /// Read a record back.
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Every record in a session directory.
    pub fn read_all(directory: &Path) -> Vec<Self> {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut records: Vec<Self> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .filter_map(|path| Self::read(&path).ok())
            .collect();
        // Stable order, so a caller's decisions do not depend on the
        // filesystem's ordering.
        records.sort_by_key(|record| (record.kind.label(), record.pid));
        records
    }

    /// Whether this record still describes the process at that identifier.
    ///
    /// Every field must match. A missing start time on the live process means
    /// the check cannot be completed, which is a refusal rather than a pass.
    pub fn describes(&self, live: &LiveProcess) -> bool {
        if self.pid != live.pid {
            return false;
        }
        // Paths are compared case-insensitively on Windows, where the same
        // executable is routinely spelled two ways.
        if !paths_equal(&self.executable, &live.executable) {
            return false;
        }
        match &live.started {
            Some(started) => &self.started == started,
            None => false,
        }
    }
}

/// Compare two paths the way the platform does.
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    if cfg!(windows) {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

/// What the operating system currently says about a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveProcess {
    pub pid: u32,
    pub executable: PathBuf,
    /// The start time, when the platform would tell us.
    pub started: Option<String>,
}

impl LiveProcess {
    /// Whether this describes the process the caller is running in.
    pub fn current() -> Option<Self> {
        inspect(std::process::id())
    }
}

/// How long to wait for the platform to answer a process query.
#[cfg(target_os = "macos")]
const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ask the operating system about a process.
///
/// Returns `None` when the process does not exist, cannot be described, or has
/// finished but not yet been reaped.
///
/// That last case is not a technicality. A process this session launched and
/// killed is a child of this process until somebody waits on it, and a child
/// that has not been reaped is a zombie — which `ps` reports quite happily on
/// macOS. `wait_for_exit` polls this function to decide whether a stop
/// succeeded, so a zombie counted as alive means `aurum stop` waits out its
/// whole timeout and then reports the process as still running, having already
/// killed it. Linux hid the problem: `/proc/<pid>/exe` disappears for a zombie,
/// so `inspect` answered `None` there for an unrelated reason and the tests
/// passed on two platforms out of three.
/// On Windows the query goes through CIM, which is the only dependency-free
/// way to obtain a start time; `tasklist` reports a name but not a start time,
/// and a name alone cannot defeat PID reuse.
pub fn inspect(pid: u32) -> Option<LiveProcess> {
    // Attribute-based selection, not `cfg!`: the macro evaluates to a bool at
    // compile time but still requires both branches to type-check, and each
    // branch only exists on its own platform.
    #[cfg(windows)]
    {
        inspect_windows(pid)
    }
    #[cfg(not(windows))]
    {
        inspect_unix(pid)
    }
}

#[cfg(windows)]
fn inspect_windows(pid: u32) -> Option<LiveProcess> {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn QueryFullProcessImageNameW(
            handle: *mut c_void,
            flags: u32,
            buffer: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn GetProcessTimes(
            handle: *mut c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: this is the non-null owned handle returned by OpenProcess.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    // Read-only query and synchronization rights. No termination or memory-write rights.
    // SAFETY: scalar inputs and a handle checked before use; no inherited handles.
    let raw = unsafe { OpenProcess(0x1000 | 0x0010_0000, 0, pid) };
    if raw.is_null() {
        return None;
    }
    let handle = Handle(raw);
    let mut path = vec![0u16; 32768];
    let mut size = path.len() as u32;
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: valid owned handle, bounded writable UTF-16 buffer and correctly laid-out outputs.
    unsafe {
        if QueryFullProcessImageNameW(handle.0, 0, path.as_mut_ptr(), &mut size) == 0
            || GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) == 0
            || WaitForSingleObject(handle.0, 0) != 258
        {
            return None;
        }
    }
    if size == 0 || size as usize >= path.len() {
        return None;
    }
    let ticks = ((created.high as u64) << 32) | created.low as u64;
    Some(LiveProcess {
        pid,
        executable: PathBuf::from(OsString::from_wide(&path[..size as usize])),
        started: Some(format!("windows-filetime:{ticks}")),
    })
}

/// Ask Linux about a process, through `/proc`.
#[cfg(target_os = "linux")]
fn inspect_unix(pid: u32) -> Option<LiveProcess> {
    // Field 3 of /proc/<pid>/stat is the state and field 22 is the start time
    // in clock ticks. Both follow the process name, which is the one field that
    // can contain anything, so the split is taken from the last `)`.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.rsplit_once(')')?.1;
    let mut fields = after_name.split_whitespace();
    let state = fields.next()?.chars().next()?;
    if finished(state) {
        return None;
    }
    // Read the executable after the state check: a zombie has no `exe` link,
    // and relying on that is what made this look correct for so long.
    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let started = fields.nth(18).map(str::to_string);
    Some(LiveProcess {
        pid,
        executable,
        started,
    })
}

/// Whether a `ps` state character means the process has finished.
///
/// `Z` is a zombie — finished, not yet reaped. `X` is a process being torn
/// down. Neither can be stopped or is running anything.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn finished(state: char) -> bool {
    matches!(state, 'Z' | 'X' | 'x')
}

/// Ask macOS about a process, through `ps`.
///
/// There is no `/proc` here, so the previous code — which read one — answered
/// `None` for every process. The consequence was not a missing nicety: a
/// session that cannot describe a process cannot prove it owns it, so on macOS
/// `aurum stop` would refuse to stop anything Studio had launched.
///
/// `lstart` is five fields, and `comm` is whatever follows them. Splitting on
/// the field count rather than on a separator is what keeps an executable path
/// containing spaces intact.
#[cfg(target_os = "macos")]
fn inspect_unix(pid: u32) -> Option<LiveProcess> {
    let outcome = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=,state=,comm="])
        .run(INSPECT_TIMEOUT)
        .ok()?;
    if !outcome.success() {
        return None;
    }
    let (executable, started, state) = parse_ps_line(&outcome.stdout)?;
    if finished(state) {
        return None;
    }
    Some(LiveProcess {
        pid,
        executable,
        started,
    })
}

/// Split one `ps -o lstart=,state=,comm=` line into its executable, start time and state.
///
/// Separated from the call so it can be tested everywhere. The alternative is
/// a parser that only runs on one operating system in CI, which is how the
/// `/proc` assumption survived this long.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_line(output: &str) -> Option<(PathBuf, Option<String>, char)> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut fields = line.split_whitespace();
    let start: Vec<&str> = fields.by_ref().take(5).collect();
    if start.len() < 5 {
        return None;
    }
    let state = fields.next()?.chars().next()?;
    // The path is last because it is the only field that can contain a space.
    let executable: Vec<&str> = fields.collect();
    if executable.is_empty() {
        return None;
    }
    let path = PathBuf::from(executable.join(" "));
    Some((resolve_to_a_file(&path), Some(start.join(" ")), state))
}

/// Turn whatever `ps` reported into a path that can be checked against the disk.
///
/// `ps` does not always report the resolved path: a process started as `sleep`
/// rather than `/bin/sleep` can come back as the bare name. A record holding a
/// name rather than a file cannot be verified against the filesystem, and
/// verifying every field is the whole point of the record — so a bare name is
/// looked up on `PATH`, and the result is what gets stored.
///
/// If nothing on `PATH` matches, the original comes back unchanged. A record
/// naming something unfindable is a record that will refuse to stop a process,
/// which is the safe direction to fail in.
#[cfg(any(target_os = "macos", test))]
fn resolve_to_a_file(path: &Path) -> PathBuf {
    if path.is_absolute() || path.is_file() {
        return path.to_path_buf();
    }
    let Some(name) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(search) = std::env::var_os("PATH") else {
        return path.to_path_buf();
    };
    for directory in std::env::split_paths(&search) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32, executable: &str, started: &str) -> OwnershipRecord {
        OwnershipRecord {
            session: "session-1".into(),
            executable: PathBuf::from(executable),
            pid,
            started: started.into(),
            project: PathBuf::from("A:/project"),
            kind: ProcessKind::Editor,
        }
    }

    fn live(pid: u32, executable: &str, started: Option<&str>) -> LiveProcess {
        LiveProcess {
            pid,
            executable: PathBuf::from(executable),
            started: started.map(str::to_string),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-ownership-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const EXE: &str = "A:/tools/godot.exe";

    #[test]
    fn a_record_describes_the_process_it_was_made_for() {
        let record = record(42, EXE, "2026-09-11T10:00:00Z");
        assert!(record.describes(&live(42, EXE, Some("2026-09-11T10:00:00Z"))));
    }

    #[test]
    fn a_reused_identifier_is_rejected() {
        // The whole point: same PID, different process.
        let record = record(42, EXE, "2026-09-11T10:00:00Z");
        assert!(!record.describes(&live(42, EXE, Some("2026-09-11T11:30:00Z"))));
    }

    #[test]
    fn a_different_executable_is_rejected() {
        let record = record(42, EXE, "2026-09-11T10:00:00Z");
        assert!(!record.describes(&live(
            42,
            "A:/tools/other.exe",
            Some("2026-09-11T10:00:00Z")
        )));
    }

    #[test]
    fn a_different_identifier_is_rejected() {
        let record = record(42, EXE, "2026-09-11T10:00:00Z");
        assert!(!record.describes(&live(43, EXE, Some("2026-09-11T10:00:00Z"))));
    }

    #[test]
    fn an_unknown_start_time_is_a_refusal_not_a_pass() {
        // Without a start time the check is incomplete, and an incomplete
        // check must not authorise terminating a process.
        let record = record(42, EXE, "2026-09-11T10:00:00Z");
        assert!(!record.describes(&live(42, EXE, None)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_compare_case_insensitively() {
        let record = record(42, "A:/Tools/Godot.EXE", "t");
        // The same executable is routinely spelled two ways on Windows.
        assert!(record.describes(&live(42, "a:/tools/godot.exe", Some("t"))));
    }

    #[test]
    fn records_round_trip_through_a_session_directory() {
        let dir = temp_dir("roundtrip");
        let record = record(4242, EXE, "2026-09-11T10:00:00Z");
        let path = record.write(&dir).unwrap();
        assert!(path.is_file(), "the record should exist at {path:?}");
        assert_eq!(path, record.path(&dir));

        let reloaded = OwnershipRecord::read(&path).unwrap();
        assert_eq!(reloaded, record);

        // Forgetting it is how a session stops mentioning a process it has
        // already stopped.
        record.remove(&dir).unwrap();
        assert!(!path.exists());
        assert!(
            record.remove(&dir).is_err(),
            "removing twice is not a promise"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_of_different_kinds_do_not_collide() {
        // A game and an editor can share an identifier across a restart, and
        // one must not overwrite the other's record.
        let dir = temp_dir("kinds");
        let editor = record(7, EXE, "t");
        let game = OwnershipRecord {
            kind: ProcessKind::Game,
            ..record(7, EXE, "t")
        };
        assert_ne!(editor.path(&dir), game.path(&dir));
        editor.write(&dir).unwrap();
        game.write(&dir).unwrap();
        assert_eq!(OwnershipRecord::read_all(&dir).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_record_in_a_session_is_readable_in_a_stable_order() {
        let dir = temp_dir("read-all");
        record(1, EXE, "t").write(&dir).unwrap();
        OwnershipRecord {
            kind: ProcessKind::Worker,
            pid: 9,
            ..record(9, "A:/tools/worker.exe", "t")
        }
        .write(&dir)
        .unwrap();
        OwnershipRecord {
            kind: ProcessKind::Game,
            pid: 5,
            ..record(5, EXE, "t")
        }
        .write(&dir)
        .unwrap();

        let all = OwnershipRecord::read_all(&dir);
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![ProcessKind::Editor, ProcessKind::Game, ProcessKind::Worker],
            "records should come back in a deterministic order"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_record_does_not_break_discovery() {
        let dir = temp_dir("broken");
        record(1, EXE, "t").write(&dir).unwrap();
        std::fs::write(dir.join("broken-2.json"), "{ not json").unwrap();
        std::fs::write(dir.join("ignored.txt"), "not a record").unwrap();

        // A corrupt record is skipped rather than failing the whole read.
        let all = OwnershipRecord::read_all(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(OwnershipRecord::read_all(&dir.join("missing")).len(), 0);
        assert!(OwnershipRecord::read(&dir.join("broken-2.json")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_current_process_can_be_described() {
        // Exercises the platform query for real, on the process running it.
        let me = LiveProcess::current().expect("the current process should be inspectable");
        assert_eq!(me.pid, std::process::id());
        assert!(
            me.started.is_some(),
            "a start time is required to defeat identifier reuse"
        );
        assert!(!me.executable.as_os_str().is_empty());
    }

    #[test]
    fn a_process_that_does_not_exist_is_not_invented() {
        // A PID far outside any plausible range.
        assert_eq!(inspect(u32::MAX - 3), None);
    }

    #[test]
    fn a_record_about_the_current_process_matches_it() {
        let me = LiveProcess::current().unwrap();
        let record = OwnershipRecord {
            session: "s".into(),
            executable: me.executable.clone(),
            pid: me.pid,
            started: me.started.clone().unwrap(),
            project: PathBuf::from("A:/p"),
            kind: ProcessKind::Worker,
        };
        assert!(
            record.describes(&me),
            "a record about this process should match it"
        );
    }

    #[test]
    fn kinds_have_distinct_labels() {
        assert_eq!(ProcessKind::Editor.label(), "editor");
        assert_eq!(ProcessKind::Game.label(), "game");
        assert_eq!(ProcessKind::Worker.label(), "worker");
    }

    #[test]
    fn a_bare_name_is_looked_up_on_path() {
        // The failure this exists for: a process started as `sleep` came back
        // from `ps` as `sleep`, and a record holding that cannot be checked
        // against the filesystem, so the launch that should have been recorded
        // was refused instead.
        //
        // `cargo` is used rather than a program invented for the test, because
        // the test only means something if the thing being resolved is real.
        let search: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        if let Some(found) = search.iter().map(|d| d.join("cargo")).find(|c| c.is_file()) {
            assert_eq!(resolve_to_a_file(Path::new("cargo")), found);
        }
        // An absolute path is never second-guessed.
        let absolute = PathBuf::from("/definitely/not/here");
        assert_eq!(resolve_to_a_file(&absolute), absolute);
        // And something that is nowhere comes back as it went in, so the
        // record is refused rather than pointing at the wrong file.
        assert_eq!(
            resolve_to_a_file(Path::new("no-such-program-anywhere")),
            PathBuf::from("no-such-program-anywhere")
        );
    }

    #[test]
    fn a_ps_line_splits_the_start_time_from_the_path() {
        // `ps -p <pid> -o lstart=,comm=` on macOS. Five fields of date, then
        // whatever is left is the executable.
        let (path, started, state) =
            parse_ps_line("Sat Sep 12 08:26:00 2026 S /Applications/Godot.app/x").unwrap();
        assert_eq!(path, PathBuf::from("/Applications/Godot.app/x"));
        assert_eq!(started.as_deref(), Some("Sat Sep 12 08:26:00 2026"));
        assert_eq!(state, 'S');
    }

    #[test]
    fn a_path_with_spaces_survives_the_split() {
        let (path, _, _) =
            parse_ps_line("Sat Sep 12 08:26:00 2026 S /Users/me/My Games/Godot").unwrap();
        assert_eq!(path, PathBuf::from("/Users/me/My Games/Godot"));
    }

    #[test]
    fn an_empty_or_short_ps_line_is_no_answer_rather_than_a_guess() {
        assert!(parse_ps_line("").is_none());
        // A process that has exited leaves nothing useful behind. The caller
        // turns this into "cannot prove ownership", which refuses to stop
        // rather than stopping the wrong thing.
        assert!(parse_ps_line("Sat Sep 12 08:26:00 2026").is_none());
        assert!(parse_ps_line("Sat Sep 12 08:26:00 2026 S").is_none());
        assert!(parse_ps_line("   \n  \n").is_none());
    }

    #[test]
    fn the_first_non_empty_ps_line_is_the_one_used() {
        let (path, _, _) = parse_ps_line("\n\nSat Sep 12 08:26:00 2026 S /bin/zsh\n").unwrap();
        assert_eq!(path, PathBuf::from("/bin/zsh"));
    }

    #[test]
    fn a_zombie_is_finished_rather_than_alive() {
        // The bug this pins. A process this session launched and killed stays a
        // child until it is reaped, and `ps` reports the zombie as present, so
        // `wait_for_exit` polled a process that had already died until its
        // timeout ran out and then reported the stop as having failed.
        assert!(finished('Z'));
        assert!(finished('X'));
        assert!(!finished('S'));
        assert!(!finished('R'));
    }
}
#[cfg(windows)]
#[test]
fn native_identity_is_stable_and_invalid_ids_are_refused() {
    let first = inspect(std::process::id()).unwrap();
    assert!(first
        .started
        .as_ref()
        .unwrap()
        .starts_with("windows-filetime:"));
    for _ in 0..32 {
        assert_eq!(inspect(std::process::id()).unwrap(), first);
    }
    assert!(inspect(0).is_none());
    assert!(inspect(u32::MAX).is_none());
}
