//! The stdio JSON-RPC loop.
//!
//! One JSON object per line in, one per line out. Nothing else may be written
//! to stdout — a stray `println!` corrupts the protocol stream — so all
//! diagnostics go to stderr.

use std::io::{BufRead, Read, Write};

use serde_json::{json, Value};

use crate::engine::Engine;
use crate::protocol::{self, failure, parse_incoming, success, Incoming, RpcError};
use crate::tools::{self, PathGuard, ToolContext};

/// How the server should behave for this session.
#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    /// When true, mutating tools are refused and omitted from `tools/list`.
    ///
    /// This mirrors the toolkit's `GODOT_MCP_READ_ONLY` posture: one
    /// annotation-driven switch, not a second code path.
    pub read_only: bool,
    /// Echo protocol traffic to stderr for debugging.
    pub trace: bool,
    /// Directory the live-editor bridge polls, from `--editor-bridge`.
    pub editor_bridge: Option<std::path::PathBuf>,
    /// Tools withheld by name, from `--deny`.
    ///
    /// Precedence, in one place so it cannot drift: a denied tool is refused
    /// whatever its annotations say; read-only then refuses mutating tools that
    /// survived the deny list; and the status tool is exempt from both, because
    /// a client that cannot ask what it is missing cannot tell a withheld tool
    /// from a misspelled one.
    pub denied: Vec<String>,
}

/// Serve MCP over `reader`/`writer` until end of input.
///
/// Generic over the streams so the whole protocol can be exercised in-memory
/// by tests, with no subprocess involved.
pub fn serve<R: BufRead, W: Write>(
    mut reader: R,
    writer: &mut W,
    engine: &mut Engine,
    paths: &PathGuard,
    config: ServerConfig,
) -> std::io::Result<()> {
    loop {
        const MAX_MESSAGE: u64 = 4 * 1024 * 1024;
        let mut bytes = Vec::new();
        if (&mut reader)
            .take(MAX_MESSAGE + 1)
            .read_until(b'\n', &mut bytes)?
            == 0
        {
            break;
        }
        if bytes.len() as u64 > MAX_MESSAGE {
            writeln!(
                writer,
                "{}",
                failure(
                    Value::Null,
                    &RpcError::invalid_params("MCP message exceeds 4 MiB")
                )
            )?;
            writer.flush()?;
            return Ok(());
        }
        let line = String::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if line.trim().is_empty() {
            continue;
        }
        if config.trace {
            eprintln!("[aurum-mcp] <- {}", truncate_for_log(&line));
        }

        let response = match parse_incoming(&line) {
            Ok(message) => handle(message, engine, paths, &config),
            // A parse failure has no id to echo, so the reply carries null.
            Err(error) => Some(failure(Value::Null, &error)),
        };

        if let Some(response) = response {
            let text = serde_json::to_string(&response).unwrap_or_else(|e| {
                // Never leave the client waiting: fall back to a bare error.
                format!(
                    r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":{},"message":"failed to serialize response: {}"}}}}"#,
                    protocol::codes::INTERNAL_ERROR,
                    e
                )
            });
            if config.trace {
                eprintln!("[aurum-mcp] -> {}", truncate_for_log(&text));
            }
            writeln!(writer, "{text}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

fn truncate_for_log(text: &str) -> String {
    const MAX: usize = 400;
    if text.len() <= MAX {
        return text.to_string();
    }
    let mut cut = MAX;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… ({} bytes)", &text[..cut], text.len())
}

/// Dispatch one parsed message. Returns `None` for notifications, which must
/// never be answered.
fn handle(
    message: Incoming,
    engine: &mut Engine,
    paths: &PathGuard,
    config: &ServerConfig,
) -> Option<Value> {
    let (id, method, params) = match message {
        Incoming::Request { id, method, params } => (Some(id), method, params),
        Incoming::Notification { method, params } => (None, method, params),
    };

    // Notifications: act, but never reply.
    if id.is_none() {
        match method.as_str() {
            "notifications/initialized" | "notifications/cancelled" => {}
            _ => {
                if config.trace {
                    eprintln!("[aurum-mcp] ignoring notification: {method}");
                }
            }
        }
        return None;
    }
    let id = id.expect("checked above");

    let result = match method.as_str() {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            Ok(protocol::initialize_result(
                requested,
                env!("CARGO_PKG_VERSION"),
            ))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list_payload_with(config.read_only, &config.denied)),
        "tools/call" => call_tool(engine, paths, config, &params),
        other => Err(RpcError::method_not_found(other)),
    };

    Some(match result {
        Ok(value) => success(id, value),
        Err(error) => failure(id, &error),
    })
}

fn call_tool(
    engine: &mut Engine,
    paths: &PathGuard,
    config: &ServerConfig,
    params: &Value,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("missing 'name'"))?;

    let tool = tools::find(name)
        .ok_or_else(|| RpcError::invalid_params(format!("unknown tool: {name}")))?;

    // The status tool is never denied: a client that cannot ask what is
    // withheld cannot distinguish a refusal from a typo, and would have no way
    // to discover why the server is behaving oddly.
    if name != tools::STATUS_TOOL && config.denied.iter().any(|denied| denied == name) {
        return Err(RpcError::invalid_params(format!(
            "'{name}' is withheld by this server's deny list; call {} to see what is available",
            tools::STATUS_TOOL
        )));
    }

    if config.read_only && !tool.read_only {
        return Err(RpcError::invalid_params(format!(
            "'{name}' mutates state and this server is running read-only"
        )));
    }

    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    let mut ctx = ToolContext {
        engine,
        paths,
        editor_bridge: config.editor_bridge.as_deref(),
        read_only: config.read_only,
        denied: &config.denied,
    };
    let outcome = (tool.handler)(&mut ctx, &arguments);

    // Tool failures are results, not transport errors: the model must be able
    // to read the message and correct itself on the next call.
    Ok(match outcome {
        Ok(value) => protocol::tool_result(&value, false),
        Err(error) => protocol::tool_result(&json!(error.to_string()), true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_denied_tool_is_both_refused_and_hidden() {
        // Both halves matter. Advertising a tool that will always be refused
        // wastes a turn and reads as a broken server; refusing one that is
        // still advertised is the same failure seen from the other side.
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aurum_entity_spawn","arguments":{}}}"#,
            ],
            ServerConfig {
                read_only: false,
                trace: false,
                editor_bridge: None,
                denied: vec!["aurum_entity_spawn".to_string()],
            },
        );

        let listed = out[0]["result"]["tools"]
            .as_array()
            .expect("a tool list")
            .iter()
            .any(|tool| tool["name"] == "aurum_entity_spawn");
        assert!(!listed, "a denied tool must not be advertised");

        let message = out[1]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains("withheld"),
            "a denied call should say it was withheld, got '{message}'"
        );
    }

    #[test]
    fn the_status_tool_can_never_be_denied() {
        // A client that cannot ask what is missing cannot tell a refusal from
        // a typo, and would have no way to discover why the server is behaving
        // oddly. That is a worse failure than the one a deny list prevents.
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aurum_mcp_status","arguments":{}}}"#,
            ],
            ServerConfig {
                read_only: false,
                trace: false,
                editor_bridge: None,
                denied: vec!["aurum_mcp_status".to_string()],
            },
        );

        let listed = out[0]["result"]["tools"]
            .as_array()
            .expect("a tool list")
            .iter()
            .any(|tool| tool["name"] == "aurum_mcp_status");
        assert!(listed, "the status tool must always be advertised");
        assert!(
            out[1].get("error").is_none(),
            "the status tool must always be callable, got {:?}",
            out[1]
        );
    }

    #[test]
    fn the_status_tool_reports_the_permissions_it_is_running_under() {
        // The point of carrying the configuration into the handler: this has
        // to describe the server it is actually in, not a plausible one.
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"aurum_mcp_status","arguments":{}}}"#,
            ],
            ServerConfig {
                read_only: true,
                trace: false,
                editor_bridge: None,
                denied: vec!["aurum_entity_spawn".to_string()],
            },
        );

        let text = out[0]["result"]["content"][0]["text"]
            .as_str()
            .expect("a text payload");
        let payload: Value = serde_json::from_str(text).expect("valid JSON");

        assert_eq!(payload["read_only"], true);
        assert!(
            payload["root"]
                .as_str()
                .is_some_and(|root| !root.is_empty()),
            "the status should name the root paths are confined to"
        );
        assert!(
            payload["tools"]["withheld"].as_i64().unwrap_or(0) > 0,
            "read-only and a deny list together withhold something"
        );
        let denied = payload["tools"]["denied"]
            .as_array()
            .expect("a denied list");
        assert!(
            denied.iter().any(|name| name == "aurum_entity_spawn"),
            "the withheld tool should be named rather than merely counted"
        );
    }

    /// Drive a whole session in memory and return the response lines.
    fn run_session(requests: &[&str], config: ServerConfig) -> Vec<Value> {
        let input = requests.join("\n");
        let mut output: Vec<u8> = Vec::new();
        let mut engine = Engine::new();
        let paths = PathGuard::new(std::env::temp_dir());
        serve(
            Cursor::new(input.into_bytes()),
            &mut output,
            &mut engine,
            &paths,
            config,
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).expect("each line is valid JSON"))
            .collect()
    }

    fn init() -> &'static str {
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#
    }

    #[test]
    fn full_handshake_then_tool_call() {
        let out = run_session(
            &[
                init(),
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"aurum_entity_spawn","arguments":{}}}"#,
            ],
            ServerConfig::default(),
        );

        // The notification must not produce a line.
        assert_eq!(out.len(), 3, "expected exactly 3 responses, got {out:#?}");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "aurum-mcp");
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert!(out[1]["result"]["tools"].as_array().unwrap().len() >= 20);
        assert_eq!(out[2]["result"]["isError"], false);
        assert!(out[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("entity"));
    }

    #[test]
    fn responses_carry_the_request_id_unchanged() {
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":"abc-123","method":"ping"}"#,
                r#"{"jsonrpc":"2.0","id":99,"method":"ping"}"#,
            ],
            ServerConfig::default(),
        );
        assert_eq!(out[0]["id"], "abc-123");
        assert_eq!(out[1]["id"], 99);
        assert_eq!(out[0]["result"], json!({}));
    }

    #[test]
    fn unknown_method_is_a_protocol_error() {
        let out = run_session(
            &[r#"{"jsonrpc":"2.0","id":1,"method":"does/not/exist"}"#],
            ServerConfig::default(),
        );
        assert_eq!(out[0]["error"]["code"], protocol::codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn malformed_json_yields_a_parse_error_with_null_id() {
        let out = run_session(&["{ this is not json"], ServerConfig::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[0]["error"]["code"], protocol::codes::PARSE_ERROR);
    }

    #[test]
    fn unknown_tool_is_a_protocol_error() {
        let out = run_session(
            &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope"}}"#],
            ServerConfig::default(),
        );
        assert_eq!(out[0]["error"]["code"], protocol::codes::INVALID_PARAMS);
    }

    #[test]
    fn tool_failure_is_an_error_result_not_a_transport_error() {
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"aurum_component_get","arguments":{"entity":5}}}"#,
            ],
            ServerConfig::default(),
        );
        assert!(
            out[0].get("error").is_none(),
            "a tool failure must not be a JSON-RPC error"
        );
        assert_eq!(out[0]["result"]["isError"], true);
        assert!(out[0]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("type_name"));
    }

    #[test]
    fn read_only_mode_refuses_mutations() {
        let config = ServerConfig {
            read_only: true,
            trace: false,
            editor_bridge: None,
            denied: Vec::new(),
        };
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aurum_entity_spawn","arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"aurum_world_snapshot","arguments":{}}}"#,
            ],
            config,
        );

        let names: Vec<&str> = out[0]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(
            !names.contains(&"aurum_entity_spawn"),
            "mutating tools must be omitted in read-only mode"
        );
        assert!(names.contains(&"aurum_world_snapshot"));

        assert_eq!(out[1]["error"]["code"], protocol::codes::INVALID_PARAMS);
        assert_eq!(out[2]["result"]["isError"], false);
    }

    #[test]
    fn read_only_list_is_a_strict_subset() {
        let all = run_session(
            &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#],
            ServerConfig::default(),
        );
        let ro = run_session(
            &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#],
            ServerConfig {
                read_only: true,
                trace: false,
                editor_bridge: None,
                denied: Vec::new(),
            },
        );
        let all_n = all[0]["result"]["tools"].as_array().unwrap().len();
        let ro_n = ro[0]["result"]["tools"].as_array().unwrap().len();
        assert!(ro_n < all_n, "read-only must expose fewer tools");
        assert!(ro_n > 0);
    }

    #[test]
    fn blank_lines_are_ignored() {
        let out = run_session(
            &["", "   ", r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#],
            ServerConfig::default(),
        );
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn empty_input_terminates_cleanly() {
        assert!(run_session(&[], ServerConfig::default()).is_empty());
    }

    #[test]
    fn session_state_persists_across_calls() {
        let out = run_session(
            &[
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"aurum_entity_spawn","arguments":{}}}"#,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aurum_world_snapshot","arguments":{}}}"#,
            ],
            ServerConfig::default(),
        );
        assert!(out[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"entity_count\": 1"));
    }
}
