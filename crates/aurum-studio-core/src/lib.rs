//! # aurum-studio-core
//!
//! Orchestration for Aurum Studio, with no UI and no Godot dependency.
//!
//! Godot is the workhorse — it stays the editor, renderer, asset pipeline, and
//! runtime. This crate is the part that decides *what to run, when, and whether
//! it worked*: finding the project, checking the toolchain, supervising child
//! processes, building the extension, installing it safely, and classifying a
//! change as a reload, a gameplay restart, or an editor restart.
//!
//! Everything here is testable without a GUI and without Godot installed, which
//! is what makes the CLI and a future window two adapters over one
//! implementation rather than two implementations.
//!
//! ## Modules
//!
//! - [`toml`] — a minimal reader for the `aurum.toml` subset
//! - [`project`] — configuration and read-only project discovery
//! - [`toolchain`] — finding Cargo, rustc, and Godot
//! - [`doctor`] — healthy, warning, and blocked findings, with evidence
//! - [`build`] — building the extension and installing it safely
//! - [`process`] — running children with capture and a bounded wait
//! - [`ownership`] — proving a process is the one Studio launched
//! - [`reload`] — what a change costs the running editor
//! - [`gameplay`] — restarting the game without restarting the editor
//! - [`session`] — session identity, state directory, and logs
//! - [`supervise`] — launching and stopping the processes a session owns
//! - [`supervisor`] — typed commands in, bounded events out, off the caller's thread
//! - [`watch`] — noticing that a project changed
//! - [`hash`] — SHA-256, for verifying artifacts
//!
//! ## Dependency policy
//!
//! This crate adds **no new packages** to the workspace. It uses `serde` and
//! `serde_json`, which were already locked, and nothing else.
//!
//! The TOML reader is written here rather than taken from a crate because a
//! TOML implementation *would* add packages, and the subset `aurum.toml` needs
//! is small enough that a focused reader is both smaller and more explicit
//! about what it refuses.

pub mod build;
pub mod build_queue;
pub mod doctor;
pub mod downloads;
pub mod files;
pub mod gameplay;
pub mod hash;
pub mod modules;
pub mod native_schema;
pub mod ownership;
pub mod presets;
pub mod process;
pub mod project;
pub mod project_ops;
pub mod random;
pub mod registry;
pub mod reload;
pub mod session;
pub mod supervise;
pub mod supervisor;
pub mod templates;
pub mod toml;
pub mod toolchain;
pub mod watch;

pub use build::{BuildError, BuildReport, BuildRequest, Profile};
pub use build_queue::{BuildLock, BuildQueue, LockError, Submission};
pub use doctor::{Finding, Health, Report};
pub use gameplay::{respond, Game, Response, Restart};
pub use hash::{sha256_file, sha256_hex, Sha256};
pub use ownership::{LiveProcess, OwnershipRecord, ProcessKind};
pub use process::{Command, Outcome};
pub use project::{clean_path, ConfigError, Layout, Project, ProjectConfig};
pub use random::Source as RandomSource;
pub use registry::{Entry, Registry, RegistryError};
pub use reload::{classify_all, classify_change, classify_path, Classification, Verdict};
pub use session::{redact, Session};
pub use supervise::{launch, stop_session, terminate, LaunchRequest, StopOutcome};
pub use supervisor::{
    Command as StudioCommand, Event as StudioEvent, Supervisor, SupervisorConfig,
};
pub use toolchain::{Tool, Toolchain};
pub use watch::{Change, ChangeKind, Debouncer, Watcher};
