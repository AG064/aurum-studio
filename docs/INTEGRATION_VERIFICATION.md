# Integration verification

## Development verification, 2026-10-07

The redesigned Studio was checked against a freshly compiled Rust backend, not only Claude's stand-in server. Across the bounded browser runs, 12 distinct scenarios passed: isolated export, actual keyboard play/live edits, workshop behavior, responsive/fullscreen handling, source save/undo and failed-build preservation, hostile-origin/message refusal, keyboard Source navigation, standalone export, ordinary 2D projects, actual operation-contract rendering and headless execution receipts with malformed-input refusal.

The original browser selectors were updated for the new ARIA tabs and Source label without weakening workflow assertions. Initial failures and traces were retained; the new headless-receipt test's expected status wording was corrected to the actual UI label. No fixture icons or server were shipped.

Strict Rust workspace tests passed 758 assertions across 28 result blocks. Formatting and all-feature/all-target Clippy passed. A build-supervisor test was corrected to wait for an in-flight result before interpreting quiet output as idle; its started/finished equality check remains unchanged.

Relay Yard revision 2 independently passed 129 native assertions, native campaign/idle/input checks, three real browser workflow cases, standalone browser controls and an extracted Windows package campaign. These are local development results, not a published version bump or evidence that a new hosted CI run has passed.

## Repository and CI-harness follow-up, 2026-10-07

The repository pass corrected workbench onboarding, added task-oriented
documentation and troubleshooting, clarified release versus source installation,
and replaced the README workbench image with a real Relay Yard browser capture.
No Rust, app-interface or game-runtime source changed in this pass.

Eleven isolated documentation-checker tests and ten loopback HTTP/control tests
passed. The local Markdown check covers files, images and headings; it does not
fetch external URLs. The documented 3D creation, status and operation-discovery
commands were executed in a disposable project, and the CI/issue-form YAML was
parsed with duplicate-key checks.

Three Relay Yard browser scenarios passed in one uninterrupted, zero-retry run:
real keyboard/mouse controls, live tuning, paused combat reconstruction after
source rebuilding and resize/resume. Actual viewport captures avoid a separate
iframe-stability wait that stalled an earlier run. A newly added test helper's
initial assumption that every query included `ok:true` was corrected against
the real raw file/discovery contracts and covered by regression tests. Initial
failures and traces were retained.

All ten integration scenarios also passed in one uninterrupted local run with
the bounded request helper: ordinary exports/notices, typed live edits, same-size
source changes, automatic rebuilding, three external rebuilds with concurrent
scene inspection, invalid-build retention, incompatible-checkpoint rollback and
stopping during export. The two affected browser suites therefore passed 13
scenarios locally. This is local software-rendered Chromium evidence, not a
replacement for the next hosted CI verdict.

The [first hosted run after the Studio redesign](https://github.com/AG064/aurum-studio/actions/runs/37549544738)
passed ten jobs but failed Relay Yard's short freshness wait and the second
repeated external-build integration scenario. The former recorded a 38-second
HTTP response; its bounded wait is now 60 seconds. The latter timed out while
awaiting headless scene inspection after native export stages had completed.
New request deadlines and credential-free pending/completed diagnostics expose
that phase; they are not proof that the observed hosted concurrency stall is
fixed. Unrun serial successors are not counted as passes. Inspect the CI run
on the relevant commit for the current hosted verdict.

## Historical October 1 verification

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
