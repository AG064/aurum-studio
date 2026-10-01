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
    /// A relative hint to the engine checkout, for projects that live beside it.
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
