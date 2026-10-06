# Agent project contracts

The compact MCP profile remains three tools. Query `describe` when a task needs more detail instead of loading every operation's instructions at startup:

```json
{"op":"describe"}
{"op":"describe","operation":"capture"}
```

The catalog reports read/write classification and required fields. An operation-specific result includes a request schema and, for gameplay, input-event examples and constraints. Discovery is available without an editor or runtime import. It does not grant execution permission.

MCP operation lists are generated from the shared backend registry. `capture`, `web_build`, input timelines, capture sizes, `pack` exports and wall budgets are advertised. Play accepts up to 3,600,000 frames and 600 wall seconds; a fixed frame rate does not guarantee deterministic game logic.

For protocol `2025-06-18`, successful object results include `structuredContent` and compact JSON text for compatibility. Project tools advertise an object output schema. Negotiated `2025-03-26` and `2024-11-05` sessions keep text results without the newer fields. Compact text reduces formatting bytes; this is not a measured guarantee of a particular model's token or latency reduction. Large discovery should still be filtered by task, such as `classes` with a query.

Project execution errors retain the object shape with `ok:false` and an `error` string. The wire format follows the [MCP tool-result specification](https://modelcontextprotocol.io/specification/2025-06-18/server/tools).

An isolated comparison against the published 0.3.0 executable measured the compact Studio tool catalog at 5,231 UTF-8 JSON bytes before and 4,858 after, retaining three tools while adding the missing operations and fields. This is a 7.1% catalog-byte reduction, not a model-token benchmark. Test receipts retain binary hashes; repeat the comparison if the catalog changes.

## A verified 3D workflow

[Relay Yard: Night Shift](../examples/relay-yard) is a small complete 3D action-extraction game that exercises the authoring and game workflow:

1. Discover the operation contract through a real stdio MCP session.
2. Import and instance a glTF resource in a transactional scene edit, then reopen the scene.
3. Save source with the preceding hash and undo it without losing the original.
4. Validate and execute a bounded, physics-driven game with a fresh verdict.
5. Send real inputs and live properties, capture rendered frames, and retain receipts.
6. Rebuild a browser preview and restore compatible mission state.
7. Package and execute a standalone Windows game.

The game's version 3 checkpoints validate before changing the mission and reconstruct enemies, projectiles, hull, heat, ability cooldowns, cargo, mission progress and exact RNG state. Future-version or malformed checkpoints leave the previous state intact. Browser acceptance checks paused combat, enemy identities/health, player position and live tuning after replacement. The original fixture's version 2 saves are deliberately refused because they contain no combat state. A separate detached migration helper remains available for compatible game schemas.

## Boundaries

This does not implement multi-file atomic change sets, a malicious-code sandbox, a generic native-process checkpoint handoff, mobile SDK provisioning or headset certification. Project code runs with the user's permissions. Run tests in disposable copies. Android/iOS/VR capability must be qualified on the relevant toolchains and actual devices.

The next practical gates are native checkpoint handoff, representative asset/material/animation workflows, target-specific preflight, and independent device tests. They should be tracked separately from this contract and 3D reference milestone.
