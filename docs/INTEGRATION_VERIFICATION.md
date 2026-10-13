# Integration verification

## Read-only target-preflight gate, 2026-10-14

The [preflight fixture](../scripts/tests/target-preflight-acceptance.mjs) prepares
ephemeral complete, missing, mismatched and unknown SDK/JDK/template metadata
trees. It uses the supplied Godot 4.7 Windows version resource and a fake
unknown-version runtime to verify explicit-path authority, uncertainty and
strict rejection of unknown flags, credentials and remote/device requests.
Keystore and preset-credential sentinels must never appear in responses.

Actual CLI `--read-only` reports must create no controller, diagnostic,
baseline, import or private-workspace state. HTTP and read-only MCP must match
the CLI reports exactly and preserve all bytes/directories after explicit
transport-host setup. Entire protected project, SDK, JDK, template, key, user,
state and temporary trees are fingerprinted around those queries.

```powershell
node scripts/tests/target-preflight-acceptance.mjs C:/Builds/aurum.exe C:/Aurum/runtime/Godot.exe
```

The compiled gate passed 96 checks through the actual read-only CLI, HTTP and
MCP interfaces. The same immutable Windows binary then passed 22 native-session
checks, including compatible replacement, checkpoint equality and confirmed
retirement of the previous process. Its SHA256 is
`0E1720D1563473A711DF551C61E9C488FB4B3A4046EA95C9DD3167F86999466F`.
The integration source passed 773 focused Rust/doc tests before two loop-only
PE-reader lint repairs; afterward, 18 metadata unit tests, strict Clippy and the
CLI build passed before these compiled gates.

Receipts are retained under `C:/Temp/aurum-roadmap-01a13506/`:

- `target-preflight-native-tests-strict/aurum-target-preflight-1384yz/verification.json`
- `target-preflight-native-session-tests/aurum-native-acceptance-6ba51k/verification.json`

Two fixture failures remain preserved. The first stopped after three checks
because the synthetic SDK omitted the required `android.jar`. The next passed
53 CLI checks, then could not read Studio's exclusively held empty Windows
lease while taking the transport baseline. The repaired inventory retains that
entry, derives its empty SHA256 and verifies bigint file identity, size and
timestamps before and after observation. Seven controlled lock checks prove
that nonempty read errors, links and metadata drift remain failures. No trees
or assertions were skipped.

The [732befb hosted run](https://github.com/AG064/aurum-studio/actions/runs/38475108342)
passed its other 12 jobs but failed the earlier compatible HTTP replacement
assertion without retaining the returned response. Its original backend cause
remains unavailable. Twelve controlled diagnostic-helper checks and a separate
22-check EF2 native-session run qualify improved failure receipts; the new
binary's 22-check pass does not establish the earlier failure's cause.

A report can expose missing installed prerequisites while keeping export,
signing, deployment, input/performance and physical-device readiness unverified.
SDK/JDK/template trees in this gate were synthetic; only the supplied Godot 4.7
Windows version resource was real runtime evidence. No SDK installation or
device action belongs to preflight. The published job/package 746/158/31
qualification below remains its own boundary. Hosted confirmation of this new
target-report slice is pending.

## Shared background-job gate, 2026-10-13

The [compiled jobs fixture](../scripts/tests/jobs-acceptance.mjs) uses an
ephemeral shared Studio controller, actual CLI/HTTP/MCP clients and controlled
owned Godot work. It holds both manager slots while respecting the serial
runtime gate, saturates eight queue slots, expires a queued mutation, and
cancels both an unspawned gate waiter and a witnessed active worker. Polling,
read-only and foreign-capability refusal, disconnect/reconnect identity,
bounded source-free summaries and result retrieval/hash contracts are checked
through the public adapters.

Small results are retained, results over 32 KiB retain only complete hashes,
and results beyond the 2 MiB streaming budget require an explicit incomplete
marker. The fixture also checks 64-terminal retention, explicit forget and
controller-crash recovery without request replay. Its actual native replacement
cancellation must retain the accepted PID/checkpoint and working source/DLL
bytes while the prepared process terminates. An unrelated process is witnessed
before and after cancellation and host interruption.

An actual export-pack job must publish `dist/job-export/game.pck` with a verified
commit and complete-directory receipt after synchronous preset configuration.
Another request into that existing parent must fail without claiming a commit
or modifying the first pack. Background export requires a fresh output parent
so pack, executable and platform sidecars share one publication point.
The working source, generated UID-file inventory and import-cache hashes are
captured after preset configuration and must remain unchanged during both
successful and refused asynchronous exports.

The expanded fixture now submits an asynchronous package to fresh
`dist/job-package`, requires a committed/verified receipt and nonempty regular
`game.pck` plus bundled executable, then runs that executable headlessly for two
frames with a finite deadline and no runtime error tags. Working source, UID
and cache inventories must remain unchanged.

Package/storage qualification on 2026-10-14 passed all 158 expanded fixture
checks against binary
`EF2B0557388F2C26141D3C5CD25881762E18799C4F7272183FAD1A19466B27C4`.
The source snapshot passed 746 focused Rust/doc tests, strict Clippy and CLI
build, including package precondition/stage checks and dual-root storage
regressions. The package job succeeded, committed with verified status and
cleaned its private source. Its nonempty regular pack and executable loaded
headlessly for two frames, exited zero, and emitted no runtime error tags.
Working source, UID and import-cache inventories stayed unchanged. Pack SHA-256
was `b4a726df89cf43dc6945922f917b26c2d6e6aba06c5d5492b38e7c1b3c2e7864`;
bundled runtime SHA-256 was
`b2ca888d5115a6cedee564764a2ee494a625f2ec2edbabd010fe33c9a88a6bf8`.
The portable Cargo gate also passed 31 checks on that same binary, with both
cancelled/deadline workers confirmed gone, uncommitted, verified and without
recovery requirements.

The first expanded run reached committed package publication, then stopped
after 152 checks because the fixture compared `C:/Temp` with Rust's canonical
`C:/temp` spelling. Both directory and executable resolved to the same physical
paths. The repaired assertion uses realpath identity and retains regular-file
and expected `game.exe` checks. The first committed result was recovered from
its public fields and verified byte-for-byte against the persisted result
hash; the failure, body and logs remain preserved. Only the fixture comparison
and immediate package-result retention changed before the passing rerun.

```powershell
node scripts/tests/jobs-acceptance.mjs C:/Builds/aurum.exe C:/Aurum/runtime/Godot.exe
pwsh scripts/tests/cargo-jobs-acceptance.ps1 -AurumBinary C:/Builds/aurum.exe -Mode Both -TempRoot C:/Temp/aurum-cargo-tests
```

The previously qualified corrected-argv0 Windows binary
`1D2B6E9B7A22940B0FC6214E4DB24D2AC48F3A61781FEF9F70AD2CC143954BB3`
passed all 152 job/Godot fixture checks. Both witnessed cancellations remained
uncommitted with verified commit status, and export-pack publication committed
successfully while working source, UID files and cache inventories stayed
unchanged. Its exported pack hash was
`4b4d7728a2e4407f9dcc4a79e7d7570bb5eaa96d9ba077ead1247f8648cf5067`.
That earlier source snapshot passed 739 focused Rust/doc tests, strict Clippy
and CLI build. The later package confinement, recovery-state, PCK-kind/size and
aggregate staging-storage changes were qualified by the 746/158/31 gates above.
The preserved 152/31 results below apply to the earlier snapshot.

The portable [real Cargo gate](../scripts/tests/cargo-jobs-acceptance.ps1)
passed 31 checks on that same binary. It used actual offline dependency-free
Cargo builds, witnessed build-script PID and creation identity before both
explicit cancellation and deadline expiry, and confirmed their termination.
Receipts ended `cancelled` and `timed_out`, uncommitted, verified and without
recovery requirements. Seven configuration/source/manifest/DLL/lock hashes
remained unchanged; an unrelated process stayed alive. Four privacy assertions
also verified omission of private capabilities from summaries and public
receipts. This is compiler-process evidence; game-state behavior is covered by
the separate 152-check Godot fixture. A preceding local Cargo witness passed
27 checks before the portable script added those privacy assertions.

The earlier Windows qualification binary
`4DBAB5A766D9CDD4AD4CEB88AA895BCD3CD680D5DEBC1777DA3A71026C906721`
passed all 152 fixture checks. The receipt confirmed both active validation
and prepared native replacement cancellation as `cancelled`, uncommitted and
commit-status verified. Export-pack publication succeeded with a verified
commit; source, UID and import-cache inventories remained unchanged. The
published pack hash was
`29b13d6e6521f498bd161daec4ea5274e484edabda3750a650e7ddd13d7b176a`.

The initial run stopped after 36 checks when a suppressed `Get-Process` lookup
for an already terminated PID returned PowerShell exit code 1. Exact-PID CIM
queries now return a successful explicit null for absence and retain errors
for failed queries or inaccessible identity fields. Only the fixture helper
changed before the passing run; its source hash was
`C74037E6950FE6CB60E0A41639FF72BDC661874167C506165E24CC7D30597E0C`.
The failed receipt, reproduction and full logs remain preserved beside the
passing receipt under the ephemeral `aurum-roadmap-01a13506` evidence tree.

The earlier binary's real Cargo witness exposed the installed rustup shim being
invoked under its canonical executable name. Owned spawning now retains the
original invocation argv0 while using the canonical executable and identity
for process ownership; the corrected-argv0 snapshot gates above cover that path.
Sources, caches, state, process markers and evidence stay
in ephemeral storage. The job receipt store
is `.aurum/background-jobs`; the existing scene worker staging directory
`.aurum/jobs` retains its separate purpose. Runtime/dependency context and real
model-token, latency and task-success benchmarks remain required roadmap work.

## Contained nested Cargo workspace gate, 2026-10-13

The [nested workspace fixture](../scripts/tests/native-workspace-acceptance.mjs)
uses `godot/project.godot` beside a `rust/` virtual Cargo workspace. Its configured
member declares the renamed `nested_game_logic` library and calls a local
support crate through an actual native method. The fixture seeds its nested
lockfile from the repository, resolves local package changes offline and checks
that every external dependency retains its pinned version/source identity.

The gate covers real native start and checkpoint capture, stale-source refusal,
invalid and missing required member source, private candidate validation,
loaded-library hashes, source-only publication/history/undo and native successor
activation. Actual Rust constructors and methods must execute while working
DLLs, both lockfiles, Cargo target sentinels and Godot cache inventories remain
unchanged. Missing, absolute, parent-escape and real Windows junction hints must
refuse replacement and retain the accepted PID/checkpoint.

```powershell
node scripts/tests/native-workspace-acceptance.mjs C:/Builds/aurum.exe C:/Aurum/runtime/Godot.exe
```

Runtime CI invokes this gate sequentially after the existing Rust fixture and
uses the same locked dependency-cache provisioning. All project sources,
targets, Studio state, junction targets and evidence are ephemeral. The compiled
Windows gate passed all 49 checks, including two linked source changes and their
combined native behavior after replacement. The full focused suite passed 687
Rust/doc tests and strict Clippy. Existing root-Cargo Rust acceptance passed 38,
script/resource candidates passed 60 and diagnostics passed 25. Truly external workspace
staging, shared compilation caching, background jobs/cancellation, devices and
the binary release remain required roadmap work.

The first nested start caught a traversal timing refusal after 498 ms with only
85 inspected entries. Storage traversal now has a finite two-second maximum;
running commands and readiness scans clamp it to their remaining deadlines.
Zero remaining budget and late traversal success are refused. The 1 GiB byte,
50,000-entry, nesting and link guards remain unchanged. A later root-Cargo
regression refused publication when A: ran out of space; its failed candidate
was cleaned and the complete regression passed in a fresh C: temporary area.

## Self-contained native source revisions, 2026-10-13

Current development source privately compiles configured Rust/GDExtension
candidates before Godot import/resource validation. The same `changes_validate`
and `changes_apply` operations used for script projects produce bounded source,
proposal and candidate fingerprints plus the generated library's path/hash.
Publication rechecks those identities under the content lease, journals the
requested text and supported UID sidecars, and excludes compiled native bytes
from publication and whole-revision undo.

The [actual Rust fixture](../scripts/tests/native-rust-acceptance.mjs) now
exercises compilation failure with accepted-PID/checkpoint/source/DLL retention,
stale-source refusal, successful private validation and cleanup, source-only
publication, DLL-free revision history and undo preserving a real installed
library. It reuses the existing normal build for that installed-library check
while the managed game runs from its private copy, then verifies source
republication, native method behavior, normal play and package execution.
Compiled-library tampering and source/candidate fingerprint mismatches also
have core regression coverage.

The final Windows continuation passed 678 focused Rust/doc tests, strict Clippy,
60 script/resource revision checks, 38 actual Rust build/revision/handoff/package
checks, 22 persistent-session checks and 16 shared-controller routing checks.
The ignored actual private-native preparation test passed explicitly, including
confirmed retirement of the original process. Source snapshots, fixtures,
compiler output and verification evidence stayed in ephemeral storage.

One initial Rust start was refused by the bounded storage-inspection guard
before a game was created. The error now distinguishes elapsed-time and
entry-count limits; the 250 ms, 50,000-entry, depth, byte and link guards remain
unchanged. The complete fresh-root Rust run passed all 38 checks. Hosted CI
must still be checked for the exact published commit.

The [first continuation CI run](https://github.com/AG064/aurum-studio/actions/runs/38394938164)
passed actual native preparation, retirement, sessions and routing, then failed
offline Rust fixture metadata because its fresh Windows cache lacked the pinned
Linux-only `libc` dependency. Runtime CI now provisions all locked dependencies
with `cargo fetch --locked` and verifies the lockfile hash before offline
acceptance. An empty ephemeral Cargo cache passed that fetch and the same
offline fixture metadata with gdext `107a87e3` and `libc 0.2.189`; the repository
lockfile remained unchanged. Production private compilation stays frozen/offline.

The [cache-provisioned run](https://github.com/AG064/aurum-studio/actions/runs/38396470681)
passed that provisioning step but stopped at the diagnostics fixture's MCP
response-count assertion. The fixture inspected output at process exit; it now
waits for stdout/stderr closure and retains the exact four-response JSON-RPC
predicate. The copied diagnostics suite passed all 25 checks, and 30 additional
real MCP exchanges returned all 120 expected responses after closure. Server
protocol and tracing code did not change.

```powershell
node scripts/tests/native-rust-acceptance.mjs C:/Builds/aurum.exe C:/Aurum/runtime/Godot.exe
```

The fixture needs Windows, a compiled current `aurum`, Godot 4.7, Cargo/rustc and
the repository-pinned godot-rust dependencies already available offline. It
creates its project, target directory, Studio state and evidence under an
ephemeral host directory. Fixture lockfile setup resolves its package change
offline without changing the pinned gdext revision; actual candidate builds
remain frozen/offline. External workspace staging, shared compilation caching,
background jobs/cancellation, rendered/device acceptance and a new binary
release remain separate gates. Trusted build scripts and native code retain
the user's permissions.

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
script preparation also passed. Native source publication is covered by the
development slice above. External workspaces, shared build caching,
background jobs/cancellation and rendered/device proof remain separate.
Each native compiler has a finite deadline and an owned
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

These are development-source results, not a new binary release. External
native/Rust dependency staging, in-flight cancellation, arbitrary concurrent
save writers, hardware rendering, mobile devices and headsets remain separate gates. Source
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
