//! Private finite native-command output; no daemon/container retirement proof.
//! Prefix truncation is distinct from pipe EOF, read failure and safe cleanup.
use super::super::{ChildEnvironment, RegisteredStartup, Worker};
use super::{
    check_control, AcceptancePermit, Admission, PipeReader, Resources, StreamController, CHUNK,
};
use crate::candidate_workspace::LifetimeFence;
use crate::operation_context::OperationContext;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

const PREFIX_BYTES: usize = 2 * 1024 * 1024;
const TRUNCATED: &[u8] = b"\nERROR: captured output exceeded 2 MiB; further output was discarded\n";
// Existing pipe accounting remains overflow-safe at the one-byte refusal edge.
// Memory stays at two fixed pending slots plus the two bounded prefixes.
const DRAIN_BYTES: u64 = u64::MAX - 1;

pub(crate) struct CommandSpawn<'a> {
    pub(crate) program: &'a Path,
    pub(crate) argv0: &'a Path,
    pub(crate) args: &'a [String],
    pub(crate) directory: Option<&'a Path>,
    pub(crate) overrides: &'a [(String, String)],
    pub(crate) lifetime: &'a LifetimeFence,
    pub(crate) deadline: Instant,
}

pub(super) struct Prefixes {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}
impl Prefixes {
    fn allocate() -> io::Result<Self> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        stdout
            .try_reserve_exact(PREFIX_BYTES + TRUNCATED.len())
            .map_err(|_| io::Error::other("command stdout prefix allocation refused"))?;
        stderr
            .try_reserve_exact(PREFIX_BYTES + TRUNCATED.len())
            .map_err(|_| io::Error::other("command stderr prefix allocation refused"))?;
        Ok(Self {
            stdout,
            stderr,
            stdout_truncated: false,
            stderr_truncated: false,
        })
    }
    fn append(&mut self, stdout: bool, bytes: &[u8]) {
        let (prefix, truncated) = if stdout {
            (&mut self.stdout, &mut self.stdout_truncated)
        } else {
            (&mut self.stderr, &mut self.stderr_truncated)
        };
        let keep = PREFIX_BYTES.saturating_sub(prefix.len()).min(bytes.len());
        prefix.extend_from_slice(&bytes[..keep]);
        *truncated |= keep < bytes.len();
    }
    fn into_bytes(mut self) -> (Vec<u8>, Vec<u8>) {
        if self.stdout_truncated {
            self.stdout.extend_from_slice(TRUNCATED);
        }
        if self.stderr_truncated {
            self.stderr.extend_from_slice(TRUNCATED);
        }
        (self.stdout, self.stderr)
    }
}

pub(crate) struct CommandResult {
    pub(crate) code: i32,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    delivery: DeliveryProof,
}
impl CommandResult {
    pub(crate) fn acceptance_gate(&self) -> io::Result<AcceptancePermit> {
        let permit = self.delivery.admission.acceptance(self.delivery.deadline)?;
        check_control(
            &self.delivery.cancelled,
            self.delivery.context.as_ref(),
            self.delivery.deadline,
        )?;
        Ok(permit)
    }
}
/// Diagnostics after positive physical retirement do not establish work success.
pub(crate) struct CommandRecovery {
    pub(crate) observed_code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    delivery: DeliveryProof,
}
impl CommandRecovery {
    /// Failure diagnostics may follow work expiry but cannot approve late work.
    pub(crate) fn timeout_delivery_gate(&self) -> io::Result<AcceptancePermit> {
        let permit = self
            .delivery
            .admission
            .failed_delivery(self.delivery.deadline)?;
        if self.delivery.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "owned command diagnostics were cancelled",
            ));
        }
        if let Some(context) = &self.delivery.context {
            context.check()?;
        }
        if Instant::now() < self.delivery.deadline {
            return Err(io::Error::other(
                "command timeout diagnostics require original deadline expiry",
            ));
        }
        Ok(permit)
    }
}
struct DeliveryProof {
    admission: Arc<Admission>,
    cancelled: Arc<AtomicBool>,
    context: Option<OperationContext>,
    deadline: Instant,
}
pub(crate) struct CommandCapture {
    controller: StreamController,
    reservation: usize,
    resources: Option<Box<Resources>>,
    observed_code: Option<i32>,
    tree_stopped: bool,
    completed: Option<CommandRecovery>,
}

fn start_retirement(resources: &mut Resources) -> Instant {
    resources.request_retirement()
}
fn effective_deadline(resources: &Resources) -> Instant {
    resources
        .retirement_deadline
        .map_or(resources.deadline, |retirement| {
            retirement.min(resources.deadline)
        })
}

impl CommandCapture {
    #[cfg(test)]
    pub(super) fn resources_for_test(&self) -> &Resources {
        self.resources.as_ref().unwrap()
    }

    pub(crate) fn spawn(
        controller: &StreamController,
        request: CommandSpawn<'_>,
    ) -> io::Result<Self> {
        let CommandSpawn {
            program,
            argv0,
            args,
            directory,
            overrides,
            lifetime,
            deadline,
        } = request;
        let context = OperationContext::current();
        if let Some(context) = &context {
            context.check()?;
        }
        let deadline = context
            .as_ref()
            .map_or(deadline, |context| deadline.min(context.deadline()));
        check_control(&AtomicBool::new(false), context.as_ref(), deadline)?;
        controller.reap()?;
        check_control(&AtomicBool::new(false), context.as_ref(), deadline)?;
        let reserved = controller.reserve(context.clone(), deadline)?;
        let reservation = reserved.index;
        let admission = reserved.admission;
        let prefixes = match Prefixes::allocate() {
            Ok(prefixes) => prefixes,
            Err(error) => {
                controller.release(reservation);
                return Err(error);
            }
        };
        let claim = match lifetime.claim() {
            Ok(claim) => claim,
            Err(error) => {
                controller.release(reservation);
                return Err(error);
            }
        };
        let wake = admission.wake.clone();
        let mut capture = Self {
            controller: controller.clone(),
            reservation,
            resources: Some(Box::new(Resources {
                retained: None,
                admission: admission.clone(),
                worker: None,
                pending: None,
                stdout: None,
                stderr: None,
                wake: wake.clone(),
                cancelled: admission.cancelled.clone(),
                context,
                claim: Some(claim),
                deadline,
                retirement_deadline: None,
                stdout_eof: false,
                stderr_eof: false,
                command_prefixes: Some(prefixes),
            })),
            observed_code: None,
            tree_stopped: false,
            completed: None,
        };
        #[cfg(test)]
        super::owner_tests::before_registered_start(&admission);
        let spawned = Worker::spawn_retained_environment_registered(
            program,
            argv0,
            args,
            directory,
            ChildEnvironment::Inherit(overrides),
            RegisteredStartup {
                lifetime: Some(lifetime),
                requested_deadline: Some(deadline),
                registration: admission.registration.clone(),
            },
        );
        let (worker, stdout, stderr) = match spawned {
            Ok(spawned) => spawned,
            Err(failure) => {
                capture.resources.as_mut().unwrap().pending = failure.pending;
                return Err(failure.error);
            }
        };
        let resources = capture.resources.as_mut().unwrap();
        resources.worker = Some(worker);
        resources.check(deadline)?;
        resources.stdout = Some(PipeReader::start(stdout, DRAIN_BYTES, wake.clone())?);
        resources.stderr = Some(PipeReader::start(stderr, DRAIN_BYTES, wake)?);
        if resources.stdout.as_ref().unwrap().identity.is_none()
            || resources.stderr.as_ref().unwrap().identity.is_none()
        {
            return Err(io::Error::other(
                "command pipe original thread identity unavailable",
            ));
        }
        resources.check(deadline)?;
        Ok(capture)
    }

    pub(crate) fn collect_checked(
        &mut self,
        mut check: impl FnMut(Instant) -> io::Result<()>,
    ) -> io::Result<CommandResult> {
        let mut bytes = [0u8; CHUNK];
        let code = loop {
            let resources = self
                .resources
                .as_mut()
                .ok_or_else(|| io::Error::other("command output is already retired"))?;
            let deadline = effective_deadline(resources);
            let generation = resources.wake.generation();
            resources.check(deadline)?;
            let checked = check(deadline);
            resources.check(deadline)?;
            checked?;
            for stdout in [true, false] {
                let reader = if stdout {
                    &resources.stdout
                } else {
                    &resources.stderr
                };
                if let Some(count) = reader.as_ref().unwrap().take(Some(&mut bytes))? {
                    if count == 0 {
                        if stdout {
                            resources.stdout_eof = true;
                        } else {
                            resources.stderr_eof = true;
                        }
                    } else {
                        resources
                            .command_prefixes
                            .as_mut()
                            .unwrap()
                            .append(stdout, &bytes[..count]);
                    }
                }
            }
            resources.check(deadline)?;
            if self.observed_code.is_none() {
                self.observed_code = resources.worker.as_ref().unwrap().try_wait()?;
            }
            if self.observed_code.is_some() && !self.tree_stopped {
                start_retirement(resources);
                resources.worker.as_ref().unwrap().terminate()?;
                self.tree_stopped = true;
                resources.check(effective_deadline(resources))?;
            }
            if resources.stdout_eof && resources.stderr_eof {
                if let Some(code) = self.observed_code {
                    break code;
                }
            }
            resources
                .wake
                .wait(generation, effective_deadline(resources));
        };
        let resources = self.resources.as_mut().unwrap();
        let deadline = effective_deadline(resources);
        start_retirement(resources);
        resources.retire()?;
        let cancelled = resources.cancelled.clone();
        let context = resources.context.clone();
        self.release_retired(deadline);
        // Release physically retired objects before refusing any late work.
        check_control(&cancelled, context.as_ref(), deadline)?;
        let checked = check(deadline);
        check_control(&cancelled, context.as_ref(), deadline)?;
        checked?;
        let completed = self.completed.take().unwrap();
        Ok(CommandResult {
            code,
            stdout: completed.stdout,
            stderr: completed.stderr,
            delivery: completed.delivery,
        })
    }

    fn release_retired(&mut self, delivery_deadline: Instant) {
        let mut resources = self.resources.take().unwrap();
        #[cfg(test)]
        super::owner_tests::after_retired(&resources);
        // All native readers have joined. Committed slots cannot change now.
        // take() delivers buffered bytes before exposing Stopped/Failed/Eof.
        // Transfer only bytes; endpoint flags remain exactly as observed by work.
        let mut bytes = [0u8; CHUNK];
        for stdout in [true, false] {
            let reader = if stdout {
                &resources.stdout
            } else {
                &resources.stderr
            };
            if let Some(reader) = reader {
                loop {
                    match reader.take(Some(&mut bytes)) {
                        Ok(Some(0)) | Ok(None) | Err(_) => break,
                        Ok(Some(count)) => {
                            resources
                                .command_prefixes
                                .as_mut()
                                .unwrap()
                                .append(stdout, &bytes[..count]);
                        }
                    }
                }
            }
        }
        let delivery = DeliveryProof {
            admission: resources.admission.clone(),
            cancelled: resources.cancelled.clone(),
            context: resources.context.clone(),
            deadline: delivery_deadline,
        };
        let (stdout, stderr) = resources.command_prefixes.take().unwrap().into_bytes();
        self.completed = Some(CommandRecovery {
            observed_code: self.observed_code,
            stdout,
            stderr,
            delivery,
        });
        self.controller.release(self.reservation);
        drop(resources);
    }

    pub(crate) fn retire_diagnostics(&mut self) -> io::Result<CommandRecovery> {
        if let Some(resources) = self.resources.as_mut() {
            start_retirement(resources);
            let deadline = effective_deadline(resources);
            resources.retire()?;
            self.release_retired(deadline);
        }
        self.completed
            .take()
            .ok_or_else(|| io::Error::other("command retirement diagnostics unavailable"))
    }
}
impl Drop for CommandCapture {
    fn drop(&mut self) {
        if let Some(mut resources) = self.resources.take() {
            start_retirement(&mut resources);
            if resources.retire().is_ok() {
                self.controller.release(self.reservation);
                drop(resources);
            } else {
                // Prefixes, original pending/worker handles and joins stay reachable.
                self.controller.retain(self.reservation, resources);
            }
        }
    }
}
