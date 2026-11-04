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

/// Coordination only. The owner must retain this lease until its worker's
/// retirement is known, including while retaining an uncertain worker.
// Only the Windows external-native path uses this.
#[cfg_attr(not(windows), allow(dead_code))]
#[must_use]
pub(crate) struct ManagementLease<'a> {
    _lease: Lease<'a>,
    deadline: Instant,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl ManagementLease<'_> {
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
struct WaitingTicket<'a> {
    gate: &'a Gate,
    ticket: u64,
    registered: bool,
}

impl Drop for WaitingTicket<'_> {
    fn drop(&mut self) {
        if self.registered {
            self.gate
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .waiting
                .retain(|value| *value != self.ticket);
            self.gate.changed.notify_all();
        }
    }
}

impl Gate {
    fn acquire(&self, budget: Duration) -> io::Result<Lease<'_>> {
        let context = crate::operation_context::OperationContext::current();
        if let Some(context) = &context {
            context.check()?;
        }
        let deadline = context.as_ref().map_or(Instant::now() + budget, |context| {
            context.deadline().min(Instant::now() + budget)
        });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.waiting.len() >= 8 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Native management worker queue reached its eight-request bound",
            ));
        }
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        state.waiting.push_back(ticket);
        self.changed.notify_all();
        loop {
            if let Some(context) = &context {
                if let Err(error) = context.check() {
                    state.waiting.retain(|value| *value != ticket);
                    self.changed.notify_all();
                    return Err(error);
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() && context.is_some() {
                state.waiting.retain(|value| *value != ticket);
                self.changed.notify_all();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Native management worker queue exceeded its operation budget",
                ));
            }
            if !state.active && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active = true;
                return Ok(Lease(self));
            }
            if remaining.is_zero() {
                state.waiting.retain(|value| *value != ticket);
                self.changed.notify_all();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Native management worker queue exceeded its operation budget",
                ));
            }
            let wait = if context.is_some() {
                remaining.min(Duration::from_millis(25))
            } else {
                remaining
            };
            state = self
                .changed
                .wait_timeout(state, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn acquire_checked(
        &self,
        deadline: Instant,
        mut check_owner: impl FnMut() -> io::Result<()>,
    ) -> io::Result<ManagementLease<'_>> {
        let context = crate::operation_context::OperationContext::current();
        let deadline = context
            .as_ref()
            .map_or(deadline, |context| deadline.min(context.deadline()));
        let mut check = || {
            let check_deadline = || {
                if Instant::now() >= deadline {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Native management worker queue exceeded its operation budget",
                    ))
                } else {
                    Ok(())
                }
            };
            check_deadline()?;
            if let Some(context) = &context {
                context.check()?;
            }
            check_owner()?;
            if let Some(context) = &context {
                context.check()?;
            }
            check_deadline()
        };
        check()?;
        let ticket = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.waiting.len() >= 8 {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Native management worker queue reached its eight-request bound",
                ));
            }
            let ticket = state.next;
            state.next = state.next.wrapping_add(1);
            state.waiting.push_back(ticket);
            self.changed.notify_all();
            ticket
        };
        let mut queued = WaitingTicket {
            gate: self,
            ticket,
            registered: true,
        };
        loop {
            // Owner callbacks may take their own locks. Never invoke one while
            // holding the gate mutex; the registered ticket preserves FIFO.
            check()?;
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.active && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active = true;
                queued.registered = false;
                let lease = Lease(self);
                drop(state);
                check()?;
                return Ok(ManagementLease {
                    _lease: lease,
                    deadline,
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(state);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Native management worker queue exceeded its operation budget",
                ));
            }
            let state = self
                .changed
                .wait_timeout(state, remaining.min(Duration::from_millis(25)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
            drop(state);
        }
    }
}

fn management_gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(Gate::default)
}

/// Acquire the same gate used by `run`, without launching a process. The caller
/// supplies checks for its original owner/controller and retains the returned
/// lease with any worker whose retirement is uncertain. Queueing consumes the
/// supplied absolute deadline, clipped to the current operation's deadline.
/// The callback must be a bounded original-owner check, without gate reentry.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn acquire_checked_management(
    deadline: Instant,
    check_owner: impl FnMut() -> io::Result<()>,
) -> io::Result<ManagementLease<'static>> {
    management_gate().acquire_checked(deadline, check_owner)
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
    let budget = if let Some(context) = crate::operation_context::OperationContext::current() {
        context.budget(budget)?
    } else {
        budget
    };
    if !management_phase(command) {
        return command.run(budget);
    }
    let started = Instant::now();
    let _lease = management_gate().acquire(budget)?;
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
    use std::sync::atomic::{AtomicBool, Ordering};
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
    #[test]
    fn queue_admission_is_bounded_and_cancelled_job_tickets_are_removed() {
        let gate = Arc::new(Gate::default());
        let held = gate.acquire(Duration::from_secs(5)).unwrap();
        let mut threads = Vec::new();
        for _ in 0..8 {
            let queued = gate.clone();
            threads.push(std::thread::spawn(move || {
                let _lease = queued.acquire(Duration::from_secs(5)).unwrap();
            }));
        }
        waiters(&gate, 8);
        assert_eq!(
            gate.acquire(Duration::from_millis(10))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        drop(held);
        for thread in threads {
            thread.join().unwrap();
        }
        let held = gate.acquire(Duration::from_secs(5)).unwrap();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(5),
        );
        let cancelling = context.clone();
        let queued = gate.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = cancelling
                .scope(|| queued.acquire(Duration::from_secs(5)).map(drop))
                .unwrap();
            sender.send(result).unwrap();
        });
        waiters(&gate, 1);
        context.request_cancel().unwrap();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        worker.join().unwrap();
        assert!(gate.state.lock().unwrap().waiting.is_empty());
        drop(held);
        assert!(context.scope(|| ()).is_err());
        let fresh = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(5),
        );
        fresh
            .scope(|| assert!(gate.acquire(Duration::ZERO).is_err()))
            .unwrap();
    }

    #[test]
    fn checked_management_and_legacy_run_admission_share_the_global_gate() {
        assert!(crate::operation_context::OperationContext::current().is_none());
        let gate = management_gate();
        let legacy = gate.acquire(Duration::from_secs(3)).unwrap();
        let (sender, receiver) = mpsc::channel();
        let checked = std::thread::spawn(move || {
            let lease =
                acquire_checked_management(Instant::now() + Duration::from_secs(3), || Ok(()))
                    .unwrap();
            assert!(sender.send(lease).is_ok());
        });
        waiters(gate, 1);
        assert!(receiver.try_recv().is_err());
        drop(legacy);
        let checked_lease = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        checked.join().unwrap();

        // Moving the opaque lease into a retained owner keeps the legacy path
        // blocked until that owner explicitly drops it.
        let (sender, receiver) = mpsc::channel();
        let legacy = std::thread::spawn(move || {
            let _lease = management_gate().acquire(Duration::from_secs(3)).unwrap();
            sender.send(()).unwrap();
        });
        waiters(gate, 1);
        assert!(receiver.try_recv().is_err());
        drop(checked_lease);
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        legacy.join().unwrap();
    }

    #[test]
    fn checked_management_refuses_expired_and_cancelled_free_admission_without_context() {
        assert!(crate::operation_context::OperationContext::current().is_none());
        let gate = Gate::default();
        let expired = gate.acquire_checked(Instant::now(), || Ok(()));
        assert_eq!(expired.err().unwrap().kind(), io::ErrorKind::TimedOut);
        let cancelled = gate.acquire_checked(Instant::now() + Duration::from_secs(3), || {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "original owner cancelled",
            ))
        });
        assert_eq!(cancelled.err().unwrap().kind(), io::ErrorKind::Interrupted);
        assert!(gate.state.lock().unwrap().waiting.is_empty());
        assert!(!gate.state.lock().unwrap().active);
        assert!(gate.acquire(Duration::ZERO).is_ok());
    }

    #[test]
    fn checked_management_rechecks_after_admission_without_holding_the_gate_mutex() {
        let gate = Gate::default();
        let result = gate.acquire_checked(Instant::now() + Duration::from_secs(3), || {
            if gate.state.lock().unwrap().active {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "owner changed at admission",
                ))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
        assert!(gate.state.lock().unwrap().waiting.is_empty());
        assert!(!gate.state.lock().unwrap().active);
        assert!(gate.acquire(Duration::ZERO).is_ok());
    }

    #[test]
    fn checked_management_cancelled_ticket_is_removed_before_the_following_legacy_waiter() {
        let gate = Arc::new(Gate::default());
        let held = gate.acquire(Duration::from_secs(3)).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let queued = gate.clone();
        let cancellation = cancelled.clone();
        let (sender, receiver) = mpsc::channel();
        let first = std::thread::spawn(move || {
            let result = queued
                .acquire_checked(Instant::now() + Duration::from_secs(3), || {
                    if cancellation.load(Ordering::Acquire) {
                        Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "original owner cancelled",
                        ))
                    } else {
                        Ok(())
                    }
                })
                .map(drop);
            sender.send(result).unwrap();
        });
        waiters(&gate, 1);
        let queued = gate.clone();
        let (following_sender, following_receiver) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let _lease = queued.acquire(Duration::from_secs(3)).unwrap();
            following_sender.send(()).unwrap();
        });
        waiters(&gate, 2);
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        first.join().unwrap();
        assert_eq!(gate.state.lock().unwrap().waiting.len(), 1);
        assert!(following_receiver.try_recv().is_err());
        drop(held);
        following_receiver
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        second.join().unwrap();
        assert!(gate.state.lock().unwrap().waiting.is_empty());
    }

    #[test]
    fn checked_management_clips_deadline_and_checks_the_current_operation() {
        let gate = Gate::default();
        let context = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(3),
        );
        context
            .scope(|| {
                let lease = gate
                    .acquire_checked(Instant::now() + Duration::from_secs(30), || Ok(()))
                    .unwrap();
                assert_eq!(lease.deadline(), context.deadline());
                drop(lease);
                context.request_cancel().unwrap();
                let result =
                    gate.acquire_checked(Instant::now() + Duration::from_secs(30), || Ok(()));
                assert_eq!(result.err().unwrap().kind(), io::ErrorKind::Interrupted);
            })
            .unwrap();
        assert!(gate.state.lock().unwrap().waiting.is_empty());
        assert!(!gate.state.lock().unwrap().active);
    }
}
