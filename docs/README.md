# Aurum Studio documentation

This documentation covers Studio 0.4.0 and current `main`. The older v0.3.0 download predates the redesigned workbench, operation-contract discovery and Relay Yard. See [release delivery](RELEASES.md) for versioned packages, and [status and limits](STUDIO_STATUS.md) when deciding whether a target workflow is supported.

## Use Studio

| Need | Guide |
| --- | --- |
| Install, create or import a project | [Getting started](GETTING_STARTED.md) |
| Edit scenes and source, manage drafts, validate and export | [Daily workflow](WORKFLOW.md) |
| Run a browser game and produce a standalone Web bundle | [Web preview](WEB_PREVIEW.md) |
| Understand live edits, checkpoints and restart boundaries | [Runtime integration](INTEGRATION.md) and [native hot reload](HOT_RELOAD.md) |
| Compile a Rust extension for the browser | [Rust Web requirements](RUST_WEB.md) |
| Choose a release or verify its download | [Release delivery](RELEASES.md) |
| Diagnose a failed preview, stale save, MCP connection or CI run | [Troubleshooting](TROUBLESHOOTING.md) |

## Work through an agent

Read [agent workflows](AGENT_WORKFLOWS.md) for contract discovery and compact results, [daily workflow](WORKFLOW.md#mcp) for MCP configuration, and [playtests](AGENT_PLAYTESTS.md) for fresh gameplay verdicts, bounded input timelines and platform exports.

Use a disposable project for automated tests that edit files. Read-only filtering limits tool writes; it does not sandbox the scripts or native code in a project. Do not run untrusted projects with your account permissions.

## Learn from the examples

- [Relay Yard: Night Shift](../examples/relay-yard/README.md): a small 3D combat/extraction mission, imported model authoring, live values, combat-state checkpoints and native/Web packaging.
- [Orbit Break](../examples/orbit-break/README.md): a 2D campaign with three-choice upgrades, persistent records, headless campaign tests and browser play.

Each example documents controls, reproducible verification and asset notices. These are workflow examples, not evidence that every genre or device is certified.

## Contribute or inspect the implementation

- [Contributing](../CONTRIBUTING.md): checks, useful bug reports and review expectations.
- [Architecture](ARCHITECTURE.md): crates, shared project operations and runtime ownership.
- [Roadmap](ROADMAP.md): planned milestones, priorities and executable acceptance gates.
- [Project revisions](PROJECT_REVISIONS.md): next atomic-change implementation slices and recovery contract.
- [Logging](LOGGING.md): local diagnostic budgets, privacy and retention boundaries.
- [Engine modules](MODULES.md): library contracts and module boundaries.
- [Design decisions](DESIGN.md): rationale for the workbench and example presentation.
- [Changelog](../CHANGELOG.md): unreleased changes versus versioned releases.
- [CI](https://github.com/AG064/aurum-studio/actions/workflows/ci.yml): per-commit checks and retained diagnostics.

## Evidence and history

[Integration verification](INTEGRATION_VERIFICATION.md) labels dated local results and their limits. [Screenshot provenance](media/README.md) identifies real captures, controlled fixtures and design references. CI status must be checked on the relevant commit; a screenshot or compiled artifact is not a gameplay verdict.

[Earlier delivery](DELIVERY.md) and the [original Studio design](superpowers/specs/2026-08-31-aurum-studio-design.md) are historical records, not current setup instructions. Prefer the guides above for daily use.
