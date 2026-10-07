# Aurum Studio status

Version [0.4.0](RELEASES.md) includes the redesigned workbench, shared operation-contract discovery, structured MCP results and Relay Yard: Night Shift, a complete small 3D action-extraction game with modeled assets, combat, audio, a Warden encounter and combat-state rebuilding. See [agent workflows](AGENT_WORKFLOWS.md).

The supported workflow is Windows Studio plus local MCP and CLI access, using the bundled Godot 4.7 runtime. Studio owns the workflow; an open native editor is optional.

## Delivered

- Browser interface with project creation/import/selection, files, scenes, agents, validation, development watching, and Windows packaging.
- Development workbench with Scene/Source/Agents/Export tabs, a live inspector, source/draft status, Output/Checks dock, on-demand operation contracts and session execution receipts.
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
- Isolated Web previews with source revisions, generic live properties, checkpoint-based rebuilds, preserved export profiles and standalone browser export.
- Shared headless and rendered playtests with input timelines, PNG captures, sampled state, configurable budgets and explicit termination diagnostics.
- Explicit Rust Web side-module builds and private host-registry staging. See [target requirements](RUST_WEB.md).
- The [Orbit Break campaign](../examples/orbit-break), with 85 gameplay/HUD checks, twelve encounters, three stations, three frames, seven patrol types, three phased bosses, five winning campaign scenarios and three-choice module stops.
- [Relay Yard: Night Shift](../examples/relay-yard/README.md), with an authored 3D model kit, complete combat/extraction mission, 129 native assertions and physics-driven campaign/failure checks.

## Evidence

The previous investigation reproduced stale native builds, lost scene nodes, incorrect bridge paths, and a broken clean checkout. Delivery tests address those faults through the actual user-facing interfaces.

`scripts/tests/verify_studio.ps1` snapshots the current working tree into a temporary directory and runs formatting, strict workspace tests, Clippy, real headless project acceptance, and live editor acceptance. Optional switches include native development reload/recovery and a release installation.

Individual acceptance scripts:

- `studio_project_acceptance.ps1`: save/reopen, failed batches, stale writes, undo, scripts, validation, gameplay, discovery, MCP, HTTP, and optional packaged-game execution.
- `studio_editor_acceptance.ps1`: create, save, undo, redo, and save again in a real editor process.
- `studio_dev_acceptance.ps1`: source edits reach one running editor; a compiler failure preserves its working DLL and the watcher recovers.
- `runtime_bridge_acceptance.ps1`: typed edits, secret/traversal refusal, batch prevalidation, int64/collection round trips and complete Orbit gameplay checkpoint reconstruction in private fixtures.
- `examples/orbit-break/tools/verify.ps1`: isolated game validation, 85 gameplay/HUD checks, five winning weapon/frame scenarios, stationary mission failure and optional portable packaging. Supply `-GodotBinary` when a bundled runtime is not discoverable.
- `examples/relay-yard/tools/verify.ps1`: isolated MCP authoring/persistence, source save/undo, 129 native assertions, combat campaign/failure, rendered inputs and optional extracted Windows package execution.
- `scripts/browser`: real Chromium/Wasm play, keyboard controls, live-edit state preservation, save/undo, failed-build preservation, origin isolation, mobile-layout and standalone-export tests. See [browser setup](WEB_PREVIEW.md).

Verification receipts are generated beside each temporary run. Historical August Phase 0 and September prototype evidence remains historical; it does not replace these current gates.

The [integration verification record](INTEGRATION_VERIFICATION.md) separates October 7 development results from historical October 1 evidence. [Release delivery](RELEASES.md) distinguishes versioned package contents and limits. For hosted results, inspect [CI on the relevant commit](https://github.com/AG064/aurum-studio/actions/workflows/ci.yml); local receipts do not imply a green hosted run.

## Boundaries

- The application is a local browser interface with a Windows launcher, not a fork of the Godot editor.
- Windows is the verified distribution target. Other operating systems require their own runtime and installation acceptance.
- Routine content and compatible native implementation work keeps Studio open. Native registration/schema changes can still require an editor restart.
- Gameplay previews may restart to load changed code or content. Arbitrary game-memory migration is not promised.
- The headless Rust simulation is distinct from the running game's process.
- VR/text genre modules and compile-time module feature selection remain separate engine work, not implemented Studio controls.
- A full runtime is included in portable packages, so they are larger than exports using stripped templates.
- Configured export presets do not imply matching templates, SDK installation, signing or a tested mobile/headset build.
- Rust browser previews require the Emscripten SDK, nightly compiler and extension-enabled templates. Platform-specific libraries still need compatible Web implementations.
