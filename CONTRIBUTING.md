# Contributing to Aurum

Start from `main` and read the [documentation index](docs/README.md),
[architecture](docs/ARCHITECTURE.md) and [current boundaries](docs/STUDIO_STATUS.md).
The downloadable release and development source are different revisions.

## Code of conduct

Be kind, assume good faith, focus on the work. We're all here to make
games.

## How to contribute

1. Discuss new modules, engine API changes and other public-contract changes
   in an issue before implementation. Small, focused fixes can go directly to a PR.
2. Create a branch from `main`. Preserve unrelated local work; do not reset
   a dirty checkout to make a test pass.
3. Add regression coverage for changed behavior. Keep UI and backend
   contracts aligned across CLI, HTTP and MCP.
4. Run the checks appropriate to the change, in an isolated copy. The Windows
   gate snapshots tracked and nonignored work and retains its evidence:

   ```powershell
   pwsh ./scripts/tests/verify_studio.ps1 -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe
   ```

   On other hosts, copy or clone the checkout into a disposable directory
   before running the same formatting, Clippy and workspace-test commands
   defined in CI. Keep build output and runtime state out of the source tree.
5. Open a PR explaining the user-visible outcome, exact checks, remaining
   limits and any migration or restart requirement.

## Choose a verification gate

| Change | Required evidence |
| --- | --- |
| Documentation or repository presentation | Documentation checker tests, local links/images/headings and review of any changed commands or screenshots |
| Rust behavior or shared contracts | Formatting, strict all-feature/all-target Clippy, workspace tests and the affected real interface acceptance |
| Studio controls or preview lifecycle | Real-backend workbench/browser scenarios, keyboard/error states and screenshots from a disposable project |
| Game behavior or assets | The example verifier, relevant rendered/browser inputs and actual package execution when packaging changes |
| Installation or release | Temporary installation/package acceptance, notices and checksums; do not infer device support from compilation |

For documentation-only work, copy or clone the repository into a temporary
directory, then run there with Node.js 24 or newer:

```powershell
node --test scripts/tests/check-docs.test.mjs
node scripts/check-docs.mjs
git diff --check
```

The checker covers repository-local Markdown links, images and headings,
including reference definitions and common HTML links. It does not fetch
external URLs, validate command semantics or replace a Markdown render review.
It uses no npm dependencies. Fixture data is created under the OS temporary
directory and retained for inspection.

For browser checks, use the pinned dependencies and commands in
[Web preview](docs/WEB_PREVIEW.md). The runner uses disposable game copies,
has zero retries and retains failure screenshots, traces and bounded HTTP
diagnostics. Do not replace real execution with a fixture response or remove
an assertion to obtain a green check. A fixture-server UI pass is not evidence
that the Rust backend or exported game works.

CI runs on Linux, Windows and macOS for Rust tests/builds, and Windows for
native runtime and Chromium workflows. These are separate gates, not mobile
or VR certification. See [CI](https://github.com/AG064/aurum-studio/actions/workflows/ci.yml).

## Report a useful bug

Include the exact commit or release, OS, runtime version, script-only versus
Rust project, steps, expected/actual outcome and the failed operation. For a
CI failure, link the run and name the job. A minimal disposable reproduction
is preferable to an entire private project.

Keep the first failing result and its diagnostics. Redact session tokens,
credentials, local account names and private paths before attaching logs.
Do not upload an MCP configuration containing secrets. Clearly label what
was tested, what failed and what has not been run.

## Code style

### Rust

- `rustfmt` default style. No custom rules.
- `clippy` clean. No `#[allow(...)]` without a comment explaining why.
- Public API gets a doc comment. Examples in the doc comments are
  exercised by doc tests where possible.
- Errors via `thiserror`; no `anyhow` in library code.
- No `unsafe` without a `// SAFETY:` comment.

### GDScript

- Tabs for indentation (Godot's default).
- `class_name` only for files meant to be referenced globally. Module
  internals stay private.
- Type hints on function signatures.
- No `print` in shipped game code; use a logging helper or the dev
  console. `print` is fine in demos and the dev console itself.

## Module contract

When you add a new module:

1. Create a crate at `crates/aurum-<name>/` with the same shape as the
   existing modules (`Cargo.toml` + `src/lib.rs` + tests).
2. Add the crate to the workspace `Cargo.toml`.
3. Define component types with `Serialize` + `Deserialize` so save/load
   works.
4. Add a doc comment block at the top of `lib.rs` listing the component
   names and field shapes (this is the contract with GDScript).
5. (Optional) Add a GDScript shim under
   `godot/addons/aurum/scripts/aurum_<name>.gd` if GDScript code
   needs to use the module's components.
6. Add a demo under `godot/demos/<name>/` if the module is
   visual or interactive.
7. Add a starter template under `godot/templates/<name>/` that
   other projects can copy.

The component-name contract is: a Rust component called `"Foo"` has
the same name in GDScript, with the same field names, in the same
shape. Mismatches silently drop fields, so be strict.

## Releasing

The maintainer cuts releases. The current version lives in each
crate's `Cargo.toml` (kept in sync via the workspace `version` field).
CI uploads the CLI and native libraries as workflow artifacts for its build
matrix. Those artifacts are not signed installers and do not include the
Godot runtime. Release claims must distinguish compilation from runtime and
device validation. Follow [release delivery](docs/RELEASES.md); a source push
does not publish a new package or update an installed application.

## License

By contributing, you agree that your contributions will be licensed
under the MIT License.
