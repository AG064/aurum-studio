//! Journaled text revision publication and conflict-preserving recovery.
use crate::{files, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_ROWS: usize = 256;
const MAX_BACKUP_BYTES: usize = 32 * 1024 * 1024;
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
const MAX_HISTORY_BYTES: u64 = 128 * 1024 * 1024;
type ChangeBytes = (String, Option<Vec<u8>>, Option<Vec<u8>>);

/// Audit provenance only. Deserializing it never supplies mutation authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalBinding {
    pub(crate) version: u8,
    pub(crate) imported_path: String,
    pub(crate) imported_identity: String,
    pub(crate) imported_revision: String,
    pub(crate) external_path: String,
    pub(crate) external_identity: String,
    pub(crate) root_id: String,
    pub(crate) member_src: String,
    pub(crate) proposal_digest: String,
    pub(crate) authorization_digest: String,
    pub(crate) targets: Vec<ExternalTarget>,
    pub(crate) cleanup_dirs: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalTarget {
    pub(crate) path: String,
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalDirectory {
    pub(crate) path: String,
    pub(crate) identity: String,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct ExternalRepairScope {
    pub(crate) binding: ExternalBinding,
    pub(crate) owned_dirs: Vec<ExternalDirectory>,
}
#[cfg(windows)]
pub(crate) trait ExternalReadIo {
    fn active(
        &self,
    ) -> Result<Option<(crate::external_workspace::ExternalObject, Vec<u8>)>, String>;
    fn read(
        &self,
        revision: &str,
        name: &str,
        maximum: u64,
    ) -> Result<Option<(crate::external_workspace::ExternalObject, Vec<u8>)>, String>;
    fn entries(&self, revision: &str) -> Result<Vec<(String, bool, u64)>, String>;
    fn directory(
        &self,
        revision: &str,
    ) -> Result<crate::external_workspace::DirectoryIdentity, String>;
    fn target(
        &self,
        path: &str,
    ) -> Result<Option<(crate::external_workspace::ExternalObject, Vec<u8>)>, String>;
    fn source_directory(
        &self,
        path: &str,
    ) -> Result<Option<crate::external_workspace::DirectoryIdentity>, String>;
    fn saved_directory(
        &self,
        revision: &str,
        name: &str,
    ) -> Result<Option<crate::external_workspace::DirectoryIdentity>, String>;
    fn source_entries(&self, path: &str) -> Result<Option<Vec<(String, bool, u64)>>, String>;
}
#[cfg(windows)]
type Object = crate::external_workspace::ExternalObject;
#[cfg(windows)]
type Identity = crate::external_workspace::DirectoryIdentity;
/// A recorded location inside the same approved root. This is evidence only;
/// native operations separately require the consumed sealed repair admission.
#[cfg(windows)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryOrigin {
    pub(crate) path: String,
    pub(crate) revision: String,
    pub(crate) directory: Identity,
    pub(crate) name: String,
    pub(crate) identity: Identity,
}
#[cfg(windows)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct V3Header {
    version: u8,
    journal: Journal,
    directory: Identity,
    originals: Vec<Option<Object>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retire_dirs: Vec<(String, Identity)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    return_dirs: Vec<DirectoryOrigin>,
}
#[cfg(windows)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum V3Action {
    State {
        state: State,
    },
    Blob {
        row: usize,
        before: bool,
        object: Object,
    },
    TempReady {
        row: usize,
        object: Object,
    },
    OldMoveIntent {
        row: usize,
        object: Object,
    },
    OldMoved {
        row: usize,
        object: Object,
    },
    PublishIntent {
        row: usize,
        object: Object,
    },
    Published {
        row: usize,
        object: Object,
    },
    RestoreIntent {
        row: usize,
    },
    Restored {
        row: usize,
    },
    DirectoryIntent {
        path: String,
    },
    DirectoryOwned {
        path: String,
        identity: Identity,
    },
    DirectoryRemoveIntent {
        path: String,
        identity: Identity,
    },
    DirectoryRemoved {
        path: String,
    },
    DirectoryRetireIntent {
        index: usize,
    },
    DirectoryRetired {
        index: usize,
    },
    DirectoryReturnIntent {
        index: usize,
    },
    DirectoryReturned {
        index: usize,
    },
    DirectoryUnretireIntent {
        index: usize,
    },
    DirectoryUnretired {
        index: usize,
    },
    DirectoryUnreturnIntent {
        index: usize,
    },
    DirectoryUnreturned {
        index: usize,
    },
}
#[cfg(windows)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct V3Event {
    version: u8,
    sequence: usize,
    previous: Object,
    action: V3Action,
}
#[cfg(windows)]
#[derive(Clone)]
pub(crate) struct ExternalV3Snapshot {
    header: V3Header,
    header_object: Object,
    head: Object,
    sequence: usize,
    state: State,
    blobs: Vec<(Option<(Object, Vec<u8>)>, Option<(Object, Vec<u8>)>)>,
    temps: Vec<Option<Object>>,
    olds: Vec<Option<Object>>,
    targets: Vec<Option<Object>>,
    owned_dirs: Vec<(String, Identity)>,
    active: Option<Object>,
    digest: String,
    immutable_records: Vec<(String, Object)>,
    retire_intents: std::collections::BTreeSet<usize>,
    return_intents: std::collections::BTreeSet<usize>,
}
#[cfg(windows)]
impl ExternalV3Snapshot {
    pub(crate) fn directory_scope(
        &self,
        undo: bool,
    ) -> (Vec<(String, Identity)>, Vec<DirectoryOrigin>) {
        if undo {
            let mut retire = self.owned_dirs.clone();
            retire.extend(
                self.header
                    .return_dirs
                    .iter()
                    .map(|d| (d.path.clone(), d.identity.clone())),
            );
            retire.sort_by(|a, b| a.0.cmp(&b.0));
            let restore = self
                .header
                .retire_dirs
                .iter()
                .enumerate()
                .map(|(index, (path, identity))| DirectoryOrigin {
                    path: path.clone(),
                    revision: self.header.journal.id.clone(),
                    directory: self.header.directory.clone(),
                    name: directory_slot(index),
                    identity: identity.clone(),
                })
                .collect();
            (retire, restore)
        } else {
            (
                self.header.retire_dirs.clone(),
                self.header.return_dirs.clone(),
            )
        }
    }
    pub(crate) fn scope(&self) -> Result<ExternalRepairScope, String> {
        let binding = self
            .header
            .journal
            .external
            .clone()
            .ok_or("external V3 binding missing")?;
        Ok(ExternalRepairScope {
            binding,
            owned_dirs: self
                .owned_dirs
                .iter()
                .map(|(path, identity)| ExternalDirectory {
                    path: path.clone(),
                    identity: crate::sha256_hex(format!("{identity:?}").as_bytes()),
                })
                .collect(),
        })
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}
#[cfg(windows)]
fn directory_slot(index: usize) -> String {
    format!("d{index:04}.dir")
}
#[cfg(windows)]
fn slot(row: usize, kind: &str) -> String {
    format!("{row:03}.{kind}")
}
#[cfg(windows)]
fn v3_conflict(reason: &str) -> String {
    format!("external_source_v3_conflict:{reason}; evidence preserved")
}
#[cfg(windows)]
pub(crate) fn inspect_external_v3(
    io: &dyn ExternalReadIo,
    revision: &str,
    undo: bool,
) -> Result<ExternalV3Snapshot, String> {
    id(revision)?;
    let (header_object, bytes) = io.read(revision, "v3.json", MAX_JOURNAL_BYTES)?.ok_or(
        "external_source_v3_header_missing_legacy_v2_or_unowned_state_preserved_unsupported",
    )?;
    let header: V3Header =
        serde_json::from_slice(&bytes).map_err(|_| v3_conflict("invalid header"))?;
    if header.version != 3
        || header.journal.version != 3
        || header.journal.id != revision
        || header.journal.rows.is_empty()
        || header.journal.rows.len() > crate::revisions::MAX_FILES
        || header.originals.len() != header.journal.rows.len()
        || io.directory(revision)? != header.directory
        || header.journal.state != State::Prepared
        || !header.journal.created_dirs.is_empty()
        || !header.journal.external_owned_dirs.is_empty()
        || header
            .journal
            .undo_of
            .as_ref()
            .is_some_and(|value| id(value).is_err())
    {
        return Err(v3_conflict("header identity or bounds"));
    }
    let binding = header
        .journal
        .external
        .as_ref()
        .ok_or("external V3 binding missing")?;
    validate_external_binding(binding, &header.journal.rows)?;
    validate_cleanup_scope(binding)?;
    validate_directory_changes(
        binding,
        &header.journal.rows,
        &header.retire_dirs,
        &header.return_dirs,
    )?;
    if (!header.retire_dirs.is_empty() || !header.return_dirs.is_empty())
        && header.journal.undo_of.is_none()
    {
        return Err(v3_conflict("directory reversal requires an undo revision"));
    }
    if header
        .retire_dirs
        .iter()
        .any(|(_, id)| !id.same_volume(&header.directory))
        || header.return_dirs.iter().any(|d| {
            !d.identity.same_volume(&header.directory)
                || !d.directory.same_volume(&header.directory)
        })
    {
        return Err(v3_conflict("directory volume"));
    }
    let count = header.journal.rows.len();
    for (row, original) in header.journal.rows.iter().zip(&header.originals) {
        if row.before.as_deref() != original.as_ref().map(|v| v.sha256.as_str())
            || original.as_ref().is_some_and(|v| {
                v.bytes > crate::revisions::MAX_FILE_BYTES as u64
                    || !v.identity.same_volume(&header.directory)
            })
            || (row.before.is_none() && row.after.is_none())
            || [&row.before, &row.after].into_iter().flatten().any(|h| {
                h.len() != 64
                    || !h
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(v3_conflict("original scope or volume"));
        }
    }
    let mut snapshot = ExternalV3Snapshot {
        header,
        header_object: header_object.clone(),
        head: header_object.clone(),
        sequence: 0,
        state: State::Prepared,
        blobs: vec![(None, None); count],
        temps: vec![None; count],
        olds: vec![None; count],
        targets: Vec::new(),
        owned_dirs: Vec::new(),
        active: None,
        digest: String::new(),
        immutable_records: vec![("v3.json".into(), header_object.clone())],
        retire_intents: std::collections::BTreeSet::new(),
        return_intents: std::collections::BTreeSet::new(),
    };
    let mut entries = io.entries(revision)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.len() > 8192 + count * 5 + snapshot.header.retire_dirs.len() + 1 {
        return Err(v3_conflict("record count"));
    }
    let event_names: Vec<_> = entries
        .iter()
        .filter(|(n, d, _)| !d && n.starts_with('e') && n.ends_with(".json"))
        .map(|e| e.0.clone())
        .collect();
    let mut observed_events = Vec::new();
    let mut directory_intents = std::collections::BTreeSet::new();
    let mut removed_dirs = std::collections::BTreeSet::new();
    for name in event_names {
        if name != format!("e{:06}.json", snapshot.sequence) || snapshot.sequence >= 8192 {
            return Err(v3_conflict("event gap"));
        }
        let (object, data) = io
            .read(revision, &name, 4096)?
            .ok_or_else(|| v3_conflict("event missing"))?;
        let event: V3Event =
            serde_json::from_slice(&data).map_err(|_| v3_conflict("partial or corrupt event"))?;
        if event.version != 3
            || event.sequence != snapshot.sequence
            || event.previous != snapshot.head
        {
            return Err(v3_conflict("event chain changed"));
        }
        let checked_row = |row: usize| -> Result<(), String> {
            if row >= count {
                Err(v3_conflict("event row"))
            } else {
                Ok(())
            }
        };
        match event.action {
            V3Action::State { state } => {
                let valid = if snapshot.sequence == 0 {
                    state == State::Prepared
                } else {
                    match (&snapshot.state, &state) {
                        (State::Prepared, State::Publishing | State::RollingBack)
                        | (State::Publishing, State::Committed)
                        | (
                            State::Publishing | State::RecoveryConflict | State::RollingBack,
                            State::RollingBack,
                        )
                        | (State::RollingBack, State::RolledBack | State::RecoveryConflict) => true,
                        _ => false,
                    }
                };
                if !valid {
                    return Err(v3_conflict("state transition"));
                }
                snapshot.state = state;
            }
            V3Action::Blob {
                row,
                before,
                object,
            } => {
                checked_row(row)?;
                let expected = if before {
                    &snapshot.header.journal.rows[row].before
                } else {
                    &snapshot.header.journal.rows[row].after
                };
                if expected.as_ref() != Some(&object.sha256) {
                    return Err(v3_conflict("blob scope"));
                }
                let actual = io
                    .read(
                        revision,
                        &slot(row, if before { "before" } else { "after" }),
                        crate::revisions::MAX_FILE_BYTES as u64,
                    )?
                    .ok_or_else(|| v3_conflict("blob missing"))?;
                if actual.0 != object || std::str::from_utf8(&actual.1).is_err() {
                    return Err(v3_conflict("blob identity or bytes"));
                }
                let used: u64 = snapshot
                    .blobs
                    .iter()
                    .map(|(before_blob, after_blob)| {
                        (if before {
                            before_blob.as_ref()
                        } else {
                            after_blob.as_ref()
                        })
                        .map_or(0, |v| v.0.bytes)
                    })
                    .sum();
                if used
                    .checked_add(object.bytes)
                    .is_none_or(|v| v > crate::revisions::MAX_TEXT_BYTES as u64)
                {
                    return Err(v3_conflict("backup aggregate limit"));
                }
                let entry = if before {
                    &mut snapshot.blobs[row].0
                } else {
                    &mut snapshot.blobs[row].1
                };
                if entry.is_some() {
                    return Err(v3_conflict("duplicate blob claim"));
                }
                *entry = Some(actual);
            }
            V3Action::TempReady { row, object } => {
                checked_row(row)?;
                if snapshot.header.journal.rows[row].after.as_ref() != Some(&object.sha256)
                    || snapshot.temps[row].is_some()
                {
                    return Err(v3_conflict("temp scope"));
                }
                snapshot.temps[row] = Some(object);
            }
            V3Action::OldMoveIntent { row, object } => {
                checked_row(row)?;
                if snapshot.header.originals[row].as_ref() != Some(&object) {
                    return Err(v3_conflict("old intent scope"));
                }
                snapshot.olds[row] = Some(object);
            }
            V3Action::OldMoved { row, object } => {
                checked_row(row)?;
                if snapshot.olds[row].as_ref() != Some(&object) {
                    return Err(v3_conflict("old observation scope"));
                }
            }
            V3Action::PublishIntent { row, object } | V3Action::Published { row, object } => {
                checked_row(row)?;
                if snapshot.temps[row].as_ref() != Some(&object) {
                    return Err(v3_conflict("published scope"));
                }
            }
            V3Action::RestoreIntent { row } | V3Action::Restored { row } => {
                checked_row(row)?;
            }
            V3Action::DirectoryIntent { path } => {
                if !binding_cleanup(&snapshot, &path) || !directory_intents.insert(path) {
                    return Err(v3_conflict("directory intent scope"));
                }
            }
            V3Action::DirectoryOwned { path, identity } => {
                if !binding_cleanup(&snapshot, &path)
                    || !directory_intents.contains(&path)
                    || !identity.same_volume(&snapshot.header.directory)
                    || snapshot.owned_dirs.iter().any(|(p, _)| p == &path)
                {
                    return Err(v3_conflict("directory ownership scope"));
                }
                snapshot.owned_dirs.push((path, identity));
            }
            V3Action::DirectoryRemoveIntent { path, identity } => {
                if !snapshot
                    .owned_dirs
                    .iter()
                    .any(|(p, i)| p == &path && i == &identity)
                {
                    return Err(v3_conflict("directory removal scope"));
                }
            }
            V3Action::DirectoryRemoved { path } => {
                if !snapshot.owned_dirs.iter().any(|(p, _)| p == &path) {
                    return Err(v3_conflict("removed directory ownership"));
                }
                removed_dirs.insert(path.clone());
                snapshot.owned_dirs.retain(|(p, _)| p != &path);
            }
            V3Action::DirectoryRetireIntent { index } => {
                if index >= snapshot.header.retire_dirs.len()
                    || !snapshot.retire_intents.insert(index)
                {
                    return Err(v3_conflict("directory retire intent"));
                }
            }
            V3Action::DirectoryRetired { index }
            | V3Action::DirectoryUnretireIntent { index }
            | V3Action::DirectoryUnretired { index } => {
                if !snapshot.retire_intents.contains(&index) {
                    return Err(v3_conflict("unowned directory retirement"));
                }
            }
            V3Action::DirectoryReturnIntent { index } => {
                if index >= snapshot.header.return_dirs.len()
                    || !snapshot.return_intents.insert(index)
                {
                    return Err(v3_conflict("directory return intent"));
                }
            }
            V3Action::DirectoryReturned { index }
            | V3Action::DirectoryUnreturnIntent { index }
            | V3Action::DirectoryUnreturned { index } => {
                if !snapshot.return_intents.contains(&index) {
                    return Err(v3_conflict("unowned directory return"));
                }
            }
        }
        snapshot.immutable_records.push((name, object.clone()));
        snapshot.head = object.clone();
        snapshot.sequence += 1;
        observed_events.push(object);
    }
    if snapshot.sequence == 0 {
        return Err(v3_conflict("unowned header preparation"));
    }
    for path in &snapshot
        .header
        .journal
        .external
        .as_ref()
        .ok_or("external V3 binding missing")?
        .cleanup_dirs
    {
        let actual = io.source_directory(path)?;
        if let Some((_, expected)) = snapshot.owned_dirs.iter().find(|(p, _)| p == path) {
            if actual.as_ref().is_some_and(|i| i != expected) {
                return Err(v3_conflict("owned directory identity changed"));
            }
        } else if actual.is_some() {
            return Err(v3_conflict("unowned present planned directory"));
        }
        if snapshot.state == State::Committed
            && (actual.is_none() || !snapshot.owned_dirs.iter().any(|(p, _)| p == path))
        {
            return Err(v3_conflict("committed directory missing"));
        }
        if removed_dirs.contains(path) && actual.is_some() {
            return Err(v3_conflict("removed directory replaced"));
        }
    }
    let active = io.active()?;
    if let Some((object, data)) = active {
        let pointer: Value =
            serde_json::from_slice(&data).map_err(|_| v3_conflict("active marker corrupt"))?;
        if pointer["version"] != 3
            || pointer["revision_id"] != revision
            || pointer["header_sha256"] != snapshot.header_object.sha256
        {
            return Err(v3_conflict("foreign active marker"));
        }
        snapshot.active = Some(object);
    }
    if undo {
        if snapshot.state != State::Committed || snapshot.active.is_some() {
            return Err(v3_conflict("undo requires committed inactive revision"));
        }
    } else if snapshot.active.is_none() {
        return Err(v3_conflict("active marker missing"));
    }
    let mut recognized = std::collections::BTreeSet::new();
    recognized.insert("v3.json".to_string());
    for sequence in 0..snapshot.sequence {
        recognized.insert(format!("e{sequence:06}.json"));
    }
    let mut directory_positions = Vec::new();
    for (index, (path, expected)) in snapshot.header.retire_dirs.iter().enumerate() {
        let source = io.source_directory(path)?;
        let saved = io.saved_directory(revision, &directory_slot(index))?;
        if source.as_ref().is_some_and(|id| id != expected)
            || saved.as_ref().is_some_and(|id| id != expected)
            || source.is_some() == saved.is_some()
            || saved.is_some() && !snapshot.retire_intents.contains(&index)
        {
            return Err(v3_conflict("directory retirement ownership or position"));
        }
        if (snapshot.state == State::Committed && source.is_some())
            || (snapshot.state == State::RolledBack && saved.is_some())
        {
            return Err(v3_conflict("terminal directory position changed"));
        }
        if saved.is_some() {
            recognized.insert(directory_slot(index));
        }
        directory_positions.push((path.clone(), source, saved));
    }
    for (index, origin) in snapshot.header.return_dirs.iter().enumerate() {
        if io.directory(&origin.revision)? != origin.directory {
            return Err(v3_conflict("directory origin changed"));
        }
        let source = io.source_directory(&origin.path)?;
        let saved = io.saved_directory(&origin.revision, &origin.name)?;
        if source.as_ref().is_some_and(|id| id != &origin.identity)
            || saved.as_ref().is_some_and(|id| id != &origin.identity)
            || source.is_some() == saved.is_some()
            || source.is_some() && !snapshot.return_intents.contains(&index)
        {
            return Err(v3_conflict("directory return ownership or position"));
        }
        if (snapshot.state == State::Committed && saved.is_some())
            || (snapshot.state == State::RolledBack && source.is_some())
        {
            return Err(v3_conflict("terminal returned directory changed"));
        }
        directory_positions.push((origin.path.clone(), source, saved));
    }
    let directory_paths: std::collections::BTreeSet<_> = snapshot
        .owned_dirs
        .iter()
        .map(|d| d.0.clone())
        .chain(snapshot.header.retire_dirs.iter().map(|d| d.0.clone()))
        .chain(snapshot.header.return_dirs.iter().map(|d| d.path.clone()))
        .collect();
    for path in &directory_paths {
        if let Some(entries) = io.source_entries(path)? {
            for (name, is_directory, _) in entries {
                let full = format!("{path}/{name}");
                if if is_directory {
                    !directory_paths.contains(&full)
                } else {
                    !snapshot
                        .header
                        .journal
                        .rows
                        .iter()
                        .any(|row| row.path == full)
                } {
                    return Err(v3_conflict("owned directory contains foreign entry"));
                }
            }
        }
    }
    for index in 0..count {
        let row = &snapshot.header.journal.rows[index];
        if row.after.is_some() && snapshot.temps[index].is_none() {
            return Err(v3_conflict("unrecorded proposed identity"));
        }
        for (before, expected, blob) in [
            (true, &row.before, &snapshot.blobs[index].0),
            (false, &row.after, &snapshot.blobs[index].1),
        ] {
            if expected.is_some() && blob.is_none() {
                return Err(v3_conflict("unrecorded backup"));
            }
            if blob.is_some() {
                let name = slot(index, if before { "before" } else { "after" });
                if let Some((object, _)) = blob {
                    snapshot
                        .immutable_records
                        .push((name.clone(), object.clone()));
                }
                recognized.insert(name);
            }
        }
        let target = io.target(&row.path)?.map(|v| v.0);
        let old = io
            .read(
                revision,
                &slot(index, "old"),
                crate::revisions::MAX_FILE_BYTES as u64,
            )?
            .map(|v| v.0);
        let temp = io
            .read(
                revision,
                &slot(index, "temp"),
                crate::revisions::MAX_FILE_BYTES as u64,
            )?
            .map(|v| v.0);
        if let Some(actual) = &old {
            if snapshot.olds[index].as_ref() != Some(actual) {
                return Err(v3_conflict("unowned old slot"));
            }
            recognized.insert(slot(index, "old"));
        }
        if let Some(actual) = &temp {
            if snapshot.temps[index].as_ref() != Some(actual) {
                return Err(v3_conflict("unowned temp slot"));
            }
            recognized.insert(slot(index, "temp"));
        }
        if target.as_ref().is_some_and(|actual| {
            snapshot.header.originals[index].as_ref() != Some(actual)
                && snapshot.temps[index].as_ref() != Some(actual)
        }) {
            return Err(v3_conflict("newer or same-hash new-identity target"));
        }
        if target
            .as_ref()
            .is_some_and(|v| old.as_ref() == Some(v) || temp.as_ref() == Some(v))
        {
            return Err(v3_conflict("duplicate object names"));
        }
        if snapshot.state == State::Committed && target.as_ref() != snapshot.temps[index].as_ref() {
            return Err(v3_conflict("terminal target changed"));
        }
        if snapshot.state == State::RolledBack
            && target.as_ref() != snapshot.header.originals[index].as_ref()
        {
            return Err(v3_conflict("restored target changed"));
        }
        snapshot.targets.push(target);
    }
    if entries
        .iter()
        .any(|(name, d, _)| !recognized.contains(name) || (*d != name.starts_with('d')))
    {
        return Err(v3_conflict("unowned present state evidence"));
    }
    let mut before_bytes = 0u64;
    let mut after_bytes = 0u64;
    for (before, after) in &snapshot.blobs {
        before_bytes += before.as_ref().map_or(0, |v| v.0.bytes);
        after_bytes += after.as_ref().map_or(0, |v| v.0.bytes);
        if before_bytes > crate::revisions::MAX_TEXT_BYTES as u64
            || after_bytes > crate::revisions::MAX_TEXT_BYTES as u64
        {
            return Err(v3_conflict("backup aggregate limit"));
        }
    }
    snapshot.digest = crate::sha256_hex(
        &serde_json::to_vec(&(
            &snapshot.header_object,
            &snapshot.head,
            &observed_events,
            &snapshot.targets,
            &snapshot.temps,
            &snapshot.olds,
            &snapshot.owned_dirs,
            &snapshot.active,
            &directory_positions,
        ))
        .map_err(|e| e.to_string())?,
    );
    Ok(snapshot)
}
#[cfg(windows)]
fn binding_cleanup(snapshot: &ExternalV3Snapshot, path: &str) -> bool {
    snapshot
        .header
        .journal
        .external
        .as_ref()
        .is_some_and(|b| b.cleanup_dirs.iter().any(|p| p == path))
}

#[cfg(windows)]
struct V3Publication<'a, 'b> {
    io: crate::external_workspace::ExternalRevisionIo<'a, 'b>,
    snapshot: ExternalV3Snapshot,
    activation_attempted: bool,
}
#[cfg(windows)]
impl<'a, 'b> V3Publication<'a, 'b> {
    fn inventory(&self) -> Result<(), String> {
        self.io.check()?;
        let mut names = std::collections::BTreeSet::from(["v3.json".to_owned()]);
        for sequence in 0..self.snapshot.sequence {
            names.insert(format!("e{sequence:06}.json"));
        }
        for index in 0..self.rows() {
            for (kind, expected) in [
                (
                    "before",
                    self.snapshot.blobs[index].0.as_ref().map(|v| &v.0),
                ),
                ("after", self.snapshot.blobs[index].1.as_ref().map(|v| &v.0)),
                ("temp", self.snapshot.temps[index].as_ref()),
                ("old", self.snapshot.olds[index].as_ref()),
            ] {
                if let Some(actual) = self
                    .io
                    .read(&slot(index, kind), crate::revisions::MAX_FILE_BYTES as u64)?
                {
                    if Some(&actual.0) != expected {
                        return Err(v3_conflict("unowned present record"));
                    }
                    names.insert(slot(index, kind));
                } else if matches!(kind, "before" | "after") && expected.is_some() {
                    return Err(v3_conflict("immutable backup missing"));
                }
            }
        }
        for (index, (path, expected)) in self.snapshot.header.retire_dirs.iter().enumerate() {
            let source = self.io.source_directory(path)?;
            let saved = self.io.saved_directory(&directory_slot(index))?;
            if source.as_ref().is_some_and(|id| id != expected)
                || saved.as_ref().is_some_and(|id| id != expected)
                || source.is_some() == saved.is_some()
                || saved.is_some() && !self.snapshot.retire_intents.contains(&index)
            {
                return Err(v3_conflict("directory relocation ownership changed"));
            }
            if saved.is_some() {
                names.insert(directory_slot(index));
            }
        }
        for (index, origin) in self.snapshot.header.return_dirs.iter().enumerate() {
            let source = self.io.source_directory(&origin.path)?;
            let saved = self.io.origin_directory(origin)?;
            if source.as_ref().is_some_and(|id| id != &origin.identity)
                || saved.as_ref().is_some_and(|id| id != &origin.identity)
                || source.is_some() == saved.is_some()
                || source.is_some() && !self.snapshot.return_intents.contains(&index)
            {
                return Err(v3_conflict("returned directory ownership changed"));
            }
        }
        if self
            .io
            .entries()?
            .into_iter()
            .any(|(name, d, _)| !names.contains(&name) || (d != name.starts_with('d')))
        {
            return Err(v3_conflict("unowned state name"));
        }
        if self.io.active()?.map(|v| v.0) != self.snapshot.active {
            return Err(v3_conflict("active marker changed"));
        }
        Ok(())
    }
    fn event(&mut self, action: V3Action) -> Result<(), String> {
        self.io.check()?;
        if self.snapshot.sequence >= 8192 {
            return Err(v3_conflict("event quota"));
        }
        let event = V3Event {
            version: 3,
            sequence: self.snapshot.sequence,
            previous: self.snapshot.head.clone(),
            action,
        };
        let bytes = serde_json::to_vec(&event).map_err(|e| e.to_string())?;
        let name = format!("e{:06}.json", self.snapshot.sequence);
        v3_checkpoint("before_event", self.snapshot.sequence);
        let object = self.io.create(&name, &bytes, 4096)?;
        self.snapshot.head = object;
        self.snapshot.sequence += 1;
        v3_checkpoint("after_event", self.snapshot.sequence - 1);
        Ok(())
    }
    fn prepare(
        io: crate::external_workspace::ExternalRevisionIo<'a, 'b>,
        changes: Vec<ChangeBytes>,
        binding: ExternalBinding,
        originals: Vec<Option<Object>>,
        undo_of: Option<String>,
        retire_dirs: Vec<(String, Identity)>,
        return_dirs: Vec<DirectoryOrigin>,
        mut observe: impl FnMut(Option<usize>),
    ) -> Result<Self, String> {
        io.check()?;
        if changes.is_empty()
            || changes.len() > crate::revisions::MAX_FILES
            || originals.len() != changes.len()
        {
            return Err(v3_conflict("row bounds"));
        }
        let rows: Vec<_> = changes
            .iter()
            .map(|(path, before, after)| Row {
                path: path.clone(),
                before: before.as_ref().map(|b| crate::sha256_hex(b)),
                after: after.as_ref().map(|b| crate::sha256_hex(b)),
            })
            .collect();
        validate_external_binding(&binding, &rows)?;
        let mut before_total = 0u64;
        let mut after_total = 0u64;
        for (index, (path, before, after)) in changes.iter().enumerate() {
            for (bytes, total) in [(before, &mut before_total), (after, &mut after_total)] {
                if let Some(bytes) = bytes {
                    if bytes.len() > crate::revisions::MAX_FILE_BYTES
                        || std::str::from_utf8(bytes).is_err()
                    {
                        return Err(v3_conflict("bounded UTF8 text required"));
                    }
                    *total = total
                        .checked_add(bytes.len() as u64)
                        .ok_or("external V3 byte overflow")?;
                    if *total > crate::revisions::MAX_TEXT_BYTES as u64 {
                        return Err(v3_conflict("aggregate text bound"));
                    }
                }
            }
            let actual = io.target(path)?;
            if actual.as_ref().map(|v| &v.0) != originals[index].as_ref()
                || actual.as_ref().map(|v| &v.1) != before.as_ref()
            {
                return Err(v3_conflict(
                    "source identity or bytes changed before preparation",
                ));
            }
        }
        validate_directory_changes(&binding, &rows, &retire_dirs, &return_dirs)?;
        if changes.len() * 12
            + (binding.cleanup_dirs.len() + retire_dirs.len() + return_dirs.len()) * 4
            + 16
            > 8192
        {
            return Err(v3_conflict("recovery event reservation exceeds quota"));
        }
        let planned =
            planned_v3_directory_cleanup(&binding, &return_dirs, &|p| io.source_directory(p))?;
        if planned != binding.cleanup_dirs {
            return Err(v3_conflict("planned missing parents changed"));
        }
        // Reserve worst-case event and relocation storage, including all partial
        // preparation/history. This check precedes every managed-state creation.
        let additional = before_total
            .checked_mul(2)
            .and_then(|n| after_total.checked_mul(2).and_then(|a| n.checked_add(a)))
            .and_then(|n| n.checked_add(8192 * 4096 + MAX_JOURNAL_BYTES + 4096))
            .ok_or("external V3 history overflow")?;
        let revision = crate::random::session_id();
        let directory = io.begin(&revision, additional)?;
        let count = rows.len();
        let header = V3Header {
            version: 3,
            journal: Journal {
                version: 3,
                id: revision,
                state: State::Prepared,
                rows,
                created_dirs: Vec::new(),
                undo_of,
                external: Some(binding),
                external_owned_dirs: Vec::new(),
            },
            directory,
            originals,
            retire_dirs,
            return_dirs,
        };
        let header_object = io.create(
            "v3.json",
            &serde_json::to_vec(&header).map_err(|e| e.to_string())?,
            MAX_JOURNAL_BYTES,
        )?;
        let mut this = Self {
            io,
            snapshot: ExternalV3Snapshot {
                header,
                header_object: header_object.clone(),
                head: header_object.clone(),
                sequence: 0,
                state: State::Prepared,
                blobs: vec![(None, None); count],
                temps: vec![None; count],
                olds: vec![None; count],
                targets: Vec::new(),
                owned_dirs: Vec::new(),
                active: None,
                digest: String::new(),
                immutable_records: Vec::new(), // Newly created immutable records are already pinned by the capability.
                retire_intents: std::collections::BTreeSet::new(),
                return_intents: std::collections::BTreeSet::new(),
            },
            activation_attempted: false,
        };
        this.event(V3Action::State {
            state: State::Prepared,
        })?;
        for (index, (_, before, after)) in changes.into_iter().enumerate() {
            for (is_before, bytes) in [(true, before), (false, after)] {
                if let Some(bytes) = bytes {
                    let object = this.io.create(
                        &slot(index, if is_before { "before" } else { "after" }),
                        &bytes,
                        crate::revisions::MAX_FILE_BYTES as u64,
                    )?;
                    this.event(V3Action::Blob {
                        row: index,
                        before: is_before,
                        object: object.clone(),
                    })?;
                    if is_before {
                        this.snapshot.blobs[index].0 = Some((object, bytes));
                    } else {
                        this.snapshot.blobs[index].1 = Some((object, bytes.clone()));
                        let temp = this.io.create(
                            &slot(index, "temp"),
                            &bytes,
                            crate::revisions::MAX_FILE_BYTES as u64,
                        )?;
                        this.event(V3Action::TempReady {
                            row: index,
                            object: temp.clone(),
                        })?;
                        this.snapshot.temps[index] = Some(temp);
                    }
                }
            }
        }
        v3_checkpoint("backups_durable", 0);
        observe(None);
        this.io.prepublication()?;
        let binding = this
            .snapshot
            .header
            .journal
            .external
            .as_ref()
            .ok_or("external V3 binding missing")?;
        if planned_v3_directory_cleanup(binding, &this.snapshot.header.return_dirs, &|p| {
            this.io.source_directory(p)
        })? != binding.cleanup_dirs
        {
            return Err(v3_conflict(
                "planned missing parents changed after durable preparation",
            ));
        }
        for (index, row) in this.snapshot.header.journal.rows.iter().enumerate() {
            if this.io.target(&row.path)?.map(|v| v.0) != this.snapshot.header.originals[index] {
                return Err(v3_conflict(
                    "original identity changed after durable preparation",
                ));
            }
        }
        this.inventory()?;
        Ok(this)
    }
    fn restore_v3(&mut self) -> Result<Value, String> {
        operation_phase(crate::operation_context::Phase::Restoring);
        self.io.check()?;
        self.inventory()?;
        let binding = self
            .snapshot
            .header
            .journal
            .external
            .as_ref()
            .ok_or_else(|| v3_conflict("external binding missing"))?;
        for path in &binding.cleanup_dirs {
            if let Some(identity) = self.io.source_directory(path)? {
                if !self
                    .snapshot
                    .owned_dirs
                    .iter()
                    .any(|(owned, expected)| owned == path && expected == &identity)
                {
                    return Err(v3_conflict("unrecorded planned directory during restore"));
                }
            }
        }
        self.transition(State::RollingBack)?;
        for (index, (path, identity)) in self
            .snapshot
            .header
            .retire_dirs
            .clone()
            .into_iter()
            .enumerate()
        {
            let source = self.io.source_directory(&path)?;
            let saved = self.io.saved_directory(&directory_slot(index))?;
            if source.as_ref() == Some(&identity) && saved.is_none() {
                continue;
            }
            if source.is_some()
                || saved.as_ref() != Some(&identity)
                || !self.snapshot.retire_intents.contains(&index)
            {
                return Err(v3_conflict("retired directory recovery conflict"));
            }
            self.event(V3Action::DirectoryUnretireIntent { index })?;
            v3_checkpoint("before_directory_unretire", index);
            self.io
                .unretire_directory(&path, &identity, &directory_slot(index))?;
            v3_checkpoint("after_directory_unretire", index);
            self.event(V3Action::DirectoryUnretired { index })?;
        }
        for index in (0..self.snapshot.header.journal.rows.len()).rev() {
            self.io.check()?;
            let row = self.snapshot.header.journal.rows[index].clone();
            let original = self.snapshot.header.originals[index].clone();
            let target = self.io.target(&row.path)?.map(|v| v.0);
            let old = self
                .io
                .read(&slot(index, "old"), crate::revisions::MAX_FILE_BYTES as u64)?
                .map(|v| v.0);
            if target == original && old.is_none() {
                continue;
            }
            if target
                .as_ref()
                .is_some_and(|v| self.snapshot.temps[index].as_ref() != Some(v))
                || old.as_ref().is_some_and(|v| original.as_ref() != Some(v))
                || (original.is_some() && old.is_none())
            {
                self.transition(State::RecoveryConflict)?;
                recovery_required();
                return Ok(
                    json!({"ok":false,"recovery_required":true,"recovery_blocked":true,"revision_id":self.revision(),"conflicts":[{"path":row.path,"reason":"exact relocation ownership required"}]}),
                );
            }
            self.event(V3Action::RestoreIntent { row: index })?;
            v3_checkpoint("before_restore", index);
            if let Some(target) = target {
                self.io.delete_target(&row.path, &target)?;
            }
            if let Some(old) = old {
                self.io.publish(&row.path, &slot(index, "old"), &old)?;
            }
            v3_checkpoint("after_restore", index);
            if self.io.target(&row.path)?.map(|v| v.0) != original {
                return Err(v3_conflict("restored identity changed"));
            }
            self.event(V3Action::Restored { row: index })?;
        }
        for (path, identity) in self.snapshot.owned_dirs.clone().into_iter().rev() {
            match self.io.source_directory(&path)? {
                None => {}
                Some(actual) if actual == identity => {
                    self.event(V3Action::DirectoryRemoveIntent {
                        path: path.clone(),
                        identity: identity.clone(),
                    })?;
                    self.io.remove_directory(&path, &identity)?;
                }
                Some(_) => return Err(v3_conflict("created directory identity changed")),
            }
            self.event(V3Action::DirectoryRemoved { path: path.clone() })?;
            self.snapshot.owned_dirs.retain(|(p, _)| p != &path);
        }
        for (index, origin) in self
            .snapshot
            .header
            .return_dirs
            .clone()
            .into_iter()
            .enumerate()
            .rev()
        {
            let source = self.io.source_directory(&origin.path)?;
            let saved = self.io.origin_directory(&origin)?;
            if source.is_none() && saved.as_ref() == Some(&origin.identity) {
                continue;
            }
            if source.as_ref() != Some(&origin.identity)
                || saved.is_some()
                || !self.snapshot.return_intents.contains(&index)
            {
                return Err(v3_conflict("returned directory recovery conflict"));
            }
            self.event(V3Action::DirectoryUnreturnIntent { index })?;
            v3_checkpoint("before_directory_unreturn", index);
            self.io.unreturn_directory(&origin)?;
            v3_checkpoint("after_directory_unreturn", index);
            self.event(V3Action::DirectoryUnreturned { index })?;
        }
        self.transition(State::RolledBack)?;
        self.clear()?;
        Ok(json!({"ok":true,"rolled_back":true,"revision_id":self.revision()}))
    }
}
#[cfg(windows)]
impl PublicationIo for V3Publication<'_, '_> {
    fn revision(&self) -> &str {
        &self.snapshot.header.journal.id
    }
    fn rows(&self) -> usize {
        self.snapshot.header.journal.rows.len()
    }
    fn undo_of(&self) -> Option<&str> {
        self.snapshot.header.journal.undo_of.as_deref()
    }
    fn abort_prepared(&mut self) -> Result<(), String> {
        Ok(())
    } // Partial private history is never erased implicitly.
    fn activate(&mut self) -> Result<(), String> {
        self.io.check()?;
        self.activation_attempted = true;
        let bytes=serde_json::to_vec(&json!({"version":3,"revision_id":self.revision(),"header_sha256":self.snapshot.header_object.sha256})).map_err(|e|e.to_string())?;
        self.snapshot.active = Some(self.io.activate(&bytes)?);
        Ok(())
    }
    fn transition(&mut self, state: State) -> Result<(), String> {
        if state == State::Committed {
            for (index, (path, identity)) in self
                .snapshot
                .header
                .retire_dirs
                .clone()
                .into_iter()
                .enumerate()
                .rev()
            {
                if self.io.source_directory(&path)?.as_ref() != Some(&identity)
                    || self.io.saved_directory(&directory_slot(index))?.is_some()
                {
                    return Err(v3_conflict("directory retirement precondition changed"));
                }
                self.event(V3Action::DirectoryRetireIntent { index })?;
                self.snapshot.retire_intents.insert(index);
                v3_checkpoint("before_directory_retire", index);
                self.io
                    .retire_directory(&path, &identity, &directory_slot(index))?;
                v3_checkpoint("after_directory_retire", index);
                self.event(V3Action::DirectoryRetired { index })?;
            }
            self.inventory()?;
        }
        self.event(V3Action::State {
            state: state.clone(),
        })?;
        self.snapshot.state = state;
        Ok(())
    }
    fn directories(&mut self) -> Result<(), String> {
        for (index, origin) in self
            .snapshot
            .header
            .return_dirs
            .clone()
            .into_iter()
            .enumerate()
        {
            if self.io.source_directory(&origin.path)?.is_some()
                || self.io.origin_directory(&origin)?.as_ref() != Some(&origin.identity)
            {
                return Err(v3_conflict("directory return precondition changed"));
            }
            self.event(V3Action::DirectoryReturnIntent { index })?;
            self.snapshot.return_intents.insert(index);
            v3_checkpoint("before_directory_return", index);
            self.io.return_directory(&origin)?;
            v3_checkpoint("after_directory_return", index);
            self.event(V3Action::DirectoryReturned { index })?;
        }
        let paths = self
            .snapshot
            .header
            .journal
            .external
            .as_ref()
            .ok_or("external V3 binding missing")?
            .cleanup_dirs
            .clone();
        for path in paths {
            if self.io.source_directory(&path)?.is_some() {
                return Err(v3_conflict("unowned present planned directory"));
            }
            self.event(V3Action::DirectoryIntent { path: path.clone() })?;
            v3_checkpoint("before_directory", self.snapshot.owned_dirs.len());
            let identity = self.io.create_directory(&path)?;
            v3_checkpoint("after_directory", self.snapshot.owned_dirs.len());
            self.event(V3Action::DirectoryOwned {
                path: path.clone(),
                identity: identity.clone(),
            })?;
            self.snapshot.owned_dirs.push((path, identity));
        }
        Ok(())
    }
    fn check(&self) -> Result<(), String> {
        self.inventory()
    }
    fn apply(&mut self, index: usize) -> Result<(), String> {
        let row = self.snapshot.header.journal.rows[index].clone();
        let original = self.snapshot.header.originals[index].clone();
        if self.io.target(&row.path)?.map(|v| v.0) != original {
            return Err(v3_conflict("source changed after durable backups"));
        }
        if let Some(original) = original {
            self.event(V3Action::OldMoveIntent {
                row: index,
                object: original.clone(),
            })?;
            v3_checkpoint("before_old_move", index);
            let moved = self
                .io
                .move_old(&row.path, &original, &slot(index, "old"))?;
            v3_checkpoint("after_old_move", index);
            if moved != original {
                return Err(v3_conflict("old move identity"));
            }
            self.snapshot.olds[index] = Some(moved.clone());
            self.event(V3Action::OldMoved {
                row: index,
                object: moved,
            })?;
        }
        if let Some(temp) = self.snapshot.temps[index].clone() {
            self.event(V3Action::PublishIntent {
                row: index,
                object: temp.clone(),
            })?;
            v3_checkpoint("before_publish", index);
            let published = self.io.publish(&row.path, &slot(index, "temp"), &temp)?;
            v3_checkpoint("after_publish", index);
            if published != temp {
                return Err(v3_conflict("published identity"));
            }
            self.event(V3Action::Published {
                row: index,
                object: published,
            })?;
        }
        if self.io.target(&row.path)?.map(|v| v.0) != self.snapshot.temps[index] {
            return Err(v3_conflict("published target changed"));
        }
        Ok(())
    }
    fn has_active(&self) -> Result<bool, String> {
        Ok(self.activation_attempted || self.snapshot.active.is_some())
    }
    fn restore(&mut self) -> Result<Value, String> {
        self.restore_v3()
    }
    fn clear(&mut self) -> Result<(), String> {
        let expected = self
            .snapshot
            .active
            .as_ref()
            .ok_or_else(|| v3_conflict("unrecorded active marker"))?;
        self.io.clear_active(expected)?;
        self.snapshot.active = None;
        Ok(())
    }
}
#[cfg(windows)]
pub(crate) fn planned_v3_cleanup(
    binding: &ExternalBinding,
    directory: &dyn Fn(&str) -> Result<Option<Identity>, String>,
) -> Result<Vec<String>, String> {
    validate_cleanup_scope(binding)?;
    if directory(&binding.member_src)?.is_none() {
        return Err(v3_conflict("existing member src required"));
    }
    let mut missing = std::collections::BTreeSet::new();
    for row in &binding.targets {
        if row.before.is_some() || row.after.is_none() {
            continue;
        }
        let mut parent = v3_parent(&row.path);
        while parent != binding.member_src {
            if !parent.starts_with(&(binding.member_src.clone() + "/")) {
                return Err(v3_conflict("parent scope"));
            }
            if directory(parent)?.is_some() {
                break;
            }
            missing.insert(parent.to_owned());
            parent = v3_parent(parent);
        }
    }
    Ok(missing.into_iter().collect())
}
#[cfg(windows)]
pub(crate) fn planned_v3_directory_cleanup(
    binding: &ExternalBinding,
    returns: &[DirectoryOrigin],
    directory: &dyn Fn(&str) -> Result<Option<Identity>, String>,
) -> Result<Vec<String>, String> {
    Ok(planned_v3_cleanup(binding, directory)?
        .into_iter()
        .filter(|path| !returns.iter().any(|origin| &origin.path == path))
        .collect())
}
#[cfg(windows)]
fn validate_directory_changes(
    binding: &ExternalBinding,
    rows: &[Row],
    retire: &[(String, Identity)],
    returns: &[DirectoryOrigin],
) -> Result<(), String> {
    if retire.len() + returns.len() > 2048 {
        return Err(v3_conflict("directory change quota"));
    }
    let mut paths = std::collections::BTreeSet::new();
    let valid = |path: &str, create: bool| {
        normalized_source_directory(path)
            && path.starts_with(&(binding.member_src.clone() + "/"))
            && rows.iter().any(|r| {
                r.path.starts_with(&(path.to_owned() + "/"))
                    && if create {
                        r.before.is_none() && r.after.is_some()
                    } else {
                        r.before.is_some() && r.after.is_none()
                    }
            })
    };
    let mut previous = None;
    for (path, _) in retire {
        if !valid(path, false)
            || !paths.insert(path.to_ascii_lowercase())
            || previous.is_some_and(|p: &str| p >= path.as_str())
            || binding.cleanup_dirs.contains(path)
        {
            return Err(v3_conflict("directory retirement scope"));
        }
        previous = Some(path.as_str());
    }
    previous = None;
    for origin in returns {
        if !valid(&origin.path, true)
            || !paths.insert(origin.path.to_ascii_lowercase())
            || previous.is_some_and(|p| p >= origin.path.as_str())
            || binding.cleanup_dirs.contains(&origin.path)
            || id(&origin.revision).is_err()
            || origin.name.len() != 9
            || !origin.name.starts_with('d')
            || !origin.name.ends_with(".dir")
            || origin.name[1..5]
                .parse::<usize>()
                .ok()
                .is_none_or(|index| index >= 2048)
        {
            return Err(v3_conflict("directory return scope"));
        }
        previous = Some(origin.path.as_str());
    }
    Ok(())
}
#[cfg(windows)]
fn v3_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |p| p.0)
}
#[cfg(all(test, windows))]
thread_local! {static V3_HOOK:std::cell::RefCell<Option<Box<dyn FnMut(&str,usize)>>> = const {std::cell::RefCell::new(None)};}
#[cfg(windows)]
fn v3_checkpoint(_phase: &str, _index: usize) {
    #[cfg(test)]
    V3_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(_phase, _index)
        }
    });
}
#[cfg(all(test, windows))]
pub(crate) fn with_external_v3_test_hook<T>(
    hook: impl FnMut(&str, usize) + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            V3_HOOK.with(|hook| *hook.borrow_mut() = None);
        }
    }
    V3_HOOK.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Box::new(hook));
    });
    let _reset = Reset;
    run()
}
pub(crate) trait DirectoryAuthority {
    fn check(&self) -> Result<(), String>;
    fn verify(&self, relative: &str, identity: Option<&str>) -> Result<(), String>;
    fn create(&self, relative: &str) -> Result<String, String>;
    fn remove(&self, relative: &str, identity: &str) -> Result<(), String>;
}

fn operation_check() -> Result<(), String> {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.check().map_err(|error| error.to_string())?;
    }
    Ok(())
}
fn operation_phase(phase: crate::operation_context::Phase) {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.phase(phase);
    }
}
fn commit_barrier() -> Result<Option<crate::operation_context::CommitGuard>, String> {
    crate::operation_context::OperationContext::current()
        .map(|context| context.commit_barrier())
        .transpose()
        .map_err(|error| error.to_string())
}
fn recovery_required() {
    if let Some(context) = crate::operation_context::OperationContext::current() {
        context.mark_recovery_required();
    }
}
fn discard_prepared(directory: &Path) -> Result<(), String> {
    crate::diagnostics::checked_path(directory).map_err(|error| error.to_string())?;
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        crate::diagnostics::checked_path(&entry.path()).map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("prepared journal cleanup refused unexpected content".into());
        }
        std::fs::remove_file(entry.path()).map_err(|error| error.to_string())?;
    }
    std::fs::remove_dir(directory).map_err(|error| error.to_string())
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    path: String,
    before: Option<String>,
    after: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum State {
    Prepared,
    Publishing,
    Committed,
    RollingBack,
    RolledBack,
    RecoveryConflict,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    id: String,
    state: State,
    rows: Vec<Row>,
    created_dirs: Vec<String>,
    undo_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external: Option<ExternalBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    external_owned_dirs: Vec<ExternalDirectory>,
}

fn id(value: &str) -> Result<(), String> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("revision ID must be 32 hexadecimal characters".into());
    }
    Ok(())
}
fn state_root(root: &Path) -> Result<PathBuf, String> {
    let path = files::confined(root, ".aurum/revisions")?;
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    Ok(path)
}
fn active(root: &Path) -> Result<PathBuf, String> {
    Ok(state_root(root)?.join("active.json"))
}
fn active_revision(root: &Path) -> Result<String, String> {
    let pointer = active(root)?;
    crate::diagnostics::checked_path(&pointer).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    std::fs::File::open(&pointer)
        .map_err(|error| error.to_string())?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 4096 {
        return Err("active revision pointer exceeds its size bound".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let revision = value["revision_id"]
        .as_str()
        .ok_or("invalid active revision pointer")?;
    id(revision)?;
    Ok(revision.into())
}

pub(crate) fn ensure_writable(root: &Path) -> Result<(), String> {
    if active(root)?.exists() {
        return Err(
            "an unresolved revision is present; run changes_recover before writing project content"
                .into(),
        );
    }
    Ok(())
}

fn durable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    files::write_atomic(path, bytes).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(path.parent().ok_or("missing journal directory")?)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}
fn save(directory: &Path, journal: &Journal) -> Result<(), String> {
    durable(
        &directory.join("journal.json"),
        &serde_json::to_vec(journal).map_err(|error| error.to_string())?,
    )
}
fn load(directory: &Path) -> Result<Journal, String> {
    crate::diagnostics::checked_path(directory).map_err(|error| error.to_string())?;
    // A declared confined revision cannot be reclassified by planting a v1
    // journal beside it. This probe supplies no V3 reads or mutation fallback.
    match std::fs::symlink_metadata(directory.join("v3.json")) {
        Ok(_) => {
            return Err(
                "confined external V3 revision requires fresh bound private authority".into(),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let path = directory.join("journal.json");
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_JOURNAL_BYTES {
        return Err("revision journal exceeds its size or file-kind bound".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err("revision journal exceeds its size bound".into());
    }
    let journal: Journal = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    id(&journal.id)?;
    if !matches!(
        (journal.version, journal.external.is_some()),
        (1, false) | (2, true)
    ) || journal.rows.is_empty()
        || journal.rows.len() > MAX_ROWS
        || journal.created_dirs.len() > 2048
    {
        return Err("unsupported revision journal".into());
    }
    for row in &journal.rows {
        for hash in [&row.before, &row.after].into_iter().flatten() {
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("invalid journal content hash".into());
            }
        }
    }
    if let Some(binding) = &journal.external {
        validate_external_binding(binding, &journal.rows)?;
        validate_external_directories(&journal)?;
    } else if !journal.external_owned_dirs.is_empty() {
        return Err("v1 revision cannot contain external directory ownership".into());
    }
    if journal.version == 1 {
        check_legacy_directory_inventory(directory, &journal)?;
    }
    Ok(journal)
}

fn check_legacy_directory_inventory(directory: &Path, journal: &Journal) -> Result<(), String> {
    let mut allowed = std::collections::BTreeSet::from(["journal.json".to_owned()]);
    for (index, row) in journal.rows.iter().enumerate() {
        if row.before.is_some() {
            allowed.insert(format!("{index:03}.before"));
        }
        if row.after.is_some() {
            allowed.insert(format!("{index:03}.after"));
        }
    }
    let mut entries = 0usize;
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        operation_check()?;
        entries += 1;
        if entries > allowed.len() {
            return Err(
                "legacy revision contains unclassified evidence; evidence preserved".into(),
            );
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "legacy revision contains an unclassified name; evidence preserved")?;
        if !allowed.contains(&name)
            || !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
        {
            return Err(
                "legacy revision contains unclassified evidence; evidence preserved".into(),
            );
        }
        crate::diagnostics::checked_path(&entry.path()).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn validate_external_binding(binding: &ExternalBinding, rows: &[Row]) -> Result<(), String> {
    let valid_hash = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if binding.version != 2
        || rows.len() > crate::revisions::MAX_FILES
        || binding.targets.len() != rows.len()
        || ![
            &binding.imported_identity,
            &binding.external_identity,
            &binding.imported_revision,
            &binding.proposal_digest,
            &binding.authorization_digest,
        ]
        .iter()
        .all(|value| valid_hash(value))
        || binding.imported_path.len() > 4096
        || binding.external_path.len() > 4096
        || !Path::new(&binding.imported_path).is_absolute()
        || !Path::new(&binding.external_path).is_absolute()
        || binding.root_id.is_empty()
        || binding.root_id.len() > 32
        || !binding.root_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
        || !binding.member_src.ends_with("/src")
    {
        return Err("invalid external source journal binding".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for (target, row) in binding.targets.iter().zip(rows) {
        if target.path != row.path
            || target.before != row.before
            || target.after != row.after
            || !seen.insert(target.path.to_ascii_lowercase())
            || !target.path.starts_with(&(binding.member_src.clone() + "/"))
            || !target.path.ends_with(".rs")
            || target.path.len() > 1024
            || target.path.split('/').any(|part| {
                part.is_empty()
                    || matches!(part, "." | ".." | "build.rs")
                    || part.ends_with('.')
                    || !part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            })
        {
            return Err("external journal scope does not match bounded source rows".into());
        }
    }
    validate_cleanup_scope(binding)?;
    Ok(())
}
fn normalized_source_directory(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && path.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        })
}
fn validate_cleanup_scope(binding: &ExternalBinding) -> Result<(), String> {
    if binding.cleanup_dirs.len() > 2048 || !normalized_source_directory(&binding.member_src) {
        return Err("invalid external source cleanup scope".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut previous: Option<&str> = None;
    for path in &binding.cleanup_dirs {
        if !normalized_source_directory(path)
            || !path.starts_with(&(binding.member_src.clone() + "/"))
            || !seen.insert(path.to_ascii_lowercase())
            || previous.is_some_and(|before| before >= path.as_str())
            || !binding.targets.iter().any(|target| {
                target.before.is_none()
                    && target.after.is_some()
                    && target.path.starts_with(&(path.clone() + "/"))
            })
        {
            return Err("external cleanup must name unique normalized create-target ancestors below member/src".into());
        }
        previous = Some(path);
    }
    Ok(())
}
fn validate_external_directories(journal: &Journal) -> Result<(), String> {
    let binding = journal
        .external
        .as_ref()
        .ok_or("external source provenance missing")?;
    let owned: Vec<_> = journal
        .external_owned_dirs
        .iter()
        .map(|directory| directory.path.clone())
        .collect();
    if owned != journal.created_dirs
        || owned.windows(2).any(|pair| pair[0] >= pair[1])
        || journal.external_owned_dirs.iter().any(|directory| {
            !binding.cleanup_dirs.contains(&directory.path)
                || directory.identity.len() != 64
                || !directory
                    .identity
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        || (journal.state == State::Committed && owned != binding.cleanup_dirs)
    {
        return Err("external revision cleanup ownership does not match its approved scope".into());
    }
    Ok(())
}
fn external_scope(journal: &Journal) -> Result<ExternalRepairScope, String> {
    validate_external_directories(journal)?;
    Ok(ExternalRepairScope {
        binding: journal
            .external
            .clone()
            .ok_or("external source provenance missing")?,
        owned_dirs: journal.external_owned_dirs.clone(),
    })
}
/// Derive future ownership only from currently missing create-target parents.
/// The publishing engine must still create each directory with create_new semantics.
pub(crate) fn planned_external_cleanup(
    root: &Path,
    binding: &ExternalBinding,
) -> Result<Vec<String>, String> {
    let source = crate::project_ops::managed_path(root, &binding.member_src)?;
    crate::diagnostics::checked_path(&source).map_err(|error| error.to_string())?;
    if !std::fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.is_dir()) {
        return Err("existing selected member/src directory required".into());
    }
    let mut planned = std::collections::BTreeSet::new();
    for target in &binding.targets {
        if target.before.is_some() || target.after.is_none() {
            continue;
        }
        let mut parent = Path::new(&target.path).parent();
        while let Some(relative) = parent {
            let text = relative
                .to_str()
                .ok_or("external directory path is not UTF-8")?
                .replace('\\', "/");
            if text == binding.member_src {
                break;
            }
            if !text.starts_with(&(binding.member_src.clone() + "/"))
                || !normalized_source_directory(&text)
            {
                return Err("external creation escaped selected member/src".into());
            }
            let full = crate::project_ops::managed_path(root, &text)?;
            match std::fs::symlink_metadata(&full) {
                Ok(metadata) if metadata.is_dir() => {
                    break;
                }
                Ok(_) => return Err("external create parent must be an ordinary directory".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    planned.insert(text);
                }
                Err(error) => return Err(error.to_string()),
            }
            parent = relative.parent();
        }
    }
    let result: Vec<_> = planned.into_iter().collect();
    let mut checked = binding.clone();
    checked.cleanup_dirs = result.clone();
    validate_cleanup_scope(&checked)?;
    Ok(result)
}

/// Historical metadata only. Never read recovery blobs or assert live hashes.
pub fn history(root: &Path, input: &Value) -> Result<Value, String> {
    let selected = input
        .get("revision_id")
        .map(|value| value.as_str().ok_or("revision_id must be a string"))
        .transpose()?;
    if let Some(selected) = selected {
        id(selected)?;
    }
    let limit = input
        .get("limit")
        .map(|value| value.as_u64().ok_or("history limit must be an integer"))
        .transpose()?
        .unwrap_or(50);
    if !(1..=200).contains(&limit) {
        return Err("history limit must be between 1 and 200".into());
    }
    let _lease = crate::content_lock::ContentLock::shared(root)?;
    let directory = state_root(root)?;
    let pointer = active(root)?;
    crate::diagnostics::checked_path(&pointer).map_err(|error| error.to_string())?;
    let active_id = match std::fs::metadata(&pointer) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
        Ok(metadata) => {
            if !metadata.is_file() || metadata.len() > 4096 {
                return Err("active revision pointer exceeds supported bounds".into());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(pointer)
                .map_err(|error| error.to_string())?
                .take(4097)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > 4096 {
                return Err("active revision pointer exceeds supported bounds".into());
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| "invalid active revision pointer")?;
            let value = value["revision_id"]
                .as_str()
                .ok_or("invalid active revision pointer")?;
            id(value)?;
            Some(value.to_string())
        }
    };
    let mut paths = Vec::new();
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    if let Some(entries) = entries {
        for (index, entry) in entries.enumerate() {
            if index >= 256 {
                return Err("revision history scan exceeds its entry bound".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if id(&name).is_err() {
                continue;
            }
            crate::diagnostics::checked_path(&entry.path()).map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_dir()
            {
                return Err("revision history contains an unexpected entry kind".into());
            }
            if paths.len() >= 32 {
                return Err("revision history exceeds its 32-entry bound".into());
            }
            paths.push((name, entry.path()));
        }
    }
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    let retained = paths.len();
    let mut revisions = Vec::new();
    for (name, path) in paths {
        if selected.is_some_and(|selected| selected != name) {
            continue;
        }
        let journal = load(&path)
            .map_err(|_| format!("revision {name} has an invalid or unavailable journal"))?;
        if journal.id != name {
            return Err("revision directory and journal identity disagree".into());
        }
        let completed = journal.state == State::Committed || journal.state == State::RolledBack;
        let mut summary = json!({"revision_id":name,"state":journal.state,"files_changed":journal.rows.len(),"undo_of":journal.undo_of,"completed":completed,
            "undo_recorded":journal.state == State::Committed,"active":active_id.as_deref() == Some(&name)});
        if selected.is_some() {
            let mut rows = Vec::new();
            let mut bytes = 0;
            for row in &journal.rows {
                // Historical target names obey the same confinement/secret rules.
                crate::project_ops::managed_path(root, &row.path)?;
                let value =
                    json!({"path":row.path,"before_sha256":row.before,"after_sha256":row.after});
                let encoded = serde_json::to_vec(&value)
                    .map_err(|error| error.to_string())?
                    .len();
                if rows.len() >= limit as usize || bytes + encoded > 128 * 1024 {
                    break;
                }
                bytes += encoded;
                rows.push(value);
            }
            summary["files_truncated"] = json!(rows.len() < journal.rows.len());
            summary["files"] = json!(rows);
        }
        revisions.push(summary);
    }
    if selected.is_some() && revisions.is_empty() {
        return Err("revision is not present in retained history".into());
    }
    Ok(
        json!({"ok":true,"revisions":revisions,"retained_revisions":retained,"active_revision":active_id,
        "ordering":"revision ID, not chronology","scope":"retained journal metadata only, not all project changes",
        "source_state_checked":false,"undo_preconditions_checked":false,"content_changed":false}),
    )
}
fn read_text(path: &Path) -> Result<Option<Vec<u8>>, String> {
    operation_check()?;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 => {
            return Err("revision targets must be regular text files of at most 2 MiB".into())
        }
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 || std::str::from_utf8(&bytes).is_err() {
        return Err("revision text file exceeds supported bounds".into());
    }
    operation_check()?;
    Ok(Some(bytes))
}
fn hash(path: &Path) -> Result<Option<String>, String> {
    Ok(read_text(path)?.map(|bytes| crate::sha256_hex(&bytes)))
}
fn blob(
    directory: &Path,
    index: usize,
    before: bool,
    expected: &Option<String>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(expected) = expected else {
        return Ok(None);
    };
    let path = directory.join(format!(
        "{index:03}.{}",
        if before { "before" } else { "after" }
    ));
    crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
    let bytes = read_text(&path)?.ok_or("revision backup is missing")?;
    if crate::sha256_hex(&bytes) != *expected {
        return Err("revision backup failed integrity verification".into());
    }
    Ok(Some(bytes))
}
fn apply_bytes(path: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    match bytes {
        Some(bytes) => durable(path, bytes),
        None => {
            if path.exists() {
                std::fs::remove_file(path).map_err(|error| error.to_string())?;
            }
            Ok(())
        }
    }
}
fn clear_active(root: &Path) -> Result<(), String> {
    let path = active(root)?;
    if path.exists() {
        std::fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}
fn restore_authorized(
    root: &Path,
    directory: &Path,
    journal: &mut Journal,
    protected_publication: bool,
    authority: &dyn Fn() -> Result<(), String>,
    directories: Option<&dyn DirectoryAuthority>,
) -> Result<Value, String> {
    operation_phase(crate::operation_context::Phase::Restoring);
    operation_check()?;
    let mut conflicts = Vec::new();
    if let Some(binding) = &journal.external {
        validate_external_binding(binding, &journal.rows)?;
        validate_external_directories(journal)?;
        for path in &binding.cleanup_dirs {
            let owned = journal
                .external_owned_dirs
                .iter()
                .find(|owned| &owned.path == path);
            let checked = directories
                .ok_or("external cleanup directory authority missing")?
                .verify(path, owned.map(|owned| owned.identity.as_str()));
            if let Err(error) = checked {
                conflicts.push(
                    json!({"path":path,"reason":"directory_ownership_conflict","error":error}),
                );
            }
        }
    }
    for (index, row) in journal.rows.iter().enumerate() {
        operation_check()?;
        let path = crate::project_ops::managed_path(root, &row.path)?;
        let current = hash(&path)?;
        if current != row.before && current != row.after {
            conflicts.push(json!({"path":row.path,"actual_sha256":current}));
        }
        blob(directory, index, true, &row.before)?;
    }
    if !conflicts.is_empty() {
        recovery_required();
        journal.state = State::RecoveryConflict;
        save(directory, journal)?;
        return Ok(
            json!({"ok":false,"recovery_blocked":true,"recovery_required":true,"conflicts":conflicts,"revision_id":journal.id}),
        );
    }
    let commit = if protected_publication {
        None
    } else {
        authority()?;
        commit_barrier()?
    };
    let result = restore_protected(root, directory, journal, directories);
    match &result {
        Ok(value) if value["ok"] == true => {
            if let Some(commit) = commit {
                commit.finish();
            }
        }
        _ => recovery_required(),
    }
    result
}

fn restore_protected(
    root: &Path,
    directory: &Path,
    journal: &mut Journal,
    directories: Option<&dyn DirectoryAuthority>,
) -> Result<Value, String> {
    journal.state = State::RollingBack;
    save(directory, journal)?;
    for (index, row) in journal.rows.iter().enumerate().rev() {
        if let Some(directories) = directories {
            directories.check()?;
        }
        let path = crate::project_ops::managed_path(root, &row.path)?;
        let current = hash(&path)?;
        if current == row.before {
            continue;
        }
        if current != row.after {
            recovery_required();
            journal.state = State::RecoveryConflict;
            save(directory, journal)?;
            return Ok(
                json!({"ok":false,"recovery_blocked":true,"recovery_required":true,"revision_id":journal.id,"conflicts":[{"path":row.path,"actual_sha256":current}]}),
            );
        }
        let bytes = blob(directory, index, true, &row.before)?;
        apply_bytes(&path, bytes.as_deref())?;
        if hash(&path)? != row.before {
            return Err("restored file failed hash verification; recovery remains required".into());
        }
    }
    for relative in journal.created_dirs.iter().rev() {
        let path = crate::project_ops::managed_path(root, relative)?;
        if path.exists() {
            let removed = if journal.external.is_some() {
                let owned = journal
                    .external_owned_dirs
                    .iter()
                    .find(|owned| &owned.path == relative)
                    .ok_or("external cleanup ownership record missing")?;
                directories
                    .ok_or("external cleanup directory authority missing")?
                    .remove(relative, &owned.identity)
            } else {
                std::fs::remove_dir(&path).map_err(|error| error.to_string())
            };
            if let Err(error) = removed {
                recovery_required();
                journal.state = State::RecoveryConflict;
                save(directory, journal)?;
                return Ok(
                    json!({"ok":false,"recovery_blocked":true,"recovery_required":true,
                    "revision_id":journal.id,"conflicts":[{"path":relative,
                    "reason":"created_directory_could_not_be_restored","error":error.to_string()}]}),
                );
            }
        }
    }
    journal.state = State::RolledBack;
    save(directory, journal)?;
    clear_active(root)?;
    Ok(json!({"ok":true,"rolled_back":true,"revision_id":journal.id}))
}

pub fn recover(project: &Project) -> Result<Value, String> {
    recover_root(&project.root)
}

pub(crate) fn recover_root(root: &Path) -> Result<Value, String> {
    operation_phase(crate::operation_context::Phase::Restoring);
    operation_check()?;
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())?;
    let _lease = crate::content_lock::ContentLock::exclusive(&root)?;
    recover_locked(&root, None, None, &|| Ok(()), None)
}
fn recover_locked(
    root: &Path,
    expected: Option<&ExternalRepairScope>,
    authorization: Option<&str>,
    authority: &dyn Fn() -> Result<(), String>,
    directories: Option<&dyn DirectoryAuthority>,
) -> Result<Value, String> {
    authority()?;
    let pointer = active(&root)?;
    if !pointer.exists() {
        return Ok(json!({"ok":true,"recovered":false}));
    }
    if std::fs::metadata(&pointer)
        .map_err(|error| error.to_string())?
        .len()
        > 4096
    {
        return Err("active revision pointer exceeds its size bound".into());
    }
    let value: Value =
        serde_json::from_slice(&std::fs::read(&pointer).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if value["version"] == 3 {
        return Err("confined external V3 marker requires fresh bound private authority".into());
    }
    let revision = value["revision_id"]
        .as_str()
        .ok_or("invalid active revision pointer")?;
    id(revision)?;
    let directory = state_root(&root)?.join(revision);
    let mut journal = load(&directory)?;
    let actual_scope = journal
        .external
        .as_ref()
        .map(|_| external_scope(&journal))
        .transpose()?;
    if actual_scope.as_ref() != expected {
        return Err("fresh bound external source recovery authority required".into());
    }
    if journal.id != revision {
        return Err("active pointer and journal identity disagree".into());
    }
    if let (Some(binding), Some(digest)) = (&mut journal.external, authorization) {
        binding.authorization_digest = digest.into();
        save(&directory, &journal)?;
    }
    if journal.state == State::Committed || journal.state == State::RolledBack {
        let commit = commit_barrier()?;
        if let Err(error) = clear_active(&root) {
            recovery_required();
            return Err(error);
        }
        if let Some(commit) = commit {
            commit.finish();
        }
        return Ok(
            json!({"ok":true,"recovered":true,"revision_id":revision,"content_changed":false}),
        );
    }
    restore_authorized(
        &root,
        &directory,
        &mut journal,
        false,
        authority,
        directories,
    )
}

fn publish_rows(
    root: &Path,
    changes: Vec<ChangeBytes>,
    undo_of: Option<String>,
    fail_after: Option<usize>,
) -> Result<Value, String> {
    publish_rows_with(root, changes, undo_of, fail_after, |_| {})
}

fn publish_rows_with(
    root: &Path,
    changes: Vec<ChangeBytes>,
    undo_of: Option<String>,
    fail_after: Option<usize>,
    observe: impl FnMut(Option<usize>),
) -> Result<Value, String> {
    publish_rows_bound_with(root, changes, undo_of, None, fail_after, observe)
}
fn publish_rows_bound_with(
    root: &Path,
    changes: Vec<ChangeBytes>,
    undo_of: Option<String>,
    external: Option<ExternalBinding>,
    fail_after: Option<usize>,
    observe: impl FnMut(Option<usize>),
) -> Result<Value, String> {
    publish_rows_authorized_with(
        root,
        changes,
        undo_of,
        external,
        fail_after,
        observe,
        || Ok(()),
        None,
    )
}
fn publish_rows_authorized_with(
    root: &Path,
    changes: Vec<ChangeBytes>,
    undo_of: Option<String>,
    external: Option<ExternalBinding>,
    fail_after: Option<usize>,
    mut observe: impl FnMut(Option<usize>),
    authority: impl Fn() -> Result<(), String>,
    directories: Option<&dyn DirectoryAuthority>,
) -> Result<Value, String> {
    operation_phase(crate::operation_context::Phase::Publishing);
    operation_check()?;
    authority()?;
    ensure_writable(root)?;
    if changes.is_empty() || changes.len() > MAX_ROWS {
        return Err("revision exceeds supported row bounds".into());
    }
    let total: usize = changes
        .iter()
        .map(|(_, before, after)| {
            before.as_ref().map_or(0, Vec::len) + after.as_ref().map_or(0, Vec::len)
        })
        .sum();
    if total > MAX_BACKUP_BYTES {
        return Err("revision backups exceed the 32 MiB recovery budget".into());
    }
    history_budget(root, total as u64)?;
    let directory = state_root(root)?.join(crate::random::session_id());
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let revision = directory
        .file_name()
        .ok_or("invalid revision directory")?
        .to_string_lossy()
        .into_owned();
    let mut journal = Journal {
        version: if external.is_some() { 2 } else { 1 },
        id: revision.clone(),
        state: State::Prepared,
        rows: Vec::new(),
        created_dirs: Vec::new(),
        undo_of,
        external,
        external_owned_dirs: Vec::new(),
    };
    let prepared = (|| -> Result<(), String> {
        for (index, (relative, before, after)) in changes.iter().enumerate() {
            operation_check()?;
            let path = crate::project_ops::managed_path(root, relative)?;
            if hash(&path)? != before.as_ref().map(|bytes| crate::sha256_hex(bytes)) {
                return Err("content changed before revision preparation".into());
            }
            for (name, bytes) in [("before", before), ("after", after)] {
                if let Some(bytes) = bytes {
                    durable(&directory.join(format!("{index:03}.{name}")), bytes)?;
                }
            }
            let mut missing = Vec::new();
            let mut parent = path.parent();
            while let Some(path) = parent {
                if path.exists() {
                    break;
                }
                missing.push(
                    path.strip_prefix(root)
                        .map_err(|error| error.to_string())?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                parent = path.parent();
            }
            if journal.external.is_none() {
                for entry in missing.into_iter().rev() {
                    if !journal.created_dirs.contains(&entry) {
                        journal.created_dirs.push(entry);
                    }
                }
            }
            journal.rows.push(Row {
                path: relative.clone(),
                before: before.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                after: after.as_ref().map(|bytes| crate::sha256_hex(bytes)),
            });
        }
        if let Some(binding) = &journal.external {
            validate_external_binding(binding, &journal.rows)?;
            if planned_external_cleanup(root, binding)? != binding.cleanup_dirs {
                return Err("external planned cleanup parents changed before publication".into());
            }
            if !binding.cleanup_dirs.is_empty() && directories.is_none() {
                return Err("external cleanup directory authority missing".into());
            }
        }
        save(&directory, &journal)?;
        observe(None);
        operation_check()?;
        authority()?;
        Ok(())
    })();
    if let Err(error) = prepared {
        discard_prepared(&directory)?;
        return Err(error);
    }
    let mut adapter = LegacyPublication {
        root,
        directory: &directory,
        journal: &mut journal,
        directories,
    };
    finish_publication(&mut adapter, fail_after, observe)
}

/// Both legacy and confined revisions use this publication/compensation state
/// machine. The adapter owns all filesystem operations and recorded identities.
trait PublicationIo {
    // Revision/row/undo metadata is immutable for this adapter's lifetime.
    // activate may persist evidence before returning Err; has_active must then
    // conservatively retain the recovery boundary, including uncertain writes.
    fn revision(&self) -> &str;
    fn rows(&self) -> usize;
    fn undo_of(&self) -> Option<&str>;
    fn abort_prepared(&mut self) -> Result<(), String>;
    fn activate(&mut self) -> Result<(), String>;
    fn transition(&mut self, state: State) -> Result<(), String>;
    fn directories(&mut self) -> Result<(), String>;
    fn check(&self) -> Result<(), String>;
    fn apply(&mut self, row: usize) -> Result<(), String>;
    fn has_active(&self) -> Result<bool, String>;
    fn restore(&mut self) -> Result<Value, String>;
    fn clear(&mut self) -> Result<(), String>;
}
struct LegacyPublication<'a> {
    root: &'a Path,
    directory: &'a Path,
    journal: &'a mut Journal,
    directories: Option<&'a dyn DirectoryAuthority>,
}
impl PublicationIo for LegacyPublication<'_> {
    fn revision(&self) -> &str {
        &self.journal.id
    }
    fn rows(&self) -> usize {
        self.journal.rows.len()
    }
    fn undo_of(&self) -> Option<&str> {
        self.journal.undo_of.as_deref()
    }
    fn abort_prepared(&mut self) -> Result<(), String> {
        discard_prepared(self.directory)
    }
    fn activate(&mut self) -> Result<(), String> {
        durable(
            &active(self.root)?,
            &serde_json::to_vec(&json!({"revision_id":self.journal.id}))
                .map_err(|e| e.to_string())?,
        )
    }
    fn transition(&mut self, state: State) -> Result<(), String> {
        self.journal.state = state;
        save(self.directory, self.journal)
    }
    fn directories(&mut self) -> Result<(), String> {
        if let Some(binding) = &self.journal.external {
            for relative in binding.cleanup_dirs.clone() {
                let identity = self
                    .directories
                    .ok_or("external cleanup directory authority missing")?
                    .create(&relative)?;
                self.journal.created_dirs.push(relative.clone());
                self.journal.external_owned_dirs.push(ExternalDirectory {
                    path: relative,
                    identity,
                });
                save(self.directory, self.journal)?;
            }
        }
        Ok(())
    }
    fn check(&self) -> Result<(), String> {
        if let Some(d) = self.directories {
            d.check()?
        }
        Ok(())
    }
    fn apply(&mut self, index: usize) -> Result<(), String> {
        let row = &self.journal.rows[index];
        let path = crate::project_ops::managed_path(self.root, &row.path)?;
        if hash(&path)? != row.before {
            return Err("content changed during revision publication".into());
        }
        let bytes = blob(self.directory, index, false, &row.after)?;
        apply_bytes(&path, bytes.as_deref())?;
        if hash(&path)? != row.after {
            return Err("published file failed hash verification".into());
        }
        Ok(())
    }
    fn has_active(&self) -> Result<bool, String> {
        Ok(active(self.root)?.exists())
    }
    fn restore(&mut self) -> Result<Value, String> {
        restore_authorized(
            self.root,
            self.directory,
            self.journal,
            true,
            &|| Ok(()),
            self.directories,
        )
    }
    fn clear(&mut self) -> Result<(), String> {
        clear_active(self.root)
    }
}
fn finish_publication(
    io: &mut dyn PublicationIo,
    fail_after: Option<usize>,
    mut observe: impl FnMut(Option<usize>),
) -> Result<Value, String> {
    let revision = io.revision().to_owned();
    let mut commit = match commit_barrier() {
        Ok(commit) => commit,
        Err(error) => {
            io.abort_prepared()?;
            return Err(error);
        }
    };
    let result = (|| -> Result<(), String> {
        io.activate()?;
        io.transition(State::Publishing)?;
        io.directories()?;
        for index in 0..io.rows() {
            io.check()?;
            observe(Some(index));
            #[cfg(test)]
            if std::env::var("AURUM_TRANSACTION_CRASH_AFTER")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                == Some(index)
            {
                std::process::exit(91);
            }
            if fail_after == Some(index) {
                return Err("injected publication interruption".into());
            }
            io.apply(index)?;
        }
        io.transition(State::Committed)?;
        Ok(())
    })();
    if let Err(error) = result {
        let recovery = if io.has_active()? {
            io.restore().unwrap_or_else(|error| {
                recovery_required();
                json!({"ok":false,"recovery_required":true,"error":error,"revision_id":revision})
            })
        } else {
            json!({"ok":true,"rolled_back":true,"content_changed":false,"revision_id":revision})
        };
        let restored = recovery["ok"] == true && recovery["rolled_back"] == true;
        if restored {
            if let Some(commit) = commit.take() {
                commit.rollback_complete();
            }
        } else {
            recovery_required();
        }
        return Ok(
            json!({"ok":false,"applied":false,"error":error,"revision_id":revision,
                "source_rolled_back":restored,"recovery_required":!restored,"recovery":recovery}),
        );
    }
    if let Some(commit) = commit.take() {
        commit.finish();
    }
    if let Err(error) = io.clear() {
        recovery_required();
        return Ok(json!({"ok":false,"applied":true,"revision_id":revision,
            "recovery_required":true,"error":error,"files_changed":io.rows(),"undo_of":io.undo_of()}));
    }
    Ok(
        json!({"ok":true,"applied":true,"revision_id":revision,"files_changed":io.rows(),"undo_of":io.undo_of()}),
    )
}

fn history_budget(root: &Path, additional: u64) -> Result<(), String> {
    let directory = state_root(root)?;
    let mut bytes = 0u64;
    let mut count = 0;
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if id(&name).is_err() {
            continue;
        }
        count += 1;
        if count >= 32 {
            return Err("revision history reached its 32-entry limit; explicitly forget an old completed revision".into());
        }
        crate::diagnostics::checked_path(&entry.path()).map_err(|error| error.to_string())?;
        for file in std::fs::read_dir(entry.path()).map_err(|error| error.to_string())? {
            let file = file.map_err(|error| error.to_string())?;
            let metadata =
                std::fs::symlink_metadata(file.path()).map_err(|error| error.to_string())?;
            if !metadata.is_file() {
                return Err("unexpected entry in revision history".into());
            }
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    if bytes
        .saturating_add(additional)
        .saturating_add(MAX_JOURNAL_BYTES)
        > MAX_HISTORY_BYTES
    {
        return Err("revision history reached its 128 MiB budget; explicitly forget an old completed revision".into());
    }
    Ok(())
}

pub fn forget(project: &Project, revision: &str) -> Result<Value, String> {
    forget_with(
        project,
        revision,
        || {},
        |path| std::fs::remove_dir_all(path),
    )
}

fn forget_with(
    project: &Project,
    revision: &str,
    prepared: impl FnOnce(),
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<Value, String> {
    operation_phase(crate::operation_context::Phase::Cleanup);
    operation_check()?;
    id(revision)?;
    let _lease = crate::content_lock::ContentLock::exclusive(&project.root)?;
    ensure_writable(&project.root)?;
    let base = state_root(&project.root)?;
    let directory = base.join(revision);
    let journal = load(&directory)?;
    if journal.external.is_some() {
        return Err(
            "external source history requires a separately attended cleanup authority".into(),
        );
    }
    if journal.id != revision || !matches!(journal.state, State::Committed | State::RolledBack) {
        return Err("only completed revisions can be forgotten".into());
    }
    let canonical = crate::project::clean_path(
        directory
            .canonicalize()
            .map_err(|error| error.to_string())?,
    );
    let expected =
        crate::project::clean_path(base.canonicalize().map_err(|error| error.to_string())?)
            .join(revision);
    if canonical != expected {
        return Err("revision cleanup refused a redirected path".into());
    }
    prepared();
    operation_check()?;
    let commit = commit_barrier()?;
    if let Err(error) = remove(&directory) {
        recovery_required();
        return Ok(
            json!({"ok":false,"forgotten_revision":revision,"content_changed":false,
            "deletion_complete":false,"recovery_required":true,"error":error.to_string()}),
        );
    }
    if let Some(commit) = commit {
        commit.finish();
    }
    Ok(
        json!({"ok":true,"forgotten_revision":revision,"content_changed":false,"undo_available":false}),
    )
}

pub(crate) struct GeneratedArtifact {
    pub path: String,
    pub sha256: String,
}
impl GeneratedArtifact {
    pub(crate) fn verified_path(&self, root: &Path) -> Result<PathBuf, String> {
        if self.path.is_empty()
            || self.path.len() > 4096
            || self.path.contains('\\')
            || self
                .path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !crate::snapshot::includes(&self.path)
            || !Path::new(&self.path).extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case(crate::build::dynamic_library_extension())
            })
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid generated native artifact exemption".into());
        }
        let path = crate::project_ops::managed_path(root, &self.path)?;
        crate::diagnostics::checked_path(&path).map_err(|error| error.to_string())?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > 128 * 1024 * 1024 {
            return Err(
                "generated native library must be a regular file of at most 128 MiB".into(),
            );
        }
        if crate::sha256_file(&path).map_err(|error| error.to_string())? != self.sha256 {
            return Err("compiled candidate library changed before source publication".into());
        }
        Ok(path)
    }
}
fn path_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}
pub(crate) fn publish_candidate_with_artifacts(
    project: &Project,
    candidate: &Project,
    source_sha: &str,
    candidate_sha: &str,
    checked: &Value,
    artifacts: &[GeneratedArtifact],
    private_imports: &crate::private_import_artifacts::PrivateArtifacts,
) -> Result<Value, String> {
    operation_phase(crate::operation_context::Phase::Publishing);
    operation_check()?;
    let _lease = crate::content_lock::ContentLock::exclusive(&project.root)?;
    ensure_writable(&project.root)?;
    if crate::snapshot::fingerprint_all(&project.root)? != source_sha
        || crate::snapshot::fingerprint_all(&candidate.root)? != candidate_sha
    {
        return Err("source or candidate changed before publication; revalidate".into());
    }
    let mut paths: Vec<String> = checked["changes"]
        .as_array()
        .ok_or("missing checked changes")?
        .iter()
        .map(|row| {
            row["path"]
                .as_str()
                .ok_or("missing checked path")
                .map(str::to_string)
        })
        .collect::<Result<_, _>>()?;
    if artifacts.len() > 1 {
        return Err("native source revision accepts at most one generated library".into());
    }
    for artifact in artifacts {
        if paths
            .iter()
            .any(|path| path_key(path) == path_key(&artifact.path))
        {
            return Err("invalid generated native artifact exemption".into());
        }
        artifact.verified_path(&candidate.root)?;
    }
    private_imports.verify(candidate, project, checked)?;
    for (relative, _) in crate::snapshot::inventory(&project.root)? {
        if paths.contains(&relative)
            || artifacts
                .iter()
                .any(|artifact| path_key(&artifact.path) == path_key(&relative))
        {
            continue;
        }
        let original = crate::project_ops::managed_path(&project.root, &relative)?;
        let copied = crate::project_ops::managed_path(&candidate.root, &relative)?;
        if !copied.is_file()
            || crate::sha256_file(&original).map_err(|error| error.to_string())?
                != crate::sha256_file(&copied).map_err(|error| error.to_string())?
        {
            return Err(crate::private_import_artifacts::refusal(
                "validation removed or altered unrequested source content",
                &relative,
            ));
        }
    }
    for (relative, _) in crate::snapshot::inventory(&candidate.root)? {
        if paths.contains(&relative)
            || private_imports.contains(&relative)
            || artifacts
                .iter()
                .any(|artifact| path_key(&artifact.path) == path_key(&relative))
        {
            continue;
        }
        let original = crate::project_ops::managed_path(&project.root, &relative)?;
        let copied = crate::project_ops::managed_path(&candidate.root, &relative)?;
        if original.exists() {
            if crate::sha256_file(&original).map_err(|error| error.to_string())?
                != crate::sha256_file(&copied).map_err(|error| error.to_string())?
            {
                return Err(crate::private_import_artifacts::refusal(
                    "validation altered unrequested source content",
                    &relative,
                ));
            }
        } else if relative.ends_with(".uid") {
            let bytes = read_text(&copied)?.ok_or("generated UID file disappeared")?;
            let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
            if bytes.len() > 128
                || !text.trim().strip_prefix("uid://").is_some_and(|value| {
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            {
                return Err("unsupported generated UID metadata".into());
            }
            paths.push(relative);
        } else {
            return Err(crate::private_import_artifacts::refusal(
                "validation generated unsupported source files",
                &relative,
            ));
        }
    }
    let mut changes = Vec::new();
    for relative in paths {
        let before = read_text(&crate::project_ops::managed_path(&project.root, &relative)?)?;
        let after = read_text(&crate::project_ops::managed_path(
            &candidate.root,
            &relative,
        )?)?;
        changes.push((relative, before, after));
    }
    publish_rows(&project.root, changes, None, None)
}

pub fn undo(project: &Project, revision: &str) -> Result<Value, String> {
    operation_phase(crate::operation_context::Phase::Restoring);
    operation_check()?;
    id(revision)?;
    let _lease = crate::content_lock::ContentLock::exclusive(&project.root)?;
    undo_locked(&project.root, revision, None, None, &|| Ok(()), None, None)
}
fn undo_locked(
    root: &Path,
    revision: &str,
    expected: Option<&ExternalRepairScope>,
    authorization: Option<&str>,
    authority: &dyn Fn() -> Result<(), String>,
    directories: Option<&dyn DirectoryAuthority>,
    approved_reverse_cleanup: Option<&[String]>,
) -> Result<Value, String> {
    authority()?;
    ensure_writable(root)?;
    let directory = state_root(root)?.join(revision);
    let journal = load(&directory)?;
    let actual_scope = journal
        .external
        .as_ref()
        .map(|_| external_scope(&journal))
        .transpose()?;
    if actual_scope.as_ref() != expected {
        return Err("fresh bound external source undo authority required".into());
    }
    if journal.id != revision || journal.state != State::Committed {
        return Err("only a committed revision can be undone".into());
    }
    let mut changes = Vec::new();
    for (index, row) in journal.rows.iter().enumerate() {
        operation_check()?;
        if hash(&crate::project_ops::managed_path(root, &row.path)?)? != row.after {
            return Err(
                "revision undo conflicts with newer content; newer edits were preserved".into(),
            );
        }
        changes.push((
            row.path.clone(),
            blob(&directory, index, false, &row.after)?,
            blob(&directory, index, true, &row.before)?,
        ));
    }
    let reverse = journal.external.map(|mut binding| {
        if let Some(digest) = authorization {
            binding.authorization_digest = digest.into();
        }
        for target in &mut binding.targets {
            std::mem::swap(&mut target.before, &mut target.after);
        }
        binding.cleanup_dirs = approved_reverse_cleanup.unwrap_or_default().to_vec();
        binding
    });
    publish_rows_authorized_with(
        root,
        changes,
        Some(revision.to_string()),
        reverse,
        None,
        |_| {},
        authority,
        directories,
    )
}

/// Read-only lookup, including when project configuration cannot be opened.
/// The returned provenance cannot be used as a mutation permit.
#[cfg(windows)]
pub(crate) fn external_repair_scope(
    root: &Path,
    revision: &str,
    undo: bool,
) -> Result<ExternalRepairScope, String> {
    id(revision)?;
    let _lease = crate::content_lock::ContentLock::shared(root)?;
    if !undo && active_revision(root)? != revision {
        return Err("external recovery active revision does not match".into());
    }
    let journal = load(&state_root(root)?.join(revision))?;
    if journal.id != revision || (undo && journal.state != State::Committed) {
        return Err("external revision is not eligible for the requested repair".into());
    }
    let mut before_bytes = 0usize;
    let mut after_bytes = 0usize;
    for (index, row) in journal.rows.iter().enumerate() {
        for (before, expected, total) in [
            (true, &row.before, &mut before_bytes),
            (false, &row.after, &mut after_bytes),
        ] {
            *total = total
                .checked_add(
                    blob(&state_root(root)?.join(revision), index, before, expected)?
                        .as_ref()
                        .map_or(0, Vec::len),
                )
                .ok_or("external recovery byte overflow")?;
            if *total > crate::revisions::MAX_TEXT_BYTES {
                return Err("external recovery text exceeds 8 MiB".into());
            }
        }
    }
    external_scope(&journal)
}

#[cfg(windows)]
pub(crate) fn publish_external_source(
    permit: crate::external_workspace::SourcePublicationPermit,
    candidate: crate::external_workspace::ValidatedSourceCandidate,
    budget: &crate::external_workspace::Budget<'_>,
) -> Result<Value, String> {
    let admission = permit.admit(candidate, budget)?;
    publish_external_admission(&admission, budget, None, |_| {})
}

/// Publish only the candidate retained by the exact postvalidation confirmation.
#[cfg(windows)]
pub(crate) fn publish_external_prevalidated_source(
    permit: crate::external_workspace::SourcePublicationPermit,
    budget: &crate::external_workspace::Budget<'_>,
) -> Result<Value, String> {
    let admission = permit.admit_prevalidated(budget)?;
    publish_external_admission(&admission, budget, None, |_| {})
}

#[cfg(windows)]
fn publish_external_admission(
    admission: &crate::external_workspace::SourceAdmission,
    budget: &crate::external_workspace::Budget<'_>,
    fail_after: Option<usize>,
    mut observe: impl FnMut(Option<usize>),
) -> Result<Value, String> {
    let io = crate::external_workspace::ExternalRevisionIo::publication(admission, budget)?;
    let originals = admission
        .observations()
        .iter()
        .map(|(_, object)| object.clone())
        .collect();
    let mut revision = V3Publication::prepare(
        io,
        admission.changes.clone(),
        admission.binding.clone(),
        originals,
        None,
        Vec::new(),
        Vec::new(),
        &mut observe,
    )?;
    finish_publication(&mut revision, fail_after, observe)
}

#[cfg(windows)]
pub(crate) fn repair_external_source(
    permit: crate::external_workspace::SourceRepairPermit,
    budget: &crate::external_workspace::Budget<'_>,
) -> Result<Value, String> {
    let admission = permit.admit(budget)?;
    let snapshot = admission.snapshot().clone();
    let io = crate::external_workspace::ExternalRevisionIo::repair(&admission, budget)?;
    if admission.undo {
        let changes = snapshot
            .header
            .journal
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                (
                    row.path.clone(),
                    snapshot.blobs[index].1.as_ref().map(|v| v.1.clone()),
                    snapshot.blobs[index].0.as_ref().map(|v| v.1.clone()),
                )
            })
            .collect();
        let mut binding = admission.scope.binding.clone();
        for target in &mut binding.targets {
            std::mem::swap(&mut target.before, &mut target.after)
        }
        binding.authorization_digest = admission.authorization_digest.clone();
        binding.cleanup_dirs = admission.reverse_cleanup.clone();
        let mut reverse = V3Publication::prepare(
            io,
            changes,
            binding,
            snapshot.targets.clone(),
            Some(admission.revision.clone()),
            snapshot.directory_scope(true).0,
            snapshot.directory_scope(true).1,
            |_| {},
        )?;
        finish_publication(&mut reverse, None, |_| {})
    } else {
        io.open(&admission.revision, &snapshot.header.directory)?;
        for (name, expected) in &snapshot.immutable_records {
            io.pin(name, expected)?;
        }
        let terminal = matches!(snapshot.state, State::Committed | State::RolledBack);
        let mut revision = V3Publication {
            io,
            snapshot,
            activation_attempted: true,
        };
        let commit = commit_barrier()?;
        let result = if terminal {
            revision.clear().map(|_|json!({"ok":true,"recovered":true,"revision_id":admission.revision,"content_changed":false}))
        } else {
            revision.restore_v3()
        };
        if result.as_ref().is_ok_and(|v| v["ok"] == true) {
            if let Some(commit) = commit {
                commit.finish()
            }
        } else {
            recovery_required()
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(Project);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("aurum-transaction-{}", crate::random::session_id()));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join("aurum.toml"),
                "schema_version=1\nname=\"transaction-test\"\nmodules=[]\n",
            )
            .unwrap();
            Self(Project::open(&root).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.root);
        }
    }
    fn linked_fixture() -> Fixture {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("first.gd"), b"first before").unwrap();
        std::fs::write(fixture.0.root.join("second.gd"), b"second before").unwrap();
        std::fs::write(
            fixture.0.root.join("accepted.dll"),
            b"accepted native library",
        )
        .unwrap();
        fixture
    }
    fn linked_changes() -> Vec<ChangeBytes> {
        vec![
            (
                "first.gd".into(),
                Some(b"first before".to_vec()),
                Some(b"first after".to_vec()),
            ),
            (
                "second.gd".into(),
                Some(b"second before".to_vec()),
                Some(b"second after".to_vec()),
            ),
        ]
    }
    fn job_context() -> crate::operation_context::OperationContext {
        crate::operation_context::OperationContext::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        )
    }

    #[cfg(windows)]
    struct ExternalFixture {
        external: Fixture,
        imported: Fixture,
        project: crate::external_workspace::ImportedProject,
        binding: ExternalBinding,
    }
    #[cfg(windows)]
    impl ExternalFixture {
        fn assert_v2_refused_unchanged(&self, revision: &str, undo: bool) {
            // Historical v2 evidence has no retained relocation identities.
            // A fresh confirmation cannot safely adopt that missing ownership.
            type Entry = (
                PathBuf,
                bool,
                u64,
                String,
                Option<std::time::SystemTime>,
                Option<crate::external_workspace::DirectoryIdentity>,
            );
            fn capture(
                path: &Path,
                b: &crate::external_workspace::Budget<'_>,
                rows: &mut Vec<Entry>,
            ) {
                let mut entries: Vec<_> = std::fs::read_dir(path)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
                entries.sort();
                for path in entries {
                    let metadata = std::fs::symlink_metadata(&path).unwrap();
                    assert!(!metadata.file_type().is_symlink());
                    if metadata.is_dir() {
                        let identity = crate::external_workspace::canonical_directory(&path, b)
                            .unwrap()
                            .1;
                        rows.push((
                            path.clone(),
                            true,
                            0,
                            String::new(),
                            metadata.created().ok(),
                            Some(identity),
                        ));
                        capture(&path, b, rows)
                    } else {
                        rows.push((
                            path.clone(),
                            false,
                            metadata.len(),
                            crate::sha256_file(&path).unwrap(),
                            metadata.created().ok(),
                            None,
                        ));
                    }
                }
            }
            let b = Self::budget();
            let mut before = Vec::new();
            capture(&self.external.0.root, &b, &mut before);
            capture(&self.imported.0.root, &b, &mut before);
            let result = crate::external_workspace::PreparedSourceRepair::prepare(
                &self.project,
                crate::external_workspace::RootId::new("engine").unwrap(),
                &self.external.0.root,
                revision,
                undo,
                &b,
            );
            let error = result
                .err()
                .expect("v2 must not acquire confined repair authority");
            assert!(
                error.contains("legacy_v2_or_unowned_state_preserved_unsupported"),
                "{error}"
            );
            let mut after = Vec::new();
            capture(&self.external.0.root, &b, &mut after);
            capture(&self.imported.0.root, &b, &mut after);
            assert_eq!(before, after);
        }
        fn new() -> Self {
            use crate::external_workspace::{
                canonical_directory, current_revision, ImportedProject, Revision,
            };
            let external = Fixture::new();
            let imported = Fixture::new();
            std::fs::create_dir_all(external.0.root.join("native/src")).unwrap();
            std::fs::write(
                external.0.root.join("native/src/first.rs"),
                b"first before\n",
            )
            .unwrap();
            std::fs::write(
                external.0.root.join("native/src/second.rs"),
                b"second before\n",
            )
            .unwrap();
            let b = Self::budget();
            let (imported_path, imported_identity) =
                canonical_directory(&imported.0.root, &b).unwrap();
            let (external_path, external_identity) =
                canonical_directory(&external.0.root, &b).unwrap();
            let revision = current_revision(&imported_path, &b).unwrap();
            let project =
                ImportedProject::bind(&imported_path, Revision::new(&revision).unwrap(), &b)
                    .unwrap();
            let binding = ExternalBinding {
                version: 2,
                imported_path: imported_path.to_string_lossy().into_owned(),
                imported_identity: crate::sha256_hex(format!("{imported_identity:?}").as_bytes()),
                imported_revision: revision,
                external_path: external_path.to_string_lossy().into_owned(),
                external_identity: crate::sha256_hex(format!("{external_identity:?}").as_bytes()),
                root_id: "engine".into(),
                member_src: "native/src".into(),
                proposal_digest: "a".repeat(64),
                authorization_digest: "b".repeat(64),
                cleanup_dirs: Vec::new(),
                targets: Self::changes()
                    .iter()
                    .map(|(path, before, after)| ExternalTarget {
                        path: path.clone(),
                        before: before.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                        after: after.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                    })
                    .collect(),
            };
            Self {
                external,
                imported,
                project,
                binding,
            }
        }
        fn budget() -> crate::external_workspace::Budget<'static> {
            crate::external_workspace::Budget::new(std::time::Duration::from_secs(30), &|| Ok(()))
                .unwrap()
        }
        fn changes() -> Vec<ChangeBytes> {
            vec![
                (
                    "native/src/first.rs".into(),
                    Some(b"first before\n".to_vec()),
                    Some(b"first after\n".to_vec()),
                ),
                (
                    "native/src/second.rs".into(),
                    Some(b"second before\n".to_vec()),
                    Some(b"second after\n".to_vec()),
                ),
            ]
        }
        fn interrupt(&self, boundary: Option<usize>) -> String {
            // This exercises the existing journal engine, with an abrupt stack
            // interruption rather than its immediate compensation failpoint.
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                publish_rows_bound_with(
                    &self.external.0.root,
                    Self::changes(),
                    None,
                    Some(self.binding.clone()),
                    None,
                    |at| {
                        if at == boundary {
                            panic!("filesystem interruption fixture")
                        }
                    },
                )
                .unwrap();
            }))
            .is_err());
            if boundary.is_some() {
                active_revision(&self.external.0.root).unwrap()
            } else {
                history(&self.external.0.root, &json!({})).unwrap()["revisions"][0]["revision_id"]
                    .as_str()
                    .unwrap()
                    .into()
            }
        }
        fn nested_interruption(&mut self) -> String {
            let cleanup = vec![
                "native/src/new".to_string(),
                "native/src/new/deep".to_string(),
            ];
            let changes = vec![
                (
                    "native/src/new/deep/created.rs".to_string(),
                    None,
                    Some(b"created\n".to_vec()),
                ),
                (
                    "native/src/first.rs".into(),
                    Some(b"first before\n".to_vec()),
                    Some(b"first after\n".to_vec()),
                ),
            ];
            self.binding.targets = changes
                .iter()
                .map(|(path, before, after)| ExternalTarget {
                    path: path.clone(),
                    before: before.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                    after: after.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                })
                .collect();
            self.binding.cleanup_dirs = cleanup.clone();
            let revision = crate::random::session_id();
            let directory = state_root(&self.external.0.root).unwrap().join(&revision);
            std::fs::create_dir_all(&directory).unwrap();
            let mut owned = Vec::new();
            for path in &cleanup {
                let full = self.external.0.root.join(path);
                std::fs::create_dir(&full).unwrap();
                let identity =
                    crate::external_workspace::canonical_directory(&full, &Self::budget())
                        .unwrap()
                        .1;
                owned.push(ExternalDirectory {
                    path: path.clone(),
                    identity: crate::sha256_hex(format!("{identity:?}").as_bytes()),
                });
            }
            let mut rows = Vec::new();
            for (index, (path, before, after)) in changes.iter().enumerate() {
                for (name, bytes) in [("before", before), ("after", after)] {
                    if let Some(bytes) = bytes {
                        durable(&directory.join(format!("{index:03}.{name}")), bytes).unwrap();
                    }
                }
                rows.push(Row {
                    path: path.clone(),
                    before: before.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                    after: after.as_ref().map(|bytes| crate::sha256_hex(bytes)),
                });
            }
            std::fs::write(
                self.external.0.root.join(&changes[0].0),
                changes[0].2.as_ref().unwrap(),
            )
            .unwrap();
            save(
                &directory,
                &Journal {
                    version: 2,
                    id: revision.clone(),
                    state: State::Publishing,
                    rows,
                    created_dirs: cleanup,
                    undo_of: None,
                    external: Some(self.binding.clone()),
                    external_owned_dirs: owned,
                },
            )
            .unwrap();
            durable(
                &active(&self.external.0.root).unwrap(),
                &serde_json::to_vec(&json!({"revision_id":revision})).unwrap(),
            )
            .unwrap();
            revision
        }
        fn broker() -> std::sync::Arc<crate::workspace_access::Broker> {
            std::sync::Arc::new(crate::workspace_access::Broker::default())
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_legacy_v2_repair_is_explicitly_unsupported_and_preserves_evidence() {
        let fixture = ExternalFixture::new();
        let revision = fixture.interrupt(Some(1));
        fn capture(path: &Path, rows: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            entries.sort();
            for entry in entries {
                let metadata = std::fs::symlink_metadata(&entry).unwrap();
                assert!(!metadata.file_type().is_symlink());
                if metadata.is_dir() {
                    capture(&entry, rows)
                } else {
                    rows.push((entry.clone(), std::fs::read(entry).unwrap()))
                }
            }
        }
        let mut before = Vec::new();
        capture(&fixture.external.0.root, &mut before);
        let result = crate::external_workspace::PreparedSourceRepair::prepare(
            &fixture.project,
            crate::external_workspace::RootId::new("engine").unwrap(),
            &fixture.external.0.root,
            &revision,
            false,
            &ExternalFixture::budget(),
        );
        assert!(result
            .err()
            .unwrap()
            .contains("legacy_v2_or_unowned_state_preserved_unsupported"));
        let mut after = Vec::new();
        capture(&fixture.external.0.root, &mut after);
        assert_eq!(before, after);
        assert_eq!(active_revision(&fixture.external.0.root).unwrap(), revision);
        assert!(crate::external_workspace::current_revision(
            &fixture.external.0.root,
            &ExternalFixture::budget()
        )
        .is_err());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_interruption_requires_fresh_authority_even_with_invalid_project_config() {
        for boundary in [None, Some(0), Some(1)] {
            let fixture = ExternalFixture::new();
            let revision = fixture.interrupt(boundary);
            assert_eq!(
                std::fs::read(fixture.imported.0.root.join("aurum.toml")).unwrap(),
                b"schema_version=1\nname=\"transaction-test\"\nmodules=[]\n"
            );
            if boundary.is_none() {
                assert!(!active(&fixture.external.0.root).unwrap().exists());
                assert_eq!(
                    std::fs::read(fixture.external.0.root.join("native/src/first.rs")).unwrap(),
                    b"first before\n"
                );
            } else {
                assert!(recover_root(&fixture.external.0.root).is_err());
                assert!(crate::external_workspace::current_revision(
                    &fixture.external.0.root,
                    &ExternalFixture::budget()
                )
                .is_err());
                std::fs::write(
                    fixture.imported.0.root.join("aurum.toml"),
                    b"invalid [ configuration",
                )
                .unwrap();
                assert_eq!(
                    external_repair_scope(&fixture.external.0.root, &revision, false)
                        .unwrap()
                        .binding,
                    fixture.binding
                );
            }
            fixture.assert_v2_refused_unchanged(&revision, false);
            assert!(state_root(&fixture.external.0.root)
                .unwrap()
                .join(&revision)
                .join("000.before")
                .exists());
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_recovery_preserves_newer_edit_corrupt_backup_and_all_retained_evidence() {
        for corrupt in [false, true] {
            let fixture = ExternalFixture::new();
            let revision = fixture.interrupt(Some(1));
            let directory = state_root(&fixture.external.0.root)
                .unwrap()
                .join(&revision);
            let target = fixture.external.0.root.join("native/src/first.rs");
            if corrupt {
                std::fs::write(directory.join("000.before"), b"corrupt backup").unwrap()
            } else {
                std::fs::write(&target, b"newer external edit").unwrap()
            }
            let before = std::fs::read(&target).unwrap();
            let backup = std::fs::read(directory.join("000.before")).unwrap();
            fixture.assert_v2_refused_unchanged(&revision, false);
            assert_eq!(std::fs::read(target).unwrap(), before);
            assert_eq!(std::fs::read(directory.join("000.before")).unwrap(), backup);
            assert!(active(&fixture.external.0.root).unwrap().exists());
            assert!(ensure_writable(&fixture.external.0.root).is_err());
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_restart_cancellation_and_redirected_journal_refuse() {
        let fixture = ExternalFixture::new();
        let revision = fixture.interrupt(Some(1));
        let old = ExternalFixture::broker();
        old.close();
        drop(old);
        fixture.assert_v2_refused_unchanged(&revision, false);
        assert!(recover_root(&fixture.external.0.root).is_err());
        let copied = ExternalFixture::new();
        let directory = state_root(&copied.external.0.root).unwrap().join(&revision);
        std::fs::create_dir_all(&directory).unwrap();
        for entry in std::fs::read_dir(
            state_root(&fixture.external.0.root)
                .unwrap()
                .join(&revision),
        )
        .unwrap()
        {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), directory.join(entry.file_name())).unwrap();
        }
        durable(
            &active(&copied.external.0.root).unwrap(),
            &serde_json::to_vec(&json!({"revision_id":revision})).unwrap(),
        )
        .unwrap();
        copied.assert_v2_refused_unchanged(&revision, false);
        let _new_session = ExternalFixture::broker();
        fixture.assert_v2_refused_unchanged(&revision, false);
        assert_eq!(active_revision(&fixture.external.0.root).unwrap(), revision);
        assert_eq!(active_revision(&copied.external.0.root).unwrap(), revision);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_stale_undo_and_interrupted_reverse_preserve_current_revision() {
        let fixture = ExternalFixture::new();
        let publication = publish_rows_bound_with(
            &fixture.external.0.root,
            ExternalFixture::changes(),
            None,
            Some(fixture.binding.clone()),
            None,
            |_| {},
        )
        .unwrap();
        let revision = publication["revision_id"].as_str().unwrap();
        assert!(undo(&fixture.external.0, revision).is_err());
        std::fs::write(
            fixture.external.0.root.join("native/src/first.rs"),
            b"newer edit",
        )
        .unwrap();
        fixture.assert_v2_refused_unchanged(revision, true);
        assert_eq!(
            std::fs::read(fixture.external.0.root.join("native/src/first.rs")).unwrap(),
            b"newer edit"
        );
        std::fs::write(
            fixture.external.0.root.join("native/src/first.rs"),
            b"first after\n",
        )
        .unwrap();
        let mut reverse = fixture.binding.clone();
        for target in &mut reverse.targets {
            std::mem::swap(&mut target.before, &mut target.after)
        }
        let changes = ExternalFixture::changes()
            .into_iter()
            .map(|(path, before, after)| (path, after, before))
            .collect();
        let reached = std::cell::Cell::new(false);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            publish_rows_bound_with(
                &fixture.external.0.root,
                changes,
                Some(revision.into()),
                Some(reverse),
                None,
                |at| {
                    if at == Some(1) {
                        reached.set(true);
                        panic!("legacy reverse interruption fixture")
                    }
                },
            )
            .unwrap()
        }))
        .is_err());
        assert!(reached.get());
        let active = active_revision(&fixture.external.0.root).unwrap();
        fixture.assert_v2_refused_unchanged(&active, false);
        assert!(ensure_writable(&fixture.external.0.root).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_reuses_history_entry_and_byte_quotas_without_deleting_evidence() {
        for full_by_entries in [false, true] {
            let fixture = ExternalFixture::new();
            let count = if full_by_entries { 32 } else { 1 };
            let mut evidence = Vec::new();
            for index in 0..count {
                let directory = state_root(&fixture.external.0.root)
                    .unwrap()
                    .join(format!("{index:032x}"));
                std::fs::create_dir_all(&directory).unwrap();
                let path = directory.join("retained-proof");
                let file = std::fs::File::create(&path).unwrap();
                file.set_len(if full_by_entries {
                    1
                } else {
                    MAX_HISTORY_BYTES
                })
                .unwrap();
                evidence.push((
                    path,
                    if full_by_entries {
                        1
                    } else {
                        MAX_HISTORY_BYTES
                    },
                ));
            }
            assert!(publish_rows_bound_with(
                &fixture.external.0.root,
                ExternalFixture::changes(),
                None,
                Some(fixture.binding.clone()),
                None,
                |_| {}
            )
            .is_err());
            for (path, bytes) in evidence {
                assert_eq!(std::fs::metadata(path).unwrap().len(), bytes);
            }
            for (path, before, _) in ExternalFixture::changes() {
                assert_eq!(
                    std::fs::read(fixture.external.0.root.join(path)).unwrap(),
                    before.unwrap()
                );
            }
            assert!(!active(&fixture.external.0.root).unwrap().exists());
            fixture.assert_v2_refused_unchanged(&"0".repeat(32), false);
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_cleanup_scope_forgery_before_approval_never_removes_directories() {
        for forged in [
            "native/src",
            "native/src/unrelated",
            "native/src/new/../other",
            "native/src/new/",
            "native/src/new",
        ] {
            let mut fixture = ExternalFixture::new();
            let revision = fixture.nested_interruption();
            let unrelated = fixture.external.0.root.join("native/src/unrelated");
            std::fs::create_dir(&unrelated).unwrap();
            let original_identity = crate::external_workspace::canonical_directory(
                &unrelated,
                &ExternalFixture::budget(),
            )
            .unwrap()
            .1;
            let directory = state_root(&fixture.external.0.root)
                .unwrap()
                .join(&revision);
            let mut journal = load(&directory).unwrap();
            journal.created_dirs.push(forged.into());
            journal.external_owned_dirs.push(ExternalDirectory {
                path: forged.into(),
                identity: "f".repeat(64),
            });
            journal
                .external
                .as_mut()
                .unwrap()
                .cleanup_dirs
                .push(forged.into());
            save(&directory, &journal).unwrap();
            assert!(crate::external_workspace::PreparedSourceRepair::prepare(
                &fixture.project,
                crate::external_workspace::RootId::new("engine").unwrap(),
                &fixture.external.0.root,
                &revision,
                false,
                &ExternalFixture::budget()
            )
            .is_err());
            assert_eq!(
                crate::external_workspace::canonical_directory(
                    &unrelated,
                    &ExternalFixture::budget()
                )
                .unwrap()
                .1,
                original_identity
            );
            assert_eq!(
                std::fs::read(
                    fixture
                        .external
                        .0
                        .root
                        .join("native/src/new/deep/created.rs")
                )
                .unwrap(),
                b"created\n"
            );
            assert!(directory.join("000.after").exists());
            assert!(active(&fixture.external.0.root).unwrap().exists());
            fixture.assert_v2_refused_unchanged(&revision, false);
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_cleanup_ownership_changes_after_approval_invalidate_the_permit() {
        // V2 cannot obtain the initial approval. The authentic V3 postapproval
        // ownership-change case is covered separately in external_workspace.
        let mut fixture = ExternalFixture::new();
        let revision = fixture.nested_interruption();
        fixture.assert_v2_refused_unchanged(&revision, false);
        let directory = state_root(&fixture.external.0.root)
            .unwrap()
            .join(&revision);
        let mut journal = load(&directory).unwrap();
        journal.created_dirs.pop();
        journal.external_owned_dirs.pop();
        save(&directory, &journal).unwrap();
        assert!(load(&directory).is_ok());
        fixture.assert_v2_refused_unchanged(&revision, false);
        assert_eq!(
            std::fs::read(
                fixture
                    .external
                    .0
                    .root
                    .join("native/src/new/deep/created.rs")
            )
            .unwrap(),
            b"created\n"
        );
        assert!(fixture.external.0.root.join("native/src/new/deep").is_dir());
        assert!(directory.join("000.after").exists());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_nested_recovery_deletes_only_owned_empty_directories() {
        // Missing v2 ownership must preserve even an empty candidate directory.
        for with_sentinel in [false, true] {
            let mut fixture = ExternalFixture::new();
            let revision = fixture.nested_interruption();
            let keep = fixture.external.0.root.join("native/src/preexisting-empty");
            std::fs::create_dir(&keep).unwrap();
            let identity =
                crate::external_workspace::canonical_directory(&keep, &ExternalFixture::budget())
                    .unwrap()
                    .1;
            let sentinel = fixture
                .external
                .0
                .root
                .join("native/src/new/deep/unrelated.keep");
            if with_sentinel {
                std::fs::write(&sentinel, b"newer directory sentinel").unwrap()
            }
            fixture.assert_v2_refused_unchanged(&revision, false);
            if with_sentinel {
                assert_eq!(
                    std::fs::read(sentinel).unwrap(),
                    b"newer directory sentinel"
                )
            }
            assert!(fixture.external.0.root.join("native/src/new/deep").is_dir());
            assert!(active(&fixture.external.0.root).unwrap().exists());
            assert!(state_root(&fixture.external.0.root)
                .unwrap()
                .join(&revision)
                .join("000.after")
                .exists());
            assert_eq!(
                crate::external_workspace::canonical_directory(&keep, &ExternalFixture::budget())
                    .unwrap()
                    .1,
                identity
            );
            assert_eq!(
                std::fs::read(fixture.external.0.root.join("native/src/first.rs")).unwrap(),
                b"first before\n"
            );
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_unowned_planned_directory_is_a_retained_crash_conflict() {
        let mut fixture = ExternalFixture::new();
        let revision = fixture.nested_interruption();
        let directory = state_root(&fixture.external.0.root)
            .unwrap()
            .join(&revision);
        let mut journal = load(&directory).unwrap();
        // Models a crash after atomic create but before its identity record
        // became durable. A later process cannot infer ownership from presence.
        journal.created_dirs.pop();
        journal.external_owned_dirs.pop();
        save(&directory, &journal).unwrap();
        assert!(external_repair_scope(&fixture.external.0.root, &revision, false).is_ok());
        assert!(crate::external_workspace::PreparedSourceRepair::prepare(
            &fixture.project,
            crate::external_workspace::RootId::new("engine").unwrap(),
            &fixture.external.0.root,
            &revision,
            false,
            &ExternalFixture::budget()
        )
        .is_err());
        assert_eq!(
            std::fs::read(
                fixture
                    .external
                    .0
                    .root
                    .join("native/src/new/deep/created.rs")
            )
            .unwrap(),
            b"created\n"
        );
        assert!(directory.join("000.after").exists());
        assert!(active(&fixture.external.0.root).unwrap().exists());
        fixture.assert_v2_refused_unchanged(&revision, false);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_concurrently_inserted_planned_parent_is_never_adopted() {
        let mut fixture = ExternalFixture::new();
        let path = "native/src/race/deep/old.rs";
        std::fs::create_dir_all(fixture.external.0.root.join("native/src/race/deep")).unwrap();
        std::fs::write(fixture.external.0.root.join(path), b"old\n").unwrap();
        fixture.binding.targets = vec![ExternalTarget {
            path: path.into(),
            before: Some(crate::sha256_hex(b"old\n")),
            after: None,
        }];
        let publication = publish_rows_bound_with(
            &fixture.external.0.root,
            vec![(path.into(), Some(b"old\n".to_vec()), None)],
            None,
            Some(fixture.binding.clone()),
            None,
            |_| {},
        )
        .unwrap();
        let revision = publication["revision_id"].as_str().unwrap();
        std::fs::remove_dir(fixture.external.0.root.join("native/src/race/deep")).unwrap();
        std::fs::remove_dir(fixture.external.0.root.join("native/src/race")).unwrap();
        fixture.assert_v2_refused_unchanged(revision, true);
        std::fs::create_dir(fixture.external.0.root.join("native/src/race")).unwrap();
        let sentinel = fixture
            .external
            .0
            .root
            .join("native/src/race/inserted.keep");
        std::fs::write(&sentinel, b"concurrent directory owner\n").unwrap();
        let identity = crate::external_workspace::canonical_directory(
            sentinel.parent().unwrap(),
            &ExternalFixture::budget(),
        )
        .unwrap()
        .1;
        fixture.assert_v2_refused_unchanged(revision, true);
        assert_eq!(
            std::fs::read(sentinel).unwrap(),
            b"concurrent directory owner\n"
        );
        assert_eq!(
            crate::external_workspace::canonical_directory(
                &fixture.external.0.root.join("native/src/race"),
                &ExternalFixture::budget()
            )
            .unwrap()
            .1,
            identity
        );
        assert!(!fixture.external.0.root.join(path).exists());
        assert!(!active(&fixture.external.0.root).unwrap().exists());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_reverse_nested_create_is_atomic_and_interrupted_recovery_removes_its_own_directories(
    ) {
        let mut fixture = ExternalFixture::new();
        let path = "native/src/gone/deep/old.rs";
        std::fs::create_dir_all(fixture.external.0.root.join("native/src/gone/deep")).unwrap();
        std::fs::write(fixture.external.0.root.join(path), b"old source\n").unwrap();
        let changes = vec![
            (path.to_owned(), Some(b"old source\n".to_vec()), None),
            (
                "native/src/first.rs".into(),
                Some(b"first before\n".to_vec()),
                Some(b"first after\n".to_vec()),
            ),
        ];
        fixture.binding.targets = changes
            .iter()
            .map(|(path, before, after)| ExternalTarget {
                path: path.clone(),
                before: before.as_ref().map(|b| crate::sha256_hex(b)),
                after: after.as_ref().map(|b| crate::sha256_hex(b)),
            })
            .collect();
        let publication = publish_rows_bound_with(
            &fixture.external.0.root,
            changes.clone(),
            None,
            Some(fixture.binding.clone()),
            None,
            |_| {},
        )
        .unwrap();
        let revision = publication["revision_id"].as_str().unwrap();
        std::fs::remove_dir(fixture.external.0.root.join("native/src/gone/deep")).unwrap();
        std::fs::remove_dir(fixture.external.0.root.join("native/src/gone")).unwrap();
        let keep = fixture.external.0.root.join("native/src/preexisting-empty");
        std::fs::create_dir(&keep).unwrap();
        let identity =
            crate::external_workspace::canonical_directory(&keep, &ExternalFixture::budget())
                .unwrap()
                .1;
        fixture.assert_v2_refused_unchanged(revision, true);
        std::fs::create_dir_all(fixture.external.0.root.join("native/src/gone/deep")).unwrap();
        // Retain Some(1) legacy engine coverage independently of unavailable
        // confined v2 repair authority. Authentic V3 reverse is tested separately.
        let mut reverse = fixture.binding.clone();
        for target in &mut reverse.targets {
            std::mem::swap(&mut target.before, &mut target.after)
        }
        let reverse_changes = changes
            .into_iter()
            .map(|(path, before, after)| (path, after, before))
            .collect();
        let reached = std::cell::Cell::new(false);
        let normal = std::cell::RefCell::new(None);
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let result = publish_rows_bound_with(
                &fixture.external.0.root,
                reverse_changes,
                Some(revision.into()),
                Some(reverse),
                None,
                |at| {
                    if at == Some(1) {
                        reached.set(true);
                        panic!("legacy nested reverse interruption fixture")
                    }
                },
            );
            *normal.borrow_mut() = Some(result);
        }));
        assert!(reached.get(), "Some(1) not reached: {:?}", normal.borrow());
        assert!(interrupted.is_err());
        let active = active_revision(&fixture.external.0.root).unwrap();
        fixture.assert_v2_refused_unchanged(&active, false);
        assert_eq!(
            crate::external_workspace::canonical_directory(&keep, &ExternalFixture::budget())
                .unwrap()
                .1,
            identity
        );
        assert_eq!(
            std::fs::read(fixture.external.0.root.join("native/src/first.rs")).unwrap(),
            b"first after\n"
        );
    }
    #[cfg(windows)]
    #[test]
    fn external_source_session_expiry_cancel_and_broker_close_after_backups_do_not_activate() {
        for action in ["cancel", "close", "expire"] {
            let fixture = ExternalFixture::new();
            let publication = publish_rows_bound_with(
                &fixture.external.0.root,
                ExternalFixture::changes(),
                None,
                Some(fixture.binding.clone()),
                None,
                |_| {},
            )
            .unwrap();
            let revision = publication["revision_id"].as_str().unwrap();
            let broker = ExternalFixture::broker();
            let cancelled = std::sync::atomic::AtomicBool::new(false);
            match action {
                "cancel" => cancelled.store(true, std::sync::atomic::Ordering::Release),
                "close" => broker.close(),
                "expire" => {
                    let expired = crate::external_workspace::Budget::new(
                        std::time::Duration::from_millis(1),
                        &|| Ok(()),
                    )
                    .unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    assert!(expired.check().is_err());
                }
                _ => unreachable!(),
            }
            // No session can be renewed into missing v2 relocation ownership.
            fixture.assert_v2_refused_unchanged(revision, true);
            assert!(!active(&fixture.external.0.root).unwrap().exists());
            assert!(state_root(&fixture.external.0.root)
                .unwrap()
                .join(revision)
                .join("000.before")
                .exists());
        }
    }
    #[test]
    fn cancellation_after_backups_before_publication_leaves_no_active_revision() {
        let fixture = linked_fixture();
        let before = crate::snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let context = job_context();
        let result = context
            .scope(|| {
                publish_rows_with(&fixture.0.root, linked_changes(), None, None, |boundary| {
                    if boundary.is_none() {
                        context.request_cancel().unwrap();
                    }
                })
            })
            .unwrap();
        assert!(result.is_err());
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert!(!context.recovery_required());
        assert_eq!(
            crate::snapshot::fingerprint_all(&fixture.0.root).unwrap(),
            before
        );
        assert!(!active(&fixture.0.root).unwrap().exists());
        assert_eq!(
            history(&fixture.0.root, &json!({})).unwrap()["retained_revisions"],
            0
        );
    }

    #[test]
    fn cancellation_inside_linked_publication_finishes_the_verified_commit() {
        let fixture = linked_fixture();
        let context = job_context();
        let result = context
            .scope(|| {
                publish_rows_with(&fixture.0.root, linked_changes(), None, None, |boundary| {
                    if boundary == Some(0) {
                        context.request_cancel().unwrap();
                    }
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["ok"], true);
        assert!(context.committed());
        assert!(context.commit_status_verified());
        assert!(context.cancellation_requested());
        assert!(context.request_cancel().is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"first after"
        );
        assert_eq!(
            std::fs::read(fixture.0.root.join("second.gd")).unwrap(),
            b"second after"
        );
        assert_eq!(
            std::fs::read(fixture.0.root.join("accepted.dll")).unwrap(),
            b"accepted native library"
        );
        let undo_context = job_context();
        undo_context
            .scope(|| undo(&fixture.0, result["revision_id"].as_str().unwrap()))
            .unwrap()
            .unwrap();
        assert!(undo_context.committed());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"first before"
        );
        assert_eq!(
            std::fs::read(fixture.0.root.join("second.gd")).unwrap(),
            b"second before"
        );
    }

    #[test]
    fn interrupted_linked_publication_proves_full_rollback_despite_cancellation() {
        let fixture = linked_fixture();
        let context = job_context();
        let result = context
            .scope(|| {
                publish_rows_with(
                    &fixture.0.root,
                    linked_changes(),
                    None,
                    Some(1),
                    |boundary| {
                        if boundary == Some(1) {
                            context.request_cancel().unwrap();
                        }
                    },
                )
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["source_rolled_back"], true);
        assert_eq!(result["recovery_required"], false);
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert!(!context.recovery_required());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"first before"
        );
        assert_eq!(
            std::fs::read(fixture.0.root.join("second.gd")).unwrap(),
            b"second before"
        );
        assert!(!active(&fixture.0.root).unwrap().exists());
        assert_eq!(
            std::fs::read(fixture.0.root.join("accepted.dll")).unwrap(),
            b"accepted native library"
        );
    }

    #[test]
    fn cancelled_rollback_conflict_retains_newer_edits_and_recovery_evidence() {
        let fixture = linked_fixture();
        let context = job_context();
        let result = context
            .scope(|| {
                publish_rows_with(
                    &fixture.0.root,
                    linked_changes(),
                    None,
                    Some(1),
                    |boundary| {
                        if boundary == Some(1) {
                            std::fs::write(fixture.0.root.join("first.gd"), b"newer external edit")
                                .unwrap();
                            context.request_cancel().unwrap();
                        }
                    },
                )
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["source_rolled_back"], false);
        assert_eq!(result["recovery_required"], true);
        assert!(!context.committed());
        assert!(!context.commit_status_verified());
        assert!(context.recovery_required());
        assert!(active(&fixture.0.root).unwrap().exists());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"newer external edit"
        );
        assert_eq!(
            std::fs::read(fixture.0.root.join("second.gd")).unwrap(),
            b"second before"
        );
        assert!(ensure_writable(&fixture.0.root).is_err());
        std::fs::write(fixture.0.root.join("first.gd"), b"first after").unwrap();
        let recovery_context = job_context();
        let recovered = recovery_context
            .scope(|| recover(&fixture.0))
            .unwrap()
            .unwrap();
        assert_eq!(recovered["ok"], true);
        assert!(recovery_context.committed());
        assert!(recovery_context.commit_status_verified());
        assert!(!active(&fixture.0.root).unwrap().exists());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"first before"
        );
    }

    #[test]
    fn rollback_preserves_new_files_in_created_directories_and_requires_recovery() {
        let fixture = linked_fixture();
        let context = job_context();
        let changes = vec![
            (
                "created/main.gd".into(),
                None,
                Some(b"candidate source".to_vec()),
            ),
            (
                "second.gd".into(),
                Some(b"second before".to_vec()),
                Some(b"second after".to_vec()),
            ),
        ];
        let result = context
            .scope(|| {
                publish_rows_with(&fixture.0.root, changes, None, Some(1), |boundary| {
                    if boundary == Some(1) {
                        std::fs::write(fixture.0.root.join("created/newer.gd"), b"external source")
                            .unwrap();
                        context.request_cancel().unwrap();
                    }
                })
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["source_rolled_back"], false);
        assert_eq!(result["recovery_required"], true);
        assert!(context.recovery_required());
        assert!(!context.commit_status_verified());
        assert!(!fixture.0.root.join("created/main.gd").exists());
        assert_eq!(
            std::fs::read(fixture.0.root.join("created/newer.gd")).unwrap(),
            b"external source"
        );
        assert!(active(&fixture.0.root).unwrap().exists());
    }

    #[test]
    fn revision_forget_is_cancellable_before_deletion_and_commits_actual_deletion() {
        let fixture = linked_fixture();
        let publication = publish_rows(&fixture.0.root, linked_changes(), None, None).unwrap();
        let revision = publication["revision_id"].as_str().unwrap();
        let before = crate::snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let context = job_context();
        let result = context
            .scope(|| {
                forget_with(
                    &fixture.0,
                    revision,
                    || {
                        context.request_cancel().unwrap();
                    },
                    |_| panic!("cancellation must not reach revision deletion"),
                )
            })
            .unwrap();
        assert!(result.is_err());
        assert!(!context.committed());
        assert!(context.commit_status_verified());
        assert!(state_root(&fixture.0.root).unwrap().join(revision).exists());
        let deletion_context = job_context();
        deletion_context
            .scope(|| forget(&fixture.0, revision))
            .unwrap()
            .unwrap();
        assert!(deletion_context.committed());
        assert!(!state_root(&fixture.0.root).unwrap().join(revision).exists());
        assert_eq!(
            crate::snapshot::fingerprint_all(&fixture.0.root).unwrap(),
            before
        );
    }

    #[test]
    fn partial_revision_forget_reports_unconfirmed_metadata_effects() {
        let fixture = linked_fixture();
        let publication = publish_rows(&fixture.0.root, linked_changes(), None, None).unwrap();
        let revision = publication["revision_id"].as_str().unwrap();
        let before = crate::snapshot::fingerprint_all(&fixture.0.root).unwrap();
        let context = job_context();
        let result = context
            .scope(|| {
                forget_with(
                    &fixture.0,
                    revision,
                    || {},
                    |directory| {
                        std::fs::remove_file(directory.join("000.before"))?;
                        Err(std::io::Error::other("injected partial metadata deletion"))
                    },
                )
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["deletion_complete"], false);
        assert_eq!(result["recovery_required"], true);
        assert!(!context.committed());
        assert!(!context.commit_status_verified());
        assert!(context.recovery_required());
        assert_eq!(
            crate::snapshot::fingerprint_all(&fixture.0.root).unwrap(),
            before
        );
    }
    #[test]
    fn compiled_artifact_is_verified_but_not_published_or_undone() {
        let original = Fixture::new();
        let copied = Fixture::new();
        let library = format!("native.{}", crate::build::dynamic_library_extension());
        std::fs::write(original.0.root.join("game.rs"), "old source").unwrap();
        std::fs::write(original.0.root.join(&library), "working library").unwrap();
        let source_sha = crate::snapshot::fingerprint_all(&original.0.root).unwrap();
        let input = json!({"changes":[{"path":"game.rs","action":"replace","expected_sha256":crate::sha256_hex(b"old source"),"text":"new source"}]});
        let checked = crate::revisions::check(&original.0.root, &input).unwrap();
        crate::snapshot::copy(&original.0.root, &copied.0.root).unwrap();
        std::fs::write(copied.0.root.join("game.rs"), "new source").unwrap();
        std::fs::write(copied.0.root.join(&library), "compiled library").unwrap();
        let artifacts = [GeneratedArtifact {
            path: library.clone(),
            sha256: crate::sha256_hex(b"compiled library"),
        }];
        let candidate_sha = crate::snapshot::fingerprint_all(&copied.0.root).unwrap();
        let bad = [GeneratedArtifact {
            path: library.clone(),
            sha256: "0".repeat(64),
        }];
        assert!(publish_candidate_with_artifacts(
            &original.0,
            &copied.0,
            &source_sha,
            &candidate_sha,
            &checked,
            &bad,
            &crate::private_import_artifacts::PrivateArtifacts::default(),
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(original.0.root.join("game.rs")).unwrap(),
            "old source"
        );
        let result = publish_candidate_with_artifacts(
            &original.0,
            &copied.0,
            &source_sha,
            &candidate_sha,
            &checked,
            &artifacts,
            &crate::private_import_artifacts::PrivateArtifacts::default(),
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read_to_string(original.0.root.join("game.rs")).unwrap(),
            "new source"
        );
        assert_eq!(
            std::fs::read_to_string(original.0.root.join(&library)).unwrap(),
            "working library"
        );
        undo(&original.0, result["revision_id"].as_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(original.0.root.join("game.rs")).unwrap(),
            "old source"
        );
        assert_eq!(
            std::fs::read_to_string(original.0.root.join(&library)).unwrap(),
            "working library"
        );
    }

    #[test]
    fn native_artifact_exemptions_are_confined_and_do_not_hide_other_changes() {
        let original = Fixture::new();
        let copied = Fixture::new();
        let library = format!("native.{}", crate::build::dynamic_library_extension());
        std::fs::write(original.0.root.join("game.rs"), "old source").unwrap();
        std::fs::write(original.0.root.join("keep.gd"), "unchanged source").unwrap();
        let source_sha = crate::snapshot::fingerprint_all(&original.0.root).unwrap();
        let checked = crate::revisions::check(
            &original.0.root,
            &json!({"changes":[{"path":"game.rs","action":"replace",
                "expected_sha256":crate::sha256_hex(b"old source"),"text":"new source"}]}),
        )
        .unwrap();
        crate::snapshot::copy(&original.0.root, &copied.0.root).unwrap();
        std::fs::write(copied.0.root.join("game.rs"), "new source").unwrap();
        std::fs::write(copied.0.root.join("keep.gd"), "unexpected source edit").unwrap();
        std::fs::write(copied.0.root.join(&library), "compiled library").unwrap();
        let sha256 = crate::sha256_hex(b"compiled library");
        for path in [
            format!("res://{library}"),
            format!("../{library}"),
            format!("target/{library}"),
            format!(".private/{library}"),
            format!("./{library}"),
            format!("folder/../{library}"),
            format!("folder\\{library}"),
        ] {
            assert!(GeneratedArtifact {
                path,
                sha256: sha256.clone(),
            }
            .verified_path(&copied.0.root)
            .is_err());
        }
        let artifacts = [GeneratedArtifact {
            path: library.clone(),
            sha256,
        }];
        let candidate_sha = crate::snapshot::fingerprint_all(&copied.0.root).unwrap();
        assert!(publish_candidate_with_artifacts(
            &original.0,
            &copied.0,
            &source_sha,
            &candidate_sha,
            &checked,
            &artifacts,
            &crate::private_import_artifacts::PrivateArtifacts::default(),
        )
        .is_err());
        assert_eq!(
            crate::snapshot::fingerprint_all(&original.0.root).unwrap(),
            source_sha
        );
        assert!(!original.0.root.join(&library).exists());
        assert!(!original.0.root.join(".aurum/revisions").exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_library_exemption_cannot_alias_a_requested_path_by_case() {
        let original = Fixture::new();
        let copied = Fixture::new();
        std::fs::write(original.0.root.join("native.dll"), "old library").unwrap();
        let source_sha = crate::snapshot::fingerprint_all(&original.0.root).unwrap();
        let checked = crate::revisions::check(
            &original.0.root,
            &json!({"changes":[{"path":"native.dll","action":"replace",
                "expected_sha256":crate::sha256_hex(b"old library"),"text":"new library"}]}),
        )
        .unwrap();
        crate::snapshot::copy(&original.0.root, &copied.0.root).unwrap();
        std::fs::write(copied.0.root.join("native.dll"), "new library").unwrap();
        let candidate_sha = crate::snapshot::fingerprint_all(&copied.0.root).unwrap();
        let artifacts = [GeneratedArtifact {
            path: "NATIVE.DLL".into(),
            sha256: crate::sha256_hex(b"new library"),
        }];
        assert!(publish_candidate_with_artifacts(
            &original.0,
            &copied.0,
            &source_sha,
            &candidate_sha,
            &checked,
            &artifacts,
            &crate::private_import_artifacts::PrivateArtifacts::default(),
        )
        .is_err());
        assert_eq!(
            std::fs::read(original.0.root.join("native.dll")).unwrap(),
            b"old library"
        );
    }

    #[test]
    fn empty_history_is_read_only_and_does_not_require_valid_configuration() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("aurum.toml"), "invalid = [").unwrap();
        let result =
            crate::project_ops::execute(&fixture.0.root, &json!({"op":"changes_history"}), true)
                .unwrap();
        assert_eq!(result["retained_revisions"], 0);
        assert!(!fixture.0.root.join(".aurum").exists());
    }
    #[test]
    fn history_discovers_undo_ids_and_returns_bounded_hashes_without_source() {
        let fixture = Fixture::new();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "first.gd".into(),
                    None,
                    Some(b"history-source-sentinel".to_vec()),
                ),
                ("second.gd".into(), None, Some(b"second".to_vec())),
            ],
            None,
            None,
        )
        .unwrap();
        let revision = result["revision_id"].as_str().unwrap();
        let catalog = history(&fixture.0.root, &json!({})).unwrap();
        assert_eq!(catalog["revisions"][0]["revision_id"], revision);
        assert!(catalog["revisions"][0].get("files").is_none());
        let detail = history(&fixture.0.root, &json!({"revision_id":revision,"limit":1})).unwrap();
        assert_eq!(detail["revisions"][0]["files"].as_array().unwrap().len(), 1);
        assert_eq!(detail["revisions"][0]["files_truncated"], true);
        assert!(!serde_json::to_string(&detail)
            .unwrap()
            .contains("history-source-sentinel"));
        let reverse = undo(&fixture.0, revision).unwrap();
        let catalog = history(&fixture.0.root, &json!({})).unwrap();
        assert_eq!(catalog["retained_revisions"], 2);
        assert!(catalog["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["revision_id"] == reverse["revision_id"] && row["undo_of"] == revision));
        forget(&fixture.0, revision).unwrap();
        assert_eq!(
            history(&fixture.0.root, &json!({})).unwrap()["retained_revisions"],
            1
        );
    }
    #[test]
    fn interrupted_history_stays_inspectable_without_claiming_live_undo_readiness() {
        let fixture = crash_fixture();
        let catalog = history(&fixture.0.root, &json!({})).unwrap();
        assert!(catalog["active_revision"].is_string());
        assert_eq!(catalog["revisions"][0]["active"], true);
        assert_eq!(catalog["revisions"][0]["completed"], false);
        assert_eq!(catalog["source_state_checked"], false);
        assert_eq!(catalog["undo_preconditions_checked"], false);
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"after"
        );
        assert!(history(&fixture.0.root, &json!({"limit":201})).is_err());
        assert!(history(&fixture.0.root, &json!({"revision_id":"../escape"})).is_err());
    }

    #[test]
    fn crash_child() {
        if let Some(root) = std::env::var_os("AURUM_TRANSACTION_CRASH_ROOT") {
            let root = PathBuf::from(root);
            let _lease = crate::content_lock::ContentLock::exclusive(&root).unwrap();
            publish_rows(
                &root,
                vec![
                    (
                        "first.gd".into(),
                        Some(b"before".to_vec()),
                        Some(b"after".to_vec()),
                    ),
                    (
                        "second.gd".into(),
                        Some(b"before".to_vec()),
                        Some(b"after".to_vec()),
                    ),
                ],
                None,
                None,
            )
            .unwrap();
        }
    }

    fn crash_fixture() -> Fixture {
        let fixture = Fixture::new();
        for name in ["first.gd", "second.gd"] {
            std::fs::write(fixture.0.root.join(name), "before").unwrap();
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let status = command
            .args(["--exact", "transactions::tests::crash_child", "--nocapture"])
            .env("AURUM_TRANSACTION_CRASH_ROOT", &fixture.0.root)
            .env("AURUM_TRANSACTION_CRASH_AFTER", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(91));
        fixture
    }

    #[test]
    fn real_process_exit_recovers_the_partially_published_revision() {
        let fixture = crash_fixture();
        assert!(ensure_writable(&fixture.0.root).is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"after"
        );
        let result = recover(&fixture.0).unwrap();
        assert_eq!(result["rolled_back"], true);
        for name in ["first.gd", "second.gd"] {
            assert_eq!(std::fs::read(fixture.0.root.join(name)).unwrap(), b"before");
        }
        assert!(ensure_writable(&fixture.0.root).is_ok());
    }

    #[test]
    fn recovery_conflict_preserves_external_content_and_blocks_new_writes() {
        let fixture = crash_fixture();
        std::fs::write(fixture.0.root.join("first.gd"), "external").unwrap();
        let result = recover(&fixture.0).unwrap();
        assert_eq!(result["recovery_blocked"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"external"
        );
        assert!(ensure_writable(&fixture.0.root).is_err());
    }

    #[test]
    fn corrupt_recovery_backups_are_not_silently_accepted() {
        let fixture = crash_fixture();
        let pointer: Value =
            serde_json::from_slice(&std::fs::read(active(&fixture.0.root).unwrap()).unwrap())
                .unwrap();
        let directory = state_root(&fixture.0.root)
            .unwrap()
            .join(pointer["revision_id"].as_str().unwrap());
        std::fs::write(directory.join("000.before"), "corrupted").unwrap();
        assert!(recover(&fixture.0).is_err());
        assert!(ensure_writable(&fixture.0.root).is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"after"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_failure_rolls_back_already_replaced_files() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        for name in ["first.gd", "second.gd"] {
            std::fs::write(fixture.0.root.join(name), "before").unwrap();
        }
        let _reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(fixture.0.root.join("second.gd"))
            .unwrap();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "first.gd".into(),
                    Some(b"before".to_vec()),
                    Some(b"after".to_vec()),
                ),
                (
                    "second.gd".into(),
                    Some(b"before".to_vec()),
                    Some(b"after".to_vec()),
                ),
            ],
            None,
            None,
        )
        .unwrap();
        assert_eq!(result["recovery"]["rolled_back"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("first.gd")).unwrap(),
            b"before"
        );
    }
    #[test]
    fn publication_and_whole_revision_undo_are_guarded() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("old.gd"), "old").unwrap();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "old.gd".into(),
                    Some(b"old".to_vec()),
                    Some(b"new".to_vec()),
                ),
                ("new.gd".into(), None, Some(b"created".to_vec())),
            ],
            None,
            None,
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("old.gd")).unwrap(),
            b"new"
        );
        let reversed = undo(&fixture.0, result["revision_id"].as_str().unwrap()).unwrap();
        assert_eq!(reversed["ok"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("old.gd")).unwrap(),
            b"old"
        );
        assert!(!fixture.0.root.join("new.gd").exists());
    }
    #[test]
    fn failure_after_first_file_restores_all_originals() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("old.gd"), "old").unwrap();
        let result = publish_rows(
            &fixture.0.root,
            vec![
                (
                    "old.gd".into(),
                    Some(b"old".to_vec()),
                    Some(b"new".to_vec()),
                ),
                ("new.gd".into(), None, Some(b"created".to_vec())),
            ],
            None,
            Some(1),
        )
        .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["recovery"]["rolled_back"], true);
        assert_eq!(
            std::fs::read(fixture.0.root.join("old.gd")).unwrap(),
            b"old"
        );
        assert!(!active(&fixture.0.root).unwrap().exists());
    }
    #[test]
    fn undo_never_overwrites_a_newer_edit() {
        let fixture = Fixture::new();
        let result = publish_rows(
            &fixture.0.root,
            vec![("main.gd".into(), None, Some(b"first".to_vec()))],
            None,
            None,
        )
        .unwrap();
        std::fs::write(fixture.0.root.join("main.gd"), "external").unwrap();
        assert!(undo(&fixture.0, result["revision_id"].as_str().unwrap()).is_err());
        assert_eq!(
            std::fs::read(fixture.0.root.join("main.gd")).unwrap(),
            b"external"
        );
    }
}
