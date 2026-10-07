# Browser preview

Studio exports a private snapshot of a Godot project and serves it on a separate loopback port. The running game is real WebAssembly/WebGL content, not a canvas mock or screenshot. The native editor stays optional. Rust extensions use the explicit [Web toolchain](RUST_WEB.md).

## Setup

Use the full Godot 4.7 executable and its matching single-thread release web template. `scripts/provision-godot.ps1` verifies the official archive checksums and extracts only the required entries. The export-template download is about 1.28 GB; only the two small web ZIPs are extracted.

```powershell
pwsh ./scripts/provision-godot.ps1 `
  -Destination "$env:LOCALAPPDATA/AurumStudio/runtime" -Mode WebTemplates
```

For a development install, set `AURUM_GODOT` to the executable and `AURUM_WEB_TEMPLATE` to `web_nothreads_release.zip`. Otherwise Studio checks `templates/4.7.stable` beside its runtime and the normal Godot template directory. Errors name the missing prerequisite; they do not claim the game started.

Use the same application directory you supplied to `install.ps1`. The command
above matches the README's source-install example; it is not a fixed required
drive or location. The portable release already includes these templates.

## Daily use

1. Open the project and press Run. The first export takes longer than subsequent play.
2. Click the game to focus it. Orbit Break uses Enter, WASD/arrows, Space and Escape.
3. Change the inspector's live values. They are hash-checked saves to `godot/tuning.json`. The game acknowledges a revision before Studio says it applied.
4. Script and scene changes invalidate the preview revision. Run rebuilds changed source; automatic rebuild and supported-state preservation are available in Live properties. Failed builds retain the previous working preview. Checkpoint-based rebuilding is distinct from arbitrary memory migration.
5. Use Export browser game for a fresh, standalone `dist/web` bundle. Existing destinations are refused. Deploying it to a public host is a separate user-controlled action.

The inspector's tuning fields are an explicit example-game contract, not automatic reflection of every Godot property. A compatible project exposes `window.aurumApplyTuning`, validates the supplied JSON, and sets `window.aurumAppliedTuning` to the accepted SHA256. Invalid input must leave the previous values intact. See Orbit Break's `main.gd` and Studio's `preview-bridge.js`.

The standalone bundle contains the game, Godot web loader, PCK and Wasm. It excludes the Studio control bridge and token. Use an HTTP(S) static server with the Wasm MIME type; opening `index.html` through `file://` is not a supported launch method. Script-only non-threaded builds do not require isolation headers. Extension builds do; their export receipt identifies this requirement.

## Headless access

Start `aurum studio PROJECT --no-open`. Its startup URL supplies a short-lived session token. A local agent can send an authenticated POST to `/api/preview` with `X-Aurum-Token`:

```json
{"project":"C:/Projects/MyGame","force":false}
```

The response supplies the isolated preview URL and session. `force:true` rebuilds. `{"action":"stop"}` closes the preview. `{"action":"export","project":"C:/Projects/MyGame","output":"dist/web"}` creates a fresh portable web build without replacing an existing preview. MCP/CLI project operations still handle source edits, scene operations, native play, validation and test reports. The browser-preview HTTP endpoint is not an additional MCP tool.

## Security and limits

- The control server requires its session token and rejects cross-origin mutations, including a different localhost port carrying the same browser cookies. Control pages cannot be framed.
- The preview server has no project-control routes. It permits GET/HEAD only, validates Host and confines paths to the generated export.
- The iframe receives no control token. The parent accepts messages only from the exact frame, origin and session; it copies a bounded scalar state schema.
- The read-only live endpoint exposes only the opted-in `tuning.json` resource, up to 64 KB. Source snapshots reject symbolic links, bound nesting/file count/bytes, and exclude generated directories.
- The process still executes project scripts with the user's OS permissions. This is browser/control isolation, not a sandbox for malicious Godot projects.
- Native extensions require compatible non-threaded Wasm libraries. Rust projects use the managed Emscripten build path. Desktop DLLs are not substituted for Web libraries. Missing SDKs or unsupported extension mappings fail explicitly.
- Chromium browser acceptance does not establish Firefox/Safari, mobile, touch-device or headset readiness.

## Verification

GitHub CI includes Rust tests on three operating systems, 100 Linux lifecycle iterations without retries, actual Windows Godot/editor/package acceptance, and a Chromium browser job. The lifecycle stress caught a real Linux spawn race: `/proc` could briefly report the parent executable before the child completed `exec`. Ownership recording now waits for the launched image. A macOS run also exposed a scheduler-dependent artifact-stability test; that polling contract now has controlled-clock size-change, timestamp-change and settle-window tests, alongside real filesystem checks.

The twelve workbench browser scenarios launch Studio against disposable projects and isolated user data. They check actual Wasm delivery, keyboard movement/dash/pause, an unchanged run across live tuning, exactly three workshop choices and weapon selection, file save/undo, invalid-build recovery, cross-origin refusal, forged-message rejection, portrait layouts, standalone export and play, runtime notices, ordinary 2D starters, real operation contracts and headless execution receipts. Ten integration scenarios additionally cover project presets/notices, typed live edits, same-size source changes, automatic rebuilding, three individually bounded external API rebuilds, failed-build preservation, rejected-checkpoint rollback/resume and stopping during export. Three Relay Yard scenarios cover actual 3D inputs, live tuning, paused combat reconstruction and resize handling. A separate Rust scenario requires an actual registered extension in the browser. Missing runtime/templates/SDKs fail setup instead of skipping. There are no test retries. Explicit screenshots, source-backed action traces, bounded HTTP diagnostics and native logs are retained as CI artifacts. Set `AURUM_CAPTURE_VIDEO=1` for an additional silent gameplay recording during the standalone-export test.

Integration and Relay Yard also retain `control-diagnostics.json` for headless
requests made outside the page. It records the operation, budget, elapsed time,
response phase and result classification without credentials or source. A
five-minute request deadline covers headers and JSON body reads; graceful
shutdown has a separate five-second budget. Relay Yard freshness waits up to
60 seconds because a hosted software-rendering run recorded a 38-second
response. Gameplay assertions and the zero-retry policy remain unchanged.

Run locally from a disposable repository copy:

```powershell
$env:AURUM_BINARY='C:/Build/aurum.exe'
$env:AURUM_GODOT='C:/Tools/Godot_v4.7-stable_win64.exe'
$env:AURUM_WEB_TEMPLATE='C:/Tools/templates/4.7.stable/web_nothreads_release.zip'
$env:AURUM_EMSDK='C:/Tools/web-toolchain/emsdk'
cd scripts/browser
npm ci
npx playwright install chromium
npm test
```

`examples/orbit-break/tools/verify.ps1 -Package` separately runs 85 deterministic
gameplay/HUD checks, five ordinary campaign scenarios covering each weapon and
flight frame, a stationary mission failure, and Windows packaging. Each moving
scenario clears twelve encounters, eleven workshops, three bosses, nine boss
phases and six caches. The workshop remains exactly three choices with one
reward per stop. These checks validate mechanics and regression behavior;
physical device testing and broader human playtesting remain separate.

The subsequent visual-depth pass was checked through native OpenGL renders and
repeatable desktop/portrait/compact fixtures. Windows and browser packages were
built, and their embedded display-font license was inspected. Browser automation
was blocked by the browser tool's URL policy for that pass, so those new renders
are native evidence. The earlier browser-suite result applies to its tested revision.

The later integration-fix pass exercised the current browser game through scoped Chromium acceptance runs, including standalone export/play and managed Rust-extension loading. See the [current verification record](INTEGRATION_VERIFICATION.md) for the exact evidence boundary.
