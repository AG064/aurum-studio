//! Shared command-line entry point for the MCP server.
//!
//! Both the `aurum-mcp` binary and `aurum mcp` run through here, so the two
//! entry points cannot drift apart.

use std::io::{self, Write};
use std::path::PathBuf;

use crate::engine::Engine;
use crate::server::{serve, ServerConfig};
use crate::tools::PathGuard;

/// Usage text shared by both binaries.
pub const USAGE: &str = "\
aurum mcp - Aurum Studio over the Model Context Protocol

USAGE:
    aurum mcp [OPTIONS]
    aurum-mcp [OPTIONS]

The server speaks newline-delimited JSON-RPC 2.0 on stdin/stdout, which is the
MCP stdio transport. Point an MCP client at this command as a local server.

OPTIONS:
    --tools <MODE>  studio: compact project tools; all: full engine catalog.
    --root <DIR>    Directory that aurum_save and aurum_load may write inside.
                    Defaults to the current working directory.
    --read-only     Refuse mutating tools and omit them from tools/list.
    --deny <names>  Withhold the named tools. Repeatable, and accepts a
                    comma-separated list. A withheld tool is refused and
                    omitted from tools/list, so a client never sees one it
                    cannot use. aurum_mcp_status is never withheld, because
                    a client that cannot ask what is missing cannot tell a
                    refusal from a typo.
    --trace         Payload-free protocol summaries to stderr.
    --editor-bridge <DIR>
                    Directory the Aurum Editor plugin polls, enabling the
                    aurum_editor_* tools. The plugin prints the path it uses.
    --print-config <client>
                    Print the configuration for a client, and write nothing.
    --install <client>[,<client>...]
                    Write it into that client's configuration file, merging
                    with whatever is already there.
    -h, --help      Print this help, including how to connect an agent.
    -V, --version   Print the version.

EXAMPLES:
    aurum mcp --root ./saves
    aurum mcp --read-only
    aurum mcp --install codex,claude-code

";

/// Parsed command-line options.
#[derive(Debug)]
pub struct Options {
    pub root: Option<PathBuf>,
    pub config: ServerConfig,
}

/// Parse an argument vector. `Ok(None)` means help or version was handled and
/// the caller should exit successfully without starting a server.
///
/// Takes a slice rather than an iterator because `--root DIR` consumes two
/// tokens, which an iterator cannot express without lookahead.
pub fn parse_args_slice(args: &[String]) -> Result<Option<Options>, String> {
    let mut root = None;
    let mut config = ServerConfig::default();
    let mut index = 0;
    let mut connection: Option<(bool, String)> = None;

    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "-h" | "--help" => {
                print!("{USAGE}\n{}", crate::connect::CONNECT_HELP);
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("aurum-mcp {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            // Connecting an agent is a command on this binary rather than a
            // separate tool, because the thing being connected *is* this
            // binary. Two entry points would be two things to keep in step.
            "--print-config" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--print-config requires a client name".to_string())?;
                connection = Some((false, value.clone()));
            }
            "--install" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--install requires a client name".to_string())?;
                connection = Some((true, value.clone()));
            }
            "--read-only" => config.read_only = true,
            "--tools" => {
                index += 1;
                let profile = args.get(index).ok_or("--tools requires studio or all")?;
                match profile.as_str() {
                    "studio" => {
                        for tool in crate::tools::catalog() {
                            if !tool.name.starts_with("aurum_project_")
                                && tool.name != crate::tools::STATUS_TOOL
                                && !config.denied.iter().any(|name| name == tool.name)
                            {
                                config.denied.push(tool.name.to_string());
                            }
                        }
                    }
                    "all" => {}
                    _ => return Err("--tools must be studio or all".into()),
                }
            }
            "--trace" => config.trace = true,
            "--deny" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--deny requires a tool name".to_string())?;
                // Accepted comma-separated as well as repeated, because a
                // caller scripting this usually has a list rather than a loop.
                for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                    if !config.denied.iter().any(|d| d == name) {
                        config.denied.push(name.to_string());
                    }
                }
            }
            "--editor-bridge" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--editor-bridge requires a directory argument".to_string())?;
                config.editor_bridge = Some(crate::editor_tools::resolve_bridge(value));
            }
            "--root" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--root requires a directory argument".to_string())?;
                root = Some(PathBuf::from(value));
            }
            other => {
                if let Some(value) = other.strip_prefix("--root=") {
                    if value.is_empty() {
                        return Err("--root requires a directory argument".into());
                    }
                    root = Some(PathBuf::from(value));
                } else if let Some(value) = other.strip_prefix("--editor-bridge=") {
                    if value.is_empty() {
                        return Err("--editor-bridge requires a directory argument".into());
                    }
                    config.editor_bridge = Some(crate::editor_tools::resolve_bridge(value));
                } else {
                    return Err(format!("unknown argument: {other}"));
                }
            }
        }
        index += 1;
    }

    if let Some((install, names)) = connection {
        let project = root
            .clone()
            .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let clients = names
            .split(',')
            .map(str::trim)
            .map(|name| {
                crate::connect::Client::parse(name)
                    .ok_or_else(|| crate::connect::unknown_client(name))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut invocation = crate::connect::Invocation::current(Some(&project));
        if config.read_only {
            invocation.args.push("--read-only".into());
        }
        if !config.denied.is_empty() {
            invocation
                .args
                .extend(["--deny".into(), config.denied.join(",")]);
        }
        if let Some(bridge) = &config.editor_bridge {
            invocation
                .args
                .extend(["--editor-bridge".into(), bridge.display().to_string()]);
        }
        for client in clients {
            if install {
                println!(
                    "{}",
                    crate::connect::install(client, &project, &invocation)?
                );
            } else {
                println!("{}", crate::connect::snippet(client, &invocation));
            }
        }
        return Ok(None);
    }
    Ok(Some(Options { root, config }))
}

/// Run the server, returning the process exit code.
pub fn run(args: &[String]) -> i32 {
    let options = match parse_args_slice(args) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(message) => {
            eprintln!("aurum mcp: {message}\n\n{USAGE}");
            return 2;
        }
    };

    let root = match options.root {
        Some(root) => root,
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => {
                eprintln!("aurum mcp: cannot determine the working directory: {e}");
                return 1;
            }
        },
    };

    if !root.is_dir() {
        eprintln!(
            "aurum mcp: root '{}' is not an existing directory",
            root.display()
        );
        return 2;
    }

    let paths = PathGuard::new(root);
    let mut engine = Engine::new();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    if options.config.trace {
        eprintln!(
            "[aurum-mcp] ready: fingerprint={}, root={}, read_only={}",
            crate::engine::runtime_fingerprint(),
            paths.root().display(),
            options.config.read_only
        );
    }

    match serve(
        stdin.lock(),
        &mut stdout,
        &mut engine,
        &paths,
        options.config,
    ) {
        Ok(()) => 0,
        // The client closing the pipe is a normal shutdown, not a failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            let _ = writeln!(io::stderr(), "aurum mcp: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args_slice(&owned)
    }

    #[test]
    fn defaults_are_empty() {
        let o = parse(&[]).unwrap().unwrap();
        assert!(o.root.is_none());
        assert!(!o.config.read_only);
        assert!(!o.config.trace);
    }

    #[test]
    fn parses_flags() {
        let o = parse(&["--read-only", "--trace"]).unwrap().unwrap();
        assert!(o.config.read_only);
        assert!(o.config.trace);
    }

    #[test]
    fn parses_root_in_both_forms() {
        assert_eq!(
            parse(&["--root", "saves"]).unwrap().unwrap().root,
            Some(PathBuf::from("saves"))
        );
        assert_eq!(
            parse(&["--root=saves"]).unwrap().unwrap().root,
            Some(PathBuf::from("saves"))
        );
    }

    #[test]
    fn flags_may_follow_root() {
        let o = parse(&["--root", "saves", "--read-only"]).unwrap().unwrap();
        assert!(o.config.read_only);
        assert_eq!(o.root, Some(PathBuf::from("saves")));
    }

    #[test]
    fn help_and_version_stop_parsing() {
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["-V"]).unwrap().is_none());
    }

    #[test]
    fn rejects_unknown_and_incomplete_arguments() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--root"]).is_err());
        assert!(parse(&["--root="]).is_err());
        assert!(parse(&["--editor-bridge"]).is_err());
    }

    #[test]
    fn parses_the_editor_bridge_in_both_forms() {
        let o = parse(&["--editor-bridge", "E:/bridge"]).unwrap().unwrap();
        assert_eq!(o.config.editor_bridge, Some(PathBuf::from("E:/bridge")));

        let o = parse(&["--editor-bridge=E:/bridge/requests"])
            .unwrap()
            .unwrap();
        assert_eq!(
            o.config.editor_bridge,
            Some(PathBuf::from("E:/bridge")),
            "a subdirectory should normalize to the bridge root"
        );

        // Absent by default, so the editor tools explain themselves.
        assert!(parse(&[]).unwrap().unwrap().config.editor_bridge.is_none());
    }
}
