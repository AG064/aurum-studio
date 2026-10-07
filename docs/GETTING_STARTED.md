# Getting started

Launch Aurum Studio from the Windows Start menu or run `aurum`. The installed application includes its rendering runtime.

For installation prerequisites and a from-source setup, start with the [repository quickstart](../README.md#get-started-on-windows). The published v0.3.0 includes [Orbit Break](../examples/orbit-break/README.md); current `main` also includes [Relay Yard](../examples/relay-yard/README.md) and the redesigned workbench described here. See the [documentation index](README.md) for other paths.

## Make a project

Use New project in the interface and choose a 2D or 3D starter. Both contain a runnable scene. Import accepts an existing project and adds Aurum configuration when needed.

```powershell
aurum new A:/Projects/MyGame --template 3d
aurum studio A:/Projects/MyGame
```

Open `main.tscn` in the Scene tab. Use Source for scripts and resource text. Unsaved drafts are retained in the project; Save checks the preceding file hash before replacing anything. Checks opens validation and bounded playtests; Output shows their diagnostics.

Select Run for an isolated browser preview. Source installations need matching Web templates first; the published portable ZIP includes them. Commands > Develop starts the native development watcher, and Commands > Run native opens a game window. You do not need to keep a Godot editor open.

Use Make start scene when another scene should launch first. Export creates a portable Windows package or standalone browser bundle with required notices. A browser bundle must be served over HTTP(S), not opened as a local file.

The Export panel also creates or reuses presets for Windows, Linux, macOS, Web, Android and iOS. Platform exports require their matching templates and SDK/signing setup. Studio displays the bounded backend log when an export fails.

## Connect an agent

Copy the configuration from Agents into an MCP client. The Studio profile provides project queries and actions without a visible editor. Read-only access is available. No model provider, paid service, or API key is built into Aurum.

See [WORKFLOW.md](WORKFLOW.md) for request examples and client configuration commands.

## Native code

The default starters need no Rust toolchain. For a Rust extension skeleton:

```powershell
aurum new A:/Projects/NativeGame --template minimal --engine A:/Source/aurum-studio
aurum dev A:/Projects/NativeGame
```

Native compilation requires Rust and the platform linker. Cargo resolves dependencies on the first build and preserves the resulting lockfile afterward. Compatible implementation changes reload; native registration changes can require a controlled editor restart. Browser code/scene changes boot a replacement runtime and restore compatible project checkpoints, not arbitrary memory. See [reload boundaries](INTEGRATION.md).

The original engine helper remains available:

```powershell
pwsh scripts/build.ps1 -DebugBuild -RunEditor
```

Daily Studio and headless-agent workflows do not require that helper or a separate Godot application.
