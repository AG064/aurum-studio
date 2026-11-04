//! Private local compilation with retained cleared workers; no sandbox or activation.
use crate::candidate_workspace::{Workspace, WORKSPACE_BYTES};
use crate::external_workspace::{Budget, CompilationLease, StagedSources};
use crate::operation_context::OperationContext;
use crate::worker_windows::binary::{StreamLimits, StreamReceipt};
use crate::worker_windows::{BinarySpawn, StreamController, Worker};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) struct ParentDeadline {
    deadline: Instant,
    original_context: Option<OperationContext>,
    origin: NativeParentOrigin,
    read_owner: Option<NativeReadOwner>,
    source_limits: Option<crate::external_workspace::ProposalLimits>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeParentOrigin(String);
struct NativeReadOwner {
    broker: std::sync::Weak<crate::workspace_access::Broker>,
    cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl ParentDeadline {
    pub(crate) fn at(deadline: Instant) -> Result<Self, String> {
        let original_context = OperationContext::current();
        let deadline = original_context
            .as_ref()
            .map_or(deadline, |context| deadline.min(context.deadline()));
        if deadline > Instant::now() + Duration::from_secs(180) {
            return Err("external_compile_native_parent_window_bound".into());
        }
        let parent = Self {
            deadline,
            original_context,
            origin: NativeParentOrigin(crate::random::session_id()),
            read_owner: None,
            source_limits: None,
        };
        parent.check()?;
        Ok(parent)
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        if let Some(context) = &self.original_context {
            context
                .check()
                .map_err(|_| "external_compile_cancelled_or_expired")?;
        }
        let current_context = OperationContext::current();
        if let Some(context) = &current_context {
            context
                .check()
                .map_err(|_| "external_compile_cancelled_or_expired")?;
        }
        if Instant::now() >= self.deadline {
            return Err("external_compile_parent_deadline".into());
        }
        if let Some(owner) = &self.read_owner {
            let replaced = match (&self.original_context, &current_context) {
                (Some(original), Some(current)) => !original.same_operation(current),
                (None, Some(_)) => true,
                _ => false,
            };
            if replaced {
                return Err("external_compile_read_operation_binding_mismatch".into());
            }
            if owner.cancelled.as_ref().is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
                || !owner
                    .broker
                    .upgrade()
                    .is_some_and(|broker| broker.managed_output_is_open())
            {
                return Err("external_compile_read_owner_cancelled_or_closed".into());
            }
        }
        Ok(())
    }
    pub(crate) fn budget(&self, limit: Duration) -> Result<Duration, String> {
        self.check()?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("external_compile_parent_deadline".into());
        }
        Ok(remaining.min(limit))
    }
    pub(crate) fn instant(&self) -> Instant {
        self.deadline
    }
    pub(crate) fn origin(&self) -> &NativeParentOrigin {
        &self.origin
    }
    pub(crate) fn bind_read_owner(
        &mut self,
        broker: &Arc<crate::workspace_access::Broker>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        limits: crate::external_workspace::ProposalLimits,
    ) -> Result<(), String> {
        self.check()?;
        if self.read_owner.is_some() {
            return Err("external_compile_read_owner_already_bound".into());
        }
        self.read_owner = Some(NativeReadOwner {
            broker: Arc::downgrade(broker),
            cancelled: Some(cancelled),
        });
        self.source_limits = Some(limits);
        self.check()
    }
    pub(crate) fn bind_runtime_read_owner(
        &mut self, broker: &Arc<crate::workspace_access::Broker>,
        limits: crate::external_workspace::ProposalLimits,
    ) -> Result<(), String> {
        self.check()?;
        if self.read_owner.is_some() { return Err("external_compile_read_owner_already_bound".into()); }
        // Cancellation is the captured actual operation and original runtime
        // broker closure, not a replacement request-local false flag.
        self.read_owner = Some(NativeReadOwner { broker: Arc::downgrade(broker), cancelled: None });
        self.source_limits = Some(limits);
        self.check()
    }
    pub(crate) fn source_budget<'a>(
        &self,
        check: &'a dyn Fn() -> Result<(), String>,
    ) -> Result<Budget<'a>, String> {
        self.check()?;
        let budget = Budget::clipped(self.instant(), check)?;
        Ok(match &self.source_limits {
            Some(limits) => limits.restrict(&budget),
            None => budget,
        })
    }
    pub(crate) fn check_read_controller(
        &self,
        controller: &StreamController,
    ) -> Result<(), String> {
        self.check()?;
        let broker = self
            .read_owner
            .as_ref()
            .and_then(|owner| owner.broker.upgrade())
            .ok_or("external_compile_read_owner_missing")?;
        controller
            .check_parent(&broker)
            .map_err(|_| "external_compile_read_controller_binding_mismatch".into())
    }
    #[cfg(test)]
    fn expired_for_test() -> Self {
        Self {
            deadline: Instant::now() - Duration::from_millis(1),
            original_context: None,
            origin: NativeParentOrigin("expired-test-parent".into()),
            read_owner: None,
            source_limits: None,
        }
    }
}

pub(crate) struct SelectedMember {
    pub(crate) package: String,
    pub(crate) package_id: String,
    pub(crate) target: String,
    pub(crate) manifest: PathBuf,
    pub(crate) source: PathBuf,
}

// Issued only beside the concrete retained runner in the real owned entrypoint.
struct RealProvenanceRequest;
/// Private retained evidence, not dependency execution or GDExtension authority.
pub(crate) struct NativeCompilationProvenance {
    selected: SelectedMember,
    host: String,
    inputs: crate::external_workspace::NativeSourceIdentity,
    resolution: Value,
    local_builds: Vec<ObservedLocalBuild>,
    local_outputs: ReportedLocalOutputs,
    artifact: PathBuf,
    artifact_hash: String,
    digest: String,
}
impl NativeCompilationProvenance {
    fn computed_digest(&self) -> Result<String, String> {
        if self.selected.package.len() > 128
            || self.selected.target.len() > 128
            || self.selected.package_id.len() > 4096
            || self.selected.manifest.to_string_lossy().len() > 4096
            || self.selected.source.to_string_lossy().len() > 4096
            || self.host.len() > 128
            || self.artifact.to_string_lossy().len() > 4096
            || self.artifact_hash.len() != 64
        {
            return Err("external_native_provenance_field_bound".into());
        }
        let graph = crate::cargo_graph::summary(&self.resolution)?;
        let bytes = serde_json::to_vec(&json!({
            "contract":"private-native-compiler-provenance/4",
            "package":self.selected.package,"package_id":self.selected.package_id,
            "target":self.selected.target,"manifest":self.selected.manifest,
            "source":self.selected.source,"host":self.host,
            "held_inputs":self.inputs.digest(),"resolution":graph,
            "held_inputs_scope":"all staged local-source content custody",
            "observed_local_builds":self.local_builds.iter().map(ObservedLocalBuild::digest_value).collect::<Vec<_>>(),
            "local_build_scope":"observed Cargo artifacts joined to held local content; unbuilt metadata packages and exact build units unverified",
            "reported_local_outputs":self.local_outputs.digest_value(),
            "artifact":self.artifact,"artifact_sha256":self.artifact_hash,
            "exact_build_units_verified":false,"dependency_content_closure_verified":false,
            "gdextension_binding_or_loading_verified":false
        })).map_err(|_| "external_native_provenance_encoding")?;
        if bytes.len() > 256 * 1024 {
            return Err("external_native_provenance_encoding_bound".into());
        }
        Ok(crate::sha256_hex(&bytes))
    }
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
    #[cfg(all(windows,test))]
    pub(crate) fn reported_outputs_for_test(&self,budget:&Budget<'_>)->Result<Value,String> {
        self.local_outputs.recheck(&self.local_builds,&self.selected,&self.artifact,&self.artifact_hash,budget)?;
        let mut observation=self.local_outputs.observations_for_test();
        observation["observed_local_builds"]=json!(self.local_builds.iter().map(|build|
            Ok(json!({"observation_key_sha256":build.observation_key()?,"observation":build.digest_value()}))
        ).collect::<Result<Vec<Value>,String>>()?);
        Ok(observation)
    }
    /// Borrow original retained content and host; this issues no new authority.
    pub(crate) fn held_binding_inputs(&self) -> (&crate::external_workspace::NativeSourceIdentity, &str) {
        (&self.inputs, &self.host)
    }
    pub(crate) fn recheck(
        &self,
        staged: &StagedSources,
        artifact: &Path,
        artifact_hash: &str,
        budget: &Budget<'_>,
    ) -> Result<(), String> {
        budget.check()?;
        if !crate::external_workspace::provenance_paths_equal(&self.artifact, artifact) || self.artifact_hash != artifact_hash
            || !crate::external_workspace::provenance_paths_equal(&self.selected.manifest, &staged.member_manifest)
        {
            return Err("external_native_provenance_binding_changed".into());
        }
        self.inputs.recheck(staged, &self.selected.manifest, &self.selected.source, budget)?;
        for build in &self.local_builds {
            self.inputs.recheck_local_input(&build.manifest, budget)?;
            self.inputs.recheck_local_input(&build.source, budget)?;
        }
        self.local_outputs.recheck(&self.local_builds,&self.selected,artifact,artifact_hash,budget)?;
        if self.computed_digest()? != self.digest {
            return Err("external_native_provenance_digest_changed".into());
        }
        budget.check()
    }
}
/// Cargo's observation, not a compiler read trace or dependency execution grant.
pub(crate) struct ObservedLocalBuild {
    package: String,
    target: Value,
    profile: Value,
    features: Vec<String>,
    manifest: crate::external_workspace::NativeLocalInput,
    source: crate::external_workspace::NativeLocalInput,
}
impl ObservedLocalBuild {
    pub(crate) fn digest_value(&self) -> Value {
        json!({"package_id_sha256":crate::sha256_hex(self.package.as_bytes()),
            "target":self.target,"profile":self.profile,"features":self.features,
            "manifest":self.manifest.digest_value(),"source":self.source.digest_value()})
    }
}
const REPORTED_LOCAL_OUTPUTS_PER_RECORD: usize = 8;
const REPORTED_LOCAL_OUTPUT_FILES: usize = 256;
const REPORTED_LOCAL_OUTPUT_BYTES: u64 = 128 * 1024 * 1024;
fn reported_output_remaining(files:usize,bytes:u64)->Result<u64,String> {
    if files>=REPORTED_LOCAL_OUTPUT_FILES || bytes>REPORTED_LOCAL_OUTPUT_BYTES {
        return Err("external_reported_output_aggregate_bound".into());
    }
    Ok(REPORTED_LOCAL_OUTPUT_BYTES-bytes)
}
#[derive(Clone,Copy,PartialEq,Eq)]
enum ReportedOutputRole { Target, Build }
impl ReportedOutputRole {
    fn label(self)->&'static str {match self {Self::Target=>"target",Self::Build=>"build"}}
}
struct ReportedLocalOutputFile {
    role: ReportedOutputRole,
    held: crate::external_workspace::NativeReportedOutput,
}
impl ReportedLocalOutputFile {
    fn digest_value(&self)->Value {
        let mut value=self.held.digest_value();value["root_role"]=json!(self.role.label());value
    }
    #[cfg(all(windows,test))]
    fn observation_for_test(&self)->Value {
        let mut value=self.held.observation_for_test();value["root_role"]=json!(self.role.label());value
    }
}
struct ReportedLocalOutputRecord {
    observation_key: String,
    outputs: Vec<ReportedLocalOutputFile>,
}
/// Cargo-reported local outputs, not exact compiler units or a read trace.
pub(crate) struct ReportedLocalOutputs {
    target: crate::external_workspace::NativeReportedOutputRoot,
    build: crate::external_workspace::NativeReportedOutputRoot,
    records: Vec<ReportedLocalOutputRecord>,
}
impl ObservedLocalBuild {
    fn observation_key(&self)->Result<String,String> {
        let encoded=serde_json::to_vec(&json!([self.package,self.target,self.profile,self.features,self.source.digest_value()]))
            .map_err(|_|"external_reported_output_observation_encoding")?;
        if encoded.len()>65536 {return Err("external_reported_output_observation_encoding".into());}
        Ok(crate::sha256_hex(&encoded))
    }
}
impl ReportedLocalOutputs {
    fn root(&self,role:ReportedOutputRole)->&crate::external_workspace::NativeReportedOutputRoot {
        match role {ReportedOutputRole::Target=>&self.target,ReportedOutputRole::Build=>&self.build}
    }
    fn recheck_roots(&self,budget:&Budget<'_>)->Result<(),String> {
        self.target.recheck(budget)?;self.build.recheck(budget)
    }
    fn digest_value(&self)->Value {
        json!({"scope":"Cargo-reported local artifact content custody, not compiler reads or exact build units",
            "records":self.records.iter().map(|record|json!({"observation_key_sha256":record.observation_key,
                "outputs":record.outputs.iter().map(|output|output.digest_value()).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "dependency_content_closure_verified":false,"exact_build_units_verified":false,
            "loading_or_publication_authority":false})
    }
    pub(crate) fn recheck(
        &self,builds:&[ObservedLocalBuild],selected:&SelectedMember,artifact:&Path,artifact_hash:&str,budget:&Budget<'_>,
    )->Result<(),String> {
        self.recheck_roots(budget)?;
        if builds.is_empty() || builds.len()>128 || self.records.len()!=builds.len() {
            return Err("external_reported_output_observation_bound".into());
        }
        let expected=builds.iter().map(ObservedLocalBuild::observation_key).collect::<Result<BTreeSet<_>,_>>()?;
        let selected_keys=builds.iter().filter(|build|build.package==selected.package_id && build.target["name"]==selected.target)
            .map(ObservedLocalBuild::observation_key).collect::<Result<Vec<_>,_>>()?;
        if expected.len()!=builds.len() || selected_keys.len()!=1 {return Err("external_reported_output_selected_observation".into());}
        let mut seen=BTreeSet::new();let mut files=0usize;let mut bytes=0u64;let mut selected_matches=0usize;
        for record in &self.records {
            budget.check()?;
            if !expected.contains(&record.observation_key) || !seen.insert(record.observation_key.clone())
                || record.outputs.is_empty() || record.outputs.len()>REPORTED_LOCAL_OUTPUTS_PER_RECORD
            {return Err("external_reported_output_observation_changed".into());}
            let mut identities=BTreeSet::new();
            for output in &record.outputs {
                let remaining=reported_output_remaining(files,bytes)?;
                if !identities.insert(output.held.identity_key()) {return Err("external_reported_output_duplicate_file".into());}
                output.held.recheck(self.root(output.role),remaining,budget)?;
                files+=1;bytes=bytes.checked_add(output.held.bytes()).ok_or("external_reported_output_aggregate_bound")?;
                if record.observation_key==selected_keys[0] && output.role==ReportedOutputRole::Target
                    && output.held.matches_artifact(artifact,artifact_hash)? {selected_matches+=1;}
            }
        }
        if selected_matches!=1 {return Err("external_reported_output_selected_artifact".into());}
        self.recheck_roots(budget)
    }
    #[cfg(all(windows,test))]
    pub(crate) fn observations_for_test(&self)->Value {
        json!({"records":self.records.iter().map(|record|json!({"observation_key_sha256":record.observation_key,
            "outputs":record.outputs.iter().map(ReportedLocalOutputFile::observation_for_test).collect::<Vec<_>>()})).collect::<Vec<_>>()})
    }
}
pub(crate) fn hold_reported_local_outputs(
    stdout:&str,builds:&[ObservedLocalBuild],inputs:&crate::external_workspace::NativeSourceIdentity,
    lease:&CompilationLease,target:&Path,build:&Path,budget:&Budget<'_>,
)->Result<ReportedLocalOutputs,String> {
    if builds.is_empty() || builds.len()>128 {return Err("external_reported_output_observation_bound".into());}
    // Exactly the two host-authored roots already held by this original lease.
    let target=lease.reported_output_root(target,budget)?;
    let build=lease.reported_output_root(build,budget)?;
    let mut records=Vec::new();let mut keys=BTreeSet::new();let mut files=0usize;let mut bytes=0u64;
    for line in stdout.lines() {
        budget.check()?;
        if line.len()>1024*1024 {return Err("external_reported_output_message_bound".into());}
        let value:Value=serde_json::from_str(line).map_err(|_|"external_reported_output_message")?;
        if value["reason"]!="compiler-artifact" {continue;}
        let feature_rows=value["features"].as_array().filter(|rows|rows.len()<=256).ok_or("external_reported_output_features")?;
        let mut features=feature_rows.iter().map(|row|row.as_str().filter(|s|!s.is_empty() && s.len()<=128 && !s.chars().any(char::is_control))
            .map(str::to_owned).ok_or("external_reported_output_features")).collect::<Result<Vec<_>,_>>()?;
        features.sort();if features.windows(2).any(|pair|pair[0]==pair[1]) {return Err("external_reported_output_features".into());}
        let target_value=json!({"name":value["target"]["name"],"kind":value["target"]["kind"],"crate_types":value["target"]["crate_types"]});
        let manifest=inputs.local_input(Path::new(value["manifest_path"].as_str().ok_or("external_reported_output_manifest")?),budget)?;
        let source=inputs.local_input(Path::new(value["target"]["src_path"].as_str().ok_or("external_reported_output_source")?),budget)?;
        let mut matching=builds.iter().filter(|build|value["package_id"]==build.package && target_value==build.target
            && value["profile"]==build.profile && features==build.features && manifest==build.manifest && source==build.source);
        let observation=matching.next().ok_or("external_reported_output_observation_missing")?;
        if matching.next().is_some() {return Err("external_reported_output_observation_ambiguous".into());}
        let key=observation.observation_key()?;
        if !keys.insert(key.clone()) {return Err("external_reported_output_duplicate_observation".into());}
        let names=value["filenames"].as_array().filter(|names|!names.is_empty() && names.len()<=REPORTED_LOCAL_OUTPUTS_PER_RECORD)
            .ok_or("external_reported_output_filename_bound")?;
        let mut outputs=Vec::new();let mut identities=BTreeSet::new();
        for name in names {
            let name=name.as_str().filter(|s|!s.is_empty() && s.len()<=4096 && !s.chars().any(char::is_control))
                .ok_or("external_reported_output_filename")?;
            let remaining=reported_output_remaining(files,bytes)?;
            let target_match=target.contains_reported_path(name,budget)?;
            let build_match=build.contains_reported_path(name,budget)?;
            let role=match (target_match,build_match) {
                (true,false)=>ReportedOutputRole::Target,
                (false,true)=>ReportedOutputRole::Build,
                (false,false)=>return Err("external_reported_output_unknown_root".into()),
                (true,true)=>return Err("external_reported_output_ambiguous_root".into()),
            };
            let root=match role {ReportedOutputRole::Target=>&target,ReportedOutputRole::Build=>&build};
            let output=root.hold(lease,name,remaining,budget)?;
            if !identities.insert(output.identity_key()) {return Err("external_reported_output_duplicate_file".into());}
            files+=1;bytes=bytes.checked_add(output.bytes()).ok_or("external_reported_output_aggregate_bound")?;
            outputs.push(ReportedLocalOutputFile {role,held:output});
        }
        outputs.sort_by(|a,b|(a.role.label(),a.held.relative()).cmp(&(b.role.label(),b.held.relative())));
        records.push(ReportedLocalOutputRecord {observation_key:key,outputs});
    }
    if records.len()!=builds.len() {return Err("external_reported_output_observation_missing".into());}
    records.sort_by(|a,b|a.observation_key.cmp(&b.observation_key));
    target.recheck(budget)?;build.recheck(budget)?;Ok(ReportedLocalOutputs {target,build,records})
}
pub(crate) fn observed_local_builds(
    stdout: &str, metadata: &Value, selected: &SelectedMember,
    inputs: &crate::external_workspace::NativeSourceIdentity, budget: &Budget<'_>,
) -> Result<Vec<ObservedLocalBuild>, String> {
    let packages = metadata["packages"].as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 128)
        .ok_or("external_local_build_metadata_bound")?;
    let mut by_id = BTreeMap::new();
    for package in packages {
        budget.check()?;
        let id = package["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 4096)
            .ok_or("external_local_build_package_id")?;
        if package.get("source") != Some(&Value::Null) || by_id.insert(id, package).is_some() {
            return Err("external_local_build_package_unsupported_or_duplicate".into());
        }
    }
    let mut seen_packages = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut builds = Vec::new();
    let mut encoded = 0usize;
    for line in stdout.lines() {
        budget.check()?;
        if line.len() > 1024 * 1024 { return Err("external_local_build_message_bound".into()); }
        let value: Value = serde_json::from_str(line).map_err(|_| "external_local_build_message")?;
        if value["reason"] != "compiler-artifact" { continue; }
        if builds.len() >= 128 { return Err("external_local_build_observation_bound".into()); }
        let id = value["package_id"].as_str().filter(|id| by_id.contains_key(*id))
            .ok_or("external_local_build_unexpected_package")?;
        let package = by_id.get(id).ok_or("external_local_build_package_missing")?;
        let target = &value["target"];
        if target["name"].as_str().filter(|name| !name.is_empty() && name.len() <= 128).is_none()
            || !target["crate_types"].as_array().is_some_and(|types| types.len() == 1
                && matches!(types[0].as_str(), Some("lib" | "rlib" | "cdylib")))
        { return Err("external_local_build_target_unsupported".into()); }
        let targets = package["targets"].as_array().filter(|rows| rows.len() <= 128)
            .ok_or("external_local_build_target_bound")?;
        let mut matching = targets.iter().filter(|candidate| {
            candidate["name"] == target["name"] && candidate["kind"] == target["kind"]
                && candidate["crate_types"] == target["crate_types"]
        });
        let declared = matching.next().ok_or("external_local_build_target_missing")?;
        if matching.next().is_some() { return Err("external_local_build_target_ambiguous".into()); }
        let kind = target["kind"].as_array().filter(|kinds| kinds.len() == 1)
            .and_then(|kinds| kinds[0].as_str()).ok_or("external_local_build_target_kind")?;
        if !matches!(kind, "lib" | "rlib" | "cdylib") {
            return Err("external_local_build_target_unsupported".into());
        }
        let manifest = inputs.local_input(Path::new(value["manifest_path"].as_str()
            .ok_or("external_local_build_manifest")?), budget)?;
        let declared_manifest = inputs.local_input(Path::new(package["manifest_path"].as_str()
            .ok_or("external_local_build_manifest")?), budget)?;
        let source = inputs.local_input(Path::new(target["src_path"].as_str()
            .ok_or("external_local_build_source")?), budget)?;
        let declared_source = inputs.local_input(Path::new(declared["src_path"].as_str()
            .ok_or("external_local_build_source")?), budget)?;
        if manifest != declared_manifest || source != declared_source {
            return Err("external_local_build_content_binding_changed".into());
        }
        let profile = &value["profile"];
        let profile_object = profile.as_object().filter(|fields| fields.len() <= 16)
            .ok_or("external_local_build_profile")?;
        if profile["opt_level"].as_str().filter(|text| text.len() <= 8).is_none()
            || !profile["debug_assertions"].is_boolean() || !profile["overflow_checks"].is_boolean()
            || profile["test"] != false
            || !(profile["debuginfo"].is_null() || profile["debuginfo"].as_u64().is_some_and(|v| v <= 2))
            || profile_object.iter().any(|(key, value)| key.len() > 64
                || !(value.is_null() || value.is_boolean() || value.as_i64().is_some()
                    || value.as_str().is_some_and(|text| text.len() <= 64)))
        { return Err("external_local_build_profile".into()); }
        let features = value["features"].as_array().filter(|rows| rows.len() <= 256)
            .ok_or("external_local_build_features")?;
        let mut features_set = BTreeSet::new();
        for feature in features {
            let feature = feature.as_str().filter(|text| !text.is_empty() && text.len() <= 128
                && !text.chars().any(char::is_control)).ok_or("external_local_build_features")?;
            if !features_set.insert(feature.to_owned()) { return Err("external_local_build_features".into()); }
        }
        let features: Vec<_> = features_set.into_iter().collect();
        // Metadata feature sets may be a workspace union. Retain the observed set.
        let target = json!({"name":target["name"],"kind":target["kind"],
            "crate_types":target["crate_types"]});
        let key = serde_json::to_string(&json!([id,&target,&profile,&features,
            source.digest_value()])).map_err(|_| "external_local_build_encoding")?;
        let build = ObservedLocalBuild { package: id.to_owned(), target,
            profile: profile.clone(), features, manifest, source };
        let record_bytes = serde_json::to_vec(&build.digest_value())
            .map_err(|_| "external_local_build_encoding")?.len();
        encoded = encoded.checked_add(key.len()).and_then(|bytes| bytes.checked_add(record_bytes))
            .ok_or("external_local_build_encoding_bound")?;
        if encoded > 65536 { return Err("external_local_build_encoding_bound".into()); }
        if !keys.insert(key) { return Err("external_local_build_duplicate_unit".into()); }
        seen_packages.insert(id.to_owned());
        builds.push(build);
    }
    if !seen_packages.contains(selected.package_id.as_str()) { return Err("external_local_build_selected_unobserved".into()); }
    builds.sort_by_key(|build| (build.package.clone(), build.digest_value().to_string()));
    budget.check()?;
    Ok(builds)
}
/// Carries the original checked artifact across compilation-owner retirement.
/// Only the retained validation path constructs this noncloneable result.
pub(crate) struct OwnedNativeValidation {
    receipt: Value,
    artifact: PathBuf,
    artifact_guard: File,
    artifact_hash: String,
    provenance: Option<NativeCompilationProvenance>,
}
impl OwnedNativeValidation {
    pub(crate) fn receipt(&self) -> &Value { &self.receipt }
    fn into_receipt(self) -> Value {
        self.receipt
    }
    pub(crate) fn into_parts(self) -> (Value, PathBuf, File, String, Option<NativeCompilationProvenance>) {
        (
            self.receipt,
            self.artifact,
            self.artifact_guard,
            self.artifact_hash,
            self.provenance,
        )
    }
}

const OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const STDERR_BYTES: u64 = 64 * 1024;

struct Executable {
    invocation: PathBuf,
    application: PathBuf,
    sha256: String,
    _guard: File,
}
impl Executable {
    fn open(invocation: PathBuf, parent: &ParentDeadline) -> Result<Self, String> {
        parent.check()?;
        let invocation = std::path::absolute(invocation)
            .map(crate::project::clean_path)
            .map_err(|_| "external_compiler_tool_unavailable")?;
        let application = invocation
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|_| "external_compiler_tool_unavailable")?;
        if !application
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
        {
            return Err("external_compiler_tool_kind_refused".into());
        }
        let (guard, bytes) = bounded_file(&application, 128 * 1024 * 1024, parent)?;
        Ok(Self {
            invocation,
            application,
            sha256: crate::sha256_hex(&bytes),
            _guard: guard,
        })
    }
    fn recheck(&self, parent: &ParentDeadline) -> Result<(), String> {
        parent.check()?;
        crate::snapshot::check_opened_path(&self._guard, &self.application, parent.instant())
            .map_err(|_| "external_compiler_tool_identity_changed")?;
        let current = self
            .invocation
            .canonicalize()
            .map(crate::project::clean_path)
            .map_err(|_| "external_compiler_tool_identity_changed")?;
        if !crate::ownership::paths_equal(&current, &self.application) {
            return Err("external_compiler_tool_identity_changed".into());
        }
        parent.check()
    }
}

// Token-specific: the configured protected Godot image exceeds the compiler's
// separate128MiB cap. This does not widen compiler or artifact admission.
const CONFIGFILE_ENGINE_BYTES: u64 = 256 * 1024 * 1024;
const CONFIGFILE_RUNTIME_ENTRIES: usize = 64;

fn configured_engine_path(
    mut host_setting: impl FnMut(&str) -> Option<PathBuf>,
    deadline: Instant,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    check()?;
    if let Some(path) = host_setting("AURUM_GODOT") {
        return checked_engine_invocation(path);
    }
    let home = host_setting("AURUM_STUDIO_HOME")
        .ok_or("external_configfile_engine_not_configured")?;
    if !home.is_absolute() { return Err("external_configfile_engine_path_refused".into()); }
    text_path(&home)?;
    let directory = home.join("runtime");
    let guard = crate::snapshot::open_checked_directory(&directory, deadline)
        .map_err(|_| "external_configfile_engine_runtime_refused")?;
    let mut candidates = Vec::new();
    for (index, entry) in std::fs::read_dir(&directory)
        .map_err(|_| "external_configfile_engine_runtime_refused")?.enumerate()
    {
        check()?;
        if index >= CONFIGFILE_RUNTIME_ENTRIES {
            return Err("external_configfile_engine_runtime_bounds".into());
        }
        let entry = entry.map_err(|_| "external_configfile_engine_runtime_refused")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("godot") && lower.ends_with(".exe") {
            let kind = entry.file_type().map_err(|_| "external_configfile_engine_runtime_refused")?;
            if kind.is_file() && !kind.is_symlink() {
                candidates.push((!lower.contains("console"), lower.len(), lower, name.to_owned()));
            }
        }
    }
    candidates.sort();
    crate::snapshot::check_opened_path(&guard, &directory, deadline)
        .map_err(|_| "external_configfile_engine_runtime_refused")?;
    check()?;
    let name = candidates.first().ok_or("external_configfile_engine_not_configured")?;
    checked_engine_invocation(directory.join(&name.3))
}

fn checked_engine_invocation(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    { return Err("external_configfile_engine_path_refused".into()); }
    text_path(&path)?;
    Ok(path)
}

#[derive(PartialEq, Eq)]
struct ConfigFileEngineStamp {
    volume: u64,
    id: [u8; 16],
    bytes: u64,
    created: std::time::SystemTime,
    modified: std::time::SystemTime,
}

fn configfile_engine_stamp(file: &File) -> Result<ConfigFileEngineStamp, String> {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct FileIdInfo { volume: u64, id: [u8; 16] }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            file: *mut std::ffi::c_void, class: u32, info: *mut std::ffi::c_void, bytes: u32,
        ) -> i32;
    }
    let metadata = file.metadata().map_err(|_| "external_configfile_engine_metadata")?;
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
        return Err("external_configfile_engine_file_refused".into());
    }
    if metadata.len() == 0 || metadata.len() > CONFIGFILE_ENGINE_BYTES {
        return Err("external_configfile_engine_bytes_bound".into());
    }
    // Same FileIdInfo ABI already used by external_workspace's held-file
    // identity checks. These are facts about this File, never serialized grants.
    let mut identity = FileIdInfo { volume: 0, id: [0; 16] };
    if unsafe { GetFileInformationByHandleEx(
        file.as_raw_handle(), 0x12, (&mut identity as *mut FileIdInfo).cast(),
        std::mem::size_of::<FileIdInfo>() as u32,
    ) } == 0 { return Err("external_configfile_engine_identity_unavailable".into()); }
    Ok(ConfigFileEngineStamp {
        volume: identity.volume, id: identity.id, bytes: metadata.len(),
        created: metadata.created().map_err(|_| "external_configfile_engine_metadata")?,
        modified: metadata.modified().map_err(|_| "external_configfile_engine_metadata")?,
    })
}

fn configfile_engine_hash(
    reader: &mut (impl Read + std::io::Seek),
    expected: u64,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<String, String> {
    use std::io::SeekFrom;
    if expected == 0 || expected > CONFIGFILE_ENGINE_BYTES {
        return Err("external_configfile_engine_bytes_bound".into());
    }
    let position = reader.stream_position().map_err(|_| "external_configfile_engine_cursor")?;
    let result = (|| {
        check()?;
        reader.seek(SeekFrom::Start(0)).map_err(|_| "external_configfile_engine_cursor")?;
        let mut hash = crate::hash::Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check()?;
            let capacity = expected.saturating_sub(bytes).saturating_add(1)
                .min(buffer.len() as u64) as usize;
            let count = reader.read(&mut buffer[..capacity])
                .map_err(|_| "external_configfile_engine_read")?;
            if count == 0 { break; }
            bytes = bytes.checked_add(count as u64).ok_or("external_configfile_engine_bytes_bound")?;
            if bytes > expected { return Err("external_configfile_engine_length_changed".into()); }
            hash.update(&buffer[..count]);
        }
        if bytes != expected { return Err("external_configfile_engine_length_changed".into()); }
        check()?;
        Ok(hash.hex())
    })();
    reader.seek(SeekFrom::Start(position)).map_err(|_| "external_configfile_engine_cursor_restore")?;
    result
}

// Internal read witness only. It cannot issue a HeldConfigFileEngine or launch.
struct ConfigFileEngineImage {
    invocation: PathBuf,
    application: PathBuf,
    guard: File,
    stamp: ConfigFileEngineStamp,
    sha256: String,
}
impl ConfigFileEngineImage {
    fn open(
        invocation: PathBuf, deadline: Instant, check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let invocation = checked_engine_invocation(invocation)?;
        // Check the declared path before canonicalizing, so a reparse spelling
        // cannot disappear behind its canonical target.
        let prior = crate::snapshot::open_checked_file(&invocation, deadline)
            .map_err(|_| "external_configfile_engine_file_refused")?;
        let before = configfile_engine_stamp(&prior)?;
        let application = invocation.canonicalize().map(crate::project::clean_path)
            .map_err(|_| "external_configfile_engine_file_refused")?;
        checked_engine_invocation(application.clone())?;
        let guard = OpenOptions::new().read(true).custom_flags(0x0020_0000).share_mode(1)
            .open(&invocation).map_err(|_| "external_configfile_engine_file_refused")?;
        if configfile_engine_stamp(&guard)? != before {
            return Err("external_configfile_engine_changed".into());
        }
        let mut image = Self { invocation, application, guard, stamp: before, sha256: String::new() };
        image.check_identity(deadline, check)?;
        image.sha256 = configfile_engine_hash(&mut &image.guard, image.stamp.bytes, check)?;
        image.check_identity(deadline, check)?;
        Ok(image)
    }
    fn check_identity(&self, deadline: Instant, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        check()?;
        crate::snapshot::check_opened_path(&self.guard, &self.invocation, deadline)
            .and_then(|_| crate::snapshot::check_opened_path(&self.guard, &self.application, deadline))
            .map_err(|_| "external_configfile_engine_identity_changed")?;
        let application = self.invocation.canonicalize().map(crate::project::clean_path)
            .map_err(|_| "external_configfile_engine_identity_changed")?;
        if !crate::ownership::paths_equal(&application, &self.application)
            || configfile_engine_stamp(&self.guard)? != self.stamp
        { return Err("external_configfile_engine_changed".into()); }
        check()
    }
    fn recheck(&self, deadline: Instant, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        self.check_identity(deadline, check)?;
        let sha256 = configfile_engine_hash(&mut &self.guard, self.stamp.bytes, check)?;
        if sha256 != self.sha256 { return Err("external_configfile_engine_hash_changed".into()); }
        self.check_identity(deadline, check)
    }
}

/// Original-owner held image identity only: no process, version, benignness,
/// permission, parser workspace, command or loading capability is issued here.
pub(crate) struct HeldConfigFileEngine<'a> {
    runtime: &'a crate::native_operations::NativeRuntime,
    root: PathBuf,
    parent: &'a ParentDeadline,
    controller: &'a StreamController,
    image: ConfigFileEngineImage,
}
impl<'a> HeldConfigFileEngine<'a> {
    pub(crate) fn hold_configured(
        runtime: &'a crate::native_operations::NativeRuntime, root: &Path,
        parent: &'a ParentDeadline, controller: &'a StreamController,
    ) -> Result<Self, String> {
        let check = || {
            parent.check_read_controller(controller)?;
            runtime.check_managed_native_output(root, controller)
        };
        check()?;
        let invocation = configured_engine_path(
            |name| std::env::var_os(name).map(PathBuf::from), parent.instant(), &check,
        )?;
        let image = ConfigFileEngineImage::open(invocation, parent.instant(), &check)?;
        let held = Self { runtime, root: root.to_path_buf(), parent, controller, image };
        held.recheck()?;
        Ok(held)
    }
    pub(crate) fn recheck(&self) -> Result<(), String> {
        let check = || {
            self.parent.check_read_controller(self.controller)?;
            self.runtime.check_managed_native_output(&self.root, self.controller)
        };
        self.image.recheck(self.parent.instant(), &check)
    }
}

const CONFIGFILE_PARSER_SCRIPT: &str = include_str!("configfile_parser.gd");
const CONFIGFILE_PARSER_OUTPUT: usize = 256 * 1024;
const CONFIGFILE_TARGET_TAGS: [&str; 6] = crate::external_workspace::PRIVATE_GDEXTENSION_FEATURES;
const CONFIGFILE_OBSERVED_TAGS: [&str; 11] = ["windows", "debug", "release", "x86_64", "arm64", "single", "double", "64", "32", "editor", "template"];

#[derive(serde::Deserialize,serde::Serialize)]
#[serde(untagged)]
enum ConfigFileValue { Text(String), Boolean(bool) }
#[derive(serde::Deserialize,serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigFileEntry { key:String, #[serde(rename="type")] kind:String, value:ConfigFileValue }
#[derive(serde::Deserialize,serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigFileSection { name:String, entries:Vec<ConfigFileEntry> }
#[derive(serde::Deserialize,serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigFileLibrary { key:String, flags:Vec<String>, path:String, matches_target:bool }
#[derive(serde::Deserialize,serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigFileOutput {
    schema:String, parse_error:i64, sections:Vec<ConfigFileSection>, libraries:Vec<ConfigFileLibrary>,
    dependencies:Vec<()>, target_tags:Vec<String>, observed_tags:Vec<String>,
}

fn configfile_parser_output(bytes: &[u8], expected: &Value) -> Result<Vec<String>, String> {
    if bytes.len() > CONFIGFILE_PARSER_OUTPUT { return Err("configfile_parser_output_bound".into()); }
    if !bytes.ends_with(b"\n") { return Err("configfile_parser_output_unterminated".into()); }
    let text = std::str::from_utf8(bytes).map_err(|_| "configfile_parser_output_utf8")?;
    let mut lines = text.lines().filter_map(|line| line.strip_prefix("AURUM_CONFIGFILE_PARSED "));
    let line = lines.next().ok_or("configfile_parser_output_missing")?;
    if lines.next().is_some() { return Err("configfile_parser_output_duplicate".into()); }
    // Derived closed structs reject repeated fields too; parsing first into
    // Value would silently discard a duplicate property's earlier value.
    let typed: ConfigFileOutput = serde_json::from_str(line).map_err(|_| "configfile_parser_output_json")?;
    let parsed = serde_json::to_value(typed).map_err(|_| "configfile_parser_output_json")?;
    let row = parsed.as_object().ok_or("configfile_parser_output_schema")?;
    let keys = ["schema", "parse_error", "sections", "libraries", "dependencies", "target_tags", "observed_tags"];
    if row.len() != keys.len() || row.keys().any(|key| !keys.contains(&key.as_str()))
        || parsed["schema"] != "aurum.configfile.output/1" || parsed["parse_error"].as_i64() != Some(0)
        || parsed["target_tags"] != json!(CONFIGFILE_TARGET_TAGS)
        || parsed["sections"] != expected["sections"] || parsed["libraries"] != expected["libraries"]
        || parsed["dependencies"] != json!([]) || expected["dependencies"] != json!([])
    { return Err("configfile_parser_supported_subset_mismatch".into()); }
    let tags = parsed["observed_tags"].as_array().ok_or("configfile_parser_observed_tags")?;
    if tags.len() > CONFIGFILE_OBSERVED_TAGS.len() { return Err("configfile_parser_observed_tags".into()); }
    let mut seen = BTreeSet::new();
    tags.iter().map(|value| {
        let tag = value.as_str().ok_or("configfile_parser_observed_tags")?;
        if !CONFIGFILE_OBSERVED_TAGS.contains(&tag) || !seen.insert(tag) {
            return Err("configfile_parser_observed_tags".into());
        }
        Ok(tag.to_owned())
    }).collect()
}

struct ConfigFileParserState {
    image: ConfigFileEngineImage,
    _management: crate::native_runtime::ManagementLease<'static>,
    workspace: Option<Workspace>,
    files: Vec<(PathBuf, File, ConfigFileEngineStamp, String)>,
}
impl Drop for ConfigFileParserState {
    fn drop(&mut self) {
        // Normal cleanup is explicit under the original budget. Error/expiry
        // cannot silently grant late deletion or release a live allocation.
        if let Some(workspace) = self.workspace.take() { std::mem::forget(workspace); }
    }
}

fn configfile_parser_file(
    root: &Path, name: &str, bytes: &[u8], parent: &ParentDeadline,
) -> Result<(PathBuf, File, ConfigFileEngineStamp, String), String> {
    use std::io::Write;
    parent.check()?;
    if bytes.is_empty() || bytes.len() > CONFIGFILE_PARSER_OUTPUT { return Err("configfile_parser_input_bound".into()); }
    let path = root.join(name);
    let mut file = OpenOptions::new().read(true).write(true).create_new(true).share_mode(1)
        .custom_flags(0x0020_0000).open(&path).map_err(|_| "configfile_parser_input_create")?;
    for chunk in bytes.chunks(8192) {
        parent.check()?;
        file.write_all(chunk).map_err(|_| "configfile_parser_input_write")?;
    }
    file.sync_all().map_err(|_| "configfile_parser_input_flush")?;
    crate::snapshot::check_opened_path(&file, &path, parent.instant())?;
    let hash = configfile_engine_hash(&mut &file, bytes.len() as u64, &|| parent.check())?;
    if hash != crate::sha256_hex(bytes) { return Err("configfile_parser_input_readback".into()); }
    let stamp = configfile_engine_stamp(&file)?;
    Ok((path, file, stamp, hash))
}

/// Run only the fixed parser in a fresh recognized peer allocation. All source
/// text has already passed the strict quoted-subset barrier in held custody.
pub(crate) fn compare_configfile(
    compiler_workspace: &Workspace, input: &crate::external_workspace::ConfigFileInput<'_>,
    parent: &ParentDeadline, controller: &StreamController,
    permission: &crate::native_operations::ConfigFileParsePermission<'_>,
) -> Result<Value, String> {
    let check = || permission.check(parent, controller);
    check()?;
    let (manifest_root,manifest_path,text,expected) = input.parts();
    permission.check_manifest(manifest_root,manifest_path)?;
    if text.is_empty() || text.len() > 65536 { return Err("configfile_parser_input_bound".into()); }
    if &crate::external_workspace::configfile_parser_document(text)? != expected {
        return Err("configfile_parser_strict_admission_document_mismatch".into());
    }
    let held = permission.hold_engine(parent, controller)?;
    let image = held.image; // owned File, not a leaked borrowed runtime/parent
    let workspace = compiler_workspace.parser_peer_checked(parent.instant(), &check)?;
    let deadline = parent.instant().min(Instant::now() + parent.budget(Duration::from_secs(30))?);
    let management = crate::native_runtime::acquire_checked_management(deadline, || check().map_err(io::Error::other))
        .map_err(|_| "configfile_parser_management_admission")?;
    let deadline = management.deadline();
    let root = workspace.path.clone();
    let state = Arc::new(std::sync::Mutex::new(ConfigFileParserState {
        image, _management: management, workspace: Some(workspace), files: Vec::new(),
    }));
    let result: Result<Value,String> = (|| {
        let input = serde_json::to_vec(&json!({"schema":"aurum.configfile.input/1", "text":text,
            "target_tags":CONFIGFILE_TARGET_TAGS})).map_err(|_| "configfile_parser_input_json")?;
        let mut owner = state.lock().map_err(|_| "configfile_parser_owner_poisoned")?;
        for name in ["profile", "temp"] {
            check()?;
            std::fs::create_dir(root.join(name)).map_err(|_| "configfile_parser_directory")?;
        }
        for (name, bytes) in [
            ("project.godot", b"config_version=5\n[application]\nconfig/name=\"Aurum ConfigFile Parser\"\n".as_slice()),
            ("parser.gd", CONFIGFILE_PARSER_SCRIPT.as_bytes()), ("input.json", input.as_slice()),
        ] { owner.files.push(configfile_parser_file(&root, name, bytes, parent)?); }
        let system = trusted_directory(std::env::var_os("SystemRoot").map(PathBuf::from), parent)?.0;
        let mut env = vec![
            ("SystemRoot".into(), text_path(&system)?), ("WINDIR".into(), text_path(&system)?),
            ("SystemDrive".into(), compiler_system_drive(&system)?),
            ("PATH".into(), text_path(&system.join("System32"))?),
        ];
        for (name, leaf) in [("HOME","profile"),("USERPROFILE","profile"),("APPDATA","profile"),
            ("LOCALAPPDATA","profile"),("XDG_DATA_HOME","profile"),("XDG_CONFIG_HOME","profile"),
            ("XDG_CACHE_HOME","profile"),("TEMP","temp"),("TMP","temp"),("TMPDIR","temp")]
        { env.push((name.into(), text_path(&root.join(leaf))?)); }
        let program = owner.image.application.clone();
        let argv0 = owner.image.invocation.clone();
        let image_sha256 = owner.image.sha256.clone();
        let fence = owner.workspace.as_ref().ok_or("configfile_parser_allocation_missing")?.lifetime.clone();
        drop(owner);
        check()?;
        let args = vec!["--headless".into(),"--path".into(),text_path(&root)?,
            "--script".into(),text_path(&root.join("parser.gd"))?];
        // The clone is installed in the same binary Resources BEFORE startup.
        // A failed/uncertain creator parks this original image/gate/allocation.
        let mut stream = Worker::spawn_binary_cleared_retaining(controller, BinarySpawn {
            program:&program, argv0:&argv0, args:&args, directory:Some(&root), overrides:&env,
            lifetime:&fence, deadline, limits:StreamLimits {stdout_bytes:CONFIGFILE_PARSER_OUTPUT as u64,stderr_bytes:65536},
        }, Box::new(state.clone())).map_err(|_| "configfile_parser_startup_refused")?;
        let observed: Result<(Value,Vec<String>),String> = (|| {
            let mut bytes = Vec::new();
            let mut buffer = [0u8;8192];
            let mut monitor = |effective| {
                check().map_err(io::Error::other)?;
                observe_storage_root(&root, parent, effective)
            };
            loop {
                let count = stream.read_before_checked(&mut buffer,deadline,&mut monitor)
                    .map_err(|_| "configfile_parser_read_refused")?;
                if count == 0 { break; }
                if bytes.len()+count > CONFIGFILE_PARSER_OUTPUT { return Err("configfile_parser_output_bound".into()); }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let receipt = stream.finish_checked(&mut monitor).map_err(|_| "configfile_parser_retirement_or_exit")?;
            check()?;
            let receipt = accepted_stream(&receipt,bytes.len(),parent)?;
            let tags = configfile_parser_output(&bytes,expected)?;
            Ok((receipt,tags))
        })();
        if observed.is_err() { let _ = stream.retire(); }
        drop(stream);
        let (receipt,tags) = observed?;
        let acceptance_check = || {
            check()?;
            if Instant::now() >= deadline { return Err("configfile_parser_phase_deadline".into()); }
            Ok(())
        };
        acceptance_check()?;
        let owner = state.lock().map_err(|_| "configfile_parser_owner_poisoned")?;
        owner.image.recheck(parent.instant(), &acceptance_check)?;
        for (path,file,stamp,hash) in &owner.files {
            crate::snapshot::check_opened_path(file,path,parent.instant())?;
            if configfile_engine_stamp(file)? != *stamp
                || configfile_engine_hash(&mut &*file,stamp.bytes,&acceptance_check)? != *hash
                || configfile_engine_stamp(file)? != *stamp
            {
                return Err("configfile_parser_held_input_changed".into());
            }
            crate::snapshot::check_opened_path(file,path,parent.instant())?;
        }
        acceptance_check()?;
        Ok(json!({"status":"matched_supported_quoted_subset","declared_target_tags":CONFIGFILE_TARGET_TAGS,
            "observed_engine_tags":tags,"engine_sha256":image_sha256,"worker":receipt,
            "dependencies":[],"loaded":false,"abi_verified":false,"source_publish":false}))
    })();
    let mut owner = state.lock().map_err(|_| "configfile_parser_owner_poisoned")?;
    let unresolved = owner.workspace.as_ref().is_some_and(|workspace| workspace.lifetime.unresolved());
    if unresolved {
        permission.mark_unconfirmed();
        return Err("configfile_parser_retirement_unconfirmed_resources_retained".into());
    }
    if check().is_ok() {
        owner.files.clear();
        let mut workspace = owner.workspace.take().ok_or("configfile_parser_allocation_missing")?;
        if workspace.clean().is_err() {
            permission.mark_unconfirmed();
            std::mem::forget(workspace);
            return Err("configfile_parser_cleanup_unconfirmed".into());
        }
    } else {
        permission.mark_unconfirmed();
        return Err("configfile_parser_original_budget_expired_allocation_retained".into());
    }
    let mut value = result?;
    value["parser_workspace_cleaned"] = json!(true);
    Ok(value)
}

fn observe_storage_root(root: &Path, parent: &ParentDeadline, deadline: Instant) -> io::Result<()> {
    parent.check().map_err(io::Error::other)?;
    crate::candidate_workspace::usage_with_budget(root, WORKSPACE_BYTES,
        deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(250)))
        .map(|_| ()).map_err(io::Error::other)
}

struct Tools {
    cargo: Executable,
    rustc: Executable,
    system: PathBuf,
    rustup: PathBuf,
    discovery: CompilerDiscoveryContext,
}
#[derive(Default)]
struct CompilerDiscoveryContext {
    env: Vec<(String, String)>,
    directories: Vec<PathBuf>,
}
fn trusted_directory(
    path: Option<PathBuf>,
    parent: &ParentDeadline,
) -> Result<(PathBuf, crate::external_workspace::DirectoryIdentity), String> {
    parent.check()?;
    let path = path.ok_or("external_trusted_tool_context_unavailable")?;
    if !path.is_absolute() {
        return Err("external_trusted_tool_context_refused".into());
    }
    text_path(&path)?;
    let check = || parent.check();
    crate::external_workspace::canonical_directory(&path, &parent.source_budget(&check)?)
        .map_err(|_| "external_trusted_tool_context_refused".into())
}
impl CompilerDiscoveryContext {
    fn discover_with(
        mut get: impl FnMut(&str) -> Option<PathBuf>,
        parent: &ParentDeadline,
    ) -> Result<Self, String> {
        let mut context = Self::default();
        let mut identities = Vec::new();
        // Rust's pinned MSVC finder uses these roots for its vswhere fallback.
        // They are discovery hints, not leases for nested linker or SDK files.
        for name in ["ProgramFiles(x86)", "ProgramFiles"] {
            let (path, identity) = trusted_directory(get(name), parent)?;
            let path = if let Some(index) = identities.iter().position(|held| held == &identity) {
                context.directories[index].clone()
            } else {
                identities.push(identity);
                context.directories.push(path.clone());
                path
            };
            context.env.push((name.into(), text_path(&path)?));
        }
        parent.check()?;
        Ok(context)
    }
}
impl Tools {
    fn discover(parent: &ParentDeadline) -> Result<Self, String> {
        let cargo = crate::toolchain::find_on_path("cargo").ok_or("external_cargo_unavailable")?;
        let rustc = crate::toolchain::find_on_path("rustc").ok_or("external_rustc_unavailable")?;
        let directory = |name: &str| -> Result<PathBuf, String> {
            trusted_directory(std::env::var_os(name).map(PathBuf::from), parent)
                .map(|(path, _)| path)
        };
        Ok(Self {
            cargo: Executable::open(cargo, parent)?,
            rustc: Executable::open(rustc, parent)?,
            system: directory("SystemRoot")?,
            rustup: directory("RUSTUP_HOME")?,
            discovery: CompilerDiscoveryContext::discover_with(
                |name| std::env::var_os(name).map(PathBuf::from),
                parent,
            )?,
        })
    }
}
struct CompilerState {
    root: PathBuf,
    cwd: PathBuf,
    target: PathBuf,
    build: PathBuf,
    directories: Vec<PathBuf>,
    output_directories: Vec<PathBuf>,
    files: Vec<PathBuf>,
    env: Vec<(String, String)>,
    tools: Tools,
}
fn text_path(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .filter(|v| !v.is_empty() && v.len() <= 4096 && !v.contains(['\0', '\n', '\r']))
        .ok_or("external_compiler_path_refused")?;
    Ok(value.replace('\\', "/"))
}
fn ancestor_configs_absent(cwd: &Path, parent: &ParentDeadline) -> Result<(), String> {
    for (depth, directory) in cwd.ancestors().enumerate() {
        parent.check()?;
        if depth > 64 {
            return Err("external_compiler_ancestor_bounds".into());
        }
        let cargo = directory.join(".cargo");
        match std::fs::symlink_metadata(&cargo) {
            Ok(metadata) => {
                use std::os::windows::fs::MetadataExt;
                if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                    return Err("external_compiler_ancestor_config_refused".into());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err("external_compiler_ancestor_config_refused".into()),
        }
        for name in ["config", "config.toml"] {
            match std::fs::symlink_metadata(directory.join(".cargo").join(name)) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err("external_compiler_ancestor_config_refused".into()),
            }
        }
    }
    parent.check()
}
fn copied_toolchain_absent(staged: &StagedSources, parent: &ParentDeadline) -> Result<(), String> {
    let mut pending: Vec<_> = staged
        .roots
        .iter()
        .map(|root| (root.staged.clone(), 0usize))
        .collect();
    let mut entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 32 {
            return Err("external_compiler_source_bounds".into());
        }
        parent.check()?;
        for entry in
            std::fs::read_dir(directory).map_err(|_| "external_compiler_source_unavailable")?
        {
            parent.check()?;
            entries += 1;
            if entries > 40_000 {
                return Err("external_compiler_source_bounds".into());
            }
            let entry = entry.map_err(|_| "external_compiler_source_unavailable")?;
            let name = entry.file_name();
            if name.eq_ignore_ascii_case("rust-toolchain")
                || name.eq_ignore_ascii_case("rust-toolchain.toml")
            {
                return Err("external_copied_toolchain_override_refused".into());
            }
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|_| "external_compiler_source_unavailable")?;
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("external_compiler_source_link_refused".into());
            }
            if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    parent.check()
}
impl CompilerState {
    fn compilation_lease(
        &self,
        staged: &StagedSources,
        parent: &ParentDeadline,
    ) -> Result<crate::external_workspace::CompilationLease, String> {
        let check = || parent.check();
        staged
            .compilation_lease(&parent.source_budget(&check)?)
            .map_err(|_| "external_compile_source_guards_refused")?
            .hold_compiler_state(
                &self.root,
                &self.directories,
                &self.output_directories,
                &self.files,
                &[
                    (
                        self.tools.cargo.invocation.clone(),
                        self.tools.cargo.application.clone(),
                    ),
                    (
                        self.tools.rustc.invocation.clone(),
                        self.tools.rustc.application.clone(),
                    ),
                ],
                &parent.source_budget(&check)?,
            )
    }
    fn create(
        workspace: &Workspace,
        staged: &StagedSources,
        tools: Tools,
        parent: &ParentDeadline,
    ) -> Result<Self, String> {
        parent.check()?;
        let (root, cwd) = crate::candidate_workspace::compiler_root_and_cwd(&workspace.path);
        crate::candidate_workspace::check_compiler_cwd(&cwd)?;
        copied_toolchain_absent(staged, parent)?;
        ancestor_configs_absent(&cwd, parent)?;
        let target = root.join("target");
        let build = root.join("build");
        let config = root.join("cargo-home/config.toml");
        let mut directories = vec![root.clone(), cwd.clone()];
        directories.extend(crate::external_workspace::compiler_output_directory_paths(
            &root,
        ));
        // Validate the complete generated layout before creating compiler state.
        // Only cwd has the CreateProcessW budget; other paths retain text_path bounds.
        for path in &directories {
            text_path(path)?;
        }
        text_path(&config)?;
        // Host-owned cwd is outside copied roots; same-user arbitrary writes are outside scope.
        let mut setup_guards = Vec::new();
        for path in &directories {
            parent.check()?;
            std::fs::create_dir(path).map_err(|_| "external_compiler_private_context_refused")?;
            setup_guards.push(
                crate::snapshot::open_checked_directory(path, parent.instant())
                    .map_err(|_| "external_compiler_private_context_refused")?,
            );
        }
        let output_directories = directories.split_off(2);
        // Container/cwd and trusted runtime roots retain their strict guards.
        directories.extend([tools.system.clone(), tools.rustup.clone()]);
        directories.extend(tools.discovery.directories.iter().cloned());
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(0x0020_0000)
            .share_mode(1)
            .open(&config)
            .map_err(|_| "external_compiler_private_config_refused")?;
        crate::snapshot::check_opened_path(&file, &config, parent.instant())
            .map_err(|_| "external_compiler_private_config_refused")?;
        file.write_all(b"[net]\noffline = true\n")
            .map_err(|_| "external_compiler_private_config_refused")?;
        file.sync_all()
            .map_err(|_| "external_compiler_private_config_refused")?;
        drop(file);
        let env = compiler_environment(&tools, &root)?;
        parent.check()?;
        Ok(Self {
            root,
            cwd,
            target,
            build,
            directories,
            output_directories,
            files: vec![config],
            env,
            tools,
        })
    }
    fn check(&self, parent: &ParentDeadline) -> Result<(), String> {
        ancestor_configs_absent(&self.cwd, parent)?;
        self.tools.cargo.recheck(parent)?;
        self.tools.rustc.recheck(parent)
    }
}
fn compiler_system_drive(system: &Path) -> Result<String, String> {
    use std::path::{Component, Prefix};
    let mut components = system.components();
    let drive = match (components.next(), components.next()) {
        (Some(Component::Prefix(prefix)), Some(Component::RootDir)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if drive.is_ascii_alphabetic() => {
                drive
            }
            _ => return Err("external_compiler_system_drive_refused".into()),
        },
        _ => return Err("external_compiler_system_drive_refused".into()),
    };
    Ok(format!("{}:", char::from(drive).to_ascii_uppercase()))
}
fn compiler_environment(tools: &Tools, root: &Path) -> Result<Vec<(String, String)>, String> {
    let path = std::env::join_paths([
        tools
            .cargo
            .invocation
            .parent()
            .ok_or("external_compiler_tool_parent")?,
        tools
            .rustc
            .invocation
            .parent()
            .ok_or("external_compiler_tool_parent")?,
        &tools.system.join("System32"),
    ])
    .map_err(|_| "external_compiler_path_refused")?
    .into_string()
    .map_err(|_| "external_compiler_path_refused")?;
    let mut env = vec![
        ("PATH".into(), path),
        ("SystemRoot".into(), text_path(&tools.system)?),
        ("WINDIR".into(), text_path(&tools.system)?),
        ("SystemDrive".into(), compiler_system_drive(&tools.system)?),
        ("RUSTUP_HOME".into(), text_path(&tools.rustup)?),
        ("RUSTUP_AUTO_INSTALL".into(), "0".into()),
        ("RUSTUP_NO_UPDATE_CHECK".into(), "1".into()),
        ("CARGO_INCREMENTAL".into(), "0".into()),
        ("CARGO_NET_OFFLINE".into(), "true".into()),
    ];
    env.extend(tools.discovery.env.iter().cloned());
    let target = root.join("target");
    let build = root.join("build");
    let home = root.join("profile");
    let cargo = root.join("cargo-home");
    let temp = root.join("temp");
    let roaming = home.join("AppData/Roaming");
    let local = home.join("AppData/Local");
    for (name, directory) in [
        ("CARGO_HOME", &cargo),
        ("CARGO_TARGET_DIR", &target),
        ("CARGO_BUILD_BUILD_DIR", &build),
        ("HOME", &home),
        ("USERPROFILE", &home),
        ("APPDATA", &roaming),
        ("LOCALAPPDATA", &local),
        ("XDG_CONFIG_HOME", &roaming),
        ("XDG_DATA_HOME", &local),
        ("TEMP", &temp),
        ("TMP", &temp),
    ] {
        env.push((name.into(), text_path(directory)?));
    }
    Ok(env)
}
fn bounded_file(
    path: &Path,
    limit: u64,
    parent: &ParentDeadline,
) -> Result<(File, Vec<u8>), String> {
    parent.check()?;
    let deadline = parent
        .instant()
        .min(Instant::now() + parent.budget(Duration::from_secs(60))?);
    let prior = crate::snapshot::open_checked_file(path, parent.instant())
        .map_err(|_| "external_compiler_checked_file_refused")?;
    let expected = prior
        .metadata()
        .map_err(|_| "external_compiler_checked_file_refused")?;
    if expected.len() == 0 || expected.len() > limit {
        return Err("external_compiler_file_bounds".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(0x0020_0000)
        .share_mode(1)
        .open(path)
        .map_err(|_| "external_compiler_checked_file_refused")?;
    crate::snapshot::check_opened_path(&file, path, parent.instant())
        .map_err(|_| "external_compiler_checked_file_refused")?;
    let before = file
        .metadata()
        .map_err(|_| "external_compiler_checked_file_refused")?;
    if before.len() != expected.len()
        || before.modified().ok() != expected.modified().ok()
        || before.created().ok() != expected.created().ok()
    {
        return Err("external_compiler_checked_file_changed".into());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        parent.check()?;
        if Instant::now() >= deadline {
            return Err("external_compile_hash_deadline".into());
        }
        let capacity =
            (before.len().saturating_sub(bytes.len() as u64) + 1).min(buffer.len() as u64) as usize;
        let count = file
            .read(&mut buffer[..capacity])
            .map_err(|_| "external_compiler_checked_file_refused")?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > before.len() as usize {
            return Err("external_compiler_checked_file_changed".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| "external_compiler_checked_file_refused")?;
    if bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.created().ok() != after.created().ok()
    {
        return Err("external_compiler_checked_file_changed".into());
    }
    crate::snapshot::check_opened_path(&file, path, parent.instant())
        .map_err(|_| "external_compiler_checked_file_changed")?;
    parent.check()?;
    if Instant::now() >= deadline {
        return Err("external_compile_hash_deadline".into());
    }
    Ok((file, bytes))
}
fn observe_storage(
    workspace: &Workspace,
    parent: &ParentDeadline,
    effective: Instant,
) -> io::Result<()> {
    parent.check().map_err(io::Error::other)?;
    let budget = effective
        .min(parent.instant())
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(2));
    crate::candidate_workspace::usage_with_budget(&workspace.path, WORKSPACE_BYTES, budget)
        .map_err(|_| io::Error::other("external_compile_storage_refused"))?;
    parent.check().map_err(io::Error::other)
}
struct Captured {
    bytes: Vec<u8>,
    receipt: Value,
}
struct Phase<'a> {
    failure: &'static str,
    tool: &'a Executable,
    args: &'a [String],
    limit: Duration,
    #[cfg(test)]
    checked_build: Option<&'a KnownCargoBuild>,
}
trait PhaseRunner {
    fn capture(
        &mut self,
        phase: Phase<'_>,
        state: &CompilerState,
        workspace: &Workspace,
        parent: &ParentDeadline,
    ) -> Result<Captured, String>;
}
struct RetainedRunner<'a>(&'a StreamController);
fn accepted_stream(
    receipt: &StreamReceipt,
    bytes: usize,
    parent: &ParentDeadline,
) -> Result<Value, String> {
    parent.check()?;
    if !receipt.stdout_eof
        || !receipt.stderr_eof
        || !receipt.owned_tree_retired
        || !receipt.owned_readers_joined
        || receipt.root_exit_code != 0
        || receipt.stdout_bytes != bytes as u64
        || receipt.stderr_bytes > STDERR_BYTES
    {
        return Err("external_compile_stream_receipt_refused".into());
    }
    Ok(
        json!({"stdout_bytes":receipt.stdout_bytes,"stderr_bytes":receipt.stderr_bytes,"stdout_eof":true,
        "stderr_eof":true,"root_exit_code":0,"owned_tree_retired":true,"owned_readers_joined":true,
        "raw_output_returned":false}),
    )
}
impl PhaseRunner for RetainedRunner<'_> {
    fn capture(
        &mut self,
        phase: Phase<'_>,
        state: &CompilerState,
        workspace: &Workspace,
        parent: &ParentDeadline,
    ) -> Result<Captured, String> {
        let Phase {
            failure: phase,
            tool,
            args,
            limit,
            #[cfg(test)]
                checked_build: _,
        } = phase;
        state.check(parent)?;
        let deadline = parent.instant().min(Instant::now() + parent.budget(limit)?);
        observe_storage(workspace, parent, deadline)
            .map_err(|_| "external_compile_storage_refused")?;
        let mut stream = Worker::spawn_binary_cleared(
            self.0,
            BinarySpawn {
                program: &tool.application,
                argv0: &tool.invocation,
                args,
                directory: Some(&state.cwd),
                overrides: &state.env,
                lifetime: &workspace.lifetime,
                deadline,
                limits: StreamLimits {
                    stdout_bytes: OUTPUT_BYTES as u64,
                    stderr_bytes: STDERR_BYTES,
                },
            },
        )
        .map_err(|_| phase)?;
        let result = (|| {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut next_scan = Instant::now();
            let mut check = |effective| {
                parent.check().map_err(io::Error::other)?;
                if Instant::now() >= next_scan {
                    observe_storage(workspace, parent, effective)?;
                    next_scan = Instant::now() + Duration::from_millis(100);
                }
                Ok(())
            };
            loop {
                let count = stream
                    .read_before_checked(&mut buffer, deadline, &mut check)
                    .map_err(|_| phase)?;
                if count == 0 {
                    break;
                }
                if bytes.len() + count > OUTPUT_BYTES {
                    return Err("external_compile_output_bounds".into());
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let receipt = stream.finish_checked(&mut check).map_err(|_| phase)?;
            parent.check()?;
            if Instant::now() >= deadline {
                return Err("external_compile_phase_deadline".into());
            }
            observe_storage(workspace, parent, deadline)
                .map_err(|_| "external_compile_storage_refused")?;
            state.check(parent)?;
            Ok(Captured {
                receipt: accepted_stream(&receipt, bytes.len(), parent)?,
                bytes,
            })
        })();
        if result.is_err() {
            let _ = stream.retire();
        }
        drop(stream); // unresolved resources park in the bounded original-object pool
        if workspace.lifetime.unresolved() {
            return Err("external_compile_retirement_unconfirmed".into());
        }
        result
    }
}
#[cfg(test)]
impl RetainedRunner<'_> {
    fn capture_known_build(
        &mut self,
        phase: Phase<'_>,
        state: &CompilerState,
        workspace: &Workspace,
        parent: &ParentDeadline,
        diagnostic: &crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle,
    ) -> Result<Captured, String> {
        let Phase {
            failure: phase,
            tool,
            args,
            limit,
            checked_build,
        } = phase;
        if phase != "external_cargo_build_failed" {
            return Err("test_known_cargo_phase_refused".into());
        }
        let checked_build = checked_build.ok_or("test_known_cargo_selected_build_seal_missing")?;
        if let Err(error) = state.check(parent) {
            diagnostic.scope_refusal("state_before_build");
            return Err(error);
        }
        let deadline = parent.instant().min(Instant::now() + parent.budget(limit)?);
        observe_storage(workspace, parent, deadline)
            .map_err(|_| "external_compile_storage_refused")?;
        let bound = diagnostic.bind_build(
            self.0,
            checked_build,
            BinarySpawn {
                program: &tool.application,
                argv0: &tool.invocation,
                args,
                directory: Some(&state.cwd),
                overrides: &state.env,
                lifetime: &workspace.lifetime,
                deadline,
                limits: StreamLimits {
                    stdout_bytes: OUTPUT_BYTES as u64,
                    stderr_bytes: STDERR_BYTES,
                },
            },
        );
        diagnostic.api(
            crate::worker_windows::binary::real_cargo_diagnostic::Stage::Spawn,
            &bound,
        );
        let mut stream = bound.map_err(|_| phase)?.spawn().map_err(|_| phase)?;
        let result = (|| {
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut next_scan = Instant::now();
            let mut check = |effective| {
                parent.check().map_err(io::Error::other)?;
                if Instant::now() >= next_scan {
                    observe_storage(workspace, parent, effective)?;
                    next_scan = Instant::now() + Duration::from_millis(100);
                }
                Ok(())
            };
            loop {
                let count = stream
                    .read_before_checked(&mut buffer, deadline, &mut check)
                    .map_err(|_| phase)?;
                if count == 0 {
                    break;
                }
                if bytes.len() + count > OUTPUT_BYTES {
                    return Err("external_compile_output_bounds".into());
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            let receipt = stream.finish_checked(&mut check).map_err(|_| phase)?;
            parent.check()?;
            if Instant::now() >= deadline {
                return Err("external_compile_phase_deadline".into());
            }
            observe_storage(workspace, parent, deadline)
                .map_err(|_| "external_compile_storage_refused")?;
            state.check(parent)?;
            Ok(Captured {
                receipt: accepted_stream(&receipt, bytes.len(), parent)?,
                bytes,
            })
        })();
        if result.is_err() {
            diagnostic.scope_refusal("build_result_refused");
            let _ = stream.retire();
        }
        drop(stream); // unresolved resources park in the bounded original-object pool
        if workspace.lifetime.unresolved() {
            return Err("external_compile_retirement_unconfirmed".into());
        }
        result
    }
}
// Only this retained compiler module can construct the checked Cargo tuple.
#[cfg(test)]
pub(crate) struct KnownCargoInvocation {
    program: PathBuf,
    argv0: PathBuf,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    target: PathBuf,
    application_sha256: String,
    parent_deadline: Instant,
}
#[cfg(test)]
impl KnownCargoInvocation {
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }
    pub(crate) fn argv0(&self) -> &Path {
        &self.argv0
    }
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub(crate) fn env(&self) -> &[(String, String)] {
        &self.env
    }
    pub(crate) fn target(&self) -> &Path {
        &self.target
    }
    pub(crate) fn application_sha256(&self) -> &str {
        &self.application_sha256
    }
    pub(crate) fn parent_deadline(&self) -> Instant {
        self.parent_deadline
    }
}
// The constructor is private to this module and is called only after the
// original guarded metadata selection and accepted compiler-host check.
// It never receives a BinarySpawn or attach-request argument vector.
#[cfg(test)]
pub(crate) struct KnownCargoBuild {
    manifest: String,
    package_id: String,
    target_triple: String,
    target_directory: String,
}
#[cfg(test)]
impl KnownCargoBuild {
    fn after_checked_selection(
        selected: &SelectedMember,
        host: &str,
        manifest: &Path,
        target: &Path,
    ) -> Result<Self, String> {
        Ok(Self {
            manifest: text_path(manifest)?,
            package_id: selected.package_id.clone(),
            target_triple: host.to_owned(),
            target_directory: text_path(target)?,
        })
    }
    pub(crate) fn belongs_to(&self, manifest: &Path, target: &Path) -> bool {
        Path::new(&self.manifest) == manifest && Path::new(&self.target_directory) == target
    }
    pub(crate) fn arguments(&self) -> Vec<String> {
        vec![
            "build".into(),
            "--manifest-path".into(),
            self.manifest.clone(),
            "--lib".into(),
            "--frozen".into(),
            "--offline".into(),
            "--jobs".into(),
            "1".into(),
            "--package".into(),
            self.package_id.clone(),
            "--target".into(),
            self.target_triple.clone(),
            "--target-dir".into(),
            self.target_directory.clone(),
            "--config".into(),
            "profile.dev.debug=0".into(),
            "--config".into(),
            "profile.dev.codegen-units=1".into(),
            "--message-format=json".into(),
            "--color".into(),
            "never".into(),
        ]
    }
}
#[cfg(test)]
struct KnownCargoRunner<'a> {
    inner: RetainedRunner<'a>,
    diagnostic: &'a crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle,
}
#[cfg(test)]
impl PhaseRunner for KnownCargoRunner<'_> {
    fn capture(
        &mut self,
        phase: Phase<'_>,
        state: &CompilerState,
        workspace: &Workspace,
        parent: &ParentDeadline,
    ) -> Result<Captured, String> {
        if phase.failure == "external_cargo_build_failed" {
            self.inner
                .capture_known_build(phase, state, workspace, parent, self.diagnostic)
        } else {
            let result = self.inner.capture(phase, state, workspace, parent);
            if result.is_err() {
                self.diagnostic.scope_refusal("non_build_phase_refused");
            }
            result
        }
    }
}
#[cfg(test)]
fn diagnostic_io(error: &io::Error) -> Value {
    // Error Display/source chains can contain paths, environment or private data.
    json!({"io_kind":format!("{:?}",error.kind()),"raw_os_code":error.raw_os_error()})
}
#[cfg(test)]
impl RetainedRunner<'_> {
    fn rustc_vv_diagnostic(
        &mut self,
        state: &CompilerState,
        workspace: &Workspace,
        parent: &ParentDeadline,
    ) -> Value {
        let mut report = json!({"phase":"rustc_vV","phase_api_succeeded":false,
            "primary_stage":"precheck","worker_api_call":"NOT_ATTEMPTED","stream_returned":false,"native_birth":"UNAVAILABLE",
            "stderr":"UNAVAILABLE_BY_CURRENT_BINARY_API","retirement_receipt":"UNAVAILABLE",
            "host_parse":"NOT_INVOKED","metadata_or_build":"NOT_INVOKED"});
        report["tool_binding"] = json!({"application_sha256":state.tools.rustc.sha256,
            "invocation_target_application_sha256":state.tools.rustc.sha256,
            "application_utf16_units":state.tools.rustc.application.as_os_str().to_string_lossy().encode_utf16().count(),
            "argv0_invocation_utf16_units":state.tools.rustc.invocation.as_os_str().to_string_lossy().encode_utf16().count(),
            "cwd_utf16_units":state.cwd.as_os_str().to_string_lossy().encode_utf16().count(),
            "curated_directory_lengths":state.env.iter().filter(|(name,_)|matches!(name.as_str(),"CARGO_HOME"|"HOME"|"TEMP"|"RUSTUP_HOME"))
                .map(|(name,value)|json!({"name":name,"utf16_units":value.encode_utf16().count()})).collect::<Vec<_>>(),
            "program_is_canonical_application":true,"argv0_is_original_invocation":true});
        if state.check(parent).is_err() {
            return report;
        }
        report["tool_binding_rechecked"] = json!(true);
        let remaining = match parent.budget(Duration::from_secs(10)) {
            Ok(value) => value,
            Err(_) => return report,
        };
        let deadline = parent.instant().min(Instant::now() + remaining);
        if observe_storage(workspace, parent, deadline).is_err() {
            report["primary_stage"] = json!("storage_before_spawn");
            return report;
        }
        report["worker_api_call"] = json!("ATTEMPTED");
        let mut stream = match Worker::spawn_binary_cleared(
            self.0,
            BinarySpawn {
                program: &state.tools.rustc.application,
                argv0: &state.tools.rustc.invocation,
                args: &["-vV".into()],
                directory: Some(&state.cwd),
                overrides: &state.env,
                lifetime: &workspace.lifetime,
                deadline,
                limits: StreamLimits {
                    stdout_bytes: OUTPUT_BYTES as u64,
                    stderr_bytes: STDERR_BYTES,
                },
            },
        ) {
            Ok(stream) => stream,
            Err(error) => {
                report["primary_stage"] = json!("spawn_api");
                report["primary_io"] = diagnostic_io(&error);
                report["admission_scope"]=json!("reserve/claim/native creator/reader startup may fail; no birth classification available");
                report["workspace_lifetime_unresolved"] = json!(workspace.lifetime.unresolved());
                report["own_controller_pending_count"] = json!(self.0.retained());
                return report;
            }
        };
        report["stream_returned"] = json!(true);
        report["native_birth"] = json!("ADMITTED_STREAM_RETURNED");
        let mut prefix = Vec::new();
        let mut total = 0usize;
        let mut buffer = [0u8; 64 * 1024];
        let mut next_scan = Instant::now();
        let mut check = |effective| {
            parent.check().map_err(io::Error::other)?;
            if Instant::now() >= next_scan {
                observe_storage(workspace, parent, effective)?;
                next_scan = Instant::now() + Duration::from_millis(100);
            }
            Ok(())
        };
        let mut eof = false;
        loop {
            match stream.read_before_checked(&mut buffer, deadline, &mut check) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(count) => {
                    total += count;
                    if total > OUTPUT_BYTES {
                        report["primary_stage"] = json!("stdout_bounds");
                        break;
                    }
                    let kept = count.min(8192usize.saturating_sub(prefix.len()));
                    prefix.extend_from_slice(&buffer[..kept]);
                }
                Err(error) => {
                    report["primary_stage"] = json!("read_api");
                    report["primary_io"] = diagnostic_io(&error);
                    break;
                }
            }
        }
        report["stdout_shape"] = json!({"bytes_read":total,"eof_observed":eof,"prefix_cap":8192,
            "prefix_bytes":prefix.len(),"prefix_sha256":crate::sha256_hex(&prefix),
            "prefix_utf8":std::str::from_utf8(&prefix).is_ok(),"prefix_truncated":total>prefix.len(),"raw_output_retained":false});
        if eof {
            match stream.finish_checked(&mut check) {
                Ok(receipt) => {
                    report["retirement_receipt"] = json!({"stdout_bytes":receipt.stdout_bytes,"stderr_bytes":receipt.stderr_bytes,
                        "stdout_eof":receipt.stdout_eof,"stderr_eof":receipt.stderr_eof,"root_exit_code":receipt.root_exit_code,
                        "owned_tree_retired":receipt.owned_tree_retired,"owned_readers_joined":receipt.owned_readers_joined});
                    if accepted_stream(&receipt, total, parent).is_ok()
                        && state.check(parent).is_ok()
                        && observe_storage(workspace, parent, deadline).is_ok()
                        && Instant::now() < deadline
                    {
                        report["primary_stage"] = json!("complete");
                        report["phase_api_succeeded"] = json!(true);
                    } else {
                        report["primary_stage"] = json!("post_finish_checks");
                    }
                }
                Err(error) => {
                    report["primary_stage"] = json!("finish_api");
                    report["primary_io"] = diagnostic_io(&error);
                }
            }
        }
        // This result is separate from primary failure. Ok on an empty stream
        // cannot manufacture the receipt discarded by finish_checked on error.
        report["later_retire_call"] = match stream.retire() {
            Ok(()) => json!({"api_ok":true,"original_receipt_inferred":false}),
            Err(error) => {
                json!({"api_ok":false,"io":diagnostic_io(&error),"original_receipt_inferred":false})
            }
        };
        drop(stream); // existing Drop parks unresolved original resources
        report["workspace_lifetime_unresolved"] = json!(workspace.lifetime.unresolved());
        report["own_controller_pending_count"] = json!(self.0.retained());
        report
    }
}
#[cfg(test)]
pub(crate) fn rustc_vv_diagnostic_for_test(
    workspace: &mut Workspace,
    staged: &StagedSources,
) -> Value {
    let failed = |stage| {
        json!({"phase":"rustc_vV","phase_api_succeeded":false,"primary_stage":stage,
        "stream_returned":false,"worker_call":"NOT_ATTEMPTED","stderr":"UNAVAILABLE_BY_CURRENT_BINARY_API",
        "retirement_receipt":"UNAVAILABLE","prepare_error_details":"REDACTED"})
    };
    let parent = match ParentDeadline::at(Instant::now() + Duration::from_secs(180)) {
        Ok(parent) => parent,
        Err(_) => return failed("parent_preparation"),
    };
    let tools = match Tools::discover(&parent) {
        Ok(tools) => tools,
        Err(_) => return failed("tool_discovery"),
    };
    let state = match CompilerState::create(workspace, staged, tools, &parent) {
        Ok(state) => state,
        Err(_) => return failed("compiler_state"),
    };
    let check = || parent.check();
    let lease = state.compilation_lease(staged, &parent);
    let lease = match lease {
        Ok(lease) => Arc::new(lease),
        Err(_) => return failed("source_and_tool_lease"),
    };
    if workspace.retain_compilation(lease.clone()).is_err() {
        return failed("lease_retention");
    }
    let controller = StreamController::for_runtime();
    let mut runner = RetainedRunner(&controller);
    let mut report = runner.rustc_vv_diagnostic(&state, workspace, &parent);
    let verified = parent
        .source_budget(&check)
        .and_then(|budget| lease.verify(&budget))
        .is_ok();
    report["leases_verified_after"] = json!(verified);
    if report["phase_api_succeeded"] == true && !verified {
        report["phase_api_succeeded"] = json!(false);
        report["primary_stage"] = json!("post_phase_lease_verify");
    }
    report["one_phase_only"] = json!(true);
    report
}
fn strict_text(bytes: &[u8], parent: &ParentDeadline) -> Result<String, String> {
    parent.check()?;
    if bytes.is_empty() || bytes.len() > OUTPUT_BYTES {
        return Err("external_compile_output_bounds".into());
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| "external_compile_output_not_utf8".into())
}
pub(crate) fn exact_artifact(
    stdout: &str,
    selected: &SelectedMember,
    target: &Path,
    lease: &CompilationLease,
    parent: &ParentDeadline,
) -> Result<(PathBuf, File, Vec<u8>), String> {
    let mut finished = 0;
    let mut matching = 0;
    let mut artifact = None;
    let check = || parent.check();
    let budget = parent.source_budget(&check)?;
    let canonical_manifest =
        lease.checked_metadata_file(&selected.manifest.to_string_lossy(), &budget)?;
    let canonical_source =
        lease.checked_metadata_file(&selected.source.to_string_lossy(), &budget)?;
    for line in stdout.lines() {
        parent.check()?;
        if line.is_empty() || line.len() > 1024 * 1024 {
            return Err("external_compile_build_json_refused".into());
        }
        let value: Value =
            serde_json::from_str(line).map_err(|_| "external_compile_build_json_refused")?;
        match value["reason"].as_str() {
            Some("build-finished") => {
                if value["success"] != true {
                    return Err("external_compile_build_failed".into());
                }
                finished += 1;
            }
            Some("compiler-artifact") => {
                if value["package_id"] == selected.package_id
                    && value["target"]["name"] == selected.target
                {
                    for (reported, expected) in [
                        (value["manifest_path"].as_str(), &canonical_manifest),
                        (value["target"]["src_path"].as_str(), &canonical_source),
                    ] {
                        let reported =
                            reported.ok_or("external_compile_artifact_source_missing")?;
                        let checked = lease.checked_metadata_file(reported, &budget);
                        #[cfg(test)]
                        metadata_file_diagnostic(
                            if expected == &canonical_manifest {
                                "artifact_manifest"
                            } else {
                                "artifact_source"
                            },
                            reported,
                            expected,
                            &checked,
                        );
                        let checked = checked?;
                        if !crate::ownership::paths_equal(&checked, expected) {
                            return Err("external_compile_artifact_source_changed".into());
                        }
                    }
                    if !value["target"]["crate_types"]
                        .as_array()
                        .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"))
                        || !value["filenames"]
                            .as_array()
                            .is_some_and(|paths| paths.iter().all(Value::is_string))
                        || value["filenames"].as_array().map(|paths| {
                            paths
                                .iter()
                                .filter(|path| {
                                    path.as_str().is_some_and(|path| {
                                        Path::new(path)
                                            .extension()
                                            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
                                    })
                                })
                                .count()
                        }) != Some(1)
                    {
                        return Err("external_compile_exact_artifact_refused".into());
                    }
                    matching += 1;
                    let filename = value["filenames"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(Value::as_str)
                        .find(|path| {
                            Path::new(path)
                                .extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
                        })
                        .ok_or("external_compile_exact_artifact_refused")?;
                    let (path, checked_guard) =
                        lease.checked_artifact_file(target, filename, &budget)?;
                    let (guard, bytes) = bounded_file(&path, 128 * 1024 * 1024, parent)?;
                    crate::snapshot::check_opened_path(&checked_guard, &path, parent.instant())
                        .map_err(|_| "external_compile_artifact_identity_changed")?;
                    artifact = Some((path, guard, bytes));
                }
            }
            Some("compiler-message") => {}
            _ => return Err("external_compile_build_json_refused".into()),
        }
    }
    if finished != 1 || matching != 1 {
        return Err("external_compile_exact_artifact_missing".into());
    }
    artifact.ok_or("external_compile_exact_artifact_missing".into())
}
fn validate_inner(
    workspace: &mut Workspace,
    staged: &StagedSources,
    parent: &ParentDeadline,
    state: CompilerState,
    runner: &mut impl PhaseRunner,
    provenance_request: Option<RealProvenanceRequest>,
) -> Result<OwnedNativeValidation, String> {
    let check = || parent.check();
    let lease = state.compilation_lease(staged, parent)?;
    let lease = Arc::new(lease);
    workspace.retain_compilation(lease.clone())?;
    let verify = || -> Result<(), String> {
        lease
            .verify(&parent.source_budget(&check)?)
            .map_err(|_| "external_compile_source_or_guard_changed".into())
    };
    verify()?;
    let version = runner.capture(
        Phase {
            failure: "external_rustc_host_failed",
            tool: &state.tools.rustc,
            args: &["-vV".into()],
            limit: Duration::from_secs(10),
            #[cfg(test)]
            checked_build: None,
        },
        &state,
        workspace,
        parent,
    )?;
    parent.check()?;
    verify()?;
    let host = compiler_host(&strict_text(&version.bytes, parent)?)?;
    let manifest = text_path(&staged.member_manifest)?;
    let metadata_args = vec![
        "metadata".into(),
        "--manifest-path".into(),
        manifest.clone(),
        "--frozen".into(),
        "--offline".into(),
        "--format-version".into(),
        "1".into(),
        "--filter-platform".into(),
        host.clone(),
    ];
    let metadata = runner.capture(
        Phase {
            failure: "external_cargo_metadata_failed",
            tool: &state.tools.cargo,
            args: &metadata_args,
            limit: Duration::from_secs(30),
            #[cfg(test)]
            checked_build: None,
        },
        &state,
        workspace,
        parent,
    )?;
    parent.check()?;
    verify()?;
    let metadata_value: Value = serde_json::from_str(&strict_text(&metadata.bytes, parent)?)
        .map_err(|_| "external_compile_metadata_json_refused")?;
    for (name, expected) in [
        ("target_directory", &state.target),
        ("build_directory", &state.build),
    ] {
        if let Some(path) = metadata_value[name].as_str() {
            let output = lease.verify_compiler_output_directory(
                expected,
                path,
                &parent.source_budget(&check)?,
            );
            #[cfg(test)]
            eprintln!(
                "external_compile_metadata_directory field={name} legacy_spelling_match={} checked_directory_accepted={} reported_shape={} expected_shape={} failure_tag={}",
                crate::ownership::paths_equal(Path::new(path), expected),
                output.is_ok(),
                metadata_directory_shape(path),
                metadata_directory_shape(&expected.to_string_lossy()),
                output.as_ref().err().map_or("none", String::as_str),
            );
            if output.is_err() {
                return Err("external_compile_metadata_output_refused".into());
            }
        } else if name == "target_directory" {
            return Err("external_compile_metadata_output_refused".into());
        }
    }
    let selected = selected_member(
        staged,
        &metadata_value,
        &lease,
        &parent.source_budget(&check)?,
    )?;
    let args = vec![
        "build".into(),
        "--manifest-path".into(),
        manifest,
        "--lib".into(),
        "--frozen".into(),
        "--offline".into(),
        "--jobs".into(),
        "1".into(),
        "--package".into(),
        selected.package_id.clone(),
        "--target".into(),
        host.clone(),
        "--target-dir".into(),
        text_path(&state.target)?,
        "--config".into(),
        "profile.dev.debug=0".into(),
        "--config".into(),
        "profile.dev.codegen-units=1".into(),
        "--message-format=json".into(),
        "--color".into(),
        "never".into(),
    ];
    #[cfg(test)]
    let checked_build = KnownCargoBuild::after_checked_selection(
        &selected,
        &host,
        &staged.member_manifest,
        &state.target,
    )?;
    let build = runner.capture(
        Phase {
            failure: "external_cargo_build_failed",
            tool: &state.tools.cargo,
            args: &args,
            limit: Duration::from_secs(120),
            #[cfg(test)]
            checked_build: Some(&checked_build),
        },
        &state,
        workspace,
        parent,
    )?;
    parent.check()?;
    verify()?;
    let (artifact, artifact_guard, bytes) = exact_artifact(
        &strict_text(&build.bytes, parent)?,
        &selected,
        &state.target,
        &lease,
        parent,
    )?;
    let mut receipt = pe_receipt(&bytes, &host)?;
    receipt["path"] =
        json!(text_path(artifact.strip_prefix(&workspace.path).map_err(
            |_| "external_compile_artifact_confinement_refused"
        )?)?);
    parent.check()?;
    verify()?;
    state.check(parent)?;
    observe_storage(workspace, parent, parent.instant())
        .map_err(|_| "external_compile_storage_refused")?;
    crate::snapshot::check_opened_path(&artifact_guard, &artifact, parent.instant())
        .map_err(|_| "external_compile_artifact_identity_changed")?;
    let artifact_hash = receipt["sha256"]
        .as_str()
        .ok_or("external_compile_artifact_hash_missing")?
        .to_owned();
    let selected_package = selected.package.clone();
    let selected_target = selected.target.clone();
    let provenance = if let Some(_request) = provenance_request {
        let budget = parent.source_budget(&check)?;
        lease.verify(&budget)?;
        let resolution = crate::cargo_graph::capture(&metadata_value, &selected.package_id, &host)?;
        let inputs = lease.native_source_identity(&selected.manifest, &selected.source, &budget)?;
        let local_builds = observed_local_builds(
            &strict_text(&build.bytes, parent)?, &metadata_value, &selected, &inputs, &budget,
        )?;
        let local_outputs=hold_reported_local_outputs(&strict_text(&build.bytes,parent)?,&local_builds,&inputs,&lease,&state.target,&state.build,&budget)?;
        let mut retained = NativeCompilationProvenance {
            selected, host: host.clone(), inputs, resolution, local_builds, local_outputs,
            artifact: artifact.clone(), artifact_hash: artifact_hash.clone(), digest: String::new(),
        };
        retained.digest = retained.computed_digest()?;
        retained.recheck(staged, &artifact, &artifact_hash, &budget)?;
        lease.verify(&budget)?;
        parent.check()?;
        Some(retained)
    } else {
        // Fake and Value-only diagnostic runners cannot issue production provenance.
        None
    };
    let receipt = json!({"validation":"private_local_native_compile","cargo_executed":true,"native_artifact_produced":true,
        "host":host,"selected_package":selected_package,"selected_target":selected_target,"artifact":receipt,
        "streams":{"rustc":version.receipt,"metadata":metadata.receipt,"build":build.receipt},
        "compiler":{"cargo_application_sha256":state.tools.cargo.sha256,"rustc_application_sha256":state.tools.rustc.sha256,
        "environment_cleared":true,"caller_cache_used":false,"exact_build_units_verified":false},
        "tool_hash_provenance":"launched application or rustup proxy; selected toolchain identity unverified",
        "originals_mutated":false,"manifest_transformed":false,"loaded":false,"activated":false,"published":false,
        "source_publish":false,"execution_sandboxed":false,"filesystem_atomicity":"unverified"});
    Ok(OwnedNativeValidation {
        receipt,
        artifact,
        artifact_guard,
        artifact_hash,
        provenance,
    })
}
pub(crate) fn validate(
    workspace: &mut Workspace,
    staged: &StagedSources,
    deadline: Instant,
    controller: &StreamController,
) -> Result<Value, String> {
    validate_owned(workspace, staged, deadline, controller).map(OwnedNativeValidation::into_receipt)
}

pub(crate) fn validate_owned(
    workspace: &mut Workspace,
    staged: &StagedSources,
    deadline: Instant,
    controller: &StreamController,
) -> Result<OwnedNativeValidation, String> {
    let parent = ParentDeadline::at(deadline)?;
    validate_owned_with_parent(workspace, staged, &parent, controller)
}

pub(crate) fn validate_owned_with_parent(
    workspace: &mut Workspace,
    staged: &StagedSources,
    parent: &ParentDeadline,
    controller: &StreamController,
) -> Result<OwnedNativeValidation, String> {
    parent.check()?;
    let tools = Tools::discover(parent)?;
    let state = CompilerState::create(workspace, staged, tools, parent)?;
    validate_inner(
        workspace,
        staged,
        parent,
        state,
        &mut RetainedRunner(controller),
        Some(RealProvenanceRequest),
    )
}

/// Fixture-only tap of the same real producer. No replacement parent or runner grant.
#[cfg(test)]
pub(crate) fn validate_owned_with_parent_diagnostic_for_test(
    workspace: &mut Workspace,
    staged: &StagedSources,
    parent: &ParentDeadline,
    controller: &StreamController,
    permit: crate::external_workspace::KnownCargoFixturePermit,
    retained: &mut Option<std::mem::ManuallyDrop<
        crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle,
    >>,
) -> Result<OwnedNativeValidation, String> {
    parent.check()?;
    if retained.is_some() { return Err("test_known_cargo_handle_already_retained".into()); }
    let tools = Tools::discover(parent)?;
    let state = CompilerState::create(workspace, staged, tools, parent)?;
    let invocation = KnownCargoInvocation {
        program: state.tools.cargo.application.clone(),
        argv0: state.tools.cargo.invocation.clone(),
        cwd: state.cwd.clone(), env: state.env.clone(), target: state.target.clone(),
        application_sha256: state.tools.cargo.sha256.clone(),
        parent_deadline: parent.instant(),
    };
    let handle = crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle::for_fixture(
        permit, workspace, staged, controller, invocation,
    ).map_err(|_| "test_known_cargo_permit_refused")?;
    *retained = Some(std::mem::ManuallyDrop::new(handle));
    let diagnostic = retained.as_ref().unwrap();
    let result = validate_inner(workspace, staged, parent, state,
        &mut KnownCargoRunner { inner: RetainedRunner(controller), diagnostic },
        Some(RealProvenanceRequest));
    if result.is_err() { diagnostic.scope_refusal("validation_result_refused"); }
    result
}

#[cfg(test)]
pub(crate) fn compiler_directory_sharing_for_test(
    workspace: &Workspace,
    staged: &StagedSources,
    deadline: Instant,
    writable_outputs: bool,
) -> Result<(), String> {
    let parent = ParentDeadline::at(deadline)?;
    let application = workspace.path.join("directory-guard-fixture.exe");
    std::fs::write(&application, tests::fixture_pe()).unwrap();
    let discovery_roots = [
        workspace.path.join("program-files-x86"),
        workspace.path.join("program-files"),
    ];
    for path in &discovery_roots {
        std::fs::create_dir(path).unwrap();
    }
    let tools = Tools {
        cargo: Executable::open(application.clone(), &parent)?,
        rustc: Executable::open(application, &parent)?,
        system: workspace.path.clone(),
        rustup: workspace.path.clone(),
        discovery: CompilerDiscoveryContext::discover_with(
            |name| Some(discovery_roots[usize::from(name == "ProgramFiles")].clone()),
            &parent,
        )?,
    };
    let state = CompilerState::create(workspace, staged, tools, &parent)?;
    let lease = state.compilation_lease(staged, &parent)?;
    let check = || parent.check();
    let budget = parent.source_budget(&check)?;
    if writable_outputs {
        for (index, path) in state.output_directories.iter().enumerate() {
            parent.check()?;
            // A compatible writer makes a transient shareREAD-only reopen fail.
            let writer = OpenOptions::new()
                .write(true)
                .share_mode(3)
                .custom_flags(0x0220_0000)
                .open(path)
                .unwrap();
            lease.verify(&budget)?;
            lease.verify_compiler_output_directory(path, &path.to_string_lossy(), &budget)?;
            drop(writer);
            let temporary = path.join("cargo-layout-temporary");
            let destination = path.join("cargo-layout-destination");
            std::fs::create_dir(&temporary).unwrap();
            std::fs::rename(&temporary, &destination).unwrap();
            assert!(!temporary.exists() && destination.is_dir());
            let moved = path
                .parent()
                .unwrap()
                .join(format!("held-output-moved-{index}"));
            let refusal = std::fs::rename(path, &moved).unwrap_err();
            assert_eq!(refusal.raw_os_error(), Some(32));
            assert!(path.is_dir() && !moved.exists());
        }
        lease.verify(&budget)?;
        let released = state.root.join("profile/AppData/Local");
        let moved = state.root.join("profile/AppData/Local-after-release");
        drop(lease);
        std::fs::rename(&released, &moved).unwrap();
        assert!(!released.exists() && moved.is_dir());
    } else {
        for directory in [
            &state.cwd,
            &state.tools.system,
            &state.tools.rustup,
            &state.tools.discovery.directories[0],
            &state.tools.discovery.directories[1],
            &staged.roots[0].source,
        ] {
            parent.check()?;
            let moved = directory.with_file_name("immutable-directory-moved");
            assert_eq!(
                std::fs::rename(directory, &moved)
                    .unwrap_err()
                    .raw_os_error(),
                Some(32)
            );
            assert!(directory.is_dir() && !moved.exists());
        }
        for path in [
            &state.files[0],
            &state.tools.cargo.application,
            &staged.member_manifest,
        ] {
            parent.check()?;
            let refusal = OpenOptions::new()
                .write(true)
                .share_mode(3)
                .open(path)
                .unwrap_err();
            assert_eq!(refusal.raw_os_error(), Some(32));
        }
        lease.verify(&budget)?;
        lease.checked_metadata_workspace(
            &staged.envelope,
            &staged.envelope.to_string_lossy(),
            &budget,
        )?;
    }
    parent.check()
}

#[cfg(test)]
pub(crate) fn fake_validation_for_test(
    workspace: &mut Workspace,
    staged: &StagedSources,
    scenario: &str,
    pending: impl FnOnce(&mut Workspace),
) -> Result<Value, String> {
    struct Fake<'a> {
        staged: &'a StagedSources,
        scenario: &'a str,
        claim: Option<crate::candidate_workspace::LifetimeClaim>,
    }
    impl PhaseRunner for Fake<'_> {
        fn capture(
            &mut self,
            phase: Phase<'_>,
            state: &CompilerState,
            workspace: &Workspace,
            parent: &ParentDeadline,
        ) -> Result<Captured, String> {
            parent.check()?;
            assert!(!state.cwd.starts_with(&self.staged.envelope));
            assert!(phase.tool.invocation.is_absolute());
            if self.scenario == "unconfirmed" {
                self.claim = Some(workspace.lifetime.claim().unwrap());
                return Err("external_compile_retirement_unconfirmed".into());
            }
            let value = match phase.args[0].as_str() {
                "-vV" => {
                    if self.scenario == "invalid_utf8" {
                        return Ok(Captured {
                            bytes: vec![0xff],
                            receipt: json!({"test_simulated":true}),
                        });
                    }
                    "host: x86_64-pc-windows-msvc\n".to_owned()
                }
                "metadata" => {
                    assert!(phase.args.iter().any(|arg| arg == "--frozen"));
                    assert!(phase.args.iter().any(|arg| arg == "--offline"));
                    json!({"version":1,"workspace_root":self.staged.cargo_manifest.parent().unwrap(),
                        "workspace_members":["native-id"],"target_directory":state.target,"build_directory":state.build,
                        "packages":[{"name":"native-game","id":"native-id","source":null,
                            "manifest_path":self.staged.member_manifest,"targets":[{"name":"external_logic","kind":["cdylib"],
                            "crate_types":["cdylib"],"src_path":self.staged.member_manifest.parent().unwrap().join("src/lib.rs")}]},
                            {"name":"value-lib","id":"support-id","source":null,"manifest_path":self.staged.envelope.join("vendor/value-lib/Cargo.toml"),
                            "targets":[{"name":"value_lib","kind":["lib"],"crate_types":["lib"],"src_path":self.staged.envelope.join("vendor/value-lib/src/lib.rs")}]}]}).to_string()
                }
                "build" => {
                    if self.scenario == "build_failure" {
                        return Err(phase.failure.into());
                    }
                    assert!(phase.args.windows(2).any(|pair| pair == ["--jobs", "1"]));
                    let path = state.target.join("external_logic.dll");
                    std::fs::write(&path, tests::fixture_pe()).unwrap();
                    format!(
                        "{}\n{}\n",
                        json!({"reason":"compiler-artifact","package_id":"native-id",
                        "manifest_path":self.staged.member_manifest,
                        "target":{"name":"external_logic","crate_types":["cdylib"],
                            "src_path":self.staged.member_manifest.parent().unwrap().join("src/lib.rs")},"filenames":[path]}),
                        json!({"reason":"build-finished","success":true})
                    )
                }
                _ => panic!("unexpected simulated phase"),
            };
            Ok(Captured {
                bytes: value.into_bytes(),
                receipt: json!({"test_simulated":true,"raw_output_returned":false}),
            })
        }
    }
    let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(30))?;
    let path = workspace.path.join("simulated-compiler.exe");
    std::fs::write(&path, tests::fixture_pe()).unwrap();
    let tools = Tools {
        cargo: Executable::open(path.clone(), &parent)?,
        rustc: Executable::open(path, &parent)?,
        system: workspace.path.clone(),
        rustup: workspace.path.clone(),
        discovery: CompilerDiscoveryContext::default(),
    };
    let state = CompilerState::create(workspace, staged, tools, &parent)?;
    let mut fake = Fake {
        staged,
        scenario,
        claim: None,
    };
    let result = validate_inner(workspace, staged, &parent, state, &mut fake, None)
        .map(OwnedNativeValidation::into_receipt);
    if fake.claim.is_some() {
        pending(workspace);
    }
    if let Some(mut claim) = fake.claim.take() {
        claim.resolve();
    }
    result.map(|mut value| {
        value["test_simulated_runner"] = json!(true);
        value
    })
}

#[cfg(test)]
pub(crate) fn actual_copied_config_witness_for_test(
    workspace: &mut Workspace,
    staged: &StagedSources,
) -> Result<Value, String> {
    struct WithConfig<'a> {
        runner: RetainedRunner<'a>,
        member: PathBuf,
        witnesses: usize,
    }
    impl PhaseRunner for WithConfig<'_> {
        fn capture(
            &mut self,
            phase: Phase<'_>,
            state: &CompilerState,
            workspace: &Workspace,
            parent: &ParentDeadline,
        ) -> Result<Captured, String> {
            if phase.args[0] == "-vV" {
                return self.runner.capture(phase, state, workspace, parent);
            }
            let directory = self.member.parent().unwrap().join(".cargo");
            if state.cwd.starts_with(self.member.parent().unwrap()) {
                return Err("test_copied_config_cwd_refused".into());
            }
            std::fs::create_dir(&directory).map_err(|_| "test_config_creation_refused")?;
            let path = directory.join("config.toml");
            let bytes = b"[build]\nrustc = 'A:/nonexistent-fixture-compiler-must-not-run.exe'\n";
            std::fs::write(&path, bytes).map_err(|_| "test_config_creation_refused")?;
            let result = self.runner.capture(phase, state, workspace, parent);
            let witness = (|| -> Result<(), String> {
                if std::fs::read(&path).map_err(|_| "test_config_witness_unavailable")? != bytes {
                    return Err("test_config_witness_changed".into());
                }
                std::fs::remove_file(&path).map_err(|_| "test_config_cleanup_refused")?;
                std::fs::remove_dir(&directory).map_err(|_| "test_config_cleanup_refused")?;
                Ok(())
            })();
            // Preserve a primary phase failure if witness cleanup also refused.
            let captured = result?;
            witness?;
            self.witnesses += 1;
            Ok(captured)
        }
    }
    let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(180))?;
    let tools = Tools::discover(&parent)?;
    let state = CompilerState::create(workspace, staged, tools, &parent)?;
    let controller = StreamController::for_runtime();
    let mut runner = WithConfig {
        runner: RetainedRunner(&controller),
        member: staged.member_manifest.clone(),
        witnesses: 0,
    };
    let mut value = validate_inner(workspace, staged, &parent, state, &mut runner, None)?.into_receipt();
    if runner.witnesses != 2 {
        return Err("test_config_witness_count_refused".into());
    }
    value["test_copied_config_inactive_witnesses"] = json!(runner.witnesses);
    value["test_grant_authority"] = json!("simulated; no attended owner approval");
    Ok(value)
}
#[cfg(test)]
pub(crate) fn actual_copied_config_diagnostic_for_test(
    workspace: &mut Workspace,
    staged: &StagedSources,
    permit: crate::external_workspace::KnownCargoFixturePermit,
    retained: &mut Option<
        std::mem::ManuallyDrop<
            crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle,
        >,
    >,
) -> Result<Value, String> {
    struct WithConfig<'a> {
        runner: KnownCargoRunner<'a>,
        member: PathBuf,
        witnesses: usize,
    }
    impl PhaseRunner for WithConfig<'_> {
        fn capture(
            &mut self,
            phase: Phase<'_>,
            state: &CompilerState,
            workspace: &Workspace,
            parent: &ParentDeadline,
        ) -> Result<Captured, String> {
            if phase.args[0] == "-vV" {
                return self.runner.capture(phase, state, workspace, parent);
            }
            let directory = self.member.parent().unwrap().join(".cargo");
            if state.cwd.starts_with(self.member.parent().unwrap()) {
                return Err("test_copied_config_cwd_refused".into());
            }
            std::fs::create_dir(&directory).map_err(|_| "test_config_creation_refused")?;
            let path = directory.join("config.toml");
            let bytes = b"[build]\nrustc = 'A:/nonexistent-fixture-compiler-must-not-run.exe'\n";
            std::fs::write(&path, bytes).map_err(|_| "test_config_creation_refused")?;
            let result = self.runner.capture(phase, state, workspace, parent);
            let witness = (|| -> Result<(), String> {
                if std::fs::read(&path).map_err(|_| "test_config_witness_unavailable")? != bytes {
                    return Err("test_config_witness_changed".into());
                }
                std::fs::remove_file(&path).map_err(|_| "test_config_cleanup_refused")?;
                std::fs::remove_dir(&directory).map_err(|_| "test_config_cleanup_refused")?;
                Ok(())
            })();
            // Preserve a primary phase failure if witness cleanup also refused.
            let captured = result?;
            witness?;
            self.witnesses += 1;
            Ok(captured)
        }
    }
    let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(180))?;
    let tools = Tools::discover(&parent)?;
    let state = CompilerState::create(workspace, staged, tools, &parent)?;
    let controller = StreamController::for_runtime();
    if retained.is_some() {
        return Err("test_known_cargo_handle_already_retained".into());
    }
    state.check(&parent)?;
    let invocation = KnownCargoInvocation {
        program: state.tools.cargo.application.clone(),
        argv0: state.tools.cargo.invocation.clone(),
        cwd: state.cwd.clone(),
        env: state.env.clone(),
        target: state.target.clone(),
        application_sha256: state.tools.cargo.sha256.clone(),
        parent_deadline: parent.instant(),
    };
    let diagnostic =
        crate::worker_windows::binary::real_cargo_diagnostic::DiagnosticHandle::for_fixture(
            permit,
            workspace,
            staged,
            &controller,
            invocation,
        )
        .map_err(|_| "test_known_cargo_permit_refused")?;
    *retained = Some(std::mem::ManuallyDrop::new(diagnostic));
    let diagnostic = retained.as_ref().unwrap();
    let mut runner = WithConfig {
        runner: KnownCargoRunner {
            inner: RetainedRunner(&controller),
            diagnostic,
        },
        member: staged.member_manifest.clone(),
        witnesses: 0,
    };
    let result = validate_inner(workspace, staged, &parent, state, &mut runner, None);
    if result.is_err() {
        diagnostic.scope_refusal("validation_result_refused");
    }
    let mut value = result?.into_receipt();
    if runner.witnesses != 2 {
        return Err("test_config_witness_count_refused".into());
    }
    value["test_copied_config_inactive_witnesses"] = json!(runner.witnesses);
    value["test_grant_authority"] = json!("simulated; no attended owner approval");
    Ok(value)
}
#[cfg(test)]
fn metadata_directory_shape(path: &str) -> Value {
    json!({
        "absolute": Path::new(&path.replace('/', "\\")).is_absolute(),
        "verbatim": path.starts_with("\\\\?\\") || path.starts_with("//?/"),
        "forward_slashes": path.contains('/'),
        "back_slashes": path.contains('\\'),
        "bytes": path.len(),
    })
}
pub(crate) fn compiler_host(stdout: &str) -> Result<String, String> {
    let hosts: Vec<_> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("host: "))
        .collect();
    if hosts.len() != 1 {
        return Err("external_compiler_host_unavailable".into());
    }
    let host = hosts[0];
    if host.len() > 128
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || !matches!(host, "x86_64-pc-windows-msvc" | "aarch64-pc-windows-msvc")
    {
        return Err("external_compiler_host_unsupported".into());
    }
    Ok(host.into())
}
pub(crate) fn selected_member(
    staged: &StagedSources,
    metadata: &Value,
    lease: &CompilationLease,
    budget: &Budget<'_>,
) -> Result<SelectedMember, String> {
    budget.check()?;
    let canonical_member =
        lease.checked_metadata_file(&staged.member_manifest.to_string_lossy(), budget)?;
    let canonical_cargo =
        lease.checked_metadata_file(&staged.cargo_manifest.to_string_lossy(), budget)?;
    let workspace = canonical_cargo
        .parent()
        .ok_or("external_workspace_manifest_missing")?;
    let canonical_envelope = lease.checked_metadata_workspace(
        &staged.envelope,
        &staged.envelope.to_string_lossy(),
        budget,
    )?;
    let mut metadata = metadata.clone();
    let reported_workspace = metadata["workspace_root"]
        .as_str()
        .ok_or("external_workspace_manifest_missing")?;
    let canonical_workspace =
        lease.checked_metadata_workspace(workspace, reported_workspace, budget)?;
    metadata["workspace_root"] = json!(canonical_workspace);
    let packages = metadata["packages"]
        .as_array_mut()
        .filter(|rows| !rows.is_empty() && rows.len() <= 128)
        .ok_or("external_local_metadata_bounds")?;
    let mut selected = None;
    for (index, package) in packages.iter_mut().enumerate() {
        budget.check()?;
        if !package["source"].is_null() {
            return Err("external_registry_or_git_dependency_unsupported".into());
        }
        let reported_manifest = package["manifest_path"]
            .as_str()
            .ok_or("external_local_manifest_missing")?;
        let manifest = lease.checked_metadata_file(reported_manifest, budget);
        #[cfg(test)]
        metadata_file_diagnostic(
            "manifest_path",
            reported_manifest,
            &canonical_member,
            &manifest,
        );
        let manifest = manifest?;
        if crate::ownership::paths_equal(&manifest, &canonical_member) {
            if selected.is_some() {
                return Err("external_selected_member_ambiguous".into());
            }
            selected = Some(index);
        }
        package["manifest_path"] = json!(manifest);
        for target in package["targets"]
            .as_array_mut()
            .ok_or("external_local_targets_missing")?
        {
            budget.check()?;
            let kinds = target["kind"]
                .as_array()
                .ok_or("external_local_target_kind_missing")?;
            if kinds
                .iter()
                .any(|kind| matches!(kind.as_str(), Some("custom-build" | "proc-macro")))
            {
                return Err("external_build_script_or_proc_macro_unsupported".into());
            }
            let reported = target["src_path"]
                .as_str()
                .ok_or("external_local_source_missing")?;
            target["src_path"] = json!(lease.checked_metadata_file(reported, budget)?);
        }
    }
    let index = selected.ok_or("external_selected_member_missing")?;
    let package = &metadata["packages"][index];
    let name = package["name"]
        .as_str()
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or("external_selected_package_name")?;
    let (package_id, target) = crate::native_dependencies::validate_graph(
        &canonical_envelope,
        &canonical_workspace,
        &metadata,
        name,
    )
    .map_err(|_| "external_local_metadata_confinement_or_target_refused")?;
    if package["id"].as_str() != Some(package_id.as_str()) {
        return Err("external_selected_package_identity_changed".into());
    }
    if !metadata["workspace_members"]
        .as_array()
        .is_some_and(|members| {
            members
                .iter()
                .any(|id| id.as_str() == Some(package_id.as_str()))
        })
    {
        return Err("external_selected_member_not_in_workspace".into());
    }
    let source = lease.checked_library_source(&canonical_member, budget)?;
    let reported_source = package["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["crate_types"]
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"))
        })
        .and_then(|row| row["src_path"].as_str())
        .ok_or("external_selected_source_missing")?;
    if !crate::ownership::paths_equal(Path::new(reported_source), &source) {
        return Err("external_selected_source_changed".into());
    }
    Ok(SelectedMember {
        package: name.into(),
        package_id,
        target,
        manifest: canonical_member,
        source,
    })
}
#[cfg(test)]
fn metadata_file_diagnostic(
    field: &str,
    reported: &str,
    expected: &Path,
    checked: &Result<PathBuf, String>,
) {
    eprintln!("external_compile_metadata_file field={field} legacy_spelling_match={} checked_expected_match={} reported_shape={} failure_tag={}",
        crate::ownership::paths_equal(Path::new(reported), expected),
        checked.as_ref().is_ok_and(|path| crate::ownership::paths_equal(path, expected)),
        metadata_directory_shape(reported), checked.as_ref().err().map_or("none", String::as_str));
}

const NAMED_EXPORT_ENTRIES: usize = 4096;
const NAMED_EXPORT_NAME_BYTES: usize = 128;
const NAMED_EXPORT_READ_BYTES: usize = 1024 * 1024;

/// Structural metadata only. This is neither an address nor a callable pointer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NamedPeExport {
    name: String,
    rva: u32,
}
impl NamedPeExport {
    pub(crate) fn name(&self) -> &str { &self.name }
    pub(crate) fn rva(&self) -> u32 { self.rva }
}

struct PeExportRead<'a, 'b, R> {
    reader: &'a mut R,
    bytes: u64,
    charged: usize,
    budget: &'a Budget<'b>,
}
impl<R: Read + io::Seek> PeExportRead<'_, '_, R> {
    fn admit(&mut self, offset: u64, count: usize) -> Result<(), String> {
        self.budget.check()?;
        let charged = self.charged.checked_add(count).ok_or("external_named_export_read_bound")?;
        if charged > NAMED_EXPORT_READ_BYTES { return Err("external_named_export_read_bound".into()); }
        if count == 0 || offset.checked_add(count as u64).is_none_or(|end| end > self.bytes) {
            return Err("external_named_export_file_range".into());
        }
        self.charged = charged;
        Ok(())
    }
    fn read_admitted(&mut self, offset: u64, bytes: &mut [u8]) -> Result<(), String> {
        self.reader.seek(io::SeekFrom::Start(offset)).map_err(|_| "external_named_export_seek_failed")?;
        self.reader.read_exact(bytes).map_err(|_| "external_named_export_read_failed")?;
        self.budget.check()
    }
    fn array<const N: usize>(&mut self, offset: u64) -> Result<[u8; N], String> {
        self.admit(offset, N)?;
        let mut bytes = [0u8; N];
        self.read_admitted(offset, &mut bytes)?;
        Ok(bytes)
    }
    fn vector(&mut self, offset: u64, count: usize) -> Result<Vec<u8>, String> {
        self.admit(offset, count)?;
        // Every allocation is charged before it occurs. Parser callers use at
        // most 4096 table entries, never a whole-DLL buffer.
        let mut bytes = vec![0u8; count];
        self.read_admitted(offset, &mut bytes)?;
        Ok(bytes)
    }
}

struct PeExportSection {
    rva: u32,
    virtual_bytes: u32,
    raw: u32,
    raw_bytes: u32,
    mapped_end: u32,
    flags: u32,
}
struct PeExportLayout {
    sections: Vec<PeExportSection>,
    image_bytes: u32,
    header_bytes: u32,
    export_rva: u32,
    export_end: u32,
}
impl PeExportLayout {
    fn range(&self, rva: u32, count: usize) -> Result<(u64, usize), String> {
        let count = u32::try_from(count).map_err(|_| "external_named_export_rva_range")?;
        let end = rva.checked_add(count).ok_or("external_named_export_rva_range")?;
        if count == 0 || end > self.image_bytes { return Err("external_named_export_rva_range".into()); }
        if rva < self.header_bytes { return Err("external_named_export_header_mapping".into()); }
        let mut matched = None;
        for (index, section) in self.sections.iter().enumerate() {
            if rva < section.mapped_end && section.rva < end {
                if matched.replace(index).is_some() { return Err("external_named_export_ambiguous_rva".into()); }
            }
        }
        let index = matched.ok_or("external_named_export_unbacked_rva")?;
        let section = &self.sections[index];
        let delta = rva.checked_sub(section.rva).ok_or("external_named_export_unbacked_rva")?;
        let relative_end = delta.checked_add(count).ok_or("external_named_export_rva_range")?;
        if relative_end > section.virtual_bytes || relative_end > section.raw_bytes {
            return Err("external_named_export_unbacked_rva".into());
        }
        let offset = section.raw.checked_add(delta).ok_or("external_named_export_file_range")?;
        if offset < self.header_bytes { return Err("external_named_export_header_mapping".into()); }
        Ok((u64::from(offset), index))
    }
    fn name_window(&self, rva: u32) -> Result<(u64, usize), String> {
        let (_, index) = self.range(rva, 1)?;
        let section = &self.sections[index];
        let delta = rva - section.rva;
        let available = (section.virtual_bytes - delta).min(section.raw_bytes - delta)
            .min((NAMED_EXPORT_NAME_BYTES + 1) as u32) as usize;
        let (offset, _) = self.range(rva, available)?;
        Ok((offset, available))
    }
}
fn pe_export_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset.checked_add(4).ok_or("external_named_export_pe_refused")?;
    Ok(u32::from_le_bytes(bytes.get(offset..end).ok_or("external_named_export_pe_refused")?
        .try_into().map_err(|_| "external_named_export_pe_refused")?))
}

/// Bounded read-backed counterpart of pe_receipt's existing header/section
/// rules. The old receipt stays byte-exact; only export-specific checks are new.
fn pe_export_layout<R: Read + io::Seek>(
    input: &mut PeExportRead<'_, '_, R>, host: &str,
) -> Result<PeExportLayout, String> {
    let machine = match host {
        "x86_64-pc-windows-msvc" => 0x8664u16,
        "aarch64-pc-windows-msvc" => 0xaa64u16,
        _ => return Err("external_compiler_host_unsupported".into()),
    };
    if !(64..=128 * 1024 * 1024).contains(&input.bytes) {
        return Err("external_named_export_pe_refused".into());
    }
    let dos = input.array::<64>(0)?;
    let offset = u64::from(pe_export_u32(&dos, 60)?);
    if &dos[..2] != b"MZ" || offset < 64 { return Err("external_named_export_pe_refused".into()); }
    let header = input.array::<24>(offset)?;
    let sections = u16::from_le_bytes([header[6], header[7]]) as usize;
    let optional = u16::from_le_bytes([header[20], header[21]]) as usize;
    if &header[..4] != b"PE\0\0" || u16::from_le_bytes([header[4], header[5]]) != machine
        || sections == 0 || sections > 96 || optional < 120
        || u16::from_le_bytes([header[22], header[23]]) & 0x2000 == 0
    { return Err("external_named_export_pe_refused".into()); }
    let optional_start = offset.checked_add(24).ok_or("external_named_export_file_range")?;
    let table_start = optional_start.checked_add(optional as u64).ok_or("external_named_export_file_range")?;
    let optional_header = input.array::<120>(optional_start)?;
    if u16::from_le_bytes([optional_header[0], optional_header[1]]) != 0x20b {
        return Err("external_named_export_pe_refused".into());
    }
    let directories = pe_export_u32(&optional_header, 108)? as usize;
    if directories == 0 { return Err("external_named_export_missing".into()); }
    if directories > (optional - 112) / 8 { return Err("external_named_export_pe_refused".into()); }
    let image_bytes = pe_export_u32(&optional_header, 56)?;
    let header_bytes = pe_export_u32(&optional_header, 60)?;
    let table_bytes = sections.checked_mul(40).ok_or("external_named_export_pe_refused")?;
    let table_end = table_start.checked_add(table_bytes as u64).ok_or("external_named_export_file_range")?;
    if u64::from(header_bytes) < table_end || u64::from(header_bytes) > input.bytes || header_bytes > image_bytes {
        return Err("external_named_export_header_extent".into());
    }
    let export_rva = pe_export_u32(&optional_header, 112)?;
    let export_bytes = pe_export_u32(&optional_header, 116)?;
    let export_end = export_rva.checked_add(export_bytes).ok_or("external_named_export_rva_range")?;
    if export_rva == 0 || export_bytes < 40 { return Err("external_named_export_missing".into()); }
    if export_end > image_bytes { return Err("external_named_export_rva_range".into()); }
    let table = input.vector(table_start, table_bytes)?;
    let mut parsed = Vec::with_capacity(sections);
    for section in table.chunks_exact(40) {
        input.budget.check()?;
        let virtual_bytes = pe_export_u32(section, 8)?;
        let rva = pe_export_u32(section, 12)?;
        let raw_bytes = pe_export_u32(section, 16)?;
        let raw = pe_export_u32(section, 20)?;
        if u64::from(raw).checked_add(u64::from(raw_bytes)).is_none_or(|end| end > input.bytes) {
            return Err("external_named_export_file_range".into());
        }
        let mapped_end = rva.checked_add(virtual_bytes.max(raw_bytes)).ok_or("external_named_export_rva_range")?;
        if mapped_end > image_bytes { return Err("external_named_export_rva_range".into()); }
        parsed.push(PeExportSection { rva, virtual_bytes, raw, raw_bytes, mapped_end, flags: pe_export_u32(section, 36)? });
    }
    Ok(PeExportLayout { sections: parsed, image_bytes, header_bytes, export_rva, export_end })
}

/// Examine bytes only, never load an image. Reader position is restored even
/// after missing symbols, malformed data, cancellation, expiry or read failure.
pub(crate) fn named_direct_pe_export<R: Read + io::Seek>(
    reader: &mut R, bytes: u64, host: &str, name: &str, budget: &Budget<'_>,
) -> Result<NamedPeExport, String> {
    let position = reader.stream_position().map_err(|_| "external_named_export_cursor_unavailable")?;
    let result = (|| {
        budget.check()?;
        if name.is_empty() || name.len() > NAMED_EXPORT_NAME_BYTES || !name.is_ascii()
            || name.bytes().any(|b| b == 0 || b.is_ascii_control())
        { return Err("external_named_export_name_bound".into()); }
        let mut input = PeExportRead { reader: &mut *reader, bytes, charged: 0, budget };
        let layout = pe_export_layout(&mut input, host)?;
        let (directory_offset, _) = layout.range(layout.export_rva, 40)?;
        let directory = input.array::<40>(directory_offset)?;
        if pe_export_u32(&directory, 0)? != 0 { return Err("external_named_export_pe_refused".into()); }
        let functions = pe_export_u32(&directory, 20)? as usize;
        let names = pe_export_u32(&directory, 24)? as usize;
        if functions == 0 || names == 0 { return Err("external_named_export_missing".into()); }
        if functions > NAMED_EXPORT_ENTRIES || names > NAMED_EXPORT_ENTRIES {
            return Err("external_named_export_entry_bound".into());
        }
        let (function_offset, _) = layout.range(pe_export_u32(&directory, 28)?, functions.checked_mul(4).ok_or("external_named_export_entry_bound")?)?;
        let (name_offset, _) = layout.range(pe_export_u32(&directory, 32)?, names.checked_mul(4).ok_or("external_named_export_entry_bound")?)?;
        let (ordinal_offset, _) = layout.range(pe_export_u32(&directory, 36)?, names.checked_mul(2).ok_or("external_named_export_entry_bound")?)?;
        let functions = input.vector(function_offset, functions * 4)?;
        let pointers = input.vector(name_offset, names * 4)?;
        let ordinals = input.vector(ordinal_offset, names * 2)?;
        let mut previous = Vec::new();
        let mut selected = None;
        for index in 0..names {
            budget.check()?;
            let ordinal = u16::from_le_bytes([ordinals[index * 2], ordinals[index * 2 + 1]]) as usize;
            // Name ordinals are unbiased indexes. Do not subtract OrdinalBase.
            if ordinal >= functions.len() / 4 { return Err("external_named_export_ordinal_refused".into()); }
            let (offset, available) = layout.name_window(pe_export_u32(&pointers, index * 4)?)?;
            let mut buffer = [0u8; NAMED_EXPORT_NAME_BYTES + 1];
            input.admit(offset, available)?;
            input.read_admitted(offset, &mut buffer[..available])?;
            let end = buffer[..available].iter().position(|b| *b == 0)
                .ok_or("external_named_export_name_bound")?;
            let current = &buffer[..end];
            if current.is_empty() || !current.is_ascii() { return Err("external_named_export_name_bound".into()); }
            if index > 0 && previous.as_slice() >= current { return Err("external_named_export_name_order".into()); }
            if current == name.as_bytes() { selected = Some(pe_export_u32(&functions, ordinal * 4)?); }
            previous.clear();
            previous.extend_from_slice(current);
        }
        let rva = selected.ok_or("external_named_export_missing")?;
        if rva == 0 { return Err("external_named_export_zero_target".into()); }
        if (layout.export_rva..layout.export_end).contains(&rva) {
            return Err("external_named_export_forwarder_refused".into());
        }
        let (target_offset, section) = layout.range(rva, 1)?;
        if layout.sections[section].flags & 0x2000_0000 == 0 {
            return Err("external_named_export_executable_target_required".into());
        }
        // Corroborate file backing through this same reader, never execute it.
        let _ = input.array::<1>(target_offset)?;
        budget.check()?;
        Ok(NamedPeExport { name: name.to_owned(), rva })
    })();
    match (result, reader.seek(io::SeekFrom::Start(position))) {
        (Ok(value), Ok(_)) => { budget.check()?; Ok(value) }
        (Err(error), Ok(_)) => Err(error),
        (_, Err(_)) => Err("external_named_export_cursor_restore_failed".into()),
    }
}

pub(crate) fn pe_receipt(bytes: &[u8], host: &str) -> Result<Value, String> {
    let machine = match host {
        "x86_64-pc-windows-msvc" => 0x8664u16,
        "aarch64-pc-windows-msvc" => 0xaa64u16,
        _ => return Err("external_compiler_host_unsupported".into()),
    };
    if bytes.len() < 64 || bytes.len() > 128 * 1024 * 1024 || &bytes[..2] != b"MZ" {
        return Err("external_native_artifact_pe_refused".into());
    }
    let offset = u32::from_le_bytes(
        bytes[60..64]
            .try_into()
            .map_err(|_| "external_native_artifact_pe_refused")?,
    ) as usize;
    let header = bytes
        .get(
            offset
                ..offset
                    .checked_add(24)
                    .ok_or("external_native_artifact_pe_refused")?,
        )
        .ok_or("external_native_artifact_pe_refused")?;
    if offset < 64
        || &header[..4] != b"PE\0\0"
        || u16::from_le_bytes([header[4], header[5]]) != machine
    {
        return Err("external_native_artifact_pe_refused".into());
    }
    let sections = u16::from_le_bytes([header[6], header[7]]) as usize;
    let optional = u16::from_le_bytes([header[20], header[21]]) as usize;
    let flags = u16::from_le_bytes([header[22], header[23]]);
    let optional_start = offset + 24;
    let optional_end = optional_start
        .checked_add(optional)
        .ok_or("external_native_artifact_pe_refused")?;
    let optional_bytes = bytes
        .get(optional_start..optional_end)
        .ok_or("external_native_artifact_pe_refused")?;
    if sections == 0
        || sections > 96
        || optional_bytes.len() < 112
        || u16::from_le_bytes([optional_bytes[0], optional_bytes[1]]) != 0x20b
        || flags & 0x2000 == 0
    {
        return Err("external_native_artifact_pe_refused".into());
    }
    let table_end = optional_end
        .checked_add(sections * 40)
        .ok_or("external_native_artifact_pe_refused")?;
    let table = bytes
        .get(optional_end..table_end)
        .ok_or("external_native_artifact_pe_refused")?;
    for index in 0..sections {
        let section_start = index
            .checked_mul(40)
            .ok_or("external_native_artifact_pe_refused")?;
        let section_end = section_start
            .checked_add(40)
            .ok_or("external_native_artifact_pe_refused")?;
        let section = table
            .get(section_start..section_end)
            .ok_or("external_native_artifact_pe_refused")?;
        let size = u32::from_le_bytes(
            section[16..20]
                .try_into()
                .map_err(|_| "external_native_artifact_pe_refused")?,
        ) as usize;
        let start = u32::from_le_bytes(
            section[20..24]
                .try_into()
                .map_err(|_| "external_native_artifact_pe_refused")?,
        ) as usize;
        if start.checked_add(size).is_none_or(|end| end > bytes.len()) {
            return Err("external_native_artifact_pe_refused".into());
        }
    }
    Ok(
        json!({"format":"PE32+","machine":if machine==0x8664{"x86_64"}else{"arm64"},
        "dll_characteristic":true,"sections":sections,"bytes":bytes.len(),"sha256":crate::sha256_hex(bytes),
        "loaded":false,"activated":false,"published":false,"execution_sandboxed":false}),
    )
}

#[cfg(test)]
impl OwnedNativeValidation {
    // Negative fixture only: true receipt fields cannot manufacture typed evidence.
    pub(crate) fn unproven_for_test(artifact: PathBuf, artifact_guard: File, artifact_hash: String) -> Self {
        Self {
            receipt: json!({"native_artifact_produced":true,"cargo_executed":true,
                "artifact":{"path":artifact.file_name().unwrap().to_string_lossy(),"sha256":artifact_hash}}),
            artifact, artifact_guard, artifact_hash, provenance: None,
        }
    }
}
#[cfg(test)]
impl NativeCompilationProvenance {
    pub(crate) fn staged_content_custody_for_test(
        &mut self,
        staged: &StagedSources,
        paths: &[PathBuf],
        budget: &Budget<'_>,
        corrupt: bool,
    ) -> Result<Value, String> {
        self.inputs.staged_content_custody_for_test(
            staged, &self.selected.manifest, &self.selected.source, paths, budget, corrupt,
        )
    }
    // Borrows only a record already issued by the genuine producer. No constructor.
    pub(crate) fn check_corruptions_for_test(
        &mut self, staged: &StagedSources, artifact: &Path, artifact_hash: &str, budget: &Budget<'_>,
    ) -> Result<Value, String> {
        fn refused(result: Result<(), String>, expected: &str) -> Result<(), String> {
            if result.as_ref().err().map(String::as_str) != Some(expected) {
                return Err("fixture_provenance_corruption_not_exactly_refused".into());
            }
            Ok(())
        }
        // Equivalent caller spellings must pass the selected-manifest token check,
        // without mutating the retained record, its digest, File or compiler owner.
        let mut equivalent = staged.clone();
        equivalent.envelope = PathBuf::from(staged.envelope.to_str().unwrap().replace('\\', "/"));
        equivalent.cargo_manifest = PathBuf::from(staged.cargo_manifest.to_str().unwrap().replace('\\', "/"));
        equivalent.member_manifest = PathBuf::from(staged.member_manifest.to_str().unwrap().replace('/', "\\").to_ascii_uppercase());
        let artifact_alias = PathBuf::from(artifact.to_str().unwrap().replace('\\', "/"));
        self.recheck(&equivalent, &artifact_alias, artifact_hash, budget)?;
        drop(equivalent);
        self.recheck(staged, artifact, artifact_hash, budget)?;
        let original_digest = self.digest.clone();
        let graph_hash = self.resolution["sha256"].as_str()
            .ok_or("fixture_provenance_graph_hash_missing")?.to_owned();
        let altered = format!("{}{}", if graph_hash.starts_with('0') { "1" } else { "0" }, &graph_hash[1..]);
        self.resolution["sha256"] = json!(altered);
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.resolution["sha256"] = json!(graph_hash);
        refused(observed, "retained Cargo graph identity mismatch")?;

        self.digest.push('0');
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.digest = original_digest.clone();
        refused(observed, "external_native_provenance_digest_changed")?;

        let manifest = std::mem::replace(&mut self.selected.manifest, staged.envelope.join("foreign/Cargo.toml"));
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.selected.manifest = manifest;
        refused(observed, "external_native_provenance_binding_changed")?;

        let target = self.selected.target.clone();
        self.selected.target.push_str("-corrupt");
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.selected.target = target;
        refused(observed, "external_native_provenance_digest_changed")?;

        let source = std::mem::replace(&mut self.selected.source, staged.envelope.join("foreign.rs"));
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.selected.source = source;
        refused(observed, "external_provenance_held_input_changed")?;

        let path = std::mem::replace(&mut self.artifact, artifact.with_extension("foreign"));
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.artifact = path;
        refused(observed, "external_native_provenance_binding_changed")?;

        let hash = self.artifact_hash.clone();
        self.artifact_hash.push('0');
        let observed = self.recheck(staged, artifact, artifact_hash, budget);
        self.artifact_hash = hash;
        refused(observed, "external_native_provenance_binding_changed")?;
        self.recheck(staged, artifact, artifact_hash, budget)?;
        if self.digest != original_digest {
            return Err("fixture_provenance_restoration_changed_digest".into());
        }
        Ok(json!({"status":"REAL_TYPED_RECORD_RECHECKED","sha256":original_digest,
            "corruption_refusals":7,"corruption_cases":["resolution_digest","record_digest",
                "selected_manifest","selected_target","selected_source","artifact_path","artifact_hash"],
            "all_mutations_restored":true,"dependency_content_closure_verified":false,
            "exact_build_units_or_gdextension_loading_verified":false}))
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn reported_local_output_admission_bounds() {
        assert_eq!(reported_output_remaining(0,0).unwrap(),128*1024*1024);
        assert_eq!(reported_output_remaining(255,128*1024*1024-1).unwrap(),1);
        assert_eq!(reported_output_remaining(255,128*1024*1024).unwrap(),0);
        assert!(reported_output_remaining(256,0).is_err());
        assert!(reported_output_remaining(0,128*1024*1024+1).is_err());
        assert!(reported_output_remaining(usize::MAX,u64::MAX).is_err());
    }
    fn configfile_parser_test_document() -> Value {
        json!({"sections":[{"name":"configuration","entries":[{"key":"entry_symbol","type":"string","value":"value"}]}],
            "libraries":[{"key":"windows.debug.x86_64","flags":["windows","debug","x86_64"],"path":"res://bin/value.dll","matches_target":true}],
            "dependencies":[]})
    }
    fn configfile_parser_test_frame(expected: &Value) -> Value {
        json!({"schema":"aurum.configfile.output/1","parse_error":0,"sections":expected["sections"],
            "libraries":expected["libraries"],"dependencies":[],"target_tags":CONFIGFILE_TARGET_TAGS,
            "observed_tags":["windows","editor"]})
    }
    #[test]
    fn configfile_parser_typed_output_compares_order_types_and_separate_observed_tags() {
        let expected = configfile_parser_test_document();
        let frame = configfile_parser_test_frame(&expected);
        let encode = |value:&Value| format!("Godot banner\nAURUM_CONFIGFILE_PARSED {}\n",value).into_bytes();
        assert_eq!(configfile_parser_output(&encode(&frame),&expected).unwrap(),["windows","editor"]);
        for field in ["sections","libraries","dependencies","target_tags"] {
            let mut altered=frame.clone();altered[field]=json!(["different"]);
            assert!(configfile_parser_output(&encode(&altered),&expected).is_err());
        }
        for wrong in [json!(true),json!("0"),json!(1)] {
            let mut altered=frame.clone();altered["parse_error"]=wrong;
            assert!(configfile_parser_output(&encode(&altered),&expected).is_err());
        }
        let mut altered=frame.clone();altered["extra"]=json!(true);
        assert!(configfile_parser_output(&encode(&altered),&expected).is_err());
        altered=frame.clone();altered["observed_tags"]=json!(["windows","windows"]);
        assert!(configfile_parser_output(&encode(&altered),&expected).is_err());
        altered["observed_tags"]=json!(["unapproved"]);
        assert!(configfile_parser_output(&encode(&altered),&expected).is_err());
        let mut ordered=expected.clone();
        ordered["sections"][0]["entries"].as_array_mut().unwrap()
            .push(json!({"key":"reloadable","type":"bool","value":false}));
        let mut changed=configfile_parser_test_frame(&ordered);
        changed["sections"][0]["entries"].as_array_mut().unwrap().reverse();
        assert!(configfile_parser_output(&encode(&changed),&ordered).is_err());
    }
    #[test]
    fn configfile_parser_output_refuses_duplicates_truncation_prefix_repair_and_bounds() {
        let expected=configfile_parser_test_document();let frame=configfile_parser_test_frame(&expected);
        let one=format!("AURUM_CONFIGFILE_PARSED {frame}\n");
        for raw in [format!("{one}{one}"),format!("\x1b[7;1H{one}"),
            one.trim_end().to_owned(),
            one.replace("\"parse_error\":0","\"parse_error\":true,\"parse_error\":0"),
            one.replace("\"type\":\"string\"","\"type\":\"bool\",\"type\":\"string\""),
            "AURUM_CONFIGFILE_PARSED {".into(),"unframed".into()]
        {assert!(configfile_parser_output(raw.as_bytes(),&expected).is_err());}
        assert!(configfile_parser_output(&[0xff],&expected).is_err());
        assert!(configfile_parser_output(&vec![b' ';CONFIGFILE_PARSER_OUTPUT+1],&expected).is_err());
    }
    #[test]
    fn held_configfile_engine_selection_uses_only_explicit_host_settings() {
        let fixture = CompilerContextFixture::new();
        let runtime = fixture.0.join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        for name in ["Godot.exe", "Godot_console.exe", "Godot_long_console.exe"] {
            std::fs::write(runtime.join(name), b"ordinary selection fixture, never executed").unwrap();
        }
        let names = std::cell::RefCell::new(Vec::new());
        let explicit = fixture.0.join("explicit.exe");
        let deadline = Instant::now() + Duration::from_secs(10);
        let selected = configured_engine_path(|name| {
            names.borrow_mut().push(name.to_owned());
            Some(explicit.clone())
        }, deadline, &|| Ok(())).unwrap();
        assert_eq!(selected, explicit);
        assert_eq!(*names.borrow(), ["AURUM_GODOT"]);
        names.borrow_mut().clear();
        let selected = configured_engine_path(|name| {
            names.borrow_mut().push(name.to_owned());
            (name == "AURUM_STUDIO_HOME").then(|| fixture.0.clone())
        }, deadline, &|| Ok(())).unwrap();
        assert_eq!(selected, runtime.join("Godot_console.exe"));
        assert_eq!(*names.borrow(), ["AURUM_GODOT", "AURUM_STUDIO_HOME"]);
    }

    #[test]
    fn held_configfile_engine_selection_refuses_fallbacks_invalid_paths_and_unbounded_runtime() {
        let fixture = CompilerContextFixture::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(configured_engine_path(|_| None, deadline, &|| Ok(())).unwrap_err(),
            "external_configfile_engine_not_configured");
        for value in [PathBuf::from(""), PathBuf::from("relative.exe"), fixture.0.join("wrong.dll"),
            fixture.0.join("bad\nname.exe")]
        {
            let requested = std::cell::RefCell::new(Vec::new());
            assert!(configured_engine_path(|name| {
                requested.borrow_mut().push(name.to_owned());
                Some(value.clone())
            }, deadline, &|| Ok(())).is_err());
            assert_eq!(*requested.borrow(), ["AURUM_GODOT"]);
        }
        let runtime = fixture.0.join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        for index in 0..=CONFIGFILE_RUNTIME_ENTRIES {
            std::fs::write(runtime.join(format!("entry{index}")), b"not an engine").unwrap();
        }
        assert_eq!(configured_engine_path(|name| {
            (name == "AURUM_STUDIO_HOME").then(|| fixture.0.clone())
        }, deadline, &|| Ok(())).unwrap_err(), "external_configfile_engine_runtime_bounds");
    }

    #[test]
    fn held_configfile_engine_read_witness_rechecks_same_file_digest_and_cursor() {
        use std::io::{Seek, SeekFrom};
        let fixture = CompilerContextFixture::new();
        let path = fixture.0.join("read-only-fixture.exe");
        let bytes: Vec<u8> = (0..200_000).map(|value| (value % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // This internal read witness has no runtime/parent and cannot issue the
        // opaque owner-bound token. The fixture bytes are never executed.
        let image = ConfigFileEngineImage::open(path.clone(), deadline, &|| Ok(())).unwrap();
        assert_eq!(image.sha256, crate::sha256_hex(&bytes));
        assert_eq!(image.stamp.bytes, bytes.len() as u64);
        let mut reader = &image.guard;
        reader.seek(SeekFrom::Start(19)).unwrap();
        image.recheck(deadline, &|| Ok(())).unwrap();
        assert_eq!(reader.stream_position().unwrap(), 19);
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(std::fs::remove_file(&path).is_err());
    }

    struct EngineHashTestReader {
        inner: std::io::Cursor<Vec<u8>>,
        fail_read: bool,
        fail_restore: bool,
        largest_read: usize,
        read_count: usize,
    }
    impl Read for EngineHashTestReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.largest_read = self.largest_read.max(buffer.len());
            self.read_count += 1;
            if self.fail_read { return Err(io::Error::other("fixed unit read failure")); }
            self.inner.read(buffer)
        }
    }
    impl std::io::Seek for EngineHashTestReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> io::Result<u64> {
            if self.fail_restore && position == std::io::SeekFrom::Start(7) {
                return Err(io::Error::other("fixed unit restore failure"));
            }
            std::io::Seek::seek(&mut self.inner, position)
        }
    }
    fn engine_hash_test_reader() -> EngineHashTestReader {
        let mut inner = std::io::Cursor::new(vec![23; 200_000]);
        inner.set_position(7);
        EngineHashTestReader { inner, fail_read: false, fail_restore: false, largest_read: 0, read_count: 0 }
    }

    #[test]
    fn held_configfile_engine_hash_streaming_and_failure_paths_restore_original_cursor() {
        let mut reader = engine_hash_test_reader();
        let expected = reader.inner.get_ref().len() as u64;
        let digest = configfile_engine_hash(&mut reader, expected, &|| Ok(())).unwrap();
        assert_eq!(digest, crate::sha256_hex(reader.inner.get_ref()));
        assert_eq!(reader.inner.position(), 7);
        assert_eq!(reader.largest_read, 64 * 1024);
        assert!(reader.read_count > 3);
        for length in [expected - 1, expected + 1] {
            assert_eq!(configfile_engine_hash(&mut reader, length, &|| Ok(())).unwrap_err(),
                "external_configfile_engine_length_changed");
            assert_eq!(reader.inner.position(), 7);
        }
        reader.fail_read = true;
        assert_eq!(configfile_engine_hash(&mut reader, expected, &|| Ok(())).unwrap_err(),
            "external_configfile_engine_read");
        assert_eq!(reader.inner.position(), 7);
        reader.fail_read = false;
        let checks = std::cell::Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            if checks.get() >= 3 { Err("fixed original-owner cancellation".into()) } else { Ok(()) }
        };
        assert_eq!(configfile_engine_hash(&mut reader, expected, &cancelled).unwrap_err(),
            "fixed original-owner cancellation");
        assert_eq!(reader.inner.position(), 7);
    }

    #[test]
    fn held_configfile_engine_hash_bound_and_failed_restoration_never_return_digest() {
        let mut reader = engine_hash_test_reader();
        for length in [0, CONFIGFILE_ENGINE_BYTES + 1] {
            assert_eq!(configfile_engine_hash(&mut reader, length, &|| Ok(())).unwrap_err(),
                "external_configfile_engine_bytes_bound");
            assert_eq!(reader.read_count, 0);
            assert_eq!(reader.inner.position(), 7);
        }
        reader.fail_restore = true;
        assert_eq!(configfile_engine_hash(&mut reader, 200_000, &|| Ok(())).unwrap_err(),
            "external_configfile_engine_cursor_restore");
        assert_eq!(CONFIGFILE_ENGINE_BYTES, 256 * 1024 * 1024);
        assert!(178_485_256 < CONFIGFILE_ENGINE_BYTES);
    }

    #[test]
    fn held_configfile_engine_read_witness_refuses_changed_facts_foreign_file_and_expiry() {
        let fixture = CompilerContextFixture::new();
        let path = fixture.0.join("identity.exe");
        std::fs::write(&path, b"fixed private read witness").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut image = ConfigFileEngineImage::open(path.clone(), deadline, &|| Ok(())).unwrap();
        let old_digest = image.sha256.clone();
        image.sha256 = "0".repeat(64);
        assert_eq!(image.recheck(deadline, &|| Ok(())).unwrap_err(),
            "external_configfile_engine_hash_changed");
        image.sha256 = old_digest;
        image.stamp.id[0] ^= 1;
        assert_eq!(image.recheck(deadline, &|| Ok(())).unwrap_err(), "external_configfile_engine_changed");
        image.stamp.id[0] ^= 1;
        let foreign = fixture.0.join("foreign.exe");
        std::fs::write(&foreign, b"fixed private read witness").unwrap();
        image.guard = File::open(&foreign).unwrap();
        assert!(image.recheck(deadline, &|| Ok(())).is_err());
        assert!(ConfigFileEngineImage::open(path.clone(), Instant::now() - Duration::from_millis(1), &|| Ok(())).is_err());
        assert!(ConfigFileEngineImage::open(path, deadline, &|| Err("original owner cancelled".into())).is_err());
        let empty = fixture.0.join("empty.exe");
        std::fs::write(&empty, b"").unwrap();
        assert!(ConfigFileEngineImage::open(empty, deadline, &|| Ok(())).is_err());
        assert!(ConfigFileEngineImage::open(fixture.0.join("missing.exe"), deadline, &|| Ok(())).is_err());
        let directory = fixture.0.join("directory.exe");
        std::fs::create_dir(&directory).unwrap();
        assert!(ConfigFileEngineImage::open(directory, deadline, &|| Ok(())).is_err());
    }

    #[test]
    fn held_configfile_engine_constructor_refuses_unbound_and_foreign_original_owners() {
        let fixture = CompilerContextFixture::new();
        let runtime = crate::native_operations::NativeRuntime::controller(&fixture.0);
        let controller = runtime.managed_native_output(&fixture.0).unwrap();
        let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(10)).unwrap();
        assert_eq!(HeldConfigFileEngine::hold_configured(&runtime, &fixture.0, &parent, &controller)
            .err().unwrap(), "external_compile_read_owner_missing");
        let broker = Arc::new(crate::workspace_access::Broker::default());
        let foreign = broker.managed_compiler_controller().unwrap();
        let mut bound = ParentDeadline::at(Instant::now() + Duration::from_secs(10)).unwrap();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let check = || Ok(());
        let budget = Budget::new(Duration::from_secs(10), &check).unwrap();
        bound.bind_read_owner(&broker, cancelled.clone(),
            crate::external_workspace::ProposalLimits::from_budget(&budget)).unwrap();
        // The original read binding is real, but belongs to another broker.
        // Every complete-token attempt in this unit is required to refuse.
        assert!(HeldConfigFileEngine::hold_configured(&runtime, &fixture.0, &bound, &foreign).is_err());
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(HeldConfigFileEngine::hold_configured(&runtime, &fixture.0, &bound, &foreign).is_err());
        let expired = ParentDeadline::expired_for_test();
        assert!(HeldConfigFileEngine::hold_configured(&runtime, &fixture.0, &expired, &controller).is_err());
        assert_eq!(controller.retained(), 0);
        assert_eq!(foreign.retained(), 0);
    }

    struct CompilerContextFixture(PathBuf);
    impl CompilerContextFixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "compiler-discovery-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir(&path).unwrap();
            for name in ["program-files-x86", "program-files", "system", "rustup"] {
                std::fs::create_dir(path.join(name)).unwrap();
            }
            Self(path)
        }
        fn get(&self, name: &str) -> Option<PathBuf> {
            match name {
                "ProgramFiles(x86)" => Some(self.0.join("program-files-x86")),
                "ProgramFiles" => Some(self.0.join("program-files")),
                _ => None,
            }
        }
    }
    impl Drop for CompilerContextFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn compiler_discovery_context_forwards_canonical_roots_and_deduplicates_identities() {
        let fixture = CompilerContextFixture::new();
        let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(10)).unwrap();
        let context = CompilerDiscoveryContext::discover_with(
            |name| fixture.get(name).map(|path| path.join(".")),
            &parent,
        )
        .unwrap();
        assert_eq!(context.directories.len(), 2);
        for (name, value) in &context.env {
            let canonical =
                crate::project::clean_path(fixture.get(name).unwrap().canonicalize().unwrap());
            assert_eq!(value, &text_path(&canonical).unwrap());
            assert!(context.directories.contains(&canonical));
        }
        let shared = fixture.get("ProgramFiles").unwrap();
        let context =
            CompilerDiscoveryContext::discover_with(|_| Some(shared.clone()), &parent).unwrap();
        assert_eq!(context.directories.len(), 1);
        assert_eq!(context.env.len(), 2);
        assert_eq!(context.env[0].1, context.env[1].1);
    }
    #[test]
    fn compiler_discovery_context_refuses_missing_relative_nonordinary_and_expired_roots() {
        let fixture = CompilerContextFixture::new();
        let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(10)).unwrap();
        let file = fixture.0.join("not-a-directory");
        std::fs::write(&file, b"ordinary file").unwrap();
        for path in [
            None,
            Some(PathBuf::from("relative-root")),
            Some(file),
            Some(fixture.0.join("missing")),
            Some(fixture.0.join("invalid\nroot")),
            Some(fixture.0.join("x".repeat(4097))),
        ] {
            assert!(CompilerDiscoveryContext::discover_with(|_| path.clone(), &parent).is_err());
        }
        let expired = ParentDeadline::expired_for_test();
        assert!(
            CompilerDiscoveryContext::discover_with(|name| fixture.get(name), &expired).is_err()
        );
    }
    #[test]
    fn compiler_system_drive_derives_local_absolute_prefix_and_refuses_other_roots() {
        for (path, expected) in [
            (r"C:\Windows", "C:"),
            ("c:/Windows", "C:"),
            (r"A:\fixture\system", "A:"),
            (r"\\?\D:\Windows", "D:"),
        ] {
            assert_eq!(compiler_system_drive(Path::new(path)).unwrap(), expected);
        }
        for path in [
            "",
            "Windows",
            "/Windows",
            r"C:Windows",
            "C:",
            r"\\server\share\Windows",
            r"\\?\UNC\server\share\Windows",
            r"\\.\C:\Windows",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\Windows",
        ] {
            assert_eq!(
                compiler_system_drive(Path::new(path)).unwrap_err(),
                "external_compiler_system_drive_refused"
            );
        }
    }
    #[test]
    fn compiler_environment_keeps_discovery_hints_and_excludes_hostile_parent_overrides() {
        let fixture = CompilerContextFixture::new();
        let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(10)).unwrap();
        let hostile_drive = if compiler_system_drive(&fixture.0).unwrap() == "Z:" {
            "Y:"
        } else {
            "Z:"
        };
        let hostile = [
            "PATH",
            "LIB",
            "LIBPATH",
            "INCLUDE",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
            "ProgramData",
            "ALLUSERSPROFILE",
            "SystemDrive",
        ];
        let mut requested = Vec::new();
        let discovery = CompilerDiscoveryContext::discover_with(
            |name| {
                requested.push(name.to_owned());
                fixture.get(name).or_else(|| {
                    if name == "SystemDrive" {
                        Some(PathBuf::from(hostile_drive))
                    } else {
                        hostile
                            .contains(&name)
                            .then(|| fixture.0.join("HOSTILE_PARENT"))
                    }
                })
            },
            &parent,
        )
        .unwrap();
        assert_eq!(requested, ["ProgramFiles(x86)", "ProgramFiles"]);
        let program = fixture.0.join("compiler.exe");
        std::fs::write(&program, fixture_pe()).unwrap();
        let tools = Tools {
            cargo: Executable::open(program.clone(), &parent).unwrap(),
            rustc: Executable::open(program, &parent).unwrap(),
            system: fixture.0.join("system"),
            rustup: fixture.0.join("rustup"),
            discovery,
        };
        let root = fixture.0.join("private-compiler-state");
        let env = compiler_environment(&tools, &root).unwrap();
        let value = |name: &str| {
            env.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        for name in hostile
            .iter()
            .filter(|name| !matches!(**name, "PATH" | "SystemDrive"))
        {
            assert!(value(name).is_none(), "unexpected parent key: {name}");
        }
        assert!(env
            .iter()
            .all(|(_, value)| !value.contains("HOSTILE_PARENT")));
        let expected_drive = compiler_system_drive(&tools.system).unwrap();
        assert_eq!(value("SystemDrive"), Some(&expected_drive));
        assert_ne!(value("SystemDrive").unwrap(), hostile_drive);
        assert_eq!(
            std::env::split_paths(value("PATH").unwrap()).collect::<Vec<_>>(),
            vec![
                tools.cargo.invocation.parent().unwrap().to_path_buf(),
                tools.rustc.invocation.parent().unwrap().to_path_buf(),
                tools.system.join("System32"),
            ]
        );
        for (name, path) in &tools.discovery.env {
            assert_eq!(value(name), Some(path));
        }
        for (name, relative) in [
            ("CARGO_HOME", "cargo-home"),
            ("HOME", "profile"),
            ("USERPROFILE", "profile"),
            ("TEMP", "temp"),
            ("TMP", "temp"),
        ] {
            assert_eq!(value(name), Some(&text_path(&root.join(relative)).unwrap()));
        }
    }
    #[test]
    fn known_cargo_selected_build_seal_refuses_mutations_before_admission() {
        let selected = super::SelectedMember {
            package: "native-game".into(),
            package_id: "path+file:///A:/mock-fixture/native#native-game@0.1.0".into(),
            target: "native_game".into(),
            manifest: std::path::PathBuf::from("A:/mock-fixture/native/Cargo.toml"),
            source: std::path::PathBuf::from("A:/mock-fixture/native/src/lib.rs"),
        };
        // Mock checked selection tests binding only, not a metadata/guard proof.
        let checked = super::KnownCargoBuild::after_checked_selection(
            &selected,
            "x86_64-pc-windows-msvc",
            &selected.manifest,
            std::path::Path::new("A:/mock-fixture/compiler-state/target"),
        )
        .unwrap();
        assert_eq!(checked.arguments()[9], selected.package_id);
        assert_eq!(checked.arguments()[11], "x86_64-pc-windows-msvc");
        let foreign_checked = super::KnownCargoBuild::after_checked_selection(
            &selected,
            "x86_64-pc-windows-msvc",
            std::path::Path::new("A:/foreign-mock-fixture/native/Cargo.toml"),
            std::path::Path::new("A:/foreign-mock-fixture/compiler-state/target"),
        )
        .unwrap();
        crate::worker_windows::binary::real_cargo_diagnostic::tests::assert_selected_build_binding_mutations(&checked, &foreign_checked);
    }
    use super::*;
    #[test]
    fn stream_acceptance_requires_eofs_retirement_counts_and_deadline() {
        let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(1)).unwrap();
        let receipt = || StreamReceipt {
            stdout_bytes: 4,
            stderr_bytes: 3,
            stdout_eof: true,
            stderr_eof: true,
            root_exit_code: 0,
            owned_tree_retired: true,
            owned_readers_joined: true,
        };
        assert!(accepted_stream(&receipt(), 4, &parent).is_ok());
        for change in 0..7 {
            let mut r = receipt();
            match change {
                0 => r.stdout_eof = false,
                1 => r.stderr_eof = false,
                2 => r.owned_tree_retired = false,
                3 => r.owned_readers_joined = false,
                4 => r.root_exit_code = 1,
                5 => r.stdout_bytes = 5,
                6 => r.stderr_bytes = STDERR_BYTES + 1,
                _ => unreachable!(),
            };
            assert_eq!(
                accepted_stream(&r, 4, &parent).unwrap_err(),
                "external_compile_stream_receipt_refused"
            );
        }
        let expired = ParentDeadline::expired_for_test();
        assert_eq!(
            accepted_stream(&receipt(), 4, &expired).unwrap_err(),
            "external_compile_parent_deadline"
        );
        assert_eq!(
            strict_text(b"private invalid UTF8\xff", &parent).unwrap_err(),
            "external_compile_output_not_utf8"
        );
    }
    #[test]
    fn ancestor_configs_refuse_without_reading_contents() {
        let root =
            std::env::temp_dir().join(format!("compiler-config-{}", crate::random::session_id()));
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        std::fs::create_dir(root.join(".cargo")).unwrap();
        let parent = ParentDeadline::at(Instant::now() + Duration::from_secs(10)).unwrap();
        for name in ["config", "config.toml"] {
            let path = root.join(".cargo").join(name);
            let private = b"fixed fake PRIVATE_CONFIG_SECRET must never appear";
            std::fs::write(&path, private).unwrap();
            let exclusive = OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            assert_eq!(
                ancestor_configs_absent(&root.join("cwd"), &parent).unwrap_err(),
                "external_compiler_ancestor_config_refused"
            );
            drop(exclusive);
            assert_eq!(std::fs::read(&path).unwrap(), private);
            std::fs::remove_file(&path).unwrap();
        }
    }
    fn export_set32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn named_export_fixture() -> Vec<u8> {
        let mut bytes = vec![0u8; 0x1600];
        bytes[..2].copy_from_slice(b"MZ");
        export_set32(&mut bytes, 60, 64);
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[70..72].copy_from_slice(&2u16.to_le_bytes());
        bytes[84..86].copy_from_slice(&240u16.to_le_bytes());
        bytes[86..88].copy_from_slice(&0x2000u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        export_set32(&mut bytes, 88 + 56, 0x4000);
        export_set32(&mut bytes, 88 + 60, 0x400);
        export_set32(&mut bytes, 88 + 108, 16);
        export_set32(&mut bytes, 88 + 112, 0x2000);
        export_set32(&mut bytes, 88 + 116, 0x300);
        for (index, rva, virtual_bytes, raw, raw_bytes, flags) in [
            (0, 0x1000, 0x100, 0x400, 0x200, 0x6000_0020),
            (1, 0x2000, 0x1000, 0x600, 0x1000, 0x4000_0040),
        ] {
            let section = 328 + index * 40;
            for (field, value) in [(8, virtual_bytes), (12, rva), (16, raw_bytes), (20, raw), (36, flags)] {
                export_set32(&mut bytes, section + field, value);
            }
        }
        export_set32(&mut bytes, 0x600 + 16, 17); // Nonzero ordinal base; name index remains zero.
        export_set32(&mut bytes, 0x600 + 20, 1);
        export_set32(&mut bytes, 0x600 + 24, 1);
        export_set32(&mut bytes, 0x600 + 28, 0x2040);
        export_set32(&mut bytes, 0x600 + 32, 0x2050);
        export_set32(&mut bytes, 0x600 + 36, 0x2060);
        export_set32(&mut bytes, 0x640, 0x1000);
        export_set32(&mut bytes, 0x650, 0x2080);
        bytes[0x680..0x686].copy_from_slice(b"value\0");
        bytes[0x400] = 0xc3; // Bytes only; never executed by these tests.
        bytes
    }
    fn named_export_budget() -> Budget<'static> {
        Budget::new(Duration::from_secs(30), &|| Ok(())).unwrap()
    }
    fn read_test_export(bytes: &[u8], name: &str) -> Result<NamedPeExport, String> {
        let mut reader = io::Cursor::new(bytes);
        reader.set_position(7);
        let result = named_direct_pe_export(&mut reader, bytes.len() as u64,
            "x86_64-pc-windows-msvc", name, &named_export_budget());
        assert_eq!(reader.position(), 7, "all parser results restore the same reader");
        result
    }
    #[test]
    fn named_pe_export_accepts_direct_name_aliases_and_unbiased_ordinal_index() {
        let mut bytes = named_export_fixture();
        let found = read_test_export(&bytes, "value").unwrap();
        assert_eq!(found.name(), "value");
        assert_eq!(found.rva(), 0x1000);
        export_set32(&mut bytes, 0x600 + 24, 2);
        export_set32(&mut bytes, 0x654, 0x2090);
        bytes[0x680..0x686].copy_from_slice(b"Value\0");
        bytes[0x690..0x696].copy_from_slice(b"value\0");
        // Two distinct names may validly alias the one function, with base17.
        assert_eq!(read_test_export(&bytes, "Value").unwrap().rva(), 0x1000);
        assert_eq!(read_test_export(&bytes, "value").unwrap().rva(), 0x1000);
        assert_eq!(read_test_export(&bytes, "VALUE").unwrap_err(), "external_named_export_missing");
        let mut longest = named_export_fixture();
        longest[0x680..0x700].fill(b'a');
        longest[0x700] = 0;
        assert!(read_test_export(&longest, &"a".repeat(128)).is_ok());
        let mut maximum = named_export_fixture();
        maximum.resize(0x20600, 0);
        export_set32(&mut maximum, 88 + 56, 0x24000);
        export_set32(&mut maximum, 88 + 116, 0x11000);
        export_set32(&mut maximum, 368 + 8, 0x20000);
        export_set32(&mut maximum, 368 + 16, 0x20000);
        export_set32(&mut maximum, 0x600 + 20, 4096);
        export_set32(&mut maximum, 0x600 + 24, 4096);
        export_set32(&mut maximum, 0x600 + 32, 0x6040);
        export_set32(&mut maximum, 0x600 + 36, 0xa040);
        maximum[0x640..0x20600].fill(0);
        export_set32(&mut maximum, 0x640, 0x1000);
        for index in 0..4096 {
            export_set32(&mut maximum, 0x4640 + index * 4, 0xc040 + index as u32 * 6);
            let name = format!("n{index:04}\0");
            maximum[0xa640 + index * 6..0xa646 + index * 6].copy_from_slice(name.as_bytes());
        }
        assert_eq!(read_test_export(&maximum, "n4095").unwrap().rva(), 0x1000);
    }
    #[test]
    fn named_pe_export_refuses_header_directory_table_and_entry_bounds() {
        for offset in [0usize, 64, 68, 70, 87, 88] {
            let mut bytes = named_export_fixture();
            bytes[offset] ^= 0xff;
            assert!(read_test_export(&bytes, "value").is_err());
        }
        for (offset, value) in [
            (60, u32::MAX), (88 + 60, 0), (88 + 60, 407), (88 + 60, u32::MAX),
            (88 + 108, 0), (88 + 108, 17),
            (88 + 112, 0), (88 + 112, u32::MAX - 10), (88 + 116, 39),
            (0x600, 1), (0x600 + 20, 0), (0x600 + 20, 4097),
            (0x600 + 24, 0), (0x600 + 24, 4097),
            (0x600 + 28, u32::MAX), (0x600 + 32, u32::MAX), (0x600 + 36, u32::MAX),
            (328 + 20, u32::MAX), (328 + 12, u32::MAX - 10),
        ] {
            let mut bytes = named_export_fixture();
            export_set32(&mut bytes, offset, value);
            assert!(read_test_export(&bytes, "value").is_err(), "malformed range/count was accepted");
        }
        let mut too_many = named_export_fixture();
        too_many[70..72].copy_from_slice(&97u16.to_le_bytes());
        assert!(read_test_export(&too_many, "value").is_err());
        let mut short_optional = named_export_fixture();
        short_optional[84..86].copy_from_slice(&112u16.to_le_bytes());
        assert!(read_test_export(&short_optional, "value").is_err());
        let bytes = named_export_fixture();
        for length in [0, 63, 80, 200, 400, 0x1000] {
            assert!(read_test_export(&bytes[..length], "value").is_err());
        }
        let mut reader = io::Cursor::new(&bytes);
        reader.set_position(7);
        assert!(named_direct_pe_export(&mut reader, 128 * 1024 * 1024 + 1,
            "x86_64-pc-windows-msvc", "value", &named_export_budget()).is_err());
        assert_eq!(reader.position(), 7);
    }
    #[test]
    fn named_pe_export_requires_sorted_unique_bounded_ascii_names_and_valid_ordinals() {
        for (first, second) in [(b"value\0".as_slice(), b"value\0".as_slice()),
            (b"zebra\0".as_slice(), b"value\0".as_slice())] {
            let mut bytes = named_export_fixture();
            export_set32(&mut bytes, 0x600 + 24, 2);
            export_set32(&mut bytes, 0x654, 0x2090);
            bytes[0x680..0x686].copy_from_slice(first);
            bytes[0x690..0x696].copy_from_slice(second);
            assert_eq!(read_test_export(&bytes, "value").unwrap_err(), "external_named_export_name_order");
        }
        let mut invalid = named_export_fixture();
        invalid[0x660..0x662].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(read_test_export(&invalid, "value").unwrap_err(), "external_named_export_ordinal_refused");
        let mut non_ascii = named_export_fixture();
        non_ascii[0x680] = 0xff;
        assert_eq!(read_test_export(&non_ascii, "value").unwrap_err(), "external_named_export_name_bound");
        let mut overlong = named_export_fixture();
        overlong[0x680..0x701].fill(b'a');
        assert_eq!(read_test_export(&overlong, "value").unwrap_err(), "external_named_export_name_bound");
        let mut unterminated = named_export_fixture();
        export_set32(&mut unterminated, 0x650, 0x2ff0);
        unterminated[0x15f0..].fill(b'a');
        assert_eq!(read_test_export(&unterminated, "value").unwrap_err(), "external_named_export_name_bound");
        for name in ["", "a\0b", "é"] { assert!(read_test_export(&named_export_fixture(), name).is_err()); }
        assert!(read_test_export(&named_export_fixture(), &"a".repeat(129)).is_err());
    }
    #[test]
    fn named_pe_export_refuses_forwarded_zero_data_virtual_tail_and_ambiguous_targets() {
        for (rva, error) in [(0, "external_named_export_zero_target"),
            (0x20a0, "external_named_export_forwarder_refused"),
            (0x2400, "external_named_export_executable_target_required")] {
            let mut bytes = named_export_fixture();
            export_set32(&mut bytes, 0x640, rva);
            assert_eq!(read_test_export(&bytes, "value").unwrap_err(), error);
        }
        let mut tail = named_export_fixture();
        export_set32(&mut tail, 328 + 8, 0x400);
        export_set32(&mut tail, 0x640, 0x1200);
        assert_eq!(read_test_export(&tail, "value").unwrap_err(), "external_named_export_unbacked_rva");
        for raw in [0, 0x3ff] {
            let mut header_alias = named_export_fixture();
            export_set32(&mut header_alias, 328 + 20, raw);
            assert_eq!(read_test_export(&header_alias, "value").unwrap_err(), "external_named_export_header_mapping");
        }
        let mut rva_alias = named_export_fixture();
        export_set32(&mut rva_alias, 328 + 12, 0x100);
        export_set32(&mut rva_alias, 0x640, 0x100);
        assert_eq!(read_test_export(&rva_alias, "value").unwrap_err(), "external_named_export_header_mapping");
        let mut ambiguous = named_export_fixture();
        ambiguous[70..72].copy_from_slice(&3u16.to_le_bytes());
        for (field, value) in [(8, 0x200), (12, 0x1000), (16, 0x200), (20, 0x1400), (36, 0x6000_0020)] {
            export_set32(&mut ambiguous, 328 + 80 + field, value);
        }
        assert_eq!(read_test_export(&ambiguous, "value").unwrap_err(), "external_named_export_ambiguous_rva");
    }
    struct ExportReadFailure {
        inner: io::Cursor<Vec<u8>>,
        reads: usize,
        fail_read: bool,
        fail_restore: bool,
    }
    impl Read for ExportReadFailure {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.fail_read && self.reads == 3 { return Err(io::Error::other("synthetic bounded export read refusal")); }
            self.inner.read(bytes)
        }
    }
    impl io::Seek for ExportReadFailure {
        fn seek(&mut self, from: io::SeekFrom) -> io::Result<u64> {
            if self.fail_restore && self.reads > 0 && matches!(from, io::SeekFrom::Start(7)) {
                return Err(io::Error::other("synthetic bounded export restore refusal"));
            }
            io::Seek::seek(&mut self.inner, from)
        }
    }
    #[test]
    fn named_pe_export_restores_cursor_on_cancel_expiry_read_error_and_missing_name() {
        let bytes = named_export_fixture();
        let calls = std::cell::Cell::new(0usize);
        let cancel = || {
            calls.set(calls.get() + 1);
            if calls.get() == 10 { Err("named_export_test_cancelled".into()) } else { Ok(()) }
        };
        let cancelled = Budget::new(Duration::from_secs(30), &cancel).unwrap();
        let mut reader = io::Cursor::new(&bytes);
        reader.set_position(7);
        assert_eq!(named_direct_pe_export(&mut reader, bytes.len() as u64,
            "x86_64-pc-windows-msvc", "value", &cancelled).unwrap_err(), "named_export_test_cancelled");
        assert_eq!(calls.get(), 10);
        assert_eq!(reader.position(), 7);
        let expired = ParentDeadline::expired_for_test();
        let check = || expired.check();
        let budget = Budget::new(Duration::from_secs(30), &check).unwrap();
        assert!(named_direct_pe_export(&mut reader, bytes.len() as u64,
            "x86_64-pc-windows-msvc", "value", &budget).is_err());
        assert_eq!(reader.position(), 7);
        let mut failing = ExportReadFailure { inner: io::Cursor::new(bytes.clone()), reads: 0, fail_read: true, fail_restore: false };
        failing.inner.set_position(7);
        assert_eq!(named_direct_pe_export(&mut failing, bytes.len() as u64,
            "x86_64-pc-windows-msvc", "value", &named_export_budget()).unwrap_err(), "external_named_export_read_failed");
        assert_eq!(failing.inner.position(), 7);
        assert_eq!(read_test_export(&bytes, "missing_value").unwrap_err(), "external_named_export_missing");
    }
    #[test]
    fn named_pe_export_aggregate_admission_and_restore_failure_never_return_descriptor() {
        let bytes = named_export_fixture();
        let budget = named_export_budget();
        let mut cursor = io::Cursor::new(&bytes);
        cursor.set_position(7);
        let mut input = PeExportRead { reader: &mut cursor, bytes: bytes.len() as u64,
            charged: NAMED_EXPORT_READ_BYTES - 1, budget: &budget };
        assert_eq!(input.array::<2>(0).unwrap_err(), "external_named_export_read_bound");
        assert_eq!(input.reader.position(), 7, "admission precedes seek/read");
        input.charged = 0;
        assert_eq!(input.array::<4>(u64::MAX).unwrap_err(), "external_named_export_file_range");
        assert_eq!(input.reader.position(), 7);
        let mut failing = ExportReadFailure { inner: io::Cursor::new(bytes.clone()), reads: 0, fail_read: false, fail_restore: true };
        failing.inner.set_position(7);
        assert_eq!(named_direct_pe_export(&mut failing, bytes.len() as u64,
            "x86_64-pc-windows-msvc", "value", &budget).unwrap_err(), "external_named_export_cursor_restore_failed");
    }

    pub(super) fn fixture_pe() -> Vec<u8> {
        let mut bytes = vec![0u8; 512];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
        bytes[84..86].copy_from_slice(&112u16.to_le_bytes());
        bytes[86..88].copy_from_slice(&0x2000u16.to_le_bytes());
        bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        bytes
    }
    #[test]
    fn private_pe_receipt_requires_actual_header_bounds_machine_and_dll_flag() {
        let bytes = fixture_pe();
        let receipt = pe_receipt(&bytes, "x86_64-pc-windows-msvc").unwrap();
        assert_eq!(receipt["sha256"], crate::sha256_hex(&bytes));
        assert_eq!(receipt["loaded"], false);
        for change in [0usize, 64, 68, 70, 87, 88] {
            let mut invalid = bytes.clone();
            invalid[change] ^= 0xff;
            assert_eq!(
                pe_receipt(&invalid, "x86_64-pc-windows-msvc").unwrap_err(),
                "external_native_artifact_pe_refused"
            );
        }
        assert!(pe_receipt(&bytes[..100], "x86_64-pc-windows-msvc").is_err());
        assert!(pe_receipt(&bytes, "aarch64-pc-windows-msvc").is_err());
    }
    #[test]
    fn existing_deadline_cannot_be_renewed_for_compiler_or_late_acceptance() {
        assert!(ParentDeadline::at(Instant::now() - Duration::from_millis(1)).is_err());
        let deadline = Instant::now() + Duration::from_secs(2);
        let parent = ParentDeadline::at(deadline).unwrap();
        assert!(parent.instant() <= deadline);
        assert!(parent.budget(Duration::from_secs(120)).unwrap() <= Duration::from_secs(2));
    }
    #[test]
    fn edited_source_native_parent_origin_and_original_context_are_fixed() {
        assert_eq!(
            ParentDeadline::at(Instant::now() + Duration::from_secs(181))
                .err()
                .unwrap(),
            "external_compile_native_parent_window_bound"
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut parent = ParentDeadline::at(deadline).unwrap();
        let replacement = ParentDeadline::at(deadline).unwrap();
        assert_ne!(parent.origin(), replacement.origin());
        let origin = parent.origin().clone();
        parent.deadline = Instant::now() - Duration::from_millis(1);
        let check = || Ok(());
        let _fresh_budget = Budget::new(Duration::from_secs(60), &check).unwrap();
        assert_eq!(parent.origin(), &origin);
        assert_eq!(
            parent.check().err().unwrap(),
            "external_compile_parent_deadline"
        );
        let operation = OperationContext::new(Instant::now() + Duration::from_secs(120));
        let captured = operation
            .scope(|| ParentDeadline::at(deadline).unwrap())
            .unwrap();
        operation.request_cancel().unwrap();
        let fresh = OperationContext::new(Instant::now() + Duration::from_secs(120));
        fresh
            .scope(|| {
                assert_eq!(
                    captured.check().err().unwrap(),
                    "external_compile_cancelled_or_expired"
                );
            })
            .unwrap();
    }
    #[test]
    fn edited_source_native_parent_read_owner_cannot_be_rebound_or_cancelled_away() {
        let broker = Arc::new(crate::workspace_access::Broker::default());
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let check = || Ok(());
        let b = Budget::new(Duration::from_secs(30), &check).unwrap();
        let limits = crate::external_workspace::ProposalLimits::from_budget(&b);
        let mut parent = ParentDeadline::at(Instant::now() + Duration::from_secs(120)).unwrap();
        let deadline = parent.instant();
        parent
            .bind_read_owner(&broker, cancelled.clone(), limits)
            .unwrap();
        let unrelated = OperationContext::new(Instant::now() + Duration::from_secs(120));
        unrelated
            .scope(|| {
                assert_eq!(
                    parent.check().err().unwrap(),
                    "external_compile_read_operation_binding_mismatch"
                );
            })
            .unwrap();
        assert_eq!(
            parent
                .bind_read_owner(&broker, cancelled.clone(), limits)
                .err()
                .unwrap(),
            "external_compile_read_owner_already_bound"
        );
        let controller = StreamController::for_runtime();
        assert!(parent.check_read_controller(&controller).is_err());
        controller.bind_parent(&broker).unwrap();
        parent.check_read_controller(&controller).unwrap();
        assert_eq!(parent.instant(), deadline);
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(
            parent.check().err().unwrap(),
            "external_compile_read_owner_cancelled_or_closed"
        );
        assert_eq!(controller.retained(), 0);
    }
    #[test]
    fn compiler_host_refuses_duplicate_unknown_or_private_metadata() {
        assert_eq!(
            compiler_host("rustc version\nhost: x86_64-pc-windows-msvc\n").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        for input in [
            "host: SECRET_PRIVATE_METADATA",
            "host: x86_64-pc-windows-msvc\nhost: aarch64-pc-windows-msvc",
            "host: x86_64-unknown-linux-gnu",
        ] {
            let error = compiler_host(input).unwrap_err();
            assert!(!error.contains("SECRET_PRIVATE_METADATA"));
        }
    }
}
