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

The intended 0.5.0 scope is bounded diagnostics, atomic project revisions,
native checkpoint replacement and efficient agent sessions. These tasks are
sequenced because replacement and background jobs need trustworthy change and
diagnostic contracts. A version number is not a delivery guarantee.

### 1. Bounded diagnostics

Status: published on `main`, with Windows acceptance and passing hosted
cross-platform CI. Not included in the 0.4.0 download.

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
and owned-worker lifetime controls also passed all 13 hosted CI jobs. Native dependency
staging remains pending.

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
also in development source. Busy-operation cancellation, external native
dependencies and final release qualification remain open. This work is not in
the 0.4.0 download.

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
records without Aurum transport tokens. `native_recover` checks recorded host/game creation
identities, refuses live or inaccessible processes, validates the working source
hash, and proves restoration in a fresh private process before activation. It
never signals a recorded PID or overwrites newer working edits. Actual host
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
refusal. Original DLLs, imports and lockfiles remain untouched. External
workspaces, private compile caching and native revision publication remain open.

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
source text. Shared jobs, progress, cancellation, runtime/dependency context and
model-token/latency benchmarks remain open. See [compact source context](AGENT_WORKFLOWS.md#compact-source-context).

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

Status: planned after the iteration foundation.

Viewport transforms and snapping, reusable prefabs, materials, animation
inspection, model/import diagnostics, collisions and LOD checks. Each action
must have the same typed backend operation for the UI and headless agents.

Acceptance: author and package a representative imported, textured, animated
3D scene through Aurum without opening the Godot editor; verify native and Web
presentation and failures, not just resource parsing.

### 6. Target preflight and device delivery

Status: planned, target by target.

Detect runtime/template/SDK/signing requirements, explain missing prerequisites,
deploy and launch. First qualify Windows, Web and one actual Android device.
Keep iOS and VR as separate gates with their own toolchains and physical device
checks. Add input and performance acceptance appropriate to each target.

Acceptance: a clean-machine setup can explain blockers and produce a tested
device build. Presets alone do not qualify a device or an entire platform.

### 7. Reproducible failures and performance budgets

Status: planned.

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
