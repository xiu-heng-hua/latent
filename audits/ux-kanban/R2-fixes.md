# Round-2 fixes — Kanban (post-testing feedback)

Fixes for issues found while testing the UI/UX overhaul. Same rules as the main board:
**neutral code comments/commits** (no reference to this file), **baseline green** after each
(`cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace -- --test-threads=1`),
**don't change rendered pixels** except where a card explicitly reworks geometry *preview* timing,
and **one conventional commit per epic**. Implementation by sub-agents. All `file:line` anchors below
were captured from the current tree and may drift — re-confirm before editing.

## Decisions answered by research
- **Keystone stays.** Capture One, Lightroom (Transform), and darktable all ship perspective/keystone
  correction — it is expected in this class of tool. We fix its interaction, not remove it.
- **RAW extensions** are expanded to the full LibRaw/dcraw set (see C1).
- **Taskbar identity** on GNOME/Wayland comes from the window **app_id** (matched to a `.desktop` file),
  not from `with_icon` — see B3.

---

## R2-A — Geometry tool interaction model (crop / straighten / keystone)  *(the big one)*

**Shared root cause.** The live preview is `render(base, settings)`, which **bakes geometry
(crop, straighten, keystone) into the texture**. So while a geometry tool is active, every handle
drag re-renders a cropped/rotated/warped, **re-fitted** texture — hence the image "moves," the area
outside the crop disappears, the keystone handles appear static while the image warps, and on CPU the
expensive warp re-runs every drag frame (the "renders forever" lag). `ViewTransform` fits to the
current texture size (`canvas.rs` `compute_fit_scale` ~117–122, used ~278), so a changing texture =
a moving view.

**Target model (all three geometry tools behave like a good crop tool):**
- While a geometry tool is **active**: show the **full, stable, un-cropped/un-warped image**; the tool's
  **handles/overlay move** to show the *pending* geometry; the area outside a crop is **dimmed, not
  clipped**; pan/zoom are manual; **Fit** (and zoom) are **relative to the tool's region** (the crop
  rectangle / the corrected quad).
- On **commit/exit**: apply the geometry and **fit the viewport to the result** (cropped/leveled/warped
  region only).
- Because geometry is applied **once on commit** (not per drag frame), the CPU warp cost and the
  view-jitter both disappear.

### R2-A1 — Suppress baked geometry while a geometry tool is active; apply on commit
- Files: `latent-app/src/gui/canvas.rs` (the render-trigger + draw), `app.rs` (`render_preview`/`poll_render`/`RenderState`, the per-frame `dirty`/repaint logic; tool-active repaint at canvas.rs ~355–359), `tools/mod.rs`, `tools/{crop,straighten,keystone}.rs`.
- Approach: when the active `CanvasTool` is a geometry tool, render the preview with that tool's
  geometry **neutralized** (e.g. render the base without crop/straighten/keystone framing — a render
  variant or a temporary `Settings` clone with the in-progress geometry cleared), and draw the tool's
  handles/overlay from the *pending* values. Commit applies the real geometry (one render). Ensure the
  tool-active `request_repaint` does **not** spawn renders (repaint ≠ render); renders fire only on a
  committed change. Verify a **Geometry reset** cancels/settles promptly (no stuck render) — the
  keystone "renders forever on CPU" must be gone.
- Acceptance: dragging crop/keystone handles leaves the image **stationary** with **moving handles**;
  the outside is visible (dimmed); on CPU, keystone no longer lags or renders continuously; resetting
  geometry returns to the full image immediately. A test around the render-gate proving no render is
  spawned per drag frame while a tool is active.

### R2-A2 — Fit-to-region during a tool; fit-to-result on exit
- Files: `canvas.rs` (`ViewTransform`, zoom/fit handlers), `app.rs` (view state on tool enter/exit).
- Approach: give the view a notion of "active region" = the crop rect / corrected quad while a geometry
  tool is active; **Fit** and the zoom ladder operate on that region (the whole image still draws,
  dimmed/spilling beyond the viewport, pannable). On tool exit, set the view to fit the committed result
  region. Keep one `ViewTransform` owner.
- Acceptance: in crop mode, **Fit** frames the crop rectangle; after exiting, the viewport shows only the
  cropped portion; manual pan/zoom work throughout.

### R2-A3 — Crop polish: dim-not-clip + "cropped" indicator
- Files: `tools/crop.rs` (`draw_overlay` ~305–353), `panels/controls.rs` / `panels/sections.rs` (the crop control header).
- Approach: ensure outside-crop is **dimmed**, never clipped, while editing (follows from A1). Add a clear
  **active-crop visual signal** (e.g. an accent dot/filled icon on the Crop subsection header and/or the
  crop toolbar button) whenever `geometry.crop` is `Some` and non-full.
- Acceptance: a set crop shows a persistent indicator; entering crop mode shows the whole image dimmed
  outside the rect.

### R2-A4 — Straighten as an intuitive 2-point horizon line
- Files: `tools/straighten.rs` (`level_angle` ~24–48, `draw_line` ~52–63).
- Approach: present it explicitly as "draw a line along a horizon or a vertical you want made level."
  Keep the image **fixed** (un-rotated) while the user draws the 2-point line (A1 already suppresses the
  rotation during editing); derive the angle from the line; on commit, apply rotation. Add a clear prompt/
  hint and endpoint handles that read as draggable.
- Acceptance: drawing a line along a tilted horizon levels the image on commit; the image does not rotate
  underfoot while drawing; the gesture is discoverable.

### R2-A5 — Auto-constrain to the valid region (remove black wedges) after straighten/keystone
- Files: `latent-pipeline` (an inscribed-valid-rectangle helper) and/or `latent-app` geometry apply; integrate with `geometry.crop`.
- Approach: after a rotation/perspective warp, the output has black corners. Compute the **largest valid
  (non-black) axis-aligned rectangle** and constrain the displayed/exported result to it (Lightroom's
  "Constrain Crop"). Either auto-set/limit `geometry.crop` to the valid region or clip the framing. Make it
  a toggle if cheap (default on). This is shared by straighten and keystone.
- Acceptance: a leveled or keystone-corrected image shows **no black wedges**; an explicit user crop still
  composes correctly; round-trips through the sidecar.
- Note: medium complexity (inscribed-rectangle math under rotation + projective warp). If the projective
  case is hard, ship the rotation case first and constrain keystone conservatively.

---

## R2-B — Icons & window identity

### R2-B1 — Real flip/rotate glyphs (fix the □ squares)
- Files: `latent-app/src/gui/icons.rs` (name→codepoint table ~15–36), `panels/toolbar.rs` (~65–83).
- Root cause: rotate/flip buttons use **hard-coded Unicode arrows** (`⟳ ⟲ ⇋ ⇅`) that bypass the Phosphor
  icon font; `⇋`/`⇅` (flip) have **no glyph** in the loaded fonts → render as tofu □.
- Approach: add `rotate_cw`, `rotate_ccw`, `flip_h`, `flip_v` to the icon table using the correct **Phosphor
  PUA codepoints** (Phosphor has `arrow-clockwise`, `arrow-counter-clockwise`, `flip-horizontal`,
  `flip-vertical`); the toolbar uses `icon_button` like the other tools. Verify each renders (not □).
- Acceptance: all four buttons show real, themed icons.

### R2-B2 — Distinguish undo/redo from rotate
- Files: `icons.rs`, `toolbar.rs`.
- Root cause: undo/redo are circular arrows (`e038`/`e036`) and rotate cw/ccw are also circular arrows →
  visually identical.
- Approach: make the two pairs visually distinct — e.g. undo/redo use **arc / U-turn** style arrows
  (`arrow-arc-left`/`arrow-arc-right` or `arrow-u-up-left`/`…-right`), rotate uses the **circular** glyphs
  (and flip uses the flip glyphs from B1). Pick distinct Phosphor codepoints.
- Acceptance: undo/redo and rotate are clearly different at a glance.

### R2-B3 — Window app_id + taskbar identity
- Files: `latent-app/src/gui/app.rs` `run()` (ViewportBuilder ~403–412; no `with_app_id` today), plus a packaging artifact.
- Root cause: no **app_id** set → GNOME shows "Unknown" and a generic icon; `with_icon` is not used by the
  Wayland taskbar (it matches `app_id`→`.desktop`).
- Approach: set `.with_app_id("latent")` (or a reverse-DNS id, kept consistent with the eframe app name).
  For a proper **taskbar icon** on GNOME/Wayland, ship a `latent.desktop` (with `Icon=` and
  `StartupWMClass=latent`) + an installed icon, and document the install step in the README; note that in a
  bare `cargo run` dev session the generic icon may persist until the `.desktop` is installed.
- Acceptance: the taskbar shows "latent" (not "Unknown"); with the `.desktop`+icon installed, the app icon appears.

---

## R2-C — File-open dialog RAW extensions

### R2-C1 — Fix case-sensitive matching (the real bug) + expand the set
- Files: `latent-app/src/gui/dialogs.rs` (`RAW_EXTENSIONS` 200–214, `pick_raw_file` 219–227).
- **Root cause (corrected):** `nef` is **already** in the filter (line 201). The reason `.NEF` files don't
  show is that cameras write **uppercase** extensions (`DSC_0001.NEF`) and rfd's Linux backend (XDG portal /
  GTK) matches the `add_filter` extensions **case-sensitively** — `nef` ≠ `NEF`. Adding more lowercase
  extensions would not have fixed it.
- Approach:
  1. **Case-insensitive filter:** emit each extension in **both lower- and upper-case** (e.g. `nef` + `NEF`)
     — simplest robust fix across rfd backends — by generating the filter list from the base set via
     `to_lowercase()`/`to_uppercase()` (dedup). (Mixed-case like `.Nef` is rare; lower+upper covers real files.)
  2. **Expand** the base set to the LibRaw/dcraw-supported list. Current base (20): nef nrw cr2 cr3 crw arw
     sr2 srf dng raf orf rw2 pef srw raw rwl iiq 3fr fff x3f. **Add:** `ari bay cap cs1 dcr dcs drf eip erf
     gpr k25 kc2 kdc mdc mef mos mrw obm ori ptx pxn qtk rdc rwz sti`. Cite LibRaw's supported-extension list
     in a neutral comment. Keep the "All files" fallback (the true gate is `latent_raw::unpack`, per the
     existing doc-comment).
- Acceptance: an uppercase `.NEF` (and other makers', any case) shows in the Open dialog by default; a unit
  test asserts the generated filter contains both `nef` and `NEF`; "All files" still available.

---

## R2-D — Before/after split divider

### R2-D1 — Make the split divider draggable (or remove split)
- Files: `latent-app/src/gui/canvas.rs` (Split arm ~289–305), `Session` (a split-position field), `app.rs`.
- Root cause: the split is hard-coded at the image **center** (`mid = image_rect.center().x`), not draggable.
- **Decision: improve (draggable divider).** Store a normalized split position on `Session`; draw a draggable
  divider handle; dragging moves the seam (a "curtain" wipe), both halves through the same `ViewTransform`
  so features stay registered. Default to center; clamp to the image rect.
- Acceptance: the seam can be dragged left/right and the comparison stays aligned across it.

---

## R2-E — Status-bar layout stability  *(multiple solutions — pick one)*

### R2-E1 — Stop the pixel-readout jitter
- Files: `latent-app/src/gui/panels/statusbar.rs` (~12–80).
- Root cause: the variable-width, conditional **pixel readout** sits **between** fixed fields (zoom · dims ·
  **readout** · backend · state) in a left-to-right `ui.horizontal`, so it pushes the backend/state around as
  the cursor moves.
- Options (complexity / drawback):
  1. **Reorder: readout last (far right).** Move the pixel readout to the end so it grows into empty space and
     pushes nothing. *Trivial. Drawback: the readout itself still changes width (but nothing else moves).*
  2. **Fixed-width reserved slot.** Always render the readout in a monospace field padded to its max width
     (e.g. `"0000,0000  sRGB 255 255 255"`), placeholder when off-image. *Low. Drawback: reserves space even
     when empty; must size the max width.*
  3. **Right-aligned readout region.** Put the volatile readout in a `right_to_left` sub-layout so it extends
     into the center gap. *Low. Drawback: slight visual asymmetry.*
  4. **Move readout out of the status bar** into a small cursor-following loupe / dedicated info chip. *Medium.
     Drawback: extra UI surface; can occlude the image.*
  5. **Fixed per-field sub-rects** (manual allocation with fixed widths). *Medium. Drawback: rigid; width tuning.*
- **Decision: option 1 (readout last).** Move the pixel readout to the **far right** of the status bar so its
  variable width grows into empty space and pushes nothing; the fixed fields (zoom · dims · backend · state)
  keep a stable left-to-right order. (Trivial; the readout itself still changes width, but nothing else moves.)
- Acceptance: moving the cursor over the image does not move the zoom/dims/backend/state fields.

---

## R2-F — Sidebar information architecture (group by purpose)

The Geometry section (and others) is a **flat list** of sliders under one `CollapsingHeader` with no
sub-grouping (`sections.rs` ~17–26; `controls.rs` Geometry ~230–247). Group controls into per-purpose
**subsections**, each with its graphical-tool button at the subsection header.

### R2-F1 — Geometry → per-tool subsections with tool buttons
- Files: `panels/controls.rs`, `panels/sections.rs`, `widgets.rs`.
- Approach: nest labeled subsections inside Geometry:
  ```
  Geometry
    Cropping        [crop-tool button]      left · top · width · height · aspect ratio
    Straighten      [level-tool button]     angle
    Keystone        [keystone-tool button]  vertical · horizontal
    Lens                                     enable · detected lens · …
    Vignette                                 amount
  ```
  The subsection's tool button activates the matching `CanvasTool` (ties to R2-A); aspect-ratio lives in the
  Cropping subsection (it constrains the crop tool). Keep the one-undo-step-per-gesture semantics and the
  per-section reset (extend to per-subsection where natural).
- Acceptance: Geometry reads as discrete tools; each graphical tool is launched from its subsection header;
  the crop subsection owns aspect ratio.

### R2-F2 — Group the other sections by purpose
- Files: `panels/controls.rs`, `panels/sections.rs`.
- Approach: apply the same "group related sliders + their helper into a subsection" pass elsewhere where it
  helps — e.g. Tone (contrast/highlights/shadows/blacks together), Detail (Sharpen amount/radius;
  Noise reduction luminance/color/radius as their own subgroups), Effects (Clarity, Dehaze, Vignette). Keep
  it light where a section is already single-purpose.
- Acceptance: each section's controls read as labeled, purpose-grouped subgroups rather than a flat stack.

---

## Suggested order
1. **R2-A** (interaction model) — the biggest UX win and it subsumes the crop/keystone/straighten/perf complaints. *(commit 1)*
2. **R2-F** (sidebar IA) — pairs naturally with A (subsection tool buttons drive the canvas tools). *(commit 2)*
3. **R2-B** (icons + app_id), **R2-C** (extensions), **R2-D** (split), **R2-E** (status bar) — independent, quick. *(commits 3–6)*
