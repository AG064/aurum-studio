# Integration verification, 2026-10-01

The six reported integration faults have implementations and local acceptance coverage: preview freshness, live properties/state-preserving rebuilds, export profile/notices preservation, rendered agent verification, explicit test budgets/diagnostics and Rust browser extensions. Additional acceptance failures identified and fixed cold asset inspection, blocked inspection during private exports, a missing 3D demo class declaration and slow Windows process identity queries.

## Checked locally

| Gate | Result |
| --- | --- |
| Rust workspace | 744 tests passed; strict all-feature/all-target Clippy and formatting passed |
| Runtime bridge | 21 typed-edit, refusal, collection and int64 checks passed |
| Orbit checkpoint | 10 state/entity/reference/RNG reconstruction checks passed |
| Browser integration | All eight scenarios passed; concurrent scene inspection during export was checked again after its lock fix |
| Browser workbench | Seven scenarios passed across scoped runs, including keyboard play, three-choice workshop, portrait/fullscreen layouts, live tuning, source save/undo, isolation and standalone play |
| Rust browser | A real compiled extension booted in Chromium with 50 native methods registered, no browser errors and cross-origin isolation enabled; managed SDK discovery was verified without `AURUM_EMSDK` |
| Agent/project interfaces | 40 actual CLI/MCP/HTTP/resource/package checks passed using the installed executable |
| Native editor | Create/save/undo/redo passed in a real editor process |
| Orbit gameplay | 85 gameplay/HUD checks and five winning twelve-encounter weapon/frame campaigns passed; stationary defeat and Windows packaging passed separately |
| Rendered agent capture | Installed executable produced real 960x540 PNGs at requested frames 30 and 60 |
| Budget diagnostics | Deliberately missing gameplay verdict failed with frame-budget exhaustion, sampled state and retained logs |

Browser results above are scoped acceptance runs, not a claim that one uninterrupted run or GitHub Actions is green. No test retry concealed a failing scenario. Aggregate UI-driver overhead was reduced by keeping a real keyboard range check and batching the remaining ordinary DOM range steps. Gameplay was not forced to win. Campaign verification supplies a 300-second wall budget separately from its frame budget.

## Installed Windows build

The installed executable matches the tested release:

```text
SHA256 F4B9B3D43FACECA05D33AAB09BC4CFBC9CEFDF6AFB65114B4C7083D3F3B140D5
```

The managed Web SDK uses Emscripten 3.1.74 and `nightly-2026-09-30`. Both ordinary and extension-enabled non-threaded templates are installed. Provisioning uses isolated Rustup state and disables Rustup self-update; global PATH/default compiler settings are not changed. Replaced application binaries are backed up and existing project/session data is preserved.

## Evidence and limits

Tests run in disposable projects and isolated data directories. Full local logs, receipts and images are retained outside the repository. Important receipts include `workspace-tests-complete.log`, `runtime-acceptance-import-gate.log`, `browser-import-lock.log`, `concurrent-inspection.log`, `workbench-release.log`, `standalone-complete.log`, `rust-managed-sdk.log`, `installed-project-final.log` and `installed-capture-final.json`. Winning campaign reports and later idle/package receipts are preserved beside their temporary runs. Public release evidence is generated separately against the versioned build; artifact hashes are release-specific.

This record describes the pre-release integration-fix pass. It is not a remote-CI verdict for v0.3.0. See [release delivery](RELEASES.md) and the linked GitHub check run for the published revision.

Checkpoint rebuilding boots a new game runtime. It is not arbitrary VM/native-memory migration. Native registration changes can still require a controlled editor restart. Rust Web support remains experimental and a Rust panic aborts the Web runtime. Platform-specific dependencies, Firefox/Safari, physical mobile devices, VR headsets, SDK signing and store delivery need their own acceptance. Project execution uses OS permissions, not a malicious-code sandbox. See [integration behavior](INTEGRATION.md) and [Rust target requirements](RUST_WEB.md).
