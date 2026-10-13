# Reload behavior

Studio and `aurum dev` share one supervisor. Default development runs without a visible editor. `--editor` starts the optional native editor; `--play` starts a managed game preview.

## Native build path

1. Cargo metadata identifies the configured package and its declared `cdylib` target, which need not share the package name.
2. Cargo checks the source and dependency graph and reports the selected artifact.
3. Studio resolves the ordered host/profile mapping in the GDExtension manifest. Relative and `res://` file paths stay inside the project; missing or unsupported mappings are refused.
4. The reported library is hashed and staged beside that declared destination.
5. An atomic replacement commits the verified library.
6. A debug hash marker is published under `.godot/aurum/`.
7. The editor plugin observes the marker and reloads the extension.
8. The live fingerprint reports which build is executing.

Matching an old build artifact with an installed library never bypasses Cargo's source check. Identical new bytes avoid an unnecessary replacement. A failed build leaves the last working library installed. A failed reload notification reports that installation succeeded but reload has not been confirmed.

Cargo success without a matching package/target artifact is not accepted, even
if a guessed old DLL exists. Normal build and the development supervisor share
the manifest destination resolver. It deliberately supports bounded quoted
library file paths and standard host/profile tags; custom feature expressions,
framework bundles and cross-target delivery need separate qualification.

## Restart boundaries

| Change | Behavior |
| --- | --- |
| GDScript, scenes, resources | Validate in the background; the native editor handles supported reloads |
| Rust implementation behind stable native declarations | Rebuild and reload the native extension |
| Registered native methods, fields, classes, entry points or mappings | Report an exceptional editor restart requirement |
| Managed gameplay affected by changed code/content | Refresh gameplay independently of Studio and the editor |

Declarations are compared against the previous source snapshot so changing an ordinary method body does not automatically become a structural change. Deleted files are included. Studio never force-closes an unrelated process.

Browser previews now expose typed live properties and optional automatic checkpoint-based rebuilding. Code and scene edits boot a new runtime while preserving compatible state, with rollback when the replacement fails. This does not migrate arbitrary native memory. See [runtime editing and agent verification](INTEGRATION.md) and [Rust Web builds](RUST_WEB.md).

## Managed native sessions

Current development source adds Windows-qualified managed native games. These
commands are not in the published 0.4.0 download. Run a built `aurum` executable
against a trusted project:

```powershell
aurum project A:/Games/MyGame --request-json '{"op":"native_start","rendered":true}'
aurum project A:/Games/MyGame --request-json '{"op":"native_status"}' --read-only
aurum project A:/Games/MyGame --request-json '{"op":"native_checkpoint"}'
aurum project A:/Games/MyGame --request-json '{"op":"native_replace"}'
aurum project A:/Games/MyGame --request-json '{"op":"native_stop"}'
aurum project A:/Games/MyGame --request-json '{"op":"native_host_stop"}'
```

CLI, Studio's project endpoint and MCP's `aurum_project_action` share the same
project controller. Start can launch a hidden Studio host. Opening Studio later
reuses it. Read-only status does not start a host or modify the project. MCP
disconnect and reconnect leave the game running. `native_stop` stops the owned
game; `native_host_stop` also shuts down the controller. Explicitly stop hosts
at the end of automated fixtures.

Preparation validates a private copy and keeps the working source untouched.
Compatible replacement captures a complete checkpoint, restores a paused
successor, recaptures and verifies it, then retires the old owned process. A
failed replacement retains the old game. Use `aurum_capture_state()` and
`aurum_restore_state(state)` for game-specific state. This is managed process
replacement, not arbitrary native memory migration. Scripts and native code
still execute with your account permissions.

`native_checkpoint` saves a bounded recovery record. After host termination,
`native_recover` verifies recorded process identities and the source hash before
restoring that checkpoint. Newer source, live or inaccessible recorded
processes, and corrupt journals are refused. Progress since the saved checkpoint
is not recovered. Save files have independent retained generations; discover
them with `native_userdata_list`, and remove only an inactive, non-current
generation with `native_userdata_forget`. Removal is permanent, never automatic.

Development source also stages self-contained GDExtension manifests and builds
a configured local Rust `cdylib` privately. Cargo runs with `--frozen --offline`
against an existing lockfile and cached dependencies. Target/output directories
stay inside the owned workspace; compiler descendants are terminated on host
exit or timeout. Build selection uses Cargo package/artifact identities, not a
guessed DLL filename, including custom library target names. The working DLL,
Cargo.lock and import directory are untouched.

Self-contained Rust/GDExtension source revisions use this private build path
through `changes_validate` and `changes_apply`. Compilation and resource checks
precede journaled source publication. The receipt reports the generated
library's path/hash and `native_artifacts_published:false`; those bytes are
verified but excluded from source replacements and undo records. Applying or
undoing source does not replace the running game or installed working DLL.
Call `native_replace` separately to verify and activate the changed game, or
`build` to install a normal native library. See
[project revisions](PROJECT_REVISIONS.md#journaled-publication-and-revision-undo).

Before activation, the game must report the expected manifest loaded and match
the prepared selected-library and declared-dependency hashes. External paths,
missing dependencies, stale artifact reports, changed private source and
compiler failures refuse the replacement. Discovery uses Godot's actual
ConfigFile parser and runtime feature tags. This covers declared extension
paths, not every system DLL or arbitrary code performing dynamic loads.

Current bounds: 16 manifests of at most 64 KiB each, 128 MiB per compiled Rust
library, a 120-second compiler deadline and the existing private-source/storage
budgets. Cold builds can hit these limits. A shared compile cache, external
workspace staging, custom validators, concurrent save writers, background jobs
and cancellation of an in-flight preparation still need qualification. Rendered Windows play,
headset and physical mobile acceptance remain separate from headless tests.

## Verify native extension reload

```powershell
pwsh scripts/tests/verify_studio.ps1 -GodotBinary A:/Tools/Godot_v4.7-stable_win64.exe -Offline -NativeReload
```

The current native gate runs the actual Rust CLI supervisor, modifies source in a temporary workspace, observes new fingerprints under one editor PID, injects a compiler error, checks that the working DLL remains unchanged, and verifies recovery. It restores the temporary source and stops its owned processes.

The older `studio_hot_reload.ps1` and Phase 0 scripts test the PowerShell path. They remain useful historical tools but are not substitutes for the current CLI/Studio acceptance gate.
