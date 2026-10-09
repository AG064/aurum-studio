# Atomic project revisions: implementation plan

Status: read-only preflight and disposable candidate validation are implemented
in current source. Journaled publication, recovery and undo are under verification. There is no released
multi-file transaction API yet; single-file guarded writes and scene transactions
remain the released mutation contract.

This follows the [bounded diagnostics milestone](LOGGING.md) and is tracked in
the [roadmap](ROADMAP.md). The implementation belongs in the shared Rust core,
with CLI, HTTP and MCP adapters calling that same implementation.

## Proposed request boundary

A change set contains a bounded list of create, replace and delete operations.
Each entry names a confined project-relative path and explicitly states its
previous hash or that the destination must not exist. Replacements carry UTF-8
text or a separately validated artifact reference. Unknown fields, duplicate
normalized paths, internal/secret paths, links outside the project, unsupported
file kinds and oversized sets are rejected before staging.

The current text-only preflight accepts 64 files, 2 MiB per file and 8 MiB
aggregate text. It does not support binary artifact replacement. HTTP/MCP body
limits also apply; a backend budget does not bypass a transport limit.

## Current preflight contract

Discover `op=changes_check` through the installed executable's `describe`
operation. This query is available through the read-only MCP project tool, CLI
and HTTP. It creates no files, locks, journal or staging directory.

```json
{"op":"changes_check","changes":[{"path":"scripts/player.gd","action":"create","expected_sha256":"","text":"extends Node3D\n"}]}
```

`create` requires an empty previous hash and a missing destination. `replace`
and `delete` require the SHA-256 from a preceding read. Create/replace require
text; delete has no text. Paths are relative to the Aurum project root, not
implicitly to a nested Godot project. Unknown entry fields are refused.

Results expose proposed/current hashes and all applicable existence/hash
conflicts, not source text. `ready` means only that these preconditions passed;
`applied` and `validated` remain false. Conflicts are query data, not a game
verdict. External edits can invalidate a result immediately, so publication
must repeat the checks under its mutation lock.

The disposable combined diagnostics acceptance exercises this operation through
actual compiled CLI, MCP and HTTP. Core tests cover duplicate aliases, secret
and internal paths, Windows device/trailing-dot names, text/file/count bounds,
stale hashes and preservation of originals. This is not atomic publication.

## Disposable candidate validation

`changes_validate` uses the same change entries, requires write permission, and
checks a private copy rather than changing the working project. On Windows it
imports through the pinned source project's Godot runtime, then loads the
candidate's scripts/scenes/resources through the existing bounded validation
worker. Both phases redirect standard application/cache data to the private
workspace. This is not a sandbox: trusted project code retains the user's
permissions and can address absolute paths.

```json
{"op":"changes_validate","changes":[{"path":"scripts/player.gd","action":"create","expected_sha256":"","text":"extends Node3D\n"}]}
```

The current slice supports self-contained script/resource projects. Native
GDExtension manifests, Rust packages, external engine hints and custom validators
are refused instead of being silently rebound or skipped. The snapshot budget
is 512 MiB, including proposed text. Snapshot exclusions apply to change targets
too: internal state, credentials, caches and excluded generated files cannot be
treated as copied source. Other platforms need private-userdata acceptance
before this runtime operation is enabled.

Construction takes a shared project-content lease. The common save/undo/scene
commit path takes an exclusive lease, so participating saves cannot interleave
with a snapshot copy. The lease is released before validation, allowing further
work while the compiler/runtime runs. Hashes of the original, copied baseline
and prepared candidate detect stale or mixed copies. A later source edit marks
the receipt stale without undoing the other writer's work. Requested candidate
files are checked again after validation so changed proposals are not accepted.
Validation-added metadata such as Godot UIDs changes the candidate fingerprint
and is reported explicitly.

The response includes validation status, resource count, source/candidate hashes,
freshness at completion, proposal integrity, timings and cleanup status.
`applied` and `gameplay_tested` remain false. A validated candidate is not a
published revision or a played game.

Normal completion removes the private source/import/userdata copy. Only the last
completed compact receipt is retained at `.aurum/checks/candidate-latest.json`,
limited to 64 KiB. `changes_last` reads it without starting a runtime or creating
state and explicitly labels it stored evidence, not a fresh source check.
Detailed future transaction originals must not use this replaceable receipt.

Abrupt host termination can leave a temporary workspace, and cleanup failures
report the retained path. Crash cleanup, aggregate temporary-storage quotas and
owned-worker recovery remain required before durable revision publication.
The source-size limit is not a hard filesystem quota or protection against
arbitrary project code writing data.

## Windows verification, 2026-10-09

The real Godot acceptance fixture validated a linked create/replace/delete set
through CLI, stdio MCP and authenticated HTTP. It rejected a deliberately broken
script, retained its error receipt, verified that working files/import state and
standard userdata remained unchanged, and checked private-copy cleanup.
The run passed 29 checks; the positive CLI receipt validated three resources in
about 8.1 seconds. This is import/resource validation, not gameplay or graphics
acceptance.

```powershell
node scripts/tests/candidates-acceptance.mjs C:/Builds/aurum.exe C:/Aurum/runtime/Godot.exe
```

The fixture and Studio state are disposable. Core regressions exercise source
leases, crash release, failed validators, stale external edits, altered proposals,
secret exclusions, bounded receipts and safe nested-link cleanup.

## Journaled publication and revision undo

`changes_apply` validates the private candidate first, then acquires the original
project's exclusive content lease and rechecks both source and candidate hashes.
Unrequested source changes are refused; supported generated UID sidecars are
included in the revision and its undo records. This slice remains text-only and
Windows-qualified through the actual validation runtime.

Before any source replacement, original and proposed bytes are synced to private
backups and a versioned journal/active pointer is atomically published. Each
replacement checks its previous hash and verifies the resulting hash. Participating
Aurum reads and writes refuse unresolved revisions instead of consuming mixed
content. Status, contract discovery and diagnostic receipt queries remain available.
Multiple filesystem renames are not globally atomic to external editors/readers.

`changes_recover` restores interrupted publication when current files still
match their recorded original or proposed hashes. Newer external edits are
preserved and reported as recovery conflicts; new content writes remain blocked
until the conflict is resolved. Recovery is accessible even if project configuration
cannot be opened. Corrupt backups never silently become restored source.

`changes_undo` takes `revision_id` and publishes a guarded reverse revision.
It refuses changed target files and preserves unrelated/newer work. Undo has its
own recovery journal rather than changing old history in place. `changes_forget`
explicitly removes a completed revision's private history without changing source;
that revision can no longer be undone afterward.

The current bounds are 256 journal rows, 32 MiB combined original/proposed backup
bytes per revision, 32 history directories and 128 MiB project history. Reaching
history limits refuses a new publication and asks for explicit cleanup; it does
not silently delete undo data. Journal and backup storage is separate from rotating
diagnostics. Filesystem/power-loss guarantees depend on the storage platform;
the journal uses synced file writes and directory sync on Unix.

The real Godot acceptance now also publishes a linked create/replace/delete set,
undoes the whole revision and forgets completed history while verifying original
files. The updated run passed 37 checks. Core failures include real process exit
after the first file, Windows sharing denial, corrupted recovery bytes and newer
external content. Additional crash/temp-storage/worker gates remain before treating
the entire roadmap as complete.

## Sequence

1. **Preflight and conflict report.** Resolve every path and check all
   preconditions. Return a compact list of conflicts without modifying content.
2. **Candidate revision.** Copy required content into a disposable private
   revision, excluding credentials, generated caches and internal ownership
   state. Resolve engine/native dependencies explicitly; relocated relative
   hints must not silently bind a different engine checkout.
3. **Whole-candidate validation.** Validate scripts/resources and applicable
   native build outputs through bounded Aurum operations. A process exiting
   successfully is not proof that a playable-game acceptance test passed.
4. **Publication preconditions.** Acquire a content-mutation lock, recheck all
   affected hashes and candidate source identity, and refuse changed inputs.
5. **Durable publication.** Record the intended revision and originals in a
   synced recovery journal before any content replacement. Publish replacements
   with phase receipts and commit the revision pointer only after success.
6. **Recovery and undo.** Recover interrupted publication before admitting
   further writes. Whole-revision undo requires the currently published hashes
   to match; conflicting external edits are preserved and reported.

## Isolation and process boundaries

- A new content-mutation lock must coordinate all participating source writers,
  including ordinary save/undo, scene edits, main-scene changes and export
  configuration. It is separate from the existing native/build lock.
- Define lock acquisition order and prohibit recursive acquisition. Candidate
  validation must not hold the publication lock across long compiler/runtime
  jobs. Status and cancellation must stay responsive.
- Coordinate Aurum reads, snapshots and preview publication with revision state
  so they cannot consume a half-published revision. Existing external editors
  and uncooperative filesystem readers are outside this isolation contract.
- A set of file renames is not globally atomic. Make interruption recovery and
  visibility guarantees explicit, including Windows file-lock failures.
- Never delete a user's newer edit as compensation. An unrecoverable conflict
  retains originals and a clear blocked recovery receipt rather than pretending
  rollback succeeded.
- Transaction originals have their own quota and retention. Do not place them
  in rotating diagnostic logs or let log cleanup delete recovery evidence.

## Implementation slices and gates

- [x] Typed request/preflight structures with path, size, duplicate and conflict
  tests; read-only planning must not create staging or state directories.
- [x] Content-lock coordination and disposable candidate construction, with
  external-edit and source-identity regression tests.
- [x] Bounded candidate validation and compact validation receipts for the current Windows script/resource slice.
- [x] Durable journal, publication and interruption recovery for the text revision slice. Inject failures
  at every file boundary and verify originals/newer edits survive.
- [x] Whole-revision undo with stale-hash refusal.
- [x] Shared discovery and actual CLI/HTTP/MCP acceptance for preflight and candidate validation.
- [ ] Shared discovery and integration acceptance for publication/recovery/undo.

Use disposable copies for every mutation/failure-injection test. Exercise real
Windows sharing locks, a second Aurum writer and an external file edit. Hosted
cross-platform checks supplement rather than replace Windows execution.

Do not expose a public `apply` operation until the recovery and conflict tests
pass. A preflight or staged candidate alone must not be labeled an atomic edit.
