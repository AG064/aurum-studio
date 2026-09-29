# Aurum workbench design QA

final result: passed

## Evidence and normalization

- Source visual truth: [selected first concept](docs/media/design-reference.png).
- Implementation: [actual browser workbench](docs/media/studio.png).
- Source and implementation: 1488 by 1056 pixels; implementation viewport 1488 by 1056 CSS pixels, device scale factor 1. No browser chrome or device frame is included.
- State: graphite desktop workbench, running Orbit Break, wave two, Output expanded. The concept's game is illustrative; player position, time, weapon and contacts are dynamic in the implementation. These are not pixel-match targets.
- Both full images were opened together in the same comparison input, before and after the corrections below. Text and controls are readable in those full-resolution views, so a separate cropped-region comparison was not required.
- Additional evidence: menu, workshop, paused/live-edit states and the narrow inspector at 390 by 844 were captured by the browser acceptance suite. Failure traces and screenshots are retained by CI.

## Comparison history

1. Initial comparison found two P2 issues: generated metadata crowded the explorer, and the inspector's tall transform/script sections pushed live controls out of the initial view. Metadata is now hidden; primary game files are ordered first; XYZ is compact and Script is disclosed. The narrow layout gained an accessible Inspector toggle.
2. The next comparison found P2 viewport letterboxing and distorted game typography when filling the available height. The project now expands its rendered viewport, while HUD text and controls use one uniform scale and a centered design coordinate system. Revised wave-two and workshop captures show readable text without stretching.
3. Final paired comparison preserves the selected hierarchy: compact header, copper Run action, narrow explorer, dominant game, contextual inspector, quiet Output. The compact transform and extra live controls are intentional functional additions, not an alternative visual direction.

## Required surfaces

- Typography: system Segoe UI, 14px base, restrained 20-22px headings, monospace reserved for technical values. The concept's exact generated font cannot be identified as a licensed font asset; the native system choice is intentional. Game HUD scaling was corrected. Long paths truncate and remain available through their title.
- Spacing: stable three-column grid, aligned header, continuous preview region and consistent 4px control corners. The inspector uses compact horizontal XYZ to make room for actual live controls. On narrow screens it becomes a labelled overlay, with a persistent toggle and no horizontal document overflow.
- Colour: graphite surfaces and copper action match the source direction. Measured contrast: primary text on panel 13.21:1; muted text on panel 6.59:1; Run text on copper 7.64:1; game muted text on its dark panel 7.76:1. These measurements do not claim a complete accessibility audit.
- Imagery/icons: the central image is the real game, rendered by Godot WebAssembly. No painted screenshot replacement is used. Icons are the locally bundled, licensed Phosphor family. The game deliberately uses its existing procedural 3D artwork rather than treating concept pixels as game assets.
- Copy: concept labels have become factual runtime states. Live tuning reports an acknowledged revision. Transform/script edits explicitly require a rebuild. Export explains local output versus public hosting. No fabricated usage statistics, testimonials or unsupported platform claims are present.

## Interaction verification

Browser acceptance covers Run, keyboard movement/dash/pause, live values with unchanged session/time/hull/position, workshop purchases and weapon switching, save/undo, failed-build retention, forged preview messages, origin separation, responsive inspector access and browser export. The tested app states have no browser JavaScript errors. Rust checks cover confined paths, native-project rejection and full-size socket responses.

## Remaining boundaries

No actionable P0/P1/P2 design findings remain in the tested states. P3 follow-up: larger projects could benefit from a collapsible folder tree instead of the current filtered file list. The underlying low-poly game art is deliberately simple. Firefox/Safari, screen-reader operation inside the WebGL game, physical touch devices, mobile exports and VR hardware were not certified by this pass.

## Implementation checklist

- Preserve the selected visual direction and working project operations.
- Use real gameplay and acknowledgements as evidence, not concept screenshots.
- Keep source metadata and infrequent tools out of the main working area.
- Retain screenshots, traces and reproducible disposable test commands.
