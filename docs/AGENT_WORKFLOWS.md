# Agent project contracts

For a first connection, use the [compact MCP setup](WORKFLOW.md#mcp). Aurum
supplies project tools, not a model provider or paid-agent account. The guides
here describe current `main`; discover the installed executable's contracts
rather than assuming it has every development feature.

The compact MCP profile remains three tools. Query `describe` when a task needs more detail instead of loading every operation's instructions at startup:

```json
{"op":"describe"}
{"op":"describe","operation":"capture"}
```

The catalog reports read/write classification and required fields. An operation-specific result includes a request schema and, for gameplay, input-event examples and constraints. Discovery is available without an editor or runtime import. It does not grant execution permission.

MCP operation lists are generated from the shared backend registry. `capture`, `web_build`, input timelines, capture sizes, `pack` exports and wall budgets are advertised. Play accepts up to 3,600,000 frames and 600 wall seconds; a fixed frame rate does not guarantee deterministic game logic.

For protocol `2025-06-18`, successful object results include `structuredContent` and compact JSON text for compatibility. Project tools advertise an object output schema. Negotiated `2025-03-26` and `2024-11-05` sessions keep text results without the newer fields. Compact text reduces formatting bytes; this is not a measured guarantee of a particular model's token or latency reduction. Large discovery should still be filtered by task, such as `classes` with a query.

Project execution errors retain the object shape with `ok:false` and an `error` string. The wire format follows the [MCP tool-result specification](https://modelcontextprotocol.io/specification/2025-06-18/server/tools).

Successful queries such as `status`, `read` and `describe` return their data
directly and do not all include `ok:true`. Check the transport/CLI outcome and
the operation's schema. A gameplay verdict has its own required boolean `ok`;
do not infer a passed game test merely from a successful discovery response.

An isolated comparison against the published 0.3.0 executable measured the compact Studio tool catalog at 5,231 UTF-8 JSON bytes before and 4,858 after, retaining three tools while adding the missing operations and fields. This is a 7.1% catalog-byte reduction, not a model-token benchmark. Test receipts retain binary hashes; repeat the comparison if the catalog changes.

## A bounded authoring loop

1. Query project status and describe only the operation needed next. Filter
   class/property discovery instead of loading the full runtime catalog.
2. Read the relevant source or scene and retain its returned hash. Inspect
   existing nodes and resources before creating replacements.
3. Save with that hash or use a transactional scene batch. If the hash is
   stale, reread and reconcile; do not silently overwrite another writer.
4. Validate, then run a bounded test scene with a fresh JSON verdict. Use
   rendered inputs/captures when the outcome depends on actual presentation.
5. Inspect the receipt and diagnostics. Missing verdicts, runtime errors and
   exhausted budgets are failures, not permission to assume success.
6. Export to a fresh destination and execute the resulting package or served
   browser bundle. A successful build alone does not establish playability.

Run mutation tests in a disposable project copy. Keep full logs in the run's
evidence directory and return the relevant failure or receipt summary to the
agent. This limits repeated context without hiding failures. API output and
test budgets are bounded; large request bodies are better supplied as files.

The same read-only discovery is available without MCP or a visible editor:

```powershell
aurum project C:/Projects/MyGame --request-json '{"op":"status"}'
aurum project C:/Projects/MyGame --request-json '{"op":"describe","operation":"play"}'
```

See [playtest requests and verdicts](AGENT_PLAYTESTS.md) and
[scene transactions](WORKFLOW.md#scene-transactions) for concrete contracts.

## A verified 3D workflow

[Relay Yard: Night Shift](../examples/relay-yard) is a small complete 3D action-extraction game that exercises the authoring and game workflow:

1. Discover the operation contract through a real stdio MCP session.
2. Import and instance a glTF resource in a transactional scene edit, then reopen the scene.
3. Save source with the preceding hash and undo it without losing the original.
4. Validate and execute a bounded, physics-driven game with a fresh verdict.
5. Send real inputs and live properties, capture rendered frames, and retain receipts.
6. Rebuild a browser preview and restore compatible mission state.
7. Package and execute a standalone Windows game.

The game's version 3 checkpoints validate before changing the mission and reconstruct enemies, projectiles, hull, heat, ability cooldowns, cargo, mission progress and exact RNG state. Future-version or malformed checkpoints leave the previous state intact. Browser acceptance checks paused combat, enemy identities/health, player position and live tuning after replacement. The original fixture's version 2 saves are deliberately refused because they contain no combat state. A separate detached migration helper remains available for compatible game schemas.

## Compact source context

Development source adds `context_snapshot`, `context_delta` and `context_forget`.
These are not included in the 0.4.0 download. They work through the same CLI,
HTTP and MCP project contract without starting Godot. Create an explicit scoped
baseline, then request only the changed paths and content hashes:

```powershell
$aurumContextBaseline = aurum project A:/Games/MyGame --request-json '{"op":"context_snapshot","prefix":"game","extensions":["gd","tscn","rs"]}' | ConvertFrom-Json
$aurumContextRequest = @{op='context_delta'; baseline_id=$aurumContextBaseline.baseline_id; limit=50} | ConvertTo-Json -Compress
aurum project A:/Games/MyGame --read-only --request-json $aurumContextRequest
```

The baseline contains paths, sizes and SHA-256 hashes, not file contents. A
prefix narrows filesystem traversal; extension filters narrow content hashing.
Deltas distinguish creates, same-size modifications and deletes. An agent can
reconnect using the retained baseline ID, then read only the relevant files.
Delta queries also work when a source edit has broken `aurum.toml`.

Use `next_after` plus the returned `current_id` for another bounded page. Source
drift between pages is refused, so restart the query rather than merging mixed
revisions. Queries never advance a baseline; create another snapshot after
processing the changes. Identical snapshots reuse their identity. Up to 16
baselines of at most 4 MiB are retained. Cleanup with `context_forget` is explicit
and permanent, and removes only the selected baseline metadata, not game files.
Snapshot/forget require write permission; delta is read-only and creates no
project state.

Scopes hash at most 512 MiB of public source with bounded nesting/file counts.
Two scans detect drift, but external editors do not participate in an atomic
filesystem snapshot. These are compact source-context operations, not runtime
state, dependency analysis or measured model-token savings. Background job
progress/cancellation and token/latency benchmarks remain roadmap work.

## Remaining boundaries

The 0.4.0 package does not include journaled multi-file revisions or managed
native-process checkpoint handoff. Development source adds those Windows
script/resource workflows and [shared native sessions](HOT_RELOAD.md#managed-native-sessions).
Neither version provides a malicious-code sandbox, mobile SDK provisioning or
headset certification. Project code runs with the user's permissions. Run tests
in disposable copies. Android/iOS/VR capability must be qualified on the
relevant toolchains and actual devices.

Remaining gates include native dependency staging, cancellable background jobs,
representative asset/material/animation workflows, target-specific preflight
and independent device tests. Track these separately from the 0.4.0 contract
and the 3D reference milestone.
