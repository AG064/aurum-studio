# Changelog

All notable changes to Aurum are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.3.0] - 2026-10-01

### Added

- Typed live property inspection and editing, automatic source-fresh preview rebuilds, checkpoint restoration and rollback.
- Shared headless/rendered playtests with input timelines, real PNG captures, configurable budgets and retained failure diagnostics.
- Rust Web side-module builds, managed SDK discovery and private native class-registry staging.
- A twelve-encounter Orbit Break campaign with three flight frames, three stations, three phased bosses, meaningful three-choice upgrades, persistent records and original music.
- Downloadable Windows Studio and game packages, with portable launchers and SHA256 checksums.

### Fixed

- Same-size source edits and deleted files no longer reuse stale browser exports.
- Concurrent live saves keep their staging files out of source scans and exports; Windows replacements handle brief reader/scanner locks with bounded waits.
- Frozen checkpoints suspend preview rendering during private builds and restore the original rendering state on resume or rollback.
- Native Web build stages and bounded fixture logs provide actionable diagnostics for pending exports.
- Private native builds isolate Godot settings and caches from source inspection and user editors.
- Native editor cache-writer phases use a fair, deadline-bound queue; status, file reads and ordinary runtime probes remain independent.
- Script-only source scene inspection and staged edits use immutable revision/runtime-keyed private caches without re-importing the original `.godot` directory.
- Windows atomic saves support extended paths; native inspection caches use shorter temporary locations for process and helper-tool compatibility.
- Agent/API preview builds report project-scoped activity so the connected UI freezes rendering early and restores the same run on adoption or failure.
- Export profiles retain HTML options, include/exclude filters and legal notices.
- Failed builds or rejected checkpoints retain and resume the previous game; stopping during export prevents late publication.
- Cold asset inspection and read-only inspection during private exports no longer require reopening an editor.
- Windows process identity queries use read-only native handles rather than slow WMI subprocesses.
- The 3D demo registers its base class, and Rust Web builds avoid incompatible exception-tag imports.

### Boundaries

- Windows and Chromium are acceptance-tested. Mobile/VR hardware, signing and store delivery remain separate gates.
- Checkpoint rebuilding is not arbitrary memory migration. Native registration changes can still require a controlled editor restart.
- Rust Web support is experimental; portable dependencies and the optional Web SDK are required.

## [0.2.0]

### Playable MVP workflow

- Fixed existing-file path resolution so headless file reads and undo work consistently on clean Windows, Linux and macOS runners; added a regression test.
- Added required JSON gameplay verdicts, bounded game arguments, and fixed-rate headless tests across CLI, HTTP, and MCP.
- Fixed explicit test-scene selection and an environment-dependent runtime discovery test.
- Added six-platform preset configuration and Studio export/test controls with full failure diagnostics.
- Exercised the workflow with the sibling Orbit Break Windows MVP, including a complete campaign, touch events, live tuning, and portable packaging.
- Mobile and XR device builds remain unverified; preset configuration does not imply device readiness.

### Aurum Studio 0.2

- Added the integrated project, scene, file, agent, and export interface.
- Added shared headless project operations, runtime schema discovery, and a compact MCP profile.
- Fixed native build freshness, atomic installation, reload notification, failure reporting, and scene persistence.
- Added saved-file undo, persistent drafts, live editor undo/redo, and safe project-bound requests.
- Added runnable 2D/3D starters and repaired the native starter.
- Added portable Windows packaging and a state-preserving installer with an included runtime.
- Added isolated end-to-end acceptance for headless work, live editing, native reload recovery, and packaging.

### Added

- **`aurum-mcp`** — headless Model Context Protocol server. Drives the engine
  over newline-delimited JSON-RPC 2.0 on stdio with no Godot process, exposing
  31 `aurum_*` tools (12 read-only, 19 mutating) across entities, components,
  events, state, time, the space simulation, visual-novel stories, and
  save/load. Adds **no external dependencies**: only `aurum-core`,
  `aurum-space`, `aurum-vn`, and the already-locked `serde` stack.
  - `--read-only` omits and refuses mutating tools.
  - `--root` confines `aurum_save` / `aurum_load` and story files, refusing
    `..` escapes and re-checking existing paths through symlinks.
  - Saves in the same JSON shape as `AurumNode.save_to_json`, so a session
    round-trips between the headless and Godot surfaces. A loaded story travels
    with its own definition, so a save file is self-contained.
  - 90 tests, including full protocol sessions driven in memory.

- **`aurum-core::dynamic`** — `DynamicWorld`, a scriptable string-keyed world
  with JSON-blob components. The generically typed `ecs::World` cannot be
  driven from a save file or an external tool; this is its counterpart, and
  the model the Godot bridge already exposes.

- **`aurum-core::state`** — `State::set_owned` accepts a runtime-owned key.
  Callers receiving keys from outside the binary no longer have to
  `Box::leak` them to satisfy `set`'s `&'static str` bound.

- **`aurum-core::time`** — `FixedTimestep` derives `Debug` and `Clone`.

- **`aurum-vn`** — `ChoiceData` is now re-exported from the crate root.

- **`aurum-cli`** — the `aurum` binary, with `aurum mcp` delegating to
  `aurum-mcp` so the two entry points cannot drift.

- **`aurum-content`** — Rust-first content authoring: procedural meshes,
  scene graphs, animations, sprite atlases, and a glTF 2.0 exporter. Adds no
  external dependencies; glTF is JSON plus one binary buffer, and the PNG
  encoder uses DEFLATE stored blocks rather than a compression crate.
  - Primitives: box, plane, UV sphere, cylinder, cone, torus.
  - Transforms with inverse-transpose normal handling and winding flips under
    mirroring; merge for compound shapes.
  - Materials in the metallic-roughness model.
  - Animations with linear/step interpolation and unit-quaternion normalization.
  - Sprite atlas packing plus a dependency-free PNG encoder.
  - 76 tests, including winding assertions that catch inside-out meshes.

- **`aurum-mcp` content tools** — 14 new tools (45 total) so an agent can
  build meshes, materials, hierarchies, animations, and sprite atlases, then
  export glTF, all without Godot or Blender running.

- **`scripts/tests/gltf_import.ps1`** — end-to-end gate: generate a scene in
  Rust, import it into Godot 4.7, and assert the meshes, node names, and
  animations survive.

- **glTF 2.0 import** (`aurum-content`) — reads `.gltf` and `.glb` back into
  the Aurum content model, so a Blender-authored asset can be inspected,
  transformed, merged, and re-exported. Handles GLB chunks, base64 data URIs,
  relative buffer paths, interleaved `byteStride` data, normalized integer
  attributes, every index component type, multi-primitive meshes (split into
  child nodes so materials survive), and `matrix` node transforms. Refuses
  Draco and meshopt with an actionable message. 19 new tests.

- **`aurum_content_import`** — the MCP tool for the above, with `append`
  (default, remaps indices) and `replace` modes. Verified end to end against a
  real Blender export: 96 meshes, 66,105 triangles, and 2 skeletal animations
  round-tripped through MCP into Godot with an identical triangle count.

- **`cargo run -p aurum-content --example inspect_gltf -- <file>`** — summarise
  any glTF or GLB.

- **Scene baking** (`aurum-content::gdscript`) — generates a GDScript that
  loads an exported glTF, attaches scripts to named nodes, and saves a real
  `.tscn`. This is the scripting path: glTF cannot express script attachment,
  and routing it through generated code means Godot owns the scene format, no
  third-party bridge is involved, and the result is version-controllable.
  Options are validated before generation, and string literals are escaped.

- **`aurum_scene_bake`** — the MCP tool for the above (50 tools total).

- **`scripts/tests/bake_scene.ps1`** — end-to-end gate: generate, run the
  script in Godot 4.7, and assert the `.tscn` references the script and carries
  it on the right node, with the hierarchy and animation intact.

- **`aurum-editor`** — a GDExtension exposing a deliberately small, stable
  native surface for driving the live Godot editor: create a node, set a
  property, attach a script, remove, reparent, describe, and save. All higher
  level tool composition stays in `aurum-mcp`, which is plain Rust and rebuilds
  freely, so adding a tool never changes the Godot-facing surface and never
  costs an editor restart, per the Phase 0 reload boundary.

  A separate crate rather than an addition to `aurum-godot`, so the Phase 0
  verified game shim stays frozen and editor code never ships in a game build.

  Bridge transport is a polled directory of JSON files, not a socket: Godot is
  not thread-safe, so a socket would need a worker thread plus a main-thread
  hand-off queue for latency an editor does not care about.

- **`godot/addons/aurum_editor/`** — the thin GDScript `EditorPlugin`. It
  supplies only the three things that genuinely need the editor context (the
  edited scene root, plugin lifecycle, per-frame pump), because gdext 0.5.4
  gates `EditorPlugin` behind `experimental-godot-api` and enabling that would
  change the build of the Phase 0-verified shim.

- **`aurum_editor_status` / `aurum_editor_op`** — the MCP tools for the above,
  with a `--editor-bridge` server option.

- **`scripts/tests/editor_plugin.ps1`** — headless gate that builds the
  extension, registers it in a scratch project, and exercises every primitive
  against a real Godot scene tree, including the file bridge round trip.

### Fixed

- `PathGuard` denied every path when the root was relative, so
  `aurum mcp --root .` refused all file operations. The root is now
  absolutized before the containment check.

- Sphere, cylinder, cone-side, and both cylinder caps were wound inside-out.
  Caught by tests that assert face normals point away from the origin.

### Documentation

- `docs/superpowers/specs/2026-09-11-aurum-mcp-design.md` — the three-layer MCP
  architecture, per-layer dependency accounting, and the phasing.
- `crates/aurum-mcp/README.md` — usage, client configuration, and the tool
  surface.

## [0.1.0] — 2026-07-28

The first public release. Foundation only — no API stability promises yet.

### Added

- **`aurum-core`** — pure Rust engine core.
  - `ecs` module: entities, components, systems, resources.
  - `events` module: typed event bus with subscribe/emit/dispatch.
  - `state` module: typed key-value state with JSON save/load.
  - `time` module: time scale and fixed timestep with anti-spiral cap.
  - `assets` module: stable resource IDs.
  - 15 unit tests, all green.

- **`aurum-godot`** — GDExtension shim exposing a `AurumNode` Node class to
  GDScript. The single Rust surface Godot sees. Includes:
  - Entity spawn / despawn.
  - JSON-blob component store with type-keyed reverse index.
  - Dynamic event bus that fires Godot signals.
  - Typed state (bool / int / float / string) with save/load.
  - Time scale control.
  - Module registration.

- **`aurum-2d`** — 2D game module.
  - `Position2D`, `Velocity2D`, `AABB`, `Sprite`, `Tag` components.
  - `step_kinematics`, `aabb_overlap`, `wrap_position` helpers.
  - 4 unit tests + 1 doc test.

- **`aurum-3d`** — 3D game module.
  - `Position3D`, `Velocity3D` components.
  - `step_kinematics` helper.
  - 1 unit test.

- **`aurum-vn`** — visual novel module.
  - `Story` parser.
  - `Interpreter` with `Event` output (Dialogue / Choice / Quit / Goto /
    Command / Error).
  - Full save/load state.
  - 5 unit tests.
  - GDScript shim exposed via the `Aurum` autoload:
    `Aurum.story_load`, `Aurum.story_advance`, `Aurum.story_pick_choice`,
    `Aurum.story_jump_to`, `Aurum.story_get_variable`,
    `Aurum.story_set_variable`, `Aurum.story_export_state`,
    `Aurum.story_import_state`, `Aurum.story_current_scene`,
    `Aurum.story_current_entry_index`. Events come back as Dictionaries.

- **`aurum-vr`**, **`aurum-text`**, **`aurum-cli`** — stub crates
  reserving the module names and a minimal surface so the workspace
  builds and the module surface is fixed.

- **Godot project** at `godot/`.
  - Add-on `addons/aurum/` with the GDExtension, plugin, and runtime
    autoload (`Aurum`).
  - `aurum_dev_console` (F1 in debug builds).
  - `aurum_2d_kinematics` system.

- **Tutorial demos**:
  - `godot/demos/2d_squares/` — player + coins, score, dev console.
  - `godot/demos/3d_bounce/` — gravity + jumping.

- **Templates** for 2D (full), 3D (stub README), VN (stub README).

- **Build pipeline**:
  - `scripts/build.ps1` (build + copy DLL + optional run).
  - `scripts/dev.ps1` (cargo-watch + auto-rebuild).
  - VS Code tasks in `.vscode/tasks.json`.

- **Documentation**:
  - `README.md`.
  - `docs/ARCHITECTURE.md`.
  - `docs/MODULES.md`.
  - `docs/GETTING_STARTED.md`.
  - `CONTRIBUTING.md`.

- **CI** on GitHub Actions: cargo fmt + clippy + test + release build
  on Linux, Windows, macOS.

### Notes

- Game projects that build on Aurum live in their own repositories:
  - `AG064/the-regular-novel` — visual novel game, consumes `aurum-vn`.
  - `AG064/life_evolution` — GPU life simulation, consumes the core
    runtime + its own GDExtension crate.
  Each game copies the engine add-on (`addons/aurum/`) from this
  repo via its own `build.ps1` and depends on the compiled
  `aurum_godot.dll` produced by `scripts/build.ps1` here.
- Cross-platform builds are configured but only the Windows x86_64
  binary is in the add-on bin/. Linux / macOS binaries are produced
  by the CI on tagged releases.
- The 2D and 3D demos are the only complete demos in this release.
  The VN module has full tests and a GDScript shim but no demo
  inside this repo (the live example is the game project above).
