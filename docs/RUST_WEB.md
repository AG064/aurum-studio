# Rust GDExtensions in the browser

Aurum builds a non-threaded Emscripten side module rather than trying to load a desktop DLL in WebAssembly. It stages a host library privately for Godot's importer and maps the Wasm library into the copied extension manifest. A running project's installed DLL is not replaced by browser preview builds.

## Explicit toolchain setup

Provision into a dedicated directory, not a drive root:

```powershell
pwsh scripts/provision-web-toolchain.ps1 -Destination A:/AurumStudio/runtime/web-toolchain
pwsh scripts/provision-godot.ps1 -Destination A:/AurumStudio/runtime -Mode WebTemplates
```

The SDK setup pins Emscripten 3.1.74 and a dated nightly Rust compiler with `rust-src` and the Emscripten target. Rustup state stays inside that directory. No global PATH, default compiler or environment setting is changed. Provisioning is explicit and can require substantial downloads.

Studio discovers its managed SDK under `AURUM_STUDIO_HOME/runtime/web-toolchain`. An external SDK can be supplied with `AURUM_EMSDK`; its sibling `rustup` and `toolchain.json` are used when present. Matching extension-enabled templates are discovered beside the ordinary Web templates, or through `AURUM_WEB_EXTENSION_TEMPLATE`.

For a project already configured with `rust_package`:

```powershell
aurum project . --request-json '{"op":"web_build"}'
aurum studio .
```

Run and standalone Web export build the extension automatically. The `web_build` receipt supplies artifact hashes and compiler-log paths. Cargo's locked dependency graph remains authoritative.

An optional portable profile can override the compiler and package feature selection:

```toml
[web]
toolchain = "nightly-2026-09-30"
features = ["godot/experimental-wasm", "godot/experimental-wasm-nothreads", "godot/lazy-function-tables"]
preset = "Browser"
```

Do not combine incompatible `api-*` features. Aurum's locked bindings use their existing API4.7 selection. Custom bindings need a project-specific feature configuration. Compiler option support is probed because recent Rust versions removed the older Emscripten exception-handling switch. The Web profile rebuilds `std` with `panic=abort` and `panic_abort` to avoid an incompatible Wasm exception-tag import. A Rust panic terminates this Web runtime; native exception recovery is not preserved across the target.

## Target boundaries

The integration supports non-threaded Web libraries. Other extensions in a project need their own project-local Web library mappings. Platform-specific OS calls, native-only dependencies, conditional class registrations and threaded code require a compatible implementation; an exporter cannot convert them automatically.

Extension builds use `web_dlink_nothreads_release.zip`. Serve their standalone files with the Wasm MIME type and cross-origin isolation headers:

```text
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

Studio provides those headers for managed previews. Ordinary script-only standalone builds do not acquire an isolation-header requirement solely from Aurum.

Godot-rust still describes Web support as experimental. The integration is not certification of every Rust dependency, browser, mobile device or VR headset. See the [godot-rust Web guide](https://godot-rust.github.io/book/toolchain/export-web.html) and [Godot Web export documentation](https://docs.godotengine.org/en/stable/tutorials/export/exporting_for_web.html).
