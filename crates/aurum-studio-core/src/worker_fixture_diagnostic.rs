//! Private artificial-child diagnostics. No generic project opt-in or raw Debug/Serialize.
use super::*;
use std::io::Write;
const CAP: usize = 32 * 1024; // Two partitions, combined 64 KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Spawn,
    Read,
    Finish,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub(super) struct Failure {
    pub stage: Stage,
    pub kind: io::ErrorKind,
    pub raw: Option<i32>,
}
#[derive(Clone, Copy, Debug)]
pub(super) enum Origin {
    Pipe(Pipe),
    Census(&'static str),
}
#[derive(Clone, Debug)]
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
struct Completion {
    root_exit: i32,
    stdout: u64,
    stderr: u64,
    eof: (bool, bool),
    tree: bool,
    readers: bool,
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
    stream_returned: bool,
}
#[derive(Debug, Default)]
struct CompilerFacts {
    messages: usize,
    errors: usize,
    build_finished: Option<bool>,
    codes: Vec<String>,
}
fn compiler_facts(bytes: &[u8]) -> CompilerFacts {
    let mut facts = CompilerFacts::default();
    for line in bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|line| line.ends_with(b"\n"))
    {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        match value["reason"].as_str() {
            Some("compiler-message") => {
                facts.messages += 1;
                if value["message"]["level"] == "error" {
                    facts.errors += 1;
                }
                if let Some(code) = value["message"]["code"]["code"].as_str() {
                    if facts.codes.len() < 16
                        && code.len() <= 32
                        && code
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    {
                        facts.codes.push(code.to_owned());
                    }
                }
            }
            Some("build-finished") => facts.build_finished = value["success"].as_bool(),
            _ => {}
        }
    }
    facts
}
// Data deliberately has neither Debug nor Serialize: it contains private raw bytes.
#[derive(Default)]
struct Pause {
    state: Mutex<(bool, usize)>,
    changed: Condvar,
}
#[derive(Clone)]
pub(super) struct Sink {
    data: Arc<Mutex<Data>>,
    pause: Arc<Pause>,
}
impl Sink {
    fn new() -> io::Result<Self> {
        let mut data = Data::default();
        for channel in &mut data.channels {
            channel
                .prefix
                .try_reserve_exact(CAP)
                .map_err(|_| io::Error::other("fixture diagnostic allocation refused"))?;
        }
        Ok(Self {
            data: Arc::new(Mutex::new(data)),
            pause: Arc::new(Pause::default()),
        })
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
    pub(super) fn api<T>(&self, stage: Stage, result: &io::Result<T>) {
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
    pub(super) fn completed(&self, receipt: &StreamReceipt) {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .completion = Some(Completion {
            root_exit: receipt.root_exit_code,
            stdout: receipt.stdout_bytes,
            stderr: receipt.stderr_bytes,
            eof: (receipt.stdout_eof, receipt.stderr_eof),
            tree: receipt.owned_tree_retired,
            readers: receipt.owned_readers_joined,
        });
    }
    pub(super) fn cleanup(&self, had_resources: bool, ok: bool) {
        // API disposition only. Never creates or overwrites the completion receipt.
        self.data.lock().unwrap_or_else(|e| e.into_inner()).cleanup = Some((had_resources, ok));
    }
    pub(super) fn before_read(&self) {
        let mut s = self.pause.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s.0 {
            return;
        }
        s.1 += 1;
        self.pause.changed.notify_all();
        let end = Instant::now() + Duration::from_secs(10);
        while s.0 {
            if Instant::now() >= end {
                drop(s);
                panic!("fixture reader barrier deadline");
            }
            s = self
                .pause
                .changed
                .wait_timeout(s, end.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    fn release(&self) {
        self.pause.state.lock().unwrap_or_else(|e| e.into_inner()).0 = false;
        self.pause.changed.notify_all();
    }
}
#[derive(Clone)]
pub(super) struct PipeTap {
    pub sink: Sink,
    pub pipe: Pipe,
}

/// No exported constructor. Only this module's artificial-fixture tests can mint it.
struct FixtureDiagnostic<'a> {
    sink: Sink,
    program: std::path::PathBuf,
    directory: std::path::PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    lifetime: &'a LifetimeFence,
}
impl FixtureDiagnostic<'_> {
    fn request(&self, deadline: Instant) -> BinarySpawn<'_> {
        BinarySpawn {
            program: &self.program,
            argv0: &self.program,
            args: &self.args,
            directory: Some(&self.directory),
            overrides: &self.env,
            lifetime: self.lifetime,
            deadline,
            limits: StreamLimits {
                stdout_bytes: 1024 * 1024,
                stderr_bytes: 64 * 1024,
            },
        }
    }
    fn spawn(&self, c: &StreamController, request: BinarySpawn<'_>) -> io::Result<BinaryStream> {
        let valid = request.program == self.program
            && request.argv0 == self.program
            && request.directory == Some(self.directory.as_path())
            && request.args == self.args
            && request.overrides == self.env
            && std::ptr::eq(request.lifetime, self.lifetime);
        let result = if valid {
            BinaryStream::spawn_environment_fixture(c, request, true, Some(self.sink.clone()))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fixture capability mismatch",
            ))
        };
        self.sink.api(Stage::Spawn, &result);
        if result.is_ok() {
            self.sink
                .data
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stream_returned = true;
        }
        result
    }
    fn snapshot(&self) -> Data {
        self.sink
            .data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn write_private_artifact(&self, path: &Path) -> io::Result<()> {
        let d = self.snapshot();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let primary = d.primary.as_ref().map(|p| (p.stage, p.kind, p.raw));
        let origin = d.origin.as_ref().map(|o| match o.origin {
            Origin::Pipe(pipe) => (Some(pipe), None, o.kind, o.raw),
            Origin::Census(phase) => (None, Some(phase), o.kind, o.raw),
        });
        let complete = d
            .completion
            .as_ref()
            .map(|r| (r.root_exit, r.stdout, r.stderr, r.eof, r.tree, r.readers));
        writeln!(file, "FIXTURE_ONLY primary={primary:?} origin={origin:?} completion={complete:?} cleanup={:?} observed={:?}",
            d.cleanup, (d.observed_exit, d.observed_eof))?;
        let facts = compiler_facts(&d.channels[0].prefix);
        writeln!(
            file,
            "compiler_messages={} compiler_errors={} build_finished={:?} codes={:?}",
            facts.messages, facts.errors, facts.build_finished, facts.codes
        )?;
        for channel in &d.channels {
            writeln!(
                file,
                "read={} consumed={} prefix={} truncated={} native_eof={}",
                channel.read,
                channel.consumed,
                channel.prefix.len(),
                channel.truncated,
                channel.native_eof
            )?;
            file.write_all(&channel.prefix)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()
    }
}
impl Drop for FixtureDiagnostic<'_> {
    fn drop(&mut self) {
        self.sink.release();
        // Best-effort first-failure evidence only; never overwrite an earlier artifact.
        let _ = self.write_private_artifact(&self.directory.join("private-fixture-diagnostic.bin"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate_workspace::Workspace;
    const CHILD: &str = "worker_windows::binary::fixture_diagnostic::tests::fixture_child";
    fn workspace() -> std::mem::ManuallyDrop<Workspace> {
        for path in [
            std::env::temp_dir().canonicalize().unwrap(),
            std::env::current_exe().unwrap(),
        ] {
            let path = crate::project::clean_path(path);
            assert!(
                path.is_absolute()
                    && path
                        .to_string_lossy()
                        .get(..2)
                        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("A:"))
            );
        }
        std::mem::ManuallyDrop::new(Workspace::create().unwrap())
    }
    fn fixture<'a>(workspace: &'a Workspace, role: &str) -> FixtureDiagnostic<'a> {
        let program = std::env::current_exe().unwrap();
        assert!(crate::project::clean_path(program.clone())
            .to_string_lossy()
            .starts_with("A:"));
        assert!(workspace.path.to_string_lossy().starts_with("A:"));
        FixtureDiagnostic {
            sink: Sink::new().unwrap(),
            program,
            directory: workspace.path.clone(),
            args: vec!["--exact".into(), CHILD.into(), "--nocapture".into()],
            env: vec![
                ("SystemRoot".into(), std::env::var("SystemRoot").unwrap()),
                ("TEMP".into(), workspace.path.to_string_lossy().into_owned()),
                ("TMP".into(), workspace.path.to_string_lossy().into_owned()),
                ("AURUM_DIAG_FIXTURE_CHILD".into(), role.into()),
            ],
            lifetime: &workspace.lifetime,
        }
    }
    fn run(
        cap: &FixtureDiagnostic<'_>,
        c: &StreamController,
        work: Instant,
    ) -> (BinaryStream, Vec<u8>) {
        let mut stream = cap.spawn(c, cap.request(work)).unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = stream
                .read_before_checked(&mut buf, work, |_| Ok(()))
                .unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..n]);
        }
        (stream, bytes)
    }
    #[test]
    fn fixture_nonzero_keeps_original_completion_and_private_bytes() {
        let _serial = crate::worker_windows::binary::pool_test_lock();
        let mut ws = workspace();
        let c = StreamController::for_runtime();
        let cap = fixture(&ws, "nonzero");
        let (mut stream, out) = run(&cap, &c, Instant::now() + Duration::from_secs(30));
        assert!(stream.finish_checked(|_| Ok(())).is_err());
        let d = cap.snapshot();
        assert_eq!(d.primary.as_ref().unwrap().stage, Stage::Finish);
        assert_eq!(compiler_facts(&d.channels[0].prefix).errors, 1);
        let receipt = d.completion.as_ref().unwrap();
        assert_eq!(receipt.root_exit, 7);
        assert!(receipt.tree && receipt.readers && receipt.eof == (true, true));
        assert_eq!(d.channels[0].prefix, out);
        assert!(d.channels[1].prefix.starts_with(b"fixture-stderr"));
        stream.retire().unwrap();
        assert_eq!(cap.snapshot().completion.unwrap().root_exit, 7);
        cap.write_private_artifact(&ws.path.join("private-fixture-diagnostic.bin"))
            .unwrap();
        drop(stream);
        drop(cap);
        ws.clean().unwrap();
    }
    #[test]
    fn fixture_success_and_bounded_prefixes_continue_to_real_eof() {
        let _serial = crate::worker_windows::binary::pool_test_lock();
        let mut ws = workspace();
        let c = StreamController::for_runtime();
        let cap = fixture(&ws, "bulk");
        let (mut stream, _) = run(&cap, &c, Instant::now() + Duration::from_secs(30));
        let receipt = stream.finish_checked(|_| Ok(())).unwrap();
        assert_eq!(receipt.root_exit_code, 0);
        let d = cap.snapshot();
        assert!(d.primary.is_none());
        assert_eq!(
            d.channels.iter().map(|p| p.prefix.len()).sum::<usize>(),
            64 * 1024
        );
        assert!(d
            .channels
            .iter()
            .all(|p| p.truncated && p.native_eof && p.read > CAP as u64));
        assert_eq!(d.returned_stdout, d.channels[0].consumed);
        drop(stream);
        drop(cap);
        ws.clean().unwrap();
    }
    #[test]
    fn default_capture_has_no_raw_tap_and_mismatched_request_is_refused() {
        let _serial = crate::worker_windows::binary::pool_test_lock();
        let mut ws = workspace();
        let c = StreamController::for_runtime();
        let cap = fixture(&ws, "success");
        let mut stream =
            BinaryStream::spawn_cleared(&c, cap.request(Instant::now() + Duration::from_secs(30)))
                .unwrap();
        assert!(stream.fixture.is_none());
        stream.retire().unwrap();
        let mut bad = cap.request(Instant::now() + Duration::from_secs(30));
        bad.args = &[];
        assert!(cap.spawn(&c, bad).is_err());
        assert!(!cap.snapshot().stream_returned);
        assert!(cap.snapshot().channels.iter().all(|p| p.prefix.is_empty()));
        drop(stream);
        drop(cap);
        ws.clean().unwrap();
    }
    #[test]
    fn typed_error_first_wins_without_display_or_cleanup_receipt_inference() {
        let sink = Sink::new().unwrap();
        let error = io::Error::from_raw_os_error(5);
        sink.origin(
            Origin::Pipe(Pipe::Stderr),
            error.kind(),
            error.raw_os_error(),
        );
        sink.api::<()>(Stage::Read, &Err(error));
        sink.cleanup(false, true);
        sink.api::<()>(Stage::Finish, &Err(io::Error::other("PRIVATE_FAKE_MARKER")));
        let d = sink.data.lock().unwrap();
        let p = d.primary.as_ref().unwrap();
        assert_eq!((p.stage, p.raw), (Stage::Read, Some(5)));
        assert!(d.completion.is_none());
    }
    #[test]
    fn late_observer_preserves_physical_receipt_without_success() {
        let _serial = crate::worker_windows::binary::pool_test_lock();
        let mut ws = workspace();
        let c = StreamController::for_runtime();
        let cap = fixture(&ws, "success");
        let work = Instant::now() + Duration::from_secs(30);
        let (mut stream, _) = run(&cap, &c, work);
        let mut crossed = false;
        let result = stream.finish_checked(|deadline| {
            if cap.snapshot().completion.is_some() {
                let lock = Mutex::new(());
                let wake = Condvar::new();
                let mut guard = lock.lock().unwrap();
                while Instant::now() < deadline {
                    guard = wake
                        .wait_timeout(guard, deadline.saturating_duration_since(Instant::now()))
                        .unwrap()
                        .0;
                }
                assert!(Instant::now() < work);
                crossed = true;
            }
            Ok(())
        });
        assert!(crossed && result.is_err());
        assert!(cap.snapshot().completion.is_some());
        drop(stream);
        drop(cap);
        ws.clean().unwrap();
    }
    #[test]
    fn uncertain_readers_retain_sink_and_cannot_create_completion_receipt() {
        let _serial = crate::worker_windows::binary::pool_test_lock();
        let mut ws = workspace();
        let c = StreamController::for_runtime();
        let cap = fixture(&ws, "success");
        cap.sink.pause.state.lock().unwrap().0 = true;
        let work = Instant::now() + Duration::from_secs(30);
        let mut stream = cap.spawn(&c, cap.request(work)).unwrap();
        {
            let end = Instant::now() + Duration::from_secs(10);
            let mut state = cap.sink.pause.state.lock().unwrap();
            while state.1 < 2 {
                if Instant::now() >= end {
                    drop(state);
                    panic!("fixture readers not bound");
                }
                state = cap
                    .sink
                    .pause
                    .changed
                    .wait_timeout(state, end.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
        assert!(stream
            .read_before_checked(&mut [0; 16], Instant::now(), |_| Ok(()))
            .is_err());
        assert!(stream.retire().is_err());
        assert!(cap.snapshot().completion.is_none());
        let first = stream
            .resources
            .as_ref()
            .unwrap()
            .retirement_deadline
            .unwrap();
        let weak = Arc::downgrade(&cap.sink.data);
        drop(stream);
        assert!(c.retained() > 0 && ws.lifetime.unresolved());
        drop(cap);
        assert!(weak.upgrade().is_some());
        loop {
            if c.reap().is_ok() {
                break;
            }
            assert!(Instant::now() < work);
            thread::yield_now();
        }
        assert!(!ws.lifetime.unresolved());
        assert!(first < Instant::now());
        assert!(weak.upgrade().is_none());
        ws.clean().unwrap();
    }
    #[test]
    fn fixture_child() {
        let Some(role) = std::env::var_os("AURUM_DIAG_FIXTURE_CHILD") else {
            return;
        };
        if role == "bulk" {
            std::io::stdout().write_all(&vec![b'x'; 48 * 1024]).unwrap();
            std::io::stderr().write_all(&vec![b'y'; 48 * 1024]).unwrap();
        } else {
            std::io::stdout().write_all(b"\n{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"code\":{\"code\":\"E_FIXTURE\"}}}\n").unwrap();
            std::io::stderr().write_all(b"fixture-stderr\n").unwrap();
        }
        std::io::stdout().flush().unwrap();
        std::io::stderr().flush().unwrap();
        std::process::exit(if role == "nonzero" { 7 } else { 0 });
    }
}
