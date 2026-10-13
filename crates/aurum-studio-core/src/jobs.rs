//! Bounded background operations for a persistent, authenticated controller.
//! Adapters must authenticate first and precheck the nested operation before
//! submission. This service never replays interrupted work or persists inputs.
use crate::operation_context::{OperationContext, Phase, StopReason};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const MAX_ACTIVE: usize = 2;
pub const MAX_QUEUED: usize = 8;
pub const MAX_TERMINAL: usize = 64;
pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESULT_BYTES: usize = 32 * 1024;
pub const MAX_DEADLINE: Duration = Duration::from_secs(600);
const SUMMARY_BYTES: usize = 8192;
const MAX_STORED: usize = MAX_ACTIVE + MAX_QUEUED + MAX_TERMINAL;

fn random_hex(length: usize) -> Result<String, String> {
    let mut random = crate::random::bytes(length);
    if random.source != crate::random::Source::OperatingSystem {
        random.bytes.fill(0);
        return Err("jobs require operating-system randomness".into());
    }
    let text = random
        .bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    random.bytes.fill(0);
    Ok(text)
}
fn hex(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn same_owner(first: &str, second: &str) -> bool {
    first.len() == second.len()
        && first
            .bytes()
            .zip(second.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// A bearer capability, never included in Debug or persisted as plaintext.
#[derive(Clone)]
pub struct JobOwner(String);
impl std::fmt::Debug for JobOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JobOwner { capability: redacted }")
    }
}
impl JobOwner {
    pub fn new() -> Result<Self, String> {
        random_hex(32).map(Self)
    }
    pub fn from_capability(capability: &str) -> Result<Self, String> {
        if !hex(capability, 64) {
            return Err("invalid job owner capability".into());
        }
        Ok(Self(capability.to_string()))
    }
    pub fn capability(&self) -> &str {
        &self.0
    }
    fn fingerprint(&self) -> String {
        crate::sha256_hex(self.0.as_bytes())
    }
}

pub struct JobTicket {
    pub id: String,
    owner: JobOwner,
}
impl JobTicket {
    pub fn owner_capability(&self) -> &str {
        self.owner.capability()
    }
}
impl std::fmt::Debug for JobTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JobTicket")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    FailureAfterCommit,
    CommitUnconfirmed,
    TerminationUnconfirmed,
    Cancelled,
    TimedOut,
    Interrupted,
}
impl JobState {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::FailureAfterCommit
                | Self::CommitUnconfirmed
                | Self::TerminationUnconfirmed
                | Self::Cancelled
                | Self::TimedOut
                | Self::Interrupted
        )
    }
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureCode {
    OperationFailed,
    WorkerPanicked,
    TerminationUnconfirmed,
    CommitUnconfirmed,
    Cancelled,
    Deadline,
    Interrupted,
}

/// Disk schema contains bounded metadata and hashes only, never work/result text.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    version: u8,
    id: String,
    owner_sha256: String,
    operation: String,
    state: JobState,
    phase: Phase,
    sequence: u64,
    created_ms: u64,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    deadline_ms: u64,
    cancellation_requested: bool,
    committed: bool,
    commit_status_verified: bool,
    #[serde(default)]
    recovery_required: bool,
    result_sha256: Option<String>,
    result_bytes: usize,
    result_complete: bool,
    result_truncated: bool,
    error_code: Option<FailureCode>,
}
type Work = Box<dyn FnOnce(Value) -> Result<Value, String> + Send + 'static>;
struct Entry {
    summary: Summary,
    context: OperationContext,
    request: Option<Value>,
    work: Option<Work>,
    result: Option<Value>,
    evidence_persisted: bool,
}
impl Entry {
    fn snapshot(&self) -> Summary {
        let mut summary = self.summary.clone();
        let progress = self.context.progress();
        summary.phase = progress.phase;
        summary.sequence = progress.sequence;
        summary.cancellation_requested |= self.context.cancellation_requested();
        summary.committed = self.context.committed();
        summary.commit_status_verified &= self.context.commit_status_verified();
        summary.recovery_required |= self.context.recovery_required();
        summary
    }
    fn public(&self) -> Value {
        let mut value =
            serde_json::to_value(self.snapshot()).expect("bounded job schema serializes");
        value.as_object_mut().unwrap().remove("owner_sha256");
        value["evidence_persisted"] = json!(self.evidence_persisted);
        value["result_available"] = json!(self.result.is_some());
        value["terminal"] = json!(self.summary.state.terminal());
        value["result_bytes_exact"] = json!(self.summary.result_complete);
        value
    }
}
#[derive(Default)]
struct State {
    entries: BTreeMap<String, Entry>,
    queue: VecDeque<String>,
    terminal: VecDeque<String>,
    active: usize,
    stopping: bool,
    retention_cleanup_failed: bool,
}
struct Store {
    directory: PathBuf,
    _lease: File,
    known: Mutex<BTreeSet<String>>,
}
impl Store {
    fn open(directory: PathBuf) -> Result<Self, String> {
        crate::diagnostics::checked_path(&directory)
            .map_err(|_| "job summary directory is unsafe")?;
        std::fs::create_dir_all(&directory)
            .map_err(|_| "job summary directory cannot be created")?;
        let directory = directory
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|_| "job summary directory is unavailable")?;
        let lock = directory.join("jobs.lock");
        crate::diagnostics::checked_path(&lock).map_err(|_| "job summary lease is unsafe")?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)
            .map_err(|_| "job summary lease is unavailable")?;
        lease
            .try_lock()
            .map_err(|_| "another job service owns this summary directory")?;
        Ok(Self {
            directory,
            _lease: lease,
            known: Mutex::new(BTreeSet::new()),
        })
    }
    fn path(&self, id: &str) -> Result<PathBuf, String> {
        if !hex(id, 32) {
            return Err("invalid job identity".into());
        }
        let path = self.directory.join(format!("job-{id}.json"));
        crate::diagnostics::checked_path(&path).map_err(|_| "job summary path is unsafe")?;
        Ok(path)
    }
    fn read(&self, id: &str) -> Result<Summary, String> {
        let path = self.path(id)?;
        let metadata = std::fs::metadata(&path).map_err(|_| "job summary is unavailable")?;
        if !metadata.is_file() || metadata.len() > SUMMARY_BYTES as u64 {
            return Err("job summary exceeds its bound".into());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|_| "job summary is unavailable")?
            .take((SUMMARY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "job summary cannot be read")?;
        if bytes.len() > SUMMARY_BYTES {
            return Err("job summary exceeds its bound".into());
        }
        let summary: Summary =
            serde_json::from_slice(&bytes).map_err(|_| "job summary schema is invalid")?;
        if summary.version != 1
            || summary.id != id
            || !hex(&summary.owner_sha256, 64)
            || !operation_name(&summary.operation)
            || summary.sequence == 0
            || summary.result_bytes > MAX_REQUEST_BYTES
            || summary
                .result_sha256
                .as_ref()
                .is_some_and(|hash| !hex(hash, 64))
            || summary.deadline_ms < summary.created_ms
            || summary.deadline_ms - summary.created_ms > MAX_DEADLINE.as_millis() as u64
        {
            return Err("job summary identity or limits are invalid".into());
        }
        Ok(summary)
    }
    fn write(&self, summary: &Summary) -> Result<(), String> {
        let path = self.path(&summary.id)?;
        let mut known = self.known.lock().unwrap_or_else(|error| error.into_inner());
        if !known.contains(&summary.id) && known.len() >= MAX_STORED {
            return Err("job summary retention storage is full".into());
        }
        let bytes = serde_json::to_vec(summary).map_err(|_| "job summary serialization failed")?;
        if bytes.len() > SUMMARY_BYTES {
            return Err("job summary exceeds its bound".into());
        }
        crate::files::write_atomic(&path, &bytes)
            .map_err(|_| "job summary could not be persisted")?;
        known.insert(summary.id.clone());
        Ok(())
    }
    fn remove(&self, id: &str) -> Result<(), String> {
        // Only a recognized bounded receipt at an exact generated name is removed.
        self.read(id)?;
        std::fs::remove_file(self.path(id)?).map_err(|_| "job summary cleanup failed")?;
        self.known
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(id);
        Ok(())
    }
    fn recover(&self) -> Result<State, String> {
        let mut summaries = Vec::new();
        for (index, entry) in std::fs::read_dir(&self.directory)
            .map_err(|_| "job summary directory cannot be inspected")?
            .enumerate()
        {
            if index > MAX_STORED {
                return Err("job summary directory exceeds its entry bound".into());
            }
            let entry = entry.map_err(|_| "job summary directory cannot be inspected")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "jobs.lock" {
                continue;
            }
            let id = name
                .strip_prefix("job-")
                .and_then(|name| name.strip_suffix(".json"))
                .filter(|id| hex(id, 32))
                .ok_or("unrecognized job summary state was preserved")?;
            let mut summary = self.read(id)?;
            self.known.lock().unwrap().insert(id.to_string());
            if !summary.state.terminal() {
                summary.state = JobState::Interrupted;
                summary.phase = Phase::Finished;
                summary.sequence = summary.sequence.saturating_add(1);
                summary.finished_ms = Some(now_ms());
                summary.cancellation_requested = true;
                // The host may have committed after its last durable receipt.
                // Interrupted work is not replayed or labelled uncommitted.
                summary.commit_status_verified = false;
                summary.error_code = Some(FailureCode::Interrupted);
                self.write(&summary)?;
            }
            summaries.push(summary);
        }
        summaries.sort_by_key(|summary| {
            (
                summary.finished_ms.unwrap_or(summary.created_ms),
                summary.id.clone(),
            )
        });
        let remove = summaries.len().saturating_sub(MAX_TERMINAL);
        for summary in summaries.drain(..remove) {
            self.remove(&summary.id)?;
        }
        let mut state = State::default();
        for summary in summaries {
            let id = summary.id.clone();
            let context = OperationContext::recovered_with_verification(
                summary.sequence,
                summary.committed,
                summary.commit_status_verified,
                summary.recovery_required,
            );
            state.terminal.push_back(id.clone());
            state.entries.insert(
                id,
                Entry {
                    summary,
                    context,
                    request: None,
                    work: None,
                    result: None,
                    evidence_persisted: true,
                },
            );
        }
        Ok(state)
    }
}
fn operation_name(op: &str) -> bool {
    !op.is_empty()
        && op.len() <= 64
        && op
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_' || byte.is_ascii_digit())
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    store: Option<Store>,
}
impl Shared {
    fn persist(&self, entry: &mut Entry) {
        entry.evidence_persisted = self
            .store
            .as_ref()
            .is_some_and(|store| store.write(&entry.snapshot()).is_ok());
    }
    fn prune(&self, state: &mut State) {
        while state.terminal.len() > MAX_TERMINAL {
            let id = state.terminal.front().cloned().unwrap();
            if self.remove_terminal(state, &id).is_err() {
                break;
            }
        }
    }
    fn remove_terminal(&self, state: &mut State, id: &str) -> Result<(), String> {
        if let Some(store) = &self.store {
            if store.remove(id).is_err() {
                state.retention_cleanup_failed = true;
                return Err(
                    "job retention cleanup failed; retained receipt remains reachable".into(),
                );
            }
        }
        state.terminal.retain(|recorded| recorded != id);
        state.entries.remove(id);
        state.retention_cleanup_failed = false;
        Ok(())
    }
    fn reserve_retention(&self, state: &mut State) -> Result<(), String> {
        // Reserve a retained-record slot before queuing work, so concurrent
        // completions cannot exceed 64 terminal summaries.
        while state.entries.len() >= MAX_TERMINAL {
            let id = state
                .terminal
                .front()
                .cloned()
                .ok_or("job retained-record capacity is full")?;
            self.remove_terminal(state, &id)?;
        }
        Ok(())
    }
    fn finish_queued(
        &self,
        state: &mut State,
        id: &str,
        reason: StopReason,
        discarded: &mut Vec<Work>,
    ) {
        let Some(entry) = state.entries.get_mut(id) else {
            return;
        };
        if entry.summary.state != JobState::Queued {
            return;
        }
        let (status, code) = stopped(reason);
        entry.summary.state = status;
        entry.summary.error_code = Some(code);
        entry.summary.finished_ms = Some(now_ms());
        entry.request.take();
        if let Some(work) = entry.work.take() {
            discarded.push(work);
        }
        entry.context.phase(Phase::Finished);
        self.persist(entry);
        state.queue.retain(|queued| queued != id);
        state.terminal.push_back(id.to_string());
        self.prune(state);
    }
    fn expire(&self, state: &mut State, discarded: &mut Vec<Work>) {
        let expired: Vec<_> = state
            .queue
            .iter()
            .filter_map(|id| {
                state
                    .entries
                    .get(id)
                    .and_then(|entry| entry.context.stop_reason())
                    .map(|reason| (id.clone(), reason))
            })
            .collect();
        for (id, reason) in expired {
            self.finish_queued(state, &id, reason, discarded);
        }
    }
}
fn stopped(reason: StopReason) -> (JobState, FailureCode) {
    match reason {
        StopReason::Cancelled => (JobState::Cancelled, FailureCode::Cancelled),
        StopReason::Deadline => (JobState::TimedOut, FailureCode::Deadline),
        StopReason::Interrupted => (JobState::Interrupted, FailureCode::Interrupted),
    }
}

pub struct JobManager {
    shared: Arc<Shared>,
}
impl std::fmt::Debug for JobManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JobManager { bounded shared operations }")
    }
}
impl JobManager {
    pub fn new(summary_directory: Option<PathBuf>) -> Result<Self, String> {
        let store = summary_directory.map(Store::open).transpose()?;
        let state = store
            .as_ref()
            .map(Store::recover)
            .transpose()?
            .unwrap_or_default();
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            changed: Condvar::new(),
            store,
        });
        let manager = Self { shared };
        for index in 0..MAX_ACTIVE {
            let shared = manager.shared.clone();
            std::thread::Builder::new()
                .name(format!("aurum-job-worker-{index}"))
                .spawn(move || worker(shared))
                .map_err(|_| "job worker could not start")?;
        }
        let shared = manager.shared.clone();
        std::thread::Builder::new()
            .name("aurum-job-deadlines".into())
            .spawn(move || loop {
                let mut discarded = Vec::new();
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                shared.expire(&mut state, &mut discarded);
                if state.stopping {
                    break;
                }
                let _waiting = shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(25))
                    .unwrap_or_else(|error| error.into_inner());
            })
            .map_err(|_| "job deadline monitor could not start")?;
        Ok(manager)
    }
    /// The supplied precheck must enforce the nested operation and its scope,
    /// including read-only permission, before any queue or receipt is created.
    pub fn submit<P, W>(
        &self,
        owner: &JobOwner,
        request: Value,
        read_only: bool,
        budget: Duration,
        precheck: P,
        work: W,
    ) -> Result<JobTicket, String>
    where
        P: FnOnce(&Value, bool) -> Result<(), String>,
        W: FnOnce(Value) -> Result<Value, String> + Send + 'static,
    {
        if budget.is_zero() || budget > MAX_DEADLINE {
            return Err("job deadline must be between zero and 600 seconds".into());
        }
        let started = Instant::now();
        let created_ms = now_ms();
        validate_request_size(&request)?;
        let operation = request["op"]
            .as_str()
            .filter(|op| operation_name(op))
            .ok_or("job operation name is invalid")?
            .to_string();
        precheck(&request, read_only)?;
        let context = OperationContext::new(started + budget);
        context
            .check()
            .map_err(|_| "job deadline expired during admission")?;
        // Declare callbacks before the lock so their captured owners are always
        // dropped after unlocking, including early refusal and panic paths.
        let mut discarded = Vec::new();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.expire(&mut state, &mut discarded);
        if state.stopping {
            return Err("job service is shutting down".into());
        }
        if state.queue.len() >= MAX_QUEUED {
            return Err("job queue reached its eight-request bound".into());
        }
        self.shared.reserve_retention(&mut state)?;
        let id = random_hex(16)?;
        if state.entries.contains_key(&id) {
            return Err("job identity collision; submission refused".into());
        }
        let mut entry = Entry {
            summary: Summary {
                version: 1,
                id: id.clone(),
                owner_sha256: owner.fingerprint(),
                operation,
                state: JobState::Queued,
                phase: Phase::Queued,
                sequence: 1,
                created_ms,
                started_ms: None,
                finished_ms: None,
                deadline_ms: created_ms.saturating_add(budget.as_millis() as u64),
                cancellation_requested: false,
                committed: false,
                commit_status_verified: true,
                recovery_required: false,
                result_sha256: None,
                result_bytes: 0,
                result_complete: false,
                result_truncated: false,
                error_code: None,
            },
            context,
            request: Some(request),
            work: Some(Box::new(work)),
            result: None,
            evidence_persisted: self.shared.store.is_some(),
        };
        if let Some(store) = &self.shared.store {
            if let Err(error) = store.write(&entry.snapshot()) {
                if let Some(work) = entry.work.take() {
                    discarded.push(work);
                }
                return Err(error);
            }
        }
        state.entries.insert(id.clone(), entry);
        state.queue.push_back(id.clone());
        drop(state);
        self.shared.changed.notify_all();
        Ok(JobTicket {
            id,
            owner: owner.clone(),
        })
    }
    pub fn status(
        &self,
        id: &str,
        owner: &JobOwner,
        after_sequence: Option<u64>,
    ) -> Result<Value, String> {
        let mut discarded = Vec::new();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.expire(&mut state, &mut discarded);
        let entry = owned(&state, id, owner)?;
        let sequence = entry.context.progress().sequence;
        if after_sequence.is_some_and(|after| after > sequence) {
            return Err("job progress cursor is newer than this job".into());
        }
        if after_sequence == Some(sequence) {
            return Ok(
                json!({"ok":true,"job_id":id,"sequence":sequence,"unchanged":true,"terminal":entry.summary.state.terminal(),"retention_cleanup_failed":state.retention_cleanup_failed}),
            );
        }
        Ok(
            json!({"ok":true,"job":entry.public(),"result":entry.result,"summary_only":entry.summary.state.terminal() && entry.result.is_none(),"retention_cleanup_failed":state.retention_cleanup_failed}),
        )
    }
    pub fn list(&self, owner: &JobOwner, limit: usize) -> Result<Value, String> {
        if limit == 0 || limit > MAX_TERMINAL {
            return Err("job list limit must be between one and 64".into());
        }
        let mut discarded = Vec::new();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.expire(&mut state, &mut discarded);
        let fingerprint = owner.fingerprint();
        let mut entries: Vec<_> = state
            .entries
            .values()
            .filter(|entry| same_owner(&entry.summary.owner_sha256, &fingerprint))
            .collect();
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(entry.summary.created_ms),
                entry.summary.id.clone(),
            )
        });
        let truncated = entries.len() > limit;
        let entries: Vec<_> = entries.into_iter().take(limit).map(Entry::public).collect();
        Ok(
            json!({"ok":true,"jobs":entries,"truncated":truncated,"retention_cleanup_failed":state.retention_cleanup_failed}),
        )
    }
    pub fn cancel(&self, id: &str, owner: &JobOwner) -> Result<Value, String> {
        let mut discarded = Vec::new();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.expire(&mut state, &mut discarded);
        let entry = owned(&state, id, owner)?;
        if entry.summary.state.terminal() {
            return Ok(json!({"ok":true,"requested":false,"terminal":true}));
        }
        entry
            .context
            .request_cancel()
            .map_err(|_| "job has committed; cancellation cannot undo it")?;
        if entry.summary.state == JobState::Queued {
            self.shared
                .finish_queued(&mut state, id, StopReason::Cancelled, &mut discarded);
        } else {
            let entry = state.entries.get_mut(id).unwrap();
            entry.summary.state = JobState::Cancelling;
            self.shared.persist(entry);
        }
        drop(state);
        self.shared.changed.notify_all();
        Ok(json!({"ok":true,"requested":true}))
    }
    pub fn forget(&self, id: &str, owner: &JobOwner) -> Result<Value, String> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = owned(&state, id, owner)?;
        if !entry.summary.state.terminal() {
            return Err("active or queued jobs cannot be forgotten".into());
        }
        if let Some(store) = &self.shared.store {
            if let Err(error) = store.remove(id) {
                state.retention_cleanup_failed = true;
                return Err(error);
            }
        }
        state.entries.remove(id);
        state.terminal.retain(|recorded| recorded != id);
        state.retention_cleanup_failed = false;
        Ok(json!({"ok":true,"forgotten":true}))
    }
    /// Shutdown requests cancellation without waiting on the preview mutex.
    /// Running closures must cooperate; they are not labelled stopped until done.
    pub fn shutdown(&self) {
        let mut discarded = Vec::new();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.stopping {
            return;
        }
        state.stopping = true;
        let queued: Vec<_> = state.queue.iter().cloned().collect();
        for id in queued {
            if let Some(entry) = state.entries.get(&id) {
                entry.context.interrupt();
            }
            self.shared
                .finish_queued(&mut state, &id, StopReason::Interrupted, &mut discarded);
        }
        for entry in state
            .entries
            .values_mut()
            .filter(|entry| !entry.summary.state.terminal())
        {
            entry.context.interrupt();
            entry.summary.state = JobState::Cancelling;
            self.shared.persist(entry);
        }
        drop(state);
        self.shared.changed.notify_all();
    }
}
impl Drop for JobManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn owned<'a>(state: &'a State, id: &str, owner: &JobOwner) -> Result<&'a Entry, String> {
    let fingerprint = owner.fingerprint();
    state
        .entries
        .get(id)
        .filter(|entry| same_owner(&entry.summary.owner_sha256, &fingerprint))
        .ok_or_else(|| "job is unavailable or not owned by this capability".into())
}
struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.limit {
            return Err(io::Error::other("job JSON exceeds its byte bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn bounded_json(value: &Value, limit: usize) -> io::Result<Vec<u8>> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
    Ok(writer.bytes)
}
pub(crate) fn validate_request_size(request: &Value) -> Result<(), String> {
    bounded_json(request, MAX_REQUEST_BYTES)
        .map(|_| ())
        .map_err(|_| "job request exceeds 2 MiB".into())
}
struct ResultCapture {
    hash: crate::Sha256,
    bytes: usize,
    complete: bool,
    deadline: Instant,
}
impl Write for ResultCapture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "job result summary deadline expired",
            ));
        }
        if bytes.len() > MAX_REQUEST_BYTES.saturating_sub(self.bytes) {
            self.bytes = MAX_REQUEST_BYTES;
            return Err(io::Error::other(
                "job result summary reached its byte bound",
            ));
        }
        self.hash.update(bytes);
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn capture_result(value: &Value, deadline: Instant) -> ResultCapture {
    let mut capture = ResultCapture {
        hash: crate::Sha256::new(),
        bytes: 0,
        complete: false,
        deadline,
    };
    capture.complete = serde_json::to_writer(&mut capture, value).is_ok();
    capture
}
fn worker(shared: Arc<Shared>) {
    loop {
        let (id, context, request, work) = loop {
            let mut discarded = Vec::new();
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            shared.expire(&mut state, &mut discarded);
            if state.stopping {
                return;
            }
            if let Some(id) = state.queue.pop_front() {
                let entry = state.entries.get_mut(&id).unwrap();
                entry.summary.state = JobState::Running;
                entry.summary.started_ms = Some(now_ms());
                entry.context.phase(Phase::Running);
                let tuple = (
                    id,
                    entry.context.clone(),
                    entry.request.take().unwrap(),
                    entry.work.take().unwrap(),
                );
                shared.persist(entry);
                state.active += 1;
                debug_assert!(state.active <= MAX_ACTIVE);
                break tuple;
            }
            let _waiting = shared
                .changed
                .wait_timeout(state, Duration::from_millis(25))
                .unwrap_or_else(|error| error.into_inner());
        };
        let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            context.scope(|| work(request))
        }));
        let (mut outcome, mut result, capture, error_code) = match execution {
            Ok(Ok(Ok(value))) => {
                let capture = capture_result(&value, context.deadline());
                let retained = capture.complete && capture.bytes <= MAX_RESULT_BYTES;
                (
                    if value["ok"] == false {
                        JobState::Failed
                    } else {
                        JobState::Succeeded
                    },
                    retained.then_some(value),
                    Some(capture),
                    None,
                )
            }
            Ok(Ok(Err(error))) => {
                // Authorized result queries may inspect a bounded failure;
                // only its hash/size and stable category enter disk summaries.
                let value = json!({"ok":false,"error":crate::diagnostics::text_line(&error, &[])});
                let capture = capture_result(&value, context.deadline());
                let retained = capture.complete && capture.bytes <= MAX_RESULT_BYTES;
                (
                    JobState::Failed,
                    retained.then_some(value),
                    Some(capture),
                    Some(FailureCode::OperationFailed),
                )
            }
            Ok(_) => (
                JobState::Failed,
                None,
                None,
                Some(FailureCode::OperationFailed),
            ),
            Err(_) => (
                JobState::Failed,
                None,
                None,
                Some(FailureCode::WorkerPanicked),
            ),
        };
        let mut error_code = error_code;
        if context.recovery_required() || !context.commit_status_verified() {
            outcome = if context.committed() {
                JobState::FailureAfterCommit
            } else {
                JobState::CommitUnconfirmed
            };
            error_code = Some(FailureCode::CommitUnconfirmed);
        } else if context.committed() && outcome == JobState::Failed {
            outcome = JobState::FailureAfterCommit;
        } else if !context.committed() {
            if let Some(reason) = context.stop_reason() {
                let (status, code) = stopped(reason);
                outcome = status;
                error_code = Some(code);
                result = None;
            }
        }
        if context.termination_unconfirmed() {
            outcome = JobState::TerminationUnconfirmed;
            error_code = Some(FailureCode::TerminationUnconfirmed);
            result = None;
        }
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.active -= 1;
        let entry = state.entries.get_mut(&id).unwrap();
        entry.summary.state = outcome;
        entry.summary.finished_ms = Some(now_ms());
        entry.summary.error_code = error_code;
        if let Some(capture) = capture {
            entry.summary.result_complete = capture.complete;
            entry.summary.result_truncated = !capture.complete || capture.bytes > MAX_RESULT_BYTES;
            entry.summary.result_bytes = capture.bytes;
            if capture.complete {
                entry.summary.result_sha256 = Some(capture.hash.hex());
            }
        }
        entry.result = result;
        context.phase(Phase::Finished);
        shared.persist(entry);
        state.terminal.push_back(id);
        shared.prune(&mut state);
        drop(state);
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("aurum-jobs-test-{}", crate::random::session_id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn permit(_: &Value, read_only: bool) -> Result<(), String> {
        if read_only {
            Err("write permission required".into())
        } else {
            Ok(())
        }
    }
    fn submit(
        manager: &JobManager,
        owner: &JobOwner,
        work: impl FnOnce(Value) -> Result<Value, String> + Send + 'static,
    ) -> JobTicket {
        manager
            .submit(
                owner,
                json!({"op":"validate"}),
                false,
                Duration::from_secs(10),
                permit,
                work,
            )
            .unwrap()
    }
    fn terminal(manager: &JobManager, owner: &JobOwner, id: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let value = manager.status(id, owner, None).unwrap();
            let state: JobState = serde_json::from_value(value["job"]["state"].clone()).unwrap();
            if state.terminal() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "job failed to settle within fixture deadline"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn uncertain_rollback_keeps_evidence_instead_of_claiming_clean_cancellation() {
        let manager = JobManager::new(None).unwrap();
        let owner = JobOwner::new().unwrap();
        let ticket = submit(&manager, &owner, |_| {
            let context = OperationContext::current().unwrap();
            let guard = context.commit_barrier().unwrap();
            context.request_cancel().unwrap();
            context.mark_recovery_required();
            drop(guard);
            Ok(
                json!({"ok":false,"recovery_required":true,"source_rolled_back":false,"revision_id":"0123456789abcdef0123456789abcdef"}),
            )
        });
        let value = terminal(&manager, &owner, &ticket.id);
        assert_eq!(value["job"]["state"], "commit_unconfirmed");
        assert_eq!(value["job"]["commit_status_verified"], false);
        assert_eq!(value["job"]["recovery_required"], true);
        assert_eq!(value["result"]["source_rolled_back"], false);
        assert_eq!(
            value["result"]["revision_id"],
            "0123456789abcdef0123456789abcdef"
        );
    }
    type Release = Arc<(Mutex<bool>, Condvar)>;
    fn blocked(
        started: mpsc::Sender<()>,
        release: Release,
    ) -> impl FnOnce(Value) -> Result<Value, String> + Send {
        move |_| {
            let context = OperationContext::current().unwrap();
            started.send(()).unwrap();
            let mut released = release.0.lock().unwrap();
            while !*released {
                context.check().map_err(|error| error.to_string())?;
                released = release
                    .1
                    .wait_timeout(released, Duration::from_millis(10))
                    .unwrap()
                    .0;
            }
            Ok(json!({"ok":true}))
        }
    }
    fn occupy(manager: &JobManager, owner: &JobOwner) -> Release {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (sender, receiver) = mpsc::channel();
        for _ in 0..MAX_ACTIVE {
            submit(manager, owner, blocked(sender.clone(), release.clone()));
        }
        for _ in 0..MAX_ACTIVE {
            receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        release
    }
    fn release(release: &Release) {
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
    }
    #[test]
    fn saturation_permissions_and_payload_limits_refuse_before_execution() {
        let manager = JobManager::new(None).unwrap();
        let owner = JobOwner::new().unwrap();
        let denied = manager.submit(
            &owner,
            json!({"op":"write"}),
            true,
            Duration::from_secs(1),
            permit,
            |_| panic!("denied work ran"),
        );
        assert!(denied.is_err());
        assert!(manager.list(&owner, MAX_TERMINAL).unwrap()["jobs"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(manager
            .submit(
                &owner,
                json!({"op":"validate","text":"x".repeat(MAX_REQUEST_BYTES)}),
                false,
                Duration::from_secs(1),
                permit,
                |_| panic!("oversized work ran")
            )
            .is_err());
        assert!(manager
            .submit(
                &owner,
                json!({"op":"validate"}),
                false,
                Duration::ZERO,
                permit,
                |_| panic!("expired work ran")
            )
            .is_err());
        let held = occupy(&manager, &owner);
        for _ in 0..MAX_QUEUED {
            submit(&manager, &owner, |_| Ok(json!({"ok":true})));
        }
        assert!(manager
            .submit(
                &owner,
                json!({"op":"validate"}),
                false,
                Duration::from_secs(1),
                permit,
                |_| panic!("excess work ran")
            )
            .unwrap_err()
            .contains("eight-request"));
        let state = manager.shared.state.lock().unwrap();
        assert_eq!(state.active, MAX_ACTIVE);
        assert_eq!(state.queue.len(), MAX_QUEUED);
        drop(state);
        release(&held);
    }
    #[test]
    fn queued_deadline_expires_without_waiting_for_an_active_worker() {
        let manager = JobManager::new(None).unwrap();
        let owner = JobOwner::new().unwrap();
        let held = occupy(&manager, &owner);
        let executed = Arc::new(AtomicUsize::new(0));
        let observed = executed.clone();
        let ticket = manager
            .submit(
                &owner,
                json!({"op":"validate"}),
                false,
                Duration::from_millis(20),
                permit,
                move |_| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"ok":true}))
                },
            )
            .unwrap();
        assert_eq!(
            terminal(&manager, &owner, &ticket.id)["job"]["state"],
            "timed_out"
        );
        assert_eq!(executed.load(Ordering::SeqCst), 0);
        release(&held);
    }
    #[test]
    fn owner_capabilities_reconnect_and_wrong_owners_cannot_cancel_or_forget() {
        let manager = JobManager::new(None).unwrap();
        let owner = JobOwner::new().unwrap();
        let other = JobOwner::new().unwrap();
        let held = occupy(&manager, &owner);
        let ticket = submit(&manager, &owner, |_| Ok(json!({"ok":true})));
        assert!(manager.cancel(&ticket.id, &other).is_err());
        assert!(manager.forget(&ticket.id, &other).is_err());
        assert!(manager.status(&ticket.id, &other, None).is_err());
        assert!(manager.list(&other, 64).unwrap()["jobs"]
            .as_array()
            .unwrap()
            .is_empty());
        let reconnected = JobOwner::from_capability(ticket.owner_capability()).unwrap();
        let before = manager.status(&ticket.id, &reconnected, None).unwrap();
        let sequence = before["job"]["sequence"].as_u64().unwrap();
        assert_eq!(
            manager
                .status(&ticket.id, &reconnected, Some(sequence))
                .unwrap()["unchanged"],
            true
        );
        manager.cancel(&ticket.id, &reconnected).unwrap();
        let cancelled = terminal(&manager, &owner, &ticket.id);
        assert_eq!(cancelled["job"]["state"], "cancelled");
        assert!(cancelled["job"]["sequence"].as_u64().unwrap() > sequence);
        manager.forget(&ticket.id, &owner).unwrap();
        assert!(manager.status(&ticket.id, &owner, None).is_err());
        assert!(!format!("{owner:?} {ticket:?}").contains(owner.capability()));
        release(&held);
    }
    #[test]
    fn discarded_work_releases_captured_owners_outside_the_manager_lock() {
        struct Reenter {
            manager: std::sync::Weak<JobManager>,
            owner: JobOwner,
            sender: mpsc::Sender<()>,
        }
        impl Drop for Reenter {
            fn drop(&mut self) {
                if let Some(manager) = self.manager.upgrade() {
                    manager.list(&self.owner, 1).unwrap();
                }
                self.sender.send(()).unwrap();
            }
        }
        let manager = Arc::new(JobManager::new(None).unwrap());
        let owner = JobOwner::new().unwrap();
        let held = occupy(&manager, &owner);
        let (sender, receiver) = mpsc::channel();
        let captured = Reenter {
            manager: Arc::downgrade(&manager),
            owner: owner.clone(),
            sender,
        };
        let ticket = submit(&manager, &owner, move |_| {
            drop(captured);
            Ok(json!({"ok":true}))
        });
        let cancelling = manager.clone();
        let cancelling_owner = owner.clone();
        std::thread::spawn(move || cancelling.cancel(&ticket.id, &cancelling_owner).unwrap());
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        release(&held);
    }
    #[test]
    fn summaries_are_private_bounded_and_oversized_committed_results_stay_successful() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("summaries");
        let manager = JobManager::new(Some(directory.clone())).unwrap();
        let owner = JobOwner::new().unwrap();
        let secret = "private-source-and-raw-argument-sentinel";
        let request = json!({"op":"validate","text":secret});
        let ticket = manager
            .submit(
                &owner,
                request,
                false,
                Duration::from_secs(5),
                permit,
                move |_| {
                    let context = OperationContext::current().unwrap();
                    context.phase(Phase::Publishing);
                    let commit = context.commit_barrier().unwrap();
                    context.request_cancel().unwrap();
                    commit.finish();
                    Ok(json!({"ok":true,"private":secret,"large":"x".repeat(MAX_RESULT_BYTES + 1)}))
                },
            )
            .unwrap();
        let status = terminal(&manager, &owner, &ticket.id);
        assert_eq!(status["job"]["state"], "succeeded");
        assert_eq!(status["job"]["committed"], true);
        assert_eq!(status["job"]["cancellation_requested"], true);
        assert_eq!(status["job"]["result_truncated"], true);
        assert_eq!(status["job"]["result_complete"], true);
        assert_eq!(status["job"]["result_available"], false);
        assert_eq!(status["job"]["result_sha256"].as_str().unwrap().len(), 64);
        assert!(status["result"].is_null());
        let disk =
            std::fs::read_to_string(directory.join(format!("job-{}.json", ticket.id))).unwrap();
        assert!(!disk.contains(secret));
        assert!(!disk.contains(owner.capability()));
        assert!(!disk.contains("large"));
        assert!(disk.len() <= SUMMARY_BYTES);
        let failed = submit(&manager, &owner, |_| {
            OperationContext::current()
                .unwrap()
                .commit_barrier()
                .unwrap()
                .finish();
            Err("raw sensitive failure text".into())
        });
        assert_eq!(
            terminal(&manager, &owner, &failed.id)["job"]["state"],
            "failure_after_commit"
        );
        let uncertain = submit(&manager, &owner, |_| {
            let context = OperationContext::current().unwrap();
            context.request_cancel().unwrap();
            context.mark_termination_unconfirmed();
            Err("OS cleanup failed".into())
        });
        assert_eq!(
            terminal(&manager, &owner, &uncertain.id)["job"]["state"],
            "termination_unconfirmed"
        );
    }
    #[test]
    fn failed_admission_persistence_drops_captured_owners_after_unlocking() {
        struct Reenter {
            manager: std::sync::Weak<JobManager>,
            owner: JobOwner,
            sender: mpsc::Sender<()>,
        }
        impl Drop for Reenter {
            fn drop(&mut self) {
                self.manager
                    .upgrade()
                    .unwrap()
                    .list(&self.owner, 1)
                    .unwrap();
                self.sender.send(()).unwrap();
            }
        }
        let fixture = Fixture::new();
        let manager = Arc::new(JobManager::new(Some(fixture.0.join("summaries"))).unwrap());
        let owner = JobOwner::new().unwrap();
        // Exercise the real persistence refusal without relying on platform
        // permission differences or changing a filesystem outside this fixture.
        for index in 0..MAX_STORED {
            manager
                .shared
                .store
                .as_ref()
                .unwrap()
                .known
                .lock()
                .unwrap()
                .insert(format!("{index:032x}"));
        }
        let (sender, receiver) = mpsc::channel();
        let captured = Reenter {
            manager: Arc::downgrade(&manager),
            owner: owner.clone(),
            sender,
        };
        let submitting = manager.clone();
        let thread = std::thread::spawn(move || {
            submitting.submit(
                &owner,
                json!({"op":"validate"}),
                false,
                Duration::from_secs(5),
                permit,
                move |_| {
                    drop(captured);
                    panic!("refused work executed")
                },
            )
        });
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(thread.join().unwrap().is_err());
        assert!(manager.shared.state.lock().unwrap().entries.is_empty());
    }
    #[cfg(windows)]
    #[test]
    fn undeletable_receipt_stays_reachable_and_blocks_new_retention_capacity() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let directory = fixture.0.join("summaries");
        let manager = JobManager::new(Some(directory.clone())).unwrap();
        let owner = JobOwner::new().unwrap();
        let mut first = None;
        for _ in 0..MAX_TERMINAL {
            let ticket = submit(&manager, &owner, |_| Ok(json!({"ok":true})));
            terminal(&manager, &owner, &ticket.id);
            if first.is_none() {
                first = Some(ticket.id);
            }
        }
        let first = first.unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(directory.join(format!("job-{first}.json")))
            .unwrap();
        let refused = manager.submit(
            &owner,
            json!({"op":"validate"}),
            false,
            Duration::from_secs(5),
            permit,
            |_| panic!("retention-blocked work executed"),
        );
        assert!(refused.unwrap_err().contains("retention cleanup failed"));
        assert_eq!(
            manager.status(&first, &owner, None).unwrap()["retention_cleanup_failed"],
            true
        );
        assert_eq!(
            manager.shared.state.lock().unwrap().entries.len(),
            MAX_TERMINAL
        );
        assert_eq!(
            manager
                .shared
                .store
                .as_ref()
                .unwrap()
                .known
                .lock()
                .unwrap()
                .len(),
            MAX_TERMINAL
        );
        drop(held);
        let accepted = submit(&manager, &owner, |_| Ok(json!({"ok":true})));
        terminal(&manager, &owner, &accepted.id);
        assert!(manager.status(&first, &owner, None).is_err());
        assert_eq!(
            manager.list(&owner, 64).unwrap()["retention_cleanup_failed"],
            false
        );
        assert_eq!(
            manager.shared.state.lock().unwrap().terminal.len(),
            MAX_TERMINAL
        );
        assert_eq!(
            std::fs::read_dir(directory).unwrap().count(),
            MAX_TERMINAL + 1
        );
    }
    #[test]
    fn terminal_retention_and_recovery_never_replay_work() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("summaries");
        let manager = JobManager::new(Some(directory.clone())).unwrap();
        let owner = JobOwner::new().unwrap();
        let first = submit(&manager, &owner, |_| Ok(json!({"ok":true})));
        terminal(&manager, &owner, &first.id);
        for _ in 0..MAX_TERMINAL {
            let ticket = submit(&manager, &owner, |_| Ok(json!({"ok":true})));
            terminal(&manager, &owner, &ticket.id);
        }
        assert!(manager.status(&first.id, &owner, None).is_err());
        assert_eq!(
            manager.list(&owner, 64).unwrap()["jobs"]
                .as_array()
                .unwrap()
                .len(),
            MAX_TERMINAL
        );
        assert_eq!(
            std::fs::read_dir(&directory).unwrap().count(),
            MAX_TERMINAL + 1
        );
        let held = occupy(&manager, &owner);
        let queued = submit(&manager, &owner, |_| panic!("interrupted work replayed"));
        manager.shutdown();
        assert_eq!(
            terminal(&manager, &owner, &queued.id)["job"]["state"],
            "interrupted"
        );
        release(&held);
        let deadline = Instant::now() + Duration::from_secs(3);
        while manager.list(&owner, 64).unwrap()["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| {
                !serde_json::from_value::<JobState>(job["state"].clone())
                    .unwrap()
                    .terminal()
            })
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(manager);
        let recovered = loop {
            match JobManager::new(Some(directory.clone())) {
                Ok(manager) => break manager,
                Err(_) => {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        assert_eq!(
            recovered.status(&queued.id, &owner, None).unwrap()["job"]["state"],
            "interrupted"
        );
        assert_eq!(
            recovered.status(&queued.id, &owner, None).unwrap()["summary_only"],
            true
        );
        assert!(recovered.shared.state.lock().unwrap().queue.is_empty());
    }
    #[test]
    fn abandoned_nonterminal_receipts_recover_as_interrupted_without_replay() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("summaries");
        let owner = JobOwner::new().unwrap();
        let id = random_hex(16).unwrap();
        let created_ms = now_ms();
        let store = Store::open(directory.clone()).unwrap();
        store
            .write(&Summary {
                version: 1,
                id: id.clone(),
                owner_sha256: owner.fingerprint(),
                operation: "validate".into(),
                state: JobState::Running,
                phase: Phase::Compiling,
                sequence: 4,
                created_ms,
                started_ms: Some(created_ms),
                finished_ms: None,
                deadline_ms: created_ms + 5000,
                cancellation_requested: false,
                committed: false,
                commit_status_verified: true,
                recovery_required: false,
                result_sha256: None,
                result_bytes: 0,
                result_complete: false,
                result_truncated: false,
                error_code: None,
            })
            .unwrap();
        drop(store);
        let manager = JobManager::new(Some(directory)).unwrap();
        let status = manager.status(&id, &owner, None).unwrap();
        assert_eq!(status["job"]["state"], "interrupted");
        assert_eq!(status["job"]["phase"], "finished");
        assert_eq!(status["job"]["sequence"], 5);
        assert_eq!(status["summary_only"], true);
        let state = manager.shared.state.lock().unwrap();
        assert_eq!(state.active, 0);
        assert!(state.queue.is_empty());
        assert!(state.entries[&id].work.is_none());
        assert!(state.entries[&id].request.is_none());
    }
    #[cfg(windows)]
    #[test]
    fn active_cancellation_ends_owned_descendants_and_preserves_an_unrelated_process() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new();
        let tree = fixture.0.join("owned");
        let foreign = fixture.0.join("foreign");
        std::fs::create_dir(&tree).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let executable = std::env::current_exe().unwrap();
        let helper = "worker_windows::tests::worker_tree_helper";
        struct Witness(std::process::Child);
        impl Drop for Witness {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut witness = Witness(
            std::process::Command::new(&executable)
                .creation_flags(0x08000000)
                .args(["--exact", helper, "--nocapture"])
                .env("AURUM_WORKER_FIXTURE", &foreign)
                .env("AURUM_WORKER_ROLE", "leaf")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let witness_identity = crate::ownership::inspect(witness.0.id()).unwrap();
        let manager = JobManager::new(None).unwrap();
        let owner = JobOwner::new().unwrap();
        let job_tree = tree.clone();
        let job_executable = executable.clone();
        let ticket = submit(&manager, &owner, move |_| {
            // No owned_tree flag: the active operation must own this transient
            // root and its children without changing synchronous callers.
            crate::Command::new(job_executable)
                .args(["--exact", helper, "--nocapture"])
                .env("AURUM_WORKER_FIXTURE", job_tree.to_string_lossy())
                .env("AURUM_WORKER_ROLE", "root")
                .run(Duration::from_secs(30))
                .map_err(|error| error.to_string())?;
            Ok(json!({"ok":true}))
        });
        let deadline = Instant::now() + Duration::from_secs(8);
        while !tree.join("ready").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let records: Vec<_> = ["root.pid", "leaf.pid"]
            .into_iter()
            .map(|name| {
                let pid = std::fs::read_to_string(tree.join(name))
                    .unwrap()
                    .parse()
                    .unwrap();
                let live = crate::ownership::inspect(pid).unwrap();
                crate::ownership::OwnershipRecord {
                    session: "fixture".into(),
                    executable: live.executable,
                    pid,
                    started: live.started.unwrap(),
                    project: tree.clone(),
                    kind: crate::ownership::ProcessKind::Worker,
                }
            })
            .collect();
        let begin = Instant::now();
        assert_eq!(
            manager.status(&ticket.id, &owner, None).unwrap()["job"]["state"],
            "running"
        );
        manager.cancel(&ticket.id, &owner).unwrap();
        assert!(begin.elapsed() < Duration::from_secs(1));
        assert_eq!(
            terminal(&manager, &owner, &ticket.id)["job"]["state"],
            "cancelled"
        );
        assert!(records.iter().all(crate::ownership::recorded_process_gone));
        assert_eq!(
            crate::ownership::inspect(witness.0.id()).unwrap(),
            witness_identity
        );
        assert!(witness.0.try_wait().unwrap().is_none());
    }
}
