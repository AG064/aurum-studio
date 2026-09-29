# Working in Aurum Studio

## Everyday work

1. Launch Aurum Studio. Its first launch creates a small welcome project if no project is selected.
2. Create a 2D or 3D project, or import an existing project. Import adds `aurum.toml` only when it is missing.
3. Open a scene in the Scene panel. Add nodes, change properties, attach scripts, and inspect the saved hierarchy.
4. Use Files for code and resource text. Saves check the hash from the preceding read so concurrent edits are not silently overwritten.
5. Select Develop to watch the project. Validate checks imports and script/resource loading. Game starts a managed preview.
6. Use Export to produce a Windows package containing its own runtime.

Studio does not need an open Godot editor for these operations. The optional editor is available for native editor workflows.

Unsaved file drafts are stored separately under `.aurum/drafts/` and restored when the file is reopened. View saved version lets you compare against disk without deleting the draft. Requests retain their project identity during a project switch. Studio refuses to close while an operation is active.

## Headless CLI

```powershell
aurum project A:/Projects/MyGame --request-json '{"op":"status"}'
aurum project A:/Projects/MyGame --request-json '{"op":"scene_inspect","scene":"main.tscn"}'
aurum project A:/Projects/MyGame --request-json '{"op":"validate"}'
aurum project A:/Projects/MyGame --request-json '{"op":"play","frames":120}'
aurum project A:/Projects/MyGame --request-json '{"op":"package","output":"dist/windows-v1"}'
```

`--request <file.json>` avoids shell quoting for larger requests. Every result is JSON and failed operations return a nonzero exit status.

## MCP

Use the configuration shown in Studio's Agents panel. For supported client formats, configuration can also be printed or explicitly installed:

```powershell
aurum mcp --root A:/Projects/MyGame --tools studio --print-config codex
aurum mcp --root A:/Projects/MyGame --tools studio --install claude-code
```

Client installation is optional and only runs when requested. Codex configuration respects `CODEX_HOME`. Read-only and denied-tool options are preserved when generating a connection.

| Tool | Purpose |
| --- | --- |
| `aurum_project_query` | Status, files, text reads, scene inspection, runtime classes/properties, presets, runtime notices |
| `aurum_project_action` | File save/undo, scene transactions, build, validation, bounded play, export and packaging |
| `aurum_mcp_status` | Effective permissions and available tools |

Use `--tools all` for procedural meshes, glTF/GLB import/export, sprites, animation, and the separate Rust simulation session. This adds no model provider or API-key requirement.

## Scene transactions

Scene paths are relative to the configured rendering project, usually `godot/`. File paths are relative to the Aurum project root.

```json
{
  "op": "scene_edit",
  "scene": "main.tscn",
  "expected_sha256": "hash returned by scene_inspect",
  "operations": [
    {
      "op": "create",
      "parent": ".",
      "name": "Crate",
      "type": "MeshInstance3D",
      "properties": {
        "position": {"x": 2, "y": 1, "z": 0},
        "mesh": {"resource": "BoxMesh"}
      }
    }
  ]
}
```

Supported scene edits are `create`, `instance`, `set`, `remove`, `reparent`, and `attach_script`. Instance operations accept a project `res://` scene or imported glTF/GLB resource. Vectors use named coordinates, colors use `r/g/b/a`, and inline resources use a class name plus properties.

`set_main_scene` selects a start scene through the runtime settings writer. Its changed file is `project.godot`, so any supplied expected hash must come from that file rather than the scene.

A batch is staged through the runtime's `PackedScene` and `ResourceSaver`. The destination changes only if the batch succeeds and its previous hash still matches. `undo` restores the previous saved file. Live editor edits use the editor's undo history instead.

## Native development

```powershell
aurum dev A:/Projects/NativeGame
aurum dev A:/Projects/NativeGame --editor
aurum dev A:/Projects/NativeGame --once --no-editor --json
```

A native project declares `rust_package` and `addon_destination` in `aurum.toml`. `engine.path_hint` can point to a shared Cargo workspace when the project has no local manifest. Project and external engine sources are watched. Native declarations are compared separately from method bodies; deletion is a change too.

The `minimal` template is an advanced Rust extension skeleton. The default `3d` and `2d` templates run without Rust compilation. First-time native dependency resolution creates a Cargo lockfile; subsequent builds keep it locked.

## Packaging

The Windows package command validates the project, uses or prepares a Windows export preset, writes a PCK, includes the runtime and native libraries, and records runtime license notices. The output directory must not already exist. Launch `game.exe` with its accompanying files.

The generic `export` operation accepts `preset`, `output`, and optional `debug` or `pack`. Platform-specific executable exports require matching export templates. Portable Windows packaging uses the supplied full runtime executable and does not require a separate engine installation on the recipient's machine.

## State and permissions

Studio keeps its registry and sessions under `AURUM_STUDIO_HOME`, normally `A:/AurumStudio`. Project-local staging and saved-file undo live under `.aurum/`, which should be ignored by Git. The optional editor bridge uses `.godot/aurum/editor/`.

HTTP control binds loopback and requires its session token. MCP uses local standard input/output. File operations reject traversal, external symlink targets, and internal state paths. Running or validating a project executes its trusted scripts and native code with the user's permissions; read-only tool filtering is not an operating-system sandbox.

Scene batches are limited to 200 edits, inspection is bounded, gameplay runs have a frame/time limit, and tool output is capped. Initial native builds can take minutes; configure an agent client's operation timeout accordingly.
