//! Build non-threaded Rust GDExtensions as Emscripten side modules.
//! SDK provisioning is explicit; this module never installs or upgrades tools.
use crate::{files, Project};
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub fn build(project: &Project) -> Result<Value, String> {
    let context = crate::operation_context::OperationContext::current();
    if let Some(context) = &context {
        context.check().map_err(|error| error.to_string())?;
        context.phase(crate::operation_context::Phase::Dependencies);
    }
    let package = project
        .config
        .rust_package
        .as_deref()
        .ok_or("This project has no Rust package configured")?;
    let sdk = sdk_directory()?;
    let settings = sdk
        .parent()
        .map(|p| p.join("toolchain.json"))
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let toolchain = project
        .config
        .web_toolchain
        .as_deref()
        .or_else(|| settings.as_ref().and_then(|v| v["toolchain"].as_str()))
        .unwrap_or("nightly");
    if toolchain.len() > 80
        || !toolchain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return Err("Invalid web.toolchain name".into());
    }
    let features = if project.config.web_features.is_empty() {
        vec![
            "godot/experimental-wasm".into(),
            "godot/experimental-wasm-nothreads".into(),
            "godot/lazy-function-tables".into(),
        ]
    } else {
        project.config.web_features.clone()
    };
    if features.len() > 32
        || features.iter().any(|s| {
            s.is_empty()
                || s.len() > 128
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'))
        })
    {
        return Err("Invalid web.features list".into());
    }
    let linker = sdk.join(if cfg!(windows) {
        "upstream/emscripten/emcc.bat"
    } else {
        "upstream/emscripten/emcc"
    });
    if !linker.is_file() || !sdk.join(".emscripten").is_file() {
        return Err(
            "AURUM_EMSDK must contain emcc and an activated .emscripten configuration".into(),
        );
    }
    let work = files::confined(&project.root, ".aurum/web-build")?;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let target = work.join("target");
    let mut compiler =
        crate::Command::new("rustc").args([format!("+{toolchain}"), "-Z".into(), "help".into()]);
    if let Some(root) = sdk_rustup(&sdk) {
        compiler = compiler.env("RUSTUP_HOME", root.display().to_string());
    }
    let help = compiler
        .run(Duration::from_secs(30))
        .map_err(|e| e.to_string())?;
    if !help.success() {
        return Err(format!(
            "The configured nightly Web compiler is unavailable: {}",
            help.failure_detail()
        ));
    }
    let mut compiler_flags = vec![
        "-C",
        "link-args=-sSIDE_MODULE=2",
        "-C",
        "panic=abort",
        "-C",
        "llvm-args=-enable-emscripten-cxx-exceptions=0",
        "-Z",
        "default-visibility=hidden",
        "-Z",
        "link-native-libraries=no",
    ];
    if help.stdout.contains("emscripten-wasm-eh") {
        compiler_flags.extend(["-Z", "emscripten-wasm-eh=false"]);
    } else {
        compiler_flags.extend(["-C", "target-feature=-exception-handling"]);
    }
    let flags = compiler_flags.join("\x1f");
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let paths =
        std::iter::once(sdk.join("upstream/emscripten")).chain(std::env::split_paths(&old_path));
    let path = std::env::join_paths(paths).map_err(|e| e.to_string())?;
    let mut command = crate::Command::new("cargo")
        .args([
            format!("+{toolchain}"),
            "build".into(),
            "--locked".into(),
            "--release".into(),
            "--target".into(),
            "wasm32-unknown-emscripten".into(),
            "-Zbuild-std=std,panic_abort".into(),
            "-p".into(),
            package.into(),
            "--features".into(),
            features.join(","),
            "--message-format=json".into(),
        ])
        .directory(project.confined_build_workspace()?)
        .env("CARGO_TARGET_DIR", target.display().to_string())
        .env("CARGO_ENCODED_RUSTFLAGS", flags)
        .env(
            "GDRUST_GODOT_BIN",
            crate::project_ops::engine_binary(project)?
                .display()
                .to_string(),
        )
        .env(
            "CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER",
            linker.display().to_string(),
        )
        .env("EM_CONFIG", sdk.join(".emscripten").display().to_string())
        .env("PATH", path.to_string_lossy());
    if context.is_some() {
        command = command.candidate_storage(&work);
    }
    if let Some(root) = sdk_rustup(&sdk) {
        command = command.env("RUSTUP_HOME", root.display().to_string());
    }
    if let Some(context) = &context {
        context.phase(crate::operation_context::Phase::Compiling);
    }
    let result = command
        .run(Duration::from_secs(600))
        .map_err(|e| e.to_string())?;
    files::write_atomic(&work.join("stdout.log"), result.stdout.as_bytes())
        .map_err(|e| e.to_string())?;
    files::write_atomic(&work.join("stderr.log"), result.stderr.as_bytes())
        .map_err(|e| e.to_string())?;
    if !result.success() {
        return Err(format!("Rust Web build failed (nightly Rust, rust-src and the Emscripten target are required): {}\nLogs: {}", result.tail(18).join("\n"), work.display()));
    }
    let mut artifacts = Vec::new();
    for line in result.stdout.lines() {
        if let Some(context) = &context {
            context.check().map_err(|error| error.to_string())?;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact"
            || !message["target"]["crate_types"]
                .as_array()
                .is_some_and(|types| types.iter().any(|v| v == "cdylib"))
        {
            continue;
        }
        for filename in message["filenames"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let artifact = PathBuf::from(filename);
            if artifact.extension().is_none_or(|ext| ext != "wasm") {
                continue;
            }
            let canonical =
                crate::project::clean_path(artifact.canonicalize().map_err(|e| e.to_string())?);
            let root =
                crate::project::clean_path(target.canonicalize().map_err(|e| e.to_string())?);
            if !canonical.starts_with(&root)
                || std::fs::symlink_metadata(&artifact)
                    .map_err(|e| e.to_string())?
                    .file_type()
                    .is_symlink()
            {
                return Err("Cargo reported an artifact outside the managed Web target".into());
            }
            let mut artifact =
                std::fs::File::open(&canonical).map_err(|error| error.to_string())?;
            let mut header = [0; 8];
            artifact
                .read_exact(&mut header)
                .map_err(|error| error.to_string())?;
            if &header != b"\0asm\x01\0\0\0" {
                return Err("Cargo did not produce a WebAssembly module".into());
            }
            let mut hash = crate::Sha256::new();
            hash.update(&header);
            let mut buffer = [0; 64 * 1024];
            loop {
                if let Some(context) = &context {
                    context.check().map_err(|error| error.to_string())?;
                }
                let count = artifact
                    .read(&mut buffer)
                    .map_err(|error| error.to_string())?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            artifacts.push(
                json!({"path":canonical,"name":message["target"]["name"],"sha256":hash.hex()}),
            );
        }
    }
    if artifacts.is_empty() {
        return Err("Rust Web build produced no cdylib Wasm artifact".into());
    }
    let value = json!({"ok":true,"target":"wasm32-unknown-emscripten","threaded":false,"toolchain":toolchain,"artifacts":artifacts,"logs":work,"published":false,"private_build":true});
    if let Some(context) = &context {
        context.check().map_err(|error| error.to_string())?;
    }
    files::write_atomic(
        &work.join("build.json"),
        &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(value)
}

pub fn sdk_directory() -> Result<PathBuf, String> {
    std::env::var_os("AURUM_EMSDK").map(PathBuf::from)
        .or_else(|| std::env::var_os("AURUM_STUDIO_HOME").map(PathBuf::from).map(|p| p.join("runtime/web-toolchain/emsdk")).filter(|p| p.is_dir()))
        .ok_or("Rust Web builds need the managed runtime/web-toolchain SDK or AURUM_EMSDK. Provision scripts/provision-web-toolchain.ps1 explicitly; no tools are silently installed.".into())
}
pub fn sdk_rustup(sdk: &std::path::Path) -> Option<PathBuf> {
    sdk.parent()
        .map(|p| p.join("rustup"))
        .filter(|p| p.is_dir())
}
