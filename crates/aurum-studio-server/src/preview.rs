//! A read-only, separate-origin web game preview. It never serves Studio APIs.
use crate::http::{self, Response};
use aurum_studio_core::{files, Project};
use serde_json::{json, Value};
use std::io::{BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct Preview {
    pub project: PathBuf,
    source_sha256: String,
    requires_isolation: bool,
    output: PathBuf,
    url: String,
    session: String,
    running: Arc<AtomicBool>,
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

struct Content {
    root: PathBuf,
    live_file: PathBuf,
    session: String,
    parent_origin: String,
    port: u16,
    running: Arc<AtomicBool>,
    connections: AtomicUsize,
}

impl Preview {
    /// Publish a fresh static bundle without Studio's live bridge or credentials.
    pub fn export_bundle(&self, relative: &str) -> Result<Value, String> {
        if !relative.replace('\\', "/").starts_with("dist/") {
            return Err("Web builds must use a new directory under dist/".into());
        }
        let output = files::confined(&self.project, relative)?;
        if output.exists() {
            return Err("Web build destination already exists; choose a new directory".into());
        }
        let stage = self
            .output
            .parent()
            .ok_or("Missing export parent")?
            .join("bundle");
        std::fs::create_dir(&stage).map_err(|e| e.to_string())?;
        let mut count = 0;
        for entry in std::fs::read_dir(&self.output).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("Unexpected directory in generated web export".into());
            }
            let target = stage.join(entry.file_name());
            if entry.file_name() == "aurum-preview.js" {
                continue;
            }
            if entry.file_name() == "index.html" {
                let html = std::fs::read_to_string(entry.path()).map_err(|e| e.to_string())?;
                files::write_atomic(
                    &target,
                    html.replace("<script src=\"aurum-preview.js\"></script>", "")
                        .as_bytes(),
                )
                .map_err(|e| e.to_string())?;
            } else {
                std::fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
            }
            count += 1;
        }
        let parent = output.parent().ok_or("Missing output parent")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        if output.exists() {
            return Err("Destination appeared during export; nothing was overwritten".into());
        }
        std::fs::rename(stage, &output).map_err(|e| e.to_string())?;
        Ok(
            json!({"ok":true,"directory":output,"files":count,"entry":"index.html","mode":"web","requires_cross_origin_isolation":self.requires_isolation,"message":if self.requires_isolation {"Serve over HTTP(S) with application/wasm, Cross-Origin-Opener-Policy: same-origin and Cross-Origin-Embedder-Policy: require-corp. No Studio process or credentials are needed."} else {"Serve this directory over HTTP(S). It needs no Studio process, credentials or cross-origin isolation headers."}}),
        )
    }

    pub fn describe(&self, reused: bool) -> Value {
        json!({"ok":true,"url":self.url,"session":self.session,"project":self.project,"source_sha256":self.source_sha256,"requires_cross_origin_isolation":self.requires_isolation,"reused":reused,"mode":"web","isolated_origin":true})
    }

    pub fn freshness(&self, project: &Project) -> Result<Value, String> {
        let current = source_revision(project)?;
        let mut result = self.describe(true);
        result["stale"] = json!(current != self.source_sha256);
        result["current_sha256"] = json!(current);
        Ok(result)
    }

    pub fn is_fresh(&self, project: &Project) -> Result<bool, String> {
        Ok(source_revision(project)? == self.source_sha256)
    }

    pub fn build(
        project: &Project,
        hint: Option<&Path>,
        parent_origin: &str,
    ) -> Result<Self, String> {
        let engine = match hint {
            Some(path) if path.is_file() => path.to_path_buf(),
            Some(_) => return Err("The configured runtime executable does not exist".into()),
            None => aurum_studio_core::project_ops::engine_binary(project)?,
        };
        let template = web_template(&engine)?;
        let extension_template = std::env::var_os("AURUM_WEB_EXTENSION_TEMPLATE")
            .map(PathBuf::from)
            .unwrap_or_else(|| template.with_file_name("web_dlink_nothreads_release.zip"));
        let godot = project
            .godot_project_dir()
            .ok_or("project.godot was not found")?;
        let source_sha256 = source_revision(project)?;
        let _lock =
            aurum_studio_core::BuildLock::try_acquire(&project.root).map_err(|e| e.to_string())?;
        let session = aurum_studio_core::random::session_id();
        let work = files::confined(&project.root, &format!(".aurum/web/{session}"))?;
        let source = work.join("source");
        let output = work.join("export");
        std::fs::create_dir_all(&source).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
        let web_build_receipt = if project.config.rust_package.is_some() {
            let mut built = aurum_studio_core::web_build::build(project)?;
            // The editor importer needs the host-side class registry even for Web-only play.
            // Install it into this snapshot's work directory, never over a running DLL.
            let package = project.config.rust_package.as_deref().unwrap();
            let name = aurum_studio_core::build::library_name(package);
            let host = work
                .join("host")
                .join(aurum_studio_core::Profile::Debug.installed_filename(&name));
            let request = aurum_studio_core::BuildRequest::new(
                &project.root,
                package,
                aurum_studio_core::Profile::Debug,
                &host,
                "cargo",
            );
            let report = aurum_studio_core::build::run(&request, false, Duration::from_secs(600))
                .map_err(|e| format!("Host registry build for Web import failed: {e}"))?;
            built["host_artifacts"] = json!([{"name":name,"path":report.installed.path,"sha256":report.installed.sha256}]);
            let receipt = work.join("rust-web.json");
            files::write_atomic(
                &receipt,
                &serde_json::to_vec(&built).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            receipt.display().to_string()
        } else {
            String::new()
        };
        aurum_studio_core::snapshot::copy_excluding(godot, &source, &["addons/aurum_editor"])?;
        if source_revision(project)? != source_sha256 {
            return Err(
                "Source changed while taking the preview snapshot; retry after saving".into(),
            );
        }
        if source.join("addons/aurum_live").exists() {
            return Err("addons/aurum_live is reserved for private preview snapshots".into());
        }
        files::write_atomic(
            &source.join("addons/aurum_live/runtime.gd"),
            include_bytes!("../../aurum-studio-core/src/runtime_bridge.gd"),
        )
        .map_err(|e| e.to_string())?;
        let setup = work.join("preview_setup.gd");
        files::write_atomic(&setup, include_bytes!("preview_setup.gd"))
            .map_err(|e| e.to_string())?;
        let setup_report = work.join("setup.json");
        // Native editor/export processes must not share mutable settings and
        // cache directories with source inspection or an open user editor.
        let native_state = work.join("native-state");
        let native_data = native_state.join("data");
        let native_config = native_state.join("config");
        let native_cache = native_state.join("cache");
        for directory in [&native_data, &native_config, &native_cache] {
            std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        }
        let run = |args: Vec<String>, label: &str| -> Result<(), String> {
            let started = std::time::Instant::now();
            eprintln!("AURUM_WEB_STAGE stage={label} status=start");
            let mut result = aurum_studio_core::Command::new(&engine)
                .args(args.clone())
                .directory(&work)
                .env("APPDATA", native_config.to_string_lossy())
                .env("LOCALAPPDATA", native_data.to_string_lossy())
                .env("XDG_DATA_HOME", native_data.to_string_lossy())
                .env("XDG_CONFIG_HOME", native_config.to_string_lossy())
                .env("XDG_CACHE_HOME", native_cache.to_string_lossy())
                .run(Duration::from_secs(180))
                .map_err(|e| e.to_string())?;
            let mut log = format!("{}\n{}", result.stdout, result.stderr);
            if label == "import"
                && aurum_studio_core::project_ops::import_shutdown_crash(result.code, &log)
            {
                files::write_atomic(&work.join("import-first-crash.log"), log.as_bytes())
                    .map_err(|e| e.to_string())?;
                result = aurum_studio_core::Command::new(&engine)
                    .args(args)
                    .directory(&work)
                    .env("APPDATA", native_config.to_string_lossy())
                    .env("LOCALAPPDATA", native_data.to_string_lossy())
                    .env("XDG_DATA_HOME", native_data.to_string_lossy())
                    .env("XDG_CONFIG_HOME", native_config.to_string_lossy())
                    .env("XDG_CACHE_HOME", native_cache.to_string_lossy())
                    .run(Duration::from_secs(180))
                    .map_err(|e| e.to_string())?;
                log = format!("{}\n{}", result.stdout, result.stderr);
            }
            files::write_atomic(&work.join(format!("{label}.log")), log.as_bytes())
                .map_err(|e| e.to_string())?;
            eprintln!(
                "AURUM_WEB_STAGE stage={label} status=finished elapsed_ms={} success={} wall_timeout={}",
                started.elapsed().as_millis(),
                result.success(),
                result.timed_out
            );
            if !result.success() || log.contains("ERROR:") {
                return Err(format!(
                    "Web preview {label} failed (exit {:?}, wall_timeout={}): {}\nEvidence: {}",
                    result.code,
                    result.timed_out,
                    result.tail(22).join("\n"),
                    work.display()
                ));
            }
            Ok(())
        };
        run(
            vec![
                "--headless".into(),
                "--path".into(),
                work.display().to_string(),
                "--script".into(),
                setup.display().to_string(),
                "--".into(),
                template.display().to_string(),
                setup_report.display().to_string(),
                "res://addons/aurum_live/runtime.gd".into(),
                extension_template.display().to_string(),
                web_build_receipt,
                project.config.web_preset.clone().unwrap_or_default(),
                source.display().to_string(),
            ],
            "setup",
        )?;
        let setup_result: Value =
            serde_json::from_slice(&std::fs::read(&setup_report).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let preset = setup_result["preset"]
            .as_str()
            .ok_or("Preview setup did not select a Web preset")?;
        run(
            vec![
                "--headless".into(),
                "--editor".into(),
                "--path".into(),
                source.display().to_string(),
                "--import".into(),
            ],
            "import",
        )?;
        run(
            vec![
                "--headless".into(),
                "--path".into(),
                source.display().to_string(),
                "--export-release".into(),
                preset.into(),
                output.join("index.html").display().to_string(),
            ],
            "export",
        )?;
        let index = output.join("index.html");
        let html = std::fs::read_to_string(&index).map_err(|e| e.to_string())?;
        if !html.contains("</head>") {
            return Err("The generated game shell has no head element".into());
        }
        files::write_atomic(
            &index,
            html.replacen(
                "</head>",
                "<script src=\"aurum-preview.js\"></script></head>",
                1,
            )
            .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        // The portable WebAssembly bundle redistributes the engine too.
        // Ask the same runtime for its notices rather than inventing an attribution list.
        let notices_path = serde_json::to_string(
            &output
                .join("third-party-licenses.json")
                .to_string_lossy()
                .replace('\\', "/"),
        )
        .map_err(|e| e.to_string())?;
        let license_path = serde_json::to_string(
            &output
                .join("LICENSE-Godot.txt")
                .to_string_lossy()
                .replace('\\', "/"),
        )
        .map_err(|e| e.to_string())?;
        let probe = work.join("runtime_notices.gd");
        let script = format!("extends SceneTree\nfunc _init():\n\tvar f = FileAccess.open({notices_path}, FileAccess.WRITE)\n\tif f == null:\n\t\tquit(1)\n\t\treturn\n\tf.store_string(JSON.stringify({{\"license\":Engine.get_license_text(),\"components\":Engine.get_copyright_info(),\"licenses\":Engine.get_license_info()}}, \"  \"))\n\tf.close()\n\tf = FileAccess.open({license_path}, FileAccess.WRITE)\n\tif f == null:\n\t\tquit(1)\n\t\treturn\n\tf.store_string(Engine.get_license_text())\n\tf.close()\n\tquit()\n");
        files::write_atomic(&probe, script.as_bytes()).map_err(|e| e.to_string())?;
        run(
            vec![
                "--headless".into(),
                "--path".into(),
                source.display().to_string(),
                "--script".into(),
                probe.display().to_string(),
            ],
            "licenses",
        )?;
        let mut preview = Self::serve(
            project.root.clone(),
            output,
            godot.join("tuning.json"),
            session,
            parent_origin.to_owned(),
        )?;
        preview.source_sha256 = source_sha256;
        preview.requires_isolation = setup_result["requires_isolation"] == true;
        Ok(preview)
    }

    fn serve(
        project: PathBuf,
        root: PathBuf,
        live_file: PathBuf,
        session: String,
        parent_origin: String,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let running = Arc::new(AtomicBool::new(true));
        let content = Arc::new(Content {
            root: root.clone(),
            live_file,
            session: session.clone(),
            parent_origin,
            port,
            running: running.clone(),
            connections: AtomicUsize::new(0),
        });
        std::thread::spawn(move || {
            while content.running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if content.connections.fetch_add(1, Ordering::SeqCst) >= 12 {
                            content.connections.fetch_sub(1, Ordering::SeqCst);
                            continue;
                        }
                        let state = content.clone();
                        std::thread::spawn(move || {
                            serve_connection(stream, &state);
                            state.connections.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(15))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            project,
            source_sha256: String::new(),
            requires_isolation: false,
            output: root,
            url: format!("http://127.0.0.1:{port}/{session}/index.html"),
            session,
            running,
        })
    }
}

fn web_template(engine: &Path) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("AURUM_WEB_TEMPLATE").map(PathBuf::from) {
        return path
            .is_file()
            .then_some(path)
            .ok_or("AURUM_WEB_TEMPLATE does not name a template file".into());
    }
    let mut roots = Vec::new();
    if let Some(parent) = engine.parent() {
        roots.push(parent.join("templates/4.7.stable"));
    }
    if let Some(home) = std::env::var_os("AURUM_STUDIO_HOME") {
        roots.push(PathBuf::from(home).join("runtime/templates/4.7.stable"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join("Godot/export_templates/4.7.stable"));
    }
    roots.into_iter().map(|root|root.join("web_nothreads_release.zip")).find(|p|p.is_file())
        .ok_or("Web templates are missing. Provision the matching Godot 4.7 web templates or set AURUM_WEB_TEMPLATE to web_nothreads_release.zip. Run native remains available.".into())
}

#[cfg(test)]
fn copy_project(
    source: &Path,
    destination: &Path,
    depth: usize,
    count: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    if depth > 24 {
        return Err("Project nesting exceeds the preview limit".into());
    }
    for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text.starts_with('.')
            || matches!(text.as_ref(), "dist" | "target" | "node_modules" | "logs")
        {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!("Preview snapshots do not follow symlinks: {text}"));
        }
        let target = destination.join(&name);
        if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            copy_project(&entry.path(), &target, depth + 1, count, bytes)?;
        } else if kind.is_file() {
            *count += 1;
            *bytes += entry.metadata().map_err(|e| e.to_string())?.len();
            if *count > 20000 || *bytes > 4 * 1024 * 1024 * 1024 {
                return Err("Project exceeds the bounded preview snapshot limit".into());
            }
            std::fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn source_revision(project: &Project) -> Result<String, String> {
    let root = if project.config.rust_package.is_some() {
        project.root.as_path()
    } else {
        project
            .godot_project_dir()
            .ok_or("project.godot was not found")?
    };
    aurum_studio_core::snapshot::fingerprint(root)
}

fn serve_connection(mut stream: TcpStream, content: &Content) {
    // Windows accepted sockets can inherit the listener's nonblocking mode.
    // Asset writes must wait for backpressure instead of truncating Wasm/JS.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    serve_response(&mut stream, content);
    // Explicitly finish the HTTP response before the last socket handle closes.
    let _ = stream.shutdown(Shutdown::Write);
}

fn serve_response(stream: &mut TcpStream, content: &Content) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let Ok(Some(request)) = http::read_request(&mut reader) else {
        return;
    };
    if !matches!(request.method.as_str(), "GET" | "HEAD") {
        let _ = Response::error(405, "The preview is read-only").write_to(stream);
        return;
    }
    if request.header("host") != Some(format!("127.0.0.1:{}", content.port).as_str()) {
        let _ = Response::error(403, "Invalid preview host").write_to(stream);
        return;
    }
    let prefix = format!("/{}/", content.session);
    let Some(relative) = request.path.strip_prefix(&prefix) else {
        let _ = Response::error(404, "Not found").write_to(stream);
        return;
    };
    let response = if relative == "live.json" {
        Some(live_response(content))
    } else if relative == "aurum-preview.js" {
        Some(Response::new(
            200,
            "text/javascript; charset=utf-8",
            bridge(content),
        ))
    } else {
        None
    };
    if let Some(response) = response {
        let _ = response
            .with_header("Cache-Control", "no-store")
            .with_header("X-Content-Type-Options", "nosniff")
            .write_to(stream);
        return;
    }
    let Ok(path) = files::confined(&content.root, relative) else {
        let _ = Response::error(403, "Invalid asset path").write_to(stream);
        return;
    };
    let Some(mime) = mime(&path) else {
        let _ = Response::error(404, "Not found").write_to(stream);
        return;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        let _ = Response::error(404, "Not found").write_to(stream);
        return;
    };
    if !metadata.is_file() || metadata.len() > 256 * 1024 * 1024 {
        let _ = Response::error(413, "Asset exceeds preview limit").write_to(stream);
        return;
    }
    let Ok(mut file) = std::fs::File::open(&path) else {
        return;
    };
    let head=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Embedder-Policy: require-corp\r\nCross-Origin-Resource-Policy: cross-origin\r\nContent-Security-Policy: frame-ancestors {}\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",metadata.len(),content.parent_origin);
    if stream.write_all(head.as_bytes()).is_ok() && request.method == "GET" {
        let _ = std::io::copy(&mut file, stream);
    }
}

fn mime(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "pck" => "application/octet-stream",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "json" | "webmanifest" => "application/json",
        "css" => "text/css; charset=utf-8",
        _ => return None,
    })
}

fn live_response(content: &Content) -> Response {
    let value = (|| {
        let parent = content.live_file.parent()?;
        let path = files::confined(parent, content.live_file.file_name()?.to_str()?).ok()?;
        if std::fs::metadata(&path).ok()?.len() > 64 * 1024 {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        let data: Value = serde_json::from_slice(&bytes).ok()?;
        if !data.is_object() {
            return None;
        }
        Some(json!({"ok":true,"sha256":aurum_studio_core::sha256_hex(&bytes),"values":data}))
    })()
    .unwrap_or(json!({"ok":false,"message":"No valid live tuning file"}));
    Response::json(200, &value)
}

fn bridge(content: &Content) -> String {
    let config =
        serde_json::to_string(&json!({"session":content.session,"parent":content.parent_origin}))
            .unwrap();
    format!(
        "'use strict';\nconst AURUM_PREVIEW={config};\n{}",
        include_str!("../ui/preview-bridge.js")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn web_bundle_is_standalone_and_refuses_existing_or_outside_destinations() {
        let root = std::env::temp_dir().join(format!(
            "aurum-web-bundle-{}",
            aurum_studio_core::random::session_id()
        ));
        let export = root.join("snapshot/export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(
            export.join("index.html"),
            "<head><script src=\"aurum-preview.js\"></script></head>",
        )
        .unwrap();
        std::fs::write(export.join("index.wasm"), b"wasm").unwrap();
        std::fs::write(export.join("index.pck"), b"game").unwrap();
        let preview = Preview::serve(
            root.clone(),
            export,
            root.join("tuning.json"),
            "test".into(),
            "http://127.0.0.1:11111".into(),
        )
        .unwrap();
        assert!(preview.export_bundle("../escaped").is_err());
        assert!(preview.export_bundle("dist/../../escaped").is_err());
        assert!(preview.export_bundle("godot/new").is_err());
        let result = preview.export_bundle("dist/web").unwrap();
        assert_eq!(result["files"], 3);
        assert_eq!(
            std::fs::read_to_string(root.join("dist/web/index.html")).unwrap(),
            "<head></head>"
        );
        assert!(!root.join("dist/web/aurum-preview.js").exists());
        assert!(preview.export_bundle("dist/web").is_err());
        drop(preview);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn request(preview: &Preview, method: &str, path: &str, host: Option<&str>) -> String {
        let address = preview
            .url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n\r\n",
            host.unwrap_or(address)
        )
        .unwrap();
        let mut output = String::new();
        stream.read_to_string(&mut output).unwrap();
        output
    }
    #[test]
    fn preview_origin_is_read_only_confined_and_observes_live_data() {
        let root = std::env::temp_dir().join(format!(
            "aurum-preview-http-{}",
            aurum_studio_core::random::session_id()
        ));
        let export = root.join("export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("game.wasm"), b"wasm fixture").unwrap();
        std::fs::write(root.join("secret.json"), b"must not escape").unwrap();
        let live = root.join("tuning.json");
        std::fs::write(&live, b"{\"player_speed\":7.6}").unwrap();
        let preview = Preview::serve(
            root.clone(),
            export,
            live.clone(),
            "fixture".into(),
            "http://127.0.0.1:1234".into(),
        )
        .unwrap();
        let response = request(&preview, "GET", "/fixture/game.wasm", None);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("Content-Type: application/wasm"));
        assert!(!response.contains("Set-Cookie"));
        assert!(request(&preview, "POST", "/fixture/game.wasm", None).starts_with("HTTP/1.1 405"));
        assert!(
            request(&preview, "GET", "/fixture/game.wasm", Some("evil.example"))
                .starts_with("HTTP/1.1 403")
        );
        assert!(
            request(&preview, "GET", "/fixture/../secret.json", None).starts_with("HTTP/1.1 403")
        );
        assert!(request(&preview, "GET", "/api/project", None).starts_with("HTTP/1.1 404"));
        assert!(request(&preview, "GET", "/fixture/live.json", None).contains("7.6"));
        std::fs::write(live, b"{\"player_speed\":9.0}").unwrap();
        assert!(request(&preview, "GET", "/fixture/live.json", None).contains("9.0"));
        let head = request(&preview, "HEAD", "/fixture/game.wasm", None);
        assert!(head.contains("Content-Length: 12"));
        assert!(!head.ends_with("wasm fixture"));
        let large = vec![b'w'; 2 * 1024 * 1024];
        std::fs::write(root.join("export/game.wasm"), &large).unwrap();
        let response = request(&preview, "GET", "/fixture/game.wasm", None);
        assert_eq!(
            response.split_once("\r\n\r\n").unwrap().1.len(),
            large.len()
        );
        drop(preview);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn preview_assets_have_correct_types_and_no_source_code_route() {
        assert_eq!(mime(Path::new("game.wasm")), Some("application/wasm"));
        assert_eq!(mime(Path::new("main.gd")), None);
        assert_eq!(mime(Path::new(".env")), None);
    }
    #[test]
    fn snapshot_does_not_copy_generated_state() {
        let root = std::env::temp_dir().join(format!(
            "aurum-preview-copy-{}",
            aurum_studio_core::random::session_id()
        ));
        let source = root.join("source");
        let output = root.join("output");
        std::fs::create_dir_all(source.join(".godot")).unwrap();
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(source.join("main.gd"), b"extends Node").unwrap();
        std::fs::write(source.join(".godot/cache"), b"ignored").unwrap();
        copy_project(&source, &output, 0, &mut 0, &mut 0).unwrap();
        assert!(output.join("main.gd").is_file());
        assert!(!output.join(".godot").exists());
        std::fs::write(source.join("native.gdextension"), b"native").unwrap();
        assert!(copy_project(&source, &output, 0, &mut 0, &mut 0).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
