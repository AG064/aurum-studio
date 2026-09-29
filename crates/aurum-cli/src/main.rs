//! `aurum` — the Aurum command-line entry point.
//!
//! A thin dispatcher. Each subcommand's behaviour lives in the crate that owns
//! it, so the CLI stays an adapter rather than a second implementation. That
//! matters here because the CLI is also the automation surface: if it had its
//! own logic, tests and agents would be exercising something the Studio shell
//! does not use.

mod commands;
mod project_command;

use std::process::ExitCode;

const USAGE: &str = "\
aurum - Aurum Studio and headless project operations

USAGE:
    aurum <COMMAND> [OPTIONS]

COMMANDS:
    project [root]       Headless project operations. Use --request-json with
                          an operation object; see `aurum project --help`.
    doctor [project]      Check a project and report healthy, warning, or
                          blocked state with evidence.
    dev [project]         Watch and develop headlessly. Use --play for a preview
                          or --editor for the optional native editor.
    build [project]       Build the GDExtension and install it. A failed build
                          leaves the installed library untouched.
    editor [project]      Launch Godot on the project, supervised.
    run [project]         Launch the project's game, supervised.
    new <path>            Make a project from a template. Refuses to write
                          into a directory that already has anything in it.
    modules [project]     What the engine ships, what this project turned
                          on, and anything named that does not exist.
    godot [project]       What Godot this machine has, or fetch one with
                          --fetch <url> --sha256 <digest>. Refuses to install
                          a download whose digest does not match.
    presets [project]     What this project can be exported into, read
                          from Godot's own export_presets.cfg.
    stop [project]        Stop the processes this project's session launched.
    restart [project]     Restart the editor, for a native structural change
                          that cannot be migrated live. Asks politely; refuses
                          to start a second editor over one that would not
                          close unless --force is given.
    studio [project]      Start the local Studio shell and open it in a
                          browser. Loopback only; every request needs the
                          session token. See `aurum studio --help`.
    import <path>         Register a project; add aurum.toml when importing
                          a plain Godot project for the first time.
    projects              List registered projects.
    forget <name|path>    Remove a project from the registry.
    mcp                   Run the headless MCP server over stdio, so an AI
                          agent can drive the engine. See `aurum mcp --help`.

OPTIONS:
    -h, --help      Print this help.
    -V, --version   Print the version.

EXAMPLES:
    aurum doctor
    aurum doctor A:/path/to/project --json
    aurum import A:/path/to/project
    aurum mcp --root . --read-only
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("mcp") => ExitCode::from(aurum_mcp::cli::run(&args[1..]) as u8),
        Some("project") => project_command::run(&args[1..]),
        Some("doctor") => commands::doctor(&args[1..]),
        Some("build") => commands::build(&args[1..]),
        Some("dev") => commands::dev(&args[1..]),
        Some("editor") => commands::editor(&args[1..]),
        Some("run") => commands::run(&args[1..]),
        Some("stop") => commands::stop(&args[1..]),
        Some("modules") => commands::modules(&args[1..]),
        Some("godot") => commands::godot(&args[1..]),
        Some("presets") => commands::presets(&args[1..]),
        Some("restart") => commands::restart(&args[1..]),
        Some("new") => commands::new(&args[1..]),
        Some("studio") => commands::studio(&args[1..]),
        Some("import") => commands::import(&args[1..]),
        Some("projects") => commands::projects(&args[1..]),
        Some("forget") => commands::forget(&args[1..]),
        None => commands::studio(&[]),
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") => {
            println!("aurum {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("aurum: unknown command '{other}'\n\n{USAGE}");
            ExitCode::from(64)
        }
    }
}
