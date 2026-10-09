//! Development sessions: identity, state directory, and structured logs.
//!
//! A session is one supervised stretch of work on one project. It owns a
//! directory under the Studio state root holding its log, its ownership
//! records, and its metadata, so a crash leaves behind everything needed to
//! understand what was running.
//!
//! The token is a secret and is treated as one: it is written to the session
//! directory with the narrowest permissions the platform offers, and every log
//! line passes through redaction before it reaches disk.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::random;

/// Strings that must never appear in a log.
///
/// Redaction is applied on the way *in*, so a secret cannot reach the file
/// even if a caller logs it by accident.
pub const REDACTED: &str = "<redacted>";

/// One development session.
///
/// Deliberately **not** `Serialize`: the token must not reach the metadata
/// file, which is the artefact most likely to be attached to a bug report.
/// It is written to its own file instead, where permissions can be tightened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    /// The bridge token. Never logged, never serialized.
    pub token: String,
    pub project: PathBuf,
    /// When the session started, as the platform reported it.
    pub started: String,
    /// The root of this session's state directory.
    pub directory: PathBuf,
}

/// The shareable half of a session. Everything here is safe to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Metadata {
    id: String,
    project: PathBuf,
    started: String,
    directory: PathBuf,
}

impl Session {
    /// Where sessions live for this machine.
    pub fn state_root() -> Option<PathBuf> {
        // The registry already resolves the Studio root per platform; sessions
        // sit beside it so both follow one rule.
        let registry = crate::registry::Registry::resolve_path()?;
        Some(registry.parent()?.join("sessions"))
    }

    /// Start a new session for a project.
    pub fn create(project: &Path) -> std::io::Result<Self> {
        let root = Self::state_root().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no Studio state directory is available on this machine",
            )
        })?;

        let id = random::session_id();
        let directory = root.join(&id);
        std::fs::create_dir_all(&directory)?;

        let session = Self {
            id,
            token: random::token(),
            project: project.to_path_buf(),
            started: now_stamp(),
            directory,
        };
        session.write_metadata()?;
        session.write_token()?;
        crate::diagnostics::report(crate::diagnostics::retain_sessions(&root));
        crate::diagnostics::report(session.log(&format!(
            "session {} started for {}",
            session.id,
            session.project.display()
        )));
        Ok(session)
    }

    /// A session directory inside `root`, for callers that supply their own.
    pub fn create_in(root: &Path, project: &Path) -> std::io::Result<Self> {
        let id = random::session_id();
        let directory = root.join(&id);
        std::fs::create_dir_all(&directory)?;
        let session = Self {
            id,
            token: random::token(),
            project: project.to_path_buf(),
            started: now_stamp(),
            directory,
        };
        session.write_metadata()?;
        session.write_token()?;
        crate::diagnostics::report(crate::diagnostics::retain_sessions(root));
        crate::diagnostics::report(session.log(&format!(
            "session {} started for {}",
            session.id,
            session.project.display()
        )));
        Ok(session)
    }

    /// Load a session from its directory.
    pub fn load(directory: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(directory.join("session.json"))?;
        let metadata: Metadata = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // The token lives apart from the metadata, so both are needed.
        let token = std::fs::read_to_string(directory.join("token"))?
            .trim()
            .to_string();
        Ok(Self {
            id: metadata.id,
            token,
            project: metadata.project,
            started: metadata.started,
            directory: metadata.directory,
        })
    }

    /// The most recent session recorded for a project, if any.
    ///
    /// Used on startup to notice that a project process is already running, so
    /// the user can be offered a supervised relaunch rather than having Studio
    /// assume it owns something it does not.
    pub fn latest_for(project: &Path) -> Option<Self> {
        let root = Self::state_root()?;
        let entries = std::fs::read_dir(&root).ok()?;
        let mut sessions: Vec<Self> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter_map(|path| Self::load(&path).ok())
            .filter(|session| session.project == project)
            .collect();
        // The identifier is random, so ordering by it is meaningless; the
        // start stamp is what orders these.
        sessions.sort_by(|a, b| a.started.cmp(&b.started));
        sessions.pop()
    }

    /// Where ownership records for this session live.
    pub fn ownership_directory(&self) -> PathBuf {
        self.directory.join("processes")
    }

    /// The session log file.
    pub fn log_path(&self) -> PathBuf {
        self.directory.join("session.log")
    }

    fn write_metadata(&self) -> std::io::Result<()> {
        let metadata = Metadata {
            id: self.id.clone(),
            project: self.project.clone(),
            started: self.started.clone(),
            directory: self.directory.clone(),
        };
        let text = serde_json::to_string_pretty(&metadata)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.directory.join("session.json"), text)
    }

    /// Write the token separately, so it can be given narrow permissions
    /// without restricting the metadata file.
    fn write_token(&self) -> std::io::Result<()> {
        let path = self.directory.join("token");
        std::fs::write(&path, &self.token)?;
        restrict_to_owner(&path);
        Ok(())
    }

    /// Append a line to the session log, with this session's secrets removed.
    pub fn log(&self, line: &str) -> std::io::Result<()> {
        let safe = redact(line, &[&self.token]);
        append_line(&self.log_path(), &safe)
    }

    /// Append a line that is already known to be safe.
    ///
    /// Still redacted: the distinction is only that the caller is asserting
    /// there is nothing to remove, not that removal may be skipped.
    pub fn log_public(&self, line: &str) -> std::io::Result<()> {
        self.log(line)
    }
}

/// Append a line to a file, creating it if needed.
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    crate::diagnostics::append(
        path,
        &crate::diagnostics::text_line(line, &[]),
        crate::diagnostics::Policy::default(),
    )
}

/// Remove every secret from a line.
///
/// Short secrets are skipped: replacing a one-character string would mangle
/// the line into uselessness while protecting nothing.
pub fn redact(line: &str, secrets: &[&str]) -> String {
    // Compiler/tool output is free-form. Omit credential-bearing lines rather
    // than pretending to parse every possible quoted or encoded value safely.
    let lower = line.to_ascii_lowercase();
    if [
        "authorization:",
        "authorization=",
        "bearer ",
        "cookie:",
        "cookie=",
        "password=",
        "password:",
        "\"password\"",
        "api_key=",
        "api_key:",
        "\"api_key\"",
        "api-key=",
        "token=",
        "token:",
        "\"token\"",
        "secret=",
        "secret:",
        "?t=",
        "&t=",
        "x-aurum-token",
        "sk-ant-",
        "sk-proj-",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
    {
        return format!("{REDACTED} credential-bearing line omitted");
    }
    let mut out = line.to_string();
    for secret in secrets {
        if secret.len() < 8 {
            continue;
        }
        if out.contains(secret) {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

/// A timestamp the platform reports, in a stable sortable form.
fn now_stamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format!("{}.{:09}", duration.as_secs(), duration.subsec_nanos()),
        Err(_) => "0.000000000".to_string(),
    }
}

/// Best-effort restriction of a file to its owner.
///
/// Failure is not fatal: the token still lives inside the user's own state
/// directory, and refusing to start a session over a permission tightening is
/// worse than proceeding without it.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) {
    use std::process::{Command as StdCommand, Stdio};
    let script = format!(
        "$p = '{}'; $a = Get-Acl $p; $a.SetAccessRuleProtection($true, $false); \
         $r = New-Object System.Security.AccessControl.FileSystemAccessRule($env:USERNAME, 'FullControl', 'Allow'); \
         $a.SetAccessRule($r); Set-Acl -Path $p -AclObject $a",
        path.display()
    );
    let _ = StdCommand::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn restrict_to_owner(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-session-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_session_gets_an_identifier_a_token_and_a_directory() {
        let root = temp_root("create");
        let session = Session::create_in(&root, Path::new("A:/project")).unwrap();

        assert_eq!(session.id.len(), 32);
        assert_eq!(session.token.len(), 64, "the bridge expects 64 characters");
        assert!(session.directory.is_dir());
        assert!(
            session.log_path().is_file(),
            "the log should exist from the start"
        );
        assert_eq!(
            session.ownership_directory(),
            session.directory.join("processes")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_sessions_never_share_an_identity() {
        let root = temp_root("unique");
        let first = Session::create_in(&root, Path::new("A:/p")).unwrap();
        let second = Session::create_in(&root, Path::new("A:/p")).unwrap();
        assert_ne!(first.id, second.id);
        assert_ne!(
            first.token, second.token,
            "a shared token would let one session authenticate as another"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn metadata_round_trips() {
        let root = temp_root("roundtrip");
        let session = Session::create_in(&root, Path::new("A:/project")).unwrap();

        let reloaded = Session::load(&session.directory).unwrap();
        assert_eq!(reloaded.id, session.id);
        assert_eq!(reloaded.token, session.token);
        assert_eq!(reloaded.project, session.project);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_token_is_kept_out_of_the_metadata_file() {
        // Metadata is the file most likely to be shared when reporting a
        // problem; the secret should not be in it.
        let root = temp_root("secrecy");
        let session = Session::create_in(&root, Path::new("A:/project")).unwrap();
        let metadata = std::fs::read_to_string(session.directory.join("session.json")).unwrap();
        assert!(
            !metadata.contains(&session.token),
            "metadata should not carry the token"
        );
        // It is written separately, where permissions can be tightened.
        let token_file = std::fs::read_to_string(session.directory.join("token")).unwrap();
        assert_eq!(token_file.trim(), session.token);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_token_never_reaches_the_log_even_when_logged_by_accident() {
        let root = temp_root("redact");
        let session = Session::create_in(&root, Path::new("A:/project")).unwrap();

        session
            .log(&format!("connecting with token {}", session.token))
            .unwrap();
        let text = std::fs::read_to_string(session.log_path()).unwrap();

        assert!(
            !text.contains(&session.token),
            "the token leaked into the log"
        );
        assert!(text.contains(REDACTED), "the log should show a redaction");
        assert!(
            text.contains("connecting with token"),
            "the rest should survive"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn redaction_removes_every_occurrence() {
        let secret = "0123456789abcdef";
        let line = format!("{secret} appears twice: {secret}");
        let safe = redact(&line, &[secret]);
        assert!(!safe.contains(secret));
        assert_eq!(safe.matches(REDACTED).count(), 2);
    }

    #[test]
    fn redaction_ignores_secrets_too_short_to_be_worth_mangling_for() {
        // Replacing every "a" would destroy the line and protect nothing.
        let line = "a normal log line";
        assert_eq!(redact(line, &["a", "e"]), line);
        // Empty entries are ignored too.
        assert_eq!(redact(line, &[""]), line);
    }

    #[test]
    fn redaction_leaves_clean_lines_untouched() {
        let line = "building aurum-godot (debug)";
        assert_eq!(redact(line, &["0123456789abcdef"]), line);
    }

    #[test]
    fn the_log_appends_rather_than_replacing() {
        let root = temp_root("append");
        let session = Session::create_in(&root, Path::new("A:/p")).unwrap();
        session.log("first").unwrap();
        session.log("second").unwrap();

        let text = std::fs::read_to_string(session.log_path()).unwrap();
        let first = text.find("first").unwrap();
        let second = text.find("second").unwrap();
        assert!(first < second, "later lines should come later");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stamps_are_sortable_as_text() {
        let root = temp_root("stamps");
        let first = Session::create_in(&root, Path::new("A:/p")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = Session::create_in(&root, Path::new("A:/p")).unwrap();
        // `latest_for` and any log reader depend on this ordering.
        assert!(
            first.started < second.started,
            "{} vs {}",
            first.started,
            second.started
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn appending_creates_missing_directories() {
        let root = temp_root("mkdir");
        let path = root.join("a/b/c/log.txt");
        append_line(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "hello");
        let _ = std::fs::remove_dir_all(&root);
    }
}
