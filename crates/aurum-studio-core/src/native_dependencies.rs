//! Private native dependency preparation. Compiler descendants belong to the host.
use crate::operation_context::{OperationContext, Phase};
use crate::{files, snapshot, Command, Project};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PLAN_BYTES: u64 = 256 * 1024;
pub const COMPILER_SECONDS: u64 = 120;
fn operation_check() -> Result<(), String> {
    if let Some(context) = OperationContext::current() {
        context.check().map_err(|error| error.to_string())?;
    }
    Ok(())
}
fn operation_phase(phase: Phase) -> Result<(), String> {
    operation_check()?;
    if let Some(context) = OperationContext::current() {
        context.phase(phase);
    }
    Ok(())
}
fn operation_budget(local: Duration) -> Result<Duration, String> {
    OperationContext::current().map_or(Ok(local), |context| {
        context.budget(local).map_err(|error| error.to_string())
    })
}

fn bounded_json(path: &Path) -> Result<Value, String> {
    crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(PLAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > PLAN_BYTES {
        return Err("native dependency plan exceeds 256 KiB".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid native dependency plan".into())
}
fn confined_absolute(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map(crate::project::clean_path)
        .map_err(|error| error.to_string())?;
    let path = crate::project::clean_path(path.to_path_buf());
    let relative = path
        .strip_prefix(&root)
        .map_err(|_| "native dependency escaped private source")?;
    let result = files::confined(
        &root,
        relative.to_str().ok_or("native paths must be UTF-8")?,
    )?;
    crate::diagnostics::checked_path(&result).map_err(|error| error.to_string())?;
    Ok(result)
}
fn validate_graph(
    root: &Path,
    workspace: &Path,
    metadata: &Value,
    package: &str,
) -> Result<(String, String), String> {
    if metadata["version"] != 1 {
        return Err("unsupported Cargo metadata version".into());
    }
    let observed_workspace = confined_absolute(
        root,
        Path::new(
            metadata["workspace_root"]
                .as_str()
                .ok_or("missing Cargo workspace")?,
        ),
    )?;
    if !crate::ownership::paths_equal(workspace, &observed_workspace) {
        return Err(
            "Cargo metadata selected a different workspace than the copied project layout".into(),
        );
    }
    let packages = metadata["packages"]
        .as_array()
        .filter(|packages| packages.len() <= 4096)
        .ok_or("invalid or oversized Cargo graph")?;
    let mut selected = None;
    for entry in packages {
        if entry["source"].is_null() {
            confined_absolute(
                root,
                Path::new(
                    entry["manifest_path"]
                        .as_str()
                        .ok_or("missing local manifest")?,
                ),
            )?;
            for target in entry["targets"].as_array().ok_or("missing Cargo targets")? {
                confined_absolute(
                    root,
                    Path::new(
                        target["src_path"]
                            .as_str()
                            .ok_or("missing local Rust source")?,
                    ),
                )?;
            }
        }
        if entry["name"] == package {
            if selected.is_some() || !entry["source"].is_null() {
                return Err("native Rust package must be one local workspace package".into());
            }
            let targets: Vec<_> = entry["targets"]
                .as_array()
                .ok_or("missing native Rust targets")?
                .iter()
                .filter(|target| {
                    target["crate_types"]
                        .as_array()
                        .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"))
                })
                .collect();
            if targets.len() != 1 {
                return Err("native Rust package must provide exactly one cdylib target".into());
            }
            selected = Some((
                entry["id"]
                    .as_str()
                    .ok_or("missing Cargo package identity")?
                    .to_string(),
                targets[0]["name"]
                    .as_str()
                    .ok_or("missing native target name")?
                    .to_string(),
            ));
        }
    }
    selected.ok_or("configured native Rust package is absent from the private workspace".into())
}
fn artifact(stdout: &str, package: &str, target: &str, output: &Path) -> Result<PathBuf, String> {
    let mut selected = None;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["reason"] != "compiler-artifact"
            || value["package_id"] != package
            || value["target"]["name"] != target
        {
            continue;
        }
        for filename in value["filenames"]
            .as_array()
            .ok_or("invalid compiler artifact filenames")?
        {
            let path = Path::new(filename.as_str().ok_or("invalid compiler artifact path")?);
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
            {
                let path = confined_absolute(output, path)?;
                if selected.as_ref().is_some_and(|previous| previous != &path) {
                    return Err("compiler reported multiple native libraries".into());
                }
                selected = Some(path);
            }
        }
    }
    selected.ok_or(
        "Cargo did not report the configured native library; stale artifacts are not accepted"
            .into(),
    )
}

pub(crate) fn prepare(
    project: &Project,
    workspace: &Path,
    engine: &Path,
    userdata: &Path,
) -> Result<Value, String> {
    operation_phase(Phase::Dependencies)?;
    if project.config.validation_command.is_some() {
        return Err(
            "private native dependencies require copied source without custom validators".into(),
        );
    }
    project.confined_build_workspace()?;
    let godot = project
        .godot_project_dir()
        .ok_or("native dependencies require project.godot")?;
    let inventory = snapshot::inventory(godot)?;
    let mut manifests = Vec::new();
    for (path, bytes) in inventory
        .iter()
        .filter(|(path, _)| path.to_ascii_lowercase().ends_with(".gdextension"))
    {
        operation_check()?;
        if *bytes > 65536 || manifests.len() >= 16 {
            return Err("native manifests exceed the 16-file/64-KiB bounds".into());
        }
        manifests.push(confined_absolute(&project.root, &godot.join(path))?);
    }
    if manifests.is_empty() {
        if project.config.rust_package.is_some() {
            return Err("configured Rust project has no GDExtension manifest".into());
        }
        return Ok(json!({"ok":true,"compiled":false,"manifests":[],"native":false}));
    }
    let guard = workspace.join("dependency-probe");
    files::write_atomic(
        &guard.join("project.godot"),
        b"config_version=5\n[application]\nconfig/name=\"AurumDependencyProbe\"\n",
    )
    .map_err(|error| error.to_string())?;
    let script = guard.join("probe.gd");
    let helper = guard.join("plan.gd");
    files::write_atomic(&script, include_bytes!("native_dependency_probe.gd"))
        .map_err(|error| error.to_string())?;
    files::write_atomic(&helper, include_bytes!("native_dependency_plan.gd"))
        .map_err(|error| error.to_string())?;
    let input = guard.join("input.json");
    let output = guard.join("output.json");
    let clean = |path: &Path| path.to_string_lossy().replace('\\', "/");
    files::write_atomic(&input,&serde_json::to_vec(&json!({"project":clean(&project.root),"godot":clean(godot),"manifests":manifests.iter().map(|path|clean(path)).collect::<Vec<_>>()})).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
    let mut command = Command::new(engine)
        .args(["--headless", "--path"])
        .arg(clean(&guard))
        .arg("--script")
        .arg(clean(&script))
        .arg("--")
        .arg(clean(&input))
        .arg(clean(&output))
        .arg(clean(&helper))
        .candidate_storage(workspace);
    for key in [
        "APPDATA",
        "LOCALAPPDATA",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
    ] {
        command = command.env(key, clean(userdata));
    }
    let result = crate::native_runtime::run(&command, operation_budget(Duration::from_secs(30))?)
        .map_err(|error| error.to_string())?;
    operation_check()?;
    let mut plan = bounded_json(&output).map_err(|_| {
        format!(
            "native dependency probe failed: {}",
            crate::diagnostics::text_line(&result.tail(8).join("\n"), &[])
        )
    })?;
    if !result.success() || plan["ok"] != true {
        return Err(plan["error"]
            .as_str()
            .unwrap_or("native dependency probe failed")
            .into());
    }
    let rows = plan["manifests"]
        .as_array()
        .filter(|rows| rows.len() == manifests.len())
        .ok_or("invalid native manifest plan")?;
    for row in rows {
        for path in row["references"]
            .as_array()
            .filter(|paths| paths.len() <= 512)
            .ok_or("invalid native reference list")?
        {
            confined_absolute(
                &project.root,
                Path::new(path.as_str().ok_or("invalid native reference path")?),
            )?;
        }
    }
    let mut compiled = None;
    if let Some(package) = &project.config.rust_package {
        let configured = project
            .layout
            .gdextension
            .as_ref()
            .ok_or("Rust add-on manifest was not found in its configured bin directory")?;
        let configured = confined_absolute(&project.root, configured)?;
        let row = rows
            .iter()
            .find(|row| {
                row["manifest"].as_str().is_some_and(|path| {
                    confined_absolute(&project.root, Path::new(path))
                        .is_ok_and(|path| crate::ownership::paths_equal(&configured, &path))
                })
            })
            .ok_or("configured Rust manifest is absent from native plan")?;
        let destination = confined_absolute(
            &project.root,
            Path::new(
                row["library"]
                    .as_str()
                    .ok_or("missing Rust library destination")?,
            ),
        )?;
        let workspace_root = project.confined_build_workspace()?;
        let lock = workspace_root.join("Cargo.lock");
        if !lock.is_file() {
            return Err("private Rust builds require an existing pinned Cargo.lock; prepare dependencies before native_start".into());
        }
        let lock_sha = crate::sha256_file(&lock).map_err(|error| error.to_string())?;
        let source_sha = snapshot::fingerprint_all(&project.root)?;
        let cargo = crate::toolchain::find_on_path("cargo")
            .ok_or("native Rust preparation requires Cargo")?;
        let rustc = crate::toolchain::find_on_path("rustc")
            .ok_or("native Rust preparation requires rustc")?;
        let version = Command::new(rustc)
            .args(["-vV"])
            .candidate_storage(workspace)
            .run(operation_budget(Duration::from_secs(10))?)
            .map_err(|error| error.to_string())?;
        let host = version
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .ok_or("Rust compiler host identity is unavailable")?;
        let architecture = match plan["architecture"].as_str() {
            Some("x86_64") => "x86_64",
            Some("arm64") => "aarch64",
            _ => return Err("native runtime architecture is unsupported".into()),
        };
        if !host.starts_with(&format!("{architecture}-")) || !host.contains("windows") {
            return Err("Rust host and Godot architecture differ; native cross-toolchain qualification is required".into());
        }
        let target = workspace.join("cargo-target");
        std::fs::create_dir(&target).map_err(|error| error.to_string())?;
        let base = Command::new(cargo)
            .directory(&workspace_root)
            .candidate_storage(workspace)
            .env("CARGO_TARGET_DIR", clean(&target))
            .env("CARGO_INCREMENTAL", "0");
        let metadata = base
            .clone()
            .args(["metadata", "--manifest-path"])
            .arg(clean(&workspace_root.join("Cargo.toml")))
            .args([
                "--frozen",
                "--offline",
                "--format-version",
                "1",
                "--filter-platform",
                host,
            ])
            .run(operation_budget(Duration::from_secs(30))?)
            .map_err(|error| error.to_string())?;
        operation_check()?;
        if !metadata.success() {
            return Err(format!(
                "private Cargo metadata failed; dependencies must be available offline: {}",
                crate::diagnostics::text_line(&metadata.stderr, &[])
            ));
        }
        let metadata: Value = serde_json::from_str(&metadata.stdout)
            .map_err(|_| "invalid or truncated Cargo metadata")?;
        confined_absolute(
            workspace,
            Path::new(
                metadata["target_directory"]
                    .as_str()
                    .ok_or("missing Cargo target directory")?,
            ),
        )?;
        if let Some(directory) = metadata["build_directory"].as_str() {
            confined_absolute(workspace, Path::new(directory))?;
        }
        let (package_id, target_name) =
            validate_graph(&project.root, &workspace_root, &metadata, package)?;
        if snapshot::fingerprint_all(&project.root)? != source_sha
            || crate::sha256_file(&lock).map_err(|error| error.to_string())? != lock_sha
            || project.confined_build_workspace()? != workspace_root
        {
            return Err(
                "Cargo metadata changed copied source or workspace identity; compilation refused"
                    .into(),
            );
        }
        operation_phase(Phase::Compiling)?;
        let build = base
            .args(["build", "--manifest-path"])
            .arg(clean(&workspace_root.join("Cargo.toml")))
            .args([
                "--lib",
                "--frozen",
                "--offline",
                "--jobs",
                "1",
                "--package",
                &package_id,
                "--target",
                host,
                "--target-dir",
            ])
            .arg(clean(&target))
            .args([
                "--config",
                "profile.dev.debug=0",
                "--config",
                "profile.dev.codegen-units=1",
                "--message-format=json-render-diagnostics",
                "--color",
                "never",
            ])
            .run(operation_budget(Duration::from_secs(COMPILER_SECONDS))?)
            .map_err(|error| error.to_string())?;
        operation_check()?;
        if !build.success() {
            return Err(format!(
                "private Rust compilation failed; previous game retained: {}",
                crate::diagnostics::text_line(&build.stderr, &[])
            ));
        }
        if snapshot::fingerprint_all(&project.root)? != source_sha
            || crate::sha256_file(&lock).map_err(|error| error.to_string())? != lock_sha
        {
            return Err(
                "Rust build changed private source or lockfile; refusing activation".into(),
            );
        }
        let artifact = artifact(&build.stdout, &package_id, &target_name, &target)?;
        operation_phase(Phase::Staging)?;
        let sha = crate::sha256_file(&artifact).map_err(|error| error.to_string())?;
        let mut binary = Vec::new();
        std::fs::File::open(&artifact)
            .map_err(|error| error.to_string())?
            .take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut binary)
            .map_err(|error| error.to_string())?;
        if binary.len() > 128 * 1024 * 1024 {
            return Err("native Rust library exceeds 128 MiB".into());
        }
        if crate::sha256_hex(&binary) != sha {
            return Err(
                "native compiler artifact changed while staging; activation refused".into(),
            );
        }
        operation_check()?;
        files::write_atomic(&destination, &binary).map_err(|error| error.to_string())?;
        operation_check()?;
        compiled = Some(
            json!({"package":package,"target":target_name,"sha256":sha,"library":destination,"lock_sha256":lock_sha,"offline":true}),
        );
    }
    for row in plan["manifests"]
        .as_array_mut()
        .ok_or("missing native plan")?
    {
        operation_check()?;
        let library = confined_absolute(
            &project.root,
            Path::new(row["library"].as_str().ok_or("missing native library")?),
        )?;
        if !library.is_file() {
            return Err("selected native library is missing from the private copy".into());
        }
        row["sha256"] = json!(crate::sha256_file(&library).map_err(|error| error.to_string())?);
        let mut hashes = serde_json::Map::new();
        for path in row["dependencies"]
            .as_array()
            .ok_or("missing native dependencies")?
        {
            operation_check()?;
            let path = path.as_str().ok_or("invalid native dependency")?;
            let target = confined_absolute(&project.root, Path::new(path))?;
            if !target.is_file() {
                return Err("selected native dependency is missing".into());
            }
            hashes.insert(
                path.to_string(),
                json!(crate::sha256_file(&target).map_err(|error| error.to_string())?),
            );
        }
        row["dependency_hashes"] = Value::Object(hashes);
        row.as_object_mut()
            .ok_or("invalid native manifest row")?
            .remove("references");
    }
    plan["compiled"] = compiled.unwrap_or(Value::Null);
    plan["native"] = json!(true);
    operation_check()?;
    Ok(plan)
}

pub(crate) fn verify(expected: &Value, actual: &Value) -> Result<(), String> {
    operation_phase(Phase::Dependencies)?;
    if actual["ok"] != true {
        return Err("running native dependency inspection failed".into());
    }
    let observed = actual["manifests"]
        .as_array()
        .ok_or("invalid running native dependencies")?;
    for row in expected["manifests"]
        .as_array()
        .ok_or("invalid prepared native dependencies")?
    {
        operation_check()?;
        let live = observed
            .iter()
            .find(|live| live["manifest"] == row["manifest"])
            .ok_or("prepared native manifest is not present in the game")?;
        if live["loaded"] != true
            || live["library"] != row["library"]
            || live["sha256"] != row["sha256"]
            || live["dependency_hashes"] != row["dependency_hashes"]
        {
            return Err(
                "running native library identity differs from the privately prepared build".into(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_dependency_preparation_never_creates_a_probe_or_runs_an_engine() {
        let fixture = Fixture::new();
        std::fs::write(
            fixture.0.join("aurum.toml"),
            "schema_version=1\nname=\"cancelled\"\nmodules=[]\n",
        )
        .unwrap();
        let project = Project::open(&fixture.0).unwrap();
        let workspace = fixture.0.join("private");
        let context = OperationContext::new(std::time::Instant::now() + Duration::from_secs(5));
        context
            .scope(|| {
                context.request_cancel().unwrap();
                let error = prepare(
                    &project,
                    &workspace,
                    Path::new("absent-engine.exe"),
                    &workspace.join("data"),
                )
                .unwrap_err();
                assert!(error.contains("cancellation"), "{error}");
            })
            .unwrap();
        assert!(!workspace.exists());
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "aurum-native-dependency-test-{}",
                crate::random::session_id()
            ));
            std::fs::create_dir_all(path.join("src")).unwrap();
            std::fs::write(path.join("Cargo.toml"), "").unwrap();
            std::fs::write(path.join("src/lib.rs"), "").unwrap();
            Self(crate::project::clean_path(path.canonicalize().unwrap()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn local_cargo_graph_and_slash_spelling_are_confined() {
        let fixture = Fixture::new();
        let metadata = json!({"version":1,"workspace_root":fixture.0,"packages":[{"name":"game","id":"opaque-package-id","source":null,"manifest_path":fixture.0.join("Cargo.toml"),"targets":[{"name":"custom_library","crate_types":["cdylib"],"src_path":fixture.0.join("src/lib.rs")}]}]});
        assert_eq!(
            validate_graph(&fixture.0, &fixture.0, &metadata, "game").unwrap(),
            ("opaque-package-id".into(), "custom_library".into())
        );
        let spelling = fixture
            .0
            .join("src/lib.rs")
            .to_string_lossy()
            .replace('\\', "/");
        assert_eq!(
            confined_absolute(&fixture.0, Path::new(&spelling)).unwrap(),
            fixture.0.join("src/lib.rs")
        );
        let mut external = metadata.clone();
        external["packages"][0]["targets"][0]["src_path"] =
            json!(fixture.0.parent().unwrap().join("external.rs"));
        assert!(validate_graph(&fixture.0, &fixture.0, &external, "game").is_err());
    }

    #[test]
    fn nested_cargo_graph_rejects_workspace_rebinding_and_external_local_members() {
        let fixture = Fixture::new();
        let workspace = fixture.0.join("rust");
        let package = workspace.join("game");
        std::fs::create_dir_all(package.join("src")).unwrap();
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(package.join("Cargo.toml"), "").unwrap();
        std::fs::write(package.join("src/lib.rs"), "").unwrap();
        let metadata = json!({"version":1,"workspace_root":workspace,"packages":[{
            "name":"game","id":"nested-package-id","source":null,
            "manifest_path":package.join("Cargo.toml"),"targets":[{
                "name":"renamed_nested_library","crate_types":["cdylib"],
                "src_path":package.join("src/lib.rs")}]}]});
        assert_eq!(
            validate_graph(&fixture.0, &workspace, &metadata, "game").unwrap(),
            ("nested-package-id".into(), "renamed_nested_library".into())
        );
        let mut rebound = metadata.clone();
        rebound["workspace_root"] = json!(fixture.0);
        assert!(validate_graph(&fixture.0, &workspace, &rebound, "game").is_err());
        let mut external = metadata;
        external["packages"].as_array_mut().unwrap().push(json!({
            "name":"external-support","id":"external-package-id","source":null,
            "manifest_path":fixture.0.parent().unwrap().join("external/Cargo.toml"),"targets":[]}));
        assert!(validate_graph(&fixture.0, &workspace, &external, "game").is_err());
    }
    #[test]
    fn dependency_verification_requires_loaded_matching_libraries_and_hashes() {
        let expected = json!({"manifests":[{"manifest":"test","library":"private.dll","sha256":"good","dependency_hashes":{}}]});
        let actual = json!({"ok":true,"manifests":[{"manifest":"test","library":"private.dll","sha256":"good","dependency_hashes":{},"loaded":true}]});
        verify(&expected, &actual).unwrap();
        for field in ["library", "sha256", "loaded", "dependency_hashes"] {
            let mut bad = actual.clone();
            bad["manifests"][0][field] = json!("wrong");
            assert!(verify(&expected, &bad).is_err());
        }
    }
    #[test]
    fn cargo_artifact_identity_has_no_stale_filename_fallback() {
        assert!(artifact("", "package", "renamed_target", Path::new("missing")).is_err());
        let unrelated=json!({"reason":"compiler-artifact","package_id":"other","target":{"name":"renamed_target"},"filenames":["outside.dll"]}).to_string();
        assert!(artifact(
            &unrelated,
            "package",
            "renamed_target",
            Path::new("missing")
        )
        .is_err());
    }
}
