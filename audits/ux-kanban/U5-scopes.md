# U5 — Histogram & scopes

> Scopes that tell the truth about the *output*. The histogram (and the clipping
> overlay) are computed over **display-referred** values — the exact sRGB8 bytes
> [`latent_export::to_srgb8`](../../latent-export/src/lib.rs) produces and the
> preview texture is uploaded from ([`gui.rs:1232-1234`](../../latent-app/src/gui.rs),
> `to_color_image`) — so what the histogram shows is what the saved file will be,
> and a pixel the histogram calls "clipped" is a pixel that clips in the export.
> Everything is **hand-drawn** with `egui::Painter` (no `egui_plot`, no new crate —
> [`README.md`](README.md) Product decision 2), reusing the exact
> `allocate_painter` + `line_segment`/`circle_filled`/`rect_filled` approach the
> curve editor already uses ([`gui.rs:672-721`](../../latent-app/src/gui.rs),
> `curves_block`). The bins are **cached and recomputed only when a new preview
> lands** — the histogram is *not* a per-frame computation; it is refreshed from
> the freshly-rendered preview in the same place the texture is uploaded
> (`poll_render` → `load_texture`, [`gui.rs:307-342`](../../latent-app/src/gui.rs)),
> off the per-frame paint path.
>
> **Shared surface:** the preview is a single `ImageBuf` the worker hands back
> (`RenderOutput::Preview`, [`gui.rs:143-145`](../../latent-app/src/gui.rs)) and
> the main thread turns into display bytes via `to_srgb8`
> ([`gui.rs:1233`](../../latent-app/src/gui.rs)) before uploading. **C1** computes
> the bins from those same display bytes once per preview and paints the
> histogram; **C2** reuses the same display-referred threshold to mark clipped
> pixels on the canvas through **U1**'s `ViewTransform` (the one screen↔image map,
> [`README.md`](README.md) Global rule 3), and wires the histogram end-caps as the
> toggles; **C3** (stretch) adds a column-wise waveform/parade from the same
> cached source, behind a scope-type selector. Take **C1** first — it owns the
> binning function, the cache, and the recompute hook the other two read.
>
> **Why display-referred, not working-linear (the load-bearing decision):** the
> working pixels are linear-light, wide-gamut ProPhoto-ish primaries
> ([`color.rs:264-277`](../../latent-image/src/color.rs)), and a value of `1.0`
> there is **not** the clip point — the output transform applies a working→sRGB
> matrix, a hue-preserving highlight rolloff (knee at `0.98`), and the sRGB OETF,
> then clamps to `[0,1]` ([`latent-export/src/lib.rs:46-101`](../../latent-export/src/lib.rs),
> `highlight_rolloff` / `to_display` / `to_srgb8`). A histogram of the *working*
> data would mis-place every bin and call the wrong pixels clipped. The histogram
> must be over the **post-transform display bytes** so its bins and its clip ends
> line up with the file and the canvas overlay. (This mirrors `to_color_image`'s
> own contract, [`gui.rs:1229-1235`](../../latent-app/src/gui.rs): the preview
> *is* the export transform.)
>
> **Luma definition (pinned, and deliberately not the working luma):** luma is
> computed on the **display** sRGB values with Rec. 709 weights
> `0.2126·R + 0.7152·G + 0.0722·B` — the same weights
> [`latent_edit::select_luma`](../../latent-edit/src/lib.rs) (`:139-144`) uses for
> "how bright is this pixel" on non-working-space data. It is **not**
> [`latent_image::color::luminance`](../../latent-image/src/color.rs) /
> `LUMA_WEIGHTS` (`:264-282`): those are *colorimetric* weights for **linear
> working** space with a near-zero blue term (`8.56539e-5`), correct for exposure
> math but wrong for a display-byte histogram (pure display-blue would read as
> near-black). The scope answers "how bright does this output pixel look", which
> is Rec. 709 on the display bytes — pin this in the binning function's doc and a
> test.

---

### U5-C1 — Live histogram
- Implements: scopes goal ([`README.md`](README.md) §U5)            Priority: High
- Crates/files: `latent-app/src/gui/scopes.rs` (new — the binning function + the painter draw), wired from the right panel ([`gui.rs:407-493`](../../latent-app/src/gui.rs)) and the preview hook ([`gui.rs:307-342`](../../latent-app/src/gui.rs)); lands on **U0-C1**'s module tree (`gui/` becomes a directory).
- Depends on: **U0-C1** (the `gui.rs` module split — `scopes.rs` is a sibling module of `panels`/`canvas`)            Blocks: **U5-C2**, **U5-C3**
- Heads-up:
  - **Compute from the display bytes, not the working pixels (the whole point — see the epic intro).** The data the user sees is `latent_export::to_srgb8(&preview)` ([`gui.rs:1233`](../../latent-app/src/gui.rs)), the same `Vec<u8>` row-major `RGBRGB…` that becomes the texture. The cleanest, most testable seam is a **pure** `histogram_bins(rgb8: &[u8]) -> Histogram` over that byte slice — no egui, no `ImageBuf`, no color math beyond the Rec. 709 luma sum, so it unit-tests without a window or a GPU (the project's pure-function testing discipline, like `export_status`/`clamp_selection`, [`gui.rs:206-218`](../../latent-app/src/gui.rs)). `Histogram` is four `[u32; 256]` arrays: `r`, `g`, `b`, `luma`.
  - **Binning is a byte→bin identity for the channels** (256 display codes → 256 bins, one-to-one — bin `= byte`), so R/G/B binning is just `bins[c][byte] += 1` over each triplet; no float math, no scaling, exact. **Luma** is `(0.2126·r + 0.7152·g + 0.0722·b)` on the **display bytes** (the Rec. 709 weights from the epic intro / `select_luma`), rounded to the nearest integer and clamped to `0..=255` for the bin index — document that this is luma of the *display* value, not a re-linearized luminance, and that the weights match `latent_edit::select_luma` (cite it) deliberately rather than `color::LUMA_WEIGHTS` (which is for linear working space). Guard the slice: iterate `rgb8.chunks_exact(3)` so a stray trailing byte can't panic or mis-align channels (the buffer is always `len()*3` from `to_srgb8`, but `chunks_exact` makes the function total for *any* input the test throws at it).
  - **Cache, recompute only on a new preview (Global: not per frame).** Add a `scopes: Option<Histogram>` (or a small `Scopes` struct holding the cached bins + the selected scope type for C3) field to `App` ([`gui.rs:104-136`](../../latent-app/src/gui.rs)). The recompute hook is **`load_texture`** ([`gui.rs:333-342`](../../latent-app/src/gui.rs)) — it already receives the freshly-rendered `&ImageBuf` and calls `to_srgb8` via `to_color_image`. Compute the histogram there from the **same** display bytes (compute the `to_srgb8` `Vec<u8>` once, build the `ColorImage` *and* the bins from it, so the transform runs once per preview, not twice). That ties the cache lifetime to exactly "a new preview arrived": every render path that updates the texture (first frame, every edit, every coalesced re-render — `poll_render` → `RenderOutput::Preview` → `load_texture`, [`gui.rs:312-342`](../../latent-app/src/gui.rs)) refreshes the bins, and nothing else does. The per-frame `update` only *draws* the cached bins — it never recomputes.
  - **Draw with the painter, like `curves_block`.** Allocate a fixed-height panel rect (`ui.allocate_painter(size, Sense::hover())`, mirroring [`gui.rs:672-673`](../../latent-app/src/gui.rs)) and paint the bins as filled bars / a polyline per channel: a dark surface `rect_filled` background, then R/G/B as additive translucent fills (`Color32::from_rgba_unmultiplied`, so overlapping channels read as their mix the way photo histograms do) and luma as a light outline — reusing `line_segment` for the polyline exactly as the curve does ([`gui.rs:704-718`](../../latent-app/src/gui.rs)). Normalize bar heights to the **max bin count across the drawn channels** (drop or soft-clip bin 0/255 spikes only for the *height* scale if you want, but document it) so the shape is visible; a `√count` or `log` vertical scale reads better for photographic data — pick one and note it in a comment. No interaction is required for C1 (the end-cap *click targets* are C2); `Sense::hover()` is enough.
  - **Placement:** put the scope at the **top of the right control panel** (above "Light", [`gui.rs:407-417`](../../latent-app/src/gui.rs)) or in its own collapsible section — coordinate with **U3-C1**'s collapsible sections if that has landed, but C1 does **not** depend on U3: a plain `ui.group`/heading at the top of the existing `SidePanel::right` is sufficient and keeps the dependency to U0 only. Keep it a function `histogram_block(ui, &Histogram)` in `scopes.rs`, called from the panel, matching the `*_block(ui, …)` convention of every other panel section ([`gui.rs:418-454`](../../latent-app/src/gui.rs)).
  - **Render output is sacred (Global rule 2).** This card reads `to_srgb8` output and never changes it — no pipeline/export math is touched. The `to_color_image_matches_the_export_transform` test ([`gui.rs:1241-1257`](../../latent-app/src/gui.rs)) and the CPU/GPU equivalence tests stay byte-identical; the histogram is a pure consumer of the existing transform.
  - **Empty/degenerate input:** before the first preview lands the texture is `None` and the panel shows "Rendering…" ([`gui.rs:513-519`](../../latent-app/src/gui.rs)); guard the scope draw on `self.scopes.is_some()` (or draw an empty frame) so there's no panic with no bins. A zero-pixel image yields all-zero bins (no div-by-zero in the height normalization — guard the `max == 0` case to a flat baseline).
- Acceptance:
  - The histogram updates after **each** render: editing a slider re-renders, `load_texture` recomputes the bins from the new preview's display bytes, and the panel redraws the new shape; the bins are computed **once per preview** (in `load_texture`, sharing the single `to_srgb8` call), **not** per frame — a comment/structure makes the once-per-preview cache explicit, and the per-frame `update` only paints the cached `Histogram`.
  - The binning is a pure, window-free function with a **unit test** `histogram_bins_counts_channels_and_luma`: a hand-built `&[u8]` (e.g. one black `[0,0,0]`, one white `[255,255,255]`, one mid `[128,128,128]`, one saturated `[255,0,0]`) asserts the R/G/B bins land in the exact code slots (bin `= byte`), the luma bin for `[255,0,0]` is `round(0.2126·255) = 54` (pin the Rec. 709 weight choice — it would differ under `color::LUMA_WEIGHTS`), white→bin 255, black→bin 0, and the bin totals equal the pixel count. A second test `histogram_bins_handles_ragged_input` feeds a slice whose length isn't a multiple of 3 and asserts no panic (the `chunks_exact` guard) and that complete triplets are still counted.
  - The scope is painter-drawn (no `egui_plot`, no new crate — Product decision 2): R/G/B + luma visible, drawn with `allocate_painter` + `rect_filled`/`line_segment` like `curves_block`; placed at the top of the right panel (or a collapsible scope area) and depending only on **U0-C1**.
  - `cargo fmt --check`, `cargo clippy --workspace --all-targets` (zero warnings), `cargo test --workspace` stay green (Global rule 1); the export transform is untouched (Global rule 2).

---

### U5-C2 — Clipping indicators
- Implements: scopes goal ([`README.md`](README.md) §U5) — clipping overlay            Priority: Medium
- Crates/files: `latent-app/src/gui/scopes.rs` (thresholds + the end-cap toggle hit-targets on the histogram), `latent-app/src/gui/canvas.rs` (the overlay draw over the image, consuming **U1**'s `ViewTransform`).
- Depends on: **U5-C1** (the cached display-referred histogram + its panel), **U1-C2** (the `ViewTransform` — the single screen↔image map)            Blocks: —
- Heads-up:
  - **Same display-referred values as the histogram — that is what makes the overlay honest.** Clipping is decided on the **display sRGB bytes** (the cached source from C1), not the working pixels: a *blown highlight* is a display value `≥` the high threshold, a *crushed shadow* is `≤` the low threshold. Pin the thresholds in code and document them: highlight `≥ 254` (display white rolls off to **254**, not 255 — the rolloff knee, [`latent-export/src/lib.rs:31-54`](../../latent-export/src/lib.rs) and the pinned `254` in [`gui.rs:1256`](../../latent-app/src/gui.rs)), shadow `≤ 1` (or `== 0`); state the exact bytes in a comment so the meaning is checkable and matches the histogram's end bins. Offer **per-channel "any"** (any channel clips → flagged, the common photo behavior) as the default; an **"all channels"** variant (only fully-blown/fully-crushed) is a documented option — note which the toggle uses.
  - **Two independent toggles**, surfaced **both** as panel checkboxes and as **clickable end-caps on the histogram** (C1's left/right edge): clicking the left end-cap toggles shadow clipping, the right end-cap toggles highlight clipping (the Lightroom triangle affordance). For the end-caps, give C1's histogram rect a small interactive sub-rect at each end (`ui.interact(end_rect, id, Sense::click())`) — this is the only interaction the scope panel needs; keep it in `scopes.rs`. Store the two bools on `App` (or the `Scopes` struct from C1), `#[serde(default)]`-free since they are transient UI state, not persisted settings (they live on `App`, not `Settings` — no sidecar/forward-compat concern, Global rule 4 applies only to the data model).
  - **Overlay rides on U1's `ViewTransform`, never its own math (Global rule 3).** The canvas already paints the preview texture in the central panel ([`gui.rs:523-528`](../../latent-app/src/gui.rs)); after **U1-C2** that draw goes through the shared `ViewTransform` (fit/zoom/pan → screen rect). The clipping overlay must paint in the **same** mapped image rect, so a clipped *image* pixel lands on the right *screen* pixel at any zoom/pan — consume the `ViewTransform` U1 owns; do **not** recompute screen↔image. Two practical draw strategies, pick per cost: (a) build a small **mask overlay texture** (an RGBA `ColorImage` the size of the preview, transparent except warning colors where clipped) once per preview alongside the histogram in `load_texture`, and draw it stretched over the same image rect the `ViewTransform` gives — cheap to paint every frame, recomputed only on a new preview like the bins; or (b) paint per-clipped-pixel rects with the painter (only viable downscaled). Prefer (a): it reuses the once-per-preview cache discipline and the single image rect, and a stretched texture is exactly how the preview itself is drawn. Use distinct warning colors (e.g. red for blown highlights, blue for crushed shadows) and only build the overlay channels whose toggle is on.
  - **Recompute with the same cache key as the bins.** The clip mask is a function of the same display bytes; build it in `load_texture` next to the histogram (one `to_srgb8` pass feeds the texture, the bins, *and* the clip mask). Toggling a clip bool does **not** need a re-render or a recompute — it only flips whether the (already-built) overlay is drawn this frame; flag the overlay-on state so `update` paints it, no `render_preview` call. (Contrast brush dabs at [`gui.rs:571-574`](../../latent-app/src/gui.rs), which *do* re-render — clipping toggles must not, they are pure view state.)
  - **Threshold is on the same bytes the histogram bins, by construction**, so the overlay and the histogram end bins agree pixel-for-pixel — assert that in a test rather than eyeballing.
- Acceptance:
  - Enabling highlight clipping marks display-`≥254` pixels on the image in the warning color; enabling shadow clipping marks display-`≤1` (or `==0`) pixels — through **U1**'s `ViewTransform`, so the marks stay registered to the image under zoom and pan (no independent screen↔image math; Global rule 3). The thresholds (254 / 1, any-channel default) are **documented** in `scopes.rs`/`canvas.rs` comments with the rolloff/`254` citation.
  - The histogram end-caps are clickable and toggle the two overlays (left = shadows, right = highlights), mirrored by panel checkboxes; both are transient `App`-level state (no sidecar persistence, no re-render on toggle).
  - A **unit test** `clip_mask_matches_histogram_ends` (pure, on the C1 display-byte source): for a hand-built byte buffer, the set of pixels the clip predicate flags equals the population of the histogram's end bins (≥254 highlights ↔ the high bins, ≤1 shadows ↔ the low bins) — pinning that the overlay and the scope read the *same* display-referred values. Toggling a clip bool does not trigger a re-render (structure/flag asserts the view-only path).
  - `cargo fmt --check`, `cargo clippy --workspace --all-targets` (0 warnings), `cargo test --workspace` green; export/pipeline math untouched (Global rule 2).

---

### U5-C3 — (stretch) Waveform / RGB parade
- Implements: scopes goal ([`README.md`](README.md) §U5) — optional waveform            Priority: Low
- Crates/files: `latent-app/src/gui/scopes.rs` (an alternate scope draw behind a scope-type selector).
- Depends on: **U5-C1** (the cached display-referred source + the scope panel/selector)            Blocks: —
- Heads-up:
  - **Optional / stretch — implement only if C1 and C2 land with room to spare; otherwise this card is *documented as deferred* and that is acceptance-complete.** It adds no correctness, only an alternate readout.
  - **Same display-referred source, same painter, no new crate.** A waveform plots, **per image column** (x = column position across the preview width), the distribution of that column's display values up the y axis (brightness), intensity = how many pixels in that column hit that value — a 2-D `[width-buckets][256]` accumulation. An **RGB parade** is three side-by-side waveforms (R, G, B). Both are built from the **same** cached display bytes C1 already produces in `load_texture` (recompute on new preview only — same cache discipline; a waveform accumulator is heavier than a 1-D histogram, so it especially must not run per frame). Bucket columns to the panel width (the preview is ≤1600px wide, [`gui.rs:22`](../../latent-app/src/gui.rs); the scope panel is far narrower, so map source columns → panel columns) to keep the accumulator small.
  - **Behind a scope-type selector.** Add a small selector (`selectable_value` row, like the curve channel picker [`gui.rs:649-653`](../../latent-app/src/gui.rs), or the `["Master","R","G","B"]` row) — `Histogram` / `Waveform` / `Parade` — stored on the `Scopes`/`App` state from C1. The histogram (C1) stays the default. Draw the waveform with the painter: many short `line_segment`s or per-cell `rect_filled` with alpha = normalized count; a `√`/`log` intensity scale reads better (note the choice). Keep it a pure `waveform_buckets(rgb8, width_buckets) -> …` + a draw fn, so the accumulation is unit-testable exactly like `histogram_bins`.
  - **Stays painter-only and display-referred** — no `egui_plot`, no new crate (Product decision 2); the column buckets are over display sRGB bytes so the waveform agrees with the histogram and the file, same as C1/C2.
- Acceptance:
  - **If implemented:** a scope-type selector switches the panel between the C1 histogram and a waveform (and optionally an RGB parade); the waveform renders column-wise from the same cached display bytes (recomputed only on a new preview, not per frame), painter-drawn, no new crate; a **unit test** on the pure `waveform_buckets` accumulation (pixels → column buckets) covers the binning. Baseline green (fmt / clippy-0 / test).
  - **If deferred (acceptable):** this card explicitly documents the waveform/parade as a deferred stretch in the file (and the commit notes it as not-done) — C1's histogram + C2's clipping are the shipped scopes. No half-built scope is left behind; the selector, if stubbed, defaults to and only offers the histogram.

---

## Epic done when

`latent` shows scopes that match the **output**, all hand-drawn with `egui::Painter`
(no `egui_plot`, no new crate — Product decision 2) and all over **display-referred**
values so they line up with the saved file and each other. An RGB + luma histogram
is computed from the preview's display sRGB8 bytes — the exact
`latent_export::to_srgb8` output the texture is uploaded from — by a pure,
unit-tested `histogram_bins` (256-bin byte→bin identity per channel; luma via the
Rec. 709 weights `0.2126/0.7152/0.0722` on the display bytes, matching
`latent_edit::select_luma`, **not** the linear-working `color::LUMA_WEIGHTS`),
**cached and recomputed only when a new preview lands** in `load_texture`
(sharing the single `to_srgb8` pass with the texture upload — never per frame),
and painted at the top of the right panel the way `curves_block` paints the curve
(**C1**). Clipping toggles mark blown highlights (display `≥254` — the rolloff
white code) and crushed shadows (display `≤1`/`0`) on the canvas in warning colors,
through **U1**'s single `ViewTransform` (Global rule 3, no independent screen↔image
math) and built from the same cached display bytes, toggled by panel checkboxes and
clickable histogram end-caps, as pure view state that does **not** re-render — with
the overlay population pinned equal to the histogram's end bins by test (**C2**).
An optional column-wise waveform/RGB-parade behind a scope-type selector, from the
same cached source, is either implemented (with its own pure-accumulation test) or
**clearly documented as deferred** (**C3**). Throughout, the render/export math is
untouched (Global rule 2 — scopes are pure consumers of the existing transform) and
the baseline stays green (fmt / clippy-0-warnings / `cargo test --workspace`,
Global rule 1).
