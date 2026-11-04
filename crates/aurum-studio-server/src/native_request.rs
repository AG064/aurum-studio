//! Connection-scoped receiver for the opt-in native request timing protocol.
use crate::http::{self, Request, Response};
use aurum_studio_core::native_request_control as control;
use aurum_studio_core::operation_context::{OperationContext, Phase};
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WATCH_POLL: Duration = Duration::from_millis(100);

pub(crate) fn requested(request: &Request) -> Result<bool, Response> {
    let controls: Vec<_> = request
        .headers
        .iter()
        .filter(|(name, _)| name == control::HEADER)
        .collect();
    if controls.is_empty() {
        return Ok(false);
    }
    let expectations: Vec<_> = request
        .headers
        .iter()
        .filter(|(name, _)| name == "expect")
        .collect();
    if controls.len() != 1
        || controls[0].1 != control::PROTOCOL
        || expectations.len() != 1
        || expectations[0].1 != "100-continue"
        || request.method != "POST"
        || request.path != "/api/project"
    {
        return Err(Response::error(400, "unsupported native request control\n"));
    }
    let content_types: Vec<_> = request
        .headers
        .iter()
        .filter(|(name, _)| name == "content-type")
        .collect();
    if content_types.len() != 1 || content_types[0].1 != "application/json" {
        return Err(Response::error(
            400,
            "native request control requires JSON\n",
        ));
    }
    Ok(true)
}

/// The caller must authenticate the request head before entering this function.
pub(crate) fn receive(
    reader: &mut BufReader<&TcpStream>,
    writer: &mut &TcpStream,
    request: &mut Request,
) -> Result<OperationContext, String> {
    let length = http::body_length(&request.headers).map_err(|error| error.to_string())?;
    if length < control::TIMING_PREFIX_BYTES + 3
        || length > control::body_len(control::MAX_REQUEST_BYTES)?
    {
        return Err("native controlled request body is outside its bound".into());
    }
    if !reader.buffer().is_empty() {
        return Err("native controlled request body arrived before ready".into());
    }
    reader
        .get_ref()
        .set_nonblocking(false)
        .map_err(|_| "native request socket setup failed")?;
    writer
        .set_write_timeout(Some(crate::READ_TIMEOUT))
        .map_err(|_| "native request ready timeout setup failed")?;
    // The caller samples remaining only after receiving this interim reply.
    // This anchor consequently cannot credit handshake, transfer or parse time.
    let ready_anchor = Instant::now();
    writer
        .write_all(control::READY)
        .map_err(|_| "native request ready write failed")?;
    let ingress_deadline = ready_anchor
        .checked_add(crate::READ_TIMEOUT)
        .ok_or("native request ingress deadline overflowed")?;
    let mut ingress = Ingress {
        reader,
        deadline: ingress_deadline,
    };
    let mut envelope = vec![0; length];
    ingress
        .read_exact(&mut envelope[..control::TIMING_PREFIX_BYTES])
        .map_err(|_| "native controlled request timing read failed")?;
    let remaining = control::decode_timing_prefix(&envelope[..control::TIMING_PREFIX_BYTES])?;
    let context = OperationContext::new(control::deadline_at(ready_anchor, remaining)?);
    context.check().map_err(|error| error.to_string())?;
    // Do not grant a new body allowance after timing arrives. Both the existing
    // ingress ceiling and the original request end apply to every remaining read.
    ingress.deadline = ingress.deadline.min(context.deadline());
    ingress
        .read_exact(&mut envelope[control::TIMING_PREFIX_BYTES..])
        .map_err(|error| {
            if matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ) {
                "native controlled request deadline expired during body read"
            } else {
                "native controlled request body read failed"
            }
        })?;
    let (_, input) = control::decode(&envelope)?;
    let body =
        serde_json::to_vec(&input).map_err(|_| "native controlled operation encoding failed")?;
    control::body_len(body.len())?;
    if !ingress.reader.buffer().is_empty() {
        return Err("native controlled request has trailing bytes".into());
    }
    context.check().map_err(|error| error.to_string())?;
    request.body = body;
    Ok(context)
}

/// Every ingress read consumes one absolute allowance, including partial reads.
struct Ingress<'a, 'b> {
    reader: &'a mut BufReader<&'b TcpStream>,
    deadline: Instant,
}
impl Read for Ingress<'_, '_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native request ingress expired",
            ));
        }
        self.reader.get_ref().set_read_timeout(Some(remaining))?;
        let count = self.reader.read(bytes)?;
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native request ingress expired",
            ));
        }
        Ok(count)
    }
}

/// The socket has exactly one request. EOF or extra inbound bytes abandon that
/// transient request; neither condition closes a controller or a gameplay tree.
struct PeerCancellation {
    stop: Arc<AtomicBool>,
    wake: TcpStream,
    worker: Option<JoinHandle<()>>,
    context: OperationContext,
}
impl PeerCancellation {
    fn start(stream: &TcpStream, context: &OperationContext) -> io::Result<Self> {
        let mut socket = crate::private_clone(stream)?;
        socket.set_read_timeout(Some(WATCH_POLL))?;
        let wake = crate::private_clone(&socket)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let operation = context.clone();
        let worker = std::thread::Builder::new()
            .name("aurum-request-peer".into())
            .spawn(move || {
                let mut byte = [0];
                while !stopping.load(Ordering::Acquire) {
                    let result = socket.read(&mut byte);
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    match result {
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::TimedOut
                                    | io::ErrorKind::WouldBlock
                                    | io::ErrorKind::Interrupted
                            ) =>
                        {
                            continue
                        }
                        _ => {
                            // Refusal after commit is expected: a request cannot
                            // use peer cancellation to undo accepted gameplay.
                            let _ = operation.request_cancel();
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            wake,
            worker: Some(worker),
            context: context.clone(),
        })
    }
}
impl Drop for PeerCancellation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Read-half shutdown wakes a parked watcher while preserving response
        // writes. A failed shutdown still leaves the checked finite read timeout.
        let _ = self.wake.shutdown(Shutdown::Read);
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            self.context.mark_recovery_required();
        }
    }
}

pub(crate) fn with_peer<T>(
    stream: &TcpStream,
    context: &OperationContext,
    work: impl FnOnce() -> T,
) -> io::Result<T> {
    context.check()?;
    let _peer = PeerCancellation::start(stream, context)?;
    context.phase(Phase::Running);
    context.scope(work)
}

#[cfg(test)]
#[path = "native_request_tests.rs"]
mod tests;
