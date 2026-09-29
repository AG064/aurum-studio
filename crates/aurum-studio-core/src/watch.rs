//! Watching a project for changes.
//!
//! A polling watcher rather than a platform notification API. Every OS has its
//! own mechanism, each has its own failure modes and edge cases, and using
//! them would mean either a dependency or three platform implementations. A
//! stat-based scan is portable, predictable, and costs microseconds on a
//! project this size — which is the right trade for something whose job is to
//! notice a file changed a moment ago.
//!
//! Two behaviours matter more than speed:
//!
//! - **Ignore rules are applied during the walk**, so build output never
//!   reaches the classifier. A rebuild that touched `target/` and then
//!   triggered another rebuild would be an infinite loop.
//! - **The first scan primes without reporting.** Otherwise starting a session
//!   would announce every file in the project as newly added.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// A directory whose contents never matter to a running editor.
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    "target",
    ".godot",
    ".aurum",
    "node_modules",
    ".vs",
    ".idea",
    "__pycache__",
];

/// Ignored by suffix, for generated files that appear beside sources.
const IGNORED_SUFFIXES: &[&str] = &[".tmp", ".staging", ".previous", ".log", "~"];

/// What happened to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Removed,
}

impl ChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Removed => "removed",
        }
    }
}

/// One observed change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// A file's identity for change detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    length: u64,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            modified: metadata.modified().ok(),
            length: metadata.len(),
        })
    }
}

/// A snapshot of a project's files, and the changes since the last one.
#[derive(Debug, Clone)]
pub struct Watcher {
    root: PathBuf,
    stamps: HashMap<PathBuf, Stamp>,
    primed: bool,
    sources: HashMap<PathBuf, String>,
    verdicts: HashMap<PathBuf, crate::Classification>,
}

impl Watcher {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            stamps: HashMap::new(),
            primed: false,
            sources: HashMap::new(),
            verdicts: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the first scan has run.
    pub fn is_primed(&self) -> bool {
        self.primed
    }

    /// How many files are being tracked.
    pub fn tracked(&self) -> usize {
        self.stamps.len()
    }

    /// Scan and return what changed since the previous scan.
    ///
    /// The first call primes the snapshot and returns nothing: a session that
    /// announced every file as new on startup would be noise, and would
    /// classify the whole project on launch.
    pub fn scan(&mut self) -> Vec<Change> {
        let mut current: HashMap<PathBuf, Stamp> = HashMap::new();
        collect(&self.root, &mut current);

        let mut changes = if self.primed {
            let mut changes = Vec::new();
            for (path, stamp) in &current {
                match self.stamps.get(path) {
                    // A file can change without its length changing, so the
                    // modification time is what decides.
                    Some(previous) if previous == stamp => {}
                    Some(_) => changes.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Modified,
                    }),
                    None => changes.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Added,
                    }),
                }
            }
            for path in self.stamps.keys() {
                if !current.contains_key(path) {
                    changes.push(Change {
                        path: path.clone(),
                        kind: ChangeKind::Removed,
                    });
                }
            }
            changes
        } else {
            Vec::new()
        };

        for change in &changes {
            let after = if change.path.extension().is_some_and(|e| e == "rs") {
                std::fs::read_to_string(&change.path).ok()
            } else {
                None
            };
            let classification = crate::native_schema::classify(
                &change.path,
                self.sources.get(&change.path).map(String::as_str),
                after.as_deref(),
            );
            self.verdicts
                .entry(change.path.clone())
                .and_modify(|old| {
                    if classification.verdict > old.verdict {
                        *old = classification.clone();
                    }
                })
                .or_insert(classification);
        }
        for path in current
            .keys()
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        {
            if !self.primed || changes.iter().any(|c| &c.path == path) {
                if let Ok(text) = std::fs::read_to_string(path) {
                    self.sources.insert(path.clone(), text);
                }
            }
        }
        self.sources.retain(|path, _| current.contains_key(path));
        self.stamps = current;
        self.primed = true;

        // Deterministic order, so a caller's decisions never depend on hash
        // iteration order.
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }

    pub fn classify(&mut self, changes: &[Change]) -> crate::Classification {
        changes
            .iter()
            .map(|change| {
                self.verdicts
                    .remove(&change.path)
                    .unwrap_or_else(|| crate::reload::classify_path(&change.path))
            })
            .max_by_key(|classification| classification.verdict)
            .unwrap_or_else(|| crate::reload::classify_path(Path::new("empty.txt")))
    }
}

/// Walk a tree, recording file stamps and skipping ignored directories.
fn collect(directory: &Path, into: &mut HashMap<PathBuf, Stamp>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if IGNORED_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            collect(&path, into);
            continue;
        }

        if IGNORED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix)) {
            continue;
        }
        if let Some(stamp) = Stamp::of(&path) {
            into.insert(path, stamp);
        }
    }
}

/// Collects changes until the tree stops changing.
///
/// A single save can produce several filesystem events, and a build writes
/// many files at once. Acting on the first event would start a build whose
/// inputs are still being written; waiting for quiet means one build per
/// logical edit.
#[derive(Debug, Clone)]
pub struct Debouncer {
    quiet: Duration,
    pending: Vec<Change>,
    last_change: Option<Instant>,
}

impl Debouncer {
    /// Create a debouncer that fires once the tree has been quiet for `quiet`.
    pub fn new(quiet: Duration) -> Self {
        Self {
            quiet,
            pending: Vec::new(),
            last_change: None,
        }
    }

    /// Add newly observed changes.
    pub fn push(&mut self, changes: impl IntoIterator<Item = Change>) {
        for change in changes {
            // A path seen twice keeps one entry, with the later kind winning:
            // a file modified then removed is a removal.
            if let Some(existing) = self.pending.iter_mut().find(|c| c.path == change.path) {
                existing.kind = change.kind;
            } else {
                self.pending.push(change);
            }
            self.last_change = Some(Instant::now());
        }
    }

    /// Whether anything is waiting.
    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Take the batch if it has settled, otherwise `None`.
    pub fn take_if_settled(&mut self) -> Option<Vec<Change>> {
        let last = self.last_change?;
        if last.elapsed() < self.quiet {
            return None;
        }
        self.last_change = None;
        let mut batch = std::mem::take(&mut self.pending);
        batch.sort_by(|a, b| a.path.cmp(&b.path));
        Some(batch)
    }

    /// How long until the batch would settle.
    pub fn time_remaining(&self) -> Option<Duration> {
        let last = self.last_change?;
        Some(self.quiet.saturating_sub(last.elapsed()))
    }
}

/// How long a project must be quiet before a batch is acted on.
pub const DEFAULT_QUIET: Duration = Duration::from_millis(400);

/// How often to scan.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(250);

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-watch-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a file, creating its directory if it is not there.
    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Rewrite a file until the watcher notices, or give up.
    ///
    /// A watcher keyed on `(modified, length)` cannot see a rewrite that lands
    /// inside the filesystem's timestamp granularity: the length is the same by
    /// design, and the clock has not moved. That is a property of the clock
    /// rather than of the watcher, and it is why `a_modified_file_is_reported`
    /// passed here and failed on CI — same code, coarser timestamps.
    ///
    /// Sleeping a magic number would trade a flake for a slower flake. Writing
    /// until the clock moves is the honest version, and the attempt limit means
    /// a filesystem that never moves it fails the test instead of hanging it.
    fn rewrite_until_seen(watcher: &mut Watcher, path: &Path, content: &str) -> Vec<Change> {
        for _ in 0..100 {
            write(path, content);
            let changes = watcher.scan();
            if !changes.is_empty() {
                return changes;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the watcher never saw {path:?} change, across a hundred rewrites");
    }

    #[test]
    fn the_first_scan_primes_without_reporting() {
        let root = temp_dir("prime");
        write(&root.join("a.gd"), "extends Node");
        write(&root.join("b.rs"), "fn main() {}");

        let mut watcher = Watcher::new(&root);
        assert!(!watcher.is_primed());
        let changes = watcher.scan();
        assert!(
            changes.is_empty(),
            "a fresh watcher should not announce the whole project: {changes:?}"
        );
        assert!(watcher.is_primed());
        assert_eq!(watcher.tracked(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_added_file_is_reported_once() {
        let root = temp_dir("added");
        write(&root.join("a.gd"), "one");
        let mut watcher = Watcher::new(&root);
        watcher.scan();

        write(&root.join("b.gd"), "two");
        let changes = watcher.scan();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Added);
        assert!(changes[0].path.ends_with("b.gd"));

        // A second scan with nothing happening reports nothing.
        assert!(watcher.scan().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_modified_file_is_reported() {
        let root = temp_dir("modified");
        let file = root.join("a.rs");
        write(&file, "fn a() {}");
        let mut watcher = Watcher::new(&root);
        watcher.scan();

        // Same length, different bytes. The length cannot give this away, so
        // the timestamp has to, which is why the rewrite is repeated until the
        // clock agrees rather than fired once and hoped for.
        let changes = rewrite_until_seen(&mut watcher, &file, "fn b() {}");
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].kind, ChangeKind::Modified);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_removed_file_is_reported() {
        let root = temp_dir("removed");
        let file = root.join("gone.gd");
        write(&file, "x");
        let mut watcher = Watcher::new(&root);
        watcher.scan();

        std::fs::remove_file(&file).unwrap();
        let changes = watcher.scan();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::Removed);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_output_is_never_watched() {
        // Ignoring `target/` is not an optimisation: a build writes there, and
        // watching it would make every build trigger another build.
        let root = temp_dir("ignored");
        write(&root.join("src/main.rs"), "fn main() {}");
        write(&root.join("target/debug/artifact.dll"), "binary");
        write(&root.join("godot/.godot/imported/x.scn"), "cache");
        write(&root.join(".git/HEAD"), "ref");
        write(&root.join("node_modules/pkg/index.js"), "x");

        let mut watcher = Watcher::new(&root);
        watcher.scan();
        assert_eq!(
            watcher.tracked(),
            1,
            "only the source file should be tracked"
        );

        // A change inside an ignored directory must not surface.
        write(&root.join("target/debug/artifact.dll"), "rebuilt");
        assert!(watcher.scan().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn generated_temp_files_are_ignored() {
        let root = temp_dir("suffixes");
        write(&root.join("a.rs"), "x");
        write(&root.join("a.dll.staging"), "x");
        write(&root.join("a.dll.previous"), "x");
        write(&root.join("session.log"), "x");
        write(&root.join("editor.tmp"), "x");

        let mut watcher = Watcher::new(&root);
        watcher.scan();
        assert_eq!(watcher.tracked(), 1, "only a.rs should be tracked");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changes_come_back_in_a_deterministic_order() {
        let root = temp_dir("order");
        let mut watcher = Watcher::new(&root);
        watcher.scan();

        for name in ["z.gd", "a.gd", "m.gd", "b.gd"] {
            write(&root.join(name), "x");
        }
        let changes = watcher.scan();
        let names: Vec<String> = changes
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["a.gd", "b.gd", "m.gd", "z.gd"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_nested_tree_is_walked() {
        let root = temp_dir("nested");
        write(&root.join("a/b/c/deep.gd"), "x");
        write(&root.join("a/shallow.gd"), "x");

        let mut watcher = Watcher::new(&root);
        watcher.scan();
        assert_eq!(watcher.tracked(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn watching_a_missing_directory_reports_nothing() {
        let mut watcher = Watcher::new("definitely-not-here");
        assert!(watcher.scan().is_empty());
        assert!(watcher.scan().is_empty());
    }

    #[test]
    fn the_debouncer_waits_for_quiet() {
        let mut debouncer = Debouncer::new(Duration::from_millis(120));
        debouncer.push([Change {
            path: PathBuf::from("a.rs"),
            kind: ChangeKind::Modified,
        }]);
        assert!(debouncer.is_pending());
        // Immediately after a change nothing should fire.
        assert!(debouncer.take_if_settled().is_none());

        std::thread::sleep(Duration::from_millis(200));
        let batch = debouncer.take_if_settled().expect("should have settled");
        assert_eq!(batch.len(), 1);
        assert!(!debouncer.is_pending());
        // Taking it clears it.
        assert!(debouncer.take_if_settled().is_none());
    }

    #[test]
    fn a_burst_of_changes_becomes_one_batch() {
        // A build writes many files at once; acting on the first would start a
        // build whose inputs are still being written.
        let mut debouncer = Debouncer::new(Duration::from_millis(100));
        for index in 0..5 {
            debouncer.push([Change {
                path: PathBuf::from(format!("file{index}.rs")),
                kind: ChangeKind::Modified,
            }]);
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(150));

        let batch = debouncer.take_if_settled().expect("should have settled");
        assert_eq!(batch.len(), 5, "one batch, not five");
    }

    #[test]
    fn a_path_seen_twice_keeps_one_entry_with_the_later_kind() {
        let mut debouncer = Debouncer::new(Duration::from_millis(50));
        debouncer.push([Change {
            path: PathBuf::from("a.rs"),
            kind: ChangeKind::Added,
        }]);
        debouncer.push([Change {
            path: PathBuf::from("a.rs"),
            kind: ChangeKind::Modified,
        }]);
        std::thread::sleep(Duration::from_millis(80));

        let batch = debouncer.take_if_settled().unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].kind, ChangeKind::Modified);
    }

    #[test]
    fn time_remaining_counts_down() {
        let mut debouncer = Debouncer::new(Duration::from_millis(500));
        assert_eq!(debouncer.time_remaining(), None);
        debouncer.push([Change {
            path: PathBuf::from("a"),
            kind: ChangeKind::Added,
        }]);
        let remaining = debouncer.time_remaining().unwrap();
        assert!(remaining <= Duration::from_millis(500));
    }

    #[test]
    fn change_kinds_have_distinct_labels() {
        assert_eq!(ChangeKind::Added.label(), "added");
        assert_eq!(ChangeKind::Modified.label(), "modified");
        assert_eq!(ChangeKind::Removed.label(), "removed");
    }
}
