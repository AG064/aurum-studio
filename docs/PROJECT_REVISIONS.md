# Atomic project revisions: implementation plan

Status: preflight, disposable validation, journaled text publication, recovery
and whole-revision undo are published on `main` and passed hosted CI. They are
not in the 0.4.0 download. Candidate workspace recovery and worker lifecycle
controls passed local Windows gates and all 13 hosted CI jobs. Native
validation and source-only revisions for self-contained Rust/GDExtension
projects are implemented in current development source. External workspaces,
shared compilation caching and final release qualification remain open.
Contained nested Cargo workspace hints are the next native integration gate;
the dedicated fixture uses a nested Godot root and a renamed member library.

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
and HTTP. It creates no project-side state, journal or staging directory. A
temporary shared OS content lease coordinates it with participating writers.

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

The current Windows slice supports self-contained script/resource projects and
local Rust/GDExtension projects. Configured Rust candidates compile privately
with Cargo's `--frozen --offline` flags, an existing pinned lockfile and cached
dependencies before Godot imports and validates their resources. Native
manifest resolution uses Godot's parser and confines selected libraries and
declared dependencies to the private source copy. External engine hints,
workspaces outside the imported root and custom validators require separate staging support and are
refused instead of being silently rebound or skipped.

The snapshot budget is 512 MiB, including proposed text. Snapshot exclusions
apply to change targets too: internal state, credentials, caches and excluded
generated files cannot be treated as copied source. Compiler output and
redirected application data share the observed workspace storage budget.
Storage traversal has a finite two-second maximum and is clamped to a running
operation's remaining deadline. Expired budgets and traversal that finishes
late are refused; byte, entry, nesting and link limits still apply.
Other platforms need private-userdata acceptance before this runtime operation
is enabled.

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

For Rust candidates, `native_build` reports whether compilation produced a
selected library, and `native_artifacts` identifies its confined path and hash.
The selected bytes are rechecked after runtime validation. A changed compiled
library cannot qualify the candidate. `native_artifacts_published` remains
false; validation leaves the working DLL, lockfile and import directory intact.

### Contained Cargo workspace layout

Development source admits a relative `[engine].path_hint` selecting a Cargo
workspace inside the imported Aurum root, including virtual workspaces, member
packages and local path dependencies. For example, `path_hint="rust"` selects
`rust/Cargo.toml` while Godot runs `godot/project.godot`. Cargo's reported
workspace root must match that discovered workspace; selected local manifests
and target sources must remain in copied project source. Revision requests and
artifact receipts keep paths relative to the Aurum root, such as
`rust/game/src/lib.rs` and `godot/addons/native/bin/active.dll`.

The declared hint is checked even when a root Cargo manifest takes discovery
precedence. Missing directories/manifests, absolute hints, parent escapes and
junction escapes are refused before compilation. The nested workspace needs
its own pinned `Cargo.lock`; compiler targets remain private. Working native
libraries, lockfiles, Cargo targets and Godot caches are preserved by validation,
source publication, undo and managed replacement.

The dedicated [nested workspace fixture](../scripts/tests/native-workspace-acceptance.mjs)
is an integration gate for this contained layout. Workspaces outside the
imported root still need an explicit dependency provenance, source-staging and
freshness contract. That external-workspace requirement remains open.

Normal completion removes the private source/import/userdata copy. Only the last
completed compact receipt is retained at `.aurum/checks/candidate-latest.json`,
limited to 64 KiB. `changes_last` reads it without starting a runtime or creating
state and explicitly labels it stored evidence, not a fresh source check.
Detailed future transaction originals must not use this replaceable receipt.

### Candidate storage and worker lifecycle

New candidates use a dedicated `aurum-candidates-v1` directory under the host's
temporary directory. Admission reserves one of two 1 GiB slots across Aurum
processes sharing that directory. A short allocation lock and per-workspace OS
lease prevent cleanup of an active validation. Normal completion removes the
copy. After a host crash, the next allocation reclaims recognized, unlocked
copies before admitting new work. This does not clean old flat temporary copies.

Cleanup requires a matching versioned marker, exact directory identity and
released lease. Unrecognized, incomplete or corrupt state is preserved and
blocks admission with an inspection path; cleanup does not guess ownership.
Nested directory links are removed as links, not followed. Project revisions,
recovery originals and unrelated temporary directories are outside this pool.

Windows 10+ validation workers are assigned to a kill-on-close job at process
creation using the documented [job-list creation attribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute).
There is no spawn-then-attach race. Only the three explicit standard-stream pipe
handles are inherited; the job handle stays with Aurum. Host exit, deadlines,
observed storage violations and normal worker completion terminate the owned
tree. Ordinary native games and editor sessions keep their existing lifecycle.
Unsupported job setup fails closed without an unowned worker fallback.

During execution, the runner observes candidate bytes approximately once per
second, including hidden import caches and redirected userdata. It refuses
over 1 GiB, links, special file kinds, more than 50,000 entries, depth over 32,
or an inspection exceeding 250 ms. It repeats the check before acceptance.
Reservations are an admission budget, not a hard filesystem quota. Writes can
overshoot between observations, slow filesystem calls can exceed a checking
budget, and trusted code can still write outside the copy. This is not a sandbox.

Receipts report reserved bytes, recovered workspace count and the final storage
check. Oversized candidates are not fingerprinted or published. Cleanup failures
report the retained path; a later allocation retries only recognized copies.

## Windows verification, 2026-10-13

The real Godot acceptance fixture validated a linked create/replace/delete set
through CLI, stdio MCP and authenticated HTTP. It rejected a deliberately broken
script, retained its error receipt, verified that working files/import state and
standard userdata remained unchanged, and checked private-copy cleanup.
The current run passed 60 checks, including CLI/MCP/HTTP publication, history discovery, whole-revision
undo and a real Godot worker interrupted by host termination. The next validation
reclaimed the abandoned private copy and passed. This is import/resource and
lifecycle validation, not gameplay or graphics acceptance.

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

Configured self-contained Rust candidates use the same validation and journal
path. The one compiled candidate library is accepted only at its recorded
confined path with matching bytes, then excluded from source replacements and
undo records. Other unrequested or unsupported generated files remain refused.
Source publication and undo preserve the installed working DLL and the running
native game. Use the separate `build` or `native_replace` operation to install
a normal build or prepare and activate a managed successor.

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

Use read-only `changes_history` to discover retained revision IDs and compact
state/count/undo-link summaries. Pass `revision_id` for recorded target paths and
before/after hashes. Details default to 50 rows, maximum 200 and 128 KiB encoded
row data, with explicit truncation. The catalog is bounded to 32 revisions and
256 directory entries. IDs are sorted lexicographically, not chronologically.

This query reads no source or recovery blobs, creates no project-side state, and
remains available with an invalid project configuration or unresolved revision.
Recorded hashes and `undo_recorded` are historical metadata, not proof that undo
still satisfies current file preconditions. Single-file saves and external edits
are not a complete change stream. Corrupt or redirected history is refused.

The current bounds are 256 journal rows, 32 MiB combined original/proposed backup
bytes per revision, 32 history directories and 128 MiB project history. Reaching
history limits refuses a new publication and asks for explicit cleanup; it does
not silently delete undo data. Journal and backup storage is separate from rotating
diagnostics. Filesystem/power-loss guarantees depend on the storage platform;
the journal uses synced file writes and directory sync on Unix.

The real Godot acceptance now also publishes a linked create/replace/delete set,
undoes the whole revision and forgets completed history while verifying original
files. Core failures include real process exit
after the first file, Windows sharing denial, corrupted recovery bytes and newer
external content. Native dependency and state-preserving replacement gates
remain before treating the entire roadmap as complete.

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
- [x] Bounded candidate validation and compact validation receipts for the current Windows script/resource and self-contained Rust/GDExtension slices.
- [x] Durable journal, publication and interruption recovery for the text revision slice. Inject failures
  at every file boundary and verify originals/newer edits survive.
- [x] Whole-revision undo with stale-hash refusal.
- [x] Crash-released candidate leases, aggregate admission limits, observed
  storage budgets and owned-worker termination through real Godot acceptance.
- [x] Shared discovery and actual CLI/HTTP/MCP acceptance for preflight and candidate validation.
- [x] Shared discovery and integration acceptance for script/resource publication/recovery/undo.
- [x] Self-contained native candidate compilation and source-only publication/undo, with compiled-library hash checks and working-DLL preservation.
- [ ] Compiled native acceptance for contained nested Cargo workspaces, renamed member libraries, local dependencies and hint/source refusal paths.
- [ ] External workspace staging, shared compilation caching, background jobs/cancellation and device/release qualification.

Use disposable copies for every mutation/failure-injection test. Exercise real
Windows sharing locks, a second Aurum writer and an external file edit. Hosted
cross-platform checks supplement rather than replace Windows execution.

Do not expose a public `apply` operation until the recovery and conflict tests
pass. A preflight or staged candidate alone must not be labeled an atomic edit.
