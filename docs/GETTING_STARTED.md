# Getting started

Launch Aurum Studio from the Windows Start menu or run `aurum`. The installed application includes its rendering runtime.

For installation prerequisites and a from-source setup, start with the [repository quickstart](../README.md#get-started-on-windows). For a complete playable project, open the included [Orbit Break example](../examples/orbit-break).

## Make a project

Use New project in the interface and choose a 2D or 3D starter. Both contain a runnable scene. Import accepts an existing project and adds Aurum configuration when needed.

```powershell
aurum new A:/Projects/MyGame --template 3d
aurum studio A:/Projects/MyGame
```

Open `main.tscn` in the Scene panel. The Files panel edits scripts and resource text. Unsaved file drafts are retained in the project, while Save applies a hash-checked change. Develop watches changes, Validate checks the project, and Game opens a preview.

Use Make start scene when another scene should launch first. Export creates a Windows package containing the runtime and required notices.

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

Native compilation requires Rust and the platform linker. Cargo resolves dependencies on the first build and preserves the resulting lockfile afterward. Compatible implementation changes reload; native registration changes require an exceptional editor restart.

The original engine helper remains available:

```powershell
pwsh scripts/build.ps1 -DebugBuild -RunEditor
```

Daily Studio and headless-agent workflows do not require that helper or a separate Godot application.
