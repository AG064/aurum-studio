# Browser preview

Studio exports a private snapshot of a script-only Godot project and serves it on a separate loopback port. The running game is real WebAssembly/WebGL content, not a canvas mock or screenshot. The native editor stays optional.

## Setup

Use the full Godot 4.7 executable and its matching single-thread release web template. `scripts/provision-godot.ps1` verifies the official archive checksums and extracts only the required entries. The export-template download is about 1.28 GB; only the two small web ZIPs are extracted.

```powershell
pwsh ./scripts/provision-godot.ps1 -Destination A:/AurumStudio/runtime -Mode WebTemplates
```

For a development install, set `AURUM_GODOT` to the executable and `AURUM_WEB_TEMPLATE` to `web_nothreads_release.zip`. Otherwise Studio checks `templates/4.7.stable` beside its runtime and the normal Godot template directory. Errors name the missing prerequisite; they do not claim the game started.

## Daily use

1. Open the project and press Run. The first export takes longer than subsequent play.
2. Click the game to focus it. Orbit Break uses Enter, WASD/arrows, Space and Escape.
3. Change the inspector's live values. They are hash-checked saves to `godot/tuning.json`. The game acknowledges a revision before Studio says it applied.
4. Rebuild the preview for script or scene changes. A failed build retains the previous working preview. Rebuild is a new run, not memory migration.
5. Use Export browser game for a fresh, standalone `dist/web` bundle. Existing destinations are refused. Deploying it to a public host is a separate user-controlled action.

The inspector's tuning fields are an explicit example-game contract, not automatic reflection of every Godot property. A compatible project exposes `window.aurumApplyTuning`, validates the supplied JSON, and sets `window.aurumAppliedTuning` to the accepted SHA256. Invalid input must leave the previous values intact. See Orbit Break's `main.gd` and Studio's `preview-bridge.js`.

The standalone bundle contains the game, Godot web loader, PCK and Wasm. It excludes the Studio bridge and control token. Use an HTTP(S) static server with the Wasm MIME type; opening `index.html` through `file://` is not a supported launch method. The single-thread build does not require cross-origin isolation headers.

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
- Native extensions are rejected with a Run native fallback. Rust is not inherently incompatible with the web, but an existing desktop DLL cannot run there. A native web-extension toolchain is separate work.
- Chromium browser acceptance does not establish Firefox/Safari, mobile, touch-device or headset readiness.

## Verification

GitHub CI includes Rust tests on three operating systems, 100 Linux lifecycle iterations without retries, actual Windows Godot/editor/package acceptance, and a Chromium browser job. The lifecycle stress caught a real Linux spawn race: `/proc` could briefly report the parent executable before the child completed `exec`. Ownership recording now waits for the launched image. A macOS run also exposed a scheduler-dependent artifact-stability test; that polling contract now has controlled-clock size-change, timestamp-change and settle-window tests, alongside real filesystem checks.

The seven browser scenarios launch Studio against disposable projects and isolated user data. They check actual Wasm delivery, keyboard movement/dash/pause, an unchanged run across live tuning, normal workshop progression/purchases, file save/undo, invalid-build recovery, cross-origin refusal, forged-message rejection, narrow layouts, standalone export and play, runtime notices, and an ordinary 2D starter without the example's bridge. Missing runtime/templates fail setup instead of skipping. There are no test retries. Screenshots, traces and logs are retained as CI artifacts.

Run locally from a disposable repository copy:

```powershell
$env:AURUM_BINARY='C:/Build/aurum.exe'
$env:AURUM_GODOT='C:/Tools/Godot_v4.7-stable_win64.exe'
$env:AURUM_WEB_TEMPLATE='C:/Tools/templates/4.7.stable/web_nothreads_release.zip'
cd scripts/browser
npm ci
npx playwright install chromium
npm test
```

`examples/orbit-break/tools/verify.ps1 -Package` separately runs 47 deterministic integration checks, winning ordinary-loop campaigns for Pulse/Lance/Arc, a stationary losing campaign, and Windows packaging. These scripted campaigns validate mechanics and regression behaviour, not broad human playtesting or device certification.
