# Implementation Plan — UI/UX Overhaul + Lens-Matching Fix (Kanban Board)

This board turns a **drastic UI/UX improvement** of the `latent` editor — plus one **lens-database-matching correctness fix** the user is actively hitting — into an executable backlog. Work is decomposed into **10 epics**; each epic file holds **cards**, and every card carries a **heads-up** (the context an implementer — here, a subagent — needs to pick it up cold) and an **acceptance** section (behavior + tests that prove it done).

**Status:** planning artifact. The board itself is working material kept **out of git history** (like the rest of `audits/`); the code lands as one **neutral conventional commit per epic** (no reference to this board in comments or messages), baseline green at each checkpoint.

## Product decisions (locked with the user)
1. **Single-image editor, deep polish** — `latent` stays an open-one-RAW-at-a-time developer; we make that excellent (no catalog/library). Shapes **U4**.
2. **Pragmatic-but-minimal dependencies** — one firm new crate, **`rfd`** (native OS file dialogs). Histogram/scopes are **hand-drawn** with `egui::Painter` (no `egui_plot`). UI/icon fonts are **embedded TTFs** via `include_bytes!` + `FontDefinitions` (no crate). `egui_extras` only if a specific card proves it necessary.
3. **Lens correction is OFF by default and toggleable** — no silent auto-apply on load (today `gui::run` force-applies a detected profile). Shapes **U6-C5** and removes the auto-apply.
4. **EXIF orientation is honored** — portrait shots currently display sideways because LibRaw's `sizes.flip` is never read; auto-orient on develop, plus a manual rotate/flip. Shapes **U1-C1** and **U2-C3**.
5. **Lens DB matching is brand-agnostic** — the "NIKON CORPORATION" vs "Nikon" miss is fixed with lensfun's own fuzzy matcher + LibRaw's normalized names, **no per-vendor hack**. Epic **L0**.

## How to use this board
- An epic is a self-contained stream; dependencies are listed per epic and per card (`Depends on` / `Blocks`).
- A card is one cohesive unit for one subagent, with explicit acceptance/tests.
- Each card's **heads-up** names the approach, the gotchas, the authoritative reference, the `file:line` anchors, and cross-cutting obligations.
- Pick cards in an epic top-to-bottom unless a `Depends on` says otherwise.

## Card format
```
### EN-Ck — <title>
- Implements: <board goal / decision>     Priority: <Critical|High|Medium|Low>
- Crates/files: <paths>
- Depends on: <card ids | —>              Blocks: <card ids | —>
- Heads-up: <approach, gotchas, the why, references, cross-cutting/test obligations>
- Acceptance: <what proves it done — behavior + named tests>
```

---

## Global rules (apply to every card)
1. **Baseline stays green.** `cargo fmt --check`, `cargo clippy --workspace --all-targets` (zero warnings), and `cargo test --workspace` (currently **313 passing**) pass after every card. Tests are serialized where the GPU device is shared (`--test-threads=1`) — keep that working.
2. **The render output is sacred.** UI/UX work must **not change rendered pixels.** The preview transform (`gui::to_color_image` → `latent_export::to_srgb8`) and the `latent_pipeline::render` math stay byte-identical; the CPU↔GPU equivalence tests stay green. Do not touch backend/shader math from a UI card (the only pipeline change here is additive geometry: manual rotate/flip in U2-C3, and the orientation applied in develop in U1-C1).
3. **One coordinate transform.** **U1-C2** owns the screen↔image mapping (a `ViewTransform` built from fit/zoom/pan state); **U2**, **U5-C2**, **U6-C4/C6** consume it — they never recompute screen↔image math. Masks and crop stay **normalized `[0,1]` over the oriented image** and are evaluated in SOURCE space exactly as today.
4. **Forward-compatible data model.** Every new field on `Settings`/`Adjustments`/`Geometry`/`LensProfile`/`Metadata` is added with `#[serde(default)]` (or a defaulted constructor) so existing `.ron` sidecars still load — the project's established forward-compat rule. New: `Geometry` manual orientation (U2-C3), a lens-enabled gate (U6-C5).
5. **Never block the UI thread.** Develop, render, and export run on the worker thread via the existing `RenderState`/`spawn`/`poll_render`/`RenderJob` machinery (`gui.rs:153-329`). Opening a file post-launch re-develops **off-thread** with a visible loading state (U8-C1) — `gui::run` currently develops synchronously before the window opens; that moves behind the async path.
6. **Pragmatic-but-minimal deps (decision 2).** New external crate: **`rfd`** only. Scopes via `egui::Painter`. Fonts/icons embedded (OFL-licensed TTFs committed under `latent-app/assets/`, loaded with `include_bytes!`). Pin versions; a card that wants any other crate must justify it in its heads-up.
7. **Product decisions are pinned by tests/behavior** (see above): lens OFF by default (U6-C5), orientation honored (U1-C1/U2-C3), fuzzy brand-agnostic matching (L0). Each carries a test or a documented behavior check.
8. **Neutral history.** No comment or commit message references this board or any `audits/` material. Conventional commits, **one per epic**, in dependency order; baseline green at each.
9. **Atomic, non-fatal persistence.** Sidecar (`<raw>.ron`) and the new app config are written **temp-then-rename**; a failed write raises a toast (U8-C2), never crashes or truncates the file. Autosave-on-idle (the existing `autosave`) is preserved.

---

## Epics & dependency order

```
L0  Lens DB matching  (independent correctness fix — DO FIRST) ───────────────► feeds U6-C5

U0  Foundation & shell  (modularize gui.rs, theme, fonts, icons, menu/status bar) ──► all UI epics
      │
      ├─► U1  Viewport  (EXIF orientation, fit/zoom/pan, before/after, pixel readout)
      │        │
      │        └─► U2  On-canvas tools  (crop, straighten/rotate, keystone, mask handles, overlay, brush)
      │                                                  ▲
      ├─► U3  Panels & controls  (collapsible, slider widget, resize, tooltips, reset) ─┐
      │                                                                                  ├─► U6  Feature parity
      ├─► U4  File & session  (rfd open/save, drag-drop, recent, export dialog, config) ─┘   (HSL, mixer, full
      │        │                                                          ▲                    locals, multi-mask,
      ├─► U5  Scopes  (histogram, clipping)  ────────────────────────────┘                    lens panel, WB tools)
      │
      ├─► U7  Ergonomics  (history, variants, presets, shortcuts, GPU toggle)  — needs U3, U4
      └─► U8  States & polish  (async load, toasts, error modals, progress, about)  — needs U0 shell
```

**Recommended sequencing**
1. **L0** — small, independent, unblocks lens correction immediately. *(commit 1)*
2. **U0** — foundation + the `gui.rs` module split everything lands on. *(commit 2)*
3. **U1** — viewport; do the **orientation correctness fix first** inside it, since all canvas coordinates depend on the displayed orientation. *(commit 3)*
4. **U2** — on-canvas manipulation; needs U1's `ViewTransform`. *(commit 4)*
5. Parallel after U0: **U3** (panels/widgets), **U4** (file/session/config), **U5** (scopes). *(commits 5–7)*
6. **U6** — feature parity; consumes U3 widgets, U2 canvas handles, and L0 + U4 (the lens toggle/pref). *(commit 8)*
7. **U7**, **U8** — ergonomics + polish. *(commits 9–10)*

---

## Epic summaries & card lists

> Full cards (heads-ups + acceptance) are in each epic file.

### [L0 — Lens database matching (brand-agnostic)](L0-lens-matching.md) *(independent; do first)*
The "NIKON CORPORATION" ≠ "Nikon" miss, fixed the way lensfun and LibRaw intend.
- **L0-C1** Fuzzy camera resolution — swap `lf_db_find_cameras` (exact `_lf_strcmp`) for `lf_db_find_cameras_ext` (fuzzy `lfFuzzyStrCmp`), best match, model-only fallback
- **L0-C2** Use LibRaw `normalized_make`/`normalized_model` for lookup (raw EXIF kept for display)

### [U0 — Visual foundation & app shell](U0-foundation.md) *(foundational)*
- **U0-C1** Modularize `latent-app` GUI into a module tree (behavior-preserving)
- **U0-C2** Theme & design tokens (tuned dark `Visuals`, neutral-gray photo surfaces, spacing/rounding/accent)
- **U0-C3** Typography (embed UI + mono fonts, register `TextStyles`)
- **U0-C4** Icon set (embed icon font + `icon()` helper)
- **U0-C5** App shell: menu bar + toolbar + status bar
- **U0-C6** Window & high-DPI defaults (size/min/ppp, app icon/title)

### [U1 — Viewport: orientation, zoom, pan, before/after](U1-viewport.md) *(dep: U0)*
- **U1-C1** EXIF auto-orientation (read LibRaw `sizes.flip`, apply in develop) — *the portrait bug*
- **U1-C2** Fit-to-window canvas + neutral surround + the `ViewTransform` (shared screen↔image map)
- **U1-C3** Zoom & pan (fit/100%/levels, wheel-zoom at cursor, drag-pan, controls + shortcuts)
- **U1-C4** Before/after (toggle + split), rendering a settings-off preview
- **U1-C5** Pixel readout / loupe (hover RGB + magnifier)

### [U2 — Direct on-canvas manipulation](U2-canvas-tools.md) *(dep: U1)*
- **U2-C1** Canvas interaction framework (overlay handles/guides, hit-test, drag state, active-tool)
- **U2-C2** Crop tool (rect + handles, aspect presets + lock, thirds overlay) — replaces 4 sliders
- **U2-C3** Straighten-by-horizon + manual rotate 90°/flip (new `Geometry` orientation field)
- **U2-C4** Keystone corner handles — replaces 2 sliders
- **U2-C5** Gradient & radial mask handles on the image
- **U2-C6** Mask-overlay visualization (colored overlay / mask-only)
- **U2-C7** Brush UX (cursor ring, size shortcuts, coverage overlay)

### [U3 — Panel & control redesign](U3-panels.md) *(dep: U0)*
- **U3-C1** Collapsible organized sections (remembered open/closed, scroll)
- **U3-C2** Reusable adjust-slider (double-click reset, numeric entry, fine drag, neutral marker; preserves begin/commit history)
- **U3-C3** Resizable + hideable side panel (width persisted, toggle)
- **U3-C4** Tooltips + in-app shortcuts cheat-sheet
- **U3-C5** Per-section & global reset; "modified" indicators

### [U4 — File & session UX](U4-file-session.md) *(dep: U0; single-image scope)*
- **U4-C1** Native Open (rfd) + in-app file switching (re-develop into the running app)
- **U4-C2** Drag-and-drop open; accept a bare path arg
- **U4-C3** Recent files (persisted)
- **U4-C4** Export dialog (rfd save, format/depth/quality, last dir, progress + toast)
- **U4-C5** Empty/welcome state (no file open)
- **U4-C6** App config persistence (window, recent, panel widths, export dir, GPU pref, theme)

### [U5 — Histogram & scopes](U5-scopes.md) *(dep: U0; clipping overlay dep: U1)*
- **U5-C1** Live histogram (RGB + luma, hand-drawn, from the preview)
- **U5-C2** Clipping indicators (highlight/shadow overlay toggles; histogram end-caps)
- **U5-C3** *(stretch)* Waveform/parade scope

### [U6 — Surfacing the full feature set](U6-feature-parity.md) *(dep: U0, U3; also L0, U2, U4)*
- **U6-C1** HSL/LCh 8-band mixer UI (`Adjustments.hsl` — currently unreachable)
- **U6-C2** Channel-mixer UI (`Adjustments.channel_mixer` — currently unreachable)
- **U6-C3** Full local adjustments (the same control surface as global, not just exposure+saturation)
- **U6-C4** Multi-shape masks + ops (shape list, Add/Subtract/Intersect, invert; canvas handles follow selection)
- **U6-C5** Lens-correction panel — **OFF by default + toggle** (remove the auto-apply; detect-on-enable via L0)
- **U6-C6** White-balance tools (Kelvin + tint, gray eyedropper via canvas, presets)

### [U7 — Editing ergonomics & workflow](U7-ergonomics.md) *(dep: U3, U4)*
- **U7-C1** History panel (clickable undo stack)
- **U7-C2** Variant management (name/thumbnail/duplicate/delete/reorder)
- **U7-C3** Copy/paste develop settings + reset-all
- **U7-C4** Develop presets (save/apply named `Settings`)
- **U7-C5** Keyboard shortcuts (complete set + cheat-sheet wiring)
- **U7-C6** In-app GPU/CPU backend toggle + status (live rebuild, pref persisted)

### [U8 — States, feedback & polish](U8-states-polish.md) *(dep: U0)*
- **U8-C1** Async loading states (develop off-thread; spinner/skeleton)
- **U8-C2** Non-blocking toasts (success/error queue)
- **U8-C3** Error modals (bad RAW; retry; no process exit)
- **U8-C4** Export/render progress indication
- **U8-C5** About dialog + first-run hint

---

## Coverage check
The drastic-UI goal decomposes into: a themed, modular shell (**U0**); a real **viewport** with orientation, zoom/pan and before/after (**U1**); **direct on-canvas** crop/straighten/keystone/mask manipulation replacing slider-only spatial editing (**U2**); organized, resettable, tooltip'd **panels** (**U3**); native **file/session** UX with dialogs, recents and an export dialog (**U4**); live **scopes** (**U5**); and **feature parity** that surfaces the HSL mixer, channel mixer, full local adjustments, multi-shape masks, the opt-in lens panel and WB tools the data model already supports but the UI hides (**U6**) — finished with **ergonomics** (**U7**) and **states/polish** (**U8**). The separate **L0** epic fixes lens-DB matching brand-agnostically. The two reported bugs map to **L0** (lens maker mismatch) and **U1-C1** (portrait orientation).
