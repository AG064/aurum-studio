# Integration verification

## Normal native builds and portable package profile, 2026-10-13

The development build path now identifies the configured package and library
target from Cargo metadata and accepts only matching compiler artifact reports.
The manifest determines installation, not a guessed package-derived filename.
Both normal build and development supervision use the same bounded host/profile
resolver. The focused suite passed 671 Rust/doc tests and strict Clippy.

Expanded actual Rust acceptance verifies normal build into a custom manifest
filename, fresh gameplay through that library and standalone package execution.
An initial package run returned `build_revision:2`, `debug_library:true`,
`ok:false`: the full bundled runtime selected the debug mapping despite a
release build. Private package validation/export now places selected release
bytes at the runtime-selected paths. The next run passed 17 checks and executed
the release native library. The strengthened run passed all 18 checks,
including working debug-DLL preservation and private-source cleanup. Script-only
project/package acceptance passed 41 checks. This is headless Windows native behavior, not rendered
graphics or device acceptance.

The [first native Rust hosted run](https://github.com/AG064/aurum-studio/actions/runs/38337102459)
failed before private compilation while its fixture tried to generate a new
lockfile offline: a fresh cache lacked an unpinned Git branch reference. Setup
now seeds Aurum's pinned lockfile and resolves the fixture's package change
offline while asserting that the gdext revision remains pinned. Production
private compilation remains frozen/offline; no failed job was blindly retried.

## Private Rust native preparation, 2026-10-13

The [published lease/context source](https://github.com/AG064/aurum-studio/actions/runs/38332879800)
passed all 13 hosted jobs on `d7d2afb`, including three-OS tests/builds and all
runtime/browser gates. This does not publish a new binary release.

The next development slice passed 14 actual Rust native checks using the pinned
godot-rust dependency and a custom `cdylib` target name. The fixture starts with
an invalid working DLL, compiles privately, uses normal action input to invoke
a Rust method, pauses, changes Rust implementation, verifies checkpoint
continuity, observes the new constructor and invokes the changed method. A
compiler error and an external DLL path retain the accepted PID and state.
Working imports, DLL bytes, game script and Cargo.lock remain unchanged.

The first run exposed forward-slash versus backslash manifest comparison on
Windows. Resolution now uses confined canonical paths, with a regression that
also checks local Cargo source confinement. Library verification compares the
running game's loaded-manifest state and selected DLL/dependency hashes, rather
than treating a compiler exit as evidence of the executing build.

Existing native lifecycle (22), routing (16), recovery (11) and actual private
script preparation also passed. External workspaces, native revision
publication, shared build caching, cancellation and rendered/device proof
remain separate. Each native compiler has a finite deadline and an owned
Windows job; trusted build scripts/native code still use the user's permissions.

## Managed native sessions, 2026-10-13

Windows development source passed 653 focused Rust/doc tests and strict Clippy.
The two actual-Godot fixtures, ignored by default, were also invoked and passed:
authenticated native transport/freeze ownership and private preparation with
verified saved-data replacement. This is not a claim about the full workspace
on other operating systems.

Five executable native suites passed 75 checks through the compiled CLI, HTTP
and MCP interfaces:

- [Shared controller routing](../scripts/tests/native-routing-acceptance.mjs): 16,
  including the same PID across clients, reconnect, read-only refusal and
  explicit controller shutdown.
- [Persistent sessions](../scripts/tests/native-sessions-acceptance.mjs): 22,
  including source preservation, rejected-restore fallback, verified
  replacement and saved-data retention.
- [Real Relay Yard mission](../scripts/tests/native-mission-acceptance.mjs): 16,
  using ordinary start, movement, dash and pause input before replacement.
- [Host-crash recovery](../scripts/tests/native-recovery-acceptance.mjs): 11
  handoff-path and 10 checkpoint-only checks, including newer-source and
  corrupt-journal refusal.

Mission acceptance compares enemy identities/state, cooldowns, inventory,
progression, RNG, camera pose and projected view. Its failures exposed missing
camera checkpoint fields and paused-camera drift; both were fixed without
weakening those comparisons. Evidence is retained in disposable run directories.

The existing workbench and Relay Yard browser suites passed all 15 cases in one
zero-retry Chromium run, plus 13 HTTP/control helper checks. They cover real
inputs, live tuning, checkpoint rebuilding, invalid-build retention,
responsive/fullscreen layouts, standalone export, origin/message refusal and
actual agent contracts/receipts. Gameplay and pause screenshots were inspected.
Rendering used software Chromium; this does not qualify native graphics or
physical devices.

These are development-source results, not a new binary release. Native/Rust
dependency staging, in-flight cancellation, arbitrary concurrent save writers,
hardware rendering, mobile devices and headsets remain separate gates. Source
copies are not security sandboxes, and a complete custom checkpoint is a
declaration by the game's hooks, not independent proof of arbitrary memory.

## Agent context and contention follow-up, 2026-10-13

The scoped context implementation passed 18 compiled CLI/HTTP/MCP checks and
eight core regressions. Cases include same-size edits, creates/deletes, exact
file scopes, corrupt/foreign baselines, explicit cleanup, reconnect, permission
refusal and content-identified pagination. Delta inspection remains available
when the project configuration is invalid. The combined focused suite passed
663 Rust/doc tests and strict Clippy.

A logging concurrency failure retained seven complete records and suppression
summaries for only 55 of the remaining 83 lines. The final summary could lose a
lock race. Counts now survive failed window summaries; an EOF summary has a
finite contention-only retry budget. The 90-line accounting assertion was not
weakened. A deterministic lock-contention regression and all 25 compiled
diagnostics/preflight checks passed after the fix.

The [first native hosted run](https://github.com/AG064/aurum-studio/actions/runs/38329557365)
passed 12 of 13 jobs, including all Windows runtime/browser gates, but failed a
macOS immediate controller-lease reacquisition test. Explicit lease release and
a retained-descriptor regression address that failure. Hosted confirmation of
the follow-up is separate from these local results. Context payloads are
compact paths/hashes; token and latency improvements have not been benchmarked.

The [follow-up hosted run](https://github.com/AG064/aurum-studio/actions/runs/38330991383)
confirmed macOS controller-lease tests but exposed immediate candidate-pool
lease reacquisition on Linux. Pool/workspace leases now release explicitly too.
All nine candidate-pool regressions, strict core Clippy and the actual private
native-preparation fixture passed locally after that change. The targeted
browser contract case also passed independently after adding workspace
navigation and assertions for the new context schema. Its earlier isolated run
had depended on navigation performed by previous gameplay cases, not a product
failure. The next hosted run must qualify these follow-ups.

The [next hosted run](https://github.com/AG064/aurum-studio/actions/runs/38332097138)
passed Linux but exposed the same immediate reacquisition failure in a macOS
saved-data store test. An audit found the remaining raw scoped leases in
saved-data and diagnostic storage. They now explicitly release on normal and
error paths, including shared monitoring and cleanup. New regressions retain
duplicate descriptors across owner shutdown while checking successful
reacquisition. Current/active save generations remain protected. The combined
local suite passed 666 focused Rust/doc tests and strict Clippy. Compiled
logging (25), native-session (22) and host-recovery (11) checks also passed after
this change. Hosted confirmation remains a separate gate.

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
