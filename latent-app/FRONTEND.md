# Frontend conventions (`latent-app`)

Conventions for the egui/eframe UI. Keep these in mind when adding or changing
controls.

## Layout stability — don't make elements appear and disappear

Avoid controls or indicators that pop in and out based on state: they shove their
neighbours around and read as noise. The **only** elements that may appear/disappear
are ones the **user explicitly drives** (e.g. the eye button that collapses a
subsection body, or expanding a section).

Prefer state changes that keep an element **in place**:

- **enable/disable** a button instead of inserting/removing it;
- **grey out** a control (`ui.add_enabled_ui(false, …)`) instead of hiding it;
- change an **icon, label, or tint** instead of adding a second element.

Worked examples (both already in the code):

- The "this section was modified" signal is the **reset button's enabled state** —
  the reset button is always present and is enabled only when the section differs
  from its defaults. There is **no separate dot** that would shift the header.
- A disabled subsection keeps its body **in place and greyed**; whether the body is
  shown is a separate, user-driven **eye button**. Toggling the enable checkbox never
  moves the layout.

## Separation of concerns: image vs UI

**Enable/disable acts on the image; show/hide acts on the UI.** They are distinct
controls and must never imply each other:

- a **checkbox** turns an effect on/off (changes the render);
- an **eye button** shows/hides that subsection's body (pure UI, no render change).

## Persisted UI state

Persist view/UI preferences in the app config (panel width, section and subsection
open/closed state, last directories, backend preference, …), keyed by **stable string
ids**, never by the display label (so renaming a header can't orphan saved state).

## Edits and history

- One slider drag / gesture = **one undo step** (begin on start, commit on end; a
  net-zero change records nothing).
- Pure UI state — tool selection, view (zoom/pan), collapse state, toggle stashes —
  lives on `Session`/`Config`, **not** in the edit history.

## Rendering

- UI code **never changes rendered pixels.** The preview uses the same output
  transform as export; scopes and overlays *read* the preview, they don't alter it.
- Develop/render/export run **off the UI thread** (the worker); the window stays
  responsive with a loading state. Anything non-`Send` (e.g. the lensfun DB) stays on
  the main thread.

## Visual design

- Neutral dark theme. The canvas surround is a **neutral gray** (equal R=G=B) so it
  casts no colour onto the photo.
- Spacing, rounding, and the single accent come from the `theme` module; numeric
  readouts use the monospace text style.
- Status-bar fields keep a **stable order**; a volatile readout (the hover pixel) is
  pinned to one end so it never jostles the fixed fields.

## Icons & fonts

- Icons are glyphs from the embedded icon font, resolved through the name→codepoint
  table in `icons.rs`. **Verify a new glyph actually exists in the bundled font**
  before using its codepoint, or it renders as a tofu box.
- A subsection's graphical-tool activator is a right-aligned **icon** on its header.

## Controls layout

- Group related controls into **purpose subsections**; a graphical tool launches from
  the header of the subsection it belongs to.
- An optional effect is a **header checkbox**. A subsection that is *only* an enable
  collapses to a single checkbox header — never a checkbox nested under a label.
