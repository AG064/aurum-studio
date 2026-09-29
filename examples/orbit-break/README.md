# Orbit Break

A complete 3D survival MVP built through Aurum's project operations. Survive five waves, choose a loadout and defeat the Warden. The game uses procedural geometry and generated sound effects, with no asset download or model service required.

![Orbit Break title screen](../../docs/media/orbit-break-menu.png)

## Play from source

Install [Aurum Studio](../../README.md#get-started-on-windows), then run these commands from the repository root:

```powershell
aurum studio ./examples/orbit-break
aurum run ./examples/orbit-break
```

The first command opens the workspace. The second launches the game directly. Neither requires a visible Godot editor.

| Control | Action |
| --- | --- |
| WASD or arrow keys | Move |
| Space | Dash with brief invulnerability; 2.2-second cooldown |
| Left mouse, held | Manual aim; otherwise targeting and firing are automatic |
| Escape | Pause or resume |
| 1, 2, 3 | Choose an upgrade |
| Enter | Start or retry |
| M | Toggle sound |
| Touch | Drag the left side to move; tap DASH |

The run also pauses when focus is lost. Scores and sound preference stay on your machine. Touch input handling is covered by synthetic events; Android/iOS device behavior is not yet verified.

## Build a portable game

From the repository root:

```powershell
pwsh ./examples/orbit-break/tools/verify.ps1 -Package `
  -OutputDirectory ./examples/orbit-break/dist/windows
```

The verifier finds the runtime beside an installed Aurum executable or under `AURUM_STUDIO_HOME`. Override it when using a development build:

```powershell
pwsh ./examples/orbit-break/tools/verify.ps1 `
  -AurumBinary ./target/debug/aurum.exe `
  -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe `
  -Package -OutputDirectory ./examples/orbit-break/dist/windows
```

Tests and packaging run in a disposable copy with isolated user data. Only after all checks pass is the package copied to the requested, previously nonexistent destination and hash-checked. The script refuses to overwrite an earlier build. Omit `-OutputDirectory` to retain the package only in the temporary verification directory.

Open **Play Orbit Break.vbs**, or launch **dist/windows/game.exe**. Keep the PCK and notices beside the executable. The package includes the full Windows runtime and needs no separate engine installation. Generated packages and local state are excluded from Git.

## Agent and test contract

```powershell
aurum mcp --root ./examples/orbit-break --tools studio
```

The standard MCP profile provides project queries, project actions and status. It can read runtime classes, edit scenes and code, validate, run tests and package the project without an open editor.

On a disposable project copy, run the acceptance scene with:

```json
{"op":"play","scene":"tests/acceptance.tscn","frames":120,"fixed_fps":60,"user_args":["--acceptance"],"report":true}
```

There are 31 gameplay checks, including movement, arena containment, keyboard/touch input, damage and invulnerability, projectiles, pickups, upgrades, all waves, victory/defeat, restart and live tuning. The suite directly drives state transitions. Separate `--autoplay` and `--idle-test` campaigns exercise the ordinary game loop without changing player statistics: movement wins, standing still loses. [Verification record](../../docs/DELIVERY.md).

## Edit without restarting

Change `godot/tuning.json` while a source preview is running. Valid player-speed and spawn-interval changes are read twice per second and preserve the run. Invalid tuning retains the last valid values. An external edit was verified with the same runtime process before and after the change.

This is game-managed data reload, not arbitrary state-preserving code replacement. Script/scene edits use Aurum's validated preview restart. The packaged PCK is not an editable source directory.

## Scope

The delivered platform is Windows. Non-Windows builds need their templates and SDKs. This is not a VR port: headset controls, an XR rig, comfort design and device performance need separate work. There is no networking or telemetry code in the example. Its scripts, geometry, sounds and SVG icon are covered by the repository's [MIT license](../../LICENSE).
