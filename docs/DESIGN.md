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

The workshop borrows the useful principles from the earlier Aphelion direction: untimed decisions, distinct weapons, explicit upgrade costs, meaningful repairs and readable boss warnings. It does not import the original project's code or modify that project.

The reusable personal `product-ui-craft` skill captures these decision rules, reference preservation and screenshot QA. It deliberately does not prescribe Aurum's dark palette or three-column layout for unrelated apps. The actual comparison and remaining boundaries are in [design-qa.md](../design-qa.md).
