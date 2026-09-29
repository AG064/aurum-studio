//! New projects, from a template.
//!
//! The first thing anybody does with a tool is make something with it, and
//! before this the answer was a paragraph in a document. A template is the
//! same paragraph made executable: the layout it describes is the layout that
//! gets written, so the two cannot drift.
//!
//! ## The one rule that matters
//!
//! Nothing here writes into a directory that already has anything in it. Not
//! "asks first" — refuses, and says which file it found. A scaffolding command
//! is run once, at the start of a project's life, usually by somebody who has
//! not yet got a backup and often against a path they mistyped. The cost of
//! refusing wrongly is a second attempt; the cost of the other mistake is
//! somebody's work.
//!
//! A directory that exists and is empty is accepted, because that is what
//! `mkdir` leaves behind and refusing it would be pedantry rather than safety.

use std::path::{Path, PathBuf};

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// The target exists and has something in it. The string names what.
    NotEmpty { path: PathBuf, found: String },
    /// The target names something that is not a directory.
    NotADirectory(PathBuf),
    /// The project name cannot be used as a crate name.
    BadName(String),
    /// Writing failed.
    Io(String),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEmpty { path, found } => write!(
                f,
                "'{}' already contains {found}; refusing to write a project into it",
                path.display()
            ),
            Self::NotADirectory(path) => {
                write!(f, "'{}' exists and is not a directory", path.display())
            }
            Self::BadName(name) => write!(
                f,
                "'{name}' cannot be a project name; use letters, digits, '-' or '_', \
                 starting with a letter"
            ),
            Self::Io(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for TemplateError {}

/// A kind of project that can be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
}

/// Every template, in the order they are offered.
///
/// One, deliberately. A second template is a promise to keep two layouts
/// current, and the honest thing is to add it when somebody actually wants it
/// rather than to ship a menu of guesses.
pub const TEMPLATES: &[Template] = &[
    Template {
        name: "3d",
        description: "a ready-to-run 3D project with a camera, light, and rotating cube",
    },
    Template {
        name: "2d",
        description: "a ready-to-run 2D project with a rotating square",
    },
    Template {
        name: "minimal",
        description: "an advanced native Rust extension skeleton",
    },
];

/// The default when none is named.
pub const DEFAULT_TEMPLATE: &str = "minimal";

/// What to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProject {
    /// The project name, which is the crate name and Godot's project name.
    pub name: String,
    /// Which template.
    pub template: String,
    /// Where the Aurum engine checkout is, which the generated workspace
    /// refers to by path.
    pub engine: PathBuf,
}

/// What was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub root: PathBuf,
    pub name: String,
    pub template: &'static str,
    /// Every file written, relative to the root, in the order written.
    pub files: Vec<PathBuf>,
}

impl Created {
    /// A line per file, for a person.
    pub fn describe(&self) -> String {
        format!(
            "created {} ({}) with {} file{}",
            self.name,
            self.template,
            self.files.len(),
            if self.files.len() == 1 { "" } else { "s" }
        )
    }
}

/// Whether a name can be a project name.
///
/// The name becomes a crate name and a Godot project name, so it is held to
/// what both accept rather than to what one of them happens to tolerate.
pub fn name_is_valid(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    characters.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Look a template up by name.
pub fn find(template: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|entry| entry.name == template)
}

/// Make a project.
///
/// See the module documentation for the rule about existing directories: this
/// is the function that enforces it, and it checks before writing anything at
/// all rather than part-way through.
pub fn create(root: &Path, request: &NewProject) -> Result<Created, TemplateError> {
    if !name_is_valid(&request.name) {
        return Err(TemplateError::BadName(request.name.clone()));
    }
    let template =
        find(&request.template).ok_or_else(|| TemplateError::BadName(request.template.clone()))?;

    // Checked first, and completely, so a refusal costs nothing and leaves
    // nothing behind.
    match std::fs::read_dir(root) {
        Ok(mut entries) => {
            if let Some(entry) = entries.next() {
                let entry = entry.map_err(|e| TemplateError::Io(e.to_string()))?;
                return Err(TemplateError::NotEmpty {
                    path: root.to_path_buf(),
                    found: entry.file_name().to_string_lossy().to_string(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
            return Err(TemplateError::NotADirectory(root.to_path_buf()));
        }
        Err(error) => return Err(TemplateError::Io(error.to_string())),
    }

    if template.name != "minimal" {
        let scene = if template.name == "2d" {
            include_str!("template_2d.tscn")
        } else {
            include_str!("template_3d.tscn")
        };
        let script = if template.name == "2d" {
            "extends Node2D\n\nfunc _process(delta: float) -> void:\n    $Square.rotation += delta * 0.35\n"
        } else {
            "extends Node3D\n\nfunc _process(delta: float) -> void:\n    $Cube.rotation.y += delta * 0.35\n"
        };
        let files = [
            ("aurum.toml", format!("schema_version = 1\nname = {:?}\ngodot_version = \"4.7\"\nmodules = []\n", request.name)),
            ("godot/project.godot",format!("config_version=5\n\n[application]\nconfig/name={:?}\nrun/main_scene=\"res://main.tscn\"\n\n[display]\nwindow/size/viewport_width=1152\nwindow/size/viewport_height=648\n\n[rendering]\nrenderer/rendering_method=\"gl_compatibility\"\n",request.name)),
            ("godot/main.tscn",scene.to_string()),
            ("godot/main.gd",script.to_string()),
            (".gitignore",".aurum/\n**/.godot/\ntarget/\n".into()),
            ("README.md",format!("# {}\n\nOpen with `aurum studio .`. Agents connect with `aurum mcp --root . --tools studio`.\n",request.name)),
        ];
        let mut written = Vec::new();
        for (relative, text) in files {
            let path = root.join(relative);
            crate::files::write_atomic(&path, text.as_bytes())
                .map_err(|e| TemplateError::Io(e.to_string()))?;
            written.push(PathBuf::from(relative));
        }
        return Ok(Created {
            root: root.to_path_buf(),
            name: request.name.clone(),
            template: template.name,
            files: written,
        });
    }
    let crate_name = request.name.replace('-', "_");
    let mut files: Vec<(String, String)> = render(&request.name, &crate_name, &request.engine)
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents))
        // The crate itself, whose paths depend on the name and so cannot sit in
        // the fixed list above.
        .chain(render_crate(&request.name, &crate_name))
        .collect();

    let mut written = Vec::with_capacity(files.len());
    for (relative, contents) in files.drain(..) {
        let path = root.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TemplateError::Io(format!("'{}': {e}", parent.display())))?;
        }
        std::fs::write(&path, contents)
            .map_err(|e| TemplateError::Io(format!("'{}': {e}", path.display())))?;
        written.push(PathBuf::from(relative));
    }

    Ok(Created {
        root: root.to_path_buf(),
        name: request.name.clone(),
        template: template.name,
        files: written,
    })
}

/// The files a minimal project is made of.
///
/// Rendered rather than copied from a directory of samples, so a template is
/// one readable function rather than a tree to keep in step with it.
fn render(name: &str, crate_name: &str, engine: &Path) -> Vec<(&'static str, String)> {
    // Written with forward slashes whatever the platform. A Windows path in a
    // TOML string is a sequence of escapes, and `\R` is not one of them — the
    // reader refuses it, correctly, and the project it just made will not
    // open. Rust and Godot both accept forward slashes on Windows, so this
    // costs nothing and removes a whole class of failure.
    let engine = engine.display().to_string().replace('\\', "/");

    vec![
        (
            "aurum.toml",
            format!(
                "# Read by `aurum doctor`, `build`, `dev`, and `studio`.\n\
                 schema_version = 1\n\
                 name = \"{name}\"\n\
                 godot_version = \"4.7\"\n\
                 rust_package = \"{crate_name}\"\n\
                 addon_destination = \"godot/addons/aurum\"\n\
                 modules = []\n\n\
                 [engine]\n\
                 # Where the Aurum engine checkout lives. Everything the project\n\
                 # needs from the engine is reached through this path.\n\
                 path_hint = \"{engine}\"\n"
            ),
        ),
        (
            "Cargo.toml",
            format!(
                "[workspace]\n\
                 resolver = \"2\"\n\
                 members = [\"crates/{crate_name}\"]\n\n\
                 [profile.release]\n\
                 opt-level = 3\n\
                 lto = \"thin\"\n\
                 codegen-units = 1\n"
            ),
        ),
        (
            "godot/project.godot",
            format!(
                "; Generated by `aurum new`. Edit freely; nothing regenerates it.\n\
                 config_version=5\n\n\
                 [application]\n\n\
                 config/name=\"{name}\"\n\
                 run/main_scene=\"res://main.tscn\"\n\
                 config/features=PackedStringArray(\"4.7\")\n\n\
                 [autoload]\n\n\
                 Aurum=\"*res://scripts/aurum_runtime.gd\"\n\n\
                 [editor_plugins]\n\n\
                 enabled=PackedStringArray(\"res://addons/aurum/plugin.cfg\")\n"
            ),
        ),
        (
            "godot/scripts/aurum_runtime.gd",
            "# The handle a scene uses to reach the engine.\n\
             #\n\
             # The autoload exists so scripts never look the node up by path:\n\
             # moving it in the scene tree cannot break them.\n\
             extends Node\n"
                .to_string(),
        ),
        (".gitignore", "target/\n**/.godot/\n.aurum/\ndist/\n*.tmp\n".to_string()),
        ("godot/main.tscn", "[gd_scene format=3]\n[node name=\"Main\" type=\"Node\"]\n".into()),
        ("godot/addons/aurum/bin/aurum.gdextension", format!("[configuration]\nentry_symbol=\"gdext_rust_init\"\ncompatibility_minimum=\"4.7\"\nreloadable=true\n[libraries]\nwindows.debug.x86_64=\"res://addons/aurum/bin/{crate_name}.debug.dll\"\nwindows.release.x86_64=\"res://addons/aurum/bin/{crate_name}.dll\"\nlinux.debug.x86_64=\"res://addons/aurum/bin/{crate_name}.debug.so\"\nlinux.release.x86_64=\"res://addons/aurum/bin/{crate_name}.so\"\nmacos.debug=\"res://addons/aurum/bin/{crate_name}.debug.dylib\"\nmacos.release=\"res://addons/aurum/bin/{crate_name}.dylib\"\n")),
        ("godot/addons/aurum/plugin.cfg", "[plugin]\nname=\"Aurum reload\"\ndescription=\"Native reload notifications\"\nauthor=\"Aurum\"\nversion=\"1\"\nscript=\"plugin.gd\"\n".into()),
        ("godot/addons/aurum/plugin.gd", format!("@tool\nextends EditorPlugin\nvar previous := \"\"\nfunc _process(_delta: float) -> void:\n    var path := \"res://.godot/aurum/{crate_name}.debug.reload\"\n    if not FileAccess.file_exists(path):\n        return\n    var value := FileAccess.get_file_as_string(path).strip_edges()\n    if value == previous:\n        return\n    var first := previous.is_empty()\n    previous = value\n    if not first:\n        var result := GDExtensionManager.reload_extension(\"res://addons/aurum/bin/aurum.gdextension\")\n        if result != OK:\n            push_warning(\"Aurum native reload requires an editor restart\")\n")),
        (
            "README.md",
            format!(
                "# {name}\n\n\
                 Made with `aurum new`.\n\n\
                 ```\n\
                 aurum doctor     # check the project and the toolchain\n\
                 aurum dev        # build, launch the editor, rebuild on changes\n\
                 aurum studio     # the same thing with a window on it\n\
                 ```\n"
            ),
        ),
    ]
}

/// The crate the extension is built from.
///
/// Separate from the fixed list only because its paths carry the project name.
fn render_crate(_name: &str, crate_name: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("crates/{crate_name}/Cargo.toml"),
            format!(
                "[package]\n\
                 name = \"{crate_name}\"\n\
                 version = \"0.1.0\"\n\
                 edition = \"2021\"\n\n\
                 [lib]\n\
                 crate-type = [\"cdylib\"]\n\n\
                 [dependencies]\n\
                 # The engine checkout named in aurum.toml supplies Godot's\n\
                 # Rust bindings, so a project does not carry its own copy.\n\
                 godot = {{ git = \"https://github.com/godot-rust/gdext\", \
                 branch = \"master\", features = [\"api-4-7\"] }}\n"
            ),
        ),
        (
            format!("crates/{crate_name}/src/lib.rs"),
            format!(
                "//! The {crate_name} extension.\n\
                 //!\n\
                 //! Add classes here and register them the way `aurum-godot` does.\n\n\
                 use godot::init::{{gdextension, ExtensionLibrary, InitLevel}};\n\
                 struct Extension;\n\n\
                 #[gdextension]\n\
                 unsafe impl ExtensionLibrary for Extension {{\n\
                 \x20   fn min_level() -> InitLevel {{\n\
                 \x20       InitLevel::Scene\n\
                 \x20   }}\n\
                 }}\n"
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_root(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("aurum-template-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn request(name: &str) -> NewProject {
        NewProject {
            name: name.to_string(),
            template: DEFAULT_TEMPLATE.to_string(),
            engine: PathBuf::from("E:/aurum-engine"),
        }
    }

    #[test]
    fn a_project_is_created_where_it_was_asked_for() {
        let root = unique_root("create");
        let created = create(&root, &request("my-game")).expect("create");

        assert_eq!(created.name, "my-game");
        assert_eq!(created.template, "minimal");
        assert!(!created.files.is_empty());
        for relative in &created.files {
            assert!(
                root.join(relative).is_file(),
                "'{}' should have been written",
                relative.display()
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_configuration_it_writes_names_the_project() {
        let root = unique_root("config");
        create(&root, &request("my-game")).expect("create");

        let config = std::fs::read_to_string(root.join("aurum.toml")).expect("read");
        assert!(config.contains("name = \"my-game\""));
        assert!(
            config.contains("E:/aurum-engine"),
            "the engine path should be recorded, got:\n{config}"
        );

        // And the written configuration is one the rest of the toolchain
        // accepts, rather than merely looking plausible.
        let project = crate::Project::open(&root).expect("the new project should open");
        assert_eq!(project.config.name, "my-game");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_non_empty_directory_is_refused_and_named() {
        // The rule the module exists to keep. The refusal names what it found,
        // because "directory not empty" leaves somebody hunting.
        let root = unique_root("notempty");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("precious.txt"), "work I care about").expect("write");

        match create(&root, &request("my-game")) {
            Err(TemplateError::NotEmpty { found, .. }) => assert_eq!(found, "precious.txt"),
            other => panic!("expected a refusal, got {other:?}"),
        }

        // And nothing was written, which is the point of checking first.
        assert_eq!(
            std::fs::read_to_string(root.join("precious.txt")).expect("read"),
            "work I care about"
        );
        assert!(!root.join("aurum.toml").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_empty_directory_is_accepted() {
        // What `mkdir` leaves behind. Refusing it would be pedantry, not
        // safety.
        let root = unique_root("empty");
        std::fs::create_dir_all(&root).expect("mkdir");
        assert!(create(&root, &request("my-game")).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_directory_is_created() {
        let root = unique_root("missing");
        assert!(!root.exists());
        assert!(create(&root, &request("my-game")).is_ok());
        assert!(root.join("aurum.toml").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_in_the_way_is_refused_rather_than_replaced() {
        let root = unique_root("file");
        std::fs::create_dir_all(root.parent().unwrap()).expect("mkdir");
        std::fs::write(&root, "not a directory").expect("write");

        let error = create(&root, &request("my-game")).unwrap_err();
        assert!(
            matches!(
                error,
                TemplateError::NotADirectory(_) | TemplateError::NotEmpty { .. }
            ),
            "expected a refusal, got {error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&root).expect("read"),
            "not a directory"
        );
        let _ = std::fs::remove_file(&root);
    }

    #[test]
    fn a_name_that_is_not_a_crate_name_is_refused() {
        let root = unique_root("badname");
        for bad in ["", "1st", "-leading", "has space", "has/slash", "has.dot"] {
            assert!(
                !name_is_valid(bad),
                "'{bad}' should not be accepted as a project name"
            );
            assert!(
                matches!(create(&root, &request(bad)), Err(TemplateError::BadName(_))),
                "'{bad}' should be refused before anything is written"
            );
        }
        assert!(
            !root.exists(),
            "a refusal must not leave a directory behind"
        );
    }

    #[test]
    fn names_that_are_valid_are_accepted() {
        for good in ["a", "my-game", "my_game", "Game2", "x9"] {
            assert!(name_is_valid(good), "'{good}' should be accepted");
        }
    }

    #[test]
    fn every_offered_template_can_actually_be_made() {
        // A template in the list that cannot be built is a menu entry that
        // fails when chosen.
        for template in TEMPLATES {
            let root = unique_root(template.name);
            let mut request = request("probe");
            request.template = template.name.to_string();
            assert!(
                create(&root, &request).is_ok(),
                "template '{}' should be usable",
                template.name
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn an_unknown_template_is_refused() {
        let root = unique_root("unknown");
        let mut request = request("probe");
        request.template = "nonexistent".to_string();
        assert!(create(&root, &request).is_err());
        assert!(!root.exists());
    }

    #[test]
    fn the_default_template_exists() {
        // The refusal path names it, so a default that is not in the list
        // would be a broken message rather than a broken feature.
        assert!(find(DEFAULT_TEMPLATE).is_some());
    }

    #[test]
    fn a_windows_engine_path_still_produces_a_readable_project() {
        // The bug this exists for: a raw Windows path in a TOML string is a
        // sequence of escapes, `\R` is not one of them, and the project the
        // command had just made would not open. doctor found it; this stops it
        // coming back.
        let root = unique_root("windowspath");
        let mut request = request("my-game");
        request.engine = PathBuf::from(r"C:\Tools\aurum-engine");
        create(&root, &request).expect("create");

        let config = std::fs::read_to_string(root.join("aurum.toml")).expect("read");
        assert!(
            !config.contains(r"C:\Tools"),
            "a backslash path would not parse, got:\n{config}"
        );
        assert!(config.contains("C:/Tools/aurum-engine"));

        let project = crate::Project::open(&root).expect("the project must open");
        assert_eq!(
            project.config.engine_path_hint.as_deref(),
            Some("C:/Tools/aurum-engine")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refusal_explains_itself() {
        let not_empty = TemplateError::NotEmpty {
            path: PathBuf::from("E:/thing"),
            found: "notes.txt".into(),
        };
        assert!(not_empty.to_string().contains("notes.txt"));
        assert!(TemplateError::BadName("1st".into())
            .to_string()
            .contains("letters"));
    }
}
