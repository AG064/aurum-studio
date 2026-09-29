//! # aurum-mcp
//!
//! A headless Model Context Protocol server for the Aurum engine.
//!
//! This is layer 3 of the MCP design
//! (`docs/superpowers/specs/2026-09-11-aurum-mcp-design.md`). It drives
//! [`aurum_core`] and [`aurum_space`] directly, with **no Godot process**, and
//! adds **no third-party dependencies** beyond what the workspace already
//! locks. The MCP stdio transport is newline-delimited JSON-RPC 2.0, which is
//! a small enough subset to implement directly rather than pull in an SDK.
//!
//! Because Aurum's authoritative simulation is Rust rather than GDScript, this
//! is the higher-fidelity surface for an AI to drive: typed, deterministic,
//! fast, and runnable in CI.
//!
//! ## Use
//!
//! ```no_run
//! use aurum_mcp::{serve, Engine, PathGuard, ServerConfig};
//!
//! let stdin = std::io::stdin();
//! let mut stdout = std::io::stdout();
//! let mut engine = Engine::new();
//! let paths = PathGuard::new(std::env::current_dir().unwrap());
//! serve(stdin.lock(), &mut stdout, &mut engine, &paths, ServerConfig::default()).unwrap();
//! ```
//!
//! ## Layout
//!
//! - [`protocol`] — JSON-RPC 2.0 / MCP wire types
//! - [`engine`] — the headless session, serialized in the Godot bridge's format
//! - [`tools`] — the `aurum_*` catalog, handlers, and the file path guard
//! - [`server`] — the stdio loop
//! - [`cli`] — argument parsing and the process entry point

pub mod cli;
pub mod connect;
pub mod content_tools;
pub mod editor_tools;
pub mod engine;
pub mod project_tools;
pub mod protocol;
pub mod server;
pub mod tools;

pub use cli::{parse_args_slice, run, Options, USAGE};
pub use engine::{runtime_fingerprint, DynamicEvent, Engine, EngineError};
pub use protocol::{Incoming, RpcError};
pub use server::{serve, ServerConfig};
pub use tools::{catalog, find, list_payload_with, PathGuard, Tool, ToolError};
