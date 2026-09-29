//! Connecting an AI agent to the engine.
//!
//! `aurum mcp` is already a Model Context Protocol server, which means any
//! agent that speaks MCP can drive the engine. What was missing is the step
//! between those two facts: knowing which file each client reads, what shape it
//! wants, and how to write it without trampling the settings already in there.
//!
//! So this does that step. `--print-config` emits the snippet for a client to
//! paste, and `--install` writes it, merging rather than replacing so a
//! configuration that already lists three other servers keeps them.
//!
//! Deliberately not a wizard. Every one of these files is a text file, the
//! command prints what it is about to write, and somebody who would rather do
//! it by hand can read the output and do so.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// A client that can be pointed at the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// OpenAI Codex, which reads TOML.
    Codex,
    /// Claude Code, which reads `.mcp.json` from the project.
    ClaudeCode,
    /// Claude Desktop, which reads a JSON file in the user's roaming profile.
    ClaudeDesktop,
    /// Cursor.
    Cursor,
    /// VS Code, whose schema differs from everyone else's.
    VsCode,
    /// Windsurf.
    Windsurf,
}

impl Client {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Cursor => "cursor",
            Self::VsCode => "vscode",
            Self::Windsurf => "windsurf",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "codex" => Some(Self::Codex),
            "claude" | "claude-code" | "claudecode" => Some(Self::ClaudeCode),
            "claude-desktop" | "claudedesktop" => Some(Self::ClaudeDesktop),
            "cursor" => Some(Self::Cursor),
            "vscode" | "vs-code" | "code" => Some(Self::VsCode),
            "windsurf" => Some(Self::Windsurf),
            _ => None,
        }
    }

    /// Every client this command knows about.
    pub fn all() -> [Self; 6] {
        [
            Self::Codex,
            Self::ClaudeCode,
            Self::ClaudeDesktop,
            Self::Cursor,
            Self::VsCode,
            Self::Windsurf,
        ]
    }

    /// Where the client reads its servers from, if it is installed.
    ///
    /// `None` means the file belongs to a project rather than to the machine,
    /// and there is nowhere sensible to write it without being told.
    pub fn config_path(self, project: &Path) -> Option<PathBuf> {
        let home = home_directory()?;
        Some(match self {
            Self::Codex => std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"))
                .join("config.toml"),
            Self::ClaudeCode => project.join(".mcp.json"),
            Self::ClaudeDesktop => roaming()?.join("Claude").join("claude_desktop_config.json"),
            Self::Cursor => home.join(".cursor").join("mcp.json"),
            Self::VsCode => project.join(".vscode").join("mcp.json"),
            Self::Windsurf => home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
        })
    }
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn roaming() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// How the client should start the server.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub command: String,
    pub args: Vec<String>,
}

impl Invocation {
    /// The command a client should run to reach this engine.
    ///
    /// The current executable rather than the bare name `aurum`, because the
    /// binary a client starts is not started from a shell and does not inherit
    /// a PATH that has been set up in one.
    pub fn current(root: Option<&Path>) -> Self {
        let command = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "aurum".to_string());

        // `aurum mcp` and `aurum-mcp` are the same server behind two entry
        // points, and only one of them takes a subcommand. Writing "mcp" into
        // the argument list for the standalone binary produces a client that
        // starts the server and is told the server does not understand "mcp".
        let standalone = Path::new(&command)
            .file_stem()
            .map(|stem| stem.to_string_lossy().ends_with("aurum-mcp"))
            .unwrap_or(false);

        let mut args = Vec::new();
        if !standalone {
            args.push("mcp".to_string());
        }
        if let Some(root) = root {
            args.push("--root".to_string());
            args.push(root.display().to_string());
        }
        Self { command, args }
    }
}

/// The JSON object that every client but Codex and VS Code expects.
fn mcp_server_entry(invocation: &Invocation) -> Value {
    json!({
        "command": invocation.command,
        "args": invocation.args,
    })
}

/// The snippet for a client, as text, ready to print or paste.
pub fn snippet(client: Client, invocation: &Invocation) -> String {
    match client {
        Client::Codex => {
            let args = invocation
                .args
                .iter()
                .map(|a| format!("\"{}\"", a.replace('\\', "\\\\")))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "[mcp_servers.aurum]\ncommand = \"{}\"\nargs = [{}]\n",
                invocation.command.replace('\\', "\\\\"),
                args
            )
        }
        Client::VsCode => {
            let body = json!({
                "servers": {
                    "aurum": {
                        "type": "stdio",
                        "command": invocation.command,
                        "args": invocation.args,
                    }
                }
            });
            serde_json::to_string_pretty(&body).unwrap_or_default()
        }
        _ => {
            let body = json!({ "mcpServers": { "aurum": mcp_server_entry(invocation) } });
            serde_json::to_string_pretty(&body).unwrap_or_default()
        }
    }
}

/// Write the configuration for a client, merging with what is already there.
///
/// Returns a line describing what happened, for the caller to print. Merging
/// rather than replacing is the whole point: these files hold other people's
/// servers, and a tool that silently dropped them would be run once.
pub fn install(client: Client, project: &Path, invocation: &Invocation) -> Result<String, String> {
    let Some(path) = client.config_path(project) else {
        return Err(format!(
            "cannot work out where {} keeps its configuration on this machine",
            client.name()
        ));
    };

    match client {
        Client::Codex => install_toml(&path, invocation),
        Client::VsCode => install_json(&path, invocation, "servers", true),
        _ => install_json(&path, invocation, "mcpServers", false),
    }
}

fn install_toml(path: &Path, invocation: &Invocation) -> Result<String, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing.contains("[mcp_servers.aurum]") {
        return Ok(format!("{} already lists it", path.display()));
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&snippet(Client::Codex, invocation));
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("wrote {}", path.display()))
}

fn install_json(
    path: &Path,
    invocation: &Invocation,
    key: &str,
    typed: bool,
) -> Result<String, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }

    let mut document: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?,
        _ => Value::Object(Map::new()),
    };

    let Some(root) = document.as_object_mut() else {
        return Err(format!("{} is not a JSON object", path.display()));
    };
    let servers = root
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        return Err(format!("{key} in {} is not an object", path.display()));
    };

    let existed = servers.contains_key("aurum");
    servers.insert(
        "aurum".to_string(),
        if typed {
            json!({
                "type": "stdio",
                "command": invocation.command,
                "args": invocation.args,
            })
        } else {
            mcp_server_entry(invocation)
        },
    );

    let text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
    std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!(
        "{} {}",
        if existed { "updated" } else { "wrote" },
        path.display()
    ))
}

/// The message for a client name that is not one of ours.
pub fn unknown_client(name: &str) -> String {
    let known: Vec<&str> = Client::all().iter().map(|c| c.name()).collect();
    format!("unknown client {name:?}; try one of {}", known.join(", "))
}

/// The prose that goes with the configuration, for `--help`.
pub const CONNECT_HELP: &str = "\
CONNECTING AN AGENT

    aurum mcp                       run the server yourself, on stdin and stdout
    aurum mcp --print-config <client>
    aurum mcp --install <client>[,<client>...]

CLIENTS
    codex              ~/.codex/config.toml
    claude-code        .mcp.json beside the project
    claude-desktop     the Claude Desktop configuration
    cursor             ~/.cursor/mcp.json
    vscode             .vscode/mcp.json
    windsurf           the Windsurf configuration

    `--install` merges into the file rather than replacing it, so any servers
    already listed there survive. `--print-config` writes nothing.

EXAMPLES
    aurum mcp --print-config cursor
    aurum mcp --install codex,claude-code
    aurum mcp --install vscode --project .
";

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation() -> Invocation {
        Invocation {
            command: "aurum".to_string(),
            args: vec!["mcp".to_string(), "--root".to_string(), "A:/p".to_string()],
        }
    }

    #[test]
    fn every_client_name_round_trips() {
        for client in Client::all() {
            assert_eq!(Client::parse(client.name()), Some(client));
        }
        // And the aliases people actually type.
        assert_eq!(Client::parse("Claude"), Some(Client::ClaudeCode));
        assert_eq!(Client::parse("code"), Some(Client::VsCode));
        assert_eq!(Client::parse("nonesuch"), None);
    }

    #[test]
    fn the_codex_snippet_is_toml_with_escaped_windows_paths() {
        let invocation = Invocation {
            command: r"C:\tools\aurum.exe".to_string(),
            args: vec!["mcp".to_string()],
        };
        let text = snippet(Client::Codex, &invocation);
        assert!(text.starts_with("[mcp_servers.aurum]"));
        // A backslash in a TOML basic string starts an escape, so a Windows
        // path written plainly is a parse error rather than a path.
        assert!(text.contains(r"C:\\tools\\aurum.exe"), "{text}");
    }

    #[test]
    fn the_vscode_snippet_uses_the_key_vscode_reads() {
        let text = snippet(Client::VsCode, &invocation());
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert!(parsed["servers"]["aurum"].is_object());
        assert_eq!(parsed["servers"]["aurum"]["type"], "stdio");
        assert!(parsed.get("mcpServers").is_none());
    }

    #[test]
    fn the_common_snippet_is_what_everyone_else_reads() {
        let text = snippet(Client::Cursor, &invocation());
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["mcpServers"]["aurum"]["command"], "aurum");
        assert_eq!(parsed["mcpServers"]["aurum"]["args"][0], "mcp");
    }

    #[test]
    fn installing_keeps_the_servers_that_were_already_there() {
        let dir = std::env::temp_dir().join(format!("aurum-connect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"someone-else":{"command":"other","args":[]}}}"#,
        )
        .unwrap();

        install_json(&path, &invocation(), "mcpServers", false).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            written["mcpServers"]["someone-else"].is_object(),
            "installing ours must not remove theirs"
        );
        assert!(written["mcpServers"]["aurum"].is_object());

        // Running it twice is not an error and does not duplicate anything.
        let message = install_json(&path, &invocation(), "mcpServers", false).unwrap();
        assert!(message.starts_with("updated"), "{message}");
        let again: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(again["mcpServers"].as_object().unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_refuses_a_file_it_cannot_parse_rather_than_replacing_it() {
        let dir = std::env::temp_dir().join(format!("aurum-connect-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let outcome = install_json(&path, &invocation(), "mcpServers", false);
        assert!(outcome.is_err());
        // The original bytes are still there, which is the point: a file with a
        // typo in it is somebody's file, not ours to overwrite.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ this is not json"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
