//! Updating an installed Studio from its own release page.
//!
//! Nothing here runs unless the person asks: there is no background check and
//! no scheduled network traffic. A check reads one JSON document, and an update
//! happens only after a second, explicit request.
//!
//! ## Where the trust comes from
//!
//! The same place as [`crate::downloads`]: the digest, written down before the
//! bytes are fetched. The expected SHA-256 comes from the `SHA256SUMS.txt`
//! published beside the archive in the same release, and the archive is
//! discarded unless it hashes to that. That proves the download is the file the
//! release published. It does **not** prove who published it: both files come
//! from one place, and the executables are not code-signed. A person who can
//! change a release can change both files, so this protects against a damaged or
//! substituted download, not against a compromised account.
//!
//! ## What an update changes
//!
//! Only what Studio ships: the two executables, the runtime when it changed,
//! documentation, examples and scripts. Projects, sessions, shortcuts, the
//! project registry and the launcher script are never touched. Every file that
//! is replaced is first moved into `backups/`, and if any step fails, the files
//! already placed are put back.
//!
//! Windows will not let a running program be overwritten or deleted, but it
//! will let one be renamed. That is what makes it possible to replace the
//! executable of the Studio that is doing the updating.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::downloads::{self, Availability, DownloadError, Fetcher};
use crate::hash::sha256_file;

/// The repository releases are read from.
pub const REPOSITORY: &str = "AG064/aurum-studio";
/// Overrides the release feed, for loopback only. See [`Feed::from_environment`].
pub const FEED_VARIABLE: &str = "AURUM_UPDATE_FEED";

const MAX_RELEASE_BYTES: u64 = 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const NOTES_CHARS: usize = 6000;
const ARCHIVE_SUFFIX: &str = "-windows-x64.zip";
const SUMS_NAME: &str = "SHA256SUMS.txt";

/// Whether this platform can apply an update. Checking works everywhere.
pub fn can_apply() -> bool {
    cfg!(windows)
}

// -- where releases come from ------------------------------------------------

/// Where to read releases from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feed {
    /// This project's GitHub releases.
    GitHub,
    /// A server on this machine that serves the same release JSON and files.
    ///
    /// Only reachable by setting an environment variable, and only for
    /// `http://127.0.0.1`. It exists so the whole update path can be exercised
    /// end to end against real packages without publishing a release.
    Loopback(String),
}

impl Feed {
    /// The feed to use, honouring [`FEED_VARIABLE`].
    pub fn from_environment() -> Result<Self, String> {
        match std::env::var(FEED_VARIABLE) {
            Ok(value) if !value.trim().is_empty() => Self::loopback(value.trim()),
            _ => Ok(Self::GitHub),
        }
    }

    fn loopback(value: &str) -> Result<Self, String> {
        let base = value.trim_end_matches('/');
        let port = base.strip_prefix("http://127.0.0.1:").unwrap_or("");
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "{FEED_VARIABLE} must look like http://127.0.0.1:<port>; nothing else is accepted"
            ));
        }
        Ok(Self::Loopback(base.to_string()))
    }

    fn release_url(&self) -> String {
        match self {
            Self::GitHub => format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
            Self::Loopback(base) => format!("{base}/latest.json"),
        }
    }

    /// Whether a file may be downloaded from `url` for release `tag`.
    ///
    /// The release JSON names where its files are, and that is data from the
    /// network. A file is only fetched if it lives where this release's files
    /// are supposed to.
    fn allows(&self, url: &str, tag: &str) -> bool {
        let prefix = match self {
            Self::GitHub => format!("https://github.com/{REPOSITORY}/releases/download/{tag}/"),
            Self::Loopback(base) => format!("{base}/"),
        };
        url.starts_with(&prefix) && !url[prefix.len()..].contains(['/', '\\', '?', '#', '%'])
    }

    fn fetcher(&self, limit: u64) -> UpdateFetcher {
        UpdateFetcher {
            loopback: matches!(self, Self::Loopback(_)),
            limit,
        }
    }
}

/// Fetches with `curl`, restricted to what an update needs.
///
/// Only HTTPS (or loopback HTTP for the test feed), never a redirect to another
/// protocol, a size ceiling, and a time ceiling. The checksum is what makes the
/// result trustworthy; these limits keep a wrong answer from being expensive.
pub struct UpdateFetcher {
    loopback: bool,
    limit: u64,
}

impl Fetcher for UpdateFetcher {
    fn fetch(&self, url: &str, destination: &Path) -> Result<(), String> {
        let protocol = if self.loopback { "=http" } else { "=https" };
        let mut command = crate::process::Command::new("curl")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("--proto")
            .arg(protocol)
            .arg("--proto-redir")
            .arg(protocol)
            .arg("--max-filesize")
            .arg(self.limit.to_string())
            .arg("--header")
            .arg("User-Agent: aurum-studio-updater")
            .arg("--header")
            .arg("Accept: application/vnd.github+json, application/octet-stream");
        if !self.loopback {
            command = command.arg("--location").arg("--max-redirs").arg("5");
        }
        let outcome = command
            .arg("--output")
            .arg(destination.display().to_string())
            .arg(url)
            .run(Duration::from_secs(30 * 60))
            .map_err(|error| format!("could not run curl: {error}"))?;
        if !outcome.success() {
            return Err(outcome.failure_detail());
        }
        let size = std::fs::metadata(destination)
            .map(|meta| meta.len())
            .map_err(|error| format!("the download left no file: {error}"))?;
        if size > self.limit {
            let _ = std::fs::remove_file(destination);
            return Err(format!(
                "the download is larger than the {} byte limit",
                self.limit
            ));
        }
        Ok(())
    }
}

// -- versions ----------------------------------------------------------------

/// `1.2.3` or `v1.2.3` as numbers. Anything else, including pre-releases, is not
/// a version this will update to.
pub fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text
        .trim()
        .strip_prefix('v')
        .unwrap_or(text.trim())
        .split('.');
    let mut number = || {
        let part = parts.next()?;
        (!part.is_empty() && part.len() <= 6 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())?
    };
    let version = [number()?, number()?, number()?];
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a strictly newer version than `current`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

// -- reading a release -------------------------------------------------------

/// A release that can be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub title: String,
    /// The release notes, cut to a readable length. Shown as plain text.
    pub notes: String,
    /// The release page, for people who want to read more or update by hand.
    pub page: String,
    pub archive: String,
    pub archive_url: String,
    pub archive_bytes: u64,
    pub sums_url: String,
}

/// Read a release from GitHub's release JSON.
pub fn parse_release(value: &Value, feed: &Feed) -> Result<Release, String> {
    let tag = value["tag_name"].as_str().ok_or("the release has no tag")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if parse_version(tag).is_none() || !tag.starts_with('v') {
        return Err(format!("'{tag}' is not a version tag this can install"));
    }
    if value["draft"].as_bool() == Some(true) || value["prerelease"].as_bool() == Some(true) {
        return Err("the newest release is a draft or pre-release, which is not offered".into());
    }
    let archive = format!("Aurum-Studio-{version}{ARCHIVE_SUFFIX}");
    let assets = value["assets"]
        .as_array()
        .ok_or("the release lists no files")?;
    let find = |name: &str| {
        assets
            .iter()
            .find(|asset| asset["name"].as_str() == Some(name))
            .ok_or_else(|| format!("the release has no '{name}'"))
    };
    let archive_asset = find(&archive)?;
    let sums_asset = find(SUMS_NAME)?;
    let url = |asset: &Value| -> Result<String, String> {
        let url = asset["browser_download_url"]
            .as_str()
            .ok_or("a release file has no address")?;
        if feed.allows(url, tag) {
            Ok(url.to_string())
        } else {
            Err("a release file is not hosted where this release's files belong".into())
        }
    };
    let notes: String = value["body"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(NOTES_CHARS)
        .collect();
    Ok(Release {
        tag: tag.to_string(),
        version: version.to_string(),
        title: value["name"]
            .as_str()
            .unwrap_or(tag)
            .chars()
            .take(120)
            .collect(),
        notes,
        page: match feed {
            Feed::GitHub => format!("https://github.com/{REPOSITORY}/releases/tag/{tag}"),
            Feed::Loopback(base) => base.clone(),
        },
        archive_url: url(archive_asset)?,
        archive_bytes: archive_asset["size"].as_u64().unwrap_or(0),
        sums_url: url(sums_asset)?,
        archive,
    })
}

/// The SHA-256 a checksum file gives for `archive`.
pub fn expected_digest(sums: &str, archive: &str) -> Result<String, String> {
    let mut found: Option<String> = None;
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let (Some(hash), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        if name.trim_start_matches('*') != archive {
            continue;
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("the checksum for '{archive}' is not a SHA-256"));
        }
        let hash = hash.to_ascii_lowercase();
        match &found {
            Some(other) if *other != hash => {
                return Err(format!(
                    "the checksum file gives '{archive}' two different digests"
                ));
            }
            _ => found = Some(hash),
        }
    }
    found.ok_or_else(|| format!("the checksum file has no entry for '{archive}'"))
}

/// Look for a newer release. `None` means this one is current.
pub fn check(
    current: &str,
    feed: &Feed,
    fetcher: &dyn Fetcher,
    scratch: &Path,
) -> Result<Option<Release>, String> {
    std::fs::create_dir_all(scratch).map_err(|e| format!("'{}': {e}", scratch.display()))?;
    let file = scratch.join("release.json");
    let _ = std::fs::remove_file(&file);
    fetcher
        .fetch(&feed.release_url(), &file)
        .map_err(|e| format!("could not read the latest release: {e}"))?;
    let text = read_limited(&file, MAX_RELEASE_BYTES)?;
    let _ = std::fs::remove_file(&file);
    let value: Value = serde_json::from_str(&text).map_err(|_| "the release data is not JSON")?;
    let release = parse_release(&value, feed)?;
    Ok(is_newer(current, &release.version).then_some(release))
}

/// [`check`] with the downloader restricted to what an update needs.
pub fn check_online(current: &str, feed: &Feed, scratch: &Path) -> Result<Option<Release>, String> {
    check(current, feed, &feed.fetcher(MAX_RELEASE_BYTES), scratch)
}

fn read_limited(path: &Path, limit: u64) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("'{}': {e}", path.display()))?;
    if meta.len() > limit {
        return Err(format!("'{}' is larger than expected", path.display()));
    }
    std::fs::read_to_string(path).map_err(|e| format!("'{}': {e}", path.display()))
}

// -- fetching and checking a package ----------------------------------------

/// Where a preparation is up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Checksums,
    Download,
    Extract,
    Probe,
}

/// A downloaded, checked and unpacked release, ready to be installed.
#[derive(Debug, Clone)]
pub struct Staged {
    pub release: Release,
    /// The unpacked package: `Aurum Studio.exe`, `bin/`, `runtime/` and so on.
    pub root: PathBuf,
    pub archive_sha256: String,
}

/// The scratch file the archive is written to while it downloads, so progress
/// can be read from its size.
pub fn partial_archive(work: &Path, release: &Release) -> PathBuf {
    work.join(&release.tag)
        .join(format!(".{}.part", release.archive))
}

/// Download `release`, check it against its published checksum, unpack it, and
/// confirm the unpacked programs are the version that was announced.
pub fn prepare(
    release: &Release,
    current: &str,
    feed: &Feed,
    work: &Path,
    progress: &mut dyn FnMut(Stage),
) -> Result<Staged, String> {
    if !can_apply() {
        return Err("Updating from inside Studio is only available on Windows".into());
    }
    if !is_newer(current, &release.version) {
        return Err(format!("{} is not newer than {current}", release.version));
    }
    let directory = work.join(&release.tag);
    if directory.exists() {
        std::fs::remove_dir_all(&directory)
            .map_err(|e| format!("'{}': {e}", directory.display()))?;
    }
    std::fs::create_dir_all(&directory).map_err(|e| format!("'{}': {e}", directory.display()))?;

    progress(Stage::Checksums);
    let sums_file = directory.join(SUMS_NAME);
    feed.fetcher(MAX_SUMS_BYTES)
        .fetch(&release.sums_url, &sums_file)
        .map_err(|e| format!("could not read the release checksums: {e}"))?;
    let digest = expected_digest(&read_limited(&sums_file, MAX_SUMS_BYTES)?, &release.archive)?;

    progress(Stage::Download);
    let available = Availability {
        version: release.version.clone(),
        platform: "windows-x64".into(),
        url: release.archive_url.clone(),
        sha256: digest.clone(),
    };
    let archive = downloads::install(&available, &directory, &feed.fetcher(MAX_ARCHIVE_BYTES))
        .map_err(|error| match error {
            DownloadError::Mismatch { .. } => {
                "The download did not match the checksum published with the release, so it was discarded and nothing was changed.".to_string()
            }
            other => other.to_string(),
        })?;
    let size = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    if release.archive_bytes != 0 && size != release.archive_bytes {
        return Err(
            "The downloaded file is not the size the release lists, so it was not used.".into(),
        );
    }

    progress(Stage::Extract);
    let root = directory.join("package");
    unpack(&archive, &root)?;
    let _ = std::fs::remove_file(&archive);

    progress(Stage::Probe);
    for (relative, prefix) in [
        ("bin/aurum.exe", "aurum"),
        ("Aurum Studio.exe", "aurum-studio"),
    ] {
        let program = root.join(relative);
        if !program.is_file() {
            return Err(format!(
                "The package has no '{relative}', so it was not used."
            ));
        }
        let reported = crate::process::Command::new(program.display().to_string())
            .arg("--version")
            .run(Duration::from_secs(20))
            .ok()
            .filter(|outcome| outcome.success())
            .map(|outcome| outcome.stdout.trim().to_string())
            .unwrap_or_default();
        if reported != format!("{prefix} {}", release.version) {
            return Err(format!(
                "'{relative}' reports '{reported}' instead of '{prefix} {}', so the package was not used.",
                release.version
            ));
        }
    }
    Ok(Staged {
        release: release.clone(),
        root,
        archive_sha256: digest,
    })
}

fn tar_program() -> String {
    // The program Windows ships. A `tar` earlier on PATH, such as one from a Unix
    // toolkit, may not read zip files.
    match std::env::var_os("SystemRoot") {
        Some(root) if cfg!(windows) => PathBuf::from(root)
            .join("System32")
            .join("tar.exe")
            .display()
            .to_string(),
        _ => "tar".to_string(),
    }
}

/// Unpack a zip into `destination`, refusing anything that would land elsewhere.
fn unpack(archive: &Path, destination: &Path) -> Result<(), String> {
    let listing = crate::process::Command::new(tar_program())
        .arg("-tf")
        .arg(archive.display().to_string())
        .run(Duration::from_secs(120))
        .map_err(|e| format!("could not list the package: {e}"))?;
    if !listing.success() {
        return Err(format!(
            "the package could not be read: {}",
            listing.failure_detail()
        ));
    }
    for entry in listing.stdout.lines() {
        if !safe_entry(entry) {
            return Err(format!(
                "the package contains an unsafe path ('{entry}'), so it was not used"
            ));
        }
    }
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("'{}': {e}", destination.display()))?;
    let unpacked = crate::process::Command::new(tar_program())
        .arg("-xf")
        .arg(archive.display().to_string())
        .arg("-C")
        .arg(destination.display().to_string())
        .run(Duration::from_secs(10 * 60))
        .map_err(|e| format!("could not unpack the package: {e}"))?;
    if !unpacked.success() {
        return Err(format!(
            "the package could not be unpacked: {}",
            unpacked.failure_detail()
        ));
    }
    reject_links(destination)
}

fn safe_entry(entry: &str) -> bool {
    let entry = entry.trim_end_matches('/');
    !entry.is_empty()
        && !entry.starts_with('/')
        && !entry.contains(['\\', ':', '\0'])
        && entry
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn reject_links(root: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(root).map_err(|e| format!("'{}': {e}", root.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!(
                "the package contains a link ('{}'), so it was not used",
                entry.path().display()
            ));
        }
        if kind.is_dir() {
            reject_links(&entry.path())?;
        }
    }
    Ok(())
}

// -- the installed copy ------------------------------------------------------

/// Whether `executable` is the Studio of a managed installation, and if so, its
/// folder.
///
/// An installation is a folder with `bin/aurum.exe` and the `install.json` the
/// installer writes. A Studio run from a build folder is not one, and must not
/// be updated: its files belong to a developer, not to a release.
pub fn installation(executable: &Path) -> Result<PathBuf, String> {
    let executable =
        std::fs::canonicalize(executable).map_err(|e| format!("could not locate Studio: {e}"))?;
    let home = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("Studio is not inside an installation folder")?
        .to_path_buf();
    let expected = home.join("bin").join("aurum.exe");
    let same = std::fs::canonicalize(&expected)
        .map(|path| path == executable)
        .unwrap_or(false);
    if !same || !home.join("install.json").is_file() {
        return Err("This Studio is running from a build folder, not an installed copy. Update it by rebuilding, or install a release.".into());
    }
    // Without the `\?\` prefix canonicalisation adds, which `cmd` cannot open.
    Ok(crate::project::clean_path(home))
}

/// What an update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub from: String,
    pub to: String,
    pub replaced: usize,
    pub unchanged: usize,
    /// Where the replaced files were kept.
    pub backups: PathBuf,
}

/// The top-level names of a package that belong to the installation. Anything
/// else in a package, including `install.json`, the launcher script, `backups`
/// and any folder a future package adds, is left alone.
const OWNED: &[&str] = &[
    "Aurum Studio.exe",
    "bin",
    "runtime",
    "docs",
    "examples",
    "scripts",
    "README.md",
    "CHANGELOG.md",
    "CONTRIBUTING.md",
    "design-qa.md",
    "LICENSE",
    "LICENSE-Aurum.txt",
];

struct Change {
    relative: PathBuf,
    source: PathBuf,
    destination: PathBuf,
    sha256: String,
}

struct Placed {
    destination: PathBuf,
    backup: Option<PathBuf>,
}

/// Install a prepared release into `home`.
///
/// Either every changed file is in place, the installed programs report the new
/// version and `install.json` is updated, or the folder is as it was.
pub fn apply(home: &Path, staged: &Staged, current: &str) -> Result<Applied, String> {
    if !can_apply() {
        return Err("Updating from inside Studio is only available on Windows".into());
    }
    if !is_newer(current, &staged.release.version) {
        return Err(format!(
            "{} is not newer than {current}",
            staged.release.version
        ));
    }
    if !home.join("install.json").is_file() {
        return Err("This is not an installed copy of Studio".into());
    }
    let (changes, unchanged) = plan(home, &staged.root)?;
    let backups = home.join("backups").join(format!(
        "update-{current}-to-{}-{}",
        staged.release.version,
        crate::random::session_id()
            .chars()
            .take(8)
            .collect::<String>()
    ));
    let mut placed: Vec<Placed> = Vec::new();

    let result = (|| -> Result<(), String> {
        for change in &changes {
            place(change, &backups, &mut placed)?;
        }
        // The programs that were placed must be the ones that were checked, and the
        // installed one must run and report the new version.
        for change in changes.iter().filter(|c| is_program(&c.relative)) {
            let actual = sha256_file(&change.destination).map_err(|e| e.to_string())?;
            if actual != change.sha256 {
                return Err(format!(
                    "'{}' does not match the checked package after copying",
                    change.relative.display()
                ));
            }
        }
        let reported =
            crate::process::Command::new(home.join("bin").join("aurum.exe").display().to_string())
                .arg("--version")
                .run(Duration::from_secs(20))
                .ok()
                .filter(|outcome| outcome.success())
                .map(|outcome| outcome.stdout.trim().to_string())
                .unwrap_or_default();
        if reported != format!("aurum {}", staged.release.version) {
            return Err(format!(
                "the installed program reports '{reported}', not 'aurum {}'",
                staged.release.version
            ));
        }
        write_receipt(home, staged)
    })();

    if let Err(error) = result {
        let problems = rollback(&mut placed);
        return Err(if problems.is_empty() {
            format!("The update failed and the previous version was restored: {error}")
        } else {
            format!(
                "The update failed ({error}) and these files could not be restored, see '{}': {}",
                backups.display(),
                problems.join("; ")
            )
        });
    }
    Ok(Applied {
        from: current.to_string(),
        to: staged.release.version.clone(),
        replaced: changes.len(),
        unchanged,
        backups,
    })
}

fn is_program(relative: &Path) -> bool {
    relative == Path::new("Aurum Studio.exe") || relative == Path::new("bin").join("aurum.exe")
}

/// Which files differ, in the order they should be placed: everything else
/// first, the two programs last.
fn plan(home: &Path, root: &Path) -> Result<(Vec<Change>, usize), String> {
    let mut changes = Vec::new();
    let mut unchanged = 0;
    for name in OWNED {
        let source = root.join(name);
        if source.is_file() {
            consider(home, root, &source, &mut changes, &mut unchanged)?;
        } else if source.is_dir() {
            walk(home, root, &source, &mut changes, &mut unchanged)?;
        }
    }
    changes.sort_by_key(|change| is_program(&change.relative));
    Ok((changes, unchanged))
}

fn walk(
    home: &Path,
    root: &Path,
    directory: &Path,
    changes: &mut Vec<Change>,
    unchanged: &mut usize,
) -> Result<(), String> {
    for entry in
        std::fs::read_dir(directory).map_err(|e| format!("'{}': {e}", directory.display()))?
    {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            walk(home, root, &path, changes, unchanged)?;
        } else {
            consider(home, root, &path, changes, unchanged)?;
        }
    }
    Ok(())
}

fn consider(
    home: &Path,
    root: &Path,
    source: &Path,
    changes: &mut Vec<Change>,
    unchanged: &mut usize,
) -> Result<(), String> {
    let relative = source
        .strip_prefix(root)
        .map_err(|_| "a package file is outside the package")?
        .to_path_buf();
    let destination = home.join(&relative);
    let sha256 = sha256_file(source).map_err(|e| format!("'{}': {e}", source.display()))?;
    let same = std::fs::metadata(&destination)
        .map(|m| m.is_file())
        .unwrap_or(false)
        && sha256_file(&destination)
            .map(|h| h == sha256)
            .unwrap_or(false);
    if same {
        *unchanged += 1;
    } else {
        changes.push(Change {
            relative,
            source: source.to_path_buf(),
            destination,
            sha256,
        });
    }
    Ok(())
}

fn place(change: &Change, backups: &Path, placed: &mut Vec<Placed>) -> Result<(), String> {
    let parent = change
        .destination
        .parent()
        .ok_or("a destination has no folder")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("'{}': {e}", parent.display()))?;
    let name = change
        .destination
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let staged = parent.join(format!(".{name}.update-{}", crate::random::session_id()));
    std::fs::copy(&change.source, &staged)
        .map_err(|e| format!("could not copy '{}': {e}", change.relative.display()))?;
    let copied = sha256_file(&staged).map_err(|e| e.to_string());
    if copied.as_deref() != Ok(change.sha256.as_str()) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!(
            "'{}' was damaged while copying",
            change.relative.display()
        ));
    }
    let backup = if change.destination.exists() {
        let backup = backups.join(&change.relative);
        if let Some(folder) = backup.parent() {
            std::fs::create_dir_all(folder).map_err(|e| format!("'{}': {e}", folder.display()))?;
        }
        // A rename, so a program that is running can still be moved aside.
        std::fs::rename(&change.destination, &backup).map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            format!("could not move '{}' aside: {e}", change.relative.display())
        })?;
        Some(backup)
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&staged, &change.destination) {
        let _ = std::fs::remove_file(&staged);
        if let Some(backup) = &backup {
            let _ = std::fs::rename(backup, &change.destination);
        }
        return Err(format!(
            "could not place '{}': {error}",
            change.relative.display()
        ));
    }
    placed.push(Placed {
        destination: change.destination.clone(),
        backup,
    });
    Ok(())
}

fn rollback(placed: &mut Vec<Placed>) -> Vec<String> {
    let mut problems = Vec::new();
    while let Some(item) = placed.pop() {
        let _ = std::fs::remove_file(&item.destination);
        if let Some(backup) = item.backup {
            if let Err(error) = std::fs::rename(&backup, &item.destination) {
                problems.push(format!("{}: {error}", item.destination.display()));
            }
        }
    }
    problems
}

/// Update `install.json`, keeping every key the installer wrote that this does
/// not know to change.
fn write_receipt(home: &Path, staged: &Staged) -> Result<(), String> {
    let path = home.join("install.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("'{}': {e}", path.display()))?;
    let mut value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| "install.json is not valid JSON")?;
    let object = value
        .as_object_mut()
        .ok_or("install.json is not an object")?;
    let version = &staged.release.version;
    let hash = |relative: &str| {
        sha256_file(&home.join(relative))
            .map(|h| h.to_ascii_uppercase())
            .map_err(|e| e.to_string())
    };
    object.insert("version".into(), Value::String(format!("aurum {version}")));
    object.insert(
        "launcher_version".into(),
        Value::String(format!("aurum-studio {version}")),
    );
    object.insert(
        "binary_sha256".into(),
        Value::String(hash("bin/aurum.exe")?),
    );
    object.insert(
        "launcher_sha256".into(),
        Value::String(hash("Aurum Studio.exe")?),
    );
    object.insert(
        "runtime_sha256".into(),
        Value::String(hash("runtime/godot.exe")?),
    );
    object.insert("updated_utc".into(), Value::String(utc_now()));
    let scratch = home.join(format!(
        ".install.json.update-{}",
        crate::random::session_id()
    ));
    std::fs::write(
        &scratch,
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("'{}': {e}", scratch.display()))?;
    std::fs::rename(&scratch, &path).map_err(|e| {
        let _ = std::fs::remove_file(&scratch);
        format!("could not update install.json: {e}")
    })
}

/// The current time as `2026-11-10T15:04:05Z`, without a date library.
fn utc_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(seconds)
}

fn format_utc(seconds: u64) -> String {
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Start the installed Studio again after this one has shut down.
///
/// A detached shell waits a few seconds, for this process to release the
/// project, then opens the installed launcher. Returns once it is scheduled.
#[cfg(windows)]
pub fn schedule_restart(home: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    let launcher = home.join("Aurum Studio.exe");
    std::process::Command::new("cmd.exe")
        .raw_arg(format!(
            "/d /c ping -n 8 127.0.0.1 >nul & start \"\" \"{}\"",
            launcher.display()
        ))
        .current_dir(home)
        // CREATE_NO_WINDOW | DETACHED_PROCESS
        .creation_flags(0x0800_0000 | 0x0000_0008)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(not(windows))]
pub fn schedule_restart(_home: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "restarting is only available on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("0.4.2", "0.4.10"));
        assert!(is_newer("0.4.9", "v0.5.0"));
        assert!(!is_newer("0.4.2", "0.4.2"));
        assert!(!is_newer("0.4.2", "0.4.1"));
        assert!(!is_newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn only_plain_release_versions_are_accepted() {
        for bad in [
            "",
            "v",
            "0.4",
            "0.4.2.1",
            "0.4.2-rc1",
            "0.4.x",
            "a.b.c",
            "0..2",
            "1234567.0.0",
        ] {
            assert!(parse_version(bad).is_none(), "{bad:?}");
        }
        assert_eq!(parse_version(" v1.2.3 "), Some([1, 2, 3]));
        assert!(!is_newer("0.4.2", "0.5.0-beta"));
        assert!(!is_newer("garbage", "0.5.0"));
    }

    fn feed() -> Feed {
        Feed::GitHub
    }

    fn release_json(tag: &str) -> Value {
        let base = format!("https://github.com/{REPOSITORY}/releases/download/{tag}");
        let version = tag.trim_start_matches('v');
        json!({
            "tag_name": tag, "name": format!("Aurum Studio {version}"), "body": "Notes",
            "draft": false, "prerelease": false,
            "assets": [
                {"name": format!("Aurum-Studio-{version}-windows-x64.zip"), "size": 123,
                 "browser_download_url": format!("{base}/Aurum-Studio-{version}-windows-x64.zip")},
                {"name": "SHA256SUMS.txt", "size": 100, "browser_download_url": format!("{base}/SHA256SUMS.txt")}
            ]
        })
    }

    #[test]
    fn a_release_names_its_archive_and_checksums() {
        let release = parse_release(&release_json("v0.5.0"), &feed()).unwrap();
        assert_eq!(release.version, "0.5.0");
        assert_eq!(release.archive, "Aurum-Studio-0.5.0-windows-x64.zip");
        assert!(release.sums_url.ends_with("/v0.5.0/SHA256SUMS.txt"));
        assert_eq!(release.archive_bytes, 123);
    }

    #[test]
    fn a_release_file_hosted_anywhere_else_is_refused() {
        for url in [
            "https://evil.example/Aurum-Studio-0.5.0-windows-x64.zip",
            "http://github.com/AG064/aurum-studio/releases/download/v0.5.0/Aurum-Studio-0.5.0-windows-x64.zip",
            "https://github.com/AG064/aurum-studio/releases/download/v0.4.0/Aurum-Studio-0.5.0-windows-x64.zip",
            "https://github.com/AG064/aurum-studio/releases/download/v0.5.0/../v0.4.0/x.zip",
            "https://github.com/other/repo/releases/download/v0.5.0/Aurum-Studio-0.5.0-windows-x64.zip",
        ] {
            let mut value = release_json("v0.5.0");
            value["assets"][0]["browser_download_url"] = json!(url);
            assert!(parse_release(&value, &feed()).is_err(), "{url}");
        }
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_not_offered() {
        let mut draft = release_json("v0.5.0");
        draft["draft"] = json!(true);
        assert!(parse_release(&draft, &feed()).is_err());
        let mut pre = release_json("v0.5.0");
        pre["prerelease"] = json!(true);
        assert!(parse_release(&pre, &feed()).is_err());
        assert!(parse_release(&release_json("v0.5.0-rc1"), &feed()).is_err());
        assert!(parse_release(&release_json("0.5.0"), &feed()).is_err());
        let mut missing = release_json("v0.5.0");
        missing["assets"] = json!([]);
        assert!(parse_release(&missing, &feed()).is_err());
    }

    #[test]
    fn the_loopback_feed_accepts_only_loopback() {
        assert!(Feed::loopback("http://127.0.0.1:8080").is_ok());
        assert!(Feed::loopback("http://127.0.0.1:8080/").is_ok());
        for bad in [
            "https://127.0.0.1:8080",
            "http://localhost:8080",
            "http://127.0.0.1",
            "http://127.0.0.1:80/evil",
            "http://10.0.0.1:80",
            "http://127.0.0.1:80@evil.example",
            "",
        ] {
            assert!(Feed::loopback(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_checksum_is_read_for_exactly_the_named_archive() {
        let hash = "a".repeat(64);
        let other = "b".repeat(64);
        let sums = format!(
            "{other}  other.zip\n{}  Aurum-Studio-0.5.0-windows-x64.zip\n",
            hash.to_ascii_uppercase()
        );
        assert_eq!(
            expected_digest(&sums, "Aurum-Studio-0.5.0-windows-x64.zip").unwrap(),
            hash
        );
        assert_eq!(
            expected_digest(&format!("{hash} *x.zip\r\n"), "x.zip").unwrap(),
            hash
        );
        assert!(expected_digest(&sums, "missing.zip").is_err());
        assert!(expected_digest("nothex  x.zip\n", "x.zip").is_err());
        assert!(expected_digest(&format!("{hash}  x.zip\n{other}  x.zip\n"), "x.zip").is_err());
        assert!(expected_digest(&format!("{hash}  x.zip.sig\n"), "x.zip").is_err());
    }

    #[test]
    fn package_paths_that_could_escape_are_refused() {
        for bad in [
            "/etc/x",
            "../x",
            "a/../../x",
            "C:/x",
            "a\\b",
            "a//b",
            "./x",
            "",
        ] {
            assert!(!safe_entry(bad), "{bad:?}");
        }
        for good in [
            "Aurum Studio.exe",
            "bin/aurum.exe",
            "docs/",
            "runtime/templates/4.7.stable/a.zip",
        ] {
            assert!(safe_entry(good), "{good:?}");
        }
    }

    struct Serving(Vec<(String, Vec<u8>)>);
    impl Fetcher for Serving {
        fn fetch(&self, url: &str, destination: &Path) -> Result<(), String> {
            let (_, bytes) = self
                .0
                .iter()
                .find(|(name, _)| url.ends_with(name.as_str()))
                .ok_or("not found")?;
            std::fs::write(destination, bytes).map_err(|e| e.to_string())
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aurum-update-{name}-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_check_reports_a_newer_release_and_nothing_for_the_current_one() {
        let dir = scratch("check");
        let body = serde_json::to_vec(&release_json("v0.5.0")).unwrap();
        let fetcher = Serving(vec![("releases/latest".into(), body)]);
        let found = check("0.4.2", &feed(), &fetcher, &dir).unwrap().unwrap();
        assert_eq!(found.version, "0.5.0");
        assert!(check("0.5.0", &feed(), &fetcher, &dir).unwrap().is_none());
        assert!(check("0.6.0", &feed(), &fetcher, &dir).unwrap().is_none());
        let broken = Serving(vec![("releases/latest".into(), b"<html>".to_vec())]);
        assert!(check("0.4.2", &feed(), &broken, &dir).is_err());
        assert!(check("0.4.2", &feed(), &Serving(vec![]), &dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rollback_restores_what_was_replaced_and_removes_what_was_added() {
        let dir = scratch("rollback");
        let (kept, added, backup) = (
            dir.join("kept.txt"),
            dir.join("added.txt"),
            dir.join("kept.txt.bak"),
        );
        std::fs::write(&backup, "original").unwrap();
        std::fs::write(&kept, "new").unwrap();
        std::fs::write(&added, "new").unwrap();
        let mut placed = vec![
            Placed {
                destination: kept.clone(),
                backup: Some(backup.clone()),
            },
            Placed {
                destination: added.clone(),
                backup: None,
            },
        ];
        assert!(rollback(&mut placed).is_empty());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "original");
        assert!(!added.exists() && !backup.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unchanged_file_is_left_alone_and_a_changed_one_is_planned() {
        let dir = scratch("plan");
        let (home, root) = (dir.join("home"), dir.join("package"));
        for folder in [home.join("docs"), root.join("docs"), root.join("projects")] {
            std::fs::create_dir_all(folder).unwrap();
        }
        std::fs::write(home.join("docs/same.md"), "same").unwrap();
        std::fs::write(root.join("docs/same.md"), "same").unwrap();
        std::fs::write(home.join("docs/changed.md"), "old").unwrap();
        std::fs::write(root.join("docs/changed.md"), "new").unwrap();
        std::fs::write(root.join("docs/added.md"), "added").unwrap();
        // Not an owned name, so never planned even though the package has it.
        std::fs::write(root.join("projects/mine.txt"), "x").unwrap();
        std::fs::write(root.join("install.json"), "{}").unwrap();
        let (changes, unchanged) = plan(&home, &root).unwrap();
        let mut names: Vec<_> = changes
            .iter()
            .map(|c| c.relative.display().to_string().replace('\\', "/"))
            .collect();
        names.sort();
        assert_eq!(names, ["docs/added.md", "docs/changed.md"]);
        assert_eq!(unchanged, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_receipt_time_is_a_real_utc_date() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1794000000), "2026-11-06T21:20:00Z");
    }

    #[test]
    fn a_build_folder_is_not_an_installation() {
        let dir = scratch("install");
        let program = dir.join("bin").join("aurum.exe");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "x").unwrap();
        assert!(installation(&program).unwrap_err().contains("build folder"));
        std::fs::write(dir.join("install.json"), "{}").unwrap();
        assert!(installation(&program).is_ok());
        let elsewhere = dir.join("target").join("release").join("aurum.exe");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, "x").unwrap();
        assert!(installation(&elsewhere).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
