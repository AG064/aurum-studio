//! `aurum` subcommands, implemented over `aurum-studio-core`.
//!
//! The CLI is the automation surface for the whole pipeline: tests, scripts,
//! and AI agents drive Studio through here. That is why every command is
//! deterministic, every report has a machine-readable form, and exit codes
//! distinguish the states a caller has to branch on.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aurum_studio_core::build::{library_name, BuildRequest, Profile};
use aurum_studio_core::build_queue::BuildLock;
use aurum_studio_core::doctor::{diagnose, Health};
use aurum_studio_core::gameplay::{respond, Game, Response};
use aurum_studio_core::ownership::{OwnershipRecord, ProcessKind};
use aurum_studio_core::project::clean_path;
use aurum_studio_core::registry::Registry;
use aurum_studio_core::reload::{Classification, Verdict};
use aurum_studio_core::session::Session;
use aurum_studio_core::supervise::{
    bridge_environment, launch, stop_session, terminate, LaunchRequest, StopOutcome,
};
use aurum_studio_core::toolchain::{discover, discover_godot_to_launch};
use aurum_studio_core::watch::{Debouncer, Watcher};
use aurum_studio_core::Project;
use aurum_studio_server::{Server, ServerConfig};

/// Exit codes callers branch on. Distinguishing "unhealthy" from "broken
/// invocation" is what lets a script act on the result rather than parse text.
pub mod exit {
    pub const OK: u8 = 0;
    pub const WARNING: u8 = 1;
    pub const BLOCKED: u8 = 2;
    pub const USAGE: u8 = 64;
    pub const FAILED: u8 = 70;
}

/// Parsed common arguments.
#[derive(Debug, Default)]
struct Options {
    project: Option<PathBuf>,
    godot: Option<PathBuf>,
    json: bool,
    release: bool,
    force: bool,
    once: bool,
    no_editor: bool,
    play: bool,
    agent: Option<String>,
    interval_ms: u64,
    port: u16,
    no_open: bool,
    positional: Vec<String>,
}

/// Parse the flag shape every command shares.
fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        interval_ms: 250,
        ..Options::default()
    };
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        match argument {
            "--json" => options.json = true,
            "--release" => options.release = true,
            "--force" => options.force = true,
            "--once" => options.once = true,
            "--no-editor" => options.no_editor = true,
            "--play" => options.play = true,
            "--agent" => {
                index += 1;
                options.agent = Some(
                    args.get(index)
                        .ok_or_else(|| "--agent requires a client name".to_string())?
                        .clone(),
                );
            }
            "--no-open" => options.no_open = true,
            "--port" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--port requires a number".to_string())?;
                options.port = value
                    .parse()
                    .map_err(|_| format!("'{value}' is not a port number"))?;
            }
            "--interval" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--interval requires milliseconds".to_string())?;
                options.interval_ms = value
                    .parse()
                    .map_err(|_| format!("'{value}' is not a number of milliseconds"))?;
            }
            "--godot" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--godot requires a path".to_string())?;
                options.godot = Some(PathBuf::from(value));
            }
            "--project" | "-p" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--project requires a path".to_string())?;
                options.project = Some(PathBuf::from(value));
            }
            "-h" | "--help" => return Err("help".into()),
            other => {
                if let Some(value) = other.strip_prefix("--godot=") {
                    options.godot = Some(PathBuf::from(value));
                } else if let Some(value) = other.strip_prefix("--project=") {
                    options.project = Some(PathBuf::from(value));
                } else if other.starts_with('-') {
                    return Err(format!("unknown option '{other}'"));
                } else {
                    options.positional.push(other.to_string());
                }
            }
        }
        index += 1;
    }
    Ok(options)
}

/// Where the project is: the flag, else the first positional, else the cwd.
fn target(options: &Options) -> PathBuf {
    options
        .project
        .clone()
        .or_else(|| options.positional.first().map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Open a URL in the desktop's default browser.
///
/// Best effort by design: the address has already been printed, so a machine
/// with no browser — a build server, a container — loses nothing but the
/// convenience, and it is told why.
fn open_browser(url: &str) {
    #[cfg(windows)]
    // The empty argument is the window title: `start` treats a first quoted
    // argument as the title, so without it the URL would be swallowed.
    let opened = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();

    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(url).spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let opened = std::process::Command::new("xdg-open").arg(url).spawn();

    if let Err(error) = opened {
        eprintln!("aurum: could not open a browser ({error}); open the address above");
    }
}

/// `aurum studio [project]`
///
/// Starts the local shell and, unless asked not to, opens it.
///
/// The server binds the loopback interface and nothing else, and every request
/// must carry the session token, so starting Studio does not open a port to
/// the network or to another page in the browser.
pub fn studio(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("studio", &message),
    };

    let path = target(&options);
    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("aurum: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    let mut config = ServerConfig::new(project);
    config.port = options.port;
    config.godot_hint = options.godot.clone();

    let server = match Server::bind(config) {
        Ok(server) => server,
        Err(error) => {
            eprintln!(
                "aurum: could not listen on 127.0.0.1:{}: {error}",
                options.port
            );
            return ExitCode::from(exit::FAILED);
        }
    };

    let url = server.url();

    if options.json {
        // A machine is driving, so the address goes to stdout as data and no
        // browser is opened.
        println!(
            "{}",
            serde_json::json!({
                "url": url,
                "port": server.port(),
                "host": "127.0.0.1",
            })
        );
    } else {
        println!("aurum studio is listening on 127.0.0.1:{}", server.port());
        println!("  open: {url}");
        println!("  this address carries a session token; treat it like a password");
        println!("  press Ctrl+C, or use the button on the page, to stop");
        if !options.no_open {
            open_browser(&url);
        }
    }

    server.serve();
    ExitCode::from(exit::OK)
}

/// `aurum doctor [project]`
pub fn doctor(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("doctor", &message),
    };
    let path = target(&options);

    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            let message = error.to_string();
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "project": path.display().to_string(),
                        "health": "blocked",
                        "error": message,
                    })
                );
            } else {
                eprintln!("aurum doctor: {message}");
            }
            // A project that cannot even be opened is blocked, not a crash.
            return ExitCode::from(exit::BLOCKED);
        }
    };

    let toolchain = discover(&project, options.godot.as_deref());
    let report = diagnose(&project, &toolchain, options.godot.as_deref());

    if options.json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.render());
    }

    match report.health() {
        Health::Healthy => ExitCode::from(exit::OK),
        Health::Warning => ExitCode::from(exit::WARNING),
        Health::Blocked => ExitCode::from(exit::BLOCKED),
    }
}

/// `aurum build [project]`
///
/// Builds the extension and installs it. The destination is only touched once
/// a verified artifact is staged, so a failure here cannot damage the library
/// Godot is using.
pub fn build(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("build", &message),
    };
    let path = target(&options);

    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("aurum build: {error}");
            return ExitCode::from(exit::BLOCKED);
        }
    };

    // A build needs a package to build and an add-on to install into.
    let Some(package) = project.config.rust_package.clone() else {
        eprintln!("aurum build: aurum.toml has no 'rust_package'; there is nothing to build");
        return ExitCode::from(exit::USAGE);
    };
    let Some(addon) = project.layout.addon_directory.clone() else {
        eprintln!(
            "aurum build: no Aurum add-on directory found; set addon_destination in aurum.toml"
        );
        return ExitCode::from(exit::BLOCKED);
    };

    let toolchain = discover(&project, options.godot.as_deref());
    let Some(cargo) = toolchain.cargo.as_ref().map(|tool| tool.path.clone()) else {
        eprintln!("aurum build: Cargo was not found on PATH");
        return ExitCode::from(exit::BLOCKED);
    };

    let profile = if options.release {
        Profile::Release
    } else {
        Profile::Debug
    };
    let library = library_name(&package);
    let destination = addon.join("bin").join(profile.installed_filename(&library));

    let request = BuildRequest::new(&project.root, &package, profile, &destination, cargo);

    // One build writes a project's artifact at a time. The supervisor takes
    // the same lock, so a bare `aurum build` and a running Studio contend
    // rather than race to replace the same library.
    let _lock = match BuildLock::try_acquire(&project.root) {
        Ok(lock) => lock,
        Err(error) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({"built": false, "error": error.to_string()})
                );
            } else {
                eprintln!("aurum: {error}");
            }
            return ExitCode::from(exit::FAILED);
        }
    };

    if !options.json {
        println!(
            "building {} ({}) -> {}",
            package,
            if profile.is_debug() {
                "debug"
            } else {
                "release"
            },
            destination.display()
        );
    }

    match aurum_studio_core::build::run(&request, options.force, BUILD_TIMEOUT) {
        Ok(report) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "package": package,
                        "profile": if profile.is_debug() { "debug" } else { "release" },
                        "built": report.built,
                        "replaced": report.replaced,
                        "source": report.source.path.display().to_string(),
                        "source_sha256": report.source.sha256,
                        "installed": report.installed.path.display().to_string(),
                        "installed_sha256": report.installed.sha256,
                        "bytes": report.installed.bytes,
                    })
                );
            } else {
                println!("{}", report.summary());
                println!("  sha256: {}", report.installed.sha256);
                // Compiler warnings are worth surfacing on a successful build.
                for line in report.output.lines().filter(|l| l.starts_with("warning")) {
                    println!("  {line}");
                }
            }
            ExitCode::from(exit::OK)
        }
        Err(error) => {
            // The previously installed library is still in place; say so,
            // because that is what decides whether the user can keep working.
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "package": package,
                        "ok": false,
                        "error": error.to_string(),
                        "installed_unchanged": destination.is_file(),
                    })
                );
            } else {
                eprintln!("aurum build: {error}");
                if destination.is_file() {
                    eprintln!(
                        "the previously installed library is unchanged at {}",
                        destination.display()
                    );
                }
            }
            ExitCode::from(exit::FAILED)
        }
    }
}

/// How long a build may take before it is killed.
///
/// Generous: a cold dependency build is slow, and killing it would be worse
/// than waiting.
const BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Open a project, its toolchain, and its Godot: the shared prologue.
fn open_for_launch(options: &Options) -> Result<(Project, PathBuf, PathBuf), (String, ExitCode)> {
    let path = target(options);
    let project =
        Project::open(&path).map_err(|e| (e.to_string(), ExitCode::from(exit::BLOCKED)))?;

    let Some(godot_project) = project.godot_project_dir().map(Path::to_path_buf) else {
        return Err((
            "no project.godot was found; there is nothing for Godot to open".into(),
            ExitCode::from(exit::BLOCKED),
        ));
    };

    // An explicit path is an assertion. Silently discovering a different Godot
    // after the user named one would launch an engine they did not ask for.
    if let Some(hint) = &options.godot {
        if !hint.exists() {
            return Err((
                format!("the Godot you named does not exist: {}", hint.display()),
                ExitCode::from(exit::BLOCKED),
            ));
        }
    }

    // The windowed build, not the console one: only a window can be asked to
    // close politely, and a console process has to be terminated forcefully,
    // which discards unsaved work.
    let Some(godot) = discover_godot_to_launch(&project, options.godot.as_deref()) else {
        return Err((
            match &options.godot {
                Some(hint) => format!(
                    "no Godot executable was found at or under '{}'",
                    hint.display()
                ),
                None => "Godot was not found; pass --godot <path> or put it on PATH".to_string(),
            },
            ExitCode::from(exit::BLOCKED),
        ));
    };

    Ok((project, godot_project, godot))
}

/// Launch Godot through a supervised session.
fn launch_godot(options: &Options, game: bool) -> ExitCode {
    let (project, godot_project, godot) = match open_for_launch(options) {
        Ok(parts) => parts,
        Err((message, code)) => {
            eprintln!("aurum: {message}");
            return code;
        }
    };

    let session = match Session::create(&project.root) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("aurum: could not start a session: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    let mut request = LaunchRequest::godot(&godot, &godot_project, &project.root);
    request.kind = if game {
        ProcessKind::Game
    } else {
        ProcessKind::Editor
    };
    request.environment = bridge_environment(&session, None);
    if game {
        // Godot runs the main scene when the editor flag is absent.
    }

    match launch(&request, &session) {
        Ok(launched) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "session": session.id,
                        "pid": launched.pid(),
                        "kind": launched.record.kind.label(),
                        "executable": launched.record.executable.display().to_string(),
                        "project": project.root.display().to_string(),
                        "log": session.log_path().display().to_string(),
                    })
                );
            } else {
                println!(
                    "launched {} (pid {}) for {}",
                    launched.record.kind.label(),
                    launched.pid(),
                    project.config.name
                );
                println!("  log: {}", session.log_path().display());
                println!("  stop with: aurum stop {}", project.config.name);
            }
            ExitCode::from(exit::OK)
        }
        Err(error) => {
            eprintln!("aurum: {error}");
            ExitCode::from(exit::FAILED)
        }
    }
}

/// `aurum editor [project]`
pub fn editor(args: &[String]) -> ExitCode {
    match parse(args) {
        Ok(options) => launch_godot(&options, false),
        Err(message) => usage("editor", &message),
    }
}

/// `aurum run [project]`
pub fn run(args: &[String]) -> ExitCode {
    match parse(args) {
        Ok(options) => launch_godot(&options, true),
        Err(message) => usage("run", &message),
    }
}

/// Whether a restart may go ahead, given what happened when the editor was
/// asked to close.
///
/// Separated from the command so the rule can be tested without an editor. The
/// whole value of the rule is that the refusal comes *before* the launch, and
/// that is a property of this decision rather than of the code around it.
///
/// `NotRunning` counts as closed: the record described a process that had
/// already exited on its own, so nothing is in the way.
fn may_start_again(outcomes: &[StopOutcome]) -> bool {
    outcomes
        .iter()
        .all(|outcome| matches!(outcome, StopOutcome::Stopped | StopOutcome::NotRunning))
}

/// `aurum restart [project]`
///
/// Restart the editor deliberately, for the one case the classifier names: a
/// change to the native surface — a new `#[func]`, property, signal, or entry
/// symbol — that cannot be migrated into an editor that is already running.
/// Everything else reloads, which is why this is exceptional rather than
/// routine.
///
/// "Controlled" is doing real work in that phrase. The editor is asked to close
/// politely, and if it declines — which is exactly what a window with unsaved
/// work does — nothing is forced and nothing is started on top of it. A restart
/// that discarded somebody's open scene to save them a keypress would be a bug
/// wearing a feature's clothes.
pub fn restart(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("restart", &message),
    };

    let (project, godot_project, godot) = match open_for_launch(&options) {
        Ok(parts) => parts,
        Err((message, code)) => {
            eprintln!("aurum: {message}");
            return code;
        }
    };

    let Some(session) = Session::latest_for(&project.root) else {
        eprintln!(
            "aurum restart: no session was recorded for {}; start the editor with \
             `aurum editor` first",
            project.root.display()
        );
        return ExitCode::from(exit::WARNING);
    };

    // Only editors. A game is not part of this decision, and stopping one here
    // would make a restart of the editor also interrupt play, which is a
    // surprise rather than a feature.
    let editors: Vec<OwnershipRecord> = OwnershipRecord::read_all(&session.ownership_directory())
        .into_iter()
        .filter(|record| record.kind == ProcessKind::Editor)
        .collect();

    if editors.is_empty() {
        eprintln!(
            "aurum restart: no editor is running for session {}; there is nothing to restart",
            session.id
        );
        return ExitCode::from(exit::WARNING);
    }

    let mut outcomes = Vec::new();
    let mut stopped = Vec::new();
    for record in &editors {
        let outcome = terminate(record, options.force, STOP_TIMEOUT);
        if matches!(outcome, StopOutcome::Stopped | StopOutcome::NotRunning) {
            let _ = record.remove(&session.ownership_directory());
            stopped.push(record.pid);
            if !options.json {
                println!("  stopped the editor (pid {})", record.pid);
            }
        } else if !options.json {
            println!("  {}", outcome.describe(ProcessKind::Editor));
        }
        outcomes.push(outcome);
    }

    if !may_start_again(&outcomes) {
        // The important part. Starting a second editor over one that would not
        // close is how a project ends up with two of them holding the same
        // files, so nothing is started until the old one is known to be gone.
        eprintln!(
            "aurum restart: the editor did not close, so nothing was started over it; \
             save your work and try again, or pass --force to discard it"
        );
        return ExitCode::from(exit::FAILED);
    }

    // Started again in the same session, so ownership records keep landing in
    // one place and a later `aurum stop` still finds everything.
    let mut request = LaunchRequest::godot(&godot, &godot_project, &project.root);
    request.kind = ProcessKind::Editor;
    request.environment = bridge_environment(&session, None);

    match launch(&request, &session) {
        Ok(launched) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "restarted": true,
                        "session": session.id,
                        "stopped": stopped,
                        "pid": launched.pid(),
                        "kind": launched.record.kind.label(),
                        "project": project.root.display().to_string(),
                        "log": session.log_path().display().to_string(),
                    })
                );
            } else {
                println!(
                    "restarted the editor (pid {} -> {})",
                    stopped
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                    launched.pid()
                );
                println!("  log: {}", session.log_path().display());
            }
            ExitCode::from(exit::OK)
        }
        Err(error) => {
            eprintln!("aurum restart: the editor was stopped but could not be started again");
            eprintln!("  {error}");
            ExitCode::from(exit::FAILED)
        }
    }
}

/// Where the running binary's own checkout is, when it can be known.
///
/// Only claimed for a binary sitting in a Cargo target directory, because that
/// is the one layout where walking up three levels means anything. An installed
/// copy lives somewhere that says nothing about where the engine is: the first
/// version walked up from `A:\AurumStudio\bin\aurum.exe` and confidently
/// reported `A:\`, which was written into a new project's configuration as
/// though it were a fact.
///
/// `None` is the honest answer there, and the caller asks for `--engine`
/// instead. A guess and a fact must not look the same in a file somebody will
/// later trust.
fn engine_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let profile_directory = executable.parent()?;
    let target_directory = profile_directory.parent()?;

    if target_directory.file_name()? != "target" {
        return None;
    }
    target_directory.parent().map(Path::to_path_buf)
}

/// `aurum new <path> [--name <name>] [--template <name>] [--engine <path>]`
///
/// Make a project. Refuses to write into a directory that already has anything
/// in it, because this is run once, at the start, usually against a path
/// somebody typed from memory.
pub fn new(args: &[String]) -> ExitCode {
    let mut path: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut template = aurum_studio_core::templates::DEFAULT_TEMPLATE.to_string();
    let mut engine = engine_root();
    let mut json = false;

    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        let mut value = |what: &str| -> Option<String> {
            index += 1;
            match args.get(index) {
                Some(value) => Some(value.clone()),
                None => {
                    eprintln!("aurum new: {argument} requires {what}");
                    None
                }
            }
        };
        match argument {
            "--json" => json = true,
            "--name" => match value("a name") {
                Some(v) => name = Some(v),
                None => return ExitCode::from(exit::USAGE),
            },
            "--template" => match value("a template name") {
                Some(v) => template = v,
                None => return ExitCode::from(exit::USAGE),
            },
            "--engine" => match value("a path") {
                Some(v) => engine = Some(PathBuf::from(v)),
                None => return ExitCode::from(exit::USAGE),
            },
            "-h" | "--help" => return usage("new", "help"),
            other if other.starts_with('-') => {
                eprintln!("aurum new: unknown option '{other}'");
                return ExitCode::from(exit::USAGE);
            }
            other => path = Some(PathBuf::from(other)),
        }
        index += 1;
    }

    let Some(root) = path else {
        eprintln!("aurum new: which directory? e.g. `aurum new ./my-game`");
        return ExitCode::from(exit::USAGE);
    };

    // The directory's own name is the obvious default, and asking for it again
    // would be a question with only one sensible answer.
    let name = name.unwrap_or_else(|| {
        root.file_name()
            .map(|part| part.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    let Some(engine) = engine else {
        eprintln!("aurum new: could not work out where the engine is; pass --engine <path>");
        return ExitCode::from(exit::FAILED);
    };

    let request = aurum_studio_core::templates::NewProject {
        name,
        template,
        engine,
    };

    match aurum_studio_core::templates::create(&root, &request) {
        Ok(created) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "root": created.root.display().to_string(),
                        "name": created.name,
                        "template": created.template,
                        "files": created
                            .files
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>(),
                    })
                );
            } else {
                println!("{}", created.describe());
                for file in &created.files {
                    println!("  {}", file.display());
                }
                println!("\nnext:");
                println!("  cd {}", created.root.display());
                println!("  aurum doctor");
            }
            ExitCode::from(exit::OK)
        }
        Err(error) => {
            eprintln!("aurum new: {error}");
            ExitCode::from(exit::FAILED)
        }
    }
}

/// `aurum modules [project]`
///
/// What the engine ships, what this project turned on, and anything named that
/// does not exist. The last of those is the reason it exists: a typo in
/// `aurum.toml` used to be accepted in silence and behave as though the module
/// were on.
pub fn modules(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("modules", &message),
    };

    let path = target(&options);
    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("aurum modules: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    let engine = project
        .config
        .engine_path_hint
        .as_deref()
        .map(PathBuf::from);
    let report = aurum_studio_core::modules::report(&project.config.modules, engine.as_deref());

    if options.json {
        let entries: Vec<serde_json::Value> = report
            .entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "description": entry.description,
                    "state": entry.state.label(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "project": project.config.name,
                "checkout_read": report.checkout_read,
                "enabled": report.with_state(aurum_studio_core::modules::State::Enabled).len(),
                "unknown": report.unknown(),
                "modules": entries,
            })
        );
    } else {
        // Enabled first, then anything wrong, then the rest: what is on is the
        // answer to the question that was asked, and a problem outranks a
        // suggestion.
        let order = [
            aurum_studio_core::modules::State::Enabled,
            aurum_studio_core::modules::State::Missing,
            aurum_studio_core::modules::State::Unknown,
            aurum_studio_core::modules::State::Available,
        ];
        println!("{} modules for {}", project.config.name, path.display());
        for state in order {
            for entry in report.with_state(state) {
                let marker = match state {
                    aurum_studio_core::modules::State::Enabled => "on ",
                    aurum_studio_core::modules::State::Missing => "!! ",
                    aurum_studio_core::modules::State::Unknown => "?? ",
                    aurum_studio_core::modules::State::Available => "   ",
                };
                let hint = match report.suggestion(&entry.name) {
                    Some(near) if state == aurum_studio_core::modules::State::Unknown => {
                        format!(" (did you mean '{near}'?)")
                    }
                    _ => String::new(),
                };
                println!("  {marker}{:<12} {}{hint}", entry.name, entry.description);
            }
        }
        if !report.checkout_read {
            println!("  (the engine checkout was not readable, so nothing is reported missing)");
        }
    }

    // Unknown names are the one outcome a script should branch on.
    if report.unknown().is_empty() {
        ExitCode::from(exit::OK)
    } else {
        ExitCode::from(exit::WARNING)
    }
}

/// `aurum godot [project] [--fetch <url> --sha256 <digest>]`
///
/// With no arguments, what Godot builds this machine already has, which is the
/// case that should keep working first: most people have one, and telling them
/// to fetch a second copy would be rude.
///
/// With `--fetch`, a verified download. **The digest is required and is not
/// defaulted.** A built-in table of hashes would be this command asserting a
/// fact about a file it has never seen, and a table that is wrong is worse than
/// no table: it turns a check into a formality. The caller supplies what they
/// expect, and the bytes are held to it.
pub fn godot(args: &[String]) -> ExitCode {
    let mut url: Option<String> = None;
    let mut digest: Option<String> = None;
    let mut into: Option<PathBuf> = None;
    let mut json = false;
    let mut path: Option<PathBuf> = None;

    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        let mut take = |what: &str| -> Option<String> {
            index += 1;
            match args.get(index) {
                Some(value) => Some(value.clone()),
                None => {
                    eprintln!("aurum godot: {argument} requires {what}");
                    None
                }
            }
        };
        match argument {
            "--json" => json = true,
            "--fetch" => match take("a url") {
                Some(v) => url = Some(v),
                None => return ExitCode::from(exit::USAGE),
            },
            "--sha256" => match take("a digest") {
                Some(v) => digest = Some(v),
                None => return ExitCode::from(exit::USAGE),
            },
            "--into" => match take("a directory") {
                Some(v) => into = Some(PathBuf::from(v)),
                None => return ExitCode::from(exit::USAGE),
            },
            "-h" | "--help" => return usage("godot", "help"),
            other if other.starts_with('-') => {
                eprintln!("aurum godot: unknown option '{other}'");
                return ExitCode::from(exit::USAGE);
            }
            other => path = Some(PathBuf::from(other)),
        }
        index += 1;
    }

    let (Some(url), Some(digest)) = (url, digest) else {
        return list_godot(path.as_deref(), json);
    };

    let into = into.unwrap_or_else(|| {
        aurum_studio_core::registry::Registry::resolve_path()
            .and_then(|registry| registry.parent().map(|home| home.join("godot")))
            .unwrap_or_else(|| PathBuf::from("godot"))
    });

    let available = aurum_studio_core::downloads::Availability {
        version: String::new(),
        platform: std::env::consts::OS.to_string(),
        url,
        sha256: digest,
    };

    match aurum_studio_core::downloads::install(
        &available,
        &into,
        &aurum_studio_core::downloads::SystemFetcher,
    ) {
        Ok(installed) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "installed": installed.display().to_string(),
                        "verified": true,
                    })
                );
            } else {
                println!("installed {} (digest verified)", installed.display());
            }
            ExitCode::from(exit::OK)
        }
        Err(aurum_studio_core::downloads::DownloadError::AlreadyInstalled(path)) => {
            if !json {
                println!("already installed and verified: {}", path.display());
            }
            ExitCode::from(exit::OK)
        }
        Err(error) => {
            eprintln!("aurum godot: {error}");
            ExitCode::from(exit::FAILED)
        }
    }
}

/// What this machine already has.
fn list_godot(path: Option<&Path>, json: bool) -> ExitCode {
    let root = path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // A project names the Godot it wants; without one there is still a PATH to
    // look at, which is what most machines actually rely on.
    let found = match Project::open(&root) {
        Ok(project) => {
            let toolchain = discover(&project, None);
            toolchain.godot
        }
        Err(_) => None,
    };

    // The windowed build is what gets launched; report the one that will be.
    let launchable = Project::open(&root)
        .ok()
        .and_then(|project| discover_godot_to_launch(&project, None));

    match (&found, &launchable) {
        (_, Some(binary)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "godot": binary.display().to_string(),
                        "version": found.as_ref().and_then(|t| t.version.clone()),
                    })
                );
            } else {
                println!("godot: {}", binary.display());
                if let Some(version) = found.as_ref().and_then(|t| t.version.as_deref()) {
                    println!("  version: {version}");
                }
            }
            ExitCode::from(exit::OK)
        }
        _ => {
            eprintln!(
                "aurum godot: no Godot was found on PATH or beside the project; \
                 fetch one with `aurum godot --fetch <url> --sha256 <digest>`"
            );
            ExitCode::from(exit::WARNING)
        }
    }
}

/// `aurum presets [project]`
///
/// What this project can be exported into. Read rather than generated: Godot
/// owns the format, the fields differ per platform, and a generator would be a
/// second implementation of somebody else's schema whose failure mode is a
/// preset that looks right and produces a broken build.
pub fn presets(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("presets", &message),
    };

    let path = target(&options);
    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("aurum presets: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    let Some(godot_root) = project.godot_project_dir() else {
        eprintln!("aurum presets: no project.godot, so there is nothing to export");
        return ExitCode::from(exit::WARNING);
    };

    let report = match aurum_studio_core::presets::read(godot_root) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("aurum presets: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    let impossible = report.impossible(aurum_studio_core::presets::KNOWN_PLATFORMS);

    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "project": project.config.name,
                "file_present": report.file_present,
                "presets": report.presets.iter().map(|preset| serde_json::json!({
                    "name": preset.name,
                    "platform": preset.platform,
                    "runnable": preset.runnable,
                })).collect::<Vec<_>>(),
                "impossible": impossible.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            })
        );
    } else if !report.file_present {
        println!("{}: no export_presets.cfg", project.config.name);
        println!("  a project carried between machines often arrives without one");
        println!("  add presets in Godot's Project > Export dialog");
    } else if report.presets.is_empty() {
        println!(
            "{}: export_presets.cfg has no usable presets",
            project.config.name
        );
    } else {
        println!("{} export presets", project.config.name);
        for preset in &report.presets {
            let marker = if preset.runnable { "run" } else { "   " };
            let flag = if impossible.iter().any(|p| p.name == preset.name) {
                "  <- Godot has no such platform"
            } else {
                ""
            };
            println!("  {marker}  {:<20} {}{flag}", preset.name, preset.platform);
        }
    }

    // A preset that cannot work is the one outcome worth branching on.
    if impossible.is_empty() && !report.presets.is_empty() {
        ExitCode::from(exit::OK)
    } else {
        ExitCode::from(exit::WARNING)
    }
}

/// `aurum stop [project]`
///
/// Stops only processes this project's most recent session launched, and only
/// after proving each one is still the process it recorded.
pub fn stop(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("stop", &message),
    };
    let path = target(&options);
    let root = path.canonicalize().map(clean_path).unwrap_or(path);

    let Some(session) = Session::latest_for(&root) else {
        eprintln!("aurum stop: no session was recorded for {}", root.display());
        return ExitCode::from(exit::WARNING);
    };

    let outcomes = stop_session(&session, options.force, STOP_TIMEOUT);
    if outcomes.is_empty() {
        println!("no processes were running for session {}", session.id);
        return ExitCode::from(exit::OK);
    }

    let mut refused = 0;
    let mut declined = 0;
    for (kind, outcome) in &outcomes {
        if options.json {
            continue;
        }
        println!("  {}", outcome.describe(*kind));
        match outcome {
            StopOutcome::Refused(_) => refused += 1,
            StopOutcome::StillRunning => declined += 1,
            _ => {}
        }
    }

    if options.json {
        let reported: Vec<serde_json::Value> = outcomes
            .iter()
            .map(|(kind, outcome)| {
                serde_json::json!({
                    "kind": kind.label(),
                    "outcome": format!("{outcome:?}"),
                    "description": outcome.describe(*kind),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "session": session.id, "outcomes": reported })
        );
    }

    if refused > 0 {
        eprintln!("aurum stop: {refused} process(es) were left alone; they are not the ones this session launched");
        return ExitCode::from(exit::FAILED);
    }
    if declined > 0 {
        eprintln!(
            "aurum stop: {declined} process(es) are still running and did not close; \
             re-run with --force to terminate them, which discards unsaved work"
        );
        return ExitCode::from(exit::WARNING);
    }
    ExitCode::from(exit::OK)
}

/// How long to wait for a process to close before reporting it still running.
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `aurum dev [project]`
///
/// The supervised loop: build, launch the editor, watch for changes, and
/// rebuild when the Rust side moves. Godot content is left to Godot, which
/// reloads it without help.
///
/// With `--play` the loop also supervises the game, so the one verdict that
/// costs a process can be acted on: a change that invalidates live gameplay
/// restarts the game while the editor keeps running.
pub fn dev(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("dev", &message),
    };
    let path = target(&options);

    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("aurum dev: {error}");
            return ExitCode::from(exit::BLOCKED);
        }
    };

    let Some(package) = project.config.rust_package.clone() else {
        eprintln!("aurum dev: aurum.toml has no 'rust_package'; there is nothing to build");
        return ExitCode::from(exit::USAGE);
    };
    let Some(addon) = project.layout.addon_directory.clone() else {
        eprintln!("aurum dev: no Aurum add-on directory found");
        return ExitCode::from(exit::BLOCKED);
    };

    let toolchain = discover(&project, options.godot.as_deref());
    let Some(cargo) = toolchain.cargo.as_ref().map(|tool| tool.path.clone()) else {
        eprintln!("aurum dev: Cargo was not found on PATH");
        return ExitCode::from(exit::BLOCKED);
    };

    let profile = if options.release {
        Profile::Release
    } else {
        Profile::Debug
    };
    let destination = addon
        .join("bin")
        .join(profile.installed_filename(&library_name(&package)));
    let request = BuildRequest::new(&project.root, &package, profile, &destination, cargo);

    // ---- initial build ---------------------------------------------------
    if !options.json {
        println!(
            "building {} ({})",
            package,
            if profile.is_debug() {
                "debug"
            } else {
                "release"
            }
        );
    }
    match aurum_studio_core::build::run(&request, options.force, BUILD_TIMEOUT) {
        Ok(report) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "event": "build",
                        "package": package,
                        "built": report.built,
                        "replaced": report.replaced,
                        "installed_sha256": report.installed.sha256,
                        "summary": report.summary(),
                    })
                );
            } else {
                println!("  {}", report.summary());
            }
        }
        Err(error) => {
            // A failed first build is not fatal to the loop: the editor can
            // still open, and a later edit may fix it. The installed library
            // is untouched either way.
            eprintln!("aurum dev: initial build failed; continuing to watch");
            eprintln!("{error}");
        }
    }

    if options.once {
        return ExitCode::from(exit::OK);
    }

    // ---- processes -------------------------------------------------------
    //
    // The editor and the game share one session, so `aurum stop` finds them
    // together and a gameplay restart cannot leave the replaced game behind in
    // a session of its own.
    let mut session: Option<Session> = None;
    if !options.no_editor || options.play {
        match Session::create(&project.root) {
            Ok(created) => session = Some(created),
            Err(error) => {
                // Degraded rather than blocked, per the design: builds and
                // watching continue without processes attached.
                eprintln!("aurum dev: could not start a session: {error}");
                eprintln!("aurum dev: continuing without an editor or a game attached");
            }
        }
    }

    let mut editor: Option<OwnershipRecord> = None;
    if !options.no_editor {
        if let Some(session) = &session {
            match launch_editor(session, &project, options.godot.as_deref()) {
                Ok(record) => {
                    if !options.json {
                        println!(
                            "editor running (pid {}), log {}",
                            record.pid,
                            session.log_path().display()
                        );
                    }
                    editor = Some(record);
                }
                Err(message) => {
                    // Degraded rather than blocked, per the design: builds and
                    // watching continue without an editor attached.
                    eprintln!("aurum dev: {message}");
                    eprintln!("aurum dev: continuing without an editor attached");
                }
            }
        }
    }

    let mut game: Option<Game> = None;
    if options.play {
        match session.as_ref() {
            Some(session) => match launch_game(session, &project, options.godot.as_deref()) {
                Ok((started, pid)) => {
                    if options.json {
                        println!("{}", serde_json::json!({ "event": "game", "pid": pid }));
                    } else {
                        println!(
                            "game running (pid {pid}); a change that invalidates it restarts it"
                        );
                    }
                    game = Some(started);
                }
                Err(message) => {
                    eprintln!("aurum dev: {message}");
                    eprintln!("aurum dev: continuing without a game attached");
                }
            },
            None => eprintln!("aurum dev: --play needs a session the game can belong to"),
        }
    }

    // ---- agent -----------------------------------------------------------
    //
    // An MCP client starts the server itself. `aurum mcp` speaks over stdio to
    // whoever spawned it, so a copy launched from here would have nobody on the
    // other end of its pipe and would sit until the loop ended. What an agent
    // cannot guess is the two things this writes into the client's own
    // configuration: where the project is, and where the running editor
    // publishes what it is running. The second is what makes the difference
    // between an agent driving a headless engine and an agent driving the one
    // on screen.
    if let Some(clients) = options.agent.as_deref() {
        let mut invocation = aurum_mcp::connect::Invocation::current(Some(&project.root));
        // Only offered when the directory exists, because a bridge path that
        // points at nothing is worse than no bridge path: the server would
        // report an editor that is not there.
        if let Some(bridge) = project
            .godot_project_dir()
            .map(|godot| godot.join(".godot").join("aurum"))
            .filter(|path| path.is_dir())
        {
            invocation.args.push("--editor-bridge".to_string());
            invocation.args.push(bridge.display().to_string());
        }
        for name in clients.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let Some(client) = aurum_mcp::connect::Client::parse(name) else {
                eprintln!("aurum dev: {}", aurum_mcp::connect::unknown_client(name));
                continue;
            };
            match aurum_mcp::connect::install(client, &project.root, &invocation) {
                Ok(message) => println!("{:<16} {message}", client.name()),
                Err(message) => eprintln!("{:<16} {message}", client.name()),
            }
        }
        if options.json {
            println!(
                "{}",
                serde_json::json!({
                    "event": "agent",
                    "clients": clients,
                    "command": invocation.command,
                    "args": invocation.args,
                })
            );
        }
    }

    // ---- watch -----------------------------------------------------------
    let mut watcher = Watcher::new(&project.root);
    watcher.scan();
    let mut debouncer = Debouncer::new(interval(options.interval_ms));

    if !options.json {
        let leaving = match (editor.is_some(), game.is_some()) {
            (true, true) => "the editor and the game are left running",
            (true, false) => "the editor is left running",
            (false, true) => "the game is left running",
            (false, false) => "nothing is left running",
        };
        println!(
            "watching {} ({} files). Ctrl+C to stop; {leaving}.",
            project.root.display(),
            watcher.tracked()
        );
        println!(
            "an agent attaches with `aurum mcp --root {}`; \
             `aurum dev --agent <client>` writes that into a client for you",
            project.root.display()
        );
    }

    loop {
        debouncer.push(watcher.scan());
        let Some(batch) = debouncer.take_if_settled() else {
            std::thread::sleep(interval(options.interval_ms));
            continue;
        };

        let classification = classify_batch(&batch);
        if classification.verdict == Verdict::NoAction {
            continue;
        }

        if options.json {
            println!(
                "{}",
                serde_json::json!({
                    "changed": batch.len(),
                    "verdict": classification.verdict.label(),
                    "reason": classification.reason,
                })
            );
        } else {
            println!(
                "{} file(s) changed -> {}: {}",
                batch.len(),
                classification.verdict.label(),
                classification.reason
            );
        }

        let rebuild = batch
            .iter()
            .any(|change| aurum_studio_core::reload::rebuild_required(&change.path));

        let mut installed = true;
        let fingerprint_before = editor_live_fingerprint(&project);
        let mut replaced = false;
        if rebuild {
            match aurum_studio_core::build::run(&request, false, BUILD_TIMEOUT) {
                Ok(report) if report.replaced => {
                    replaced = true;
                    println!("  rebuilt: {}", report.summary());
                }
                Ok(_) => println!("  rebuilt: no change in the artifact"),
                Err(error) => {
                    // The working library is untouched, which is what lets the
                    // loop keep going.
                    installed = false;
                    eprintln!("  build failed; the installed extension is unchanged");
                    eprintln!("  {error}");
                }
            }
        }

        // Only asked when the installed library actually changed. A build that
        // produced identical bytes has nothing to reload, so demanding
        // evidence for it would report a failure where nothing was attempted.
        if replaced {
            report_reload_evidence(&project, fingerprint_before.as_deref(), options.json);
        }

        // A verdict is acted on only once the build it implies has landed:
        // restarting the game onto a half-installed change would leave it
        // running something the user cannot see in the sources.
        if installed {
            let response = respond(&classification, game.as_mut(), options.force, STOP_TIMEOUT);
            report_response(
                classification.verdict,
                &response,
                editor.as_ref(),
                options.json,
            );
        }
    }
}

/// What the running editor reports about the extension it has loaded.
///
/// A rebuild that installs a new library proves a *file* was written. Only the
/// editor can say whether the new code is the code running, and it says so by
/// publishing the fingerprint the loaded extension returns — see the
/// `aurum_editor` plugin. Without that plugin there is no evidence either way,
/// and the loop says so rather than implying a reload it cannot see.
///
/// `None` means the bridge is silent, which is a different report from a
/// fingerprint that did not change.
fn editor_live_fingerprint(project: &Project) -> Option<String> {
    let godot = project.godot_project_dir()?;
    let path = godot
        .join(".godot")
        .join("aurum")
        .join("live-fingerprint.txt");
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Say whether a rebuild was picked up, and be precise about what that means.
///
/// The comparison is the point. A fingerprint that changed between builds is
/// evidence the editor is running the new code. The same fingerprint twice is
/// *not* evidence of anything, and reporting it as though it were would be the
/// exact assumption this exists to remove.
fn report_reload_evidence(project: &Project, before: Option<&str>, json: bool) {
    let after = editor_live_fingerprint(project);
    let message = reload_evidence_message(after.as_deref(), before);

    if json {
        println!(
            "{}",
            serde_json::json!({ "reload_evidence": message, "fingerprint": after })
        );
    } else {
        println!("  {message}");
    }
}

/// The sentence a rebuild earns, given what the editor said before and after.
///
/// Separated from the printing so the reasoning can be tested directly. The
/// distinction it draws is the whole point of the feature: evidence, no
/// evidence, and *absence* of a bridge are three different situations, and
/// collapsing them into one cheerful "reloaded" is what this replaces.
fn reload_evidence_message(after: Option<&str>, before: Option<&str>) -> String {
    match (after, before) {
        (Some(now), Some(was)) if now != was => {
            format!("the editor is running the new build (fingerprint {was} -> {now})")
        }
        (Some(now), Some(_)) => format!(
            "the editor still reports {now}, unchanged, so this is not evidence of a reload; \
             the fingerprint is fixed at compile time, so a normal edit cannot move it"
        ),
        (Some(now), None) => format!("the editor reports {now}"),
        (None, _) => "the editor bridge is silent, so a reload could not be confirmed; \
                      run `aurum doctor` to check the aurum_editor plugin"
            .to_string(),
    }
}

/// Say what a verdict did, and no more than it did.
///
/// A gameplay restart is the only one this loop takes, so it is also the only
/// one that reports a process moving. An editor restart is reported with the
/// reason the classifier found and nothing else, because taking it is the
/// user's decision and needs their unsaved work dealt with first.
fn report_response(
    verdict: Verdict,
    response: &Response,
    editor: Option<&OwnershipRecord>,
    json: bool,
) {
    if *response == Response::Nothing {
        return;
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "event": "outcome",
                "verdict": verdict.label(),
                "outcome": response.describe(),
                "restarted": response.restarted(),
            })
        );
        return;
    }

    println!("  {}", response.describe());
    // The other half of the criterion, said where it can be checked against a
    // process list: a gameplay restart is not an editor restart.
    if response.restarted() {
        if let Some(editor) = editor {
            println!("  the editor (pid {}) was not touched", editor.pid);
        }
    }
}

/// Launch the editor into a dev session.
fn launch_editor(
    session: &Session,
    project: &Project,
    godot_hint: Option<&Path>,
) -> Result<OwnershipRecord, String> {
    let Some(godot_project) = project.godot_project_dir().map(Path::to_path_buf) else {
        return Err("no project.godot was found".into());
    };
    let Some(godot) = discover_godot_to_launch(project, godot_hint) else {
        return Err("Godot was not found; pass --godot <path>".into());
    };

    let mut request = LaunchRequest::godot(&godot, &godot_project, &project.root);
    request.environment = bridge_environment(session, None);

    let launched = launch(&request, session).map_err(|e| e.to_string())?;
    Ok(launched.record)
}

/// Launch the game this session supervises, so a gameplay restart has
/// something to replace.
///
/// Without `--editor` Godot runs the project's main scene, which is exactly
/// what a gameplay restart replaces.
fn launch_game(
    session: &Session,
    project: &Project,
    godot_hint: Option<&Path>,
) -> Result<(Game, u32), String> {
    let Some(godot_project) = project.godot_project_dir().map(Path::to_path_buf) else {
        return Err("no project.godot was found".into());
    };
    let Some(godot) = discover_godot_to_launch(project, godot_hint) else {
        return Err("Godot was not found; pass --godot <path>".into());
    };

    let mut request = LaunchRequest::godot(&godot, &godot_project, &project.root);
    request.environment = bridge_environment(session, None);

    let mut game = Game::new(request, session.clone());
    let pid = game.start().map_err(|e| e.to_string())?;
    Ok((game, pid))
}

/// Classify a batch, reading Rust sources so schema changes are visible.
fn classify_batch(batch: &[aurum_studio_core::watch::Change]) -> Classification {
    let pairs: Vec<(PathBuf, Option<String>)> = batch
        .iter()
        .filter(|change| change.kind != aurum_studio_core::watch::ChangeKind::Removed)
        .map(|change| {
            // Only Rust sources need their contents: the schema markers live
            // there, and reading every file in a batch would be wasteful.
            let contents = (change.path.extension().is_some_and(|e| e == "rs"))
                .then(|| std::fs::read_to_string(&change.path).ok())
                .flatten();
            (change.path.clone(), contents)
        })
        .collect();

    aurum_studio_core::reload::classify_all(
        pairs
            .iter()
            .map(|(path, contents)| (path.as_path(), contents.as_deref())),
    )
}

fn interval(milliseconds: u64) -> std::time::Duration {
    std::time::Duration::from_millis(milliseconds.clamp(50, 5_000))
}

/// `aurum projects`
pub fn projects(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("projects", &message),
    };

    let Some(path) = Registry::resolve_path() else {
        eprintln!("aurum projects: no home directory to keep a registry in");
        return ExitCode::from(exit::FAILED);
    };
    let registry = match Registry::load(&path) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("aurum projects: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    if options.json {
        let entries: Vec<serde_json::Value> = registry
            .projects
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "path": entry.path.display().to_string(),
                    "present": entry.path.is_dir(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({ "registry": path.display().to_string(), "projects": entries })
        );
        return ExitCode::from(exit::OK);
    }

    println!("registry: {}", path.display());
    if registry.is_empty() {
        println!("no projects registered; add one with `aurum import <path>`");
        return ExitCode::from(exit::OK);
    }
    for entry in &registry.projects {
        // A registered project whose directory is gone is shown, not hidden:
        // it may just be on a drive that is not mounted right now.
        let mark = if entry.path.is_dir() { ' ' } else { '!' };
        println!("  [{mark}] {:<24} {}", entry.name, entry.path.display());
    }
    ExitCode::from(exit::OK)
}

/// `aurum import <path>`
///
/// Read-only against the project: it opens and validates, then records the
/// path. Nothing in the project is written without being asked.
pub fn import(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("import", &message),
    };
    let path = target(&options);

    let project = match Project::open(&path) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("aurum import: {error}");
            eprintln!("hint: point at the directory containing aurum.toml");
            return ExitCode::from(exit::BLOCKED);
        }
    };

    let Some(registry_path) = Registry::resolve_path() else {
        eprintln!("aurum import: no home directory to keep a registry in");
        return ExitCode::from(exit::FAILED);
    };
    let mut registry = match Registry::load(&registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("aurum import: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    let added = registry.register(&project.config.name, &project.root);
    if let Err(error) = registry.save(&registry_path) {
        eprintln!("aurum import: {error}");
        return ExitCode::from(exit::FAILED);
    }

    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "name": project.config.name,
                "path": project.root.display().to_string(),
                "added": added,
                "registry": registry_path.display().to_string(),
            })
        );
    } else {
        println!(
            "{} '{}' at {}",
            if added { "imported" } else { "updated" },
            project.config.name,
            project.root.display()
        );
    }
    ExitCode::from(exit::OK)
}

/// `aurum forget <name|path>`
pub fn forget(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => return usage("forget", &message),
    };
    let Some(needle) = options.positional.first() else {
        return usage("forget", "a project name or path is required");
    };

    let Some(registry_path) = Registry::resolve_path() else {
        eprintln!("aurum forget: no home directory to keep a registry in");
        return ExitCode::from(exit::FAILED);
    };
    let mut registry = match Registry::load(&registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("aurum forget: {error}");
            return ExitCode::from(exit::FAILED);
        }
    };

    // Removing from the registry never touches the project on disk.
    if !registry.remove(needle) {
        eprintln!("aurum forget: '{needle}' is not registered");
        return ExitCode::from(exit::WARNING);
    }
    if let Err(error) = registry.save(&registry_path) {
        eprintln!("aurum forget: {error}");
        return ExitCode::from(exit::FAILED);
    }

    if options.json {
        println!("{}", serde_json::json!({ "removed": needle }));
    } else {
        println!("forgot '{needle}' (the project itself is untouched)");
    }
    ExitCode::from(exit::OK)
}

/// Print a usage error for a command.
fn usage(command: &str, message: &str) -> ExitCode {
    if message != "help" {
        eprintln!("aurum {command}: {message}");
    }
    eprintln!("\n{}", command_usage(command));
    ExitCode::from(if message == "help" {
        exit::OK
    } else {
        exit::USAGE
    })
}

fn command_usage(command: &str) -> &'static str {
    match command {
        "doctor" => {
            "usage: aurum doctor [project] [--godot <path>] [--json]\n\
             \n\
             Reports healthy, warning, or blocked state with evidence.\n\
             exit 0 healthy, 1 warning, 2 blocked"
        }
        "projects" => "usage: aurum projects [--json]",
        "build" => {
            "usage: aurum build [project] [--release] [--force] [--json]\n\
             \n\
             Builds the GDExtension and installs it. A failed build leaves the\n\
             installed library untouched."
        }
        "dev" => {
            "usage: aurum dev [project] [--godot <path>] [--release] [--force]\n\
             \n\
             [--once] [--no-editor] [--play] [--interval <ms>]\n\
             [--agent <client>[,<client>...]] [--json]\n\
             \n\
             Builds, launches the editor, and rebuilds when the Rust side moves.\n\
             Godot reloads its own content. --play also supervises the game, so a\n\
             change that invalidates live gameplay restarts the game and never the\n\
             editor. An editor restart is reported with its reason, never taken.\n\
             --force rebuilds even when the artifact is current, and terminates a\n\
             game that will not close when asked. Ctrl+C leaves both running."
        }
        "editor" => "usage: aurum editor [project] [--godot <path>] [--json]",
        "run" => "usage: aurum run [project] [--godot <path>] [--json]",
        "stop" => "usage: aurum stop [project] [--force] [--json]",
        "import" => "usage: aurum import <project-path> [--json]",
        "forget" => "usage: aurum forget <name-or-path>",
        _ => "usage: aurum <command>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--agent` names the clients an MCP connection is written into, and it
    /// takes a value. Parsed here rather than at the call site because the
    /// failure it prevents is the quiet kind: a flag that swallows the next
    /// argument would make `aurum dev --agent` mean `aurum dev` with a project
    /// called nothing.
    #[test]
    fn agent_takes_a_client_name_and_refuses_to_be_given_none() {
        let options = parse(&["--agent".to_string(), "codex,claude-code".to_string()])
            .expect("--agent with a value parses");
        assert_eq!(options.agent.as_deref(), Some("codex,claude-code"));

        let bare = parse(&["--agent".to_string()]);
        assert!(bare.is_err(), "a bare --agent must not parse");
        assert!(bare.unwrap_err().contains("client name"));
    }

    /// Without the flag nothing is written into any client, which is what keeps
    /// `dev` from editing a user's editor configuration behind their back.
    #[test]
    fn no_agent_flag_means_no_client_is_touched() {
        let options = parse(&[]).expect("no arguments parses");
        assert!(options.agent.is_none());
    }
    use super::*;

    #[test]
    fn a_changed_fingerprint_is_the_only_thing_that_counts_as_evidence() {
        let changed = reload_evidence_message(Some("build-2"), Some("build-1"));
        assert!(
            changed.contains("running the new build"),
            "a moved fingerprint is evidence, got '{changed}'"
        );
        assert!(changed.contains("build-1") && changed.contains("build-2"));
    }

    #[test]
    fn an_unchanged_fingerprint_is_reported_as_no_evidence() {
        // The honest case, and the common one: the fingerprint is fixed at
        // compile time, so an ordinary edit leaves it alone. Saying "reloaded"
        // here would be an assumption dressed as an observation.
        let same = reload_evidence_message(Some("aurum-unmanaged"), Some("aurum-unmanaged"));
        assert!(
            same.contains("not evidence of a reload"),
            "an unchanged fingerprint proves nothing, got '{same}'"
        );
        assert!(
            !same.contains("running the new build"),
            "it must not claim a reload it cannot see"
        );
    }

    #[test]
    fn a_silent_bridge_says_so_and_says_what_to_do() {
        // Absence of the bridge is a third state, distinct from "no evidence":
        // there is nothing wrong with the build, and the user can fix it.
        let silent = reload_evidence_message(None, Some("build-1"));
        assert!(silent.contains("could not be confirmed"), "got '{silent}'");
        assert!(
            silent.contains("aurum doctor"),
            "it should point at the check that explains why, got '{silent}'"
        );
    }

    #[test]
    fn a_first_build_with_a_live_bridge_states_what_the_editor_reports() {
        // Nothing to compare against yet, so it reports the observation
        // without dressing it up as a comparison.
        let first = reload_evidence_message(Some("aurum-unmanaged"), None);
        assert!(first.contains("aurum-unmanaged"), "got '{first}'");
        assert!(!first.contains("->"), "there was nothing to compare with");
    }

    #[test]
    fn a_restart_proceeds_only_when_the_old_editor_is_gone() {
        // The rule the command exists to keep. A refusal has to come before the
        // launch, because starting a second editor over one that would not
        // close is how a project ends up with two holding the same files.
        assert!(may_start_again(&[StopOutcome::Stopped]));
        assert!(may_start_again(&[StopOutcome::NotRunning]));
        assert!(may_start_again(&[
            StopOutcome::Stopped,
            StopOutcome::NotRunning
        ]));
        assert!(
            may_start_again(&[]),
            "nothing running is not a reason to refuse"
        );
    }

    #[test]
    fn a_restart_is_refused_when_the_editor_would_not_close() {
        assert!(
            !may_start_again(&[StopOutcome::StillRunning]),
            "a window with unsaved work must not be discarded to save a keypress"
        );
        assert!(
            !may_start_again(&[StopOutcome::Refused("not ours".into())]),
            "a process this session did not launch must never be replaced"
        );
    }

    #[test]
    fn one_stubborn_editor_stops_the_whole_restart() {
        // With several records, one refusal is enough: starting a replacement
        // while any of the old ones is still up is the situation the rule is
        // for.
        assert!(!may_start_again(&[
            StopOutcome::Stopped,
            StopOutcome::StillRunning,
        ]));
        assert!(!may_start_again(&[
            StopOutcome::StillRunning,
            StopOutcome::Stopped,
        ]));
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_the_shared_flag_shape() {
        let options = parse(&args(&["--json", "--godot", "A:/g.exe", "--project", "p"])).unwrap();
        assert!(options.json);
        assert_eq!(options.godot, Some(PathBuf::from("A:/g.exe")));
        assert_eq!(options.project, Some(PathBuf::from("p")));
    }

    #[test]
    fn accepts_equals_forms_and_short_project() {
        let options = parse(&args(&["--godot=A:/g.exe", "-p", "p"])).unwrap();
        assert_eq!(options.godot, Some(PathBuf::from("A:/g.exe")));
        assert_eq!(options.project, Some(PathBuf::from("p")));
    }

    #[test]
    fn positionals_are_collected_and_flags_are_separated() {
        let options = parse(&args(&["some/path", "--json"])).unwrap();
        assert_eq!(options.positional, vec!["some/path".to_string()]);
        assert!(options.json);
    }

    #[test]
    fn the_game_is_supervised_only_when_it_is_asked_for() {
        // Without this the loop would launch a game nobody wanted, and the
        // verdict that restarts it would have nothing to act on.
        assert!(!parse(&args(&[])).unwrap().play);
        assert!(!parse(&args(&["--no-editor"])).unwrap().play);
        assert!(parse(&args(&["--play"])).unwrap().play);
        assert!(parse(&args(&["--play", "--no-editor"])).unwrap().play);
    }

    #[test]
    fn unknown_options_and_missing_values_are_refused() {
        assert!(parse(&args(&["--nope"])).is_err());
        assert!(parse(&args(&["--godot"])).is_err());
        assert!(parse(&args(&["--project"])).is_err());
        // A single dash is a negative-looking positional, not an option.
        assert!(parse(&args(&["-x"])).is_err());
    }

    #[test]
    fn help_is_signalled_distinctly_from_an_error() {
        assert_eq!(parse(&args(&["--help"])).unwrap_err(), "help");
    }

    #[test]
    fn the_target_prefers_the_flag_then_the_positional_then_cwd() {
        let mut options = Options {
            project: Some(PathBuf::from("flagged")),
            ..Default::default()
        };
        options.positional.push("positional".into());
        assert_eq!(target(&options), PathBuf::from("flagged"));

        let options = Options {
            positional: vec!["positional".into()],
            ..Default::default()
        };
        assert_eq!(target(&options), PathBuf::from("positional"));

        let options = Options::default();
        assert_eq!(target(&options), std::env::current_dir().unwrap());
    }

    #[test]
    fn exit_codes_are_distinct_per_state() {
        // Callers branch on these, so collisions would be a bug.
        let codes = [
            exit::OK,
            exit::WARNING,
            exit::BLOCKED,
            exit::USAGE,
            exit::FAILED,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
        assert_eq!(exit::OK, 0, "success must be zero");
    }
}
