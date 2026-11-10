# Aurum Studio roadmap

This is planned work, not a statement of released capabilities. Studio 0.4.0
remains the published baseline. Work is accepted by executable checks and real
workflow evidence, not by adding controls or export presets alone.

## Implementation stack

The orchestration, project operations, CLI, HTTP server, MCP server and engine
libraries are Rust. The browser workbench is HTML, CSS and JavaScript. Godot
itself is primarily C++, and editor/runtime adapters and the example games also
use GDScript. Rust game code uses GDExtension. Keep one Rust implementation of
each project operation across human and agent interfaces; do not rewrite the
interface or fork Godot merely to make the language list uniform.

## Next milestone: reliable iteration and agent autonomy

Public tracking: [GitHub roadmap issue #1](https://github.com/AG064/aurum-studio/issues/1).

The intended 0.4.1 scope is bounded diagnostics, atomic project revisions,
native checkpoint replacement and efficient agent sessions. These tasks are
sequenced because replacement and background jobs need trustworthy change and
diagnostic contracts. A version number is not a delivery guarantee.

The 0.4.1 Windows package must include `Aurum Studio.exe` as the direct
double-click entry point, with its runtime and supporting CLI beside it.
Installation shortcuts must launch that executable. Release verification must
cover a fresh extraction, relocation, paths containing spaces and launch from
an unrelated working directory. Source publication alone does not deliver this
user-facing executable.

The frozen Windows 0.4.1 delivery is locally verified and installed. The unchanged
launcher fixture passed all 51 emitted checks, covering 38 required semantics
and ten owned cleanup records. It verified matching GUI/CLI/Godot files across
four contexts, four Web templates in both package contexts, and a 179-entry ZIP
with a fresh extraction. Strict GUI Clippy with `-D warnings` passed without
diagnostics, and all seven browser-policy tests passed.

The installed executable and direct shortcut with no arguments were verified,
and the installed nonlaunch `--check` passed at 0.4.1. Eight replaced managed
files have verified backups; Studio user state and the user environment remained
unchanged. The installed GUI was not launched. Browser routing was qualified
through an optional isolated Chromium/profile configuration. OS-default routing,
new rendering evidence, signing, public release, physical devices and representative
model benchmarks remain separate gates. These results cover the frozen delivery,
not later unqualified development edits.

An earlier 0.4.1 development candidate also passed local
portable-package verification. All 47 emitted checks passed, covering 38 required
launcher behaviors and ten cleanup records. The checks joined the GUI, CLI and
Godot files across four locations, including paths with spaces and Unicode,
and verified all 179 ZIP entries against the exercised fresh extraction. The
original runner closed with exit 0 after independent evidence review and disposal
of its owned resources within the original deadline.

This candidate is available locally; the installed frozen delivery above has
not been replaced. The run used an isolated Chromium profile. New visible
rendering, OS-default browser routing, signing, public release and physical-device
qualification remain open for this candidate.

The newer workspace-access helper filename fix passed 19 focused tests and
matching GUI/CLI version and nonlaunch checks. Its portable ZIP also passed
179-entry metadata/content verification and a fresh 178-file extraction, with
the full runtime and four Web templates. Package assembly queried Godot 4.7.
This verifies the matching archive; attended confirmation and visible startup
remain open for that build. See [release delivery](RELEASES.md) for its checksum.
Its matching version and nonlaunch checks also passed after relocation to a
path containing spaces and Unicode, from an unrelated directory. Independent
review verified all copied files and the three probes' normal exits and closure.

### 1. Bounded diagnostics

Status: published on `main`, with Windows acceptance and passing hosted
cross-platform CI. Not included in the 0.4.0 download; it ships in 0.4.1.

- Structured operation IDs, start/completion timestamps, duration, outcome and
  stable failure categories shared by CLI, HTTP and MCP.
- Size-based rotation, finite generations, bounded records and reads, session
  log retention, and explicit reporting when diagnostics cannot be persisted.
- No source text, request/response bodies, credentials or environment dumps in
  the operation journal. Verbose protocol tracing must not reveal payloads.
- Do not log successful polling requests by default. Never write diagnostics
  to MCP stdout. Measure overhead and concurrency rather than assuming it.
- Keep detailed run evidence separate. A logging cap must not silently delete
  game packages, checkpoints, process ownership, tokens or test verdicts.

Acceptance: concurrent writers produce valid records; rotation and retention
stay within documented limits; credentials and source text are absent; a full,
locked or unwritable log does not turn a successful operation into a failure;
CLI and MCP can query a bounded diagnostic tail.

### 2. Atomic project revisions and rollback

Status: in progress. Read-only `changes_check` and disposable `changes_validate`
are published on `main`. The current Windows script/resource slice includes
content leases, real Godot validation and journaled text publication/recovery/undo
with passing hosted CI. Candidate crash cleanup, shared storage reservations
and owned-worker lifetime controls also passed all 13 hosted CI jobs. Current
development source also privately compiles self-contained Rust/GDExtension
candidates before source-only publication and undo. Compiled candidate DLLs
are hash-verified and excluded from revision records, preserving working
libraries and the accepted native game. External workspace staging, shared
compilation caching and device/release qualification remain open.
Contained nested Cargo workspace support is published on `main` in `38e68fc`.
Its Windows fixture passed 49 checks, and
[that commit's hosted CI run](https://github.com/AG064/aurum-studio/actions/runs/38445290216)
passed all 13 jobs. It covers nested Godot roots, virtual workspaces, renamed
member libraries and copied local dependencies, with source/hint refusal and
working-cache preservation checks.

The private Windows external-source filesystem and retained-handle namespace
layer are integrated into durable journal I/O in development source. A matching
isolated 0.4.1 core build passed all 109 selected tests: 67 external-source tests,
40 exact regressions and two distinct real compile-first producer fixtures. These cover
linked create/replace/delete, interruption recovery, owned-directory retirement
and return, whole-revision undo/redo, newer-edit preservation, quotas,
cancellation, original deadlines and retained compiler ownership. All 90 raw
output streams joined their recorded hashes. The protected source, tools,
installed files and held inputs matched before and after the run. The original
runner closed with exit 0 after independent review and same-original retirement.

The compiler proof binds the staged envelope, workspace and member manifests,
selected source and lockfile to the returned artifact. The five added regressions
cover held-input identity, cancellation, cursor restoration and witness lifetime.
They accept equivalent Windows path spellings and refuse ambiguous, escaping or
mismatched input bindings. The real producer also rechecked its private typed
record and rejected seven controlled corruptions with every mutation restored.
Observed Cargo package, target, profile and feature records now join held local
manifest/source content. This passed 23 focused tests and a matching real
two-artifact fixture in a short temporary workspace, including retirement,
custody and preservation under its original 60-second limit.
A private fixed-slot DLL placement operation also passed 14 focused tests and a
matching real compiler fixture, with original custody/deadline checks, bounded
copying, same-writer hash readback, cursor restoration, retry refusal and cleanup.
A private quoted GDExtension binding plan passed 25 focused tests and a matching
real compiler fixture against the original held manifest and placed DLL.
Structural named-export checking then passed 22 focused tests and a matching
real fixture, confirming the direct `value` export and missing-name refusal
through that same retained DLL. Header aliases, malformed tables and forwarders
are refused; the returned name/RVA metadata grants no loading authority.
Dependency content closure, exact build units, full Godot configuration coverage,
export completeness, ABI compatibility and loading remain unverified.

A shared Godot management gate prerequisite passed nine focused tests. Its private
lease shares admission with existing workers, checks an absolute deadline and
cancellation, and can be retained while worker retirement is uncertain. Held
configured-engine identity subsequently passed eight focused tests and one real
compiler/custody fixture with the original runtime, read broker and controller.
The image stayed held through compilation and custody, then passed a final
identity/hash recheck. The fixture finished in 7.18 seconds within its original
60-second budget, preserved all four source roots and confirmed owned cleanup.
It did not execute Godot. Thirteen later focused ConfigFile checks passed,
including public-request staging with the granted dependency layout and refusal
before compiler allocation. The isolated run preserved all 182 recorded inputs
and snapshots and restored all 24 process environment entries. A matching real
public-request fixture then compiled the Rust candidate and ran the held Godot
image to compare the supported quoted ConfigFile subset, including the selected
library, entry symbol and observed engine tags. It passed in 6.30 seconds within
one original 60-second budget. Compiler and parser workers retired with joined
readers and both EOFs; both private workspaces were cleaned, all four source
roots were preserved, and the original controller closed with no retained
resources. The outer wrapper preserved its 17 known inputs, binary and held
engine image and restored its 17 process environment entries. This verifies the
restricted comparison path. ABI compatibility, loading, activation and source
publication remain unverified by this fixture.

The second real producer verified terminal artifact custody within one original
60-second budget. It moved the original artifact handle and five held provenance
inputs with the candidate workspace after unique compiler retirement. Four new
regressions and five controlled real-producer refusals checked owner, lease,
deadline, identity and binding changes. The workspace was removed after the held
artifact and inputs were dropped. Posttest PE replica bytes and the absence of
the reported original paths were checked separately; these observations do not
adopt the original native handles or qualify loading, relocation or publication.

Development source now retains the complete staged local-source inventory with
the real compiler witness through artifact custody. Six new custody regressions
passed. A separate matching build's real compile fixture checked 16 staged files
across three roots and 12 nonanchor original-handle observations within one
60-second budget. Its captured typed report was validated independently after
capture; the outer collector's failed runs remain recorded separately. These
checks cover staged source custody. Compiler read closure, exact build units,
loading and publication remain unverified. A Windows 0.4.1 portable candidate
containing this custody implementation has since been rebuilt and passed
launcher/package verification; that delivery check does not qualify the
remaining native proof.

Reported local build-output custody passed ten focused tests, including five
new regressions for output bounds, identity, cursor restoration and refusal
cases. Two preserved real-compile failures exposed a missing held build-directory
binding and an output reader that inherited the smaller source-file allowance.
The corrected collector binds both original directories, keeps the selected DLL
confined to target, and applies the separate bounded output allowance without
changing source quotas or the original owner, deadline and cancellation checks.
Its matching fresh build passed all ten focused tests. The real fixture then
held six reported outputs across two local build observations, including a
1,191,936-byte PDB, within the original 60-second budget in 7.138 seconds. The
compiler tree and readers retired. Workspace cleanup completed and the complete
four-root preservation checks matched before the native runtime closed. These checks
cover reported-output content custody; compiler read closure, exact build units,
loading and publication remain open. An updated Windows 0.4.1 portable candidate
includes this correction in the direct Rust Studio executable and CLI. Fixed-copy
hashes, AMD64 PE subsystems, both version probes and a layout check from an
unrelated private directory passed. The check confirmed the runtime was present
and returned before launch. Actual backend/browser UI, signing and export-package
verification remain open for this candidate.

The real producer compiled edited Rust with one local dependency and passed
exact-candidate admission, retaining the original checked DLL handle across
unique compiler retirement. Its complete fixture source, working DLL, user-data
and ungranted-root preservation checks passed. The original native budget was
180 seconds; a separate postcompile confirmation window was at most 60 seconds.
That confirmation uses private test authority. The fixture did not publish
source or load/activate the DLL. Complete external native delivery, GDExtension
dependencies/loading and attended publication remain open. Public `SourcePublish`
intake remains closed pending confined relocation/recovery and attended
broker/transport integration. These development-source results do not qualify
an updated installed package.
Hardware qualification and representative model benchmarks remain open.

The private compile-first publication path now connects its retained,
postvalidation candidate directly to the existing transaction engine. Eight
focused filesystem unit tests passed for publication, refusal, interruption,
fresh repair and undo. Their private test authority does not qualify attended
approval or native delivery; public `SourcePublish` intake remains closed.

The next implementation slices and lock/recovery boundaries are specified in
[project revisions](PROJECT_REVISIONS.md).

- Multi-file change sets with required previous hashes and create/delete
  preconditions, bounded file counts and sizes, and conflict receipts.
- Stage and validate the complete candidate in a disposable project copy.
- Serialize participating writers and publish only a validated candidate.
- Durable transaction journal, interruption recovery and whole-revision undo.
- Preserve external edits; refuse rollback if files have since changed.

Acceptance: linked script/scene/resource changes apply together; a stale hash,
invalid candidate, interrupted publication or failed rollback retains or
recovers the working revision without overwriting unrelated work. Define the
visibility boundary for external editors and watchers explicitly: independent
filesystem renames are not globally atomic.

### 3. State-preserving native replacement

Status: in progress; browser checkpoint replacement already exists. Current
development source includes private Windows native preparation, verified
replacement, failure fallback and persistent HTTP/MCP session operations.
Checkpoint/host-crash recovery and shared CLI/MCP/Studio controller routing are
implemented in development source. Self-contained native dependency staging is
also in development source. Shared jobs now provide qualified owned
cancellation paths. External native dependencies and final release
qualification remain open. This work is not in the 0.4.0 download; it ships in 0.4.1.

The opt-in private runtime bridge binds loopback on an ephemeral port and uses
a per-launch token, versioned JSON framing, a 1 MiB request/reply bound, four
connections, non-blocking chunks and finite connection deadlines. The Rust
client verifies game process identity before and after each call, rejects
foreign reply IDs/PIDs and future versions, and uses a total transport deadline.
Tokens never enter readiness files or transport error text. Ordinary game and
Web launches do not open this channel.

Native freeze requests have a finite lease, defaulting to 30 seconds. If the
controller disappears, the runtime restores its previous pause/render state.
Successful resume or restore clears the lease. This avoids leaving a game
permanently frozen while the replacement controller is still under development.

A real two-process 3D fixture transferred position, inventory, enemy identities,
cooldowns and mission progress through custom checkpoint hooks. It rejected bad
authentication, oversized framing and a future checkpoint without changing the
accepted state, and verified automatic unfreezing. This is transport/checkpoint
evidence, not general game-quality acceptance. The Godot-equipped CI gate runs
the same fixture explicitly.

The native session implementation validates a private source copy, boots it
paused, restores and recaptures the checkpoint, and proves compatibility before
retiring the original owned process. Rejected restoration resumes the old game.
Operation-owned freeze leases prevent another operation from accidentally
releasing or modifying a suspended run. `native_input` delivers registered
Godot actions inside the owned game, never OS keyboard input.

Complete explicit checkpoints and verified handoffs persist bounded recovery
records without Aurum transport tokens. `native_recover` checks recorded
host/game creation identities, refuses live or inaccessible processes,
validates the working source hash, and proves restoration in a fresh private
process before activation. It never signals a recorded PID or overwrites newer
working edits. Actual host
termination, checkpoint-only recovery, newer-source refusal and corrupt-journal
refusal are executable gates; recovery resumes stored checkpoint state, not
arbitrary memory or progress since that checkpoint.

Save data has a separate recoverable generation store. Replacement stages a
stable copy and changes the current generation only after verified activation.
Stop removes the temporary source/import copy, not user saves. Current or
actively leased save generations cannot be forgotten. Retention is bounded and
cleanup is explicit; it is observed/admission control, not a hard disk quota.
Drift checks can refuse active save writers, but do not provide an atomic
snapshot of arbitrary external writers. Previous generations remain retained.

CLI and MCP discover the same project-scoped Studio controller instead of
launching separate games. Starting or recovering a native session can start a
hidden controller; status queries never do. Routing validates process creation
identity, Windows listener ownership, authentication and the served project.
Client disconnect does not stop the game. `native_stop` stops gameplay, while
`native_host_stop` shuts down the controller and its owned game. See
[native session commands](HOT_RELOAD.md#managed-native-sessions).

Actual Relay Yard native mission tests exercise ordinary start, movement, dash
and pause input, then a compatible code edit and replacement. They check player,
enemy identity/state, cooldowns, RNG, inventory, progression, camera pose and
projected view. This exposed and fixed missing camera checkpoint state and
camera drift during mission pause. Headless mission proof does not qualify a
graphical window, headset, mobile device or arbitrary third-party game.

Self-contained Rust native preparation now runs Cargo privately with an existing
lockfile, offline dependencies and owned compiler processes. It selects the
declared `cdylib` from package/artifact metadata, including renamed library
targets. Godot parses manifests in a blank probe project; before activation the
actual game reports loaded manifests and verifies selected-library/dependency
hashes. A real Rust fixture covers code replacement, native method behavior,
checkpoint continuity, stale working DLLs, compiler failure and external-path
refusal. Native source revisions now compile candidates before journaled
publication and preserve installed DLLs and accepted gameplay through undo.
Original imports and lockfiles remain untouched. Development source adds
contained relative Cargo workspace hints and a dedicated compiled acceptance
gate for nested roots and member dependencies. Workspaces outside the imported
root still require source-staging, provenance and freshness support. External
workspace delivery, private compile caching, runtime/dependency context,
agent benchmarks and device/release gates remain open.

- Extend versioned capture/restore hooks to supervised native games.
- Prepare a replacement privately, validate state compatibility, then hand
  over; retain the previous game on build, boot or restoration failure.
- Record process identity, checkpoint schema, restored fields and partial
  restoration explicitly. Stop only processes proven to be owned by Aurum.

Acceptance: a real native mission preserves position, enemy identities,
inventory, cooldowns and progression across compatible edits. Malformed or
future checkpoints leave the old mission intact. This is managed replacement,
not arbitrary native memory migration or a promise of zero process restarts.

### 4. Efficient agent sessions and background jobs

Status: in progress. Development source adds explicit scoped content-hash
baselines, read-only bounded deltas, reconnect by baseline ID and
content-identified pagination. These operations report paths/hashes rather than
source text. Development source also adds a shared bounded job manager with
owner capabilities, named progress, queue-inclusive deadlines, cancellation,
result summaries and crash/reconnect receipts. The latest Windows qualification
binary passed 158 expanded job/Godot/package checks and 31 portable real Cargo
cancellation, deadline and privacy checks; its source snapshot passed 746
focused Rust/doc tests and strict Clippy. Package confinement, recovery/PCK
checks, aggregate staging-storage observation and standalone packaged execution
are included in those gates. Runtime/dependency context and real model-token,
latency and task-success benchmarks remain open. See
[compact source context](AGENT_WORKFLOWS.md#compact-source-context) and
[shared jobs](AGENT_WORKFLOWS.md#shared-background-jobs).

The matching Windows 0.4.1 candidate also passed nine public CLI, HTTP and MCP
runtime-context discovery, no-session and unknown-field refusal checks. The
transports agreed on discovery and unavailable reports, protected inputs were
unchanged and all five known test processes closed normally. This qualifies
public routing without an accepted game. The earlier attempt is preserved as
a failed report collector; the corrected checker used a fresh disposable project.

Development source now supports an optional `runtime_context.resources` list
that selects returned dependency rows by exact accepted manifest name. Whole-session
identity, source, dependency and health checks remain in force, and the default
response is unchanged. Nine new and three affected unit tests passed. A fresh
Windows 0.4.1 release build also passed 15 CLI, HTTP and MCP behavior checks;
independent review verified matching reports and owned-resource closure after
a final collector error, whose failed result remains preserved. Both Rust
executables reported 0.4.1, with the Studio binary using the Windows GUI subsystem.
Accepted-game and package verification of this build, available-runtime inspection
and measured I/O or model-token savings remain open.

A local pilot measured two correct responses to a public synthetic source-change
fixture through the configured OpenAI CLI. Reported input tokens were 12,845 for
the full prompt and 18,839 for the compact prompt; each response used 76 output
tokens. This pilot does not demonstrate token savings or isolate model latency.
Representative Aurum authoring, failure and recovery benchmarks remain open.

Two later frozen studies ended without a complete comparison. The first stopped
at a provider schema rejection; its token usage is unavailable. The second
recorded six completed model turns totaling 86,442 input tokens and 1,898 output
tokens, but encountered an unconfigured Godot runtime and an incomplete timed
row. These partial results establish no token savings or task-success comparison.
Both attempts remain preserved. A neutral, comment-only validation of the exact
shared eight-file baseline passed five Godot resource checks in 7.3 seconds,
preserved all original files and applied no changes. This verifies initial
resource eligibility, not benchmark task success. A third frozen study used the
corrected schema and explicit Godot route. Its four completed model turns
reported 57,730 input tokens and 1,240 output tokens. After 29 Aurum calls, a
storage observer encountered a candidate directory removed during cleanup.
Recorded timings include client and parent-observer work; provider latency and
the returned model identifier are unavailable. The incomplete cohort is
preserved, and its original processes and resources closed within the original
deadline. It establishes no complete task-success or token-savings comparison.
Further frozen studies are authorized. A fourth study used the observer
correction and preserved two disappearance gaps. Its five completed model turns
reported 75,383 input tokens and 1,511 output tokens. Reply supervision crossed
the row-work deadline by 2 ms after the model process had completed; the required
final context was still unavailable, so no task pass is inferred. Its original
resources retired within the original outer deadline. A complete six-row
comparison and controller-overhead verification remain open.

A later preparation stopped before inference at a stale native child-directory
binding. The next study completed one model turn reporting 13,281 input tokens
and 517 output tokens, plus five Aurum calls. Its benchmark controller then
refused a cached storage sample that aged from 249 ms to 256 ms across a check,
exceeding the unchanged 250 ms bound. No row completed. Both failed cohorts
remain preserved. The later run's original resources retired and its host exited
1; independent review also confirmed exact Process environment restoration.

A subsequent study completed both representations for tasks A and B, with four
completed rows, 14 model turns and 92 Aurum calls overall. It stopped during
task C because the controller compared a reservation timestamp with a process
start timestamp. Independent review confirmed all 214 captured streams reached
EOF, the protected inputs were unchanged and the original resources closed
within their existing deadline. The partial cohort remains preserved; the
six-row comparison and its grading remain incomplete.

A further frozen study completed all six rows with 17 recorded model turns and
111 Aurum calls. Independent review verified 258 captured streams, 640 unchanged
protected inputs and normal retirement with the Process environment restored.
The model responses reported 249,462 input tokens and 3,149 output tokens;
29,184 cached input tokens are included in the input total. Inspection
corroborated the three source-stage task behaviors in both representations.
Automatic grading remains unavailable because the per-row registry prefix and
scan/archive duration contracts disagree. The recorded acceptance flags remain
unchanged, and the broader milestone remains open.

Prospective collector corrections passed 61 data-only regression cases and
independent source review. The next frozen cohort archived evidence for five
rows and 13 model turns before its original control session became unavailable.
Those turns reported 188,778 input tokens and 2,641 output tokens, including
38,912 cached input tokens. The sixth row remained incomplete. No terminal
receipt, final grading or original-resource retirement is available, so the
interrupted cohort remains separate from the earlier results. Another study
requires a controller connection verified to survive a goal-turn transition.

A separate neutral controller probe now passed that transition. A new turn
inspected the same original process and runspace, then closed them normally;
the outer process exit was observed before its unchanged deadline. Independent
review accepted the persistence and cleanup evidence. This qualifies the neutral
controller only. The subsequent complete study below also verified persistence
in the benchmark's original caller context.

That newly frozen study completed all six rows, 18 model calls and 110 Aurum calls.
Independent review accepted execution, the same original controller across a
goal-turn transition and normal closure of all owned resources. Responses reported
268,762 input tokens and 3,314 output tokens; 19,456 cached input tokens are included
in the input total. Compact rows used 131,997 input tokens against 136,765 for full
rows, a descriptive 3.49% reduction in this fixed-order sample. Model client
transport times totaled 127.744 and 131.171 seconds respectively; these do not
isolate provider latency or time to first token. The requested model was
`gpt-6.1-sol` with low reasoning effort, but the actual returned model identifier
was unreported. All automatic supplied semantic checks passed. Formal task grading
and runtime acceptance remain unavailable under the frozen grader contract, and
their recorded null or false flags are unchanged. No causal efficiency or
statistical claim follows from this single comparison. Earlier failed and partial
cohorts remain preserved, and the broader benchmark milestone remains open.

A separately reviewed integrity reconstruction subsequently hashed 855 retained
files and matched 3,278 transport, frame, source edit, event and handoff joins.
Actual model identity, provider-only timing, OS-wide I/O and new live authority
remain unavailable. The failed first reconstruction and all frozen formal grading
flags are preserved; these joins add bounded provenance for the source tasks.

Two jobs may be active while Godot's management gate remains serial. Cancellation
must confirm owned-worker termination or preserved gate-waiting state before
claiming completion, and must not relabel committed or uncertain mutations as
clean cancellation. Controller crash recovery loads metadata without replaying
requests or retaining plaintext capabilities/source. These contracts do not
complete the broader efficient-agent milestone or its benchmark requirement.

- Revision-based change queries, filtered dependency/context discovery and
  knowledge keyed to the installed runtime and operation contract.
- Stable job IDs, bounded queues, progress, deadlines, cancellation, reconnect
  and compact result/evidence summaries.
- Shared permission enforcement across CLI, HTTP and MCP. No provider lock-in.
- Measure tokens, latency and successful task completion on representative
  authoring tasks, including failures and recovery.

Acceptance: an agent can resume work after reconnecting, read only relevant
  changes, cancel its own job and reconcile another writer's edits. Benchmarks
  retain model/tool versions and do not infer token savings from JSON bytes.

## Following milestones

### 5. Visual authoring and asset workflows

Status: the local Windows 0.4.1 candidate passed 62 compiled CLI/HTTP/MCP
authoring and refusal checks, plus 22 native delivery checks. The gates cover
imported geometry and material references, typed textures, local transform
animation, a successful MCP edit with CLI/HTTP readback, and unchanged source,
asset and cache files after refusals. Managed native capture and standalone
Windows package execution show distinct animation poses. Standalone Web playback
also passed in Chrome 155 on Windows, with two inspected captures. These checks
do not qualify the full visual-authoring milestone.

Viewport transforms and snapping, reusable prefabs, materials, animation
inspection, model/import diagnostics, collisions and LOD checks. Each action
must have the same typed backend operation for the UI and headless agents.

Acceptance: author and package a representative imported, textured, animated
3D scene through Aurum without opening the Godot editor; verify native and Web
presentation and failures, not just resource parsing.

### 6. Target preflight and device delivery

Status: in progress, target by target. Development source adds shared read-only
`target_preflight` metadata reports for runtime/templates, selected presets,
SDK/JDK versions and signing-key presence. Explicit paths remain authoritative;
unknown inputs, credentials and device/network actions are refused. The compiled
CLI/HTTP/MCP parity and protected-state gate passed 96 checks, followed by
22 native-session checks on the same immutable Windows binary. The SDK/JDK and
template fixtures use synthetic metadata; Godot 4.7 version-resource evidence is
real. A separate read-only Android preflight against the installed SDK and JDK
also completed on the current-development Windows CLI. Its 24 checks reported
15 available, three mismatched, four missing and two unverified inputs. The
installed SDK lacks the required API 35, Build Tools 35.0.1, NDK 28.1.13356709
and CMake 3.10.2.4988404. Android export templates are also missing; the temporary
minimal project has no export preset or debug keystore. JDK 17 is available.
These requirements match the [Godot 4.7 Android export documentation](https://docs.godotengine.org/en/4.7/tutorials/export/exporting_for_android.html).
The report invoked no SDK tools, signing, network or device query. Development
source now also checks explicit Rust Android ABI/profile mappings and bounded
ELF header metadata for confined `.so` files. This software slice passed 23
preflight tests, 20 project tests and 177 CLI/HTTP/MCP checks. These checks do not
establish Rust provenance, symbols, dependencies, linking or GDExtension ABI
compatibility. Build/load readiness, hosted confirmation, export, signing,
deployment and actual device behavior remain unverified. iOS verification is
unavailable on the user's current hardware.

Detect runtime/template/SDK/signing requirements, explain missing prerequisites,
deploy and launch. First qualify Windows, Web and one actual Android device.
Keep iOS and VR as separate gates with their own toolchains and physical device
checks. Add input and performance acceptance appropriate to each target.

Acceptance: a clean-machine setup can explain blockers and produce a tested
device build. Presets alone do not qualify a device or an entire platform.

### 7. Reproducible failures and performance budgets

Status: in progress. Development source includes bounded playtest performance
requests, a GDScript sampler, host validation and a seeded fixture with 144 meshes
and 24 textures. Available monitor values can be assessed against explicit
budgets; requested unavailable metrics cannot pass. These are game-reported
monitor observations, with their measurement scope retained in the report.
Three headless CLI checks completed on the matching current-development Windows
0.4.1 candidate, each with 120 samples. The fixture reported a peak of 151 nodes:
a 256-node budget passed, a 32-node budget failed as exceeded, and a requested
GPU frame-time budget failed as unverified with no fabricated value. Named
fixture bytes and the CLI/Godot executable hashes were unchanged afterward;
the application reported private-workspace cleanup. These results do not
independently attribute the runtime process or prove rendered or device timing.
These checks cover the portable candidate before the bundle changes below.
The matching Windows 0.4.1 release build now creates a bounded failure bundle,
inspects it without running project code, and reproduces the retained 151-node
failure in a separate fresh carrier project. The retained request uses seed 17,
a 32-node budget and 120 completed samples. Separate seed 17 and 18 checks match
independent Godot initializer controls and confirm scene readiness before the
frame-one property event. The observer now waits for the seeded scene to finish
changing before it advances frame counting and events. These are local headless
script checks; arbitrary game determinism is not guaranteed. The top-level
verification processes and their capture resources closed normally. Independent
descendant attribution, rendered regression evidence, hosted CI, general
checkpoint/capture portability and cross-environment replay remain unverified.
Current execution remains outside an OS sandbox.

Replay bundles containing build identity, input events, compatible checkpoints,
seeds and sanitized diagnostics. CPU/GPU frame time, memory and asset budgets;
representative asset-heavy CI fixtures and rendered regression checks.

Acceptance: another environment can reproduce a retained failure against the
identified build. Declare nondeterministic boundaries; do not promise exact
replay for arbitrary game code. Bound bundle size and retention independently.

### 8. Optional isolated execution

Status: planned; current project execution is not sandboxed.

An isolated runner for unfamiliar projects with explicit filesystem, network
and device access, resource budgets and a clear trust boundary. Keep ordinary
trusted local development available. Never label a disposable copy as a
security sandbox.

Acceptance: adversarial fixtures cannot write outside granted paths or reach
disallowed networks, while the supported build/play/export workflows work
inside the runner. Native libraries remain executable project code.

## End-to-end milestone gate

An agent changes multiple linked files, validates the candidate, updates a
running native mission without losing compatible state, rolls back the entire
revision, and exports and executes a standalone game through Aurum alone.
Retain diagnostics and failure-path evidence for this gate.

The existing workbench design remains the baseline. Add task-specific controls
when backed by working operations; no separate cosmetic redesign is planned.

See [current status](STUDIO_STATUS.md), [architecture](ARCHITECTURE.md),
[agent contracts](AGENT_WORKFLOWS.md) and [logging](LOGGING.md).
