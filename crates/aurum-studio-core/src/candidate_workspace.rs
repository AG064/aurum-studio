//! Recognized private workspaces with crash-released leases and bounded admission.
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) const WORKSPACE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const SLOTS: usize = 2;
#[cfg(any(windows, test))]
const MAX_LIFETIME_CLAIMS: usize = 64;
const LIFETIME_CLOSED: usize = 1usize << (usize::BITS - 1);
const MAX_POOL_ENTRIES: usize = 32;
const MAX_TREE_ENTRIES: usize = 50_000;
const INSPECTION_BUDGET: Duration = Duration::from_secs(2);
const POOL_NAME: &str = "aurum-candidates-v1";

/// One allocation's in-memory cleanup permission. This is not crash evidence.
#[derive(Clone, Debug, Default)]
pub(crate) struct LifetimeFence(Arc<AtomicUsize>);
#[cfg(any(windows, test))]
pub(crate) struct LifetimeClaim(Option<Arc<AtomicUsize>>);
impl LifetimeFence {
    #[cfg(all(test, windows))]
    pub(crate) fn same_origin_for_cargo_fixture(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    #[cfg(any(windows, test))]
    pub(crate) fn claim(&self) -> std::io::Result<LifetimeClaim> {
        // A compare-exchange loop: `fetch_update` is deprecated on newer toolchains and the
        // replacement does not exist on older ones.
        let mut count = self.0.load(Ordering::Acquire);
        loop {
            if count >= MAX_LIFETIME_CLAIMS {
                return Err(std::io::Error::other(if count == LIFETIME_CLOSED {
                    "private workspace lifetime admission is closed"
                } else {
                    "private workspace lifetime claim limit reached"
                }));
            }
            match self.0.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        Ok(LifetimeClaim(Some(self.0.clone())))
    }
    pub(crate) fn unresolved(&self) -> bool {
        self.0.load(Ordering::Acquire) & !LIFETIME_CLOSED != 0
    }
    /// Atomically stop future claims before cleanup or source publication.
    pub(crate) fn close_if_resolved(&self) -> bool {
        if self.unresolved() {
            return false;
        }
        match self
            .0
            .compare_exchange(0, LIFETIME_CLOSED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(count) => count == LIFETIME_CLOSED,
        }
    }
}
#[cfg(any(windows, test))]
impl LifetimeClaim {
    /// Only affirmative no-spawn or complete original-object retirement proves
    /// this claim resolved. Dropping an unresolved claim preserves refusal.
    pub(crate) fn resolve(&mut self) {
        if let Some(state) = self.0.take() {
            state.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    id: String,
    reserved_bytes: u64,
}

fn open_lock(path: &Path, create: bool) -> Result<File, String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)
        .map_err(|error| error.to_string())
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn manager(root: &Path) -> Result<Lease, String> {
    crate::diagnostics::checked_path(root).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let lock = open_lock(&root.join("pool.lock"), true)?;
    lock.try_lock().map_err(|_| {
        "candidate storage is busy; retry after the current allocation or cleanup".to_string()
    })?;
    Ok(Lease(lock))
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
type CompilerPathCheck = fn(&Path) -> Result<(), String>;
fn candidate_path(pool: &Path, id: &str) -> PathBuf {
    pool.join(format!("candidate-{id}"))
}
#[cfg(windows)]
pub(crate) fn compiler_root_and_cwd(workspace: &Path) -> (PathBuf, PathBuf) {
    let root = workspace.join("compiler-state");
    let cwd = root.join("cwd");
    (root, cwd)
}
/// CreateProcessW's current-directory budget includes a trailing separator and NUL.
/// This limit concerns cwd only; it does not impose MAX_PATH on ordinary files.
#[cfg(windows)]
pub(crate) fn check_compiler_cwd(cwd: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    let units = cwd.as_os_str().encode_wide().count();
    let last = cwd.as_os_str().encode_wide().last();
    let trailing = matches!(last, Some(47 | 92));
    if units.saturating_add(1 + usize::from(!trailing)) > 260 {
        return Err("external_native_cwd_platform_budget_refused".into());
    }
    Ok(())
}
#[cfg(windows)]
fn check_compiler_workspace(workspace: &Path) -> Result<(), String> {
    let (_, cwd) = compiler_root_and_cwd(workspace);
    check_compiler_cwd(&cwd)
}
fn recognized(root: &Path, path: &Path) -> Result<bool, String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    if path.parent() != Some(root) {
        return Ok(false);
    }
    let Some(id) = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("candidate-"))
    else {
        return Ok(false);
    };
    if !valid_id(id) {
        return Ok(false);
    }
    let marker = path.join("workspace.json");
    crate::diagnostics::checked_path(&marker).map_err(|error| error.to_string())?;
    let Ok(metadata) = std::fs::metadata(&marker) else {
        return Ok(false);
    };
    if !metadata.is_file() || metadata.len() > 1024 {
        return Ok(false);
    }
    let bytes = std::fs::read(marker).map_err(|error| error.to_string())?;
    if bytes.len() > 1024 {
        return Ok(false);
    }
    let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
        return Ok(false);
    };
    Ok(manifest.version == 1 && manifest.id == id && manifest.reserved_bytes == WORKSPACE_BYTES)
}
fn remove(root: &Path, path: &Path) -> Result<(), String> {
    if !recognized(root, path)?
        || crate::project::clean_path(path.canonicalize().map_err(|error| error.to_string())?)
            != path
    {
        return Err("candidate cleanup refused an unrecognized private workspace".into());
    }
    // std removes nested links themselves. Never follow them or clean legacy
    // flat temporary directories, unmarked directories, or project revisions.
    std::fs::remove_dir_all(path).map_err(|error| error.to_string())
}

pub(crate) struct Workspace {
    pub(crate) path: PathBuf,
    pool: PathBuf,
    lease: Option<Lease>,
    pub(crate) recovered: usize,
    pub(crate) lifetime: LifetimeFence,
    #[cfg(windows)]
    retained_compilation: Option<Arc<crate::external_workspace::CompilationLease>>,
}
/// In-memory evidence of resolved original ownership, never persisted authority.
#[cfg(windows)]
pub(crate) struct ResolvedCompilation {
    workspace: PathBuf,
    envelope: PathBuf,
    lifetime: LifetimeFence,
}
#[cfg(windows)]
impl ResolvedCompilation {
    pub(crate) fn matches(&self, workspace: &Path, envelope: &Path) -> bool {
        self.workspace == workspace
            && self.envelope == envelope
            && self.lifetime.0.load(Ordering::Acquire) == LIFETIME_CLOSED
    }
    /// Inspect the same resolved fence and held allocation lease, without reopening.
    pub(crate) fn check_workspace_owner(
        &self,
        workspace: &Workspace,
        envelope: &Path,
        budget: &crate::external_workspace::Budget<'_>,
    ) -> Result<(), String> {
        budget.check()?;
        if !self.matches(&workspace.path, envelope)
            || !Arc::ptr_eq(&self.lifetime.0, &workspace.lifetime.0)
            || workspace.retained_compilation.is_some()
            || workspace.path.parent() != Some(workspace.pool.as_path())
        {
            return Err("external_artifact_custody_workspace_owner_mismatch".into());
        }
        let lease = workspace
            .lease
            .as_ref()
            .ok_or("external_artifact_custody_workspace_lease_missing")?;
        crate::snapshot::check_opened_path(
            &lease.0,
            &workspace.path.join("workspace.lock"),
            budget.deadline(),
        )?;
        budget.check()
    }

    /// A fixed private output slot in this allocation, not a source envelope or
    /// a caller-selected publication destination. Original retirement stays closed.
    pub(crate) fn private_artifact_slot(
        &self,
        workspace: &Workspace,
        envelope: &Path,
        budget: &crate::external_workspace::Budget<'_>,
    ) -> Result<PrivateArtifactSlot, String> {
        self.check_workspace_owner(workspace, envelope, budget)?;
        let directory =
            crate::snapshot::open_checked_directory(&workspace.path, budget.deadline())?;
        let slot = PrivateArtifactSlot {
            path: workspace.path.join("checked-artifact.dll"),
            directory,
            lifetime: self.lifetime.clone(),
        };
        slot.recheck(self, workspace, envelope, budget)?;
        Ok(slot)
    }
}

/// In-memory destination authority issued only by the original resolved owner.
/// The held allocation directory denies replacement while CreateNew uses its
/// single fixed leaf. It confers no loading, relocation or publication authority.
#[cfg(windows)]
pub(crate) struct PrivateArtifactSlot {
    path: PathBuf,
    directory: File,
    lifetime: LifetimeFence,
}
#[cfg(windows)]
impl PrivateArtifactSlot {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn recheck(
        &self,
        resolved: &ResolvedCompilation,
        workspace: &Workspace,
        envelope: &Path,
        budget: &crate::external_workspace::Budget<'_>,
    ) -> Result<(), String> {
        resolved.check_workspace_owner(workspace, envelope, budget)?;
        if !Arc::ptr_eq(&self.lifetime.0, &resolved.lifetime.0)
            || self.path != workspace.path.join("checked-artifact.dll")
        {
            return Err("external_private_artifact_slot_owner_mismatch".into());
        }
        crate::snapshot::check_opened_path(&self.directory, &workspace.path, budget.deadline())?;
        budget.check()
    }

    pub(crate) fn create_new(
        &self,
        resolved: &ResolvedCompilation,
        workspace: &Workspace,
        envelope: &Path,
        budget: &crate::external_workspace::Budget<'_>,
    ) -> Result<File, String> {
        use std::os::windows::fs::OpenOptionsExt;
        self.recheck(resolved, workspace, envelope, budget)?;
        // Retain the returned writer before any further fallible work. Both the
        // parent guard and this non-reparse CreateNew handle deny replacement.
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(0x0020_0000)
            .share_mode(1)
            .open(&self.path)
            .map_err(|_| "external_private_artifact_create_new_refused".into())
    }
}
/// Only the recognized allocator can borrow this destination. The lease and
/// lifetime owner remain held until staging or owned workers are resolved.
#[cfg(windows)]
pub(crate) struct SourceEnvelope<'a> {
    workspace: &'a Workspace,
    path: PathBuf,
    guard: File,
}
#[cfg(windows)]
impl SourceEnvelope<'_> {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn recheck(&self, deadline: Instant) -> Result<(), String> {
        if self.workspace.lease.is_none() || self.workspace.lifetime.0.load(Ordering::Acquire) != 0
        {
            return Err("recognized_workspace_lease_missing".into());
        }
        crate::snapshot::check_opened_path(&self.guard, &self.workspace.path, deadline)
    }
}
impl Workspace {
    /// A separate recognized allocation and fresh fence. Never recover or
    /// delete another slot while admitting a parser under the original budget.
    #[cfg(any(windows, test))]
    pub(crate) fn parser_peer_checked(
        &self,
        deadline: Instant,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        if self.lease.is_none() {
            return Err("configfile_parser_original_allocation_required".into());
        }
        let checked = || {
            check()?;
            if Instant::now() >= deadline {
                return Err("configfile_parser_allocation_deadline".into());
            }
            Ok(())
        };
        Self::in_pool_controlled(&self.pool, SLOTS, None, Some(&checked))
    }
    /// At most one compilation lease per recognized slot. It is retained with
    /// the existing OS allocation lease if original-worker retirement is unknown.
    #[cfg(windows)]
    pub(crate) fn retain_compilation(
        &mut self,
        lease: Arc<crate::external_workspace::CompilationLease>,
    ) -> Result<(), String> {
        if self.lease.is_none() || self.retained_compilation.is_some() || self.lifetime.unresolved()
        {
            return Err("external_compile_retention_admission_refused".into());
        }
        self.retained_compilation = Some(lease);
        Ok(())
    }
    /// Release compilation read locks without deleting the candidate. Aliased
    /// leases or unresolved original workers retain all evidence and refuse.
    #[cfg(windows)]
    pub(crate) fn retire_compilation_for_publication(
        &mut self,
        budget: &crate::external_workspace::Budget<'_>,
    ) -> Result<ResolvedCompilation, String> {
        let retained = self
            .retained_compilation
            .as_ref()
            .ok_or("external_compile_retirement_missing")?;
        if self.lease.is_none() || Arc::strong_count(retained) != 1 || self.lifetime.unresolved() {
            return Err("external_compile_retirement_unresolved_or_aliased".into());
        }
        retained.verify(budget)?;
        if !self.lifetime.close_if_resolved() {
            return Err("external_compile_retirement_unresolved_or_aliased".into());
        }
        let envelope = retained.staged_envelope().to_path_buf();
        self.retained_compilation.take();
        Ok(ResolvedCompilation {
            workspace: self.path.clone(),
            envelope,
            lifetime: self.lifetime.clone(),
        })
    }
    #[cfg(windows)]
    pub(crate) fn source_envelope(&self, deadline: Instant) -> Result<SourceEnvelope<'_>, String> {
        if self.lease.is_none() || self.lifetime.0.load(Ordering::Acquire) != 0 {
            return Err("recognized_workspace_unavailable".into());
        }
        let guard = crate::snapshot::open_checked_directory(&self.path, deadline)?;
        crate::snapshot::check_opened_path(&guard, &self.path, deadline)?;
        Ok(SourceEnvelope {
            workspace: self,
            path: self.path.join("external-sources"),
            guard,
        })
    }

    pub(crate) fn create() -> Result<Self, String> {
        Self::in_pool(&std::env::temp_dir().join(POOL_NAME))
    }
    #[cfg(all(windows, not(test)))]
    pub(crate) fn create_for_native_compile() -> Result<Self, String> {
        Self::in_temp_for_native_compile(&std::env::temp_dir())
    }
    #[cfg(windows)]
    fn in_temp_for_native_compile(temp: &Path) -> Result<Self, String> {
        Self::in_pool_with_path_check(&temp.join(POOL_NAME), SLOTS, Some(check_compiler_workspace))
    }
    #[cfg(windows)]
    pub(crate) fn create_native() -> Result<Self, String> {
        Self::in_pool_with_slots(&std::env::temp_dir().join("aurum-native-previews-v1"), 4)
    }
    #[cfg(test)]
    pub(crate) fn for_test_project(project: &Path) -> Result<Self, String> {
        Self::in_pool(&std::env::temp_dir().join(format!(
            "aurum-candidate-fixture-{}",
            crate::sha256_hex(project.to_string_lossy().as_bytes())
        )))
    }
    #[cfg(all(windows, test))]
    pub(crate) fn for_test_project_native_compile(project: &Path) -> Result<Self, String> {
        Self::in_pool_with_path_check(
            &std::env::temp_dir().join(format!(
                "aurum-candidate-fixture-{}",
                crate::sha256_hex(project.to_string_lossy().as_bytes())
            )),
            SLOTS,
            Some(check_compiler_workspace),
        )
    }
    fn in_pool(pool: &Path) -> Result<Self, String> {
        Self::in_pool_with_slots(pool, SLOTS)
    }
    fn in_pool_with_slots(pool: &Path, slots: usize) -> Result<Self, String> {
        Self::in_pool_with_path_check(pool, slots, None)
    }
    fn in_pool_with_path_check(
        pool: &Path,
        slots: usize,
        check: Option<CompilerPathCheck>,
    ) -> Result<Self, String> {
        Self::in_pool_controlled(pool, slots, check, None)
    }
    fn in_pool_controlled(
        pool: &Path,
        slots: usize,
        check: Option<CompilerPathCheck>,
        control: Option<&dyn Fn() -> Result<(), String>>,
    ) -> Result<Self, String> {
        let checkpoint = || control.map_or(Ok(()), |check| check());
        checkpoint()?;
        // Use the same ID and naming rule for the forecast and actual allocation.
        // A foreseeable cwd refusal must precede pool creation, recovery and leases.
        let preflight = if let Some(check) = check {
            let id = crate::random::session_id();
            check(&candidate_path(pool, &id))?;
            Some((id, check))
        } else {
            None
        };
        let _manager = manager(pool)?;
        checkpoint()?;
        let pool =
            crate::project::clean_path(pool.canonicalize().map_err(|error| error.to_string())?);
        if let Some((id, check)) = &preflight {
            check(&candidate_path(&pool, id))?;
        }
        let mut active = 0;
        let mut recovered = 0;
        for (index, entry) in std::fs::read_dir(&pool)
            .map_err(|error| error.to_string())?
            .enumerate()
        {
            checkpoint()?;
            if index >= MAX_POOL_ENTRIES {
                return Err(
                    "candidate pool has too many entries; inspect private storage before retrying"
                        .into(),
                );
            }
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.file_name() == "pool.lock" {
                continue;
            }
            let path = entry.path();
            if !recognized(&pool, &path)? {
                return Err(format!(
                    "candidate pool contains unrecognized state; preserved for explicit inspection: {}",
                    pool.display()
                ));
            }
            checkpoint()?;
            if control.is_some() {
                // Stale slots also occupy their reservation. No recovery or
                // removal is permitted by parser admission.
                active += 1;
                continue;
            }
            let lease = open_lock(&path.join("workspace.lock"), false)?;
            match lease.try_lock() {
                Ok(()) => {
                    let _lease = Lease(lease);
                    remove(&pool, &path)?;
                    recovered += 1;
                }
                Err(std::fs::TryLockError::WouldBlock) => active += 1,
                Err(error) => return Err(error.to_string()),
            }
        }
        if active >= slots {
            return Err(format!(
                "private storage reservations are full ({slots} slots of 1 GiB); retry after an owned operation completes"
            ));
        }
        let id = preflight.map_or_else(crate::random::session_id, |(id, _)| id);
        let path = candidate_path(&pool, &id);
        checkpoint()?;
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        checkpoint()?;
        // The manager lease prevents another allocator observing partial setup.
        // A crash before the marker completes leaves unrecognized state, which
        // is preserved rather than guessed to be safe to delete.
        let lease = open_lock(&path.join("workspace.lock"), true)?;
        lease.try_lock().map_err(|error| error.to_string())?;
        // Keep a failed partial allocation's original lease until host exit.
        if let Err(error) = checkpoint() {
            std::mem::forget(lease);
            return Err(error);
        }
        crate::files::write_atomic(
            &path.join("workspace.json"),
            &serde_json::to_vec(&Manifest {
                version: 1,
                id,
                reserved_bytes: WORKSPACE_BYTES,
            })
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let workspace = Self {
            path,
            pool,
            lease: Some(Lease(lease)),
            recovered,
            lifetime: LifetimeFence::default(),
            #[cfg(windows)]
            retained_compilation: None,
        };
        if let Err(error) = checkpoint() {
            std::mem::forget(workspace);
            return Err(error);
        }
        Ok(workspace)
    }
    pub(crate) fn clean(&mut self) -> Result<(), String> {
        if !self.lifetime.close_if_resolved()
            || crate::operation_context::OperationContext::current()
                .is_some_and(|context| context.termination_unconfirmed())
        {
            return Err(
                "owned worker termination is unconfirmed; private workspace retained".into(),
            );
        }
        if self.lease.is_none() {
            return Ok(());
        }
        // Resolved retirement only permits cleanup. It cannot authorize a late
        // compiler result or source publication after the operation deadline.
        #[cfg(windows)]
        self.retained_compilation.take();
        let _manager = manager(&self.pool)?;
        remove(&self.pool, &self.path)?;
        self.lease.take();
        Ok(())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        if !self.lifetime.close_if_resolved()
            || crate::operation_context::OperationContext::current()
                .is_some_and(|context| context.termination_unconfirmed())
        {
            // Keep the OS lease until host exit, so another allocation cannot
            // reclaim source still potentially used by our uncertain worker.
            if let Some(lease) = self.lease.take() {
                std::mem::forget(lease);
            }
            #[cfg(windows)]
            if let Some(lease) = self.retained_compilation.take() {
                std::mem::forget(lease);
            }
            eprintln!(
                "aurum candidates: private workspace and lease retained until host exit because worker termination is unconfirmed"
            );
            return;
        }
        if self.lease.is_some() && self.clean().is_err() {
            eprintln!(
                "aurum candidates: private workspace retained for next-allocation cleanup; source was not removed"
            );
        }
    }
}

/// Observed-byte safeguard, not a hard filesystem quota. Bounded traversal
/// refuses links, special files and oversized/complex trees without reading
/// content. The process runner checks it periodically and before acceptance.
#[cfg(any(windows, test))]
pub(crate) fn usage(root: &Path, limit: u64) -> Result<u64, String> {
    usage_with_budget(root, limit, INSPECTION_BUDGET)
}

fn inspection_limits(count: usize, started: Instant, budget: Duration) -> Result<(), String> {
    let elapsed = started.elapsed();
    if count > MAX_TREE_ENTRIES {
        return Err(format!(
            "candidate storage exceeds its entry inspection budget: inspected_entries={count} entry_limit={MAX_TREE_ENTRIES} elapsed_ms={} time_limit_ms={}",
            elapsed.as_millis(),
            budget.as_millis()
        ));
    }
    if budget.is_zero() || elapsed > budget {
        return Err(format!(
            "candidate storage exceeds its elapsed inspection budget: inspected_entries={count} entry_limit={MAX_TREE_ENTRIES} elapsed_ms={} time_limit_ms={}",
            elapsed.as_millis(),
            budget.as_millis()
        ));
    }
    Ok(())
}

/// Never allocate a scan more time than the enclosing operation still has.
pub(crate) fn usage_with_budget(root: &Path, limit: u64, budget: Duration) -> Result<u64, String> {
    let started = Instant::now();
    let budget = budget.min(INSPECTION_BUDGET);
    inspection_limits(0, started, budget)?;
    crate::diagnostics::checked_path(root).map_err(|error| error.to_string())?;
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut count = 0;
    let mut bytes = 0u64;
    while let Some((directory, depth)) = pending.pop() {
        inspection_limits(count, started, budget)?;
        if depth > 32 {
            return Err("candidate storage nesting exceeds its inspection budget".into());
        }
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        inspection_limits(count, started, budget)?;
        for entry in entries {
            count += 1;
            inspection_limits(count, started, budget)?;
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            inspection_limits(count, started, budget)?;
            if metadata.file_type().is_symlink() {
                return Err("candidate storage contains an unsupported link".into());
            }
            if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > limit {
                    return Err("candidate workspace exceeds its 1 GiB observed-byte budget".into());
                }
            } else {
                return Err("candidate storage contains an unsupported file kind".into());
            }
        }
    }
    inspection_limits(count, started, budget)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configfile_parser_peer_is_fresh_and_never_recovers_stale_state() {
        let pool =
            std::env::temp_dir().join(format!("parser-pool-{}", crate::random::session_id()));
        let mut original = Workspace::in_pool(&pool).unwrap();
        assert!(original.lifetime.close_if_resolved());
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut parser = original.parser_peer_checked(deadline, &|| Ok(())).unwrap();
        assert_ne!(original.path, parser.path);
        #[cfg(windows)]
        assert!(original.source_envelope(deadline).is_err());
        let mut claim = parser.lifetime.claim().unwrap();
        claim.resolve();
        let stale = parser.path.clone();
        drop(parser.lease.take());
        std::mem::forget(parser);
        assert!(original.parser_peer_checked(deadline, &|| Ok(())).is_err());
        assert!(stale.exists());
        let before = std::fs::read_dir(&pool).unwrap().count();
        assert!(original
            .parser_peer_checked(Instant::now(), &|| Ok(()))
            .is_err());
        assert!(original
            .parser_peer_checked(deadline, &|| Err("original cancelled".into()))
            .is_err());
        assert_eq!(std::fs::read_dir(&pool).unwrap().count(), before);
        original.clean().unwrap();
        // Only the exact test-owned stale slot is removed after proving that
        // admission preserved it. No production recovery path was exercised.
        remove(&pool, &stale).unwrap();
        std::fs::remove_file(pool.join("pool.lock")).unwrap();
        std::fs::remove_dir(pool).unwrap();
    }
    #[test]
    fn lifetime_claims_are_bounded_and_dropping_one_never_proves_retirement() {
        let fence = LifetimeFence::default();
        let mut claims: Vec<_> = (0..MAX_LIFETIME_CLAIMS)
            .map(|_| fence.claim().unwrap())
            .collect();
        assert!(fence.claim().is_err());
        for claim in &mut claims {
            claim.resolve();
        }
        assert!(!fence.unresolved());
        drop(fence.claim().unwrap());
        assert!(fence.unresolved());
        assert!(!LifetimeFence::default().unresolved());
    }
    #[test]
    fn cleanup_closes_future_claims_from_retained_command_clones() {
        let fence = LifetimeFence::default();
        let late = fence.clone();
        let mut claim = fence.claim().unwrap();
        assert!(!fence.close_if_resolved());
        claim.resolve();
        assert!(fence.close_if_resolved());
        assert!(!fence.unresolved());
        assert!(late.claim().is_err());
        assert!(fence.close_if_resolved());
    }
    #[cfg(windows)]
    #[test]
    fn external_source_artifact_custody_resolved_owner_requires_same_fence_and_lease() {
        let pool = Pool::new();
        let mut workspace = Workspace::in_pool(&pool.0).unwrap();
        let budget =
            crate::external_workspace::Budget::new(std::time::Duration::from_secs(30), &|| Ok(()))
                .unwrap();
        let envelope = workspace.path.join("external-sources");
        // Owner-join unit evidence only, not a positive native compilation witness.
        assert!(workspace.lifetime.close_if_resolved());
        let resolved = ResolvedCompilation {
            workspace: workspace.path.clone(),
            envelope: envelope.clone(),
            lifetime: workspace.lifetime.clone(),
        };
        resolved
            .check_workspace_owner(&workspace, &envelope, &budget)
            .unwrap();
        let original_fence = workspace.lifetime.clone();
        workspace.lifetime = LifetimeFence::default();
        assert_eq!(
            resolved
                .check_workspace_owner(&workspace, &envelope, &budget)
                .err()
                .unwrap(),
            "external_artifact_custody_workspace_owner_mismatch"
        );
        workspace.lifetime = original_fence;
        let lease = workspace.lease.take().unwrap();
        assert_eq!(
            resolved
                .check_workspace_owner(&workspace, &envelope, &budget)
                .err()
                .unwrap(),
            "external_artifact_custody_workspace_lease_missing"
        );
        workspace.lease = Some(lease);
        resolved
            .check_workspace_owner(&workspace, &envelope, &budget)
            .unwrap();
        assert_eq!(
            resolved
                .check_workspace_owner(&workspace, &pool.0.join("foreign"), &budget)
                .err()
                .unwrap(),
            "external_artifact_custody_workspace_owner_mismatch"
        );
        drop(resolved);
        workspace.clean().unwrap();
        assert!(!workspace.path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn private_artifact_slot_requires_original_allocation_and_never_reopens_source_envelope() {
        use std::io::Write;
        let pool = Pool::new();
        let mut workspace = Workspace::in_pool(&pool.0).unwrap();
        let mut foreign = Workspace::in_pool(&pool.0).unwrap();
        let b =
            crate::external_workspace::Budget::new(Duration::from_secs(30), &|| Ok(())).unwrap();
        let envelope = workspace.path.join("external-sources");
        // This unit exercises the allocation capability, not real compiler issuance.
        assert!(workspace.lifetime.close_if_resolved());
        let resolved = ResolvedCompilation {
            workspace: workspace.path.clone(),
            envelope: envelope.clone(),
            lifetime: workspace.lifetime.clone(),
        };
        let slot = resolved
            .private_artifact_slot(&workspace, &envelope, &b)
            .unwrap();
        assert_eq!(slot.path(), workspace.path.join("checked-artifact.dll"));
        assert!(
            workspace.source_envelope(b.deadline()).is_err(),
            "retired source staging stays refused"
        );
        assert!(slot.create_new(&resolved, &foreign, &envelope, &b).is_err());
        assert!(!foreign.path.join("checked-artifact.dll").exists());
        let original_lifetime = workspace.lifetime.clone();
        workspace.lifetime = LifetimeFence::default();
        assert!(slot
            .create_new(&resolved, &workspace, &envelope, &b)
            .is_err());
        workspace.lifetime = original_lifetime;
        let lease = workspace.lease.take().unwrap();
        assert!(slot
            .create_new(&resolved, &workspace, &envelope, &b)
            .is_err());
        workspace.lease = Some(lease);
        let mut writer = slot
            .create_new(&resolved, &workspace, &envelope, &b)
            .unwrap();
        writer.write_all(b"first exact bytes").unwrap();
        writer.sync_all().unwrap();
        assert!(slot
            .create_new(&resolved, &workspace, &envelope, &b)
            .is_err());
        assert_eq!(std::fs::read(slot.path()).unwrap(), b"first exact bytes");
        drop(writer);
        drop(slot);
        workspace.clean().unwrap();
        foreign.clean().unwrap();
        assert!(!workspace.path.exists() && !foreign.path.exists());
    }

    #[test]
    fn unresolved_lifetime_refuses_only_its_exact_workspace_without_context() {
        assert!(crate::operation_context::OperationContext::current().is_none());
        let pool = Pool::new();
        let mut uncertain = Workspace::in_pool(&pool.0).unwrap();
        let mut other = Workspace::in_pool(&pool.0).unwrap();
        let mut claim = uncertain.lifetime.claim().unwrap();
        assert!(uncertain.clean().is_err());
        assert!(uncertain.path.join("workspace.json").exists());
        other.clean().unwrap();
        assert!(!other.path.exists());
        claim.resolve();
        uncertain.clean().unwrap();
        assert!(!uncertain.path.exists());
    }
    struct Pool(PathBuf);
    impl Pool {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("aurum-pool-test-{}", crate::random::session_id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Pool {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(windows)]
    #[test]
    fn compiler_cwd_budget_counts_utf16_separator_and_nul() {
        use std::os::windows::ffi::OsStrExt;
        let cwd = PathBuf::from(format!("A:\\{}\u{10000}", "x".repeat(253)));
        assert_eq!(cwd.as_os_str().encode_wide().count(), 258);
        assert!(check_compiler_cwd(&cwd).is_ok());
        let mut trailing = cwd.as_os_str().to_os_string();
        trailing.push("\\");
        assert!(check_compiler_cwd(Path::new(&trailing)).is_ok());
        let mut over = cwd.as_os_str().to_os_string();
        over.push("x");
        assert_eq!(Path::new(&over).as_os_str().encode_wide().count(), 259);
        assert_eq!(
            check_compiler_cwd(Path::new(&over)).unwrap_err(),
            "external_native_cwd_platform_budget_refused"
        );
        over.push("\\");
        assert!(check_compiler_cwd(Path::new(&over)).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn native_compiler_short_temp_uses_real_candidate_layout_and_lease() {
        let temp = Pool::new();
        std::fs::write(temp.0.join("cache-sentinel"), b"unchanged cache").unwrap();
        let mut workspace = Workspace::in_temp_for_native_compile(&temp.0).unwrap();
        let id = workspace
            .path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("candidate-")
            .unwrap();
        assert!(valid_id(id));
        assert_eq!(workspace.path, candidate_path(&workspace.pool, id));
        assert_eq!(workspace.pool.file_name().unwrap(), POOL_NAME);
        assert!(check_compiler_workspace(&workspace.path).is_ok());
        assert!(workspace.lease.is_some() && !workspace.lifetime.unresolved());
        let deadline = Instant::now() + INSPECTION_BUDGET;
        let envelope = workspace.source_envelope(deadline).unwrap();
        envelope.recheck(deadline).unwrap();
        assert!(!envelope.path().exists());
        drop(envelope);
        assert!(!workspace.path.join("compiler-state").exists());
        assert_eq!(
            std::fs::read(temp.0.join("cache-sentinel")).unwrap(),
            b"unchanged cache"
        );
        workspace.clean().unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn native_compiler_long_temp_refuses_before_pool_mutation_or_workspace_claim() {
        use std::os::windows::ffi::OsStrExt;
        let base = Pool::new();
        let id = crate::random::session_id();
        assert!(valid_id(&id));
        let (_, cwd) = compiler_root_and_cwd(&candidate_path(&base.0.join(POOL_NAME), &id));
        let units = cwd.as_os_str().encode_wide().count();
        assert!(
            units < 258,
            "this fixture requires the reviewed short outer TEMP"
        );
        let temp = base.0.join("t".repeat(258 - units));
        std::fs::create_dir(&temp).unwrap();
        let (_, forecast) = compiler_root_and_cwd(&candidate_path(&temp.join(POOL_NAME), &id));
        assert_eq!(forecast.as_os_str().encode_wide().count(), 259);
        std::fs::write(temp.join("source-sentinel"), b"unchanged source").unwrap();
        std::fs::write(temp.join("cache-sentinel"), b"unchanged cache").unwrap();
        let deadline = Instant::now() + INSPECTION_BUDGET;
        let oracle_budget =
            crate::external_workspace::Budget::clipped(deadline, &|| Ok(())).unwrap();
        let oracle_roots = [("allocation", temp.as_path())];
        let before = crate::external_workspace::fixture_all_entry_oracle::capture(
            &oracle_roots,
            &oracle_budget,
        )
        .unwrap();
        assert_eq!(
            Workspace::in_temp_for_native_compile(&temp).err().unwrap(),
            "external_native_cwd_platform_budget_refused"
        );
        assert!(!temp.join(POOL_NAME).exists());
        assert_eq!(
            crate::external_workspace::fixture_all_entry_oracle::capture(
                &oracle_roots,
                &oracle_budget,
            )
            .unwrap(),
            before
        );
        // No Workspace or lifetime fence was constructed. Allocation has no Worker call.
    }
    #[test]
    fn pool_lease_child() {
        let Some(pool) = std::env::var_os("AURUM_POOL_LEASE_FIXTURE") else {
            return;
        };
        let pool = PathBuf::from(pool);
        let workspace = Workspace::in_pool(&pool).unwrap();
        std::fs::write(
            pool.with_extension("ready"),
            workspace.path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }
    #[test]
    fn a_real_host_crash_releases_the_lease_for_safe_reclamation() {
        let pool = Pool::new();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .args([
                "--exact",
                "candidate_workspace::tests::pool_lease_child",
                "--nocapture",
            ])
            .env("AURUM_POOL_LEASE_FIXTURE", &pool.0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = pool.0.with_extension("ready");
        let started = Instant::now();
        while !ready.exists() {
            if started.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("pool lease child did not become ready");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let abandoned = PathBuf::from(std::fs::read_to_string(&ready).unwrap());
        let active = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(active.recovered, 0);
        assert!(abandoned.exists());
        assert!(Workspace::in_pool(&pool.0).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        let replacement = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(replacement.recovered, 1);
        assert!(!abandoned.exists());
        assert!(active.path.exists());
        std::fs::remove_file(ready).unwrap();
    }
    #[test]
    fn corrupted_markers_never_authorize_cleanup() {
        let pool = Pool::new();
        let mut old = Workspace::in_pool(&pool.0).unwrap();
        std::fs::write(old.path.join("workspace.json"), "{}\n").unwrap();
        drop(old.lease.take());
        assert!(Workspace::in_pool(&pool.0).is_err());
        assert!(old.path.exists());
    }
    #[test]
    fn storage_inspection_rejects_excessive_nesting() {
        let pool = Pool::new();
        let workspace = Workspace::in_pool(&pool.0).unwrap();
        let mut directory = workspace.path.clone();
        for _ in 0..34 {
            directory.push("d");
        }
        std::fs::create_dir_all(directory).unwrap();
        assert!(usage(&workspace.path, WORKSPACE_BYTES)
            .unwrap_err()
            .contains("nesting"));
    }
    #[test]
    fn pool_and_workspace_leases_release_retained_descriptors() {
        let pool = Pool::new();
        let manager = manager(&pool.0).unwrap();
        let retained_manager = manager.0.try_clone().unwrap();
        drop(manager);
        let mut workspace = Workspace::in_pool(&pool.0).unwrap();
        let retained_workspace = workspace.lease.as_ref().unwrap().0.try_clone().unwrap();
        let path = workspace.path.clone();
        // Simulate a crashed source owner with another descriptor still open.
        drop(workspace.lease.take());
        let next = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(next.recovered, 1);
        assert!(!path.exists());
        drop(retained_workspace);
        drop(retained_manager);
    }

    #[test]
    fn active_reservations_are_never_swept_and_capacity_is_bounded() {
        let pool = Pool::new();
        let first = Workspace::in_pool(&pool.0).unwrap();
        let second = Workspace::in_pool(&pool.0).unwrap();
        assert!(Workspace::in_pool(&pool.0)
            .err()
            .unwrap()
            .contains("reservations are full"));
        assert!(first.path.exists() && second.path.exists());
        drop(first);
        let third = Workspace::in_pool(&pool.0).unwrap();
        assert!(third.path.exists() && second.path.exists());
    }
    #[test]
    fn abandoned_recognized_copies_are_removed_on_next_allocation() {
        let pool = Pool::new();
        let mut old = Workspace::in_pool(&pool.0).unwrap();
        let path = old.path.clone();
        drop(old.lease.take()); // Simulate OS releasing handles after host death.
        let current = Workspace::in_pool(&pool.0).unwrap();
        assert_eq!(current.recovered, 1);
        assert!(!path.exists());
    }
    #[test]
    fn unknown_state_is_preserved_and_admission_fails_closed() {
        let pool = Pool::new();
        let path = pool.0.join("candidate-unrecognized");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "untouched").unwrap();
        assert!(Workspace::in_pool(&pool.0).is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("keep")).unwrap(),
            "untouched"
        );
    }
    #[test]
    fn observed_bytes_include_hidden_cache_and_userdata() {
        let pool = Pool::new();
        let workspace = Workspace::in_pool(&pool.0).unwrap();
        std::fs::create_dir(workspace.path.join(".godot")).unwrap();
        std::fs::write(workspace.path.join(".godot/cache"), vec![0; 2048]).unwrap();
        assert!(usage(&workspace.path, 1024).is_err());
        assert!(usage(&workspace.path, 4096).unwrap() >= 2048);
    }
    #[test]
    fn expired_scan_budget_refuses_even_absent_or_empty_trees() {
        let pool = Pool::new();
        for root in [pool.0.clone(), pool.0.join("absent")] {
            let error = usage_with_budget(&root, WORKSPACE_BYTES, Duration::ZERO).unwrap_err();
            assert!(error.contains("elapsed inspection budget"), "{error}");
            assert!(error.contains("inspected_entries=0"), "{error}");
            assert!(error.contains("time_limit_ms=0"), "{error}");
        }
        assert!(!pool.0.join("absent").exists());
    }
    #[test]
    fn entry_and_elapsed_refusals_remain_distinct_and_bounded() {
        let started = Instant::now();
        let error =
            inspection_limits(MAX_TREE_ENTRIES + 1, started, INSPECTION_BUDGET).unwrap_err();
        assert!(error.contains("entry inspection budget"), "{error}");
        assert!(error.contains("entry_limit=50000"), "{error}");
        let error =
            inspection_limits(85, started - Duration::from_secs(3), INSPECTION_BUDGET).unwrap_err();
        assert!(error.contains("elapsed inspection budget"), "{error}");
        assert!(error.contains("inspected_entries=85"), "{error}");
        assert!(error.contains("time_limit_ms=2000"), "{error}");
    }
    #[test]
    fn storage_inspection_still_refuses_directory_links() {
        let pool = Pool::new();
        let workspace = Workspace::in_pool(&pool.0).unwrap();
        let target = pool.0.join("outside-source");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), "untouched").unwrap();
        let link = workspace.path.join("redirected");
        #[cfg(windows)]
        {
            let created = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&link)
                .arg(&target)
                .output()
                .unwrap();
            assert!(created.status.success());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(usage(&workspace.path, WORKSPACE_BYTES)
            .unwrap_err()
            .contains("link"));
        drop(workspace);
        assert_eq!(
            std::fs::read_to_string(target.join("keep")).unwrap(),
            "untouched"
        );
    }
}
