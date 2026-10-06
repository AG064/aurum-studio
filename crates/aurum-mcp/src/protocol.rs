//! Minimal MCP / JSON-RPC 2.0 wire types.
//!
//! The Model Context Protocol's stdio transport is newline-delimited
//! JSON-RPC 2.0: one JSON object per line, no framing headers. That is a small
//! enough subset to implement directly, which keeps this crate free of any
//! protocol SDK dependency.
//!
//! Only the surface this server needs is modelled: `initialize`, `ping`,
//! `tools/list`, `tools/call`, and the `notifications/initialized` handshake.

use serde::Deserialize;
use serde_json::{json, Value};

/// The newest protocol revision this server implements.
pub const PROTOCOL_VERSION_LATEST: &str = "2025-06-18";

/// Revisions this server can speak. A client asking for one of these gets it
/// echoed back; anything else is answered with [`PROTOCOL_VERSION_LATEST`],
/// which is what the specification requires.
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC 2.0 standard error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A JSON-RPC error object, ready to be embedded in a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(codes::PARSE_ERROR, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_REQUEST, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }
}

/// A parsed inbound message.
///
/// The distinction matters: a request carries an `id` and must be answered,
/// while a notification must never be answered. Getting that wrong makes
/// clients hang.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

impl Incoming {
    pub fn method(&self) -> &str {
        match self {
            Self::Request { method, .. } | Self::Notification { method, .. } => method,
        }
    }
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

/// Parse one wire line into an [`Incoming`] message.
///
/// Responses (an `id` with no `method`) are not expected from a client — this
/// server never issues requests — and are reported as invalid rather than
/// silently accepted, so a misbehaving client is visible.
pub fn parse_incoming(line: &str) -> Result<Incoming, RpcError> {
    let raw: RawMessage =
        serde_json::from_str(line).map_err(|e| RpcError::parse_error(e.to_string()))?;

    if raw.jsonrpc.as_deref() != Some("2.0") {
        return Err(RpcError::invalid_request("expected jsonrpc \"2.0\""));
    }

    let method = raw
        .method
        .ok_or_else(|| RpcError::invalid_request("missing method"))?;
    let params = raw.params.unwrap_or(Value::Null);

    Ok(match raw.id {
        Some(id) => Incoming::Request { id, method, params },
        None => Incoming::Notification { method, params },
    })
}

/// Build a successful JSON-RPC response.
pub fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC error response.
pub fn failure(id: Value, error: &RpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": error.code, "message": error.message },
    })
}

/// Build the `initialize` result for a client-requested protocol version.
pub fn initialize_result(requested: Option<&str>, server_version: &str) -> Value {
    let agreed = match requested {
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
        _ => PROTOCOL_VERSION_LATEST,
    };
    json!({
        "protocolVersion": agreed,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "aurum-mcp", "version": server_version },
    })
}

/// Wrap a tool payload as an MCP tool result.
///
/// MCP distinguishes *protocol* failures (JSON-RPC errors) from *tool*
/// failures. A tool that ran and failed reports `isError: true` inside a
/// successful result, so the model can read the message and retry rather than
/// seeing the transport break.
pub fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = match payload {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    if payload.is_object() {
        result["structuredContent"] = payload.clone();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request_with_id() {
        let msg = parse_incoming(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(
            msg,
            Incoming::Request {
                id: json!(1),
                method: "tools/list".into(),
                params: Value::Null
            }
        );
    }

    #[test]
    fn parses_a_notification_without_id() {
        let msg =
            parse_incoming(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert_eq!(
            msg,
            Incoming::Notification {
                method: "notifications/initialized".into(),
                params: Value::Null
            }
        );
    }

    #[test]
    fn string_and_numeric_ids_both_survive() {
        let a = parse_incoming(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).unwrap();
        let b = parse_incoming(r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#).unwrap();
        match (a, b) {
            (Incoming::Request { id: a, .. }, Incoming::Request { id: b, .. }) => {
                assert_eq!(a, json!("abc"));
                assert_eq!(b, json!(42));
            }
            _ => panic!("expected two requests"),
        }
    }

    #[test]
    fn rejects_malformed_json_as_parse_error() {
        let err = parse_incoming("{not json").unwrap_err();
        assert_eq!(err.code, codes::PARSE_ERROR);
    }

    #[test]
    fn rejects_missing_jsonrpc_version() {
        let err = parse_incoming(r#"{"id":1,"method":"ping"}"#).unwrap_err();
        assert_eq!(err.code, codes::INVALID_REQUEST);
    }

    #[test]
    fn rejects_a_response_shaped_message() {
        // A client must not send us results; we never issue requests.
        let err = parse_incoming(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap_err();
        assert_eq!(err.code, codes::INVALID_REQUEST);
    }

    #[test]
    fn initialize_echoes_a_supported_version() {
        let r = initialize_result(Some("2024-11-05"), "0.1.0");
        assert_eq!(r["protocolVersion"], "2024-11-05");
        assert_eq!(r["serverInfo"]["name"], "aurum-mcp");
        assert_eq!(r["serverInfo"]["version"], "0.1.0");
    }

    #[test]
    fn initialize_falls_back_to_latest_for_unknown_version() {
        let r = initialize_result(Some("1999-01-01"), "0.1.0");
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION_LATEST);
        let r = initialize_result(None, "0.1.0");
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION_LATEST);
    }

    #[test]
    fn initialize_advertises_tool_capability() {
        let r = initialize_result(Some(PROTOCOL_VERSION_LATEST), "0.1.0");
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tool_result_serializes_objects_as_text() {
        let r = tool_result(&json!({"ok": true}), false);
        assert_eq!(r["isError"], false);
        assert_eq!(r["content"][0]["type"], "text");
        assert!(r["content"][0]["text"].as_str().unwrap().contains("ok"));
        assert_eq!(r["structuredContent"], json!({"ok":true}));
        assert_eq!(r["content"][0]["text"], "{\"ok\":true}");
    }

    #[test]
    fn tool_result_marks_errors() {
        let r = tool_result(&json!("boom"), true);
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "boom");
        assert!(r.get("structuredContent").is_none());
    }

    #[test]
    fn compact_text_round_trips_unicode_and_avoids_pretty_print_overhead() {
        let payload =
            json!({"nodes":[{"name":"Crate","position":{"x":1.0,"y":2.0,"z":3.0}}],"label":"тест"});
        let result = tool_result(&payload, false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), payload);
        assert!(text.len() < serde_json::to_string_pretty(&payload).unwrap().len());
        assert!(tool_result(&json!([1, 2]), false)
            .get("structuredContent")
            .is_none());
    }
}
