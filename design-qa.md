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

## User-requested game presentation follow-up

The later request supersedes the old game's shop and simple geometry, not the
approved Studio interface. The new game presents exactly three reward cards,
with weapons in that same pool and no extra purchase or continuation controls.
The backdrop and deck are bundled generated textures; ships, lighting, shadows
and combat effects are rendered in the game. The interface stays clear of the
central combat area. A portrait stack and compact landscape layout keep the
three options usable in smaller previews. Text is rasterized at its final
canvas size to avoid blurred scaled labels.

The latest local visual pass inspected the real menu, combat, two workshop
states, fullscreen view and a 390 by 844 browser layout. The 55-check gameplay/HUD
suite and seven browser scenarios passed in disposable projects, including
single-choice protection, meaningful weapon effects, live tuning and export.
A further targeted render pass verified the final small-screen text correction.
Local gameplay capture is test evidence, not a performance benchmark or a claim
of mobile-device certification. These follow-up edits are not represented by
the previously published CI run until they are published and checked again.

## Instrument HUD refinement

The HUD now has layered, clipped housings with upper-edge highlights and
inset equipment diagrams. Hull, weapon and dash form one primary cluster;
sector progress and score form a smaller secondary cluster. The center and
lower-middle remain clear. Damage leaves a short gauge echo, while the dash
dial shows actual recharge progress. Reduced motion skips the gauge trail
and animated focus transition. No extra workshop actions were introduced.

Screenshot review found crowded effect text in both short and tall portrait
modules. The corrected layouts reserve space below two-line descriptions.
The browser suite now captures embedded portrait, fullscreen portrait,
compact landscape and portrait combat, and checks that layout changes do not
advance the paused workshop. Native assertions cover the gauge values,
presentation-only damage echo, matching diagrams/shortcuts and safe label bounds.
Desktop and portrait browser checks are not physical-device certification.

## Scene and material depth

The next review found that the textured arena still had a flat silhouette and
that wire diagrams did little to give equipment a physical character. The
updated scene has raised perimeter panels and ribs, a layered reactor lens,
faceted ship hulls, cockpits, hardpoints, contact shadows and directional light.
Station armour is merged into one mesh. Surface normals reveal deck detail;
perspective separates the near and far edges. Module illustrations now use
shaded housings and emitters, with a recessed well and layered frame. Locally
bundled Barlow Condensed is used for titles and large instrument values.

Native before/after captures use the same fixture states and viewport sizes.
Review found two functional presentation issues: portrait framing cropped the
movement boundary, and the background stretched across aspect ratios. The
camera now fits the full boundary and the backdrop crops to cover the view.
The 56-check suite includes a camera contract for desktop, portrait and compact
layouts; all three ordinary moving weapon campaigns still win, while idle play
loses. Windows and Web exports were built, and the font notice was verified in
both PCKs. The earlier seven browser scenarios were not rerun for this revision:
the browser tool rejected the preview URL. Current visual evidence is native
OpenGL, with no shader diagnostics in the inspected captures.

## Campaign expansion

The larger game adds three stations, twelve encounters, three flight frames,
seven patrol types, three distinct bosses, fifteen module/weapon choices,
recovery/defense/hold objectives, a tactical archive and an original score.
Shared art and interface treatments keep those additions coherent. Hangar,
workshop, boss salvage, each station, pause builds and flight records were
captured in disposable native renderer fixtures. Long effect captions found
in review were shortened; a font-metric assertion now checks the complete
offer pool. The current deterministic suite has 85 checks.

Five ordinary scenarios covering every weapon and frame clear the full route.
The stationary scenario exposed recovery soft-locking; recovery now fails at
its displayed lockdown deadline. A diagnostic run traced shutdown warnings to
headless audio playback objects. Headless tests skip audio construction and
runtime exit hooks stop playback. The final campaign gate has no warnings.
Native music checks cover looping, mute and resume. Browser test entry flows
were updated for the hangar, but current browser execution remains unverified
after the tool's earlier URL-policy rejection. Native fixtures are explicitly
staged for repeatable visual comparisons, not human playtime or performance claims.

## Release acceptance update

The integration-fix pass subsequently exercised the expanded game in Chromium: hangar entry, movement/dash/pause, live tuning, three-choice upgrades, weapon acquisition, portrait/fullscreen layouts, save/undo, failure recovery, origin isolation and standalone export/play. These are scoped local browser runs, not physical-device or human-playtime claims. The v0.3.0 delivery runs versioned acceptance and links its GitHub checks separately.
