# Orbit Break artwork

`orbital-nebula.png` and `station-deck.png` were generated for this project with the built-in image generation tool on 29 September 2026. They are production textures, not mock screenshots. The game renders its own ships, station structures, lighting, shadows, particles, projectiles and interface over them.

The original generated files are retained outside the repository. No third-party game art was copied. The deck is sampled with mipmaps and clipped to its circular boundary in the floor shader, with sampled normals and roughness variation. The background covers the camera view while preserving the texture's aspect ratio. The current game screenshots in the documentation are native renders of repeatable fixtures.

Display type is the locally bundled Barlow Condensed SemiBold font. Its source,
hash and OFL notice are in [fonts/README.md](fonts/README.md). The font notice is
included in every export by the game's small notices plugin.

## Backdrop prompt

Use case: stylized-concept. Asset type: production background texture for a polished indie orbital survival game, no UI. Wide 3:2 landscape space environment, dark desaturated petrol and midnight blue nebula clouds with restrained dusty copper light at the far edge, a distant eclipsed planet's thin atmospheric crescent on the far upper right, fine sparse stars, layered painterly cinematic depth, subtle illuminated dust, beautifully art-directed not rainbow space wallpaper. Center and lower center mostly calm dark negative space because a large playable space station will be rendered above it in real 3D. Crisp premium science fiction illustration with realistic material detail. No ships, no buildings, no text, no letters, no icons, no HUD, no lens-flare streaks, no frames. This is only the backdrop asset, not a screenshot of the game.

## Deck prompt

Use case: stylized-concept. Asset type: square top-down albedo texture for the floor of a 3D orbital station arena in an indie action game. Perfectly orthographic straight-down view, NO perspective. A single massive circular industrial landing/refinery deck occupying 96 percent of image width and height, centered exactly. Intricate but restrained weathered charcoal gunmetal plating, brushed steel seams, radial structural bays and service hatches around the outer 15 percent, a subtle central circular reactor socket, worn ivory lane markings and sparse muted ochre hazard chevrons near the rim. The inner 70 percent is a mostly open matte dark steel playing floor with broad irregular modular plating, a few scratches and small maintenance markings, not a uniform grid. Premium hand-authored hard-surface science fiction game texture with fine material detail, believable wear and restrained patina. Very dark navy outside the disc. Even diffuse illumination, NO strong baked shadows. No neon perimeter, no bright glow, no spaceship, no enemy, no characters, no text, no lettering, no readable numbers, no UI, no HUD. The game adds its own dynamic lighting, 3D architecture, actors and effects over this texture.
