# Manual testing procedure — UI/UX overhaul + lens fix

A follow-along checklist to verify everything built this iteration. Work top to
bottom; each step has an **action** and the **expected** result. Mark ✅/❌.

The two reported bugs are **§2 (lens)** and **§3 (orientation)** — test those first.

---

## 0. Prerequisites & test assets

- **Build & automated baseline** (must pass before any manual testing):
  ```
  cargo fmt --check
  cargo clippy --workspace --all-targets      # zero warnings
  cargo test --workspace -- --test-threads=1  # 428 passing
  cargo build --release
  ```
  (`--test-threads=1` is required — the GPU tests share one device.)
- **lensfun database installed** (system package) — needed for §2 and the lens panel.
- **Test RAWs** (gather a few):
  - **A** — any normal landscape-orientation RAW.
  - **B** — a **portrait-orientation** RAW (camera rotated 90°) → for §3.
  - **C** — a RAW shot on a body+lens that **exists in lensfun**, ideally one whose
    EXIF maker has a suffix (e.g. a **Nikon** body, EXIF `NIKON CORPORATION`) → for §2.
- The binary is `latent` (`./target/release/latent`).
- **Reset between runs:** edits autosave to a `<raw>.ron` sidecar next to the file.
  To test "fresh document" behavior, delete that `.ron` first.

**Launch modes to know:**
| Command | Expecting |
|---|---|
| `latent` | Welcome screen (no file) |
| `latent path/to/A.raw` | Opens A directly (bare path) |
| `latent open A.raw [--gpu]` | Opens A (optionally on GPU) |
| `latent develop A.raw out.tiff [--depth 8\|16]` | Headless develop to file |

> Tip: keep **Help ▸ Keyboard shortcuts** (`?`) open in a corner — it's the
> authoritative list of current key bindings; use it wherever a step says "the … key".

---

## 1. Smoke test & welcome state (U4, U8)

1. **Action:** run `latent` with no arguments.
   **Expected:** the window opens immediately to a **welcome screen** — large *Open*
   button, (empty) recent list, a drag-and-drop hint, and the version. No freeze.
2. **Action:** click *Open*, choose RAW **A**.
   **Expected:** a **spinner / "Developing …"** appears briefly, then the editor shows
   the image. The window was responsive the whole time (never a frozen black window).

---

## 2. 🐞 Lens database matching — brand-agnostic (L0)

> The fix: camera lookup now uses lensfun's fuzzy matcher + LibRaw's normalized
> names, so `NIKON CORPORATION` (EXIF) matches `Nikon` (DB).

1. **Action:** open RAW **C** (delete its `.ron` first). In the controls panel open
   **Geometry ▸ Lens Corrections** and tick **Enable**.
   **Expected:** it reports the **detected lens name** (not "No matching lens profile").
   Distortion/vignetting correction visibly applies. *Before this fix this lens failed to match.*
2. **Action:** open a RAW whose lens is genuinely **not** in lensfun.
   **Expected:** enabling reports **"No matching lens profile found"** — gracefully, no crash.
3. **Negative/robustness:** with **no lensfun database installed**, enabling lens
   correction reports no profile rather than crashing (the feature degrades).
4. *Note:* there is no automated test for the live DB query (external data), so this
   section is the only proof of the maker-mismatch fix.

---

## 3. 🐞 EXIF orientation — portrait upright (U1-C1)

1. **GUI:** open the **portrait** RAW **B**.
   **Expected:** it displays **upright** (taller than wide), matching what a normal
   image viewer shows — *not* rotated 90° into landscape.
2. **Export round-trip (CLI):** `latent develop B.raw b_out.tiff` then open `b_out.tiff`
   in any viewer.
   **Expected:** the exported file is upright too (orientation is baked into develop).
3. **Manual rotate on top (U2-C3):** with B open, use the toolbar **Rotate 90° CW/CCW**
   and **Flip H/V**.
   **Expected:** the image rotates/flips in 90° steps and mirrors; these compose on top
   of the automatic orientation; undo (Ctrl+Z) reverts each as one step.
4. **Regression:** open landscape RAW **A** → still correct (unchanged).

---

## 4. Theme, shell & window (U0)

1. **Expected at a glance:** dark, neutral-gray theme; a **menu bar** (File/Edit/View/Help),
   a **toolbar**, and a **status bar**.
2. **Canvas surround:** the area around the image is a **neutral gray** (no color tint).
3. **Status bar** shows: zoom %, image **dimensions**, the active **backend** (CPU/GPU),
   and a render/save state (Rendering… / Saved / Editing).
4. **Window:** sensible default size; try to shrink it — it stops at a **minimum size**;
   the title bar and taskbar show the filename and an app icon.

---

## 5. Viewport — zoom, pan, before/after, readout (U1)

1. **Fit:** the image **fits the window** and stays centered; resize the window → it refits.
2. **Zoom:** scroll-wheel over the image **zooms toward the cursor**; toolbar **Fit / 100% / − / +**
   and the keys (`0` fit, `1` 100%, `+`/`−`) work; status bar shows the live %.
3. **Pan:** when zoomed in, **middle-drag** (or space+drag) pans; at Fit, panning is inert.
4. **Before/after:** the before/after control cycles **Off → Before → Split**; *Before* shows
   the unedited develop, *Split* shows original|edited side by side, aligned.
5. **Pixel readout:** hover the image → the status bar shows the pixel `x,y` and its **sRGB RGB**;
   move off the image → it clears.

---

## 6. On-canvas editing tools (U2)

> All of these are *also* still editable via the numeric sliders in the panel — verify the
> handles and the numbers stay in sync.

1. **Crop:** select the crop tool → a **rectangle with 8 handles** appears; drag handles/interior;
   outside is **dimmed**; a **rule-of-thirds** grid shows. Switch **aspect presets**
   (Free/Original/1:1/3:2/4:3/16:9) and toggle **lock** → dragging keeps the ratio.
2. **Straighten:** drag the **horizon line** → the image levels; the Angle slider mirrors it.
3. **Keystone:** drag the **four corner handles** → converging verticals/horizontals correct;
   matches the Vertical/Horizontal sliders.
4. **Mask handles:** add a **Graduated** local (see §10) → drag its **line/endpoints** on the
   image; add a **Radial** → drag **center / radius ring / feather ring**.
5. **Mask overlay:** toggle the mask overlay → the selected mask shows as a **red wash**
   (and a mask-only mode); it reflects luminosity/color masks correctly (driven by image content).
6. **Brush:** add a **Brush** local → a **cursor ring** shows size/feather; `[` / `]` change size,
   Shift+`[`/`]` change feather; paint a stroke → coverage shows; one stroke = **one undo step**.

---

## 7. Panels & controls (U3)

1. **Collapsible sections:** the controls are grouped (Basic/Tone/Color/Curves/Detail/Effects/
   Geometry/Masks) and **collapse/expand**; close some, **restart the app** → state **persisted**.
2. **Slider widget:** **double-click** any slider → resets to default; type into the **numeric box**
   → commits as one undo step; a **modified indicator** appears when a value is non-default.
3. **Resize/hide panel:** **drag the panel's left edge** to resize (persists across restart);
   press **Tab** → the panel hides for a full-screen canvas; Tab again restores it.
4. **Tooltips:** hover any control → a concise tooltip (what it does + range).
5. **Cheat sheet:** press **`?`** → a modal lists current shortcuts; Esc closes.
6. **Per-section reset:** a section's reset button returns just that section to defaults as **one undo step**.
7. **Focus guard:** while typing in a text/number field, bare-letter shortcuts (Tab, `?`, C, B…)
   **do not fire**.

---

## 8. File & session (U4)

1. **Open dialog:** File ▸ Open (or **Ctrl+O**) → native picker; opening **switches** the image
   without relaunching (history/view reset).
2. **Drag-and-drop:** drag a RAW onto the window → it opens; a banner shows while hovering.
3. **Recent files:** File ▸ Open Recent lists prior files (most-recent first, deduped); the welcome
   screen lists them too; removing a file from disk prunes it.
4. **Export dialog:** File ▸ Export → native save picker with **format (JPEG/PNG/TIFF)**, **bit depth**
   (8 for JPEG; 8/16 for PNG/TIFF), and a **JPEG quality** slider (only for JPEG). It remembers the
   **last export directory**. Export runs in the background.
   - **Invalid combo:** force 16-bit JPEG → a graceful **error toast**, no crash, no file written.
5. **Config persistence:** change theme-affecting state (panel width, recents, last export dir,
   GPU pref, window size), restart → all **restored**. Corrupt/delete the config file → app starts
   with **defaults**, no crash.

---

## 9. Scopes (U5)

1. **Histogram:** an **RGB + luma histogram** shows at the top of the panel and updates after each edit
   (e.g. raise exposure → mass shifts right).
2. **Clipping:** toggle highlight/shadow clipping (panel checkboxes or histogram **end-caps**) →
   blown/crushed pixels are flagged on the image in warning colors; toggling does **not** re-render.
3. **Waveform/parade:** switch the scope type to **Waveform** then **Parade** → each renders from the preview.

---

## 10. Feature parity — previously hidden tools (U6)

1. **HSL mixer:** Color section → enable **HSL**; adjust a band's hue/sat/lum (e.g. push **blue** luminance) →
   visible, live, undoable; swatches indicate each band.
2. **Channel mixer:** enable **Channel Mixer**; change an output row's input weights → image responds;
   **Preserve luminosity** toggle works.
3. **Full local adjustments:** add a local (§6) → it now exposes the **full** control set (tone, curves,
   clarity, sharpen, dehaze, NR, HSL…), not just exposure+saturation; effects stay **within the mask**.
4. **Multi-shape masks:** on one local, **add several shapes** and set each shape's op
   (**Add / Subtract / Intersect**) and **invert**; the overlay shows the combined region; the selected
   shape drives its on-image handles.
5. **Lens panel default (important):** open a **fresh** document (delete `.ron`) → lens correction is
   **OFF** by default (nothing applied). Enable it (§2). Then: edit with it enabled, let it autosave,
   **reopen** → the saved lens is **still applied** (saved intent respected; only the fresh default is off).
6. **White balance:** the **Temperature (Kelvin)** + **Tint** sliders warm/cool the image; the **gray
   eyedropper** — click a neutral/gray patch → the image neutralizes there; **presets** (Daylight, Shade,
   Tungsten, …) set sensible values; **As Shot** clears the override.

---

## 11. Ergonomics & workflow (U7)

1. **History panel:** make several edits → the panel lists steps; **click an earlier step** → the image
   jumps to it; Ctrl+Z/Ctrl+Y still work.
2. **Variants:** add a variant (+), **rename** it, **duplicate**, **reorder**, **delete**; **thumbnails**
   reflect each variant; names **persist** in the sidecar; reopen → names restored.
3. **Copy/paste settings:** Edit ▸ Copy settings on one variant, **Paste** onto another → develop settings
   transfer (geometry/crop stays the target's) as one undo step; **Reset all develop** returns to neutral.
4. **Presets:** save the current look as a **named preset**; apply it to another image → adjustments apply
   (geometry excluded); presets **persist** across runs.
5. **Shortcuts:** spot-check the cheat-sheet bindings (open/export/undo/zoom/before-after/variant `,`/`.`/
   tools C,B). They don't fire while typing.
6. **GPU toggle:** View ▸ Use GPU → the status bar shows **GPU** and the image re-renders identically;
   toggle back to CPU. If no GPU is available, it **falls back to CPU with a toast**. The preference
   persists across restart. (Switching mid-render waits for the current render to finish.)

---

## 12. States, feedback & polish (U8)

1. **Async load:** open a large RAW → **spinner + "Developing …"**, window responsive; then editor.
2. **Toasts:** trigger an export (success toast), a failed save (e.g. read-only dir → error toast),
   a GPU fallback → transient **toasts** stack in a corner and auto-dismiss; the status bar keeps steady state.
3. **Error modal:** open a **corrupt/non-RAW** file (e.g. a text file renamed `.nef`) → a friendly **modal**
   with the error and **Retry / Open another / Dismiss**; the **app stays alive** (does *not* exit).
   - Contrast (CLI): `latent develop bogus.nef out.tiff` → **exits non-zero** with an error (CLI behavior unchanged).
4. **Export progress:** during a full-res export the status bar shows an **indeterminate spinner +
   "Exporting…"** and the Export action is disabled.
5. **About:** Help ▸ About → app version, **lensfun version**, active backend.
6. **First-run hint:** on a clean config, a dismissable hint points at Open/drag-drop; dismiss → it
   doesn't return.

---

## 13. Regression — output integrity (don't-break checks)

1. **Preview == export:** the on-screen preview and the exported file match tonally (same output transform).
   Develop the same edit to a file and compare.
2. **CPU vs GPU:** the same edit renders identically on CPU and GPU (toggle in §11.6) — this is also
   covered by the automated equivalence tests.
3. **Sidecar round-trip:** edit, close, reopen → all edits restored from `<raw>.ron`.
4. **Forward-compat:** an **old sidecar** (from before this iteration, without the new orientation/variant-name
   fields) still **loads** without error.
5. **Re-run the full automated suite** at the end: `cargo test --workspace -- --test-threads=1` → 428 pass.

---

### Quick report template
```
Env: OS ___  GPU ___  lensfun ___  RAWs used: A__ B(portrait)__ C(lens)__
Automated: fmt[ ] clippy[ ] test 428[ ]
Bugs:   §2 lens[ ]  §3 orientation[ ]
UI:     §4[ ] §5[ ] §6[ ] §7[ ] §8[ ] §9[ ] §10[ ] §11[ ] §12[ ] §13[ ]
Notes/failures:
```
