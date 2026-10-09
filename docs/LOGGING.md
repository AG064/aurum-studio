# Bounded local diagnostics

This guide describes the local development implementation and its Windows
verification. It is not part of the published 0.4.0 executable. The broader
[roadmap](ROADMAP.md) remains planned work.

## Contract

The operation journal is local to a project at `.aurum/logs/operations.jsonl`.
Each record uses an allowlisted schema: version, operation ID, timestamp,
operation, phase, duration and outcome/failure category. Source, request bodies,
response bodies, arbitrary error text, credentials and environment values are
not written to this journal.

The default budget is 2 MiB per file, one active file and three rotated
generations: at most 8 MiB per journal. Session text logs use the same budget,
with UTF-8-safe 4 KiB line truncation. Records and query results are bounded.
Successful lightweight reads/polls are omitted; mutations and substantial
runtime queries are recorded. Their failures are categorized without copying
user data. Malformed requests and permission refusals are rejected before
opening a project or creating logs; omitted lightweight queries remain omitted
on failure too.

Writers coordinate through a short-lived OS file lock, released on process
exit. Logging must not wait indefinitely or corrupt the operation's result.
Diagnostic writes do not perform a durability flush per record; the latest
records can be lost on abrupt power failure. Transaction journals, ownership
records and test verdicts have different durability requirements.

Old session logs are eligible for cleanup after 14 days, with a 64 MiB total
session-log retention target. Cleanup touches only recognized log files, never
session metadata, process ownership, tokens, packages or run evidence. A busy
log or a cleanup error is retained and reported, not forcibly unlocked. Limits
across many active or locked sessions are therefore best-effort.

The startup retention pass scans at most 4,096 session directories and has a
250 ms work budget. It can leave further cleanup for a later startup. Rotation
discards the oldest generation; a legacy active file already larger than the
new limit is reset rather than retained as an oversized archive. Copy any
historical log you need before migrating a development installation.

## Native child output

A detached Rust helper drains native game/editor stdout and stderr, so capture
continues after the launching CLI exits. It shares the bounded session logger;
an inherited file handle no longer bypasses rotation. On Windows, the original
CLI standard handles are made non-inheritable so descendants cannot keep a
calling client's output pipes open. Explicit child output pipes still work.

The helper accepts at most 200 child lines per second, summarizes suppression,
and backs off storage writes for one second after an error or after spending
20 ms on logging I/O in a window, while continuing to
drain. Lines without newlines remain memory-bounded. Windows CRLF is normalized.
Session output redacts the session token and omits common credential-bearing
lines. Free-form tool output is not a guarantee of removing every private value;
review session logs before sharing them. Protocol trace mode omits payloads,
request IDs and unknown method names entirely.

Rate/storage limits can discard individual child messages, including errors
inside a flood. Suppression is explicit; missing text is not evidence of
success. Use the bounded game's verdict and operation receipt to establish
success. Helper processes are reaped by long-lived hosts and end when child
writers close their pipes.

## Query from an agent

Use `op=describe` with `operation=logs` to discover the installed contract.
The same request works through CLI, HTTP and the read-only MCP project tool:

```powershell
aurum project C:/Projects/MyGame --request-json '{"op":"logs","limit":50,"failures_only":true}' --read-only
```

The default is 50 records; the maximum is 200, scanning at most 256 KiB across
the four generations. `operation` filters by a known operation name. Results
include retention budgets, truncation/invalid-record indicators and process-local
write/drop counters. A new CLI process has fresh counters; they are not a
cross-process aggregate or a lifetime total. Missing journals return an empty
tail without creating state. Correlate start/completion records by operation ID.

## Windows verification, 2026-10-09

The disposable acceptance script verifies actual compiled CLI, stdio MCP and
authenticated HTTP saves/queries, stale-write refusal, source/token omission,
payload-free tracing, Windows junction refusal and native output after the
launching CLI exits. The native process is a purpose-built lifecycle fixture,
not a game-playability or device-certification test.

An acceptance run passed 25 combined diagnostics/preflight checks. Thirty
sequential local HTTP saves took 3,042 ms in total, about 101 ms per save, and
the operation journal occupied 11,354 bytes. This includes HTTP/server scheduling and file writes; it is not
an isolated logger-overhead benchmark or evidence of model token savings.

Reproduce in a disposable state location with a freshly compiled executable:

```powershell
node scripts/tests/diagnostics-acceptance.mjs C:/Builds/aurum.exe
```

The script creates its own temporary project and Studio state, retains a JSON
verification receipt, and does not update the installed application. Core
regressions cover rotation, bounded Unicode, interruption repair, concurrent
threads/processes, retention, lock contention, flooding and storage failures.
Linux/macOS execution remains a hosted CI gate, not a Windows test claim.

## Acceptance gates

- Concurrent threads and independent processes, including rotation.
- Lock release after a writer exits; bounded behavior under contention.
- Size/age retention, oversized Unicode messages and bounded tail reads.
- Source/credential omission and payload-free MCP protocol traces.
- Refusal to follow diagnostic symlinks outside managed state.
- Disk/path failures preserve the original operation result.
- Actual CLI and stdio MCP queries, including read-only enforcement.
- Representative write overhead and no per-frame logging.

Detailed gameplay/import/build receipts remain in their existing evidence
directories. Their retention needs a separate policy; this milestone does not
claim to cap all generated project artifacts.
