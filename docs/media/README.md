# Documentation visuals

These images accompany the Aurum Studio 0.2 documentation:

- `aurum-banner.svg`: an earlier decorative vector banner, retained for history and no longer used in the README.
- `studio.png`: the actual browser workbench from the earlier HUD pass, with the live inspector open.
- `orbit-break-menu.png`: the title screen in the native renderer, with the current material and type treatment.
- `orbit-break-gameplay.png`: a native combat fixture showing the perspective arena and grouped instrument HUD.
- `orbit-break-workshop.png`: the current three-choice reward screen in the native renderer.
- `orbit-break-hangar.png`: the three available flight frames and their starting traits.
- `orbit-break-relay.png`: Aperture Relay, with interceptors and a sentinel.
- `orbit-break-foundry.png`: Ash Foundry, including a marked vent lane.
- `orbit-break-carrier.png`: the Carrier encounter in the native renderer.
- `orbit-break-records.png`: a staged sortie ledger and tactical contact archive.
- `design-reference.png`: the selected first visual concept. This is a design reference, not a running-app screenshot.

The Studio screenshot was captured from a disposable project on 29 September
2026 at a 1488 by 1056 browser viewport. The campaign images were captured on
30 September 2026 at 1280 by 800 in native OpenGL from disposable visual fixtures. Fixed combat values and
positions make those fixtures repeatable; they are rendered by the game, not
painted mockups. Portrait and compact captures remain in the local verification
evidence. Browser automation was blocked by its URL policy during this depth
pass, so the new game images do not claim browser execution of this revision.
Paths visible in Studio identify the demonstration checkout. No new remote CI
run is implied by these local follow-ups.

The game uses custom procedural geometry, synthesized audio, an SVG icon, two
bundled generated textures and a locally bundled display font. [Texture
provenance and prompts](../../examples/orbit-break/godot/assets/README.md) and
[font attribution](../../examples/orbit-break/godot/assets/fonts/README.md)
identify those assets. Runtime notices and the font's OFL notice are included
in exported packages.
