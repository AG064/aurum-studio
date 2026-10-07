# Troubleshooting

First identify the executable you are using: `aurum --version`, its absolute
path, and the commit or release that supplied it. Version 0.4.0 and the
older v0.3.0 download have different interfaces and capabilities. Rebuilding source
does not replace an installed executable automatically.

## Run does not start a browser game

Open Output and read the failed stage. A source install needs the full Godot
4.7 executable and matching Web templates; the runtime alone is not enough.
Follow [Web setup](WEB_PREVIEW.md#setup), using the same application directory
as your installation. Do not substitute a desktop DLL for a Web extension.
Rust projects also need the [Web toolchain](RUST_WEB.md).

Click the game canvas to give it keyboard focus. A ready preview with no input
is different from a failed export. Check the example's controls: both examples
use Enter to begin, but movement and actions are project-specific.

## A standalone Web bundle does not open

Serve its directory over HTTP(S) with `application/wasm`; `file://` is not a
supported launch path. Rust extension bundles require the isolation headers
listed in their export receipt. Players do not need Studio or a Rust compiler.
Public hosting is a separate deployment step.

## An edit is refused or the game still shows old code

| Symptom | Check |
| --- | --- |
| Save reports a stale hash | Read the current file again and reconcile the changes. Do not bypass the hash to overwrite another writer. |
| A file draft differs from disk | Use Source > Saved version to inspect disk. The draft remains available until deliberately saved or discarded. |
| Preview says Source changed | Run a rebuild or enable automatic rebuilding. Live properties and source replacement use different paths. |
| A rebuild failed but the old game is visible | This is retained-run behavior. Read Output and the reload message, fix the failure, then rebuild. |
| State restoration is refused | Check the game's checkpoint version/schema and start scene. Not every project provides checkpoint support. |
| A native declaration changed | A controlled editor restart can be necessary. Compatible method-body reload is not universal schema migration. |

See [runtime integration](INTEGRATION.md) for the exact preservation and rollback
boundaries. The separate Rust simulation is not the running game's memory.

## An MCP client cannot connect or rejects an operation

Use an absolute `aurum.exe` path if the client does not inherit PATH. Start
with the compact Studio profile shown in [MCP setup](WORKFLOW.md#mcp), then
inspect `aurum_mcp_status` and discover the operation contract on the installed
version. Read-only mode intentionally refuses actions. Extra tools do not
grant OS isolation or make untrusted projects safe to execute.

For long native builds, set the client's operation timeout to cover the
documented build budget. Use `--request file.json` for large CLI requests to
avoid shell-quoting errors. Keep MCP stdout reserved for protocol messages.

## Tests or GitHub Actions fail

Keep the first failure, name the job/scenario and inspect its artifacts.
Browser artifacts contain screenshots, action traces and bounded HTTP timing;
runtime artifacts contain native logs and verdicts. A completed native export
does not prove that the browser adopted it or that a concurrent inspection
finished. A skipped serial successor is not a passing test.

Reproduce in a disposable checkout/project. Separate prerequisite failures,
native stages, headless requests, browser controls and gameplay verdicts.
Change a test budget only with timing evidence; do not add retries or remove
the behavior being asserted. [Contributor verification gates](../CONTRIBUTING.md#choose-a-verification-gate).

Before sharing a bug report, redact session-token URLs, credentials and private
paths. The Studio startup URL is a control credential, not a public game link.
