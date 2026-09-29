<p align="center"><img src="docs/media/aurum-banner.svg" alt="Aurum Studio. Build games with people and agents." width="100%"></p>

<p align="center">
  <a href="https://github.com/AG064/aurum-studio/actions/workflows/ci.yml"><img src="https://github.com/AG064/aurum-studio/actions/workflows/ci.yml/badge.svg" alt="Build and test status"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-dac28c?labelColor=151a22" alt="MIT license"></a>
  <img src="https://img.shields.io/badge/desktop-Windows%20verified-79cfb2?labelColor=151a22" alt="Windows desktop verified">
  <img src="https://img.shields.io/badge/agents-MCP%20%2B%20CLI-dac28c?labelColor=151a22" alt="MCP and CLI agent interfaces">
</p>

# Aurum Studio

**A local game-development workspace for people and AI agents.** Create a project, edit its scenes and code, run headless gameplay tests, and package a playable game through one application.

Godot 4.7 handles rendering and resources underneath. Aurum owns the project workflow, the local interface, and the agent-facing operations. You do not need to keep a Godot editor open. Use GDScript for gameplay and Rust GDExtensions when native code is useful.

[Get started](#get-started-on-windows) · [Play the example](#a-game-you-can-run) · [Connect an agent](#headless-by-design) · [Documentation](#documentation)

![Aurum Studio managing the Orbit Break project, with packaging and platform export controls](docs/media/studio.png)

## What you can do

| Workflow | Available today |
| --- | --- |
| Create and edit | Runnable 2D/3D starters, scene hierarchy, node properties, scripts, resources, persistent drafts and undo |
| Work with agents | Standard local MCP, CLI and HTTP using the same project operations; runtime class and property discovery |
| Test real gameplay | Explicit test scenes, bounded runs, game arguments, fixed simulation rate and required JSON verdicts |
| Iterate safely | Hash-checked saves, transactional scene edits, failed-build preservation and managed previews |
| Ship a game | Portable Windows packaging with the runtime included; configurable presets for six export targets |
| Use native code | Rust extension builds, atomic library installation and explicit reload boundaries |

No model subscription, provider account or API key is built into Aurum. Bring your preferred MCP-compatible agent. Project operations run locally; HTTP control stays on loopback with a session token.

## A game you can run

**[Orbit Break](examples/orbit-break)** is included as a complete source example, not a mockup: a 3D survival arena with five waves, three enemy types, upgrades, dash, repair pickups, sound, local scores and a boss.

![Orbit Break in play: pilot integrity, wave progression, auto-fire and dash controls](docs/media/orbit-break-gameplay.png)

The example has **31 gameplay checks**, a complete winning autopilot campaign, and a stationary-run check that must lose. Its tuning file can change during play without replacing the game process. [Verification scope](docs/DELIVERY.md) explains what was tested and what remains unverified.

After installing Aurum, from this repository:

```powershell
aurum studio ./examples/orbit-break
# Select Game in Studio, or launch directly:
aurum run ./examples/orbit-break
```

To test and produce a standalone Windows package, without running tests in the source tree:

```powershell
pwsh ./examples/orbit-break/tools/verify.ps1 -Package `
  -OutputDirectory ./examples/orbit-break/dist/windows
```

Then open `examples/orbit-break/Play Orbit Break.vbs`. See the [example guide](examples/orbit-break/README.md) for controls and runtime configuration. Generated game binaries are not checked into Git.

## Get started on Windows

Prerequisites: PowerShell 7, Rust with the Windows C++ build tools, and the full **Godot 4.7 Windows executable**, not the small console launcher. Source builds were verified with Rust 1.98. The installer copies the runtime so subsequent use does not require a separate engine installation.

```powershell
git clone https://github.com/AG064/aurum-studio.git
cd aurum-studio
pwsh ./scripts/install.ps1 `
  -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe `
  -StudioHome "$env:LOCALAPPDATA/AurumStudio"
```

Open **Aurum Studio** from Start. In a new terminal, `aurum` opens the same workspace. The installer preserves projects and state, backs up replaced binaries, and supports `-WhatIf`. Building the application needs Rust; using the script-only project starters does not.

```powershell
aurum new C:/Projects/MyGame --template 3d
aurum studio C:/Projects/MyGame
aurum dev C:/Projects/MyGame --play
```

`dev` is headless by default. `--play` adds a managed game preview; `--editor` opens the optional native editor. See [getting started](docs/GETTING_STARTED.md) for the native Rust template and existing-project import.

## Headless by design

The compact MCP profile exposes three tools: `aurum_project_query`, `aurum_project_action` and `aurum_mcp_status`. Agents discover the installed runtime's classes instead of depending only on remembered engine APIs.

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

Use the absolute executable path if the client does not inherit your terminal's PATH. Add `--read-only` to withhold writes. Client configuration is changed only when you explicitly request installation.

The same test can run through MCP, Studio or the CLI:

```json
{
  "op": "play",
  "scene": "tests/acceptance.tscn",
  "frames": 120,
  "fixed_fps": 60,
  "user_args": ["--acceptance"],
  "report": true
}
```

A required verdict must be freshly written by the game. Missing reports, false verdicts, runtime errors and timeouts fail the operation. For tests that modify files, use a disposable project copy as the example verifier does. [Agent operations and report contract](docs/AGENT_PLAYTESTS.md).

```mermaid
flowchart LR
    Studio[Studio interface] --> API[Shared project operations]
    Agent[MCP agent] --> API
    CLI[Command line] --> API
    API --> Files[Scenes, scripts and resources]
    API --> Runtime[Managed Godot runtime]
    Runtime --> Tests[Gameplay reports]
    Runtime --> Package[Playable packages]
```

`--tools all` additionally exposes procedural content and the Rust simulation tools. The simulation is a separate session, not a connection to a running game's memory. Running a project executes its scripts and native code with your permissions; the tool boundary is not an operating-system sandbox.

## What reloads, and what does not

Studio stays open while you work. Game-managed data, such as Orbit Break's tuning, can reload without losing the current run. Compatible native implementation changes can reload in the editor, and failed builds preserve the working library.

Arbitrary script and scene changes currently use a validated preview restart. Native registration or schema changes can require a controlled editor restart. **Universal zero-restart development is not claimed.** [Reload behavior](docs/HOT_RELOAD.md).

## Platform status

Windows is the verified desktop and portable-game target. Presets can be configured for Windows, Linux, macOS, Web, Android and iOS. Other targets still need matching export templates and, where applicable, SDKs and signing. A preset is not a verified device build.

Mobile and headset delivery have not been device-tested. The Rust VR and text modules remain placeholders. Aurum uses the existing Godot backend rather than a maintained engine fork. [Current status and boundaries](docs/STUDIO_STATUS.md).

## Verify and contribute

The full local gate copies the working tree to a temporary location before running tests:

```powershell
pwsh ./scripts/tests/verify_studio.ps1 `
  -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe `
  -NativeReload -ReleaseInstall
```

Add `-Offline` when Cargo dependencies are already cached. The gate covers strict Rust tests, formatting, Clippy, real scene persistence, headless gameplay, editor undo/redo, native reload recovery and a temporary installation. [Contributing](CONTRIBUTING.md).

## Documentation

- [Getting started](docs/GETTING_STARTED.md)
- [Daily workflow and project operations](docs/WORKFLOW.md)
- [Headless testing and platform exports](docs/AGENT_PLAYTESTS.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Verification record](docs/DELIVERY.md)
- [Status and limitations](docs/STUDIO_STATUS.md)
- [Engine modules](docs/MODULES.md)

The engine workspace also includes ECS, state/events, 2D/3D helpers, space flight, visual-novel interpretation and procedural content libraries. The original PowerShell helpers remain available for existing automation.

[MIT licensed](LICENSE). Portable game packages include the underlying runtime's license and third-party notices. Screenshots show the working application and included example; the banner is a repository-native SVG.
