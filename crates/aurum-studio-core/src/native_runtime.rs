//! Fair, bounded coordination for short-lived native Godot management workers.
//! Status/file APIs and version probes do not acquire this gate.
use std::collections::VecDeque;
use std::io;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    active: bool,
    next: u64,
    waiting: VecDeque<u64>,
}

#[derive(Default)]
struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

struct Lease<'a>(&'a Gate);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active = false;
        self.0.changed.notify_all();
    }
}

impl Gate {
    fn acquire(&self, budget: Duration) -> io::Result<Lease<'_>> {
        let deadline = Instant::now() + budget;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        state.waiting.push_back(ticket);
        self.changed.notify_all();
        loop {
            if !state.active && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active = true;
                return Ok(Lease(self));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.waiting.retain(|value| *value != ticket);
                self.changed.notify_all();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Native management worker queue exceeded its operation budget",
                ));
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

fn management_phase(command: &crate::Command) -> bool {
    command
        .arguments()
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--editor" || arg == "--headless" || arg.starts_with("--export-"))
}

/// Serialize bounded native management phases, not entire builds or games.
/// Headless setup/inspection workers also initialize Godot and must not race
/// editor import startup. Status and file requests do not spawn these workers.
/// Queue waiting is
/// included in the caller's existing budget; no test or process retry is added.
pub fn run(command: &crate::Command, budget: Duration) -> io::Result<crate::Outcome> {
    if !management_phase(command) {
        return command.run(budget);
    }
    static GATE: OnceLock<Gate> = OnceLock::new();
    let started = Instant::now();
    let _lease = GATE.get_or_init(Gate::default).acquire(budget)?;
    let remaining = budget.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Native management worker budget expired while queued",
        ));
    }
    let diagnostic = std::env::var("AURUM_NATIVE_DIAGNOSTICS").as_deref() == Ok("1");
    if diagnostic {
        command
            .clone()
            .arg_before_separator("--verbose")
            .run(remaining)
    } else {
        command.run(remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    fn waiters(gate: &Gate, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut state = gate.state.lock().unwrap();
        while state.waiting.len() < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "worker did not queue");
            state = gate.changed.wait_timeout(state, remaining).unwrap().0;
        }
    }

    #[test]
    fn management_workers_are_gated_but_version_and_user_flags_are_not() {
        assert!(management_phase(
            &crate::Command::new("engine").arg("--editor")
        ));
        assert!(management_phase(
            &crate::Command::new("engine").arg("--export-release")
        ));
        assert!(management_phase(&crate::Command::new("engine").args([
            "--headless",
            "--script",
            "inspect.gd"
        ])));
        assert!(!management_phase(
            &crate::Command::new("engine").arg("--version")
        ));
        assert!(!management_phase(&crate::Command::new("engine").args([
            "--",
            "--editor",
            "--headless"
        ])));
    }

    #[test]
    fn waiting_native_phases_run_in_fifo_order() {
        let gate = Arc::new(Gate::default());
        let first = gate.acquire(Duration::from_secs(3)).unwrap();
        let (sender, receiver) = mpsc::channel();
        let second_gate = gate.clone();
        let second_sender = sender.clone();
        let second = std::thread::spawn(move || {
            let _lease = second_gate.acquire(Duration::from_secs(3)).unwrap();
            second_sender.send(1).unwrap();
        });
        waiters(&gate, 1);
        let third_gate = gate.clone();
        let third = std::thread::spawn(move || {
            let _lease = third_gate.acquire(Duration::from_secs(3)).unwrap();
            sender.send(2).unwrap();
        });
        waiters(&gate, 2);
        assert!(receiver.try_recv().is_err());
        drop(first);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(3)).unwrap(), 1);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(3)).unwrap(), 2);
        second.join().unwrap();
        third.join().unwrap();
        assert!(!gate.state.lock().unwrap().active);
    }

    #[test]
    fn expired_waiter_does_not_block_following_requests() {
        let gate = Gate::default();
        let first = gate.acquire(Duration::from_secs(1)).unwrap();
        let error = gate.acquire(Duration::ZERO).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(gate.state.lock().unwrap().waiting.is_empty());
        drop(first);
        assert!(gate.acquire(Duration::ZERO).is_ok());
    }
}
