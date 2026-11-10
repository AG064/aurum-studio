//! Opt-in timing envelope for one authenticated native-controller request.
//! The ready anchor is receiver-owned and precedes caller budget sampling.
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};

pub const HEADER: &str = "x-aurum-request-control";
pub const PROTOCOL: &str = "aurum-controller-request/1";
pub const READY: &[u8] =
    b"HTTP/1.1 100 Continue\r\nX-Aurum-Request-Control: aurum-controller-request/1\r\n\r\n";
pub const MAX_REMAINING: Duration = Duration::from_secs(600);
pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const PREFIX: &str = "{\"remaining_ms\":\"";
const MIDDLE: &str = "\",\"request\":";
const DIGITS: usize = 20;
pub const TIMING_PREFIX_BYTES: usize = PREFIX.len() + DIGITS + MIDDLE.len();

pub fn body_len(request_len: usize) -> Result<usize, String> {
    if request_len > MAX_REQUEST_BYTES {
        return Err("native controlled request exceeds 2 MiB".into());
    }
    Ok(TIMING_PREFIX_BYTES + request_len + 1)
}

/// Fixed-width timing keeps Content-Length stable across the ready handshake.
/// Millisecond truncation only shortens the existing caller budget.
pub fn encode(request: &[u8], remaining: Duration) -> Result<Vec<u8>, String> {
    let length = body_len(request.len())?;
    let millis = remaining.as_millis();
    if millis == 0 || remaining > MAX_REMAINING {
        return Err("native controlled request budget is outside its bound".into());
    }
    let mut body = Vec::with_capacity(length);
    body.extend_from_slice(PREFIX.as_bytes());
    body.extend_from_slice(format!("{millis:020}").as_bytes());
    body.extend_from_slice(MIDDLE.as_bytes());
    body.extend_from_slice(request);
    body.push(b'}');
    Ok(body)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    remaining_ms: String,
    request: Value,
}

fn parse_remaining(text: &str) -> Result<Duration, String> {
    if text.len() != DIGITS || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid native controlled request budget".into());
    }
    let millis = text
        .parse::<u64>()
        .map_err(|_| "invalid native controlled request budget")?;
    let remaining = Duration::from_millis(millis);
    if remaining.is_zero() || remaining > MAX_REMAINING {
        return Err("native controlled request budget is outside its bound".into());
    }
    Ok(remaining)
}

/// Parse timing before the operation body, so subsequent ingress shares its end.
pub fn decode_timing_prefix(prefix: &[u8]) -> Result<Duration, String> {
    if prefix.len() != TIMING_PREFIX_BYTES
        || !prefix.starts_with(PREFIX.as_bytes())
        || !prefix.ends_with(MIDDLE.as_bytes())
    {
        return Err("invalid native controlled request timing prefix".into());
    }
    let text = std::str::from_utf8(&prefix[PREFIX.len()..PREFIX.len() + DIGITS])
        .map_err(|_| "invalid native controlled request budget")?;
    parse_remaining(text)
}

/// Returns an unprivileged timing constraint and the original operation only.
pub fn decode(body: &[u8]) -> Result<(Duration, Value), String> {
    if body.len() > body_len(MAX_REQUEST_BYTES)? {
        return Err("native controlled request exceeds its bound".into());
    }
    let envelope: Envelope =
        serde_json::from_slice(body).map_err(|_| "invalid native controlled request envelope")?;
    if !envelope.request.is_object() {
        return Err("invalid native controlled request operation".into());
    }
    let remaining = parse_remaining(&envelope.remaining_ms)?;
    Ok((remaining, envelope.request))
}

/// Never anchor at body completion: that would credit transfer and parsing time.
pub fn deadline_at(ready_anchor: Instant, remaining: Duration) -> Result<Instant, String> {
    if remaining.is_zero() || remaining > MAX_REMAINING {
        return Err("native controlled request budget is outside its bound".into());
    }
    ready_anchor
        .checked_add(remaining)
        .ok_or_else(|| "native controlled request deadline overflowed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controlled_envelope_preserves_operation_and_fixed_content_length() {
        let request = br#"{"op":"native_status"}"#;
        let first = encode(request, Duration::from_millis(1)).unwrap();
        let last = encode(request, MAX_REMAINING).unwrap();
        assert_eq!(first.len(), body_len(request.len()).unwrap());
        assert_eq!(first.len(), last.len());
        assert_eq!(
            decode_timing_prefix(&first[..TIMING_PREFIX_BYTES]).unwrap(),
            Duration::from_millis(1)
        );
        let (remaining, decoded) = decode(&first).unwrap();
        assert_eq!(remaining, Duration::from_millis(1));
        assert_eq!(decoded, serde_json::json!({"op":"native_status"}));
        assert!(decoded.get("remaining_ms").is_none());
        assert!(decoded.get("owner_capability").is_none());
    }

    #[test]
    fn controlled_envelope_refuses_duplicate_unknown_and_invalid_budget_fields() {
        for body in [
            r#"{"remaining_ms":"00000000000000000001","remaining_ms":"00000000000000000002","request":{}}"#,
            r#"{"remaining_ms":"00000000000000000001","request":{},"read_only":false}"#,
            r#"{"remaining_ms":"00000000000000000000","request":{}}"#,
            r#"{"remaining_ms":"00000000000000600001","request":{}}"#,
            r#"{"remaining_ms":"99999999999999999999","request":{}}"#,
            r#"{"remaining_ms":"1","request":{}}"#,
            r#"{"remaining_ms":"+0000000000000000001","request":{}}"#,
            r#"{"remaining_ms":1,"request":{}}"#,
            r#"{"remaining_ms":"00000000000000000001","request":[]}"#,
        ] {
            assert!(decode(body.as_bytes()).is_err(), "accepted {body}");
        }
        assert!(encode(b"{}", Duration::from_nanos(1)).is_err());
        assert!(encode(b"{}", MAX_REMAINING + Duration::from_nanos(1)).is_err());
        assert!(body_len(MAX_REQUEST_BYTES + 1).is_err());
    }

    #[test]
    fn ready_anchor_consumes_handshake_transfer_and_parsing_time() {
        let anchor = Instant::now();
        let caller_sample = anchor + Duration::from_millis(40);
        let caller_end = caller_sample + Duration::from_millis(80);
        let receiver_end = deadline_at(anchor, caller_end - caller_sample).unwrap();
        assert_eq!(receiver_end, anchor + Duration::from_millis(80));
        assert!(receiver_end < caller_end);
        let body_done = anchor + Duration::from_millis(90);
        assert!(body_done >= receiver_end);
    }
}
