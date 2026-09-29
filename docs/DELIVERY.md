# Aurum Studio delivery

## Product contract

The owner opens Aurum Studio to select or create a project, develop, validate,
play, and export it. Agents use a documented standard MCP interface or the same
CLI and HTTP operations. Scene authoring, inspection, validation, and bounded
game runs work without a visible Godot editor. Godot remains an implementation
dependency that Aurum discovers and invokes.

Routine content edits keep Studio running. Rust builds ask Cargo about source
freshness, install verified artifacts, and publish reload requests. Native API
changes have an explicit restart boundary. No claim of universal zero restarts
or unrestricted Godot API coverage is made.

## Milestones

1. Repair build freshness, artifact names, installation/reload publication,
   failure exits, change classification, agent paths, and optional Toolkit setup.
2. Add shared project operations: confined files, headless scene transactions,
   inspection, validation, bounded play, export, and Godot class discovery.
3. Expose the same project operations through MCP, CLI, and Studio; complete
   project selection, creation, Develop, and diagnostic interfaces.
4. Make editor changes persistent and undoable, validate new projects and the
   real development loop, reconcile documentation, and update the local install.

## Verification and boundaries

All tests run in temporary copies and projects. Use regression checks for the
reproduced faults, full Rust tests and CI lint settings, real headless Godot
scene save/reopen and validation, MCP protocol checks, HTTP/UI checks, and live
reload evidence. Generated runtime packages, session data and raw machine-local
logs are kept outside version control.
Record remaining limits explicitly. Do not alter agent configuration without
an explicit client-install command.

## Version and example

Aurum Studio 0.2.0 was verified on Windows against Godot 4.7. The repository
includes the [Orbit Break source example](../examples/orbit-break), its
gameplay test suite, and [real application screenshots](media/README.md).

## Initial 0.2 publication outcome

- The publication gate passed 730 workspace tests and documentation tests with
  warnings treated as errors. CI and generated receipts report each run's count.
- Formatting and workspace Clippy passed.
- The publication gate passed 40 headless project checks, including persisted scenes,
  failed-batch preservation, start-scene selection, drafts, MCP permissions,
  project-bound HTTP requests, and a runnable Windows package.
- Live editor creation, save, undo, and redo passed against Godot 4.7.
- The actual Rust development controller changed three live fingerprints under
  one editor PID, preserved the working DLL during a compiler failure, recovered,
  and stopped its owned processes.
- The native starter built, validated, and ran in a temporary project.
- Browser checks covered project creation, scene editing, file save/undo,
  agent configuration, headless play, and draft recovery after reload.
- A temporary upgrade verified binary backups and left the user environment
  unchanged. The installed test application ran from its included runtime and
  created its first-launch welcome project.
- Orbit Break passed 31 gameplay checks through both the CLI and a standard
  stdio MCP client. A full moving-autopilot campaign won all five waves with
  ordinary player statistics; the same loadout without movement lost in wave four.
- An external tuning edit through Aurum was acknowledged by the running game
  without a process replacement, and that campaign then completed successfully.
- Platform preset creation was checked for six targets. Android export was
  exercised through Studio and reported missing export templates and SDK
  components. This is a verified diagnostic, not a delivered Android build.

## Reproduce the checks

```powershell
pwsh ./scripts/tests/verify_studio.ps1 -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe
pwsh ./examples/orbit-break/tools/verify.ps1 -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe -Package
```

Each command reports a unique temporary evidence directory. Receipts contain
the checks, outcomes and log locations for that run. The example verifier can
copy a passing package to a new explicit `-OutputDirectory`; it never overwrites
an existing build. The install receipt records executable/runtime hashes, and
replaced binaries are retained in backups.

The deterministic gameplay suite directly drives collisions and state
transitions. The initial two full-campaign scenarios used the ordinary game loop.
Neither substitutes for hardware testing or a broad human usability study.

Windows is the verified target. Mobile, VR hardware, signing and device
performance remain unverified. Script/scene edits still have managed preview
restart boundaries; native schema changes may need an editor restart.
The publication does not include unrelated consumer game repositories,
machine-local session data, or automatic agent-client configuration changes.

## Browser workbench update

The first selected design is implemented as a compact workbench, with an actual
Wasm game in the central canvas and acknowledged live tuning in the inspector.
The README now uses the working interface rather than a decorative banner.

The updated Orbit Break suite has 47 checks. Its ordinary-loop campaigns won
with Pulse (72 simulation seconds), Lance (67) and Arc (110), passing through
four workshops and all three boss phases. The stationary run lost in wave five.
These numbers describe deterministic regression scenarios, not user benchmarks.

The browser tests verify real loading, input, live state preservation,
workshop purchases, file save/undo, failed-build retention, control-origin
isolation, responsive access and standalone export. They exposed and led to
fixes for truncated Windows socket writes and a save/undo UI race.

GitHub's Linux lifecycle stress reproduced a separate child-launch identity
race. The fix passed 100 iterations without retries, and the Windows runtime,
editor and packaged-game CI checks passed. The current workflow adds the
Chromium acceptance job with retained screenshots and traces. The workflow
badge and its run-specific artifacts remain the source of truth for each commit.

[Browser architecture and reproducible commands](WEB_PREVIEW.md) and
[design QA](../design-qa.md) document the current scope. This work does not claim
arbitrary state-preserving script replacement, native Rust web-extension
support, mobile/headset validation, or public hosting deployment.
