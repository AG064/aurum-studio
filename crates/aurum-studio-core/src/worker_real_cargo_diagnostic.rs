//! cfg(test) only: real Cargo on the controlled external-workspace fixture.
//! Raw data has no Debug/Serde/journal route. This is not the synthetic permit.
use super::*;
use crate::candidate_workspace::{LifetimeFence, Workspace};
use crate::external_workspace::{KnownCargoFixturePermit, StagedSources};
use std::io::Write;
const CAP: usize = 32 * 1024;
#[derive(Clone, Copy, Debug)]
pub(crate) enum Stage {
    Spawn,
    Read,
    Finish,
}
#[derive(Clone, Copy, Debug)]
pub(super) enum Pipe {
    Stdout,
    Stderr,
}
impl Pipe {
    fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    pub stage: Stage,
    pub kind: io::ErrorKind,
    pub raw: Option<i32>,
}
#[derive(Clone, Copy, Debug)]
pub(super) enum Origin {
    Pipe(Pipe),
    Census(&'static str),
}
#[derive(Clone)]
struct OriginFailure {
    origin: Origin,
    kind: io::ErrorKind,
    raw: Option<i32>,
}
#[derive(Clone, Default)]
struct Channel {
    prefix: Vec<u8>,
    read: u64,
    consumed: u64,
    native_eof: bool,
    truncated: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Completion {
    pub root_exit: i32,
    pub stdout: u64,
    pub stderr: u64,
    pub eof: (bool, bool),
    pub tree: bool,
    pub readers: bool,
}
#[derive(Clone, Default)]
struct Data {
    channels: [Channel; 2],
    primary: Option<Failure>,
    origin: Option<OriginFailure>,
    returned_stdout: u64,
    observed_exit: Option<i32>,
    observed_eof: (bool, bool),
    completion: Option<Completion>,
    cleanup: Option<(bool, bool)>,
    scope_refusal: Option<&'static str>,
    admission: Option<Weak<Admission>>,
    stream_returned: bool,
}
#[derive(Clone)]
pub(super) struct Sink {
    data: Arc<Mutex<Data>>,
    owner: Weak<Owner>,
}
impl Sink {
    fn new(controller: &StreamController) -> io::Result<Self> {
        let mut data = Data::default();
        for channel in &mut data.channels {
            channel
                .prefix
                .try_reserve_exact(CAP)
                .map_err(|_| io::Error::other("known Cargo diagnostic allocation refused"))?;
        }
        Ok(Self {
            data: Arc::new(Mutex::new(data)),
            owner: Arc::downgrade(&controller.owner),
        })
    }
    pub(super) fn admitted(&self, admission: &Arc<Admission>) -> io::Result<()> {
        if !Weak::ptr_eq(&self.owner, &Arc::downgrade(&admission.owner)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "known Cargo owner mismatch",
            ));
        }
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if data.admission.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "known Cargo admission already bound",
            ));
        }
        data.admission = Some(Arc::downgrade(admission));
        Ok(())
    }
    pub(super) fn bytes(&self, pipe: Pipe, bytes: &[u8]) {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let c = &mut data.channels[pipe.index()];
        c.read = c.read.saturating_add(bytes.len() as u64);
        let keep = bytes.len().min(CAP.saturating_sub(c.prefix.len()));
        c.prefix.extend_from_slice(&bytes[..keep]);
        c.truncated |= keep < bytes.len();
    }
    pub(super) fn consumed(&self, pipe: Pipe, count: usize) {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let c = &mut data.channels[pipe.index()];
        c.consumed = c.consumed.saturating_add(count as u64);
    }
    pub(super) fn end(&self, pipe: Pipe, end: End) {
        if matches!(end, End::Eof) {
            self.data.lock().unwrap_or_else(|e| e.into_inner()).channels[pipe.index()].native_eof =
                true;
        }
    }
    pub(super) fn origin(&self, origin: Origin, kind: io::ErrorKind, raw: Option<i32>) {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .origin
            .get_or_insert(OriginFailure { origin, kind, raw });
    }
    pub(crate) fn api<T>(&self, stage: Stage, result: &io::Result<T>) {
        if let Err(error) = result {
            self.data
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .primary
                .get_or_insert(Failure {
                    stage,
                    kind: error.kind(),
                    raw: error.raw_os_error(),
                });
        }
    }
    pub(super) fn returned(&self, result: &io::Result<usize>) {
        if let Ok(count) = result {
            let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
            d.returned_stdout = d.returned_stdout.saturating_add(*count as u64);
        }
    }
    pub(super) fn observed(&self, resources: &Resources, exit: Option<i32>) {
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        d.observed_eof = (resources.stdout_eof, resources.stderr_eof);
        if exit.is_some() {
            d.observed_exit = exit;
        }
        drop(d);
        let census = resources
            .worker
            .as_ref()
            .map(|w| &w.census)
            .or_else(|| resources.pending.as_ref().map(|w| &w.census));
        if let Some((kind, raw, phase)) = census.and_then(|c| c.fixture_failure()) {
            self.origin(Origin::Census(phase), kind, raw);
        }
    }
    pub(super) fn completed(&self, r: &StreamReceipt) {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completion = Some(Completion {
            root_exit: r.root_exit_code,
            stdout: r.stdout_bytes,
            stderr: r.stderr_bytes,
            eof: (r.stdout_eof, r.stderr_eof),
            tree: r.owned_tree_retired,
            readers: r.owned_readers_joined,
        });
    }
    pub(super) fn cleanup(&self, had_resources: bool, ok: bool) {
        self.data.lock().unwrap_or_else(|e| e.into_inner()).cleanup = Some((had_resources, ok));
    }
}
pub(super) struct PipeTap {
    pub sink: Sink,
    pub pipe: Pipe,
}
struct Binding {
    program: PathBuf,
    argv0: PathBuf,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    member_manifest: PathBuf,
    target: PathBuf,
    application_sha256: String,
    lifetime: LifetimeFence,
    parent_deadline: Instant,
}
/// Holds the SAME controller, not a new owner. The private fixture retains it
/// in ManuallyDrop to host exit if original resources are unresolved.
pub(crate) struct DiagnosticHandle {
    sink: Sink,
    controller: StreamController,
    binding: Binding,
    artifact_root: PathBuf,
    started: AtomicBool,
    write_started: AtomicBool,
}
pub(crate) struct BoundBuild<'a> {
    handle: &'a DiagnosticHandle,
    request: BinarySpawn<'a>,
    exact_args: Vec<String>,
    exact_deadline: Instant,
}
impl DiagnosticHandle {
    pub(crate) fn for_fixture(
        permit: KnownCargoFixturePermit,
        workspace: &Workspace,
        staged: &StagedSources,
        controller: &StreamController,
        invocation: crate::external_native::KnownCargoInvocation,
    ) -> io::Result<Self> {
        permit.check(workspace, staged)?;
        let application_sha256 = invocation.application_sha256();
        if application_sha256.len() != 64
            || !application_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "known Cargo application hash refused",
            ));
        }
        Ok(Self {
            sink: Sink::new(controller)?,
            controller: controller.clone(),
            binding: Binding {
                program: invocation.program().to_path_buf(),
                argv0: invocation.argv0().to_path_buf(),
                cwd: invocation.cwd().to_path_buf(),
                env: invocation.env().to_vec(),
                member_manifest: staged.member_manifest.clone(),
                target: invocation.target().to_path_buf(),
                application_sha256: application_sha256.to_owned(),
                lifetime: workspace.lifetime.clone(),
                parent_deadline: invocation.parent_deadline(),
            },
            artifact_root: permit.into_artifact_root(),
            started: AtomicBool::new(false),
            write_started: AtomicBool::new(false),
        })
    }
    pub(crate) fn bind_build<'a>(
        &'a self,
        controller: &StreamController,
        checked: &crate::external_native::KnownCargoBuild,
        request: BinarySpawn<'a>,
    ) -> io::Result<BoundBuild<'a>> {
        let exact_args = checked.arguments();
        let valid = Arc::ptr_eq(&controller.owner, &self.controller.owner)
            && request.program == self.binding.program
            && request.argv0 == self.binding.argv0
            && request.directory == Some(self.binding.cwd.as_path())
            && request.overrides == self.binding.env
            && request
                .lifetime
                .same_origin_for_cargo_fixture(&self.binding.lifetime)
            && request.deadline <= self.binding.parent_deadline
            && request.limits.stdout_bytes == 4 * 1024 * 1024
            && request.limits.stderr_bytes == 64 * 1024
            && checked.belongs_to(&self.binding.member_manifest, &self.binding.target)
            && request.args == exact_args;
        if !valid
            || self
                .started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            let error = io::Error::new(
                io::ErrorKind::InvalidInput,
                "known Cargo build binding refused",
            );
            self.sink.api::<()>(
                Stage::Spawn,
                &Err(io::Error::new(
                    error.kind(),
                    "known Cargo build binding refused",
                )),
            );
            return Err(error);
        }
        Ok(BoundBuild {
            handle: self,
            exact_args,
            exact_deadline: request.deadline,
            request,
        })
    }
    pub(crate) fn scope_refusal(&self, stage: &'static str) {
        self.sink
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scope_refusal
            .get_or_insert(stage);
    }
    pub(crate) fn api<T>(&self, stage: Stage, result: &io::Result<T>) {
        self.sink.api(stage, result);
    }
    pub(crate) fn completion(&self) -> Option<Completion> {
        self.sink
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completion
            .clone()
    }
    pub(crate) fn compiler_error_codes(&self) -> Vec<String> {
        let d = self.sink.data.lock().unwrap_or_else(|e| e.into_inner());
        compiler_codes(&d.channels[0].prefix)
    }
    pub(crate) fn write_private(&self) -> io::Result<()> {
        if self.write_started.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                "known Cargo diagnostic write already attempted"));
        }
        let d = self
            .sink
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let path = self
            .artifact_root
            .join("real-cargo-build-private-diagnostic.bin");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        writeln!(file,"REAL_CARGO_KNOWN_FIXTURE simulated_test_grant=true application_sha256={} scope={:?} admission_bound={} stream_returned={} returned_stdout={}",
            self.binding.application_sha256,d.scope_refusal,d.admission.is_some(),d.stream_returned,d.returned_stdout)?;
        let primary = d.primary.as_ref().map(|f| (f.stage, f.kind, f.raw));
        let origin = d.origin.as_ref().map(|f| match f.origin {
            Origin::Pipe(p) => (Some(p), None, f.kind, f.raw),
            Origin::Census(p) => (None, Some(p), f.kind, f.raw),
        });
        writeln!(
            file,
            "primary={primary:?} origin={origin:?} completion={:?} cleanup={:?} observed={:?}",
            d.completion,
            d.cleanup,
            (d.observed_exit, d.observed_eof)
        )?;
        writeln!(
            file,
            "complete_prefix_compiler_codes={:?}",
            compiler_codes(&d.channels[0].prefix)
        )?;
        for c in &d.channels {
            writeln!(
                file,
                "read={} consumed={} prefix={} truncated={} native_eof={}",
                c.read,
                c.consumed,
                c.prefix.len(),
                c.truncated,
                c.native_eof
            )?;
            file.write_all(&c.prefix)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()
    }
}
impl BoundBuild<'_> {
    pub(crate) fn spawn(self) -> io::Result<BinaryStream> {
        if self.request.args != self.exact_args
            || self.request.deadline != self.exact_deadline
            || !self
                .request
                .lifetime
                .same_origin_for_cargo_fixture(&self.handle.binding.lifetime)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "known Cargo exact request changed",
            ));
        }
        let result = BinaryStream::spawn_cargo_fixture(
            &self.handle.controller,
            self.request,
            self.handle.sink.clone(),
        );
        self.handle.sink.api(Stage::Spawn, &result);
        if result.is_ok() {
            self.handle
                .sink
                .data
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stream_returned = true;
        }
        result
    }
}
impl Drop for DiagnosticHandle {
    fn drop(&mut self) {
        if !self.write_started.load(Ordering::Acquire) {
            let _ = self.write_private();
        }
    }
}
fn compiler_codes(bytes: &[u8]) -> Vec<String> {
    let mut codes = Vec::new();
    for line in bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|line| line.ends_with(b"\n"))
    {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-message" || value["message"]["level"] != "error" {
            continue;
        }
        if let Some(code) = value["message"]["code"]["code"].as_str() {
            if codes.len() < 16
                && code.len() == 5
                && code.as_bytes()[0] == b'E'
                && code.as_bytes()[1..].iter().all(u8::is_ascii_digit)
            {
                codes.push(code.to_owned());
            }
        }
    }
    codes
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // Called only by the source-unit case in external_native::tests. It creates
    // no permit, worker or native admission and returns no diagnostic authority.
    // The caller constructs its mock selected-member seal in the private module.
    pub(crate) fn assert_selected_build_binding_mutations(
        checked: &crate::external_native::KnownCargoBuild,
        foreign_checked: &crate::external_native::KnownCargoBuild,
    ) {
        let controller = StreamController::for_runtime();
        let original = LifetimeFence::default();
        let args = checked.arguments();
        let parent_deadline = Instant::now() + Duration::from_secs(30);
        let handle = std::mem::ManuallyDrop::new(DiagnosticHandle {
            sink: Sink::new(&controller).unwrap(),
            controller: controller.clone(),
            binding: Binding {
                program: PathBuf::from("A:/mock-checked-cargo.exe"),
                argv0: PathBuf::from("A:/mock-invocation-cargo.exe"),
                cwd: PathBuf::from("A:/mock-fixture/compiler-state/cwd"),
                env: vec![("MOCK_CURATED".into(), "fixture-only".into())],
                member_manifest: PathBuf::from(&args[2]),
                target: PathBuf::from(&args[13]),
                application_sha256: "A".repeat(64),
                lifetime: original.clone(),
                parent_deadline,
            },
            artifact_root: PathBuf::from("A:/unwritten-mock-artifact-root"),
            started: AtomicBool::new(false),
            write_started: AtomicBool::new(false),
        });
        fn refused(
            handle: &DiagnosticHandle,
            checked: &crate::external_native::KnownCargoBuild,
            controller: &StreamController,
            request: BinarySpawn<'_>,
        ) {
            let result = handle.bind_build(controller, checked, request);
            assert!(
                matches!(result, Err(ref error) if error.kind() == io::ErrorKind::InvalidInput)
            );
            assert!(!handle.started.load(Ordering::Acquire));
            assert!(handle.sink.data.lock().unwrap().admission.is_none());
            assert!(!handle.sink.data.lock().unwrap().stream_returned);
            assert!(!handle.binding.lifetime.unresolved());
        }
        fn request<'a>(
            handle: &'a DiagnosticHandle,
            original: &'a LifetimeFence,
            args: &'a [String],
            deadline: Instant,
        ) -> BinarySpawn<'a> {
            BinarySpawn {
                program: handle.binding.program.as_path(),
                argv0: handle.binding.argv0.as_path(),
                args,
                directory: Some(handle.binding.cwd.as_path()),
                overrides: &handle.binding.env,
                lifetime: original,
                deadline,
                limits: StreamLimits {
                    stdout_bytes: 4 * 1024 * 1024,
                    stderr_bytes: 64 * 1024,
                },
            }
        }
        // Includes the exact selected package (9), triple (11), manifest/target
        // and every fixed option. Empty, shortened and appended vectors refuse.
        for index in 0..args.len() {
            let mut changed = args.clone();
            changed[index].push_str("-changed");
            refused(
                &handle,
                checked,
                &controller,
                request(&handle, &original, &changed, parent_deadline),
            );
        }
        for changed in [Vec::new(), args[..args.len() - 1].to_vec(), {
            let mut extra = args.clone();
            extra.push("extra".into());
            extra
        }] {
            refused(
                &handle,
                checked,
                &controller,
                request(&handle, &original, &changed, parent_deadline),
            );
        }
        let foreign_path = Path::new("A:/foreign.exe");
        let foreign_env = vec![("MOCK_CURATED".into(), "changed".into())];
        let foreign_fence = LifetimeFence::default();
        for index in 0..8 {
            let mut changed = request(&handle, &original, &args, parent_deadline);
            match index {
                0 => changed.program = foreign_path,
                1 => changed.argv0 = foreign_path,
                2 => changed.directory = None,
                3 => changed.overrides = &foreign_env,
                4 => changed.lifetime = &foreign_fence,
                5 => changed.deadline = parent_deadline + Duration::from_nanos(1),
                6 => changed.limits.stdout_bytes -= 1,
                7 => changed.limits.stderr_bytes -= 1,
                _ => unreachable!(),
            }
            refused(&handle, checked, &controller, changed);
        }
        let foreign_controller = StreamController::for_runtime();
        refused(
            &handle,
            checked,
            &foreign_controller,
            request(&handle, &original, &args, parent_deadline),
        );
        // Matching the foreign seal's complete arguments cannot transfer its
        // manifest/target binding to this handle. Every other request field fits.
        let foreign_args = foreign_checked.arguments();
        assert_ne!(foreign_args[2], args[2]);
        assert_ne!(foreign_args[13], args[13]);
        for index in 0..args.len() {
            if index != 2 && index != 13 {
                assert_eq!(foreign_args[index], args[index]);
            }
        }
        assert!(
            !foreign_checked.belongs_to(&handle.binding.member_manifest, &handle.binding.target,)
        );
        refused(
            &handle,
            foreign_checked,
            &controller,
            request(&handle, &original, &foreign_args, parent_deadline),
        );
        let bound = handle
            .bind_build(
                &controller,
                checked,
                request(&handle, &original, &args, parent_deadline),
            )
            .unwrap();
        assert_eq!(bound.exact_args, checked.arguments());
        assert!(handle.started.load(Ordering::Acquire));
        assert!(handle.sink.data.lock().unwrap().admission.is_none());
        assert!(!handle.sink.data.lock().unwrap().stream_returned);
        assert!(!original.unresolved());
        drop(bound); // Never call spawn; no native work or artifact is created.
        let duplicate = handle.bind_build(
            &controller,
            checked,
            request(&handle, &original, &args, parent_deadline),
        );
        assert!(matches!(duplicate, Err(ref error) if error.kind() == io::ErrorKind::InvalidInput));
        assert!(handle.started.load(Ordering::Acquire));
        assert!(handle.sink.data.lock().unwrap().admission.is_none());
        assert!(!handle.sink.data.lock().unwrap().stream_returned);
        assert!(!original.unresolved());
    }
    #[test]
    fn known_cargo_prefix_caps_keep_distinct_counts_and_no_cleanup_receipt() {
        let controller = StreamController::for_runtime();
        let sink = Sink::new(&controller).unwrap();
        sink.bytes(Pipe::Stdout, &vec![b'x'; CAP * 2]);
        sink.bytes(Pipe::Stderr, &vec![b'e'; CAP * 2]);
        sink.consumed(Pipe::Stdout, 100);
        sink.returned(&Ok(80));
        sink.end(Pipe::Stdout, End::Eof);
        sink.end(Pipe::Stderr, End::Stopped);
        sink.cleanup(false, true);
        let data = sink.data.lock().unwrap();
        assert_eq!(data.channels[0].prefix.len(), CAP);
        assert_eq!(data.channels[1].prefix.len(), CAP);
        assert_eq!(data.channels[0].read, (CAP * 2) as u64);
        assert_eq!(data.channels[0].consumed, 100);
        assert_eq!(data.returned_stdout, 80);
        assert!(
            data.channels[0].truncated && data.channels[1].truncated && data.channels[0].native_eof
        );
        assert!(!data.channels[1].native_eof);
        assert!(data.completion.is_none());
    }
    #[test]
    fn known_cargo_private_dump_keeps_first_bytes_and_never_retries_on_drop() {
        let root = std::env::temp_dir().join(format!("cargo-diagnostic-write-{}", crate::random::session_id()));
        std::fs::create_dir(&root).unwrap();
        let controller = StreamController::for_runtime();
        let handle = DiagnosticHandle {
            sink: Sink::new(&controller).unwrap(), controller: controller.clone(),
            binding: Binding {
                program: PathBuf::from("A:/unexecuted-cargo.exe"),
                argv0: PathBuf::from("A:/unexecuted-cargo.exe"), cwd: root.clone(),
                env: Vec::new(), member_manifest: root.join("Cargo.toml"), target: root.join("target"),
                application_sha256: "A".repeat(64), lifetime: LifetimeFence::default(),
                parent_deadline: Instant::now() + Duration::from_secs(30),
            },
            artifact_root: root.clone(), started: AtomicBool::new(false),
            write_started: AtomicBool::new(false),
        };
        handle.sink.bytes(Pipe::Stderr, b"private diagnostic fixture bytes\n");
        handle.sink.api::<()>(Stage::Finish, &Err(io::Error::from_raw_os_error(5)));
        handle.write_private().unwrap();
        let path = root.join("real-cargo-build-private-diagnostic.bin");
        let first = std::fs::read(&path).unwrap();
        assert!(first.windows(b"private diagnostic fixture bytes".len())
            .any(|bytes| bytes == b"private diagnostic fixture bytes"));
        assert_eq!(handle.write_private().unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        drop(handle);
        assert_eq!(std::fs::read(&path).unwrap(), first);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn known_cargo_typed_failure_is_not_replaced_by_cleanup() {
        let controller = StreamController::for_runtime();
        let sink = Sink::new(&controller).unwrap();
        let first: io::Result<()> = Err(io::Error::from_raw_os_error(5));
        sink.api(Stage::Spawn, &first);
        let later: io::Result<()> = Err(io::Error::new(io::ErrorKind::TimedOut, "later"));
        sink.api(Stage::Finish, &later);
        sink.origin(
            Origin::Pipe(Pipe::Stderr),
            io::ErrorKind::PermissionDenied,
            Some(5),
        );
        sink.cleanup(true, true);
        let data = sink.data.lock().unwrap();
        let primary = data.primary.as_ref().unwrap();
        assert!(matches!(primary.stage, Stage::Spawn));
        assert_eq!(primary.raw, Some(5));
        assert_eq!(primary.kind, io::ErrorKind::PermissionDenied);
        assert!(data.completion.is_none());
    }
    #[test]
    fn known_cargo_codes_require_complete_json_and_never_return_rendered_payload() {
        let bytes=b"{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"code\":{\"code\":\"E0308\"},\"rendered\":\"PRIVATE_MARKER\"}}\n";
        assert_eq!(compiler_codes(bytes), vec!["E0308"]);
        assert!(compiler_codes(&bytes[..bytes.len() - 1]).is_empty());
    }
    #[test]
    fn known_cargo_codes_refuse_private_or_malformed_code_fields() {
        for code in [
            "",
            "PRIVATE_MARKER",
            "UPPERCASEPAYLOAD",
            "E123",
            "E12345",
            "E12X4",
            "e1234",
            "E１２３４",
        ] {
            let mut line = serde_json::to_vec(&serde_json::json!({"reason":"compiler-message",
                "message":{"level":"error","code":{"code":code},"rendered":"PRIVATE_MARKER"}}))
            .unwrap();
            line.push(b'\n');
            assert!(compiler_codes(&line).is_empty());
        }
        let complete = b"{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"code\":{\"code\":\"E0001\"},\"rendered\":\"PRIVATE_MARKER\"}}\n";
        assert_eq!(compiler_codes(complete), vec!["E0001"]);
        assert!(compiler_codes(&complete[..complete.len() - 1]).is_empty());
    }
    #[test]
    fn shared_fence_identity_is_not_an_admission_or_retirement_proof() {
        let original = LifetimeFence::default();
        let clone = original.clone();
        let foreign = LifetimeFence::default();
        assert!(original.same_origin_for_cargo_fixture(&clone));
        assert!(!original.same_origin_for_cargo_fixture(&foreign));
        assert!(!original.unresolved());
        assert!(!foreign.unresolved());
    }
}
