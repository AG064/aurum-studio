use super::*;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::mpsc;

fn socket_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    (client, server)
}

fn head(length: usize) -> Request {
    Request {
        method: "POST".into(),
        path: "/api/project".into(),
        query: Vec::new(),
        headers: vec![
            (control::HEADER.into(), control::PROTOCOL.into()),
            ("expect".into(), "100-continue".into()),
            ("content-type".into(), "application/json".into()),
            ("content-length".into(), length.to_string()),
        ],
        body: Vec::new(),
    }
}

#[test]
fn control_headers_refuse_duplicates_unsupported_values_and_wrong_routes() {
    let valid = head(50);
    assert!(requested(&valid).unwrap());
    for key in [control::HEADER, "expect", "content-type"] {
        let mut duplicate = valid.clone();
        let field = duplicate
            .headers
            .iter()
            .find(|(name, _)| name == key)
            .unwrap()
            .clone();
        duplicate.headers.push(field);
        assert!(requested(&duplicate).is_err());
    }
    for (key, value) in [
        (control::HEADER, "other"),
        ("expect", "other"),
        ("content-type", "text/plain"),
    ] {
        let mut invalid = valid.clone();
        invalid
            .headers
            .iter_mut()
            .find(|(name, _)| name == key)
            .unwrap()
            .1 = value.into();
        assert!(requested(&invalid).is_err());
    }
    let mut wrong_route = valid.clone();
    wrong_route.path = "/api/stop".into();
    assert!(requested(&wrong_route).is_err());
    let mut ordinary = valid;
    ordinary
        .headers
        .retain(|(name, _)| name != control::HEADER && name != "expect");
    assert!(!requested(&ordinary).unwrap());
    ordinary
        .headers
        .push(("expect".into(), "100-continue".into()));
    assert!(!requested(&ordinary).unwrap());
}

#[test]
fn delayed_body_uses_the_ready_anchor_and_refuses_expired_work() {
    for expired in [false, true] {
        let (mut client, server) = socket_pair();
        let remaining = Duration::from_millis(if expired { 10 } else { 2000 });
        let body = control::encode(br#"{"op":"native_status"}"#, remaining).unwrap();
        let request = head(body.len());
        let (anchor_sent, anchor_received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let before_ready = Instant::now();
            anchor_sent.send(before_ready).unwrap();
            let mut reader = BufReader::new(&server);
            let mut writer = &server;
            let mut request = request;
            receive(&mut reader, &mut writer, &mut request)
                .map(|context| (context.deadline(), request.body))
        });
        let before_ready = anchor_received
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let mut ready = vec![0; control::READY.len()];
        client.read_exact(&mut ready).unwrap();
        assert_eq!(ready, control::READY);
        let sampled = Instant::now();
        client
            .write_all(&body[..control::TIMING_PREFIX_BYTES])
            .unwrap();
        std::thread::sleep(Duration::from_millis(40));
        let send = client.write_all(&body[control::TIMING_PREFIX_BYTES..]);
        let result = worker.join().unwrap();
        if expired {
            assert!(result.unwrap_err().contains("deadline"));
        } else {
            send.unwrap();
            let (deadline, original) = result.unwrap();
            assert!(deadline >= before_ready + remaining);
            assert!(deadline <= sampled + remaining);
            assert_eq!(original, br#"{"op":"native_status"}"#);
        }
    }
}

#[test]
fn peer_eof_cancels_the_scoped_operation_without_touching_another_context() {
    let (client, server) = socket_pair();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
    let unrelated = OperationContext::new(context.deadline());
    let working = context.clone();
    let (entered, started) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        with_peer(&server, &working, || {
            assert!(OperationContext::current().is_some());
            entered.send(()).unwrap();
            loop {
                if let Err(error) = working.check() {
                    return error.kind();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(client);
    assert_eq!(worker.join().unwrap().unwrap(), io::ErrorKind::Interrupted);
    assert!(context.cancellation_requested());
    assert!(!unrelated.cancellation_requested());
    assert!(!context.termination_unconfirmed());
}

#[test]
fn watcher_stops_and_joins_with_an_open_peer_and_response_writes_survive() {
    let (mut client, mut server) = socket_pair();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
    let start = Instant::now();
    with_peer(&server, &context, || {
        assert!(OperationContext::current().is_some())
    })
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(OperationContext::current().is_none());
    assert!(!context.cancellation_requested());
    server.write_all(b"ok").unwrap();
    let mut response = [0; 2];
    client.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"ok");
}

#[test]
fn peer_cancellation_during_commit_keeps_the_barrier_atomic() {
    let (client, server) = socket_pair();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
    let working = context.clone();
    let (entered, started) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        with_peer(&server, &working, || {
            let guard = working.commit_barrier().unwrap();
            entered.send(()).unwrap();
            let end = Instant::now() + Duration::from_secs(2);
            while !working.cancellation_requested() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(working.cancellation_requested());
            working.check().unwrap();
            guard.finish();
            working.check().unwrap();
        })
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(client);
    worker.join().unwrap().unwrap();
    assert!(context.committed());
    assert!(context.commit_status_verified());
    assert!(!context.recovery_required());
}

#[test]
fn expired_receiver_context_refuses_dispatch_and_scope_unwind_joins_watcher() {
    let (_client, server) = socket_pair();
    let expired = OperationContext::new(Instant::now() - Duration::from_millis(1));
    let entered = AtomicBool::new(false);
    assert!(with_peer(&server, &expired, || entered.store(true, Ordering::SeqCst)).is_err());
    assert!(!entered.load(Ordering::SeqCst));
    let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_peer(&server, &context, || panic!("fixture scope unwind")).unwrap();
    }));
    assert!(result.is_err());
    assert!(OperationContext::current().is_none());
    assert!(!context.cancellation_requested());
}

#[test]
fn accepted_background_job_retains_its_own_context_after_rpc_eof() {
    use aurum_studio_core::jobs::{JobManager, JobOwner};
    let (client, server) = socket_pair();
    let manager = Arc::new(JobManager::new(None).unwrap());
    let owner = JobOwner::new().unwrap();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
    let working = context.clone();
    let jobs = Arc::clone(&manager);
    let submitting_owner = owner.clone();
    let (ticket_sent, ticket_received) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        with_peer(&server, &working, || {
            let ticket = jobs.submit(&submitting_owner, serde_json::json!({"op":"fixture"}),
            false, Duration::from_secs(5), |_, _| Ok(()), move |_| {
                let owned = OperationContext::current().unwrap();
                released.recv_timeout(Duration::from_secs(3)).map_err(|_| "fixture release missing")?;
                owned.check().map_err(|error| error.to_string())?;
                Ok(serde_json::json!({"ok":true,"cancellation_requested":owned.cancellation_requested()}))
            }).unwrap();
            ticket_sent.send(ticket.id).unwrap();
            while working.check().is_ok() {
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    });
    let id = ticket_received
        .recv_timeout(Duration::from_secs(3))
        .unwrap();
    drop(client);
    worker.join().unwrap().unwrap();
    assert!(context.cancellation_requested());
    release.send(()).unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let status = manager.status(&id, &owner, None).unwrap();
        if status["job"]["state"] == "succeeded" {
            assert_eq!(status["job"]["cancellation_requested"], false);
            break;
        }
        assert!(
            Instant::now() < end,
            "background job did not succeed: {status}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    manager.shutdown();
}

#[cfg(windows)]
fn peer_command_ticks() -> u64 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetTickCount64"]
        fn ticks() -> u64;
    }
    unsafe { ticks() }
}

#[cfg(windows)]
fn peer_command_stop_child(
    child: &mut std::process::Child,
    remaining: impl Fn() -> Duration,
) -> io::Result<bool> {
    if child.try_wait()?.is_some() {
        return Ok(true);
    }
    let killed = child.kill();
    if child.try_wait()?.is_some() {
        return Ok(true);
    }
    killed?;
    loop {
        let left = remaining();
        if left.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(5).min(left));
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
    }
}

#[cfg(windows)]
#[test]
fn peer_owned_command_fixture_child() {
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command as SystemCommand, Stdio};
    if std::env::var("AURUM_PEER_COMMAND_ROLE").ok().as_deref() != Some("owned-root") {
        return;
    }
    let root = std::path::PathBuf::from(
        std::env::var_os("AURUM_PEER_COMMAND_ROOT").expect("owned fixture root is required"),
    );
    let end: u64 = std::env::var("AURUM_PEER_COMMAND_END_TICKS")
        .expect("original fixture deadline is required")
        .parse()
        .expect("original fixture deadline must be numeric");
    let left = end.saturating_sub(peer_command_ticks());
    assert!(root.is_absolute() && left > 0 && left <= 8_000);
    struct Descendant {
        child: Option<Child>,
        end: u64,
    }
    impl Drop for Descendant {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let end = self.end;
                let retired = peer_command_stop_child(&mut child, || {
                    Duration::from_millis(end.saturating_sub(peer_command_ticks()))
                });
                if !matches!(retired, Ok(true)) {
                    eprintln!("owned fixture descendant retirement unknown: {retired:?}");
                    std::mem::forget(child);
                }
            }
        }
    }
    let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let child = SystemCommand::new(system.join("ping.exe"))
        .args(["-n", "100", "127.0.0.1"])
        .current_dir(&root)
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut descendant = Descendant {
        child: Some(child),
        end,
    };
    assert!(descendant
        .child
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none());
    std::fs::write(root.join("ready.txt"), b"native child and descendant ready").unwrap();
    loop {
        assert!(
            descendant
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none(),
            "owned fixture descendant exited before cancellation"
        );
        let left = end.saturating_sub(peer_command_ticks());
        assert!(
            left > 0,
            "owned fixture reached the original context deadline"
        );
        std::thread::sleep(Duration::from_millis(5).min(Duration::from_millis(left)));
    }
}

#[cfg(windows)]
#[test]
fn peer_eof_retires_an_owned_transient_command_and_preserves_an_unrelated_child() {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Command as SystemCommand, Stdio};
    struct Cleanup {
        root: PathBuf,
        unrelated: Option<Child>,
        end: Option<Instant>,
        owned_retired: bool,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let end = self.end.unwrap_or_else(Instant::now);
            let unrelated_retired = self.unrelated.take().is_none_or(|mut child| {
                let retired = peer_command_stop_child(&mut child, || {
                    end.saturating_duration_since(Instant::now())
                });
                if matches!(retired, Ok(true)) {
                    true
                } else {
                    eprintln!("unrelated fixture child retirement unknown: {retired:?}");
                    std::mem::forget(child);
                    false
                }
            });
            if self.owned_retired && unrelated_retired && Instant::now() < end {
                if let Err(error) = std::fs::remove_dir_all(&self.root) {
                    eprintln!("retired fixture input cleanup failed: {error}");
                }
            } else {
                eprintln!(
                    "peer fixture inputs retained: owned_retired={} unrelated_retired={} root={}",
                    self.owned_retired,
                    unrelated_retired,
                    self.root.display()
                );
            }
        }
    }
    let root = std::env::temp_dir().join(format!(
        "aurum-peer-owned-{}-{}",
        std::process::id(),
        aurum_studio_core::random::session_id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    let unrelated = SystemCommand::new(system.join("ping.exe"))
        .args(["-n", "100", "127.0.0.1"])
        .current_dir(&root)
        .creation_flags(0x08000000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut cleanup = Cleanup {
        root: root.clone(),
        unrelated: Some(unrelated),
        end: None,
        owned_retired: true,
    };
    let (client, server) = socket_pair();
    let context = OperationContext::new(Instant::now() + Duration::from_secs(8));
    let context_end = context.deadline();
    cleanup.end = Some(context_end);
    let working = context.clone();
    // The earlier tick sample makes this a conservative encoding of the same
    // context end, rather than granting the child a fresh duration at startup.
    let ticks = peer_command_ticks();
    let remaining = context_end.saturating_duration_since(Instant::now());
    let child_end = ticks
        .checked_add(u64::try_from(remaining.as_millis()).unwrap())
        .unwrap();
    let command = aurum_studio_core::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_request::tests::peer_owned_command_fixture_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .directory(&root)
        .env("AURUM_PEER_COMMAND_ROLE", "owned-root")
        .env("AURUM_PEER_COMMAND_ROOT", root.to_string_lossy())
        .env("AURUM_PEER_COMMAND_END_TICKS", child_end.to_string());
    let (finished, result) = mpsc::sync_channel(1);
    cleanup.owned_retired = false;
    let worker = std::thread::spawn(move || {
        let original = with_peer(&server, &working, || command.run(Duration::from_secs(7)));
        let _ = finished.send(original);
    });
    let end = (Instant::now() + Duration::from_secs(3)).min(context_end);
    let mut ready = false;
    let mut original = None;
    let mut original_observed_before_end = false;
    let mut finished_observed_before_end = false;
    let mut disconnected = false;
    while Instant::now() < end {
        let observed = root.join("ready.txt").is_file();
        if Instant::now() >= end {
            break;
        }
        if observed {
            ready = true;
            break;
        }
        match result.try_recv() {
            Ok(completed) => {
                original = Some(completed);
                original_observed_before_end = Instant::now() < context_end;
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        std::thread::sleep(
            Duration::from_millis(5).min(end.saturating_duration_since(Instant::now())),
        );
    }
    drop(client);
    if !ready {
        // Failure cleanup may cancel explicitly; the passing path above still
        // depends solely on the real peer EOF to request cancellation.
        let _ = context.request_cancel();
    }
    while Instant::now() < context_end {
        if original.is_none() && !disconnected {
            match result.try_recv() {
                Ok(completed) => {
                    original = Some(completed);
                    original_observed_before_end = Instant::now() < context_end;
                }
                Err(mpsc::TryRecvError::Disconnected) => disconnected = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if worker.is_finished() {
            finished_observed_before_end = Instant::now() < context_end;
            break;
        }
        std::thread::sleep(
            Duration::from_millis(5).min(context_end.saturating_duration_since(Instant::now())),
        );
    }
    // A final nonblocking drain preserves diagnostics, but late observations
    // cannot qualify the result or the worker's completion for test credit.
    if original.is_none() && !disconnected {
        match result.try_recv() {
            Ok(completed) => {
                original = Some(completed);
                original_observed_before_end = Instant::now() < context_end;
            }
            Err(mpsc::TryRecvError::Disconnected) => disconnected = true,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
    let bounded = |text: &str| text.chars().take(512).collect::<String>();
    let diagnostic = match original.as_ref() {
        Some(Err(error)) => format!(
            "peer scope error kind={:?} os={:?}: {}",
            error.kind(),
            error.raw_os_error(),
            bounded(&error.to_string())
        ),
        Some(Ok(Err(error))) => format!(
            "command error kind={:?} os={:?}: {}",
            error.kind(),
            error.raw_os_error(),
            bounded(&error.to_string())
        ),
        Some(Ok(Ok(outcome))) => format!(
            "command outcome code={:?} timed_out={} stdout={:?} stderr={:?}",
            outcome.code,
            outcome.timed_out,
            bounded(&outcome.stdout),
            bounded(&outcome.stderr)
        ),
        None => format!("original result unavailable; disconnected={disconnected}"),
    };
    let reported_retired = match original.as_ref() {
        Some(Ok(Ok(_))) => true,
        Some(Ok(Err(error))) => {
            error.kind() == io::ErrorKind::Interrupted && context.cancellation_requested()
        }
        _ => false,
    };
    cleanup.owned_retired = worker.is_finished()
        && reported_retired
        && !context.termination_unconfirmed()
        && !context.recovery_required();
    if !cleanup.owned_retired {
        // Preserve the original handle, receiver/result and scope until host
        // exit when this attempt cannot prove that the original work retired.
        std::mem::forget((worker, result, original, context.clone()));
        panic!("owned command retirement unknown; ready={ready}; {diagnostic}");
    }
    drop(worker); // Finished handle only; no unbounded join is introduced.
    assert!(
        original_observed_before_end
            && finished_observed_before_end
            && Instant::now() < context_end,
        "owned command observations exceeded the original context end; {diagnostic}"
    );
    assert!(ready, "owned child did not enter the fixture; {diagnostic}");
    let error = match original {
        Some(Ok(Err(error))) => error,
        _ => panic!("owned command did not cancel after peer EOF; {diagnostic}"),
    };
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(context.cancellation_requested());
    assert!(!context.termination_unconfirmed());
    assert!(!context.recovery_required());
    assert!(cleanup
        .unrelated
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none());
    drop(cleanup);
    assert!(
        Instant::now() < context_end,
        "final cancellation assertions or cleanup exceeded the original context end; {diagnostic}"
    );
}
