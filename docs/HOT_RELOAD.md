# Reload behavior

Studio and `aurum dev` share one supervisor. Default development runs without a visible editor. `--editor` starts the optional native editor; `--play` starts a managed game preview.

## Native build path

1. Cargo checks the source and dependency graph.
2. Studio reads the produced artifact from Cargo's structured output.
3. The library is hashed and staged beside its destination.
4. An atomic replacement commits the verified library.
5. A debug hash marker is published under `.godot/aurum/`.
6. The editor plugin observes the marker and reloads the extension.
7. The live fingerprint reports which build is executing.

Matching an old build artifact with an installed library never bypasses Cargo's source check. Identical new bytes avoid an unnecessary replacement. A failed build leaves the last working library installed. A failed reload notification reports that installation succeeded but reload has not been confirmed.

## Restart boundaries

| Change | Behavior |
| --- | --- |
| GDScript, scenes, resources | Validate in the background; the native editor handles supported reloads |
| Rust implementation behind stable native declarations | Rebuild and reload the native extension |
| Registered native methods, fields, classes, entry points or mappings | Report an exceptional editor restart requirement |
| Managed gameplay affected by changed code/content | Refresh gameplay independently of Studio and the editor |

Declarations are compared against the previous source snapshot so changing an ordinary method body does not automatically become a structural change. Deleted files are included. Studio never force-closes an unrelated process.

Browser previews now expose typed live properties and optional automatic checkpoint-based rebuilding. Code and scene edits boot a new runtime while preserving compatible state, with rollback when the replacement fails. This does not migrate arbitrary native memory. See [runtime editing and agent verification](INTEGRATION.md) and [Rust Web builds](RUST_WEB.md).

## Prove the real workflow

```powershell
pwsh scripts/tests/verify_studio.ps1 -GodotBinary A:/Tools/Godot_v4.7-stable_win64.exe -Offline -NativeReload
```

The current native gate runs the actual Rust CLI supervisor, modifies source in a temporary workspace, observes new fingerprints under one editor PID, injects a compiler error, checks that the working DLL remains unchanged, and verifies recovery. It restores the temporary source and stops its owned processes.

The older `studio_hot_reload.ps1` and Phase 0 scripts test the PowerShell path. They remain useful historical tools but are not substitutes for the current CLI/Studio acceptance gate.
