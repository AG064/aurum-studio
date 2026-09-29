# aurum-mcp

A local [Model Context Protocol](https://modelcontextprotocol.io) server for
Aurum Studio and the Aurum engine libraries.

For game-project work, use `--tools studio`: inspect and edit saved files and
scenes, query the runtime schema, validate, run bounded gameplay tests and
package a game. These operations invoke the managed Godot runtime headlessly;
they do not require a visible editor.

`--tools all` also provides the independent Rust simulation, procedural content
authoring and optional live-editor bridge. Rust simulation tools run without
a Godot process, but do not inspect or mutate a running game's memory.

## Why headless

The Studio profile has three tools: `aurum_project_query`,
`aurum_project_action` and `aurum_mcp_status`. Their operation schema is shared
with the CLI and HTTP interface. Runtime discovery grounds class/property
requests in the installed engine version. Structured gameplay reports let
clients distinguish a successful process exit from a passing test.

The Rust simulation is useful for deterministic engine-level work. Actual
gameplay testing uses the project operations, not that separate simulation.

The earlier simulation design is recorded in
[`docs/superpowers/specs/2026-09-11-aurum-mcp-design.md`](../../docs/superpowers/specs/2026-09-11-aurum-mcp-design.md).

## Zero new dependencies

The crate adds **no external packages** to the workspace. It uses only
the workspace libraries and the `serde` / `serde_json` / `thiserror`
dependencies the workspace already locked.

The MCP stdio transport is newline-delimited JSON-RPC 2.0 — a small enough
subset to implement directly rather than adopt a protocol SDK. You can verify
the claim yourself:

```pwsh
cargo tree -p aurum-mcp --edges normal      # only workspace crates + serde stack
```

## Running it

```pwsh
aurum mcp --root ./examples/orbit-break --tools studio
aurum mcp --root ./examples/orbit-break --tools studio --read-only
aurum mcp --root ./saves --tools all       # simulation and content authoring
cargo run -p aurum-mcp -- --trace          # standalone, protocol echoed to stderr
```

`--root` (default: the working directory) confines `aurum_save` and
`aurum_load`. `..` escapes are refused lexically, and existing paths are
re-checked through symlinks.

### MCP client configuration

```json
{
  "mcpServers": {
    "aurum": {
      "command": "aurum",
      "args": ["mcp", "--root", "C:/Projects/MyGame", "--tools", "studio"]
    }
  }
}
```

Point it at the absolute path to `aurum.exe` if it is not on your `PATH`.

## Tools

The Studio profile has three tools. The full catalog adds the groups below;
clients can discover the exact enabled set with `tools/list`. Read-only tools
carry the `readOnlyHint` annotation.

The additional groups cover **Rust simulation** (entities, components, events,
state, time, space, story, save/load), **content authoring** (meshes, materials,
nodes, animations, sprites, glTF import/export) and the optional editor bridge.

For project operation examples and the gameplay report contract, see
[the agent guide](../../docs/AGENT_PLAYTESTS.md). The following tables describe
the additional simulation and content tools, not the compact Studio catalog.

| Read-only | Mutating |
|---|---|
| `aurum_world_snapshot` | `aurum_entity_spawn` |
| `aurum_entity_list` | `aurum_entity_despawn` |
| `aurum_component_get` | `aurum_component_set` |
| `aurum_state_get` | `aurum_component_remove` |
| `aurum_state_list` | `aurum_event_emit` |
| `aurum_module_list` | `aurum_event_drain` |
| `aurum_fingerprint` | `aurum_state_set` |
| `aurum_space_state` | `aurum_time_set_scale` |
| `aurum_time_get` | `aurum_module_register` |
| `aurum_story_state` | `aurum_space_step` |
| `aurum_story_get_variable` | `aurum_save` |
| `aurum_story_export_state` | `aurum_load` |
| | `aurum_reset` |
| | `aurum_story_load` |
| | `aurum_story_advance` |
| | `aurum_story_pick_choice` |
| | `aurum_story_jump_to` |
| | `aurum_story_set_variable` |
| | `aurum_story_import_state` |

**Start with `aurum_world_snapshot`.** It returns the whole session — entities
and their components, global state, time scale, modules, pending event count,
the space snapshot, and the story cursor — in one call. That is deliberate: an
agent grounded in one round trip does not need an N+1 exploration loop. Entity
payloads are bounded by `entity_limit` (default 200) and report
`entities_truncated`.

### Content authoring

The `aurum_content_*`, `aurum_mesh_*`, `aurum_node_*`, `aurum_material_*`,
`aurum_animation_*`, and `aurum_sprite_*` tools build scenes in Rust and write
**glTF 2.0**, which Godot imports natively. No Godot process and no Blender
install are needed.

A typical build:

```
aurum_mesh_add        kind=box, sphere, cylinder, cone, torus, plane
aurum_material_add    colour, metallic, roughness, emissive
aurum_node_add        parent, mesh, material, transform
aurum_animation_spin  spin a node about an axis
aurum_animation_add   arbitrary keyframes on translation/rotation/scale
aurum_content_export  writes <name>.gltf + <name>.bin
```

Two conventions matter when scripting this:

- **Indices are stable.** `aurum_node_remove` detaches a node from the graph
  but keeps its slot, so animation targets stay valid. Call
  `aurum_content_state` to see current indices.
- **Content is not session state.** `aurum_reset` does not clear it and it is
  not written into a save file — authored work is exported, not checkpointed.

`aurum_sprite_atlas` packs named rectangles and returns the layout. Give each
sprite a `color` and it also composes a placeholder PNG, which is enough to
block out a UI before real art exists. The PNG encoder is written against
DEFLATE stored blocks, so it needs no compression crate.

### Driving the live editor

`aurum_editor_status` and `aurum_editor_op` reach the scene open in the Godot
editor, through the `AurumEditor` GDExtension and its plugin.

```
aurum mcp --root . --editor-bridge <dir>   # the plugin prints <dir> on startup
```

The bridge is a directory pair the plugin polls once per frame rather than a
socket: Godot is not thread-safe, so a socket would need a worker thread plus a
main-thread hand-off queue for latency nobody needs in an editor.

**One op tool, not one tool per primitive.** The op names below *are* the
stable native surface, so mirroring them individually here would duplicate a
contract already stated on the other side — and the point of keeping that
surface small is that changing a tool never costs an editor restart.

| op | arguments |
|---|---|
| `describe_scene` | — |
| `node_count` | — |
| `create_node` | `parent`, `type`, `name` |
| `set_property` | `node`, `property`, `value` |
| `attach_script` | `node`, `script` |
| `remove_node` | `node` |
| `reparent_node` | `node`, `parent` |

Node paths are relative to the edited scene root; `"."` is the root itself.
`set_property` reads the property's existing type first, so
`{"x": 1, "y": 2, "z": 3}` assigns a `Vector3` rather than failing as a
Dictionary.

Without `--editor-bridge`, both tools still work and simply explain how to
configure one.

### Baking a Godot scene

glTF carries geometry, materials, and animation, but it cannot express
*script attachment* — binding a `.gd` to a node is a Godot-native resource
edit. `aurum_scene_bake` closes that gap by generating a GDScript that loads
the exported glTF, attaches scripts by node path, and saves a real `.tscn`.

Aurum never hand-writes a scene file: Godot's own `PackedScene` and
`ResourceSaver` own that format. The result is a version-controllable scene
with full editor undo, produced without any third-party code.

```
aurum_content_export   -> res://models/level.gltf
aurum_scene_bake       -> gen/bake.gd
godot --headless --script res://gen/bake.gd   -> res://models/level.tscn
```

Verified by `scripts/tests/bake_scene.ps1`, which runs the generated script in
Godot 4.7 and asserts the finished `.tscn` both references the script *and*
carries it on the node — a scene can reference a resource without using it,
which would look like success and do nothing.

### Importing Blender work

`aurum_content_import` reads `.gltf` and `.glb` — including Blender's default
`.glb` export — back into the same model the procedural tools build. `mode:
append` (the default) merges into what you have and remaps every index;
`replace` starts fresh from the file.

That makes the loop bidirectional: a Blender mesh can be imported, inspected,
transformed, merged with procedural geometry, and exported again. Verified
against a real 1.9 MB Blender mech — 96 meshes, 66,105 triangles, 2 skeletal
animations — which round-tripped through MCP and back into Godot with the
triangle count unchanged.

Interleaved buffers (Blender writes them routinely), normalized integer
attributes, base64 data URIs, and `matrix` node transforms are all handled.
Draco and meshopt compression are refused with a message that says to
re-export without compression, rather than silently producing an empty mesh.

Blender is optional by construction: it exports the same glTF that Aurum
writes, so a mesh can come from either and still flow through the same
pipeline.

### Story / visual novel

`aurum_story_load` accepts a story **file** (`path`, confined to the server
root) or **inline JSON** (`story`). `aurum_story_advance` then returns one
event per call — `Dialogue`, `Choice`, `SceneEnded`, `Quit`, `Goto`,
`Command`, or `Error` — and a `Choice` event carries its own indices, so
`aurum_story_pick_choice` needs no guessing. Story variables are `bool`,
`number`, or `string`, matching the interpreter's model.

A loaded story travels in the save payload together with its own definition, so
`aurum_save` produces a self-contained file that `aurum_load` can rebuild
without the story file still being on disk.

## Read-only mode

`--read-only` removes mutating tools from `tools/list` *and* refuses them if
called anyway. A read-only client sees a smaller, honest tool surface rather
than a set of tools that fail at call time.

## Compatibility with the Godot surface

`aurum_save` writes the same JSON shape as `AurumNode.save_to_json` —
`next_entity_id`, `time_scale`, `state`, `components`, `space`. A session saved
from a running Godot editor loads here, and vice versa. Component blobs are the
same dynamic `{ "entity id": { "TypeName": <json> } }` model, so tool
semantics match across surfaces.

## Errors

A tool that runs and fails returns `isError: true` inside a successful
JSON-RPC result, so the model can read the message and correct itself. A
malformed request, an unknown tool, or a protocol violation is a JSON-RPC
error. The distinction is the MCP specification's, and getting it backwards
makes agents retry things that cannot succeed.

## Layout

| File | Role |
|---|---|
| `protocol.rs` | JSON-RPC 2.0 / MCP wire types |
| `engine.rs` | The headless session and its save format |
| `tools.rs` | The `aurum_*` catalog, handlers, and the path guard |
| `server.rs` | The stdio loop |
| `cli.rs` | Argument parsing shared by `aurum-mcp` and `aurum mcp` |

Handlers are thin adapters. All simulation semantics live in `aurum-core` and
`aurum-space` — deliberately, so the engine keeps one source of truth.

## Testing

```pwsh
cargo test -p aurum-mcp
```

The whole protocol is exercised in memory by driving `serve()` with a `Cursor`,
so no subprocess is needed to test a full session.
