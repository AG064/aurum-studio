//! Finding the tools Studio drives: Cargo, rustc, and Godot.
//!
//! Godot is the workhorse, but Studio still has to *find* it. Discovery is
//! deliberately layered — an explicit hint wins, then `PATH`, then the
//! conventional layout where a checkout sits beside the engine — and every
//! layer is a pure filesystem check, so it is testable without the tools
//! installed.

use std::path::{Path, PathBuf};

use crate::process::{Command, PROBE_TIMEOUT};
use crate::project::Project;

/// A tool Studio found, with its version when it could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub path: PathBuf,
    pub version: Option<String>,
}

impl Tool {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            version: None,
        }
    }

    /// The version with a `v` prefix and build metadata trimmed.
    pub fn short_version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

/// Everything Studio needs, each entry present only when it was found.
///
/// Absence is information, not an error: `doctor` reports it, and every
/// command decides for itself what it actually requires.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Toolchain {
    pub cargo: Option<Tool>,
    pub rustc: Option<Tool>,
    pub godot: Option<Tool>,
}

impl Toolchain {
    /// Whether a build can run.
    pub fn can_build(&self) -> bool {
        self.cargo.is_some()
    }

    /// Whether an editor can be launched.
    pub fn can_launch_godot(&self) -> bool {
        self.godot.is_some()
    }
}

/// Find an executable on `PATH`, honouring Windows executable extensions.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    for directory in std::env::split_paths(&path) {
        // Try the bare name first, then each executable extension.
        let direct = directory.join(name);
        if is_executable(&direct) {
            return Some(direct);
        }
        for extension in &extensions {
            if extension.is_empty() {
                continue;
            }
            let candidate = directory.join(format!("{name}{extension}"));
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The Godot executable names Studio will accept.
const GODOT_NAMES: &[&str] = &["godot", "godot4", "Godot"];

/// Which build of Godot to prefer when several are present.
///
/// Godot ships a windowed build and a console build with identical engines.
/// Which one is right depends on what it is for, and the difference is not
/// cosmetic: a console process has no window, so `taskkill` cannot ask it to
/// close and must terminate it forcefully. Launching an editor with the
/// console build therefore means every stop discards unsaved work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GodotFlavor {
    /// Preferred for probing, where captured stdout makes the version legible.
    PreferConsole,
    /// Preferred for launching, where a window is what allows a polite close.
    PreferWindowed,
}

/// Find Godot, in order of how much the caller knows.
///
/// 1. An explicit path, which is authoritative.
/// 2. `PATH`.
/// 3. A `godot/` directory beside the project, which is the layout an engine
///    checkout uses when it keeps a pinned editor next to itself.
/// 4. A `godot/` directory inside the project.
pub fn discover_godot(
    hint: Option<&Path>,
    project_root: &Path,
    flavor: GodotFlavor,
) -> Option<PathBuf> {
    if let Some(hint) = hint {
        if hint.is_file() {
            return Some(hint.to_path_buf());
        }
        // A hint naming a directory means "look inside here".
        if hint.is_dir() {
            return find_godot_in(hint, flavor);
        }
        // An explicit path that does not resolve is an error, not a request to
        // guess. Falling through here meant a typo silently launched whatever
        // Godot happened to be on PATH instead of the one that was asked for.
        return None;
    }

    if let Some(path) = std::env::var_os("AURUM_GODOT").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
        return None;
    }
    if let Some(home) = std::env::var_os("AURUM_STUDIO_HOME") {
        if let Some(found) = find_godot_in(&PathBuf::from(home).join("runtime"), flavor) {
            return Some(found);
        }
    }

    for name in GODOT_NAMES {
        if let Some(found) = find_on_path(name) {
            return Some(found);
        }
    }

    discover_project_godot(project_root, flavor)
}

fn discover_project_godot(project_root: &Path, flavor: GodotFlavor) -> Option<PathBuf> {
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(parent) = project_root.parent() {
        directories.push(parent.join("godot"));
    }
    directories.push(project_root.join("godot"));

    for directory in directories {
        if let Some(found) = find_godot_in(&directory, flavor) {
            return Some(found);
        }
    }
    None
}

/// Find a Godot executable in one directory.
pub fn find_godot_in(directory: &Path, flavor: GodotFlavor) -> Option<PathBuf> {
    let entries = std::fs::read_dir(directory).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    let lower = n.to_ascii_lowercase();
                    (lower.starts_with("godot") || lower.starts_with("godot_v"))
                        && (lower.ends_with(".exe") || !cfg!(windows))
                })
        })
        .collect();

    // Prefer a console build, then the shortest name, then alphabetical, so
    // the choice is deterministic rather than directory-order dependent.
    candidates.sort_by_key(|path| {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let is_console = name.contains("console");
        let flavor_rank = match flavor {
            GodotFlavor::PreferConsole => usize::from(!is_console),
            GodotFlavor::PreferWindowed => usize::from(is_console),
        };
        (flavor_rank, name.len(), name)
    });
    candidates.into_iter().next()
}

/// Read a tool's version by running it.
pub fn probe_version(program: &Path, version_argument: &str) -> Option<String> {
    let outcome = Command::new(program)
        .arg(version_argument)
        .run(PROBE_TIMEOUT)
        .ok()?;
    if !outcome.success() {
        return None;
    }
    parse_version(&outcome.stdout).or_else(|| parse_version(&outcome.stderr))
}

/// Pull a version out of a `--version` line.
///
/// Handles the three shapes the tools actually emit:
/// `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`, `rustc 1.95.0 (...)`, and
/// `4.7.stable.official.5b4e0cb0f`.
pub fn parse_version(output: &str) -> Option<String> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut parts = line.split_whitespace();

    let first = parts.next()?;
    // "cargo 1.95.0 (...)" — the version is the second token.
    if let Some(second) = parts.next() {
        if second.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Some(second.to_string());
        }
    }
    // "4.7.stable.official.5b4e0cb0f" — the whole token is the version.
    if first.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Some(first.split('.').take(2).collect::<Vec<_>>().join("."));
    }
    None
}

/// Whether a reported version satisfies a required major.minor.
pub fn version_satisfies(reported: Option<&str>, required: &str) -> bool {
    let Some(reported) = reported else {
        return false;
    };
    let mut reported_parts = reported.split('.');
    let mut required_parts = required.split('.');
    match (reported_parts.next(), required_parts.next()) {
        (Some(a), Some(b)) => {
            if a != b {
                return false;
            }
        }
        _ => return false,
    }
    match (reported_parts.next(), required_parts.next()) {
        // Only a major version was required, and it matched.
        (_, None) => true,
        (Some(a), Some(b)) => a == b,
        (None, Some(_)) => false,
    }
}

/// Find the Godot to launch, preferring the build that can be asked to close.
///
/// Separate from [`discover`] because the two want opposite things: probing
/// reads stdout, launching needs a window so a stop can be polite.
pub fn discover_godot_to_launch(project: &Project, hint: Option<&Path>) -> Option<PathBuf> {
    discover_godot(hint, &project.root, GodotFlavor::PreferWindowed)
}

/// Discover the whole toolchain for a project.
pub fn discover(project: &Project, godot_hint: Option<&Path>) -> Toolchain {
    let mut toolchain = Toolchain::default();

    if let Some(path) = find_on_path("cargo") {
        let mut tool = Tool::new(&path);
        tool.version = probe_version(&path, "--version");
        toolchain.cargo = Some(tool);
    }
    if let Some(path) = find_on_path("rustc") {
        let mut tool = Tool::new(&path);
        tool.version = probe_version(&path, "--version");
        toolchain.rustc = Some(tool);
    }
    if let Some(path) = discover_godot(godot_hint, &project.root, GodotFlavor::PreferConsole) {
        let mut tool = Tool::new(&path);
        tool.version = probe_version(&path, "--version");
        toolchain.godot = Some(tool);
    }

    toolchain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aurum-toolchain-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_a_cargo_version_line() {
        assert_eq!(
            parse_version("cargo 1.95.0 (f2d3ce0bd 2026-03-21)").as_deref(),
            Some("1.95.0")
        );
    }

    #[test]
    fn parses_a_rustc_version_line() {
        assert_eq!(
            parse_version("rustc 1.95.0 (59807616e 2026-04-14)").as_deref(),
            Some("1.95.0")
        );
    }

    #[test]
    fn parses_a_godot_version_line() {
        // Godot prints a long version with no tool name in front of it.
        assert_eq!(
            parse_version("4.7.stable.official.5b4e0cb0f").as_deref(),
            Some("4.7")
        );
    }

    #[test]
    fn version_parsing_handles_noise_and_emptiness() {
        assert_eq!(parse_version("").as_deref(), None);
        assert_eq!(parse_version("\n\n   \n").as_deref(), None);
        // Leading blank lines are skipped.
        assert_eq!(
            parse_version("\n  cargo 1.80.0 (x)\n").as_deref(),
            Some("1.80.0")
        );
        assert_eq!(parse_version("no version here").as_deref(), None);
    }

    #[test]
    fn version_satisfaction_compares_major_and_minor() {
        assert!(version_satisfies(Some("4.7"), "4.7"));
        assert!(version_satisfies(Some("4.7.1"), "4.7"));
        assert!(version_satisfies(Some("4.7.stable.official"), "4.7"));
        // A major-only requirement accepts any minor.
        assert!(version_satisfies(Some("4.9"), "4"));

        assert!(!version_satisfies(Some("4.6"), "4.7"));
        assert!(!version_satisfies(Some("5.0"), "4.7"));
        assert!(!version_satisfies(Some("4"), "4.7"), "too vague to accept");
        assert!(!version_satisfies(None, "4.7"), "unknown is not satisfied");
    }

    #[test]
    fn godot_discovery_prefers_a_console_build() {
        let directory = temp_dir("godot-pick");
        std::fs::write(directory.join("Godot_v4.7-stable_win64.exe"), b"stub").unwrap();
        std::fs::write(
            directory.join("Godot_v4.7-stable_win64_console.exe"),
            b"stub",
        )
        .unwrap();
        std::fs::write(directory.join("readme.txt"), b"not a godot").unwrap();

        let found = find_godot_in(&directory, GodotFlavor::PreferConsole).unwrap();
        assert!(
            found
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("console"),
            "expected the console build, got {found:?}"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_windowed_build_is_preferred_for_launching() {
        // The distinction matters: only a windowed build can be asked to
        // close, so launching the console build would make every stop
        // forceful.
        let directory = temp_dir("godot-flavor");
        std::fs::write(directory.join("Godot_v4.7-stable_win64.exe"), b"stub").unwrap();
        std::fs::write(
            directory.join("Godot_v4.7-stable_win64_console.exe"),
            b"stub",
        )
        .unwrap();

        let probing = find_godot_in(&directory, GodotFlavor::PreferConsole).unwrap();
        assert!(probing.to_string_lossy().contains("console"));

        let launching = find_godot_in(&directory, GodotFlavor::PreferWindowed).unwrap();
        assert!(
            !launching.to_string_lossy().contains("console"),
            "got {launching:?}"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn godot_discovery_returns_none_for_a_directory_without_godot() {
        let directory = temp_dir("godot-none");
        std::fs::write(directory.join("something.dll"), b"stub").unwrap();
        assert_eq!(find_godot_in(&directory, GodotFlavor::PreferConsole), None);
        assert_eq!(
            find_godot_in(&directory.join("missing"), GodotFlavor::PreferConsole),
            None
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_explicit_godot_hint_wins_over_everything() {
        let directory = temp_dir("hint");
        // Named like Godot on purpose: discovery refuses arbitrary executables
        // in a directory, so a stub called "MyGodot.exe" is correctly ignored.
        let hinted = directory.join("Godot_v4.7-custom_console.exe");
        std::fs::write(&hinted, b"stub").unwrap();

        let project_root = temp_dir("hint-project");
        let found = discover_godot(Some(&hinted), &project_root, GodotFlavor::PreferConsole);
        assert_eq!(found, Some(hinted.clone()));

        // A directory hint searches inside it.
        let found = discover_godot(Some(&directory), &project_root, GodotFlavor::PreferConsole);
        assert_eq!(found, Some(hinted));

        let _ = std::fs::remove_dir_all(&directory);
        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn godot_is_found_beside_the_project() {
        let parent = temp_dir("sibling");
        let project_root = parent.join("aurum-engine");
        let godot_dir = parent.join("godot");
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&godot_dir).unwrap();
        std::fs::write(
            godot_dir.join("Godot_v4.7-stable_win64_console.exe"),
            b"stub",
        )
        .unwrap();

        // Test the fallback independently of the caller's configured runtime
        // and PATH. An installed Aurum must not make this fixture nondeterministic.
        let found = discover_project_godot(&project_root, GodotFlavor::PreferConsole);
        assert!(
            found.as_ref().is_some_and(|p| p.starts_with(&godot_dir)),
            "expected a sibling discovery, got {found:?}"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn an_explicit_hint_that_does_not_exist_is_not_quietly_ignored() {
        let project_root = temp_dir("bad-hint");
        let missing = project_root.join("nope").join("godot.exe");
        // The caller named a Godot. Returning some other one would mean a typo
        // silently launches a different engine.
        assert_eq!(
            discover_godot(Some(&missing), &project_root, GodotFlavor::PreferConsole),
            None
        );
        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn a_directory_hint_without_godot_does_not_fall_through_either() {
        let project_root = temp_dir("empty-hint-dir");
        let empty = project_root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            discover_godot(Some(&empty), &project_root, GodotFlavor::PreferConsole),
            None
        );
        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn path_lookup_finds_a_real_executable() {
        // One of these exists on every supported machine.
        let found = find_on_path("cmd")
            .or_else(|| find_on_path("sh"))
            .or_else(|| find_on_path("cargo"));
        assert!(found.is_some(), "no familiar executable found on PATH");
        assert!(found.unwrap().is_file());
    }

    #[test]
    fn path_lookup_returns_none_for_nonsense() {
        assert_eq!(find_on_path("definitely-not-a-real-tool-xyz"), None);
    }

    #[test]
    fn probing_a_real_tool_reads_its_version() {
        // Cargo is present in any environment that can build this crate.
        let cargo = find_on_path("cargo").expect("cargo should be on PATH");
        let version = probe_version(&cargo, "--version");
        assert!(
            version.as_deref().is_some_and(|v| v.starts_with("1.")),
            "unexpected cargo version: {version:?}"
        );
    }

    #[test]
    fn probing_a_broken_tool_yields_none_rather_than_failing() {
        let directory = temp_dir("probe-broken");
        let script = directory.join("broken.cmd");
        // Exits non-zero, so there is no version to trust.
        std::fs::write(&script, "@echo off\r\nexit /b 1\r\n").unwrap();
        assert_eq!(probe_version(&script, "--version"), None);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn toolchain_capabilities_reflect_what_was_found() {
        let empty = Toolchain::default();
        assert!(!empty.can_build());
        assert!(!empty.can_launch_godot());

        let partial = Toolchain {
            cargo: Some(Tool::new("cargo")),
            ..Default::default()
        };
        assert!(partial.can_build());
        assert!(!partial.can_launch_godot());
    }
}
