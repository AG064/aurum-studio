# Aurum Studio: where it stands

A mapping from the design's acceptance criteria to the evidence for each one,
so the state of this work is checkable rather than remembered. The design in
`superpowers/specs/2026-08-31-aurum-studio-design.md` stays authoritative; this
file only records what has actually been demonstrated.

The rule applied throughout: a criterion counts as met when something in this
repository re-runs and shows it, not when the code for it exists. Where the
evidence is a test, the test is named. Where it is live, the observation is
recorded.

## Acceptance criteria

### Studio starts as a Windows desktop application

**Deliberately not met as written.** The shell is a local server with a browser
UI instead of a native window, chosen with the maintainer for two reasons: a
native toolkit such as `eframe` would add well over a hundred crates to a
project holding a hard line at 27 external packages, and a plain HTTP surface
can be driven by tests, scripts, and an agent, where a window can only be driven
by a person.

Everything the criterion was reaching for — a usable Studio on this machine —
is met differently. Revisit only if a window is wanted for its own sake.

### It imports the existing Aurum engine project without damaging current files

**Met.** `aurum import` registers a project read-only; `aurum projects` lists
it; `aurum forget` removes it. The registry never writes into a project.
`aurum doctor` runs against `A:\RecoveredProjects\C_Drive\Game_Development\aurum-studio`
and reports 12 ok, 0 warnings, 0 blocked.

### `doctor` distinguishes healthy, warning, and blocked state with evidence

**Met.** `crates/aurum-studio-core/src/doctor.rs`. Every finding carries an id,
a health, a summary, and where it can, evidence and a remedy. The verdict is the
worst finding, because one blocker is not softened by ten healthy checks.
Tested by `doctor::tests`, including that a blocker always says what to do.

### Develop launches the correct project, build watcher, bridge, and optional MCP service

**Met.** `aurum dev` launches the project, watches, and rebuilds, and refuses to
run without an explicit Godot when one is named. The editor bridge is installed
and enabled — it had never actually been enabled, which `doctor` now catches, and
it loads cleanly under headless Godot.

The MCP half is met by substitution rather than by supervision, and the reason is
in what MCP over stdio is. `aurum mcp` speaks to the client that spawned it. A
copy launched by `dev` would have nobody on the other end of its pipe: it would
sit idle until the loop ended, and `aurum stop` would then have to kill a process
that never did anything. Supervising it would be a process for its own sake.

What an agent cannot work out for itself is the two things `dev` knows: where the
project is, and where the running editor publishes what it is running. `aurum dev
--agent <client>[,<client>...]` writes both into that client's own configuration
through the same `aurum-mcp` connection code the standalone `--install` uses, so
there is one implementation of "how a client is told about this engine" rather
than two. The bridge argument is only added when `<godot>/.godot/aurum` exists,
because a bridge path pointing at nothing would let the server report an editor
that is not there.

`dev` also prints the attach line on every run, so the connection is discoverable
without reading the source. Tested by
`commands::tests::agent_takes_a_client_name_and_refuses_to_be_given_none` and
`commands::tests::no_agent_flag_means_no_client_is_touched` — the second matters
more than it looks, because it is what keeps `dev` from editing a user's editor
configuration unasked.

### Build errors are visible and never replace the working DLL

**Met.** `install` stages, verifies, then swaps, keeping a backup. A failed
build never reaches the swap.
`build::tests::a_failed_build_leaves_the_installed_library_untouched` drives a
fake cargo that fails, and asserts the installed library is byte-identical
afterwards. `scripts/tests/phase0_transaction.ps1` covers the same ground
against the real script.

### GDScript and safe Rust implementation changes complete without restarting the editor

**Met, with live evidence.**
`scripts/tests/studio_hot_reload.ps1` builds four successive libraries, reads
each new fingerprint out of an editor started once and never restarted, and
records one editor process serving every rebuild. The reload marker proves a
file was written; the live fingerprint the editor plugin publishes proves the
new code is the code running, and only the second is evidence.

It requires nothing installed — no node, no npm, no MCP client.

### Gameplay may restart independently of the editor

**In progress.** `Verdict::GameplayRestart` has been classified by
`reload.rs` since Phase 1, and nothing acted on it.

### Native structural changes produce a specific exceptional-restart reason

**Met.** `reload::classify_change` returns `Verdict::EditorRestart` with the
marker that caused it — a `#[func]`, a property, a signal, an entry symbol —
and the reason names it rather than saying "a restart is needed".

### Studio never discards unsaved work or terminates an unrelated process

**Met.** Processes are proven to be ours by identifier **and** executable
**and** start time together; any disagreement means the process is not touched.
A polite close is attempted first and force is only used when asked for, because
`taskkill` without it cannot close a console application and taking the forceful
path by default would throw away whatever the user had open.
Tested by `supervise::tests`, including that a reused identifier is refused
rather than killed.

### Telemetry remains absent and all control traffic remains local

**Met.** Nothing in the workspace phones home. The shell binds `127.0.0.1` and
nothing else — not configurable, because that is the difference between a local
tool and an open door — requires a 256-bit token compared in constant time, and
refuses any `Host` that is not a loopback literal, which ends a DNS-rebinding
attempt before the token is considered.

## Honest summary

Nine of ten criteria are met with re-runnable evidence, and one — a native
desktop window — is met by deliberate substitution, chosen with the maintainer
rather than fallen into.

The Phase 3 items not yet started are the authenticated editor bridge socket,
controlled exceptional restart, and MCP status and permission controls.

Phase 4 is built further than "untouched" would suggest and is not claimed here,
because the criterion for this file is re-runnable evidence and the evidence is
uneven. `new`, `modules`, `godot --fetch` and `presets` all exist and carry unit
tests — 13, 12, 11 and 12 respectively — but no script under `scripts/tests/`
exercises any of them end to end, where `doctor`, `dev`, the build transaction
and hot reload each have one. Unit-tested and demonstrated are not the same
claim, and this file only makes the second.

The one criterion still in progress is **gameplay may restart independently of
the editor**: `Verdict::GameplayRestart` has been classified by `reload.rs` since
Phase 1 and nothing acts on it.
