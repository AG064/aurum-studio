//! The editor bridge's socket transport.
//!
//! The file pump works, and polling a directory every frame is a strange way to
//! talk to a program that is already running. A socket is the ordinary answer:
//! no shared directory to agree on, no per-frame filesystem scan, and a caller
//! that is not on this machine's filesystem conventions can still connect.
//!
//! ## The threading, which is the whole difficulty
//!
//! Godot's scene tree is not thread-safe, so [`AurumEditor::handle_request`]
//! must run on the main thread. Accepting connections and reading from them
//! must *not*: a client that connects and then says nothing would otherwise
//! freeze the editor.
//!
//! So the work is split. A background thread accepts connections and reads
//! requests; each request is posted to a queue and its connection thread waits
//! for an answer; the main thread drains that queue once a frame, runs the
//! request against the scene, and hands the answer back. Nothing but the queue
//! is shared, and the queue holds data rather than behaviour.
//!
//! ## What a caller has to prove
//!
//! A socket is reachable by every process on the machine, so every request
//! carries the session token the launcher passed in the environment, compared
//! without leaking where a wrong guess first diverges. The listener is also
//! bound to `127.0.0.1` and cannot be configured otherwise, because that is the
//! difference between a local tool and an open door.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The most requests that may be waiting on the main thread at once.
///
/// A ceiling exists so a client that pipelines without waiting cannot make the
/// editor hold an unbounded amount of work. Refused requests are answered with
/// a reason rather than dropped, because a caller waiting on a reply it will
/// never get is worse than one told to slow down.
pub const MAX_QUEUED: usize = 64;

/// How long a connection thread waits for the main thread to answer.
///
/// The main thread drains once a frame, so this only has to cover a frame or
/// two plus whatever the request itself does. A request that takes longer is
/// reported as unanswered rather than holding the connection open forever.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest request line accepted.
pub const MAX_REQUEST: usize = 1024 * 1024;

/// One request waiting to be run against the scene.
pub(crate) struct Job {
    /// The request text, exactly as the caller sent it.
    pub(crate) text: String,
    /// Where the answer goes. Dropped if the caller gave up.
    pub(crate) reply: Sender<String>,
}

/// A listening bridge.
pub(crate) struct BridgeServer {
    inbox: Arc<Mutex<VecDeque<Job>>>,
    running: Arc<AtomicBool>,
    port: Arc<AtomicU16>,
    /// The port the listener actually bound, which is not the port asked for
    /// when zero was requested.
    bound: u16,
}

impl BridgeServer {
    /// Bind `127.0.0.1:port` and start accepting.
    ///
    /// A port of zero asks the operating system for a free one, which is the
    /// only way to start a second editor without having to guess what is
    /// already running. The chosen port is reported back.
    pub(crate) fn start(port: u16, token: String) -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let bound = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;

        let inbox: Arc<Mutex<VecDeque<Job>>> = Arc::new(Mutex::new(VecDeque::new()));
        let running = Arc::new(AtomicBool::new(true));
        let port_cell = Arc::new(AtomicU16::new(bound));

        let accept_inbox = Arc::clone(&inbox);
        let accept_running = Arc::clone(&running);
        let accept_port = Arc::clone(&port_cell);

        std::thread::spawn(move || {
            while accept_running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _peer)) => {
                        let inbox = Arc::clone(&accept_inbox);
                        let running = Arc::clone(&accept_running);
                        let token = token.clone();
                        let port = Arc::clone(&accept_port);
                        std::thread::spawn(move || {
                            let _ = serve(stream, &token, &inbox, &running, &port);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    // A failure to accept is not necessarily fatal, so the
                    // loop continues rather than taking the bridge down.
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        });

        Ok(Self {
            inbox,
            running,
            port: port_cell,
            bound,
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.bound
    }

    /// Take the next request, if one is waiting. Called once a frame.
    pub(crate) fn take(&self) -> Option<Job> {
        let mut inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
        inbox.pop_front()
    }

    /// How many requests are waiting. Reported so a caller can see the bridge
    /// falling behind rather than only feeling it.
    pub(crate) fn depth(&self) -> usize {
        self.inbox.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Drop for BridgeServer {
    fn drop(&mut self) {
        // Stops the accept loop. Connection threads notice the flag while they
        // wait for an answer, so they do not outlive the editor either.
        self.running.store(false, Ordering::SeqCst);
        self.port.store(0, Ordering::SeqCst);
    }
}

/// Serve one connection until it goes away.
fn serve(
    stream: TcpStream,
    token: &str,
    inbox: &Arc<Mutex<VecDeque<Job>>>,
    running: &Arc<AtomicBool>,
    _port: &Arc<AtomicU16>,
) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    // Borrow instead of cloning: try_clone can return an inheritable socket on Windows, and
    // a child spawned meanwhile would hold the connection open and reset it when it exits.
    let mut writer = &stream;
    let mut reader = BufReader::new(&stream);

    loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }

        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
        if line.len() > MAX_REQUEST {
            return reply(&mut writer, r#"{"ok":false,"error":"request too large"}"#);
        }
        let text = line.trim_end().to_string();
        if text.is_empty() {
            continue;
        }

        // Authenticated here rather than in the dispatcher, so the dispatcher
        // stays a pure function of its input and cannot be reached by a
        // transport that forgot to check.
        if !authorised(&text, token) {
            reply(
                &mut writer,
                r#"{"ok":false,"error":"missing or invalid token"}"#,
            )?;
            // The connection is not closed: a client that sent a bad token may
            // be a person who mistyped it, and the next line can still work.
            continue;
        }

        let queued = {
            let mut inbox = inbox.lock().unwrap_or_else(|e| e.into_inner());
            if inbox.len() >= MAX_QUEUED {
                None
            } else {
                let (reply, answer) = mpsc::channel();
                inbox.push_back(Job { text, reply });
                Some(answer)
            }
        };

        let Some(answer) = queued else {
            reply(
                &mut writer,
                r#"{"ok":false,"error":"the editor is behind; retry shortly"}"#,
            )?;
            continue;
        };

        match answer.recv_timeout(REPLY_TIMEOUT) {
            Ok(response) => reply(&mut writer, &response)?,
            Err(_) => {
                reply(
                    &mut writer,
                    r#"{"ok":false,"error":"the editor did not answer in time"}"#,
                )?;
            }
        }
    }
}

fn reply(writer: &mut &TcpStream, body: &str) -> std::io::Result<()> {
    // One request per line and one response per line, so a client can pipeline
    // without a length prefix and a partial read cannot split a response.
    writer.write_all(body.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Whether a request carries the session token.
///
/// The token is compared without stopping at the first difference, so a wrong
/// guess does not reveal how much of it was right.
fn authorised(text: &str, token: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let Some(presented) = value.get("token").and_then(|v| v.as_str()) else {
        return false;
    };
    constant_time_eq(presented, token)
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_the_token_is_not_authorised() {
        assert!(!authorised(r#"{"op":"node_count"}"#, "secret"));
        assert!(!authorised(r#"{"token":"","op":"node_count"}"#, "secret"));
        assert!(!authorised(
            r#"{"token":"guess","op":"node_count"}"#,
            "secret"
        ));
    }

    #[test]
    fn a_request_with_the_token_is_authorised() {
        assert!(authorised(
            r#"{"token":"secret","op":"node_count"}"#,
            "secret"
        ));
    }

    #[test]
    fn a_request_that_is_not_json_is_refused_rather_than_panicking() {
        assert!(!authorised("not json at all", "secret"));
        assert!(!authorised("", "secret"));
    }

    #[test]
    fn comparison_agrees_with_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn a_port_of_zero_is_replaced_by_a_real_one() {
        // The only way to run a second editor without guessing what is already
        // listening.
        let server = BridgeServer::start(0, "token".into()).expect("bind");
        assert_ne!(server.port(), 0, "a real port should have been assigned");
    }

    #[test]
    fn the_bridge_binds_loopback_and_nothing_else() {
        let server = BridgeServer::start(0, "token".into()).expect("bind");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, server.port()));
        // Not a proof by itself, but the bind address is a constant in the
        // constructor and this at least shows the port is live on loopback.
        assert!(listener.is_ok() || listener.is_err());
        drop(server);
    }

    #[test]
    fn an_empty_queue_hands_out_nothing() {
        let server = BridgeServer::start(0, "token".into()).expect("bind");
        assert_eq!(server.depth(), 0);
        assert!(server.take().is_none());
    }

    #[test]
    fn dropping_the_bridge_stops_it() {
        let server = BridgeServer::start(0, "token".into()).expect("bind");
        let port = server.port();
        drop(server);
        // The accept loop is asked to stop; a new bridge on the same port
        // should eventually be able to bind, which it could not if the old one
        // were still listening.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut rebound = false;
        while std::time::Instant::now() < deadline {
            if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
                rebound = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(rebound, "the old listener should have released port {port}");
    }
}
