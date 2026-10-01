# Runtime editing and agent verification

Run reuses a Web preview only when its source SHA256 still matches. Same-size edits, renames and deletions invalidate the snapshot. Live tuning does not invalidate a code export. Studio shows the loaded revision and offers automatic rebuilding.

## Live properties and rebuilds

The inspector discovers exported script values and supported native properties in an ordinary project. Supply a node path relative to the running main scene. Scalars, vectors, colors and bounded JSON collections are editable without starting another game. A batch is type-checked before any property is assigned. Executable resources, arbitrary methods, outside-scene paths and credential-named fields are not exposed.

Scripts and scene structure are loaded through a new runtime. Studio captures a checkpoint, freezes the previous game, boots the new frame and restores compatible state. The previous frame and asset server remain available until the new frame is accepted. A failed build or restore retains the previous game. This is checkpoint-based rebuilding, not arbitrary memory migration or in-place code patching.

Ordinary scenes use conservative property checkpoints. Untouched values follow changed blueprint defaults; values changed during play or through live controls are preserved. Missing nodes, unsupported objects and incompatible fields produce an explicit partial-restoration receipt. Stateful games can define these methods on their main scene:

```gdscript
func aurum_capture_state() -> Dictionary:
    return {"version": 1, "health": health}

func aurum_restore_state(state: Dictionary) -> bool:
    if state.get("version") != 1:
        return false
    health = float(state.health)
    return true
```

The game owns validation and reconstruction of its custom state. Orbit Break includes a versioned implementation covering gameplay entities, mission progress, projectile hit references, inventory, records and exact RNG state. Transient renderer and audio internals are not virtual-machine snapshots.

## Headless and rendered tests

CLI, HTTP and MCP share the same project operations:

Scene inspection and editing prepare imported resources on a cold project without requiring the user to open an editor. An import stamp is refreshed when source content changes. Native cache writers are serialized, and importer diagnostics are reported instead of a generic failure.

```json
{"op":"capture","frames":120,"fixed_fps":60,"capture_frames":[60,120],"width":1280,"height":720,"timeout_seconds":90,"events":[{"frame":5,"type":"key","key":"Enter","pressed":true},{"frame":6,"type":"key","key":"Enter","pressed":false}]}
```

`play` runs without a renderer. `capture`, or `play` with `rendered:true`, produces real PNG frames using an offscreen compatibility-renderer window. The input timeline supports actions, keys, mouse buttons and validated live-property changes. Captures require an available graphical driver; a simulation pass is not a graphics pass.

Each run uses a private source snapshot and isolated application data. Its receipt retains input budgets, observed frames, sampled node state, renderer counters, full stdout/stderr logs and image paths under `.aurum/playtests/<session>`. Project scripts still execute with OS permissions; this is test-data isolation, not a malicious-code sandbox.

Frame budgets accept 1 to 3,600,000 frames. Wall budgets accept 1 to 600 seconds, default 90. Requesting a JSON verdict requires the game to write a fresh report. Missing reports fail with a termination reason such as `frame_budget_exhausted`, `wall_timeout` or `process_error`; exit code zero alone is not a gameplay pass.

Import-stage failures retain their logs and identify the exit code, wall timeout and evidence directory. A completed Godot 4.7 Windows import that exits with the known shutdown access-violation code is retried once. Script errors, incomplete imports and gameplay failures are not retried.

Windows process identity is queried directly through read-only process handles, avoiding PowerShell/WMI startup and timeout costs. Creation time still participates in ownership checks. Older WMI timestamp records are preserved but do not authorize stopping a process under the new identity format; unrelated or unverifiable processes remain untouched.

## Preview and export APIs

- `GET /api/preview` returns the active URL, source revision and staleness.
- `POST /api/preview` reuses only fresh snapshots; `force:true` explicitly rebuilds.
- `action:"commit"` accepts the active session and closes its fallback.
- `action:"rollback"` restores the fallback only when the active session matches the supplied session.
- `action:"export"` writes a fresh standalone build, refusing an existing destination.

Exporting does not hold the active-preview mutex, so agents and the UI can continue reading preview status during a build. Stopping a preview invalidates pending publication. Concurrent replacements use a generation check and preserve the active session when another client changes it.

Read-only scene/class inspection is independent of the preview's build lock. Native import-cache writers use a separate lock, so reopening the workspace while a private preview builds does not fail with a misleading project-busy error.

These control routes remain authenticated. Runtime messages are source-, origin- and session-checked and bounded. The game receives no control API token or filesystem command API.

Web exports preserve the selected project preset, include/exclude filters and HTML options. Required preview constraints are non-threaded operation, managed canvas sizing and no preview PWA cache. Legal-notice patterns are added to the include filter. A named profile can be selected with `[web] preset = "Browser"` in `aurum.toml`.

See [Rust Web builds](RUST_WEB.md) for compiled extensions and their additional hosting requirements.
