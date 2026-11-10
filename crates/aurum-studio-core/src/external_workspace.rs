//! Private external staging and owner-bound single-root source transactions.
//! Public intake remains read-only until recovery and undo acceptance completes.
#[cfg(windows)]
#[path = "external_namespace_io.rs"]
mod namespace_io;
use crate::content_lock::ContentLock;
use crate::hash::Sha256;
use crate::toml::Document;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const MAX_ROOTS: usize = 8;
const MAX_MANIFESTS: usize = 32;
const MAX_SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 40_000;
const MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RootId(String);
impl RootId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn new(value: &str) -> Result<Self, String> {
        if value.is_empty()
            || value.len() > 32
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
        {
            return Err("invalid_root_id".into());
        }
        Ok(Self(value.into()))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision(String);
impl Revision {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn new(value: &str) -> Result<Self, String> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_grant_revision".into());
        }
        Ok(Self(value.into()))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    DependencyRead,
    SourcePublish,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) struct DirectoryIdentity {
    volume: u64,
    id: [u8; 16],
    created: Option<SystemTime>,
}
impl DirectoryIdentity {
    pub(crate) fn same_volume(&self, other: &Self) -> bool {
        self.volume == other.volume
    }
}
/// Recorded identity facts are evidence, never a filesystem capability.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalObject {
    pub(crate) identity: DirectoryIdentity,
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
}
pub(crate) struct ExternalReadAccess<'a, 'b> {
    inner: namespace_io::StateInspector<'a, 'b>,
}
impl<'a, 'b> ExternalReadAccess<'a, 'b> {
    fn new(grant: &'a PreparedGrant, budget: &'a Budget<'b>) -> Result<Self, String> {
        Ok(Self {
            inner: namespace_io::StateInspector::prepared(grant, budget)?,
        })
    }
}
impl crate::transactions::ExternalReadIo for ExternalReadAccess<'_, '_> {
    fn active(&self) -> Result<Option<(ExternalObject, Vec<u8>)>, String> {
        self.inner.active()
    }
    fn read(
        &self,
        revision: &str,
        name: &str,
        maximum: u64,
    ) -> Result<Option<(ExternalObject, Vec<u8>)>, String> {
        self.inner.read(revision, name, maximum)
    }
    fn entries(&self, revision: &str) -> Result<Vec<(String, bool, u64)>, String> {
        Ok(self
            .inner
            .entries(revision)?
            .into_iter()
            .map(|e| (e.name, e.directory, e.bytes))
            .collect())
    }
    fn directory(&self, revision: &str) -> Result<DirectoryIdentity, String> {
        self.inner.directory_identity(revision)
    }
    fn target(&self, path: &str) -> Result<Option<(ExternalObject, Vec<u8>)>, String> {
        self.inner.target(path)
    }
    fn source_directory(&self, path: &str) -> Result<Option<DirectoryIdentity>, String> {
        self.inner.source_directory(path)
    }
    fn saved_directory(
        &self,
        revision: &str,
        name: &str,
    ) -> Result<Option<DirectoryIdentity>, String> {
        self.inner.saved_directory(revision, name)
    }
    fn source_entries(&self, path: &str) -> Result<Option<Vec<(String, bool, u64)>>, String> {
        Ok(self.inner.source_entries(path)?.map(|entries| {
            entries
                .into_iter()
                .map(|e| (e.name, e.directory, e.bytes))
                .collect()
        }))
    }
}
/// A single admitted authority, original budget and already acquired leases.
/// Selectors below name only fixed managed state or approved source rows.
pub(crate) struct ExternalRevisionIo<'a, 'b> {
    inner: namespace_io::WriteNamespace<'a, 'b>,
}
impl<'a, 'b> ExternalRevisionIo<'a, 'b> {
    pub(crate) fn publication(
        admission: &'a SourceAdmission,
        budget: &'a Budget<'b>,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: namespace_io::WriteNamespace::publication(admission, budget)?,
        })
    }
    pub(crate) fn repair(
        admission: &'a SourceRepairAdmission,
        budget: &'a Budget<'b>,
    ) -> Result<Self, String> {
        Ok(Self {
            inner: namespace_io::WriteNamespace::repair(admission, budget)?,
        })
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.inner.check()
    }
    pub(crate) fn prepublication(&self) -> Result<(), String> {
        self.inner.prepublication()
    }
    pub(crate) fn begin(
        &self,
        revision: &str,
        additional: u64,
    ) -> Result<DirectoryIdentity, String> {
        if self.inner.active()?.is_some() {
            return Err("external_source_v3_active_revision_present".into());
        }
        let proof = self.inner.preflight(additional)?;
        self.inner.begin_state(revision, proof)
    }
    pub(crate) fn open(&self, revision: &str, identity: &DirectoryIdentity) -> Result<(), String> {
        self.inner.open_state(revision, identity)
    }
    pub(crate) fn read(
        &self,
        name: &str,
        maximum: u64,
    ) -> Result<Option<(ExternalObject, Vec<u8>)>, String> {
        self.inner.state_read(name, maximum)
    }
    pub(crate) fn create(
        &self,
        name: &str,
        bytes: &[u8],
        maximum: u64,
    ) -> Result<ExternalObject, String> {
        self.inner.create_record(name, bytes, maximum)
    }
    pub(crate) fn pin(&self, name: &str, expected: &ExternalObject) -> Result<(), String> {
        self.inner.pin_record(name, expected)
    }
    pub(crate) fn entries(&self) -> Result<Vec<(String, bool, u64)>, String> {
        Ok(self
            .inner
            .state_entries()?
            .into_iter()
            .map(|e| (e.name, e.directory, e.bytes))
            .collect())
    }
    pub(crate) fn target(&self, path: &str) -> Result<Option<(ExternalObject, Vec<u8>)>, String> {
        self.inner.observe_target(path)
    }
    pub(crate) fn move_old(
        &self,
        path: &str,
        expected: &ExternalObject,
        name: &str,
    ) -> Result<ExternalObject, String> {
        let reserved = self.inner.reserve(name)?;
        self.inner.move_target(path, expected, reserved)
    }
    pub(crate) fn publish(
        &self,
        path: &str,
        name: &str,
        expected: &ExternalObject,
    ) -> Result<ExternalObject, String> {
        self.inner.publish_slot(path, name, expected)
    }
    pub(crate) fn delete_target(
        &self,
        path: &str,
        expected: &ExternalObject,
    ) -> Result<(), String> {
        self.inner.delete_target(path, expected)
    }
    // No caller or test reaches this yet; it belongs to the unconnected publication surface.
    #[allow(dead_code)]
    pub(crate) fn delete_slot(&self, name: &str, expected: &ExternalObject) -> Result<(), String> {
        self.inner.delete_slot(name, expected)
    }
    pub(crate) fn active(&self) -> Result<Option<(ExternalObject, Vec<u8>)>, String> {
        self.inner.active()
    }
    pub(crate) fn activate(&self, bytes: &[u8]) -> Result<ExternalObject, String> {
        self.inner.create_active(bytes)
    }
    pub(crate) fn clear_active(&self, expected: &ExternalObject) -> Result<(), String> {
        self.inner.clear_active(expected)
    }
    pub(crate) fn source_directory(&self, path: &str) -> Result<Option<DirectoryIdentity>, String> {
        self.inner.source_directory_identity(path)
    }
    pub(crate) fn create_directory(&self, path: &str) -> Result<DirectoryIdentity, String> {
        self.inner.create_source_directory(path)
    }
    pub(crate) fn remove_directory(
        &self,
        path: &str,
        expected: &DirectoryIdentity,
    ) -> Result<(), String> {
        self.inner.remove_source_directory(path, expected)
    }
    pub(crate) fn saved_directory(&self, name: &str) -> Result<Option<DirectoryIdentity>, String> {
        self.inner.saved_directory(name)
    }
    pub(crate) fn origin_directory(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<Option<DirectoryIdentity>, String> {
        self.inner.origin_directory(origin)
    }
    pub(crate) fn retire_directory(
        &self,
        path: &str,
        identity: &DirectoryIdentity,
        name: &str,
    ) -> Result<(), String> {
        self.inner.retire_directory(path, identity, name)
    }
    pub(crate) fn unretire_directory(
        &self,
        path: &str,
        identity: &DirectoryIdentity,
        name: &str,
    ) -> Result<(), String> {
        self.inner.unretire_directory(path, identity, name)
    }
    pub(crate) fn return_directory(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<(), String> {
        self.inner.return_directory(origin)
    }
    pub(crate) fn unreturn_directory(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<(), String> {
        self.inner.unreturn_directory(origin)
    }
}

fn file_identity(file: &File) -> Result<DirectoryIdentity, String> {
    let metadata = file
        .metadata()
        .map_err(|_| "identity_metadata_unavailable")?;
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct FileIdInfo {
            volume: u64,
            id: [u8; 16],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandleEx(
                handle: *mut std::ffi::c_void,
                class: u32,
                data: *mut std::ffi::c_void,
                bytes: u32,
            ) -> i32;
        }
        let mut info = FileIdInfo {
            volume: 0,
            id: [0; 16],
        };
        // FileIdInfo is class0x12. File owns the handle; the ABI buffer is exactly24 bytes.
        // https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                0x12,
                (&mut info as *mut FileIdInfo).cast(),
                std::mem::size_of::<FileIdInfo>() as u32,
            )
        } == 0
        {
            return Err("directory_identity_unavailable".into());
        }
        Ok(DirectoryIdentity {
            volume: info.volume,
            id: info.id,
            created: metadata.created().ok(),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mut id = [0; 16];
        id[..8].copy_from_slice(&metadata.ino().to_le_bytes());
        Ok(DirectoryIdentity {
            volume: metadata.dev(),
            id,
            created: metadata.created().ok(),
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = metadata;
        Err("directory_identity_unsupported".into())
    }
}
pub(crate) fn canonical_directory(
    path: &Path,
    budget: &Budget<'_>,
) -> Result<(PathBuf, DirectoryIdentity), String> {
    budget.check()?;
    let file = crate::snapshot::open_checked_directory(path, budget.deadline)?;
    let canonical =
        crate::project::clean_path(path.canonicalize().map_err(|_| "root_unavailable")?);
    crate::snapshot::check_opened_path(&file, &canonical, budget.deadline)?;
    Ok((canonical, file_identity(&file)?))
}
#[derive(Clone, Copy)]
enum CompilerDirectoryRole {
    Immutable,
    WritableOutput,
}

pub(crate) fn compiler_output_directory_paths(root: &Path) -> [PathBuf; 8] {
    [
        "target",
        "build",
        "profile",
        "cargo-home",
        "temp",
        "profile/AppData",
        "profile/AppData/Roaming",
        "profile/AppData/Local",
    ]
    .map(|relative| root.join(relative))
}

fn checked_compiler_output_directory(
    expected: &Path,
    identity: &DirectoryIdentity,
    held: &File,
    reported: &str,
    budget: &Budget<'_>,
    role: CompilerDirectoryRole,
) -> Result<(), String> {
    budget.check()?;
    #[cfg(windows)]
    let reported = PathBuf::from(reported.replace('/', "\\"));
    #[cfg(not(windows))]
    let reported = PathBuf::from(reported);
    if !reported.is_absolute()
        || reported
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("external_compile_output_directory_path_refused".into());
    }
    crate::snapshot::check_opened_path(held, expected, budget.deadline)
        .map_err(|_| "external_compile_output_directory_guard_changed")?;
    if &file_identity(held)? != identity {
        return Err("external_compile_output_directory_guard_changed".into());
    }
    // Checked opening rejects remote roots and reparse entries in every ancestor.
    let opened = match role {
        CompilerDirectoryRole::Immutable => {
            crate::snapshot::open_checked_directory(&reported, budget.deadline)
        }
        CompilerDirectoryRole::WritableOutput => {
            crate::snapshot::open_checked_writable_directory_identity(&reported, budget.deadline)
        }
    }
    .map_err(|_| "external_compile_output_directory_path_refused")?;
    let actual = crate::project::clean_path(
        reported
            .canonicalize()
            .map_err(|_| "external_compile_output_directory_path_refused")?,
    );
    let expected_canonical = crate::project::clean_path(
        expected
            .canonicalize()
            .map_err(|_| "external_compile_output_directory_guard_changed")?,
    );
    crate::snapshot::check_opened_path(&opened, &actual, budget.deadline)
        .map_err(|_| "external_compile_output_directory_path_refused")?;
    crate::snapshot::check_opened_path(held, &expected_canonical, budget.deadline)
        .map_err(|_| "external_compile_output_directory_guard_changed")?;
    if !crate::ownership::paths_equal(&actual, &expected_canonical)
        || &file_identity(&opened)? != identity
    {
        return Err("external_compile_output_directory_identity_refused".into());
    }
    budget.check()
}
fn key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}
fn overlaps(a: &Path, b: &Path) -> bool {
    let a = key(a);
    let b = key(b);
    a == b || a.starts_with(&(b.clone() + "/")) || b.starts_with(&(a + "/"))
}
#[derive(Clone, Debug)]
pub(crate) struct ImportedProject {
    root: PathBuf,
    identity: DirectoryIdentity,
    revision: Revision,
}
impl ImportedProject {
    /// Host import permission is a separate prerequisite. This binds its already selected root.
    pub(crate) fn bind(
        root: &Path,
        revision: Revision,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        let (root, identity) = canonical_directory(root, budget)?;
        Ok(Self {
            root,
            identity,
            revision,
        })
    }
}
#[cfg(test)]
pub(crate) struct ApprovalRequest<'a> {
    pub(crate) imported_project: &'a Path,
    pub(crate) requested_root: &'a Path,
    pub(crate) mode: Mode,
    pub(crate) revision: &'a Revision,
}
/// Implemented only by an authenticated owner-intake host. No transport deserializer exists.
/// Unit tests use an explicitly labelled test authority; they do not prove human intake.
#[cfg(test)]
pub(crate) trait OwnerAuthority {
    fn approve(&self, request: &ApprovalRequest<'_>) -> Result<bool, String>;
}
#[derive(Clone)]
pub(crate) struct VerifiedGrant {
    id: RootId,
    root: PathBuf,
    identity: DirectoryIdentity,
    mode: Mode,
    imported: PathBuf,
    imported_identity: DirectoryIdentity,
    revision: Revision,
}
impl std::fmt::Debug for VerifiedGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VerifiedGrant { authority: redacted }")
    }
}
/// Identity is prepared and retained before the attended confirmation.
/// No transport can construct the proof accepted by finish().
pub(crate) struct PreparedGrant {
    project: ImportedProject,
    id: RootId,
    root: PathBuf,
    identity: DirectoryIdentity,
    project_handle: File,
    root_handle: File,
    mode: Mode,
}
impl PreparedGrant {
    pub(crate) fn dependency_read(
        project: &ImportedProject,
        id: RootId,
        requested: &Path,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        Self::prepare(project, id, requested, Mode::DependencyRead, budget)
    }
    fn prepare(
        project: &ImportedProject,
        id: RootId,
        requested: &Path,
        mode: Mode,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        budget.check()?;
        let project_handle =
            crate::snapshot::open_checked_directory(&project.root, budget.deadline)?;
        crate::snapshot::check_opened_path(&project_handle, &project.root, budget.deadline)?;
        if file_identity(&project_handle)? != project.identity {
            return Err("imported_project_identity_changed".into());
        }
        let root_handle = crate::snapshot::open_checked_directory(requested, budget.deadline)?;
        let root =
            crate::project::clean_path(requested.canonicalize().map_err(|_| "root_unavailable")?);
        crate::snapshot::check_opened_path(&root_handle, &root, budget.deadline)?;
        let identity = file_identity(&root_handle)?;
        if identity.created.is_none() || project.identity.created.is_none() {
            return Err("prepared_directory_creation_identity_unavailable".into());
        }
        if identity.volume != project.identity.volume {
            return Err("cross_volume_staging_unsupported".into());
        }
        if overlaps(&root, &project.root) || identity == project.identity {
            return Err("grant_overlaps_imported_project".into());
        }
        Ok(Self {
            project: project.clone(),
            id,
            root,
            identity,
            project_handle,
            root_handle,
            mode,
        })
    }
    pub(crate) fn canonical_root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn digest(&self) -> String {
        // The digest binds full OS identities internally; IDs are never main-flow copy.
        crate::sha256_hex(
            format!(
                "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
                self.project.root,
                self.project.identity,
                self.project.revision,
                self.id,
                self.root,
                self.identity,
                self.mode,
                "owner-dependency-read/1"
            )
            .as_bytes(),
        )
    }
    fn recheck(&self, revision: &Revision, budget: &Budget<'_>) -> Result<(), String> {
        budget.check()?;
        if &self.project.revision != revision {
            return Err("grant_revision_mismatch".into());
        }
        crate::snapshot::check_opened_path(
            &self.project_handle,
            &self.project.root,
            budget.deadline,
        )?;
        crate::snapshot::check_opened_path(&self.root_handle, &self.root, budget.deadline)?;
        if file_identity(&self.project_handle)? != self.project.identity
            || file_identity(&self.root_handle)? != self.identity
        {
            return Err("prepared_owner_identity_changed".into());
        }
        let (root, identity) = canonical_directory(&self.root, budget)?;
        let (project, project_identity) = canonical_directory(&self.project.root, budget)?;
        if root != self.root
            || identity != self.identity
            || project != self.project.root
            || project_identity != self.project.identity
        {
            return Err("prepared_owner_identity_changed".into());
        }
        Ok(())
    }
    pub(crate) fn finish(
        self,
        confirmation: &crate::workspace_access::OwnerConfirmation,
        revision: &Revision,
        budget: &Budget<'_>,
    ) -> Result<VerifiedGrant, String> {
        if self.mode != Mode::DependencyRead {
            return Err("external_source_publish_unimplemented".into());
        }
        if !confirmation.matches(&self.digest()) {
            return Err("owner_confirmation_binding_mismatch".into());
        }
        self.recheck(revision, budget)?;
        Ok(self.into_grant())
    }
    fn into_grant(self) -> VerifiedGrant {
        VerifiedGrant {
            id: self.id,
            root: self.root,
            identity: self.identity,
            mode: self.mode,
            imported: self.project.root,
            imported_identity: self.project.identity,
            revision: self.project.revision,
        }
    }
}
impl VerifiedGrant {
    pub(crate) fn public_metadata(&self) -> serde_json::Value {
        serde_json::json!({"root_id":self.id.as_str(),"canonical_path":self.root,"mode":"dependency_read",
            "revision":self.revision.as_str(),"session_only":true,"source_publish":false})
    }
    pub(crate) fn is_dependency_read(&self) -> bool {
        self.mode == Mode::DependencyRead
    }
}

fn identity_digest(identity: &DirectoryIdentity) -> String {
    crate::sha256_hex(format!("{identity:?}").as_bytes())
}
type SourceBytes = (String, Option<Vec<u8>>, Option<Vec<u8>>);

struct SourceSession {
    broker: std::sync::Weak<crate::workspace_access::Broker>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    expires: Instant,
    publication_operation: Option<PublicationOperationScope>,
    source_limits: Option<ProposalLimits>,
}
impl SourceSession {
    fn check(&self, budget: &Budget<'_>) -> Result<(), String> {
        budget.check()?;
        if let Some(limits) = &self.source_limits {
            limits.check_not_wider(budget)?;
            if budget.deadline > self.expires {
                return Err("external_source_confirmation_window_renewal_or_expiry".into());
            }
        }
        self.check_authority()
    }
    // This contains no Budget callback, so a composed Budget cannot recurse.
    fn check_authority(&self) -> Result<(), String> {
        if let Some(operation) = &self.publication_operation {
            operation.check()?;
        }
        if Instant::now() >= self.expires
            || self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            || !self
                .broker
                .upgrade()
                .is_some_and(|broker| broker.managed_output_is_open())
        {
            return Err("external_source_authority_cancelled_or_closed".into());
        }
        Ok(())
    }
}
#[derive(Clone)]
struct PublicationOperationScope {
    original: Option<crate::operation_context::OperationContext>,
}
impl PublicationOperationScope {
    fn capture() -> Self {
        Self {
            original: crate::operation_context::OperationContext::current(),
        }
    }
    fn check(&self) -> Result<(), String> {
        if let Some(context) = &self.original {
            context
                .check()
                .map_err(|_| "external_source_publication_operation_cancelled_or_expired")?;
        }
        let current = crate::operation_context::OperationContext::current();
        if let Some(context) = &current {
            context
                .check()
                .map_err(|_| "external_source_publication_operation_cancelled_or_expired")?;
        }
        let replaced = match (&self.original, &current) {
            (Some(original), Some(current)) => !original.same_operation(current),
            (None, Some(_)) => true,
            _ => false,
        };
        if replaced {
            return Err("external_source_publication_operation_binding_mismatch".into());
        }
        Ok(())
    }
}
struct NativeConfirmationWindow {
    expires: Instant,
    broker: std::sync::Weak<crate::workspace_access::Broker>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    operation: PublicationOperationScope,
}
impl NativeConfirmationWindow {
    fn new(
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        if budget.deadline > Instant::now() + Duration::from_secs(60) {
            return Err("external_source_confirmation_window_short_bound".into());
        }
        let window = Self {
            expires: budget.deadline,
            broker: std::sync::Arc::downgrade(broker),
            cancelled,
            operation: PublicationOperationScope::capture(),
        };
        window.check(budget)?;
        Ok(window)
    }
    fn matches_owner(
        &self,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        self.broker.ptr_eq(&std::sync::Arc::downgrade(broker))
            && std::sync::Arc::ptr_eq(&self.cancelled, cancelled)
    }
    fn check(&self, budget: &Budget<'_>) -> Result<(), String> {
        budget.check()?;
        self.check_authority(budget.deadline)
    }
    // Check only fixed authority here; the caller's original Budget is separate.
    fn check_authority(&self, deadline: Instant) -> Result<(), String> {
        self.operation.check()?;
        if deadline > self.expires || Instant::now() >= self.expires {
            return Err("external_source_confirmation_window_renewal_or_expiry".into());
        }
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            || !self
                .broker
                .upgrade()
                .is_some_and(|broker| broker.managed_output_is_open())
        {
            return Err("external_source_confirmation_owner_cancelled_or_closed".into());
        }
        Ok(())
    }
}
struct SourceGuards(
    std::sync::Mutex<Vec<(PathBuf, DirectoryIdentity, File)>>,
    BTreeSet<PathBuf>,
);
impl SourceGuards {
    fn capture(
        root: &Path,
        paths: impl Iterator<Item = String>,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        Self::capture_with_directory_operations(root, paths, std::iter::empty(), budget)
    }
    fn capture_with_directory_operations(
        root: &Path,
        paths: impl Iterator<Item = String>,
        directory_operations: impl Iterator<Item = String>,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        let mut directories = BTreeSet::new();
        let mut atomic_parents = BTreeSet::new();
        // A directory relocation creates a child entry in its exact parent.
        // The kernel's relative rename target open needs write sharing there.
        // This changes no requested access and still denies directory delete.
        for path in directory_operations {
            budget.check()?;
            relative_path_for_directory(&path)?;
            let parent = root
                .join(path)
                .parent()
                .ok_or("source_directory_operation_parent_required")?
                .to_path_buf();
            if !parent.starts_with(root) {
                return Err("source_directory_operation_parent_escape".into());
            }
            atomic_parents.insert(parent);
        }
        directories.insert(root.to_path_buf());
        for path in paths {
            let mut parent = root
                .join(path)
                .parent()
                .ok_or("source_parent_required")?
                .to_path_buf();
            // Publication and compensation may atomically replace a child in
            // this direct parent. Sharing permits that operation; the guard
            // itself still requests only read access and denies directory delete.
            atomic_parents.insert(parent.clone());
            loop {
                match std::fs::symlink_metadata(&parent) {
                    Ok(metadata) => {
                        ordinary(&metadata)?;
                        if !metadata.is_dir() {
                            return Err("source_parent_directory_required".into());
                        }
                        directories.insert(parent.clone());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err("source_parent_unavailable".into()),
                }
                if parent == root {
                    break;
                }
                if !parent.pop() || !parent.starts_with(root) {
                    return Err("source_parent_escape".into());
                }
            }
        }
        let mut guards = Vec::new();
        for path in directories {
            let guard = if atomic_parents.contains(&path) {
                crate::snapshot::open_checked_writable_directory_identity(&path, budget.deadline)?
            } else {
                crate::snapshot::open_checked_directory(&path, budget.deadline)?
            };
            guards.push((path, file_identity(&guard)?, guard));
        }
        Ok(Self(std::sync::Mutex::new(guards), atomic_parents))
    }
    fn check(&self, budget: &Budget<'_>) -> Result<(), String> {
        let held = self
            .0
            .lock()
            .map_err(|_| "external_source_guards_unavailable")?;
        Self::check_held(&held, budget)
    }
    fn check_held(
        held: &[(PathBuf, DirectoryIdentity, File)],
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        for (path, identity, guard) in held {
            budget.check()?;
            crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
            if &file_identity(guard)? != identity {
                return Err("external_source_parent_identity_changed".into());
            }
        }
        Ok(())
    }
    fn observations(
        &self,
        root: &Path,
        scope: &[String],
        budget: &Budget<'_>,
    ) -> Result<Vec<(String, Option<String>)>, String> {
        let held = self
            .0
            .lock()
            .map_err(|_| "external_source_guards_unavailable")?;
        Self::check_held(&held, budget)?;
        let mut observed = Vec::new();
        for relative in scope {
            let path = root.join(relative);
            let identity = if let Some((_, identity, _)) =
                held.iter().find(|(held, _, _)| *held == path)
            {
                Some(identity_digest(identity))
            } else {
                if std::fs::symlink_metadata(&path).is_ok() {
                    return Err("external_source_cleanup_directory_was_not_held".into());
                }
                if !matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                {
                    return Err("external_source_cleanup_directory_unavailable".into());
                }
                None
            };
            observed.push((relative.clone(), identity));
        }
        Ok(observed)
    }
}

/// Directory operations are available only through a live admitted source
/// permit, within the approved cleanup scope. Other ancestor guards stay held.
pub(crate) struct SourceDirectoryAuthority<'a> {
    root: &'a Path,
    scope: &'a [String],
    guards: &'a SourceGuards,
    budget: &'a Budget<'a>,
}
impl SourceDirectoryAuthority<'_> {
    fn path(&self, relative: &str) -> Result<PathBuf, String> {
        if !self.scope.iter().any(|approved| approved == relative) {
            return Err("external_source_cleanup_outside_approval".into());
        }
        relative_path_for_directory(relative)?;
        Ok(self.root.join(relative))
    }
}
fn relative_path_for_directory(path: &str) -> Result<(), String> {
    relative(path, false)?;
    if path
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with('.') || windows_device_name(part))
    {
        return Err("external_source_cleanup_path_refused".into());
    }
    Ok(())
}
impl crate::transactions::DirectoryAuthority for SourceDirectoryAuthority<'_> {
    fn check(&self) -> Result<(), String> {
        self.guards.check(self.budget)
    }
    fn verify(&self, relative: &str, expected: Option<&str>) -> Result<(), String> {
        let path = self.path(relative)?;
        let held = self
            .guards
            .0
            .lock()
            .map_err(|_| "external_source_guards_unavailable")?;
        SourceGuards::check_held(&held, self.budget)?;
        match held.iter().find(|(held, _, _)| *held == path) {
            Some((_, identity, _)) if expected == Some(identity_digest(identity).as_str()) => {
                Ok(())
            }
            Some(_) => Err("external_source_cleanup_ownership_mismatch".into()),
            None => match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err("external_source_unowned_cleanup_directory_present".into()),
            },
        }
    }
    fn create(&self, relative: &str) -> Result<String, String> {
        let path = self.path(relative)?;
        let mut held = self
            .guards
            .0
            .lock()
            .map_err(|_| "external_source_guards_unavailable")?;
        SourceGuards::check_held(&held, self.budget)?;
        if held.iter().any(|(held, _, _)| *held == path) {
            return Err("external_source_create_parent_already_present".into());
        }
        let parent = path
            .parent()
            .ok_or("external_source_create_parent_missing")?;
        let (_, _, parent_guard) = held
            .iter()
            .find(|(held, _, _)| held == parent)
            .ok_or("external_source_create_parent_not_held")?;
        let leaf = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("external_source_create_name_invalid")?;
        let guard = create_owned_directory(parent_guard, leaf, self.guards.1.contains(&path))?;
        crate::snapshot::check_opened_path(&guard, &path, self.budget.deadline)?;
        let identity = file_identity(&guard)?;
        if identity.created.is_none() {
            return Err("external_source_created_directory_identity_unavailable".into());
        }
        let digest = identity_digest(&identity);
        held.push((path, identity, guard));
        Ok(digest)
    }
    fn remove(&self, relative: &str, expected: &str) -> Result<(), String> {
        let path = self.path(relative)?;
        let mut held = self
            .guards
            .0
            .lock()
            .map_err(|_| "external_source_guards_unavailable")?;
        SourceGuards::check_held(&held, self.budget)?;
        let Some(index) = held.iter().position(|(held, _, _)| *held == path) else {
            return match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err("external_source_unowned_cleanup_directory_present".into()),
            };
        };
        if identity_digest(&held[index].1) != expected {
            return Err("external_source_cleanup_ownership_mismatch".into());
        }
        let parent = path
            .parent()
            .ok_or("external_source_cleanup_parent_missing")?;
        if !held.iter().any(|(held, _, _)| held == parent) {
            return Err("external_source_cleanup_parent_not_held".into());
        }
        // Only this approved leaf loses its ordinary guard. Its parent and all
        // other guards remain held throughout the identity-checked transition.
        let (_, identity, guard) = held.remove(index);
        drop(guard);
        let result = delete_owned_directory(&path, &identity, self.budget);
        if result.is_err() {
            let guard = if self.guards.1.contains(&path) {
                crate::snapshot::open_checked_writable_directory_identity(
                    &path,
                    self.budget.deadline,
                )
            } else {
                crate::snapshot::open_checked_directory(&path, self.budget.deadline)
            };
            if let Ok(guard) = guard {
                if file_identity(&guard).is_ok_and(|actual| actual == identity) {
                    held.push((path, identity, guard));
                }
            }
        }
        result
    }
}
fn create_owned_directory(parent: &File, leaf: &str, atomic_parent: bool) -> Result<File, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum: u16,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root: *mut std::ffi::c_void,
        name: *mut UnicodeString,
        attributes: u32,
        security: *mut std::ffi::c_void,
        qos: *mut std::ffi::c_void,
    }
    #[repr(C)]
    struct IoStatus {
        status: usize,
        information: usize,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            handle: *mut *mut std::ffi::c_void,
            access: u32,
            attributes: *mut ObjectAttributes,
            status: *mut IoStatus,
            allocation: *const i64,
            file_attributes: u32,
            share: u32,
            disposition: u32,
            options: u32,
            ea: *const std::ffi::c_void,
            ea_length: u32,
        ) -> i32;
    }
    relative_path_for_directory(leaf)?;
    if leaf.contains('/') {
        return Err("external_source_create_leaf_required".into());
    }
    let mut name: Vec<u16> = leaf.encode_utf16().collect();
    let bytes = u16::try_from(
        name.len()
            .checked_mul(2)
            .ok_or("external_source_create_name_overflow")?,
    )
    .map_err(|_| "external_source_create_name_overflow")?;
    let mut unicode = UnicodeString {
        length: bytes,
        maximum: bytes,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root: parent.as_raw_handle(),
        name: &mut unicode,
        attributes: 0,
        security: std::ptr::null_mut(),
        qos: std::ptr::null_mut(),
    };
    let mut io = IoStatus {
        status: 0,
        information: 0,
    };
    let mut raw = std::ptr::null_mut();
    // Atomic directory-relative FILE_CREATE returns ownership and a guard in
    // one operation. Existing names fail; there is no create/open adoption gap.
    // https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntcreatefile
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            0x0010_0081,
            &mut attributes,
            &mut io,
            std::ptr::null(),
            0x80,
            if atomic_parent { 3 } else { 1 },
            2,
            0x21,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        return Err(format!(
            "external_source_create_directory_refused_{:08x}",
            status as u32
        ));
    }
    if raw.is_null() {
        return Err("external_source_create_directory_handle_missing".into());
    }
    // Successful NtCreateFile transfers exactly one handle to File.
    let guard = unsafe { File::from_raw_handle(raw) };
    if io.information != 2 {
        return Err("external_source_directory_not_newly_created".into());
    }
    Ok(guard)
}
fn delete_owned_directory(
    path: &Path,
    expected: &DirectoryIdentity,
    budget: &Budget<'_>,
) -> Result<(), String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    #[repr(C)]
    struct Disposition {
        delete_file: u8,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            handle: *mut std::ffi::c_void,
            class: u32,
            information: *mut std::ffi::c_void,
            bytes: u32,
        ) -> i32;
    }
    let guard = OpenOptions::new()
        .access_mode(0x0001_0081)
        .share_mode(1)
        .custom_flags(0x0220_0000)
        .open(path)
        .map_err(|_| "external_source_cleanup_delete_handle_refused")?;
    crate::snapshot::check_opened_path(&guard, path, budget.deadline)?;
    if &file_identity(&guard)? != expected {
        return Err("external_source_cleanup_identity_changed".into());
    }
    // FileDispositionInfo checks emptiness in the kernel and deletes exactly
    // this owned object on close. No recursive deletion or path-based unlink.
    // https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setfileinformationbyhandle
    let mut disposition = Disposition { delete_file: 1 };
    if unsafe {
        SetFileInformationByHandle(
            guard.as_raw_handle(),
            4,
            (&mut disposition as *mut Disposition).cast(),
            std::mem::size_of::<Disposition>() as u32,
        )
    } == 0
    {
        return Err("external_source_cleanup_directory_not_empty_or_delete_refused".into());
    }
    drop(guard);
    if !matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Err("external_source_cleanup_completion_unverified".into());
    }
    Ok(())
}

/// Private attended-intake preparation. It cannot upgrade VerifiedGrant, and
/// no public request accepts this mode or constructs its confirmation.
pub(crate) struct PreparedSourcePublication {
    grant: PreparedGrant,
    binding: crate::transactions::ExternalBinding,
    roots: Vec<(Root, Fingerprint)>,
    unused_roots: Vec<Root>,
    proposed: Fingerprint,
    changes: Vec<SourceBytes>,
    observations: Vec<(String, Option<ExternalObject>)>,
    guards: SourceGuards,
    private_validation_pending: bool,
    native_validation_tag: Option<String>,
    native_candidate: Option<ValidatedSourceCandidate>,
    native_confirmation_window: Option<NativeConfirmationWindow>,
}
impl PreparedSourcePublication {
    pub(crate) fn prepare(
        plan: &StagingPlan,
        root_id: &RootId,
        changes: Vec<crate::revisions::Change>,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        if root_id.as_str() == "project"
            || &plan.selection.member.root_id != root_id
            || !plan.selected.contains(root_id)
        {
            return Err("external_selected_member_source_root_required".into());
        }
        let root = plan.root(root_id)?;
        let grant = PreparedGrant::prepare(
            &plan.project,
            root_id.clone(),
            &root.source,
            Mode::SourcePublish,
            budget,
        )?;
        let member_src = format!("{}/src", directory_of(&plan.selection.member.path));
        relative(&member_src, false)?;
        // Existing src identity must be held even when every target is a create.
        let _src = crate::snapshot::open_checked_directory(
            &root.source.join(&member_src),
            budget.deadline,
        )?;
        if changes.is_empty() || changes.len() > crate::revisions::MAX_FILES {
            return Err("source change count exceeds 1..64".into());
        }
        let mut seen = BTreeSet::new();
        let mut proposed_bytes = 0usize;
        for change in &changes {
            source_target(&member_src, &change.path)?;
            if !seen.insert(change.path.to_ascii_lowercase()) {
                return Err("source_case_alias_or_duplicate".into());
            }
            let expected = &change.expected_sha256;
            let valid_hash =
                expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit());
            match change.action {
                crate::revisions::Action::Create
                    if expected.is_empty() && change.text.is_some() => {}
                crate::revisions::Action::Replace if valid_hash && change.text.is_some() => {}
                crate::revisions::Action::Delete if valid_hash && change.text.is_none() => {}
                _ => return Err("external_source_change_shape_refused".into()),
            }
            let size = change.text.as_ref().map_or(0, String::len);
            proposed_bytes = proposed_bytes
                .checked_add(size)
                .ok_or("external_source_proposed_byte_overflow")?;
            if size > crate::revisions::MAX_FILE_BYTES
                || proposed_bytes > crate::revisions::MAX_TEXT_BYTES
            {
                return Err("external_source_proposed_bytes_exceeded".into());
            }
        }
        let _leases = plan.leases(budget)?;
        let reader = ExternalReadAccess::new(&grant, budget)?;
        let guards = SourceGuards::capture(
            &root.source,
            changes.iter().map(|change| change.path.clone()),
            budget,
        )?;
        let mut roots = Vec::new();
        let mut target_entries = Vec::new();
        let mut usage = Usage::default();
        for entry in &plan.roots {
            if !plan.selected.contains(&entry.id) {
                continue;
            }
            let (rows, fingerprint) = scan(&entry.source, budget, &mut usage)?;
            if &entry.id == root_id {
                target_entries = rows;
            }
            if entry.id.as_str() == "project"
                && fingerprint.sha256 != plan.project.revision.as_str()
            {
                return Err("imported_project_revision_changed".into());
            }
            roots.push((entry.clone(), fingerprint));
        }
        let mut source_bytes = 0usize;
        let mut rows = Vec::new();
        let mut targets = Vec::new();
        let mut observations = Vec::new();
        for change in changes {
            let observed = crate::transactions::ExternalReadIo::target(&reader, &change.path)?;
            let before = observed.as_ref().map(|(_, bytes)| bytes.clone());
            if before
                .as_ref()
                .is_some_and(|bytes| std::str::from_utf8(bytes).is_err())
            {
                return Err("source_target_utf8_required".into());
            }
            observations.push((change.path.clone(), observed.map(|v| v.0)));
            source_bytes = source_bytes
                .checked_add(before.as_ref().map_or(0, Vec::len))
                .ok_or("external_source_original_byte_overflow")?;
            if source_bytes > crate::revisions::MAX_TEXT_BYTES {
                return Err("external_source_original_bytes_exceeded".into());
            }
            let before_hash = before.as_ref().map(|bytes| crate::sha256_hex(bytes));
            let expected = if change.expected_sha256.is_empty() {
                None
            } else {
                Some(change.expected_sha256.to_ascii_lowercase())
            };
            if before_hash != expected {
                return Err("external_source_target_changed_during_preparation".into());
            }
            let after = change.text.map(String::into_bytes);
            let after_hash = after.as_ref().map(|bytes| crate::sha256_hex(bytes));
            target_entries.retain(|entry| entry.path != change.path);
            if let Some(bytes) = &after {
                target_entries.push(Entry {
                    path: change.path.clone(),
                    bytes: bytes.len() as u64,
                    sha256: after_hash.clone().ok_or("source_proposal_hash_missing")?,
                    directory: false,
                });
                let mut parent = directory_of(&change.path);
                while !parent.is_empty() {
                    if !target_entries.iter().any(|entry| entry.path == parent) {
                        target_entries.push(Entry {
                            path: parent.into(),
                            bytes: 0,
                            sha256: String::new(),
                            directory: true,
                        });
                    }
                    parent = directory_of(parent);
                }
            }
            targets.push(crate::transactions::ExternalTarget {
                path: change.path.clone(),
                before: before_hash,
                after: after_hash,
            });
            rows.push((change.path, before, after));
        }
        target_entries.sort_by(|a, b| a.path.cmp(&b.path));
        let proposed = fingerprint_entries(&target_entries);
        let mut binding = crate::transactions::ExternalBinding {
            version: 2,
            imported_path: plan.project.root.to_string_lossy().into_owned(),
            imported_identity: identity_digest(&plan.project.identity),
            imported_revision: plan.project.revision.0.clone(),
            external_path: root.source.to_string_lossy().into_owned(),
            external_identity: identity_digest(&root.identity),
            root_id: root_id.0.clone(),
            member_src,
            proposal_digest: String::new(),
            authorization_digest: String::new(),
            targets,
            cleanup_dirs: Vec::new(),
        };
        binding.cleanup_dirs = crate::transactions::planned_v3_cleanup(&binding, &|path| {
            crate::transactions::ExternalReadIo::source_directory(&reader, path)
        })?;
        drop(reader);
        let unused_roots: Vec<_> = plan
            .roots
            .iter()
            .filter(|root| !plan.selected.contains(&root.id))
            .cloned()
            .collect();
        binding.proposal_digest = crate::sha256_hex(
            format!(
                "owner-external-source-publication/3|{}|{}|{:?}|{:?}|{:?}|{:?}",
                grant.digest(),
                serde_json::to_string(&binding).map_err(|error| error.to_string())?,
                roots
                    .iter()
                    .map(|(root, fingerprint)| (&root.id, fingerprint))
                    .collect::<Vec<_>>(),
                proposed,
                unused_roots
                    .iter()
                    .map(|root| (&root.id, &root.source, &root.identity))
                    .collect::<Vec<_>>(),
                observations
            )
            .as_bytes(),
        );
        binding.authorization_digest = binding.proposal_digest.clone();
        guards.check(budget)?;
        Ok(Self {
            grant,
            binding,
            roots,
            unused_roots,
            proposed,
            changes: rows,
            observations,
            guards,
            private_validation_pending: false,
            native_validation_tag: None,
            native_candidate: None,
            native_confirmation_window: None,
        })
    }
    pub(crate) fn digest(&self) -> &str {
        &self.binding.authorization_digest
    }
    pub(crate) fn reviewed_changes(&self) -> &[crate::transactions::ExternalTarget] {
        &self.binding.targets
    }
    pub(crate) fn finish(
        self,
        confirmation: crate::workspace_access::OwnerConfirmation,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        revision: &Revision,
        budget: &Budget<'_>,
    ) -> Result<SourcePublicationPermit, String> {
        if self.private_validation_pending && self.native_validation_tag.is_none() {
            return Err("external_source_postvalidation_confirmation_required".into());
        }
        if !confirmation.matches(self.digest()) {
            return Err("owner_source_confirmation_binding_mismatch".into());
        }
        let session = if self.private_validation_pending {
            let window = self
                .native_confirmation_window
                .as_ref()
                .ok_or("external_source_postvalidation_confirmation_required")?;
            window.check(budget)?;
            if !window.matches_owner(broker, &cancelled) {
                return Err("external_source_confirmation_owner_binding_mismatch".into());
            }
            let candidate = self
                .native_candidate
                .as_ref()
                .ok_or("external_source_private_native_candidate_missing")?;
            candidate.proposal.limits.check_not_wider(budget)?;
            let check = || {
                budget.check()?;
                window.check_authority(budget.deadline)
            };
            let checked = budget.with_check(&check);
            self.grant.recheck(revision, &checked)?;
            self.recheck_native_candidate(candidate, &checked)?;
            let session = SourceSession {
                broker: std::sync::Arc::downgrade(broker),
                cancelled,
                expires: budget.deadline,
                publication_operation: Some(window.operation.clone()),
                source_limits: Some(candidate.proposal.limits),
            };
            session.check(&checked)?;
            self.guards.check(&checked)?;
            checked.check()?;
            session
        } else {
            self.grant.recheck(revision, budget)?;
            let session = SourceSession {
                broker: std::sync::Arc::downgrade(broker),
                cancelled,
                expires: budget.deadline,
                publication_operation: None,
                source_limits: None,
            };
            session.check(budget)?;
            self.guards.check(budget)?;
            session
        };
        Ok(SourcePublicationPermit {
            prepared: self,
            session,
        })
    }
    /// Private read/compile staging only. No owner confirmation, publication
    /// session or permission to modify an original source is created here.
    pub(crate) fn stage_for_native_validation(
        &mut self,
        plan: &StagingPlan,
        envelope: &crate::candidate_workspace::SourceEnvelope<'_>,
        parent: &mut crate::external_native::ParentDeadline,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        budget: &Budget<'_>,
    ) -> Result<SourceProposal, String> {
        budget.check()?;
        parent.check()?;
        if self.private_validation_pending || self.native_validation_tag.is_some() {
            return Err("external_source_private_proposal_already_staged".into());
        }
        if budget.deadline > parent.instant() {
            return Err("external_source_staging_exceeds_original_native_parent".into());
        }
        parent.bind_read_owner(broker, cancelled, ProposalLimits::from_budget(budget))?;
        let check = || {
            budget.check()?;
            parent.check()
        };
        let checked = budget.with_check(&check);
        self.recheck_unconfirmed_originals(&checked)?;
        let staged = plan.stage_owned(envelope, &checked)?;
        let proposal = self.overlay_private(staged, &checked, Some(parent.origin().clone()))?;
        parent.check()?;
        self.private_validation_pending = true;
        Ok(proposal)
    }
    /// The new confirmation digest is unavailable until the same opaque
    /// compiler result and its original File/retirement are revalidated.
    pub(crate) fn bind_validated_candidate(
        &mut self,
        candidate: ValidatedSourceCandidate,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        budget.check()?;
        if !self.private_validation_pending
            || self.native_validation_tag.is_some()
            || self.native_candidate.is_some()
        {
            return Err("external_source_private_candidate_binding_state".into());
        }
        let window = NativeConfirmationWindow::new(broker, cancelled, budget)?;
        let check = || {
            budget.check()?;
            window.check_authority(budget.deadline)
        };
        let checked = budget.with_check(&check);
        self.recheck_native_candidate(&candidate, &checked)?;
        let tag = candidate
            .native_validation_tag
            .as_ref()
            .ok_or("external_source_private_native_validation_missing")?;
        let authorization_digest = crate::sha256_hex(
            format!(
                "owner-external-source-after-native/1|{}|{tag}",
                self.binding.proposal_digest
            )
            .as_bytes(),
        );
        checked.check()?;
        self.binding.authorization_digest = authorization_digest;
        self.native_validation_tag = Some(tag.clone());
        self.native_candidate = Some(candidate);
        self.native_confirmation_window = Some(window);
        Ok(())
    }
    fn recheck_native_candidate(
        &self,
        candidate: &ValidatedSourceCandidate,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        let restricted = candidate.proposal.limits.restrict(budget);
        let budget = &restricted;
        let tag = candidate
            .native_validation_tag
            .as_ref()
            .ok_or("external_source_private_native_validation_missing")?;
        if candidate.proposal.native_parent_origin.is_none()
            || candidate.proposal.digest != self.binding.proposal_digest
            || self
                .native_validation_tag
                .as_ref()
                .is_some_and(|expected| expected != tag)
            || !candidate
                .ownership
                .matches(&candidate.workspace, &candidate.proposal.staged.envelope)
        {
            return Err("external_source_validated_candidate_binding_mismatch".into());
        }
        self.recheck_unconfirmed_originals(budget)?;
        check_originals(&self.roots, &candidate.proposal.staged, budget)?;
        check_proposed(&candidate.proposal.staged, budget)?;
        candidate.compiler_provenance.recheck(
            &candidate.proposal.staged,
            &candidate.artifact,
            &candidate.artifact_hash,
            budget,
        )?;
        if hash_validated_artifact(&candidate.artifact, &candidate.artifact_guard, budget)?
            != candidate.artifact_hash
        {
            return Err("external_source_validated_artifact_changed".into());
        }
        Ok(())
    }
    fn recheck_unconfirmed_originals(&self, budget: &Budget<'_>) -> Result<(), String> {
        self.grant.recheck(&self.grant.project.revision, budget)?;
        self.guards.check(budget)?;
        if current_revision(&self.grant.project.root, budget)?
            != self.grant.project.revision.as_str()
        {
            return Err("external_source_imported_revision_changed_before_confirmation".into());
        }
        let mut usage = Usage::default();
        for (root, expected) in &self.roots {
            if canonical_directory(&root.source, budget)?.1 != root.identity
                || scan(&root.source, budget, &mut usage)?.1 != *expected
            {
                return Err("external_source_original_fingerprint_changed".into());
            }
        }
        for root in &self.unused_roots {
            let (path, identity) = canonical_directory(&root.source, budget)?;
            if path != root.source || identity != root.identity {
                return Err("unused_granted_root_identity_changed".into());
            }
        }
        Ok(())
    }
}
fn source_target(member_src: &str, path: &str) -> Result<(), String> {
    relative(path, false)?;
    if !path.starts_with(&(member_src.to_owned() + "/"))
        || !path.ends_with(".rs")
        || !public_source(path)
        || path.split('/').any(|part| {
            part.eq_ignore_ascii_case("build.rs")
                || part.starts_with('.')
                || part.ends_with('.')
                || windows_device_name(part)
        })
    {
        return Err("only_selected_member_utf8_rust_source_is_publishable".into());
    }
    Ok(())
}
fn windows_device_name(part: &str) -> bool {
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
#[cfg(windows)]
fn source_text(root: &Path, path: &str, budget: &Budget<'_>) -> Result<Option<Vec<u8>>, String> {
    namespace_io::read_source(root, path, budget)
}
#[cfg(not(windows))]
fn source_text(root: &Path, path: &str, budget: &Budget<'_>) -> Result<Option<Vec<u8>>, String> {
    // Reject alternate case spellings even on case-insensitive filesystems.
    // Missing suffixes are create destinations, never aliases of existing names.
    let mut parent = root.to_path_buf();
    for part in relative(path, false)? {
        budget.check()?;
        match std::fs::read_dir(&parent) {
            Ok(entries) => {
                for (index, entry) in entries.enumerate() {
                    budget.check()?;
                    if index >= MAX_ENTRIES {
                        return Err("source_parent_entry_limit".into());
                    }
                    let name = entry.map_err(|_| "source_parent_unavailable")?.file_name();
                    let name = name.to_str().ok_or("source_filename_not_utf8")?;
                    if name.eq_ignore_ascii_case(part) && name != part {
                        return Err("source_case_alias_or_duplicate".into());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err("source_parent_unavailable".into()),
        }
        parent.push(part);
    }
    let full = root.join(path);
    match std::fs::symlink_metadata(&full) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("source_target_unavailable".into()),
        Ok(metadata) => {
            ordinary(&metadata)?;
            if !metadata.is_file() {
                return Err("source_target_regular_file_required".into());
            }
            let (_, bytes) = read_file_with_capture(
                &full,
                crate::revisions::MAX_FILE_BYTES as u64,
                budget,
                None,
                None,
                ReadCapture::BoundedBytes,
            )?;
            std::str::from_utf8(&bytes).map_err(|_| "source_target_utf8_required")?;
            Ok(Some(bytes))
        }
    }
}

/// Noncloneable, nonserializable, single-use owner authority.
pub(crate) struct SourcePublicationPermit {
    prepared: PreparedSourcePublication,
    session: SourceSession,
}
pub(crate) struct SourceProposal {
    staged: StagedSources,
    digest: String,
    limits: ProposalLimits,
    native_parent_origin: Option<crate::external_native::NativeParentOrigin>,
}
// The small variant exists only in test builds, so production has one variant.
#[cfg_attr(test, allow(clippy::large_enum_variant))]
enum CandidateCompilerProvenance {
    Real(crate::external_native::NativeCompilationProvenance),
    #[cfg(test)]
    SyntheticAuthorityOnly,
}
impl CandidateCompilerProvenance {
    fn real(&self) -> Result<&crate::external_native::NativeCompilationProvenance, String> {
        match self {
            Self::Real(value) => Ok(value),
            #[cfg(test)]
            Self::SyntheticAuthorityOnly => {
                Err("external_source_synthetic_provenance_refused".into())
            }
        }
    }
    fn recheck(
        &self,
        staged: &StagedSources,
        artifact: &Path,
        artifact_hash: &str,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        match self {
            Self::Real(value) => value.recheck(staged, artifact, artifact_hash, budget),
            #[cfg(test)]
            Self::SyntheticAuthorityOnly => Ok(()),
        }
    }
    fn real_digest(&self) -> Result<&str, String> {
        match self {
            Self::Real(value) => Ok(value.digest()),
            #[cfg(test)]
            Self::SyntheticAuthorityOnly => {
                Err("external_source_synthetic_provenance_refused".into())
            }
        }
    }
}
pub(crate) struct ValidatedSourceCandidate {
    proposal: SourceProposal,
    ownership: crate::candidate_workspace::ResolvedCompilation,
    workspace: PathBuf,
    artifact: PathBuf,
    artifact_hash: String,
    artifact_guard: File,
    native_validation_tag: Option<String>,
    compiler_provenance: CandidateCompilerProvenance,
}
/// Private scoped custody. Candidate guards, including staged-content witnesses,
/// provenance witness, drop before the original allocation can be cleaned.
pub(crate) struct ExternalArtifactCustody<'a> {
    // Destination and candidate handles drop before recognized allocation cleanup.
    private_artifact: Option<PrivateArtifactPlacement>,
    gdextension_binding: Option<PrivateGdExtensionBinding>,
    candidate: ValidatedSourceCandidate,
    workspace: crate::candidate_workspace::Workspace,
    parent: &'a crate::external_native::ParentDeadline,
    controller: &'a crate::worker_windows::StreamController,
    budget: Budget<'a>,
}

pub(crate) fn validate_configfile_operation(
    mut workspace: crate::candidate_workspace::Workspace,
    plan: &StagingPlan,
    selection: &ManifestRef,
    parent: &crate::external_native::ParentDeadline,
    controller: &crate::worker_windows::StreamController,
    budget: &Budget<'_>,
    permission: &crate::native_operations::ConfigFileParsePermission<'_>,
) -> Result<serde_json::Value, String> {
    let check = || {
        budget.check()?;
        permission.check(parent, controller)
    };
    let checked = budget.with_check(&check);
    check()?;
    let staged = {
        let envelope = workspace.source_envelope(checked.deadline())?;
        plan.stage_owned(&envelope, &checked)?
    };
    let rows: Vec<_> = staged.roots.iter().map(|row| {
        let slot = row.staged.strip_prefix(&staged.envelope).map_err(|_| "configfile_parser_root_escaped")?;
        Ok(serde_json::json!({"root_id":row.root_id.as_str(),"slot":slot.to_string_lossy().replace('\\',"/"),
            "permission":if row.root_id.as_str()=="project" {"project_source_read"} else {"dependency_read"},
            "sha256":row.fingerprint.sha256,"files":row.fingerprint.files,"bytes":row.fingerprint.bytes}))
    }).collect::<Result<_,String>>()?;
    let proposal = SourceProposal::from_read_staging(staged, parent, &checked)?;
    let validation = proposal.validate_read_with_receipt(&mut workspace, controller, parent);
    let (candidate, mut value) = match validation {
        Ok(value) => value,
        Err(error) => {
            if workspace.lifetime.unresolved() {
                permission.mark_unconfirmed();
            }
            if workspace.clean().is_err() {
                return Err("configfile_parser_compiler_cleanup_unconfirmed".into());
            }
            return Err(error);
        }
    };
    let mut custody = match candidate.into_artifact_custody(workspace, parent, controller, &checked)
    {
        Ok(value) => value,
        Err((error, candidate, mut workspace)) => {
            drop(candidate);
            if workspace.clean().is_err() {
                return Err("configfile_parser_custody_cleanup_unconfirmed".into());
            }
            return Err(error);
        }
    };
    custody.place_private_artifact()?;
    custody.prepare_gdextension_binding(&selection.root_id, &selection.path)?;
    let comparison = custody.compare_configfile(permission)?;
    custody.finish_read_validation()?;
    check()?;
    value["ok"] = serde_json::json!(true);
    value["roots"] = serde_json::json!(rows);
    value["configfile_comparison"] = comparison;
    value["private_workspace_cleaned"] = serde_json::json!(true);
    value["candidate_lease_owned"] = serde_json::json!(true);
    value["source_publish"] = serde_json::json!(false);
    value["loaded"] = serde_json::json!(false);
    value["abi_verified"] = serde_json::json!(false);
    Ok(value)
}
impl ValidatedSourceCandidate {
    /// Move both original inputs. Refusal returns both without cleanup or renewal.
    // Refusal must hand both owned inputs back to the caller, so the error stays large.
    #[allow(clippy::result_large_err)]
    pub(crate) fn into_artifact_custody<'a>(
        self,
        workspace: crate::candidate_workspace::Workspace,
        parent: &'a crate::external_native::ParentDeadline,
        controller: &'a crate::worker_windows::StreamController,
        budget: &Budget<'a>,
    ) -> Result<ExternalArtifactCustody<'a>, (String, Self, crate::candidate_workspace::Workspace)>
    {
        let custody = ExternalArtifactCustody {
            private_artifact: None,
            gdextension_binding: None,
            candidate: self,
            workspace,
            parent,
            controller,
            budget: budget.with_check(budget.check_cancelled),
        };
        match custody.recheck() {
            Ok(()) => Ok(custody),
            Err(error) => {
                let ExternalArtifactCustody {
                    candidate,
                    workspace,
                    ..
                } = custody;
                Err((error, candidate, workspace))
            }
        }
    }
}
impl ExternalArtifactCustody<'_> {
    pub(crate) fn compare_configfile(
        &self,
        permission: &crate::native_operations::ConfigFileParsePermission<'_>,
    ) -> Result<serde_json::Value, String> {
        self.recheck_gdextension_binding()?;
        let selected = &self
            .gdextension_binding
            .as_ref()
            .ok_or("configfile_parser_binding_missing")?
            .selected;
        let input = ConfigFileInput { selected };
        let mut value = crate::external_native::compare_configfile(
            &self.workspace,
            &input,
            self.parent,
            self.controller,
            permission,
        )?;
        self.recheck_gdextension_binding()?;
        value["manifest"] = selected.manifest.digest_value();
        value["entry_symbol"] = serde_json::json!(selected.entry_symbol);
        value["library_relative"] = serde_json::json!(selected.library_relative);
        Ok(value)
    }
    fn finish_read_validation(self) -> Result<(), String> {
        self.budget.check()?;
        self.parent.check_read_controller(self.controller)?;
        let Self {
            private_artifact,
            gdextension_binding,
            candidate,
            mut workspace,
            ..
        } = self;
        drop(private_artifact);
        drop(gdextension_binding);
        drop(candidate);
        workspace.clean()
    }
    /// Original custody status only, without issuing loading or publication rights.
    pub(crate) fn recheck(&self) -> Result<(), String> {
        let check = || {
            self.budget.check()?;
            self.parent.check_read_controller(self.controller)
        };
        let checked = self.budget.with_check(&check);
        checked.check()?;
        if checked.deadline > self.parent.instant()
            || self.candidate.proposal.native_parent_origin.as_ref() != Some(self.parent.origin())
            || self.candidate.native_validation_tag.is_none()
            || self.candidate.workspace != self.workspace.path
        {
            return Err("external_artifact_custody_parent_or_seal_mismatch".into());
        }
        self.candidate.proposal.limits.check_not_wider(&checked)?;
        self.candidate.ownership.check_workspace_owner(
            &self.workspace,
            &self.candidate.proposal.staged.envelope,
            &checked,
        )?;
        // A test-only authority marker is never a positive compiler witness.
        self.candidate.compiler_provenance.real_digest()?;
        self.candidate.compiler_provenance.recheck(
            &self.candidate.proposal.staged,
            &self.candidate.artifact,
            &self.candidate.artifact_hash,
            &checked,
        )?;
        if stamp(&self.candidate.artifact_guard)?.bytes > checked.max_file {
            return Err("external_artifact_custody_original_file_limit".into());
        }
        check_proposed(&self.candidate.proposal.staged, &checked)?;
        if hash_validated_artifact(
            &self.candidate.artifact,
            &self.candidate.artifact_guard,
            &checked,
        )? != self.candidate.artifact_hash
        {
            return Err("external_artifact_custody_held_artifact_changed".into());
        }
        checked.check()
    }

    /// Place the checked DLL once in this same allocation's fixed private slot.
    /// Errors retain any created writer and partial bytes in this custody owner.
    pub(crate) fn place_private_artifact(&mut self) -> Result<(), String> {
        if self.private_artifact.is_some() {
            return Err("external_private_artifact_already_attempted".into());
        }
        // Rechecks also hash the original. Preserve its shared cursor across the
        // entire operation, including cancellation and post-copy refusals.
        let mut cursor = self
            .candidate
            .artifact_guard
            .try_clone()
            .map_err(|_| "external_private_artifact_cursor_unavailable")?;
        let position = std::io::Seek::stream_position(&mut cursor)
            .map_err(|_| "external_private_artifact_cursor_unavailable")?;
        let result = self.place_private_artifact_inner();
        restore_private_artifact_cursor(&mut cursor, position, result, &self.budget)?;
        self.private_artifact
            .as_mut()
            .ok_or("external_private_artifact_missing")?
            .complete = true;
        Ok(())
    }

    fn place_private_artifact_inner(&mut self) -> Result<(), String> {
        self.recheck()?;
        let before = stamp(&self.candidate.artifact_guard)?;
        let used = crate::candidate_workspace::usage_with_budget(
            &self.workspace.path,
            crate::candidate_workspace::WORKSPACE_BYTES,
            self.budget
                .deadline()
                .saturating_duration_since(Instant::now()),
        )?;
        private_artifact_admission(before.bytes, used, &self.budget)?;
        let slot = self.candidate.ownership.private_artifact_slot(
            &self.workspace,
            &self.candidate.proposal.staged.envelope,
            &self.budget,
        )?;
        // Preregister the destination before CreateNew; never delete or retry a
        // refused/partial output by guessing its path or dropping original custody.
        self.private_artifact = Some(PrivateArtifactPlacement {
            slot,
            writer: None,
            expected: None,
            complete: false,
        });
        {
            let placement = self
                .private_artifact
                .as_mut()
                .ok_or("external_private_artifact_missing")?;
            placement.writer = Some(placement.slot.create_new(
                &self.candidate.ownership,
                &self.workspace,
                &self.candidate.proposal.staged.envelope,
                &self.budget,
            )?);
            let PrivateArtifactPlacement {
                slot,
                writer,
                expected,
                ..
            } = placement;
            let check = || {
                self.budget.check()?;
                self.parent.check_read_controller(self.controller)?;
                slot.recheck(
                    &self.candidate.ownership,
                    &self.workspace,
                    &self.candidate.proposal.staged.envelope,
                    &self.budget,
                )
            };
            let checked = self.budget.with_check(&check);
            *expected = Some(copy_checked_private_artifact(
                &self.candidate.artifact,
                &self.candidate.artifact_guard,
                &before,
                &self.candidate.artifact_hash,
                slot.path(),
                writer
                    .as_mut()
                    .ok_or("external_private_artifact_writer_missing")?,
                &checked,
            )?);
            checked.check()?;
            crate::candidate_workspace::usage_with_budget(
                &self.workspace.path,
                crate::candidate_workspace::WORKSPACE_BYTES,
                checked.deadline().saturating_duration_since(Instant::now()),
            )?;
        }
        self.recheck()
    }

    /// Check the same retained writer and original custody, never a reopened DLL.
    pub(crate) fn recheck_private_artifact(&self) -> Result<(), String> {
        let mut cursor = self
            .candidate
            .artifact_guard
            .try_clone()
            .map_err(|_| "external_private_artifact_cursor_unavailable")?;
        let position = std::io::Seek::stream_position(&mut cursor)
            .map_err(|_| "external_private_artifact_cursor_unavailable")?;
        let result = (|| {
            self.recheck()?;
            let placement = self
                .private_artifact
                .as_ref()
                .ok_or("external_private_artifact_missing")?;
            if !placement.complete {
                return Err("external_private_artifact_incomplete".into());
            }
            placement.slot.recheck(
                &self.candidate.ownership,
                &self.workspace,
                &self.candidate.proposal.staged.envelope,
                &self.budget,
            )?;
            let writer = placement
                .writer
                .as_ref()
                .ok_or("external_private_artifact_writer_missing")?;
            check_private_artifact_copy(
                placement.slot.path(),
                writer,
                placement
                    .expected
                    .as_ref()
                    .ok_or("external_private_artifact_incomplete")?,
                &self.candidate.artifact_hash,
                &self.budget,
            )?;
            self.recheck()
        })();
        restore_private_artifact_cursor(&mut cursor, position, result, &self.budget)
    }

    /// Prepare only a quoted Windows-debug/x86_64 manifest-to-private-DLL plan.
    /// Selection names held source, never a caller-chosen output destination.
    pub(crate) fn prepare_gdextension_binding(
        &mut self,
        root: &RootId,
        manifest_relative: &str,
    ) -> Result<&PrivateGdExtensionBinding, String> {
        if self.gdextension_binding.is_some() {
            return Err("external_private_binding_already_prepared".into());
        }
        self.recheck_private_artifact()?;
        let binding = {
            let check = || {
                self.budget.check()?;
                self.parent.check_read_controller(self.controller)
            };
            let checked = self.budget.with_check(&check);
            let provenance = self.candidate.compiler_provenance.real()?;
            let (inputs, host) = provenance.held_binding_inputs();
            if host != "x86_64-pc-windows-msvc" {
                return Err("external_private_binding_host_unsupported".into());
            }
            let selected = inputs.gdextension_manifest(root, manifest_relative, &checked)?;
            let writer = self
                .private_artifact
                .as_ref()
                .and_then(|placement| placement.writer.as_ref())
                .ok_or("external_private_artifact_writer_missing")?;
            let direct_export = self.inspect_private_named_export(&selected.entry_symbol)?;
            checked.check()?;
            PrivateGdExtensionBinding {
                selected,
                provenance_digest: provenance.digest().to_owned(),
                placed_identity: file_identity(writer)?,
                direct_export,
            }
        };
        self.recheck_private_artifact()?;
        self.gdextension_binding = Some(binding);
        self.gdextension_binding
            .as_ref()
            .ok_or_else(|| "external_private_binding_missing".into())
    }

    pub(crate) fn recheck_gdextension_binding(&self) -> Result<(), String> {
        self.recheck_private_artifact()?;
        let check = || {
            self.budget.check()?;
            self.parent.check_read_controller(self.controller)
        };
        let checked = self.budget.with_check(&check);
        let binding = self
            .gdextension_binding
            .as_ref()
            .ok_or("external_private_binding_missing")?;
        let provenance = self.candidate.compiler_provenance.real()?;
        let (inputs, host) = provenance.held_binding_inputs();
        let writer = self
            .private_artifact
            .as_ref()
            .and_then(|placement| placement.writer.as_ref())
            .ok_or("external_private_artifact_writer_missing")?;
        if host != "x86_64-pc-windows-msvc"
            || binding.provenance_digest != provenance.digest()
            || binding.placed_identity != file_identity(writer)?
        {
            return Err("external_private_binding_owner_changed".into());
        }
        let selected = inputs.gdextension_manifest(
            &binding.selected.manifest.root,
            &binding.selected.manifest.relative,
            &checked,
        )?;
        if selected != binding.selected {
            return Err("external_private_binding_changed".into());
        }
        if self.inspect_private_named_export(&selected.entry_symbol)? != binding.direct_export {
            return Err("external_private_binding_export_changed".into());
        }
        checked.check()?;
        self.recheck_private_artifact()
    }

    /// Read only the same retained placed writer. Even an export refusal is
    /// followed by original custody/hash checks, not a retry or a new budget.
    fn inspect_private_named_export(
        &self,
        name: &str,
    ) -> Result<crate::external_native::NamedPeExport, String> {
        self.recheck_private_artifact()?;
        let result = (|| {
            let check = || {
                self.budget.check()?;
                self.parent.check_read_controller(self.controller)
            };
            let checked = self.budget.with_check(&check);
            let provenance = self.candidate.compiler_provenance.real()?;
            let (_, host) = provenance.held_binding_inputs();
            let writer = self
                .private_artifact
                .as_ref()
                .and_then(|placement| placement.writer.as_ref())
                .ok_or("external_private_artifact_writer_missing")?;
            let mut reader = writer;
            crate::external_native::named_direct_pe_export(
                &mut reader,
                stamp(writer)?.bytes,
                host,
                name,
                &checked,
            )
        })();
        self.recheck_private_artifact()?;
        result
    }
}

/// Borrowed only from its original custody owner. Plain names are a limited plan,
/// with one structural named export, never ConfigFile/ABI or loading authority.
pub(crate) struct PrivateGdExtensionBinding {
    selected: HeldGdExtensionManifest,
    provenance_digest: String,
    placed_identity: DirectoryIdentity,
    direct_export: crate::external_native::NamedPeExport,
}
impl PrivateGdExtensionBinding {
    pub(crate) fn library_relative(&self) -> &str {
        &self.selected.library_relative
    }
    pub(crate) fn entry_symbol(&self) -> &str {
        &self.selected.entry_symbol
    }
    pub(crate) fn direct_export_rva(&self) -> u32 {
        self.direct_export.rva()
    }
}

#[derive(PartialEq, Eq)]
struct HeldGdExtensionManifest {
    manifest: NativeLocalInput,
    project: NativeLocalInput,
    library_relative: String,
    entry_symbol: String,
    text: String,
    document: serde_json::Value,
}
/// Borrowed only after real typed custody and the held manifest were checked.
/// No constructor or serialized form is exposed to the execution adapter.
pub(crate) struct ConfigFileInput<'a> {
    selected: &'a HeldGdExtensionManifest,
}
impl ConfigFileInput<'_> {
    pub(crate) fn parts(&self) -> (&str, &str, &str, &serde_json::Value) {
        (
            self.selected.manifest.root.as_str(),
            &self.selected.manifest.relative,
            &self.selected.text,
            &self.selected.document,
        )
    }
}

pub(crate) const PRIVATE_GDEXTENSION_FEATURES: [&str; 6] =
    ["windows", "debug", "x86_64", "single", "64", "editor"];
const PRIVATE_GDEXTENSION_BYTES: u64 = 65536;

fn private_gdextension_parts(value: &str) -> Result<Vec<&str>, String> {
    let parts = relative(value, false).map_err(|_| "external_private_binding_unsafe_path")?;
    for part in &parts {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.ends_with('.')
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err("external_private_binding_unsafe_path".into());
        }
    }
    Ok(parts)
}

fn private_gdextension_line(raw: &str) -> Result<&str, String> {
    if raw.len() > 4096 {
        return Err("external_private_binding_text_bound".into());
    }
    let raw = raw.trim();
    if raw.starts_with('#') {
        return Ok("");
    }
    let mut quoted = false;
    for (index, character) in raw.char_indices() {
        if character == ';' && !quoted {
            return Ok(raw[..index].trim());
        }
        if character == '\\' {
            return Err("external_private_binding_escape_unsupported".into());
        }
        if character == '"' {
            quoted = !quoted;
        }
    }
    if quoted {
        return Err("external_private_binding_variant_unsupported".into());
    }
    Ok(raw)
}

fn private_gdextension_quoted(value: &str) -> Result<String, String> {
    if value.contains('\\') {
        return Err("external_private_binding_escape_unsupported".into());
    }
    serde_json::from_str::<String>(value)
        .map_err(|_| "external_private_binding_variant_unsupported".into())
}

/// Strict quoted subset only. Godot ConfigFile semantics and export/ABI validity
/// are deliberately not inferred from this library-selection plan.
fn private_gdextension_mapping(text: &str) -> Result<(String, String), String> {
    if text.is_empty()
        || text.len() as u64 > PRIVATE_GDEXTENSION_BYTES
        || text.starts_with('\u{feff}')
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
    {
        return Err("external_private_binding_text_bound".into());
    }
    let mut sections = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut section = "";
    let mut entry_symbol = None;
    let mut selected = None;
    let mut libraries = 0usize;
    for raw in text.lines() {
        let line = private_gdextension_line(raw)?;
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            if section == "dependencies" {
                return Err("external_private_binding_dependencies_unsupported".into());
            }
            if !matches!(section, "configuration" | "libraries") || !sections.insert(section) {
                return Err("external_private_binding_section_unsupported_or_duplicate".into());
            }
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or("external_private_binding_variant_unsupported")?;
        let key = key.trim();
        let key = if key.starts_with('"') {
            private_gdextension_quoted(key)?
        } else {
            key.to_owned()
        };
        let value = value.trim();
        if key.is_empty()
            || key.len() > 128
            || keys.len() >= 132
            || !keys.insert((section.to_owned(), key.clone()))
        {
            return Err("external_private_binding_duplicate_or_oversized_key".into());
        }
        match section {
            "configuration" => match key.as_str() {
                "entry_symbol" => {
                    let entry = private_gdextension_quoted(value)?;
                    if entry.is_empty()
                        || entry.len() > 128
                        || !entry.bytes().enumerate().all(|(i, b)| {
                            b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                        })
                    {
                        return Err("external_private_binding_entry_symbol_unsupported".into());
                    }
                    entry_symbol = Some(entry);
                }
                "compatibility_minimum" | "compatibility_maximum" => {
                    let version = private_gdextension_quoted(value)?;
                    if version.len() > 32
                        || version.split('.').any(|part| {
                            part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit())
                        })
                    {
                        return Err("external_private_binding_variant_unsupported".into());
                    }
                }
                "reloadable" if matches!(value, "true" | "false") => {}
                _ => return Err("external_private_binding_variant_unsupported".into()),
            },
            "libraries" => {
                libraries += 1;
                if libraries > 128 {
                    return Err("external_private_binding_text_bound".into());
                }
                let flags: Vec<_> = key.split('.').collect();
                let unique: BTreeSet<_> = flags.iter().copied().collect();
                if unique.len() != flags.len()
                    || flags.iter().any(|flag| {
                        flag.is_empty()
                            || !flag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
                {
                    return Err("external_private_binding_variant_unsupported".into());
                }
                let library = private_gdextension_quoted(value)?;
                private_gdextension_parts(library.strip_prefix("res://").unwrap_or(&library))?;
                if flags
                    .iter()
                    .all(|flag| PRIVATE_GDEXTENSION_FEATURES.contains(flag))
                    && selected.replace(library).is_some()
                {
                    return Err("external_private_binding_ambiguous_library".into());
                }
            }
            _ => return Err("external_private_binding_variant_unsupported".into()),
        }
    }
    let selected = selected.ok_or("external_private_binding_library_missing")?;
    let mapped = crate::project::gdextension_library_mapping(text, &PRIVATE_GDEXTENSION_FEATURES)?;
    if mapped.as_deref() != Some(selected.as_str()) {
        return Err("external_private_binding_parser_selection_mismatch".into());
    }
    if !Path::new(&selected)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
    {
        return Err("external_private_binding_library_kind_unsupported".into());
    }
    Ok((
        selected,
        entry_symbol.ok_or("external_private_binding_entry_symbol_missing")?,
    ))
}

pub(crate) fn configfile_parser_document(text: &str) -> Result<serde_json::Value, String> {
    // This is an admission barrier before ConfigFile can interpret any Variant.
    // Keep the supported subset and dependency refusal identical to the plan.
    private_gdextension_mapping(text)?;
    let mut sections: Vec<serde_json::Value> = Vec::new();
    let mut libraries = Vec::new();
    for raw in text.lines() {
        let line = private_gdextension_line(raw)?;
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            sections.push(serde_json::json!({"name":&line[1..line.len()-1],"entries":[]}));
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("configfile_parser_document")?;
        let key = key.trim();
        let key = if key.starts_with('"') {
            private_gdextension_quoted(key)?
        } else {
            key.into()
        };
        let value = value.trim();
        let (kind, value) = if matches!(value, "true" | "false") {
            ("bool", serde_json::Value::Bool(value == "true"))
        } else {
            (
                "string",
                serde_json::Value::String(private_gdextension_quoted(value)?),
            )
        };
        let section = sections.last_mut().ok_or("configfile_parser_document")?;
        section["entries"]
            .as_array_mut()
            .ok_or("configfile_parser_document")?
            .push(serde_json::json!({"key":key,"type":kind,"value":value}));
        if section["name"] == "libraries" {
            let flags: Vec<_> = key.split('.').collect();
            libraries.push(serde_json::json!({"key":key,"flags":flags,"path":value,
                "matches_target":flags.iter().all(|flag| PRIVATE_GDEXTENSION_FEATURES.contains(flag))}));
        }
    }
    Ok(serde_json::json!({"sections":sections,"libraries":libraries,"dependencies":[]}))
}

fn private_gdextension_library_relative(
    manifest: &str,
    project: &str,
    library: &str,
) -> Result<String, String> {
    let manifest = private_gdextension_parts(manifest)?;
    let project = private_gdextension_parts(project)?;
    let godot = &project[..project.len() - 1];
    let mut result = if library.starts_with("res://") {
        godot.to_vec()
    } else {
        manifest[..manifest.len() - 1].to_vec()
    };
    result.extend(private_gdextension_parts(
        library.strip_prefix("res://").unwrap_or(library),
    )?);
    if !result.starts_with(godot) {
        return Err("external_private_binding_unsafe_path".into());
    }
    let result = result.join("/");
    private_gdextension_parts(&result)?;
    Ok(result)
}

struct PrivateArtifactPlacement {
    slot: crate::candidate_workspace::PrivateArtifactSlot,
    writer: Option<File>,
    expected: Option<Stamp>,
    complete: bool,
}

fn private_artifact_admission(bytes: u64, used: u64, budget: &Budget<'_>) -> Result<(), String> {
    budget.check()?;
    if bytes == 0
        || bytes > budget.max_file
        || bytes > budget.max_bytes
        || budget.max_files == 0
        || budget.max_entries == 0
        || used
            .checked_add(bytes)
            .is_none_or(|total| total > crate::candidate_workspace::WORKSPACE_BYTES)
    {
        return Err("external_private_artifact_storage_admission_refused".into());
    }
    Ok(())
}

fn restore_private_artifact_cursor<T>(
    cursor: &mut File,
    position: u64,
    result: Result<T, String>,
    budget: &Budget<'_>,
) -> Result<T, String> {
    // Restoration is cleanup, not late-work acceptance; attempt it after errors too.
    match (
        result,
        std::io::Seek::seek(cursor, std::io::SeekFrom::Start(position)),
    ) {
        (Ok(value), Ok(_)) => {
            budget.check()?;
            Ok(value)
        }
        (Err(error), Ok(_)) => Err(error),
        (_, Err(_)) => Err("external_private_artifact_cursor_restore_failed".into()),
    }
}

fn check_private_artifact_copy(
    path: &Path,
    writer: &File,
    expected: &Stamp,
    sha256: &str,
    budget: &Budget<'_>,
) -> Result<(), String> {
    budget.check()?;
    if stamp(writer)? != *expected || hash_provenance_input(path, writer, budget)? != sha256 {
        return Err("external_private_artifact_changed".into());
    }
    budget.check()
}

fn copy_checked_private_artifact(
    source_path: &Path,
    source: &File,
    before: &Stamp,
    sha256: &str,
    destination_path: &Path,
    destination: &mut File,
    budget: &Budget<'_>,
) -> Result<Stamp, String> {
    let mut cursor = source
        .try_clone()
        .map_err(|_| "external_private_artifact_cursor_unavailable")?;
    let position = std::io::Seek::stream_position(&mut cursor)
        .map_err(|_| "external_private_artifact_cursor_unavailable")?;
    let result = (|| {
        private_artifact_admission(before.bytes, 0, budget)?;
        if stamp(source)? != *before || stamp(destination)?.bytes != 0 {
            return Err("external_private_artifact_input_changed".into());
        }
        crate::snapshot::check_opened_path(source, source_path, budget.deadline())?;
        crate::snapshot::check_opened_path(destination, destination_path, budget.deadline())?;
        std::io::Seek::seek(&mut cursor, std::io::SeekFrom::Start(0))
            .map_err(|_| "external_private_artifact_seek_failed")?;
        let mut digest = Sha256::new();
        let mut copied = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            budget.check()?;
            let capacity = before
                .bytes
                .saturating_sub(copied)
                .saturating_add(1)
                .min(buffer.len() as u64) as usize;
            let count = cursor
                .read(&mut buffer[..capacity])
                .map_err(|_| "external_private_artifact_read_failed")?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or("external_private_artifact_input_changed")?;
            if copied > before.bytes {
                return Err("external_private_artifact_input_changed".into());
            }
            budget.check()?;
            destination
                .write_all(&buffer[..count])
                .map_err(|_| "external_private_artifact_write_failed")?;
            digest.update(&buffer[..count]);
        }
        if copied != before.bytes || digest.hex() != sha256 || stamp(source)? != *before {
            return Err("external_private_artifact_input_changed".into());
        }
        budget.check()?;
        destination
            .flush()
            .map_err(|_| "external_private_artifact_flush_failed")?;
        budget.check()?;
        destination
            .sync_all()
            .map_err(|_| "external_private_artifact_sync_failed")?;
        budget.check()?;
        // Same original writer supplies readback, identity and final byte evidence.
        if hash_provenance_input(destination_path, destination, budget)? != sha256 {
            return Err("external_private_artifact_readback_changed".into());
        }
        crate::snapshot::check_opened_path(source, source_path, budget.deadline())?;
        if stamp(source)? != *before {
            return Err("external_private_artifact_input_changed".into());
        }
        budget.check()?;
        stamp(destination)
    })();
    restore_private_artifact_cursor(&mut cursor, position, result, budget)
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProposalLimits {
    bytes: u64,
    file: u64,
    files: usize,
    entries: usize,
}
impl ProposalLimits {
    pub(crate) fn from_budget(budget: &Budget<'_>) -> Self {
        Self {
            bytes: budget.max_bytes,
            file: budget.max_file,
            files: budget.max_files,
            entries: budget.max_entries,
        }
    }
    fn clipped<'a>(
        &self,
        parent: &crate::external_native::ParentDeadline,
        check: &'a dyn Fn() -> Result<(), String>,
    ) -> Result<Budget<'a>, String> {
        let budget = parent.source_budget(check)?;
        Ok(self.restrict(&budget))
    }
    fn check_not_wider(&self, budget: &Budget<'_>) -> Result<(), String> {
        if budget.max_bytes > self.bytes
            || budget.max_file > self.file
            || budget.max_files > self.files
            || budget.max_entries > self.entries
        {
            return Err("external_source_original_proposal_limits_widened".into());
        }
        Ok(())
    }
    pub(crate) fn restrict<'a>(&self, budget: &Budget<'a>) -> Budget<'a> {
        Budget {
            deadline: budget.deadline,
            check_cancelled: budget.check_cancelled,
            max_bytes: budget.max_bytes.min(self.bytes),
            max_file: budget.max_file.min(self.file),
            max_files: budget.max_files.min(self.files),
            max_entries: budget.max_entries.min(self.entries),
        }
    }
}
pub(crate) struct SourceAdmission {
    pub(crate) root: PathBuf,
    pub(crate) changes: Vec<SourceBytes>,
    pub(crate) binding: crate::transactions::ExternalBinding,
    _leases: Vec<ContentLock>,
    _permit: SourcePublicationPermit,
    _candidate: ValidatedSourceCandidate,
}
impl SourceAdmission {
    pub(crate) fn observations(&self) -> &[(String, Option<ExternalObject>)] {
        &self._permit.prepared.observations
    }
    pub(crate) fn check_authority(&self, budget: &Budget<'_>) -> Result<(), String> {
        self._permit.session.check(budget)?;
        let check = || {
            budget.check()?;
            self._permit.session.check_authority()
        };
        let checked = budget.with_check(&check);
        let budget = if self._permit.session.source_limits.is_some() {
            &checked
        } else {
            budget
        };
        self._permit
            .prepared
            .grant
            .recheck(&self._permit.prepared.grant.project.revision, budget)?;
        self._permit.prepared.guards.check(budget)?;
        let reader = ExternalReadAccess::new(&self._permit.prepared.grant, budget)?;
        if crate::transactions::planned_v3_cleanup(&self.binding, &|path| {
            crate::transactions::ExternalReadIo::source_directory(&reader, path)
        })? != self.binding.cleanup_dirs
        {
            return Err("external_source_planned_directory_presence_changed".into());
        }
        Ok(())
    }
    // No caller or test reaches this yet; it belongs to the unconnected publication surface.
    #[allow(dead_code)]
    pub(crate) fn directory_authority<'a>(
        &'a self,
        budget: &'a Budget<'a>,
    ) -> SourceDirectoryAuthority<'a> {
        SourceDirectoryAuthority {
            root: &self.root,
            scope: &self.binding.cleanup_dirs,
            guards: &self._permit.prepared.guards,
            budget,
        }
    }
}
impl PreparedSourcePublication {
    fn overlay_private(
        &self,
        mut staged: StagedSources,
        budget: &Budget<'_>,
        native_parent_origin: Option<crate::external_native::NativeParentOrigin>,
    ) -> Result<SourceProposal, String> {
        budget.check()?;
        check_originals(&self.roots, &staged, budget)?;
        let target_index = staged
            .roots
            .iter()
            .position(|root| root.root_id == self.grant.id)
            .ok_or("source_proposal_root_missing")?;
        let target_root = staged.roots[target_index].staged.clone();
        let guards = std::sync::Arc::get_mut(&mut staged.directory_guards)
            .ok_or("private_overlay_directory_guards_aliased")?;
        for (path, before, after) in &self.changes {
            check_private_overlay_guards(guards, budget)?;
            if source_text(&target_root, path, budget)? != *before {
                return Err("source_proposal_overlay_stale".into());
            }
            ensure_private_overlay_parent(
                &target_root,
                directory_of(path),
                after.is_some(),
                guards,
                budget,
            )?;
            private_overlay_leaf(
                &target_root.join(path),
                before.as_deref(),
                after.as_deref(),
                budget,
            )?;
            check_private_overlay_guards(guards, budget)?;
        }
        let fingerprint = scan(&target_root, budget, &mut Usage::default())?.1;
        if fingerprint != self.proposed {
            return Err("source_proposal_fingerprint_mismatch".into());
        }
        staged.roots[target_index].proposed_fingerprint = Some(fingerprint);
        check_proposed(&staged, budget)?;
        budget.check()?;
        Ok(SourceProposal {
            staged,
            digest: self.binding.proposal_digest.clone(),
            limits: ProposalLimits::from_budget(budget),
            native_parent_origin,
        })
    }
}
impl SourcePublicationPermit {
    pub(crate) fn admit_prevalidated(
        mut self,
        budget: &Budget<'_>,
    ) -> Result<SourceAdmission, String> {
        self.session.check(budget)?;
        if !self.prepared.private_validation_pending
            || self.prepared.native_validation_tag.is_none()
        {
            return Err("external_source_private_candidate_binding_state".into());
        }
        let candidate = self
            .prepared
            .native_candidate
            .take()
            .ok_or("external_source_private_native_candidate_missing")?;
        self.admit(candidate, budget)
    }
    pub(crate) fn stage(
        &self,
        plan: &StagingPlan,
        envelope: &crate::candidate_workspace::SourceEnvelope<'_>,
        budget: &Budget<'_>,
    ) -> Result<SourceProposal, String> {
        self.session.check(budget)?;
        let staged = plan.stage_owned(envelope, budget)?;
        self.overlay(staged, budget)
    }
    fn overlay(
        &self,
        staged: StagedSources,
        budget: &Budget<'_>,
    ) -> Result<SourceProposal, String> {
        self.session.check(budget)?;
        if self.prepared.private_validation_pending {
            return Err("external_source_bound_native_candidate_requires_direct_admission".into());
        }
        let proposal = self.prepared.overlay_private(staged, budget, None)?;
        self.session.check(budget)?;
        Ok(proposal)
    }
    pub(crate) fn admit(
        self,
        candidate: ValidatedSourceCandidate,
        budget: &Budget<'_>,
    ) -> Result<SourceAdmission, String> {
        let leases = {
            if candidate.proposal.native_parent_origin.is_some() {
                self.session.check(budget)?;
            }
            let check = || {
                budget.check()?;
                self.session.check_authority()
            };
            let checked = budget.with_check(&check);
            let restricted = candidate.proposal.limits.restrict(&checked);
            let budget = if candidate.proposal.native_parent_origin.is_some() {
                &restricted
            } else {
                budget
            };
            self.session.check(budget)?;
            if self.prepared.native_candidate.is_some() {
                return Err(
                    "external_source_bound_native_candidate_requires_direct_admission".into(),
                );
            }
            match (
                self.prepared.native_validation_tag.as_ref(),
                candidate.native_validation_tag.as_ref(),
                candidate.proposal.native_parent_origin.is_some(),
            ) {
                (None, None, false) if !self.prepared.private_validation_pending => {}
                (Some(expected), Some(actual), true)
                    if self.prepared.private_validation_pending && expected == actual =>
                {
                    self.prepared
                        .native_confirmation_window
                        .as_ref()
                        .ok_or("external_source_postvalidation_confirmation_required")?
                        .check(budget)?;
                    self.prepared.recheck_unconfirmed_originals(budget)?;
                }
                _ => return Err("external_source_postvalidation_candidate_tag_mismatch".into()),
            }
            if candidate.proposal.digest != self.prepared.binding.proposal_digest
                || !candidate
                    .ownership
                    .matches(&candidate.workspace, &candidate.proposal.staged.envelope)
            {
                return Err("external_source_validated_candidate_binding_mismatch".into());
            }
            let leases = source_leases(
                &self
                    .prepared
                    .roots
                    .iter()
                    .map(|(root, _)| root.clone())
                    .chain(self.prepared.unused_roots.iter().cloned())
                    .collect::<Vec<_>>(),
                &self.prepared.grant.id,
                false,
                budget,
            )?;
            self.prepared
                .grant
                .recheck(&self.prepared.grant.project.revision, budget)?;
            self.prepared.guards.check(budget)?;
            for root in &self.prepared.unused_roots {
                let (path, identity) = canonical_directory(&root.source, budget)?;
                if path != root.source || identity != root.identity {
                    return Err("unused_granted_root_identity_changed".into());
                }
            }
            check_originals(&self.prepared.roots, &candidate.proposal.staged, budget)?;
            check_proposed(&candidate.proposal.staged, budget)?;
            candidate.compiler_provenance.recheck(
                &candidate.proposal.staged,
                &candidate.artifact,
                &candidate.artifact_hash,
                budget,
            )?;
            let target = candidate
                .proposal
                .staged
                .roots
                .iter()
                .find(|root| root.root_id == self.prepared.grant.id)
                .ok_or("source_proposal_root_missing")?;
            if target.proposed_fingerprint.as_ref() != Some(&self.prepared.proposed) {
                return Err("source_proposal_fingerprint_mismatch".into());
            }
            for (path, before, after) in &self.prepared.changes {
                if source_text(&target.source, path, budget)? != *before
                    || source_text(&target.staged, path, budget)? != *after
                {
                    return Err("external_source_target_hash_changed".into());
                }
            }
            let current = ExternalReadAccess::new(&self.prepared.grant, budget)?;
            for (path, expected) in &self.prepared.observations {
                if crate::transactions::ExternalReadIo::target(&current, path)?.map(|v| v.0)
                    != *expected
                {
                    return Err("external_source_target_identity_changed".into());
                }
            }
            drop(current);
            if hash_validated_artifact(&candidate.artifact, &candidate.artifact_guard, budget)?
                != candidate.artifact_hash
            {
                return Err("external_source_validated_artifact_changed".into());
            }
            self.session.check(budget)?;
            leases
        };
        Ok(SourceAdmission {
            root: self.prepared.grant.root.clone(),
            changes: self.prepared.changes.clone(),
            binding: self.prepared.binding.clone(),
            _leases: leases,
            _permit: self,
            _candidate: candidate,
        })
    }
}
fn check_private_overlay_guards(
    guards: &StagedDirectoryGuards,
    budget: &Budget<'_>,
) -> Result<(), String> {
    for (path, identity, guard) in &guards.0 {
        budget.check()?;
        crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
        if &file_identity(guard)? != identity {
            return Err("private_overlay_directory_identity_changed".into());
        }
    }
    Ok(())
}
fn ensure_private_overlay_parent(
    root: &Path,
    relative_parent: &str,
    create: bool,
    guards: &mut StagedDirectoryGuards,
    budget: &Budget<'_>,
) -> Result<(), String> {
    let mut current = root.to_path_buf();
    if !guards.0.iter().any(|(path, _, _)| path == root) {
        return Err("private_overlay_root_guard_missing".into());
    }
    let parts = if relative_parent.is_empty() {
        Vec::new()
    } else {
        relative(relative_parent, false)?
    };
    for part in parts {
        check_private_overlay_guards(guards, budget)?;
        let next = current.join(part);
        if !guards.0.iter().any(|(path, _, _)| *path == next) {
            if !create {
                return Err("private_overlay_parent_guard_missing".into());
            }
            let (_, _, parent) = guards
                .0
                .iter()
                .find(|(path, _, _)| *path == current)
                .ok_or("private_overlay_parent_guard_missing")?;
            // FILE_CREATE both proves new ownership and returns its immutable
            // guard. An unheld existing directory is never adopted.
            let guard = create_owned_directory(parent, part, false)?;
            crate::snapshot::check_opened_path(&guard, &next, budget.deadline)?;
            let identity = file_identity(&guard)?;
            if identity.created.is_none() {
                return Err("private_overlay_created_identity_unavailable".into());
            }
            guards.0.push((next.clone(), identity, guard));
        }
        current = next;
    }
    check_private_overlay_guards(guards, budget)
}
fn private_overlay_read(file: &mut File, budget: &Budget<'_>) -> Result<Vec<u8>, String> {
    use std::io::{Seek, SeekFrom};
    let before = stamp(file)?;
    if before.bytes > crate::revisions::MAX_FILE_BYTES as u64 {
        return Err("private_overlay_leaf_byte_limit".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "private_overlay_seek_failed")?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        budget.check()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| "private_overlay_read_failed")?;
        if count == 0 {
            break;
        }
        if bytes
            .len()
            .checked_add(count)
            .is_none_or(|length| length > crate::revisions::MAX_FILE_BYTES)
        {
            return Err("private_overlay_leaf_byte_limit".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.len() as u64 != before.bytes || stamp(file)? != before {
        return Err("private_overlay_leaf_changed".into());
    }
    Ok(bytes)
}
fn private_overlay_leaf(
    path: &Path,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    budget: &Budget<'_>,
) -> Result<(), String> {
    use std::io::{Seek, SeekFrom};
    use std::os::windows::fs::OpenOptionsExt;
    if before.is_none() && after.is_none() {
        return Err("private_overlay_empty_action".into());
    }
    if before.is_some_and(|bytes| bytes.len() > crate::revisions::MAX_FILE_BYTES)
        || after.is_some_and(|bytes| bytes.len() > crate::revisions::MAX_FILE_BYTES)
    {
        return Err("private_overlay_leaf_byte_limit".into());
    }
    let expected = if before.is_some() {
        let checked = crate::snapshot::open_checked_file(path, budget.deadline)?;
        Some(stamp(&checked)?)
    } else {
        None
    };
    let mut options = OpenOptions::new();
    options.read(true).share_mode(0).custom_flags(0x0020_0000);
    if after.is_some() {
        options.write(true);
    } else {
        options.access_mode(0x0011_0081);
    }
    if before.is_none() {
        options.create_new(true);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "private_overlay_exclusive_leaf_refused")?;
    crate::snapshot::check_opened_path(&file, path, budget.deadline)?;
    let identity = file_identity(&file)?;
    let metadata = file
        .metadata()
        .map_err(|_| "private_overlay_leaf_metadata_unavailable")?;
    ordinary(&metadata)?;
    if !metadata.is_file() {
        return Err("private_overlay_regular_leaf_required".into());
    }
    if let Some(expected) = expected {
        if stamp(&file)? != expected {
            return Err("private_overlay_leaf_identity_changed".into());
        }
        if private_overlay_read(&mut file, budget)?.as_slice()
            != before.ok_or("private_overlay_before_missing")?
        {
            return Err("private_overlay_original_bytes_changed".into());
        }
    } else if metadata.len() != 0 {
        return Err("private_overlay_new_leaf_not_empty".into());
    }
    if let Some(bytes) = after {
        // Only the private unpublished candidate is edited in place. Verify
        // original bytes first; interruption leaves it retained and unvalidated.
        budget.check()?;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "private_overlay_seek_failed")?;
        file.set_len(0)
            .map_err(|_| "private_overlay_truncate_failed")?;
        for chunk in bytes.chunks(64 * 1024) {
            budget.check()?;
            file.write_all(chunk)
                .map_err(|_| "private_overlay_write_failed")?;
        }
        file.sync_all().map_err(|_| "private_overlay_sync_failed")?;
        if file_identity(&file)? != identity
            || private_overlay_read(&mut file, budget)?.as_slice() != bytes
        {
            return Err("private_overlay_written_bytes_changed".into());
        }
        crate::snapshot::check_opened_path(&file, path, budget.deadline)?;
    } else {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Disposition {
            delete_file: u8,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn SetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                class: u32,
                information: *mut std::ffi::c_void,
                bytes: u32,
            ) -> i32;
        }
        let mut disposition = Disposition { delete_file: 1 };
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                4,
                (&mut disposition as *mut Disposition).cast(),
                std::mem::size_of::<Disposition>() as u32,
            )
        } == 0
        {
            return Err("private_overlay_checked_delete_refused".into());
        }
    }
    Ok(())
}
fn source_leases(
    roots: &[Root],
    target: &RootId,
    repair: bool,
    budget: &Budget<'_>,
) -> Result<Vec<ContentLock>, String> {
    let mut ordered: Vec<_> = roots.iter().collect();
    ordered.sort_by_key(|root| (key(&root.source), root.identity.clone()));
    let mut leases = Vec::new();
    let mut seen = BTreeSet::new();
    for root in ordered {
        if !seen.insert(key(&root.source)) {
            return Err("external_source_lease_alias".into());
        }
        let (path, identity) = canonical_directory(&root.source, budget)?;
        if path != root.source || identity != root.identity {
            return Err("external_source_lease_identity_changed".into());
        }
        leases.push(if &root.id == target {
            ContentLock::exclusive(&root.source)?
        } else {
            ContentLock::shared(&root.source)?
        });
        if !repair || &root.id != target {
            namespace_io::check_root_barrier(&root.source, &root.identity, budget)?;
        }
    }
    Ok(leases)
}
fn check_originals(
    roots: &[(Root, Fingerprint)],
    staged: &StagedSources,
    budget: &Budget<'_>,
) -> Result<(), String> {
    if roots.len() != staged.roots.len() {
        return Err("external_source_candidate_root_count_mismatch".into());
    }
    let mut usage = Usage::default();
    for (original, fingerprint) in roots {
        let root = staged
            .roots
            .iter()
            .find(|root| root.root_id == original.id)
            .ok_or("external_source_candidate_root_mismatch")?;
        if root.source != original.source
            || root.source_identity != original.identity
            || root.fingerprint != *fingerprint
            || canonical_directory(&root.source, budget)?.1 != original.identity
            || scan(&root.source, budget, &mut usage)?.1 != *fingerprint
        {
            return Err("external_source_original_fingerprint_changed".into());
        }
    }
    Ok(())
}
fn check_proposed(staged: &StagedSources, budget: &Budget<'_>) -> Result<(), String> {
    if canonical_directory(&staged.envelope, budget)?.1 != staged.envelope_identity {
        return Err("external_source_candidate_identity_changed".into());
    }
    for (path, identity, guard) in &staged.directory_guards.0 {
        crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
        if &file_identity(guard)? != identity {
            return Err("external_source_candidate_identity_changed".into());
        }
    }
    let mut usage = Usage::default();
    for root in &staged.roots {
        if canonical_directory(&root.staged, budget)?.1 != root.staged_identity
            || &scan(&root.staged, budget, &mut usage)?.1
                != root
                    .proposed_fingerprint
                    .as_ref()
                    .unwrap_or(&root.fingerprint)
        {
            return Err("external_source_proposed_fingerprint_changed".into());
        }
    }
    Ok(())
}
impl SourceProposal {
    pub(crate) fn validate_read_with_receipt(
        self,
        workspace: &mut crate::candidate_workspace::Workspace,
        controller: &crate::worker_windows::StreamController,
        parent: &crate::external_native::ParentDeadline,
    ) -> Result<(ValidatedSourceCandidate, serde_json::Value), String> {
        parent.check_read_controller(controller)?;
        if self.native_parent_origin.as_ref() != Some(parent.origin()) {
            return Err("external_source_original_native_parent_required".into());
        }
        let check = || parent.check_read_controller(controller);
        let checked = self.limits.clipped(parent, &check)?;
        check_proposed(&self.staged, &checked)?;
        let validation = crate::external_native::validate_owned_with_parent(
            workspace,
            &self.staged,
            parent,
            controller,
        )?;
        let receipt = validation.receipt().clone();
        parent.check_read_controller(controller)?;
        let checked = self.limits.clipped(parent, &check)?;
        Ok((
            self.seal_validation(workspace, validation, &checked, true)?,
            receipt,
        ))
    }
    /// The authenticated read-validation operation keeps typed custody. This
    /// does not prepare a publication session or alter any original source.
    pub(crate) fn from_read_staging(
        staged: StagedSources,
        parent: &crate::external_native::ParentDeadline,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        parent.check()?;
        check_proposed(&staged, budget)?;
        if staged.originals_mutated || staged.manifest_transformed {
            return Err("configfile_parser_unmodified_read_staging_required".into());
        }
        let rows: Vec<_> = staged
            .roots
            .iter()
            .map(|row| {
                serde_json::json!({
                    "root":row.root_id.as_str(),"fingerprint":row.fingerprint.sha256,
                })
            })
            .collect();
        let digest =
            crate::sha256_hex(&serde_json::to_vec(&rows).map_err(|_| "configfile_parser_digest")?);
        Ok(Self {
            staged,
            digest,
            limits: ProposalLimits::from_budget(budget),
            native_parent_origin: Some(parent.origin().clone()),
        })
    }
    /// Only the retained compiler creates production validation evidence. The
    /// result is sealed after positively resolving its original worker owner.
    // No caller or test reaches this yet; it belongs to the unconnected publication surface.
    #[allow(dead_code)]
    pub(crate) fn validate(
        self,
        workspace: &mut crate::candidate_workspace::Workspace,
        controller: &crate::worker_windows::StreamController,
        budget: &Budget<'_>,
    ) -> Result<ValidatedSourceCandidate, String> {
        budget.check()?;
        if self.native_parent_origin.is_some() {
            return Err("external_source_original_native_parent_required".into());
        }
        let validation = crate::external_native::validate_owned(
            workspace,
            &self.staged,
            budget.deadline,
            controller,
        )?;
        self.seal_validation(workspace, validation, budget, false)
    }
    pub(crate) fn validate_before_publication(
        self,
        workspace: &mut crate::candidate_workspace::Workspace,
        controller: &crate::worker_windows::StreamController,
        parent: &crate::external_native::ParentDeadline,
    ) -> Result<ValidatedSourceCandidate, String> {
        parent.check()?;
        if self.native_parent_origin.as_ref() != Some(parent.origin()) {
            return Err("external_source_original_native_parent_required".into());
        }
        parent.check_read_controller(controller)?;
        let check = || parent.check_read_controller(controller);
        let before = self.limits.clipped(parent, &check)?;
        check_proposed(&self.staged, &before)?;
        let validation = crate::external_native::validate_owned_with_parent(
            workspace,
            &self.staged,
            parent,
            controller,
        )?;
        parent.check_read_controller(controller)?;
        let after = self.limits.clipped(parent, &check)?;
        self.seal_validation(workspace, validation, &after, true)
    }
    #[cfg(test)]
    fn validate_before_publication_diagnostic_for_test(
        self,
        workspace: &mut crate::candidate_workspace::Workspace,
        controller: &crate::worker_windows::StreamController,
        parent: &crate::external_native::ParentDeadline,
        permit: KnownCargoFixturePermit,
        diagnostic: &mut Option<
            std::mem::ManuallyDrop<
                crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle,
            >,
        >,
    ) -> Result<ValidatedSourceCandidate, String> {
        parent.check()?;
        if self.native_parent_origin.as_ref() != Some(parent.origin()) {
            return Err("external_source_original_native_parent_required".into());
        }
        parent.check_read_controller(controller)?;
        let check = || parent.check_read_controller(controller);
        let before = self.limits.clipped(parent, &check)?;
        check_proposed(&self.staged, &before)?;
        let validation = crate::external_native::validate_owned_with_parent_diagnostic_for_test(
            workspace,
            &self.staged,
            parent,
            controller,
            permit,
            diagnostic,
        )?;
        parent.check_read_controller(controller)?;
        let after = self.limits.clipped(parent, &check)?;
        self.seal_validation(workspace, validation, &after, true)
    }
    fn seal_validation(
        self,
        workspace: &mut crate::candidate_workspace::Workspace,
        validation: crate::external_native::OwnedNativeValidation,
        budget: &Budget<'_>,
        before_publication: bool,
    ) -> Result<ValidatedSourceCandidate, String> {
        budget.check()?;
        let (result, artifact, artifact_guard, artifact_hash, compiler_provenance) =
            validation.into_parts();
        if result["native_artifact_produced"] != true || result["cargo_executed"] != true {
            return Err("external_source_native_validation_missing".into());
        }
        let compiler_provenance = CandidateCompilerProvenance::Real(
            compiler_provenance.ok_or("external_source_real_compiler_provenance_missing")?,
        );
        compiler_provenance.recheck(&self.staged, &artifact, &artifact_hash, budget)?;
        let relative_artifact = result["artifact"]["path"]
            .as_str()
            .ok_or("external_source_artifact_missing")?;
        relative(relative_artifact, false)?;
        if workspace.path.join(relative_artifact) != artifact
            || result["artifact"]["sha256"].as_str() != Some(artifact_hash.as_str())
        {
            return Err("external_source_artifact_receipt_mismatch".into());
        }
        let ownership = workspace.retire_compilation_for_publication(budget)?;
        if hash_validated_artifact(&artifact, &artifact_guard, budget)? != artifact_hash {
            return Err("external_source_validated_artifact_changed".into());
        }
        check_proposed(&self.staged, budget)?;
        compiler_provenance.recheck(&self.staged, &artifact, &artifact_hash, budget)?;
        let native_validation_tag = if before_publication {
            let parent_origin = self
                .native_parent_origin
                .as_ref()
                .ok_or("external_source_original_native_parent_required")?;
            if !ownership.matches(&workspace.path, &self.staged.envelope) {
                return Err("external_source_compilation_retirement_binding_mismatch".into());
            }
            let nonce = crate::random::bytes(32);
            if nonce.source != crate::random::Source::OperatingSystem {
                return Err("external_source_postvalidation_OS_nonce_required".into());
            }
            let nonce: String = nonce
                .bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            Some(crate::sha256_hex(
                format!(
                    "private-edited-native-result/2|{nonce}|{}|{parent_origin:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{}",
                    self.digest,
                    workspace.path,
                    self.staged.envelope,
                    self.staged.envelope_identity,
                    artifact,
                    file_identity(&artifact_guard)?,
                    artifact_hash,
                    self.limits,
                    compiler_provenance.real_digest()?,
                )
                .as_bytes(),
            ))
        } else {
            None
        };
        budget.check()?;
        Ok(ValidatedSourceCandidate {
            proposal: self,
            ownership,
            workspace: workspace.path.clone(),
            artifact,
            artifact_hash,
            artifact_guard,
            native_validation_tag,
            compiler_provenance,
        })
    }
}

pub(crate) struct PreparedSourceRepair {
    grant: PreparedGrant,
    scope: crate::transactions::ExternalRepairScope,
    reverse_cleanup: Vec<String>,
    // No caller or test reaches this yet; it belongs to the unconnected publication surface.
    #[allow(dead_code)]
    cleanup_observations: Vec<(String, Option<String>)>,
    revision: String,
    undo: bool,
    digest: String,
    guards: SourceGuards,
    v3: crate::transactions::ExternalV3Snapshot,
    imported_observation: Fingerprint,
}
pub(crate) struct SourceRepairPermit {
    prepared: PreparedSourceRepair,
    session: SourceSession,
}
pub(crate) struct SourceRepairAdmission {
    pub(crate) root: PathBuf,
    pub(crate) revision: String,
    pub(crate) undo: bool,
    pub(crate) scope: crate::transactions::ExternalRepairScope,
    pub(crate) reverse_cleanup: Vec<String>,
    pub(crate) authorization_digest: String,
    _leases: Vec<ContentLock>,
    _permit: SourceRepairPermit,
}
impl SourceRepairAdmission {
    pub(crate) fn snapshot(&self) -> &crate::transactions::ExternalV3Snapshot {
        &self._permit.prepared.v3
    }
    pub(crate) fn check_authority(&self, budget: &Budget<'_>) -> Result<(), String> {
        self._permit.session.check(budget)?;
        self._permit
            .prepared
            .grant
            .recheck(&self._permit.prepared.grant.project.revision, budget)?;
        let prepared = &self._permit.prepared;
        prepared.guards.check(budget)?;
        if scan(&prepared.grant.project.root, budget, &mut Usage::default())?.1
            != prepared.imported_observation
        {
            return Err("external_source_repair_imported_bytes_changed".into());
        }
        let reader = ExternalReadAccess::new(&prepared.grant, budget)?;
        let snapshot =
            crate::transactions::inspect_external_v3(&reader, &prepared.revision, prepared.undo)?;
        if snapshot.digest() != prepared.v3.digest() {
            return Err("external_source_v3_repair_evidence_changed".into());
        }
        if self.undo {
            let mut reverse = self.scope.binding.clone();
            for target in &mut reverse.targets {
                std::mem::swap(&mut target.before, &mut target.after);
            }
            reverse.cleanup_dirs.clear();
            if crate::transactions::planned_v3_directory_cleanup(
                &reverse,
                &prepared.v3.directory_scope(true).1,
                &|path| crate::transactions::ExternalReadIo::source_directory(&reader, path),
            )? != self.reverse_cleanup
            {
                return Err("external_source_reverse_directory_presence_changed".into());
            }
        }
        Ok(())
    }
    // No caller or test reaches this yet; it belongs to the unconnected publication surface.
    #[allow(dead_code)]
    pub(crate) fn directory_authority<'a>(
        &'a self,
        budget: &'a Budget<'a>,
    ) -> SourceDirectoryAuthority<'a> {
        SourceDirectoryAuthority {
            root: &self.root,
            scope: if self.undo {
                &self.reverse_cleanup
            } else {
                &self.scope.binding.cleanup_dirs
            },
            guards: &self._permit.prepared.guards,
            budget,
        }
    }
}
impl PreparedSourceRepair {
    pub(crate) fn prepare(
        project: &ImportedProject,
        root_id: RootId,
        requested: &Path,
        revision: &str,
        undo: bool,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        if root_id.as_str() == "project" {
            return Err("external_source_root_required".into());
        }
        let grant =
            PreparedGrant::prepare(project, root_id, requested, Mode::SourcePublish, budget)?;
        let _leases = source_leases(&repair_roots(&grant)?, &grant.id, true, budget)?;
        let imported_observation = scan(&grant.project.root, budget, &mut Usage::default())?.1;
        let reader = ExternalReadAccess::new(&grant, budget)?;
        let v3 = crate::transactions::inspect_external_v3(&reader, revision, undo)?;
        let scope = v3.scope()?;
        let binding = &scope.binding;
        if binding.external_path != grant.root.to_string_lossy()
            || binding.external_identity != identity_digest(&grant.identity)
            || binding.imported_path != project.root.to_string_lossy()
            || binding.imported_identity != identity_digest(&project.identity)
            || binding.root_id != grant.id.as_str()
        {
            return Err("external_source_repair_identity_mismatch".into());
        }
        for target in &binding.targets {
            source_target(&binding.member_src, &target.path)?;
        }
        let (retire, origins) = v3.directory_scope(undo);
        let guards = SourceGuards::capture_with_directory_operations(
            &grant.root,
            binding.targets.iter().map(|target| target.path.clone()),
            retire
                .into_iter()
                .map(|(path, _)| path)
                .chain(origins.into_iter().map(|origin| origin.path)),
            budget,
        )?;
        let cleanup_observations =
            guards.observations(&grant.root, &binding.cleanup_dirs, budget)?;
        for (path, observed) in &cleanup_observations {
            let owned = scope.owned_dirs.iter().find(|owned| &owned.path == path);
            match (owned, observed) {
                (Some(owned), Some(identity)) if &owned.identity == identity => {}
                (_, None) => {}
                _ => return Err("external_source_cleanup_ownership_mismatch".into()),
            }
        }
        let reverse_cleanup = if undo {
            let mut reversed = binding.clone();
            for target in &mut reversed.targets {
                std::mem::swap(&mut target.before, &mut target.after);
            }
            reversed.cleanup_dirs.clear();
            crate::transactions::planned_v3_directory_cleanup(
                &reversed,
                &v3.directory_scope(true).1,
                &|path| crate::transactions::ExternalReadIo::source_directory(&reader, path),
            )?
        } else {
            Vec::new()
        };
        let digest = crate::sha256_hex(
            format!(
                "owner-external-source-repair/3|{}|{revision}|{undo}|{}|{:?}|{:?}|{}|{:?}",
                grant.digest(),
                serde_json::to_string(&scope).map_err(|error| error.to_string())?,
                cleanup_observations,
                reverse_cleanup,
                v3.digest(),
                imported_observation
            )
            .as_bytes(),
        );
        drop(reader);
        Ok(Self {
            grant,
            scope,
            reverse_cleanup,
            cleanup_observations,
            revision: revision.into(),
            undo,
            digest,
            guards,
            v3,
            imported_observation,
        })
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn finish(
        self,
        confirmation: crate::workspace_access::OwnerConfirmation,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        budget: &Budget<'_>,
    ) -> Result<SourceRepairPermit, String> {
        if !confirmation.matches(&self.digest) {
            return Err("owner_source_repair_confirmation_binding_mismatch".into());
        }
        self.grant.recheck(&self.grant.project.revision, budget)?;
        let session = SourceSession {
            broker: std::sync::Arc::downgrade(broker),
            cancelled,
            expires: budget.deadline,
            publication_operation: None,
            source_limits: None,
        };
        session.check(budget)?;
        Ok(SourceRepairPermit {
            prepared: self,
            session,
        })
    }
}
impl SourceRepairPermit {
    pub(crate) fn admit(self, budget: &Budget<'_>) -> Result<SourceRepairAdmission, String> {
        self.session.check(budget)?;
        let grant = &self.prepared.grant;
        let roots = repair_roots(grant)?;
        let leases = source_leases(&roots, &grant.id, true, budget)?;
        grant.recheck(&grant.project.revision, budget)?;
        self.prepared.guards.check(budget)?;
        if scan(&grant.project.root, budget, &mut Usage::default())?.1
            != self.prepared.imported_observation
        {
            return Err("external_source_repair_imported_bytes_changed".into());
        }
        let reader = ExternalReadAccess::new(grant, budget)?;
        let snapshot = crate::transactions::inspect_external_v3(
            &reader,
            &self.prepared.revision,
            self.prepared.undo,
        )?;
        if snapshot.digest() != self.prepared.v3.digest() {
            return Err("external_source_v3_repair_evidence_changed".into());
        }
        drop(reader);
        self.session.check(budget)?;
        Ok(SourceRepairAdmission {
            root: grant.root.clone(),
            revision: self.prepared.revision.clone(),
            undo: self.prepared.undo,
            scope: self.prepared.scope.clone(),
            reverse_cleanup: self.prepared.reverse_cleanup.clone(),
            authorization_digest: self.prepared.digest.clone(),
            _leases: leases,
            _permit: self,
        })
    }
}
fn repair_roots(grant: &PreparedGrant) -> Result<Vec<Root>, String> {
    Ok(vec![
        Root {
            id: RootId::new("project")?,
            source: grant.project.root.clone(),
            identity: grant.project.identity.clone(),
            mode: Mode::DependencyRead,
            slot: String::new(),
        },
        Root {
            id: grant.id.clone(),
            source: grant.root.clone(),
            identity: grant.identity.clone(),
            mode: Mode::SourcePublish,
            slot: String::new(),
        },
    ])
}
#[cfg(test)]
pub(crate) fn issue_grant(
    authority: &dyn OwnerAuthority,
    project: &ImportedProject,
    id: RootId,
    requested: &Path,
    mode: Mode,
    budget: &Budget<'_>,
) -> Result<VerifiedGrant, String> {
    // Test-only authority preserves earlier staging coverage; production has no callback issuer.
    let prepared = PreparedGrant::prepare(project, id, requested, mode, budget)?;
    if !authority.approve(&ApprovalRequest {
        imported_project: &project.root,
        requested_root: prepared.canonical_root(),
        mode,
        revision: &project.revision,
    })? {
        return Err("trusted_owner_approval_required".into());
    }
    prepared.recheck(&project.revision, budget)?;
    Ok(prepared.into_grant())
}
#[derive(Clone, Debug)]
struct Root {
    id: RootId,
    source: PathBuf,
    identity: DirectoryIdentity,
    mode: Mode,
    slot: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Placement {
    pub(crate) root_id: RootId,
    pub(crate) slot: String,
}
#[derive(Clone, Debug)]
pub(crate) struct ManifestRef {
    pub(crate) root_id: RootId,
    pub(crate) path: String,
}
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub(crate) workspace: ManifestRef,
    pub(crate) member: ManifestRef,
}
pub(crate) struct Budget<'a> {
    deadline: Instant,
    check_cancelled: &'a dyn Fn() -> Result<(), String>,
    max_bytes: u64,
    max_file: u64,
    max_files: usize,
    max_entries: usize,
}
impl<'a> Budget<'a> {
    /// One bounded phase inside an already fixed parent deadline.
    pub(crate) fn clipped(
        parent_deadline: Instant,
        check_cancelled: &'a dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let now = Instant::now();
        if now >= parent_deadline {
            return Err("external_parent_deadline".into());
        }
        let mut budget = Self::new(Duration::from_secs(60), check_cancelled)?;
        budget.deadline = parent_deadline.min(now + Duration::from_secs(60));
        budget.check()?;
        Ok(budget)
    }
    fn with_check<'b>(&self, check_cancelled: &'b dyn Fn() -> Result<(), String>) -> Budget<'b> {
        Budget {
            deadline: self.deadline,
            check_cancelled,
            max_bytes: self.max_bytes,
            max_file: self.max_file,
            max_files: self.max_files,
            max_entries: self.max_entries,
        }
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn new(
        duration: Duration,
        check_cancelled: &'a dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if duration.is_zero() || duration > Duration::from_secs(60) {
            return Err("invalid_staging_deadline".into());
        }
        Ok(Self {
            deadline: Instant::now() + duration,
            check_cancelled,
            max_bytes: MAX_SOURCE_BYTES,
            max_file: MAX_FILE_BYTES,
            max_files: MAX_FILES,
            max_entries: MAX_ENTRIES,
        })
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        (self.check_cancelled)()?;
        if Instant::now() >= self.deadline {
            return Err("external_staging_deadline".into());
        }
        Ok(())
    }
}
fn relative(value: &str, parents: bool) -> Result<Vec<&str>, String> {
    if value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value.contains(':')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err("unsupported_relative_path".into());
    }
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() > 24
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || (!parents && *part == "..")
                || (*part != ".."
                    && !part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
        })
    {
        return Err("unsupported_relative_path".into());
    }
    Ok(parts)
}
fn normalize(base: &str, path: &str) -> Result<String, String> {
    let mut components: Vec<String> = if base.is_empty() {
        Vec::new()
    } else {
        relative(base, false)?
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    for part in relative(path, true)? {
        if part == ".." {
            components.pop().ok_or("relative_layout_escape")?;
        } else {
            components.push(part.into());
        }
    }
    Ok(components.join("/"))
}
fn directory_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}
pub(crate) fn current_revision(root: &Path, budget: &Budget<'_>) -> Result<String, String> {
    let (root, _) = canonical_directory(root, budget)?;
    let _lease = ContentLock::shared(&root)?;
    crate::transactions::ensure_writable(&root)?;
    let (_, fingerprint) = scan(&root, budget, &mut Usage::default())?;
    Ok(fingerprint.sha256)
}
fn public_source(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or("");
    !leaf.eq_ignore_ascii_case("credentials.toml")
        && crate::snapshot::includes(path)
        && !matches!(
            leaf.rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase())
                .as_deref(),
            Some("pem" | "p12" | "pfx" | "jks" | "keystore" | "pkcs12")
        )
}
fn ordinary(metadata: &std::fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err("source_link_refused".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("source_reparse_refused".into());
        }
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err("source_entry_type_refused".into());
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    bytes: u64,
    sha256: String,
    directory: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    pub(crate) sha256: String,
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) entries: usize,
}
#[derive(Default)]
struct Usage {
    bytes: u64,
    files: usize,
    entries: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: DirectoryIdentity,
    bytes: u64,
    modified: Option<SystemTime>,
}
fn stamp(file: &File) -> Result<Stamp, String> {
    let metadata = file.metadata().map_err(|_| "source_metadata_unavailable")?;
    ordinary(&metadata)?;
    Ok(Stamp {
        identity: file_identity(file)?,
        bytes: metadata.len(),
        modified: metadata.modified().ok(),
    })
}
fn hash_validated_artifact(
    path: &Path,
    guard: &File,
    budget: &Budget<'_>,
) -> Result<String, String> {
    budget.check()?;
    crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
    let before = stamp(guard)?;
    if before.bytes > MAX_FILE_BYTES {
        return Err("external_source_artifact_size_refused".into());
    }
    let mut reader = guard;
    std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))
        .map_err(|_| "external_source_artifact_seek_failed")?;
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        budget.check()?;
        let capacity = before
            .bytes
            .saturating_sub(bytes)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let count = reader
            .read(&mut buffer[..capacity])
            .map_err(|_| "external_source_artifact_read_failed")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or("external_source_artifact_size_refused")?;
        if bytes > before.bytes {
            return Err("external_source_validated_artifact_changed".into());
        }
        digest.update(&buffer[..count]);
    }
    if bytes != before.bytes || stamp(guard)? != before {
        return Err("external_source_validated_artifact_changed".into());
    }
    crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
    budget.check()?;
    Ok(digest.hex())
}

#[derive(Clone, Copy)]
enum ReadCapture {
    HashOnly,
    BoundedBytes,
}
fn read_file(
    path: &Path,
    maximum: u64,
    budget: &Budget<'_>,
    destination: Option<&mut File>,
    expected: Option<u64>,
) -> Result<(String, Vec<u8>), String> {
    // Preserve manifest capture and existing streaming/hash-only callers.
    let capture = if maximum == MANIFEST_BYTES {
        ReadCapture::BoundedBytes
    } else {
        ReadCapture::HashOnly
    };
    read_file_with_capture(path, maximum, budget, destination, expected, capture)
}
fn read_file_with_capture(
    path: &Path,
    maximum: u64,
    budget: &Budget<'_>,
    mut destination: Option<&mut File>,
    expected: Option<u64>,
    capture: ReadCapture,
) -> Result<(String, Vec<u8>), String> {
    if destination.is_none()
        && matches!(capture, ReadCapture::BoundedBytes)
        && maximum > crate::revisions::MAX_FILE_BYTES as u64
    {
        return Err("source_capture_byte_limit".into());
    }
    let mut file = crate::snapshot::open_checked_file(path, budget.deadline)?;
    let before = stamp(&file)?;
    if before.bytes > maximum {
        return Err("source_file_byte_limit".into());
    }
    if expected.is_some_and(|bytes| bytes != before.bytes) {
        return Err("source_changed_before_read".into());
    }
    let mut digest = Sha256::new();
    let mut contents = Vec::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        budget.check()?;
        let capacity = (before
            .bytes
            .saturating_sub(bytes)
            .saturating_add(1)
            .min(buffer.len() as u64)) as usize;
        let count = file
            .read(&mut buffer[..capacity])
            .map_err(|_| "source_read_failed")?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or("source_byte_overflow")?;
        if bytes > before.bytes {
            return Err("source_growth_refused".into());
        }
        digest.update(&buffer[..count]);
        if let Some(output) = destination.as_deref_mut() {
            output
                .write_all(&buffer[..count])
                .map_err(|_| "private_copy_write_failed")?;
        } else if matches!(capture, ReadCapture::BoundedBytes) {
            contents.extend_from_slice(&buffer[..count]);
        }
    }
    if bytes != before.bytes || stamp(&file)? != before {
        return Err("source_changed_while_reading".into());
    }
    crate::snapshot::check_opened_path(&file, path, budget.deadline)?;
    let check = crate::snapshot::open_checked_file(path, budget.deadline)?;
    if stamp(&check)? != before {
        return Err("source_path_identity_changed".into());
    }
    budget.check()?;
    Ok((digest.hex(), contents))
}

/// Windows sharing excludes replacement of each held private ancestor.
/// This confines staging writes; it does not isolate executed project code.
#[cfg(windows)]
struct DestinationDirectories {
    root: PathBuf,
    handles: BTreeMap<String, File>,
}
#[cfg(windows)]
impl DestinationDirectories {
    fn new(root: &Path, budget: &Budget<'_>) -> Result<Self, String> {
        let handle = crate::snapshot::open_checked_directory(root, budget.deadline)?;
        let mut handles = BTreeMap::new();
        handles.insert(String::new(), handle);
        Ok(Self {
            root: root.to_path_buf(),
            handles,
        })
    }
    fn parts(relative: &str) -> Result<Vec<&str>, String> {
        if relative.len() > 1024
            || relative.contains(':')
            || relative.contains('\\')
            || relative.starts_with('/')
            || relative.chars().any(char::is_control)
        {
            return Err("private_destination_path_invalid".into());
        }
        if relative.is_empty() {
            return Ok(Vec::new());
        }
        let parts: Vec<_> = relative.split('/').collect();
        if parts.len() > 24
            || parts.iter().any(|part| {
                part.is_empty()
                    || matches!(*part, "." | "..")
                    || part.ends_with('.')
                    || part.ends_with(' ')
            })
        {
            return Err("private_destination_path_invalid".into());
        }
        Ok(parts)
    }
    fn recheck(&self, relative: &str, budget: &Budget<'_>) -> Result<(), String> {
        let mut key = String::new();
        crate::snapshot::check_opened_path(
            self.handles.get("").ok_or("private_root_guard_missing")?,
            &self.root,
            budget.deadline,
        )?;
        for part in Self::parts(relative)? {
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(part);
            let handle = self
                .handles
                .get(&key)
                .ok_or("private_ancestor_guard_missing")?;
            crate::snapshot::check_opened_path(handle, &self.root.join(&key), budget.deadline)?;
        }
        Ok(())
    }
    fn ensure(&mut self, relative: &str, budget: &Budget<'_>) -> Result<PathBuf, String> {
        let mut current = String::new();
        for part in Self::parts(relative)? {
            budget.check()?;
            self.recheck(&current, budget)?;
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(part);
            if !self.handles.contains_key(&current) {
                let path = self.root.join(&current);
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) => {
                        ordinary(&metadata)?;
                        if !metadata.is_dir() {
                            return Err("private_destination_directory_required".into());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        std::fs::create_dir(&path)
                            .map_err(|_| "private_directory_create_failed")?;
                    }
                    Err(_) => return Err("private_destination_metadata_unavailable".into()),
                }
                let handle = crate::snapshot::open_checked_directory(&path, budget.deadline)?;
                self.handles.insert(current.clone(), handle);
            }
            self.recheck(&current, budget)?;
        }
        budget.check()?;
        self.recheck(relative, budget)?;
        Ok(self.root.join(relative))
    }
    fn output(&mut self, relative: &str, budget: &Budget<'_>) -> Result<File, String> {
        use std::os::windows::fs::OpenOptionsExt;
        Self::parts(relative)?;
        let parent = directory_of(relative);
        self.ensure(parent, budget)?;
        budget.check()?;
        self.recheck(parent, budget)?;
        let path = self.root.join(relative);
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(0x0020_0000)
            .share_mode(1)
            .open(&path)
            .map_err(|_| "private_copy_create_failed")?;
        let metadata = output
            .metadata()
            .map_err(|_| "private_output_metadata_unavailable")?;
        ordinary(&metadata)?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err("private_output_empty_regular_file_required".into());
        }
        crate::snapshot::check_opened_path(&output, &path, budget.deadline)?;
        self.recheck(parent, budget)?;
        Ok(output)
    }
}
#[cfg(windows)]
fn scan(
    root: &Path,
    budget: &Budget<'_>,
    usage: &mut Usage,
) -> Result<(Vec<Entry>, Fingerprint), String> {
    namespace_io::scan_source(root, budget, usage)
}
#[cfg(not(windows))]
fn scan(
    root: &Path,
    budget: &Budget<'_>,
    usage: &mut Usage,
) -> Result<(Vec<Entry>, Fingerprint), String> {
    fn visit(
        root: &Path,
        path: &Path,
        prefix: &str,
        depth: usize,
        budget: &Budget<'_>,
        usage: &mut Usage,
        rows: &mut Vec<Entry>,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 24 {
            return Err("source_depth_limit".into());
        }
        let handle = crate::snapshot::open_checked_directory(path, budget.deadline)?;
        let identity = file_identity(&handle)?;
        let mut names = Vec::new();
        for entry in std::fs::read_dir(path).map_err(|_| "source_directory_unavailable")? {
            budget.check()?;
            usage.entries = usage
                .entries
                .checked_add(1)
                .ok_or("source_entry_overflow")?;
            if usage.entries > budget.max_entries {
                return Err("aggregate_entry_limit".into());
            }
            names.push(entry.map_err(|_| "source_entry_unavailable")?.file_name());
        }
        names.sort();
        for name in names {
            budget.check()?;
            let name = name.to_str().ok_or("source_filename_not_utf8")?;
            let tail = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if tail.len() > 1024 {
                return Err("source_path_byte_limit".into());
            }
            let full = path.join(name);
            let metadata =
                std::fs::symlink_metadata(&full).map_err(|_| "source_metadata_unavailable")?;
            ordinary(&metadata)?;
            if name == ".cargo" || name == "build.rs" {
                return Err("unsupported_cargo_configuration_or_build_script".into());
            }
            if !public_source(&tail) {
                continue;
            }
            if metadata.is_dir() {
                rows.push(Entry {
                    path: tail.clone(),
                    bytes: 0,
                    sha256: String::new(),
                    directory: true,
                });
                visit(root, &full, &tail, depth + 1, budget, usage, rows)?;
            } else {
                usage.files += 1;
                usage.bytes = usage
                    .bytes
                    .checked_add(metadata.len())
                    .ok_or("source_byte_overflow")?;
                if usage.files > budget.max_files || usage.bytes > budget.max_bytes {
                    return Err("aggregate_source_limit".into());
                }
                let (sha256, _) =
                    read_file(&full, budget.max_file, budget, None, Some(metadata.len()))?;
                rows.push(Entry {
                    path: tail,
                    bytes: metadata.len(),
                    sha256,
                    directory: false,
                });
            }
        }
        crate::snapshot::check_opened_path(&handle, path, budget.deadline)?;
        if file_identity(&handle)? != identity {
            return Err("source_directory_identity_changed".into());
        }
        let _ = root;
        Ok(())
    }
    let mut rows = Vec::new();
    visit(root, root, "", 0, budget, usage, &mut rows)?;
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    let fingerprint = fingerprint_entries(&rows);
    Ok((rows, fingerprint))
}
fn fingerprint_entries(rows: &[Entry]) -> Fingerprint {
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut files = 0;
    for row in rows {
        digest.update(&(row.path.len() as u64).to_le_bytes());
        digest.update(row.path.as_bytes());
        digest.update(&[u8::from(row.directory)]);
        digest.update(&row.bytes.to_le_bytes());
        digest.update(row.sha256.as_bytes());
        bytes += row.bytes;
        if !row.directory {
            files += 1;
        }
    }
    Fingerprint {
        sha256: digest.hex(),
        files,
        bytes,
        entries: rows.len(),
    }
}

#[derive(Clone, Debug)]
struct CargoShape {
    members: Vec<String>,
    dependencies: Vec<(String, String)>,
    library: Option<String>,
    package: Option<String>,
    cdylib: bool,
    build_scripts_disabled: bool,
}
fn cargo_shape(contents: &[u8]) -> Result<CargoShape, String> {
    if contents.len() > MANIFEST_BYTES as usize {
        return Err("cargo_manifest_byte_limit".into());
    }
    let text = std::str::from_utf8(contents).map_err(|_| "cargo_manifest_not_utf8")?;
    // Closed header grammar also rejects unsupported empty tables. No text is rewritten.
    let mut dependency_headers = BTreeSet::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            let end = line.find(']').ok_or("cargo_header_invalid")?;
            let header = &line[1..end];
            if !line[end + 1..].trim().is_empty() && !line[end + 1..].trim().starts_with('#') {
                return Err("cargo_header_invalid".into());
            }
            if !matches!(header, "workspace" | "package" | "lib")
                && !header.strip_prefix("dependencies.").is_some_and(identifier)
            {
                return Err("unsupported_cargo_table".into());
            }
            if let Some(name) = header.strip_prefix("dependencies.") {
                dependency_headers.insert(name.to_string());
                if dependency_headers.len() > 128 {
                    return Err("cargo_dependency_limit".into());
                }
            }
        }
    }
    let doc = Document::parse(text).map_err(|_| "unsupported_cargo_syntax")?;
    if dependency_headers
        .iter()
        .any(|name| doc.string(&format!("dependencies.{name}.path")).is_none())
    {
        return Err("explicit_path_dependency_required".into());
    }
    let mut dependencies = BTreeMap::new();
    for (name, value) in doc.entries() {
        match name {
            "workspace.members" => {
                let values = value.as_array().ok_or("cargo_members_type")?;
                if values.is_empty() || values.len() > 16 {
                    return Err("cargo_member_limit".into());
                }
                for path in values {
                    relative(path, false)?;
                }
            }
            "workspace.resolver" => {
                if value.as_str() != Some("2") {
                    return Err("unsupported_cargo_resolver".into());
                }
            }
            "package.name" | "lib.name" => {
                if !value.as_str().is_some_and(identifier) {
                    return Err("cargo_name_invalid".into());
                }
            }
            "package.version" => {
                if !value.as_str().is_some_and(|v| {
                    v.len() < 64
                        && v.split('.').count() == 3
                        && v.split('.').all(|part| {
                            !part.is_empty()
                                && part.bytes().all(|b| b.is_ascii_digit())
                                && (part == "0" || !part.starts_with('0'))
                        })
                }) {
                    return Err("cargo_version_invalid".into());
                }
            }
            "package.edition" => {
                if value.as_str() != Some("2021") {
                    return Err("unsupported_cargo_edition".into());
                }
            }
            "package.autobins" | "package.build" => {
                if value.as_bool() != Some(false) {
                    return Err("unsupported_cargo_build_inputs".into());
                }
            }
            "lib.path" => {
                relative(value.as_str().ok_or("cargo_library_path_type")?, false)?;
            }
            "lib.crate-type" => {
                let types = value.as_array().ok_or("cargo_library_type")?;
                if types.len() != 1 || !matches!(types[0].as_str(), "rlib" | "cdylib") {
                    return Err("unsupported_cargo_library_type".into());
                }
            }
            _ => {
                let part = name
                    .strip_prefix("dependencies.")
                    .and_then(|name| name.strip_suffix(".path"))
                    .filter(|name| identifier(name))
                    .ok_or("unsupported_cargo_field")?;
                let path = value.as_str().ok_or("cargo_dependency_path_type")?;
                relative(path, true)?;
                dependencies.insert(part.to_string(), path.to_string());
            }
        }
    }
    let package = doc.string("package.name").map(str::to_string);
    let members = doc.array("workspace.members").unwrap_or(&[]).to_vec();
    if package.is_some() == !members.is_empty() {
        return Err("one_package_or_virtual_workspace_required".into());
    }
    if !members.is_empty() && doc.string("workspace.resolver") != Some("2") {
        return Err("explicit_workspace_resolver_required".into());
    }
    if package.is_some()
        && (doc.string("package.version").is_none() || doc.string("package.edition").is_none())
    {
        return Err("cargo_package_identity_required".into());
    }
    Ok(CargoShape {
        members,
        dependencies: dependencies.into_iter().collect(),
        library: package
            .as_ref()
            .map(|_| doc.string("lib.path").unwrap_or("src/lib.rs").into()),
        package,
        cdylib: doc
            .array("lib.crate-type")
            .is_some_and(|types| types == ["cdylib"]),
        build_scripts_disabled: doc.boolean("package.build") == Some(false),
    })
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

pub(crate) struct StagingPlan {
    project: ImportedProject,
    roots: Vec<Root>,
    selected: BTreeSet<RootId>,
    selection: Selection,
    manifests: BTreeMap<(RootId, String), String>,
}
impl StagingPlan {
    pub(crate) fn resolve(
        project: &ImportedProject,
        grants: &[VerifiedGrant],
        placements: &[Placement],
        selection: Selection,
        revision: &Revision,
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        if selection.workspace.path != "Cargo.toml" {
            return Err("workspace_root_manifest_required".into());
        }
        if &project.revision != revision
            || grants.len() + 1 > MAX_ROOTS
            || placements.len() != grants.len() + 1
        {
            return Err("grant_revision_or_root_limit".into());
        }
        let project_id = RootId::new("project")?;
        let mut roots = vec![Root {
            id: project_id,
            source: project.root.clone(),
            identity: project.identity.clone(),
            mode: Mode::SourcePublish,
            slot: String::new(),
        }];
        for grant in grants {
            if grant.imported != project.root
                || grant.imported_identity != project.identity
                || &grant.revision != revision
            {
                return Err("grant_project_or_revision_mismatch".into());
            }
            roots.push(Root {
                id: grant.id.clone(),
                source: grant.root.clone(),
                identity: grant.identity.clone(),
                mode: grant.mode,
                slot: String::new(),
            });
        }
        let mut ids = BTreeSet::new();
        let mut slots = BTreeSet::new();
        for root in &mut roots {
            if !ids.insert(root.id.clone()) {
                return Err("duplicate_root_id".into());
            }
            let slot = &placements
                .iter()
                .find(|p| p.root_id == root.id)
                .ok_or("undeclared_root_placement")?
                .slot;
            relative(slot, false)?;
            if !public_source(slot) || slot.split('/').any(|part| part == ".cargo") {
                return Err("private_stage_slot_refused".into());
            }
            if !slots.insert(slot.to_ascii_lowercase()) {
                return Err("ambiguous_stage_slot".into());
            }
            root.slot = slot.clone();
            let (path, identity) = canonical_directory(&root.source, budget)?;
            if path != root.source || identity != root.identity {
                return Err("granted_root_identity_changed".into());
            }
        }
        for (index, a) in roots.iter().enumerate() {
            for b in roots.iter().skip(index + 1) {
                if a.identity == b.identity
                    || overlaps(&a.source, &b.source)
                    || overlaps(Path::new(&a.slot), Path::new(&b.slot))
                {
                    return Err("overlapping_or_aliased_roots".into());
                }
            }
        }
        if placements.iter().any(|p| !ids.contains(&p.root_id)) {
            return Err("undeclared_root_placement".into());
        }
        let mut plan = Self {
            project: project.clone(),
            roots,
            selected: BTreeSet::new(),
            selection,
            manifests: BTreeMap::new(),
        };
        let _leases = plan.leases(budget)?;
        plan.selected.insert(RootId::new("project")?);
        let workspace_ref = plan.selection.workspace.clone();
        let workspace = plan.load(&workspace_ref, budget)?;
        if workspace.package.is_some() || workspace.members.is_empty() {
            return Err("external_virtual_workspace_required".into());
        }
        let selected = plan.selection.member.clone();
        if selected.root_id != plan.selection.workspace.root_id
            || !workspace
                .members
                .iter()
                .any(|member| format!("{member}/Cargo.toml") == selected.path)
        {
            return Err("selected_member_not_declared".into());
        }
        plan.selected
            .insert(plan.selection.workspace.root_id.clone());
        let mut todo: Vec<_> = workspace
            .members
            .iter()
            .map(|member| ManifestRef {
                root_id: plan.selection.workspace.root_id.clone(),
                path: format!("{member}/Cargo.toml"),
            })
            .collect();
        let mut seen = BTreeSet::new();
        while let Some(reference) = todo.pop() {
            budget.check()?;
            if !seen.insert((reference.root_id.clone(), reference.path.clone())) {
                continue;
            }
            if seen.len() > MAX_MANIFESTS {
                return Err("cargo_manifest_count_limit".into());
            }
            let shape = plan.load(&reference, budget)?;
            if shape.package.is_none() || !shape.members.is_empty() {
                return Err("workspace_member_package_required".into());
            }
            if reference.root_id == selected.root_id
                && reference.path == selected.path
                && !shape.cdylib
            {
                return Err("selected_cdylib_required".into());
            }
            plan.selected.insert(reference.root_id.clone());
            for (name, path) in shape.dependencies {
                let dependency = plan.reference(&reference, &path)?;
                let shape = plan.load(&dependency, budget)?;
                if shape.package.as_deref() != Some(name.as_str()) {
                    return Err("dependency_package_name_mismatch".into());
                }
                todo.push(dependency);
            }
        }
        Ok(plan)
    }
    fn root(&self, id: &RootId) -> Result<&Root, String> {
        self.roots
            .iter()
            .find(|root| &root.id == id)
            .ok_or_else(|| "ungranted_root".into())
    }
    fn load(&mut self, reference: &ManifestRef, budget: &Budget<'_>) -> Result<CargoShape, String> {
        relative(&reference.path, false)?;
        if !public_source(&reference.path) || !reference.path.ends_with("Cargo.toml") {
            return Err("invalid_cargo_manifest_reference".into());
        }
        let root = self.root(&reference.root_id)?;
        let (digest, bytes) = read_file(
            &root.source.join(&reference.path),
            MANIFEST_BYTES,
            budget,
            None,
            None,
        )?;
        let shape = cargo_shape(&bytes)?;
        if let Some(library) = &shape.library {
            let resolved = normalize(directory_of(&reference.path), library)?;
            if !public_source(&resolved) || resolved.split('/').any(|part| part == ".cargo") {
                return Err("library_source_not_public".into());
            }
            let file =
                crate::snapshot::open_checked_file(&root.source.join(&resolved), budget.deadline)?;
            if !file
                .metadata()
                .map_err(|_| "library_source_unavailable")?
                .is_file()
            {
                return Err("library_source_required".into());
            }
        }
        let key = (reference.root_id.clone(), reference.path.clone());
        if self
            .manifests
            .get(&key)
            .is_some_and(|before| before != &digest)
        {
            return Err("manifest_changed_during_resolution".into());
        }
        self.manifests.insert(key, digest);
        Ok(shape)
    }
    fn reference(&self, from: &ManifestRef, path: &str) -> Result<ManifestRef, String> {
        let root = self.root(&from.root_id)?;
        let source_base = root.source.join(directory_of(&from.path));
        let mut original = source_base;
        for part in relative(path, true)? {
            if part == ".." {
                if !original.pop() {
                    return Err("relative_origin_escape".into());
                }
            } else {
                original.push(part);
            }
        }
        let target = self
            .roots
            .iter()
            .find(|root| original.strip_prefix(&root.source).is_ok())
            .ok_or("ungranted_path_dependency")?;
        let suffix = original
            .strip_prefix(&target.source)
            .map_err(|_| "ungranted_path_dependency")?
            .to_str()
            .ok_or("dependency_path_not_utf8")?
            .replace('\\', "/");
        let source_slot = normalize(&root.slot, &from.path)?;
        let stage_target = normalize(directory_of(&source_slot), path)?;
        let expected = if suffix.is_empty() {
            target.slot.clone()
        } else {
            format!("{}/{}", target.slot, suffix)
        };
        if stage_target != expected {
            return Err("relative_layout_mismatch".into());
        }
        let manifest = if suffix.is_empty() {
            "Cargo.toml".into()
        } else {
            format!("{suffix}/Cargo.toml")
        };
        Ok(ManifestRef {
            root_id: target.id.clone(),
            path: manifest,
        })
    }
    fn leases(&self, budget: &Budget<'_>) -> Result<Vec<ContentLock>, String> {
        let mut roots: Vec<_> = self.roots.iter().collect();
        roots.sort_by_key(|root| (key(&root.source), root.identity.clone()));
        let mut leases = Vec::new();
        for root in roots {
            budget.check()?;
            let (_, identity) = canonical_directory(&root.source, budget)?;
            if identity != root.identity {
                return Err("granted_root_identity_changed".into());
            }
            leases.push(ContentLock::shared(&root.source)?);
            crate::transactions::ensure_writable(&root.source)?;
        }
        Ok(leases)
    }
    #[cfg(test)]
    pub(crate) fn authorize_source_change(
        &self,
        root_id: &RootId,
        path: &str,
        revision: &Revision,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        budget.check()?;
        if &self.project.revision != revision {
            return Err("grant_revision_mismatch".into());
        }
        let root = self.root(root_id)?;
        relative(path, false)?;
        if !self.selected.contains(root_id)
            || !public_source(path)
            || path
                .split('/')
                .any(|part| matches!(part, ".cargo" | "build.rs"))
            || root.mode != Mode::SourcePublish
        {
            return Err("source_publish_grant_required".into());
        }
        let (_, identity) = canonical_directory(&root.source, budget)?;
        if identity != root.identity {
            return Err("granted_root_identity_changed".into());
        }
        Ok(())
    }
    pub(crate) fn stage_owned(
        &self,
        envelope: &crate::candidate_workspace::SourceEnvelope<'_>,
        budget: &Budget<'_>,
    ) -> Result<StagedSources, String> {
        envelope.recheck(budget.deadline)?;
        let result = self.stage_windows(envelope.path(), budget)?;
        envelope.recheck(budget.deadline)?;
        Ok(result)
    }
    #[cfg(test)]
    pub(crate) fn stage(
        &self,
        destination: &Path,
        budget: &Budget<'_>,
    ) -> Result<StagedSources, String> {
        #[cfg(not(windows))]
        {
            let _ = (destination, budget);
            Err("confined_stage_writes_unsupported_platform".into())
        }
        #[cfg(windows)]
        {
            self.stage_windows(destination, budget)
        }
    }
    #[cfg(windows)]
    fn stage_windows(
        &self,
        destination: &Path,
        budget: &Budget<'_>,
    ) -> Result<StagedSources, String> {
        budget.check()?;
        if destination.exists() {
            return Err("private_stage_must_be_fresh".into());
        }
        let parent = destination
            .parent()
            .ok_or("private_stage_parent_required")?;
        let (parent, parent_identity) = canonical_directory(parent, budget)?;
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("private_stage_name_invalid")?;
        relative(name, false)?;
        let destination = parent.join(name);
        if self
            .roots
            .iter()
            .any(|root| overlaps(&destination, &root.source))
        {
            return Err("private_stage_overlaps_original_root".into());
        }
        let _leases = self.leases(budget)?;
        let mut handles = Vec::new();
        for root in &self.roots {
            let handle = crate::snapshot::open_checked_directory(&root.source, budget.deadline)?;
            if file_identity(&handle)? != root.identity {
                return Err("granted_root_identity_changed".into());
            }
            handles.push(handle);
        }
        let parent_handle = crate::snapshot::open_checked_directory(&parent, budget.deadline)?;
        if file_identity(&parent_handle)? != parent_identity {
            return Err("private_stage_parent_changed".into());
        }
        let mut usage = Usage::default();
        let mut snapshots = Vec::new();
        for root in self
            .roots
            .iter()
            .filter(|root| self.selected.contains(&root.id))
        {
            let (rows, fingerprint) = scan(&root.source, budget, &mut usage)?;
            for row in rows
                .iter()
                .filter(|row| !row.directory && row.path.ends_with("Cargo.toml"))
            {
                let expected = self
                    .manifests
                    .get(&(root.id.clone(), row.path.clone()))
                    .ok_or("unresolved_extra_cargo_manifest")?;
                let (digest, body) = read_file(
                    &root.source.join(&row.path),
                    MANIFEST_BYTES,
                    budget,
                    None,
                    None,
                )?;
                if &digest != expected {
                    return Err("resolved_manifest_changed_before_staging".into());
                }
                cargo_shape(&body)?;
            }
            snapshots.push((root, rows, fingerprint));
        }
        // Only this newly created private tree can receive writes. Failure retains it.
        std::fs::create_dir(&destination).map_err(|_| "private_stage_create_failed")?;
        let stage_handle = crate::snapshot::open_checked_directory(&destination, budget.deadline)?;
        let mut directories = DestinationDirectories::new(&destination, budget)?;
        let mut result = Vec::new();
        for (root, rows, fingerprint) in &snapshots {
            let target = directories.ensure(&root.slot, budget)?;
            for row in rows {
                budget.check()?;
                let relative = format!("{}/{}", root.slot, row.path);
                let path = destination.join(&relative);
                if row.directory {
                    directories.ensure(&relative, budget)?;
                    continue;
                }
                let mut output = directories.output(&relative, budget)?;
                let (copied, _) = read_file(
                    &root.source.join(&row.path),
                    budget.max_file,
                    budget,
                    Some(&mut output),
                    Some(row.bytes),
                )?;
                output.sync_all().map_err(|_| "private_copy_sync_failed")?;
                crate::snapshot::check_opened_path(&output, &path, budget.deadline)?;
                directories.recheck(directory_of(&relative), budget)?;
                if copied != row.sha256 {
                    return Err("source_changed_during_private_copy".into());
                }
            }
            let (_, copied) = scan(&target, budget, &mut Usage::default())?;
            if copied != *fingerprint {
                return Err("copied_fingerprint_mismatch".into());
            }
            let staged_identity = canonical_directory(&target, budget)?.1;
            result.push(StagedRoot {
                root_id: root.id.clone(),
                mode: root.mode,
                source: root.source.clone(),
                staged: target,
                fingerprint: fingerprint.clone(),
                proposed_fingerprint: None,
                source_identity: root.identity.clone(),
                staged_identity,
            });
        }
        let mut after_usage = Usage::default();
        for (root, _, before) in &snapshots {
            let (_, after) = scan(&root.source, budget, &mut after_usage)?;
            if &after != before {
                return Err("source_changed_during_staging".into());
            }
        }
        for (root, handle) in self.roots.iter().zip(&handles) {
            crate::snapshot::check_opened_path(handle, &root.source, budget.deadline)?;
            if file_identity(handle)? != root.identity {
                return Err("granted_root_identity_changed".into());
            }
        }
        crate::snapshot::check_opened_path(&stage_handle, &destination, budget.deadline)?;
        crate::snapshot::check_opened_path(&parent_handle, &parent, budget.deadline)?;
        let selected = self.root(&self.selection.workspace.root_id)?;
        let cargo_manifest = destination
            .join(&selected.slot)
            .join(&self.selection.workspace.path);
        let member_root = self.root(&self.selection.member.root_id)?;
        let member_manifest = destination
            .join(&member_root.slot)
            .join(&self.selection.member.path);
        let envelope_identity = file_identity(&stage_handle)?;
        let mut directory_guards = Vec::new();
        for (relative, guard) in directories.handles {
            let path = if relative.is_empty() {
                destination.clone()
            } else {
                destination.join(relative)
            };
            crate::snapshot::check_opened_path(&guard, &path, budget.deadline)?;
            directory_guards.push((path, file_identity(&guard)?, guard));
        }
        directory_guards.push((parent.clone(), parent_identity, parent_handle));
        budget.check()?;
        Ok(StagedSources {
            envelope: destination,
            roots: result,
            cargo_manifest,
            member_manifest,
            envelope_identity,
            directory_guards: std::sync::Arc::new(StagedDirectoryGuards(directory_guards)),
            originals_mutated: false,
            manifest_transformed: false,
        })
    }
}
#[derive(Clone, Debug)]
pub(crate) struct StagedRoot {
    pub(crate) root_id: RootId,
    pub(crate) mode: Mode,
    pub(crate) source: PathBuf,
    pub(crate) staged: PathBuf,
    pub(crate) fingerprint: Fingerprint,
    // Only the bounded proposal overlay can change compilation expectations.
    proposed_fingerprint: Option<Fingerprint>,
    source_identity: DirectoryIdentity,
    staged_identity: DirectoryIdentity,
}
#[derive(Clone, Debug)]
pub(crate) struct StagedSources {
    pub(crate) envelope: PathBuf,
    pub(crate) roots: Vec<StagedRoot>,
    pub(crate) cargo_manifest: PathBuf,
    pub(crate) member_manifest: PathBuf,
    envelope_identity: DirectoryIdentity,
    directory_guards: std::sync::Arc<StagedDirectoryGuards>,
    pub(crate) originals_mutated: bool,
    pub(crate) manifest_transformed: bool,
}
struct StagedDirectoryGuards(Vec<(PathBuf, DirectoryIdentity, File)>);
impl std::fmt::Debug for StagedDirectoryGuards {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("StagedDirectoryGuards { private identities: redacted }")
    }
}

/// Guards for the compilation interval, not an execution sandbox. Source data
/// validity and original-worker retirement are separate acceptance conditions.
pub(crate) struct CompilationLease {
    staged: StagedSources,
    _content_leases: Vec<ContentLock>,
    original_guards: Vec<(PathBuf, DirectoryIdentity, File)>,
    staged_guards: Vec<(PathBuf, DirectoryIdentity, File)>,
    staged_file_guards: Vec<(PathBuf, Stamp, File)>,
    staged_content: Vec<StagedContentInventory>,
    compiler_directories: Vec<(PathBuf, DirectoryIdentity, File)>,
    compiler_output_directories: Vec<(PathBuf, DirectoryIdentity, File)>,
    compiler_files: Vec<(PathBuf, Stamp, File)>,
    compiler_links: Vec<(PathBuf, PathBuf, DirectoryIdentity, File)>,
}
#[derive(Clone)]
struct StagedContentInventory {
    root_id: RootId,
    path: PathBuf,
    identity: DirectoryIdentity,
    fingerprint: Fingerprint,
    entries: Vec<Entry>,
    identities: Vec<DirectoryIdentity>,
}
const NATIVE_CONTENT_ENCODING_LIMIT: usize = 65536;
// Conservative framing/anchor allowance, charged before inventory allocation.
const NATIVE_CONTENT_ENCODING_BASE: usize = 256 + 4 * (4096 + 128);
fn native_content_charge(used: &mut usize, bytes: usize) -> Result<(), String> {
    *used = used
        .checked_add(bytes)
        .ok_or("external_provenance_input_encoding_bound")?;
    if *used > NATIVE_CONTENT_ENCODING_LIMIT {
        return Err("external_provenance_input_encoding_bound".into());
    }
    Ok(())
}
fn native_content_shape(
    path: &Path,
    root_id: &RootId,
    rows: &[Entry],
    encoded: &mut usize,
    usage: &mut Usage,
    budget: &Budget<'_>,
) -> Result<(), String> {
    budget.check()?;
    let path = path
        .to_str()
        .ok_or("external_provenance_input_path_invalid")?;
    if path.len() > 4096 {
        return Err("external_provenance_input_path_bound".into());
    }
    native_content_charge(encoded, 512 + path.len() + root_id.as_str().len())?;
    let mut previous: Option<&str> = None;
    for row in rows {
        budget.check()?;
        if row.path.len() > 1024 || row.sha256.len() > 64 {
            return Err("external_provenance_content_shape_refused".into());
        }
        // Admit the path's complete fact allowance before path parsing allocates.
        native_content_charge(
            encoded,
            512 + path.len() + row.path.len() + row.sha256.len(),
        )?;
        relative(&row.path, false)?;
        if previous.is_some_and(|before| before >= row.path.as_str())
            || (row.directory && (row.bytes != 0 || !row.sha256.is_empty()))
            || (!row.directory
                && (row.sha256.len() != 64 || !row.sha256.bytes().all(|b| b.is_ascii_hexdigit())))
        {
            return Err("external_provenance_content_shape_refused".into());
        }
        previous = Some(&row.path);
        usage.entries = usage
            .entries
            .checked_add(1)
            .ok_or("external_provenance_content_limit")?;
        if !row.directory {
            usage.files = usage
                .files
                .checked_add(1)
                .ok_or("external_provenance_content_limit")?;
            usage.bytes = usage
                .bytes
                .checked_add(row.bytes)
                .ok_or("external_provenance_content_limit")?;
            if row.bytes > budget.max_file {
                return Err("external_provenance_content_limit".into());
            }
        }
        if usage.entries > budget.max_entries
            || usage.files > budget.max_files
            || usage.bytes > budget.max_bytes
        {
            return Err("external_provenance_content_limit".into());
        }
    }
    budget.check()
}
fn native_provenance_guard_bound(counts: &[usize], duplicates: usize) -> Result<(), String> {
    if counts
        .iter()
        .try_fold(0usize, |sum, count| sum.checked_add(*count))
        .and_then(|sum| sum.checked_add(duplicates))
        .is_none_or(|sum| sum > 8192)
    {
        return Err("external_provenance_guard_bound".into());
    }
    Ok(())
}
fn native_content_anchor_aliases(paths: [&Path; 4]) -> Result<usize, String> {
    let mut aliases = 0usize;
    for index in 0..paths.len() {
        for prior in 0..index {
            if provenance_paths_equal(paths[prior], paths[index]) {
                // A standalone package has one manifest with two semantic roles.
                // Other role overlaps are not permission for duplicate bindings.
                if prior != 0 || index != 1 {
                    return Err("external_provenance_input_ambiguous".into());
                }
                aliases += 1;
            }
        }
    }
    Ok(aliases)
}
fn native_content_duplicate_count(
    unique_files: usize,
    workspace: &Path,
    member: &Path,
) -> Result<usize, String> {
    // Only the permitted manifest-role pair can add a duplicate content object.
    // This count is available before allocating the temporary lockfile path.
    unique_files
        .checked_add(if provenance_paths_equal(workspace, member) {
            1
        } else {
            0
        })
        .and_then(|count| count.checked_add(1)) // Original envelope duplicate.
        .ok_or_else(|| "external_provenance_guard_bound".into())
}
struct NativeContentDigest {
    bytes: usize,
    hash: Sha256,
}
impl NativeContentDigest {
    fn new() -> Self {
        Self {
            bytes: 0,
            hash: Sha256::new(),
        }
    }
    fn field(&mut self, value: &[u8]) -> Result<(), String> {
        let bytes = 8usize
            .checked_add(value.len())
            .ok_or("external_provenance_input_encoding_bound")?;
        native_content_charge(&mut self.bytes, bytes)?;
        self.hash.update(&(value.len() as u64).to_le_bytes());
        self.hash.update(value);
        Ok(())
    }
    fn identity(&mut self, identity: &DirectoryIdentity) -> Result<(), String> {
        self.field(&identity.volume.to_le_bytes())?;
        self.field(&identity.id)?;
        match identity.created {
            None => self.field(&[0]),
            Some(created) => {
                self.field(&[1])?;
                let (negative, duration) = match created.duration_since(std::time::UNIX_EPOCH) {
                    Ok(duration) => (false, duration),
                    Err(error) => (true, error.duration()),
                };
                self.field(&[u8::from(negative)])?;
                self.field(&duration.as_secs().to_le_bytes())?;
                self.field(&duration.subsec_nanos().to_le_bytes())
            }
        }
    }
}
// Lexical provenance binding only. File identity and bytes still come from held guards.
fn provenance_path_components(path: &Path) -> Option<Vec<String>> {
    if !path.is_absolute() {
        return None;
    }
    let raw = path.to_str()?;
    if raw.len() > 4096
        || raw.chars().any(char::is_control)
        || raw
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
    {
        return None;
    }
    let mut parts = Vec::new();
    for component in path.components() {
        use std::path::{Component, Prefix};
        let value = match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(drive) => format!("disk:{}", drive.to_ascii_lowercase()),
                Prefix::VerbatimDisk(drive) => {
                    format!("verbatim-disk:{}", drive.to_ascii_lowercase())
                }
                _ => return None,
            },
            Component::RootDir => "<root>".to_owned(),
            Component::Normal(part) => {
                let text = part.to_str()?;
                if text.is_empty()
                    || text.contains(':')
                    || text.ends_with('.')
                    || text.ends_with(' ')
                {
                    return None;
                }
                let mut text = text.to_owned();
                if cfg!(windows) {
                    text.make_ascii_lowercase();
                }
                text
            }
            Component::CurDir | Component::ParentDir => return None,
        };
        parts.push(value);
        if parts.len() > 128 {
            return None;
        }
    }
    Some(parts)
}
pub(crate) fn provenance_paths_equal(left: &Path, right: &Path) -> bool {
    match (
        provenance_path_components(left),
        provenance_path_components(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}
fn provenance_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let root = provenance_path_components(root).ok_or("external_provenance_input_path_invalid")?;
    let path = provenance_path_components(path).ok_or("external_provenance_input_path_invalid")?;
    if path.len() <= root.len() || !path.starts_with(&root) {
        return Err("external_provenance_input_outside_envelope".into());
    }
    Ok(path[root.len()..].join("/"))
}
// try_clone shares offsets. Always restore the held object's cursor, even on refusal.
fn hash_provenance_input(path: &Path, guard: &File, budget: &Budget<'_>) -> Result<String, String> {
    budget.check()?;
    let mut reader = guard;
    let position = std::io::Seek::stream_position(&mut reader)
        .map_err(|_| "external_provenance_cursor_unavailable")?;
    let result = hash_validated_artifact(path, guard, budget);
    // Cursor restoration is cleanup only and does not accept work after expiry.
    let restore = std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(position));
    match (result, restore) {
        (Ok(digest), Ok(_)) => {
            budget.check()?;
            Ok(digest)
        }
        (Err(error), Ok(_)) => Err(error),
        (_, Err(_)) => Err("external_provenance_cursor_restore_failed".into()),
    }
}
struct NativeProvenanceFile {
    role: &'static str,
    path: PathBuf,
    expected: Stamp,
    sha256: String,
    guard: File,
}

fn held_gdextension_text(
    file: &NativeProvenanceFile,
    budget: &Budget<'_>,
) -> Result<String, String> {
    let mut reader = &file.guard;
    let position = std::io::Seek::stream_position(&mut reader)
        .map_err(|_| "external_private_binding_cursor_unavailable")?;
    let result = (|| {
        budget.check()?;
        if file.expected.bytes > PRIVATE_GDEXTENSION_BYTES
            || file.expected.bytes > budget.max_file
            || file.expected.bytes > budget.max_bytes
        {
            return Err("external_private_binding_text_bound".into());
        }
        if stamp(&file.guard)? != file.expected {
            return Err("external_private_binding_manifest_changed".into());
        }
        crate::snapshot::check_opened_path(&file.guard, &file.path, budget.deadline())?;
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))
            .map_err(|_| "external_private_binding_seek_failed")?;
        let mut bytes = Vec::with_capacity(file.expected.bytes as usize + 1);
        let mut buffer = [0u8; 8192];
        loop {
            budget.check()?;
            let capacity = (file.expected.bytes as usize)
                .saturating_sub(bytes.len())
                .saturating_add(1)
                .min(buffer.len());
            let count = reader
                .read(&mut buffer[..capacity])
                .map_err(|_| "external_private_binding_read_failed")?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) as u64 > file.expected.bytes {
                return Err("external_private_binding_manifest_changed".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        if bytes.len() as u64 != file.expected.bytes
            || crate::sha256_hex(&bytes) != file.sha256
            || stamp(&file.guard)? != file.expected
        {
            return Err("external_private_binding_manifest_changed".into());
        }
        crate::snapshot::check_opened_path(&file.guard, &file.path, budget.deadline())?;
        budget.check()?;
        String::from_utf8(bytes).map_err(|_| "external_private_binding_utf8_required".into())
    })();
    // Restoration is attempted after every read/refusal, without renewing work.
    match (
        result,
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(position)),
    ) {
        (Ok(text), Ok(_)) => {
            budget.check()?;
            Ok(text)
        }
        (Err(error), Ok(_)) => Err(error),
        (_, Err(_)) => Err("external_private_binding_cursor_restore_failed".into()),
    }
}
/// Same-object read custody for every staged local-source file, with four anchors.
/// This owns no compiler claim/Arc and proves no complete compiler read closure.
/// Original compiler-output directory custody, not execution or loading authority.
pub(crate) struct NativeReportedOutputRoot {
    target: PathBuf,
    canonical: PathBuf,
    identity: DirectoryIdentity,
    guard: File,
}
/// One Cargo-reported file held through the original compilation provenance.
/// Fields cannot be populated from serialized receipts.
pub(crate) struct NativeReportedOutput {
    path: PathBuf,
    relative: String,
    root_identity: DirectoryIdentity,
    expected: Stamp,
    sha256: String,
    guard: File,
}
fn hash_reported_local_output(
    path: &Path,
    guard: &File,
    expected: &Stamp,
    maximum: u64,
    budget: &Budget<'_>,
) -> Result<String, String> {
    let mut reader = guard;
    let position = std::io::Seek::stream_position(&mut reader)
        .map_err(|_| "external_reported_output_cursor")?;
    let observed = (|| -> Result<String, String> {
        budget.check()?;
        crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
        if stamp(guard)? != *expected {
            return Err("external_reported_output_changed".into());
        }
        // Output allowance is separate from the original source-content quotas.
        // The same budget still supplies owner/cancellation/deadline checks.
        if expected.bytes > maximum.min(MAX_FILE_BYTES) {
            return Err("external_reported_output_byte_bound".into());
        }
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))
            .map_err(|_| "external_reported_output_seek")?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            budget.check()?;
            let capacity = expected
                .bytes
                .saturating_sub(bytes)
                .saturating_add(1)
                .min(buffer.len() as u64) as usize;
            let n = reader
                .read(&mut buffer[..capacity])
                .map_err(|_| "external_reported_output_read")?;
            if n == 0 {
                break;
            }
            bytes = bytes
                .checked_add(n as u64)
                .ok_or("external_reported_output_byte_bound")?;
            if bytes > expected.bytes {
                return Err("external_reported_output_changed".into());
            }
            hash.update(&buffer[..n]);
        }
        if bytes != expected.bytes || stamp(guard)? != *expected {
            return Err("external_reported_output_changed".into());
        }
        crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
        budget.check()?;
        Ok(hash.hex())
    })();
    let restored = std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(position))
        .map_err(|_| "external_reported_output_cursor_restore")?;
    if restored != position {
        return Err("external_reported_output_cursor_restore".into());
    }
    observed
}
impl NativeReportedOutputRoot {
    /// Classification only. All path/identity failures propagate, never select another root.
    pub(crate) fn contains_reported_path(
        &self,
        reported: &str,
        budget: &Budget<'_>,
    ) -> Result<bool, String> {
        self.recheck(budget)?;
        let reported = checked_reported_path(reported)?;
        let checked = crate::snapshot::open_checked_file(&reported, budget.deadline)?;
        let canonical = crate::project::clean_path(
            reported
                .canonicalize()
                .map_err(|_| "external_reported_output_path")?,
        );
        crate::snapshot::check_opened_path(&checked, &canonical, budget.deadline)?;
        let _ = stamp(&checked)?;
        self.recheck(budget)?;
        Ok(canonical.starts_with(&self.canonical))
    }
    pub(crate) fn recheck(&self, budget: &Budget<'_>) -> Result<(), String> {
        budget.check()?;
        crate::snapshot::check_opened_path(&self.guard, &self.target, budget.deadline)?;
        crate::snapshot::check_opened_path(&self.guard, &self.canonical, budget.deadline)?;
        if file_identity(&self.guard)? != self.identity {
            return Err("external_reported_output_root_changed".into());
        }
        budget.check()
    }
    pub(crate) fn hold(
        &self,
        lease: &CompilationLease,
        reported: &str,
        maximum: u64,
        budget: &Budget<'_>,
    ) -> Result<NativeReportedOutput, String> {
        self.recheck(budget)?;
        let (path, checked) = lease.checked_artifact_file(&self.target, reported, budget)?;
        let expected = stamp(&checked)?;
        // Match the checked original, then deny write/delete sharing before hashing.
        let guard = open_compile_source(&path, budget)?;
        if stamp(&guard)? != expected {
            return Err("external_reported_output_changed".into());
        }
        let relative = path
            .strip_prefix(&self.canonical)
            .map_err(|_| "external_reported_output_outside_target")?
            .to_str()
            .ok_or("external_reported_output_path")?
            .replace('\\', "/");
        if relative.is_empty() || relative.len() > 4096 || relative.chars().any(char::is_control) {
            return Err("external_reported_output_path".into());
        }
        let sha256 = hash_reported_local_output(&path, &guard, &expected, maximum, budget)?;
        self.recheck(budget)?;
        Ok(NativeReportedOutput {
            path,
            relative,
            root_identity: self.identity.clone(),
            expected,
            sha256,
            guard,
        })
    }
}
impl NativeReportedOutput {
    pub(crate) fn bytes(&self) -> u64 {
        self.expected.bytes
    }
    pub(crate) fn relative(&self) -> &str {
        &self.relative
    }
    pub(crate) fn identity_key(&self) -> String {
        identity_digest(&self.expected.identity)
    }
    pub(crate) fn digest_value(&self) -> serde_json::Value {
        serde_json::json!({"relative":self.relative,"bytes":self.expected.bytes,"sha256":self.sha256,
            "identity_sha256":self.identity_key(),"root_identity_sha256":identity_digest(&self.root_identity)})
    }
    pub(crate) fn matches_artifact(&self, path: &Path, hash: &str) -> Result<bool, String> {
        Ok(
            provenance_paths_equal(&local_build_path(&self.path)?, &local_build_path(path)?)
                && self.sha256 == hash,
        )
    }
    pub(crate) fn recheck(
        &self,
        root: &NativeReportedOutputRoot,
        maximum: u64,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        root.recheck(budget)?;
        if self.root_identity != root.identity
            || !provenance_paths_equal(&self.path, &root.canonical.join(&self.relative))
        {
            return Err("external_reported_output_root_changed".into());
        }
        if hash_reported_local_output(&self.path, &self.guard, &self.expected, maximum, budget)?
            != self.sha256
        {
            return Err("external_reported_output_changed".into());
        }
        root.recheck(budget)
    }
    #[cfg(all(windows, test))]
    pub(crate) fn observation_for_test(&self) -> serde_json::Value {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        serde_json::json!({"output":self.digest_value(),"original_handle":self.guard.as_raw_handle() as usize,
            "write_access_refused":OpenOptions::new().write(true).open(&self.path).is_err(),
            "delete_access_refused":OpenOptions::new().access_mode(0x0001_0000).share_mode(7)
                .custom_flags(0x0020_0000).open(&self.path).is_err(),
            "scope":"same-original retained test observation, not posttest handle adoption or loading authority"})
    }
}
pub(crate) struct NativeSourceIdentity {
    envelope: PathBuf,
    envelope_identity: DirectoryIdentity,
    envelope_guard: File,
    files: Vec<NativeProvenanceFile>,
    content: Vec<StagedContentInventory>,
    digest: String,
}
/// A logical local input joined to an original held file, not a read-closure claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeLocalInput {
    root: RootId,
    relative: String,
    sha256: String,
    root_fingerprint: String,
    identity: DirectoryIdentity,
}
impl NativeLocalInput {
    pub(crate) fn digest_value(&self) -> serde_json::Value {
        serde_json::json!({
            "root": self.root.as_str(), "relative": self.relative,
            "sha256": self.sha256, "root_fingerprint": self.root_fingerprint
        })
    }
}
// Normalize only reported local-build spellings, never the global ownership rules.
// Matching still requires the declared inventory path and its original held identity.
fn local_build_path(path: &Path) -> Result<PathBuf, String> {
    let path = crate::project::clean_path(checked_reported_path(
        path.to_str().ok_or("external_local_build_input_path")?,
    )?);
    provenance_path_components(&path).ok_or("external_local_build_input_path")?;
    Ok(path)
}
impl NativeSourceIdentity {
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    fn gdextension_manifest(
        &self,
        root_id: &RootId,
        manifest_relative: &str,
        budget: &Budget<'_>,
    ) -> Result<HeldGdExtensionManifest, String> {
        budget.check()?;
        let manifest_parts = private_gdextension_parts(manifest_relative)?;
        if !manifest_relative.ends_with(".gdextension") {
            return Err("external_private_binding_manifest_kind".into());
        }
        let mut roots = self.content.iter().filter(|root| &root.root_id == root_id);
        let root = roots
            .next()
            .ok_or("external_private_binding_root_not_held")?;
        if roots.next().is_some() {
            return Err("external_private_binding_root_ambiguous".into());
        }
        let mut entries = root
            .entries
            .iter()
            .filter(|row| !row.directory && row.path == manifest_relative);
        let row = entries
            .next()
            .ok_or("external_private_binding_manifest_not_held")?;
        if entries.next().is_some() {
            return Err("external_private_binding_manifest_ambiguous".into());
        }
        if row.bytes > PRIVATE_GDEXTENSION_BYTES
            || row.bytes > budget.max_file
            || row.bytes > budget.max_bytes
        {
            return Err("external_private_binding_text_bound".into());
        }
        let path = root.path.join(manifest_relative);
        let manifest = self.local_input(&path, budget)?;
        let mut held = self
            .files
            .iter()
            .filter(|file| provenance_paths_equal(&file.path, &path));
        let file = held
            .next()
            .ok_or("external_private_binding_manifest_not_held")?;
        if held.next().is_some()
            || file.role != "staged_content"
            || manifest.root != *root_id
            || file.expected.identity != manifest.identity
            || file.sha256 != manifest.sha256
        {
            return Err("external_private_binding_manifest_ambiguous".into());
        }
        let text = held_gdextension_text(file, budget)?;
        let mut project_relative = None;
        for entry in &root.entries {
            budget.check()?;
            if entry.directory || entry.path.rsplit('/').next() != Some("project.godot") {
                continue;
            }
            let parts = private_gdextension_parts(&entry.path)?;
            if manifest_parts.starts_with(&parts[..parts.len() - 1])
                && project_relative.replace(entry.path.as_str()).is_some()
            {
                return Err("external_private_binding_godot_root_ambiguous".into());
            }
        }
        let project_relative =
            project_relative.ok_or("external_private_binding_godot_root_not_held")?;
        let project = self.local_input(&root.path.join(project_relative), budget)?;
        if project.root != *root_id {
            return Err("external_private_binding_root_ambiguous".into());
        }
        let (library, entry_symbol) = private_gdextension_mapping(&text)?;
        let library_relative =
            private_gdextension_library_relative(manifest_relative, project_relative, &library)?;
        budget.check()?;
        let document = configfile_parser_document(&text)?;
        Ok(HeldGdExtensionManifest {
            manifest,
            project,
            library_relative,
            entry_symbol,
            text,
            document,
        })
    }
    pub(crate) fn local_input(
        &self,
        reported: &Path,
        budget: &Budget<'_>,
    ) -> Result<NativeLocalInput, String> {
        budget.check()?;
        let reported = local_build_path(reported)?;
        let mut matched = None;
        for root in &self.content {
            for row in &root.entries {
                budget.check()?;
                if row.directory
                    || !provenance_paths_equal(
                        &local_build_path(&root.path.join(&row.path))?,
                        &reported,
                    )
                {
                    continue;
                }
                if matched.is_some() {
                    return Err("external_local_build_input_ambiguous".into());
                }
                let mut held = None;
                for file in &self.files {
                    if !provenance_paths_equal(&local_build_path(&file.path)?, &reported) {
                        continue;
                    }
                    budget.check()?;
                    crate::snapshot::check_opened_path(&file.guard, &reported, budget.deadline)?;
                    if stamp(&file.guard)? != file.expected
                        || file.sha256 != row.sha256
                        || hash_provenance_input(&file.path, &file.guard, budget)? != row.sha256
                    {
                        return Err("external_local_build_input_changed".into());
                    }
                    if held
                        .as_ref()
                        .is_some_and(|identity| identity != &file.expected.identity)
                    {
                        return Err("external_local_build_input_ambiguous".into());
                    }
                    held = Some(file.expected.identity.clone());
                }
                let identity = held.ok_or("external_local_build_input_not_held")?;
                matched = Some(NativeLocalInput {
                    root: root.root_id.clone(),
                    relative: row.path.clone(),
                    sha256: row.sha256.clone(),
                    root_fingerprint: root.fingerprint.sha256.clone(),
                    identity,
                });
            }
        }
        matched.ok_or_else(|| "external_local_build_input_not_held".into())
    }
    pub(crate) fn recheck_local_input(
        &self,
        input: &NativeLocalInput,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        let mut roots = self
            .content
            .iter()
            .filter(|root| root.root_id == input.root);
        let root = roots.next().ok_or("external_local_build_root_changed")?;
        if roots.next().is_some()
            || self.local_input(&root.path.join(&input.relative), budget)? != *input
        {
            return Err("external_local_build_input_changed".into());
        }
        budget.check()
    }
    pub(crate) fn recheck(
        &self,
        staged: &StagedSources,
        member: &Path,
        source: &Path,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        budget.check()?;
        if !provenance_paths_equal(&self.envelope, &staged.envelope)
            || self.envelope_identity != staged.envelope_identity
            || self.files.len() < 4
        {
            return Err("external_provenance_envelope_changed".into());
        }
        crate::snapshot::check_opened_path(&self.envelope_guard, &self.envelope, budget.deadline)?;
        if file_identity(&self.envelope_guard)? != self.envelope_identity {
            return Err("external_provenance_envelope_changed".into());
        }
        let lock = staged
            .cargo_manifest
            .parent()
            .ok_or("external_provenance_workspace_missing")?
            .join("Cargo.lock");
        let expected = [
            ("workspace_manifest", staged.cargo_manifest.as_path()),
            ("selected_manifest", member),
            ("selected_source", source),
            ("lock", lock.as_path()),
        ];
        for (file, (role, path)) in self.files.iter().take(4).zip(expected) {
            budget.check()?;
            if file.role != role
                || !provenance_paths_equal(&file.path, path)
                || provenance_relative_path(&self.envelope, &file.path).is_err()
                || provenance_relative_path(&self.envelope, path).is_err()
                || file.expected.bytes > budget.max_file
                || stamp(&file.guard)? != file.expected
            {
                return Err("external_provenance_held_input_changed".into());
            }
        }
        if self.content_digest(staged, budget)? != self.digest {
            return Err("external_provenance_content_digest_changed".into());
        }
        // Existing namespace scanner corroborates membership, never replaces custody.
        check_proposed(staged, budget)?;
        budget.check()
    }
    fn content_digest(
        &self,
        staged: &StagedSources,
        budget: &Budget<'_>,
    ) -> Result<String, String> {
        budget.check()?;
        if self.content.len() != staged.roots.len()
            || self.content.is_empty()
            || self.content.len() > MAX_ROOTS
            || self.files.len() < 4
        {
            return Err("external_provenance_content_binding_changed".into());
        }
        let aliases = native_content_anchor_aliases([
            self.files[0].path.as_path(),
            self.files[1].path.as_path(),
            self.files[2].path.as_path(),
            self.files[3].path.as_path(),
        ])?;
        let mut charged = NATIVE_CONTENT_ENCODING_BASE;
        let mut usage = Usage::default();
        for root in &self.content {
            native_content_shape(
                &root.path,
                &root.root_id,
                &root.entries,
                &mut charged,
                &mut usage,
                budget,
            )?;
            if root.entries.len() != root.identities.len()
                || fingerprint_entries(&root.entries) != root.fingerprint
            {
                return Err("external_provenance_content_binding_changed".into());
            }
        }
        // Content quotas count unique inventory files, while allowed manifest
        // aliases retain an additional independently checked role-bearing object.
        if usage.files.checked_add(aliases) != Some(self.files.len()) {
            return Err("external_provenance_content_binding_changed".into());
        }
        // Allocation is admitted by the aggregate shape/encoding bounds above.
        let mut roots: Vec<_> = self.content.iter().collect();
        roots.sort_by(|a, b| a.root_id.cmp(&b.root_id));
        let mut digest = NativeContentDigest::new();
        digest.field(b"private-staged-local-source-custody/1")?;
        digest.identity(&self.envelope_identity)?;
        let mut previous = None;
        for root in roots {
            budget.check()?;
            if previous == Some(&root.root_id) {
                return Err("external_provenance_content_binding_changed".into());
            }
            previous = Some(&root.root_id);
            let mut matching = staged
                .roots
                .iter()
                .filter(|candidate| candidate.root_id == root.root_id);
            let candidate = matching
                .next()
                .ok_or("external_provenance_content_binding_changed")?;
            if matching.next().is_some()
                || !provenance_paths_equal(&root.path, &candidate.staged)
                || root.identity != candidate.staged_identity
                || &root.fingerprint
                    != candidate
                        .proposed_fingerprint
                        .as_ref()
                        .unwrap_or(&candidate.fingerprint)
            {
                return Err("external_provenance_content_binding_changed".into());
            }
            digest.field(root.root_id.as_str().as_bytes())?;
            digest.field(provenance_relative_path(&self.envelope, &root.path)?.as_bytes())?;
            digest.identity(&root.identity)?;
            digest.field(root.fingerprint.sha256.as_bytes())?;
            digest.field(&(root.fingerprint.entries as u64).to_le_bytes())?;
            digest.field(&(root.fingerprint.files as u64).to_le_bytes())?;
            digest.field(&root.fingerprint.bytes.to_le_bytes())?;
            for (row, identity) in root.entries.iter().zip(&root.identities) {
                budget.check()?;
                let path = root.path.join(&row.path);
                digest.field(row.path.as_bytes())?;
                digest.field(&[u8::from(row.directory)])?;
                digest.identity(identity)?;
                digest.field(&row.bytes.to_le_bytes())?;
                digest.field(row.sha256.as_bytes())?;
                if row.directory {
                    let mut matching = staged
                        .directory_guards
                        .0
                        .iter()
                        .filter(|(held_path, _, _)| provenance_paths_equal(held_path, &path));
                    let (_, expected, guard) = matching
                        .next()
                        .ok_or("external_provenance_content_directory_not_held")?;
                    if matching.next().is_some()
                        || expected != identity
                        || file_identity(guard)? != *identity
                    {
                        return Err("external_provenance_content_directory_changed".into());
                    }
                    crate::snapshot::check_opened_path(guard, &path, budget.deadline)?;
                } else {
                    let mut first_match = None;
                    for (index, file) in self.files.iter().enumerate() {
                        budget.check()?;
                        if provenance_paths_equal(&file.path, &path) {
                            if let Some(first) = first_match {
                                if first != 0 || index != 1 || aliases != 1 {
                                    return Err("external_provenance_input_ambiguous".into());
                                }
                            } else {
                                first_match = Some(index);
                            }
                            // Validate every matching role object against this one
                            // inventory row; neither alias substitutes for the other.
                            if (index >= 4 && file.role != "staged_content")
                                || file.expected.identity != *identity
                                || file.expected.bytes != row.bytes
                                || file.sha256 != row.sha256
                                || stamp(&file.guard)? != file.expected
                                || hash_provenance_input(&file.path, &file.guard, budget)?
                                    != row.sha256
                            {
                                return Err("external_provenance_held_input_changed".into());
                            }
                        }
                    }
                    if first_match.is_none() {
                        return Err("external_provenance_input_not_held".into());
                    }
                }
            }
        }
        for file in self.files.iter().take(4) {
            digest.field(file.role.as_bytes())?;
            digest.field(provenance_relative_path(&self.envelope, &file.path)?.as_bytes())?;
        }
        budget.check()?;
        Ok(digest.hash.hex())
    }
    #[cfg(test)]
    pub(crate) fn staged_content_custody_for_test(
        &mut self,
        staged: &StagedSources,
        member: &Path,
        source: &Path,
        paths: &[PathBuf],
        budget: &Budget<'_>,
        corrupt: bool,
    ) -> Result<serde_json::Value, String> {
        self.recheck(staged, member, source, budget)?;
        if paths.is_empty() || paths.len() > 16 {
            return Err("staged_content_test_path_bound".into());
        }
        let mut observations = Vec::with_capacity(paths.len());
        let mut corruption = None;
        for path in paths {
            budget.check()?;
            let index = self
                .files
                .iter()
                .position(|file| provenance_paths_equal(&file.path, path))
                .ok_or("staged_content_test_input_missing")?;
            let file = &self.files[index];
            if file_identity(&file.guard)? != file.expected.identity
                || file.role != "staged_content"
            {
                return Err("staged_content_test_original_input_missing".into());
            }
            observations.push(serde_json::json!({
                "path":provenance_relative_path(&self.envelope, path)?,
                "handle":std::os::windows::io::AsRawHandle::as_raw_handle(&file.guard) as usize,
                "identity":file.expected.identity,"sha256":file.sha256,"bytes":file.expected.bytes,
            }));
            if corrupt && corruption.is_none() {
                self.files[index].sha256.push('0');
                let observed = self.recheck(staged, member, source, budget);
                self.files[index].sha256.pop();
                let error = observed
                    .err()
                    .ok_or("staged_content_test_corruption_accepted")?;
                if error != "external_provenance_held_input_changed" {
                    return Err(error);
                }
                self.recheck(staged, member, source, budget)?;
                corruption = Some(error);
            }
        }
        Ok(
            serde_json::json!({"inputs":observations,"restored_corruption":corruption,
            "staged_files":self.files.len(),"staged_roots":self.content.len(),
            "scope":"all staged local-source content custody",
            "dependency_content_closure_verified":false,"exact_build_units_verified":false}),
        )
    }
}
fn open_compile_source(path: &Path, budget: &Budget<'_>) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    budget.check()?;
    let before = crate::snapshot::open_checked_file(path, budget.deadline)?;
    let expected = stamp(&before)?;
    let guarded = OpenOptions::new()
        .read(true)
        .custom_flags(0x0020_0000)
        .share_mode(1)
        .open(path)
        .map_err(|_| "external_compile_source_guard_refused")?;
    crate::snapshot::check_opened_path(&guarded, path, budget.deadline)?;
    if stamp(&guarded)? != expected {
        return Err("external_compile_staged_file_changed".into());
    }
    budget.check()?;
    Ok(guarded)
}
impl StagedSources {
    pub(crate) fn compilation_lease(
        &self,
        budget: &Budget<'_>,
    ) -> Result<CompilationLease, String> {
        budget.check()?;
        // Existing file guards cannot prevent arbitrary new child insertion.
        // Require Cargo's authored switch to disable implicit build.rs discovery.
        // No manifest is rewritten and staging-only behavior remains unchanged.
        let guard_limit = 8192usize;
        let estimated = self
            .directory_guards
            .0
            .len()
            .checked_add(1)
            .ok_or("external_compile_guard_limit")?
            .checked_add(self.roots.len() * 2)
            .and_then(|count| {
                self.roots.iter().try_fold(count, |count, root| {
                    count.checked_add(
                        root.proposed_fingerprint
                            .as_ref()
                            .unwrap_or(&root.fingerprint)
                            .entries,
                    )
                })
            })
            .ok_or("external_compile_guard_limit")?;
        if estimated > guard_limit {
            return Err("external_compile_guard_limit".into());
        }
        let mut ordered: Vec<_> = self.roots.iter().collect();
        ordered.sort_by_key(|root| (key(&root.source), root.source_identity.clone()));
        let mut content_leases = Vec::new();
        let mut original_guards = Vec::new();
        for root in ordered {
            let guard = crate::snapshot::open_checked_directory(&root.source, budget.deadline)?;
            crate::snapshot::check_opened_path(&guard, &root.source, budget.deadline)?;
            if file_identity(&guard)? != root.source_identity {
                return Err("external_compile_original_identity_changed".into());
            }
            content_leases.push(ContentLock::shared(&root.source)?);
            crate::transactions::ensure_writable(&root.source)?;
            original_guards.push((root.source.clone(), root.source_identity.clone(), guard));
        }
        let envelope = crate::snapshot::open_checked_directory(&self.envelope, budget.deadline)?;
        crate::snapshot::check_opened_path(&envelope, &self.envelope, budget.deadline)?;
        if file_identity(&envelope)? != self.envelope_identity {
            return Err("external_compile_envelope_identity_changed".into());
        }
        let mut staged_guards = vec![(
            self.envelope.clone(),
            self.envelope_identity.clone(),
            envelope,
        )];
        for (path, expected, guard) in &self.directory_guards.0 {
            budget.check()?;
            crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
            if &file_identity(guard)? != expected {
                return Err("external_compile_staged_identity_changed".into());
            }
        }
        let mut original_usage = Usage::default();
        let mut staged_usage = Usage::default();
        let mut staged_file_guards = Vec::new();
        let mut staged_content = Vec::new();
        let mut content_encoded = NATIVE_CONTENT_ENCODING_BASE;
        let mut content_usage = Usage::default();
        for root in &self.roots {
            budget.check()?;
            let original = scan(&root.source, budget, &mut original_usage)?.1;
            if original != root.fingerprint {
                return Err("external_compile_original_fingerprint_changed".into());
            }
            let (rows, copied) = scan(&root.staged, budget, &mut staged_usage)?;
            if &copied
                != root
                    .proposed_fingerprint
                    .as_ref()
                    .unwrap_or(&root.fingerprint)
            {
                return Err("external_compile_staged_fingerprint_changed".into());
            }
            native_content_shape(
                &root.staged,
                &root.root_id,
                &rows,
                &mut content_encoded,
                &mut content_usage,
                budget,
            )?;
            let mut identities = Vec::with_capacity(rows.len());
            let guard = crate::snapshot::open_checked_directory(&root.staged, budget.deadline)?;
            if file_identity(&guard)? != root.staged_identity {
                return Err("external_compile_staged_identity_changed".into());
            }
            staged_guards.push((root.staged.clone(), root.staged_identity.clone(), guard));
            // Every copied directory stays held while descendants are compiled.
            for row in &rows {
                budget.check()?;
                let path = root.staged.join(&row.path);
                if row.directory {
                    let guard = crate::snapshot::open_checked_directory(&path, budget.deadline)?;
                    crate::snapshot::check_opened_path(&guard, &path, budget.deadline)?;
                    let identity = file_identity(&guard)?;
                    identities.push(identity.clone());
                    staged_guards.push((path, identity, guard));
                } else {
                    if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                        let (_, bytes) = read_file(&path, MANIFEST_BYTES, budget, None, None)?;
                        let shape = cargo_shape(&bytes)?;
                        if shape.package.is_some() && !shape.build_scripts_disabled {
                            return Err("external_compile_requires_explicit_build_false".into());
                        }
                    }
                    let guard = open_compile_source(&path, budget)?;
                    let expected = stamp(&guard)?;
                    if expected.bytes != row.bytes {
                        return Err("external_compile_staged_file_changed".into());
                    }
                    identities.push(expected.identity.clone());
                    staged_file_guards.push((path, expected, guard));
                }
            }
            staged_content.push(StagedContentInventory {
                root_id: root.root_id.clone(),
                path: root.staged.clone(),
                identity: root.staged_identity.clone(),
                fingerprint: copied,
                entries: rows,
                identities,
            });
        }
        let lease = CompilationLease {
            staged: self.clone(),
            _content_leases: content_leases,
            original_guards,
            staged_guards,
            staged_file_guards,
            staged_content,
            compiler_directories: Vec::new(),
            compiler_output_directories: Vec::new(),
            compiler_files: Vec::new(),
            compiler_links: Vec::new(),
        };
        lease.verify(budget)?;
        Ok(lease)
    }
}
impl CompilationLease {
    /// Duplicate only existing held input objects; never open an input by path.
    pub(crate) fn native_source_identity(
        &self,
        member: &Path,
        source: &Path,
        budget: &Budget<'_>,
    ) -> Result<NativeSourceIdentity, String> {
        self.verify(budget)?;
        let counts = [
            self.original_guards.len(),
            self.staged_guards.len(),
            self.staged_file_guards.len(),
            self.compiler_directories.len(),
            self.compiler_output_directories.len(),
            self.compiler_files.len(),
            self.compiler_links.len(),
            self.staged.directory_guards.0.len(),
        ];
        let duplicates = native_content_duplicate_count(
            self.staged_file_guards.len(),
            &self.staged.cargo_manifest,
            member,
        )?;
        native_provenance_guard_bound(&counts, duplicates)?;
        let mut charged = NATIVE_CONTENT_ENCODING_BASE;
        let mut usage = Usage::default();
        for root in &self.staged_content {
            native_content_shape(
                &root.path,
                &root.root_id,
                &root.entries,
                &mut charged,
                &mut usage,
                budget,
            )?;
        }
        if self.staged_content.len() != self.staged.roots.len() {
            return Err("external_provenance_content_binding_changed".into());
        }
        for (index, (path, _, _)) in self.staged_file_guards.iter().enumerate() {
            for (other, _, _) in &self.staged_file_guards[..index] {
                budget.check()?;
                if provenance_paths_equal(path, other) {
                    return Err("external_provenance_input_ambiguous".into());
                }
            }
        }
        if usage.files != self.staged_file_guards.len() {
            return Err("external_provenance_input_not_held".into());
        }
        if !provenance_paths_equal(member, &self.staged.member_manifest) {
            return Err("external_provenance_selected_manifest_changed".into());
        }
        let lock = self
            .staged
            .cargo_manifest
            .parent()
            .ok_or("external_provenance_workspace_missing")?
            .join("Cargo.lock");
        let requested = [
            ("workspace_manifest", self.staged.cargo_manifest.as_path()),
            ("selected_manifest", member),
            ("selected_source", source),
            ("lock", lock.as_path()),
        ];
        native_content_anchor_aliases(requested.map(|(_, path)| path))?;
        let mut envelope_match = None;
        for row in &self.staged_guards {
            budget.check()?;
            if provenance_paths_equal(&row.0, &self.staged.envelope) {
                if envelope_match.is_some() {
                    return Err("external_provenance_envelope_ambiguous".into());
                }
                envelope_match = Some(row);
            }
        }
        let (_, identity, envelope) =
            envelope_match.ok_or("external_provenance_envelope_not_held")?;
        crate::snapshot::check_opened_path(envelope, &self.staged.envelope, budget.deadline)?;
        if *identity != self.staged.envelope_identity || file_identity(envelope)? != *identity {
            return Err("external_provenance_envelope_changed".into());
        }
        let envelope_guard = envelope
            .try_clone()
            .map_err(|_| "external_provenance_handle_duplication_failed")?;
        let mut files = Vec::new();
        for (role, requested) in requested {
            budget.check()?;
            let _ = provenance_relative_path(&self.staged.envelope, requested)?;
            let mut matched = None;
            for row in &self.staged_file_guards {
                budget.check()?;
                if provenance_paths_equal(&row.0, requested) {
                    if matched.is_some() {
                        return Err("external_provenance_input_ambiguous".into());
                    }
                    matched = Some(row);
                }
            }
            let (path, expected, held) = matched.ok_or("external_provenance_input_not_held")?;
            crate::snapshot::check_opened_path(held, requested, budget.deadline)?;
            if expected.bytes > budget.max_file || stamp(held)? != *expected {
                return Err("external_provenance_input_changed".into());
            }
            let relative = provenance_relative_path(&self.staged.envelope, path)?;
            if relative.len() > 4096 {
                return Err("external_provenance_input_path_bound".into());
            }
            let sha256 = hash_provenance_input(path, held, budget)?;
            let guard = held
                .try_clone()
                .map_err(|_| "external_provenance_handle_duplication_failed")?;
            files.push(NativeProvenanceFile {
                role,
                path: path.clone(),
                expected: expected.clone(),
                sha256,
                guard,
            });
        }
        // Named anchors are first; other objects follow inventory rows, independent
        // of guard-vector ordering. The digest sorts roots and binds exact row order.
        for root in &self.staged_content {
            for row in &root.entries {
                budget.check()?;
                if row.directory {
                    continue;
                }
                let path = root.path.join(&row.path);
                if files
                    .iter()
                    .take(4)
                    .any(|file| provenance_paths_equal(&file.path, &path))
                {
                    continue;
                }
                let mut matched = None;
                for held in &self.staged_file_guards {
                    budget.check()?;
                    if provenance_paths_equal(&held.0, &path) {
                        if matched.is_some() {
                            return Err("external_provenance_input_ambiguous".into());
                        }
                        matched = Some(held);
                    }
                }
                let (path, expected, held) = matched.ok_or("external_provenance_input_not_held")?;
                crate::snapshot::check_opened_path(held, path, budget.deadline)?;
                if expected.bytes > budget.max_file || stamp(held)? != *expected {
                    return Err("external_provenance_input_changed".into());
                }
                let sha256 = hash_provenance_input(path, held, budget)?;
                let guard = held
                    .try_clone()
                    .map_err(|_| "external_provenance_handle_duplication_failed")?;
                files.push(NativeProvenanceFile {
                    role: "staged_content",
                    path: path.clone(),
                    expected: expected.clone(),
                    sha256,
                    guard,
                });
            }
        }
        let mut result = NativeSourceIdentity {
            envelope: self.staged.envelope.clone(),
            envelope_identity: identity.clone(),
            envelope_guard,
            files,
            content: self.staged_content.clone(),
            digest: String::new(),
        };
        result.digest = result.content_digest(&self.staged, budget)?;
        result.recheck(&self.staged, member, source, budget)?;
        self.verify(budget)?;
        Ok(result)
    }
    pub(crate) fn staged_envelope(&self) -> &Path {
        &self.staged.envelope
    }
    /// Metadata may spell Windows paths differently, but cannot choose new files.
    pub(crate) fn checked_metadata_file(
        &self,
        reported: &str,
        budget: &Budget<'_>,
    ) -> Result<PathBuf, String> {
        budget.check()?;
        let reported = checked_reported_path(reported)?;
        let opened = crate::snapshot::open_checked_file(&reported, budget.deadline)
            .map_err(|_| "external_metadata_file_path_refused")?;
        let actual = crate::project::clean_path(
            reported
                .canonicalize()
                .map_err(|_| "external_metadata_file_path_refused")?,
        );
        crate::snapshot::check_opened_path(&opened, &actual, budget.deadline)
            .map_err(|_| "external_metadata_file_path_refused")?;
        let observed = stamp(&opened)?;
        for (expected_path, expected, held) in &self.staged_file_guards {
            budget.check()?;
            if observed.identity != expected.identity {
                continue;
            }
            let canonical = crate::project::clean_path(
                expected_path
                    .canonicalize()
                    .map_err(|_| "external_metadata_file_guard_changed")?,
            );
            if !crate::ownership::paths_equal(&actual, &canonical) {
                continue;
            }
            crate::snapshot::check_opened_path(held, &canonical, budget.deadline)
                .map_err(|_| "external_metadata_file_guard_changed")?;
            if &stamp(held)? != expected || &observed != expected {
                return Err("external_metadata_file_guard_changed".into());
            }
            return Ok(canonical);
        }
        Err("external_metadata_file_not_held".into())
    }
    pub(crate) fn checked_metadata_workspace(
        &self,
        expected: &Path,
        reported: &str,
        budget: &Budget<'_>,
    ) -> Result<PathBuf, String> {
        let (_, identity, held) = self
            .staged_guards
            .iter()
            .chain(&self.staged.directory_guards.0)
            .find(|(path, _, _)| path == expected)
            .ok_or("external_metadata_workspace_not_held")?;
        checked_compiler_output_directory(
            expected,
            identity,
            held,
            reported,
            budget,
            CompilerDirectoryRole::Immutable,
        )?;
        Ok(crate::project::clean_path(
            expected
                .canonicalize()
                .map_err(|_| "external_metadata_workspace_guard_changed")?,
        ))
    }
    /// Bind the selected target to its unchanged copied manifest declaration.
    pub(crate) fn checked_library_source(
        &self,
        manifest: &Path,
        budget: &Budget<'_>,
    ) -> Result<PathBuf, String> {
        let manifest = self.checked_metadata_file(&manifest.to_string_lossy(), budget)?;
        let envelope = self.checked_metadata_workspace(
            &self.staged.envelope,
            &self.staged.envelope.to_string_lossy(),
            budget,
        )?;
        let (_, bytes) = read_file(&manifest, MANIFEST_BYTES, budget, None, None)?;
        let library = cargo_shape(&bytes)?
            .library
            .ok_or("external_selected_library_missing")?;
        let relative = manifest
            .strip_prefix(&envelope)
            .map_err(|_| "external_selected_manifest_outside_stage")?
            .to_str()
            .ok_or("external_selected_manifest_encoding")?
            .replace('\\', "/");
        let source = envelope.join(normalize(directory_of(&relative), &library)?);
        self.checked_metadata_file(&source.to_string_lossy(), budget)
    }
    /// The output root is host-selected and held before Cargo runs. The new
    /// artifact is opened through the same checked ancestor policy and stays held.
    pub(crate) fn reported_output_root(
        &self,
        target: &Path,
        budget: &Budget<'_>,
    ) -> Result<NativeReportedOutputRoot, String> {
        self.verify_compiler_output_directory(target, &target.to_string_lossy(), budget)?;
        let (_, identity, held) = self
            .compiler_output_directories
            .iter()
            .find(|(path, _, _)| path == target)
            .ok_or("external_compile_output_directory_unheld")?;
        let canonical = crate::project::clean_path(
            target
                .canonicalize()
                .map_err(|_| "external_reported_output_root_changed")?,
        );
        crate::snapshot::check_opened_path(held, &canonical, budget.deadline)?;
        let guard = held
            .try_clone()
            .map_err(|_| "external_reported_output_handle_duplication")?;
        let root = NativeReportedOutputRoot {
            target: target.to_path_buf(),
            canonical,
            identity: identity.clone(),
            guard,
        };
        root.recheck(budget)?;
        Ok(root)
    }
    pub(crate) fn checked_artifact_file(
        &self,
        target: &Path,
        reported: &str,
        budget: &Budget<'_>,
    ) -> Result<(PathBuf, File), String> {
        self.verify_compiler_output_directory(target, &target.to_string_lossy(), budget)?;
        let reported = checked_reported_path(reported)?;
        let opened = crate::snapshot::open_checked_file(&reported, budget.deadline)
            .map_err(|_| "external_artifact_file_path_refused")?;
        let canonical = crate::project::clean_path(
            reported
                .canonicalize()
                .map_err(|_| "external_artifact_file_path_refused")?,
        );
        let root = crate::project::clean_path(
            target
                .canonicalize()
                .map_err(|_| "external_artifact_root_changed")?,
        );
        if !canonical.starts_with(&root) {
            return Err("external_artifact_file_outside_target".into());
        }
        crate::snapshot::check_opened_path(&opened, &canonical, budget.deadline)
            .map_err(|_| "external_artifact_file_path_refused")?;
        budget.check()?;
        Ok((canonical, opened))
    }
    /// Expected is selected by host compiler state, never by reported metadata.
    /// Both checked canonical spelling and the original held identity must match.
    pub(crate) fn verify_compiler_output_directory(
        &self,
        expected: &Path,
        reported: &str,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        let (_, identity, held) = self
            .compiler_output_directories
            .iter()
            .find(|(path, _, _)| path == expected)
            .ok_or("external_compile_output_directory_unheld")?;
        checked_compiler_output_directory(
            expected,
            identity,
            held,
            reported,
            budget,
            CompilerDirectoryRole::WritableOutput,
        )
    }
    /// Retain host state/tool guards with source leases on uncertain retirement.
    pub(crate) fn hold_compiler_state(
        mut self,
        compiler_root: &Path,
        directories: &[PathBuf],
        output_directories: &[PathBuf],
        files: &[PathBuf],
        programs: &[(PathBuf, PathBuf)],
        budget: &Budget<'_>,
    ) -> Result<Self, String> {
        if directories
            .len()
            .checked_add(output_directories.len())
            .is_none_or(|count| count > 16)
            || files.len() > 8
            || programs.len() > 2
        {
            return Err("external_compile_host_guard_bounds".into());
        }
        let expected_root = self
            .staged
            .envelope
            .parent()
            .ok_or("external_compile_output_root_refused")?
            .join("compiler-state");
        let (expected_root, expected_identity) = canonical_directory(&expected_root, budget)?;
        let (compiler_root, root_identity) = canonical_directory(compiler_root, budget)?;
        if compiler_root != expected_root || root_identity != expected_identity {
            return Err("external_compile_output_root_refused".into());
        }
        let allowed = compiler_output_directory_paths(&compiler_root);
        let mut output_paths = BTreeSet::new();
        for path in output_directories {
            budget.check()?;
            let canonical = crate::project::clean_path(
                path.canonicalize()
                    .map_err(|_| "external_compile_output_role_refused")?,
            );
            if !allowed.contains(&canonical) || !output_paths.insert(canonical) {
                return Err("external_compile_output_role_refused".into());
            }
        }
        let mut root_held = false;
        for path in directories {
            budget.check()?;
            let canonical = crate::project::clean_path(
                path.canonicalize()
                    .map_err(|_| "external_compile_host_guard_refused")?,
            );
            if output_paths.contains(&canonical) {
                return Err("external_compile_output_role_conflict".into());
            }
            root_held |= canonical == compiler_root;
            let guard = crate::snapshot::open_checked_directory(path, budget.deadline)?;
            let identity = file_identity(&guard)?;
            if !self
                .compiler_directories
                .iter()
                .any(|(_, held, _)| held == &identity)
            {
                self.compiler_directories
                    .push((path.clone(), identity, guard));
            }
        }
        if !root_held {
            return Err("external_compile_output_root_unheld".into());
        }
        for path in output_directories {
            budget.check()?;
            let guard =
                crate::snapshot::open_checked_writable_directory_identity(path, budget.deadline)?;
            let identity = file_identity(&guard)?;
            if self
                .original_guards
                .iter()
                .chain(&self.staged_guards)
                .chain(&self.staged.directory_guards.0)
                .chain(&self.compiler_directories)
                .any(|(_, held_identity, _)| held_identity == &identity)
            {
                return Err("external_compile_output_role_conflict".into());
            }
            self.compiler_output_directories
                .push((path.clone(), identity, guard));
        }
        for path in files {
            budget.check()?;
            let guard = open_compile_source(path, budget)?;
            self.compiler_files
                .push((path.clone(), stamp(&guard)?, guard));
        }
        let mut ancestors = BTreeSet::new();
        let mut applications = BTreeSet::new();
        for (invocation, application) in programs {
            budget.check()?;
            for path in [invocation, application] {
                for ancestor in path.ancestors().skip(1) {
                    if ancestors.len() >= 64 {
                        return Err("external_compile_tool_ancestor_bounds".into());
                    }
                    ancestors.insert(ancestor.to_path_buf());
                }
            }
            applications.insert(application.clone());
        }
        let additional = ancestors.len() + applications.len() + programs.len();
        let existing = self.original_guards.len()
            + self.staged_guards.len()
            + self.staged_file_guards.len()
            + self.staged.directory_guards.0.len()
            + self.compiler_directories.len()
            + self.compiler_output_directories.len()
            + self.compiler_files.len();
        if existing
            .checked_add(additional)
            .is_none_or(|count| count > 8192)
        {
            return Err("external_compile_guard_limit".into());
        }
        for path in ancestors {
            budget.check()?;
            let canonical = crate::project::clean_path(
                path.canonicalize()
                    .map_err(|_| "external_compile_tool_ancestor_refused")?,
            );
            if output_paths.contains(&canonical) {
                return Err("external_compile_output_role_conflict".into());
            }
            let guard = crate::snapshot::open_checked_directory(&path, budget.deadline)?;
            self.compiler_directories
                .push((path, file_identity(&guard)?, guard));
        }
        for path in applications {
            let guard = open_compile_source(&path, budget)?;
            self.compiler_files.push((path, stamp(&guard)?, guard));
        }
        for (invocation, application) in programs {
            budget.check()?;
            let metadata = std::fs::symlink_metadata(invocation)
                .map_err(|_| "external_compile_invocation_unavailable")?;
            if metadata.file_type().is_symlink() {
                // Hold the link object and ancestors, denying writes/deletion.
                use std::os::windows::fs::OpenOptionsExt;
                let guard = OpenOptions::new()
                    .read(true)
                    .custom_flags(0x0020_0000)
                    .share_mode(1)
                    .open(invocation)
                    .map_err(|_| "external_compile_invocation_guard_refused")?;
                if !guard
                    .metadata()
                    .map_err(|_| "external_compile_invocation_guard_refused")?
                    .file_type()
                    .is_symlink()
                {
                    return Err("external_compile_invocation_guard_refused".into());
                }
                let actual = invocation
                    .canonicalize()
                    .map(crate::project::clean_path)
                    .map_err(|_| "external_compile_invocation_changed")?;
                if !crate::ownership::paths_equal(&actual, application) {
                    return Err("external_compile_invocation_changed".into());
                }
                self.compiler_links.push((
                    invocation.clone(),
                    application.clone(),
                    file_identity(&guard)?,
                    guard,
                ));
            } else {
                let guard = open_compile_source(invocation, budget)?;
                self.compiler_files
                    .push((invocation.clone(), stamp(&guard)?, guard));
            }
        }
        self.verify(budget)?;
        Ok(self)
    }
    pub(crate) fn verify(&self, budget: &Budget<'_>) -> Result<(), String> {
        budget.check()?;
        for (path, expected, guard) in self
            .original_guards
            .iter()
            .chain(&self.staged_guards)
            .chain(&self.compiler_directories)
            .chain(&self.compiler_output_directories)
        {
            budget.check()?;
            crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
            if &file_identity(guard)? != expected {
                return Err("external_compile_held_directory_changed".into());
            }
        }
        for (path, application, expected, guard) in &self.compiler_links {
            budget.check()?;
            if &file_identity(guard)? != expected {
                return Err("external_compile_invocation_changed".into());
            }
            let actual = path
                .canonicalize()
                .map(crate::project::clean_path)
                .map_err(|_| "external_compile_invocation_changed")?;
            if !crate::ownership::paths_equal(&actual, application) {
                return Err("external_compile_invocation_changed".into());
            }
        }
        for (path, expected, guard) in &self.staged.directory_guards.0 {
            budget.check()?;
            crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
            if &file_identity(guard)? != expected {
                return Err("external_compile_held_directory_changed".into());
            }
        }
        for (path, expected, guard) in self.staged_file_guards.iter().chain(&self.compiler_files) {
            budget.check()?;
            crate::snapshot::check_opened_path(guard, path, budget.deadline)?;
            if &stamp(guard)? != expected {
                return Err("external_compile_staged_file_changed".into());
            }
        }
        let mut original_usage = Usage::default();
        let mut staged_usage = Usage::default();
        for root in &self.staged.roots {
            budget.check()?;
            if scan(&root.source, budget, &mut original_usage)?.1 != root.fingerprint {
                return Err("external_compile_original_fingerprint_changed".into());
            }
            if &scan(&root.staged, budget, &mut staged_usage)?.1
                != root
                    .proposed_fingerprint
                    .as_ref()
                    .unwrap_or(&root.fingerprint)
            {
                return Err("external_compile_staged_fingerprint_changed".into());
            }
        }
        budget.check()
    }
}
fn checked_reported_path(reported: &str) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let path = PathBuf::from(reported.replace('/', "\\"));
    #[cfg(not(windows))]
    let path = PathBuf::from(reported);
    if reported.len() > 4096
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("external_reported_path_refused".into());
    }
    Ok(path)
}

#[cfg(test)]
pub(crate) struct KnownCargoFixturePermit {
    artifact_root: PathBuf,
    workspace: PathBuf,
    member: PathBuf,
    envelope: PathBuf,
}
#[cfg(test)]
impl KnownCargoFixturePermit {
    pub(crate) fn check(
        &self,
        workspace: &crate::candidate_workspace::Workspace,
        staged: &StagedSources,
    ) -> std::io::Result<()> {
        let valid = self.artifact_root.is_absolute()
            && self
                .artifact_root
                .to_string_lossy()
                .get(..2)
                .is_some_and(|p| p.eq_ignore_ascii_case(&crate::test_drive()))
            && self.workspace == workspace.path
            && self.member == staged.member_manifest
            && self.envelope == staged.envelope
            && staged.member_manifest.starts_with(&staged.envelope)
            && staged.envelope.starts_with(&workspace.path)
            && std::fs::symlink_metadata(&self.artifact_root)
                .ok()
                .is_some_and(|m| m.is_dir() && ordinary(&m).is_ok());
        if valid {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "known Cargo fixture binding refused",
            ))
        }
    }
    pub(crate) fn into_artifact_root(self) -> PathBuf {
        self.artifact_root
    }
}
#[cfg(test)]
#[path = "fixture_all_entry_oracle.rs"]
pub(crate) mod fixture_all_entry_oracle;
#[cfg(test)]
mod tests {
    use super::*;
    const PRIVATE_BINDING_MANIFEST: &str = "[configuration]\nentry_symbol=\"value\"\ncompatibility_minimum=\"4.1\"\nreloadable=false\n[libraries]\nwindows.debug.x86_64=\"res://bin/external_logic.dll\"\nwindows.release.x86_64=\"res://bin/release.dll\"\n";
    #[test]
    fn configfile_parser_strict_document_barrier_rejects_variants_dependencies_and_ambiguity() {
        let document = configfile_parser_document(PRIVATE_BINDING_MANIFEST).unwrap();
        assert_eq!(document["dependencies"], serde_json::json!([]));
        assert_eq!(document["sections"][0]["name"], "configuration");
        for text in [
            PRIVATE_BINDING_MANIFEST.replace(
                "entry_symbol=\"value\"",
                "entry_symbol=Object(\"Resource\")",
            ),
            format!("{PRIVATE_BINDING_MANIFEST}\n[dependencies]\nwindows=[]\n"),
            format!("{PRIVATE_BINDING_MANIFEST}\nwindows=\"res://bin/ambiguous.dll\"\n"),
            PRIVATE_BINDING_MANIFEST.replace("res://bin/external_logic.dll", "res://../escape.dll"),
        ] {
            assert!(configfile_parser_document(&text).is_err());
        }
    }
    fn seed_private_gdextension_fixture(fixture: &Fixture) {
        std::fs::create_dir_all(fixture.app.join("bin")).unwrap();
        std::fs::write(
            fixture.app.join("bin/checked.gdextension"),
            PRIVATE_BINDING_MANIFEST,
        )
        .unwrap();
    }
    #[test]
    fn private_gdextension_mapping_selects_explicit_debug_without_borrowing_other_profiles() {
        let (library, entry) = private_gdextension_mapping(PRIVATE_BINDING_MANIFEST).unwrap();
        assert_eq!(library, "res://bin/external_logic.dll");
        assert_eq!(entry, "value");
        let quoted = PRIVATE_BINDING_MANIFEST
            .replace("windows.debug.x86_64=", "\"windows.debug.x86_64\"=")
            .replace("\n", "\r\n");
        assert_eq!(
            private_gdextension_mapping(&quoted).unwrap(),
            (library.clone(), entry.clone())
        );
        let comments = format!(
            "# ignored \\ comment\n{}; trailing \\ comment\n",
            PRIVATE_BINDING_MANIFEST
        );
        assert_eq!(
            private_gdextension_mapping(&comments).unwrap(),
            (library, entry)
        );
        let release_only = PRIVATE_BINDING_MANIFEST.replace(
            "windows.debug.x86_64=\"res://bin/external_logic.dll\"\n",
            "",
        );
        assert_eq!(
            private_gdextension_mapping(&release_only).unwrap_err(),
            "external_private_binding_library_missing"
        );
        let other_arch =
            PRIVATE_BINDING_MANIFEST.replace("windows.debug.x86_64", "windows.debug.arm64");
        assert_eq!(
            private_gdextension_mapping(&other_arch).unwrap_err(),
            "external_private_binding_library_missing"
        );
    }
    #[test]
    fn private_gdextension_mapping_refuses_dependencies_variants_escapes_and_ambiguous_choices() {
        for manifest in [
            format!("{PRIVATE_BINDING_MANIFEST}[dependencies]\n"),
            PRIVATE_BINDING_MANIFEST.replace(
                "\"res://bin/external_logic.dll\"",
                "preload(\"res://bin/external_logic.dll\")",
            ),
            PRIVATE_BINDING_MANIFEST.replace(
                "res://bin/external_logic.dll",
                "res:\\u002f\\u002fbin/external_logic.dll",
            ),
            format!("{PRIVATE_BINDING_MANIFEST}windows=\"res://bin/fallback.dll\"\n"),
            format!("{PRIVATE_BINDING_MANIFEST}windows.debug.x86_64=\"res://bin/duplicate.dll\"\n"),
            format!("{PRIVATE_BINDING_MANIFEST}[libraries]\nwindows=\"res://bin/other.dll\"\n"),
            PRIVATE_BINDING_MANIFEST.replace("res://bin/external_logic.dll", "res://../escape.dll"),
            PRIVATE_BINDING_MANIFEST.replace("res://bin/external_logic.dll", "A:/foreign.dll"),
            PRIVATE_BINDING_MANIFEST.replace(
                "res://bin/external_logic.dll",
                "res://bin/external_logic.so",
            ),
            PRIVATE_BINDING_MANIFEST.replace("entry_symbol=\"value\"", "entry_symbol=&\"value\""),
            PRIVATE_BINDING_MANIFEST.replace("reloadable=false", "reloadable=Vector2(1,2)"),
        ] {
            assert!(
                private_gdextension_mapping(&manifest).is_err(),
                "unsupported subset was admitted"
            );
        }
        assert!(
            private_gdextension_mapping(&format!("\u{feff}{PRIVATE_BINDING_MANIFEST}")).is_err()
        );
        assert!(private_gdextension_mapping(&format!("{PRIVATE_BINDING_MANIFEST}\0")).is_err());
        assert!(
            private_gdextension_mapping(&"x".repeat(PRIVATE_GDEXTENSION_BYTES as usize + 1))
                .is_err()
        );
    }
    #[test]
    fn private_gdextension_paths_bind_one_godot_root_and_refuse_unsafe_destinations() {
        assert_eq!(
            private_gdextension_library_relative(
                "godot/addons/game/checked.gdextension",
                "godot/project.godot",
                "res://bin/game.dll"
            )
            .unwrap(),
            "godot/bin/game.dll"
        );
        assert_eq!(
            private_gdextension_library_relative(
                "godot/addons/game/checked.gdextension",
                "godot/project.godot",
                "game.dll"
            )
            .unwrap(),
            "godot/addons/game/game.dll"
        );
        for path in [
            "../escape.dll",
            "/absolute.dll",
            "C:/absolute.dll",
            "res://../escape.dll",
            "res://bin/NUL.dll",
            "res://bin/COM1.dll",
            "res://bin/trailing./game.dll",
            "res://bin/percent%2e.dll",
            "res://bin\\escape.dll",
        ] {
            assert!(private_gdextension_library_relative(
                "godot/bin/checked.gdextension",
                "godot/project.godot",
                path
            )
            .is_err());
        }
        assert!(private_gdextension_library_relative(
            "other/checked.gdextension",
            "godot/project.godot",
            "other.dll"
        )
        .is_err());
    }
    #[cfg(windows)]
    #[test]
    fn private_gdextension_manifest_guard_survives_retirement_and_refuses_foreign_witness() {
        let fixture = Fixture::new();
        seed_private_gdextension_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline()).unwrap();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage_owned(&envelope, &b)
            .unwrap();
        drop(envelope);
        let source = staged.member_manifest.parent().unwrap().join("src/lib.rs");
        let lease = std::sync::Arc::new(staged.compilation_lease(&b).unwrap());
        workspace.retain_compilation(lease.clone()).unwrap();
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let manifest_path = staged.envelope.join("app/bin/checked.gdextension");
        let held = witness
            .files
            .iter()
            .find(|file| provenance_paths_equal(&file.path, &manifest_path))
            .unwrap();
        std::io::Seek::seek(&mut &held.guard, std::io::SeekFrom::Start(9)).unwrap();
        let weak = std::sync::Arc::downgrade(&lease);
        drop(lease);
        let ownership = workspace.retire_compilation_for_publication(&b).unwrap();
        assert!(weak.upgrade().is_none());
        assert!(workspace.source_envelope(b.deadline()).is_err());
        // Held-input selection only: no real compiler provenance is synthesized.
        let selected = witness
            .gdextension_manifest(
                &RootId::new("project").unwrap(),
                "bin/checked.gdextension",
                &b,
            )
            .unwrap();
        assert_eq!(selected.library_relative, "bin/external_logic.dll");
        assert_eq!(selected.entry_symbol, "value");
        assert_eq!(
            selected.manifest.sha256,
            crate::sha256_hex(PRIVATE_BINDING_MANIFEST.as_bytes())
        );
        assert_eq!(selected.project.relative, "project.godot");
        assert_eq!(std::io::Seek::stream_position(&mut &held.guard).unwrap(), 9);
        assert!(OpenOptions::new().write(true).open(&manifest_path).is_err());
        let other = Fixture::new();
        seed_private_gdextension_fixture(&other);
        let (other_staged, other_lease, other_source) = provenance_fixture_stage(&other, &b);
        let foreign = other_lease
            .native_source_identity(&other_staged.member_manifest, &other_source, &b)
            .unwrap();
        assert!(foreign.recheck_local_input(&selected.manifest, &b).is_err());
        assert_eq!(
            witness
                .gdextension_manifest(
                    &RootId::new("project").unwrap(),
                    "bin/checked.gdextension",
                    &b
                )
                .unwrap()
                .manifest,
            selected.manifest
        );
        drop(foreign);
        drop(other_lease);
        drop(other_staged);
        drop(witness);
        drop(ownership);
        drop(staged);
        workspace.clean().unwrap();
        assert!(!workspace.path.exists());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn private_gdextension_manifest_selection_refuses_missing_ambiguous_and_tampered_inputs() {
        let fixture = Fixture::new();
        seed_private_gdextension_fixture(&fixture);
        let b = budget();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let root = RootId::new("project").unwrap();
        assert!(witness
            .gdextension_manifest(
                &RootId::new("ungranted").unwrap(),
                "bin/checked.gdextension",
                &b
            )
            .is_err());
        assert!(witness
            .gdextension_manifest(&root, "../outside.gdextension", &b)
            .is_err());
        assert!(witness
            .gdextension_manifest(&root, "bin/missing.gdextension", &b)
            .is_err());
        let path = staged.envelope.join("app/bin/checked.gdextension");
        let index = witness
            .files
            .iter()
            .position(|file| provenance_paths_equal(&file.path, &path))
            .unwrap();
        witness.files[index].sha256.push('0');
        assert!(witness
            .gdextension_manifest(&root, "bin/checked.gdextension", &b)
            .is_err());
        witness.files[index].sha256.pop();
        witness.files[index].expected.identity.volume ^= 1;
        assert!(witness
            .gdextension_manifest(&root, "bin/checked.gdextension", &b)
            .is_err());
        witness.files[index].expected.identity.volume ^= 1;
        let duplicate = NativeProvenanceFile {
            role: witness.files[index].role,
            path: witness.files[index].path.clone(),
            expected: witness.files[index].expected.clone(),
            sha256: witness.files[index].sha256.clone(),
            guard: witness.files[index].guard.try_clone().unwrap(),
        };
        witness.files.push(duplicate);
        assert!(witness
            .gdextension_manifest(&root, "bin/checked.gdextension", &b)
            .is_err());
        witness.files.pop();
        witness
            .gdextension_manifest(&root, "bin/checked.gdextension", &b)
            .unwrap();
        let ambiguous = Fixture::new();
        seed_private_gdextension_fixture(&ambiguous);
        std::fs::write(
            ambiguous.app.join("bin/project.godot"),
            "config_version=5\n",
        )
        .unwrap();
        let (nested_staged, nested_lease, nested_source) = provenance_fixture_stage(&ambiguous, &b);
        let nested = nested_lease
            .native_source_identity(&nested_staged.member_manifest, &nested_source, &b)
            .unwrap();
        assert!(
            matches!(nested.gdextension_manifest(&root, "bin/checked.gdextension", &b),
            Err(error) if error == "external_private_binding_godot_root_ambiguous")
        );
    }
    #[cfg(windows)]
    #[test]
    fn private_gdextension_held_read_bounds_utf8_cancel_and_expiry_preserve_cursor() {
        let fixture = Fixture::new();
        let b = budget();
        let path = fixture.base.join("held.gdextension");
        let text = format!(
            "{PRIVATE_BINDING_MANIFEST}{}",
            "; bounded comment\n".repeat(1024)
        );
        std::fs::write(&path, &text).unwrap();
        let guard = open_compile_source(&path, &b).unwrap();
        let mut held = NativeProvenanceFile {
            role: "staged_content",
            path,
            expected: stamp(&guard).unwrap(),
            sha256: crate::sha256_hex(text.as_bytes()),
            guard,
        };
        std::io::Seek::seek(&mut &held.guard, std::io::SeekFrom::Start(7)).unwrap();
        assert_eq!(held_gdextension_text(&held, &b).unwrap(), text);
        assert_eq!(std::io::Seek::stream_position(&mut &held.guard).unwrap(), 7);
        let cancel = || {
            if std::io::Seek::stream_position(&mut &held.guard).unwrap() >= 8192 {
                Err("private_binding_test_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cancelled = b.with_check(&cancel);
        assert_eq!(
            held_gdextension_text(&held, &cancelled).unwrap_err(),
            "private_binding_test_cancelled"
        );
        assert_eq!(std::io::Seek::stream_position(&mut &held.guard).unwrap(), 7);
        let mut expired = b.with_check(b.check_cancelled);
        expired.deadline = Instant::now();
        assert_eq!(
            held_gdextension_text(&held, &expired).unwrap_err(),
            "external_staging_deadline"
        );
        assert_eq!(std::io::Seek::stream_position(&mut &held.guard).unwrap(), 7);
        held.expected.bytes = PRIVATE_GDEXTENSION_BYTES + 1;
        assert_eq!(
            held_gdextension_text(&held, &b).unwrap_err(),
            "external_private_binding_text_bound"
        );
        assert_eq!(std::io::Seek::stream_position(&mut &held.guard).unwrap(), 7);
        let invalid_path = fixture.base.join("invalid.gdextension");
        std::fs::write(&invalid_path, [0xff, 0xfe, 0xfd]).unwrap();
        let guard = open_compile_source(&invalid_path, &b).unwrap();
        let invalid = NativeProvenanceFile {
            role: "staged_content",
            path: invalid_path,
            expected: stamp(&guard).unwrap(),
            sha256: crate::sha256_hex(&[0xff, 0xfe, 0xfd]),
            guard,
        };
        std::io::Seek::seek(&mut &invalid.guard, std::io::SeekFrom::Start(1)).unwrap();
        assert_eq!(
            held_gdextension_text(&invalid, &b).unwrap_err(),
            "external_private_binding_utf8_required"
        );
        assert_eq!(
            std::io::Seek::stream_position(&mut &invalid.guard).unwrap(),
            1
        );
    }
    #[cfg(windows)]
    fn provenance_fixture_stage(
        fixture: &Fixture,
        b: &Budget<'_>,
    ) -> (StagedSources, std::sync::Arc<CompilationLease>, PathBuf) {
        let staged = fixture
            .plan(Mode::DependencyRead, b)
            .stage(&fixture.base.join("provenance-stage"), b)
            .unwrap();
        let source = staged.member_manifest.parent().unwrap().join("src/lib.rs");
        let lease = std::sync::Arc::new(staged.compilation_lease(b).unwrap());
        (staged, lease, source)
    }
    #[cfg(windows)]
    #[test]
    fn external_source_provenance_held_cursor_restores_on_success_and_cancel() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let path = fixture.base.join("cursor-input");
        let contents = b"ordinary bounded provenance cursor fixture\n";
        std::fs::write(&path, contents).unwrap();
        let guard = open_compile_source(&path, &b).unwrap();
        let mut reader = &guard;
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(3)).unwrap();
        assert_eq!(
            hash_provenance_input(&path, &guard, &b).unwrap(),
            crate::sha256_hex(contents)
        );
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 3);
        let calls = std::cell::Cell::new(0usize);
        let cancel_during_hash = || {
            calls.set(calls.get() + 1);
            if calls.get() == 4 {
                Err("fixture_provenance_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cancelled = b.with_check(&cancel_during_hash);
        assert_eq!(
            hash_provenance_input(&path, &guard, &cancelled)
                .err()
                .unwrap(),
            "fixture_provenance_cancelled"
        );
        assert_eq!(calls.get(), 4);
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 3);
        assert_eq!(
            hash_provenance_input(&fixture.base.join("foreign-cursor-path"), &guard, &b)
                .err()
                .unwrap(),
            "opened file resolved away from its declared path"
        );
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 3);
        assert_eq!(std::fs::read(&path).unwrap(), contents);
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_provenance_capture_cancel_restores_shared_cursor_and_drops_partials() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let (_, _, held) = lease
            .staged_file_guards
            .iter()
            .find(|(path, _, _)| path == &source)
            .unwrap();
        let mut reader = held;
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(3)).unwrap();
        let triggered = std::cell::Cell::new(false);
        let stop_on_shared_cursor_movement = || {
            let mut reader = held;
            if std::io::Seek::stream_position(&mut reader).unwrap() != 3 {
                triggered.set(true);
                Err("fixture_provenance_capture_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cancelled = b.with_check(&stop_on_shared_cursor_movement);
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &source, &cancelled)
                .err()
                .unwrap(),
            "fixture_provenance_capture_cancelled"
        );
        assert!(triggered.get());
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 3);
        assert_eq!(std::sync::Arc::strong_count(&lease), 1);
        let source_bytes = std::fs::read(&source).unwrap();
        drop(lease);
        // Cancellation after two manifest duplicates must drop every partial witness.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&staged.cargo_manifest)
                .unwrap(),
        );
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .unwrap(),
        );
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_provenance_inputs_refuse_missing_and_controlled_binding_drift() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let (staged, mut lease, source) = provenance_fixture_stage(&fixture, &b);
        let member_slash =
            PathBuf::from(staged.member_manifest.to_str().unwrap().replace('\\', "/"));
        let member_backslash =
            PathBuf::from(staged.member_manifest.to_str().unwrap().replace('/', "\\"));
        let source_slash = PathBuf::from(source.to_str().unwrap().replace('\\', "/"));
        let source_backslash = PathBuf::from(source.to_str().unwrap().replace('/', "\\"));
        assert_ne!(source_slash.to_str(), source_backslash.to_str());
        assert!(provenance_paths_equal(&member_slash, &member_backslash));
        assert!(provenance_paths_equal(&source_slash, &source_backslash));
        let equivalent = lease
            .native_source_identity(&member_slash, &source_backslash, &b)
            .unwrap();
        equivalent
            .recheck(
                &staged,
                &PathBuf::from(member_backslash.to_str().unwrap().to_ascii_uppercase()),
                &PathBuf::from(source_slash.to_str().unwrap().to_ascii_uppercase()),
                &b,
            )
            .unwrap();
        drop(equivalent);
        let outside = fixture.base.join("outside-source.rs");
        assert!(!provenance_paths_equal(&source, &outside));
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &outside, &b)
                .err()
                .unwrap(),
            "external_provenance_input_outside_envelope"
        );
        let parent_dir = source.parent().unwrap().join("..").join("src/lib.rs");
        assert!(!provenance_paths_equal(&source, &parent_dir));
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &parent_dir, &b)
                .err()
                .unwrap(),
            "external_provenance_input_path_invalid"
        );
        let changed_prefix = PathBuf::from(format!(r"\\?\{}", source_backslash.display()));
        assert!(!provenance_paths_equal(&source, &changed_prefix));
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &changed_prefix, &b)
                .err()
                .unwrap(),
            "external_provenance_input_outside_envelope"
        );
        let different_input = source.parent().unwrap().join("different-file.rs");
        assert!(!provenance_paths_equal(&source, &different_input));
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &different_input, &b)
                .err()
                .unwrap(),
            "external_provenance_input_not_held"
        );
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        witness
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
        let role = witness.files[3].role;
        witness.files[3].role = "selected_source";
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[3].role = role;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        let path = std::mem::replace(
            &mut witness.files[3].path,
            fixture.base.join("foreign-lock"),
        );
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[3].path = path;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        witness.files[3].expected.identity.volume ^= 1;
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[3].expected.identity.volume ^= 1;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        witness.files[3].sha256.push('0');
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[3].sha256.pop();
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        let foreign = fixture.base.join("foreign-held-input");
        std::fs::write(&foreign, b"foreign ordinary input must remain untouched\n").unwrap();
        let held = std::mem::replace(
            &mut witness.files[3].guard,
            open_compile_source(&foreign, &b).unwrap(),
        );
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[3].guard = held;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        witness
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
        drop(witness);
        {
            let lease = std::sync::Arc::get_mut(&mut lease).unwrap();
            let index = lease
                .staged_file_guards
                .iter()
                .position(|(path, _, _)| path == &source)
                .unwrap();
            let row = &lease.staged_file_guards[index];
            let alias = PathBuf::from(row.0.to_str().unwrap().replace('\\', "/"));
            let duplicate = (alias, row.1.clone(), row.2.try_clone().unwrap());
            lease.staged_file_guards.push(duplicate);
            let observed = lease.native_source_identity(&member_backslash, &source_slash, &b);
            lease.staged_file_guards.pop();
            assert_eq!(
                observed.err().unwrap(),
                "external_provenance_input_ambiguous"
            );
        }
        let lock = staged.cargo_manifest.parent().unwrap().join("Cargo.lock");
        for missing in [&lock, &source] {
            let lease = std::sync::Arc::get_mut(&mut lease).unwrap();
            let index = lease
                .staged_file_guards
                .iter()
                .position(|(path, _, _)| path == missing)
                .unwrap();
            let held = lease.staged_file_guards.remove(index);
            let observed = lease.native_source_identity(&staged.member_manifest, &source, &b);
            lease.staged_file_guards.insert(index, held);
            assert_eq!(
                observed.err().unwrap(),
                "external_provenance_input_not_held"
            );
        }
        lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap()
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(
            std::fs::read(&foreign).unwrap(),
            b"foreign ordinary input must remain untouched\n"
        );
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_provenance_witness_preserves_unique_retirement_and_file_drop_boundary() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline()).unwrap();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage_owned(&envelope, &b)
            .unwrap();
        drop(envelope);
        let source = staged.member_manifest.parent().unwrap().join("src/lib.rs");
        let contents = std::fs::read(&source).unwrap();
        let lease = std::sync::Arc::new(staged.compilation_lease(&b).unwrap());
        workspace.retain_compilation(lease.clone()).unwrap();
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(std::sync::Arc::strong_count(&lease), 2);
        assert_eq!(
            workspace
                .retire_compilation_for_publication(&b)
                .err()
                .unwrap(),
            "external_compile_retirement_unresolved_or_aliased"
        );
        let weak = std::sync::Arc::downgrade(&lease);
        drop(lease);
        let ownership = workspace.retire_compilation_for_publication(&b).unwrap();
        assert!(weak.upgrade().is_none());
        assert!(!workspace.lifetime.unresolved());
        witness
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
        let input_paths = [
            staged.cargo_manifest.clone(),
            staged.member_manifest.clone(),
            source.clone(),
            staged.cargo_manifest.parent().unwrap().join("Cargo.lock"),
        ];
        for path in &input_paths {
            assert!(std::fs::OpenOptions::new().write(true).open(path).is_err());
        }
        drop(witness);
        for path in &input_paths {
            drop(std::fs::OpenOptions::new().write(true).open(path).unwrap());
        }
        assert_eq!(std::fs::read(&source).unwrap(), contents);
        drop(ownership);
        drop(staged);
        workspace.clean().unwrap();
        assert!(!workspace.path.exists());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_provenance_true_receipt_and_synthetic_marker_cannot_seal_real_candidate() {
        let fixture = Fixture::new();
        seed_publication_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (prepared, candidate, mut workspace) =
            private_native_binding_fixture(&fixture, &broker, &b);
        assert_eq!(
            candidate.compiler_provenance.real_digest().err().unwrap(),
            "external_source_synthetic_provenance_refused"
        );
        let ValidatedSourceCandidate {
            proposal,
            artifact,
            artifact_guard,
            artifact_hash,
            ..
        } = candidate;
        let unproven = crate::external_native::OwnedNativeValidation::unproven_for_test(
            artifact,
            artifact_guard,
            artifact_hash,
        );
        assert_eq!(
            proposal
                .seal_validation(&mut workspace, unproven, &b, true)
                .err()
                .unwrap(),
            "external_source_real_compiler_provenance_missing"
        );
        drop(prepared);
        workspace.clean().unwrap();
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }

    #[cfg(windows)]
    fn seed_staged_content_fixture(fixture: &Fixture) {
        std::fs::create_dir_all(fixture.vendor.join("support/src")).unwrap();
        std::fs::write(fixture.vendor.join("Cargo.toml"),
            "[package]\nname=\"value-lib\"\nversion=\"0.1.0\"\nedition=\"2021\"\nbuild=false\n[dependencies.support-lib]\npath=\"support\"\n").unwrap();
        std::fs::write(
            fixture.vendor.join("src/lib.rs"),
            "mod nested;\npub fn value() -> i32 { support_lib::value() + nested::offset() }\n",
        )
        .unwrap();
        std::fs::write(
            fixture.vendor.join("src/nested.rs"),
            "pub fn offset() -> i32 { include_bytes!(\"offset.dat\")[0] as i32 }\n",
        )
        .unwrap();
        std::fs::write(fixture.vendor.join("src/offset.dat"), [0u8]).unwrap();
        std::fs::write(
            fixture.vendor.join("support/Cargo.toml"),
            "[package]\nname=\"support-lib\"\nversion=\"0.1.0\"\nedition=\"2021\"\nbuild=false\n",
        )
        .unwrap();
        std::fs::write(
            fixture.vendor.join("support/src/lib.rs"),
            "pub fn value() -> i32 { 17 }\n",
        )
        .unwrap();
        std::fs::write(fixture.engine.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"support-lib\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\ndependencies = [\"support-lib\"]\n").unwrap();
    }
    #[cfg(windows)]
    fn staged_content_dependency_paths(staged: &StagedSources) -> Vec<PathBuf> {
        [
            "Cargo.toml",
            "src/lib.rs",
            "src/nested.rs",
            "src/offset.dat",
            "support/Cargo.toml",
            "support/src/lib.rs",
        ]
        .into_iter()
        .map(|path| staged.envelope.join("vendor/value-lib").join(path))
        .collect()
    }
    #[cfg(windows)]
    fn local_build_fixture(
        staged: &StagedSources,
    ) -> (
        serde_json::Value,
        Vec<serde_json::Value>,
        crate::external_native::SelectedMember,
    ) {
        let paths = [
            (
                "native",
                "external_logic",
                staged.member_manifest.clone(),
                "cdylib",
            ),
            (
                "value",
                "value_lib",
                staged.envelope.join("vendor/value-lib/Cargo.toml"),
                "lib",
            ),
            (
                "support",
                "support_lib",
                staged.envelope.join("vendor/value-lib/support/Cargo.toml"),
                "lib",
            ),
        ];
        let mut packages = Vec::new();
        let mut artifacts = Vec::new();
        for (id, name, manifest, kind) in paths {
            let target = serde_json::json!({"name":name,"kind":[kind],"crate_types":[kind],
                "src_path":manifest.parent().unwrap().join("src/lib.rs")});
            packages.push(serde_json::json!({"id":id,"source":null,
                "manifest_path":manifest,"targets":[target.clone()]}));
            artifacts.push(
                serde_json::json!({"reason":"compiler-artifact","package_id":id,
                "manifest_path":manifest,"target":target,"features":[],
                "profile":{"opt_level":"0","debuginfo":0,"debug_assertions":true,
                    "overflow_checks":true,"test":false}}),
            );
        }
        let selected = crate::external_native::SelectedMember {
            package: "native-game".into(),
            package_id: "native".into(),
            target: "external_logic".into(),
            manifest: staged.member_manifest.clone(),
            source: staged.member_manifest.parent().unwrap().join("src/lib.rs"),
        };
        (
            serde_json::json!({"packages":packages}),
            artifacts,
            selected,
        )
    }
    #[cfg(windows)]
    fn local_build_lines(rows: &[serde_json::Value]) -> String {
        rows.iter().map(|row| row.to_string() + "\n").collect()
    }
    #[cfg(windows)]
    #[test]
    fn reported_local_output_holds_same_file_cursor_and_drop_boundary() {
        let fixture = Fixture::new();
        let b = budget();
        let (staged, lease, _, build, _, rows, _) = reported_local_output_fixture(&fixture, &b);
        let before = fixture_oracle(&fixture, &b).unwrap();
        let path = PathBuf::from(rows[1]["filenames"][0].as_str().unwrap());
        let root = lease.reported_output_root(&build, &b).unwrap();
        let mut held: Option<NativeReportedOutput> = None;
        for style in 0..4 {
            let output = root
                .hold(&lease, &windows_spelling(&path, style), 1024, &b)
                .unwrap();
            assert_eq!(output.bytes(), std::fs::metadata(&path).unwrap().len());
            assert_eq!(output.relative(), "libvalue.rlib");
            if let Some(prior) = &held {
                assert_eq!(output.digest_value(), prior.digest_value());
            }
            held = Some(output);
        }
        let output = held.unwrap();
        let identity = output.identity_key();
        std::io::Seek::seek(&mut &output.guard, std::io::SeekFrom::Start(3)).unwrap();
        let moved = Box::new((root, output));
        drop(lease);
        moved.1.recheck(&moved.0, 1024, &b).unwrap();
        assert_eq!(
            std::io::Seek::stream_position(&mut &moved.1.guard).unwrap(),
            3
        );
        assert_eq!(moved.1.identity_key(), identity);
        let observation = moved.1.observation_for_test();
        assert_eq!(observation["write_access_refused"], true);
        assert_eq!(observation["delete_access_refused"], true);
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
        drop(moved);
        assert!(OpenOptions::new().write(true).open(&path).is_ok());
        drop(staged);
    }
    #[cfg(windows)]
    fn reported_local_output_fixture(
        fixture: &Fixture,
        b: &Budget<'_>,
    ) -> (
        StagedSources,
        CompilationLease,
        PathBuf,
        PathBuf,
        serde_json::Value,
        Vec<serde_json::Value>,
        crate::external_native::SelectedMember,
    ) {
        seed_staged_content_fixture(fixture);
        let staged = fixture
            .plan(Mode::DependencyRead, b)
            .stage(&fixture.base.join("reported-output-stage"), b)
            .unwrap();
        let compiler_root = fixture.base.join("compiler-state");
        let target = compiler_root.join("target");
        let build = compiler_root.join("build");
        let profile = compiler_root.join("profile");
        for path in [&target, &build, &profile] {
            std::fs::create_dir_all(path).unwrap();
        }
        let (metadata, mut rows, selected) = local_build_fixture(&staged);
        for (index, row) in rows.iter_mut().enumerate() {
            let path = match index {
                0 => target.join("selected.dll"),
                1 => build.join("libvalue.rlib"),
                _ => build.join("libsupport.rmeta"),
            };
            std::fs::write(&path, format!("ordinary reported-output witness {index}\n")).unwrap();
            row["filenames"] = serde_json::json!([path]);
        }
        let lease = staged
            .compilation_lease(b)
            .unwrap()
            .hold_compiler_state(
                &compiler_root,
                std::slice::from_ref(&compiler_root),
                &[target.clone(), build.clone(), profile],
                &[],
                &[],
                b,
            )
            .unwrap();
        (staged, lease, target, build, metadata, rows, selected)
    }
    #[cfg(windows)]
    #[test]
    fn reported_local_output_binds_distinct_observations_and_selected_artifact() {
        let fixture = Fixture::new();
        let mut b = budget();
        let (staged, lease, target, build, mut metadata, mut rows, selected) =
            reported_local_output_fixture(&fixture, &b);
        let before = fixture_oracle(&fixture, &b).unwrap();
        // Small SOURCE quotas must not become compiler-output allowances.
        b.max_file = 8192;
        b.max_bytes = 16384;
        let source_limits = (b.max_file, b.max_bytes, b.deadline());
        let dependency = PathBuf::from(rows[1]["filenames"][0].as_str().unwrap());
        std::fs::write(&dependency, vec![b'x'; 16385]).unwrap();
        assert_eq!(
            read_file(&dependency, b.max_file, &b, None, None)
                .err()
                .as_deref(),
            Some("source_file_byte_limit")
        );
        metadata["packages"].as_array_mut().unwrap().push(serde_json::json!({"id":"inactive","source":null,
            "manifest_path":staged.member_manifest,"targets":[],"features":{"workspace_union":[]}}));
        let mut distinct = rows[1].clone();
        distinct["profile"]["opt_level"] = serde_json::json!("2");
        let path = build.join("libvalue-optimized.rlib");
        std::fs::write(&path, b"distinct observed profile\n").unwrap();
        distinct["filenames"] = serde_json::json!([path]);
        rows.push(distinct);
        let inputs = lease
            .native_source_identity(&staged.member_manifest, &selected.source, &b)
            .unwrap();
        let stdout = local_build_lines(&rows);
        let builds = crate::external_native::observed_local_builds(
            &stdout, &metadata, &selected, &inputs, &b,
        )
        .unwrap();
        assert_eq!(builds.len(), 4);
        let held = crate::external_native::hold_reported_local_outputs(
            &stdout, &builds, &inputs, &lease, &target, &build, &b,
        )
        .unwrap();
        let artifact = PathBuf::from(rows[0]["filenames"][0].as_str().unwrap());
        let hash = crate::sha256_file(&artifact).unwrap();
        held.recheck(&builds, &selected, &artifact, &hash, &b)
            .unwrap();
        assert!(held
            .recheck(&builds, &selected, &artifact, &"0".repeat(64), &b)
            .is_err());
        let observation = held.observations_for_test();
        assert_eq!(observation["records"].as_array().unwrap().len(), 4);
        let mut roles = BTreeSet::new();
        for record in observation["records"].as_array().unwrap() {
            for output in record["outputs"].as_array().unwrap() {
                roles.insert(output["root_role"].as_str().unwrap());
            }
        }
        assert_eq!(roles, BTreeSet::from(["build", "target"]));
        let moved = Box::new(held);
        moved
            .recheck(&builds, &selected, &artifact, &hash, &b)
            .unwrap();
        assert_eq!(moved.observations_for_test(), observation);
        assert_eq!((b.max_file, b.max_bytes, b.deadline()), source_limits);
        let root = lease.reported_output_root(&build, &b).unwrap();
        let output = root
            .hold(&lease, &dependency.to_string_lossy(), 32768, &b)
            .unwrap();
        std::io::Seek::seek(&mut &output.guard, std::io::SeekFrom::Start(3)).unwrap();
        assert_eq!(
            hash_reported_local_output(&dependency, &output.guard, &output.expected, 16384, &b)
                .err()
                .as_deref(),
            Some("external_reported_output_byte_bound")
        );
        assert_eq!(
            std::io::Seek::stream_position(&mut &output.guard).unwrap(),
            3
        );
        output.recheck(&root, 32768, &b).unwrap();
        let mut missing = rows.clone();
        missing.pop();
        assert!(crate::external_native::hold_reported_local_outputs(
            &local_build_lines(&missing),
            &builds,
            &inputs,
            &lease,
            &target,
            &build,
            &b
        )
        .is_err());
        let mut duplicate = rows.clone();
        duplicate.push(rows[1].clone());
        assert!(crate::external_native::hold_reported_local_outputs(
            &local_build_lines(&duplicate),
            &builds,
            &inputs,
            &lease,
            &target,
            &build,
            &b
        )
        .is_err());
        let mut wrong = rows.clone();
        wrong[1]["profile"]["opt_level"] = serde_json::json!("9");
        assert!(crate::external_native::hold_reported_local_outputs(
            &local_build_lines(&wrong),
            &builds,
            &inputs,
            &lease,
            &target,
            &build,
            &b
        )
        .is_err());
        let mut foreign = rows.clone();
        foreign[1]["manifest_path"] = serde_json::json!(fixture.base.join("ungranted/keep"));
        assert!(crate::external_native::hold_reported_local_outputs(
            &local_build_lines(&foreign),
            &builds,
            &inputs,
            &lease,
            &target,
            &build,
            &b
        )
        .is_err());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn reported_local_output_refuses_foreign_aliases_and_filename_bounds() {
        let fixture = Fixture::new();
        let b = budget();
        let (staged, lease, target, build, metadata, rows, selected) =
            reported_local_output_fixture(&fixture, &b);
        let before = fixture_oracle(&fixture, &b).unwrap();
        let inputs = lease
            .native_source_identity(&staged.member_manifest, &selected.source, &b)
            .unwrap();
        let builds = crate::external_native::observed_local_builds(
            &local_build_lines(&rows),
            &metadata,
            &selected,
            &inputs,
            &b,
        )
        .unwrap();
        let path = PathBuf::from(rows[1]["filenames"][0].as_str().unwrap());
        let root = lease.reported_output_root(&build, &b).unwrap();
        assert!(root.hold(&lease, &path.to_string_lossy(), 0, &b).is_err());
        assert!(lease
            .reported_output_root(&target, &b)
            .unwrap()
            .hold(&lease, &path.to_string_lossy(), 1024, &b)
            .is_err());
        assert!(root
            .hold(
                &lease,
                &fixture.base.join("ungranted/keep").to_string_lossy(),
                1024,
                &b
            )
            .is_err());
        assert!(lease.reported_output_root(&fixture.base, &b).is_err());
        let unknown = fixture.base.join("compiler-state/profile/unknown.rlib");
        std::fs::write(&unknown, b"held but not an admitted output root").unwrap();
        for names in [
            serde_json::json!([]),
            serde_json::json!([fixture.base.join("ungranted/keep")]),
            serde_json::json!([unknown]),
            serde_json::json!([path.clone(), windows_spelling(&path, 1)]),
            serde_json::json!((0..9).map(|_| path.clone()).collect::<Vec<_>>()),
            serde_json::json!([true]),
        ] {
            let mut wrong = rows.clone();
            wrong[1]["filenames"] = names;
            assert!(crate::external_native::hold_reported_local_outputs(
                &local_build_lines(&wrong),
                &builds,
                &inputs,
                &lease,
                &target,
                &build,
                &b
            )
            .is_err());
        }
        let other_dll = build.join("misplaced-selected.dll");
        std::fs::write(&other_dll, b"not an admissible selected DLL").unwrap();
        let mut misplaced = rows.clone();
        misplaced[0]["filenames"] = serde_json::json!([other_dll]);
        let stdout = local_build_lines(&misplaced);
        let held = crate::external_native::hold_reported_local_outputs(
            &stdout, &builds, &inputs, &lease, &target, &build, &b,
        )
        .unwrap();
        let hash = crate::sha256_file(&other_dll).unwrap();
        assert_eq!(
            held.recheck(&builds, &selected, &other_dll, &hash, &b)
                .err()
                .as_deref(),
            Some("external_reported_output_selected_artifact")
        );
        let parent = crate::external_native::ParentDeadline::at(b.deadline()).unwrap();
        let finished = stdout + "{\"reason\":\"build-finished\",\"success\":true}\n";
        assert_eq!(
            crate::external_native::exact_artifact(&finished, &selected, &target, &lease, &parent)
                .err()
                .as_deref(),
            Some("external_artifact_file_outside_target")
        );
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn reported_local_output_rechecks_metadata_cancel_expiry_and_restores_cursor() {
        let fixture = Fixture::new();
        let b = budget();
        let (staged, lease, _, build, _, rows, _) = reported_local_output_fixture(&fixture, &b);
        let path = PathBuf::from(rows[1]["filenames"][0].as_str().unwrap());
        let root = lease.reported_output_root(&build, &b).unwrap();
        let mut output = root
            .hold(&lease, &path.to_string_lossy(), 1024, &b)
            .unwrap();
        std::io::Seek::seek(&mut &output.guard, std::io::SeekFrom::Start(3)).unwrap();
        assert!(hash_reported_local_output(&path, &output.guard, &output.expected, 0, &b).is_err());
        assert_eq!(
            std::io::Seek::stream_position(&mut &output.guard).unwrap(),
            3
        );
        let original = output.sha256.clone();
        output.sha256 = "0".repeat(64);
        assert!(output.recheck(&root, 1024, &b).is_err());
        output.sha256 = original;
        let identity = output.root_identity.clone();
        output.root_identity.volume ^= 1;
        assert!(output.recheck(&root, 1024, &b).is_err());
        output.root_identity = identity;
        let other = File::open(fixture.base.join("ungranted/keep")).unwrap();
        let original = std::mem::replace(&mut output.guard, other);
        assert!(output.recheck(&root, 1024, &b).is_err());
        output.guard = original;
        let cancelled = AtomicBool::new(false);
        let check = || {
            if cancelled.load(Ordering::Acquire) {
                Err("reported_output_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cb = Budget::new(Duration::from_secs(30), &check).unwrap();
        cancelled.store(true, Ordering::Release);
        assert!(
            hash_reported_local_output(&path, &output.guard, &output.expected, 1024, &cb).is_err()
        );
        let mut expired = budget();
        expired.deadline = Instant::now() - Duration::from_millis(1);
        assert!(
            hash_reported_local_output(&path, &output.guard, &output.expected, 1024, &expired)
                .is_err()
        );
        assert_eq!(
            std::io::Seek::stream_position(&mut &output.guard).unwrap(),
            3
        );
        output.recheck(&root, 1024, &b).unwrap();
        drop(staged);
    }
    #[cfg(windows)]
    #[test]
    fn external_local_build_join_binds_transitive_held_content_and_observed_units() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let (mut metadata, mut rows, selected) = local_build_fixture(&staged);
        // Unbuilt/inactive metadata and workspace feature unions are not actual units.
        metadata["packages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id":"inactive","source":null,"manifest_path":staged.member_manifest,
                "targets":[],"features":{"unused":[]}
            }));
        rows[1]["features"] = serde_json::json!(["observed_only"]);
        rows[1]["profile"]["opt_level"] = serde_json::json!("2");
        let mut other_target = rows[1].clone();
        other_target["target"]["name"] = serde_json::json!("other_value_lib");
        metadata["packages"][1]["targets"]
            .as_array_mut()
            .unwrap()
            .push(other_target["target"].clone());
        rows.push(other_target);
        let mut other_profile = rows[1].clone();
        other_profile["profile"]["opt_level"] = serde_json::json!("3");
        rows.push(other_profile);
        let mut reader = &witness.files[0].guard;
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(3)).unwrap();
        let builds = crate::external_native::observed_local_builds(
            &local_build_lines(&rows),
            &metadata,
            &selected,
            &witness,
            &b,
        )
        .unwrap();
        assert_eq!(builds.len(), 5);
        let values: Vec<_> = builds.iter().map(|build| build.digest_value()).collect();
        assert!(values
            .iter()
            .any(|v| v["source"]["relative"] == "support/src/lib.rs"));
        assert!(values
            .iter()
            .any(|v| v["features"] == serde_json::json!(["observed_only"])
                && v["profile"]["opt_level"] == "2"));
        assert!(!values.iter().any(|v| v.to_string().contains("file:///")));
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 3);
        witness
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_local_build_join_refuses_duplicate_foreign_and_unsupported_observations() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let (metadata, rows, selected) = local_build_fixture(&staged);
        let run = |rows: &[serde_json::Value], metadata: &serde_json::Value| {
            crate::external_native::observed_local_builds(
                &local_build_lines(rows),
                metadata,
                &selected,
                &witness,
                &b,
            )
            .err()
            .unwrap()
        };
        let mut duplicate = rows.clone();
        duplicate.push(rows[1].clone());
        assert_eq!(
            run(&duplicate, &metadata),
            "external_local_build_duplicate_unit"
        );
        let mut changed = rows.clone();
        changed[1]["target"]["src_path"] = rows[2]["target"]["src_path"].clone();
        assert_eq!(
            run(&changed, &metadata),
            "external_local_build_content_binding_changed"
        );
        let mut changed = rows.clone();
        changed[1]["manifest_path"] = serde_json::json!(fixture.base.join("unheld"));
        assert_eq!(
            run(&changed, &metadata),
            "external_local_build_input_not_held"
        );
        let mut changed = metadata.clone();
        changed["packages"][1]["source"] = serde_json::json!("registry+redacted");
        assert_eq!(
            run(&rows, &changed),
            "external_local_build_package_unsupported_or_duplicate"
        );
        let mut changed = rows.clone();
        changed[1]["target"]["kind"] = serde_json::json!(["proc-macro"]);
        assert_eq!(
            run(&changed, &metadata),
            "external_local_build_target_missing"
        );
        let mut declared = metadata.clone();
        declared["packages"][1]["targets"][0]["kind"] = serde_json::json!(["proc-macro"]);
        assert_eq!(
            run(&changed, &declared),
            "external_local_build_target_unsupported"
        );
        let mut changed = rows.clone();
        changed[1]["profile"]["test"] = serde_json::json!(true);
        assert_eq!(run(&changed, &metadata), "external_local_build_profile");
        assert_eq!(
            run(&rows[1..], &metadata),
            "external_local_build_selected_unobserved"
        );
        let mut oversized = rows.clone();
        oversized[1]["features"] = serde_json::json!((0..256)
            .map(|i| format!("{i:03}{}", "x".repeat(125)))
            .collect::<Vec<_>>());
        assert_eq!(
            run(&oversized, &metadata),
            "external_local_build_encoding_bound"
        );
    }
    #[cfg(windows)]
    #[test]
    fn external_local_build_join_rechecks_binding_and_preserves_cancelled_cursor() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let path = staged.envelope.join("vendor/value-lib/src/lib.rs");
        let mut input = witness.local_input(&path, &b).unwrap();
        witness.recheck_local_input(&input, &b).unwrap();
        input.sha256 = "0".repeat(64);
        assert_eq!(
            witness.recheck_local_input(&input, &b).err().unwrap(),
            "external_local_build_input_changed"
        );
        let guard = witness
            .files
            .iter()
            .find(|file| provenance_paths_equal(&file.path, &path))
            .unwrap();
        let mut reader = &guard.guard;
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(5)).unwrap();
        let ticks = std::cell::Cell::new(0);
        let cancelled = || {
            ticks.set(ticks.get() + 1);
            if ticks.get() >= 4 {
                Err("local_join_cancel".into())
            } else {
                Ok(())
            }
        };
        assert_eq!(
            witness
                .local_input(&path, &b.with_check(&cancelled))
                .err()
                .unwrap(),
            "local_join_cancel"
        );
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 5);
        let expired = Budget {
            deadline: Instant::now() - std::time::Duration::from_millis(1),
            ..b
        };
        assert!(witness.local_input(&path, &expired).is_err());
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 5);
    }
    #[cfg(windows)]
    #[test]
    fn external_local_build_join_accepts_checked_windows_spellings() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let (metadata, rows, selected) = local_build_fixture(&staged);
        let original = crate::external_native::observed_local_builds(
            &local_build_lines(&rows),
            &metadata,
            &selected,
            &witness,
            &b,
        )
        .unwrap()
        .iter()
        .map(|build| build.digest_value())
        .collect::<Vec<_>>();
        for metadata_style in 0..4 {
            let mut metadata = metadata.clone();
            for package in metadata["packages"].as_array_mut().unwrap() {
                package["manifest_path"] = serde_json::json!(windows_spelling(
                    Path::new(package["manifest_path"].as_str().unwrap()),
                    metadata_style
                ));
                for target in package["targets"].as_array_mut().unwrap() {
                    target["src_path"] = serde_json::json!(windows_spelling(
                        Path::new(target["src_path"].as_str().unwrap()),
                        metadata_style
                    ));
                }
            }
            for artifact_style in 0..4 {
                let mut rows = rows.clone();
                for row in &mut rows {
                    row["manifest_path"] = serde_json::json!(windows_spelling(
                        Path::new(row["manifest_path"].as_str().unwrap()),
                        artifact_style
                    ));
                    row["target"]["src_path"] = serde_json::json!(windows_spelling(
                        Path::new(row["target"]["src_path"].as_str().unwrap()),
                        artifact_style
                    ));
                }
                let actual = crate::external_native::observed_local_builds(
                    &local_build_lines(&rows),
                    &metadata,
                    &selected,
                    &witness,
                    &b,
                )
                .unwrap()
                .iter()
                .map(|build| build.digest_value())
                .collect::<Vec<_>>();
                assert_eq!(
                    actual, original,
                    "metadata={metadata_style}, artifact={artifact_style}"
                );
            }
        }
        assert!(witness
            .local_input(&staged.envelope.join("unheld/src/lib.rs"), &b)
            .is_err());
        witness
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_staged_content_inventory_covers_transitive_modules_data_and_guard_order() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let (staged, mut lease, source) = provenance_fixture_stage(&fixture, &b);
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let paths = staged_content_dependency_paths(&staged);
        let observation = witness
            .staged_content_custody_for_test(
                &staged,
                &staged.member_manifest,
                &source,
                &paths,
                &b,
                false,
            )
            .unwrap();
        assert_eq!(observation["inputs"].as_array().unwrap().len(), paths.len());
        assert_eq!(witness.files.len(), lease.staged_file_guards.len());
        let digest = witness.digest().to_owned();
        drop(witness);
        let writable = std::sync::Arc::get_mut(&mut lease).unwrap();
        writable.staged_file_guards.reverse();
        writable.staged_content.reverse();
        let reordered = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(reordered.digest(), digest);
        drop(reordered);
        let writable = std::sync::Arc::get_mut(&mut lease).unwrap();
        let last = writable.staged_file_guards.pop().unwrap();
        assert_eq!(
            writable
                .native_source_identity(&staged.member_manifest, &source, &b)
                .err()
                .unwrap(),
            "external_provenance_input_not_held"
        );
        writable.staged_file_guards.push(last);
        let held = &writable.staged_file_guards[0];
        let duplicate = (held.0.clone(), held.1.clone(), held.2.try_clone().unwrap());
        writable.staged_file_guards.push(duplicate);
        assert_eq!(
            writable
                .native_source_identity(&staged.member_manifest, &source, &b)
                .err()
                .unwrap(),
            "external_provenance_input_ambiguous"
        );
        writable.staged_file_guards.pop();
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_staged_content_corruption_and_new_namespace_children_refuse() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let paths = staged_content_dependency_paths(&staged);
        witness
            .staged_content_custody_for_test(
                &staged,
                &staged.member_manifest,
                &source,
                &paths,
                &b,
                true,
            )
            .unwrap();
        let index = witness
            .files
            .iter()
            .position(|file| file.path == paths[2])
            .unwrap();
        witness.files[index].expected.identity.volume ^= 1;
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[index].expected.identity.volume ^= 1;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        let saved = std::mem::replace(
            &mut witness.files[index].path,
            fixture.base.join("foreign-input"),
        );
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[index].path = saved;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_input_not_held"
        );
        let foreign = fixture.base.join("foreign-dependency.rs");
        std::fs::write(&foreign, std::fs::read(&paths[2]).unwrap()).unwrap();
        let saved = std::mem::replace(
            &mut witness.files[index].guard,
            open_compile_source(&foreign, &b).unwrap(),
        );
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.files[index].guard = saved;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        witness.content[0].fingerprint.sha256.push('0');
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        witness.content[0].fingerprint.sha256.pop();
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_content_binding_changed"
        );
        let added = staged.envelope.join("vendor/value-lib/src/new-child.rs");
        std::fs::write(&added, b"new staged child must not gain content custody\n").unwrap();
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &b);
        std::fs::remove_file(&added).unwrap();
        assert_eq!(
            observed.err().unwrap(),
            "external_source_proposed_fingerprint_changed"
        );
        witness
            .recheck(&staged, &staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_staged_content_original_handles_survive_unique_retirement_then_release() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let staged = {
            let envelope = workspace.source_envelope(b.deadline()).unwrap();
            fixture
                .plan(Mode::DependencyRead, &b)
                .stage_owned(&envelope, &b)
                .unwrap()
        };
        let source = staged.member_manifest.parent().unwrap().join("src/lib.rs");
        let lease = std::sync::Arc::new(staged.compilation_lease(&b).unwrap());
        workspace.retain_compilation(lease.clone()).unwrap();
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        let paths = staged_content_dependency_paths(&staged);
        let held = witness
            .staged_content_custody_for_test(
                &staged,
                &staged.member_manifest,
                &source,
                &paths,
                &b,
                false,
            )
            .unwrap();
        let weak = std::sync::Arc::downgrade(&lease);
        assert_eq!(std::sync::Arc::strong_count(&lease), 2);
        drop(lease);
        let ownership = workspace.retire_compilation_for_publication(&b).unwrap();
        assert!(weak.upgrade().is_none());
        let after = witness
            .staged_content_custody_for_test(
                &staged,
                &staged.member_manifest,
                &source,
                &paths,
                &b,
                false,
            )
            .unwrap();
        assert_eq!(held, after);
        for path in &paths {
            assert!(OpenOptions::new().write(true).open(path).is_err());
            assert!(OpenOptions::new()
                .access_mode(0x0001_0000)
                .share_mode(7)
                .custom_flags(0x0020_0000)
                .open(path)
                .is_err());
        }
        drop(witness);
        for path in &paths {
            drop(OpenOptions::new().write(true).open(path).unwrap());
            drop(
                OpenOptions::new()
                    .access_mode(0x0001_0000)
                    .share_mode(7)
                    .custom_flags(0x0020_0000)
                    .open(path)
                    .unwrap(),
            );
        }
        drop(ownership);
        drop(staged);
        let workspace_path = workspace.path.clone();
        workspace.clean().unwrap();
        assert!(!workspace_path.exists());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_staged_content_cancel_and_expiry_restore_cursors_and_partial_handles() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let paths = staged_content_dependency_paths(&staged);
        let (_, _, held) = lease
            .staged_file_guards
            .iter()
            .find(|(path, _, _)| path == &paths[3])
            .unwrap();
        let mut reader = held;
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(1)).unwrap();
        let triggered = std::cell::Cell::new(false);
        let stop = || {
            let mut reader = held;
            if std::io::Seek::stream_position(&mut reader).unwrap() != 1 {
                triggered.set(true);
                Err("staged_content_dependency_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cancelled = b.with_check(&stop);
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &source, &cancelled)
                .err()
                .unwrap(),
            "staged_content_dependency_cancelled"
        );
        assert!(triggered.get());
        assert_eq!(std::io::Seek::stream_position(&mut reader).unwrap(), 1);
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(std::sync::Arc::strong_count(&lease), 1);
        let mut expired = b.with_check(b.check_cancelled);
        expired.deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            lease
                .native_source_identity(&staged.member_manifest, &source, &expired)
                .err()
                .unwrap(),
            "external_staging_deadline"
        );
        assert_eq!(
            witness
                .recheck(&staged, &staged.member_manifest, &source, &expired)
                .err()
                .unwrap(),
            "external_staging_deadline"
        );
        let observed = witness
            .staged_content_custody_for_test(
                &staged,
                &staged.member_manifest,
                &source,
                &paths,
                &b,
                false,
            )
            .unwrap();
        assert_eq!(observed["inputs"].as_array().unwrap().len(), paths.len());
        drop(witness);
        drop(lease);
        for path in &paths {
            drop(OpenOptions::new().write(true).open(path).unwrap());
        }
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_staged_content_exact_resource_edges_refuse_without_wider_limits() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let (staged, lease, source) = provenance_fixture_stage(&fixture, &b);
        let mut used = NATIVE_CONTENT_ENCODING_LIMIT - 1;
        native_content_charge(&mut used, 1).unwrap();
        assert_eq!(used, NATIVE_CONTENT_ENCODING_LIMIT);
        assert_eq!(
            native_content_charge(&mut used, 1).err().unwrap(),
            "external_provenance_input_encoding_bound"
        );
        native_provenance_guard_bound(&[8180], 12).unwrap();
        assert_eq!(
            native_provenance_guard_bound(&[8181], 12).err().unwrap(),
            "external_provenance_guard_bound"
        );
        assert!(native_provenance_guard_bound(&[usize::MAX], 1).is_err());
        let mut charged = NATIVE_CONTENT_ENCODING_BASE;
        let mut usage = Usage::default();
        for root in &lease.staged_content {
            native_content_shape(
                &root.path,
                &root.root_id,
                &root.entries,
                &mut charged,
                &mut usage,
                &b,
            )
            .unwrap();
        }
        let largest = lease
            .staged_content
            .iter()
            .flat_map(|root| &root.entries)
            .filter(|row| !row.directory)
            .map(|row| row.bytes)
            .max()
            .unwrap();
        let mut exact = b.with_check(b.check_cancelled);
        exact.max_files = usage.files;
        exact.max_entries = usage.entries;
        exact.max_bytes = usage.bytes;
        exact.max_file = largest;
        let witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        witness
            .recheck(&staged, &staged.member_manifest, &source, &exact)
            .unwrap();
        for which in ["files", "entries", "bytes", "file"] {
            let mut smaller = exact.with_check(exact.check_cancelled);
            match which {
                "files" => smaller.max_files -= 1,
                "entries" => smaller.max_entries -= 1,
                "bytes" => smaller.max_bytes -= 1,
                _ => smaller.max_file -= 1,
            }
            assert!(witness
                .recheck(&staged, &staged.member_manifest, &source, &smaller)
                .is_err());
        }
        assert_eq!(exact.deadline(), b.deadline());
    }

    #[cfg(windows)]
    #[test]
    fn external_staged_content_standalone_manifest_role_aliases_preserve_unique_counts() {
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let mut staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("standalone-alias-stage"), &b)
            .unwrap();
        // Only this private fixture copy becomes a standalone package. Production
        // resolution remains virtual-workspace-only and originals stay untouched.
        let standalone = Fixture::native_manifest("../vendor/value-lib").replacen(
            "[lib]\n",
            "[lib]\npath=\"native/src/lib.rs\"\n",
            1,
        );
        std::fs::write(&staged.cargo_manifest, standalone).unwrap();
        std::fs::remove_file(&staged.member_manifest).unwrap();
        staged.member_manifest = staged.cargo_manifest.clone();
        let root = staged
            .roots
            .iter_mut()
            .find(|root| root.root_id.as_str() == "engine")
            .unwrap();
        root.proposed_fingerprint = Some(scan(&root.staged, &b, &mut Usage::default()).unwrap().1);
        let source = staged
            .member_manifest
            .parent()
            .unwrap()
            .join("native/src/lib.rs");
        let mut lease = std::sync::Arc::new(staged.compilation_lease(&b).unwrap());
        let unique_files = lease.staged_file_guards.len();
        assert_eq!(
            lease
                .staged_file_guards
                .iter()
                .filter(|(path, _, _)| path == &staged.cargo_manifest)
                .count(),
            1
        );
        let mut witness = lease
            .native_source_identity(&staged.member_manifest, &source, &b)
            .unwrap();
        assert_eq!(
            witness
                .files
                .iter()
                .take(4)
                .map(|file| file.role)
                .collect::<Vec<_>>(),
            [
                "workspace_manifest",
                "selected_manifest",
                "selected_source",
                "lock"
            ]
        );
        assert_eq!(witness.files.len(), unique_files + 1);
        assert_eq!(
            witness.files[0].expected.identity,
            witness.files[1].expected.identity
        );
        assert_eq!(witness.files[0].sha256, witness.files[1].sha256);
        assert_ne!(
            std::os::windows::io::AsRawHandle::as_raw_handle(&witness.files[0].guard),
            std::os::windows::io::AsRawHandle::as_raw_handle(&witness.files[1].guard)
        );
        let mut exact = b.with_check(b.check_cancelled);
        exact.max_files = unique_files;
        witness
            .recheck(&staged, &staged.member_manifest, &source, &exact)
            .unwrap();
        let mut below = exact.with_check(exact.check_cancelled);
        below.max_files -= 1;
        assert!(witness
            .recheck(&staged, &staged.member_manifest, &source, &below)
            .is_err());
        let lock = staged.cargo_manifest.parent().unwrap().join("Cargo.lock");
        let anchors = [
            staged.cargo_manifest.as_path(),
            staged.member_manifest.as_path(),
            source.as_path(),
            lock.as_path(),
        ];
        let duplicated_objects =
            native_content_duplicate_count(unique_files, anchors[0], anchors[1]).unwrap();
        assert_eq!(duplicated_objects, witness.files.len() + 1);
        assert_eq!(duplicated_objects, unique_files + 2);
        native_provenance_guard_bound(&[8192 - duplicated_objects], duplicated_objects).unwrap();
        assert_eq!(
            native_provenance_guard_bound(&[8193 - duplicated_objects], duplicated_objects)
                .err()
                .unwrap(),
            "external_provenance_guard_bound"
        );
        assert!(native_content_duplicate_count(usize::MAX, anchors[0], anchors[1]).is_err());
        assert_eq!(
            native_content_anchor_aliases([anchors[0], anchors[1], anchors[2], anchors[2]])
                .err()
                .unwrap(),
            "external_provenance_input_ambiguous"
        );
        for index in 0..2 {
            let role = witness.files[index].role;
            witness.files[index].role = "staged_content";
            let observed = witness.recheck(&staged, &staged.member_manifest, &source, &exact);
            witness.files[index].role = role;
            assert_eq!(
                observed.err().unwrap(),
                "external_provenance_held_input_changed"
            );
            witness.files[index].sha256.push('0');
            let observed = witness.recheck(&staged, &staged.member_manifest, &source, &exact);
            witness.files[index].sha256.pop();
            assert_eq!(
                observed.err().unwrap(),
                "external_provenance_held_input_changed"
            );
            witness.files[index].expected.identity.volume ^= 1;
            let observed = witness.recheck(&staged, &staged.member_manifest, &source, &exact);
            witness.files[index].expected.identity.volume ^= 1;
            assert_eq!(
                observed.err().unwrap(),
                "external_provenance_held_input_changed"
            );
        }
        let foreign = fixture.base.join("foreign-alias-Cargo.toml");
        std::fs::write(&foreign, std::fs::read(&staged.cargo_manifest).unwrap()).unwrap();
        let original = std::mem::replace(
            &mut witness.files[1].guard,
            open_compile_source(&foreign, &b).unwrap(),
        );
        let expected = witness.files[1].expected.clone();
        let foreign_expected = stamp(&witness.files[1].guard).unwrap();
        witness.files[1].expected = foreign_expected;
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &exact);
        witness.files[1].guard = original;
        witness.files[1].expected = expected;
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_held_input_changed"
        );
        let file = &witness.files[0];
        let arbitrary_duplicate = NativeProvenanceFile {
            role: "staged_content",
            path: file.path.clone(),
            expected: file.expected.clone(),
            sha256: file.sha256.clone(),
            guard: file.guard.try_clone().unwrap(),
        };
        witness.files.push(arbitrary_duplicate);
        let observed = witness.recheck(&staged, &staged.member_manifest, &source, &exact);
        witness.files.pop();
        assert_eq!(
            observed.err().unwrap(),
            "external_provenance_content_binding_changed"
        );
        witness
            .recheck(&staged, &staged.member_manifest, &source, &exact)
            .unwrap();
        assert_eq!(exact.deadline(), b.deadline());
        drop(witness);
        let writable = std::sync::Arc::get_mut(&mut lease).unwrap();
        let row = writable
            .staged_file_guards
            .iter()
            .find(|(path, _, _)| path == &staged.cargo_manifest)
            .unwrap();
        let duplicate = (row.0.clone(), row.1.clone(), row.2.try_clone().unwrap());
        writable.staged_file_guards.push(duplicate);
        assert_eq!(
            writable
                .native_source_identity(&staged.member_manifest, &source, &b)
                .err()
                .unwrap(),
            "external_provenance_input_ambiguous"
        );
        writable.staged_file_guards.pop();
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }

    fn fixture_oracle(fixture: &Fixture, budget: &Budget<'_>) -> fixture_all_entry_oracle::Outcome {
        fixture_all_entry_oracle::capture(
            &[
                ("app", fixture.app.as_path()),
                ("engine", fixture.engine.as_path()),
                ("vendor", fixture.vendor.as_path()),
                ("ungranted", fixture.base.join("ungranted").as_path()),
            ],
            budget,
        )
    }
    #[test]
    fn compiler_state_output_leases_allow_atomic_child_rename_and_deny_parent_rename() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project_native_compile(&fixture.app)
                .unwrap();
        let staged = {
            let envelope = workspace.source_envelope(b.deadline()).unwrap();
            fixture
                .plan(Mode::DependencyRead, &b)
                .stage_owned(&envelope, &b)
                .unwrap()
        };
        crate::external_native::compiler_directory_sharing_for_test(
            &workspace,
            &staged,
            b.deadline(),
            true,
        )
        .unwrap();
        assert_eq!(before, fixture_oracle(&fixture, &b).unwrap());
        drop(staged);
        workspace.clean().unwrap();
    }
    #[test]
    fn compiler_state_immutable_leases_keep_sources_tools_cwd_and_config_read_only() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project_native_compile(&fixture.app)
                .unwrap();
        let staged = {
            let envelope = workspace.source_envelope(b.deadline()).unwrap();
            fixture
                .plan(Mode::DependencyRead, &b)
                .stage_owned(&envelope, &b)
                .unwrap()
        };
        crate::external_native::compiler_directory_sharing_for_test(
            &workspace,
            &staged,
            b.deadline(),
            false,
        )
        .unwrap();
        assert_eq!(before, fixture_oracle(&fixture, &b).unwrap());
        drop(staged);
        workspace.clean().unwrap();
    }
    #[test]
    fn compiler_output_roles_refuse_foreign_cwd_duplicate_and_immutable_aliases() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("role-stage"), &b)
            .unwrap();
        let root = fixture.base.join("compiler-state");
        std::fs::create_dir(&root).unwrap();
        let target = root.join("target");
        let cwd = root.join("cwd");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        for outputs in [
            vec![fixture.app.clone()],
            vec![cwd],
            vec![target.clone(), target.clone()],
        ] {
            assert!(staged
                .compilation_lease(&b)
                .unwrap()
                .hold_compiler_state(&root, std::slice::from_ref(&root), &outputs, &[], &[], &b)
                .is_err());
        }
        assert!(
            matches!(staged.compilation_lease(&b).unwrap().hold_compiler_state(&root, &[root.clone(), target.clone()], std::slice::from_ref(&target), &[], &[], &b), Err(error) if error == "external_compile_output_role_conflict")
        );
        assert_eq!(before, fixture_oracle(&fixture, &b).unwrap());
    }
    #[test]
    fn all_entry_oracle_detects_dotfiles_cache_configs_and_empty_directories() {
        let fixture = Fixture::new();
        let b = budget();
        let config = fixture.engine.join(".cargo/config.toml");
        std::fs::create_dir(config.parent().unwrap()).unwrap();
        std::fs::write(&config, b"[net]\noffline=true\n").unwrap();
        for path in [
            fixture.app.join(".env"),
            fixture.app.join(".godot/cache-sentinel"),
            fixture.engine.join("target/cache-sentinel"),
            config,
        ] {
            let before = fixture_oracle(&fixture, &b).unwrap();
            let original = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"changed controlled fixture bytes\n").unwrap();
            assert_ne!(fixture_oracle(&fixture, &b).unwrap(), before);
            std::fs::write(&path, original).unwrap();
        }
        let before = fixture_oracle(&fixture, &b).unwrap();
        let empty = fixture.engine.join("target/.empty-private");
        std::fs::create_dir(&empty).unwrap();
        let added = fixture_oracle(&fixture, &b).unwrap();
        assert_ne!(added, before);
        std::fs::remove_dir(&empty).unwrap();
        assert_ne!(fixture_oracle(&fixture, &b).unwrap(), added);
        let result = fixture_oracle(&fixture, &b);
        assert_eq!(
            fixture_all_entry_oracle::status(&result)["coverage"],
            "ALL_ENTRY"
        );
        assert_eq!(
            result.as_ref().unwrap(),
            &fixture_oracle(&fixture, &b).unwrap()
        );
    }
    #[test]
    fn all_entry_oracle_aggregate_limits_depth_and_paths_refuse_without_baseline() {
        let fixture = Fixture::new();
        let first = fixture.base.join("oracle-first");
        let second = fixture.base.join("oracle-second");
        for path in [&first, &second] {
            std::fs::create_dir(path).unwrap();
            std::fs::write(path.join("one"), b"x").unwrap();
        }
        let roots = [("first", first.as_path()), ("second", second.as_path())];
        for kind in ["bytes", "files", "entries", "file_bytes"] {
            let mut b = budget();
            match kind {
                "bytes" => b.max_bytes = 1,
                "files" => b.max_files = 1,
                "entries" => b.max_entries = 3,
                "file_bytes" => b.max_file = 0,
                _ => unreachable!(),
            }
            let result = fixture_all_entry_oracle::capture(&roots, &b);
            assert_eq!(result.as_ref().unwrap_err().category, kind);
            let status = fixture_all_entry_oracle::status(&result);
            assert!(status["coverage"].is_null() && status["fingerprint"].is_null());
        }
        for (name, part, count, expected) in [
            ("deep", "d".to_owned(), 25, "depth"),
            ("wide", "w".repeat(45), 23, "path_bytes"),
        ] {
            let root = fixture.base.join(name);
            let mut path = root.clone();
            for _ in 0..count {
                path.push(&part);
            }
            std::fs::create_dir_all(path).unwrap();
            let b = budget();
            let result = fixture_all_entry_oracle::capture(&[("fixture", root.as_path())], &b);
            assert_eq!(result.unwrap_err().category, expected);
        }
    }
    #[test]
    fn all_entry_oracle_cancellation_expiry_and_absence_are_refused_null() {
        let fixture = Fixture::new();
        let cancelled = || Err("PRIVATE_CANCEL_DETAIL_MUST_NOT_EXPORT".to_owned());
        let b = Budget::new(Duration::from_secs(30), &cancelled).unwrap();
        let result = fixture_oracle(&fixture, &b);
        let status = fixture_all_entry_oracle::status(&result);
        assert_eq!(status["status"], "REFUSED");
        assert!(status["coverage"].is_null() && status["fingerprint"].is_null());
        assert!(!status.to_string().contains("PRIVATE_CANCEL_DETAIL"));
        let mut b = budget();
        b.deadline = Instant::now() - Duration::from_secs(1);
        assert!(fixture_oracle(&fixture, &b).is_err());
        let b = budget();
        let absent = fixture.base.join("absent");
        let result = fixture_all_entry_oracle::capture(&[("fixture", absent.as_path())], &b);
        assert!(fixture_all_entry_oracle::status(&result)["coverage"].is_null());
        assert!(!absent.exists());
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn all_entry_oracle_reparse_refusal_never_exports_a_baseline() {
        let fixture = Fixture::new();
        let alias = fixture.base.join("linked-engine");
        create_owned_test_junction(&fixture.base, &alias, &fixture.engine).unwrap();
        let b = budget();
        let result = fixture_all_entry_oracle::capture(&[("fixture", fixture.base.as_path())], &b);
        assert_eq!(result.as_ref().unwrap_err().category, "linked_or_special");
        let status = fixture_all_entry_oracle::status(&result);
        assert!(status["coverage"].is_null() && status["fingerprint"].is_null());
        std::fs::remove_dir(alias).unwrap();
    }
    #[test]
    fn external_runner_simulated_phases_keep_guards_and_originals() {
        for scenario in ["success", "invalid_utf8", "build_failure", "unconfirmed"] {
            let fixture = Fixture::new();
            let b = budget();
            let before = fixture.originals();
            let mut workspace =
                crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
            let staged = {
                let envelope = workspace.source_envelope(b.deadline()).unwrap();
                fixture
                    .plan(Mode::DependencyRead, &b)
                    .stage_owned(&envelope, &b)
                    .unwrap()
            };
            let pending_tool = workspace.path.join("simulated-compiler.exe");
            let value = crate::external_native::fake_validation_for_test(
                &mut workspace,
                &staged,
                scenario,
                |workspace| {
                    assert!(workspace.lifetime.unresolved());
                    assert!(workspace.clean().is_err());
                    assert!(ContentLock::exclusive(&fixture.engine).is_err());
                    assert!(std::fs::write(&pending_tool, b"changed compiler").is_err());
                    assert!(std::fs::rename(
                        &pending_tool,
                        workspace.path.join("rebound-compiler.exe")
                    )
                    .is_err());
                },
            );
            match scenario {
                "success" => {
                    let value = value.unwrap();
                    assert_eq!(value["test_simulated_runner"], true);
                    assert_eq!(value["artifact"]["sha256"].as_str().unwrap().len(), 64);
                    assert_eq!(value["artifact"]["loaded"], false);
                    assert_eq!(value["published"], false);
                }
                "invalid_utf8" => {
                    assert_eq!(value.unwrap_err(), "external_compile_output_not_utf8")
                }
                "build_failure" => assert_eq!(value.unwrap_err(), "external_cargo_build_failed"),
                "unconfirmed" => assert_eq!(
                    value.unwrap_err(),
                    "external_compile_retirement_unconfirmed"
                ),
                _ => unreachable!(),
            }
            assert_eq!(fixture.originals(), before);
            drop(staged);
            workspace.clean().unwrap();
            drop(ContentLock::exclusive(&fixture.engine).unwrap());
            assert_eq!(fixture.originals(), before);
        }
    }
    #[test]
    fn external_runner_build_true_and_string_refuse_without_mutation() {
        let fixture = Fixture::new();
        let before = fixture.originals();
        for declaration in ["build=true", "build=\"build.rs\""] {
            let manifest = Fixture::native_manifest("../../vendor/value-lib")
                .replace("build=false", declaration);
            assert_eq!(
                cargo_shape(manifest.as_bytes()).err().unwrap(),
                "unsupported_cargo_build_inputs"
            );
        }
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn compiler_output_directories_accept_held_windows_separator_spellings() {
        let fixture = Fixture::new();
        let expected = fixture.base.join("private-output");
        std::fs::create_dir(&expected).unwrap();
        let b = budget();
        let held = crate::snapshot::open_checked_directory(&expected, b.deadline).unwrap();
        let identity = file_identity(&held).unwrap();
        let ordinary = expected.to_string_lossy().into_owned();
        let verbatim = expected
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for reported in [
            ordinary.clone(),
            ordinary.replace('\\', "/"),
            verbatim.clone(),
            verbatim.replace('\\', "/"),
        ] {
            checked_compiler_output_directory(
                &expected,
                &identity,
                &held,
                &reported,
                &b,
                CompilerDirectoryRole::WritableOutput,
            )
            .unwrap();
        }
    }
    #[test]
    fn compiler_output_directories_refuse_sibling_outside_and_reparse_aliases() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new();
        let expected = fixture.base.join("private-output");
        let sibling = fixture.base.join("private-output-other");
        let outside = std::env::temp_dir().join(format!(
            "external-output-outside-{}",
            crate::random::session_id()
        ));
        for path in [&expected, &sibling, &outside] {
            std::fs::create_dir(path).unwrap();
        }
        let b = budget();
        let held = crate::snapshot::open_checked_directory(&expected, b.deadline).unwrap();
        let identity = file_identity(&held).unwrap();
        for path in [&sibling, &outside] {
            assert_eq!(
                checked_compiler_output_directory(
                    &expected,
                    &identity,
                    &held,
                    &path.to_string_lossy(),
                    &b,
                    CompilerDirectoryRole::WritableOutput,
                )
                .unwrap_err(),
                "external_compile_output_directory_identity_refused"
            );
        }
        for (alias, target, reported) in [
            (
                fixture.base.join("output-junction"),
                expected.clone(),
                fixture.base.join("output-junction"),
            ),
            (
                fixture.base.join("parent-junction"),
                fixture.base.clone(),
                fixture.base.join("parent-junction/private-output"),
            ),
        ] {
            let result = std::process::Command::new("cmd.exe")
                .creation_flags(0x0800_0000)
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&alias)
                .arg(&target)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "closed junction fixture setup failed"
            );
            assert_eq!(
                checked_compiler_output_directory(
                    &expected,
                    &identity,
                    &held,
                    &reported.to_string_lossy(),
                    &b,
                    CompilerDirectoryRole::WritableOutput,
                )
                .unwrap_err(),
                "external_compile_output_directory_path_refused"
            );
            std::fs::remove_dir(&alias).unwrap();
        }
        std::fs::remove_dir(&outside).unwrap();
    }
    #[test]
    #[ignore = "requires reviewed retained runner and one serialized real offline Cargo lane"]
    fn external_runner_actual_local_cargo_with_copied_config_inactive() {
        // Never serialize an arbitrary error String or private path into this report.
        fn failure_category(error: &str) -> &'static str {
            match error {
                "external_native_cwd_platform_budget_refused" => "PLATFORM_CWD_BUDGET",
                "external_rustc_host_failed" => "RUSTC_PHASE",
                "external_cargo_metadata_failed" => "METADATA_PHASE",
                "external_cargo_build_failed" => "BUILD_PHASE",
                "external_compile_retirement_unconfirmed" => "RETIREMENT_UNCONFIRMED",
                "external_compile_parent_deadline" => "PARENT_DEADLINE",
                "external_compile_cancelled_or_expired" => "CANCELLED_OR_EXPIRED",
                "test_config_witness_unavailable"
                | "test_config_witness_changed"
                | "test_config_witness_count_refused"
                | "test_config_cleanup_refused"
                | "test_config_creation_refused"
                | "test_copied_config_cwd_refused" => "COPIED_CONFIG_WITNESS",
                _ => "REDACTED_FAILURE",
            }
        }
        let fixture = Fixture::new();
        std::fs::write(fixture.engine.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\n").unwrap();
        let b = budget();
        let before = fixture_oracle(&fixture, &b);
        let mut workspace = None;
        let mut staged_retained = false;
        let mut stage = "fixture_oracle_before";
        let result = (|| -> Result<serde_json::Value, String> {
            before.as_ref().map_err(|_| "test_before_oracle_refused")?;
            stage = "workspace_allocation";
            workspace = Some(std::mem::ManuallyDrop::new(
                crate::candidate_workspace::Workspace::for_test_project_native_compile(
                    &fixture.app,
                )?,
            ));
            let workspace = workspace.as_mut().unwrap();
            stage = "source_staging";
            let envelope = workspace.source_envelope(b.deadline())?;
            let staged = std::mem::ManuallyDrop::new(
                fixture
                    .plan(Mode::DependencyRead, &b)
                    .stage_owned(&envelope, &b)?,
            );
            staged_retained = true;
            stage = "actual_compile";
            crate::external_native::actual_copied_config_witness_for_test(workspace, &staged)
        })();
        let after = fixture_oracle(&fixture, &b);
        let originals_match = match (&before, &after) {
            (Ok(a), Ok(b)) => Some(a == b),
            _ => None,
        };
        let mut report = match &result {
            Ok(value) => value.clone(),
            Err(error) => serde_json::json!({"result":"REFUSED","primary_stage":stage,
                "failure_category":failure_category(error),"error_details":"REDACTED",
                "artifact_config_and_stream_receipts":"UNAVAILABLE_FROM_FAILED_RESULT_API"}),
        };
        report["actual_api_succeeded"] = serde_json::json!(result.is_ok());
        report["fixture_oracle_before"] = fixture_all_entry_oracle::status(&before);
        report["fixture_oracle_after"] = fixture_all_entry_oracle::status(&after);
        report["originals_and_cache_match_after_phase"] = serde_json::json!(originals_match);
        report["oracle_coverage"] = serde_json::json!(if before.is_ok() && after.is_ok() {
            Some("ALL_ENTRY")
        } else {
            None
        });
        report["oracle_deadline_reused_without_extension"] = serde_json::json!(true);
        report["original_workspace_holder"] = serde_json::json!({"kind":"ManuallyDrop","until":"HOST_EXIT",
            "present":workspace.is_some(),"lifecycle_proof":false});
        report["original_staged_guard_holder"] = serde_json::json!({"kind":"ManuallyDrop","until":"HOST_EXIT",
            "present":staged_retained,"lifecycle_proof":false});
        let lifetime_unresolved = workspace
            .as_ref()
            .map(|workspace| workspace.lifetime.unresolved());
        report["workspace_lifetime_unresolved_before_test_exit"] =
            serde_json::json!(lifetime_unresolved);
        report["workspace_clean_api_ok"] =
            serde_json::json!("NOT_ATTEMPTED_TEST_INPUTS_RETAINED_UNTIL_HOST_EXIT");
        let receipt = fixture.base.join("actual-private-compile-receipt.json");
        std::fs::write(&receipt, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!("private_compile_receipt={}", receipt.display());
        // Report phase/oracle refusal before asserting. Holding inputs proves no retirement.
        assert_eq!(originals_match, Some(true));
        let Ok(value) = result else {
            panic!("actual compile refusal was recorded before assertions");
        };
        assert_eq!(value["validation"], "private_local_native_compile");
        assert_eq!(value["native_artifact_produced"], true);
        assert_eq!(value["test_copied_config_inactive_witnesses"], 2);
        assert_eq!(value["compiler"]["environment_cleared"], true);
        assert_eq!(value["compiler"]["caller_cache_used"], false);
        for name in ["rustc", "metadata", "build"] {
            assert_eq!(value["streams"][name]["owned_tree_retired"], true);
            assert_eq!(value["streams"][name]["owned_readers_joined"], true);
        }
        assert_eq!(lifetime_unresolved, Some(false));
    }
    #[test]
    #[ignore = "requires reviewed known-Cargo diagnostic producer and one serialized real offline Cargo lane"]
    fn external_runner_actual_known_cargo_build_private_diagnostic() {
        // Never serialize an arbitrary error String or private path into this report.
        fn failure_category(error: &str) -> &'static str {
            match error {
                "external_native_cwd_platform_budget_refused" => "PLATFORM_CWD_BUDGET",
                "external_rustc_host_failed" => "RUSTC_PHASE",
                "external_cargo_metadata_failed" => "METADATA_PHASE",
                "external_cargo_build_failed" => "BUILD_PHASE",
                "external_compile_retirement_unconfirmed" => "RETIREMENT_UNCONFIRMED",
                "external_compile_parent_deadline" => "PARENT_DEADLINE",
                "external_compile_cancelled_or_expired" => "CANCELLED_OR_EXPIRED",
                "test_config_witness_unavailable"
                | "test_config_witness_changed"
                | "test_config_witness_count_refused"
                | "test_config_cleanup_refused"
                | "test_config_creation_refused"
                | "test_copied_config_cwd_refused" => "COPIED_CONFIG_WITNESS",
                _ => "REDACTED_FAILURE",
            }
        }
        let fixture = Fixture::new();
        std::fs::write(fixture.engine.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\n").unwrap();
        let b = budget();
        let before = fixture_oracle(&fixture, &b);
        let mut workspace = None;
        let mut staged_retained = false;
        let mut diagnostic = None;
        let mut stage = "fixture_oracle_before";
        let result = (|| -> Result<serde_json::Value, String> {
            before.as_ref().map_err(|_| "test_before_oracle_refused")?;
            stage = "workspace_allocation";
            workspace = Some(std::mem::ManuallyDrop::new(
                crate::candidate_workspace::Workspace::for_test_project_native_compile(
                    &fixture.app,
                )?,
            ));
            let workspace = workspace.as_mut().unwrap();
            stage = "source_staging";
            let envelope = workspace.source_envelope(b.deadline())?;
            let staged = std::mem::ManuallyDrop::new(
                fixture
                    .plan(Mode::DependencyRead, &b)
                    .stage_owned(&envelope, &b)?,
            );
            staged_retained = true;
            stage = "actual_compile";
            let permit = fixture.cargo_diagnostic_permit(workspace, &staged)?;
            crate::external_native::actual_copied_config_diagnostic_for_test(
                workspace,
                &staged,
                permit,
                &mut diagnostic,
            )
        })();
        let after = fixture_oracle(&fixture, &b);
        let originals_match = match (&before, &after) {
            (Ok(a), Ok(b)) => Some(a == b),
            _ => None,
        };
        let mut report = match &result {
            Ok(value) => value.clone(),
            Err(error) => serde_json::json!({"result":"REFUSED","primary_stage":stage,
                "failure_category":failure_category(error),"error_details":"REDACTED",
                "artifact_config_and_stream_receipts":"UNAVAILABLE_FROM_FAILED_RESULT_API"}),
        };
        report["actual_api_succeeded"] = serde_json::json!(result.is_ok());
        report["fixture_oracle_before"] = fixture_all_entry_oracle::status(&before);
        report["fixture_oracle_after"] = fixture_all_entry_oracle::status(&after);
        report["originals_and_cache_match_after_phase"] = serde_json::json!(originals_match);
        report["oracle_coverage"] = serde_json::json!(if before.is_ok() && after.is_ok() {
            Some("ALL_ENTRY")
        } else {
            None
        });
        report["oracle_deadline_reused_without_extension"] = serde_json::json!(true);
        report["original_workspace_holder"] = serde_json::json!({"kind":"ManuallyDrop","until":"HOST_EXIT",
            "present":workspace.is_some(),"lifecycle_proof":false});
        report["original_staged_guard_holder"] = serde_json::json!({"kind":"ManuallyDrop","until":"HOST_EXIT",
            "present":staged_retained,"lifecycle_proof":false});
        let lifetime_unresolved = workspace
            .as_ref()
            .map(|workspace| workspace.lifetime.unresolved());
        report["workspace_lifetime_unresolved_before_test_exit"] =
            serde_json::json!(lifetime_unresolved);
        report["workspace_clean_api_ok"] =
            serde_json::json!("NOT_ATTEMPTED_TEST_INPUTS_RETAINED_UNTIL_HOST_EXIT");
        report["test_grant_authority"] =
            serde_json::json!("simulated; no attended product grant approval");
        let artifact_written = diagnostic
            .as_ref()
            .map(|handle| handle.write_private().is_ok());
        let completion = diagnostic.as_ref().and_then(|handle| handle.completion());
        report["real_cargo_known_fixture_diagnostic"] = serde_json::json!({"present":diagnostic.is_some(),
            "private_artifact_write_ok":artifact_written,
            "original_completion_available":completion.is_some(),
            "raw_payload_exported":false,"holder":"ManuallyDrop until HOST_EXIT; original controller clone"});
        let receipt = fixture.base.join("actual-private-compile-receipt.json");
        std::fs::write(&receipt, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!("private_compile_receipt={}", receipt.display());
        // Report phase/oracle refusal before asserting. Holding inputs proves no retirement.
        assert_eq!(originals_match, Some(true));
        assert_eq!(artifact_written, Some(true));
        let completion = completion.expect("valid build requires original positive completion");
        assert_eq!(completion.root_exit, 0);
        assert!(completion.tree && completion.readers && completion.eof == (true, true));
        assert!(completion.stdout > 0);
        assert!(completion.stdout <= 4 * 1024 * 1024 && completion.stderr <= 64 * 1024);
        let Ok(value) = result else {
            panic!("actual compile refusal was recorded before assertions");
        };
        assert_eq!(value["validation"], "private_local_native_compile");
        assert_eq!(value["native_artifact_produced"], true);
        assert_eq!(value["test_copied_config_inactive_witnesses"], 2);
        assert_eq!(value["compiler"]["environment_cleared"], true);
        assert_eq!(value["compiler"]["caller_cache_used"], false);
        for name in ["rustc", "metadata", "build"] {
            assert_eq!(value["streams"][name]["owned_tree_retired"], true);
            assert_eq!(value["streams"][name]["owned_readers_joined"], true);
        }
        assert_eq!(lifetime_unresolved, Some(false));
    }
    #[test]
    #[ignore = "requires reviewed known-Cargo diagnostic producer and one serialized real offline Cargo lane"]
    fn external_runner_known_cargo_error_retains_private_diagnostic() {
        let fixture = Fixture::new();
        std::fs::write(fixture.engine.join("native/src/lib.rs"),
            "#[no_mangle] pub extern \"C\" fn value() -> i32 { \"known-fixture-private-marker\" }\n").unwrap();
        std::fs::write(fixture.engine.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\n").unwrap();
        let b = budget();
        let before = fixture_oracle(&fixture, &b);
        let mut diagnostic = None;
        let mut workspace = std::mem::ManuallyDrop::new(
            crate::candidate_workspace::Workspace::for_test_project_native_compile(&fixture.app)
                .unwrap(),
        );
        let result = (|| -> Result<serde_json::Value, String> {
            before.as_ref().map_err(|_| "test_before_oracle_refused")?;
            let envelope = workspace.source_envelope(b.deadline())?;
            let staged = std::mem::ManuallyDrop::new(
                fixture
                    .plan(Mode::DependencyRead, &b)
                    .stage_owned(&envelope, &b)?,
            );
            let permit = fixture.cargo_diagnostic_permit(&workspace, &staged)?;
            crate::external_native::actual_copied_config_diagnostic_for_test(
                &mut workspace,
                &staged,
                permit,
                &mut diagnostic,
            )
        })();
        let after = fixture_oracle(&fixture, &b);
        let matched = match (&before, &after) {
            (Ok(a), Ok(b)) => Some(a == b),
            _ => None,
        };
        let artifact_written = diagnostic
            .as_ref()
            .map(|handle| handle.write_private().is_ok());
        let completion = diagnostic.as_ref().and_then(|handle| handle.completion());
        let codes = diagnostic
            .as_ref()
            .map(|handle| handle.compiler_error_codes())
            .unwrap_or_default();
        let report = serde_json::json!({"scope":"real Cargo controlled compile error; simulated product grant", "actual_api_succeeded":result.is_ok(),
            "private_artifact_write_ok":artifact_written,"original_completion_available":completion.is_some(),"originals_and_cache_match":matched,
            "raw_payload_exported":false,"workspace_clean":"NOT_ATTEMPTED_INPUTS_RETAINED","worker_retirement_inferred_from_holder":false});
        std::fs::write(
            fixture.base.join("known-cargo-error-receipt.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
        assert_eq!(matched, Some(true));
        assert_eq!(artifact_written, Some(true));
        assert_eq!(
            result.as_ref().err().map(String::as_str),
            Some("external_cargo_build_failed")
        );
        let completion = completion.expect("original completion must precede nonzero failure");
        assert_ne!(completion.root_exit, 0);
        assert!(completion.tree && completion.readers && completion.eof == (true, true));
        assert!(completion.stdout > 0);
        assert!(completion.stderr <= 64 * 1024);
        assert!(codes.iter().any(|code| code == "E0308"));
    }
    #[test]
    #[ignore = "requires separately reviewed one-phase retained-worker rustc diagnostic"]
    fn external_runner_rustc_vv_only_private_diagnostic() {
        let fixture = Fixture::new();
        let b = budget();
        // Retain this original test-owned object and its allocation/compilation
        // leases through unwinding and host exit. This is input retention only.
        let mut workspace = std::mem::ManuallyDrop::new(
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap(),
        );
        let before = fixture_oracle(&fixture, &b);
        let mut report = if before.is_ok() {
            let envelope = workspace.source_envelope(b.deadline()).unwrap();
            let staged = std::mem::ManuallyDrop::new(
                fixture
                    .plan(Mode::DependencyRead, &b)
                    .stage_owned(&envelope, &b)
                    .unwrap(),
            );
            let mut value =
                crate::external_native::rustc_vv_diagnostic_for_test(&mut workspace, &staged);
            value["original_staged_guard_holder"] = serde_json::json!({"kind":"ManuallyDrop","until":"HOST_EXIT","lifecycle_proof":false});
            value
        } else {
            serde_json::json!({"phase":"rustc_vV","phase_api_succeeded":false,
            "primary_stage":"fixture_oracle_before","worker_api_call":"NOT_ATTEMPTED"})
        };
        let after = fixture_oracle(&fixture, &b);
        let originals_match = match (&before, &after) {
            (Ok(a), Ok(b)) => Some(a == b),
            _ => None,
        };
        report["fixture_oracle_before"] = fixture_all_entry_oracle::status(&before);
        report["fixture_oracle_after"] = fixture_all_entry_oracle::status(&after);
        report["originals_and_cache_match_after_phase"] = serde_json::json!(originals_match);
        report["oracle_coverage"] = serde_json::json!(if before.is_ok() && after.is_ok() {
            Some("ALL_ENTRY")
        } else {
            None
        });
        let phase_succeeded = report["phase_api_succeeded"] == true;
        report["workspace_clean_api_ok"] =
            serde_json::json!("NOT_ATTEMPTED_TEST_INPUTS_RETAINED_UNTIL_HOST_EXIT");
        report["original_workspace_holder"] = serde_json::json!({"kind":"ManuallyDrop","until":"HOST_EXIT",
            "lifecycle_proof":false,"workspace_and_attached_leases_retained":true});
        report["workspace_lifetime_unresolved_before_test_exit"] =
            serde_json::json!(workspace.lifetime.unresolved());
        report["oracle_deadline_reused_without_extension"] = serde_json::json!(true);
        let path = fixture.base.join("rustc-vv-private-diagnostic.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        println!("private_rustc_vv_diagnostic_receipt={}", path.display());
        assert_eq!(originals_match, Some(true));
        assert!(
            phase_succeeded,
            "rustc-vV diagnostic recorded a primary phase failure"
        );
        assert_eq!(report["leases_verified_after"], true);
    }
    #[test]
    fn external_compile_selected_manifest_and_retained_content_leases_are_real() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture.originals();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("compile-stage"), &b)
            .unwrap();
        assert_eq!(
            staged.member_manifest,
            staged.envelope.join("engine/native/Cargo.toml")
        );
        let lease = staged.compilation_lease(&b).unwrap();
        for root in &staged.roots {
            assert!(ContentLock::exclusive(&root.source).is_err());
        }
        lease.verify(&b).unwrap();
        assert_eq!(fixture.originals(), before);
        drop(lease);
        for root in &staged.roots {
            drop(ContentLock::exclusive(&root.source).unwrap());
        }
    }
    #[test]
    fn external_compile_original_and_staged_drift_refuse_separately() {
        let fixture = Fixture::new();
        let b = budget();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("compile-drift"), &b)
            .unwrap();
        let private = staged.envelope.join("vendor/value-lib/src/lib.rs");
        let private_bytes = std::fs::read(&private).unwrap();
        std::fs::write(&private, "pub fn value()->i32 { 31 }\n").unwrap();
        assert_eq!(
            staged.compilation_lease(&b).err().unwrap(),
            "external_compile_staged_fingerprint_changed"
        );
        std::fs::write(&private, private_bytes).unwrap();
        let lease = staged.compilation_lease(&b).unwrap();
        assert!(std::fs::write(&private, "unallowed copied-source mutation").is_err());
        lease.verify(&b).unwrap();
        std::fs::write(
            fixture.vendor.join("src/lib.rs"),
            "pub fn value()->i32 { 32 }\n",
        )
        .unwrap();
        assert_eq!(
            lease.verify(&b).unwrap_err(),
            "external_compile_original_fingerprint_changed"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.engine.join("target/cache-sentinel")).unwrap(),
            "working Cargo cache\n"
        );
    }
    #[test]
    fn external_compile_deadline_is_not_reset_by_cleanup_resolution() {
        let fixture = Fixture::new();
        let b = budget();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let staged = {
            let envelope = workspace.source_envelope(b.deadline()).unwrap();
            fixture
                .plan(Mode::DependencyRead, &b)
                .stage_owned(&envelope, &b)
                .unwrap()
        };
        let lease = std::sync::Arc::new(staged.compilation_lease(&b).unwrap());
        workspace.retain_compilation(lease.clone()).unwrap();
        assert!(workspace.retain_compilation(lease.clone()).is_err());
        let mut claim = workspace.lifetime.claim().unwrap();
        assert!(workspace.clean().is_err());
        assert!(ContentLock::exclusive(&fixture.app).is_err());
        claim.resolve();
        let mut expired = budget();
        expired.deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(
            lease.verify(&expired).unwrap_err(),
            "external_staging_deadline"
        );
        assert!(Budget::clipped(expired.deadline, &|| Ok(())).is_err());
        drop(lease);
        drop(staged);
        workspace.clean().unwrap();
        drop(ContentLock::exclusive(&fixture.app).unwrap());
    }
    #[test]
    fn external_compile_all_staged_ancestors_remain_held_after_stage_returns() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture.originals();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("held-compile-stage"), &b)
            .unwrap();
        let parent = staged.envelope.join("vendor");
        let replacement = fixture.base.join("outside-redirect");
        let error = OpenOptions::new()
            .access_mode(0x0001_0000)
            .share_mode(7)
            .custom_flags(0x0200_0000)
            .open(&parent)
            .unwrap_err();
        assert!(matches!(error.raw_os_error(), Some(5 | 32)));
        assert!(std::fs::rename(&parent, &replacement).is_err());
        assert!(!replacement.exists());
        assert_eq!(fixture.originals(), before);
        staged.compilation_lease(&b).unwrap().verify(&b).unwrap();
    }
    fn metadata_for_fixture(staged: &StagedSources) -> serde_json::Value {
        serde_json::json!({"version":1,"workspace_root":staged.envelope.join("engine"),
        "workspace_members":["opaque-selected"],"packages":[
            {"name":"native-game","id":"opaque-selected","source":null,"manifest_path":staged.member_manifest,
            "targets":[{"name":"external_logic","kind":["cdylib"],"crate_types":["cdylib"],"src_path":staged.envelope.join("engine/native/src/lib.rs")}]},
            {"name":"value-lib","id":"opaque-support","source":null,"manifest_path":staged.envelope.join("vendor/value-lib/Cargo.toml"),
            "targets":[{"name":"value_lib","kind":["lib"],"crate_types":["rlib"],"src_path":staged.envelope.join("vendor/value-lib/src/lib.rs")}]}
        ]})
    }
    fn windows_spelling(path: &Path, style: usize) -> String {
        let text = if style < 2 {
            path.to_path_buf()
        } else {
            path.canonicalize().unwrap()
        }
        .to_string_lossy()
        .into_owned();
        if style == 1 || style == 3 {
            text.replace('\\', "/")
        } else {
            text
        }
    }
    fn metadata_spelling(staged: &StagedSources, style: usize) -> serde_json::Value {
        let mut value = metadata_for_fixture(staged);
        value["workspace_root"] = serde_json::json!(windows_spelling(
            staged.cargo_manifest.parent().unwrap(),
            style
        ));
        for package in value["packages"].as_array_mut().unwrap() {
            package["manifest_path"] = serde_json::json!(windows_spelling(
                Path::new(package["manifest_path"].as_str().unwrap()),
                style
            ));
            for target in package["targets"].as_array_mut().unwrap() {
                target["src_path"] = serde_json::json!(windows_spelling(
                    Path::new(target["src_path"].as_str().unwrap()),
                    style
                ));
            }
        }
        value
    }
    fn mixed_expected_path(path: &Path) -> PathBuf {
        let mut text = windows_spelling(path, 1);
        let last = text.rfind('/').unwrap();
        text.replace_range(last..last + 1, "\\");
        assert!(text.contains('/') && text.contains('\\'));
        PathBuf::from(text)
    }
    #[test]
    fn external_metadata_files_accept_spellings_and_refuse_identity_changes() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new();
        let before = fixture.originals();
        let b = budget();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("metadata-spelling"), &b)
            .unwrap();
        let hardlink = fixture.base.join("outside-manifest.toml");
        // Stage already retains its destination ancestors and parent. This
        // proves alias creation is denied, not same-file metadata alias binding.
        assert!(!hardlink.try_exists().unwrap());
        let denied = std::fs::hard_link(&staged.member_manifest, &hardlink)
            .expect_err("staging guards must refuse a new hard-link alias");
        assert_eq!(denied.raw_os_error(), Some(32));
        assert!(!hardlink.try_exists().unwrap());
        let lease = staged.compilation_lease(&b).unwrap();
        let mut mixed_expected = staged.clone();
        mixed_expected.member_manifest = mixed_expected_path(&staged.member_manifest);
        mixed_expected.cargo_manifest = mixed_expected_path(&staged.cargo_manifest);
        mixed_expected.envelope = mixed_expected_path(&staged.envelope);
        for style in 0..4 {
            for expected in [&staged, &mixed_expected] {
                let selected = crate::external_native::selected_member(
                    expected,
                    &metadata_spelling(&staged, style),
                    &lease,
                    &b,
                )
                .unwrap();
                assert_eq!(selected.package_id, "opaque-selected");
                assert_eq!(selected.manifest, staged.member_manifest);
                assert_eq!(
                    selected.source,
                    staged.member_manifest.parent().unwrap().join("src/lib.rs")
                );
                assert!(!selected.manifest.to_string_lossy().contains('/'));
                assert!(!selected.source.to_string_lossy().contains('/'));
            }
        }
        let metadata = metadata_for_fixture(&staged);
        let select = |value: &serde_json::Value| {
            crate::external_native::selected_member(&staged, value, &lease, &b)
        };
        for path in [
            &fixture.engine.join("native/Cargo.toml"),
            &staged.envelope.join("vendor/value-lib/Cargo.toml"),
        ] {
            let mut wrong = metadata.clone();
            wrong["packages"][0]["manifest_path"] = serde_json::json!(windows_spelling(path, 1));
            assert!(select(&wrong).is_err());
        }
        let mut source = metadata.clone();
        source["packages"][0]["targets"][0]["src_path"] =
            metadata["packages"][1]["targets"][0]["src_path"].clone();
        assert_eq!(
            select(&source).err().unwrap(),
            "external_selected_source_changed"
        );
        for path in [
            fixture.vendor.join("src/lib.rs"),
            fixture.base.join("missing.rs"),
        ] {
            source["packages"][0]["targets"][0]["src_path"] = serde_json::json!(path);
            assert!(select(&source).is_err());
        }
        let mut workspace = metadata.clone();
        workspace["workspace_root"] = serde_json::json!(staged.envelope.join("vendor/value-lib"));
        assert!(select(&workspace).is_err());
        let mut duplicate = metadata.clone();
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(metadata_spelling(&staged, 1)["packages"][0].clone());
        assert_eq!(
            select(&duplicate).err().unwrap(),
            "external_selected_member_ambiguous"
        );
        let mut membership = metadata.clone();
        membership["workspace_members"] = serde_json::json!(["opaque-support"]);
        assert_eq!(
            select(&membership).err().unwrap(),
            "external_selected_member_not_in_workspace"
        );
        let alias = fixture.base.join("metadata-junction");
        let result = std::process::Command::new("cmd.exe")
            .creation_flags(0x0800_0000)
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&alias)
            .arg(staged.member_manifest.parent().unwrap())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "closed junction fixture setup failed"
        );
        let mut redirected = metadata.clone();
        redirected["packages"][0]["manifest_path"] = serde_json::json!(alias.join("Cargo.toml"));
        assert!(select(&redirected).is_err());
        redirected = metadata.clone();
        redirected["packages"][0]["targets"][0]["src_path"] =
            serde_json::json!(alias.join("src/lib.rs"));
        assert!(select(&redirected).is_err());
        std::fs::remove_dir(&alias).unwrap();
        lease.verify(&b).unwrap();
        assert_eq!(fixture.originals(), before);
    }
    fn artifact_message(
        selected: &crate::external_native::SelectedMember,
        path: &Path,
        style: usize,
    ) -> serde_json::Value {
        serde_json::json!({"reason":"compiler-artifact","package_id":selected.package_id,
            "manifest_path":windows_spelling(&selected.manifest, style),
            "target":{"name":selected.target,"crate_types":["cdylib"],"src_path":windows_spelling(&selected.source, style)},
            "filenames":[windows_spelling(path, style)]})
    }
    #[test]
    fn external_artifact_paths_accept_spellings_and_refuse_identity_changes() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new();
        let before = fixture.originals();
        let b = budget();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("artifact-spelling"), &b)
            .unwrap();
        let compiler_root = fixture.base.join("compiler-state");
        std::fs::create_dir(&compiler_root).unwrap();
        let target = compiler_root.join("target");
        std::fs::create_dir(&target).unwrap();
        let path = target.join("selected.dll");
        std::fs::write(&path, b"closed artifact fixture").unwrap();
        let sibling = fixture.base.join("compiler-target-other");
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(sibling.join("selected.dll"), b"closed artifact fixture").unwrap();
        let lease = staged
            .compilation_lease(&b)
            .unwrap()
            .hold_compiler_state(
                &compiler_root,
                std::slice::from_ref(&compiler_root),
                std::slice::from_ref(&target),
                &[],
                &[],
                &b,
            )
            .unwrap();
        let selected = crate::external_native::selected_member(
            &staged,
            &metadata_for_fixture(&staged),
            &lease,
            &b,
        )
        .unwrap();
        let parent = crate::external_native::ParentDeadline::at(b.deadline()).unwrap();
        let mixed_selected = crate::external_native::SelectedMember {
            package: selected.package.clone(),
            package_id: selected.package_id.clone(),
            target: selected.target.clone(),
            manifest: mixed_expected_path(&selected.manifest),
            source: mixed_expected_path(&selected.source),
        };
        let mixed_target = mixed_expected_path(&target);
        for style in 0..4 {
            let stdout = format!(
                "{}\n{}\n",
                artifact_message(&selected, &path, style),
                serde_json::json!({"reason":"build-finished","success":true})
            );
            for (expected, root) in [(&selected, &target), (&mixed_selected, &mixed_target)] {
                let (actual, held, bytes) = crate::external_native::exact_artifact(
                    &stdout, expected, root, &lease, &parent,
                )
                .unwrap();
                assert_eq!(actual, path);
                assert_eq!(bytes, b"closed artifact fixture");
                crate::snapshot::check_opened_path(&held, &actual, b.deadline()).unwrap();
            }
        }
        let reject = |message: &serde_json::Value| {
            let stdout = format!(
                "{}\n{}\n",
                message,
                serde_json::json!({"reason":"build-finished","success":true})
            );
            assert!(crate::external_native::exact_artifact(
                &stdout, &selected, &target, &lease, &parent
            )
            .is_err());
        };
        let message = artifact_message(&selected, &path, 1);
        let mut wrong = message.clone();
        wrong["target"]["src_path"] =
            serde_json::json!(staged.envelope.join("vendor/value-lib/src/lib.rs"));
        reject(&wrong);
        wrong = message.clone();
        wrong["manifest_path"] =
            serde_json::json!(staged.envelope.join("vendor/value-lib/Cargo.toml"));
        reject(&wrong);
        wrong = message.clone();
        wrong["filenames"] = serde_json::json!([sibling.join("selected.dll")]);
        reject(&wrong);
        wrong = message.clone();
        wrong["filenames"] = serde_json::json!([path, 1]);
        reject(&wrong);
        wrong = message.clone();
        wrong["package_id"] = serde_json::json!("other-package");
        reject(&wrong);
        wrong = message.clone();
        wrong["target"]["name"] = serde_json::json!("other-target");
        reject(&wrong);
        let duplicate = format!(
            "{message}\n{message}\n{}\n",
            serde_json::json!({"reason":"build-finished","success":true})
        );
        assert!(crate::external_native::exact_artifact(
            &duplicate, &selected, &target, &lease, &parent
        )
        .is_err());
        let alias = fixture.base.join("artifact-junction");
        let result = std::process::Command::new("cmd.exe")
            .creation_flags(0x0800_0000)
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&alias)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "closed junction fixture setup failed"
        );
        wrong = message.clone();
        wrong["filenames"] = serde_json::json!([alias.join("selected.dll")]);
        reject(&wrong);
        std::fs::remove_dir(&alias).unwrap();
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn external_compile_metadata_selects_exact_copied_member_and_refuses_scope_changes() {
        let fixture = Fixture::new();
        let b = budget();
        let before = fixture.originals();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("metadata-stage"), &b)
            .unwrap();
        let lease = staged.compilation_lease(&b).unwrap();
        let metadata = metadata_for_fixture(&staged);
        let select = |value: &serde_json::Value| {
            crate::external_native::selected_member(&staged, value, &lease, &b)
        };
        let selected = select(&metadata).unwrap();
        assert_eq!(selected.package, "native-game");
        assert_eq!(selected.package_id, "opaque-selected");
        assert_eq!(selected.target, "external_logic");
        let mut registry = metadata.clone();
        registry["packages"][1]["source"] = serde_json::json!("registry+PRIVATE_SENTINEL");
        let error = select(&registry).err().unwrap();
        assert_eq!(error, "external_registry_or_git_dependency_unsupported");
        assert!(!error.contains("PRIVATE_SENTINEL"));
        let mut outside = metadata.clone();
        outside["packages"][1]["manifest_path"] =
            serde_json::json!(fixture.vendor.join("Cargo.toml"));
        assert!(select(&outside).is_err());
        let mut wrong = metadata.clone();
        wrong["packages"][0]["manifest_path"] = metadata["packages"][1]["manifest_path"].clone();
        assert!(select(&wrong).is_err());
        let mut script = metadata.clone();
        script["packages"][1]["targets"][0]["kind"] = serde_json::json!(["custom-build"]);
        assert!(select(&script).is_err());
        let mut proc_macro = metadata.clone();
        proc_macro["packages"][1]["targets"][0]["kind"] = serde_json::json!(["proc-macro"]);
        assert!(select(&proc_macro).is_err());
        let mut duplicate = metadata.clone();
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(metadata["packages"][0].clone());
        assert!(select(&duplicate).is_err());
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn external_compile_requires_authored_build_false_without_rewriting_staging() {
        let fixture = Fixture::new();
        let native = fixture.engine.join("native/Cargo.toml");
        let original = std::fs::read_to_string(&native)
            .unwrap()
            .replace("build=false\n", "");
        std::fs::write(&native, &original).unwrap();
        let b = budget();
        let before = fixture.originals();
        let staged = fixture
            .plan(Mode::DependencyRead, &b)
            .stage(&fixture.base.join("implicit-build-stage"), &b)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&staged.member_manifest).unwrap(),
            original
        );
        assert_eq!(
            staged.compilation_lease(&b).err().unwrap(),
            "external_compile_requires_explicit_build_false"
        );
        assert_eq!(fixture.originals(), before);
        assert!(!staged
            .member_manifest
            .parent()
            .unwrap()
            .join("build.rs")
            .exists());
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    struct TestOwnerAuthority;
    impl OwnerAuthority for TestOwnerAuthority {
        fn approve(&self, request: &ApprovalRequest<'_>) -> Result<bool, String> {
            assert!(request.imported_project.is_absolute() && request.requested_root.is_absolute());
            assert_eq!(request.revision.0, "1".repeat(64));
            assert!(matches!(
                request.mode,
                Mode::DependencyRead | Mode::SourcePublish
            ));
            Ok(true)
        }
    }
    struct PublicationFixtureOwner<'a> {
        project: &'a ImportedProject,
        canonical_root: &'a Path,
    }
    impl OwnerAuthority for PublicationFixtureOwner<'_> {
        fn approve(&self, request: &ApprovalRequest<'_>) -> Result<bool, String> {
            assert_eq!(request.imported_project, self.project.root.as_path());
            assert_eq!(request.requested_root, self.canonical_root);
            assert_eq!(request.revision, &self.project.revision);
            assert_eq!(request.mode, Mode::DependencyRead);
            Ok(true)
        }
    }
    struct Fixture {
        base: PathBuf,
        app: PathBuf,
        engine: PathBuf,
        vendor: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "external-workspace-fixture-{}",
                crate::random::session_id()
            ));
            #[cfg(windows)]
            assert!(
                crate::project::clean_path(std::path::absolute(&base).unwrap())
                    .to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with(&crate::test_drive())
            );
            let app = base.join("app");
            let engine = base.join("engine");
            let vendor = base.join("vendor/value-lib");
            for path in [
                &app,
                &engine.join("native/src"),
                &vendor.join("src"),
                &engine.join("target"),
                &app.join(".godot"),
                &base.join("ungranted"),
            ] {
                std::fs::create_dir_all(path).unwrap();
            }
            std::fs::write(
                app.join("aurum.toml"),
                "schema_version=1\nname=\"external-staging-fixture\"\nmodules=[]\n",
            )
            .unwrap();
            std::fs::write(app.join("project.godot"), "config_version=5\n").unwrap();
            std::fs::write(app.join("main.gd"), "extends Node\n# original\n").unwrap();
            std::fs::write(
                app.join(".env"),
                "PRIVATE_FIXTURE_KEY=not-a-real-credential\n",
            )
            .unwrap();
            std::fs::write(app.join(".godot/cache-sentinel"), "working import cache\n").unwrap();
            std::fs::write(
                engine.join("Cargo.toml"),
                "[workspace]\nresolver=\"2\"\nmembers=[\"native\"]\n",
            )
            .unwrap();
            std::fs::write(
                engine.join("Cargo.lock"),
                "# pinned dependency-free fixture lock\nversion=4\n",
            )
            .unwrap();
            std::fs::write(
                engine.join("native/Cargo.toml"),
                Self::native_manifest("../../vendor/value-lib"),
            )
            .unwrap();
            std::fs::write(
                engine.join("native/src/lib.rs"),
                "#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() }\n",
            )
            .unwrap();
            std::fs::write(
                vendor.join("Cargo.toml"),
                "[package]\nname=\"value-lib\"\nversion=\"0.1.0\"\nedition=\"2021\"\nbuild=false\n",
            )
            .unwrap();
            std::fs::write(vendor.join("src/lib.rs"), "pub fn value() -> i32 { 17 }\n").unwrap();
            std::fs::write(
                engine.join("target/cache-sentinel"),
                "working Cargo cache\n",
            )
            .unwrap();
            std::fs::write(
                base.join("ungranted/keep"),
                "ungranted sibling must never be copied\n",
            )
            .unwrap();
            Self {
                base,
                app,
                engine,
                vendor,
            }
        }
        // The sole constructor is in the controlled Fixture, not a project API.
        fn cargo_diagnostic_permit(
            &self,
            workspace: &crate::candidate_workspace::Workspace,
            staged: &StagedSources,
        ) -> Result<KnownCargoFixturePermit, String> {
            let source = std::fs::read(self.engine.join("native/src/lib.rs"))
                .map_err(|_| "test_known_cargo_source_unavailable")?;
            let valid = b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() }\n";
            let controlled_error=b"#[no_mangle] pub extern \"C\" fn value() -> i32 { \"known-fixture-private-marker\" }\n";
            if source != valid && source != controlled_error {
                return Err("test_known_cargo_source_refused".into());
            }
            if std::fs::read(self.vendor.join("src/lib.rs"))
                .map_err(|_| "test_known_cargo_source_unavailable")?
                != b"pub fn value() -> i32 { 17 }\n"
                || std::fs::read_to_string(self.engine.join("native/Cargo.toml"))
                    .map_err(|_| "test_known_cargo_source_unavailable")?
                    != Self::native_manifest("../../vendor/value-lib")
            {
                return Err("test_known_cargo_source_refused".into());
            }
            Ok(KnownCargoFixturePermit {
                artifact_root: self.base.clone(),
                workspace: workspace.path.clone(),
                member: staged.member_manifest.clone(),
                envelope: staged.envelope.clone(),
            })
        }
        fn native_manifest(path: &str) -> String {
            format!("[package]\nname=\"native-game\"\nversion=\"0.1.0\"\nedition=\"2021\"\nbuild=false\n[lib]\nname=\"external_logic\"\ncrate-type=[\"cdylib\"]\n[dependencies.value-lib]\npath=\"{path}\"\n")
        }
        fn imported(&self, budget: &Budget<'_>) -> ImportedProject {
            ImportedProject::bind(&self.app, Revision::new(&"1".repeat(64)).unwrap(), budget)
                .unwrap()
        }
        fn grants(
            &self,
            project: &ImportedProject,
            mode: Mode,
            budget: &Budget<'_>,
        ) -> Vec<VerifiedGrant> {
            vec![
                issue_grant(
                    &TestOwnerAuthority,
                    project,
                    RootId::new("engine").unwrap(),
                    &self.engine,
                    mode,
                    budget,
                )
                .unwrap(),
                issue_grant(
                    &TestOwnerAuthority,
                    project,
                    RootId::new("vendor").unwrap(),
                    &self.vendor,
                    Mode::DependencyRead,
                    budget,
                )
                .unwrap(),
            ]
        }
        fn publication_grant(
            &self,
            project: &ImportedProject,
            id: &str,
            requested: &Path,
            budget: &Budget<'_>,
        ) -> VerifiedGrant {
            let canonical_root = canonical_directory(requested, budget).unwrap().0;
            issue_grant(
                &PublicationFixtureOwner {
                    project,
                    canonical_root: &canonical_root,
                },
                project,
                RootId::new(id).unwrap(),
                requested,
                Mode::DependencyRead,
                budget,
            )
            .unwrap()
        }
        fn publication_grants(
            &self,
            project: &ImportedProject,
            budget: &Budget<'_>,
        ) -> Vec<VerifiedGrant> {
            vec![
                self.publication_grant(project, "engine", &self.engine, budget),
                self.publication_grant(project, "vendor", &self.vendor, budget),
            ]
        }
        fn placements() -> Vec<Placement> {
            vec![
                Placement {
                    root_id: RootId::new("project").unwrap(),
                    slot: "app".into(),
                },
                Placement {
                    root_id: RootId::new("engine").unwrap(),
                    slot: "engine".into(),
                },
                Placement {
                    root_id: RootId::new("vendor").unwrap(),
                    slot: "vendor/value-lib".into(),
                },
            ]
        }
        fn selection() -> Selection {
            Selection {
                workspace: ManifestRef {
                    root_id: RootId::new("engine").unwrap(),
                    path: "Cargo.toml".into(),
                },
                member: ManifestRef {
                    root_id: RootId::new("engine").unwrap(),
                    path: "native/Cargo.toml".into(),
                },
            }
        }
        fn plan(&self, mode: Mode, budget: &Budget<'_>) -> StagingPlan {
            let project = self.imported(budget);
            let grants = self.grants(&project, mode, budget);
            StagingPlan::resolve(
                &project,
                &grants,
                &Self::placements(),
                Self::selection(),
                &project.revision,
                budget,
            )
            .unwrap()
        }
        fn originals(&self) -> Vec<(String, Vec<u8>)> {
            fn visit(path: &Path, prefix: &str, rows: &mut Vec<(String, Vec<u8>)>) {
                let mut names: Vec<_> = std::fs::read_dir(path)
                    .unwrap()
                    .map(|e| e.unwrap().file_name())
                    .collect();
                names.sort();
                for name in names {
                    let full = path.join(&name);
                    let meta = std::fs::symlink_metadata(&full).unwrap();
                    assert!(!meta.file_type().is_symlink());
                    let relative = format!("{prefix}/{}", name.to_string_lossy());
                    if meta.is_dir() {
                        visit(&full, &relative, rows)
                    } else {
                        rows.push((relative, std::fs::read(&full).unwrap()));
                    }
                }
            }
            let mut rows = Vec::new();
            for (name, path) in [
                ("app", &self.app),
                ("engine", &self.engine),
                ("vendor", &self.vendor),
                ("ungranted", &self.base.join("ungranted")),
            ] {
                visit(path, name, &mut rows);
            }
            rows
        }
    }
    fn budget() -> Budget<'static> {
        Budget::new(Duration::from_secs(30), &|| Ok(())).unwrap()
    }
    fn seed_publication_fixture(fixture: &Fixture) {
        std::fs::write(
            fixture.engine.join("native/src/old.rs"),
            b"pub fn old() {}\n",
        )
        .unwrap();
    }
    fn publication_plan(fixture: &Fixture, budget: &Budget<'_>) -> StagingPlan {
        seed_publication_fixture(fixture);
        seeded_publication_plan(fixture, budget)
    }
    fn seeded_publication_plan(fixture: &Fixture, budget: &Budget<'_>) -> StagingPlan {
        let project = ImportedProject::bind(
            &fixture.app,
            Revision::new(&current_revision(&fixture.app, budget).unwrap()).unwrap(),
            budget,
        )
        .unwrap();
        let grants = fixture.publication_grants(&project, budget);
        StagingPlan::resolve(
            &project,
            &grants,
            &Fixture::placements(),
            Fixture::selection(),
            &project.revision,
            budget,
        )
        .unwrap()
    }
    fn publication_changes(fixture: &Fixture) -> Vec<crate::revisions::Change> {
        vec![
            crate::revisions::Change {
                path: "native/src/lib.rs".into(),
                action: crate::revisions::Action::Replace,
                expected_sha256: crate::sha256_file(&fixture.engine.join("native/src/lib.rs"))
                    .unwrap(),
                text: Some("pub fn proposed() -> i32 { 29 }\n".into()),
            },
            crate::revisions::Change {
                path: "native/src/old.rs".into(),
                action: crate::revisions::Action::Delete,
                expected_sha256: crate::sha256_file(&fixture.engine.join("native/src/old.rs"))
                    .unwrap(),
                text: None,
            },
            crate::revisions::Change {
                path: "native/src/new.rs".into(),
                action: crate::revisions::Action::Create,
                expected_sha256: String::new(),
                text: Some("pub fn new_value() -> i32 { 29 }\n".into()),
            },
        ]
    }
    fn publication_permit(
        fixture: &Fixture,
        plan: &StagingPlan,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<AtomicBool>,
        budget: &Budget<'_>,
    ) -> SourcePublicationPermit {
        let prepared = PreparedSourcePublication::prepare(
            plan,
            &RootId::new("engine").unwrap(),
            publication_changes(fixture),
            budget,
        )
        .unwrap();
        assert_eq!(prepared.reviewed_changes().len(), 3);
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        prepared
            .finish(
                confirmation,
                broker,
                cancelled,
                &plan.project.revision,
                budget,
            )
            .unwrap()
    }
    // Filesystem acceptance only: no compiler result, native execution or human
    // approval is claimed. Production validation has no callback or proof issuer.
    fn filesystem_candidate(
        fixture: &Fixture,
        plan: &StagingPlan,
        permit: &SourcePublicationPermit,
        budget: &Budget<'_>,
    ) -> (
        ValidatedSourceCandidate,
        crate::candidate_workspace::Workspace,
    ) {
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(budget.deadline).unwrap();
        let proposal = permit.stage(plan, &envelope, budget).unwrap();
        drop(envelope);
        let lease = std::sync::Arc::new(proposal.staged.compilation_lease(budget).unwrap());
        workspace.retain_compilation(lease.clone()).unwrap();
        // Aliasing and an unresolved original claim both preserve all guards.
        assert!(workspace
            .retire_compilation_for_publication(budget)
            .is_err());
        drop(lease);
        let mut claim = workspace.lifetime.claim().unwrap();
        assert!(workspace
            .retire_compilation_for_publication(budget)
            .is_err());
        claim.resolve();
        let ownership = workspace
            .retire_compilation_for_publication(budget)
            .unwrap();
        assert!(workspace.path.exists());
        assert!(workspace
            .retire_compilation_for_publication(budget)
            .is_err());
        let artifact = workspace.path.join("fixture.dll");
        std::fs::write(&artifact, b"private filesystem artifact fixture").unwrap();
        let artifact_hash = crate::sha256_file(&artifact).unwrap();
        let artifact_guard = open_compile_source(&artifact, budget).unwrap();
        (
            ValidatedSourceCandidate {
                proposal,
                ownership,
                workspace: workspace.path.clone(),
                artifact,
                artifact_hash,
                artifact_guard,
                native_validation_tag: None,
                compiler_provenance: CandidateCompilerProvenance::SyntheticAuthorityOnly,
            },
            workspace,
        )
    }
    // Authority-only fixture. Its synthetic completed tag is not a compiler
    // receipt or native qualification; only the test module can construct it.
    #[cfg(windows)]
    fn private_native_binding_fixture(
        fixture: &Fixture,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        b: &Budget<'_>,
    ) -> (
        PreparedSourcePublication,
        ValidatedSourceCandidate,
        crate::candidate_workspace::Workspace,
    ) {
        let plan = seeded_publication_plan(fixture, b);
        let mut prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            publication_changes(fixture),
            b,
        )
        .unwrap();
        let mut parent =
            crate::external_native::ParentDeadline::at(Instant::now() + Duration::from_secs(120))
                .unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline()).unwrap();
        let proposal = prepared
            .stage_for_native_validation(
                &plan,
                &envelope,
                &mut parent,
                broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                b,
            )
            .unwrap();
        drop(envelope);
        let lease = std::sync::Arc::new(proposal.staged.compilation_lease(b).unwrap());
        workspace.retain_compilation(lease.clone()).unwrap();
        drop(lease);
        let ownership = workspace.retire_compilation_for_publication(b).unwrap();
        let artifact = workspace.path.join("authority-fixture.dll");
        std::fs::write(&artifact, b"synthetic authority-only artifact\n").unwrap();
        let artifact_hash = crate::sha256_file(&artifact).unwrap();
        let artifact_guard = open_compile_source(&artifact, b).unwrap();
        let candidate = ValidatedSourceCandidate {
            proposal,
            ownership,
            workspace: workspace.path.clone(),
            artifact,
            artifact_hash,
            artifact_guard,
            compiler_provenance: CandidateCompilerProvenance::SyntheticAuthorityOnly,
            native_validation_tag: Some(format!(
                "synthetic-authority-only-{}",
                crate::random::session_id()
            )),
        };
        (prepared, candidate, workspace)
    }
    #[cfg(windows)]
    #[test]
    fn external_source_native_proposal_original_parent_and_limits_are_sealed() {
        let fixture = Fixture::new();
        seed_publication_fixture(&fixture);
        let mut b = budget();
        b.max_bytes = 1024 * 1024;
        b.max_file = 8192;
        b.max_files = 128;
        b.max_entries = 256;
        let original = fixture.originals();
        let plan = seeded_publication_plan(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let mut prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            publication_changes(&fixture),
            &b,
        )
        .unwrap();
        let mut parent =
            crate::external_native::ParentDeadline::at(Instant::now() + Duration::from_secs(120))
                .unwrap();
        let deadline = parent.instant();
        let origin = parent.origin().clone();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline()).unwrap();
        let proposal = prepared
            .stage_for_native_validation(
                &plan,
                &envelope,
                &mut parent,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            )
            .unwrap();
        drop(envelope);
        b.deadline = Instant::now() - Duration::from_millis(1);
        assert!(b.check().is_err());
        let check = || parent.check();
        let local = parent.source_budget(&check).unwrap();
        assert_eq!(parent.instant(), deadline);
        assert_eq!(parent.origin(), &origin);
        assert!(local.deadline() <= deadline);
        assert_eq!((local.max_bytes, local.max_file), (b.max_bytes, b.max_file));
        assert_eq!(
            (local.max_files, local.max_entries),
            (b.max_files, b.max_entries)
        );
        let replacement = crate::external_native::ParentDeadline::at(deadline).unwrap();
        let controller = crate::worker_windows::StreamController::for_runtime();
        let error = proposal
            .validate_before_publication(&mut workspace, &controller, &replacement)
            .err()
            .unwrap();
        assert_eq!(error, "external_source_original_native_parent_required");
        assert_eq!(controller.retained(), 0);
        workspace.clean().unwrap();
        assert_eq!(fixture.originals(), original);
        // Cancellation is triggered by observable private-stage creation, not
        // by a guessed number of internal file-loop checks.
        for close_broker in [false, true] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let b = budget();
            let original = fixture.originals();
            let plan = seeded_publication_plan(&fixture, &b);
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let mut prepared = PreparedSourcePublication::prepare(
                &plan,
                &RootId::new("engine").unwrap(),
                publication_changes(&fixture),
                &b,
            )
            .unwrap();
            let mut parent = crate::external_native::ParentDeadline::at(
                Instant::now() + Duration::from_secs(120),
            )
            .unwrap();
            let mut workspace =
                crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
            let envelope = workspace.source_envelope(b.deadline()).unwrap();
            let stage_path = envelope.path().to_path_buf();
            let triggered = std::cell::Cell::new(false);
            let stop_at_private_creation = || {
                if stage_path.exists() {
                    triggered.set(true);
                    if close_broker {
                        broker.close();
                    } else {
                        cancelled.store(true, Ordering::Release);
                    }
                }
                Ok(())
            };
            let observed = b.with_check(&stop_at_private_creation);
            let error = prepared
                .stage_for_native_validation(
                    &plan,
                    &envelope,
                    &mut parent,
                    &broker,
                    cancelled.clone(),
                    &observed,
                )
                .err()
                .unwrap();
            assert!(triggered.get());
            assert_eq!(error, "external_compile_read_owner_cancelled_or_closed");
            assert!(stage_path.exists());
            assert!(std::fs::read_dir(&stage_path).unwrap().next().is_none());
            drop(envelope);
            workspace.clean().unwrap();
            assert_eq!(fixture.originals(), original);
        }
        for phase in ["admit", "io"] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let mut b = budget();
            b.max_bytes = 1024 * 1024;
            b.max_file = 8192;
            b.max_files = 128;
            b.max_entries = 256;
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let (mut prepared, candidate, mut workspace) =
                private_native_binding_fixture(&fixture, &broker, &b);
            prepared
                .bind_validated_candidate(candidate, &broker, cancelled.clone(), &b)
                .unwrap();
            let confirmation =
                crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
            let revision = prepared.grant.project.revision.clone();
            let permit = prepared
                .finish(confirmation, &broker, cancelled, &revision, &b)
                .unwrap();
            let original = fixture.originals();
            if phase == "admit" {
                let mut wider = b.with_check(b.check_cancelled);
                wider.max_bytes += 1;
                assert_eq!(
                    crate::transactions::publish_external_prevalidated_source(permit, &wider)
                        .err()
                        .unwrap(),
                    "external_source_original_proposal_limits_widened"
                );
                assert!(!fixture.engine.join(".aurum/revisions").exists());
            } else {
                let admission = permit.admit_prevalidated(&b).unwrap();
                for dimension in 0..4 {
                    let mut wider = b.with_check(b.check_cancelled);
                    match dimension {
                        0 => wider.max_bytes += 1,
                        1 => wider.max_file += 1,
                        2 => wider.max_files += 1,
                        _ => wider.max_entries += 1,
                    }
                    assert_eq!(
                        admission.check_authority(&wider).err().unwrap(),
                        "external_source_original_proposal_limits_widened"
                    );
                    assert_eq!(
                        ExternalRevisionIo::publication(&admission, &wider)
                            .err()
                            .unwrap(),
                        "external_source_original_proposal_limits_widened"
                    );
                    assert_eq!(fixture.originals(), original);
                }
                drop(admission);
            }
            workspace.clean().unwrap();
            assert_eq!(fixture.originals(), original);
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_native_proposal_confirmation_is_postvalidation_and_same_candidate() {
        for mode in [
            "early",
            "old_digest",
            "foreign_tag",
            "success",
            "publish",
            "interrupted",
        ] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let b = budget();
            let original = fixture.originals();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let (mut prepared, candidate, mut workspace) =
                private_native_binding_fixture(&fixture, &broker, &b);
            let before_digest = prepared.digest().to_owned();
            let artifact_identity = file_identity(&candidate.artifact_guard).unwrap();
            if mode == "early" {
                let revision = prepared.grant.project.revision.clone();
                let confirmation = crate::workspace_access::simulated_private_confirmation_for_test(
                    &before_digest,
                );
                let error = prepared
                    .finish(confirmation, &broker, cancelled, &revision, &b)
                    .err()
                    .unwrap();
                assert_eq!(
                    error,
                    "external_source_postvalidation_confirmation_required"
                );
                drop(candidate);
            } else {
                prepared
                    .bind_validated_candidate(candidate, &broker, cancelled.clone(), &b)
                    .unwrap();
                let after_digest = prepared.digest().to_owned();
                assert_ne!(before_digest, after_digest);
                assert_eq!(prepared.binding.proposal_digest, before_digest);
                if mode == "foreign_tag" {
                    prepared
                        .native_candidate
                        .as_mut()
                        .unwrap()
                        .native_validation_tag = Some("foreign-completed-candidate".into());
                }
                let confirmation = crate::workspace_access::simulated_private_confirmation_for_test(
                    if mode == "old_digest" {
                        &before_digest
                    } else {
                        &after_digest
                    },
                );
                let project = prepared.grant.project.clone();
                let revision = project.revision.clone();
                let result = prepared.finish(confirmation, &broker, cancelled, &revision, &b);
                if mode == "old_digest" {
                    assert_eq!(
                        result.err().unwrap(),
                        "owner_source_confirmation_binding_mismatch"
                    );
                } else if mode == "foreign_tag" {
                    assert_eq!(
                        result.err().unwrap(),
                        "external_source_validated_candidate_binding_mismatch"
                    );
                } else if mode == "publish" || mode == "interrupted" {
                    // Authority-only filesystem coverage, not a real compiler or owner approval.
                    let original_source: Vec<_> = original
                        .iter()
                        .filter(|(path, _)| !path.starts_with("engine/.aurum/"))
                        .cloned()
                        .collect();
                    assert_eq!(v3_source_oracle(&fixture), original_source);
                    let permit = result.unwrap();
                    let revision = if mode == "publish" {
                        let publication =
                            crate::transactions::publish_external_prevalidated_source(permit, &b)
                                .unwrap();
                        assert_eq!(publication["ok"], true);
                        assert!(!fixture.engine.join("native/src/old.rs").exists());
                        assert_eq!(
                            std::fs::read(fixture.engine.join("native/src/new.rs")).unwrap(),
                            b"pub fn new_value() -> i32 { 29 }\n"
                        );
                        publication["revision_id"].as_str().unwrap().to_owned()
                    } else {
                        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
                        let flag = reached.clone();
                        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::transactions::with_external_v3_test_hook(
                                move |phase, index| {
                                    if phase == "after_publish" && index == 0 {
                                        flag.set(true);
                                        panic!("prevalidated bridge interruption");
                                    }
                                },
                                || {
                                    crate::transactions::publish_external_prevalidated_source(
                                        permit, &b,
                                    )
                                },
                            )
                        }))
                        .is_err());
                        assert!(reached.get());
                        assert!(fixture.engine.join("native/src/old.rs").exists());
                        assert!(!fixture.engine.join("native/src/new.rs").exists());
                        v3_active(&fixture)
                    };
                    assert_eq!(
                        std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
                        b"pub fn proposed() -> i32 { 29 }\n"
                    );
                    let repair = v3_repair(
                        &fixture,
                        &project,
                        &revision,
                        mode == "publish",
                        &broker,
                        &b,
                    );
                    assert_eq!(
                        crate::transactions::repair_external_source(repair, &b).unwrap()["ok"],
                        true
                    );
                    assert_eq!(v3_source_oracle(&fixture), original_source);
                    assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
                } else {
                    let admission = result.unwrap().admit_prevalidated(&b).unwrap();
                    assert_eq!(admission.binding.proposal_digest, before_digest);
                    assert_eq!(admission.binding.authorization_digest, after_digest);
                    assert_eq!(
                        file_identity(&admission._candidate.artifact_guard).unwrap(),
                        artifact_identity
                    );
                    drop(admission);
                }
            }
            workspace.clean().unwrap();
            if mode != "publish" && mode != "interrupted" {
                assert_eq!(fixture.originals(), original);
            }
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_native_proposal_fresh_window_refuses_renewal_owner_and_drift() {
        for mode in [
            "renew",
            "broker",
            "cancel_owner",
            "cancelled",
            "source_drift",
        ] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let b = budget();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let (mut prepared, candidate, mut workspace) =
                private_native_binding_fixture(&fixture, &broker, &b);
            prepared
                .bind_validated_candidate(candidate, &broker, cancelled.clone(), &b)
                .unwrap();
            if mode == "cancelled" {
                cancelled.store(true, Ordering::Release);
            }
            if mode == "source_drift" {
                std::fs::write(
                    fixture.engine.join("native/src/lib.rs"),
                    b"newer owner bytes\n",
                )
                .unwrap();
            }
            let evidence = fixture.originals();
            let confirmation =
                crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
            let revision = prepared.grant.project.revision.clone();
            let other = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let other_cancel = std::sync::Arc::new(AtomicBool::new(false));
            let mut renewed = budget();
            renewed.deadline = b.deadline + Duration::from_secs(1);
            let error = prepared
                .finish(
                    confirmation,
                    if mode == "broker" { &other } else { &broker },
                    if mode == "cancel_owner" {
                        other_cancel
                    } else {
                        cancelled
                    },
                    &revision,
                    if mode == "renew" { &renewed } else { &b },
                )
                .err()
                .unwrap();
            let expected = match mode {
                "renew" => "external_source_confirmation_window_renewal_or_expiry",
                "broker" | "cancel_owner" => "external_source_confirmation_owner_binding_mismatch",
                "cancelled" => "external_source_confirmation_owner_cancelled_or_closed",
                _ => "external_source_original_fingerprint_changed",
            };
            assert_eq!(error, expected);
            workspace.clean().unwrap();
            assert_eq!(fixture.originals(), evidence);
        }
        // Once the original callback signals cancellation, no later callback
        // or source revalidation step may run under the same operation.
        for phase in ["bind", "finish"] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let b = budget();
            let original = fixture.originals();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let (mut prepared, candidate, mut workspace) =
                private_native_binding_fixture(&fixture, &broker, &b);
            let mut candidate = Some(candidate);
            if phase == "finish" {
                prepared
                    .bind_validated_candidate(
                        candidate.take().unwrap(),
                        &broker,
                        cancelled.clone(),
                        &b,
                    )
                    .unwrap();
            }
            let calls = std::cell::Cell::new(0usize);
            let after_cancel = std::cell::Cell::new(0usize);
            let triggered = std::cell::Cell::new(false);
            let stop_inside_revalidation = || {
                if cancelled.load(Ordering::Acquire) {
                    after_cancel.set(after_cancel.get() + 1);
                } else {
                    calls.set(calls.get() + 1);
                    if calls.get() == 3 {
                        triggered.set(true);
                        cancelled.store(true, Ordering::Release);
                    }
                }
                Ok(())
            };
            let observed = b.with_check(&stop_inside_revalidation);
            let error = if phase == "bind" {
                prepared
                    .bind_validated_candidate(
                        candidate.take().unwrap(),
                        &broker,
                        cancelled.clone(),
                        &observed,
                    )
                    .err()
                    .unwrap()
            } else {
                let confirmation = crate::workspace_access::simulated_private_confirmation_for_test(
                    prepared.digest(),
                );
                let revision = prepared.grant.project.revision.clone();
                prepared
                    .finish(
                        confirmation,
                        &broker,
                        cancelled.clone(),
                        &revision,
                        &observed,
                    )
                    .err()
                    .unwrap()
            };
            assert!(triggered.get());
            assert_eq!(after_cancel.get(), 0);
            assert_eq!(
                error,
                "external_source_confirmation_owner_cancelled_or_closed"
            );
            workspace.clean().unwrap();
            assert_eq!(fixture.originals(), original);
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_native_proposal_publication_context_cannot_be_reopened() {
        for cancel_original in [false, true] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let b = budget();
            let original = fixture.originals();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let (mut prepared, candidate, mut workspace) =
                private_native_binding_fixture(&fixture, &broker, &b);
            let operation = crate::operation_context::OperationContext::new(
                Instant::now() + Duration::from_secs(60),
            );
            operation
                .scope(|| {
                    prepared.bind_validated_candidate(candidate, &broker, cancelled.clone(), &b)
                })
                .unwrap()
                .unwrap();
            if cancel_original {
                operation.request_cancel().unwrap();
            }
            let confirmation =
                crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
            let revision = prepared.grant.project.revision.clone();
            let fresh = crate::operation_context::OperationContext::new(
                Instant::now() + Duration::from_secs(60),
            );
            let error = fresh
                .scope(|| prepared.finish(confirmation, &broker, cancelled, &revision, &b))
                .unwrap()
                .err()
                .unwrap();
            assert_eq!(
                error,
                if cancel_original {
                    "external_source_publication_operation_cancelled_or_expired"
                } else {
                    "external_source_publication_operation_binding_mismatch"
                }
            );
            workspace.clean().unwrap();
            assert_eq!(fixture.originals(), original);
        }
    }
    #[cfg(windows)]
    fn namespace_admission(
        fixture: &Fixture,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<AtomicBool>,
        budget: &Budget<'_>,
    ) -> (SourceAdmission, crate::candidate_workspace::Workspace) {
        let plan = publication_plan(fixture, budget);
        let permit = publication_permit(fixture, &plan, broker, cancelled, budget);
        let (candidate, workspace) = filesystem_candidate(fixture, &plan, &permit, budget);
        (permit.admit(candidate, budget).unwrap(), workspace)
    }
    #[cfg(windows)]
    fn v3_active(fixture: &Fixture) -> String {
        let bytes = std::fs::read(fixture.engine.join(".aurum/revisions/active.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["version"], 3);
        value["revision_id"].as_str().unwrap().to_owned()
    }
    #[cfg(windows)]
    fn v3_repair(
        fixture: &Fixture,
        project: &ImportedProject,
        revision: &str,
        undo: bool,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        b: &Budget<'_>,
    ) -> SourceRepairPermit {
        let prepared = PreparedSourceRepair::prepare(
            project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            revision,
            undo,
            b,
        )
        .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        prepared
            .finish(
                confirmation,
                broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                b,
            )
            .unwrap()
    }
    #[cfg(windows)]
    fn v3_source_oracle(fixture: &Fixture) -> Vec<(String, Vec<u8>)> {
        fixture
            .originals()
            .into_iter()
            .filter(|(p, _)| !p.starts_with("engine/.aurum/"))
            .collect()
    }
    #[cfg(windows)]
    fn v3_nested_candidate(
        fixture: &Fixture,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        b: &Budget<'_>,
    ) -> (
        StagingPlan,
        SourcePublicationPermit,
        ValidatedSourceCandidate,
        crate::candidate_workspace::Workspace,
    ) {
        let plan = publication_plan(fixture, b);
        let mut changes = publication_changes(fixture);
        changes[2].path = "native/src/new/deep/created.rs".into();
        let prepared =
            PreparedSourcePublication::prepare(&plan, &RootId::new("engine").unwrap(), changes, b)
                .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let permit = prepared
            .finish(
                confirmation,
                broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &plan.project.revision,
                b,
            )
            .unwrap();
        let (candidate, workspace) = filesystem_candidate(fixture, &plan, &permit, b);
        (plan, permit, candidate, workspace)
    }
    #[cfg(windows)]
    fn v3_directory_oracle(fixture: &Fixture, b: &Budget<'_>) -> Vec<(String, DirectoryIdentity)> {
        ["native/src/new", "native/src/new/deep"]
            .into_iter()
            .map(|path| {
                (
                    path.into(),
                    canonical_directory(&fixture.engine.join(path), b)
                        .unwrap()
                        .1,
                )
            })
            .collect()
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_injected_legacy_journal_cannot_reclassify_private_history() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::write(
            fixture.engine.join("aurum.toml"),
            b"schema_version=1\nname=\"private-v3-fixture\"\nmodules=[]\n",
        )
        .unwrap();
        let plan = publication_plan(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let rows: Vec<_> = permit
            .prepared
            .binding
            .targets
            .iter()
            .map(|row| serde_json::json!({"path":row.path,"before":row.before,"after":row.after}))
            .collect();
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let value = crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
        assert_eq!(value["ok"], true);
        let revision = value["revision_id"].as_str().unwrap();
        let state = fixture.engine.join(".aurum/revisions").join(revision);
        std::fs::write(state.join("journal.json"),serde_json::to_vec(&serde_json::json!({"version":1,"id":revision,"state":"committed","rows":rows,"created_dirs":[],"undo_of":null})).unwrap()).unwrap();
        let managed = crate::Project::open(&fixture.engine).unwrap();
        let evidence = fixture.originals();
        assert!(crate::transactions::history(&fixture.engine, &serde_json::json!({})).is_err());
        assert!(crate::transactions::undo(&managed, revision).is_err());
        assert!(crate::transactions::forget(&managed, revision).is_err());
        assert_eq!(fixture.originals(), evidence);
        std::fs::write(fixture.engine.join(".aurum/revisions/active.json"),serde_json::to_vec(&serde_json::json!({"version":3,"revision_id":revision,"header_sha256":crate::sha256_file(&state.join("v3.json")).unwrap()})).unwrap()).unwrap();
        let evidence = fixture.originals();
        assert!(crate::transactions::recover_root(&fixture.engine).is_err());
        assert_eq!(fixture.originals(), evidence);
        assert_eq!(v3_active(&fixture), revision);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_stale_undo_source_edit_is_preserved_with_exact_evidence() {
        let fixture = Fixture::new();
        let b = budget();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (plan, permit, candidate, mut workspace) = v3_nested_candidate(&fixture, &broker, &b);
        let value = crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
        assert_eq!(value["ok"], true);
        let ids = v3_directory_oracle(&fixture, &b);
        let revision = value["revision_id"].as_str().unwrap();
        let undo = v3_repair(&fixture, &plan.project, revision, true, &broker, &b);
        std::fs::write(
            fixture.engine.join("native/src/lib.rs"),
            b"newer owner source edit\n",
        )
        .unwrap();
        let evidence = fixture.originals();
        assert!(crate::transactions::repair_external_source(undo, &b).is_err());
        assert_eq!(fixture.originals(), evidence);
        assert_eq!(v3_directory_oracle(&fixture, &b), ids);
        assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
        assert_eq!(
            std::fs::read_dir(fixture.engine.join(".aurum/revisions"))
                .unwrap()
                .count(),
            1
        );
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_concurrently_created_planned_parent_is_never_adopted() {
        let fixture = Fixture::new();
        let b = budget();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (_plan, permit, candidate, mut workspace) = v3_nested_candidate(&fixture, &broker, &b);
        let original = v3_source_oracle(&fixture);
        let root = fixture.engine.clone();
        let deadline = b.deadline;
        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = reached.clone();
        let identity = std::rc::Rc::new(std::cell::RefCell::new(None));
        let recorded = identity.clone();
        let result = crate::transactions::with_external_v3_test_hook(
            move |phase, _| {
                if phase == "backups_durable" {
                    flag.set(true);
                    let directory = root.join("native/src/new");
                    std::fs::create_dir(&directory).unwrap();
                    std::fs::write(
                        directory.join("foreign.keep"),
                        b"concurrent directory owner\n",
                    )
                    .unwrap();
                    let guard = crate::snapshot::open_checked_writable_directory_identity(
                        &directory, deadline,
                    )
                    .unwrap();
                    *recorded.borrow_mut() = Some(file_identity(&guard).unwrap());
                }
            },
            || crate::transactions::publish_external_source(permit, candidate, &b),
        );
        assert!(reached.get());
        assert!(result.is_err());
        assert_eq!(
            canonical_directory(&fixture.engine.join("native/src/new"), &b)
                .unwrap()
                .1,
            *identity.borrow().as_ref().unwrap()
        );
        assert_eq!(
            std::fs::read(fixture.engine.join("native/src/new/foreign.keep")).unwrap(),
            b"concurrent directory owner\n"
        );
        assert!(!fixture.engine.join("native/src/new/deep").exists());
        assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
        let mut expected = original;
        expected.push((
            "engine/native/src/new/foreign.keep".into(),
            b"concurrent directory owner\n".to_vec(),
        ));
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        let mut actual = v3_source_oracle(&fixture);
        actual.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(actual, expected);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_reduced_original_file_bound_refuses_before_record_creation() {
        let fixture = Fixture::new();
        let mut b = budget();
        b.max_file = 1024;
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (admission, mut workspace) = namespace_admission(
            &fixture,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let original = v3_source_oracle(&fixture);
        {
            let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
            let proof = namespace.preflight(0).unwrap();
            namespace
                .begin_state(&crate::random::session_id(), proof)
                .unwrap();
            assert_eq!(
                namespace
                    .create_record("000.after", b"bounded record\n", 2048)
                    .err()
                    .unwrap(),
                "external_state_create_bound_or_volume_refused"
            );
            assert!(namespace.state_entries().unwrap().is_empty());
            assert!(namespace.state_read("000.after", 1024).unwrap().is_none());
        }
        assert_eq!(v3_source_oracle(&fixture), original);
        assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
        drop(admission);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_interrupted_reverse_create_removes_only_its_own_missing_parent_directories(
    ) {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::create_dir_all(fixture.engine.join("native/src/gone/deep")).unwrap();
        let target = "native/src/gone/deep/old.rs";
        std::fs::write(fixture.engine.join(target), b"pub fn old_source() {}\n").unwrap();
        let plan = publication_plan(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let changes = vec![
            crate::revisions::Change {
                path: target.into(),
                action: crate::revisions::Action::Delete,
                expected_sha256: crate::sha256_file(&fixture.engine.join(target)).unwrap(),
                text: None,
            },
            publication_changes(&fixture).remove(0),
        ];
        let prepared =
            PreparedSourcePublication::prepare(&plan, &RootId::new("engine").unwrap(), changes, &b)
                .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let permit = prepared
            .finish(
                confirmation,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &plan.project.revision,
                &b,
            )
            .unwrap();
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let published =
            crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
        assert_eq!(published["ok"], true);
        std::fs::remove_dir(fixture.engine.join("native/src/gone/deep")).unwrap();
        std::fs::remove_dir(fixture.engine.join("native/src/gone")).unwrap();
        let keep = fixture.engine.join("native/src/preexisting-empty");
        std::fs::create_dir(&keep).unwrap();
        let identity = canonical_directory(&keep, &b).unwrap().1;
        let preimage = current_revision(&fixture.engine, &b).unwrap();
        let permit = v3_repair(
            &fixture,
            &plan.project,
            published["revision_id"].as_str().unwrap(),
            true,
            &broker,
            &b,
        );
        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = reached.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::transactions::with_external_v3_test_hook(
                move |phase, index| {
                    if phase == "before_old_move" && index == 1 {
                        flag.set(true);
                        panic!("reverse reached second row after nested create")
                    }
                },
                || crate::transactions::repair_external_source(permit, &b),
            )
        }));
        assert!(reached.get(), "reverse failed before second row");
        assert!(result.is_err());
        assert!(fixture.engine.join(target).exists());
        let revision = v3_active(&fixture);
        let repair = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
        assert_eq!(
            crate::transactions::repair_external_source(repair, &b).unwrap()["ok"],
            true
        );
        assert_eq!(current_revision(&fixture.engine, &b).unwrap(), preimage);
        assert!(!fixture.engine.join("native/src/gone").exists());
        assert_eq!(canonical_directory(&keep, &b).unwrap().1, identity);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_missing_header_cannot_reclassify_retained_history_as_v1() {
        for reverse in [false, true] {
            let fixture = Fixture::new();
            let b = budget();
            std::fs::write(
                fixture.engine.join("aurum.toml"),
                b"schema_version=1\nname=\"private-v3-fixture\"\nmodules=[]\n",
            )
            .unwrap();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let (plan, permit, candidate, mut workspace) =
                v3_nested_candidate(&fixture, &broker, &b);
            let publication =
                crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
            let mut revision = publication["revision_id"].as_str().unwrap().to_owned();
            if reverse {
                let permit = v3_repair(&fixture, &plan.project, &revision, true, &broker, &b);
                let undo = crate::transactions::repair_external_source(permit, &b).unwrap();
                assert_eq!(undo["ok"], true);
                revision = undo["revision_id"].as_str().unwrap().to_owned();
            }
            let state = fixture.engine.join(".aurum/revisions").join(&revision);
            let header: serde_json::Value =
                serde_json::from_slice(&std::fs::read(state.join("v3.json")).unwrap()).unwrap();
            let saved_ids: Vec<_> = ["d0000.dir", "d0001.dir"]
                .into_iter()
                .filter(|name| state.join(name).exists())
                .map(|name| (name, canonical_directory(&state.join(name), &b).unwrap().1))
                .collect();
            assert_eq!(saved_ids.len(), if reverse { 2 } else { 0 });
            std::fs::remove_file(state.join("v3.json")).unwrap();
            std::fs::write(
                state.join("journal.json"),
                serde_json::to_vec(&serde_json::json!({
                    "version":1,"id":revision,"state":"committed",
                    "rows":header["journal"]["rows"],"created_dirs":[],"undo_of":null
                }))
                .unwrap(),
            )
            .unwrap();
            let managed = crate::Project::open(&fixture.engine).unwrap();
            let evidence = fixture.originals();
            let error = crate::transactions::forget(&managed, &revision).unwrap_err();
            assert!(error.contains("unclassified evidence"), "{error}");
            assert_eq!(fixture.originals(), evidence);
            let error = crate::transactions::undo(&managed, &revision).unwrap_err();
            assert!(error.contains("unclassified evidence"), "{error}");
            assert_eq!(fixture.originals(), evidence);
            if !reverse {
                let error = crate::transactions::history(&fixture.engine, &serde_json::json!({}))
                    .unwrap_err();
                assert_eq!(
                    error,
                    format!("revision {revision} has an invalid or unavailable journal")
                );
                assert_eq!(fixture.originals(), evidence);
            }
            std::fs::write(
                fixture.engine.join(".aurum/revisions/active.json"),
                serde_json::to_vec(&serde_json::json!({"version":1,"revision_id":revision}))
                    .unwrap(),
            )
            .unwrap();
            let evidence = fixture.originals();
            let error = crate::transactions::recover_root(&fixture.engine).unwrap_err();
            assert!(error.contains("unclassified evidence"), "{error}");
            assert_eq!(fixture.originals(), evidence);
            for (name, identity) in saved_ids {
                assert_eq!(
                    canonical_directory(&state.join(name), &b).unwrap().1,
                    identity
                );
            }
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_returned_directory_owned_error_retains_active_conflict() {
        let fixture = Fixture::new();
        let fail_next = std::sync::Arc::new(AtomicBool::new(false));
        let failures = std::sync::atomic::AtomicUsize::new(0);
        let check = || {
            if fail_next.swap(false, Ordering::SeqCst) {
                failures.fetch_add(1, Ordering::SeqCst);
                Err("injected_directory_owned_persistence_check".to_owned())
            } else {
                Ok(())
            }
        };
        let b = Budget::new(std::time::Duration::from_secs(60), &check).unwrap();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (plan, permit, candidate, mut workspace) = v3_nested_candidate(&fixture, &broker, &b);
        let source_before = v3_source_oracle(&fixture);
        let reached = std::sync::Arc::new(AtomicBool::new(false));
        let hook_reached = reached.clone();
        let hook_failure = fail_next.clone();
        let publication = crate::transactions::with_external_v3_test_hook(
            move |phase, index| {
                if phase == "after_directory" && index == 0 {
                    hook_reached.store(true, Ordering::SeqCst);
                    hook_failure.store(true, Ordering::SeqCst);
                }
            },
            || crate::transactions::publish_external_source(permit, candidate, &b),
        )
        .unwrap();
        assert!(reached.load(Ordering::SeqCst));
        assert_eq!(failures.load(Ordering::SeqCst), 1);
        assert!(check().is_ok());
        assert_eq!(publication["ok"], false);
        assert_eq!(publication["source_rolled_back"], false);
        assert_eq!(publication["recovery_required"], true);
        assert!(publication["recovery"]["error"]
            .as_str()
            .unwrap()
            .contains("unrecorded planned directory during restore"));
        assert_eq!(v3_source_oracle(&fixture), source_before);
        let revision = v3_active(&fixture);
        let directory = fixture.engine.join("native/src/new");
        let identity = canonical_directory(&directory, &b).unwrap().1;
        assert!(!fixture.engine.join("native/src/new/deep").exists());
        let evidence = fixture.originals();
        assert!(PreparedSourceRepair::prepare(
            &plan.project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            &revision,
            false,
            &b
        )
        .is_err());
        assert_eq!(fixture.originals(), evidence);
        assert_eq!(canonical_directory(&directory, &b).unwrap().1, identity);
        assert_eq!(v3_active(&fixture), revision);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_whole_undo_and_redo_restore_complete_fingerprints_and_directory_identities(
    ) {
        let fixture = Fixture::new();
        let pub_budget = budget();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (plan, permit, candidate, mut workspace) =
            v3_nested_candidate(&fixture, &broker, &pub_budget);
        let original = current_revision(&fixture.engine, &pub_budget).unwrap();
        let original_bytes = v3_source_oracle(&fixture);
        let publication =
            crate::transactions::publish_external_source(permit, candidate, &pub_budget).unwrap();
        assert_eq!(publication["ok"], true);
        let forward = current_revision(&fixture.engine, &pub_budget).unwrap();
        let directory_ids = v3_directory_oracle(&fixture, &pub_budget);
        pub_budget.check().unwrap();

        let undo_budget = budget();
        let undo = v3_repair(
            &fixture,
            &plan.project,
            publication["revision_id"].as_str().unwrap(),
            true,
            &broker,
            &undo_budget,
        );
        let reversed = crate::transactions::repair_external_source(undo, &undo_budget).unwrap();
        assert_eq!(reversed["ok"], true);
        assert_eq!(
            current_revision(&fixture.engine, &undo_budget).unwrap(),
            original
        );
        assert_eq!(v3_source_oracle(&fixture), original_bytes);
        assert!(!fixture.engine.join("native/src/new").exists());
        undo_budget.check().unwrap();

        let stale_budget = budget();
        stale_budget.check().unwrap();
        let stale = PreparedSourceRepair::prepare(
            &plan.project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            publication["revision_id"].as_str().unwrap(),
            true,
            &stale_budget,
        );
        stale_budget.check().unwrap();
        let stale_error = stale.err().expect("stale forward history must refuse");
        assert!(
            stale_error.starts_with("external_source_v3_conflict:"),
            "unexpected stale-history refusal: {stale_error}"
        );

        let redo_budget = budget();
        let redo = v3_repair(
            &fixture,
            &plan.project,
            reversed["revision_id"].as_str().unwrap(),
            true,
            &broker,
            &redo_budget,
        );
        assert_eq!(
            crate::transactions::repair_external_source(redo, &redo_budget).unwrap()["ok"],
            true
        );
        assert_eq!(
            current_revision(&fixture.engine, &redo_budget).unwrap(),
            forward
        );
        assert_eq!(v3_directory_oracle(&fixture, &redo_budget), directory_ids);
        redo_budget.check().unwrap();
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_directory_retirement_and_return_interruptions_are_reversible() {
        for (phase, index, redo) in [
            ("before_directory_retire", 1, false),
            ("after_directory_retire", 1, false),
            ("before_directory_retire", 0, false),
            ("after_directory_retire", 0, false),
            ("before_directory_return", 0, true),
            ("after_directory_return", 0, true),
            ("after_directory_return", 1, true),
        ] {
            let fixture = Fixture::new();
            let b = budget();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let (plan, permit, candidate, mut workspace) =
                v3_nested_candidate(&fixture, &broker, &b);
            let pristine = current_revision(&fixture.engine, &b).unwrap();
            let publication =
                crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
            assert_eq!(publication["ok"], true);
            let forward = current_revision(&fixture.engine, &b).unwrap();
            let ids = v3_directory_oracle(&fixture, &b);
            let mut revision = publication["revision_id"].as_str().unwrap().to_owned();
            if redo {
                let value = crate::transactions::repair_external_source(
                    v3_repair(&fixture, &plan.project, &revision, true, &broker, &b),
                    &b,
                )
                .unwrap();
                assert_eq!(value["ok"], true);
                revision = value["revision_id"].as_str().unwrap().into();
                assert_eq!(current_revision(&fixture.engine, &b).unwrap(), pristine);
            }
            let permit = v3_repair(&fixture, &plan.project, &revision, true, &broker, &b);
            let reached = std::rc::Rc::new(std::cell::Cell::new(false));
            let flag = reached.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::transactions::with_external_v3_test_hook(
                    move |at, row| {
                        if at == phase && row == index {
                            flag.set(true);
                            panic!("directory relocation interruption")
                        }
                    },
                    || crate::transactions::repair_external_source(permit, &b),
                )
            }));
            assert!(
                reached.get(),
                "directory boundary not reached: {phase}/{index}"
            );
            assert!(result.is_err());
            let active = v3_active(&fixture);
            let permit = v3_repair(&fixture, &plan.project, &active, false, &broker, &b);
            // Recovery may return deep into an ancestor already held at capture.
            // Only exact relocation parents gain write sharing, never delete.
            {
                use std::os::windows::fs::OpenOptionsExt;
                let guards = &permit.prepared.guards;
                assert!(!guards.1.contains(&fixture.engine.join("native")));
                let delete_control = OpenOptions::new()
                    .access_mode(0x0011_0000)
                    .share_mode(7)
                    .custom_flags(0x0220_0000)
                    .open(fixture.base.join("ungranted"))
                    .unwrap();
                drop(delete_control);
                for relative in ["native/src", "native/src/new"] {
                    let parent = fixture.engine.join(relative);
                    assert!(guards.1.contains(&parent), "{phase}/{index}: {relative}");
                    let captured = guards
                        .0
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|(path, _, _)| path == &parent);
                    assert_eq!(captured, parent.exists());
                    if captured {
                        let writer = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .share_mode(7)
                            .custom_flags(0x0220_0000)
                            .open(&parent)
                            .unwrap();
                        drop(writer);
                        assert!(OpenOptions::new()
                            .access_mode(0x0011_0000)
                            .share_mode(7)
                            .custom_flags(0x0220_0000)
                            .open(&parent)
                            .is_err());
                    }
                }
                assert!(OpenOptions::new()
                    .read(true)
                    .write(true)
                    .share_mode(7)
                    .custom_flags(0x0220_0000)
                    .open(fixture.engine.join("native"))
                    .is_err());
                guards.check(&b).unwrap();
            }
            let recovery = crate::transactions::repair_external_source(permit, &b);
            assert!(
                recovery.is_ok(),
                "recovery {phase}/{index}/{redo}: {recovery:?}"
            );
            assert_eq!(recovery.unwrap()["ok"], true);
            if redo {
                assert_eq!(current_revision(&fixture.engine, &b).unwrap(), pristine);
                assert!(!fixture.engine.join("native/src/new").exists())
            } else {
                assert_eq!(current_revision(&fixture.engine, &b).unwrap(), forward);
                assert_eq!(v3_directory_oracle(&fixture, &b), ids)
            }
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_foreign_directory_contents_and_unowned_slots_preserve_all_evidence() {
        for fault in ["foreign", "slot", "unowned_create"] {
            let fixture = Fixture::new();
            let b = budget();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let (plan, permit, candidate, mut workspace) =
                v3_nested_candidate(&fixture, &broker, &b);
            if fault == "unowned_create" {
                let reached = std::rc::Rc::new(std::cell::Cell::new(false));
                let flag = reached.clone();
                assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::transactions::with_external_v3_test_hook(
                        move |phase, index| {
                            if phase == "after_directory" && index == 0 {
                                flag.set(true);
                                panic!("unrecorded created directory")
                            }
                        },
                        || crate::transactions::publish_external_source(permit, candidate, &b),
                    )
                }))
                .is_err());
                assert!(reached.get());
                let revision = v3_active(&fixture);
                let evidence = fixture.originals();
                let identity = canonical_directory(&fixture.engine.join("native/src/new"), &b)
                    .unwrap()
                    .1;
                assert!(PreparedSourceRepair::prepare(
                    &plan.project,
                    RootId::new("engine").unwrap(),
                    &fixture.engine,
                    &revision,
                    false,
                    &b
                )
                .is_err());
                assert_eq!(fixture.originals(), evidence);
                assert_eq!(
                    canonical_directory(&fixture.engine.join("native/src/new"), &b)
                        .unwrap()
                        .1,
                    identity
                );
            } else {
                let publication =
                    crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
                assert_eq!(publication["ok"], true);
                let ids = v3_directory_oracle(&fixture, &b);
                let revision = publication["revision_id"].as_str().unwrap();
                if fault == "foreign" {
                    std::fs::write(
                        fixture.engine.join("native/src/new/deep/foreign.keep"),
                        b"foreign directory content\n",
                    )
                    .unwrap();
                    let evidence = fixture.originals();
                    assert!(PreparedSourceRepair::prepare(
                        &plan.project,
                        RootId::new("engine").unwrap(),
                        &fixture.engine,
                        revision,
                        true,
                        &b
                    )
                    .is_err());
                    assert_eq!(fixture.originals(), evidence);
                    assert_eq!(v3_directory_oracle(&fixture, &b), ids);
                } else {
                    let undo = v3_repair(&fixture, &plan.project, revision, true, &broker, &b);
                    let root = fixture.engine.clone();
                    let reached = std::rc::Rc::new(std::cell::Cell::new(false));
                    let flag = reached.clone();
                    let result = crate::transactions::with_external_v3_test_hook(
                        move |phase, index| {
                            if phase == "before_directory_retire" && index == 1 {
                                flag.set(true);
                                let value: serde_json::Value = serde_json::from_slice(
                                    &std::fs::read(root.join(".aurum/revisions/active.json"))
                                        .unwrap(),
                                )
                                .unwrap();
                                let slot = root
                                    .join(".aurum/revisions")
                                    .join(value["revision_id"].as_str().unwrap())
                                    .join("d0001.dir");
                                std::fs::create_dir(&slot).unwrap();
                                std::fs::write(
                                    slot.join("foreign.keep"),
                                    b"unowned slot contents\n",
                                )
                                .unwrap();
                            }
                        },
                        || crate::transactions::repair_external_source(undo, &b),
                    )
                    .unwrap();
                    assert!(reached.get());
                    assert_eq!(result["recovery_required"], true);
                    let active = v3_active(&fixture);
                    let evidence = fixture.originals();
                    assert!(PreparedSourceRepair::prepare(
                        &plan.project,
                        RootId::new("engine").unwrap(),
                        &fixture.engine,
                        &active,
                        false,
                        &b
                    )
                    .is_err());
                    assert_eq!(fixture.originals(), evidence);
                    assert_eq!(v3_directory_oracle(&fixture, &b), ids);
                }
            }
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_intent_and_observation_interruptions_recover_exact_objects() {
        for phase in [
            "before_old_move",
            "after_old_move",
            "before_publish",
            "after_publish",
        ] {
            let fixture = Fixture::new();
            let b = budget();
            let plan = publication_plan(&fixture, &b);
            let original = v3_source_oracle(&fixture);
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let permit = publication_permit(
                &fixture,
                &plan,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            );
            let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
            let reached = std::rc::Rc::new(std::cell::Cell::new(false));
            let observed = reached.clone();
            let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::transactions::with_external_v3_test_hook(
                    move |at, index| {
                        if at == phase && index == 0 {
                            observed.set(true);
                            panic!("V3 deterministic interruption")
                        }
                    },
                    || crate::transactions::publish_external_source(permit, candidate, &b),
                )
            }));
            assert!(reached.get(), "phase not reached: {phase}");
            assert!(interrupted.is_err());
            let revision = v3_active(&fixture);
            assert!(current_revision(&fixture.engine, &b).is_err());
            assert!(crate::transactions::recover_root(&fixture.engine).is_err());
            let permit = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
            assert_eq!(
                crate::transactions::repair_external_source(permit, &b).unwrap()["ok"],
                true
            );
            assert_eq!(v3_source_oracle(&fixture), original);
            assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_nested_directory_ownership_and_interrupted_restore_preserve_sentinels() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let keep = fixture.engine.join("native/src/preexisting-empty");
        std::fs::create_dir(&keep).unwrap();
        let keep_id = canonical_directory(&keep, &b).unwrap().1;
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let mut changes = publication_changes(&fixture);
        changes[2].path = "native/src/new/deep/created.rs".into();
        let prepared =
            PreparedSourcePublication::prepare(&plan, &RootId::new("engine").unwrap(), changes, &b)
                .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let permit = prepared
            .finish(
                confirmation,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &plan.project.revision,
                &b,
            )
            .unwrap();
        let original = v3_source_oracle(&fixture);
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = reached.clone();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::transactions::with_external_v3_test_hook(
                move |phase, index| {
                    if phase == "after_publish" && index == 2 {
                        flag.set(true);
                        panic!("nested V3 source interruption")
                    }
                },
                || crate::transactions::publish_external_source(permit, candidate, &b),
            )
        }))
        .is_err());
        assert!(reached.get());
        let revision = v3_active(&fixture);
        assert!(fixture
            .engine
            .join("native/src/new/deep/created.rs")
            .exists());
        let repair = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = reached.clone();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::transactions::with_external_v3_test_hook(
                move |phase, index| {
                    if phase == "after_restore" && index == 2 {
                        flag.set(true);
                        panic!("nested V3 restore interruption")
                    }
                },
                || crate::transactions::repair_external_source(repair, &b),
            )
        }))
        .is_err());
        assert!(reached.get());
        let repair = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
        assert_eq!(
            crate::transactions::repair_external_source(repair, &b).unwrap()["ok"],
            true
        );
        assert_eq!(v3_source_oracle(&fixture), original);
        assert!(!fixture.engine.join("native/src/new").exists());
        assert_eq!(canonical_directory(&keep, &b).unwrap().1, keep_id);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_unknown_slot_corrupt_backup_and_same_hash_new_identity_are_conflicts() {
        for fault in ["slot", "backup", "identity", "postapproval"] {
            let fixture = Fixture::new();
            let b = budget();
            let plan = publication_plan(&fixture, &b);
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let permit = publication_permit(
                &fixture,
                &plan,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            );
            let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::transactions::with_external_v3_test_hook(
                    |phase, index| {
                        if phase == "after_publish" && index == 0 {
                            panic!("V3 fault preparation")
                        }
                    },
                    || crate::transactions::publish_external_source(permit, candidate, &b),
                )
            }))
            .is_err());
            let revision = v3_active(&fixture);
            let state = fixture.engine.join(".aurum/revisions").join(&revision);
            let stale = if fault == "postapproval" {
                Some(v3_repair(
                    &fixture,
                    &plan.project,
                    &revision,
                    false,
                    &broker,
                    &b,
                ))
            } else {
                None
            };
            match fault {
                "slot" => {
                    std::fs::write(state.join("063.displaced"), b"foreign slot evidence\n").unwrap()
                }
                "backup" => {
                    std::fs::write(state.join("000.before"), b"corrupt retained backup\n").unwrap()
                }
                "identity" => {
                    let target = fixture.engine.join("native/src/lib.rs");
                    let bytes = std::fs::read(&target).unwrap();
                    std::fs::remove_file(&target).unwrap();
                    std::fs::write(target, bytes).unwrap();
                }
                "postapproval" => {
                    let header = state.join("v3.json");
                    let bytes = std::fs::read(&header).unwrap();
                    std::fs::remove_file(&header).unwrap();
                    std::fs::write(header, bytes).unwrap();
                }
                _ => unreachable!(),
            }
            let evidence = fixture.originals();
            if let Some(stale) = stale {
                assert!(crate::transactions::repair_external_source(stale, &b).is_err())
            } else {
                assert!(PreparedSourceRepair::prepare(
                    &plan.project,
                    RootId::new("engine").unwrap(),
                    &fixture.engine,
                    &revision,
                    false,
                    &b
                )
                .is_err())
            }
            assert_eq!(fixture.originals(), evidence);
            assert_eq!(v3_active(&fixture), revision);
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_interrupted_whole_undo_restores_the_forward_revision() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let value = crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
        assert_eq!(value["ok"], true);
        let forward = v3_source_oracle(&fixture);
        let revision = value["revision_id"].as_str().unwrap();
        let repair = v3_repair(&fixture, &plan.project, revision, true, &broker, &b);
        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = reached.clone();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::transactions::with_external_v3_test_hook(
                move |phase, index| {
                    if phase == "after_publish" && index == 0 {
                        flag.set(true);
                        panic!("V3 reverse interruption")
                    }
                },
                || crate::transactions::repair_external_source(repair, &b),
            )
        }))
        .is_err());
        assert!(reached.get());
        let reverse = v3_active(&fixture);
        assert_ne!(reverse, revision);
        let repair = v3_repair(&fixture, &plan.project, &reverse, false, &broker, &b);
        assert_eq!(
            crate::transactions::repair_external_source(repair, &b).unwrap()["ok"],
            true
        );
        assert_eq!(v3_source_oracle(&fixture), forward);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_cancel_broker_close_or_original_expiry_after_backups_never_activates() {
        for action in ["cancel", "close", "expire"] {
            let fixture = Fixture::new();
            let b = if action == "expire" {
                Budget::new(Duration::from_secs(5), &|| Ok(())).unwrap()
            } else {
                budget()
            };
            let plan = publication_plan(&fixture, &b);
            let original = v3_source_oracle(&fixture);
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let permit = publication_permit(&fixture, &plan, &broker, cancelled.clone(), &b);
            let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
            let actor = broker.clone();
            let reached = std::rc::Rc::new(std::cell::Cell::new(false));
            let flag = reached.clone();
            let deadline = b.deadline;
            let result = crate::transactions::with_external_v3_test_hook(
                move |phase, _| {
                    if phase == "backups_durable" {
                        flag.set(true);
                        match action {
                            "close" => actor.close(),
                            "cancel" => cancelled.store(true, std::sync::atomic::Ordering::Release),
                            "expire" => std::thread::sleep(
                                deadline.saturating_duration_since(Instant::now())
                                    + Duration::from_millis(1),
                            ),
                            _ => unreachable!(),
                        }
                    }
                },
                || crate::transactions::publish_external_source(permit, candidate, &b),
            );
            assert!(reached.get());
            assert!(result.is_err());
            assert_eq!(v3_source_oracle(&fixture), original);
            assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
            let states = std::fs::read_dir(fixture.engine.join(".aurum/revisions"))
                .unwrap()
                .count();
            assert_eq!(states, 1);
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_restart_requires_fresh_authority_and_repairs_invalid_configuration() {
        let fixture = Fixture::new();
        let b = budget();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (plan, permit, candidate, mut workspace) = v3_nested_candidate(&fixture, &broker, &b);
        let original = current_revision(&fixture.engine, &b).unwrap();
        let reached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = reached.clone();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::transactions::with_external_v3_test_hook(
                move |phase, index| {
                    if phase == "after_publish" && index == 0 {
                        flag.set(true);
                        panic!("restart preparation")
                    }
                },
                || crate::transactions::publish_external_source(permit, candidate, &b),
            )
        }))
        .is_err());
        assert!(reached.get());
        let revision = v3_active(&fixture);
        let abandoned = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
        broker.close();
        std::fs::write(fixture.app.join("aurum.toml"), b"invalid [ configuration\n").unwrap();
        let evidence = fixture.originals();
        assert!(crate::transactions::repair_external_source(abandoned, &b).is_err());
        assert_eq!(fixture.originals(), evidence);
        assert!(crate::transactions::recover_root(&fixture.engine).is_err());
        let fresh_budget = budget();
        let new_broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let fresh = v3_repair(
            &fixture,
            &plan.project,
            &revision,
            false,
            &new_broker,
            &fresh_budget,
        );
        assert_eq!(
            crate::transactions::repair_external_source(fresh, &fresh_budget).unwrap()["ok"],
            true
        );
        assert_eq!(
            current_revision(&fixture.engine, &fresh_budget).unwrap(),
            original
        );
        assert_eq!(
            std::fs::read(fixture.app.join("aurum.toml")).unwrap(),
            b"invalid [ configuration\n"
        );
        assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_cleanup_scope_forgery_and_postapproval_ownership_changes_preserve_directories(
    ) {
        for forged in [
            "native/src",
            "native/src/unrelated",
            "native/src/new/../other",
            "native/src/new/",
            "native/src/new",
            "postapproval",
        ] {
            let fixture = Fixture::new();
            let b = budget();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let (plan, permit, candidate, mut workspace) =
                v3_nested_candidate(&fixture, &broker, &b);
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::transactions::with_external_v3_test_hook(
                    |phase, index| {
                        if phase == "after_publish" && index == 2 {
                            panic!("owned directory fixture interruption")
                        }
                    },
                    || crate::transactions::publish_external_source(permit, candidate, &b),
                )
            }))
            .is_err());
            let revision = v3_active(&fixture);
            let state = fixture.engine.join(".aurum/revisions").join(&revision);
            let unrelated = fixture.engine.join("native/src/unrelated");
            std::fs::create_dir(&unrelated).unwrap();
            let identity = canonical_directory(&unrelated, &b).unwrap().1;
            if forged == "postapproval" {
                let permit = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
                let mut changed = false;
                for entry in std::fs::read_dir(&state).unwrap() {
                    let path = entry.unwrap().path();
                    if path.file_name().unwrap().to_string_lossy().starts_with('e') {
                        let mut value: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                        if value["action"]["kind"] == "directory_owned" {
                            let original = value["action"]["identity"]["id"][0].as_u64().unwrap();
                            value["action"]["identity"]["id"][0] =
                                serde_json::json!((original + 1) % 256);
                            std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                            changed = true;
                            break;
                        }
                    }
                }
                assert!(changed);
                let evidence = fixture.originals();
                assert!(crate::transactions::repair_external_source(permit, &b).is_err());
                assert_eq!(fixture.originals(), evidence);
            } else {
                let path = state.join("v3.json");
                let mut value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                value["journal"]["external"]["cleanup_dirs"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!(forged));
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
                let evidence = fixture.originals();
                let error = PreparedSourceRepair::prepare(
                    &plan.project,
                    RootId::new("engine").unwrap(),
                    &fixture.engine,
                    &revision,
                    false,
                    &b,
                )
                .err()
                .unwrap();
                assert!(error.contains("cleanup"), "{error}");
                assert_eq!(fixture.originals(), evidence);
            }
            assert_eq!(canonical_directory(&unrelated, &b).unwrap().1, identity);
            assert_eq!(
                std::fs::read(fixture.engine.join("native/src/new/deep/created.rs")).unwrap(),
                b"pub fn new_value() -> i32 { 29 }\n"
            );
            assert_eq!(v3_active(&fixture), revision);
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_state_reads_temp_events_marker_and_old_moves_are_confined_after_check() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::create_dir_all(fixture.engine.join(".aurum/revisions")).unwrap();
        for name in [
            "000.before",
            "000.temp",
            "000.old",
            "e000000.json",
            "active.json",
        ] {
            std::fs::write(
                fixture.base.join("ungranted").join(name),
                b"outside state oracle\n",
            )
            .unwrap();
        }
        let original = fixture.originals();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (admission, mut workspace) = namespace_admission(
            &fixture,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let baseline = fixture.originals();
        {
            let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
            assert_eq!(
                namespace_io::qualify_state_parent_races(
                    &namespace,
                    &fixture.base,
                    &fixture.base.join("ungranted")
                )
                .unwrap(),
                9
            );
        }
        assert_eq!(fixture.originals(), baseline);
        for name in [
            "000.before",
            "000.temp",
            "000.old",
            "e000000.json",
            "active.json",
        ] {
            assert_eq!(
                std::fs::read(fixture.base.join("ungranted").join(name)).unwrap(),
                b"outside state oracle\n"
            );
        }
        assert!(original
            .iter()
            .all(|entry| fixture.originals().contains(entry)));
        drop(admission);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_recovery_event_append_counts_foreign_partial_history_before_mutation() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::transactions::with_external_v3_test_hook(
                |phase, index| {
                    if phase == "after_publish" && index == 0 {
                        panic!("recovery quota fixture")
                    }
                },
                || crate::transactions::publish_external_source(permit, candidate, &b),
            )
        }))
        .is_err());
        let revision = v3_active(&fixture);
        let state = fixture.engine.join(".aurum/revisions").join(&revision);
        let records = || {
            let mut rows: Vec<_> = std::fs::read_dir(&state)
                .unwrap()
                .map(|e| {
                    let path = e.unwrap().path();
                    (
                        path.file_name().unwrap().to_owned(),
                        std::fs::metadata(&path).unwrap().len(),
                        crate::sha256_file(&path).unwrap(),
                    )
                })
                .collect();
            rows.sort();
            rows
        };
        let before = records();
        let source = std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap();
        let partial = fixture.engine.join(".aurum/revisions/unowned-partial");
        std::fs::create_dir(&partial).unwrap();
        let foreign = partial.join("foreign.bin");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&foreign)
            .unwrap();
        file.set_len(128 * 1024 * 1024).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let permit = v3_repair(&fixture, &plan.project, &revision, false, &broker, &b);
        let error = crate::transactions::repair_external_source(permit, &b)
            .err()
            .unwrap();
        assert!(error.contains("history_byte_quota"), "{error}");
        assert_eq!(records(), before);
        assert_eq!(
            std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
            source
        );
        assert_eq!(v3_active(&fixture), revision);
        assert_eq!(std::fs::metadata(foreign).unwrap().len(), 128 * 1024 * 1024);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_v3_partial_history_bytes_and_unknown_directories_count_before_creation() {
        for quota in ["bytes", "entries"] {
            let fixture = Fixture::new();
            let b = budget();
            let history = fixture.engine.join(".aurum/revisions");
            std::fs::create_dir_all(&history).unwrap();
            if quota == "bytes" {
                let partial = history.join("unknown-partial");
                std::fs::create_dir(&partial).unwrap();
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(partial.join("retained.bin"))
                    .unwrap();
                file.set_len(128 * 1024 * 1024).unwrap();
                file.sync_all().unwrap();
            } else {
                for index in 0..32 {
                    std::fs::create_dir(history.join(format!("unowned-{index}"))).unwrap();
                }
            }
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let (admission, mut workspace) = namespace_admission(
                &fixture,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            );
            let original = scan(&fixture.engine, &b, &mut Usage::default()).unwrap().1;
            let count = std::fs::read_dir(&history).unwrap().count();
            {
                let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
                let error = namespace.preflight(1).err().unwrap();
                assert!(error.contains(if quota == "bytes" {
                    "history_byte_quota"
                } else {
                    "history_entry_quota"
                }));
            }
            assert_eq!(
                scan(&fixture.engine, &b, &mut Usage::default()).unwrap().1,
                original
            );
            assert_eq!(std::fs::read_dir(&history).unwrap().count(), count);
            assert!(!history.join("active.json").exists());
            if quota == "bytes" {
                assert_eq!(
                    std::fs::metadata(history.join("unknown-partial/retained.bin"))
                        .unwrap()
                        .len(),
                    128 * 1024 * 1024
                )
            }
            assert_eq!(
                std::fs::read(fixture.base.join("ungranted/keep")).unwrap(),
                b"ungranted sibling must never be copied\n"
            );
            drop(admission);
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_retained_enumeration_fingerprint_and_quotas() {
        let fixture = Fixture::new();
        let b = budget();
        let data = vec![b'x'; 153_601];
        std::fs::write(fixture.engine.join("native/src/large.rs"), &data).unwrap();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let project = ImportedProject::bind(
            &fixture.app,
            Revision::new(&current_revision(&fixture.app, &b).unwrap()).unwrap(),
            &b,
        )
        .unwrap();
        let grants = fixture.publication_grants(&project, &b);
        {
            let reader =
                namespace_io::ReadNamespace::dependency(&grants[0], &project.revision, &b).unwrap();
            let directory = reader.directory("native/src").unwrap();
            let entries = directory.entries().unwrap();
            assert!(entries.iter().any(|entry| entry.name == "large.rs"
                && entry.bytes == data.len() as u64
                && !entry.directory));
            let captured = directory
                .read("large.rs", 2 * 1024 * 1024, namespace_io::Capture::Bytes)
                .unwrap();
            assert_eq!(captured.contents.as_deref(), Some(data.as_slice()));
            assert_eq!(captured.sha256, crate::sha256_hex(&data));
            let hashed = directory
                .read("large.rs", MAX_FILE_BYTES, namespace_io::Capture::HashOnly)
                .unwrap();
            assert!(hashed.contents.is_none());
            assert_eq!(hashed.sha256, captured.sha256);
            for invalid in [
                "../keep", "a/b.rs", "a\\b.rs", "c:stream", ".", "..", "NUL.rs", "bad.",
            ] {
                assert!(
                    directory
                        .read(invalid, 1, namespace_io::Capture::Bytes)
                        .is_err(),
                    "{invalid}"
                );
            }
            let root = reader.directory("").unwrap();
            let snapshot = root.snapshot().unwrap();
            assert_eq!(
                snapshot.fingerprint,
                scan(&fixture.engine, &b, &mut Usage::default()).unwrap().1
            );
            assert!(std::fs::write(fixture.engine.join("native/src/large.rs"), b"other").is_err());
            snapshot.check().unwrap();
        }
        for quota in ["entries", "files", "bytes", "file"] {
            let mut bounded = budget();
            match quota {
                // The imported fixture still fits each bound. Its external
                // source tree must independently exercise the selected quota.
                "entries" => bounded.max_entries = 5,
                "files" => bounded.max_files = 3,
                "bytes" => bounded.max_bytes = 1024,
                "file" => bounded.max_file = 1024,
                _ => unreachable!(),
            }
            let reader =
                namespace_io::ReadNamespace::dependency(&grants[0], &project.revision, &bounded)
                    .unwrap();
            assert!(
                reader.directory("").unwrap().snapshot().is_err(),
                "missing {quota} bound"
            );
        }
        assert_eq!(before, fixture_oracle(&fixture, &b).unwrap());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_capture_empty_exact_boundary_and_read_authority_refusals() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::write(fixture.engine.join("native/src/empty.rs"), b"").unwrap();
        let exact = vec![b'z'; crate::revisions::MAX_FILE_BYTES];
        std::fs::write(fixture.engine.join("native/src/exact.rs"), &exact).unwrap();
        std::fs::write(
            fixture.engine.join("native/src/over.rs"),
            vec![b'z'; exact.len() + 1],
        )
        .unwrap();
        let project = ImportedProject::bind(
            &fixture.app,
            Revision::new(&current_revision(&fixture.app, &b).unwrap()).unwrap(),
            &b,
        )
        .unwrap();
        let grants = fixture.publication_grants(&project, &b);
        {
            let reader =
                namespace_io::ReadNamespace::dependency(&grants[0], &project.revision, &b).unwrap();
            let directory = reader.directory("native/src").unwrap();
            assert_eq!(
                directory
                    .read("empty.rs", 0, namespace_io::Capture::Bytes)
                    .unwrap()
                    .contents,
                Some(Vec::new())
            );
            assert_eq!(
                directory
                    .read("exact.rs", exact.len() as u64, namespace_io::Capture::Bytes)
                    .unwrap()
                    .contents,
                Some(exact)
            );
            assert!(directory
                .read(
                    "over.rs",
                    CAPTURE_LIMIT_FOR_NAMESPACE_TEST,
                    namespace_io::Capture::Bytes
                )
                .is_err());
            assert!(directory
                .read("missing.rs", 2, namespace_io::Capture::Bytes)
                .is_err());
            assert!(directory
                .read("empty.rs", MAX_FILE_BYTES, namespace_io::Capture::Bytes)
                .is_err());
            assert!(ContentLock::exclusive(&fixture.engine).is_err());
            std::fs::write(fixture.app.join("new-imported.gd"), b"new imported bytes\n").unwrap();
            assert!(reader.directory("native/src").is_err());
            std::fs::remove_file(fixture.app.join("new-imported.gd")).unwrap();
            directory
                .read("empty.rs", 0, namespace_io::Capture::HashOnly)
                .unwrap();
        }
        let wrong = Revision::new(&"2".repeat(64)).unwrap();
        assert!(namespace_io::ReadNamespace::dependency(&grants[0], &wrong, &b).is_err());
        let mut wrong_mode = grants[0].clone();
        wrong_mode.mode = Mode::SourcePublish;
        assert!(
            namespace_io::ReadNamespace::dependency(&wrong_mode, &project.revision, &b).is_err()
        );
        let mut forged = grants[0].clone();
        forged.identity.volume ^= 1;
        assert!(namespace_io::ReadNamespace::dependency(&forged, &project.revision, &b).is_err());
        let original_imported = std::fs::read(fixture.app.join("main.gd")).unwrap();
        std::fs::write(
            fixture.app.join("main.gd"),
            b"extends Node\n# changed after approval\n",
        )
        .unwrap();
        assert_eq!(
            namespace_io::ReadNamespace::dependency(&grants[0], &project.revision, &b)
                .err()
                .unwrap(),
            "external_namespace_imported_revision_mismatch"
        );
        std::fs::write(fixture.app.join("main.gd"), original_imported).unwrap();
        let state = fixture.engine.join(".aurum/revisions");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("active.json"), b"{}\n").unwrap();
        assert!(
            namespace_io::ReadNamespace::dependency(&grants[0], &project.revision, &b)
                .err()
                .unwrap()
                .contains("unresolved revision")
        );
        assert_eq!(std::fs::read(state.join("active.json")).unwrap(), b"{}\n");
    }
    #[cfg(windows)]
    const CAPTURE_LIMIT_FOR_NAMESPACE_TEST: u64 = crate::revisions::MAX_FILE_BYTES as u64;
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_direct_and_subtree_apertures_refuse_excluded_content() {
        let fixture = Fixture::new();
        let b = budget();
        let private = [
            "credentials.toml",
            ".env",
            "token.pem",
            ".aurum/private.rs",
            ".hidden/private.rs",
            "target/cache-private.rs",
            "native/src/credentials.toml",
            "native/src/.hidden/private.rs",
            "native/src/token.pfx",
        ];
        for relative in private {
            let path = fixture.engine.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"controlled private source sentinel\n").unwrap();
        }
        let private_before: Vec<_> = private
            .iter()
            .map(|relative| {
                (
                    fixture.engine.join(relative),
                    std::fs::read(fixture.engine.join(relative)).unwrap(),
                )
            })
            .collect();
        let project = ImportedProject::bind(
            &fixture.app,
            Revision::new(&current_revision(&fixture.app, &b).unwrap()).unwrap(),
            &b,
        )
        .unwrap();
        let grants = fixture.publication_grants(&project, &b);
        {
            let reader =
                namespace_io::ReadNamespace::dependency(&grants[0], &project.revision, &b).unwrap();
            let root = reader.directory("").unwrap();
            for name in ["credentials.toml", "Credentials.toml", ".env", "token.pem"] {
                assert_eq!(
                    root.read(name, 1024, namespace_io::Capture::Bytes)
                        .err()
                        .unwrap(),
                    "external_namespace_source_aperture_refused",
                    "{name}"
                );
            }
            for path in [
                ".aurum",
                ".hidden",
                "target",
                "TARGET",
                "dist",
                "node_modules",
                "logs",
                "test-results",
                "playwright-report",
                "native/src/.hidden",
                "native/src/credentials.toml",
            ] {
                assert_eq!(
                    reader
                        .directory(path)
                        .and_then(|directory| directory.snapshot().map(|_| ()))
                        .err()
                        .unwrap(),
                    "external_namespace_source_aperture_refused",
                    "{path}"
                );
            }
            let source = reader.directory("native/src").unwrap();
            for name in ["credentials.toml", "token.pfx"] {
                assert_eq!(
                    source
                        .read(name, 1024, namespace_io::Capture::Bytes)
                        .err()
                        .unwrap(),
                    "external_namespace_source_aperture_refused"
                );
            }
            for path in [".cargo", ".CARGO", "native/src/.cargo"] {
                assert_eq!(
                    reader.directory(path).err().unwrap(),
                    "unsupported_cargo_configuration_or_build_script"
                );
            }
            for name in ["build.rs", "BUILD.RS"] {
                assert_eq!(
                    source
                        .read(name, 1024, namespace_io::Capture::Bytes)
                        .err()
                        .unwrap(),
                    "unsupported_cargo_configuration_or_build_script"
                );
            }
            assert!(!root.entries().unwrap().iter().any(|entry| [
                "credentials.toml",
                ".aurum",
                ".hidden",
                "target",
                "token.pem"
            ]
            .contains(&entry.name.as_str())));
            assert_eq!(
                root.snapshot().unwrap().fingerprint,
                scan(&fixture.engine, &b, &mut Usage::default()).unwrap().1
            );
            assert_eq!(
                source.snapshot().unwrap().fingerprint,
                scan(
                    &fixture.engine.join("native/src"),
                    &b,
                    &mut Usage::default()
                )
                .unwrap()
                .1
            );
        }
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (admission, mut workspace) = namespace_admission(
            &fixture,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        {
            let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
            let source = namespace.reader_directory("native/src").unwrap();
            assert_eq!(
                source
                    .read("credentials.toml", 1024, namespace_io::Capture::Bytes)
                    .err()
                    .unwrap(),
                "external_namespace_source_aperture_refused"
            );
            for path in [".aurum", "target", "native/src/.hidden"] {
                assert_eq!(
                    namespace
                        .reader_directory(path)
                        .and_then(|directory| directory.snapshot().map(|_| ()))
                        .err()
                        .unwrap(),
                    "external_namespace_source_aperture_refused"
                );
            }
            // Physical Cargo configuration now exists, but no reader aperture
            // may expose it or the build script. This remains a test fixture.
            std::fs::create_dir(fixture.engine.join(".cargo")).unwrap();
            std::fs::write(
                fixture.engine.join(".cargo/config.toml"),
                b"controlled cargo private marker\n",
            )
            .unwrap();
            std::fs::write(
                fixture.engine.join("build.rs"),
                b"controlled build private marker\n",
            )
            .unwrap();
            assert_eq!(
                namespace.reader_directory(".cargo").err().unwrap(),
                "unsupported_cargo_configuration_or_build_script"
            );
            let root = namespace.reader_directory("").unwrap();
            assert_eq!(
                root.read("build.rs", 1024, namespace_io::Capture::Bytes)
                    .err()
                    .unwrap(),
                "unsupported_cargo_configuration_or_build_script"
            );
            assert_eq!(
                root.snapshot().err().unwrap(),
                "unsupported_cargo_configuration_or_build_script"
            );
        }
        for (path, bytes) in private_before {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
        assert_eq!(
            std::fs::read(fixture.base.join("ungranted/keep")).unwrap(),
            b"ungranted sibling must never be copied\n"
        );
        drop(admission);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_publication_mirrors_cannot_expand_sealed_scope() {
        let fixture = Fixture::new();
        let b = budget();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (mut admission, mut workspace) = namespace_admission(
            &fixture,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let original = fixture.originals();
        let binding = admission.binding.clone();
        admission
            .binding
            .targets
            .push(crate::transactions::ExternalTarget {
                path: "native/src/unapproved.rs".into(),
                before: None,
                after: Some(crate::sha256_hex(b"unapproved\n")),
            });
        assert_eq!(
            namespace_io::WriteNamespace::publication(&admission, &b)
                .err()
                .unwrap(),
            "external_namespace_publication_mirror_changed"
        );
        admission.binding = binding;
        let changes = admission.changes.clone();
        admission.changes[0].2 = Some(b"unapproved\n".to_vec());
        assert!(namespace_io::WriteNamespace::publication(&admission, &b).is_err());
        admission.changes = changes;
        let root = admission.root.clone();
        admission.root = fixture.base.join("ungranted");
        assert!(namespace_io::WriteNamespace::publication(&admission, &b).is_err());
        admission.root = root;
        {
            let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
            assert!(namespace.target("native/src/unapproved.rs").is_err());
            namespace
                .target("native/src/lib.rs")
                .unwrap()
                .read_target(namespace_io::Capture::HashOnly)
                .unwrap();
        }
        assert_eq!(original, fixture.originals());
        drop(admission);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_repair_requires_unchanged_fresh_sealed_scope() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let original = std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap();
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let committed =
            crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
        assert_eq!(committed["ok"], true);
        let revision = committed["revision_id"].as_str().unwrap();
        let prepared = PreparedSourceRepair::prepare(
            &plan.project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            revision,
            true,
            &b,
        )
        .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let permit = prepared
            .finish(
                confirmation,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            )
            .unwrap();
        let mut admission = permit.admit(&b).unwrap();
        let scope = admission.scope.clone();
        admission
            .scope
            .binding
            .targets
            .push(crate::transactions::ExternalTarget {
                path: "native/src/unreviewed.rs".into(),
                before: None,
                after: Some(crate::sha256_hex(b"unreviewed\n")),
            });
        assert_eq!(
            namespace_io::WriteNamespace::repair(&admission, &b)
                .err()
                .unwrap(),
            "external_namespace_repair_mirror_changed"
        );
        admission.scope = scope;
        let digest = admission.authorization_digest.clone();
        admission.authorization_digest = "unreviewed".into();
        assert!(namespace_io::WriteNamespace::repair(&admission, &b).is_err());
        admission.authorization_digest = digest;
        let before = fixture.originals();
        {
            let namespace = namespace_io::WriteNamespace::repair(&admission, &b).unwrap();
            let target = namespace.target("native/src/lib.rs").unwrap();
            target.create_temp(&original).unwrap().delete().unwrap();
            assert!(namespace.target("native/src/unreviewed.rs").is_err());
        }
        assert_eq!(before, fixture.originals());
        drop(admission);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_combined_parent_roles_preserve_old_temp_and_concurrent_target() {
        let fixture = Fixture::new();
        let b = budget();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let (admission, mut workspace) = namespace_admission(
            &fixture,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let unrelated = fixture_all_entry_oracle::capture(
            &[
                ("app", &fixture.app),
                ("vendor", &fixture.vendor),
                ("ungranted", &fixture.base.join("ungranted")),
            ],
            &b,
        )
        .unwrap();
        let original = std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap();
        let proposal = admission
            .changes
            .iter()
            .find(|row| row.0 == "native/src/lib.rs")
            .unwrap()
            .2
            .as_ref()
            .unwrap()
            .clone();
        {
            let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
            assert!(namespace.target("native/src/unapproved.rs").is_err());
            assert!(namespace.target("../ungranted/keep.rs").is_err());
            let target = namespace.target("native/src/lib.rs").unwrap();
            assert_eq!(
                target
                    .read_target(namespace_io::Capture::Bytes)
                    .unwrap()
                    .contents
                    .as_deref(),
                Some(original.as_slice())
            );
            assert!(target.open_expected(&"0".repeat(64)).is_err());
            assert!(target.create_temp(b"unapproved bytes").is_err());
            let mut old = target.open_expected(&crate::sha256_hex(&original)).unwrap();
            assert!(
                std::fs::write(fixture.engine.join("native/src/lib.rs"), b"competing write")
                    .is_err()
            );
            assert!(std::fs::remove_file(fixture.engine.join("native/src/lib.rs")).is_err());
            let mut proposed = target.create_temp(&proposal).unwrap();
            assert!(proposed.publish_no_replace().is_err());
            assert_eq!(
                std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
                original
            );
            old.move_aside().unwrap();
            std::fs::write(
                fixture.engine.join("native/src/lib.rs"),
                b"concurrent owner\n",
            )
            .unwrap();
            assert!(proposed.publish_no_replace().is_err());
            assert_eq!(
                std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
                b"concurrent owner\n"
            );
            std::fs::remove_file(fixture.engine.join("native/src/lib.rs")).unwrap();
            proposed.publish_no_replace().unwrap();
            assert!(proposed.publish_no_replace().is_err());
            assert_eq!(
                std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
                proposal
            );
            old.delete().unwrap();
            drop(proposed);
            let snapshot = namespace.reader_directory("native/src").unwrap();
            snapshot.snapshot().unwrap().check().unwrap();
        }
        assert_eq!(
            unrelated,
            fixture_all_entry_oracle::capture(
                &[
                    ("app", &fixture.app),
                    ("vendor", &fixture.vendor),
                    ("ungranted", &fixture.base.join("ungranted"))
                ],
                &b
            )
            .unwrap()
        );
        drop(admission);
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_original_budget_cancel_expiry_and_broker_close_retain_temp() {
        for stop in ["budget", "cancel", "expire", "broker"] {
            let fixture = Fixture::new();
            let stopped = AtomicBool::new(false);
            let check = || {
                if stopped.load(Ordering::Acquire) {
                    Err("namespace_original_budget_cancelled".into())
                } else {
                    Ok(())
                }
            };
            let mut b = Budget::new(Duration::from_secs(30), &check).unwrap();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let (mut admission, mut workspace) =
                namespace_admission(&fixture, &broker, cancelled.clone(), &b);
            let expires = Instant::now() + Duration::from_secs(2);
            if stop == "expire" {
                admission._permit.session.expires = expires;
                b.deadline = expires;
            }
            let original = std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap();
            let proposal = admission
                .changes
                .iter()
                .find(|row| row.0 == "native/src/lib.rs")
                .unwrap()
                .2
                .as_ref()
                .unwrap()
                .clone();
            {
                let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
                let target = namespace.target("native/src/lib.rs").unwrap();
                let mut temp = target.create_temp(&proposal).unwrap();
                let source_reader = namespace.reader_directory("native/src").unwrap();
                match stop {
                    "budget" => stopped.store(true, Ordering::Release),
                    "cancel" => cancelled.store(true, Ordering::Release),
                    "broker" => broker.close(),
                    "expire" => {
                        while Instant::now() < expires {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    _ => unreachable!(),
                }
                let fresh_unrelated_budget = budget();
                fresh_unrelated_budget.check().unwrap();
                let expected = if stop == "budget" {
                    "namespace_original_budget_cancelled"
                } else if stop == "expire" {
                    "external_staging_deadline"
                } else {
                    "external_source_authority_cancelled_or_closed"
                };
                assert_eq!(
                    source_reader
                        .read("credentials.toml", 1024, namespace_io::Capture::Bytes)
                        .err()
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    namespace
                        .reader_directory("target")
                        .and_then(|directory| directory.snapshot().map(|_| ()))
                        .err()
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    temp.publish_no_replace().err().unwrap(),
                    if stop == "budget" {
                        "namespace_original_budget_cancelled"
                    } else if stop == "expire" {
                        "external_staging_deadline"
                    } else {
                        "external_source_authority_cancelled_or_closed"
                    },
                    "{stop}"
                );
                assert!(temp.delete().is_err());
                assert!(namespace_io::WriteNamespace::publication(
                    &admission,
                    &fresh_unrelated_budget
                )
                .is_err());
            }
            assert_eq!(
                std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
                original
            );
            let evidence: Vec<_> = std::fs::read_dir(fixture.engine.join("native/src"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".aurum-src-")
                })
                .collect();
            assert_eq!(evidence.len(), 1, "{stop}");
            assert_eq!(std::fs::read(&evidence[0]).unwrap(), proposal);
            drop(admission);
            workspace.clean().unwrap();
        }
    }
    #[cfg(windows)]
    #[test]
    fn external_source_namespace_metadata_only_after_check_races_keep_outside_canaries() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::create_dir(fixture.engine.join("native/src/race")).unwrap();
        std::fs::write(
            fixture.base.join("ungranted/new.rs"),
            b"outside source sentinel\n",
        )
        .unwrap();
        let before = fixture_all_entry_oracle::capture(
            &[
                ("app", &fixture.app),
                ("vendor", &fixture.vendor),
                ("ungranted", &fixture.base.join("ungranted")),
            ],
            &b,
        )
        .unwrap();
        let plan = publication_plan(&fixture, &b);
        let prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            vec![crate::revisions::Change {
                path: "native/src/race/new.rs".into(),
                action: crate::revisions::Action::Create,
                expected_sha256: String::new(),
                text: Some("pub fn namespace_proposal() -> i32 { 41 }\n".into()),
            }],
            &b,
        )
        .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = prepared
            .finish(
                confirmation,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &plan.project.revision,
                &b,
            )
            .unwrap();
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let admission = permit.admit(candidate, &b).unwrap();
        {
            let namespace = namespace_io::WriteNamespace::publication(&admission, &b).unwrap();
            let target = namespace.target("native/src/race/new.rs").unwrap();
            assert_eq!(
                namespace_io::qualify_metadata_only_parent_races(
                    &target,
                    &fixture.base,
                    &fixture.base.join("ungranted")
                )
                .unwrap(),
                5
            );
        }
        assert_eq!(
            before,
            fixture_all_entry_oracle::capture(
                &[
                    ("app", &fixture.app),
                    ("vendor", &fixture.vendor),
                    ("ungranted", &fixture.base.join("ungranted"))
                ],
                &b
            )
            .unwrap()
        );
        assert_eq!(
            std::fs::read_dir(fixture.engine.join("native/src/race"))
                .unwrap()
                .count(),
            0
        );
        drop(admission);
        workspace.clean().unwrap();
    }
    #[test]
    fn external_source_linked_revision_and_fresh_whole_undo_preserve_accepted_state() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::write(fixture.app.join("accepted.dll"), b"accepted native dll").unwrap();
        let plan = publication_plan(&fixture, &b);
        let before = fixture.originals();
        let unrelated = fixture_all_entry_oracle::capture(
            &[
                ("app", fixture.app.as_path()),
                ("vendor", fixture.vendor.as_path()),
                ("ungranted", fixture.base.join("ungranted").as_path()),
            ],
            &b,
        )
        .unwrap();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        let result = crate::transactions::publish_external_source(permit, candidate, &b).unwrap();
        assert_eq!(result["ok"], true);
        assert!(!fixture.engine.join("native/src/old.rs").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.engine.join("native/src/new.rs")).unwrap(),
            "pub fn new_value() -> i32 { 29 }\n"
        );
        let revision = result["revision_id"].as_str().unwrap();
        let prepared = PreparedSourceRepair::prepare(
            &plan.project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            revision,
            true,
            &b,
        )
        .unwrap();
        let confirmation =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let undo = prepared
            .finish(
                confirmation,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            )
            .unwrap();
        assert_eq!(
            crate::transactions::repair_external_source(undo, &b).unwrap()["ok"],
            true
        );
        let after: Vec<_> = fixture
            .originals()
            .into_iter()
            .filter(|(path, _)| !path.starts_with("engine/.aurum/"))
            .collect();
        assert_eq!(after, before);
        assert_eq!(
            fixture_all_entry_oracle::capture(
                &[
                    ("app", fixture.app.as_path()),
                    ("vendor", fixture.vendor.as_path()),
                    ("ungranted", fixture.base.join("ungranted").as_path())
                ],
                &b
            )
            .unwrap(),
            unrelated
        );
        assert_eq!(
            std::fs::read(fixture.app.join("accepted.dll")).unwrap(),
            b"accepted native dll"
        );
        workspace.clean().unwrap();
    }
    #[test]
    fn external_source_read_confirmation_forgery_cancel_and_closed_session_refuse() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let before = fixture_oracle(&fixture, &b);
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            publication_changes(&fixture),
            &b,
        )
        .unwrap();
        let read = PreparedGrant::dependency_read(
            &plan.project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            &b,
        )
        .unwrap();
        assert_ne!(prepared.digest(), read.digest());
        assert!(prepared
            .finish(
                crate::workspace_access::simulated_private_confirmation_for_test(&read.digest()),
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &plan.project.revision,
                &b
            )
            .is_err());
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let permit = publication_permit(&fixture, &plan, &broker, cancelled.clone(), &b);
        cancelled.store(true, Ordering::Release);
        assert!(permit.session.check(&b).is_err());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        broker.close();
        assert!(permit.session.check(&b).is_err());
        assert_eq!(fixture_oracle(&fixture, &b), before);
    }
    #[test]
    fn external_source_stale_original_dependency_candidate_and_artifact_are_preserved() {
        for changed in ["imported", "dependency", "target", "candidate", "artifact"] {
            let fixture = Fixture::new();
            let b = budget();
            let plan = publication_plan(&fixture, &b);
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let permit = publication_permit(
                &fixture,
                &plan,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &b,
            );
            let (mut candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
            let path = match changed {
                "imported" => fixture.app.join("new.gd"),
                "dependency" => fixture.vendor.join("src/lib.rs"),
                "target" => fixture.engine.join("native/src/lib.rs"),
                "candidate" => candidate
                    .proposal
                    .staged
                    .roots
                    .iter()
                    .find(|root| root.root_id.as_str() == "engine")
                    .unwrap()
                    .staged
                    .join("native/src/lib.rs"),
                _ => candidate.artifact.clone(),
            };
            if changed == "artifact" {
                assert!(std::fs::write(&path, b"changed artifact").is_err());
                // Independently refuse a mismatched accepted hash as well.
                candidate.artifact_hash = "0".repeat(64);
            } else {
                std::fs::write(&path, b"newer edit\n").unwrap();
            }
            let actual = std::fs::read(&path).unwrap();
            assert!(
                crate::transactions::publish_external_source(permit, candidate, &b).is_err(),
                "{changed}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), actual);
            assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
            workspace.clean().unwrap();
        }
    }
    #[test]
    fn external_source_scope_alias_byte_and_wrong_revision_refusals_are_read_only() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let before = fixture_oracle(&fixture, &b);
        for path in [
            "Cargo.toml",
            "native/build.rs",
            "native/src/build.rs",
            "native/src/../other.rs",
            "native/src/CON.rs",
            "native/src/secret.pem",
            "sibling/src/lib.rs",
        ] {
            let mut changes = publication_changes(&fixture);
            changes[0].path = path.into();
            assert!(PreparedSourcePublication::prepare(
                &plan,
                &RootId::new("engine").unwrap(),
                changes,
                &b
            )
            .is_err());
        }
        let mut changes = publication_changes(&fixture);
        changes.push(crate::revisions::Change {
            path: "native/src/NEW.rs".into(),
            action: crate::revisions::Action::Create,
            expected_sha256: String::new(),
            text: Some("alias".into()),
        });
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            changes,
            &b
        )
        .is_err());
        let mut changes = publication_changes(&fixture);
        changes[0].text = Some("x".repeat(crate::revisions::MAX_FILE_BYTES + 1));
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            changes,
            &b
        )
        .is_err());
        let many = (0..65)
            .map(|index| crate::revisions::Change {
                path: format!("native/src/bound{index}.rs"),
                action: crate::revisions::Action::Create,
                expected_sha256: String::new(),
                text: Some("pub fn value() {}\n".into()),
            })
            .collect();
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            many,
            &b
        )
        .is_err());
        let aggregate = (0..5)
            .map(|index| crate::revisions::Change {
                path: format!("native/src/large{index}.rs"),
                action: crate::revisions::Action::Create,
                expected_sha256: String::new(),
                text: Some("x".repeat(crate::revisions::MAX_FILE_BYTES)),
            })
            .collect();
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            aggregate,
            &b
        )
        .is_err());
        let mut changes = publication_changes(&fixture);
        changes[0].expected_sha256 = "0".repeat(64);
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            changes,
            &b
        )
        .is_err());
        let prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            publication_changes(&fixture),
            &b,
        )
        .unwrap();
        let proof =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        assert!(prepared
            .finish(
                proof,
                &std::sync::Arc::new(crate::workspace_access::Broker::default()),
                std::sync::Arc::new(AtomicBool::new(false)),
                &Revision::new(&"0".repeat(64)).unwrap(),
                &b
            )
            .is_err());
        assert_eq!(fixture_oracle(&fixture, &b), before);
    }
    #[test]
    fn external_source_held_root_and_parent_identity_prevent_redirects() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            publication_changes(&fixture),
            &b,
        )
        .unwrap();
        let before = fixture_oracle(&fixture, &b).unwrap();
        assert!(std::fs::rename(&fixture.engine, fixture.base.join("renamed-engine")).is_err());
        assert!(std::fs::rename(
            fixture.engine.join("native/src"),
            fixture.engine.join("native/renamed-src")
        )
        .is_err());
        assert!(set_owned_test_junction(
            &fixture.base,
            &fixture.engine.join("native"),
            &fixture.base.join("ungranted"),
            false
        )
        .is_err());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
        drop(prepared);
        let alias = fixture.engine.join("native/src/alias");
        create_owned_test_junction(&fixture.base, &alias, &fixture.base.join("ungranted")).unwrap();
        let mut changes = publication_changes(&fixture);
        changes[2].path = "native/src/alias/new.rs".into();
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            changes,
            &b
        )
        .is_err());
        std::fs::remove_dir(alias).unwrap();
        let changes = publication_changes(&fixture);
        std::fs::rename(&fixture.engine, fixture.base.join("original-engine")).unwrap();
        std::fs::create_dir(&fixture.engine).unwrap();
        assert!(PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            changes,
            &b
        )
        .is_err());
    }
    #[test]
    fn external_source_unused_read_grant_is_identity_checked_but_not_copied_or_fingerprinted() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::write(
            fixture.engine.join("native/src/old.rs"),
            b"pub fn old() {}\n",
        )
        .unwrap();
        let unused = fixture.base.join("unused");
        std::fs::create_dir(&unused).unwrap();
        std::fs::write(
            unused.join("untouched.sentinel"),
            b"unused source must not be copied\n",
        )
        .unwrap();
        let project = ImportedProject::bind(
            &fixture.app,
            Revision::new(&current_revision(&fixture.app, &b).unwrap()).unwrap(),
            &b,
        )
        .unwrap();
        let mut grants = fixture.publication_grants(&project, &b);
        grants.push(fixture.publication_grant(&project, "unused", &unused, &b));
        let mut placements = Fixture::placements();
        placements.push(Placement {
            root_id: RootId::new("unused").unwrap(),
            slot: "unused".into(),
        });
        let plan = StagingPlan::resolve(
            &project,
            &grants,
            &placements,
            Fixture::selection(),
            &project.revision,
            &b,
        )
        .unwrap();
        assert!(!plan.selected.contains(&RootId::new("unused").unwrap()));
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        assert_eq!(permit.prepared.unused_roots.len(), 1);
        let (candidate, mut workspace) = filesystem_candidate(&fixture, &plan, &permit, &b);
        assert!(candidate
            .proposal
            .staged
            .roots
            .iter()
            .all(|root| root.root_id.as_str() != "unused"));
        assert!(!candidate.proposal.staged.envelope.join("unused").exists());
        // An unused grant participates through identity, not source content.
        std::fs::write(unused.join("later.sentinel"), b"not a dependency\n").unwrap();
        let before =
            fixture_all_entry_oracle::capture(&[("unused", unused.as_path())], &b).unwrap();
        assert_eq!(
            crate::transactions::publish_external_source(permit, candidate, &b).unwrap()["ok"],
            true
        );
        assert_eq!(
            fixture_all_entry_oracle::capture(&[("unused", unused.as_path())], &b).unwrap(),
            before
        );
        workspace.clean().unwrap();
    }
    #[test]
    fn external_source_atomic_replace_parent_role_retains_ancestor_and_delete_guards() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let b = budget();
        let source = fixture.engine.join("native/src/lib.rs");
        let parent = source.parent().unwrap();
        let ancestor = parent.parent().unwrap();
        let before_identity = canonical_directory(parent, &b).unwrap().1;
        let guards = SourceGuards::capture(
            &fixture.engine,
            std::iter::once("native/src/lib.rs".to_string()),
            &b,
        )
        .unwrap();
        crate::files::write_atomic(&source, b"pub fn atomically_replaced() {}\n").unwrap();
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"pub fn atomically_replaced() {}\n"
        );
        guards.check(&b).unwrap();
        assert_eq!(canonical_directory(parent, &b).unwrap().1, before_identity);
        let writable = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(7)
            .custom_flags(0x0220_0000)
            .open(parent)
            .unwrap();
        drop(writable);
        assert!(OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(7)
            .custom_flags(0x0220_0000)
            .open(ancestor)
            .is_err());
        assert!(std::fs::rename(parent, ancestor.join("renamed-src")).is_err());
        assert!(std::fs::rename(ancestor, fixture.engine.join("renamed-native")).is_err());
        guards.check(&b).unwrap();
    }
    #[test]
    fn external_source_writable_parent_reparse_change_is_detected_before_row_mutation() {
        let fixture = Fixture::new();
        let b = budget();
        let parent = fixture.engine.join("native/src/empty-direct");
        std::fs::create_dir(&parent).unwrap();
        let outside = fixture.base.join("ungranted");
        let outside_before =
            fixture_all_entry_oracle::capture(&[("ungranted", outside.as_path())], &b).unwrap();
        let guards = SourceGuards::capture(
            &fixture.engine,
            std::iter::once("native/src/empty-direct/new.rs".to_string()),
            &b,
        )
        .unwrap();
        set_owned_test_junction(&fixture.base, &parent, &outside, false).unwrap();
        assert!(guards.check(&b).is_err());
        let authority = SourceDirectoryAuthority {
            root: &fixture.engine,
            scope: &[],
            guards: &guards,
            budget: &b,
        };
        assert!(crate::transactions::DirectoryAuthority::check(&authority).is_err());
        assert!(!outside.join("new.rs").exists());
        assert_eq!(
            fixture_all_entry_oracle::capture(&[("ungranted", outside.as_path())], &b).unwrap(),
            outside_before
        );
        drop(guards);
        std::fs::remove_dir(&parent).unwrap();
    }
    #[test]
    fn external_source_text_capture_preserves_exact_utf8_empty_absent_and_streaming_modes() {
        let fixture = Fixture::new();
        let b = budget();
        let text = format!("// {}\n", "ä".repeat(40_000));
        assert!(text.len() > 64 * 1024);
        let relative = "native/src/captured.rs";
        let path = fixture.engine.join(relative);
        std::fs::write(&path, text.as_bytes()).unwrap();
        let actual = source_text(&fixture.engine, relative, &b).unwrap().unwrap();
        assert_eq!(actual, text.as_bytes());
        assert_eq!(
            crate::sha256_hex(&actual),
            crate::sha256_hex(text.as_bytes())
        );
        let (digest, contents) = read_file(&path, MAX_FILE_BYTES, &b, None, None).unwrap();
        assert_eq!(digest, crate::sha256_hex(text.as_bytes()));
        assert!(contents.is_empty());
        let copied = fixture.base.join("captured-copy");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&copied)
            .unwrap();
        let (streamed_digest, streamed_contents) = read_file(
            &path,
            MAX_FILE_BYTES,
            &b,
            Some(&mut output),
            Some(text.len() as u64),
        )
        .unwrap();
        output.sync_all().unwrap();
        drop(output);
        assert_eq!(streamed_digest, digest);
        assert!(streamed_contents.is_empty());
        assert_eq!(std::fs::read(copied).unwrap(), text.as_bytes());
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            source_text(&fixture.engine, relative, &b).unwrap(),
            Some(Vec::new())
        );
        assert_eq!(
            source_text(&fixture.engine, "native/src/absent.rs", &b).unwrap(),
            None
        );
    }
    #[test]
    fn external_source_text_capture_enforces_utf8_and_exact_two_mib_limit() {
        let fixture = Fixture::new();
        let b = budget();
        let relative = "native/src/bounded.rs";
        let path = fixture.engine.join(relative);
        std::fs::write(&path, [0xff, b'x']).unwrap();
        assert_eq!(
            source_text(&fixture.engine, relative, &b).unwrap_err(),
            "source_target_utf8_required"
        );
        let boundary = vec![b'x'; crate::revisions::MAX_FILE_BYTES];
        std::fs::write(&path, &boundary).unwrap();
        assert_eq!(
            source_text(&fixture.engine, relative, &b).unwrap(),
            Some(boundary)
        );
        std::fs::write(&path, vec![b'x'; crate::revisions::MAX_FILE_BYTES + 1]).unwrap();
        assert_eq!(
            source_text(&fixture.engine, relative, &b).unwrap_err(),
            "source_file_byte_limit"
        );
        assert_eq!(
            read_file_with_capture(
                &path,
                MAX_FILE_BYTES,
                &b,
                None,
                None,
                ReadCapture::BoundedBytes
            )
            .unwrap_err(),
            "source_capture_byte_limit"
        );
    }
    #[test]
    fn external_source_private_overlay_preserves_immutable_guards_and_exact_nested_changes() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let before = fixture.originals();
        let mut changes = publication_changes(&fixture);
        changes[2].path = "native/src/nested/deep/new.rs".into();
        let prepared =
            PreparedSourcePublication::prepare(&plan, &RootId::new("engine").unwrap(), changes, &b)
                .unwrap();
        let proof =
            crate::workspace_access::simulated_private_confirmation_for_test(prepared.digest());
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = prepared
            .finish(
                proof,
                &broker,
                std::sync::Arc::new(AtomicBool::new(false)),
                &plan.project.revision,
                &b,
            )
            .unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline).unwrap();
        let proposal = permit.stage(&plan, &envelope, &b).unwrap();
        drop(envelope);
        let target = proposal
            .staged
            .roots
            .iter()
            .find(|root| root.root_id.as_str() == "engine")
            .unwrap();
        assert_eq!(
            std::fs::read(target.staged.join("native/src/lib.rs")).unwrap(),
            b"pub fn proposed() -> i32 { 29 }\n"
        );
        assert!(!target.staged.join("native/src/old.rs").exists());
        assert_eq!(
            std::fs::read(target.staged.join("native/src/nested/deep/new.rs")).unwrap(),
            b"pub fn new_value() -> i32 { 29 }\n"
        );
        for relative in ["native/src", "native/src/nested", "native/src/nested/deep"] {
            let path = target.staged.join(relative);
            let (_, identity, guard) = proposal
                .staged
                .directory_guards
                .0
                .iter()
                .find(|(held, _, _)| *held == path)
                .unwrap();
            crate::snapshot::check_opened_path(guard, &path, b.deadline).unwrap();
            assert_eq!(&file_identity(guard).unwrap(), identity);
            assert!(OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(7)
                .custom_flags(0x0220_0000)
                .open(&path)
                .is_err());
            assert!(std::fs::rename(&path, path.with_file_name("renamed-parent")).is_err());
        }
        check_proposed(&proposal.staged, &b).unwrap();
        let lease = proposal.staged.compilation_lease(&b).unwrap();
        lease.verify(&b).unwrap();
        drop(lease);
        assert_eq!(fixture.originals(), before);
        drop(proposal);
        workspace.clean().unwrap();
    }
    #[test]
    fn external_source_private_overlay_failure_retains_partial_candidate_without_publication() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = publication_plan(&fixture, &b);
        let original = fixture.originals();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let permit = publication_permit(
            &fixture,
            &plan,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &b,
        );
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline).unwrap();
        let staged = plan.stage_owned(&envelope, &b).unwrap();
        drop(envelope);
        let target = staged
            .roots
            .iter()
            .find(|root| root.root_id.as_str() == "engine")
            .unwrap()
            .staged
            .clone();
        std::fs::write(
            target.join("native/src/old.rs"),
            b"private intervening edit\n",
        )
        .unwrap();
        let error = permit.overlay(staged, &b).err().unwrap();
        assert_eq!(error, "source_proposal_overlay_stale");
        assert!(target.is_dir());
        assert_eq!(
            std::fs::read(target.join("native/src/lib.rs")).unwrap(),
            b"pub fn proposed() -> i32 { 29 }\n"
        );
        assert_eq!(
            std::fs::read(target.join("native/src/old.rs")).unwrap(),
            b"private intervening edit\n"
        );
        assert!(!target.join("native/src/new.rs").exists());
        assert_eq!(fixture.originals(), original);
        assert!(!fixture.engine.join(".aurum/revisions/active.json").exists());
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn owner_denial_after_preparation_never_reads_or_mutates_source() {
        use std::os::windows::fs::OpenOptionsExt;
        struct DeniedPrepared<'a> {
            called: &'a AtomicBool,
            expected: &'a Path,
        }
        impl OwnerAuthority for DeniedPrepared<'_> {
            fn approve(&self, request: &ApprovalRequest<'_>) -> Result<bool, String> {
                assert_eq!(request.requested_root, self.expected);
                assert!(request.imported_project.is_absolute());
                assert_eq!(request.mode, Mode::DependencyRead);
                self.called.store(true, Ordering::Release);
                Ok(false)
            }
        }
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let before = fixture.originals();
        let source = fixture.engine.join("native/src/lib.rs");
        let guard = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&source)
            .unwrap();
        assert!(
            std::fs::read(&source).is_err(),
            "exclusive source guard must actually deny reads"
        );
        let called = AtomicBool::new(false);
        let canonical = fixture
            .engine
            .canonicalize()
            .map(crate::project::clean_path)
            .unwrap();
        let authority = DeniedPrepared {
            called: &called,
            expected: &canonical,
        };
        let error = issue_grant(
            &authority,
            &project,
            RootId::new("engine").unwrap(),
            &fixture.engine,
            Mode::DependencyRead,
            &b,
        )
        .unwrap_err();
        assert_eq!(error, "trusted_owner_approval_required");
        assert!(called.load(Ordering::Acquire));
        drop(guard);
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn missing_root_refuses_before_prepared_confirmation() {
        struct UnexpectedConfirmation;
        impl OwnerAuthority for UnexpectedConfirmation {
            fn approve(&self, _: &ApprovalRequest<'_>) -> Result<bool, String> {
                panic!("missing root must refuse before confirmation")
            }
        }
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let before = fixture.originals();
        let missing = fixture.base.join("missing-unapproved-root");
        assert!(issue_grant(
            &UnexpectedConfirmation,
            &project,
            RootId::new("missing").unwrap(),
            &missing,
            Mode::DependencyRead,
            &b
        )
        .is_err());
        assert!(!missing.exists());
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn ungranted_workspace_refuses_without_expanding_project_authority() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let before = fixture.originals();
        let result = StagingPlan::resolve(
            &project,
            &[],
            &[Placement {
                root_id: RootId::new("project").unwrap(),
                slot: "app".into(),
            }],
            Fixture::selection(),
            &project.revision,
            &b,
        );
        assert!(matches!(result,Err(ref e)if e=="ungranted_root"));
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn external_workspace_and_sibling_copy_only_declared_public_roots() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let before = fixture.originals();
        let staged = plan.stage(&fixture.base.join("private"), &b).unwrap();
        assert_eq!(staged.roots.len(), 3);
        assert!(!staged.originals_mutated && !staged.manifest_transformed);
        assert_eq!(
            staged.cargo_manifest,
            staged.envelope.join("engine/Cargo.toml")
        );
        assert_eq!(
            std::fs::read(&staged.cargo_manifest).unwrap(),
            std::fs::read(fixture.engine.join("Cargo.toml")).unwrap()
        );
        assert_eq!(
            std::fs::read(staged.envelope.join("engine/native/Cargo.toml")).unwrap(),
            std::fs::read(fixture.engine.join("native/Cargo.toml")).unwrap()
        );
        assert_eq!(
            std::fs::read(staged.envelope.join("vendor/value-lib/src/lib.rs")).unwrap(),
            std::fs::read(fixture.vendor.join("src/lib.rs")).unwrap()
        );
        for path in [
            "ungranted",
            "vendor/keep",
            "engine/target",
            "app/.godot",
            "app/.env",
        ] {
            assert!(!staged.envelope.join(path).exists());
        }
        for row in &staged.roots {
            let (_, original) = scan(&row.source, &b, &mut Usage::default()).unwrap();
            let (_, copied) = scan(&row.staged, &b, &mut Usage::default()).unwrap();
            assert_eq!(original, row.fingerprint);
            assert_eq!(copied, row.fingerprint);
        }
        assert_eq!(
            staged
                .roots
                .iter()
                .find(|row| row.root_id == RootId::new("vendor").unwrap())
                .unwrap()
                .mode,
            Mode::DependencyRead
        );
        plan.authorize_source_change(
            &RootId::new("engine").unwrap(),
            "native/src/lib.rs",
            &plan.project.revision,
            &b,
        )
        .unwrap();
        assert!(plan
            .authorize_source_change(
                &RootId::new("vendor").unwrap(),
                "src/lib.rs",
                &plan.project.revision,
                &b
            )
            .is_err());
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn dependency_read_grants_cannot_authorize_source_publication() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::DependencyRead, &b);
        let before = fixture.originals();
        assert_eq!(
            plan.authorize_source_change(
                &RootId::new("engine").unwrap(),
                "native/src/lib.rs",
                &plan.project.revision,
                &b
            )
            .unwrap_err(),
            "source_publish_grant_required"
        );
        assert!(plan
            .authorize_source_change(
                &RootId::new("project").unwrap(),
                ".env",
                &plan.project.revision,
                &b
            )
            .is_err());
        assert!(plan
            .authorize_source_change(
                &RootId::new("project").unwrap(),
                ".cargo/config.toml",
                &plan.project.revision,
                &b
            )
            .is_err());
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn grants_are_bound_to_imported_project_and_revision() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let grants = fixture.grants(&project, Mode::SourcePublish, &b);
        assert!(StagingPlan::resolve(
            &project,
            &grants,
            &Fixture::placements(),
            Fixture::selection(),
            &Revision::new(&"2".repeat(64)).unwrap(),
            &b
        )
        .is_err());
        let other = fixture.base.join("other-imported");
        std::fs::create_dir(&other).unwrap();
        let imported = ImportedProject::bind(&other, project.revision.clone(), &b).unwrap();
        assert!(StagingPlan::resolve(
            &imported,
            &grants,
            &Fixture::placements(),
            Fixture::selection(),
            &imported.revision,
            &b
        )
        .is_err());
    }
    #[test]
    fn aliases_overlapping_roots_and_ambiguous_slots_refuse() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let mut grants = fixture.grants(&project, Mode::SourcePublish, &b);
        grants.push(
            issue_grant(
                &TestOwnerAuthority,
                &project,
                RootId::new("alias").unwrap(),
                &fixture.engine,
                Mode::DependencyRead,
                &b,
            )
            .unwrap(),
        );
        let mut placements = Fixture::placements();
        placements.push(Placement {
            root_id: RootId::new("alias").unwrap(),
            slot: "alias".into(),
        });
        assert!(StagingPlan::resolve(
            &project,
            &grants,
            &placements,
            Fixture::selection(),
            &project.revision,
            &b
        )
        .is_err());
        grants.pop();
        placements.pop();
        grants.push(
            issue_grant(
                &TestOwnerAuthority,
                &project,
                RootId::new("nested").unwrap(),
                &fixture.engine.join("native"),
                Mode::DependencyRead,
                &b,
            )
            .unwrap(),
        );
        placements.push(Placement {
            root_id: RootId::new("nested").unwrap(),
            slot: "nested".into(),
        });
        assert!(StagingPlan::resolve(
            &project,
            &grants,
            &placements,
            Fixture::selection(),
            &project.revision,
            &b
        )
        .is_err());
        grants.pop();
        placements.pop();
        placements[2].slot = "ENGINE".into();
        assert!(StagingPlan::resolve(
            &project,
            &grants,
            &placements,
            Fixture::selection(),
            &project.revision,
            &b
        )
        .is_err());
        assert!(issue_grant(
            &TestOwnerAuthority,
            &project,
            RootId::new("inside").unwrap(),
            &fixture.app,
            Mode::DependencyRead,
            &b
        )
        .is_err());
    }
    #[test]
    fn directory_replacement_is_refused_before_private_writes() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let replacement = fixture.base.join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        let old = fixture.base.join("original-engine");
        assert!(old.starts_with(&fixture.base) && replacement.starts_with(&fixture.base));
        std::fs::rename(&fixture.engine, &old).unwrap();
        std::fs::rename(&replacement, &fixture.engine).unwrap();
        let private = fixture.base.join("private");
        assert_eq!(
            plan.stage(&private, &b).unwrap_err(),
            "granted_root_identity_changed"
        );
        assert!(!private.exists());
        assert!(old.join("Cargo.lock").is_file());
    }
    #[test]
    fn resolved_manifest_drift_refuses_before_copy_and_preserves_foreign_edit() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let changed = Fixture::native_manifest("../../ungranted");
        std::fs::write(fixture.engine.join("native/Cargo.toml"), &changed).unwrap();
        let private = fixture.base.join("private");
        assert_eq!(
            plan.stage(&private, &b).unwrap_err(),
            "resolved_manifest_changed_before_staging"
        );
        assert!(!private.exists());
        assert_eq!(
            std::fs::read_to_string(fixture.engine.join("native/Cargo.toml")).unwrap(),
            changed
        );
    }
    #[test]
    fn relative_layout_mismatch_and_absolute_dependency_refuse_without_rewrite() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let grants = fixture.grants(&project, Mode::SourcePublish, &b);
        let before = fixture.originals();
        let mut placements = Fixture::placements();
        placements[2].slot = "different/value-lib".into();
        assert!(
            matches!(StagingPlan::resolve(&project,&grants,&placements,Fixture::selection(),&project.revision,&b),Err(ref e)if e=="relative_layout_mismatch")
        );
        assert_eq!(fixture.originals(), before);
        std::fs::write(
            fixture.engine.join("native/Cargo.toml"),
            Fixture::native_manifest("A:/ungranted"),
        )
        .unwrap();
        assert!(StagingPlan::resolve(
            &project,
            &grants,
            &Fixture::placements(),
            Fixture::selection(),
            &project.revision,
            &b
        )
        .is_err());
    }
    #[test]
    fn closed_cargo_subset_refuses_unknown_inline_inherited_and_config_inputs() {
        for addition in [
            "\n[patch.crates-io]\n",
            "\n[workspace.package]\nversion=\"1.0.0\"\n",
            "\n[dependencies]\nvalue-lib={path=\"../value-lib\"}\n",
            "\n[target.fake.dependencies.value-lib]\npath=\"../value-lib\"\n",
            "\n[package.metadata]\nunknown=true\n",
        ] {
            assert!(cargo_shape(
                (Fixture::native_manifest("../../vendor/value-lib") + addition).as_bytes()
            )
            .is_err());
        }
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        std::fs::create_dir(fixture.engine.join(".cargo")).unwrap();
        std::fs::write(
            fixture.engine.join(".cargo/config.toml"),
            "[build]\ntarget-dir=\"unsafe\"\n",
        )
        .unwrap();
        assert_eq!(
            plan.stage(&fixture.base.join("private"), &b).unwrap_err(),
            "unsupported_cargo_configuration_or_build_script"
        );
        assert!(!fixture.base.join("private").exists());
        let other = Fixture::new();
        let plan = other.plan(Mode::SourcePublish, &b);
        std::fs::write(other.engine.join("native/build.rs"), "fn main() {}\n").unwrap();
        assert_eq!(
            plan.stage(&other.base.join("private"), &b).unwrap_err(),
            "unsupported_cargo_configuration_or_build_script"
        );
        assert!(!other.base.join("private").exists());
    }
    #[test]
    fn ungranted_dependency_and_extra_unresolved_manifest_refuse() {
        let fixture = Fixture::new();
        let b = budget();
        std::fs::write(
            fixture.engine.join("native/Cargo.toml"),
            Fixture::native_manifest("../../ungranted"),
        )
        .unwrap();
        let project = fixture.imported(&b);
        let grants = fixture.grants(&project, Mode::SourcePublish, &b);
        assert!(
            matches!(StagingPlan::resolve(&project,&grants,&Fixture::placements(),Fixture::selection(),&project.revision,&b),Err(ref e)if e=="ungranted_path_dependency")
        );
        let other = Fixture::new();
        let plan = other.plan(Mode::SourcePublish, &b);
        std::fs::create_dir_all(other.engine.join("unknown/src")).unwrap();
        std::fs::write(
            other.engine.join("unknown/Cargo.toml"),
            "[package]\nname=\"unknown\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
        )
        .unwrap();
        std::fs::write(other.engine.join("unknown/src/lib.rs"), "pub fn x() {}\n").unwrap();
        assert_eq!(
            plan.stage(&other.base.join("private"), &b).unwrap_err(),
            "unresolved_extra_cargo_manifest"
        );
        assert!(!other.base.join("private").exists());
    }
    #[test]
    fn unused_granted_root_is_not_copied_and_stage_must_be_fresh() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let mut grants = fixture.grants(&project, Mode::SourcePublish, &b);
        grants.push(
            issue_grant(
                &TestOwnerAuthority,
                &project,
                RootId::new("unused").unwrap(),
                &fixture.base.join("ungranted"),
                Mode::DependencyRead,
                &b,
            )
            .unwrap(),
        );
        let mut placements = Fixture::placements();
        placements.push(Placement {
            root_id: RootId::new("unused").unwrap(),
            slot: "unused".into(),
        });
        let plan = StagingPlan::resolve(
            &project,
            &grants,
            &placements,
            Fixture::selection(),
            &project.revision,
            &b,
        )
        .unwrap();
        let private = fixture.base.join("private");
        let staged = plan.stage(&private, &b).unwrap();
        assert_eq!(staged.roots.len(), 3);
        assert!(!private.join("unused").exists());
        assert_eq!(
            plan.stage(&private, &b).unwrap_err(),
            "private_stage_must_be_fresh"
        );
        assert!(plan
            .stage(&fixture.engine.join("unsafe-private"), &b)
            .is_err());
        assert!(!fixture.engine.join("unsafe-private").exists());
    }
    #[test]
    fn byte_and_entry_budgets_refuse_before_any_private_write() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let before = fixture.originals();
        let mut small = budget();
        small.max_bytes = 1;
        assert_eq!(
            plan.stage(&fixture.base.join("private-bytes"), &small)
                .unwrap_err(),
            "external_namespace_aggregate_byte_limit"
        );
        assert!(!fixture.base.join("private-bytes").exists());
        let mut small = budget();
        small.max_entries = 1;
        assert_eq!(
            plan.stage(&fixture.base.join("private-entries"), &small)
                .unwrap_err(),
            "external_namespace_entry_limit"
        );
        assert!(!fixture.base.join("private-entries").exists());
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn cancellation_and_source_drift_retain_only_owned_partial_stage() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let before = fixture.originals();
        let destination = fixture.base.join("cancelled-private");
        let stop = || {
            if destination.join("app/main.gd").exists() {
                Err("fixture_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cancelled = Budget::new(Duration::from_secs(30), &stop).unwrap();
        assert_eq!(
            plan.stage(&destination, &cancelled).unwrap_err(),
            "fixture_cancelled"
        );
        assert!(destination.exists());
        assert_eq!(fixture.originals(), before);
        let other = Fixture::new();
        let plan = other.plan(Mode::SourcePublish, &b);
        let destination = other.base.join("stale-private");
        let changed = AtomicBool::new(false);
        let edit = || {
            if destination.join("app/main.gd").exists() && !changed.swap(true, Ordering::SeqCst) {
                std::fs::write(other.app.join("main.gd"), "extends Node\n# altered!\n").unwrap();
            }
            Ok(())
        };
        let budget = Budget::new(Duration::from_secs(30), &edit).unwrap();
        assert!(plan.stage(&destination, &budget).is_err());
        assert!(destination.exists());
        assert_eq!(
            std::fs::read_to_string(other.app.join("main.gd")).unwrap(),
            "extends Node\n# altered!\n"
        );
    }
    #[test]
    fn ordered_source_leases_contend_without_source_or_stage_mutation() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let before = fixture.originals();
        let writer = ContentLock::exclusive(&fixture.vendor).unwrap();
        assert!(plan.stage(&fixture.base.join("private"), &b).is_err());
        assert!(!fixture.base.join("private").exists());
        assert_eq!(fixture.originals(), before);
        drop(writer);
        assert!(plan.stage(&fixture.base.join("after-release"), &b).is_ok());
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    fn create_owned_test_junction(base: &Path, link: &Path, outside: &Path) -> Result<(), u32> {
        set_owned_test_junction(base, link, outside, true)
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    fn set_owned_test_junction(
        base: &Path,
        link: &Path,
        outside: &Path,
        create: bool,
    ) -> Result<(), u32> {
        use std::os::windows::{
            fs::MetadataExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        };
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateFileW(
                path: *const u16,
                access: u32,
                share: u32,
                security: *mut std::ffi::c_void,
                disposition: u32,
                flags: u32,
                template: *mut std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            fn DeviceIoControl(
                handle: *mut std::ffi::c_void,
                code: u32,
                input: *mut std::ffi::c_void,
                input_bytes: u32,
                output: *mut std::ffi::c_void,
                output_bytes: u32,
                returned: *mut u32,
                overlapped: *mut std::ffi::c_void,
            ) -> i32;
        }
        let error = || std::io::Error::last_os_error().raw_os_error().unwrap_or(1) as u32;
        let source_alias = create
            && link == base.join("engine/native/src/alias")
            && outside == base.join("ungranted");
        let source_role_probe = !create
            && outside == base.join("ungranted")
            && (link == base.join("engine/native")
                || link == base.join("engine/native/src/empty-direct"));
        let allowed = (link == base.join("linked-engine") && outside == base.join("engine"))
            || (link == base.join("engine/native/linked-source")
                && outside == base.join("ungranted"))
            || (link == base.join("private/app") && outside == base.join("outside-stage-target"))
            || source_alias
            || source_role_probe;
        if !allowed {
            return Err(87);
        }
        if !create
            && (link != base.join("private/app") || outside != base.join("outside-stage-target"))
            && !source_role_probe
        {
            return Err(87);
        }
        // Only a fixture directly below this run's canonical A TEMP may be
        // modified. No durable-prefix assumption and no general A-root grant.
        let canonical_base = base
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|_| 123u32)?;
        let canonical_temp = std::env::temp_dir()
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|_| 123u32)?;
        let leaf = canonical_base
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(123u32)?;
        let id = leaf
            .strip_prefix("external-workspace-fixture-")
            .ok_or(87u32)?;
        if canonical_base.parent() != Some(canonical_temp.as_path())
            || id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !canonical_temp
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with(&crate::test_drive())
            || canonical_base.to_string_lossy().len() > 1024
        {
            return Err(123);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let _base_guard =
            crate::snapshot::open_checked_directory(base, deadline).map_err(|_| 123u32)?;
        let _outside_guard =
            crate::snapshot::open_checked_directory(outside, deadline).map_err(|_| 123u32)?;
        if create {
            std::fs::create_dir(link)
                .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        } else if !std::fs::symlink_metadata(link)
            .map_err(|_| 123u32)?
            .is_dir()
        {
            return Err(87);
        }
        let path = link.to_string_lossy().replace('/', "\\");
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateFileW(
                path.as_ptr(),
                0x40000080,
                if source_role_probe { 3 } else { 0 },
                std::ptr::null_mut(),
                3,
                0x02200000,
                std::ptr::null_mut(),
            )
        };
        if raw.is_null() || raw as isize == -1 {
            return Err(error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        // Mount-point ABI: tag/length/reserved (8 bytes), four name-offset/
        // length u16 fields (8 bytes), then two terminated UTF-16 names.
        let printed = outside.to_string_lossy().replace('/', "\\");
        let printed = printed.strip_prefix(r"\\?\").unwrap_or(&printed);
        if printed.len() > 1024 || printed.chars().any(char::is_control) {
            return Err(123);
        }
        let substitute: Vec<u16> = format!(r"\??\{printed}").encode_utf16().collect();
        let print_name: Vec<u16> = printed.encode_utf16().collect();
        let names = (substitute.len() + 1 + print_name.len() + 1) * 2;
        let data_length = 8 + names;
        if 8 + data_length > 16 * 1024 || data_length > u16::MAX as usize {
            return Err(87);
        }
        let mut buffer = vec![0u8; 8 + data_length];
        buffer[..4].copy_from_slice(&0xa0000003u32.to_le_bytes());
        buffer[4..6].copy_from_slice(&(data_length as u16).to_le_bytes());
        buffer[10..12].copy_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
        buffer[12..14].copy_from_slice(&(((substitute.len() + 1) * 2) as u16).to_le_bytes());
        buffer[14..16].copy_from_slice(&((print_name.len() * 2) as u16).to_le_bytes());
        for (index, unit) in substitute
            .iter()
            .chain(std::iter::once(&0))
            .chain(print_name.iter())
            .chain(std::iter::once(&0))
            .enumerate()
        {
            buffer[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let mut returned = 0u32;
        if unsafe {
            DeviceIoControl(
                handle.as_raw_handle(),
                0x000900a4,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(error());
        }
        drop(handle);
        let metadata = std::fs::symlink_metadata(link)
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        if metadata.file_attributes() & 0x410 != 0x410 {
            return Err(4390);
        }
        let resolved = std::fs::read_link(link)
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        if resolved.as_os_str().len() > 1024 {
            return Err(123);
        }
        let resolved = resolved
            .canonicalize()
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        let expected = outside
            .canonicalize()
            .map_err(|failure| failure.raw_os_error().unwrap_or(1) as u32)?;
        if resolved != expected {
            return Err(4390);
        }
        Ok(())
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn windows_junction_roots_and_source_children_are_refused() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let alias = fixture.base.join("linked-engine");
        create_owned_test_junction(&fixture.base, &alias, &fixture.engine).unwrap();
        assert!(issue_grant(
            &TestOwnerAuthority,
            &project,
            RootId::new("alias").unwrap(),
            &alias,
            Mode::DependencyRead,
            &b
        )
        .is_err());
        std::fs::remove_dir(&alias).unwrap();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let child = fixture.engine.join("native/linked-source");
        create_owned_test_junction(&fixture.base, &child, &fixture.base.join("ungranted")).unwrap();
        assert!(plan.stage(&fixture.base.join("private"), &b).is_err());
        assert!(!fixture.base.join("private").exists());
        std::fs::remove_dir(&child).unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.base.join("ungranted/keep")).unwrap(),
            "ungranted sibling must never be copied\n"
        );
    }
    #[cfg(all(windows, target_pointer_width = "64"))]
    #[test]
    fn held_destination_ancestors_refuse_replacement_and_junction_before_writes() {
        let fixture = Fixture::new();
        let b = budget();
        let plan = fixture.plan(Mode::SourcePublish, &b);
        let before = fixture.originals();
        let outside = fixture.base.join("outside-stage-target");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), "outside remains exact\n").unwrap();
        let destination = fixture.base.join("private");
        let app = destination.join("app");
        let moved = destination.join("app-displaced");
        let attempted = AtomicBool::new(false);
        let attack = || {
            if app.is_dir() && !attempted.swap(true, Ordering::SeqCst) {
                assert!(app.starts_with(&fixture.base) && moved.starts_with(&fixture.base));
                let rename = std::fs::rename(&app, &moved);
                assert!(
                    rename.is_err(),
                    "Held destination ancestor must refuse namespace replacement"
                );
                assert!(matches!(rename.unwrap_err().raw_os_error(), Some(5 | 32)));
                let redirect =
                    set_owned_test_junction(&fixture.base, &app, &outside, false).unwrap_err();
                assert!(
                    matches!(redirect, 5 | 32),
                    "Held destination ancestor must refuse direct reparse write"
                );
            }
            Ok(())
        };
        let guarded = Budget::new(Duration::from_secs(30), &attack).unwrap();
        let staged = plan.stage(&destination, &guarded).unwrap();
        assert!(attempted.load(Ordering::SeqCst));
        assert!(!moved.exists());
        assert_eq!(staged.roots.len(), 3);
        assert_eq!(
            std::fs::read_to_string(outside.join("sentinel")).unwrap(),
            "outside remains exact\n"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "No outside file or directory creation"
        );
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn prepared_owner_identity_is_held_before_confirmation_and_revision_drift_refuses() {
        let fixture = Fixture::new();
        let before = fixture.originals();
        let b = budget();
        let project = fixture.imported(&b);
        let prepared = PreparedGrant::dependency_read(
            &project,
            RootId::new("owner_read").unwrap(),
            &fixture.engine,
            &b,
        )
        .unwrap();
        let digest = prepared.digest();
        let rename = std::fs::rename(&fixture.engine, fixture.base.join("replaced-engine"));
        assert!(matches!(rename.unwrap_err().raw_os_error(), Some(5 | 32)));
        assert_eq!(prepared.digest(), digest);
        assert!(prepared
            .recheck(&Revision::new(&"2".repeat(64)).unwrap(), &b)
            .is_err());
        let proof = crate::workspace_access::simulated_private_confirmation_for_test(&digest);
        let grant = prepared.finish(&proof, &project.revision, &b).unwrap();
        assert_eq!(
            grant.root,
            fixture
                .engine
                .canonicalize()
                .map(crate::project::clean_path)
                .unwrap()
        );
        assert!(grant.is_dependency_read());
        assert!(!format!("{grant:?}").contains("DirectoryIdentity"));
        assert_eq!(fixture.originals(), before);
    }
    #[test]
    fn prepared_wrong_identity_confirmation_and_source_publish_never_issue() {
        let fixture = Fixture::new();
        let b = budget();
        let project = fixture.imported(&b);
        let prepared = PreparedGrant::dependency_read(
            &project,
            RootId::new("read").unwrap(),
            &fixture.engine,
            &b,
        )
        .unwrap();
        let wrong = crate::workspace_access::simulated_private_confirmation_for_test(
            "different_prepared_identity",
        );
        assert_eq!(
            prepared.finish(&wrong, &project.revision, &b).unwrap_err(),
            "owner_confirmation_binding_mismatch"
        );
        let publish = PreparedGrant::prepare(
            &project,
            RootId::new("publish").unwrap(),
            &fixture.engine,
            Mode::SourcePublish,
            &b,
        )
        .unwrap();
        let proof =
            crate::workspace_access::simulated_private_confirmation_for_test(&publish.digest());
        assert_eq!(
            publish.finish(&proof, &project.revision, &b).unwrap_err(),
            "external_source_publish_unimplemented"
        );
    }
    #[test]
    fn session_read_grants_stage_in_recognized_workspace_without_original_mutation() {
        // Protocol/authority is explicitly simulated here; actual attendance and
        // actual CLI/HTTP/MCP transport qualification remain separate held gates.
        let fixture = Fixture::new();
        let before = fixture.originals();
        let b = budget();
        let revision = Revision::new(&current_revision(&fixture.app, &b).unwrap()).unwrap();
        let project = ImportedProject::bind(&fixture.app, revision.clone(), &b).unwrap();
        let broker = crate::workspace_access::Broker::default();
        let engine_id = "a".repeat(32);
        let vendor_id = "b".repeat(32);
        let mut grants = Vec::new();
        for (id, path) in [(&engine_id, &fixture.engine), (&vendor_id, &fixture.vendor)] {
            let prepared =
                PreparedGrant::dependency_read(&project, RootId::new(id).unwrap(), path, &b)
                    .unwrap();
            let proof = crate::workspace_access::simulated_private_confirmation_for_test(
                &prepared.digest(),
            );
            grants.push((id.clone(), prepared.finish(&proof, &revision, &b).unwrap()));
        }
        broker.insert_simulated_grants_for_test(grants);
        let request = serde_json::json!({"op":"external_workspace_validate","root_ids":[engine_id,vendor_id],"placements":[
            {"root_id":"project","slot":"app"},{"root_id":engine_id,"slot":"engine"},{"root_id":vendor_id,"slot":"vendor/value-lib"}],
            "workspace":{"root_id":engine_id,"path":"Cargo.toml"},"member":{"root_id":engine_id,"path":"native/Cargo.toml"}});
        let result = broker.validate(&fixture.app, &request).unwrap();
        assert_eq!(result["validation"], "confined_source_staging");
        assert_eq!(result["roots"].as_array().unwrap().len(), 3);
        for field in [
            "cargo_executed",
            "native_artifact_produced",
            "originals_mutated",
            "manifest_transformed",
            "source_publish",
        ] {
            assert_eq!(result[field], false);
        }
        assert_eq!(result["candidate_lease_owned"], true);
        assert_eq!(result["private_workspace_cleaned"], true);
        assert_eq!(result["cargo_manifest"], "engine/Cargo.toml");
        assert_eq!(fixture.originals(), before);
        std::fs::write(
            fixture.app.join("new-source.gd"),
            "new public source after owner approval\n",
        )
        .unwrap();
        let revised = fixture.originals();
        assert!(broker.validate(&fixture.app, &request).is_err());
        assert_eq!(fixture.originals(), revised);
        broker.close();
        assert!(broker.validate(&fixture.app, &request).is_err());
    }

    #[cfg(windows)]
    fn private_copy_test_writer(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(0x0020_0000)
            .share_mode(1)
            .open(path)
            .unwrap()
    }
    #[cfg(windows)]
    #[test]
    fn private_artifact_copy_uses_same_writer_and_restores_original_cursor() {
        let fixture = Fixture::new();
        let b = budget();
        let bytes = vec![0x53; 2 * 65536 + 19];
        let source_path = fixture.base.join("private-copy-source.dll");
        std::fs::write(&source_path, &bytes).unwrap();
        let source = open_compile_source(&source_path, &b).unwrap();
        std::io::Seek::seek(&mut &source, std::io::SeekFrom::Start(17)).unwrap();
        let before = stamp(&source).unwrap();
        let output = fixture.base.join("private-copy-output.dll");
        let mut writer = private_copy_test_writer(&output);
        let sha256 = crate::sha256_hex(&bytes);
        let expected = copy_checked_private_artifact(
            &source_path,
            &source,
            &before,
            &sha256,
            &output,
            &mut writer,
            &b,
        )
        .unwrap();
        assert_eq!(expected.bytes, bytes.len() as u64);
        assert_ne!(expected.identity, before.identity);
        assert_eq!(std::io::Seek::stream_position(&mut &source).unwrap(), 17);
        check_private_artifact_copy(&output, &writer, &expected, &sha256, &b).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        assert!(OpenOptions::new().write(true).open(&output).is_err());
        assert!(std::fs::remove_file(&output).is_err());
        assert_eq!(stamp(&source).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn private_artifact_copy_cancel_and_expiry_restore_cursor_and_retain_partial_bytes() {
        let fixture = Fixture::new();
        let b = budget();
        let bytes = vec![0x61; 2 * 65536 + 11];
        let source_path = fixture.base.join("private-copy-source.dll");
        std::fs::write(&source_path, &bytes).unwrap();
        let source = open_compile_source(&source_path, &b).unwrap();
        std::io::Seek::seek(&mut &source, std::io::SeekFrom::Start(13)).unwrap();
        let before = stamp(&source).unwrap();
        let output = fixture.base.join("private-copy-cancelled.dll");
        let mut writer = private_copy_test_writer(&output);
        let cancel = || {
            if std::fs::metadata(&output).unwrap().len() >= 65536 {
                Err("private_copy_test_cancelled".into())
            } else {
                Ok(())
            }
        };
        let cancelled = b.with_check(&cancel);
        assert_eq!(
            copy_checked_private_artifact(
                &source_path,
                &source,
                &before,
                &crate::sha256_hex(&bytes),
                &output,
                &mut writer,
                &cancelled
            )
            .unwrap_err(),
            "private_copy_test_cancelled"
        );
        assert_eq!(writer.metadata().unwrap().len(), 65536);
        assert_eq!(std::fs::read(&output).unwrap(), bytes[..65536]);
        assert_eq!(std::io::Seek::stream_position(&mut &source).unwrap(), 13);
        assert_eq!(stamp(&source).unwrap(), before);
        let expired_path = fixture.base.join("private-copy-expired.dll");
        let mut expired_writer = private_copy_test_writer(&expired_path);
        let mut expired = b.with_check(b.check_cancelled);
        expired.deadline = Instant::now();
        assert_eq!(
            copy_checked_private_artifact(
                &source_path,
                &source,
                &before,
                &crate::sha256_hex(&bytes),
                &expired_path,
                &mut expired_writer,
                &expired
            )
            .unwrap_err(),
            "external_staging_deadline"
        );
        assert_eq!(expired_writer.metadata().unwrap().len(), 0);
        assert_eq!(std::io::Seek::stream_position(&mut &source).unwrap(), 13);
    }
    #[cfg(windows)]
    #[test]
    fn private_artifact_copy_rejects_source_binding_and_retained_writer_tampering() {
        let fixture = Fixture::new();
        let b = budget();
        let bytes = b"checked private DLL data only";
        let source_path = fixture.base.join("private-copy-source.dll");
        std::fs::write(&source_path, bytes).unwrap();
        let source = open_compile_source(&source_path, &b).unwrap();
        std::io::Seek::seek(&mut &source, std::io::SeekFrom::Start(3)).unwrap();
        let before = stamp(&source).unwrap();
        let refused = fixture.base.join("private-copy-wrong-digest.dll");
        let mut partial = private_copy_test_writer(&refused);
        assert_eq!(
            copy_checked_private_artifact(
                &source_path,
                &source,
                &before,
                &"0".repeat(64),
                &refused,
                &mut partial,
                &b
            )
            .unwrap_err(),
            "external_private_artifact_input_changed"
        );
        assert_eq!(partial.metadata().unwrap().len(), bytes.len() as u64);
        assert_eq!(std::io::Seek::stream_position(&mut &source).unwrap(), 3);
        let output = fixture.base.join("private-copy-output.dll");
        let mut writer = private_copy_test_writer(&output);
        let mut wrong_identity = before.clone();
        wrong_identity.identity.volume ^= 1;
        assert_eq!(
            copy_checked_private_artifact(
                &source_path,
                &source,
                &wrong_identity,
                &crate::sha256_hex(bytes),
                &output,
                &mut writer,
                &b
            )
            .unwrap_err(),
            "external_private_artifact_input_changed"
        );
        assert_eq!(writer.metadata().unwrap().len(), 0);
        let expected = copy_checked_private_artifact(
            &source_path,
            &source,
            &before,
            &crate::sha256_hex(bytes),
            &output,
            &mut writer,
            &b,
        )
        .unwrap();
        std::io::Seek::seek(&mut writer, std::io::SeekFrom::Start(0)).unwrap();
        writer.write_all(b"!").unwrap();
        writer.flush().unwrap();
        writer.sync_all().unwrap();
        assert_eq!(
            check_private_artifact_copy(&output, &writer, &expected, &crate::sha256_hex(bytes), &b)
                .unwrap_err(),
            "external_private_artifact_changed"
        );
        assert_eq!(stamp(&source).unwrap(), before);
        assert_eq!(std::io::Seek::stream_position(&mut &source).unwrap(), 3);
        assert_eq!(std::fs::read(&source_path).unwrap(), bytes);
    }
    #[cfg(windows)]
    #[test]
    fn private_artifact_admission_checks_file_operation_and_workspace_limits_before_create() {
        let mut b = budget();
        b.max_file = 17;
        b.max_bytes = 17;
        let maximum = crate::candidate_workspace::WORKSPACE_BYTES;
        private_artifact_admission(17, maximum - 17, &b).unwrap();
        for (bytes, used) in [(0, 0), (18, 0), (17, maximum - 16), (17, u64::MAX)] {
            assert_eq!(
                private_artifact_admission(bytes, used, &b).unwrap_err(),
                "external_private_artifact_storage_admission_refused"
            );
        }
        b.max_bytes = 16;
        assert!(private_artifact_admission(17, 0, &b).is_err());
        b.max_bytes = 17;
        b.max_files = 0;
        assert!(private_artifact_admission(17, 0, &b).is_err());
        b.max_files = 1;
        b.max_entries = 0;
        assert!(private_artifact_admission(17, 0, &b).is_err());
    }

    #[cfg(windows)]
    fn custody_synthetic_refusal_fixture(
        fixture: &Fixture,
        broker: &std::sync::Arc<crate::workspace_access::Broker>,
        cancelled: std::sync::Arc<AtomicBool>,
        parent: &mut crate::external_native::ParentDeadline,
        controller: &crate::worker_windows::StreamController,
        b: &Budget<'_>,
    ) -> (
        PreparedSourcePublication,
        ValidatedSourceCandidate,
        crate::candidate_workspace::Workspace,
    ) {
        let plan = seeded_publication_plan(fixture, b);
        let mut prepared = PreparedSourcePublication::prepare(
            &plan,
            &RootId::new("engine").unwrap(),
            publication_changes(fixture),
            b,
        )
        .unwrap();
        controller.bind_parent(broker).unwrap();
        let mut workspace =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let envelope = workspace.source_envelope(b.deadline()).unwrap();
        let proposal = prepared
            .stage_for_native_validation(&plan, &envelope, parent, broker, cancelled, b)
            .unwrap();
        drop(envelope);
        // Move the sole lease into Workspace; this helper does not clone its Arc.
        workspace
            .retain_compilation(std::sync::Arc::new(
                proposal.staged.compilation_lease(b).unwrap(),
            ))
            .unwrap();
        let ownership = workspace.retire_compilation_for_publication(b).unwrap();
        let artifact = workspace.path.join("custody-synthetic-refusal.dll");
        std::fs::write(&artifact, b"synthetic refusal fixture only\n").unwrap();
        let candidate = ValidatedSourceCandidate {
            proposal,
            ownership,
            workspace: workspace.path.clone(),
            artifact_hash: crate::sha256_file(&artifact).unwrap(),
            artifact_guard: open_compile_source(&artifact, b).unwrap(),
            artifact,
            native_validation_tag: Some("synthetic-cannot-prove-custody".into()),
            compiler_provenance: CandidateCompilerProvenance::SyntheticAuthorityOnly,
        };
        (prepared, candidate, workspace)
    }
    #[cfg(windows)]
    fn custody_assert_returned_inputs(
        candidate: &ValidatedSourceCandidate,
        workspace: &crate::candidate_workspace::Workspace,
        handle: usize,
        workspace_path: &Path,
    ) {
        assert_eq!(
            std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard) as usize,
            handle
        );
        assert_eq!(workspace.path, workspace_path);
        assert!(workspace.path.join("workspace.lock").is_file());
        assert!(candidate.artifact_guard.metadata().unwrap().is_file());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_artifact_custody_wrong_parent_controller_and_owner_return_inputs() {
        let fixture = Fixture::new();
        seed_publication_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let mut parent = crate::external_native::ParentDeadline::at(b.deadline()).unwrap();
        let controller = crate::worker_windows::StreamController::for_runtime();
        let (prepared, candidate, workspace) = custody_synthetic_refusal_fixture(
            &fixture,
            &broker,
            cancelled.clone(),
            &mut parent,
            &controller,
            &b,
        );
        let handle =
            std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard) as usize;
        let workspace_path = workspace.path.clone();
        let foreign =
            crate::candidate_workspace::Workspace::for_test_project(&fixture.app).unwrap();
        let foreign_path = foreign.path.clone();
        let (error, candidate, foreign) = candidate
            .into_artifact_custody(foreign, &parent, &controller, &b)
            .err()
            .unwrap();
        assert_eq!(error, "external_artifact_custody_parent_or_seal_mismatch");
        custody_assert_returned_inputs(&candidate, &foreign, handle, &foreign_path);
        drop(foreign);
        let mut other_parent =
            crate::external_native::ParentDeadline::at(parent.instant()).unwrap();
        other_parent
            .bind_read_owner(&broker, cancelled, ProposalLimits::from_budget(&b))
            .unwrap();
        let (error, candidate, workspace) = candidate
            .into_artifact_custody(workspace, &other_parent, &controller, &b)
            .err()
            .unwrap();
        assert_eq!(error, "external_artifact_custody_parent_or_seal_mismatch");
        custody_assert_returned_inputs(&candidate, &workspace, handle, &workspace_path);
        let other_controller = crate::worker_windows::StreamController::for_runtime();
        let (error, candidate, workspace) = candidate
            .into_artifact_custody(workspace, &parent, &other_controller, &b)
            .err()
            .unwrap();
        assert_eq!(error, "external_compile_read_controller_binding_mismatch");
        custody_assert_returned_inputs(&candidate, &workspace, handle, &workspace_path);
        drop(candidate);
        drop(workspace);
        drop(prepared);
        assert!(!workspace_path.exists());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_artifact_custody_synthetic_proof_and_wider_limits_are_refused() {
        let fixture = Fixture::new();
        seed_publication_fixture(&fixture);
        let b = budget();
        let before = fixture_oracle(&fixture, &b).unwrap();
        let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
        let mut parent = crate::external_native::ParentDeadline::at(b.deadline()).unwrap();
        let controller = crate::worker_windows::StreamController::for_runtime();
        let (prepared, candidate, workspace) = custody_synthetic_refusal_fixture(
            &fixture,
            &broker,
            std::sync::Arc::new(AtomicBool::new(false)),
            &mut parent,
            &controller,
            &b,
        );
        let handle =
            std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard) as usize;
        let path = workspace.path.clone();
        let mut wider = b.with_check(b.check_cancelled);
        wider.max_files += 1;
        let (error, candidate, workspace) = candidate
            .into_artifact_custody(workspace, &parent, &controller, &wider)
            .err()
            .unwrap();
        assert_eq!(error, "external_source_original_proposal_limits_widened");
        custody_assert_returned_inputs(&candidate, &workspace, handle, &path);
        let (error, candidate, workspace) = candidate
            .into_artifact_custody(workspace, &parent, &controller, &b)
            .err()
            .unwrap();
        assert_eq!(error, "external_source_synthetic_provenance_refused");
        custody_assert_returned_inputs(&candidate, &workspace, handle, &path);
        drop(candidate);
        drop(workspace);
        drop(prepared);
        assert!(!path.exists());
        assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn external_source_artifact_custody_original_cancel_and_expiry_return_inputs() {
        for cancel in [false, true] {
            let fixture = Fixture::new();
            seed_publication_fixture(&fixture);
            let b = budget();
            let before = fixture_oracle(&fixture, &b).unwrap();
            let broker = std::sync::Arc::new(crate::workspace_access::Broker::default());
            let cancelled = std::sync::Arc::new(AtomicBool::new(false));
            let mut parent = crate::external_native::ParentDeadline::at(b.deadline()).unwrap();
            let controller = crate::worker_windows::StreamController::for_runtime();
            let (prepared, candidate, workspace) = custody_synthetic_refusal_fixture(
                &fixture,
                &broker,
                cancelled.clone(),
                &mut parent,
                &controller,
                &b,
            );
            let handle = std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard)
                as usize;
            let path = workspace.path.clone();
            // A narrowed expired copy is refusal-only; it is never renewed or accepted.
            let mut expired = b.with_check(b.check_cancelled);
            expired.deadline = Instant::now() - Duration::from_millis(1);
            if cancel {
                cancelled.store(true, Ordering::Release);
            }
            let observed = if cancel {
                candidate.into_artifact_custody(workspace, &parent, &controller, &b)
            } else {
                candidate.into_artifact_custody(workspace, &parent, &controller, &expired)
            };
            let (error, candidate, workspace) = observed.err().unwrap();
            assert_eq!(
                error,
                if cancel {
                    "external_compile_read_owner_cancelled_or_closed"
                } else {
                    "external_staging_deadline"
                }
            );
            custody_assert_returned_inputs(&candidate, &workspace, handle, &path);
            drop(candidate);
            drop(workspace);
            drop(prepared);
            assert!(!path.exists());
            // Same original oracle budget; no expired or cancelled candidate is accepted.
            assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires separately admitted terminal custody producer and exact private TEMP"]
    fn native_artifact_custody_real_compile_first_owner_gate() {
        artifact_custody_real_compile_fixture(false)
    }
    #[cfg(windows)]
    fn public_configfile_fixture_request(
        fixture: &Fixture,
        broker: &crate::workspace_access::Broker,
        budget: &Budget<'_>,
        compile_native: bool,
    ) -> Result<serde_json::Value, String> {
        let revision = Revision::new(&current_revision(&fixture.app, budget)?)?;
        let project = ImportedProject::bind(&fixture.app, revision, budget)?;
        let engine = "a".repeat(32);
        let vendor = "b".repeat(32);
        let engine_root = canonical_directory(&fixture.engine, budget)?.0;
        let vendor_root = canonical_directory(&fixture.vendor, budget)?.0;
        let grants = vec![
            (
                engine.clone(),
                issue_grant(
                    &PublicationFixtureOwner {
                        project: &project,
                        canonical_root: &engine_root,
                    },
                    &project,
                    RootId::new(&engine)?,
                    &fixture.engine,
                    Mode::DependencyRead,
                    budget,
                )?,
            ),
            (
                vendor.clone(),
                issue_grant(
                    &PublicationFixtureOwner {
                        project: &project,
                        canonical_root: &vendor_root,
                    },
                    &project,
                    RootId::new(&vendor)?,
                    &fixture.vendor,
                    Mode::DependencyRead,
                    budget,
                )?,
            ),
        ];
        // Same accepted layout as seeded_publication_plan, with only public IDs remapped.
        let placements = Fixture::placements()
            .into_iter()
            .map(|placement| {
                let root_id = match placement.root_id.as_str() {
                    "project" => "project",
                    "engine" => engine.as_str(),
                    "vendor" => vendor.as_str(),
                    _ => unreachable!("fixed fixture root identity"),
                };
                serde_json::json!({"root_id":root_id,"slot":placement.slot})
            })
            .collect::<Vec<_>>();
        // Genuine retained test-owned grants, not attended approval UI proof.
        broker.insert_simulated_grants_for_test(grants);
        let mut request = serde_json::json!({"op":"external_workspace_validate","root_ids":[engine,vendor],
            "placements":placements,"workspace":{"root_id":engine,"path":"Cargo.toml"},
            "member":{"root_id":engine,"path":"native/Cargo.toml"},"compile_native":compile_native});
        if compile_native {
            request["gdextension_manifest"] =
                serde_json::json!({"root_id":"project","path":"bin/checked.gdextension"});
        }
        Ok(request)
    }
    #[cfg(windows)]
    #[test]
    fn configfile_parser_public_request_stages_granted_dependency_layout_without_compiler() {
        let fixture = Fixture::new();
        seed_private_gdextension_fixture(&fixture);
        let b = budget();
        let operation = crate::operation_context::OperationContext::new(b.deadline());
        operation
            .scope(|| {
                let runtime = crate::native_operations::NativeRuntime::controller(&fixture.app);
                let (broker, controller) = runtime
                    .original_managed_output_for_test(&fixture.app)
                    .unwrap();
                let before = fixture_oracle(&fixture, &b).unwrap();
                let request =
                    public_configfile_fixture_request(&fixture, broker, &b, false).unwrap();
                assert_eq!(request["compile_native"], false);
                assert!(request.get("gdextension_manifest").is_none());
                assert_eq!(request["placements"][2]["slot"], "vendor/value-lib");
                let pool = std::env::temp_dir().join(format!(
                    "aurum-candidate-fixture-{}",
                    crate::sha256_hex(fixture.app.to_string_lossy().as_bytes())
                ));
                assert!(!pool.exists());
                let mut wrong = request.clone();
                wrong["placements"][2]["slot"] = serde_json::json!("vendor");
                assert_eq!(
                    crate::project_ops::execute_in_runtime(&fixture.app, &wrong, false, &runtime)
                        .unwrap_err(),
                    "relative_layout_mismatch"
                );
                assert!(!pool.exists());
                let value =
                    crate::project_ops::execute_in_runtime(&fixture.app, &request, false, &runtime)
                        .unwrap();
                assert_eq!(value["ok"], true);
                assert_eq!(value["validation"], "confined_source_staging");
                assert_eq!(value["cargo_manifest"], "engine/Cargo.toml");
                for flag in [
                    "cargo_executed",
                    "native_artifact_produced",
                    "source_publish",
                    "originals_mutated",
                    "manifest_transformed",
                ] {
                    assert_eq!(value[flag], false);
                }
                assert_eq!(value["candidate_lease_owned"], true);
                assert_eq!(value["private_workspace_cleaned"], true);
                assert!(value.get("configfile_comparison").is_none());
                let roots = value["roots"].as_array().unwrap();
                assert_eq!(roots.len(), 3);
                for expected in ["app", "engine", "vendor/value-lib"] {
                    assert_eq!(
                        roots.iter().filter(|row| row["slot"] == expected).count(),
                        1
                    );
                }
                assert!(std::fs::read_dir(&pool).unwrap().all(|entry| {
                    let entry = entry.unwrap();
                    !entry.file_type().unwrap().is_dir()
                        || !entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("candidate-")
                }));
                assert_eq!(controller.retained(), 0);
                runtime.close();
                assert!(!broker.managed_output_is_open());
                assert_eq!(controller.retained(), 0);
                assert_eq!(fixture_oracle(&fixture, &b).unwrap(), before);
                operation.check().unwrap();
            })
            .unwrap();
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "requires separately authorized real public compiler and configured ConfigFile engine"]
    fn real_public_configfile_comparison() {
        const LOCK: &str = "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\n";
        let admitted = std::env::var_os("AURUM_ARTIFACT_CUSTODY_GATE_TEMP")
            .expect("fresh admitted private TEMP required");
        assert_eq!(
            crate::project::clean_path(std::env::temp_dir()),
            crate::project::clean_path(PathBuf::from(admitted))
        );
        assert!(crate::operation_context::OperationContext::current().is_none());
        let fixture = Fixture::new();
        seed_private_gdextension_fixture(&fixture);
        std::fs::write(fixture.engine.join("Cargo.lock"), LOCK).unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_secs(60);
        let operation =
            std::mem::ManuallyDrop::new(crate::operation_context::OperationContext::new(deadline));
        let runtime = std::mem::ManuallyDrop::new(
            crate::native_operations::NativeRuntime::controller(&fixture.app),
        );
        let (broker, controller) = runtime
            .original_managed_output_for_test(&fixture.app)
            .unwrap();
        let mut before = None;
        let mut after = None;
        let mut runtime_closed = false;
        let result = operation
            .scope(|| -> Result<serde_json::Value, String> {
                let check = || {
                    operation
                        .check()
                        .map_err(|_| "public_parser_original_deadline".into())
                };
                let budget = Budget::clipped(deadline, &check)?;
                before = Some(fixture_oracle(&fixture, &budget));
                before
                    .as_ref()
                    .ok_or("public_parser_before")?
                    .as_ref()
                    .map_err(|_| "public_parser_before")?;
                let request = public_configfile_fixture_request(&fixture, broker, &budget, true)?;
                let value = crate::project_ops::execute_in_runtime(
                    &fixture.app,
                    &request,
                    false,
                    &runtime,
                )?;
                operation
                    .check()
                    .map_err(|_| "public_parser_original_deadline")?;
                if value["ok"] != true
                    || value["cargo_executed"] != true
                    || value["configfile_comparison"]["status"] != "matched_supported_quoted_subset"
                    || value["configfile_comparison"]["entry_symbol"] != "value"
                    || value["configfile_comparison"]["library_relative"]
                        != "bin/external_logic.dll"
                    || value["configfile_comparison"]["parser_workspace_cleaned"] != true
                    || value["private_workspace_cleaned"] != true
                    || controller.retained() != 0
                {
                    return Err("public_parser_actual_result_or_retirement".into());
                }
                runtime.close();
                runtime_closed = !broker.managed_output_is_open() && controller.retained() == 0;
                if !runtime_closed {
                    return Err("public_parser_runtime_close".into());
                }
                after = Some(fixture_oracle(&fixture, &budget));
                if !matches!((&before,&after),(Some(Ok(a)),Some(Ok(z))) if a==z) {
                    return Err("public_parser_original_roots_changed".into());
                }
                operation
                    .check()
                    .map_err(|_| "public_parser_original_deadline")?;
                Ok(value)
            })
            .map_err(|_| "public_parser_original_scope".to_owned())
            .and_then(|value| value);
        let succeeded = result.is_ok();
        let report = serde_json::json!({"schema":"real_public_configfile_fixture_v1","succeeded":succeeded,
            "result":result.as_ref().ok(),"failure":result.as_ref().err(),"original_budget_ms":60000,
            "elapsed_ms":started.elapsed().as_millis(),"runtime_known_closed":runtime_closed,
            "controller_retained":controller.retained(),"before":before.as_ref().map(fixture_all_entry_oracle::status),
            "after":after.as_ref().map(fixture_all_entry_oracle::status),"source_publication":false,"loaded":false,
            "abi_verified":false,"read_grant_scope":"test-owned genuine retained file grants; no attended UI claim"});
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 65536);
        let path = fixture.base.join("real-public-configfile-result.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.sync_all().unwrap();
        println!("public_configfile_fixture_receipt={}", path.display());
        assert!(
            succeeded,
            "first public parser result retained without retry"
        );
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "requires separately admitted diagnostic custody producer and exact private TEMP"]
    fn native_artifact_custody_real_compile_diagnostic_gate() {
        artifact_custody_real_compile_fixture(true)
    }
    #[cfg(windows)]
    fn artifact_custody_real_compile_fixture(diagnostic_requested: bool) {
        fn held_copy_bytes(file: &File, budget: &Budget<'_>) -> Result<Vec<u8>, String> {
            let mut reader = file;
            let position =
                std::io::Seek::stream_position(&mut reader).map_err(|_| "custody_gate_cursor")?;
            let observed = (|| -> Result<Vec<u8>, String> {
                budget.check()?;
                std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))
                    .map_err(|_| "custody_gate_seek")?;
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 65536];
                loop {
                    budget.check()?;
                    let count = std::io::Read::read(&mut reader, &mut buffer)
                        .map_err(|_| "custody_gate_read")?;
                    if count == 0 {
                        break;
                    }
                    if bytes.len().saturating_add(count) as u64 > budget.max_file {
                        return Err("custody_gate_replica_bound".into());
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                budget.check()?;
                Ok(bytes)
            })();
            let restored = std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(position));
            match (observed, restored) {
                (Ok(bytes), Ok(_)) => {
                    budget.check()?;
                    Ok(bytes)
                }
                (Err(error), Ok(_)) => Err(error),
                (_, Err(_)) => Err("custody_gate_cursor_restore_failed".into()),
            }
        }
        fn delete_access_refused(path: &Path, directory: bool) -> bool {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .access_mode(0x0001_0000)
                .share_mode(7)
                .custom_flags(0x0020_0000 | if directory { 0x0200_0000 } else { 0 })
                .open(path)
                .is_err()
        }
        const LOCK: &str = "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\n";
        const ORIGINAL: &[u8] =
            b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() }\n";
        const EDITED: &[u8] =
            b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() + 12 }\n";
        let admitted = std::env::var_os("AURUM_ARTIFACT_CUSTODY_GATE_TEMP")
            .expect("separately admitted exact private TEMP is required");
        assert_eq!(
            crate::project::clean_path(std::env::temp_dir()),
            crate::project::clean_path(PathBuf::from(admitted))
        );
        assert!(crate::operation_context::OperationContext::current().is_none());
        let fixture = Fixture::new();
        assert_eq!(
            std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
            ORIGINAL
        );
        seed_private_gdextension_fixture(&fixture);
        std::fs::write(fixture.engine.join("Cargo.lock"), LOCK).unwrap();
        std::fs::write(
            fixture.app.join("accepted.dll"),
            b"previous accepted DLL fixture\n",
        )
        .unwrap();
        std::fs::create_dir(fixture.app.join("userdata")).unwrap();
        std::fs::write(
            fixture.app.join("userdata/save.bin"),
            b"working userdata fixture\n",
        )
        .unwrap();
        let started = Instant::now();
        let fixed_deadline = started + Duration::from_secs(60);
        let operation = std::mem::ManuallyDrop::new(
            crate::operation_context::OperationContext::new(fixed_deadline),
        );
        let runtime = std::mem::ManuallyDrop::new(
            crate::native_operations::NativeRuntime::controller(&fixture.app),
        );
        let (broker, original_controller) = runtime
            .original_managed_output_for_test(&fixture.app)
            .expect("the actual original runtime must issue its own read controller");
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let controller = std::mem::ManuallyDrop::new(original_controller);
        let mut parent = None;
        let mut retained_workspace = None;
        let mut retained_candidate = None;
        let mut before = None;
        let mut after = None;
        let mut native_calls = 0usize;
        let mut compiler_diagnostic = None;
        let result = operation.scope(|| -> Result<serde_json::Value, String> {
            let check = || {
                operation.check().map_err(|_| "custody_gate_original_context_expired")?;
                if cancelled.load(Ordering::Acquire) || !broker.managed_output_is_open() {
                    return Err("custody_gate_original_owner_closed".into());
                }
                Ok(())
            };
            parent = Some(std::mem::ManuallyDrop::new(crate::external_native::ParentDeadline::at(fixed_deadline)?));
            let parent = parent.as_mut().ok_or("custody_gate_parent_missing")?;
            let mut budget = Budget::clipped(parent.instant(), &check)?;
            budget.max_bytes = 4 * 1024 * 1024;
            budget.max_file = 1024 * 1024;
            budget.max_files = 128;
            budget.max_entries = 256;
            if budget.deadline() != fixed_deadline || parent.instant() != fixed_deadline {
                return Err("custody_gate_initial_deadline_changed".into());
            }
            let original_limits = [budget.max_bytes, budget.max_file, budget.max_files as u64, budget.max_entries as u64];
            let plan = publication_plan(&fixture, &budget);
            before = Some(fixture_oracle(&fixture, &budget));
            before.as_ref().ok_or("custody_gate_before_missing")?.as_ref().map_err(|_| "custody_gate_before_refused")?;
            runtime.check_managed_native_output(&fixture.app, &controller)?;
            let mut prepared = PreparedSourcePublication::prepare(
                &plan, &RootId::new("engine")?,
                vec![crate::revisions::Change {
                    path: "native/src/lib.rs".into(), action: crate::revisions::Action::Replace,
                    expected_sha256: crate::sha256_hex(ORIGINAL), text: Some(std::str::from_utf8(EDITED).unwrap().into()),
                }], &budget,
            )?;
            retained_workspace = Some(std::mem::ManuallyDrop::new(
                crate::candidate_workspace::Workspace::for_test_project_native_compile(&fixture.app)?,
            ));
            let workspace = retained_workspace.as_mut().ok_or("custody_gate_workspace_missing")?;
            let envelope = workspace.source_envelope(budget.deadline())?;
            let proposal = prepared.stage_for_native_validation(
                &plan, &envelope, parent, broker, cancelled.clone(), &budget,
            )?;
            drop(envelope);
            // The diagnostic entry is the separately admitted engine-token
            // fixture. Retain its original image on every uncertain path.
            let held_engine = if diagnostic_requested {
                Some(std::mem::ManuallyDrop::new(crate::external_native::HeldConfigFileEngine::hold_configured(
                    &runtime, &fixture.app, parent, &controller,
                )?))
            } else { None };
            let source = proposal.staged.member_manifest.parent().ok_or("custody_gate_member_parent")?.join("src/lib.rs");
            let input_paths = [proposal.staged.cargo_manifest.clone(), proposal.staged.member_manifest.clone(),
                source.clone(), proposal.staged.cargo_manifest.parent().ok_or("custody_gate_lock_parent")?.join("Cargo.lock")];
            native_calls += 1;
            let candidate = if diagnostic_requested {
                let permit = fixture.cargo_diagnostic_permit(workspace, &proposal.staged)?;
                proposal.validate_before_publication_diagnostic_for_test(
                    workspace, &controller, parent, permit, &mut compiler_diagnostic,
                )?
            } else {
                proposal.validate_before_publication(workspace, &controller, parent)?
            };
            retained_candidate = Some(std::mem::ManuallyDrop::new(candidate));
            let candidate = retained_candidate.as_ref().ok_or("custody_gate_candidate_missing")?;
            if workspace.lifetime.unresolved() || controller.retained() != 0
                || !candidate.ownership.matches(&workspace.path, &candidate.proposal.staged.envelope)
                || candidate.native_validation_tag.is_none()
            {
                return Err("custody_gate_unique_retirement_or_seal_missing".into());
            }
            parent.check_read_controller(&controller)?;
            budget.check()?;
            let identity = file_identity(&candidate.artifact_guard)?;
            let handle = std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard) as usize;
            let artifact_hash = candidate.artifact_hash.clone();
            let provenance_digest = candidate.compiler_provenance.real_digest()?.to_owned();
            let reported_outputs_before=match &candidate.compiler_provenance {
                CandidateCompilerProvenance::Real(value)=>value.reported_outputs_for_test(&budget)?,
                _=>return Err("custody_gate_reported_output_real_witness_required".into()),
            };
            let reported_rows=reported_outputs_before["records"].as_array().ok_or("custody_gate_reported_output_records")?;
            let value_keys=reported_outputs_before["observed_local_builds"].as_array().ok_or("custody_gate_reported_output_units")?
                .iter().filter(|row|row["observation"]["target"]["name"]=="value_lib")
                .map(|row|row["observation_key_sha256"].as_str().map(str::to_owned).ok_or("custody_gate_reported_output_key"))
                .collect::<Result<BTreeSet<_>,_>>()?;
            let mut reported_rlib=false;let mut reported_dll=false;
            for record in reported_rows {
                for output in record["outputs"].as_array().ok_or("custody_gate_reported_output_rows")? {
                    let name=output["output"]["relative"].as_str().ok_or("custody_gate_reported_output_name")?.to_ascii_lowercase();
                    let role=output["root_role"].as_str().ok_or("custody_gate_reported_output_root_role")?;
                    if !matches!(role,"target"|"build") {return Err("custody_gate_reported_output_root_role".into());}
                    reported_rlib|=role=="build" && name.ends_with(".rlib") && record["observation_key_sha256"].as_str().is_some_and(|key|value_keys.contains(key));
                    if role=="target" && name.ends_with(".dll") && output["output"]["sha256"]==artifact_hash {
                        if output["output"]["identity_sha256"]!=identity_digest(&identity) {
                            return Err("custody_gate_selected_reported_output_identity_changed".into());
                        }
                        reported_dll=true;
                    }
                    if output["write_access_refused"]!=true || output["delete_access_refused"]!=true {
                        return Err("custody_gate_reported_output_sharing_missing".into());
                    }
                }
            }
            if !reported_rlib || !reported_dll {return Err("custody_gate_dependency_rlib_and_selected_dll_unobserved".into());}
            let bytes = held_copy_bytes(&candidate.artifact_guard, &budget)?;
            if crate::sha256_hex(&bytes) != artifact_hash { return Err("custody_gate_held_copy_changed".into()); }
            let pe = crate::external_native::pe_receipt(&bytes, "x86_64-pc-windows-msvc")?;
            budget.check()?;
            let candidate = std::mem::ManuallyDrop::into_inner(retained_candidate.take().ok_or("custody_gate_move_candidate")?);
            let workspace = std::mem::ManuallyDrop::into_inner(retained_workspace.take().ok_or("custody_gate_move_workspace")?);
            let custody = match candidate.into_artifact_custody(workspace, parent, &controller, &budget) {
                Ok(custody) => custody,
                Err((error, candidate, workspace)) => {
                    retained_candidate = Some(std::mem::ManuallyDrop::new(candidate));
                    retained_workspace = Some(std::mem::ManuallyDrop::new(workspace));
                    return Err(error);
                }
            };
            // On any unexpected error, retain all inner owners until host exit.
            let mut custody = std::mem::ManuallyDrop::new(custody);
            custody.recheck()?;
            let reported_outputs_after=match &custody.candidate.compiler_provenance {
                CandidateCompilerProvenance::Real(value)=>value.reported_outputs_for_test(&budget)?,
                _=>return Err("custody_gate_reported_output_real_witness_required".into()),
            };
            if reported_outputs_after!=reported_outputs_before {return Err("custody_gate_reported_output_handles_changed_on_move".into());}
            let cursor = std::io::Seek::stream_position(&mut &custody.candidate.artifact_guard)
                .map_err(|_| "custody_gate_private_cursor")?;
            custody.place_private_artifact()?;
            if std::io::Seek::stream_position(&mut &custody.candidate.artifact_guard)
                .map_err(|_| "custody_gate_private_cursor")? != cursor
            { return Err("custody_gate_private_cursor_changed".into()); }
            custody.recheck_private_artifact()?;
            let replica = custody.private_artifact.as_ref().ok_or("custody_gate_private_copy_missing")?
                .slot.path().to_path_buf();
            if replica != custody.workspace.path.join("checked-artifact.dll") {
                return Err("custody_gate_private_copy_outside_allocation".into());
            }
            if custody.place_private_artifact().err().as_deref() != Some("external_private_artifact_already_attempted") {
                return Err("custody_gate_private_copy_retry_accepted".into());
            }
            let binding = custody.prepare_gdextension_binding(
                &RootId::new("project")?, "bin/checked.gdextension",
            )?;
            let binding_library = binding.library_relative().to_owned();
            let binding_entry = binding.entry_symbol().to_owned();
            let binding_export_rva = binding.direct_export_rva();
            if binding_library != "bin/external_logic.dll" || binding_entry != "value" || binding_export_rva == 0 {
                return Err("custody_gate_private_binding_selection_changed".into());
            }
            custody.recheck_gdextension_binding()?;
            let placed_writer = custody.private_artifact.as_ref().and_then(|placement| placement.writer.as_ref())
                .ok_or("custody_gate_private_writer_missing")?;
            let placed_cursor = std::io::Seek::stream_position(&mut &*placed_writer)
                .map_err(|_| "custody_gate_private_cursor")?;
            // A direct read-only lookup on the same writer, not a second plan or
            // a mutation of the held manifest/selected symbol after retirement.
            if custody.inspect_private_named_export("missing_aurum_entry").err().as_deref()
                != Some("external_named_export_missing")
            { return Err("custody_gate_missing_named_export_accepted".into()); }
            if std::io::Seek::stream_position(&mut &*placed_writer)
                .map_err(|_| "custody_gate_private_cursor")? != placed_cursor
            { return Err("custody_gate_private_export_cursor_changed".into()); }
            custody.recheck_gdextension_binding()?;
            if custody.prepare_gdextension_binding(&RootId::new("project")?, "bin/checked.gdextension")
                .err().as_deref() != Some("external_private_binding_already_prepared")
            { return Err("custody_gate_private_binding_retry_accepted".into()); }
            custody.gdextension_binding.as_mut().ok_or("custody_gate_binding_missing")?
                .placed_identity.volume ^= 1;
            let foreign_binding = custody.recheck_gdextension_binding();
            custody.gdextension_binding.as_mut().ok_or("custody_gate_binding_missing")?
                .placed_identity.volume ^= 1;
            if foreign_binding.err().as_deref() != Some("external_private_binding_owner_changed") {
                return Err("custody_gate_foreign_private_binding_accepted".into());
            }
            custody.recheck_gdextension_binding()?;
            let mut refusals = Vec::new();
            let saved = std::mem::replace(&mut custody.candidate.compiler_provenance, CandidateCompilerProvenance::SyntheticAuthorityOnly);
            let observed = custody.recheck();
            custody.candidate.compiler_provenance = saved;
            let error = observed.err().ok_or("custody_gate_synthetic_proof_accepted")?;
            if error != "external_source_synthetic_provenance_refused" { return Err(error); }
            custody.recheck()?;
            refusals.push(serde_json::json!({"case":"synthetic_provenance","error":error}));
            let saved = std::mem::replace(&mut custody.candidate.proposal.staged.member_manifest, fixture.base.join("foreign/Cargo.toml"));
            let observed = custody.recheck();
            custody.candidate.proposal.staged.member_manifest = saved;
            let error = observed.err().ok_or("custody_gate_wrong_source_accepted")?;
            if error != "external_native_provenance_binding_changed" { return Err(error); }
            custody.recheck()?;
            refusals.push(serde_json::json!({"case":"selected_source_binding","error":error}));
            custody.candidate.proposal.staged.envelope_identity.volume ^= 1;
            let observed = custody.recheck();
            custody.candidate.proposal.staged.envelope_identity.volume ^= 1;
            let error = observed.err().ok_or("custody_gate_input_identity_accepted")?;
            if error != "external_provenance_envelope_changed" { return Err(error); }
            custody.recheck()?;
            refusals.push(serde_json::json!({"case":"held_input_envelope_identity","error":error}));
            custody.candidate.artifact_hash.push('0');
            let observed = custody.recheck();
            custody.candidate.artifact_hash.pop();
            let error = observed.err().ok_or("custody_gate_artifact_hash_accepted")?;
            if error != "external_native_provenance_binding_changed" { return Err(error); }
            custody.recheck()?;
            refusals.push(serde_json::json!({"case":"artifact_hash","error":error}));
            let replacement = custody.private_artifact.as_ref().ok_or("custody_gate_private_copy_missing")?
                .writer.as_ref().ok_or("custody_gate_private_writer_missing")?.try_clone()
                .map_err(|_| "custody_gate_private_writer_clone")?;
            let original_guard = std::mem::replace(&mut custody.candidate.artifact_guard, replacement);
            let observed = custody.recheck();
            custody.candidate.artifact_guard = original_guard;
            let error = observed.err().ok_or("custody_gate_foreign_same_bytes_handle_accepted")?;
            // Successful checks immediately before and after exclude deadline masking.
            custody.recheck()?;
            refusals.push(serde_json::json!({"case":"foreign_same_bytes_artifact_handle","error":error}));
            if file_identity(&custody.candidate.artifact_guard)? != identity
                || std::os::windows::io::AsRawHandle::as_raw_handle(&custody.candidate.artifact_guard) as usize != handle
                || custody.candidate.artifact_hash != artifact_hash
                || custody.candidate.compiler_provenance.real_digest()? != provenance_digest
                || custody.budget.deadline() != fixed_deadline || parent.instant() != fixed_deadline
                || [custody.budget.max_bytes, custody.budget.max_file, custody.budget.max_files as u64, custody.budget.max_entries as u64] != original_limits
                || !crate::operation_context::OperationContext::current().is_some_and(|current| current.same_operation(&operation))
            {
                return Err("custody_gate_original_owner_or_deadline_changed".into());
            }
            for path in &input_paths {
                if std::fs::OpenOptions::new().write(true).open(path).is_ok() || !delete_access_refused(path, false) {
                    return Err("custody_gate_input_sharing_boundary_missing".into());
                }
            }
            if !delete_access_refused(&custody.candidate.proposal.staged.envelope, true)
                || !delete_access_refused(&custody.candidate.artifact, false)
            { return Err("custody_gate_delete_sharing_boundary_missing".into()); }
            custody.recheck()?;
            custody.recheck_private_artifact()?;
            custody.recheck_gdextension_binding()?;
            if let Some(engine) = &held_engine { engine.recheck()?; }
            let workspace_path = custody.workspace.path.clone();
            let artifact_path = custody.candidate.artifact.clone();
            let envelope_path = custody.candidate.proposal.staged.envelope.clone();
            let workspace_text = workspace_path.to_str().ok_or("custody_gate_workspace_encoding")?;
            if !workspace_path.is_absolute() || workspace_text.len() > 4096 {
                return Err("custody_gate_workspace_report_bound".into());
            }
            let confined_relative = |path: &Path| -> Result<String, String> {
                let value = path.strip_prefix(&workspace_path)
                    .map_err(|_| "custody_gate_cleanup_path_outside_workspace")?
                    .to_str().ok_or("custody_gate_cleanup_path_encoding")?.replace('\\', "/");
                relative(&value, false)?;
                if value.len() > 4096 {
                    return Err("custody_gate_cleanup_path_bound".into());
                }
                Ok(value)
            };
            let artifact_relative = confined_relative(&artifact_path)?;
            let envelope_relative = confined_relative(&envelope_path)?;
            let mut input_relatives = Vec::with_capacity(4);
            for (role, path) in ["workspace_manifest", "selected_manifest", "selected_source", "lock"]
                .into_iter().zip(input_paths.iter())
            {
                input_relatives.push(serde_json::json!({"role":role,"path":confined_relative(path)?}));
            }
            if input_relatives.len() != 4 {
                return Err("custody_gate_cleanup_input_count".into());
            }
            let reported_identity = format!("{identity:?}");
            if reported_identity.len() > 512 {
                return Err("custody_gate_reported_identity_bound".into());
            }
            if let Some(engine) = held_engine { drop(std::mem::ManuallyDrop::into_inner(engine)); }
            let custody = std::mem::ManuallyDrop::into_inner(custody);
            drop(custody);
            budget.check()?;
            if workspace_path.exists() || artifact_path.exists() || envelope_path.exists()
                || replica.exists() || input_paths.iter().any(|path| path.exists()) {
                return Err("custody_gate_guard_before_workspace_cleanup_unproven".into());
            }
            after = Some(fixture_oracle(&fixture, &budget));
            let preserved = matches!((&before, &after), (Some(Ok(a)), Some(Ok(z))) if a == z);
            if !preserved { return Err("custody_gate_outside_all_entries_changed".into()); }
            parent.check_read_controller(&controller)?;
            budget.check()?;
            Ok(serde_json::json!({
                "same_original_artifact_handle_after_move":true,"real_provenance_digest":provenance_digest,
                "real_provenance_and_original_artifact_rechecked":true,"controlled_refusals":refusals,
                "all_mutations_restored":true,"original_native_context_and_deadline_unchanged":true,
                "unique_compiler_retirement_and_seal_before_move":true,"workspace_cleaned_before_scope_return":true,
                "five_handle_witness_drop_boundary":"OPAQUE_REAL_WITNESS_RECHECK_PLUS_OBSERVED_WORKSPACE_REMOVAL",
                "four_selected_inputs_write_and_delete_access_refused":true,"artifact_and_envelope_delete_access_refused":true,
                "original_workspace":workspace_path,"original_artifact_relative":artifact_relative,
                "original_envelope_relative":envelope_relative,"original_selected_input_relatives":input_relatives,
                "reported_original_artifact_identity":reported_identity,
                "reported_identity_scope":"REPORTED_PRE_DROP_TEST_OBSERVATION_NOT_POSTTEST_HANDLE_ADOPTION",
                "artifact_replica":{"path":replica,"bytes":bytes.len(),"sha256":artifact_hash,"pe":pe},
                "artifact_replica_scope":"Product fixed-slot CreateNew copy from held original, retained and cleaned in the same allocation; no loading authority",
                "private_placement_same_writer_readback_and_original_cursor_preserved":true,
                "private_placement_retry_refused":true,
                "reported_local_outputs":reported_outputs_after,"same_original_reported_output_handles_after_move":true,
                "dependency_rlib_and_selected_DLL_reported_and_held":true,
                "dependency_rlib_original_build_root_and_selected_DLL_original_target_root_verified":true,
                "reported_output_scope":"Cargo-reported local output content custody only; no compiler read closure or exact build unit claim",
                "held_configfile_engine":if diagnostic_requested { serde_json::json!({
                    "original_runtime_issued_controller":true,"same_original_parent_and_read_broker":true,
                    "constructor_returned":true,"final_same_guard_recheck_returned":true,
                    "held_through_compile_and_custody":true,"dropped_before_known_workspace_cleanup":true,
                    "engine_processes":0,"scope":"configured image identity only; no version, benignness, launch or ConfigFile authority"
                }) } else { serde_json::Value::Null },
                "private_gdextension_binding":{"manifest":"bin/checked.gdextension","library":binding_library,
                    "entry_symbol":binding_entry,"original_held_manifest_rechecked":true,
                    "same_private_Dll_identity_rechecked":true,"foreign_copy_identity_refused":true,
                    "direct_named_export_rva":binding_export_rva,"missing_name_lookup_refused":true,
                    "missing_name_scope":"read-only same-writer lookup; existing plan and held manifest unchanged",
                    "foreign_copy_scope":"stored binding identity metadata flipped and restored; no second-writer substitution",
                    "scope":"quoted Windows debug/x86_64 plan plus direct named executable PE export; no ConfigFile, ABI or loading qualification"},
                "working_source_DLL_userdata_ungranted_all_entries_equal":preserved,
                "source_publication_called":false,"loaded":false,"activated":false,"relocation_attempted":false
            }))
        }).map_err(|_| "custody_gate_original_native_scope_refused".to_owned()).and_then(|inner| inner);
        let mut runtime_known_closed = false;
        let result = result.and_then(|value| {
            operation
                .check()
                .map_err(|_| "custody_gate_original_context_expired")?;
            if controller.retained() != 0
                || retained_workspace.is_some()
                || retained_candidate.is_some()
            {
                return Err("custody_gate_runtime_close_not_settled".into());
            }
            // No previews, jobs or dialogs were created by this fixture. Close
            // only after known worker retirement, cleanup and the unchanged
            // four-root oracle. On every earlier failure the runtime is retained.
            runtime.close();
            if broker.managed_output_is_open()
                || controller.retained() != 0
                || runtime
                    .check_managed_native_output(&fixture.app, &controller)
                    .is_ok()
            {
                return Err("custody_gate_runtime_close_unconfirmed".into());
            }
            operation
                .check()
                .map_err(|_| "custody_gate_original_context_expired")?;
            runtime_known_closed = true;
            Ok(value)
        });
        let succeeded = result.is_ok();
        let failure = result.as_ref().err().cloned();
        let mut report = result.unwrap_or_else(|error| serde_json::json!({"failure":error}));
        report["schema"] =
            serde_json::json!("native_artifact_custody_terminal_fixture_observations_v1");
        report["api_observations_succeeded"] = serde_json::json!(succeeded);
        report["failure"] = serde_json::json!(failure);
        report["real_native_calls"] = serde_json::json!(native_calls);
        report["original_native_parent_and_source_budget_ms"] = serde_json::json!(60000);
        report["postseal_budget_or_publication_window_created"] = serde_json::json!(false);
        report["fixture_oracle_before"] = before
            .as_ref()
            .map(fixture_all_entry_oracle::status)
            .unwrap_or_else(|| serde_json::json!({"status":"NOT_ATTEMPTED"}));
        report["fixture_oracle_after"] = after
            .as_ref()
            .map(fixture_all_entry_oracle::status)
            .unwrap_or_else(
                || serde_json::json!({"status":"UNAVAILABLE_NO_RENEWAL_AFTER_FAILURE"}),
            );
        report["controller_retained"] = serde_json::json!(controller.retained());
        report["original_native_runtime_known_closed"] = serde_json::json!(runtime_known_closed);
        report["compiler_phase_raw_receipts"] =
            serde_json::json!("UNEXPORTED_BY_CURRENT_VALIDATED_CANDIDATE_API");
        report["elapsed_ms"] = serde_json::json!(started.elapsed().as_millis());
        report["remaining_boundary"] = serde_json::json!("Selected-input custody only; no dependency content closure/exact build units, relocation, SourcePublish, DLL or GDExtension loading");
        if diagnostic_requested {
            let data = compiler_diagnostic.as_ref().map(|diagnostic| {
                let completion = diagnostic.completion().map(|c| serde_json::json!({
                    "root_exit":c.root_exit,"stdout_bytes":c.stdout,"stderr_bytes":c.stderr,
                    "stdout_eof":c.eof.0,"stderr_eof":c.eof.1,"tree_retired":c.tree,"readers_joined":c.readers
                }));
                let written = diagnostic.write_private();
                serde_json::json!({
                    "path":fixture.base.join("real-cargo-build-private-diagnostic.bin"),
                    "write_returned":written.is_ok(),
                    "write_error":written.err().map(|e| serde_json::json!({
                        "kind":format!("{:?}",e.kind()),"raw_os_code":e.raw_os_error()
                    })),
                    "completion":completion,"compiler_error_codes":diagnostic.compiler_error_codes(),
                    "prefix_cap_per_stream":32768,
                    "scope":"private original pipe prefixes and typed first failures; not a full compiler trace or new authority"
                })
            });
            report["private_compiler_diagnostic"] = serde_json::json!(data);
        }
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 65536);
        let receipt = fixture
            .base
            .join("actual-native-artifact-custody-terminal.json");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt)
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.sync_all().unwrap();
        println!(
            "native_artifact_custody_real_producer_receipt={}",
            receipt.display()
        );
        assert!(
            succeeded,
            "terminal custody observations refused; receipt retained"
        );
        assert_eq!(native_calls, 1);
        assert_eq!(controller.retained(), 0);
        assert!(retained_workspace.is_none() && retained_candidate.is_none());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires separately admitted staged-content producer and exact private TEMP"]
    fn native_staged_content_custody_real_compile_first_owner_gate() {
        use std::os::windows::fs::OpenOptionsExt;
        const ORIGINAL: &[u8] =
            b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() }\n";
        const EDITED: &[u8] =
            b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() + 12 }\n";
        const EXPECTED_STAGED_FILES: [(&str, &str); 16] = [
            ("project", "app/accepted.dll"),
            ("project", "app/aurum.toml"),
            ("project", "app/main.gd"),
            ("project", "app/project.godot"),
            ("project", "app/userdata/save.bin"),
            ("engine", "engine/Cargo.lock"),
            ("engine", "engine/Cargo.toml"),
            ("engine", "engine/native/Cargo.toml"),
            ("engine", "engine/native/src/lib.rs"),
            ("engine", "engine/native/src/old.rs"),
            ("vendor", "vendor/value-lib/Cargo.toml"),
            ("vendor", "vendor/value-lib/src/lib.rs"),
            ("vendor", "vendor/value-lib/src/nested.rs"),
            ("vendor", "vendor/value-lib/src/offset.dat"),
            ("vendor", "vendor/value-lib/support/Cargo.toml"),
            ("vendor", "vendor/value-lib/support/src/lib.rs"),
        ];
        const EXPECTED_ANCHORS: [&str; 4] = [
            "engine/Cargo.toml",
            "engine/native/Cargo.toml",
            "engine/native/src/lib.rs",
            "engine/Cargo.lock",
        ];
        let admitted = std::env::var_os("AURUM_STAGED_CONTENT_CUSTODY_GATE_TEMP")
            .expect("separately admitted exact private TEMP is required");
        assert_eq!(
            crate::project::clean_path(std::env::temp_dir()),
            crate::project::clean_path(PathBuf::from(admitted))
        );
        assert!(crate::operation_context::OperationContext::current().is_none());
        let fixture = Fixture::new();
        seed_staged_content_fixture(&fixture);
        assert_eq!(
            std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
            ORIGINAL
        );
        std::fs::write(
            fixture.app.join("accepted.dll"),
            b"previous accepted DLL fixture\n",
        )
        .unwrap();
        std::fs::create_dir(fixture.app.join("userdata")).unwrap();
        std::fs::write(
            fixture.app.join("userdata/save.bin"),
            b"working userdata fixture\n",
        )
        .unwrap();
        let started = Instant::now();
        let fixed_deadline = started + Duration::from_secs(60);
        let operation = std::mem::ManuallyDrop::new(
            crate::operation_context::OperationContext::new(fixed_deadline),
        );
        let broker = std::mem::ManuallyDrop::new(std::sync::Arc::new(
            crate::workspace_access::Broker::default(),
        ));
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let controller =
            std::mem::ManuallyDrop::new(crate::worker_windows::StreamController::for_runtime());
        let mut parent = None;
        let mut retained_workspace = None;
        let mut retained_candidate = None;
        let mut before = None;
        let mut after = None;
        let mut native_calls = 0usize;
        let result = operation.scope(|| -> Result<serde_json::Value, String> {
            let check = || {
                operation.check().map_err(|_| "staged_custody_gate_original_context_expired")?;
                if cancelled.load(Ordering::Acquire) || !broker.managed_output_is_open() {
                    return Err("staged_custody_gate_original_owner_closed".into());
                }
                Ok(())
            };
            parent = Some(std::mem::ManuallyDrop::new(crate::external_native::ParentDeadline::at(fixed_deadline)?));
            let parent = parent.as_mut().ok_or("staged_custody_gate_parent_missing")?;
            let mut budget = Budget::clipped(parent.instant(), &check)?;
            budget.max_bytes = 4 * 1024 * 1024;
            budget.max_file = 1024 * 1024;
            budget.max_files = 128;
            budget.max_entries = 256;
            let limits = [budget.max_bytes, budget.max_file, budget.max_files as u64, budget.max_entries as u64];
            if budget.deadline() != fixed_deadline || parent.instant() != fixed_deadline {
                return Err("staged_custody_gate_initial_deadline_changed".into());
            }
            let plan = publication_plan(&fixture, &budget);
            before = Some(fixture_oracle(&fixture, &budget));
            before.as_ref().ok_or("staged_custody_gate_before_missing")?.as_ref()
                .map_err(|_| "staged_custody_gate_before_refused")?;
            controller.bind_parent(&broker).map_err(|_| "staged_custody_gate_controller_binding")?;
            let mut prepared = PreparedSourcePublication::prepare(
                &plan, &RootId::new("engine")?,
                vec![crate::revisions::Change {
                    path: "native/src/lib.rs".into(), action: crate::revisions::Action::Replace,
                    expected_sha256: crate::sha256_hex(ORIGINAL), text: Some(std::str::from_utf8(EDITED).unwrap().into()),
                }], &budget,
            )?;
            retained_workspace = Some(std::mem::ManuallyDrop::new(
                crate::candidate_workspace::Workspace::for_test_project_native_compile(&fixture.app)?,
            ));
            let workspace = retained_workspace.as_mut().ok_or("staged_custody_gate_workspace_missing")?;
            let envelope = workspace.source_envelope(budget.deadline())?;
            let proposal = prepared.stage_for_native_validation(
                &plan, &envelope, parent, &broker, cancelled.clone(), &budget,
            )?;
            drop(envelope);
            let staged = &proposal.staged;
            let source = staged.member_manifest.parent().ok_or("staged_custody_gate_member_parent")?.join("src/lib.rs");
            let lock = staged.cargo_manifest.parent().ok_or("staged_custody_gate_lock_parent")?.join("Cargo.lock");
            let anchors = [staged.cargo_manifest.as_path(), staged.member_manifest.as_path(), source.as_path(), lock.as_path()];
            for (relative, actual) in EXPECTED_ANCHORS.into_iter().zip(anchors) {
                budget.check()?;
                if !provenance_paths_equal(&staged.envelope.join(relative), actual) {
                    return Err("staged_custody_gate_expected_anchor_binding_missing".into());
                }
            }
            let mut expected_paths = BTreeSet::new();
            let mut expected_roots = BTreeSet::new();
            let mut paths = Vec::with_capacity(12);
            for (root_id, relative) in EXPECTED_STAGED_FILES {
                budget.check()?;
                let mut matching = staged.roots.iter().filter(|root| root.root_id.as_str() == root_id);
                let root = matching.next().ok_or("staged_custody_gate_expected_root_missing")?;
                let path = staged.envelope.join(relative);
                if matching.next().is_some() || !path.starts_with(&root.staged) || !expected_paths.insert(relative) {
                    return Err("staged_custody_gate_expected_path_binding_missing".into());
                }
                expected_roots.insert(root_id);
                if !EXPECTED_ANCHORS.contains(&relative) { paths.push(path); }
            }
            if expected_paths.len() != 16 || expected_roots.len() != 3 || staged.roots.len() != 3 || paths.len() != 12 {
                return Err("staged_custody_gate_expected_coverage_shape_missing".into());
            }
            native_calls += 1;
            let candidate = proposal.validate_before_publication(workspace, &controller, parent)?;
            retained_candidate = Some(std::mem::ManuallyDrop::new(candidate));
            let candidate: &mut ValidatedSourceCandidate = retained_candidate.as_mut()
                .ok_or("staged_custody_gate_candidate_missing")?;
            if workspace.lifetime.unresolved() || controller.retained() != 0
                || !candidate.ownership.matches(&workspace.path, &candidate.proposal.staged.envelope)
                || candidate.native_validation_tag.is_none()
            { return Err("staged_custody_gate_unique_retirement_missing".into()); }
            let artifact_identity = file_identity(&candidate.artifact_guard)?;
            let artifact_handle = std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard) as usize;
            let artifact_hash = candidate.artifact_hash.clone();
            let provenance_digest = candidate.compiler_provenance.real_digest()?.to_owned();
            let staged = &candidate.proposal.staged;
            let first = match &mut candidate.compiler_provenance {
                CandidateCompilerProvenance::Real(record) => record.staged_content_custody_for_test(staged, &paths, &budget, false)?,
                CandidateCompilerProvenance::SyntheticAuthorityOnly => return Err("staged_custody_gate_real_provenance_missing".into()),
            };
            // Five project files, four anchors, preserved old.rs and six dependency files.
            if first["staged_files"] != 16 || first["staged_roots"] != 3
                || first["inputs"].as_array().is_none_or(|inputs| inputs.len() != paths.len())
            {
                return Err("staged_custody_gate_inventory_coverage_missing".into());
            }
            budget.check()?;
            let candidate = std::mem::ManuallyDrop::into_inner(retained_candidate.take().ok_or("staged_custody_gate_move_candidate")?);
            let workspace = std::mem::ManuallyDrop::into_inner(retained_workspace.take().ok_or("staged_custody_gate_move_workspace")?);
            let custody = match candidate.into_artifact_custody(workspace, parent, &controller, &budget) {
                Ok(custody) => custody,
                Err((error, candidate, workspace)) => {
                    retained_candidate = Some(std::mem::ManuallyDrop::new(candidate));
                    retained_workspace = Some(std::mem::ManuallyDrop::new(workspace));
                    return Err(error);
                }
            };
            // Unknown failures retain original owners through host exit.
            let mut custody = std::mem::ManuallyDrop::new(custody);
            custody.recheck()?;
            let candidate = &mut custody.candidate;
            let second = match &mut candidate.compiler_provenance {
                CandidateCompilerProvenance::Real(record) => record.staged_content_custody_for_test(
                    &candidate.proposal.staged, &paths, &budget, true,
                )?,
                CandidateCompilerProvenance::SyntheticAuthorityOnly => return Err("staged_custody_gate_real_provenance_missing".into()),
            };
            if first["inputs"] != second["inputs"]
                || second["restored_corruption"] != "external_provenance_held_input_changed"
                || file_identity(&custody.candidate.artifact_guard)? != artifact_identity
                || std::os::windows::io::AsRawHandle::as_raw_handle(&custody.candidate.artifact_guard) as usize != artifact_handle
                || custody.candidate.artifact_hash != artifact_hash
                || custody.candidate.compiler_provenance.real_digest()? != provenance_digest
                || custody.budget.deadline() != fixed_deadline || parent.instant() != fixed_deadline
                || [custody.budget.max_bytes, custody.budget.max_file, custody.budget.max_files as u64, custody.budget.max_entries as u64] != limits
                || !crate::operation_context::OperationContext::current().is_some_and(|current| current.same_operation(&operation))
            { return Err("staged_custody_gate_original_objects_or_budget_changed".into()); }
            for path in &paths {
                if OpenOptions::new().write(true).open(path).is_ok()
                    || OpenOptions::new().access_mode(0x0001_0000).share_mode(7)
                        .custom_flags(0x0020_0000).open(path).is_ok()
                { return Err("staged_custody_gate_dependency_sharing_missing".into()); }
            }
            custody.recheck()?;
            let workspace_path = custody.workspace.path.clone();
            let artifact_path = custody.candidate.artifact.clone();
            let custody = std::mem::ManuallyDrop::into_inner(custody);
            drop(custody);
            budget.check()?;
            if workspace_path.exists() || artifact_path.exists() || paths.iter().any(|path| path.exists()) {
                return Err("staged_custody_gate_drop_before_cleanup_unproven".into());
            }
            after = Some(fixture_oracle(&fixture, &budget));
            if !matches!((&before, &after), (Some(Ok(a)), Some(Ok(z))) if a == z) {
                return Err("staged_custody_gate_protected_all_entries_changed".into());
            }
            parent.check_read_controller(&controller)?;
            budget.check()?;
            Ok(serde_json::json!({"same_original_artifact_and_dependency_handles_after_move":true,
                "staged_content":second,"real_provenance_digest":provenance_digest,
                "unique_compiler_retirement":true,"dependency_write_delete_access_refused":true,
                "workspace_cleaned_before_scope_return":true,"protected_all_entries_equal":true,
                "original_native_context_and_deadline_unchanged":true,
                "source_publication_called":false,"loaded":false,"activated":false,"relocation_attempted":false}))
        }).map_err(|_| "staged_custody_gate_original_scope_refused".to_owned()).and_then(|inner| inner);
        let succeeded = result.is_ok();
        let mut report = result.unwrap_or_else(|error| serde_json::json!({"failure":error}));
        report["schema"] = serde_json::json!("native_staged_local_content_custody_observations_v1");
        report["expected_staged_files"] = serde_json::json!(EXPECTED_STAGED_FILES);
        report["api_observations_succeeded"] = serde_json::json!(succeeded);
        report["real_native_calls"] = serde_json::json!(native_calls);
        report["original_native_parent_and_source_budget_ms"] = serde_json::json!(60000);
        report["postseal_budget_or_publication_window_created"] = serde_json::json!(false);
        report["dependency_content_closure_verified"] = serde_json::json!(false);
        report["exact_build_units_verified"] = serde_json::json!(false);
        report["fixture_oracle_before"] = before
            .as_ref()
            .map(fixture_all_entry_oracle::status)
            .unwrap_or_else(|| serde_json::json!({"status":"NOT_ATTEMPTED"}));
        report["fixture_oracle_after"] = after
            .as_ref()
            .map(fixture_all_entry_oracle::status)
            .unwrap_or_else(
                || serde_json::json!({"status":"UNAVAILABLE_NO_RENEWAL_AFTER_FAILURE"}),
            );
        report["controller_retained"] = serde_json::json!(controller.retained());
        report["elapsed_ms"] = serde_json::json!(started.elapsed().as_millis());
        report["remaining_boundary"] = serde_json::json!("Staged local-source custody only; compiler read closure, exact build units, relocation, publication and loading remain unverified");
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 65536);
        let receipt = fixture
            .base
            .join("actual-native-staged-content-custody.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt)
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.sync_all().unwrap();
        println!(
            "native_staged_content_custody_real_producer_receipt={}",
            receipt.display()
        );
        assert!(
            succeeded,
            "staged content custody refused; original receipt retained"
        );
        assert_eq!(native_calls, 1);
        assert_eq!(controller.retained(), 0);
        assert!(retained_workspace.is_none() && retained_candidate.is_none());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires separate reviewed exact real compile-first owner gate and private TEMP"]
    fn native_api04_real_compile_first_owner_gate() {
        fn held_bytes(file: &File, budget: &Budget<'_>) -> Result<Vec<u8>, String> {
            budget.check()?;
            let mut reader = file.try_clone().map_err(|_| "gate_artifact_clone")?;
            std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(0))
                .map_err(|_| "gate_artifact_seek")?;
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 65536];
            loop {
                budget.check()?;
                let count = std::io::Read::read(&mut reader, &mut buffer)
                    .map_err(|_| "gate_artifact_read")?;
                if count == 0 {
                    break;
                }
                if (bytes.len() as u64).saturating_add(count as u64) > budget.max_file {
                    return Err("gate_held_file_bound".into());
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            budget.check()?;
            Ok(bytes)
        }
        fn private_bytes(path: &Path, budget: &Budget<'_>) -> Result<Vec<u8>, String> {
            let file = open_compile_source(path, budget)?;
            held_bytes(&file, budget)
        }
        fn limits(value: ProposalLimits) -> [u64; 4] {
            [
                value.bytes,
                value.file,
                value.files as u64,
                value.entries as u64,
            ]
        }
        fn same_candidate(
            candidate: &ValidatedSourceCandidate,
            identity: &DirectoryIdentity,
            handle: usize,
            hash: &str,
            tag: &str,
            budget: &Budget<'_>,
        ) -> Result<(), String> {
            budget.check()?;
            if file_identity(&candidate.artifact_guard)? != *identity
                || std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard)
                    as usize
                    != handle
                || candidate.artifact_hash != hash
                || candidate.native_validation_tag.as_deref() != Some(tag)
                || hash_validated_artifact(&candidate.artifact, &candidate.artifact_guard, budget)?
                    != hash
                || !candidate
                    .ownership
                    .matches(&candidate.workspace, &candidate.proposal.staged.envelope)
            {
                return Err("gate_original_candidate_custody_changed".into());
            }
            Ok(())
        }
        const LOCK: &str = "version = 4\n\n[[package]]\nname = \"native-game\"\nversion = \"0.1.0\"\ndependencies = [\"value-lib\"]\n\n[[package]]\nname = \"value-lib\"\nversion = \"0.1.0\"\n";
        const ORIGINAL: &[u8] =
            b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() }\n";
        const EDITED: &[u8] =
            b"#[no_mangle] pub extern \"C\" fn value() -> i32 { value_lib::value() + 12 }\n";
        const DEPENDENCY: &[u8] = b"pub fn value() -> i32 { 17 }\n";
        // The future owner must pin this exact private TEMP. No old gate path is reused.
        let admitted_temp = std::env::var_os("AURUM_NATIVE_API04_GATE_TEMP")
            .expect("separately admitted exact private TEMP is required");
        assert_eq!(
            crate::project::clean_path(std::env::temp_dir()),
            crate::project::clean_path(PathBuf::from(admitted_temp))
        );
        assert!(crate::operation_context::OperationContext::current().is_none());
        let fixture = Fixture::new();
        assert_eq!(
            std::fs::read(fixture.engine.join("native/src/lib.rs")).unwrap(),
            ORIGINAL
        );
        assert_eq!(
            std::fs::read(fixture.vendor.join("src/lib.rs")).unwrap(),
            DEPENDENCY
        );
        std::fs::write(fixture.engine.join("Cargo.lock"), LOCK).unwrap();
        std::fs::write(
            fixture.app.join("accepted.dll"),
            b"previous accepted DLL fixture\n",
        )
        .unwrap();
        std::fs::create_dir(fixture.app.join("userdata")).unwrap();
        std::fs::write(
            fixture.app.join("userdata/save.bin"),
            b"working userdata fixture\n",
        )
        .unwrap();

        let started = Instant::now();
        let native_deadline = started + Duration::from_secs(180);
        let native_operation = std::mem::ManuallyDrop::new(
            crate::operation_context::OperationContext::new(native_deadline),
        );
        let native_broker = std::mem::ManuallyDrop::new(std::sync::Arc::new(
            crate::workspace_access::Broker::default(),
        ));
        let native_cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let controller =
            std::mem::ManuallyDrop::new(crate::worker_windows::StreamController::for_runtime());
        let publication_broker = std::mem::ManuallyDrop::new(std::sync::Arc::new(
            crate::workspace_access::Broker::default(),
        ));
        let publication_cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let mut parent: Option<std::mem::ManuallyDrop<crate::external_native::ParentDeadline>> =
            None;
        let mut workspace: Option<std::mem::ManuallyDrop<crate::candidate_workspace::Workspace>> =
            None;
        let mut retained_admission: Option<std::mem::ManuallyDrop<SourceAdmission>> = None;
        let mut before: Option<fixture_all_entry_oracle::Outcome> = None;
        let mut after: Option<fixture_all_entry_oracle::Outcome> = None;
        let mut stage = "before_native_scope";
        let mut native_calls = 0usize;
        let mut native_sealed = false;
        let mut confirmations = 0usize;
        let result = (|| -> Result<serde_json::Value, String> {
            let (
                mut prepared,
                candidate,
                provenance_observations,
                proposal_digest,
                original_limits,
                artifact_identity,
                artifact_handle,
                artifact_hash,
                tag,
                artifact_receipt,
                artifact_relative,
                staged_source,
                staged_dependency,
                staged_source_hash,
                dependency_hash,
            ) = native_operation
                .scope(|| -> Result<_, String> {
                    let check = || {
                        native_operation
                            .check()
                            .map_err(|_| "gate_native_context_expired")?;
                        if native_cancelled.load(Ordering::Acquire)
                            || !native_broker.managed_output_is_open()
                        {
                            return Err("gate_native_owner_closed".into());
                        }
                        Ok(())
                    };
                    parent = Some(std::mem::ManuallyDrop::new(
                        crate::external_native::ParentDeadline::at(native_deadline)?,
                    ));
                    let parent = parent.as_mut().ok_or("gate_original_parent_missing")?;
                    let origin = parent.origin().clone();
                    let mut budget = Budget::clipped(parent.instant(), &check)?;
                    budget.max_bytes = 4 * 1024 * 1024;
                    budget.max_file = 1024 * 1024;
                    budget.max_files = 128;
                    budget.max_entries = 256;
                    let original_limits = ProposalLimits::from_budget(&budget);
                    let plan = publication_plan(&fixture, &budget);
                    before = Some(fixture_oracle(&fixture, &budget));
                    before
                        .as_ref()
                        .ok_or("gate_before_oracle_missing")?
                        .as_ref()
                        .map_err(|_| "gate_before_oracle_refused")?;
                    controller
                        .bind_parent(&native_broker)
                        .map_err(|_| "gate_controller_binding_refused")?;
                    stage = "prepare_without_publication_permit";
                    let mut prepared = PreparedSourcePublication::prepare(
                        &plan,
                        &RootId::new("engine")?,
                        vec![crate::revisions::Change {
                            path: "native/src/lib.rs".into(),
                            action: crate::revisions::Action::Replace,
                            expected_sha256: crate::sha256_hex(ORIGINAL),
                            text: Some(std::str::from_utf8(EDITED).unwrap().into()),
                        }],
                        &budget,
                    )?;
                    let proposal_digest = prepared.digest().to_owned();
                    workspace = Some(std::mem::ManuallyDrop::new(
                        crate::candidate_workspace::Workspace::for_test_project_native_compile(
                            &fixture.app,
                        )?,
                    ));
                    let workspace = workspace.as_mut().ok_or("gate_workspace_missing")?;
                    stage = "stage_for_original_native_parent";
                    let envelope = workspace.source_envelope(parent.instant())?;
                    let proposal = prepared.stage_for_native_validation(
                        &plan,
                        &envelope,
                        parent,
                        &native_broker,
                        native_cancelled.clone(),
                        &budget,
                    )?;
                    drop(envelope);
                    if proposal.digest != proposal_digest
                        || limits(proposal.limits) != limits(original_limits)
                        || proposal.native_parent_origin.as_ref() != Some(&origin)
                    {
                        return Err("gate_staged_proposal_binding_changed".into());
                    }
                    let engine = proposal
                        .staged
                        .roots
                        .iter()
                        .find(|root| root.root_id.as_str() == "engine")
                        .ok_or("gate_staged_engine_missing")?;
                    let vendor = proposal
                        .staged
                        .roots
                        .iter()
                        .find(|root| root.root_id.as_str() == "vendor")
                        .ok_or("gate_staged_vendor_missing")?;
                    let staged_source = engine.staged.join("native/src/lib.rs");
                    let staged_dependency = vendor.staged.join("src/lib.rs");
                    if private_bytes(&staged_source, &budget)? != EDITED
                        || private_bytes(&staged_dependency, &budget)? != DEPENDENCY
                    {
                        return Err("gate_staged_edit_or_dependency_mismatch".into());
                    }
                    let staged_source_hash = crate::sha256_hex(EDITED);
                    let dependency_hash = crate::sha256_hex(DEPENDENCY);
                    stage = "real_validate_before_publication";
                    native_calls += 1;
                    // Only this real owned producer may construct and seal the candidate.
                    let mut candidate =
                        proposal.validate_before_publication(workspace, &controller, parent)?;
                    let sealed_check = || parent.check_read_controller(&controller);
                    let inspected = parent.source_budget(&sealed_check)?;
                    if parent.instant() != native_deadline
                        || parent.origin() != &origin
                        || workspace.lifetime.unresolved()
                        || controller.retained() != 0
                        || candidate.proposal.digest != proposal_digest
                        || limits(candidate.proposal.limits) != limits(original_limits)
                        || !candidate
                            .ownership
                            .matches(&workspace.path, &candidate.proposal.staged.envelope)
                    {
                        return Err("gate_original_native_seal_or_retirement_unproven".into());
                    }
                    let provenance_observations = match &mut candidate.compiler_provenance {
                        CandidateCompilerProvenance::Real(record) => record
                            .check_corruptions_for_test(
                                &candidate.proposal.staged,
                                &candidate.artifact,
                                &candidate.artifact_hash,
                                &inspected,
                            )?,
                        CandidateCompilerProvenance::SyntheticAuthorityOnly => {
                            return Err("gate_genuine_compiler_provenance_missing".into());
                        }
                    };
                    let tag = candidate
                        .native_validation_tag
                        .as_ref()
                        .ok_or("gate_real_postvalidation_tag_missing")?
                        .clone();
                    let artifact_identity = file_identity(&candidate.artifact_guard)?;
                    let artifact_handle =
                        std::os::windows::io::AsRawHandle::as_raw_handle(&candidate.artifact_guard)
                            as usize;
                    let artifact_bytes = held_bytes(&candidate.artifact_guard, &inspected)?;
                    let artifact_hash = crate::sha256_hex(&artifact_bytes);
                    same_candidate(
                        &candidate,
                        &artifact_identity,
                        artifact_handle,
                        &artifact_hash,
                        &tag,
                        &inspected,
                    )?;
                    let artifact_receipt = crate::external_native::pe_receipt(
                        &artifact_bytes,
                        "x86_64-pc-windows-msvc",
                    )?;
                    let artifact_relative = candidate
                        .artifact
                        .strip_prefix(&workspace.path)
                        .map_err(|_| "gate_artifact_outside_workspace")?
                        .to_string_lossy()
                        .replace('\\', "/");
                    if private_bytes(&staged_source, &inspected)? != EDITED
                        || private_bytes(&staged_dependency, &inspected)? != DEPENDENCY
                    {
                        return Err("gate_sealed_source_or_dependency_changed".into());
                    }
                    inspected.check()?;
                    native_sealed = true;
                    Ok((
                        prepared,
                        candidate,
                        provenance_observations,
                        proposal_digest,
                        original_limits,
                        artifact_identity,
                        artifact_handle,
                        artifact_hash,
                        tag,
                        artifact_receipt,
                        artifact_relative,
                        staged_source,
                        staged_dependency,
                        staged_source_hash,
                        dependency_hash,
                    ))
                })
                .map_err(|_| "gate_native_operation_scope_refused")??;
            if crate::operation_context::OperationContext::current().is_some() || !native_sealed {
                return Err("gate_native_scope_not_retired_before_publication".into());
            }
            stage = "new_publication_operation_after_native_seal";
            let publication_deadline = Instant::now() + Duration::from_secs(60);
            let publication_operation =
                crate::operation_context::OperationContext::new(publication_deadline);
            publication_operation.scope(|| -> Result<serde_json::Value, String> {
                let check = || publication_operation.check()
                    .map_err(|_| "gate_publication_operation_expired".into());
                let local = Budget::clipped(publication_deadline, &check)?;
                let budget = original_limits.restrict(&local);
                let fixed_deadline = budget.deadline();
                stage = "bind_same_real_candidate_for_fresh_confirmation";
                prepared.bind_validated_candidate(
                    candidate, &publication_broker, publication_cancelled.clone(), &budget,
                )?;
                let bound = prepared.native_candidate.as_ref().ok_or("gate_bound_candidate_missing")?;
                same_candidate(bound, &artifact_identity, artifact_handle, &artifact_hash, &tag, &budget)?;
                let authorization_digest = prepared.digest().to_owned();
                if authorization_digest == proposal_digest
                    || prepared.binding.proposal_digest != proposal_digest
                    || prepared.native_validation_tag.as_deref() != Some(tag.as_str())
                    || prepared.native_confirmation_window.as_ref()
                        .ok_or("gate_confirmation_window_missing")?.expires != fixed_deadline
                {
                    return Err("gate_postvalidation_confirmation_binding_missing".into());
                }
                // No old digest or precompile confirmation is used. This is cfg(test) authority only.
                let confirmation = crate::workspace_access::simulated_private_confirmation_for_test(
                    &authorization_digest,
                );
                confirmations += 1;
                let revision = prepared.grant.project.revision.clone();
                stage = "finish_fresh_exact_postvalidation_confirmation";
                let permit = prepared.finish(
                    confirmation, &publication_broker, publication_cancelled.clone(), &revision, &budget,
                )?;
                if permit.session.expires != fixed_deadline
                    || limits(permit.session.source_limits.ok_or("gate_session_limits_missing")?)
                        != limits(original_limits)
                {
                    return Err("gate_publication_session_renewed_or_widened".into());
                }
                stage = "admit_same_stored_candidate_without_publication";
                let admission = permit.admit_prevalidated(&budget)?;
                admission.check_authority(&budget)?;
                same_candidate(&admission._candidate, &artifact_identity, artifact_handle, &artifact_hash, &tag, &budget)?;
                if admission.binding.proposal_digest != proposal_digest
                    || admission.binding.authorization_digest != authorization_digest
                    || admission._permit.session.expires != fixed_deadline
                    || Some(admission._candidate.compiler_provenance.real_digest()?)
                        != provenance_observations["sha256"].as_str()
                    || limits(admission._candidate.proposal.limits) != limits(original_limits)
                    || private_bytes(&staged_source, &budget)? != EDITED
                    || private_bytes(&staged_dependency, &budget)? != DEPENDENCY
                {
                    return Err("gate_admitted_source_or_authority_binding_changed".into());
                }
                after = Some(fixture_oracle(&fixture, &budget));
                let preserved = match (&before, &after) {
                    (Some(Ok(a)), Some(Ok(z))) => a == z,
                    _ => false,
                };
                if !preserved {
                    return Err("gate_all_entry_preservation_unproven".into());
                }
                admission.check_authority(&budget)?;
                budget.check()?;
                let workspace_path = admission._candidate.workspace.clone();
                retained_admission = Some(std::mem::ManuallyDrop::new(admission));
                Ok(serde_json::json!({
                    "real_compile_first_producer_returned":true,
                    "compiler_provenance":provenance_observations,
                    "native_artifact_produced":"required by actual production seal",
                    "tag_provenance":"actual OS-only production nonce branch, source-plus-execution join required",
                    "selected_package":"native-game","selected_target":"external_logic",
                    "proposal_digest":proposal_digest,"authorization_digest":authorization_digest,
                    "postvalidation_tag_sha256":crate::sha256_hex(tag.as_bytes()),
                    "artifact":artifact_receipt,"artifact_relative":artifact_relative,
                    "artifact_identity":format!("{artifact_identity:?}"),
                    "same_original_artifact_handle_after_admit":true,
                    "artifact_hash_after_admit":artifact_hash,
                    "staged_source_sha256":staged_source_hash,"dependency_sha256":dependency_hash,
                    "source_limits":limits(original_limits),"workspace":workspace_path,
                    "original_native_parent_ms":180000,"fresh_publication_budget_at_most_ms":60000,
                    "native_parent_renewed":false,"publication_window_renewed":false,
                    "same_candidate_exact_admission":true,"source_publication_called":false,
                    "loaded":false,"activated":false,
                    "compiler_phase_raw_receipts":"NOT_EXPORTED_BY_CURRENT_VALIDATED_CANDIDATE_API"
                }))
            }).map_err(|_| "gate_publication_operation_scope_refused")?
        })();
        let originals_equal = match (&before, &after) {
            (Some(Ok(a)), Some(Ok(z))) => Some(a == z),
            _ => None,
        };
        let unresolved = workspace.as_ref().map(|value| value.lifetime.unresolved());
        let mut report = match &result {
            Ok(value) => value.clone(),
            Err(_) => serde_json::json!({"primary_stage":stage,"failure_details":"REDACTED"}),
        };
        report["schema"] =
            serde_json::json!("native_api04_real_compile_first_fixture_observations_v2");
        report["api_observations_succeeded"] = serde_json::json!(result.is_ok());
        report["real_native_calls"] = serde_json::json!(native_calls);
        report["new_postcompile_confirmations"] = serde_json::json!(confirmations);
        report["native_retired_and_sealed_before_publication_scope"] =
            serde_json::json!(native_sealed);
        report["fixture_oracle_before"] = before
            .as_ref()
            .map(fixture_all_entry_oracle::status)
            .unwrap_or_else(|| serde_json::json!({"status":"NOT_ATTEMPTED"}));
        report["fixture_oracle_after"] = after
            .as_ref()
            .map(fixture_all_entry_oracle::status)
            .unwrap_or_else(
                || serde_json::json!({"status":"UNAVAILABLE_NO_RENEWAL_AFTER_FAILURE"}),
            );
        report["working_source_DLL_userdata_ungranted_all_entries_equal"] =
            serde_json::json!(originals_equal);
        report["workspace_lifetime_unresolved"] = serde_json::json!(unresolved);
        report["controller_retained"] = serde_json::json!(controller.retained());
        report["elapsed_ms"] = serde_json::json!(started.elapsed().as_millis());
        report["authority"] = serde_json::json!("SIMULATED_PRIVATE_POSTCOMPILE_CONFIRMATION_ONLY");
        report["evidence_holders"] = serde_json::json!(
            "Original native parent/context/controller/broker/workspace and successful admission retained until HOST_EXIT; holder existence is not retirement proof"
        );
        report["remaining_boundary"] = serde_json::json!(
            "Exact source-plus-real-producer/artifact/retirement join still required; no attended approval, source publication, DLL load, gdext, dependency trust or full native qualification"
        );
        let bytes = serde_json::to_vec_pretty(&report).unwrap();
        assert!(bytes.len() <= 65536);
        let receipt = fixture
            .base
            .join("actual-native-api04-compile-first-validation.json");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&receipt)
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.sync_all().unwrap();
        println!("native_api04_real_producer_receipt={}", receipt.display());
        assert!(
            result.is_ok(),
            "real compile-first observations refused; receipt retained"
        );
        assert_eq!(native_calls, 1);
        assert_eq!(confirmations, 1);
        assert!(native_sealed);
        assert_eq!(originals_equal, Some(true));
        assert_eq!(unresolved, Some(false));
        assert_eq!(controller.retained(), 0);
        assert!(retained_admission.is_some());
    }
}
