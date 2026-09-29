# Aurum Studio 0.2 status

The supported workflow is Windows Studio plus local MCP and CLI access, using the bundled Godot 4.7 runtime. Studio owns the workflow; an open native editor is optional.

## Delivered

- Browser interface with project creation/import/selection, files, scenes, agents, validation, development watching, and Windows packaging.
- Shared Rust project operations for CLI, HTTP, and MCP.
- Headless scene creation/editing/inspection through native resource APIs, with atomic commits, stale-write protection, and saved-file undo.
- Live editor scene ownership and undo/redo, including save/reopen persistence.
- Cargo freshness checks, verified atomic DLL replacement, reload markers, accurate failure exits, declaration-aware restart classification, and deleted-file handling.
- Shared development supervisor with optional editor/game launch and managed preview refresh.
- Compact MCP Studio profile, read-only enforcement, runtime class discovery, and explicit client configuration generation.
- Runnable 2D/3D templates and an advanced native skeleton.
- Portable Windows game packaging with runtime and license notices.
- Local installation with a bundled runtime, preserved state, and backups of replaced binaries.
- Explicit test scenes, game arguments, fixed-rate play and fresh JSON gameplay verdicts across CLI, HTTP and MCP.
- Preset creation for Windows, Linux, macOS, Web, Android and iOS, with bounded diagnostics in Studio.
- Isolated in-workspace web preview, acknowledged live tuning and standalone browser export for script projects.
- The source-complete [Orbit Break example](../examples/orbit-break), its 47 gameplay checks, three winning weapon campaigns, stationary losing scenario, salvage workshop and phased boss.

## Evidence

The previous investigation reproduced stale native builds, lost scene nodes, incorrect bridge paths, and a broken clean checkout. Delivery tests address those faults through the actual user-facing interfaces.

`scripts/tests/verify_studio.ps1` snapshots the current working tree into a temporary directory and runs formatting, strict workspace tests, Clippy, real headless project acceptance, and live editor acceptance. Optional switches include native development reload/recovery and a release installation.

Individual acceptance scripts:

- `studio_project_acceptance.ps1`: save/reopen, failed batches, stale writes, undo, scripts, validation, gameplay, discovery, MCP, HTTP, and optional packaged-game execution.
- `studio_editor_acceptance.ps1`: create, save, undo, redo, and save again in a real editor process.
- `studio_dev_acceptance.ps1`: source edits reach one running editor; a compiler failure preserves its working DLL and the watcher recovers.
- `examples/orbit-break/tools/verify.ps1`: isolated game validation, 47 behavior checks, three winning weapon campaigns, a stationary loss and optional portable packaging. Supply `-GodotBinary` when a bundled runtime is not discoverable.
- `scripts/browser`: real Chromium/Wasm play, keyboard controls, live-edit state preservation, save/undo, failed-build preservation, origin isolation, mobile-layout and standalone-export tests. See [browser setup](WEB_PREVIEW.md).

Verification receipts are generated beside each temporary run. Historical August Phase 0 and September prototype evidence remains historical; it does not replace these current gates.

## Boundaries

- The application is a local browser interface with a Windows launcher, not a fork of the Godot editor.
- Windows is the verified distribution target. Other operating systems require their own runtime and installation acceptance.
- Routine content and compatible native implementation work keeps Studio open. Native registration/schema changes can still require an editor restart.
- Gameplay previews may restart to load changed code or content. Arbitrary game-memory migration is not promised.
- The headless Rust simulation is distinct from the running game's process.
- VR/text genre modules and compile-time module feature selection remain separate engine work, not implemented Studio controls.
- A full runtime is included in portable packages, so they are larger than exports using stripped templates.
- Configured export presets do not imply matching templates, SDK installation, signing or a tested mobile/headset build.
- The browser preview currently requires script-only projects. Desktop Rust DLLs do not automatically become web-compatible extensions.
