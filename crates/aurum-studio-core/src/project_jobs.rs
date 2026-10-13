//! Shared, capability-scoped background project operations.
use crate::jobs::JobOwner;
use crate::native_operations::NativeRuntime;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const OPERATIONS: &[&str] = &[
    "job_submit",
    "job_status",
    "job_result",
    "job_list",
    "job_cancel",
    "job_forget",
    "job_reconnect",
];
pub fn supported(op: &str) -> bool {
    OPERATIONS.contains(&op)
}
pub fn is_read_only(op: &str) -> bool {
    matches!(op, "job_status" | "job_result" | "job_list")
}

fn text<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} must be a string"))
}
fn owner(input: &Value, new_allowed: bool) -> Result<JobOwner, String> {
    match input.get("owner_capability") {
        Some(value) => {
            JobOwner::from_capability(value.as_str().ok_or("owner_capability must be a string")?)
        }
        None if new_allowed => JobOwner::new(),
        None => Err("owner_capability is required for an existing job".into()),
    }
}
fn fields(input: &Value, op: &str) -> Result<(), String> {
    let allowed: &[&str] = match op {
        "job_submit" => &[
            "op",
            "project",
            "owner_capability",
            "request",
            "timeout_seconds",
        ],
        "job_status" => &[
            "op",
            "project",
            "owner_capability",
            "job_id",
            "after_sequence",
        ],
        "job_result" | "job_cancel" | "job_forget" => {
            &["op", "project", "owner_capability", "job_id"]
        }
        "job_list" | "job_reconnect" => &["op", "project", "owner_capability", "limit"],
        _ => return Err("unknown job operation".into()),
    };
    if input
        .as_object()
        .ok_or("job request must be an object")?
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err("job request contains an unsupported field".into());
    }
    Ok(())
}
fn limit(input: &Value) -> Result<usize, String> {
    let value = input
        .get("limit")
        .map(|value| value.as_u64().ok_or("job limit must be an integer"))
        .transpose()?
        .unwrap_or(20);
    if !(1..=64).contains(&value) {
        return Err("job limit must be between 1 and 64".into());
    }
    Ok(value as usize)
}
fn precheck(request: &Value, read_only: bool) -> Result<(), String> {
    let op = text(request, "op")?;
    if !crate::project_ops::OPERATIONS.contains(&op) || supported(op) || op == "native_host_stop" {
        return Err("nested job operation is not supported".into());
    }
    if request.get("project").is_some() {
        return Err("a job cannot change its project scope".into());
    }
    if read_only && !crate::project_ops::is_read_only(op) {
        return Err("nested job operation requires write permission".into());
    }
    Ok(())
}

fn validate_query(input: &Value, op: &str) -> Result<(), String> {
    if matches!(
        op,
        "job_status" | "job_result" | "job_cancel" | "job_forget"
    ) {
        let id = text(input, "job_id")?;
        if id.len() != 32
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid job identity".into());
        }
    }
    if op == "job_status"
        && input
            .get("after_sequence")
            .is_some_and(|value| value.as_u64().is_none())
    {
        return Err("after_sequence must be an integer".into());
    }
    if matches!(op, "job_list" | "job_reconnect") {
        limit(input)?;
    }
    Ok(())
}

pub(crate) fn preflight(input: &Value, read_only: bool) -> Result<(), String> {
    let op = text(input, "op")?;
    fields(input, op)?;
    if read_only && !is_read_only(op) {
        return Err(format!("{op} requires write permission"));
    }
    validate_query(input, op)?;
    if op == "job_submit" {
        let request = input
            .get("request")
            .filter(|value| value.is_object())
            .ok_or("request must be a project operation object")?;
        crate::jobs::validate_request_size(request)?;
        precheck(request, read_only)?;
        let seconds = input
            .get("timeout_seconds")
            .map(|value| value.as_u64().ok_or("timeout_seconds must be an integer"))
            .transpose()?
            .unwrap_or(120);
        if !(1..=600).contains(&seconds) {
            return Err("job timeout must be between 1 and 600 seconds".into());
        }
        if let Some(capability) = input.get("owner_capability") {
            JobOwner::from_capability(
                capability
                    .as_str()
                    .ok_or("owner_capability must be a string")?,
            )?;
        }
    } else {
        owner(input, false)?;
    }
    Ok(())
}

pub fn execute(
    root: &Path,
    input: &Value,
    read_only: bool,
    runtime: Arc<NativeRuntime>,
) -> Result<Value, String> {
    preflight(input, read_only)?;
    let op = text(input, "op")?;
    if !runtime.is_controller_for(root) {
        return crate::native_host::route(root, input);
    }
    let owner = owner(input, op == "job_submit")?;
    let create = matches!(op, "job_submit" | "job_reconnect");
    let Some(manager) = runtime.job_manager(root, create)? else {
        return Ok(
            json!({"ok":true,"host_available":true,"jobs_available":false,"reconnect_required":true}),
        );
    };
    match op {
        "job_submit" => {
            let request = input["request"].clone();
            let seconds = input
                .get("timeout_seconds")
                .and_then(Value::as_u64)
                .unwrap_or(120);
            let root = root.to_path_buf();
            let runner = Arc::clone(&runtime);
            let ticket = manager.submit(
                &owner,
                request,
                read_only,
                Duration::from_secs(seconds),
                precheck,
                move |request| {
                    crate::project_ops::execute_in_service(
                        &root,
                        &request,
                        read_only,
                        Arc::clone(&runner),
                    )
                },
            )?;
            Ok(
                json!({"ok":true,"submitted":true,"job_id":ticket.id,"owner_capability":ticket.owner_capability(),
                "permissions":"project operation permission checked before admission","deadline_includes_queue":true}),
            )
        }
        "job_list" | "job_reconnect" => manager.list(&owner, limit(input)?),
        "job_status" => {
            let sequence = input
                .get("after_sequence")
                .map(|value| value.as_u64().ok_or("after_sequence must be an integer"))
                .transpose()?;
            let mut value = manager.status(text(input, "job_id")?, &owner, sequence)?;
            if let Some(object) = value.as_object_mut() {
                object.remove("result");
            }
            Ok(value)
        }
        "job_result" => manager.status(text(input, "job_id")?, &owner, None),
        "job_cancel" => manager.cancel(text(input, "job_id")?, &owner),
        "job_forget" => manager.forget(text(input, "job_id")?, &owner),
        _ => Err("unknown job operation".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_queries_are_refused_before_controller_discovery() {
        assert!(validate_query(&json!({"job_id":"../other"}), "job_status").is_err());
        assert!(validate_query(
            &json!({"job_id":"0".repeat(32),"after_sequence":-1}),
            "job_status"
        )
        .is_err());
        assert!(validate_query(&json!({"limit":0}), "job_reconnect").is_err());
        assert!(validate_query(&json!({"limit":65}), "job_list").is_err());
        assert!(validate_query(
            &json!({"job_id":"0".repeat(32),"after_sequence":1}),
            "job_status"
        )
        .is_ok());
    }
    #[test]
    fn synchronous_routing_rejects_malformed_reconnect_without_starting_a_host() {
        let root =
            std::env::temp_dir().join(format!("aurum-job-route-{}", crate::random::session_id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("aurum.toml"),
            crate::ProjectConfig::default().to_toml(),
        )
        .unwrap();
        for input in [
            json!({"op":"job_reconnect","owner_capability":"0".repeat(64),"limit":0}),
            json!({"op":"job_status","owner_capability":"0".repeat(64),"job_id":"bad"}),
            json!({"op":"job_submit","request":{"op":"job_list"}}),
            json!({"op":"job_submit","request":{"op":"write","path":"main.gd","text":"x".repeat(crate::jobs::MAX_REQUEST_BYTES)}}),
        ] {
            assert!(crate::project_ops::execute(&root, &input, false).is_err());
            assert!(!root.join(".aurum").exists());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn admission_does_not_allow_nested_jobs_or_scope_changes() {
        assert!(precheck(&json!({"op":"job_submit","request":{"op":"status"}}), false).is_err());
        assert!(precheck(&json!({"op":"native_host_stop"}), false).is_err());
        assert!(precheck(
            &json!({"op":"read","project":"other","path":"main.gd"}),
            false
        )
        .is_err());
        assert!(precheck(&json!({"op":"write","path":"main.gd"}), true).is_err());
        assert!(precheck(&json!({"op":"status"}), true).is_ok());
    }
    #[test]
    fn wire_bounds_and_capabilities_are_explicit() {
        assert!(owner(&json!({}), false).is_err());
        assert!(owner(&json!({"owner_capability":"guessed"}), false).is_err());
        assert!(fields(
            &json!({"op":"job_list","request":{"op":"write"}}),
            "job_list"
        )
        .is_err());
        assert!(limit(&json!({"limit":0})).is_err());
        assert!(limit(&json!({"limit":65})).is_err());
        assert_eq!(limit(&json!({})).unwrap(), 20);
    }
}
