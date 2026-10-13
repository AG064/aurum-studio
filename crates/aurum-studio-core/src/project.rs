//! The `aurum.toml` configuration and project discovery.
//!
//! An imported project is described by a small portable file at its root.
//! Machine-specific facts — where Godot lives, where the registry keeps state —
//! never enter it, so a project checked into Git behaves the same on every
//! machine (design: "Absolute machine paths and session tokens never enter
//! `aurum.toml`").

use std::path::{Path, PathBuf};

use crate::toml::{Document, ParseError};

/// The configuration schema this build understands.
pub const SCHEMA_VERSION: i64 = 1;

/// Strip Windows' verbatim prefix from an absolute path.
///
/// `canonicalize` returns `\\?\C:\...`, which is an operating-system detail:
/// it leaks into reports, into the registry, and into anything comparing
/// paths as text. Removing it changes nothing about how the path resolves.
pub fn clean_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    path
}

/// Why a configuration could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Io(String),
    Parse(ParseError),
    /// A required field is missing or has the wrong type.
    Field {
        field: &'static str,
        problem: String,
    },
    /// The file was written by a newer Studio.
    UnsupportedSchema(i64),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) => write!(f, "{m}"),
            Self::Parse(e) => write!(f, "aurum.toml: {e}"),
            Self::Field { field, problem } => write!(f, "aurum.toml: '{field}' {problem}"),
            Self::UnsupportedSchema(v) => write!(
                f,
                "aurum.toml declares schema version {v}, but this build understands {SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// The portable half of a project's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConfig {
    pub schema_version: i64,
    pub name: String,
    /// The Godot version this project expects, when it pins one.
    pub godot_version: Option<String>,
    /// The crate that produces the GDExtension.
    pub rust_package: Option<String>,
    /// Where the Aurum add-on lives, relative to the project root.
    pub addon_destination: Option<String>,
    pub modules: Vec<String>,
    /// A relative Cargo workspace hint. Private staging requires it inside the project.
    pub engine_path_hint: Option<String>,
    /// A project-specific validation command, run by `doctor`.
    pub validation_command: Option<String>,
    /// Portable Rust Web build profile. Machine SDK paths stay in the environment.
    pub web_features: Vec<String>,
    pub web_toolchain: Option<String>,
    pub web_preset: Option<String>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            name: String::new(),
            godot_version: None,
            rust_package: None,
            addon_destination: None,
            modules: Vec::new(),
            engine_path_hint: None,
            validation_command: None,
            web_features: Vec::new(),
            web_toolchain: None,
            web_preset: None,
        }
    }
}

impl ProjectConfig {
    /// Read a configuration from TOML text.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let document = Document::parse(text).map_err(ConfigError::Parse)?;

        let schema_version = match document.get("schema_version") {
            Some(value) => value.as_integer().ok_or_else(|| ConfigError::Field {
                field: "schema_version",
                problem: format!("must be an integer, found {}", value.kind()),
            })?,
            // Absent means the first schema, which is the only one that
            // predates the field.
            None => SCHEMA_VERSION,
        };
        if schema_version > SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchema(schema_version));
        }

        let required_string = |field: &'static str| -> Result<String, ConfigError> {
            match document.get(field) {
                Some(value) => {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| ConfigError::Field {
                            field,
                            problem: format!("must be a string, found {}", value.kind()),
                        })
                }
                None => Err(ConfigError::Field {
                    field,
                    problem: "is required".into(),
                }),
            }
        };

        let optional_string =
            |field: &'static str| -> Result<Option<String>, ConfigError> {
                match document.get(field) {
                    Some(value) => value.as_str().map(|s| Some(s.to_string())).ok_or_else(|| {
                        ConfigError::Field {
                            field,
                            problem: format!("must be a string, found {}", value.kind()),
                        }
                    }),
                    None => Ok(None),
                }
            };

        Ok(Self {
            schema_version,
            name: required_string("name")?,
            godot_version: optional_string("godot_version")?,
            rust_package: optional_string("rust_package")?,
            addon_destination: optional_string("addon_destination")?,
            // Read strictly: a wrongly-typed list silently becoming empty is
            // how a project ends up missing modules with no explanation.
            modules: match document.get("modules") {
                Some(value) => {
                    value
                        .as_array()
                        .map(<[String]>::to_vec)
                        .ok_or_else(|| ConfigError::Field {
                            field: "modules",
                            problem: format!("must be an array of strings, found {}", value.kind()),
                        })?
                }
                None => Vec::new(),
            },
            engine_path_hint: optional_string("engine.path_hint")?,
            validation_command: optional_string("validation.command")?,
            web_features: match document.get("web.features") {
                Some(value) => {
                    value
                        .as_array()
                        .map(<[String]>::to_vec)
                        .ok_or_else(|| ConfigError::Field {
                            field: "web.features",
                            problem: "must be an array of strings".into(),
                        })?
                }
                None => Vec::new(),
            },
            web_toolchain: optional_string("web.toolchain")?,
            web_preset: optional_string("web.preset")?,
        })
    }

    /// Read `aurum.toml` from a project root.
    pub fn load(project_root: &Path) -> Result<Self, ConfigError> {
        let path = project_root.join("aurum.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::Io(format!("could not read '{}': {e}", path.display())))?;
        Self::parse(&text)
    }

    /// Render as TOML, so `aurum import` can write a starting point.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str("# Aurum project configuration.\n");
        out.push_str("# Portable: no absolute machine paths belong in this file.\n\n");
        out.push_str(&format!("schema_version = {}\n", self.schema_version));
        out.push_str(&format!("name = \"{}\"\n", escape(&self.name)));
        if let Some(version) = &self.godot_version {
            out.push_str(&format!("godot_version = \"{}\"\n", escape(version)));
        }
        if let Some(package) = &self.rust_package {
            out.push_str(&format!("rust_package = \"{}\"\n", escape(package)));
        }
        if let Some(destination) = &self.addon_destination {
            out.push_str(&format!(
                "addon_destination = \"{}\"\n",
                escape(destination)
            ));
        }
        if !self.modules.is_empty() {
            let items: Vec<String> = self
                .modules
                .iter()
                .map(|m| format!("\"{}\"", escape(m)))
                .collect();
            out.push_str(&format!("modules = [{}]\n", items.join(", ")));
        }
        if self.engine_path_hint.is_some() || self.validation_command.is_some() {
            out.push_str("\n[engine]\n");
            if let Some(hint) = &self.engine_path_hint {
                out.push_str(&format!("path_hint = \"{}\"\n", escape(hint)));
            }
        }
        if let Some(command) = &self.validation_command {
            out.push_str("\n[validation]\n");
            out.push_str(&format!("command = \"{}\"\n", escape(command)));
        }
        if !self.web_features.is_empty()
            || self.web_toolchain.is_some()
            || self.web_preset.is_some()
        {
            out.push_str("\n[web]\n");
            if !self.web_features.is_empty() {
                let values: Vec<_> = self
                    .web_features
                    .iter()
                    .map(|s| format!("\"{}\"", escape(s)))
                    .collect();
                out.push_str(&format!("features = [{}]\n", values.join(", ")));
            }
            if let Some(value) = &self.web_toolchain {
                out.push_str(&format!("toolchain = \"{}\"\n", escape(value)));
            }
            if let Some(value) = &self.web_preset {
                out.push_str(&format!("preset = \"{}\"\n", escape(value)));
            }
        }
        out
    }
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Which parts of a project layout were found on disk.
///
/// Every field is optional because discovery is read-only and must be able to
/// describe a broken project: `doctor` needs to report what is missing, which
/// it cannot do if discovery fails outright.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    pub godot_project: Option<PathBuf>,
    pub cargo_manifest: Option<PathBuf>,
    pub gdextension: Option<PathBuf>,
    pub addon_directory: Option<PathBuf>,
    pub installed_library: Option<PathBuf>,
    pub build_script: Option<PathBuf>,
}

impl Layout {
    /// Everything the project needs before a build can be trusted.
    pub fn is_buildable(&self) -> bool {
        self.cargo_manifest.is_some() && self.godot_project.is_some()
    }
}

/// A discovered project: where it is, how it is configured, and what is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
    pub layout: Layout,
}

impl Project {
    /// Add Aurum configuration when importing an existing Godot project.
    /// Existing configuration is never overwritten.
    pub fn import(root: &Path) -> Result<Self, ConfigError> {
        if !root.join("aurum.toml").exists() {
            let root = root
                .canonicalize()
                .map(clean_path)
                .map_err(|e| ConfigError::Io(e.to_string()))?;
            let config = ProjectConfig {
                name: root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                ..ProjectConfig::default()
            };
            if discover_layout(&root, &config).godot_project.is_none() {
                return Err(ConfigError::Io(
                    "project.godot was not found; create a new project instead".into(),
                ));
            }
            crate::files::write_atomic(&root.join("aurum.toml"), config.to_toml().as_bytes())
                .map_err(|e| ConfigError::Io(e.to_string()))?;
        }
        Self::open(root)
    }
    /// Open a project rooted at `root`.
    ///
    /// The root is canonicalized so later containment checks compare like with
    /// like; a path that does not exist is reported rather than guessed at.
    pub fn open(root: &Path) -> Result<Self, ConfigError> {
        let root = root
            .canonicalize()
            .map(clean_path)
            .map_err(|e| ConfigError::Io(format!("could not open '{}': {e}", root.display())))?;
        let config = ProjectConfig::load(&root)?;
        if let Some(destination) = &config.addon_destination {
            crate::files::confined(&root, destination).map_err(|problem| ConfigError::Field {
                field: "addon_destination",
                problem,
            })?;
        }
        let layout = discover_layout(&root, &config);
        if let Some(path) = &layout.godot_project {
            let canonical = path
                .canonicalize()
                .map(clean_path)
                .map_err(|e| ConfigError::Io(e.to_string()))?;
            if !canonical.starts_with(&root) {
                return Err(ConfigError::Io(
                    "the Godot project resolves outside the imported root".into(),
                ));
            }
        }
        Ok(Self {
            root,
            config,
            layout,
        })
    }

    /// The directory Godot should be pointed at.
    pub fn godot_project_dir(&self) -> Option<&Path> {
        self.layout.godot_project.as_deref().and_then(Path::parent)
    }

    pub fn build_workspace(&self) -> &Path {
        self.layout
            .cargo_manifest
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(&self.root)
    }

    /// Resolve copied Cargo source without rebinding an external checkout.
    /// Root Cargo discovery keeps precedence over a valid contained hint.
    pub(crate) fn confined_build_workspace(&self) -> Result<PathBuf, String> {
        let manifest = |path: &Path| -> Result<PathBuf, String> {
            let relative = path
                .strip_prefix(&self.root)
                .map_err(|_| "Cargo workspace escaped the imported project")?
                .to_str()
                .ok_or("Cargo workspace paths must be UTF-8")?;
            let confined = crate::files::confined(&self.root, relative)?;
            crate::diagnostics::checked_path(path).map_err(|error| error.to_string())?;
            let relative = confined
                .strip_prefix(&self.root)
                .map_err(|_| "Cargo workspace escaped the imported project")?
                .to_str()
                .ok_or("Cargo workspace paths must be UTF-8")?;
            if !crate::snapshot::includes(relative)
                || !std::fs::metadata(&confined)
                    .map_err(|error| error.to_string())?
                    .is_file()
            {
                return Err(
                    "Cargo manifest must be a regular file in copied project source".into(),
                );
            }
            Ok(confined)
        };
        if let Some(hint) = &self.config.engine_path_hint {
            if hint.is_empty() || hint.len() > 4096 || hint.contains(':') {
                return Err(
                    "engine.path_hint must name a relative contained Cargo workspace".into(),
                );
            }
            let directory = crate::files::confined(&self.root, hint)?;
            crate::diagnostics::checked_path(&self.root.join(hint))
                .map_err(|error| error.to_string())?;
            if !directory.is_dir() {
                return Err(
                    "engine.path_hint must name an existing contained Cargo directory".into(),
                );
            }
            manifest(&directory.join("Cargo.toml"))?;
        }
        if discover_layout(&self.root, &self.config).cargo_manifest != self.layout.cargo_manifest {
            return Err(
                "Cargo layout changed since project discovery; reopen before staging".into(),
            );
        }
        match &self.layout.cargo_manifest {
            Some(path) => manifest(path)?
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| "Cargo manifest has no workspace directory".into()),
            None if self.config.rust_package.is_some() => {
                Err("configured Rust project has no contained Cargo manifest".into())
            }
            None => Ok(self.root.clone()),
        }
    }

    /// Resolve the declared host library, including a not-yet-built destination.
    /// This portable subset accepts quoted library paths, not arbitrary Variant
    /// expressions. Private runtime staging additionally uses Godot ConfigFile.
    pub fn library_destination(&self, release: bool) -> Result<PathBuf, String> {
        let manifest = self
            .layout
            .gdextension
            .as_ref()
            .ok_or("configured GDExtension manifest was not found")?;
        self.library_destination_for(manifest, release)
    }
    pub(crate) fn library_destination_for(
        &self,
        manifest: &Path,
        release: bool,
    ) -> Result<PathBuf, String> {
        let mut bytes = Vec::new();
        use std::io::Read;
        std::fs::File::open(manifest)
            .map_err(|error| error.to_string())?
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > 65536 {
            return Err("GDExtension manifest exceeds 64 KiB".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "GDExtension manifest must be UTF-8")?;
        let system = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        let architecture = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86" => "x86_32",
            value => value,
        };
        let profile = if release { "release" } else { "debug" };
        let mut section = "";
        for raw in text.lines() {
            let mut quoted = false;
            let mut escaped = false;
            let mut end = raw.len();
            for (index, character) in raw.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if quoted && character == '\\' {
                    escaped = true;
                    continue;
                }
                if character == '"' {
                    quoted = !quoted;
                }
                if !quoted && character == ';' {
                    end = index;
                    break;
                }
            }
            let line = raw[..end].trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = &line[1..line.len() - 1];
                continue;
            }
            if section != "libraries" {
                continue;
            }
            let (flags, value) = line
                .split_once('=')
                .ok_or("invalid GDExtension library assignment")?;
            let flags = flags.trim();
            let flags = if flags.starts_with('"') {
                serde_json::from_str::<String>(flags)
                    .map_err(|_| "unsupported native feature key")?
            } else {
                flags.into()
            };
            let bits = if usize::BITS == 64 { "64" } else { "32" };
            if !flags.split('.').all(|flag| {
                [system, architecture, profile, "single", bits].contains(&flag)
                    || (!release && flag == "editor")
            }) {
                continue;
            }
            let value = serde_json::from_str::<String>(value.trim())
                .map_err(|_| "native library paths must be quoted strings")?;
            if value.is_empty() || value.contains('\\') || value.chars().any(char::is_control) {
                return Err("invalid native library path".into());
            }
            let candidate = if let Some(relative) = value.strip_prefix("res://") {
                self.godot_project_dir()
                    .ok_or("native manifest requires project.godot")?
                    .join(relative)
            } else {
                if Path::new(&value).is_absolute() || value.contains(':') {
                    return Err("native build destination must stay inside the project".into());
                }
                manifest
                    .parent()
                    .ok_or("manifest parent is unavailable")?
                    .join(value)
            };
            let relative = candidate
                .strip_prefix(&self.root)
                .map_err(|_| "native destination escaped the project")?;
            let mut normalized = PathBuf::new();
            for part in relative.components() {
                match part {
                    std::path::Component::Normal(name) => normalized.push(name),
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        if !normalized.pop() {
                            return Err("native destination escaped the project".into());
                        }
                    }
                    _ => return Err("invalid native destination components".into()),
                }
            }
            let result = crate::files::confined(
                &self.root,
                normalized
                    .to_str()
                    .ok_or("native destination must be UTF-8")?,
            )?;
            crate::diagnostics::checked_path(&result).map_err(|error| error.to_string())?;
            if !result
                .extension()
                .is_some_and(|extension| extension == crate::build::dynamic_library_extension())
            {
                return Err("native manifest target is not a supported host library file; framework and cross-target packaging require separate staging".into());
            }
            return Ok(result);
        }
        Err(format!(
            "native manifest has no supported {system}.{profile}.{architecture} library mapping"
        ))
    }
}

/// Find the parts of a project, read-only.
///
/// Deliberately tolerant: a missing file is recorded as `None` so `doctor` can
/// explain what is wrong, rather than making discovery itself the failure.
pub fn discover_layout(root: &Path, config: &ProjectConfig) -> Layout {
    let mut layout = Layout::default();

    // `project.godot` may sit at the root or in a subdirectory (the Aurum
    // engine checkout keeps it under godot/).
    let direct = root.join("project.godot");
    if direct.is_file() {
        layout.godot_project = Some(direct);
    } else if let Ok(entries) = std::fs::read_dir(root) {
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("project.godot"))
            .filter(|path| path.is_file())
            .collect();
        candidates.sort();
        layout.godot_project = candidates.into_iter().next();
    }

    let cargo = root.join("Cargo.toml");
    if cargo.is_file() {
        layout.cargo_manifest = Some(cargo);
    } else if let Some(hint) = &config.engine_path_hint {
        let cargo = root.join(hint).join("Cargo.toml");
        if cargo.is_file() {
            layout.cargo_manifest = Some(cargo);
        }
    }

    // The add-on directory is configured, else looked for in the usual places.
    let addon_candidates: Vec<PathBuf> = match &config.addon_destination {
        Some(destination) => vec![root.join(destination)],
        None => vec![root.join("godot/addons/aurum"), root.join("addons/aurum")],
    };
    layout.addon_directory = addon_candidates.into_iter().find(|path| path.is_dir());

    // The GDExtension manifest lives in the add-on's bin directory.
    if let Some(addon) = &layout.addon_directory {
        let mut manifests: Vec<PathBuf> = std::fs::read_dir(addon.join("bin"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|e| e == "gdextension"))
                    .collect()
            })
            .unwrap_or_default();
        manifests.sort();
        layout.gdextension = manifests.into_iter().next();

        // The installed library is whichever file the manifest names.
        if let Some(manifest) = &layout.gdextension {
            layout.installed_library = installed_library(manifest, addon);
        }
    }

    for candidate in ["scripts/build.ps1", "scripts/dev.ps1"] {
        let path = root.join(candidate);
        if path.is_file() {
            layout.build_script = Some(path);
            break;
        }
    }

    layout
}

/// The library path a `.gdextension` manifest maps for the current platform.
///
/// Only the debug mapping is read: Studio drives development builds, and the
/// release mapping is a packaging concern.
fn installed_library(manifest: &Path, addon: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(manifest).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().starts_with("windows.debug") {
            continue;
        }
        let relative = value.trim().trim_matches('"');
        if relative.is_empty() {
            continue;
        }
        // The manifest path is res://-relative to the Godot project, and the
        // add-on directory is where we found it, so resolve against that.
        let file = relative.rsplit('/').next().unwrap_or(relative);
        let candidate = addon.join("bin").join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aurum-project-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const VALID: &str = r#"
schema_version = 1
name = "aurum-engine"
godot_version = "4.7"
rust_package = "aurum-godot"
addon_destination = "godot/addons/aurum"
modules = ["aurum-2d", "aurum-space"]

[engine]
path_hint = "."

[validation]
command = "pwsh scripts/tests/phase0_contract.ps1"
"#;

    #[test]
    fn build_destination_uses_ordered_host_mapping_and_preserves_relative_paths() {
        let root = temp_dir("manifest-destination");
        std::fs::write(
            root.join("aurum.toml"),
            "schema_version=1\nname=\"manifest\"\naddon_destination=\"addons/native\"\n",
        )
        .unwrap();
        std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
        let bin = root.join("addons/native/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let manifest = bin.join("game.gdextension");
        let system = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            std::env::consts::ARCH
        };
        let extension = crate::build::dynamic_library_extension();
        std::fs::write(&manifest,format!("[libraries]\nweb.debug=\"res://unrelated.wasm\"\n{system}.debug.{arch}=\"../runtime/custom.{extension}\" ; host mapping\n{system}.release.{arch}=\"res://release/packaged.{extension}\"\n")).unwrap();
        let project = Project::open(&root).unwrap();
        assert_eq!(
            project.library_destination(false).unwrap(),
            project
                .root
                .join(format!("addons/native/runtime/custom.{extension}"))
        );
        assert_eq!(
            project.library_destination(true).unwrap(),
            project.root.join(format!("release/packaged.{extension}"))
        );
        std::fs::write(
            &manifest,
            format!("[libraries]\n{system}.debug.{arch}=\"../../../../outside.{extension}\"\n"),
        )
        .unwrap();
        assert!(project.library_destination(false).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_a_complete_configuration() {
        let config = ProjectConfig::parse(VALID).unwrap();
        assert_eq!(config.schema_version, 1);
        assert_eq!(config.name, "aurum-engine");
        assert_eq!(config.godot_version.as_deref(), Some("4.7"));
        assert_eq!(config.rust_package.as_deref(), Some("aurum-godot"));
        assert_eq!(
            config.addon_destination.as_deref(),
            Some("godot/addons/aurum")
        );
        assert_eq!(config.modules, ["aurum-2d", "aurum-space"]);
        assert_eq!(config.engine_path_hint.as_deref(), Some("."));
        assert_eq!(
            config.validation_command.as_deref(),
            Some("pwsh scripts/tests/phase0_contract.ps1")
        );
    }

    #[test]
    fn a_minimal_configuration_is_enough() {
        let config = ProjectConfig::parse("name = \"tiny\"\n").unwrap();
        assert_eq!(config.name, "tiny");
        assert_eq!(config.schema_version, SCHEMA_VERSION);
        assert!(config.godot_version.is_none());
        assert!(config.modules.is_empty());
    }

    #[test]
    fn a_name_is_required() {
        let error = ProjectConfig::parse("schema_version = 1\n").unwrap_err();
        assert!(matches!(error, ConfigError::Field { field: "name", .. }));
    }

    #[test]
    fn web_profiles_round_trip_without_machine_paths() {
        let config = ProjectConfig::parse("name = \"web-game\"\n[web]\nfeatures = [\"godot/experimental-wasm\", \"nothreads\"]\ntoolchain = \"nightly-2026-09-30\"\npreset = \"Browser\"\n").unwrap();
        assert_eq!(ProjectConfig::parse(&config.to_toml()).unwrap(), config);
        assert!(ProjectConfig::parse("name = \"bad\"\n[web]\nfeatures = true\n").is_err());
    }

    #[test]
    fn wrong_types_name_the_field_and_what_was_found() {
        let error = ProjectConfig::parse("name = 5\n").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("'name'"), "{message}");
        assert!(message.contains("integer"), "{message}");

        // A wrongly-typed list must be reported, not silently emptied.
        let error = ProjectConfig::parse("name = \"x\"\nmodules = 3\n").unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Field {
                    field: "modules",
                    ..
                }
            ),
            "got {error:?}"
        );

        let error = ProjectConfig::parse("name = true\n").unwrap_err();
        assert!(error.to_string().contains("boolean"), "{error}");
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_guessed_at() {
        let error = ProjectConfig::parse("schema_version = 99\nname = \"x\"\n").unwrap_err();
        assert_eq!(error, ConfigError::UnsupportedSchema(99));
        assert!(error.to_string().contains("99"));
    }

    #[test]
    fn toml_output_round_trips() {
        let original = ProjectConfig::parse(VALID).unwrap();
        let rendered = original.to_toml();
        let reparsed = ProjectConfig::parse(&rendered).unwrap();
        assert_eq!(
            original, reparsed,
            "rendered TOML did not round trip:\n{rendered}"
        );
    }

    #[test]
    fn toml_output_escapes_quotes_and_backslashes() {
        let config = ProjectConfig {
            name: "a\"b\\c".into(),
            ..Default::default()
        };
        let rendered = config.to_toml();
        let reparsed = ProjectConfig::parse(&rendered).unwrap();
        assert_eq!(reparsed.name, "a\"b\\c");
    }

    #[test]
    fn discovery_finds_a_project_at_the_root() {
        let root = temp_dir("flat");
        std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

        let layout = discover_layout(&root, &ProjectConfig::default());
        assert_eq!(layout.godot_project, Some(root.join("project.godot")));
        assert_eq!(layout.cargo_manifest, Some(root.join("Cargo.toml")));
        assert!(layout.is_buildable());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_finds_a_project_in_a_subdirectory() {
        let root = temp_dir("nested");
        std::fs::create_dir_all(root.join("godot")).unwrap();
        std::fs::write(root.join("godot/project.godot"), "config_version=5\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();

        let layout = discover_layout(&root, &ProjectConfig::default());
        assert_eq!(layout.godot_project, Some(root.join("godot/project.godot")));
        assert_eq!(
            Project {
                root: root.clone(),
                config: ProjectConfig::default(),
                layout: layout.clone()
            }
            .godot_project_dir(),
            Some(root.join("godot").as_path())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn contained_cargo_hints_preserve_discovery_and_private_copy_mapping() {
        let root = temp_dir("contained-cargo-hint");
        let copied = temp_dir("contained-cargo-hint-copy");
        std::fs::create_dir(root.join("rust")).unwrap();
        std::fs::write(root.join("rust/Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
        let config = ProjectConfig {
            rust_package: Some("nested-game".into()),
            engine_path_hint: Some("rust".into()),
            ..Default::default()
        };
        std::fs::write(root.join("aurum.toml"), config.to_toml()).unwrap();
        let project = Project::open(&root).unwrap();
        assert_eq!(
            project.confined_build_workspace().unwrap(),
            project.root.join("rust")
        );
        crate::snapshot::copy(&project.root, &copied).unwrap();
        let private = Project::open(&copied).unwrap();
        assert_eq!(
            private.confined_build_workspace().unwrap(),
            private.root.join("rust")
        );
        assert_eq!(
            project
                .confined_build_workspace()
                .unwrap()
                .strip_prefix(&project.root)
                .unwrap(),
            private
                .confined_build_workspace()
                .unwrap()
                .strip_prefix(&private.root)
                .unwrap()
        );
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        assert!(project.confined_build_workspace().is_err());
        let rediscovered = Project::open(&root).unwrap();
        assert_eq!(
            rediscovered.confined_build_workspace().unwrap(),
            rediscovered.root
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(copied).unwrap();
    }

    #[test]
    fn invalid_hints_are_refused_even_when_root_cargo_has_discovery_precedence() {
        let root = temp_dir("invalid-cargo-hints");
        let outside = temp_dir("outside-cargo-hint");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(outside.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(root.join("target/workspace")).unwrap();
        std::fs::write(root.join("target/workspace/Cargo.toml"), "[workspace]\n").unwrap();
        for hint in [
            String::new(),
            "missing".into(),
            format!("../{}", outside.file_name().unwrap().to_str().unwrap()),
            outside.to_string_lossy().into_owned(),
            "res://target/workspace".into(),
            "target/workspace".into(),
        ] {
            let config = ProjectConfig {
                rust_package: Some("nested-game".into()),
                engine_path_hint: Some(hint),
                ..Default::default()
            };
            std::fs::write(root.join("aurum.toml"), config.to_toml()).unwrap();
            let project = Project::open(&root).unwrap();
            assert_eq!(project.build_workspace(), project.root);
            assert!(project.confined_build_workspace().is_err());
        }
        assert_eq!(
            std::fs::read(outside.join("Cargo.toml")).unwrap(),
            b"[workspace]\n"
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn cargo_hint_links_cannot_rebind_an_external_workspace() {
        let root = temp_dir("linked-cargo-hint");
        let outside = temp_dir("external-linked-cargo");
        std::fs::write(outside.join("Cargo.toml"), "[workspace]\n").unwrap();
        let config = ProjectConfig {
            rust_package: Some("nested-game".into()),
            engine_path_hint: Some("linked".into()),
            ..Default::default()
        };
        std::fs::write(root.join("aurum.toml"), config.to_toml()).unwrap();
        let linked = root.join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let status = std::process::Command::new("cmd")
                .creation_flags(0x08000000)
                .args(["/C", "mklink", "/J"])
                .arg(&linked)
                .arg(&outside)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        }
        let project = Project::open(&root).unwrap();
        assert!(project.confined_build_workspace().is_err());
        assert_eq!(
            std::fs::read(outside.join("Cargo.toml")).unwrap(),
            b"[workspace]\n"
        );
        #[cfg(unix)]
        std::fs::remove_file(linked).unwrap();
        #[cfg(windows)]
        std::fs::remove_dir(linked).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn script_projects_without_a_hint_need_no_cargo_workspace() {
        let root = temp_dir("script-without-cargo");
        std::fs::write(root.join("aurum.toml"), ProjectConfig::default().to_toml()).unwrap();
        let project = Project::open(&root).unwrap();
        assert_eq!(project.confined_build_workspace().unwrap(), project.root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_reads_the_installed_library_from_the_manifest() {
        let root = temp_dir("manifest");
        let bin = root.join("godot/addons/aurum/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("aurum.gdextension"),
            "[configuration]\nentry_symbol = \"gdext_rust_init\"\n\n[libraries]\n\
             windows.debug.x86_64 = \"res://addons/aurum/bin/aurum_godot.debug.dll\"\n\
             windows.release.x86_64 = \"res://addons/aurum/bin/aurum_godot.dll\"\n",
        )
        .unwrap();

        let layout = discover_layout(&root, &ProjectConfig::default());
        assert!(layout.gdextension.is_some());
        // The debug library is the one that matters for development, and it is
        // only reported when it actually exists.
        assert_eq!(layout.installed_library, None, "not built yet");

        std::fs::write(bin.join("aurum_godot.debug.dll"), b"stub").unwrap();
        let layout = discover_layout(&root, &ProjectConfig::default());
        assert_eq!(
            layout.installed_library,
            Some(bin.join("aurum_godot.debug.dll"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_reports_nothing_rather_than_failing_on_an_empty_directory() {
        let root = temp_dir("empty");
        let layout = discover_layout(&root, &ProjectConfig::default());
        assert_eq!(layout, Layout::default());
        assert!(!layout.is_buildable());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clean_path_removes_the_windows_verbatim_prefix() {
        assert_eq!(
            clean_path(PathBuf::from(r"\\?\C:\projects\aurum")),
            PathBuf::from(r"C:\projects\aurum")
        );
        // A UNC share keeps its leading double backslash.
        assert_eq!(
            clean_path(PathBuf::from(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
        // Anything else is untouched, including ordinary paths.
        assert_eq!(
            clean_path(PathBuf::from(r"C:\plain")),
            PathBuf::from(r"C:\plain")
        );
        assert_eq!(
            clean_path(PathBuf::from("/unix/path")),
            PathBuf::from("/unix/path")
        );
    }

    #[test]
    fn opening_a_missing_directory_is_an_error() {
        let error = Project::open(Path::new("definitely-not-here")).unwrap_err();
        assert!(matches!(error, ConfigError::Io(_)));
    }
}
