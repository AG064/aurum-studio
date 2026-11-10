use super::*;
use aurum_studio_core::native_request_control as control;
use std::io::Read;
use std::time::Instant;

struct Fixture {
    root: PathBuf,
    server: Arc<Server>,
    serving: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "aurum-request-control-{}-{}",
            std::process::id(),
            aurum_studio_core::random::session_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("aurum.toml"),
            "schema_version = 1\nname = \"request-control\"\n",
        )
        .unwrap();
        std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
        let project = Project::open(&root).unwrap();
        let server = Arc::new(Server::bind(ServerConfig::new(project)).unwrap());
        let running = Arc::clone(&server);
        let serving = Some(std::thread::spawn(move || running.serve()));
        Self {
            root,
            server,
            serving,
        }
    }
    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, self.server.port())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }
    fn controlled_head(&self, length: usize, token: &str, extra: &str) -> String {
        format!("POST /api/project HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-Aurum-Token: {token}\r\nX-Aurum-Request-Control: {}\r\nExpect: 100-continue\r\nContent-Type: application/json\r\nContent-Length: {length}\r\n{extra}Connection: close\r\n\r\n",
            self.server.port(), control::PROTOCOL)
    }
    fn ready(&self, body_len: usize) -> TcpStream {
        let mut stream = self.connect();
        stream
            .write_all(
                self.controlled_head(body_len, self.server.token(), "")
                    .as_bytes(),
            )
            .unwrap();
        let mut ready = vec![0; control::READY.len()];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(ready, control::READY);
        stream
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.stop();
        if let Some(serving) = self.serving.take() {
            serving.join().unwrap();
        }
        // All writes and any later qualification execute only in this fresh temp root.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn response(stream: &mut TcpStream) -> String {
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn ordinary_project_post_and_native_host_capability_remain_compatible() {
    let fixture = Fixture::new();
    let mut probe = fixture.connect();
    write!(
        probe,
        "GET /api/native_host HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: {}\r\n\r\n",
        fixture.server.token()
    )
    .unwrap();
    let probe_reply = response(&mut probe);
    let body = probe_reply.split_once("\r\n\r\n").unwrap().1;
    let value: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(value["protocol"], "aurum-native-host/1");
    assert_eq!(value["request_control"], control::PROTOCOL);
    for (expectation, path) in [
        ("", "ordinary.txt"),
        ("Expect: 100-continue\r\n", "ordinary-expect.txt"),
    ] {
        let body =
            serde_json::to_vec(&serde_json::json!({"op":"write","path":path,"text":"ordinary"}))
                .unwrap();
        let mut stream = fixture.connect();
        write!(stream, "POST /api/project HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: {}\r\nContent-Length: {}\r\n{expectation}\r\n",
            fixture.server.token(), body.len()).unwrap();
        stream.write_all(&body).unwrap();
        let reply = response(&mut stream);
        assert!(reply.starts_with("HTTP/1.1 200 "));
        assert!(!reply.contains("100 Continue"));
        assert_eq!(
            std::fs::read_to_string(fixture.root.join(path)).unwrap(),
            "ordinary"
        );
    }
}

#[test]
fn controlled_ready_requires_auth_and_duplicates_are_refused_before_ack() {
    let fixture = Fixture::new();
    let length = control::body_len(2).unwrap();
    for (token, extra, status) in [
        ("wrong", String::new(), "HTTP/1.1 401 "),
        (
            fixture.server.token(),
            format!("X-Aurum-Request-Control: {}\r\n", control::PROTOCOL),
            "HTTP/1.1 400 ",
        ),
        (
            fixture.server.token(),
            "Expect: 100-continue\r\n".into(),
            "HTTP/1.1 400 ",
        ),
        (
            fixture.server.token(),
            "Content-Length: 2\r\n".into(),
            "HTTP/1.1 400 ",
        ),
    ] {
        let mut stream = fixture.connect();
        stream
            .write_all(fixture.controlled_head(length, token, &extra).as_bytes())
            .unwrap();
        let reply = response(&mut stream);
        assert!(reply.starts_with(status), "unexpected status");
        assert!(!reply.contains("100 Continue"));
        assert!(!fixture.root.join("controlled.txt").exists());
    }
}

#[test]
fn controlled_body_expiry_cannot_publish_and_valid_control_can_publish() {
    let fixture = Fixture::new();
    let request = br#"{"op":"write","path":"controlled.txt","text":"accepted"}"#;
    let expired = control::encode(request, Duration::from_millis(20)).unwrap();
    let mut stream = fixture.ready(expired.len());
    stream
        .write_all(&expired[..control::TIMING_PREFIX_BYTES])
        .unwrap();
    let started = Instant::now();
    let reply = response(&mut stream);
    assert!(reply.starts_with("HTTP/1.1 400 "));
    assert!(reply.contains("deadline"));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!fixture.root.join("controlled.txt").exists());
    let valid = control::encode(request, Duration::from_secs(5)).unwrap();
    let mut stream = fixture.ready(valid.len());
    stream.write_all(&valid).unwrap();
    assert!(response(&mut stream).starts_with("HTTP/1.1 200 "));
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("controlled.txt")).unwrap(),
        "accepted"
    );
}

#[test]
fn controlled_envelope_cannot_override_authority_or_duplicate_timing() {
    let fixture = Fixture::new();
    let request = br#"{"op":"write","path":"controlled.txt","text":"refused"}"#;
    for extra in [
        b",\"read_only\":false".as_slice(),
        b",\"remaining_ms\":\"00000000000000005000\"".as_slice(),
    ] {
        let mut body = control::encode(request, Duration::from_secs(5)).unwrap();
        assert_eq!(body.pop(), Some(b'}'));
        body.extend_from_slice(extra);
        body.push(b'}');
        let mut stream = fixture.ready(body.len());
        stream.write_all(&body).unwrap();
        assert!(response(&mut stream).starts_with("HTTP/1.1 400 "));
        assert!(!fixture.root.join("controlled.txt").exists());
    }
}

#[cfg(windows)]
#[test]
fn a_child_spawned_during_a_request_cannot_hold_or_reset_its_connection() {
    let fixture = Fixture::new();
    let body =
        serde_json::to_vec(&serde_json::json!({"op":"write","path":"spawned.txt","text":"ok"}))
            .unwrap();
    let mut stream = fixture.connect();
    write!(
        stream,
        "POST /api/project HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: {}\r\nContent-Length: {}\r\n\r\n",
        fixture.server.token(),
        body.len()
    )
    .unwrap();
    // Wait until a handler owns this connection and is waiting for the body.
    let accepted = Instant::now() + Duration::from_secs(5);
    while fixture.server.shell.connections_count() == 0 {
        assert!(Instant::now() < accepted, "the server never accepted");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    // std spawns with handle inheritance on, as every Studio child process does.
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let mut child = std::process::Command::new(system.join("ping.exe"))
        .args(["-n", "8", "127.0.0.1"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    stream.write_all(&body).unwrap();
    let mut reply = String::new();
    let read = stream.read_to_string(&mut reply);
    let child_alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    read.unwrap();
    assert!(child_alive, "the reply closed only after the child exited");
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
}

#[cfg(windows)]
#[test]
fn a_private_socket_clone_is_not_inheritable() {
    use std::os::windows::io::AsRawSocket;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
    }
    let fixture = Fixture::new();
    let stream = fixture.connect();
    let clone = crate::private_clone(&stream).unwrap();
    let mut flags = u32::MAX;
    // SAFETY: `clone` owns this live socket handle for the whole call.
    let ok = unsafe { GetHandleInformation(clone.as_raw_socket() as *mut _, &mut flags) };
    assert_ne!(ok, 0);
    assert_eq!(flags & 1, 0);
}

#[test]
fn a_refused_connection_still_receives_its_503() {
    let fixture = Fixture::new();
    let held: Vec<_> = (0..MAX_CONNECTIONS).map(|_| fixture.connect()).collect();
    let full = Instant::now() + Duration::from_secs(10);
    while fixture.server.shell.connections_count() < MAX_CONNECTIONS {
        assert!(
            Instant::now() < full,
            "the server never reached its ceiling"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..8 {
        let mut refused = fixture.connect();
        // Sent before the refusal, as a browser does, so unread bytes are waiting at close.
        write!(
            refused,
            "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: {}\r\n\r\n",
            fixture.server.token()
        )
        .unwrap();
        let reply = response(&mut refused);
        assert!(reply.starts_with("HTTP/1.1 503 "), "{reply}");
    }
    drop(held);
}

fn update_request(fixture: &Fixture, method: &str, body: &str) -> String {
    let mut stream = fixture.connect();
    write!(
        stream,
        "{method} /api/update HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: {}\r\nContent-Length: {}\r\n\r\n{body}",
        fixture.server.token(),
        body.len()
    )
    .unwrap();
    response(&mut stream)
}

#[test]
fn the_update_api_reports_its_state_and_is_not_blocked_by_its_own_request() {
    let fixture = Fixture::new();
    let state = update_request(&fixture, "GET", "");
    assert!(state.starts_with("HTTP/1.1 200 "), "{state}");
    let body: serde_json::Value =
        serde_json::from_str(state.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["phase"], "idle");
    assert_eq!(body["current"], env!("CARGO_PKG_VERSION"));
    // A test binary is not an installed copy, so it is told so rather than updated.
    assert_eq!(body["installed_copy"], false);

    // The request that asks must not count itself as the running operation it checks for.
    let install = update_request(&fixture, "POST", r#"{"action":"install"}"#);
    assert!(install.starts_with("HTTP/1.1 409 "), "{install}");
    assert!(install.contains("build folder"), "{install}");
    assert!(!install.contains("still running"), "{install}");

    let restart = update_request(&fixture, "POST", r#"{"action":"restart"}"#);
    assert!(
        restart.starts_with("HTTP/1.1 409 ") && !restart.contains("still running"),
        "{restart}"
    );

    let unknown = update_request(&fixture, "POST", r#"{"action":"format-disk"}"#);
    assert!(unknown.starts_with("HTTP/1.1 400 "), "{unknown}");
    let malformed = update_request(&fixture, "POST", "not json");
    assert!(malformed.starts_with("HTTP/1.1 400 "), "{malformed}");
}

#[test]
fn the_update_api_requires_the_session_token() {
    let fixture = Fixture::new();
    let mut stream = fixture.connect();
    write!(
        stream,
        "GET /api/update HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
    )
    .unwrap();
    assert!(response(&mut stream).starts_with("HTTP/1.1 401 "));
    let mut wrong = fixture.connect();
    write!(wrong, "POST /api/update HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: nope\r\nContent-Length: 18\r\n\r\n{{\"action\":\"check\"}}").unwrap();
    assert!(response(&mut wrong).starts_with("HTTP/1.1 401 "));
}

#[test]
fn closing_waits_for_in_flight_work_instead_of_refusing_at_once() {
    let fixture = Fixture::new();
    // The counter every project operation holds while it runs, as when the page has just loaded.
    fixture
        .server
        .shell
        .operations
        .fetch_add(1, Ordering::SeqCst);
    let operations = Arc::clone(&fixture.server.shell.operations);
    let finishing = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        operations.fetch_sub(1, Ordering::SeqCst);
    });
    let started = Instant::now();
    let mut stream = fixture.connect();
    write!(
        stream,
        "POST /api/stop HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Aurum-Token: {}\r\nContent-Length: 0\r\n\r\n",
        fixture.server.token()
    )
    .unwrap();
    let reply = response(&mut stream);
    finishing.join().unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(
        started.elapsed() >= Duration::from_millis(600),
        "it did not wait for the work to finish"
    );
    assert!(!reply.contains("still running"), "{reply}");
}
