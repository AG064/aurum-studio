//! Building the native extension and installing it safely.
//!
//! The guarantee this module exists to provide is narrow and absolute: **a
//! failed build never damages the working extension.** Everything is ordered
//! around that. Cargo runs, the artifact is waited on until it stops changing,
//! it is hashed and staged *inside the destination directory*, and only then
//! is the installed library swapped. Any failure before the swap leaves the
//! destination byte-identical to what it was.
//!
//! Installed sources are never touched, and Godot is never terminated to win a
//! file lock. A locked library is a bounded, reported failure with a remedy,
//! because killing the editor to force a copy would discard the user's work to
//! save a retry.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::hash::sha256_file;
use crate::process::{Command, Outcome};

/// Which Cargo profile a build uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Debug,
    Release,
}

impl Profile {
    /// The flag to pass Cargo, if any.
    pub fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Self::Debug => None,
            Self::Release => Some("--release"),
        }
    }

    /// The directory Cargo writes into.
    pub fn directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    pub fn is_debug(self) -> bool {
        matches!(self, Self::Debug)
    }

    /// The installed filename for this profile.
    ///
    /// Debug and release use distinct names so a development build can never
    /// silently overwrite a packaged release library.
    pub fn installed_filename(self, library: &str) -> String {
        if self.is_debug() {
            format!("{library}.debug.{}", dynamic_library_extension())
        } else {
            format!("{library}.{}", dynamic_library_extension())
        }
    }
}

/// The platform's dynamic library extension, without a dot.
pub fn dynamic_library_extension() -> &'static str {
    if cfg!(windows) {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// A Cargo package name as its artifact is named on disk.
pub fn library_name(package: &str) -> String {
    package.replace('-', "_")
}

/// What to build and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequest {
    /// The Cargo workspace root.
    pub workspace: PathBuf,
    /// The package that produces the extension.
    pub package: String,
    pub profile: Profile,
    /// The library file to install, e.g. `addons/aurum/bin/aurum_godot.debug.dll`.
    pub destination: PathBuf,
    /// The Cargo executable, so tests can substitute a fake.
    pub cargo: PathBuf,
    /// Whether to pass `--locked`, keeping dependency state pinned.
    pub locked: bool,
}

impl BuildRequest {
    pub fn new(
        workspace: impl Into<PathBuf>,
        package: impl Into<String>,
        profile: Profile,
        destination: impl Into<PathBuf>,
        cargo: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            package: package.into(),
            profile,
            destination: destination.into(),
            cargo: cargo.into(),
            locked: true,
        }
    }

    /// The library Cargo produces.
    pub fn source_artifact(&self) -> PathBuf {
        self.workspace
            .join("target")
            .join(self.profile.directory())
            .join(format!(
                "{}.{}",
                library_name(&self.package),
                dynamic_library_extension()
            ))
    }

    /// The Cargo command this request runs.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.cargo)
            .arg("build")
            .arg("-p")
            .arg(&self.package)
            .directory(&self.workspace);
        if let Some(flag) = self.profile.cargo_flag() {
            command = command.arg(flag);
        }
        if self.locked {
            command = command.arg("--locked");
        }
        // Cargo's colours and progress bars make captured output harder to
        // read and harder to test.
        command = command
            .arg("--color")
            .arg("never")
            .arg("--message-format=json-render-diagnostics");
        command
    }
}

/// A file that was produced or installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

/// Why a build did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// Cargo itself failed; the text is the compiler's own output.
    Cargo(String),
    /// Cargo succeeded but produced no artifact.
    NoArtifact(PathBuf),
    /// The artifact never stopped changing within the deadline.
    Unstable {
        path: PathBuf,
        seconds: u64,
    },
    /// The installed library could not be replaced.
    Install(String),
    Io(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cargo(detail) => write!(f, "build failed:\n{detail}"),
            Self::NoArtifact(path) => {
                write!(
                    f,
                    "cargo succeeded but produced no artifact at '{}'",
                    path.display()
                )
            }
            Self::Unstable { path, seconds } => write!(
                f,
                "'{}' was still being written after {seconds}s",
                path.display()
            ),
            Self::Install(detail) => write!(f, "could not install the extension: {detail}"),
            Self::Io(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// What a build did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    /// The artifact Cargo produced.
    pub source: Artifact,
    /// The installed library after the build.
    pub installed: Artifact,
    /// Whether the destination actually changed.
    pub replaced: bool,
    /// Whether Cargo ran at all, as opposed to the build being skipped.
    pub built: bool,
    /// Combined compiler output, useful even on success for warnings.
    pub output: String,
}

impl BuildReport {
    pub fn summary(&self) -> String {
        if !self.built {
            return format!("up to date ({})", short_hash(&self.installed.sha256));
        }
        if self.replaced {
            format!(
                "installed {} ({})",
                self.installed.path.display(),
                short_hash(&self.installed.sha256)
            )
        } else {
            format!(
                "rebuilt, already current ({})",
                short_hash(&self.installed.sha256)
            )
        }
    }
}

/// The first eight characters of a digest, for logs.
pub fn short_hash(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

/// Wait until a file stops changing.
///
/// Cargo writes the artifact and the linker may still be finishing when the
/// process exits, so the file is polled until its size and modification time
/// hold still across a settle window. Hashing a half-written library is how a
/// hash check passes and the loaded extension crashes.
pub fn wait_until_stable(
    path: &Path,
    settle: Duration,
    timeout: Duration,
) -> Result<u64, BuildError> {
    let started = Instant::now();
    wait_until_stable_with(
        path,
        settle,
        timeout,
        || {
            let metadata = std::fs::metadata(path)
                .map_err(|e| BuildError::Io(format!("could not stat '{}': {e}", path.display())))?;
            Ok((
                metadata.len(),
                metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            ))
        },
        || started.elapsed(),
        || std::thread::sleep(Duration::from_millis(25)),
    )
}

/// Keep the polling contract testable without assumptions about writer scheduling.
fn wait_until_stable_with(
    path: &Path,
    settle: Duration,
    timeout: Duration,
    mut snapshot: impl FnMut() -> Result<(u64, std::time::SystemTime), BuildError>,
    mut elapsed: impl FnMut() -> Duration,
    mut wait: impl FnMut(),
) -> Result<u64, BuildError> {
    let mut last: Option<(u64, std::time::SystemTime)> = None;
    let mut stable_since = elapsed();

    loop {
        let current = snapshot()?;
        let now = elapsed();

        match last {
            Some(previous) if previous == current => {
                if now.saturating_sub(stable_since) >= settle {
                    return Ok(current.0);
                }
            }
            _ => {
                last = Some(current);
                stable_since = now;
            }
        }

        if now >= timeout {
            return Err(BuildError::Unstable {
                path: path.to_path_buf(),
                seconds: timeout.as_secs(),
            });
        }
        wait();
    }
}

/// How long an artifact must hold still before it is considered finished.
pub const ARTIFACT_SETTLE: Duration = Duration::from_millis(250);
/// How long to wait for an artifact to appear and settle.
pub const ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30);

/// Stage an artifact beside its destination, and swap it in.
///
/// Staging inside the destination directory keeps the swap on one volume,
/// where a rename is atomic. The original is moved aside first and restored if
/// the swap fails, so there is no path through this function that loses the
/// installed library.
pub fn install(staged_source: &Path, destination: &Path) -> Result<Artifact, BuildError> {
    let directory = destination.parent().ok_or_else(|| {
        BuildError::Install(format!(
            "'{}' has no parent directory",
            destination.display()
        ))
    })?;
    std::fs::create_dir_all(directory).map_err(|e| {
        BuildError::Install(format!("could not create '{}': {e}", directory.display()))
    })?;

    let staged = destination.with_extension(format!(
        "{}.staging",
        destination
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("tmp")
    ));

    // Copy first, verify, then swap. Nothing before the swap touches the
    // destination.
    std::fs::copy(staged_source, &staged)
        .map_err(|e| BuildError::Install(format!("could not stage '{}': {e}", staged.display())))?;

    let staged_hash = sha256_file(&staged)
        .map_err(|e| BuildError::Install(format!("could not hash the staged artifact: {e}")))?;
    let source_hash = sha256_file(staged_source)
        .map_err(|e| BuildError::Install(format!("could not hash the source artifact: {e}")))?;
    if staged_hash != source_hash {
        let _ = std::fs::remove_file(&staged);
        return Err(BuildError::Install(format!(
            "the staged copy does not match the source ({staged_hash} vs {source_hash})"
        )));
    }

    let bytes = std::fs::metadata(&staged)
        .map_err(|e| BuildError::Install(e.to_string()))?
        .len();

    // ---- commit boundary: everything fallible above has succeeded --------
    if let Err(error) = crate::files::replace(&staged, destination) {
        let _ = std::fs::remove_file(&staged);
        return Err(BuildError::Install(locked_message(destination, &error)));
    }

    Ok(Artifact {
        path: destination.to_path_buf(),
        sha256: staged_hash,
        bytes,
    })
}

/// Explain a failed swap in terms a user can act on.
fn locked_message(destination: &Path, error: &std::io::Error) -> String {
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| destination.display().to_string());
    format!(
        "could not replace '{name}': {error}. \
         If Godot has the extension loaded, close the running game and retry; \
         the previously installed library is still in place. Studio will not \
         terminate the editor to force the copy."
    )
}

/// Build and install, or leave the destination untouched.
///
/// `force` skips the up-to-date check, which is what a user pressing Build
/// expects.
/// Run cargo, retrying briefly if the system says the binary is busy.
///
/// `ETXTBSY` means the executable is still open for writing somewhere. It
/// happens when a process forks between another thread opening a file and
/// closing it, because the child inherits the descriptor, and the failure
/// surfaces as "Text file busy" at the exec. It is transient by definition —
/// the holder is on its way to closing it — and it is a real possibility for a
/// build tool, not only a test artefact: anything that writes a wrapper script
/// and then runs it can meet this.
///
/// Three attempts over about a tenth of a second. Beyond that the holder is not
/// about to let go, and saying so beats spinning.
fn run_cargo_with_retry(
    command: &crate::process::Command,
    timeout: Duration,
) -> Result<Outcome, BuildError> {
    let mut delay = Duration::from_millis(10);
    for attempt in 0..3 {
        match command.run(timeout) {
            Ok(outcome) => return Ok(outcome),
            Err(error) if error.raw_os_error() == Some(TEXT_FILE_BUSY) && attempt < 2 => {
                std::thread::sleep(delay);
                delay *= 3;
            }
            Err(error) => {
                return Err(BuildError::Io(format!("could not run cargo: {error}")));
            }
        }
    }
    unreachable!("the loop returns on every path")
}

/// The errno Linux and macOS report for an executable that is still open for
/// writing. Named rather than spelled 26, which means something else on Windows
/// and nothing at all to a reader.
#[cfg(unix)]
const TEXT_FILE_BUSY: i32 = 26;
#[cfg(not(unix))]
const TEXT_FILE_BUSY: i32 = -1;
pub fn run(
    request: &BuildRequest,
    _force: bool,
    cargo_timeout: Duration,
) -> Result<BuildReport, BuildError> {
    // Only Cargo knows whether dependencies or source files changed.
    let command = request.command();
    let outcome: Outcome = run_cargo_with_retry(&command, cargo_timeout)?;
    let mut diagnostics = outcome.stderr.clone();
    for line in outcome.stdout.lines() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(rendered) = value["message"]["rendered"].as_str() {
                if !diagnostics.contains(rendered) {
                    diagnostics.push_str(rendered);
                }
            }
        } else if diagnostics.is_empty() {
            diagnostics.push_str(line);
            diagnostics.push('\n');
        }
    }

    if !outcome.success() {
        // The destination was never touched, which is the whole point.
        return Err(BuildError::Cargo(if diagnostics.trim().is_empty() {
            outcome.failure_detail()
        } else {
            diagnostics
        }));
    }

    let source = outcome
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value["reason"] == "compiler-artifact"
                && value["target"]["name"] == request.package.replace('-', "_")
        })
        .filter_map(|value| {
            value["filenames"].as_array().and_then(|files| {
                files
                    .iter()
                    .filter_map(|v| v.as_str())
                    .find(|file| {
                        Path::new(file)
                            .extension()
                            .is_some_and(|extension| extension == dynamic_library_extension())
                    })
                    .map(PathBuf::from)
            })
        })
        .next_back()
        .unwrap_or_else(|| request.source_artifact());

    if !source.is_file() {
        return Err(BuildError::NoArtifact(source));
    }
    wait_until_stable(&source, ARTIFACT_SETTLE, ARTIFACT_TIMEOUT)?;

    let source_artifact = artifact_of(&source)?;

    // Capture what was installed *before* the swap, so "replaced" reports
    // whether the content actually changed rather than merely that a build ran.
    let previous = sha256_file(&request.destination).ok();
    let installed = if previous.as_deref() == Some(source_artifact.sha256.as_str()) {
        artifact_of(&request.destination)?
    } else {
        install(&source, &request.destination)?
    };
    let replaced = previous.as_deref() != Some(installed.sha256.as_str());
    let mut output = diagnostics;
    if request.profile.is_debug() {
        if let Err(error) = publish_reload_marker(&request.destination, &installed.sha256) {
            output.push_str(&format!(
                "\nInstalled successfully; reload notification failed: {error}"
            ));
        }
    }

    Ok(BuildReport {
        source: source_artifact,
        installed,
        replaced,
        built: true,
        output,
    })
}

fn publish_reload_marker(destination: &Path, hash: &str) -> std::io::Result<()> {
    let Some(bin) = destination
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == "bin"))
    else {
        return Ok(());
    };
    let Some(project) = bin.parent().and_then(Path::parent).and_then(Path::parent) else {
        return Ok(());
    };
    let Some(name) = destination.file_stem() else {
        return Ok(());
    };
    let marker = project
        .join(".godot/aurum")
        .join(format!("{}.reload", name.to_string_lossy()));
    crate::files::write_atomic(&marker, hash.to_uppercase().as_bytes())
}

fn artifact_of(path: &Path) -> Result<Artifact, BuildError> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| BuildError::Io(format!("could not stat '{}': {e}", path.display())))?;
    let sha256 = sha256_file(path)
        .map_err(|e| BuildError::Io(format!("could not hash '{}': {e}", path.display())))?;
    Ok(Artifact {
        path: path.to_path_buf(),
        sha256,
        bytes: metadata.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-build-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake Cargo that writes the artifact a real one would.
    #[cfg(windows)]
    fn fake_cargo(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-cargo.cmd");
        std::fs::write(&path, format!("@echo off\r\n{body}\r\n")).unwrap();
        path
    }

    #[cfg(not(windows))]
    fn fake_cargo(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("fake-cargo.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        // Unix will not run a script that is not executable, and a new file is
        // 0644. Without this every build test fails with "could not run cargo:
        // Permission denied", which reads as a fault in the build code rather
        // than in the fixture standing in for it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The line ending of whichever shell the fixtures below are written for.
    ///
    /// These are shell scripts for the host, and batch and `sh` are not the
    /// same language. Writing `\r\n` into a `#!/bin/sh` file is what made every
    /// build test fail on Linux and macOS with "Bad fd number": the carriage
    /// return ends up inside the command, so `1>&2` reads as `1>&2\r`.
    const NL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

    /// A shell command that creates an empty file.
    fn touch(path: &Path) -> String {
        if cfg!(windows) {
            format!("copy /Y NUL \"{}\" > NUL", path.display())
        } else {
            format!(": > \"{}\"", path.display())
        }
    }

    fn request_for(dir: &Path, cargo: PathBuf) -> BuildRequest {
        // A real build always has a workspace to run in; spawning would fail
        // with an invalid-directory error if it did not exist.
        std::fs::create_dir_all(dir.join("workspace")).unwrap();
        BuildRequest::new(
            dir.join("workspace"),
            "aurum-godot",
            Profile::Debug,
            dir.join("installed").join("aurum_godot.debug.dll"),
            cargo,
        )
    }

    #[test]
    fn profile_paths_and_names_are_distinct() {
        assert_eq!(Profile::Debug.directory(), "debug");
        assert_eq!(Profile::Release.directory(), "release");
        assert_eq!(Profile::Debug.cargo_flag(), None);
        assert_eq!(Profile::Release.cargo_flag(), Some("--release"));

        // A development build must never be able to overwrite a packaged
        // release library, so the names differ.
        let debug = Profile::Debug.installed_filename("aurum_godot");
        let release = Profile::Release.installed_filename("aurum_godot");
        assert_ne!(debug, release);
        assert!(debug.contains(".debug."), "{debug}");
        assert!(!release.contains(".debug."), "{release}");
    }

    #[test]
    fn package_names_become_library_names() {
        assert_eq!(library_name("aurum-godot"), "aurum_godot");
        assert_eq!(library_name("aurum"), "aurum");
    }

    #[test]
    fn the_request_names_the_artifact_cargo_will_produce() {
        let request = BuildRequest::new("W", "aurum-godot", Profile::Debug, "D", "cargo");
        let expected = PathBuf::from("W")
            .join("target")
            .join("debug")
            .join(format!("aurum_godot.{}", dynamic_library_extension()));
        assert_eq!(request.source_artifact(), expected);
    }

    #[test]
    fn the_command_is_built_from_arguments_not_a_shell_string() {
        let request = BuildRequest::new("W", "aurum-godot", Profile::Release, "D", "cargo");
        let command = request.command();
        let arguments = command.arguments();
        assert_eq!(arguments[0], "build");
        assert_eq!(arguments[1], "-p");
        assert_eq!(arguments[2], "aurum-godot");
        assert!(arguments.contains(&"--release".to_string()));
        assert!(arguments.contains(&"--locked".to_string()));
        assert!(arguments.contains(&"--color".to_string()));
    }

    #[test]
    fn wait_until_stable_returns_a_settled_size() {
        let dir = temp_dir("stable");
        let path = dir.join("artifact.bin");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();

        let size =
            wait_until_stable(&path, Duration::from_millis(50), Duration::from_secs(5)).unwrap();
        assert_eq!(size, 1024);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn changing_size_or_timestamp_each_poll_reaches_the_timeout() {
        use std::cell::Cell;
        for change_size in [true, false] {
            let clock = Cell::new(Duration::ZERO);
            let result = wait_until_stable_with(
                Path::new("changing.bin"),
                Duration::from_millis(150),
                Duration::from_millis(1200),
                || {
                    Ok((
                        if change_size {
                            clock.get().as_millis() as u64
                        } else {
                            1024
                        },
                        std::time::SystemTime::UNIX_EPOCH
                            + if change_size {
                                Duration::ZERO
                            } else {
                                clock.get()
                            },
                    ))
                },
                || clock.get(),
                || clock.set(clock.get() + Duration::from_millis(25)),
            );
            assert!(
                matches!(result, Err(BuildError::Unstable { .. })),
                "{result:?}"
            );
            assert_eq!(clock.get(), Duration::from_millis(1200));
        }
    }

    #[test]
    fn a_change_resets_the_entire_settle_window() {
        use std::cell::Cell;
        let clock = Cell::new(Duration::ZERO);
        let result = wait_until_stable_with(
            Path::new("settling.bin"),
            Duration::from_millis(50),
            Duration::from_secs(1),
            || {
                Ok((
                    if clock.get().is_zero() { 10 } else { 20 },
                    std::time::SystemTime::UNIX_EPOCH,
                ))
            },
            || clock.get(),
            || clock.set(clock.get() + Duration::from_millis(25)),
        );
        assert_eq!(result.unwrap(), 20);
        assert_eq!(clock.get(), Duration::from_millis(75));
    }

    #[test]
    fn wait_until_stable_reports_a_missing_file() {
        let result = wait_until_stable(
            Path::new("definitely-not-here.bin"),
            Duration::from_millis(10),
            Duration::from_millis(100),
        );
        assert!(matches!(result, Err(BuildError::Io(_))));
    }

    #[test]
    fn install_creates_the_destination_and_its_directory() {
        let dir = temp_dir("install-new");
        let source = dir.join("source.dll");
        std::fs::write(&source, b"library bytes").unwrap();
        let destination = dir.join("nested/deeper/target.dll");

        let artifact = install(&source, &destination).unwrap();
        assert_eq!(artifact.bytes, 13);
        assert_eq!(std::fs::read(&destination).unwrap(), b"library bytes");
        assert_eq!(artifact.sha256, sha256_file(&destination).unwrap());

        // No staging or backup files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(destination.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("staging") || name.contains("previous"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_replaces_an_existing_library() {
        let dir = temp_dir("install-replace");
        let source = dir.join("source.dll");
        let destination = dir.join("target.dll");
        std::fs::write(&destination, b"old").unwrap();
        std::fs::write(&source, b"new and improved").unwrap();

        let artifact = install(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"new and improved");
        assert_eq!(artifact.sha256, sha256_file(&source).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_leaves_the_destination_alone_when_the_source_is_missing() {
        let dir = temp_dir("install-missing-source");
        let destination = dir.join("target.dll");
        std::fs::write(&destination, b"the working library").unwrap();

        let result = install(&dir.join("no-such-source.dll"), &destination);
        assert!(matches!(result, Err(BuildError::Install(_))), "{result:?}");
        // The whole guarantee, in one assertion.
        assert_eq!(std::fs::read(&destination).unwrap(), b"the working library");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_build_leaves_the_installed_library_untouched() {
        let dir = temp_dir("failed-build");
        let cargo = fake_cargo(&dir, &format!("echo a compiler error 1>&2{NL}exit 101"));
        let request = request_for(&dir, cargo);

        std::fs::create_dir_all(request.destination.parent().unwrap()).unwrap();
        std::fs::write(&request.destination, b"the last working library").unwrap();
        let before = sha256_file(&request.destination).unwrap();

        let result = run(&request, true, Duration::from_secs(30));
        match result {
            Err(BuildError::Cargo(detail)) => {
                assert!(detail.contains("compiler error"), "{detail}");
            }
            other => panic!("expected a cargo failure, got {other:?}"),
        }

        // Not merely present: byte-identical.
        assert_eq!(sha256_file(&request.destination).unwrap(), before);
        assert_eq!(
            std::fs::read(&request.destination).unwrap(),
            b"the last working library"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_build_installs_the_artifact() {
        let dir = temp_dir("success");
        let workspace = dir.join("workspace");
        let artifact = workspace
            .join("target")
            .join("debug")
            .join(format!("aurum_godot.{}", dynamic_library_extension()));
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();

        let cargo = fake_cargo(&dir, &format!("echo building{NL}{}", touch(&artifact)));
        let request = request_for(&dir, cargo);

        let report = run(&request, true, Duration::from_secs(30)).unwrap();
        assert!(report.built);
        // Nothing was installed before, so this is a replacement.
        assert!(report.replaced);
        assert!(request.destination.is_file());
        assert_eq!(
            sha256_file(&request.destination).unwrap(),
            report.installed.sha256
        );
        assert!(
            report.summary().contains("installed"),
            "{}",
            report.summary()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replaced_reports_whether_the_content_changed_not_whether_cargo_ran() {
        let dir = temp_dir("replaced-flag");
        let workspace = dir.join("workspace");
        let artifact = workspace
            .join("target")
            .join("debug")
            .join(format!("aurum_godot.{}", dynamic_library_extension()));
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();

        // The fake cargo copies a payload the test controls, so each build can
        // produce different bytes without touching the fake itself.
        let payload = dir.join("payload.bin");
        #[cfg(windows)]
        let body = format!(
            "copy /Y \"{}\" \"{}\" > NUL",
            payload.display(),
            artifact.display()
        );
        #[cfg(not(windows))]
        let body = format!("cp \"{}\" \"{}\"", payload.display(), artifact.display());
        let request = request_for(&dir, fake_cargo(&dir, &body));
        std::fs::create_dir_all(request.destination.parent().unwrap()).unwrap();

        // First install: there was nothing there, so it counts as replaced.
        std::fs::write(&payload, b"version one").unwrap();
        let first = run(&request, true, Duration::from_secs(30)).unwrap();
        assert!(first.replaced, "installing over nothing is a replacement");

        // Second install of different bytes: replaced.
        std::fs::write(&payload, b"version two").unwrap();
        std::fs::write(&artifact, b"stale artifact so the build is not skipped").unwrap();
        let second = run(&request, true, Duration::from_secs(30)).unwrap();
        assert!(second.replaced, "different bytes are a replacement");

        // The up-to-date path never reaches the install at all, and must not
        // claim to have replaced anything.
        let third = run(&request, false, Duration::from_secs(30)).unwrap();
        assert!(third.built, "Cargo must check source freshness");
        assert!(!third.replaced);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cargo_that_succeeds_without_producing_an_artifact_is_reported() {
        let dir = temp_dir("no-artifact");
        let cargo = fake_cargo(&dir, &format!("echo nothing to see{NL}exit 0"));
        let request = request_for(&dir, cargo);

        let result = run(&request, true, Duration::from_secs(30));
        assert!(
            matches!(result, Err(BuildError::NoArtifact(_))),
            "got {result:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_up_to_date_build_does_not_touch_the_installed_library() {
        let dir = temp_dir("uptodate");
        let workspace = dir.join("workspace");
        let artifact = workspace
            .join("target")
            .join("debug")
            .join(format!("aurum_godot.{}", dynamic_library_extension()));
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"identical bytes").unwrap();

        let request = request_for(&dir, fake_cargo(&dir, "exit 0"));
        std::fs::create_dir_all(request.destination.parent().unwrap()).unwrap();
        std::fs::write(&request.destination, b"identical bytes").unwrap();

        // Cargo must still evaluate source freshness even when artifacts match.
        let report = run(&request, false, Duration::from_secs(30)).unwrap();
        assert!(report.built, "cargo must check the source");
        assert!(!report.replaced);
        assert!(
            report.summary().contains("already current"),
            "{}",
            report.summary()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn force_rebuilds_even_when_the_artifact_already_matches() {
        let dir = temp_dir("force");
        let workspace = dir.join("workspace");
        let artifact = workspace
            .join("target")
            .join("debug")
            .join(format!("aurum_godot.{}", dynamic_library_extension()));
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"identical bytes").unwrap();

        // Writes the artifact again, so the build is observable.
        let cargo = fake_cargo(
            &dir,
            &format!("echo fake library> \"{}\"", artifact.display()),
        );
        let request = request_for(&dir, cargo);
        std::fs::create_dir_all(request.destination.parent().unwrap()).unwrap();
        std::fs::write(&request.destination, b"identical bytes").unwrap();

        let report = run(&request, true, Duration::from_secs(30)).unwrap();
        assert!(report.built, "force should have run cargo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_destination_is_reported_with_a_remedy() {
        // Opening the destination for exclusive use reproduces the real case:
        // Godot has the extension loaded and Windows refuses the rename.
        let dir = temp_dir("locked");
        let source = dir.join("source.dll");
        let destination = dir.join("target.dll");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();

        let handle = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination);

        if let Ok(handle) = handle {
            let result = install(&source, &destination);
            match result {
                // Windows refuses; the message must explain what to do.
                Err(BuildError::Install(message)) => {
                    assert!(
                        message.contains("still in place"),
                        "the message should reassure: {message}"
                    );
                    assert!(
                        message.contains("will not terminate"),
                        "the message should state the policy: {message}"
                    );
                }
                // On a platform that permits the rename, the install succeeds
                // and there is nothing to assert beyond that.
                Ok(_) => {}
                Err(other) => panic!("unexpected error {other:?}"),
            }
            drop(handle);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn short_hash_copes_with_a_short_input() {
        assert_eq!(short_hash("abcdef1234567890"), "abcdef12");
        assert_eq!(short_hash("abc"), "abc");
        assert_eq!(short_hash(""), "");
    }
}
