//! Building the native extension and installing it safely.
//!
//! The guarantee this module exists to provide is narrow and absolute: **a
//! failed compilation never damages the working extension.** Everything is ordered
//! around that. Cargo runs, the artifact is waited on until it stops changing,
//! it is hashed and staged *inside the destination directory*, and only then
//! is the installed library swapped. Any failure before the swap leaves the
//! destination byte-identical to what it was.
//!
//! Installed sources are never touched, and Godot is never terminated to win a
//! file lock. A locked library is a bounded, reported failure with a remedy,
//! because killing the editor to force a copy would discard the user's work to
//! save a retry.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::hash::sha256_file;
use crate::process::{Command, Outcome};

fn operation_check() -> Result<(), BuildError> {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context
            .check()
            .map_err(|error| BuildError::Io(error.to_string()))?;
    }
    Ok(())
}
fn operation_phase(phase: crate::operation_context::Phase) {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.phase(phase);
    }
}
fn operation_budget(local: Duration) -> Result<Duration, BuildError> {
    crate::operation_context::OperationContext::current()
        .map(|context| context.budget(local))
        .transpose()
        .map_err(|error| BuildError::Io(error.to_string()))
        .map(|budget| budget.unwrap_or(local))
}
fn checked_hash(path: &Path) -> std::io::Result<String> {
    let Some(context) = crate::operation_context::OperationContext::current() else {
        return sha256_file(path);
    };
    context.check()?;
    let mut hash = crate::hash::Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        context.check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    context.check()?;
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn copy_artifact(source: &Path, destination: &Path) -> std::io::Result<()> {
    let context = crate::operation_context::OperationContext::current();
    if let Some(context) = &context {
        context.check()?;
    }
    let mut source = std::fs::File::open(source)?;
    let permissions = source.metadata()?.permissions();
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        if let Some(context) = &context {
            context.check()?;
        }
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if let Some(context) = &context {
            context.check()?;
        }
        target.write_all(&buffer[..count])?;
    }
    std::fs::set_permissions(destination, permissions)?;
    target.sync_all()?;
    if let Some(context) = &context {
        context.check()?;
    }
    Ok(())
}
struct StagedLibrary(PathBuf);
impl Drop for StagedLibrary {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Ok(metadata) = std::fs::symlink_metadata(&self.0) {
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.permissions().readonly()
            {
                let mut permissions = metadata.permissions();
                // Windows readonly is a file attribute, not Unix mode bits.
                // This applies only to our fresh abandoned staging file.
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&self.0, permissions);
            }
        }
        let _ = std::fs::remove_file(&self.0);
    }
}

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
    let timeout = operation_budget(timeout)?;
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
        || {
            let delay = crate::operation_context::OperationContext::current()
                .map(|context| context.deadline().saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_millis(25))
                .min(Duration::from_millis(25));
            std::thread::sleep(delay);
        },
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
        operation_check()?;
        let current = snapshot()?;
        let now = elapsed();
        operation_check()?;

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
/// where a rename is atomic. A failed swap preserves the existing library.
/// The successful atomic replacement of the verified staging file records
/// the installed-library commit before optional reload notification.
pub fn install(staged_source: &Path, destination: &Path) -> Result<Artifact, BuildError> {
    install_with(staged_source, destination, || {})
}

/// Stage verified library bytes in a private snapshot or unpublished package.
/// This keeps job cancellation and deadlines without recording publication.
pub fn install_private(staged_source: &Path, destination: &Path) -> Result<Artifact, BuildError> {
    install_with_policy(staged_source, destination, false, || {})
}

fn install_with(
    staged_source: &Path,
    destination: &Path,
    prepared: impl FnOnce(),
) -> Result<Artifact, BuildError> {
    install_with_policy(staged_source, destination, true, prepared)
}

fn install_with_policy(
    staged_source: &Path,
    destination: &Path,
    record_commit: bool,
    prepared: impl FnOnce(),
) -> Result<Artifact, BuildError> {
    operation_phase(crate::operation_context::Phase::Staging);
    operation_check()?;
    let previous = if destination.exists() {
        Some(checked_hash(destination).map_err(|error| BuildError::Install(error.to_string()))?)
    } else {
        None
    };
    let directory = destination.parent().ok_or_else(|| {
        BuildError::Install(format!(
            "'{}' has no parent directory",
            destination.display()
        ))
    })?;
    std::fs::create_dir_all(directory).map_err(|e| {
        BuildError::Install(format!("could not create '{}': {e}", directory.display()))
    })?;

    let staged = StagedLibrary(crate::files::sibling_temp(destination));

    // Copy first, verify, then swap. Nothing before the swap touches the
    // destination.
    copy_artifact(staged_source, &staged.0).map_err(|e| {
        BuildError::Install(format!("could not stage '{}': {e}", staged.0.display()))
    })?;

    let staged_hash = checked_hash(&staged.0)
        .map_err(|e| BuildError::Install(format!("could not hash the staged artifact: {e}")))?;
    let source_hash = checked_hash(staged_source)
        .map_err(|e| BuildError::Install(format!("could not hash the source artifact: {e}")))?;
    if staged_hash != source_hash {
        return Err(BuildError::Install(format!(
            "the staged copy does not match the source ({staged_hash} vs {source_hash})"
        )));
    }

    let bytes = std::fs::metadata(&staged.0)
        .map_err(|e| BuildError::Install(e.to_string()))?
        .len();

    prepared();
    operation_check()?;
    let current = if destination.exists() {
        Some(checked_hash(destination).map_err(|error| BuildError::Install(error.to_string()))?)
    } else {
        None
    };
    if current != previous {
        return Err(BuildError::Install(
            "installed library changed while staging; newer bytes were preserved".into(),
        ));
    }
    if record_commit {
        operation_phase(crate::operation_context::Phase::Publishing);
    }
    let context = crate::operation_context::OperationContext::current();
    let mut commit = if record_commit {
        context
            .as_ref()
            .map(|context| context.commit_barrier())
            .transpose()
            .map_err(|error| BuildError::Install(error.to_string()))?
    } else {
        None
    };
    if let Err(error) = crate::files::replace(&staged.0, destination) {
        let unchanged = match &previous {
            Some(hash) => checked_hash(destination).is_ok_and(|current| current == *hash),
            None => !destination.exists(),
        };
        if unchanged {
            if let Some(commit) = commit.take() {
                commit.rollback_complete();
            }
        } else if record_commit {
            if let Some(context) = &context {
                context.mark_recovery_required();
            }
        }
        return Err(BuildError::Install(locked_message(destination, &error)));
    }
    if let Some(commit) = commit.take() {
        commit.finish();
    }
    if !record_commit {
        operation_check()?;
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
        operation_check()?;
        match command.run(timeout) {
            Ok(outcome) => return Ok(outcome),
            Err(error) if error.raw_os_error() == Some(TEXT_FILE_BUSY) && attempt < 2 => {
                std::thread::sleep(operation_budget(delay)?);
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
    force: bool,
    cargo_timeout: Duration,
) -> Result<BuildReport, BuildError> {
    run_with_policy(request, force, cargo_timeout, true)
}

/// Build a library for private validation/import without publishing it.
pub fn run_private(
    request: &BuildRequest,
    force: bool,
    cargo_timeout: Duration,
) -> Result<BuildReport, BuildError> {
    run_with_policy(request, force, cargo_timeout, false)
}

fn run_with_policy(
    request: &BuildRequest,
    _force: bool,
    cargo_timeout: Duration,
    record_commit: bool,
) -> Result<BuildReport, BuildError> {
    operation_phase(crate::operation_context::Phase::Dependencies);
    operation_check()?;
    // Package names and library target names are independent. Cargo's opaque
    // package identity also distinguishes dependency artifacts from this build.
    let started = std::time::Instant::now();
    let mut metadata = Command::new(&request.cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--color",
            "never",
        ])
        .directory(&request.workspace);
    if request.locked {
        metadata = metadata.arg("--locked");
    }
    let metadata = run_cargo_with_retry(&metadata, cargo_timeout.min(Duration::from_secs(30)))?;
    if !metadata.success() {
        return Err(BuildError::Cargo(metadata.failure_detail()));
    }
    let metadata: serde_json::Value = serde_json::from_str(&metadata.stdout)
        .map_err(|_| BuildError::Cargo("Cargo metadata is invalid or truncated".into()))?;
    let (package_id, target_name) = library_identity(&metadata, &request.package)?;
    let remaining = cargo_timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(BuildError::Cargo(
            "Build deadline expired during Cargo metadata".into(),
        ));
    }
    // Only Cargo knows whether dependencies or source files changed.
    operation_phase(crate::operation_context::Phase::Compiling);
    operation_check()?;
    let command = request.command();
    let outcome: Outcome = run_cargo_with_retry(&command, remaining)?;
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

    let source = reported_library(&outcome.stdout, &package_id, &target_name)?;
    operation_phase(crate::operation_context::Phase::Validating);
    operation_check()?;

    if !source.is_file() {
        return Err(BuildError::NoArtifact(source));
    }
    wait_until_stable(&source, ARTIFACT_SETTLE, ARTIFACT_TIMEOUT)?;

    let source_artifact = artifact_of(&source)?;

    // Capture what was installed *before* the swap, so "replaced" reports
    // whether the content actually changed rather than merely that a build ran.
    let previous = checked_hash(&request.destination).ok();
    operation_check()?;
    let installed = if previous.as_deref() == Some(source_artifact.sha256.as_str()) {
        artifact_of(&request.destination)?
    } else if !record_commit {
        install_private(&source, &request.destination)?
    } else {
        install(&source, &request.destination)?
    };
    let replaced = previous.as_deref() != Some(installed.sha256.as_str());
    operation_phase(crate::operation_context::Phase::Cleanup);
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

fn library_identity(
    metadata: &serde_json::Value,
    package: &str,
) -> Result<(String, String), BuildError> {
    if metadata["version"] != 1 {
        return Err(BuildError::Cargo(
            "Unsupported Cargo metadata version".into(),
        ));
    }
    let packages = metadata["packages"]
        .as_array()
        .filter(|packages| packages.len() <= 4096)
        .ok_or_else(|| {
            BuildError::Cargo("Cargo package graph is missing or exceeds its bound".into())
        })?;
    let selected: Vec<_> = packages
        .iter()
        .filter(|entry| entry["name"] == package)
        .collect();
    if selected.len() != 1 {
        return Err(BuildError::Cargo(
            "Configured Rust package is not unique in this workspace".into(),
        ));
    }
    let entry = selected[0];
    let targets: Vec<_> = entry["targets"]
        .as_array()
        .ok_or_else(|| BuildError::Cargo("Cargo targets are missing".into()))?
        .iter()
        .filter(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "cdylib"))
        })
        .collect();
    if targets.len() != 1 {
        return Err(BuildError::Cargo(
            "Configured package must provide one cdylib library target".into(),
        ));
    }
    Ok((
        entry["id"]
            .as_str()
            .ok_or_else(|| BuildError::Cargo("Cargo package identity is missing".into()))?
            .into(),
        targets[0]["name"]
            .as_str()
            .ok_or_else(|| BuildError::Cargo("Cargo library target name is missing".into()))?
            .into(),
    ))
}
fn reported_library(stdout: &str, package: &str, target: &str) -> Result<PathBuf, BuildError> {
    let mut selected = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact"
            || value["package_id"] != package
            || value["target"]["name"] != target
        {
            continue;
        }
        for filename in value["filenames"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
        {
            let path = PathBuf::from(filename);
            if path
                .extension()
                .is_some_and(|extension| extension == dynamic_library_extension())
            {
                if selected.as_ref().is_some_and(|previous| previous != &path) {
                    return Err(BuildError::Cargo(
                        "Cargo reported multiple libraries for the selected target".into(),
                    ));
                }
                selected = Some(path);
            }
        }
    }
    selected.ok_or_else(||BuildError::Cargo("Cargo succeeded without reporting the selected native library; no stale artifact fallback is permitted".into()))
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
    let sha256 = checked_hash(path)
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

    #[test]
    fn private_installs_allow_multiple_stages_without_committing_and_remain_cancellable() {
        let root = temp_dir("private-installs");
        let source = root.join("compiled.dll");
        let accepted = root.join("accepted.dll");
        std::fs::write(&source, b"compiled library").unwrap();
        std::fs::write(&accepted, b"accepted library").unwrap();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        context
            .scope(|| {
                install_private(&source, &root.join("private/first.dll")).unwrap();
                install_private(&source, &root.join("private/second.dll")).unwrap();
                assert!(!context.committed());
                assert!(context.request_cancel().unwrap());
                assert!(install_private(&source, &root.join("private/cancelled.dll")).is_err());
            })
            .unwrap();
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert!(!root.join("private/cancelled.dll").exists());
        assert_eq!(std::fs::read(&accepted).unwrap(), b"accepted library");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_artifact_staging_preserves_the_installed_library() {
        let root = temp_dir("cancelled-staging");
        let source = root.join("compiled.dll");
        let destination = root.join("installed/active.dll");
        std::fs::create_dir(destination.parent().unwrap()).unwrap();
        std::fs::write(&source, b"compiled library").unwrap();
        std::fs::write(&destination, b"accepted library").unwrap();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let result = context
            .scope(|| {
                install_with(&source, &destination, || {
                    assert!(!context.committed());
                    context.request_cancel().unwrap();
                })
            })
            .unwrap();
        assert!(result.is_err());
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert_eq!(std::fs::read(&destination).unwrap(), b"accepted library");
        assert_eq!(
            std::fs::read_dir(destination.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_library_switch_commits_and_refuses_late_cancellation() {
        let root = temp_dir("committed-switch");
        let source = root.join("compiled.dll");
        let destination = root.join("active.dll");
        std::fs::write(&source, b"compiled library").unwrap();
        std::fs::write(&destination, b"accepted library").unwrap();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let artifact = context
            .scope(|| {
                install_with(&source, &destination, || {
                    assert!(!context.committed());
                })
            })
            .unwrap()
            .unwrap();
        assert!(context.committed());
        assert!(context.commit_status_verified());
        assert!(context.request_cancel().is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"compiled library");
        assert_eq!(artifact.sha256, crate::sha256_hex(b"compiled library"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn library_changes_during_staging_are_preserved() {
        let root = temp_dir("newer-library");
        let source = root.join("compiled.dll");
        let destination = root.join("active.dll");
        std::fs::write(&source, b"compiled library").unwrap();
        std::fs::write(&destination, b"accepted library").unwrap();
        assert!(install_with(&source, &destination, || {
            std::fs::write(&destination, b"newer external library").unwrap();
        })
        .is_err());
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            b"newer external library"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_during_artifact_settling_never_installs() {
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        let result = context
            .scope(|| {
                wait_until_stable_with(
                    Path::new("private-artifact"),
                    ARTIFACT_SETTLE,
                    ARTIFACT_TIMEOUT,
                    || {
                        context.request_cancel().unwrap();
                        Ok((1, std::time::SystemTime::UNIX_EPOCH))
                    },
                    || Duration::ZERO,
                    || panic!("cancelled settling must not wait"),
                )
            })
            .unwrap();
        assert!(result.is_err());
        assert!(!context.committed());
    }

    #[test]
    fn renamed_library_requires_matching_package_and_reported_artifact() {
        let metadata = serde_json::json!({"version":1,"packages":[{"name":"game-package","id":"opaque-game-id","targets":[{"name":"custom_library","kind":["cdylib"]}]}]});
        assert_eq!(
            library_identity(&metadata, "game-package").unwrap(),
            ("opaque-game-id".into(), "custom_library".into())
        );
        let path = format!("custom_library.{}", dynamic_library_extension());
        let message=serde_json::json!({"reason":"compiler-artifact","package_id":"opaque-game-id","target":{"name":"custom_library"},"filenames":[path]}).to_string();
        assert_eq!(
            reported_library(&message, "opaque-game-id", "custom_library").unwrap(),
            PathBuf::from(&path)
        );
        assert!(reported_library(&message, "another-package", "custom_library").is_err());
        assert!(reported_library("", "opaque-game-id", "custom_library").is_err());
    }

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
        let (metadata, artifact) = fake_messages(dir);
        std::fs::write(&path, format!("@echo off\r\nif \"%~1\"==\"metadata\" (type \"{}\" & exit /b 0)\r\ntype \"{}\"\r\n{body}\r\n",metadata.display(),artifact.display())).unwrap();
        path
    }

    #[cfg(not(windows))]
    fn fake_cargo(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = dir.join("fake-cargo.sh");
        let (metadata, artifact) = fake_messages(dir);
        std::fs::write(&path, format!("#!/bin/sh\nif [ \"$1\" = metadata ]; then cat '{}'; exit 0; fi\ncat '{}'\n{body}\n",metadata.display(),artifact.display())).unwrap();
        // Unix will not run a script that is not executable, and a new file is
        // 0644. Without this every build test fails with "could not run cargo:
        // Permission denied", which reads as a fault in the build code rather
        // than in the fixture standing in for it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fake_messages(dir: &Path) -> (PathBuf, PathBuf) {
        let metadata = dir.join("fake-metadata.json");
        let artifact = dir.join("fake-artifact.json");
        let path = dir
            .join("workspace/target/debug")
            .join(format!("aurum_godot.{}", dynamic_library_extension()));
        std::fs::write(&metadata,serde_json::json!({"version":1,"packages":[{"name":"aurum-godot","id":"fixture-package-id","targets":[{"name":"aurum_godot","kind":["cdylib"]}]}]}).to_string()+"\n").unwrap();
        std::fs::write(&artifact,serde_json::json!({"reason":"compiler-artifact","package_id":"fixture-package-id","target":{"name":"aurum_godot"},"filenames":[path]}).to_string()+"\n").unwrap();
        (metadata, artifact)
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
