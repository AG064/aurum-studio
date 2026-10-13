//! Scoped cancellation, absolute deadlines and named progress for owned jobs.
//! Persistent gameplay is not registered as a transient command in this scope.
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::io;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Running,
    Staging,
    Dependencies,
    Compiling,
    Importing,
    Validating,
    Restoring,
    Activating,
    Publishing,
    Cleanup,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    Deadline,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub sequence: u64,
}

struct Control {
    cancelled: bool,
    interrupted: bool,
    committing: bool,
    committed: bool,
    commit_status_verified: bool,
    recovery_required: bool,
    termination_unconfirmed: bool,
}
struct Inner {
    deadline: Instant,
    control: Mutex<Control>,
    progress: Mutex<Progress>,
}

#[derive(Clone)]
pub struct OperationContext(Arc<Inner>);
impl std::fmt::Debug for OperationContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OperationContext { scoped operation }")
    }
}
thread_local! {
    static CURRENT: RefCell<Vec<OperationContext>> = const { RefCell::new(Vec::new()) };
}
struct Scope {
    _not_send: PhantomData<Rc<()>>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

impl OperationContext {
    pub fn new(deadline: Instant) -> Self {
        Self(Arc::new(Inner {
            deadline,
            control: Mutex::new(Control {
                cancelled: false,
                interrupted: false,
                committing: false,
                committed: false,
                commit_status_verified: true,
                recovery_required: false,
                termination_unconfirmed: false,
            }),
            progress: Mutex::new(Progress {
                phase: Phase::Queued,
                sequence: 1,
            }),
        }))
    }
    pub fn current() -> Option<Self> {
        CURRENT.with(|stack| stack.borrow().last().cloned())
    }
    /// Nested scopes may reuse this job but cannot override another job's control.
    pub fn scope<T>(&self, work: impl FnOnce() -> T) -> io::Result<T> {
        self.check()?;
        CURRENT.with(|stack| {
            let mut stack = stack.borrow_mut();
            if stack.len() >= 8
                || stack
                    .last()
                    .is_some_and(|parent| !Arc::ptr_eq(&parent.0, &self.0))
            {
                return Err(io::Error::other("operation scope nesting was refused"));
            }
            stack.push(self.clone());
            Ok(())
        })?;
        let _scope = Scope {
            _not_send: PhantomData,
        };
        Ok(work())
    }
    pub fn deadline(&self) -> Instant {
        self.0.deadline
    }
    pub fn stop_reason(&self) -> Option<StopReason> {
        let control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if control.interrupted {
            Some(StopReason::Interrupted)
        } else if Instant::now() >= self.0.deadline {
            Some(StopReason::Deadline)
        } else if control.cancelled {
            Some(StopReason::Cancelled)
        } else {
            None
        }
    }
    pub fn check(&self) -> io::Result<()> {
        let control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // An entered commit must finish or fail atomically. The caller must not
        // use this narrow barrier around compilation or other long-running work.
        if control.committing {
            return Ok(());
        }
        if control.interrupted {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "job host is shutting down",
            ));
        }
        if Instant::now() >= self.0.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "job absolute deadline expired",
            ));
        }
        if control.cancelled && !control.committed {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "job cancellation requested",
            ));
        }
        Ok(())
    }
    pub fn budget(&self, local: Duration) -> io::Result<Duration> {
        self.check()?;
        let remaining = self
            .0
            .deadline
            .saturating_duration_since(Instant::now())
            .min(local);
        if remaining.is_zero() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "job absolute deadline expired",
            ))
        } else {
            Ok(remaining)
        }
    }
    pub fn request_cancel(&self) -> io::Result<bool> {
        let mut control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if control.committed {
            return Err(io::Error::other(
                "job has committed; cancellation cannot undo it",
            ));
        }
        let changed = !control.cancelled;
        control.cancelled = true;
        drop(control);
        if changed {
            self.touch();
        }
        Ok(changed)
    }
    pub(crate) fn interrupt(&self) {
        let mut control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control.interrupted = true;
        drop(control);
        self.touch();
    }
    pub fn cancellation_requested(&self) -> bool {
        let control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control.cancelled || control.interrupted
    }
    pub fn committed(&self) -> bool {
        self.0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .committed
    }
    pub fn commit_status_verified(&self) -> bool {
        self.0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .commit_status_verified
    }
    pub fn recovery_required(&self) -> bool {
        self.0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recovery_required
    }
    pub fn mark_recovery_required(&self) {
        let mut control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control.recovery_required = true;
        drop(control);
        self.touch();
    }
    pub fn termination_unconfirmed(&self) -> bool {
        self.0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .termination_unconfirmed
    }
    pub(crate) fn mark_termination_unconfirmed(&self) {
        self.0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .termination_unconfirmed = true;
        self.touch();
    }
    pub fn progress(&self) -> Progress {
        *self
            .0
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    pub fn phase(&self, phase: Phase) {
        let mut progress = self
            .0
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if progress.phase != Phase::Finished && progress.phase != phase {
            progress.phase = phase;
            progress.sequence = progress.sequence.saturating_add(1);
        }
    }
    pub(crate) fn touch(&self) {
        let mut progress = self
            .0
            .progress
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        progress.sequence = progress.sequence.saturating_add(1);
    }
    pub(crate) fn recovered_with_verification(
        sequence: u64,
        committed: bool,
        verified: bool,
        recovery_required: bool,
    ) -> Self {
        let context = Self::new(Instant::now());
        *context.0.progress.lock().unwrap() = Progress {
            phase: Phase::Finished,
            sequence,
        };
        {
            let mut control = context.0.control.lock().unwrap();
            control.committed = committed;
            control.commit_status_verified = verified;
            control.recovery_required = recovery_required;
        }
        context
    }
    pub fn commit_barrier(&self) -> io::Result<CommitGuard> {
        let mut control = self
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if control.committing {
            return Err(io::Error::other("job commit barrier already entered"));
        }
        if !control.commit_status_verified || control.recovery_required {
            return Err(io::Error::other("job commit status requires recovery"));
        }
        if control.cancelled || control.interrupted || Instant::now() >= self.0.deadline {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "job cannot enter commit after cancellation or deadline",
            ));
        }
        control.committing = true;
        Ok(CommitGuard {
            context: self.clone(),
            finished: false,
            _not_send: PhantomData,
        })
    }
}

/// Call finish only after the publication/activation actually committed.
pub struct CommitGuard {
    context: OperationContext,
    finished: bool,
    _not_send: PhantomData<Rc<()>>,
}
impl CommitGuard {
    pub fn finish(mut self) {
        let mut control = self
            .context
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control.committed = true;
        control.committing = false;
        self.finished = true;
        drop(control);
        self.context.touch();
    }
    /// Certify that this attempt left no effects after a checked full rollback.
    pub fn rollback_complete(mut self) {
        let mut control = self
            .context
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control.committing = false;
        self.finished = true;
        drop(control);
        self.context.touch();
    }
}
impl Drop for CommitGuard {
    fn drop(&mut self) {
        if !self.finished {
            let mut control = self
                .context
                .0
                .control
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            control.committing = false;
            control.commit_status_verified = false;
            drop(control);
            self.context.touch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_scopes_restore_context_even_after_a_panic() {
        let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
        assert!(OperationContext::current().is_none());
        context
            .scope(|| {
                let other = OperationContext::new(context.deadline());
                assert!(other.scope(|| ()).is_err());
                context
                    .scope(|| assert!(OperationContext::current().is_some()))
                    .unwrap();
                let panic = std::panic::catch_unwind(|| context.scope(|| panic!("fixture")));
                assert!(panic.is_err());
                assert!(OperationContext::current().is_some());
            })
            .unwrap();
        assert!(OperationContext::current().is_none());
    }
    #[test]
    fn progress_and_commit_report_deferred_cancellation_accurately() {
        let context = OperationContext::new(Instant::now() + Duration::from_secs(5));
        let sequence = context.progress().sequence;
        context.phase(Phase::Compiling);
        assert!(context.progress().sequence > sequence);
        let commit = context.commit_barrier().unwrap();
        context.request_cancel().unwrap();
        assert!(context.check().is_ok());
        commit.finish();
        assert!(context.committed());
        assert!(context.request_cancel().is_err());
        assert!(context.check().is_ok());
        let rejected = OperationContext::new(context.deadline());
        rejected.request_cancel().unwrap();
        assert!(rejected.commit_barrier().is_err());
        let aborted = OperationContext::new(context.deadline());
        let commit = aborted.commit_barrier().unwrap();
        aborted.request_cancel().unwrap();
        drop(commit);
        assert!(!aborted.committed());
        assert!(aborted.check().is_err());
    }
    #[test]
    fn incomplete_commit_requires_evidence_and_verified_rollback_preserves_status() {
        let uncertain = OperationContext::new(Instant::now() + Duration::from_secs(5));
        drop(uncertain.commit_barrier().unwrap());
        assert!(!uncertain.commit_status_verified());
        assert!(uncertain.commit_barrier().is_err());
        let rolled_back = OperationContext::new(uncertain.deadline());
        let guard = rolled_back.commit_barrier().unwrap();
        rolled_back.request_cancel().unwrap();
        guard.rollback_complete();
        assert!(!rolled_back.committed());
        assert!(rolled_back.commit_status_verified());
        assert!(rolled_back.check().is_err());
        let sequential = OperationContext::new(uncertain.deadline());
        sequential.commit_barrier().unwrap().finish();
        sequential.commit_barrier().unwrap().rollback_complete();
        assert!(sequential.committed());
        assert!(sequential.commit_status_verified());
    }
    #[test]
    fn absolute_deadlines_do_not_reset_inside_nested_operations() {
        let context = OperationContext::new(Instant::now() - Duration::from_millis(1));
        assert_eq!(context.check().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(context.budget(Duration::from_secs(600)).is_err());
    }
}
