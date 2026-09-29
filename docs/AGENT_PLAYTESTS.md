# Headless gameplay tests and platform exports

The same project operation contract is available through CLI, Studio HTTP, and the compact stdio MCP profile. Run mutating game tests in a disposable copy; executing a project is not an OS sandbox.

## Structured gameplay verdict

```json
{"op":"play","scene":"tests/acceptance.tscn","frames":120,"fixed_fps":60,"user_args":["--acceptance"],"report":true}
```

- `scene` selects an explicit scene with the backend's `--scene` option, rather than silently running the project default.
- `frames` is an integer from 1 to 36000, default 120. The wall-clock deadline remains 90 seconds.
- `fixed_fps` optionally fixes simulation time, from 1 to 240. This does not make arbitrary threaded or nondeterministic game logic deterministic.
- `user_args` accepts at most 32 strings, each at most 4096 bytes. They follow the engine argument separator and cannot replace engine flags. `--aurum-report` is reserved.
- `report: true` creates a unique report location and passes `--aurum-report <absolute path>` to the game. Write a JSON object with a boolean `ok`. Reports are limited to 1 MiB. Missing/invalid reports, `ok: false`, runtime errors, timeouts, or a failing process exit fail the operation.
- Results include the structured report, report path, engine exit, bounded log tail, and errors. A game-written verdict is evidence from game code, not an independent proof of correctness.

Studio's Agents panel accepts a scene, JSON argument array, and a required-verdict checkbox. Its output retains full bounded diagnostic context on failure.

## Export presets

```json
{"op":"configure_export","platform":"Android"}
```

Accepted platform names: Windows Desktop, Linux, macOS, Web, Android, iOS. Omission preserves the Windows default. Existing presets for the target are reused without modification; other presets are preserved when a new one is appended. Configuration is not installation or device validation.

```json
{"op":"export","preset":"Aurum Android","output":"dist/android/game.apk","debug":true}
```

Export destinations must be new project-relative paths. Android requires matching templates, Java and Android SDK configuration. iOS final builds require Apple's tooling on macOS. XR uses the relevant platform export plus XR configuration and plugins. No headset or mobile deployment is implied by class availability or preset creation. Application identifiers and signing must be configured for the actual product before distribution.

Platform references: [iOS requirements](https://docs.godotengine.org/en/stable/tutorials/export/exporting_for_ios.html) and [Android XR deployment](https://docs.godotengine.org/en/stable/tutorials/xr/deploying_to_android.html).

Windows `package` remains a separate portable path that includes the installed runtime and notices without requiring export templates. It uses the full runtime, not a minimized distribution binary.

## Reference MVP

The included [Orbit Break project](../examples/orbit-break) exercises authoring, runtime validation, headless gameplay reports, stdio MCP, touch-event handling, live tuning, and portable packaging. Its verification script creates isolated copies and retains evidence. The reference game's restart-free tuning is implemented by the game itself. Arbitrary script/scene edits still use validated preview restarts; this is not universal state-preserving code reload.
