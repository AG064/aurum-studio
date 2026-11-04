//! Private Windows capabilities for sealed source and fixed revision state.
//! Root bootstrap is checked once against existing authority. Every subsequent
//! name lookup, enumeration and mutation is relative to a retained object.
use super::{Budget, DirectoryIdentity, Fingerprint, Mode, Revision, VerifiedGrant};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::{
    fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle},
};
use std::path::Path;

type Result<T> = std::result::Result<T, String>;
type Handle = *mut std::ffi::c_void;
const READER: u32 = 0x0010_0081;
const MUTATION_PARENT: u32 = 0x0010_00a0;
const FROZEN: u32 = READER | 0x0001_0000;
const OWNED: u32 = FROZEN | 2;
const SHARE_READ: u32 = 1;
const SHARE_RW: u32 = 3;
const OPEN_REPARSE: u32 = 0x0020_0000;
const DONT_REPARSE: u32 = 0x1000;
const PAGE_BYTES: usize = 64 * 1024;
const CAPTURE_BYTES: u64 = crate::revisions::MAX_FILE_BYTES as u64;

#[repr(C)]
struct Unicode {
    length: u16,
    maximum: u16,
    buffer: *mut u16,
}
#[repr(C)]
struct Attributes {
    length: u32,
    root: Handle,
    name: *mut Unicode,
    flags: u32,
    security: Handle,
    qos: Handle,
}
#[repr(C)]
struct IoStatus {
    status: usize,
    information: usize,
}
#[repr(C)]
struct Rename {
    flags_or_replace: u32,
    root: Handle,
    length: u32,
    name: [u16; 1],
}
#[repr(C)]
struct Disposition {
    delete_file: u8,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        handle: *mut Handle,
        access: u32,
        attributes: *mut Attributes,
        io: *mut IoStatus,
        allocation: *const i64,
        file_attributes: u32,
        sharing: u32,
        disposition: u32,
        options: u32,
        ea: *const std::ffi::c_void,
        ea_bytes: u32,
    ) -> i32;
    fn NtSetInformationFile(
        file: Handle,
        io: *mut IoStatus,
        information: Handle,
        bytes: u32,
        class: u32,
    ) -> i32;
    fn NtQueryDirectoryFile(
        file: Handle,
        event: Handle,
        apc: Handle,
        context: Handle,
        io: *mut IoStatus,
        information: Handle,
        bytes: u32,
        class: u32,
        single: u8,
        name: *mut Unicode,
        restart: u8,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetFileInformationByHandle(file: Handle, class: u32, data: Handle, bytes: u32) -> i32;
}
fn native_error(api: &str, status: i32) -> String {
    format!("external_namespace_{api}_{:08x}", status as u32)
}
fn abi() -> Result<()> {
    let actual = (
        std::mem::size_of::<Unicode>(),
        std::mem::size_of::<Attributes>(),
        std::mem::size_of::<IoStatus>(),
        std::mem::offset_of!(Rename, root),
        std::mem::offset_of!(Rename, length),
        std::mem::offset_of!(Rename, name),
    );
    let expected = if std::mem::size_of::<usize>() == 8 {
        (16, 48, 16, 8, 16, 20)
    } else {
        (8, 24, 8, 4, 8, 12)
    };
    if actual != expected || std::mem::size_of::<Disposition>() != 1 {
        return Err("external_namespace_abi_refused".into());
    }
    Ok(())
}
fn leaf(name: &str) -> Result<Vec<u16>> {
    if name.is_empty()
        || name.len() > 120
        || matches!(name, "." | "..")
        || name.ends_with('.')
        || super::windows_device_name(name)
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("external_namespace_single_leaf_required".into());
    }
    Ok(name.encode_utf16().collect())
}
fn cargo_component(name: &str) -> bool {
    name.eq_ignore_ascii_case(".cargo") || name.eq_ignore_ascii_case("build.rs")
}
fn visible_source(path: &str) -> bool {
    // Windows aliases must not turn an excluded component into an aperture.
    super::public_source(path) && super::public_source(&path.to_ascii_lowercase())
}
fn source_aperture(path: &str, root: bool) -> Result<()> {
    if path.is_empty() && root {
        return Ok(());
    }
    let components = super::relative(path, false)?;
    if components.iter().any(|name| cargo_component(name)) {
        return Err("unsupported_cargo_configuration_or_build_script".into());
    }
    if !visible_source(path) {
        return Err("external_namespace_source_aperture_refused".into());
    }
    Ok(())
}
fn child_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}
fn ordinary(file: &File, directory: bool) -> Result<()> {
    let metadata = file
        .metadata()
        .map_err(|_| "external_namespace_metadata_unavailable")?;
    super::ordinary(&metadata)?;
    if metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
        return Err("external_namespace_kind_changed".into());
    }
    Ok(())
}
fn open(
    parent: &File,
    name: &str,
    access: u32,
    sharing: u32,
    create: bool,
    directory: bool,
) -> Result<File> {
    let mut name = leaf(name)?;
    let length = u16::try_from(
        name.len()
            .checked_mul(2)
            .ok_or("external_namespace_name_overflow")?,
    )
    .map_err(|_| "external_namespace_name_overflow")?;
    let mut text = Unicode {
        length,
        maximum: length,
        buffer: name.as_mut_ptr(),
    };
    let mut attributes = Attributes {
        length: std::mem::size_of::<Attributes>() as u32,
        root: parent.as_raw_handle(),
        name: &mut text,
        flags: DONT_REPARSE,
        security: std::ptr::null_mut(),
        qos: std::ptr::null_mut(),
    };
    let mut io = IoStatus {
        status: 0,
        information: 0,
    };
    let mut raw = std::ptr::null_mut();
    let options = 0x20
        | if create && directory {
            1
        } else {
            OPEN_REPARSE | if directory { 0 } else { 0x40 }
        };
    let status = unsafe {
        NtCreateFile(
            &mut raw,
            access,
            &mut attributes,
            &mut io,
            std::ptr::null(),
            0x80,
            sharing,
            if create { 2 } else { 1 },
            options,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 {
        return Err(native_error("open", status));
    }
    if raw.is_null() || raw as isize == -1 {
        return Err("external_namespace_handle_missing".into());
    }
    // A successful call transfers exactly one kernel handle to File.
    let file = unsafe { File::from_raw_handle(raw) };
    if io.information != if create { 2 } else { 1 } {
        return Err("external_namespace_disposition_refused".into());
    }
    ordinary(&file, directory)?;
    Ok(file)
}

#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    file: u64,
    files: usize,
    entries: usize,
}
trait Gate {
    // Intentionally no Send/Sync contract. Capabilities borrow this private
    // trait object, so enumeration cannot be shared across worker threads.
    fn check(&self) -> Result<()>;
    fn check_scope(&self) -> Result<()> {
        self.check()
    }
    fn limits(&self) -> Limits;
    fn deadline(&self) -> std::time::Instant;
}
struct ReadBudget<'a, 'b>(&'a Budget<'b>);
impl Gate for ReadBudget<'_, '_> {
    fn check(&self) -> Result<()> {
        self.0.check()
    }
    fn deadline(&self) -> std::time::Instant {
        self.0.deadline
    }
    fn limits(&self) -> Limits {
        Limits {
            bytes: self.0.max_bytes,
            file: self.0.max_file,
            files: self.0.max_files,
            entries: self.0.max_entries,
        }
    }
}
/// Sanitized, read-only internal scans. No mutation conversion exists. Callers
/// retain their ordered leases and original identity guards across the scan.
pub(super) fn scan_source(
    path: &Path,
    budget: &Budget<'_>,
    usage: &mut super::Usage,
) -> Result<(Vec<super::Entry>, Fingerprint)> {
    let (path, identity) = super::canonical_directory(path, budget)?;
    let root = bootstrap(&path, &identity, READER, budget)?;
    let gate = ReadBudget(budget);
    readonly_barrier(&root, &gate)?;
    let mut snapshot = TreeSnapshot {
        gate: &gate,
        fingerprint: super::fingerprint_entries(&[]),
        directories: Vec::new(),
        files: Vec::new(),
        entries: 0,
        bytes: 0,
    };
    let mut rows = Vec::new();
    snapshot.visit(root, "", "", 0, &mut rows)?;
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    snapshot.check()?;
    usage.bytes = usage
        .bytes
        .checked_add(snapshot.bytes)
        .ok_or("external_namespace_byte_overflow")?;
    usage.files = usage
        .files
        .checked_add(snapshot.files.len())
        .ok_or("external_namespace_file_overflow")?;
    usage.entries = usage
        .entries
        .checked_add(snapshot.entries)
        .ok_or("external_namespace_entry_overflow")?;
    if usage.bytes > budget.max_bytes
        || usage.files > budget.max_files
        || usage.entries > budget.max_entries
    {
        return Err("external_namespace_scan_aggregate_limit".into());
    }
    let fingerprint = super::fingerprint_entries(&rows);
    Ok((rows, fingerprint))
}
pub(super) fn read_source(
    path: &Path,
    relative: &str,
    budget: &Budget<'_>,
) -> Result<Option<Vec<u8>>> {
    let (path, identity) = super::canonical_directory(path, budget)?;
    let root = bootstrap(&path, &identity, READER, budget)?;
    let value = source_observe(&root, relative, &ReadBudget(budget)).map_err(|e| {
        if e == "external_namespace_file_byte_limit" {
            "source_file_byte_limit".to_owned()
        } else {
            e
        }
    })?;
    if value
        .as_ref()
        .is_some_and(|v| std::str::from_utf8(&v.1).is_err())
    {
        return Err("source_target_utf8_required".into());
    }
    Ok(value.map(|v| v.1))
}
pub(super) fn check_root_barrier(
    path: &Path,
    identity: &DirectoryIdentity,
    budget: &Budget<'_>,
) -> Result<()> {
    readonly_barrier(
        &bootstrap(path, identity, READER, budget)?,
        &ReadBudget(budget),
    )
}
enum Proof<'a> {
    Dependency(&'a VerifiedGrant, &'a Revision),
    Publication(&'a super::SourceAdmission),
    Repair(&'a super::SourceRepairAdmission),
    Inspection(&'a super::PreparedGrant),
}
struct Context<'a, 'b> {
    proof: Proof<'a>,
    budget: &'a Budget<'b>,
    imported: Option<(File, DirectoryIdentity)>,
    imported_capture: Option<ImportedCapture>,
    _leases: Vec<crate::content_lock::ContentLock>,
}
impl Gate for Context<'_, '_> {
    fn check(&self) -> Result<()> {
        self.budget.check()?;
        match &self.proof {
            Proof::Dependency(grant, revision) => {
                if grant.mode != Mode::DependencyRead || &grant.revision != *revision {
                    return Err("external_namespace_read_grant_mismatch".into());
                }
                let (file, expected) = self
                    .imported
                    .as_ref()
                    .ok_or("external_namespace_import_guard_missing")?;
                ordinary(file, true)?;
                if &super::file_identity(file)? != expected {
                    return Err("external_namespace_import_identity_changed".into());
                }
                // Both roots passed the unresolved-revision barrier while the
                // ordered shared leases were acquired. The leases stay held.
            }
            Proof::Publication(admission) => {
                admission._permit.session.check(self.budget)?;
                retained_grant(&admission._permit.prepared.grant)?;
            }
            Proof::Repair(admission) => {
                admission._permit.session.check(self.budget)?;
                retained_grant(&admission._permit.prepared.grant)?;
            }
            Proof::Inspection(grant) => retained_grant(grant)?,
        }
        self.budget.check()
    }
    fn limits(&self) -> Limits {
        Limits {
            bytes: self.budget.max_bytes,
            file: self.budget.max_file,
            files: self.budget.max_files,
            entries: self.budget.max_entries,
        }
    }
    fn check_scope(&self) -> Result<()> {
        self.check()?;
        if let Some(capture) = &self.imported_capture {
            for (directory, expected) in &capture.directories {
                let mut actual = enumerate(directory, self)?;
                actual.sort_by(|a, b| a.name.cmp(&b.name));
                if &actual != expected {
                    return Err("external_namespace_imported_revision_changed".into());
                }
            }
            for (file, expected) in &capture.files {
                self.check()?;
                if &super::stamp(file)? != expected {
                    return Err("external_namespace_imported_revision_changed".into());
                }
            }
        }
        self.check()
    }
    fn deadline(&self) -> std::time::Instant {
        self.budget.deadline
    }
}
fn retained_grant(grant: &super::PreparedGrant) -> Result<()> {
    ordinary(&grant.project_handle, true)?;
    ordinary(&grant.root_handle, true)?;
    if super::file_identity(&grant.project_handle)? != grant.project.identity
        || super::file_identity(&grant.root_handle)? != grant.identity
    {
        return Err("external_namespace_retained_authority_identity_changed".into());
    }
    Ok(())
}
struct Directory {
    file: File,
    identity: DirectoryIdentity,
}
impl Directory {
    fn check(&self) -> Result<()> {
        ordinary(&self.file, true)?;
        if super::file_identity(&self.file)? != self.identity {
            return Err("external_namespace_directory_identity_changed".into());
        }
        Ok(())
    }
    fn duplicate(&self) -> Result<Self> {
        self.check()?;
        // Duplicate handles share the kernel file object's enumeration cursor.
        // All queries are synchronous and restart scans on this single thread;
        // this is not an independent iterator or a concurrency guarantee.
        Ok(Self {
            file: self
                .file
                .try_clone()
                .map_err(|_| "external_namespace_duplicate_failed")?,
            identity: self.identity.clone(),
        })
    }
    fn child(&self, name: &str, access: u32) -> Result<Self> {
        self.check()?;
        let file = open(&self.file, name, access, SHARE_RW, false, true)?;
        let identity = super::file_identity(&file)?;
        if identity.volume != self.identity.volume {
            return Err("external_namespace_cross_volume".into());
        }
        self.check()?;
        Ok(Self { file, identity })
    }
}
fn bootstrap(
    path: &Path,
    expected: &DirectoryIdentity,
    access: u32,
    budget: &Budget<'_>,
) -> Result<Directory> {
    budget.check()?;
    abi()?;
    // This is the sole absolute root bootstrap. It is not a retry or fallback
    // for a failed relative operation. Existing authority fixes its identity.
    let checked = crate::snapshot::open_checked_writable_directory_identity(path, budget.deadline)?;
    if &super::file_identity(&checked)? != expected {
        return Err("external_namespace_bootstrap_identity_changed".into());
    }
    let file = OpenOptions::new()
        .access_mode(access)
        .share_mode(SHARE_RW)
        .custom_flags(0x0220_0000)
        .open(path)
        .map_err(|_| "external_namespace_root_open_refused")?;
    crate::snapshot::check_opened_path(&file, path, budget.deadline)?;
    ordinary(&file, true)?;
    let identity = super::file_identity(&file)?;
    if &identity != expected {
        return Err("external_namespace_bootstrap_identity_changed".into());
    }
    budget.check()?;
    Ok(Directory { file, identity })
}
fn walk(root: &Directory, path: &str, gate: &dyn Gate) -> Result<Vec<Directory>> {
    gate.check_scope()?;
    let mut directories = vec![root.duplicate()?];
    if !path.is_empty() {
        for name in super::relative(path, false)? {
            gate.check()?;
            check_spelling(
                directories
                    .last()
                    .ok_or("external_namespace_parent_missing")?,
                name,
                gate,
            )?;
            let child = directories
                .last()
                .ok_or("external_namespace_parent_missing")?
                .child(name, READER)?;
            directories.push(child);
        }
    }
    gate.check_scope()?;
    Ok(directories)
}
fn mutation_role(chain: &[Directory], path: &str) -> Result<Directory> {
    let name = path
        .rsplit('/')
        .next()
        .ok_or("external_namespace_parent_missing")?;
    let parent = chain
        .get(
            chain
                .len()
                .checked_sub(2)
                .ok_or("external_namespace_parent_missing")?,
        )
        .ok_or("external_namespace_parent_missing")?;
    let directory = parent.child(name, MUTATION_PARENT)?;
    if directory.identity
        != chain
            .last()
            .ok_or("external_namespace_parent_missing")?
            .identity
    {
        return Err("external_namespace_paired_parent_identity_changed".into());
    }
    Ok(directory)
}

/// Has no mutation conversion, constructor or transferable write token.
pub(super) struct ReadNamespace<'a, 'b> {
    context: Context<'a, 'b>,
    root: Directory,
}
impl<'a, 'b> ReadNamespace<'a, 'b> {
    pub(super) fn dependency(
        grant: &'a VerifiedGrant,
        revision: &'a Revision,
        budget: &'a Budget<'b>,
    ) -> Result<Self> {
        if grant.mode != Mode::DependencyRead || &grant.revision != revision {
            return Err("external_namespace_read_grant_mismatch".into());
        }
        let mut ordered = [
            (&grant.imported, &grant.imported_identity),
            (&grant.root, &grant.identity),
        ];
        ordered.sort_by_key(|(path, identity)| (super::key(path), (**identity).clone()));
        if super::overlaps(&grant.imported, &grant.root) {
            return Err("external_namespace_read_alias".into());
        }
        let mut leases = Vec::new();
        for (path, expected) in ordered {
            if &super::canonical_directory(path, budget)?.1 != expected {
                return Err("external_namespace_read_identity_changed".into());
            }
            leases.push(crate::content_lock::ContentLock::shared(path)?);
        }
        let imported = bootstrap(&grant.imported, &grant.imported_identity, READER, budget)?;
        let root = bootstrap(&grant.root, &grant.identity, READER, budget)?;
        let imported_directory = imported.duplicate()?;
        let mut context = Context {
            proof: Proof::Dependency(grant, revision),
            budget,
            imported: Some((imported.file, imported.identity)),
            imported_capture: None,
            _leases: leases,
        };
        context.check()?;
        readonly_barrier(&imported_directory, &context)?;
        readonly_barrier(&root, &context)?;
        // Establish the actual imported revision under the leases already held
        // above. No current_revision() call and no nested content lock.
        let capture = {
            let reader = ReadDirectory {
                gate: &context,
                chain: vec![imported_directory],
                prefix: String::new(),
            };
            let snapshot = reader.snapshot()?;
            if snapshot.fingerprint.sha256 != revision.as_str() {
                return Err("external_namespace_imported_revision_mismatch".into());
            }
            ImportedCapture {
                directories: snapshot.directories,
                files: snapshot.files,
            }
        };
        context.imported_capture = Some(capture);
        context.check_scope()?;
        Ok(Self { context, root })
    }
    pub(super) fn directory(&self, path: &str) -> Result<ReadDirectory<'_>> {
        self.context.check_scope()?;
        source_aperture(path, true)?;
        Ok(ReadDirectory {
            gate: &self.context,
            chain: walk(&self.root, path, &self.context)?,
            prefix: path.into(),
        })
    }
}

/// Borrows the admitted opaque permit and its already acquired leases. There
/// is no grant-based constructor, caller-supplied authority or deserialization.
pub(super) struct WriteNamespace<'a, 'b> {
    context: Context<'a, 'b>,
    root: Directory,
    reader_root: Directory,
    state: std::cell::RefCell<Option<StateRoot>>,
    created: std::cell::RefCell<std::collections::BTreeMap<String, Directory>>,
    immutable_records: std::cell::RefCell<Vec<(File, super::Stamp)>>,
}
impl<'a, 'b> WriteNamespace<'a, 'b> {
    pub(super) fn publication(
        admission: &'a super::SourceAdmission,
        budget: &'a Budget<'b>,
    ) -> Result<Self> {
        let prepared = &admission._permit.prepared;
        if admission.root != prepared.grant.root
            || admission.binding != prepared.binding
            || admission.changes != prepared.changes
        {
            return Err("external_namespace_publication_mirror_changed".into());
        }
        if budget.deadline > admission._permit.session.expires {
            return Err("external_namespace_budget_renewal_refused".into());
        }
        admission.check_authority(budget)?;
        let context = Context {
            proof: Proof::Publication(admission),
            budget,
            imported: None,
            imported_capture: None,
            _leases: Vec::new(),
        };
        context.check()?;
        let root = bootstrap(
            &prepared.grant.root,
            &admission._permit.prepared.grant.identity,
            MUTATION_PARENT,
            budget,
        )?;
        let reader_root = bootstrap(&prepared.grant.root, &root.identity, READER, budget)?;
        Ok(Self {
            context,
            root,
            reader_root,
            state: std::cell::RefCell::new(None),
            created: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            immutable_records: std::cell::RefCell::new(Vec::new()),
        })
    }
    pub(super) fn repair(
        admission: &'a super::SourceRepairAdmission,
        budget: &'a Budget<'b>,
    ) -> Result<Self> {
        let prepared = &admission._permit.prepared;
        if admission.root != prepared.grant.root
            || admission.revision != prepared.revision
            || admission.undo != prepared.undo
            || admission.scope != prepared.scope
            || admission.reverse_cleanup != prepared.reverse_cleanup
            || admission.authorization_digest != prepared.digest
        {
            return Err("external_namespace_repair_mirror_changed".into());
        }
        if budget.deadline > admission._permit.session.expires {
            return Err("external_namespace_budget_renewal_refused".into());
        }
        admission.check_authority(budget)?;
        let context = Context {
            proof: Proof::Repair(admission),
            budget,
            imported: None,
            imported_capture: None,
            _leases: Vec::new(),
        };
        context.check()?;
        let root = bootstrap(
            &prepared.grant.root,
            &admission._permit.prepared.grant.identity,
            MUTATION_PARENT,
            budget,
        )?;
        let reader_root = bootstrap(&prepared.grant.root, &root.identity, READER, budget)?;
        Ok(Self {
            context,
            root,
            reader_root,
            state: std::cell::RefCell::new(None),
            created: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            immutable_records: std::cell::RefCell::new(Vec::new()),
        })
    }
    pub(super) fn reader_directory(&self, path: &str) -> Result<ReadDirectory<'_>> {
        self.context.check_scope()?;
        source_aperture(path, true)?;
        Ok(ReadDirectory {
            gate: &self.context,
            chain: walk(&self.reader_root, path, &self.context)?,
            prefix: path.into(),
        })
    }
    pub(super) fn target(&self, path: &str) -> Result<MutationDirectory<'_>> {
        self.context.check()?;
        let (binding, publication) = match &self.context.proof {
            Proof::Publication(admission) => (&admission._permit.prepared.binding, true),
            Proof::Repair(admission) => (&admission._permit.prepared.scope.binding, false),
            Proof::Dependency(_, _) | Proof::Inspection(_) => {
                return Err("external_namespace_mutation_authority_required".into())
            }
        };
        super::source_target(&binding.member_src, path)?;
        source_aperture(path, false)?;
        let row = binding
            .targets
            .iter()
            .find(|row| row.path == path)
            .ok_or("external_namespace_target_not_approved")?;
        let parent = super::directory_of(path);
        let (ancestor, directory) = parent.rsplit_once('/').unwrap_or(("", parent));
        let chain = walk(&self.reader_root, ancestor, &self.context)?;
        let anchor = chain.last().ok_or("external_namespace_parent_missing")?;
        let reader = anchor.child(directory, READER)?;
        let mutation = anchor.child(directory, MUTATION_PARENT)?;
        if reader.identity != mutation.identity {
            return Err("external_namespace_paired_parent_identity_changed".into());
        }
        let allowed: Vec<String> = [&row.before, &row.after]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        let writable = if publication {
            row.after.iter().cloned().collect()
        } else {
            allowed.clone()
        };
        let staging_chain = walk(&self.reader_root, &binding.member_src, &self.context)?;
        let staging = mutation_role(&staging_chain, &binding.member_src)?;
        Ok(MutationDirectory {
            gate: &self.context,
            chain,
            reader,
            mutation,
            staging_chain,
            staging,
            parent_name: directory.into(),
            target: path
                .rsplit('/')
                .next()
                .ok_or("external_namespace_target_missing")?
                .into(),
            allowed,
            writable,
        })
    }
}

pub(super) enum Capture {
    HashOnly,
    Bytes,
}
pub(super) struct ReadResult {
    pub(super) identity: DirectoryIdentity,
    pub(super) sha256: String,
    pub(super) bytes: u64,
    pub(super) contents: Option<Vec<u8>>,
}
pub(super) struct ReadDirectory<'a> {
    gate: &'a dyn Gate,
    chain: Vec<Directory>,
    prefix: String,
}
impl ReadDirectory<'_> {
    fn current(&self) -> Result<&Directory> {
        self.chain
            .last()
            .ok_or("external_namespace_parent_missing".into())
    }
    fn check(&self) -> Result<()> {
        self.gate.check_scope()?;
        source_aperture(&self.prefix, true)?;
        for directory in &self.chain {
            directory.check()?;
        }
        Ok(())
    }
    pub(super) fn read(&self, name: &str, maximum: u64, capture: Capture) -> Result<ReadResult> {
        self.check()?;
        leaf(name)?;
        source_aperture(&child_path(&self.prefix, name), false)?;
        let mut file = open(
            &self.current()?.file,
            name,
            READER,
            SHARE_READ,
            false,
            false,
        )?;
        let result = read_same_object(&mut file, maximum, capture, self.gate)?;
        self.check()?;
        Ok(result)
    }
    pub(super) fn entries(&self) -> Result<Vec<DirectoryEntry>> {
        self.check()?;
        let mut result = Vec::new();
        for entry in enumerate(self.current()?, self.gate)? {
            let full_path = child_path(&self.prefix, &entry.name);
            super::relative(&full_path, false)?;
            if cargo_component(&entry.name) {
                return Err("unsupported_cargo_configuration_or_build_script".into());
            }
            if visible_source(&full_path) {
                result.push(entry);
            }
        }
        self.check()?;
        Ok(result)
    }
    pub(super) fn snapshot(&self) -> Result<TreeSnapshot<'_>> {
        self.check()?;
        let mut snapshot = TreeSnapshot {
            gate: self.gate,
            fingerprint: super::fingerprint_entries(&[]),
            directories: Vec::new(),
            files: Vec::new(),
            entries: 0,
            bytes: 0,
        };
        let mut rows = Vec::new();
        let depth = if self.prefix.is_empty() {
            0
        } else {
            self.prefix.split('/').count()
        };
        snapshot.visit(
            self.current()?.duplicate()?,
            "",
            &self.prefix,
            depth,
            &mut rows,
        )?;
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        snapshot.fingerprint = super::fingerprint_entries(&rows);
        snapshot.check()?;
        self.check()?;
        Ok(snapshot)
    }
}
fn read_same_object(
    file: &mut File,
    maximum: u64,
    capture: Capture,
    gate: &dyn Gate,
) -> Result<ReadResult> {
    gate.check()?;
    if maximum > gate.limits().file
        || (matches!(capture, Capture::Bytes) && maximum > CAPTURE_BYTES)
    {
        return Err("external_namespace_read_bound_refused".into());
    }
    let before = super::stamp(file)?;
    if before.bytes > maximum {
        return Err("external_namespace_file_byte_limit".into());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "external_namespace_seek_failed")?;
    let mut digest = crate::hash::Sha256::new();
    let mut contents = if matches!(capture, Capture::Bytes) {
        Some(Vec::new())
    } else {
        None
    };
    let mut length = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        gate.check()?;
        let capacity = before
            .bytes
            .saturating_sub(length)
            .saturating_add(1)
            .min(buffer.len() as u64) as usize;
        let count = file
            .read(&mut buffer[..capacity])
            .map_err(|_| "external_namespace_read_failed")?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .ok_or("external_namespace_byte_overflow")?;
        if length > before.bytes {
            return Err("external_namespace_file_growth".into());
        }
        digest.update(&buffer[..count]);
        if let Some(contents) = &mut contents {
            contents.extend_from_slice(&buffer[..count]);
        }
    }
    if length != before.bytes || super::stamp(file)? != before {
        return Err("external_namespace_read_changed".into());
    }
    gate.check()?;
    Ok(ReadResult {
        identity: before.identity,
        sha256: digest.hex(),
        bytes: length,
        contents,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DirectoryEntry {
    pub(super) name: String,
    pub(super) directory: bool,
    pub(super) bytes: u64,
}
fn enumerate(directory: &Directory, gate: &dyn Gate) -> Result<Vec<DirectoryEntry>> {
    directory.check()?;
    let rows = enumerate_pages(directory, gate)?;
    directory.check()?;
    Ok(rows)
}
fn enumerate_pages(directory: &Directory, gate: &dyn Gate) -> Result<Vec<DirectoryEntry>> {
    let mut pages = vec![0u64; PAGE_BYTES / 8]; // FILE_DIRECTORY_INFORMATION requires 8-byte alignment.
    let mut rows = Vec::new();
    let mut names = BTreeSet::new();
    let mut restart = 1;
    for _ in 0..=gate.limits().entries {
        gate.check()?;
        let mut io = IoStatus {
            status: 0,
            information: 0,
        };
        let status = unsafe {
            NtQueryDirectoryFile(
                directory.file.as_raw_handle(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut io,
                pages.as_mut_ptr().cast(),
                PAGE_BYTES as u32,
                1,
                0,
                std::ptr::null_mut(),
                restart,
            )
        };
        restart = 0;
        if status as u32 == 0x8000_0006 {
            return Ok(rows);
        }
        if status != 0 {
            return Err(native_error("enumerate", status));
        }
        if io.information < 64 || io.information > PAGE_BYTES {
            return Err("external_namespace_enumeration_page_refused".into());
        }
        // Bounds checked below before interpreting any variable-length member.
        let bytes =
            unsafe { std::slice::from_raw_parts(pages.as_ptr().cast::<u8>(), io.information) };
        for entry in decode_page(bytes, || gate.check())? {
            if !names.insert(entry.name.to_ascii_lowercase()) {
                return Err("external_namespace_case_alias_or_duplicate".into());
            }
            if rows.len() >= gate.limits().entries {
                return Err("external_namespace_entry_limit".into());
            }
            rows.push(entry);
        }
    }
    Err("external_namespace_enumeration_page_limit".into())
}
fn decode_page(bytes: &[u8], mut check: impl FnMut() -> Result<()>) -> Result<Vec<DirectoryEntry>> {
    if bytes.len() < 64 || bytes.len() > PAGE_BYTES {
        return Err("external_namespace_enumeration_page_refused".into());
    }
    let mut rows = Vec::new();
    let mut offset = 0usize;
    loop {
        check()?;
        let header = bytes
            .get(
                offset
                    ..offset
                        .checked_add(64)
                        .ok_or("external_namespace_enumeration_overflow")?,
            )
            .ok_or("external_namespace_enumeration_header")?;
        let word = |start: usize| u32::from_le_bytes(header[start..start + 4].try_into().unwrap());
        let next = word(0) as usize;
        let name_bytes = word(60) as usize;
        let end = offset
            .checked_add(64)
            .and_then(|v| v.checked_add(name_bytes))
            .ok_or("external_namespace_enumeration_overflow")?;
        if name_bytes == 0
            || !name_bytes.is_multiple_of(2)
            || end > bytes.len()
            || (next != 0
                && (!next.is_multiple_of(8)
                    || next < 64 + name_bytes
                    || next - (64 + name_bytes) > 7
                    || offset.checked_add(next).is_none_or(|v| v > bytes.len())))
        {
            return Err("external_namespace_enumeration_bounds".into());
        }
        let units: Vec<u16> = bytes[offset + 64..end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let name = String::from_utf16(&units).map_err(|_| "external_namespace_filename_utf16")?;
        if !matches!(name.as_str(), "." | "..") {
            leaf(&name)?;
            let attributes = word(56);
            if attributes & 0x400 != 0 {
                return Err("external_namespace_enumerated_reparse".into());
            }
            let length = i64::from_le_bytes(header[40..48].try_into().unwrap());
            let length = u64::try_from(length).map_err(|_| "external_namespace_negative_length")?;
            rows.push(DirectoryEntry {
                name,
                directory: attributes & 0x10 != 0,
                bytes: length,
            });
        }
        if next == 0 {
            if bytes.len() - end > 7 {
                return Err("external_namespace_enumeration_terminal_padding".into());
            }
            return Ok(rows);
        }
        offset += next;
    }
}
pub(super) struct TreeSnapshot<'a> {
    gate: &'a dyn Gate,
    pub(super) fingerprint: Fingerprint,
    directories: Vec<(Directory, Vec<DirectoryEntry>)>,
    files: Vec<(File, super::Stamp)>,
    entries: usize,
    bytes: u64,
}
struct ImportedCapture {
    directories: Vec<(Directory, Vec<DirectoryEntry>)>,
    files: Vec<(File, super::Stamp)>,
}
impl TreeSnapshot<'_> {
    fn visit(
        &mut self,
        directory: Directory,
        prefix: &str,
        aperture: &str,
        depth: usize,
        rows: &mut Vec<super::Entry>,
    ) -> Result<()> {
        self.gate.check()?;
        if depth > 24 {
            return Err("external_namespace_depth_limit".into());
        }
        let mut listing = enumerate(&directory, self.gate)?;
        listing.sort_by(|a, b| a.name.cmp(&b.name));
        self.entries = self
            .entries
            .checked_add(listing.len())
            .ok_or("external_namespace_entry_overflow")?;
        if self.entries > self.gate.limits().entries {
            return Err("external_namespace_entry_limit".into());
        }
        for item in &listing {
            self.gate.check()?;
            let path = child_path(prefix, &item.name);
            let full_path = child_path(aperture, &item.name);
            if path.len() > 1024 || full_path.len() > 1024 {
                return Err("external_namespace_path_limit".into());
            }
            super::relative(&full_path, false)?;
            if cargo_component(&item.name) {
                return Err("unsupported_cargo_configuration_or_build_script".into());
            }
            if !visible_source(&full_path) {
                continue;
            }
            if item.directory {
                let child = directory.child(&item.name, READER)?;
                rows.push(super::Entry {
                    path: path.clone(),
                    bytes: 0,
                    sha256: String::new(),
                    directory: true,
                });
                self.visit(child, &path, &full_path, depth + 1, rows)?;
            } else {
                if self.files.len() >= self.gate.limits().files {
                    return Err("external_namespace_file_limit".into());
                }
                let mut file = open(
                    &directory.file,
                    &item.name,
                    READER,
                    SHARE_READ,
                    false,
                    false,
                )?;
                let read = read_same_object(
                    &mut file,
                    self.gate.limits().file,
                    Capture::HashOnly,
                    self.gate,
                )?;
                if read.bytes != item.bytes {
                    return Err("external_namespace_enumerated_length_changed".into());
                }
                self.bytes = self
                    .bytes
                    .checked_add(read.bytes)
                    .ok_or("external_namespace_byte_overflow")?;
                if self.bytes > self.gate.limits().bytes {
                    return Err("external_namespace_aggregate_byte_limit".into());
                }
                let stamp = super::stamp(&file)?;
                rows.push(super::Entry {
                    path,
                    bytes: read.bytes,
                    sha256: read.sha256,
                    directory: false,
                });
                self.files.push((file, stamp));
            }
        }
        self.directories.push((directory, listing));
        Ok(())
    }
    pub(super) fn check(&self) -> Result<()> {
        self.gate.check()?;
        for (directory, expected) in &self.directories {
            let mut actual = enumerate(directory, self.gate)?;
            actual.sort_by(|a, b| a.name.cmp(&b.name));
            if &actual != expected {
                return Err("external_namespace_directory_listing_changed".into());
            }
        }
        for (file, expected) in &self.files {
            self.gate.check()?;
            if &super::stamp(file)? != expected {
                return Err("external_namespace_snapshot_leaf_changed".into());
            }
        }
        self.gate.check()
    }
}

pub(super) struct MutationDirectory<'a> {
    gate: &'a dyn Gate,
    chain: Vec<Directory>,
    reader: Directory,
    mutation: Directory,
    staging_chain: Vec<Directory>,
    staging: Directory,
    target: String,
    allowed: Vec<String>,
    writable: Vec<String>,
    parent_name: String,
}
impl MutationDirectory<'_> {
    fn check(&self) -> Result<()> {
        self.gate.check_scope()?;
        for directory in &self.chain {
            directory.check()?;
        }
        for directory in &self.staging_chain {
            directory.check()?;
        }
        self.staging.check()?;
        self.reader.check()?;
        self.mutation.check()?;
        if self.reader.identity != self.mutation.identity {
            return Err("external_namespace_paired_parent_identity_changed".into());
        }
        Ok(())
    }
    pub(super) fn read_target(&self, capture: Capture) -> Result<ReadResult> {
        self.read_target_after_check(capture, || Ok(()))
    }
    fn read_target_after_check(
        &self,
        capture: Capture,
        before_io: impl FnOnce() -> Result<()>,
    ) -> Result<ReadResult> {
        self.check()?;
        before_io()?;
        let mut file = open(
            &self.reader.file,
            &self.target,
            READER,
            SHARE_READ,
            false,
            false,
        )?;
        let result = read_same_object(&mut file, CAPTURE_BYTES, capture, self.gate)?;
        self.check()?;
        Ok(result)
    }
    pub(super) fn open_expected(&self, expected: &str) -> Result<OwnedLeaf<'_>> {
        self.check()?;
        if !self.allowed.iter().any(|hash| hash == expected) {
            return Err("external_namespace_hash_not_approved".into());
        }
        let mut file = open(
            &self.reader.file,
            &self.target,
            FROZEN,
            SHARE_READ,
            false,
            false,
        )?;
        let read = read_same_object(&mut file, CAPTURE_BYTES, Capture::HashOnly, self.gate)?;
        if read.sha256 != expected {
            return Err("external_namespace_expected_hash_changed".into());
        }
        self.check()?;
        Ok(OwnedLeaf {
            parent: self,
            file,
            identity: read.identity,
            sha256: read.sha256,
            name: self.target.clone(),
            kind: LeafKind::Expected,
        })
    }
    pub(super) fn create_temp(&self, bytes: &[u8]) -> Result<OwnedLeaf<'_>> {
        self.check()?;
        if bytes.len() as u64 > CAPTURE_BYTES || bytes.len() as u64 > self.gate.limits().file {
            return Err("external_namespace_temp_byte_limit".into());
        }
        let sha256 = crate::sha256_hex(bytes);
        if !self.writable.contains(&sha256) {
            return Err("external_namespace_proposed_hash_not_approved".into());
        }
        let name = format!(".aurum-src-{}.tmp", crate::random::session_id());
        self.check()?;
        let mut file = open(&self.staging.file, &name, OWNED, SHARE_READ, true, false)?;
        let identity = super::file_identity(&file)?;
        // No failure cleanup: a partially written private temp remains evidence.
        for chunk in bytes.chunks(64 * 1024) {
            self.gate.check()?;
            file.write_all(chunk)
                .map_err(|_| "external_namespace_temp_write_failed")?;
        }
        file.sync_all()
            .map_err(|_| "external_namespace_temp_sync_failed")?;
        let actual = read_same_object(&mut file, CAPTURE_BYTES, Capture::HashOnly, self.gate)?;
        if actual.identity != identity || actual.sha256 != sha256 {
            return Err("external_namespace_temp_changed".into());
        }
        self.check()?;
        Ok(OwnedLeaf {
            parent: self,
            file,
            identity,
            sha256,
            name,
            kind: LeafKind::Temporary,
        })
    }
}
#[derive(PartialEq, Eq)]
enum LeafKind {
    Expected,
    Saved,
    Temporary,
    Published,
}
pub(super) struct OwnedLeaf<'a> {
    parent: &'a MutationDirectory<'a>,
    file: File,
    identity: DirectoryIdentity,
    sha256: String,
    name: String,
    kind: LeafKind,
}
impl OwnedLeaf<'_> {
    fn check(&mut self) -> Result<()> {
        self.parent.check()?;
        let actual = read_same_object(
            &mut self.file,
            CAPTURE_BYTES,
            Capture::HashOnly,
            self.parent.gate,
        )?;
        if actual.identity != self.identity || actual.sha256 != self.sha256 {
            return Err("external_namespace_owned_leaf_changed".into());
        }
        Ok(())
    }
    fn rename_after_check(
        &mut self,
        destination: &str,
        saved: bool,
        before_io: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.check()?;
        before_io()?;
        rename_no_replace(&self.file, &self.parent.mutation.file, destination)?;
        // Record the actual move before any later cancellation/postcheck fails.
        self.name = destination.into();
        self.kind = if saved {
            LeafKind::Saved
        } else {
            LeafKind::Published
        };
        self.check()
    }
    pub(super) fn move_aside(&mut self) -> Result<()> {
        if self.kind != LeafKind::Expected {
            return Err("external_namespace_expected_leaf_required".into());
        }
        let name = format!(".aurum-src-{}.old", crate::random::session_id());
        self.rename_after_check(&name, true, || Ok(()))
    }
    pub(super) fn publish_no_replace(&mut self) -> Result<()> {
        if self.kind != LeafKind::Temporary {
            return Err("external_namespace_owned_temp_required".into());
        }
        let name = self.parent.target.clone();
        self.rename_after_check(&name, false, || Ok(()))
    }
    pub(super) fn delete(mut self) -> Result<()> {
        self.delete_after_check(|| Ok(()))
    }
    fn delete_after_check(&mut self, before_io: impl FnOnce() -> Result<()>) -> Result<()> {
        self.check()?;
        before_io()?;
        let mut information = Disposition { delete_file: 1 };
        if unsafe {
            SetFileInformationByHandle(
                self.file.as_raw_handle(),
                4,
                (&mut information as *mut Disposition).cast(),
                std::mem::size_of::<Disposition>() as u32,
            )
        } == 0
        {
            return Err(format!(
                "external_namespace_delete_{}",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(1)
            ));
        }
        // Consuming the sole owned leaf handle completes disposition on close.
        Ok(())
    }
}
fn rename_no_replace(file: &File, parent: &File, name: &str) -> Result<()> {
    abi()?;
    if super::file_identity(file)?.volume != super::file_identity(parent)?.volume {
        return Err("external_namespace_cross_volume_relocation_unsupported".into());
    }
    let name = leaf(name)?;
    let name_bytes = name
        .len()
        .checked_mul(2)
        .ok_or("external_namespace_rename_overflow")?;
    let length = std::mem::size_of::<Rename>()
        .checked_add(name_bytes)
        .ok_or("external_namespace_rename_overflow")?;
    let units = length
        .checked_add(std::mem::size_of::<usize>() - 1)
        .ok_or("external_namespace_rename_overflow")?
        / std::mem::size_of::<usize>();
    let mut allocation = vec![0usize; units];
    let header = allocation.as_mut_ptr().cast::<Rename>();
    unsafe {
        (*header).flags_or_replace = 0;
        (*header).root = parent.as_raw_handle();
        (*header).length =
            u32::try_from(name_bytes).map_err(|_| "external_namespace_rename_overflow")?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            allocation
                .as_mut_ptr()
                .cast::<u8>()
                .add(std::mem::offset_of!(Rename, name))
                .cast::<u16>(),
            name.len(),
        );
    }
    let mut io = IoStatus {
        status: 0,
        information: 0,
    };
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut io,
            allocation.as_mut_ptr().cast(),
            u32::try_from(length).map_err(|_| "external_namespace_rename_overflow")?,
            10,
        )
    };
    if status != 0 {
        return Err(native_error("rename", status));
    }
    Ok(())
}

// The state aperture is deliberately separate from sanitized source readers.
// Its constructors accept only retained prepared/admitted authority, never a path.
pub(super) struct StateInspector<'a, 'b> {
    context: Context<'a, 'b>,
    root: Directory,
}
impl<'a, 'b> StateInspector<'a, 'b> {
    pub(super) fn prepared(
        grant: &'a super::PreparedGrant,
        budget: &'a Budget<'b>,
    ) -> Result<Self> {
        if grant.mode != Mode::SourcePublish {
            return Err("external_state_source_scope_required".into());
        }
        let context = Context {
            proof: Proof::Inspection(grant),
            budget,
            imported: None,
            imported_capture: None,
            _leases: Vec::new(),
        };
        context.check()?;
        let root = bootstrap(&grant.root, &grant.identity, READER, budget)?;
        Ok(Self { context, root })
    }
    pub(super) fn active(&self) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
        let Some(chain) = optional_walk(&self.root, ".aurum/revisions", &self.context)? else {
            return Ok(None);
        };
        state_read(
            chain.last().ok_or("external_state_parent_missing")?,
            "active.json",
            4096,
            &self.context,
        )
    }
    pub(super) fn read(
        &self,
        revision: &str,
        name: &str,
        maximum: u64,
    ) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
        revision_id(revision)?;
        state_name(name)?;
        let Some(chain) = optional_walk(
            &self.root,
            &format!(".aurum/revisions/{revision}"),
            &self.context,
        )?
        else {
            return Ok(None);
        };
        state_read(
            chain.last().ok_or("external_state_parent_missing")?,
            name,
            maximum,
            &self.context,
        )
    }
    pub(super) fn entries(&self, revision: &str) -> Result<Vec<DirectoryEntry>> {
        revision_id(revision)?;
        let chain = walk(
            &self.root,
            &format!(".aurum/revisions/{revision}"),
            &self.context,
        )?;
        enumerate(
            chain.last().ok_or("external_state_parent_missing")?,
            &self.context,
        )
    }
    pub(super) fn target(&self, path: &str) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
        source_observe(&self.root, path, &self.context)
    }
    pub(super) fn directory_identity(&self, revision: &str) -> Result<DirectoryIdentity> {
        revision_id(revision)?;
        let chain = walk(
            &self.root,
            &format!(".aurum/revisions/{revision}"),
            &self.context,
        )?;
        Ok(chain
            .last()
            .ok_or("external_state_parent_missing")?
            .identity
            .clone())
    }
    pub(super) fn saved_directory(
        &self,
        revision: &str,
        name: &str,
    ) -> Result<Option<DirectoryIdentity>> {
        revision_id(revision)?;
        directory_name(name)?;
        let chain = walk(
            &self.root,
            &format!(".aurum/revisions/{revision}"),
            &self.context,
        )?;
        saved_directory(
            chain.last().ok_or("external_state_parent_missing")?,
            name,
            &self.context,
        )
    }
    pub(super) fn source_directory(&self, path: &str) -> Result<Option<DirectoryIdentity>> {
        source_aperture(path, true)?;
        Ok(optional_walk(&self.root, path, &self.context)?
            .and_then(|v| v.last().map(|d| d.identity.clone())))
    }
    pub(super) fn source_entries(&self, path: &str) -> Result<Option<Vec<DirectoryEntry>>> {
        source_aperture(path, true)?;
        let Some(chain) = optional_walk(&self.root, path, &self.context)? else {
            return Ok(None);
        };
        Ok(Some(enumerate(
            chain.last().ok_or("external_state_parent_missing")?,
            &self.context,
        )?))
    }
}
fn readonly_barrier(root: &Directory, gate: &dyn Gate) -> Result<()> {
    if let Some(chain) = optional_walk(root, ".aurum/revisions", gate)? {
        if state_read(
            chain.last().ok_or("external_state_parent_missing")?,
            "active.json",
            4096,
            gate,
        )?
        .is_some()
        {
            return Err(
                "an unresolved revision is present; fresh bound recovery is required".into(),
            );
        }
    }
    Ok(())
}
fn missing(error: &str) -> bool {
    error.ends_with("_c0000034") || error.ends_with("_c000003a")
}
fn optional_walk(root: &Directory, path: &str, gate: &dyn Gate) -> Result<Option<Vec<Directory>>> {
    gate.check()?;
    let mut chain = vec![root.duplicate()?];
    for name in super::relative(path, false)? {
        check_spelling(
            chain.last().ok_or("external_state_parent_missing")?,
            name,
            gate,
        )?;
        match chain
            .last()
            .ok_or("external_state_parent_missing")?
            .child(name, READER)
        {
            Ok(child) => chain.push(child),
            Err(error) if missing(&error) => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(Some(chain))
}
fn revision_id(value: &str) -> Result<()> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err("external_state_revision_id_refused".into());
    }
    Ok(())
}
fn state_name(value: &str) -> Result<()> {
    leaf(value)?;
    let event = value
        .strip_prefix('e')
        .and_then(|v| v.strip_suffix(".json"))
        .is_some_and(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_digit()));
    let slot = value.split_once('.').is_some_and(|(row, kind)| {
        row.len() == 3
            && row.bytes().all(|b| b.is_ascii_digit())
            && matches!(kind, "before" | "after" | "temp" | "old" | "displaced")
    });
    if value != "v3.json" && !event && !slot && directory_name(value).is_err() {
        return Err("external_state_leaf_refused".into());
    }
    Ok(())
}
fn directory_name(value: &str) -> Result<()> {
    if value.len() != 9
        || !value.starts_with('d')
        || !value.ends_with(".dir")
        || !value.as_bytes()[1..5].iter().all(u8::is_ascii_digit)
    {
        return Err("external_state_directory_slot_refused".into());
    }
    Ok(())
}
fn saved_directory(
    parent: &Directory,
    name: &str,
    gate: &dyn Gate,
) -> Result<Option<DirectoryIdentity>> {
    gate.check()?;
    parent.check()?;
    directory_name(name)?;
    let child = match parent.child(name, READER) {
        Ok(child) => child,
        Err(e) if missing(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    if !enumerate(&child, gate)?.is_empty() {
        return Err("external_state_saved_directory_nonempty_conflict_evidence_preserved".into());
    }
    Ok(Some(child.identity))
}
fn immutable_name(value: &str) -> bool {
    value == "v3.json"
        || value.starts_with('e')
        || value.ends_with(".before")
        || value.ends_with(".after")
}
fn facts(read: ReadResult) -> super::ExternalObject {
    super::ExternalObject {
        identity: read.identity,
        sha256: read.sha256,
        bytes: read.bytes,
    }
}
fn state_read(
    parent: &Directory,
    name: &str,
    maximum: u64,
    gate: &dyn Gate,
) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
    state_read_after_check(parent, name, maximum, gate, || Ok(()))
}
fn state_read_after_check(
    parent: &Directory,
    name: &str,
    maximum: u64,
    gate: &dyn Gate,
    before_io: impl FnOnce() -> Result<()>,
) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
    gate.check()?;
    parent.check()?;
    before_io()?;
    let mut file = match open(&parent.file, name, READER, SHARE_READ, false, false) {
        Ok(file) => file,
        Err(error) if missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let read = read_same_object(&mut file, maximum, Capture::Bytes, gate)?;
    let bytes = read
        .contents
        .clone()
        .ok_or("external_state_capture_missing")?;
    parent.check()?;
    Ok(Some((facts(read), bytes)))
}
fn source_observe(
    root: &Directory,
    path: &str,
    gate: &dyn Gate,
) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
    gate.check_scope()?;
    source_aperture(path, false)?;
    let parent = super::directory_of(path);
    let chain = if parent.is_empty() {
        Some(vec![root.duplicate()?])
    } else {
        optional_walk(root, parent, gate)?
    };
    let Some(chain) = chain else { return Ok(None) };
    check_spelling(
        chain.last().ok_or("external_state_parent_missing")?,
        path.rsplit('/')
            .next()
            .ok_or("external_target_leaf_missing")?,
        gate,
    )?;
    state_read(
        chain.last().ok_or("external_state_parent_missing")?,
        path.rsplit('/')
            .next()
            .ok_or("external_target_leaf_missing")?,
        CAPTURE_BYTES.min(gate.limits().file),
        gate,
    )
}
fn check_spelling(parent: &Directory, name: &str, gate: &dyn Gate) -> Result<()> {
    if enumerate(parent, gate)?
        .iter()
        .any(|e| e.name.eq_ignore_ascii_case(name) && e.name != name)
    {
        return Err("source_case_alias_or_duplicate".into());
    }
    Ok(())
}
pub(super) struct StatePreflight {
    root: DirectoryIdentity,
    existing: Vec<(String, DirectoryIdentity)>,
    state_parents: Vec<(String, Option<DirectoryIdentity>)>,
    additional: u64,
}
struct StateRoot {
    revision: String,
    chain: Vec<Directory>,
}
/// Noncloneable reservation consumed by a create or handle relocation.
pub(super) struct ReservedSlot<'a> {
    gate: &'a dyn Gate,
    parent: Directory,
    name: String,
    volume: u64,
}
impl ReservedSlot<'_> {
    pub(super) fn create(self, bytes: &[u8], maximum: u64) -> Result<super::ExternalObject> {
        self.create_after_check(bytes, maximum, || Ok(()))
    }
    fn create_after_check(
        self,
        bytes: &[u8],
        maximum: u64,
        before_io: impl FnOnce() -> Result<()>,
    ) -> Result<super::ExternalObject> {
        self.gate.check()?;
        self.parent.check()?;
        if bytes.len() as u64 > maximum
            || maximum > CAPTURE_BYTES
            || maximum > self.gate.limits().file
            || self.parent.identity.volume != self.volume
        {
            return Err("external_state_create_bound_or_volume_refused".into());
        }
        before_io()?;
        let mut file = open(
            &self.parent.file,
            &self.name,
            OWNED,
            SHARE_READ,
            true,
            false,
        )?;
        let identity = super::file_identity(&file)?;
        if identity.volume != self.volume {
            return Err("external_namespace_cross_volume_relocation_unsupported".into());
        }
        for chunk in bytes.chunks(64 * 1024) {
            self.gate.check()?;
            file.write_all(chunk)
                .map_err(|_| "external_state_write_failed")?;
        }
        file.sync_all().map_err(|_| "external_state_sync_failed")?;
        let read = read_same_object(&mut file, maximum, Capture::HashOnly, self.gate)?;
        if read.identity != identity || read.sha256 != crate::sha256_hex(bytes) {
            return Err("external_state_created_object_changed".into());
        }
        self.parent.check()?;
        Ok(facts(read))
    }
}
type DirectoryScope = (
    Vec<(String, DirectoryIdentity)>,
    Vec<crate::transactions::DirectoryOrigin>,
);
impl WriteNamespace<'_, '_> {
    fn binding(&self) -> Result<&crate::transactions::ExternalBinding> {
        match &self.context.proof {
            Proof::Publication(a) => Ok(&a._permit.prepared.binding),
            Proof::Repair(a) => Ok(&a._permit.prepared.scope.binding),
            _ => Err("external_state_write_authority_required".into()),
        }
    }
    pub(super) fn observe_target(
        &self,
        path: &str,
    ) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
        let binding = self.binding()?;
        if !binding.targets.iter().any(|r| r.path == path) {
            return Err("external_namespace_target_not_approved".into());
        }
        source_observe(&self.reader_root, path, &self.context)
    }
    /// Read-only and complete before any state directory, temp or marker creation.
    pub(super) fn preflight(&self, additional: u64) -> Result<StatePreflight> {
        self.context.check_scope()?;
        let mut existing = Vec::new();
        let mut state_parents = Vec::new();
        for row in &self.binding()?.targets {
            source_aperture(&row.path, false)?;
            if self
                .observe_target(&row.path)?
                .as_ref()
                .is_some_and(|(object, _)| object.identity.volume != self.root.identity.volume)
            {
                return Err("external_namespace_cross_volume_relocation_unsupported".into());
            }
            let mut path = String::new();
            let mut current = self.reader_root.duplicate()?;
            for part in super::relative(super::directory_of(&row.path), false)? {
                path = child_path(&path, part);
                match current.child(part, READER) {
                    Ok(child) => {
                        current = child;
                        existing.push((path.clone(), current.identity.clone()));
                    }
                    Err(error) if missing(&error) => break,
                    Err(error) => return Err(error),
                }
            }
        }
        if matches!(&self.context.proof, Proof::Repair(_)) {
            let (retire, origins) = self.directory_scope()?;
            for (path, identity) in retire {
                if identity.volume != self.root.identity.volume
                    || self.source_directory_identity(&path)?.as_ref() != Some(&identity)
                {
                    return Err("external_state_directory_preflight_changed".into());
                }
            }
            for origin in origins {
                let old = self.checked_origin(&origin)?;
                if saved_directory(
                    old.last().ok_or("external_state_parent_missing")?,
                    &origin.name,
                    &self.context,
                )?
                .as_ref()
                    != Some(&origin.identity)
                {
                    return Err("external_state_directory_origin_changed".into());
                }
            }
        }
        for path in [".aurum", ".aurum/revisions"] {
            let identity = optional_walk(&self.reader_root, path, &self.context)?
                .and_then(|chain| chain.last().map(|d| d.identity.clone()));
            if let Some(identity) = &identity {
                existing.push((path.into(), identity.clone()));
            }
            state_parents.push((path.into(), identity));
        }
        if let Some(chain) = optional_walk(&self.reader_root, ".aurum/revisions", &self.context)? {
            let directory = chain.last().ok_or("external_state_parent_missing")?;
            if enumerate(directory, &self.context)?
                .iter()
                .filter(|e| e.directory)
                .count()
                >= 32
            {
                return Err("external_state_history_entry_quota".into());
            }
            if directory.identity.volume != self.root.identity.volume {
                return Err("external_namespace_cross_volume_relocation_unsupported".into());
            }
            let used = state_usage(directory, &self.context, 0, &mut 0usize)?;
            if used
                .checked_add(additional)
                .is_none_or(|v| v > history_limit(&self.context))
            {
                return Err("external_state_history_byte_quota".into());
            }
        } else if additional > history_limit(&self.context) {
            return Err("external_state_history_byte_quota".into());
        }
        for (_, identity) in &existing {
            if identity.volume != self.root.identity.volume {
                return Err("external_namespace_cross_volume_relocation_unsupported".into());
            }
        }
        Ok(StatePreflight {
            root: self.root.identity.clone(),
            existing,
            state_parents,
            additional,
        })
    }
    pub(super) fn begin_state(
        &self,
        revision: &str,
        proof: StatePreflight,
    ) -> Result<DirectoryIdentity> {
        self.context.check_scope()?;
        revision_id(revision)?;
        if proof.root != self.root.identity {
            return Err("external_state_preflight_changed".into());
        }
        for (path, expected) in proof.existing {
            let chain = walk(&self.reader_root, &path, &self.context)?;
            if chain
                .last()
                .ok_or("external_state_parent_missing")?
                .identity
                != expected
            {
                return Err("external_state_preflight_changed".into());
            }
        }
        let mut state = self.state.borrow_mut();
        if state.is_some() {
            return Err("external_state_revision_replay_refused".into());
        }
        let mut chain = vec![self.root.duplicate()?];
        let mut state_path = String::new();
        for name in [".aurum", "revisions", revision] {
            state_path = child_path(&state_path, name);
            let parent = chain.last().ok_or("external_state_parent_missing")?;
            let child = match parent.child(name, MUTATION_PARENT) {
                Ok(child) if name != revision => {
                    if proof
                        .state_parents
                        .iter()
                        .find(|(path, _)| path == &state_path)
                        .and_then(|(_, identity)| identity.as_ref())
                        != Some(&child.identity)
                    {
                        return Err("external_state_parent_presence_or_identity_changed".into());
                    }
                    if name == "revisions" {
                        let reader = parent.child(name, READER)?;
                        if reader.identity != child.identity {
                            return Err("external_state_parent_identity_changed".into());
                        }
                        if enumerate(&reader, &self.context)?
                            .iter()
                            .filter(|entry| entry.directory)
                            .count()
                            >= 32
                        {
                            return Err("external_state_history_entry_quota".into());
                        }
                        if state_usage(&reader, &self.context, 0, &mut 0usize)?
                            .checked_add(proof.additional)
                            .is_none_or(|n| n > history_limit(&self.context))
                        {
                            return Err("external_state_history_byte_quota".into());
                        }
                    }
                    child
                }
                Ok(_) => return Err("external_state_revision_present_unowned".into()),
                Err(error) if missing(&error) => {
                    if name != revision
                        && proof
                            .state_parents
                            .iter()
                            .find(|(path, _)| path == &state_path)
                            .and_then(|(_, identity)| identity.as_ref())
                            .is_some()
                    {
                        return Err("external_state_parent_presence_or_identity_changed".into());
                    }
                    self.context.check()?;
                    let file = open(&parent.file, name, MUTATION_PARENT, SHARE_RW, true, true)?;
                    let identity = super::file_identity(&file)?;
                    if identity.volume != self.root.identity.volume {
                        return Err("external_namespace_cross_volume_relocation_unsupported".into());
                    }
                    Directory { file, identity }
                }
                Err(error) => return Err(error),
            };
            chain.push(child);
        }
        let identity = chain
            .last()
            .ok_or("external_state_parent_missing")?
            .identity
            .clone();
        *state = Some(StateRoot {
            revision: revision.into(),
            chain,
        });
        Ok(identity)
    }
    pub(super) fn open_state(&self, revision: &str, expected: &DirectoryIdentity) -> Result<()> {
        self.context.check_scope()?;
        revision_id(revision)?;
        let path = format!(".aurum/revisions/{revision}");
        let mut chain = walk(&self.reader_root, &path, &self.context)?;
        let parent = chain
            .get(chain.len() - 2)
            .ok_or("external_state_parent_missing")?;
        let directory = parent.child(revision, MUTATION_PARENT)?;
        if &directory.identity != expected || expected.volume != self.root.identity.volume {
            return Err("external_state_revision_identity_changed".into());
        }
        chain.push(directory);
        *self.state.borrow_mut() = Some(StateRoot {
            revision: revision.into(),
            chain,
        });
        Ok(())
    }
    fn state_parent(&self) -> Result<Directory> {
        self.context.check()?;
        self.check_records()?;
        let state = self.state.borrow();
        let state = state.as_ref().ok_or("external_state_not_open")?;
        for directory in &state.chain {
            directory.check()?;
        }
        state
            .chain
            .last()
            .ok_or("external_state_parent_missing")?
            .duplicate()
    }
    pub(super) fn reserve(&self, name: &str) -> Result<ReservedSlot<'_>> {
        state_name(name)?;
        let parent = self.state_parent()?;
        if state_read(&parent, name, CAPTURE_BYTES, &self.context)?.is_some() {
            return Err("external_state_reserved_name_present".into());
        }
        Ok(ReservedSlot {
            gate: &self.context,
            parent,
            name: name.into(),
            volume: self.root.identity.volume,
        })
    }
    pub(super) fn state_read(
        &self,
        name: &str,
        maximum: u64,
    ) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
        state_name(name)?;
        state_read(&self.state_parent()?, name, maximum, &self.context)
    }
    pub(super) fn state_entries(&self) -> Result<Vec<DirectoryEntry>> {
        let (revision, expected) = {
            let state = self.state.borrow();
            let state = state.as_ref().ok_or("external_state_not_open")?;
            (
                state.revision.clone(),
                state
                    .chain
                    .last()
                    .ok_or("external_state_parent_missing")?
                    .identity
                    .clone(),
            )
        };
        let chain = walk(
            &self.reader_root,
            &format!(".aurum/revisions/{revision}"),
            &self.context,
        )?;
        let reader = chain.last().ok_or("external_state_parent_missing")?;
        if reader.identity != expected {
            return Err("external_state_revision_identity_changed".into());
        }
        enumerate(reader, &self.context)
    }
    pub(super) fn create_record(
        &self,
        name: &str,
        bytes: &[u8],
        maximum: u64,
    ) -> Result<super::ExternalObject> {
        if bytes.len() as u64 > maximum
            || maximum > CAPTURE_BYTES
            || maximum > self.context.budget.max_file
        {
            return Err("external_state_create_bound_or_volume_refused".into());
        }
        self.check_storage(bytes.len() as u64, 1)?;
        let object = self.reserve(name)?.create(bytes, maximum)?;
        if immutable_name(name) {
            self.pin_record(name, &object)?;
        }
        self.check_storage(0, 0)?;
        Ok(object)
    }
    fn check_storage(&self, additional: u64, added_entries: usize) -> Result<()> {
        self.context.check_scope()?;
        let Some(chain) = optional_walk(&self.reader_root, ".aurum/revisions", &self.context)?
        else {
            return if additional > history_limit(&self.context) {
                Err("external_state_history_byte_quota".into())
            } else {
                Ok(())
            };
        };
        let parent = chain.last().ok_or("external_state_parent_missing")?;
        if enumerate(parent, &self.context)?
            .iter()
            .filter(|e| e.directory)
            .count()
            > 32
        {
            return Err("external_state_history_entry_quota".into());
        }
        let mut entries = 0;
        let bytes = state_usage(parent, &self.context, 0, &mut entries)?;
        if bytes
            .checked_add(additional)
            .is_none_or(|v| v > history_limit(&self.context))
        {
            return Err("external_state_history_byte_quota".into());
        }
        if entries
            .checked_add(added_entries)
            .is_none_or(|v| v > self.context.budget.max_entries)
        {
            return Err("external_state_history_entries".into());
        }
        Ok(())
    }
    pub(super) fn pin_record(&self, name: &str, expected: &super::ExternalObject) -> Result<()> {
        state_name(name)?;
        if !immutable_name(name) {
            return Err("external_state_mutable_slot_cannot_be_pinned".into());
        }
        let parent = self.state_parent()?;
        let mut file = open(&parent.file, name, READER, SHARE_READ, false, false)?;
        if facts(read_same_object(
            &mut file,
            CAPTURE_BYTES.min(self.context.budget.max_file),
            Capture::HashOnly,
            &self.context,
        )?) != *expected
        {
            return Err("external_state_immutable_record_changed".into());
        }
        let stamp = super::stamp(&file)?;
        self.immutable_records.borrow_mut().push((file, stamp));
        Ok(())
    }
    pub(super) fn active(&self) -> Result<Option<(super::ExternalObject, Vec<u8>)>> {
        let Some(chain) = optional_walk(&self.reader_root, ".aurum/revisions", &self.context)?
        else {
            return Ok(None);
        };
        state_read(
            chain.last().ok_or("external_state_parent_missing")?,
            "active.json",
            4096,
            &self.context,
        )
    }
    pub(super) fn create_active(&self, bytes: &[u8]) -> Result<super::ExternalObject> {
        self.check_storage(bytes.len() as u64, 1)?;
        let chain = walk(&self.reader_root, ".aurum/revisions", &self.context)?;
        let parent = chain[chain.len() - 2].child("revisions", MUTATION_PARENT)?;
        if parent.identity
            != chain
                .last()
                .ok_or("external_state_parent_missing")?
                .identity
        {
            return Err("external_state_parent_identity_changed".into());
        }
        ReservedSlot {
            gate: &self.context,
            parent,
            name: "active.json".into(),
            volume: self.root.identity.volume,
        }
        .create(bytes, 4096)
    }
    pub(super) fn clear_active(&self, expected: &super::ExternalObject) -> Result<()> {
        let chain = walk(&self.reader_root, ".aurum/revisions", &self.context)?;
        delete_recorded(
            chain.last().ok_or("external_state_parent_missing")?,
            "active.json",
            expected,
            &self.context,
        )
    }
    // No caller or test reaches this yet; it belongs to the unconnected publication surface.
    #[allow(dead_code)]
    pub(super) fn delete_slot(&self, name: &str, expected: &super::ExternalObject) -> Result<()> {
        state_name(name)?;
        delete_recorded(&self.state_parent()?, name, expected, &self.context)
    }
    pub(super) fn move_target(
        &self,
        path: &str,
        expected: &super::ExternalObject,
        reserved: ReservedSlot<'_>,
    ) -> Result<super::ExternalObject> {
        self.move_target_after_check(path, expected, reserved, || Ok(()))
    }
    fn move_target_after_check(
        &self,
        path: &str,
        expected: &super::ExternalObject,
        reserved: ReservedSlot<'_>,
        before_io: impl FnOnce() -> Result<()>,
    ) -> Result<super::ExternalObject> {
        if !std::ptr::eq(
            (reserved.gate as *const dyn Gate).cast::<()>(),
            std::ptr::from_ref(&self.context).cast::<()>(),
        ) {
            return Err("external_state_reservation_authority_mismatch".into());
        }
        self.check_storage(expected.bytes, 1)?;
        let target = self.target(path)?;
        target.check()?;
        if !target.allowed.contains(&expected.sha256) {
            return Err("external_namespace_hash_not_approved".into());
        }
        let mut file = recorded(&target.reader, &target.target, expected, &self.context)?;
        if expected.identity.volume != reserved.volume
            || reserved.parent.identity.volume != expected.identity.volume
        {
            return Err("external_namespace_cross_volume_relocation_unsupported".into());
        }
        reserved.parent.check()?;
        before_io()?;
        rename_no_replace(&file, &reserved.parent.file, &reserved.name)?;
        Ok(facts(read_same_object(
            &mut file,
            CAPTURE_BYTES,
            Capture::HashOnly,
            &self.context,
        )?))
    }
    pub(super) fn publish_slot(
        &self,
        path: &str,
        name: &str,
        expected: &super::ExternalObject,
    ) -> Result<super::ExternalObject> {
        state_name(name)?;
        let target = self.target(path)?;
        target.check()?;
        if !target.allowed.contains(&expected.sha256) {
            return Err("external_namespace_hash_not_approved".into());
        }
        let mut file = recorded(&self.state_parent()?, name, expected, &self.context)?;
        rename_no_replace(&file, &target.mutation.file, &target.target)?;
        Ok(facts(read_same_object(
            &mut file,
            CAPTURE_BYTES,
            Capture::HashOnly,
            &self.context,
        )?))
    }
    pub(super) fn delete_target(&self, path: &str, expected: &super::ExternalObject) -> Result<()> {
        let target = self.target(path)?;
        if !target.allowed.contains(&expected.sha256) {
            return Err("external_namespace_hash_not_approved".into());
        }
        delete_recorded(&target.reader, &target.target, expected, &self.context)
    }
    pub(super) fn check(&self) -> Result<()> {
        self.context.check_scope()?;
        self.check_records()
    }
    pub(super) fn prepublication(&self) -> Result<()> {
        self.check()?;
        match &self.context.proof {
            Proof::Publication(a) => {
                for root in &a._permit.prepared.unused_roots {
                    let (path, identity) =
                        super::canonical_directory(&root.source, self.context.budget)?;
                    if path != root.source || identity != root.identity {
                        return Err("unused_granted_root_identity_changed".into());
                    }
                }
                super::check_originals(
                    &a._permit.prepared.roots,
                    &a._candidate.proposal.staged,
                    self.context.budget,
                )?;
                super::check_proposed(&a._candidate.proposal.staged, self.context.budget)?;
                if super::hash_validated_artifact(
                    &a._candidate.artifact,
                    &a._candidate.artifact_guard,
                    self.context.budget,
                )? != a._candidate.artifact_hash
                {
                    return Err("external_source_validated_artifact_changed".into());
                }
                Ok(())
            }
            Proof::Repair(a) => a.check_authority(self.context.budget),
            _ => Err("external_state_mutation_authority_required".into()),
        }
    }
    fn check_records(&self) -> Result<()> {
        for (file, expected) in self.immutable_records.borrow().iter() {
            self.context.check()?;
            if &super::stamp(file)? != expected {
                return Err("external_state_immutable_record_changed".into());
            }
        }
        Ok(())
    }
    pub(super) fn source_directory_identity(
        &self,
        path: &str,
    ) -> Result<Option<DirectoryIdentity>> {
        source_aperture(path, true)?;
        Ok(optional_walk(&self.reader_root, path, &self.context)?
            .and_then(|v| v.last().map(|d| d.identity.clone())))
    }
    pub(super) fn create_source_directory(&self, path: &str) -> Result<DirectoryIdentity> {
        self.context.check_scope()?;
        let binding = self.binding()?;
        let permitted = binding.cleanup_dirs.iter().any(|p| p == path)
            || match &self.context.proof {
                Proof::Repair(a) => a._permit.prepared.reverse_cleanup.iter().any(|p| p == path),
                _ => false,
            };
        if !permitted
            || path == binding.member_src
            || !path.starts_with(&(binding.member_src.clone() + "/"))
        {
            return Err("external_state_directory_not_approved".into());
        }
        source_aperture(path, false)?;
        let ancestor = super::directory_of(path);
        let name = path
            .rsplit('/')
            .next()
            .ok_or("external_state_directory_leaf")?;
        let chain = walk(&self.reader_root, ancestor, &self.context)?;
        let parent = chain.last().ok_or("external_state_parent_missing")?;
        let file = open(&parent.file, name, MUTATION_PARENT, SHARE_RW, true, true)?;
        let identity = super::file_identity(&file)?;
        if identity.volume != self.root.identity.volume {
            return Err("external_namespace_cross_volume_relocation_unsupported".into());
        }
        self.created.borrow_mut().insert(
            path.into(),
            Directory {
                file,
                identity: identity.clone(),
            },
        );
        Ok(identity)
    }
    pub(super) fn remove_source_directory(
        &self,
        path: &str,
        expected: &DirectoryIdentity,
    ) -> Result<()> {
        self.context.check_scope()?;
        let binding = self.binding()?;
        if path == binding.member_src || !path.starts_with(&(binding.member_src.clone() + "/")) {
            return Err("external_state_directory_not_approved".into());
        }
        let owned =
            match &self.context.proof {
                Proof::Publication(_) => self
                    .created
                    .borrow()
                    .get(path)
                    .is_some_and(|d| &d.identity == expected),
                Proof::Repair(a) => {
                    self.created
                        .borrow()
                        .get(path)
                        .is_some_and(|d| &d.identity == expected)
                        || a._permit.prepared.scope.owned_dirs.iter().any(|d| {
                            d.path == path && d.identity == super::identity_digest(expected)
                        })
                }
                _ => false,
            };
        if !owned {
            return Err("external_state_directory_ownership_missing".into());
        }
        let (root, guards) = match &self.context.proof {
            Proof::Publication(a) => (&a._permit.prepared.grant.root, &a._permit.prepared.guards),
            Proof::Repair(a) => (&a._permit.prepared.grant.root, &a._permit.prepared.guards),
            _ => return Err("external_state_write_authority_required".into()),
        };
        let ancestor = super::directory_of(path);
        let name = path
            .rsplit('/')
            .next()
            .ok_or("external_state_directory_leaf")?;
        let chain = walk(&self.reader_root, ancestor, &self.context)?;
        let mut held = guards
            .0
            .lock()
            .map_err(|_| "external_state_guard_retirement")?;
        let label = root.join(path);
        if let Some(index) = held.iter().position(|(p, _, _)| p == &label) {
            if &held[index].1 != expected {
                return Err("external_state_directory_identity_changed".into());
            }
            held.remove(index);
        }
        drop(held);
        self.created.borrow_mut().remove(path);
        let parent = chain.last().ok_or("external_state_parent_missing")?;
        let file = open(&parent.file, name, FROZEN, SHARE_READ, false, true)?;
        if &super::file_identity(&file)? != expected {
            return Err("external_state_directory_identity_changed".into());
        }
        let mut info = Disposition { delete_file: 1 };
        self.context.check()?;
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                4,
                (&mut info as *mut Disposition).cast(),
                1,
            )
        } == 0
        {
            return Err("external_state_owned_directory_not_empty_or_refused".into());
        }
        Ok(())
    }
    fn directory_scope(&self) -> Result<DirectoryScope> {
        match &self.context.proof {
            Proof::Repair(a) => Ok(a
                ._permit
                .prepared
                .v3
                .directory_scope(a._permit.prepared.undo)),
            _ => Err("external_state_directory_relocation_requires_fresh_repair".into()),
        }
    }
    fn checked_origin(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<Vec<Directory>> {
        let (_, origins) = self.directory_scope()?;
        if !origins.contains(origin) {
            return Err("external_state_directory_origin_not_approved".into());
        }
        revision_id(&origin.revision)?;
        directory_name(&origin.name)?;
        if origin.directory.volume != self.root.identity.volume
            || origin.identity.volume != self.root.identity.volume
        {
            return Err("external_namespace_cross_volume_relocation_unsupported".into());
        }
        let chain = walk(
            &self.reader_root,
            &format!(".aurum/revisions/{}", origin.revision),
            &self.context,
        )?;
        if chain
            .last()
            .ok_or("external_state_parent_missing")?
            .identity
            != origin.directory
        {
            return Err("external_state_directory_origin_changed".into());
        }
        Ok(chain)
    }
    fn retire_directory_guard(&self, path: &str, expected: &DirectoryIdentity) -> Result<()> {
        let (root, guards) = match &self.context.proof {
            Proof::Repair(a) => (&a._permit.prepared.grant.root, &a._permit.prepared.guards),
            _ => return Err("external_state_directory_relocation_requires_fresh_repair".into()),
        };
        let mut held = guards
            .0
            .lock()
            .map_err(|_| "external_state_guard_retirement")?;
        let label = root.join(path);
        if let Some(index) = held.iter().position(|(p, _, _)| p == &label) {
            if &held[index].1 != expected {
                return Err("external_state_directory_identity_changed".into());
            }
            held.remove(index);
        }
        drop(held);
        if self
            .created
            .borrow()
            .get(path)
            .is_some_and(|d| &d.identity != expected)
        {
            return Err("external_state_directory_identity_changed".into());
        }
        self.created.borrow_mut().remove(path);
        Ok(())
    }
    fn check_retirement(&self, path: &str, identity: &DirectoryIdentity, name: &str) -> Result<()> {
        let (retire, _) = self.directory_scope()?;
        if !retire
            .iter()
            .enumerate()
            .any(|(i, (p, id))| p == path && id == identity && name == format!("d{i:04}.dir"))
        {
            return Err("external_state_directory_retirement_not_approved".into());
        }
        source_aperture(path, false)?;
        directory_name(name)?;
        Ok(())
    }
    fn hold_returned(&self, path: &str, identity: &DirectoryIdentity) -> Result<()> {
        let chain = walk(&self.reader_root, super::directory_of(path), &self.context)?;
        let directory = chain.last().ok_or("external_state_parent_missing")?.child(
            path.rsplit('/')
                .next()
                .ok_or("external_state_directory_leaf")?,
            MUTATION_PARENT,
        )?;
        if &directory.identity != identity {
            return Err("external_state_directory_identity_changed".into());
        }
        self.created.borrow_mut().insert(path.into(), directory);
        Ok(())
    }
    pub(super) fn saved_directory(&self, name: &str) -> Result<Option<DirectoryIdentity>> {
        saved_directory(&self.state_parent()?, name, &self.context)
    }
    pub(super) fn origin_directory(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<Option<DirectoryIdentity>> {
        let chain = self.checked_origin(origin)?;
        saved_directory(
            chain.last().ok_or("external_state_parent_missing")?,
            &origin.name,
            &self.context,
        )
    }
    pub(super) fn retire_directory(
        &self,
        path: &str,
        identity: &DirectoryIdentity,
        name: &str,
    ) -> Result<()> {
        self.check_retirement(path, identity, name)?;
        self.check_storage(0, 1)?;
        let chain = walk(&self.reader_root, super::directory_of(path), &self.context)?;
        let parent = chain.last().ok_or("external_state_parent_missing")?;
        let state = self.state_parent()?;
        if saved_directory(&state, name, &self.context)?.is_some() {
            return Err("external_state_directory_slot_present_unowned".into());
        }
        self.retire_directory_guard(path, identity)?;
        relocate_empty_directory(
            parent,
            path.rsplit('/')
                .next()
                .ok_or("external_state_directory_leaf")?,
            &state,
            name,
            identity,
            &self.context,
        )
    }
    pub(super) fn unretire_directory(
        &self,
        path: &str,
        identity: &DirectoryIdentity,
        name: &str,
    ) -> Result<()> {
        self.check_retirement(path, identity, name)?;
        let chain = walk(&self.reader_root, super::directory_of(path), &self.context)?;
        let parent = mutation_role(&chain, super::directory_of(path))?;
        relocate_empty_directory(
            &self.state_parent()?,
            name,
            &parent,
            path.rsplit('/')
                .next()
                .ok_or("external_state_directory_leaf")?,
            identity,
            &self.context,
        )?;
        self.hold_returned(path, identity)
    }
    pub(super) fn return_directory(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<()> {
        let old = self.checked_origin(origin)?;
        let source = walk(
            &self.reader_root,
            super::directory_of(&origin.path),
            &self.context,
        )?;
        let destination = mutation_role(&source, super::directory_of(&origin.path))?;
        relocate_empty_directory(
            old.last().ok_or("external_state_parent_missing")?,
            &origin.name,
            &destination,
            origin
                .path
                .rsplit('/')
                .next()
                .ok_or("external_state_directory_leaf")?,
            &origin.identity,
            &self.context,
        )?;
        self.hold_returned(&origin.path, &origin.identity)
    }
    pub(super) fn unreturn_directory(
        &self,
        origin: &crate::transactions::DirectoryOrigin,
    ) -> Result<()> {
        self.check_storage(0, 1)?;
        let old = self.checked_origin(origin)?;
        let source = walk(
            &self.reader_root,
            super::directory_of(&origin.path),
            &self.context,
        )?;
        self.retire_directory_guard(&origin.path, &origin.identity)?;
        let destination = mutation_role(&old, &format!(".aurum/revisions/{}", origin.revision))?;
        relocate_empty_directory(
            source.last().ok_or("external_state_parent_missing")?,
            origin
                .path
                .rsplit('/')
                .next()
                .ok_or("external_state_directory_leaf")?,
            &destination,
            &origin.name,
            &origin.identity,
            &self.context,
        )
    }
}
fn relocate_empty_directory(
    source: &Directory,
    name: &str,
    destination: &Directory,
    target: &str,
    identity: &DirectoryIdentity,
    gate: &dyn Gate,
) -> Result<()> {
    relocate_empty_directory_after_check(source, name, destination, target, identity, gate, || {
        Ok(())
    })
}
fn relocate_empty_directory_after_check(
    source: &Directory,
    name: &str,
    destination: &Directory,
    target: &str,
    identity: &DirectoryIdentity,
    gate: &dyn Gate,
    before_io: impl FnOnce() -> Result<()>,
) -> Result<()> {
    gate.check_scope()?;
    source.check()?;
    destination.check()?;
    if identity.volume != source.identity.volume || identity.volume != destination.identity.volume {
        return Err("external_namespace_cross_volume_relocation_unsupported".into());
    }
    let file = open(&source.file, name, FROZEN, SHARE_READ, false, true)?;
    if &super::file_identity(&file)? != identity {
        return Err("external_state_directory_identity_changed".into());
    }
    let observed = Directory {
        file: file
            .try_clone()
            .map_err(|_| "external_state_directory_duplicate")?,
        identity: identity.clone(),
    };
    if !enumerate(&observed, gate)?.is_empty() {
        return Err("external_state_directory_nonempty_conflict_evidence_preserved".into());
    }
    gate.check()?;
    before_io()?;
    rename_no_replace(&file, &destination.file, target)?;
    if !enumerate(&observed, gate)?.is_empty() {
        return Err(
            "external_state_relocated_directory_nonempty_conflict_evidence_preserved".into(),
        );
    }
    observed.check()
}
fn recorded(
    parent: &Directory,
    name: &str,
    expected: &super::ExternalObject,
    gate: &dyn Gate,
) -> Result<File> {
    gate.check()?;
    parent.check()?;
    let mut file = open(&parent.file, name, FROZEN, SHARE_READ, false, false)?;
    let actual = facts(read_same_object(
        &mut file,
        CAPTURE_BYTES,
        Capture::HashOnly,
        gate,
    )?);
    if &actual != expected {
        return Err("external_state_recorded_identity_or_bytes_changed".into());
    }
    Ok(file)
}
fn delete_recorded(
    parent: &Directory,
    name: &str,
    expected: &super::ExternalObject,
    gate: &dyn Gate,
) -> Result<()> {
    let file = recorded(parent, name, expected, gate)?;
    let mut info = Disposition { delete_file: 1 };
    gate.check()?;
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            4,
            (&mut info as *mut Disposition).cast(),
            1,
        )
    } == 0
    {
        return Err("external_state_recorded_disposition_refused".into());
    }
    Ok(())
}
fn history_limit(gate: &dyn Gate) -> u64 {
    (128u64 * 1024 * 1024).min(gate.limits().bytes)
}
fn state_usage(
    directory: &Directory,
    gate: &dyn Gate,
    depth: usize,
    entries: &mut usize,
) -> Result<u64> {
    if depth > 24 {
        return Err("external_state_history_depth".into());
    }
    let mut bytes = 0u64;
    for item in enumerate(directory, gate)? {
        *entries = entries
            .checked_add(1)
            .ok_or("external_state_history_entries")?;
        if *entries > gate.limits().entries {
            return Err("external_state_history_entries".into());
        }
        let amount = if item.directory {
            state_usage(
                &directory.child(&item.name, READER)?,
                gate,
                depth + 1,
                entries,
            )?
        } else {
            item.bytes
        };
        bytes = bytes
            .checked_add(amount)
            .ok_or("external_state_history_overflow")?;
        if bytes > history_limit(gate) {
            return Err("external_state_history_byte_quota".into());
        }
    }
    Ok(bytes)
}

#[cfg(test)]
pub(super) fn qualify_metadata_only_parent_races(
    parent: &MutationDirectory<'_>,
    base: &Path,
    outside: &Path,
) -> Result<usize> {
    // One explicit metadata-only hypothesis, not a search for a convenient
    // access mask. A refusal is missing qualification and fails this case.
    const METADATA_ONLY: u32 = 0x0010_0180; // SYNCHRONIZE, READ/WRITE_ATTRIBUTES. No data access.
    let base = crate::project::clean_path(base.canonicalize().map_err(|_| "namespace_test_base")?);
    let temp = crate::project::clean_path(
        std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "namespace_test_temp")?,
    );
    let token = base
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("external-workspace-fixture-"))
        .ok_or("namespace_test_fixture_required")?;
    if base.parent() != Some(temp.as_path())
        || !temp.to_string_lossy().starts_with("A:")
        || token.len() != 32
        || !token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || outside != base.join("ungranted")
    {
        return Err("namespace_test_scope_refused".into());
    }
    parent.check()?;
    // Positively bind this test capability to the one disposable fixture pair.
    // Drop the extra bootstrap guard before the sharing/mutation observation.
    let expected_parent = crate::snapshot::open_checked_writable_directory_identity(
        &base.join("engine/native/src/race"),
        parent.gate.deadline(),
    )?;
    if super::file_identity(&expected_parent)? != parent.reader.identity
        || parent.reader.identity != parent.mutation.identity
    {
        return Err("namespace_test_parent_scope_refused".into());
    }
    drop(expected_parent);
    if !enumerate(&parent.reader, parent.gate)?.is_empty() {
        return Err("namespace_test_empty_parent_required".into());
    }
    let writer = open(
        &parent.chain.last().ok_or("namespace_test_anchor")?.file,
        &parent.parent_name,
        METADATA_ONLY,
        SHARE_RW,
        false,
        true,
    )?;
    if super::file_identity(&writer)? != parent.reader.identity {
        return Err("namespace_test_actor_identity".into());
    }
    let outside_file = crate::snapshot::open_checked_directory(outside, parent.gate.deadline())?;
    let outside_identity = super::file_identity(&outside_file)?;
    let printed = crate::project::clean_path(
        outside
            .canonicalize()
            .map_err(|_| "namespace_test_outside")?,
    )
    .to_str()
    .ok_or("namespace_test_outside")?
    .replace('/', "\\");
    let printed = printed.strip_prefix(r"\\?\").unwrap_or(&printed);
    let substitute: Vec<u16> = format!(r"\??\{printed}").encode_utf16().collect();
    let display: Vec<u16> = printed.encode_utf16().collect();
    let mut probe = MetadataProbe {
        writer,
        original: &parent.reader,
        outside: outside_file,
        outside_identity,
        substitute,
        display,
        dirty: false,
        verified: 0,
    };

    let read = parent.read_target_after_check(Capture::Bytes, || probe.install());
    probe.clear()?;
    require_relative_reparse_refusal(read.map(|_| ()), "read")?;

    // The same native FILE_CREATE helper and flags as create_temp, here aimed
    // at the empty approved target parent so mutation can actually be installed.
    parent.check()?;
    probe.install()?;
    let created = open(
        &parent.mutation.file,
        "namespace-owned.tmp",
        OWNED,
        SHARE_READ,
        true,
        false,
    );
    probe.clear()?;
    require_relative_reparse_refusal(created.map(drop), "create")?;

    let payload_hash = parent
        .writable
        .first()
        .ok_or("namespace_test_payload_missing")?;
    let payload = b"pub fn namespace_proposal() -> i32 { 41 }\n";
    if &crate::sha256_hex(payload) != payload_hash {
        return Err("namespace_test_payload_binding".into());
    }
    let mut proposed = parent.create_temp(payload)?;
    let expected_id = proposed.identity.clone();
    let original_name = proposed.name.clone();
    let target = parent.target.clone();
    let renamed = proposed.rename_after_check(&target, false, || probe.install());
    probe.clear()?;
    require_relative_reparse_refusal(renamed, "rename")?;
    proposed.check()?;
    if proposed.identity != expected_id
        || proposed.name != original_name
        || proposed.kind != LeafKind::Temporary
    {
        return Err("namespace_test_proposal_not_preserved".into());
    }

    // Enumeration has no pathname or RootDirectory argument. Observe the
    // actual held-object query before the ordinary-parent postcheck rejects
    // changed metadata. Empty original rows or DONT_REPARSE refusal are safe;
    // returning any outside entry is an unsafe qualification failure.
    parent.check()?;
    probe.install()?;
    let enumerated = enumerate_pages(&parent.reader, parent.gate);
    probe.clear()?;
    match enumerated {
        Ok(entries) if entries.is_empty() => {}
        Err(error) if error.ends_with("_c0000280") => {}
        other => return Err(format!("namespace_test_enumeration_not_confined:{other:?}")),
    }

    let disposed = proposed.delete_after_check(|| probe.install());
    drop(proposed); // Disposition completes only when this owned handle closes.
    probe.clear()?;
    disposed?;
    let observed = open(
        &parent.staging.file,
        &original_name,
        READER,
        SHARE_READ,
        false,
        false,
    );
    if !matches!(observed, Err(ref error) if error.ends_with("_c0000034")) {
        return Err("namespace_test_owned_disposition_incomplete".into());
    }
    if probe.verified != 5 {
        return Err("namespace_test_metadata_mutation_coverage_missing".into());
    }
    Ok(probe.verified)
}
#[cfg(test)]
pub(super) fn qualify_state_parent_races(
    namespace: &WriteNamespace<'_, '_>,
    base: &Path,
    outside: &Path,
) -> Result<usize> {
    // Same one metadata-only actor as the accepted capability qualification.
    // Refused injection is missing coverage, never immutability evidence.
    const METADATA_ONLY: u32 = 0x0010_0180;
    let temp = crate::project::clean_path(
        std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "namespace_test_temp")?,
    );
    let base = crate::project::clean_path(base.canonicalize().map_err(|_| "namespace_test_base")?);
    let token = base
        .file_name()
        .and_then(|v| v.to_str())
        .and_then(|v| v.strip_prefix("external-workspace-fixture-"))
        .ok_or("namespace_test_fixture_required")?;
    if base.parent() != Some(temp.as_path())
        || !temp.to_string_lossy().starts_with("A:")
        || token.len() != 32
        || !token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || outside != base.join("ungranted")
    {
        return Err("namespace_test_scope_refused".into());
    }
    namespace.context.check_scope()?;
    let chain = walk(
        &namespace.reader_root,
        ".aurum/revisions",
        &namespace.context,
    )?;
    let parent = chain.last().ok_or("namespace_test_state_parent")?;
    let anchor = chain
        .get(chain.len() - 2)
        .ok_or("namespace_test_state_parent")?;
    if !enumerate(parent, &namespace.context)?.is_empty() {
        return Err("namespace_test_empty_parent_required".into());
    }
    let expected = crate::snapshot::open_checked_writable_directory_identity(
        &base.join("engine/.aurum/revisions"),
        namespace.context.budget.deadline,
    )?;
    if super::file_identity(&expected)? != parent.identity {
        return Err("namespace_test_parent_scope_refused".into());
    }
    drop(expected);
    let writer = open(
        &anchor.file,
        "revisions",
        METADATA_ONLY,
        SHARE_RW,
        false,
        true,
    )?;
    if super::file_identity(&writer)? != parent.identity {
        return Err("namespace_test_actor_identity".into());
    }
    let outside_file =
        crate::snapshot::open_checked_directory(outside, namespace.context.budget.deadline)?;
    let outside_identity = super::file_identity(&outside_file)?;
    let printed = crate::project::clean_path(
        outside
            .canonicalize()
            .map_err(|_| "namespace_test_outside")?,
    )
    .to_str()
    .ok_or("namespace_test_outside")?
    .replace('/', "\\");
    let printed = printed.strip_prefix(r"\\?\").unwrap_or(&printed);
    let mut probe = MetadataProbe {
        writer,
        original: parent,
        outside: outside_file,
        outside_identity,
        substitute: format!(r"\??\{printed}").encode_utf16().collect(),
        display: printed.encode_utf16().collect(),
        dirty: false,
        verified: 0,
    };
    // This operation also enumerates the parent spelling during its storage
    // precheck. Run it before the held-parent probes change reparse metadata.
    let original = namespace
        .observe_target("native/src/lib.rs")?
        .ok_or("namespace_test_original_missing")?
        .0;
    let reserved = ReservedSlot {
        gate: &namespace.context,
        parent: mutation_role(&chain, ".aurum/revisions")?,
        name: "000.old".into(),
        volume: namespace.root.identity.volume,
    };
    let verified = probe.verified;
    let mut old_move_injected = false;
    let moved = namespace.move_target_after_check("native/src/lib.rs", &original, reserved, || {
        probe.install()?;
        old_move_injected = true;
        Ok(())
    });
    probe.clear()?;
    if !old_move_injected || probe.verified != verified + 1 {
        return Err("namespace_test_state_old_move_injection_not_reached".into());
    }
    require_relative_reparse_refusal(moved.map(|_| ()), "state_old_move")?;
    if namespace.observe_target("native/src/lib.rs")?.map(|v| v.0) != Some(original) {
        return Err("namespace_test_original_not_preserved".into());
    }
    for name in ["000.before", "e000000.json", "active.json"] {
        let verified = probe.verified;
        let read =
            state_read_after_check(parent, name, 4096, &namespace.context, || probe.install());
        probe.clear()?;
        if probe.verified != verified + 1 {
            return Err("namespace_test_state_read_injection_not_reached".into());
        }
        require_relative_reparse_refusal(read.map(|_| ()), "state_read")?;
    }
    for name in ["000.temp", "e000000.json", "active.json"] {
        let reserved = ReservedSlot {
            gate: &namespace.context,
            parent: mutation_role(&chain, ".aurum/revisions")?,
            name: name.into(),
            volume: namespace.root.identity.volume,
        };
        let verified = probe.verified;
        let created =
            reserved.create_after_check(b"owned state fixture\n", 4096, || probe.install());
        probe.clear()?;
        if probe.verified != verified + 1 {
            return Err("namespace_test_state_create_injection_not_reached".into());
        }
        require_relative_reparse_refusal(created.map(|_| ()), "state_create")?;
    }
    parent.check()?;
    let verified = probe.verified;
    probe.install()?;
    let enumeration = enumerate_pages(parent, &namespace.context);
    probe.clear()?;
    if probe.verified != verified + 1 {
        return Err("namespace_test_state_enumeration_injection_not_reached".into());
    }
    match enumeration {
        Ok(rows) if rows.is_empty() => {}
        Err(e) if e.ends_with("_c0000280") => {}
        other => return Err(format!("namespace_test_state_quota_not_confined:{other:?}")),
    }
    // Qualify the directory form of the same native rename class separately.
    // This actor owns one exact empty directory in the disposable fixture.
    let source = walk(&namespace.reader_root, "native/src", &namespace.context)?;
    let source = source.last().ok_or("namespace_test_source_parent")?;
    let name = "namespace-owned-directory";
    let owned = open(&source.file, name, MUTATION_PARENT, SHARE_RW, true, true)?;
    let identity = super::file_identity(&owned)?;
    drop(owned);
    let outside_directory = Directory {
        file: probe
            .outside
            .try_clone()
            .map_err(|_| "namespace_test_outside_duplicate")?,
        identity: probe.outside_identity.clone(),
    };
    let outside_before = enumerate(&outside_directory, &namespace.context)?;
    let destination = mutation_role(&chain, ".aurum/revisions")?;
    let verified = probe.verified;
    let mut directory_move_injected = false;
    let renamed = relocate_empty_directory_after_check(
        source,
        name,
        &destination,
        "d0000.dir",
        &identity,
        &namespace.context,
        || {
            probe.install()?;
            directory_move_injected = true;
            Ok(())
        },
    );
    let outside_after = enumerate(&outside_directory, &namespace.context)?;
    let unsafe_destination = outside_before != outside_after;
    if unsafe_destination {
        eprintln!("namespace_test_unsafe_directory_destination_observed");
    }
    probe.clear()?;
    if !directory_move_injected || probe.verified != verified + 1 {
        return Err("namespace_test_state_directory_rename_injection_not_reached".into());
    }
    if unsafe_destination {
        return Err("namespace_test_unsafe_directory_destination_observed".into());
    }
    require_relative_reparse_refusal(renamed, "state_directory_rename")?;
    let current = source.child(name, READER)?;
    if current.identity != identity || !enumerate(&current, &namespace.context)?.is_empty() {
        return Err("namespace_test_owned_directory_not_preserved".into());
    }
    drop(current);
    let owned = open(&source.file, name, FROZEN, SHARE_READ, false, true)?;
    if super::file_identity(&owned)? != identity {
        return Err("namespace_test_owned_directory_identity".into());
    }
    let mut disposition = Disposition { delete_file: 1 };
    namespace.context.check()?;
    if unsafe {
        SetFileInformationByHandle(
            owned.as_raw_handle(),
            4,
            (&mut disposition as *mut Disposition).cast(),
            1,
        )
    } == 0
    {
        return Err("namespace_test_owned_directory_cleanup_refused".into());
    }
    drop(owned);
    if !matches!(source.child(name,READER),Err(ref e) if missing(e)) {
        return Err("namespace_test_owned_directory_cleanup_unverified".into());
    }
    if probe.verified != 9 {
        return Err("namespace_test_metadata_mutation_coverage_missing".into());
    }
    Ok(probe.verified)
}
#[cfg(test)]
fn require_relative_reparse_refusal(result: Result<()>, operation: &str) -> Result<()> {
    match result {
        Err(error) if error.ends_with("_c0000280") => Ok(()),
        other => Err(format!(
            "namespace_test_{operation}_after_check_not_confined:{other:?}"
        )),
    }
}
#[cfg(test)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn DeviceIoControl(
        file: Handle,
        code: u32,
        input: Handle,
        input_bytes: u32,
        output: Handle,
        output_bytes: u32,
        returned: *mut u32,
        overlapped: Handle,
    ) -> i32;
}
#[cfg(test)]
struct MetadataProbe<'a> {
    writer: File,
    original: &'a Directory,
    outside: File,
    outside_identity: DirectoryIdentity,
    substitute: Vec<u16>,
    display: Vec<u16>,
    dirty: bool,
    verified: usize,
}
#[cfg(test)]
impl MetadataProbe<'_> {
    fn control(&self, code: u32, data: &mut [u8]) -> Result<()> {
        let mut returned = 0;
        if unsafe {
            DeviceIoControl(
                self.writer.as_raw_handle(),
                code,
                data.as_mut_ptr().cast(),
                u32::try_from(data.len()).map_err(|_| "namespace_test_reparse_bound")?,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "namespace_test_metadata_ioctl_{}",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(1)
            ));
        }
        Ok(())
    }
    fn identities(&self) -> Result<()> {
        if super::file_identity(&self.writer)? != self.original.identity
            || super::file_identity(&self.original.file)? != self.original.identity
            || super::file_identity(&self.outside)? != self.outside_identity
        {
            return Err("namespace_test_reparse_identity_changed".into());
        }
        ordinary(&self.outside, true)
    }
    fn install(&mut self) -> Result<()> {
        self.identities()?;
        self.original.check()?;
        let names = self
            .substitute
            .len()
            .checked_add(self.display.len())
            .and_then(|n| n.checked_add(2))
            .and_then(|n| n.checked_mul(2))
            .ok_or("namespace_test_reparse_bound")?;
        let length = names.checked_add(8).ok_or("namespace_test_reparse_bound")?;
        if length > 16 * 1024 - 8 {
            return Err("namespace_test_reparse_bound".into());
        }
        let mut buffer = vec![0u8; length + 8];
        buffer[..4].copy_from_slice(&0xa000_0003u32.to_le_bytes());
        buffer[4..6].copy_from_slice(&(length as u16).to_le_bytes());
        buffer[10..12].copy_from_slice(&((self.substitute.len() * 2) as u16).to_le_bytes());
        buffer[12..14].copy_from_slice(&(((self.substitute.len() + 1) * 2) as u16).to_le_bytes());
        buffer[14..16].copy_from_slice(&((self.display.len() * 2) as u16).to_le_bytes());
        for (index, value) in self
            .substitute
            .iter()
            .chain(std::iter::once(&0))
            .chain(self.display.iter())
            .chain(std::iter::once(&0))
            .enumerate()
        {
            buffer[16 + index * 2..18 + index * 2].copy_from_slice(&value.to_le_bytes());
        }
        // Attempted installation is retained for cleanup even when an API
        // reports failure after changing metadata. Only exact readback credits it.
        self.dirty = true;
        let setter = self.control(0x0009_00a4, &mut buffer);
        let mut readback = vec![0u8; 16 * 1024];
        let mut returned = 0u32;
        if unsafe {
            DeviceIoControl(
                self.original.file.as_raw_handle(),
                0x0009_00a8,
                std::ptr::null_mut(),
                0,
                readback.as_mut_ptr().cast(),
                readback.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(format!(
                "namespace_test_minimal_metadata_injection_unverified:{setter:?}"
            ));
        }
        let returned = returned as usize;
        if returned < 16 || returned > readback.len() {
            return Err("namespace_test_get_reparse_length".into());
        }
        readback.truncate(returned);
        let word = |start: usize| -> Result<usize> {
            Ok(u16::from_le_bytes(
                readback
                    .get(start..start + 2)
                    .ok_or("namespace_test_get_reparse_bounds")?
                    .try_into()
                    .map_err(|_| "namespace_test_get_reparse_bounds")?,
            ) as usize)
        };
        let end = 8usize
            .checked_add(word(4)?)
            .ok_or("namespace_test_get_reparse_bounds")?;
        if readback[..4] != 0xa000_0003u32.to_le_bytes() || end < 16 || end > returned {
            return Err("namespace_test_get_reparse_tag".into());
        }
        let decode = |offset: usize, length: usize| -> Result<Vec<u16>> {
            let offset = word(offset)?;
            let length = word(length)?;
            let start = 16usize
                .checked_add(offset)
                .ok_or("namespace_test_get_reparse_bounds")?;
            let finish = start
                .checked_add(length)
                .ok_or("namespace_test_get_reparse_bounds")?;
            if offset % 2 != 0 || length % 2 != 0 || finish > end {
                return Err("namespace_test_get_reparse_bounds".into());
            }
            Ok(readback[start..finish]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect())
        };
        if decode(8, 10)? != self.substitute || decode(12, 14)? != self.display {
            return Err("namespace_test_get_reparse_target".into());
        }
        self.identities()?;
        self.verified += 1;
        Ok(())
    }
    fn clear(&mut self) -> Result<()> {
        if self.dirty && ordinary(&self.original.file, true).is_err() {
            self.identities()?;
            let mut header = [0u8; 8];
            header[..4].copy_from_slice(&0xa000_0003u32.to_le_bytes());
            self.control(0x0009_00ac, &mut header)?;
        }
        self.original.check()?;
        self.identities()?;
        self.dirty = false;
        Ok(())
    }
}
#[cfg(test)]
impl Drop for MetadataProbe<'_> {
    fn drop(&mut self) {
        if self.dirty {
            if let Err(error) = self.clear() {
                eprintln!("namespace_test_reparse_cleanup_unverified:{error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(name: &str, next: u32) -> Vec<u8> {
        let text: Vec<u16> = name.encode_utf16().collect();
        let mut data = vec![0u8; 64 + text.len() * 2];
        data[..4].copy_from_slice(&next.to_le_bytes());
        data[40..48].copy_from_slice(&7i64.to_le_bytes());
        data[60..64].copy_from_slice(&((text.len() * 2) as u32).to_le_bytes());
        for (index, word) in text.into_iter().enumerate() {
            data[64 + index * 2..66 + index * 2].copy_from_slice(&word.to_le_bytes());
        }
        data
    }
    #[test]
    fn external_source_namespace_directory_decoder_refuses_hidden_or_malformed_records() {
        let valid = record("a.rs", 0);
        assert_eq!(
            decode_page(&valid, || Ok(())).unwrap(),
            vec![DirectoryEntry {
                name: "a.rs".into(),
                directory: false,
                bytes: 7
            }]
        );
        let mut padded = valid.clone();
        padded.extend_from_slice(&[0; 7]);
        assert_eq!(decode_page(&padded, || Ok(())).unwrap().len(), 1);
        let mut hidden = valid.clone();
        hidden.extend_from_slice(&record("unreported.rs", 0));
        assert_eq!(
            decode_page(&hidden, || Ok(())).err().unwrap(),
            "external_namespace_enumeration_terminal_padding"
        );
        let mut unaligned = record("a.rs", 73);
        unaligned.extend_from_slice(&record("b.rs", 0));
        assert!(decode_page(&unaligned, || Ok(())).is_err());
        let mut gap = record("a.rs", 88);
        gap.resize(88, 0);
        gap.extend_from_slice(&record("b.rs", 0));
        assert!(decode_page(&gap, || Ok(())).is_err());
        let mut huge = valid.clone();
        huge[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_page(&huge, || Ok(())).is_err());
        let mut utf16 = record("a", 0);
        utf16[64..66].copy_from_slice(&0xd800u16.to_le_bytes());
        assert!(decode_page(&utf16, || Ok(())).is_err());
        let mut reparse = valid.clone();
        reparse[56..60].copy_from_slice(&0x400u32.to_le_bytes());
        assert!(decode_page(&reparse, || Ok(())).is_err());
        assert!(decode_page(&valid[..63], || Ok(())).is_err());
        assert_eq!(
            decode_page(&valid, || Err("cancelled_original_budget".into()))
                .err()
                .unwrap(),
            "cancelled_original_budget"
        );
    }
}
