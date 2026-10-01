# Design direction

The selected first concept is a compact graphite workbench with a copper primary action, quiet navigation, a dominant playable canvas, a contextual inspector and a collapsible output area. The implemented interface retains that hierarchy. It adds live tuning without turning the app into a dashboard.

## Lessons applied

- [Linear's interface redesign](https://linear.app/now/how-we-redesigned-the-linear-ui): align navigation, headers and content so application chrome supports the work. Studio now has one stable header and a large continuous game area.
- [NN/g on minimalist design](https://www.nngroup.com/articles/aesthetic-minimalist-design/): reduce irrelevant information, not useful labels. Runtime diagnostics, native tools and advanced properties remain available but no longer compete with Run.
- [NN/g on progressive disclosure](https://www.nngroup.com/articles/progressive-disclosure/): keep common actions immediate and advanced controls discoverable. Script attachment and uncommon scene actions are labelled disclosures.
- [W3C contrast guidance](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html): readability is measurable. Quiet text still needs contrast, keyboard focus needs a visible treatment, and state must not rely on colour alone.

The controls use the system UI font and bundled Phosphor icons with their MIT notice. No external font requests, analytics, gradient hero artwork or fabricated product metrics were added. README imagery comes from the running app.

## Behaviour is part of the design

Run starts a real game. Saving a slider is not enough to claim live editing: the game must acknowledge the revision. Structural edits are labelled as requiring a rebuild. Failed exports preserve the working preview. Source saves have draft retention, conflict checks and undo, and dependent controls cannot race a save.

The workshop retains untimed decisions and distinct weapons, but the owner's follow-up simplified it to exactly three cards with one selection per stop. Weapons arrive as rewards alongside weapon-specific evolutions, survival and support choices. There is no separate economy or weapon-picker layer. This supersedes the earlier shop layout. The Aphelion project itself is not modified.

Game presentation is separate from Studio's interface. The game now uses bundled orbital artwork, bevelled ship geometry, shadows, restrained edge HUDs, pooled sparks, weapon-specific firing effects and a visible shield instead of blinking the pilot away. The existing Studio design is preserved. The generated textures and their prompts are documented in the example's assets folder.

The HUD follows the spacecraft rather than Studio's application chrome. Clipped instrument housings have a shallow shadow, a lit upper edge and inset gauges. Hull, weapon identity and dash charge occupy one primary cluster; sector progress and score form the secondary cluster. Persistent instruments occupy about 10% of the desktop design canvas and 20% in portrait. Equipment diagrams distinguish workshop choices without adding actions. Motion responds to damage, hover and focus, with reduced-motion handling and no idle pulsing. The diagrams and material treatment are native drawing code, not additional bitmap assets.

The later depth pass extends that material language into the scene. Raised
perimeter armour and ribs replace a flat silhouette, while a reactor lens,
surface-normal shading, contact shadows and warm/cool light separate the forms.
Ships have vertical hull walls, chamfers, cockpit surfaces and armoured hardpoints.
A restrained perspective view preserves the full arena movement boundary in
portrait; the backdrop crops to fit instead of stretching. Large values and
titles use the locally bundled Barlow Condensed font. Module bays show shaded
equipment illustrations inside recessed wells. Their housings have a distinct
rim, face and lower edge, with a thicker keyboard-focus outline.

The reusable personal `product-ui-craft` skill captures these decision rules, reference preservation and screenshot QA. It deliberately does not prescribe Aurum's dark palette or three-column layout for unrelated apps. The actual comparison and remaining boundaries are in [design-qa.md](../design-qa.md).

## Campaign presentation

Orbit Break now has a fixed twelve-encounter route across three stations. Each
station has an authored threat mix, a physical silhouette, a colour treatment
and a score arrangement. New objectives require movement or protection, and
the three bosses have distinct attacks. Modules introduce combat relationships:
dash into a charged volley, convert linked Arc hits into repairs, or chain a
kill into area damage. Three flight frames provide separate starting traits.

The hangar describes those traits in three cards. Workshops keep their three
choices and pause indefinitely. Reactor health, caches, hold time and recovery
lockdown appear in the existing objective instrument. The pause surface lists
the fitted build; Flight Records holds the sortie ledger and tactical contacts.
Commendations come from actual sortie events. The end screen identifies hull,
reactor or recovery failure.
