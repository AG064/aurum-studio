# Release delivery

## v0.3.0

[Download the release](https://github.com/AG064/aurum-studio/releases/tag/v0.3.0).

- `Aurum-Studio-0.3.0-windows-x64.zip`: portable Studio, CLI/MCP, Godot 4.7 runtime, ordinary and extension-enabled Web templates, notices, a copy of Orbit Break and reproduction/setup scripts.
- `Orbit-Break-0.3.0-windows-x64.zip`: standalone Windows game. Extract and open `Play Orbit Break.vbs`.
- `Orbit-Break-0.3.0-web.zip`: standalone browser game. Serve over HTTP(S) with the Wasm MIME type; double-clicking `index.html` is not supported.
- `SHA256SUMS.txt` and `verification.json`: file hashes and the versioned acceptance summary.

Extract into a writable directory. Open `Launch Aurum Studio.vbs` to start the workspace. Projects and sessions stay local to this installation; extracting a release does not change global PATH or migrate an existing installation. Use `bin/aurum.exe` for the CLI or an MCP client. No Rust compiler is required for script-only projects.

Rust projects need Rust/C++ build tools. Browser extensions additionally need the optional Emscripten SDK and dated nightly compiler. Run the included `scripts/provision-web-toolchain.ps1` with a dedicated destination under `runtime/web-toolchain`; see [Rust Web builds](RUST_WEB.md). The SDK is not bundled to keep the application download smaller.

## Verification and scope

The release is built from its tagged source, not from an untracked local executable. Release files are prepared and tested in temporary directories. `verification.json` records the release binary/runtime hashes and acceptance scope. GitHub check status is linked from the release.

Windows x64 and Chromium are the distribution acceptance targets. Linux/macOS builds are checked in CI but are not packaged desktop releases. Physical mobile devices, VR headsets, signing, notarization and app stores are not certified by this release. Live values apply without a game restart; changed code is loaded through checkpoint rebuilding. Native registration changes retain their controlled restart boundary.

Source-built packaging is available through `scripts/package-release.ps1`. It refuses an existing output directory and does not overwrite user projects or state.
