//! Detached, bounded capture for native children that outlive the launching CLI.
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{diagnostics, Session};

pub const COMMAND: &str = "__aurum-log-relay";
pub const LINES_PER_SECOND: u64 = 200;

// A launching CLI may itself have inherited Node/MCP output pipes. Keeping
// those handles in a detached descendant prevents its client from observing
// EOF after the CLI exits. Explicit child Stdio handles are duplicated by
// Command; the launching process's original standard handles need not inherit.
#[cfg(windows)]
fn confine_standard_handles() -> io::Result<()> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn SetHandleInformation(handle: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    for kind in [-10i32, -11, -12] {
        let handle = unsafe { GetStdHandle(kind as u32) };
        if !handle.is_null()
            && handle as usize != usize::MAX
            && unsafe { SetHandleInformation(handle, 1, 0) } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Internal CLI entry point. No credentials are passed in arguments.
pub fn run(args: &[String]) -> u8 {
    let [directory] = args else {
        return 64;
    };
    let result = (|| -> io::Result<()> {
        let directory = Path::new(directory);
        diagnostics::checked_path(directory)?;
        let session = Session::load(directory)?;
        if session.directory.canonicalize()? != directory.canonicalize()? {
            return Err(io::Error::other(
                "session metadata does not match relay directory",
            ));
        }
        println!("AURUM_LOG_READY");
        io::stdout().flush()?;
        drain(io::stdin().lock(), &session);
        Ok(())
    })();
    if result.is_ok() {
        0
    } else {
        1
    }
}

/// Drain even on disk failures. No-newline streams also stay memory-bounded.
pub fn drain(mut reader: impl Read, session: &Session) {
    let mut chunk = [0u8; 8192];
    let mut line = Vec::with_capacity(diagnostics::LINE_BYTES + 256);
    let mut oversized = false;
    let mut window = Instant::now();
    let mut emitted = 0;
    let mut suppressed = 0u64;
    let mut failed_until = None;
    let mut emit = |line: &[u8], oversized: bool| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let now = Instant::now();
        if now.duration_since(window) >= Duration::from_secs(1) {
            if suppressed > 0 {
                diagnostics::report(session.log(&format!(
                    "diagnostics: {suppressed} child lines suppressed by rate/storage limits"
                )));
                suppressed = 0;
            }
            window = now;
            emitted = 0;
        }
        if emitted >= LINES_PER_SECOND || failed_until.is_some_and(|until| now < until) {
            suppressed = suppressed.saturating_add(1);
            return;
        }
        emitted += 1;
        let text = String::from_utf8_lossy(line);
        let text = if oversized {
            format!("{text} [truncated]")
        } else {
            text.into_owned()
        };
        let result = session.log(&text);
        if result.is_err() {
            failed_until = Some(now + Duration::from_secs(1));
            suppressed = suppressed.saturating_add(1);
        }
        diagnostics::report(result);
    };
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                emit(&line, oversized);
                line.clear();
                oversized = false;
            } else if line.len() < diagnostics::LINE_BYTES + 256 {
                line.push(*byte);
            } else {
                oversized = true;
            }
        }
    }
    if !line.is_empty() || oversized {
        emit(&line, oversized);
    }
    if suppressed > 0 {
        diagnostics::report(session.log(&format!(
            "diagnostics: {suppressed} child lines suppressed by rate/storage limits"
        )));
    }
}

/// Shared child stdout/stderr handles. The helper survives its launching CLI.
pub fn output(session: &Session) -> io::Result<(Stdio, Stdio)> {
    #[cfg(windows)]
    confine_standard_handles()?;
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    command.arg(COMMAND).arg(&session.directory);
    #[cfg(test)]
    command
        .args([
            "--exact",
            "diagnostic_relay::tests::relay_entry",
            "--nocapture",
        ])
        .env("AURUM_TEST_RELAY_DIRECTORY", &session.directory);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut relay = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = relay
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("relay has no handshake pipe"))?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut reader = io::BufReader::new(output);
        for _ in 0..16 {
            let mut line = Vec::new();
            if (&mut reader)
                .take(256)
                .read_until(b'\n', &mut line)
                .unwrap_or(0)
                == 0
            {
                break;
            }
            if line.len() >= 256 {
                break;
            }
            if line.windows(15).any(|window| window == b"AURUM_LOG_READY") {
                let _ = sender.send(());
                return;
            }
        }
    });
    if receiver.recv_timeout(Duration::from_secs(5)).is_err() {
        let _ = relay.kill();
        let _ = relay.wait();
        return Err(io::Error::other("diagnostic relay did not become ready"));
    }
    let input = relay
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("relay has no input pipe"))?;
    #[cfg(windows)]
    let stdout = {
        use std::os::windows::io::AsHandle;
        Stdio::from(input.as_handle().try_clone_to_owned()?)
    };
    #[cfg(windows)]
    let stderr = {
        use std::os::windows::io::AsHandle;
        Stdio::from(input.as_handle().try_clone_to_owned()?)
    };
    #[cfg(not(windows))]
    let stdout = {
        use std::os::fd::AsFd;
        Stdio::from(input.as_fd().try_clone_to_owned()?)
    };
    #[cfg(not(windows))]
    let stderr = {
        use std::os::fd::AsFd;
        Stdio::from(input.as_fd().try_clone_to_owned()?)
    };
    // Long-lived hosts must reap finished helpers. A short-lived CLI may exit
    // first; the relay then remains alive until its child writers close.
    std::thread::spawn(move || {
        let _ = relay.wait();
    });
    Ok((stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relay_entry() {
        if let Some(directory) = std::env::var_os("AURUM_TEST_RELAY_DIRECTORY") {
            std::process::exit(run(&[directory.to_string_lossy().into_owned()]).into());
        }
    }
    #[test]
    fn draining_large_lines_and_storage_failure_does_not_stop_reader() {
        let root =
            std::env::temp_dir().join(format!("aurum-relay-{}", crate::random::session_id()));
        let session = Session::create_in(&root, &root).unwrap();
        let input = format!(
            "{}\nAuthorization: Bearer private-value\nlast",
            "界".repeat(100_000)
        );
        drain(input.as_bytes(), &session);
        let text = std::fs::read_to_string(session.log_path()).unwrap();
        assert!(text.len() < 5000 && text.contains("truncated") && text.contains("last"));
        assert!(!text.contains("private-value"));
        std::fs::create_dir(session.log_path().with_file_name("session.log.2")).unwrap();
        let mut input = io::Cursor::new(b"still\ndrain\n".as_slice());
        drain(&mut input, &session);
        assert_eq!(input.position(), 12);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn noisy_children_are_drained_without_logging_every_frame() {
        let root =
            std::env::temp_dir().join(format!("aurum-relay-rate-{}", crate::random::session_id()));
        let session = Session::create_in(&root, &root).unwrap();
        let input = "frame output\n".repeat(100_000);
        drain(input.as_bytes(), &session);
        let text = std::fs::read_to_string(session.log_path()).unwrap();
        assert!(text.len() < 100_000 && text.contains("suppressed by rate/storage limits"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_relays_share_a_session_without_interleaving_records() {
        let root = std::env::temp_dir().join(format!(
            "aurum-relay-processes-{}",
            crate::random::session_id()
        ));
        let session = Session::create_in(&root, &root).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..3 {
                let session = &session;
                scope.spawn(move || {
                    let (stdout, stderr) = output(session).unwrap();
                    #[cfg(windows)]
                    let mut child = {
                        use std::os::windows::process::CommandExt;
                        let mut command = Command::new("powershell");
                        command.creation_flags(0x08000000).args(["-NoProfile", "-NonInteractive", "-Command", "1..30 | ForEach-Object { Write-Output 'complete-relay-record' }"]);
                        command
                    };
                    #[cfg(not(windows))]
                    let mut child = {
                        let mut command = Command::new("/bin/sh");
                        command.args(["-c", "i=0; while [ $i -lt 30 ]; do echo complete-relay-record; i=$((i+1)); done"]);
                        command
                    };
                    assert!(child.stdout(stdout).stderr(stderr).status().unwrap().success());
                });
            }
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(session.log_path()).unwrap();
            let complete = text
                .lines()
                .filter(|line| *line == "complete-relay-record")
                .count();
            let suppressed: usize = text
                .lines()
                .filter_map(|line| line.strip_prefix("diagnostics: "))
                .filter_map(|line| line.split_whitespace().next()?.parse::<usize>().ok())
                .sum();
            if complete + suppressed == 90 {
                assert!(complete > 0);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "relay records missing or interleaved"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
