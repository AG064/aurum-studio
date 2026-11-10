# Release delivery

Release packages are separate from source commits. Check the [GitHub release page](https://github.com/AG064/aurum-studio/releases) for published downloads; `main` can contain later development changes. An existing installation is not updated by pulling or pushing the repository.

## v0.4.2 published download

[Release page](https://github.com/AG064/aurum-studio/releases/tag/v0.4.2), built from `main` at `9c9892f`. A patch release for 0.4.1 with one fix: Studio waits up to three minutes, not 30 seconds, for a game to start in the preview. On a cold first load Relay Yard took about 53 seconds on an RTX 2070, so every cold load of 0.4.1 showed "Preview unavailable".

- `Aurum-Studio-0.4.2-windows-x64.zip`: the same contents as 0.4.1 with the fix. SHA-256 `CF53880E74E42B5663E6110D1C9A484F246BDE06CFE6C7F64F62994066D1E747`.
- `SHA256SUMS.txt`: the archive checksum, with Unix line endings.

Checked on an RTX 2070 with fresh browser profiles: two cold loads of Relay Yard and one of Orbit Break reached "Preview ready", and Orbit Break responded to a real key press. The launcher acceptance script passed all 55 checks with the browser pinned to an isolated Edge profile. With the system default browser it failed to stop Studio ("An operation is still running"), also for 0.4.1, and the cause is not identified. The native game verifications ran on the 0.4.1 package. The executables are not signed, and devices and benchmarks were not tested.

## v0.4.1 published download

[Release page](https://github.com/AG064/aurum-studio/releases/tag/v0.4.1), built from `main` at `64d6a24`.

- `Aurum-Studio-0.4.1-windows-x64.zip`: Studio with the direct `Aurum Studio.exe` launcher, CLI/MCP, the Godot 4.7 runtime, ordinary and extension-enabled Web templates, notices, documentation and example sources. SHA-256 `3EF48D6523A6663CD75A05B17323F9141FC9EB26F2ED93797F372A3AECA381A5`.
- `SHA256SUMS.txt`: the archive checksum.

Extract into a new writable folder and open `Aurum Studio.exe`; keep `bin` and `runtime` beside it. Standalone game packages were not republished, so the 0.4.0 Orbit Break and Relay Yard downloads remain current.

The packaged executables passed the project's launcher acceptance, 55 checks covering fresh install, shortcut, relocation and fresh extraction, with no failures; all 13 hosted CI jobs passed on the merged source. The executables are not digitally signed. Browser rendering of this exact package, physical devices, gameplay on hardware and model-provider benchmarks were not verified.

## 0.4.1 locally verified Windows candidate

This section describes how the 0.4.1 package was built and verified locally; the published download is described above. The primary Windows entrypoint is `Aurum Studio.exe` at the package root. Keep `bin/aurum.exe`, `runtime/godot.exe` and the rest of the package beside it. The console CLI and MCP entrypoint remain separate.

From an A: source checkout with PowerShell 7, Rust 1.98/Windows C++ tools and the full Godot 4.7 runtime, build both binaries and install into a dedicated folder:

```powershell
Set-Location A:/Projects/aurum-studio
$buildDirectory = Join-Path ([IO.Path]::GetTempPath()) ('aurum-041-build-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $buildDirectory 'tmp') -Force | Out-Null
$env:TEMP = Join-Path $buildDirectory 'tmp'
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = Join-Path $buildDirectory 'target'
cargo build --locked --release -p aurum-cli --bins
pwsh ./scripts/install.ps1 `
  -Binary "$env:CARGO_TARGET_DIR/release/aurum.exe" `
  -LauncherBinary "$env:CARGO_TARGET_DIR/release/aurum-studio.exe" `
  -GodotBinary A:/Tools/Godot_v4.7-stable_win64.exe `
  -StudioHome A:/AurumStudio-0.4.1 `
  -NoEnvironment -NoShortcut
```

Open `A:/AurumStudio-0.4.1/Aurum Studio.exe`. The two switches above leave user PATH and Start-menu shortcuts untouched. Omit them when deliberately installing those conveniences. The installer requires matching CLI/launcher versions, verifies file copies and preserves replaced binaries in backups. Its runtime notice probe uses isolated state. VBScript remains a compatibility fallback.

For a portable candidate, supply the same built binaries, the full runtime and its four matching ordinary/extension Web templates. Provision the templates with the included checksum-pinned script if needed, then package into a new output directory:

```powershell
pwsh ./scripts/provision-godot.ps1 -Destination A:/Tools/Aurum-Godot -Mode WebTemplates
pwsh ./scripts/package-release.ps1 `
  -Binary "$env:CARGO_TARGET_DIR/release/aurum.exe" `
  -LauncherBinary "$env:CARGO_TARGET_DIR/release/aurum-studio.exe" `
  -GodotBinary A:/Tools/Godot_v4.7-stable_win64.exe `
  -TemplatesDirectory A:/Tools/Aurum-Godot/templates/4.7.stable `
  -OutputDirectory A:/AurumPackages/0.4.1-candidate
```

Packaging refuses an existing output directory and does not publish a GitHub release. Extract `Aurum-Studio-0.4.1-windows-x64.zip` into a fresh writable directory and open `Aurum Studio.exe`. Script-only projects need no compiler. Rust projects still need their optional toolchains.

The current frozen Windows launcher/package gate passed all 51 emitted checks, covering 38 required semantics and ten owned cleanup records. It verified fresh installation, a direct shortcut target, duplicate-host reuse, relocation, visible startup failure and environment preservation. Matching GUI/CLI/Godot files were verified in four contexts, along with all four Web templates in both package contexts and the 179-entry ZIP after fresh extraction. The emitted count includes polling observations; it is not a count of distinct semantics. Strict GUI Clippy with `-D warnings` passed without diagnostics, and all seven browser-policy tests passed.

This qualified candidate was installed locally. The installed executable, direct shortcut with no arguments and nonlaunch `--check` were verified at 0.4.1. Eight replaced managed files have backups verified by bytes, hashes and attributes. Studio user state and the user environment remained unchanged. The installed GUI was not launched.

The GUI optionally accepts the process-local settings `AURUM_STUDIO_BROWSER_EXECUTABLE` and `AURUM_STUDIO_BROWSER_PROFILE`. Supply both together: an existing ordinary absolute `.exe` path and an existing ordinary absolute dedicated Chromium profile directory. Invalid or incomplete settings produce a redacted configuration error before browser startup. Leaving both unset preserves the system-default browser request. The installer does not persist these settings; nonlaunch `--check`, `--version` and `--help` ignore them.

Browser delivery was qualified only through isolated opt-in Chromium/profile routing. OS-default delivery, new rendering evidence, signing, a public 0.4.1 download, physical devices and representative model benchmarks remain separate gates. These results cover the frozen candidate, not later unqualified source edits. Local installation and source publication do not update the published download.

A later 0.4.1 Windows build corrects attended workspace access to use the packaged `Aurum Studio.exe` filename. All 19 focused workspace-access tests passed, including the two package-layout regressions. Its matching GUI and CLI release binaries also passed version checks and a nonlaunch `--check` from an unrelated directory with a private profile. That nonlaunch check confirms the adjacent runtime is present; attended confirmation and visible startup through a browser remain separate verification gates for this build.

The matching portable ZIP passed metadata and content checks for all 179 entries, with all 178 files matching a fresh extraction. It includes the rebuilt GUI/CLI, full runtime, four Web templates, notices, documentation and example sources. Assembly queried the bundled Godot 4.7 runtime successfully. Matching version and nonlaunch checks also passed after relocation to a path containing spaces and Unicode, from an unrelated working directory. Independent review verified the copied files, three normal-exit probes and owned-resource closure. This local archive has SHA256 `FF4CFD9E32B0567A71EFE38166646055B1A246573BE00AD561A5797B59E9F628`. Its initial verifier rejected an empty directory with unspecified ZIP attributes; the corrected check verified the same archive without rebuilding or rerunning the runtime probe. The original failed result remains preserved.

The 0.4.1 candidate containing failure bundles and the seeded-scene readiness fix also passed isolated portable verification: 47 checks, including 38 required checks and 10 cleanup records. The root `Aurum Studio.exe` and adjacent CLI/runtime were checked across four installation, relocation and extraction contexts; all 179 ZIP entries matched the exercised extraction. The private verification hosts closed normally. This package has SHA256 `E372FBA1DA72535B7DF80B2A6BAB8A54BB3937DDABBD8605574CD5987516F394` and remains a local candidate within the browser and device limits above.

## v0.4.0 published download

[Release page](https://github.com/AG064/aurum-studio/releases/tag/v0.4.0).

- `Aurum-Studio-0.4.0-windows-x64.zip`: the redesigned Studio, CLI/MCP, Godot 4.7 runtime, ordinary and extension-enabled Web templates, operation discovery, both example source projects and local documentation.
- `Orbit-Break-0.4.0-windows-x64.zip` and `Orbit-Break-0.4.0-web.zip`: the 2D campaign as standalone native and browser builds.
- `Relay-Yard-0.4.0-windows-x64.zip` and `Relay-Yard-0.4.0-web.zip`: Night Shift's 3D mission as standalone native and browser builds.
- `SHA256SUMS.txt` and `verification.json`: archive hashes, source commit, runtime/binary identity and actual acceptance scope.

Extract the Studio ZIP into a new writable directory and open `Launch Aurum Studio.vbs`. To open the included 3D game directly in Studio, run from that folder:

```powershell
$env:AURUM_STUDIO_HOME = (Get-Location).Path
.\bin\aurum.exe studio .\examples\relay-yard
```

Standalone Windows ZIPs include `Play Orbit Break.vbs` or `Play Relay Yard.vbs`. Keep each launcher beside its runtime and PCK. Web ZIPs need HTTP(S) hosting with the Wasm MIME type, not a `file://` launch. No Rust compiler is needed to play either script-based example.

Use a fresh extraction for upgrades. Copy or import your project deliberately; do not replace a running installation or merge old session state into the archive. Extracting does not change global PATH, shortcuts or an existing installation. Rust projects still need the optional toolchain described below.

## v0.3.0

[Download the release](https://github.com/AG064/aurum-studio/releases/tag/v0.3.0).

- `Aurum-Studio-0.3.0-windows-x64.zip`: portable Studio, CLI/MCP, Godot 4.7 runtime, ordinary and extension-enabled Web templates, notices, a copy of Orbit Break and reproduction/setup scripts.
- `Orbit-Break-0.3.0-windows-x64.zip`: standalone Windows game. Extract and open `Play Orbit Break.vbs`.
- `Orbit-Break-0.3.0-web.zip`: standalone browser game. Serve over HTTP(S) with the Wasm MIME type; double-clicking `index.html` is not supported.
- `SHA256SUMS.txt` and `verification.json`: file hashes and the versioned acceptance summary.

Extract into a writable directory. Open `Launch Aurum Studio.vbs` to start the workspace. Projects and sessions stay local to this installation; extracting a release does not change global PATH or migrate an existing installation. Use `bin/aurum.exe` for the CLI or an MCP client. No Rust compiler is required for script-only projects.

Rust projects need Rust/C++ build tools. Browser extensions additionally need the optional Emscripten SDK and dated nightly compiler. Run the included `scripts/provision-web-toolchain.ps1` with a dedicated destination under `runtime/web-toolchain`; see [Rust Web builds](RUST_WEB.md). The SDK is not bundled to keep the application download smaller.

## Verification and scope

The release is built from its tagged source, not from an untracked local executable. Release files are prepared and tested in temporary directories. `verification.json` records the release binary/runtime hashes and acceptance scope. GitHub check status is linked from the release.

Windows x64 and Chromium are the distribution acceptance targets. Linux/macOS builds are checked in CI but are not packaged desktop releases. Physical mobile devices, VR headsets, signing, notarization and app stores are not certified by this release. Live values apply without a game restart; changed code is loaded through checkpoint rebuilding. Native registration changes retain their controlled restart boundary.

Source-built packaging is available through `scripts/package-release.ps1`. It refuses an existing output directory and does not overwrite user projects or state.

Unreleased development source also corrects project `op=package` native-profile
selection. The bundled full runtime advertises debug features, so packaging
validates/exports a private source copy with release-library bytes at its
runtime-selected paths. This does not overwrite the working debug DLL. A real
Rust fixture checks the executing library's profile in the standalone package,
not only that a release DLL was produced. The 0.4.0 downloads predate this fix.
