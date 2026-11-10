//! Bounded resolution evidence excluding raw package IDs, Cargo filesystem and
//! source URL fields. Bounded project-declared metadata strings are retained.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

const PACKAGES: usize = 4096;
const EDGES: usize = 16384;
const GRAPH_BYTES: usize = 128 * 1024;
#[cfg(any(windows, test))]
const PLAN_BYTES: usize = 256 * 1024;
// Adding one comma and the fixed JSON string field sha256 adds 76 bytes.
const HASH_FIELD_BYTES: usize = 76;
#[cfg(any(windows, test))]
const METADATA_BYTES: usize = 2 * 1024 * 1024;
const SCOPE: &str =
    "Cargo metadata workspace resolution filtered by platform; not exact build units";
const IDENTITY_SCOPE: &str = "one preparation; opaque package ID hashes are not relocation stable";

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("Cargo graph byte bound exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &Value, limit: usize) -> Result<Vec<u8>, String> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| "Cargo graph byte bound exceeded")?;
    Ok(writer.bytes)
}
fn platform(value: &Value) -> Result<&str, String> {
    platform_text(text(value, 128)?)
}
fn platform_text(value: &str) -> Result<&str, String> {
    let value = bounded_text(value, 128)?;
    if !value.contains('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || value.split('-').any(str::is_empty)
    {
        return Err("unsupported builtin-shaped Cargo platform".into());
    }
    Ok(value)
}
fn digest(value: &Value) -> Result<&str, String> {
    let value = text(value, 64)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid retained Cargo graph identity".into());
    }
    Ok(value)
}
fn fields<'a>(
    value: &'a Value,
    required: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or("invalid closed Cargo graph object")?;
    if object.len() != required.len() || required.iter().any(|field| !object.contains_key(*field)) {
        return Err("invalid closed Cargo graph fields".into());
    }
    Ok(object)
}
fn array(value: &Value, maximum: usize) -> Result<&[Value], String> {
    value
        .as_array()
        .filter(|values| values.len() <= maximum)
        .map(Vec::as_slice)
        .ok_or_else(|| "invalid retained Cargo graph count".into())
}

/// Validate the retained schema, not merely a caller-supplied digest.
fn validate(graph: &Value, hashed: bool) -> Result<(), String> {
    let mut required = vec![
        "version",
        "metadata_format",
        "platform",
        "selected",
        "scope",
        "graph_identity_scope",
        "packages",
        "edges",
        "selected_transitive",
        "cargo_filesystem_fields_returned",
        "cargo_source_urls_returned",
        "project_declared_strings_returned",
    ];
    if hashed {
        required.push("sha256");
    }
    fields(graph, &required)?;
    if graph["version"] != 1
        || graph["metadata_format"] != 1
        || graph["scope"].as_str() != Some(SCOPE)
        || graph["graph_identity_scope"].as_str() != Some(IDENTITY_SCOPE)
        || graph["cargo_filesystem_fields_returned"] != false
        || graph["cargo_source_urls_returned"] != false
        || graph["project_declared_strings_returned"] != true
    {
        return Err("invalid retained Cargo graph schema".into());
    }
    platform(&graph["platform"])?;
    let selected = digest(&graph["selected"])?;
    if hashed {
        digest(&graph["sha256"])?;
    }
    let packages = array(&graph["packages"], PACKAGES)?;
    if packages.is_empty() {
        return Err("empty retained Cargo graph".into());
    }
    let mut ids = BTreeSet::new();
    let mut previous = None;
    for package in packages {
        fields(
            package,
            &["id", "name", "version", "source_kind", "features"],
        )?;
        let id = digest(&package["id"])?;
        if previous.is_some_and(|previous| previous >= id) || !ids.insert(id) {
            return Err("noncanonical retained Cargo package identities".into());
        }
        previous = Some(id);
        text(&package["name"], 128)?;
        text(&package["version"], 128)?;
        if !matches!(
            package["source_kind"].as_str(),
            Some("local" | "registry" | "git" | "other")
        ) {
            return Err("invalid retained Cargo source kind".into());
        }
        let mut previous_feature = None;
        for feature in array(&package["features"], 256)? {
            let name = text(feature, 256)?;
            if previous_feature.is_some_and(|previous| previous >= name) {
                return Err("noncanonical retained Cargo features".into());
            }
            previous_feature = Some(name);
        }
    }
    if !ids.contains(selected) {
        return Err("retained Cargo selected package is absent".into());
    }
    let mut adjacency: BTreeMap<&str, BTreeSet<&str>> =
        ids.iter().map(|id| (*id, BTreeSet::new())).collect();
    let mut previous_edge = None;
    for edge in array(&graph["edges"], EDGES)? {
        fields(edge, &["from", "to", "name", "kinds"])?;
        let from = digest(&edge["from"])?;
        let to = digest(&edge["to"])?;
        let name = text(&edge["name"], 128)?;
        if !ids.contains(from) || !ids.contains(to) {
            return Err("unknown retained Cargo edge identity".into());
        }
        let key = (from, name, to);
        if previous_edge.is_some_and(|previous| previous >= key) {
            return Err("noncanonical retained Cargo edges".into());
        }
        previous_edge = Some(key);
        let kinds = array(&edge["kinds"], 64)?;
        if kinds.is_empty() {
            return Err("empty retained Cargo dependency kinds".into());
        }
        let mut previous_kind = None;
        for kind in kinds {
            fields(kind, &["kind", "target"])?;
            let name = text(&kind["kind"], 6)?;
            if !matches!(name, "normal" | "dev" | "build") {
                return Err("unsupported retained Cargo dependency kind".into());
            }
            let target = match &kind["target"] {
                Value::Null => None,
                value => Some(text(value, 1024)?),
            };
            let key = (name, target);
            if previous_kind.is_some_and(|previous| previous >= key) {
                return Err("noncanonical retained Cargo dependency kinds".into());
            }
            previous_kind = Some(key);
        }
        adjacency
            .get_mut(from)
            .ok_or("missing Cargo adjacency")?
            .insert(to);
    }
    let mut transitive = BTreeSet::new();
    let mut previous_transitive = None;
    for value in array(&graph["selected_transitive"], PACKAGES)? {
        let id = digest(value)?;
        if !ids.contains(id) || previous_transitive.is_some_and(|previous| previous >= id) {
            return Err("invalid retained Cargo transitive identities".into());
        }
        previous_transitive = Some(id);
        transitive.insert(id);
    }
    let mut reachable = BTreeSet::new();
    let mut pending = vec![selected];
    while let Some(id) = pending.pop() {
        if reachable.insert(id) {
            pending.extend(
                adjacency
                    .get(id)
                    .ok_or("missing Cargo adjacency")?
                    .iter()
                    .copied(),
            );
        }
    }
    if reachable != transitive {
        return Err("retained Cargo selected reachability disagrees".into());
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn admit_bytes(used: &mut usize, value: &Value) -> Result<(), String> {
    let remaining = GRAPH_BYTES.saturating_sub(*used);
    *used += encode(value, remaining)?.len();
    Ok(())
}

fn text(value: &Value, maximum: usize) -> Result<&str, String> {
    bounded_text(
        value.as_str().ok_or("invalid bounded Cargo graph field")?,
        maximum,
    )
}
fn bounded_text(value: &str, maximum: usize) -> Result<&str, String> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err("invalid bounded Cargo graph field".into());
    }
    Ok(value)
}

/// This is a filtered workspace resolution graph, not the exact compilation
/// unit graph. Cargo metadata can include dev edges and unify workspace features.
#[cfg(any(windows, test))]
pub(crate) fn capture(metadata: &Value, selected: &str, host: &str) -> Result<Value, String> {
    encode(metadata, METADATA_BYTES)?;
    if metadata["version"] != 1 {
        return Err("unsupported Cargo graph metadata version".into());
    }
    let platform = platform_text(host)?.to_string();
    bounded_text(selected, 4096)?;
    let packages = metadata["packages"]
        .as_array()
        .filter(|p| !p.is_empty() && p.len() <= PACKAGES)
        .ok_or("Cargo graph package bound exceeded")?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .filter(|p| p.len() == packages.len())
        .ok_or("complete Cargo resolution graph is required")?;
    let mut identities = BTreeMap::new();
    let mut rows = BTreeMap::new();
    let mut used = 0;
    for package in packages {
        let id = text(&package["id"], 4096)?;
        let name = text(&package["name"], 128)?;
        let version = text(&package["version"], 128)?;
        // IDs are opaque Cargo identities. Do not parse or return embedded URLs.
        let key = crate::sha256_hex(id.as_bytes());
        if identities.insert(id.to_string(), key.clone()).is_some() {
            return Err("duplicate Cargo package identity".into());
        }
        let source_kind = match package
            .get("source")
            .ok_or("missing explicit Cargo source field")?
        {
            Value::Null => "local",
            Value::String(source)
                if !source.is_empty()
                    && source.len() <= 4096
                    && !source.chars().any(char::is_control) =>
            {
                match source.split_once('+').map(|(kind, _)| kind) {
                    Some("registry" | "sparse") => "registry",
                    Some("git") => "git",
                    _ => "other",
                }
            }
            _ => return Err("invalid Cargo source identity".into()),
        };
        let row = json!({"id":key,"name":name,"version":version,"source_kind":source_kind});
        admit_bytes(&mut used, &row)?;
        rows.insert(key.clone(), row);
    }
    let selected = identities
        .get(selected)
        .ok_or("selected package is absent from Cargo graph")?
        .clone();
    let mut edges = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut adjacency = BTreeMap::new();
    let mut edge_count = 0;
    for node in nodes {
        let original = text(&node["id"], 4096)?;
        let id = identities
            .get(original)
            .ok_or("unknown Cargo resolution node")?;
        if !seen.insert(id.clone()) {
            return Err("duplicate Cargo resolution node".into());
        }
        let features = node["features"]
            .as_array()
            .filter(|f| f.len() <= 256)
            .ok_or("invalid Cargo feature bound")?;
        let mut feature_names = BTreeSet::new();
        for feature in features {
            if !feature_names.insert(text(feature, 256)?.to_string()) {
                return Err("duplicate Cargo resolved feature".into());
            }
        }
        let feature_names = json!(feature_names);
        admit_bytes(&mut used, &feature_names)?;
        rows.get_mut(id).ok_or("missing Cargo package row")?["features"] = feature_names;
        let dependencies = node["deps"]
            .as_array()
            .filter(|d| d.len() <= PACKAGES)
            .ok_or("invalid Cargo dependency bound")?;
        let listed = node["dependencies"]
            .as_array()
            .filter(|d| d.len() <= PACKAGES)
            .ok_or("invalid Cargo dependency identities")?;
        let mut expected = BTreeSet::new();
        for dependency in listed {
            let identity = text(dependency, 4096)?;
            if !identities.contains_key(identity) || !expected.insert(identity.to_string()) {
                return Err("unknown or duplicate Cargo dependency identity".into());
            }
        }
        let mut observed = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for dependency in dependencies {
            edge_count += 1;
            if edge_count > EDGES {
                return Err("Cargo graph edge bound exceeded".into());
            }
            let original_target = text(&dependency["pkg"], 4096)?;
            let target = identities
                .get(original_target)
                .ok_or("unknown Cargo dependency target")?;
            let alias = text(&dependency["name"], 128)?;
            let kinds = dependency["dep_kinds"]
                .as_array()
                .filter(|k| !k.is_empty() && k.len() <= 64)
                .ok_or("invalid Cargo dependency kinds")?;
            let mut kind_rows = BTreeMap::new();
            for kind in kinds {
                let kind_name = match kind
                    .get("kind")
                    .ok_or("missing explicit Cargo dependency kind")?
                {
                    Value::Null => "normal",
                    Value::String(name) if matches!(name.as_str(), "build" | "dev") => name,
                    _ => return Err("unsupported Cargo dependency kind".into()),
                };
                let target = match kind
                    .get("target")
                    .ok_or("missing explicit Cargo dependency target")?
                {
                    Value::Null => None,
                    value => Some(text(value, 1024)?.to_string()),
                };
                if kind_rows
                    .insert(
                        (kind_name.to_string(), target.clone()),
                        json!({"kind":kind_name,"target":target}),
                    )
                    .is_some()
                {
                    return Err("duplicate Cargo dependency kind".into());
                }
            }
            let key = (id.clone(), alias.to_string(), target.clone());
            let edge = json!({"from":id,"to":target,"name":alias,"kinds":kind_rows.into_values().collect::<Vec<_>>()});
            admit_bytes(&mut used, &edge)?;
            if edges.insert(key, edge).is_some() {
                return Err("duplicate Cargo dependency edge".into());
            }
            observed.insert(original_target.to_string());
            targets.insert(target.clone());
        }
        if expected != observed {
            return Err("Cargo dependency identities disagree".into());
        }
        adjacency.insert(id.clone(), targets);
    }
    let mut reachable = BTreeSet::new();
    let mut pending = vec![selected.clone()];
    while let Some(id) = pending.pop() {
        if reachable.insert(id.clone()) {
            pending.extend(
                adjacency
                    .get(&id)
                    .ok_or("incomplete Cargo node coverage")?
                    .iter()
                    .cloned(),
            );
        }
    }
    let mut graph = json!({"version":1,"metadata_format":1,"platform":platform,"selected":selected,
        "scope":SCOPE,"graph_identity_scope":IDENTITY_SCOPE,
        "packages":rows.into_values().collect::<Vec<_>>(),"edges":edges.into_values().collect::<Vec<_>>(),
        "selected_transitive":reachable,"cargo_source_urls_returned":false,
        "cargo_filesystem_fields_returned":false,"project_declared_strings_returned":true});
    validate(&graph, false)?;
    let encoded = encode(&graph, GRAPH_BYTES.saturating_sub(HASH_FIELD_BYTES))?;
    graph["sha256"] = json!(crate::sha256_hex(&encoded));
    Ok(graph)
}

pub(crate) fn summary(graph: &Value) -> Result<Value, String> {
    validate(graph, true)?;
    // Enforce the encoded bound before cloning untrusted retained evidence.
    encode(graph, GRAPH_BYTES)?;
    let hash = digest(&graph["sha256"])?;
    let mut verified = graph.clone();
    verified
        .as_object_mut()
        .ok_or("invalid retained Cargo graph")?
        .remove("sha256");
    if crate::sha256_hex(&encode(
        &verified,
        GRAPH_BYTES.saturating_sub(HASH_FIELD_BYTES),
    )?) != hash
    {
        return Err("retained Cargo graph identity mismatch".into());
    }
    Ok(
        json!({"retained":true,"sha256":hash,"scope":SCOPE,"platform":graph["platform"],"graph_identity_scope":IDENTITY_SCOPE,
        "packages":graph["packages"].as_array().ok_or("missing retained Cargo packages")?.len(),
        "edges":graph["edges"].as_array().ok_or("missing retained Cargo edges")?.len(),
        "selected_transitive_packages":graph["selected_transitive"].as_array().ok_or("missing retained transitive graph")?.len(),
        "full_graph_returned":false,"exact_build_units_verified":false}),
    )
}

/// Retain the existing public plan shape while excluding the full private graph.
/// Only the validated bounded summary enters public output. Its digest copy is
/// made after validation and bounded encoding, never from an oversized graph.
#[cfg(any(windows, test))]
pub(crate) fn public_plan(plan: &Value) -> Value {
    fn public(plan: &Value) -> Result<Value, String> {
        let object = plan.as_object().ok_or("invalid public native plan")?;
        if object.len() > 32 {
            return Err("public native plan field bound exceeded".into());
        }
        let mut output = serde_json::Map::new();
        let mut used = 0usize;
        for (key, value) in object {
            bounded_text(key, 64)?;
            let copied = if key == "compiled"
                && value
                    .as_object()
                    .is_some_and(|row| row.contains_key("cargo_graph"))
            {
                let compiled = value
                    .as_object()
                    .ok_or("invalid public compile provenance")?;
                if compiled.len() > 16 {
                    return Err("public compile provenance field bound exceeded".into());
                }
                let mut copied = serde_json::Map::new();
                for (name, field) in compiled {
                    bounded_text(name, 64)?;
                    let field = if name == "cargo_graph" {
                        summary(field).unwrap_or_else(|_| json!({"retained":false,"reason":"invalid_retained_cargo_graph","full_graph_returned":false}))
                    } else {
                        let encoded = encode(field, PLAN_BYTES.saturating_sub(used))?;
                        used += encoded.len();
                        field.clone()
                    };
                    copied.insert(name.clone(), field);
                }
                Value::Object(copied)
            } else {
                let encoded = encode(value, PLAN_BYTES.saturating_sub(used))?;
                used += encoded.len();
                value.clone()
            };
            output.insert(key.clone(), copied);
        }
        let output = Value::Object(output);
        encode(&output, PLAN_BYTES)?;
        Ok(output)
    }
    if plan.is_null() {
        return Value::Null;
    }
    public(plan)
        .unwrap_or_else(|_| json!({"ok":false,"reason":"invalid_bounded_public_native_plan"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"version":1,"packages":[
            {"id":"path+file:///A:/PRIVATE/app#app@1.0.0","name":"app","version":"1.0.0","source":null},
            {"id":"registry+https://PRIVATE-TOKEN@example.invalid#dep@2.0.0","name":"dep","version":"2.0.0","source":"registry+https://PRIVATE-TOKEN@example.invalid"},
            {"id":"third","name":"third","version":"3.0.0","source":null}
        ],"resolve":{"nodes":[
            {"id":"path+file:///A:/PRIVATE/app#app@1.0.0","features":["default"],"dependencies":["registry+https://PRIVATE-TOKEN@example.invalid#dep@2.0.0"],"deps":[{"name":"renamed","pkg":"registry+https://PRIVATE-TOKEN@example.invalid#dep@2.0.0","dep_kinds":[{"kind":"build","target":"cfg(windows)"}]}]},
            {"id":"registry+https://PRIVATE-TOKEN@example.invalid#dep@2.0.0","features":[],"dependencies":["third"],"deps":[{"name":"third","pkg":"third","dep_kinds":[{"kind":null,"target":null}]}]},
            {"id":"third","features":[],"dependencies":[],"deps":[]}
        ]}})
    }
    fn run(value: &Value) -> Result<Value, String> {
        capture(
            value,
            "path+file:///A:/PRIVATE/app#app@1.0.0",
            "x86_64-pc-windows-msvc",
        )
    }
    #[test]
    fn transitive_alias_kind_features_and_privacy() {
        let graph = run(&fixture()).unwrap();
        let report = summary(&graph).unwrap();
        assert_eq!(report["packages"], 3);
        assert_eq!(report["edges"], 2);
        assert_eq!(report["selected_transitive_packages"], 3);
        let text = graph.to_string();
        assert!(!text.contains("PRIVATE"));
        assert!(!text.contains("example.invalid"));
        assert!(text.contains("renamed"));
        assert!(text.contains("cfg(windows)"));
        assert!(text.contains("default"));
    }
    #[test]
    fn reordered_metadata_preserves_graph_identity() {
        let first = run(&fixture()).unwrap();
        let mut second = fixture();
        second["packages"].as_array_mut().unwrap().reverse();
        second["resolve"]["nodes"].as_array_mut().unwrap().reverse();
        assert_eq!(first, run(&second).unwrap());
    }
    #[test]
    fn null_or_incomplete_resolution_refuses() {
        let mut value = fixture();
        value["resolve"] = Value::Null;
        assert!(run(&value).is_err());
        let mut value = fixture();
        value["resolve"]["nodes"].as_array_mut().unwrap().pop();
        assert!(run(&value).is_err());
    }
    #[test]
    fn duplicate_packages_nodes_edges_and_kinds_refuse() {
        let mut value = fixture();
        value["packages"][2] = value["packages"][0].clone();
        assert!(run(&value).is_err());
        let mut value = fixture();
        value["resolve"]["nodes"][2] = value["resolve"]["nodes"][0].clone();
        assert!(run(&value).is_err());
        let mut value = fixture();
        let edge = value["resolve"]["nodes"][0]["deps"][0].clone();
        value["resolve"]["nodes"][0]["deps"]
            .as_array_mut()
            .unwrap()
            .push(edge);
        assert!(run(&value).is_err());
        let mut value = fixture();
        let kind = value["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0].clone();
        value["resolve"]["nodes"][0]["deps"][0]["dep_kinds"]
            .as_array_mut()
            .unwrap()
            .push(kind);
        assert!(run(&value).is_err());
    }
    #[test]
    fn unknown_dependencies_and_disagreement_refuse() {
        let mut value = fixture();
        value["resolve"]["nodes"][0]["deps"][0]["pkg"] = json!("missing");
        assert!(run(&value).is_err());
        let mut value = fixture();
        value["resolve"]["nodes"][0]["dependencies"] = json!([]);
        assert!(run(&value).is_err());
        assert!(capture(&fixture(), "missing", "x86_64-pc-windows-msvc").is_err());
    }
    #[test]
    fn modified_retained_graph_refuses_identity() {
        let mut graph = run(&fixture()).unwrap();
        graph["platform"] = json!("different");
        assert!(summary(&graph).is_err());
    }
    #[test]
    fn cycles_are_bounded_and_reachable() {
        let mut value = fixture();
        let app = value["packages"][0]["id"].clone();
        value["resolve"]["nodes"][2]["dependencies"] = json!([app]);
        value["resolve"]["nodes"][2]["deps"] =
            json!([{"name":"app","pkg":app,"dep_kinds":[{"kind":null,"target":null}]}]);
        assert_eq!(
            run(&value).unwrap()["selected_transitive"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }
    #[test]
    fn oversized_and_control_fields_refuse() {
        let mut value = fixture();
        value["packages"][0]["name"] = json!("a".repeat(129));
        assert!(run(&value).is_err());
        let mut value = fixture();
        value["resolve"]["nodes"][0]["features"] = json!(["secret\nbody"]);
        assert!(run(&value).is_err());
        let mut value = fixture();
        value["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0]["kind"] = json!("future");
        assert!(run(&value).is_err());
        let mut value = fixture();
        value["packages"] = json!(vec![value["packages"][0].clone(); PACKAGES + 1]);
        assert!(run(&value).is_err());
    }

    fn rehash(graph: &mut Value) {
        graph.as_object_mut().unwrap().remove("sha256");
        let bytes = encode(graph, GRAPH_BYTES * 2).unwrap();
        graph["sha256"] = json!(crate::sha256_hex(&bytes));
    }

    #[test]
    fn explicit_nullable_cargo_fields_are_required() {
        assert!(run(&fixture()).is_ok());
        let mut value = fixture();
        value["packages"][0]
            .as_object_mut()
            .unwrap()
            .remove("source");
        assert!(run(&value).is_err());
        for field in ["kind", "target"] {
            let mut value = fixture();
            value["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(run(&value).is_err());
        }
    }

    #[test]
    fn host_platform_paths_empty_components_and_controls_refuse() {
        let value = fixture();
        let selected = value["packages"][0]["id"].as_str().unwrap();
        for invalid in [
            "A:/private/host.json",
            "x86_64\\pc-windows",
            "-windows",
            "x86_64--windows",
            "x86_64",
            "x86_64-pc\n-windows",
        ] {
            assert!(capture(&value, selected, invalid).is_err(), "{invalid:?}");
        }
        assert!(capture(&value, selected, "wasm32-wasip1").is_ok());
        assert!(capture(&value, &"x".repeat(4097), "x86_64-pc-windows-msvc").is_err());
    }

    #[test]
    fn project_declared_predicates_are_retained_without_blanket_path_claims() {
        let mut value = fixture();
        let predicate = "cfg(any(windows, marker = \"A:/PRIVATE/setting\"))";
        value["resolve"]["nodes"][0]["deps"][0]["dep_kinds"][0]["target"] = json!(predicate);
        let graph = run(&value).unwrap();
        assert!(graph.to_string().contains("PRIVATE"));
        assert_eq!(graph["cargo_filesystem_fields_returned"], false);
        assert_eq!(graph["cargo_source_urls_returned"], false);
        assert_eq!(graph["project_declared_strings_returned"], true);
        assert!(graph.get("host_paths_returned").is_none());
        assert!(graph.get("raw_source_urls_returned").is_none());
        assert!(!summary(&graph).unwrap().to_string().contains("PRIVATE"));
    }

    #[test]
    fn multiple_versions_dependency_kinds_and_all_input_orderings_are_preserved() {
        let mut value = fixture();
        let dep = "registry+https://example.invalid#dep@4.0.0";
        value["packages"].as_array_mut().unwrap().push(json!({"id":dep,"name":"dep","version":"4.0.0","source":"registry+https://example.invalid"}));
        value["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":dep,"features":["beta","alpha"],"dependencies":[],"deps":[]}));
        value["resolve"]["nodes"][0]["features"] = json!(["beta", "alpha", "default"]);
        value["resolve"]["nodes"][0]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(json!(dep));
        value["resolve"]["nodes"][0]["deps"].as_array_mut().unwrap().push(json!({"name":"other_version","pkg":dep,"dep_kinds":[{"kind":"dev","target":"cfg(windows)"},{"kind":null,"target":null},{"kind":"build","target":null}]}));
        let first = run(&value).unwrap();
        let versions: BTreeSet<_> = first["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|package| package["name"] == "dep")
            .map(|package| package["version"].as_str().unwrap())
            .collect();
        assert_eq!(versions, BTreeSet::from(["2.0.0", "4.0.0"]));
        let edge = first["edges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|edge| edge["name"] == "other_version")
            .unwrap();
        assert_eq!(
            edge["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|kind| kind["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["build", "dev", "normal"]
        );
        value["packages"].as_array_mut().unwrap().reverse();
        value["resolve"]["nodes"].as_array_mut().unwrap().reverse();
        for node in value["resolve"]["nodes"].as_array_mut().unwrap() {
            node["features"].as_array_mut().unwrap().reverse();
            node["dependencies"].as_array_mut().unwrap().reverse();
            node["deps"].as_array_mut().unwrap().reverse();
            for edge in node["deps"].as_array_mut().unwrap() {
                edge["dep_kinds"].as_array_mut().unwrap().reverse();
            }
        }
        assert_eq!(first, run(&value).unwrap());
    }

    #[test]
    fn opaque_local_ids_bind_one_preparation_without_returning_the_path() {
        let first = run(&fixture()).unwrap();
        let mut value = fixture();
        let selected = "path+file:///A:/OTHER-TEMP/app#app@1.0.0";
        value["packages"][0]["id"] = json!(selected);
        value["resolve"]["nodes"][0]["id"] = json!(selected);
        let second = capture(&value, selected, "x86_64-pc-windows-msvc").unwrap();
        assert_ne!(first["sha256"], second["sha256"]);
        assert_eq!(second["graph_identity_scope"], IDENTITY_SCOPE);
        assert!(!second.to_string().contains("OTHER-TEMP"));
    }

    #[test]
    fn self_rehashed_wrong_schema_types_edges_and_reachability_refuse() {
        let graph = run(&fixture()).unwrap();
        let cases = [
            ("version", json!(2)),
            ("scope", json!("PRIVATE output body")),
            ("platform", json!("A:/PRIVATE/platform.json")),
            ("packages", json!(false)),
            ("selected", json!("a".repeat(64))),
            ("selected_transitive", json!([])),
            ("cargo_source_urls_returned", json!(true)),
        ];
        for (field, value) in cases {
            let mut malformed = graph.clone();
            malformed[field] = value;
            rehash(&mut malformed);
            assert!(summary(&malformed).is_err(), "{field}");
        }
        let mut malformed = graph.clone();
        malformed["edges"][0]["to"] = json!("a".repeat(64));
        rehash(&mut malformed);
        assert!(summary(&malformed).is_err());
        let mut malformed = graph.clone();
        malformed["packages"][0]["body"] = json!("PRIVATE");
        rehash(&mut malformed);
        assert!(summary(&malformed).is_err());
        let mut malformed = graph.clone();
        malformed["packages"].as_array_mut().unwrap().reverse();
        rehash(&mut malformed);
        assert!(summary(&malformed).is_err());
    }

    fn graph_at_limit() -> Value {
        let mut graph = run(&fixture()).unwrap();
        graph.as_object_mut().unwrap().remove("sha256");
        for package in graph["packages"].as_array_mut().unwrap().iter_mut().take(2) {
            package["features"] = json!((0..256)
                .map(|index| format!("f{index:04}"))
                .collect::<Vec<_>>());
        }
        let current = encode(&graph, GRAPH_BYTES).unwrap().len();
        let mut needed = GRAPH_BYTES - HASH_FIELD_BYTES - current;
        for package in graph["packages"].as_array_mut().unwrap().iter_mut().take(2) {
            for feature in package["features"].as_array_mut().unwrap() {
                let padding = needed.min(251);
                *feature = json!(format!(
                    "{}{}",
                    feature.as_str().unwrap(),
                    "x".repeat(padding)
                ));
                needed -= padding;
            }
        }
        assert_eq!(needed, 0);
        rehash(&mut graph);
        graph
    }

    #[test]
    fn exact_graph_byte_bound_passes_and_one_more_byte_refuses() {
        let mut graph = graph_at_limit();
        assert_eq!(encode(&graph, GRAPH_BYTES).unwrap().len(), GRAPH_BYTES);
        assert!(summary(&graph).is_ok());
        let feature = graph["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .take(2)
            .flat_map(|package| package["features"].as_array_mut().unwrap())
            .find(|feature| feature.as_str().unwrap().len() < 256)
            .unwrap();
        *feature = json!(format!("{}x", feature.as_str().unwrap()));
        rehash(&mut graph);
        assert_eq!(
            encode(&graph, GRAPH_BYTES + 1).unwrap().len(),
            GRAPH_BYTES + 1
        );
        assert!(summary(&graph).is_err());
    }

    #[test]
    fn bounded_writer_counts_json_escaping_without_exceeding_its_buffer() {
        assert_eq!(
            encode(&json!("x".repeat(GRAPH_BYTES - 2)), GRAPH_BYTES)
                .unwrap()
                .len(),
            GRAPH_BYTES
        );
        assert!(encode(&json!("x".repeat(GRAPH_BYTES - 1)), GRAPH_BYTES).is_err());
        assert!(encode(&json!("\n".repeat(GRAPH_BYTES / 2)), GRAPH_BYTES).is_err());
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            limit: 3,
        };
        writer.write_all(b"12").unwrap();
        assert!(writer.write_all(b"34").is_err());
        assert_eq!(writer.bytes, b"12");
    }

    #[test]
    fn public_plan_excludes_full_graph_and_private_payload_even_on_invalid_graph() {
        let mut value = fixture();
        value["resolve"]["nodes"][0]["features"] = json!(["PRIVATE-GRAPH-PAYLOAD-SENTINEL"]);
        let graph = run(&value).unwrap();
        let mut plan = json!({"ok":true,"native":true,"manifests":[{"library":"legacy.dll"}],"compiled":{"package":"app","offline":true,"cargo_graph":graph}});
        let output = public_plan(&plan);
        assert_eq!(output["manifests"], plan["manifests"]);
        assert_eq!(output["compiled"]["package"], "app");
        assert_eq!(output["compiled"]["cargo_graph"]["retained"], true);
        assert_eq!(
            output["compiled"]["cargo_graph"]["full_graph_returned"],
            false
        );
        let encoded = output.to_string();
        assert!(!encoded.contains("PRIVATE-GRAPH-PAYLOAD-SENTINEL"));
        assert!(!encoded.contains("selected_transitive\""));
        assert!(!encoded.contains("\"edges\":["));
        plan["compiled"]["cargo_graph"]["untrusted_body"] =
            json!("PRIVATE-GRAPH-PAYLOAD-SENTINEL".repeat(6000));
        let output = public_plan(&plan);
        assert_eq!(output["compiled"]["cargo_graph"]["retained"], false);
        assert_eq!(
            output["compiled"]["cargo_graph"]["reason"],
            "invalid_retained_cargo_graph"
        );
        assert!(!output
            .to_string()
            .contains("PRIVATE-GRAPH-PAYLOAD-SENTINEL"));
    }

    #[test]
    fn public_plan_preserves_legacy_shape_and_bounds_other_fields() {
        let legacy = json!({"ok":true,"native":true,"manifests":[],"compiled":{"package":"app","offline":true}});
        assert_eq!(public_plan(&legacy), legacy);
        assert_eq!(public_plan(&Value::Null), Value::Null);
        let oversized = json!({"ok":true,"payload":"PRIVATE".repeat(PLAN_BYTES)});
        let result = public_plan(&oversized);
        assert_eq!(result["ok"], false);
        assert!(!result.to_string().contains("PRIVATE"));
    }
}
