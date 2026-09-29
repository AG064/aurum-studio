# Contributing to Aurum

Thanks for your interest in Aurum. This guide covers the basics of
contributing, the code style, and how to add new modules.

## Code of conduct

Be kind, assume good faith, focus on the work. We're all here to make
games.

## How to contribute

1. **Open an issue first** for any non-trivial change. Module additions,
   engine API changes, anything that affects the public surface — discuss
   it before writing code.
2. **Fork the repo** and create a branch from `main`.
3. **Write code + tests.** No PR without tests for new behavior.
4. **Run checks in an isolated copy.** The Windows gate creates temporary
   source and project directories and retains its evidence:

   ```powershell
   pwsh ./scripts/tests/verify_studio.ps1 -GodotBinary C:/Tools/Godot_v4.7-stable_win64.exe
   ```

   On other hosts, copy or clone the checkout into a disposable directory
   before running the same formatting, Clippy and workspace-test commands
   defined in CI. Keep build output and runtime state out of the source tree.
5. **Open a PR** with a clear description of what changed and why.

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
device validation.

## License

By contributing, you agree that your contributions will be licensed
under the MIT License.
