# Decision Register — Code Review 04 (Export, Lens FFI, App/UI)

**Source review:** [`../../code-review/04-export-lens-app.md`](../../code-review/04-export-lens-app.md)
**Crates:** `latent-export`, `latent-lens` (lensfun FFI + `build.rs`, `examples/lookup.rs`), `latent-app` (`main.rs` CLI + `gui.rs` egui editor)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Triaged autonomously, finding-by-finding in the review's severity order, optimizing for correctness / safety / robustness / UX. Source spot-checked at `latent-export/src/lib.rs`, `latent-app/src/main.rs`, `latent-app/src/gui.rs` where a decision hinged on it.

This register records, for every finding of the code review, whether the software will be changed or kept as-is, the rationale, and the concrete action implied. It does **not** itself modify any source. Where a finding overlaps an already-decided image-processing item it is marked **Covered by …** (still registered, not re-planned).

---

## Decision summary

| Finding | Severity | Decision | Outcome |
|---|---|---|---|
| **H1** GUI renders synchronously on UI thread (`gui.rs:156-183,335-337,390-392`) | High | **Move render/export to a worker thread** | Change |
| **H2** Unknown extension → silent untagged save (`export/lib.rs:154`) | High | **Reject unsupported extensions with a typed error** | Change |
| **M1** `save`/`save_16` don't validate dimensions (`export/lib.rs:86-106`) | Medium | **Early typed error on 0-dim image** | Change |
| **M2** `save` `.expect` reachable via overflow (`export/lib.rs:87-88`) | Medium | **Map to `ImageError`, honor `ImageResult`** | Change |
| **M3** Optical-center normalization mismatch (`lens/lib.rs:142`) | Medium | **Covered by IP-05 §2.3-L1** (scale by min(w,h)/2) — Initiative C | Change (elsewhere) |
| **M4** CA/vignetting coeffs not finiteness-guarded (`lens/lib.rs:212-231`) | Medium | **Add `is_finite` guards (match distortion)** | Change |
| **L1** JPEG quality not configurable (`export/lib.rs:148-153`) | Low | **Higher default + plumb a quality param** | Change |
| **L2** 16-bit path unreachable from CLI (`main.rs:73-77`) | Low | **Auto-select depth by format + `--depth` override** | Change |
| **L3** `self.texture.unwrap()` unenforced invariant (`gui.rs:339-340`) | Low | **Thread texture out of `render_preview` (no unwrap)** | Change |
| **L4** Free-text export path, no extension validation (`gui.rs:312-318`) | Low | **Validate extension in GUI (folds into H2)** | Change |
| **L5** Lensfun DB loaded synchronously every editor open (`gui.rs:81-94`) | Low | **Keep** (pre-window, acceptable; note for later) | No change |
| **N1** `examples/lookup.rs:20` `.expect` on DB load | Nit | **Fix** (friendly message; it's a demo) | Change (small) |
| **N2** `srgb_icc()` `.expect` (`export/lib.rs:111-115`) | Nit | **Keep** (pure, input-independent; verified safe) | No change |
| **N3** Duplicated `local_sel` clamp (`gui.rs:885,908`) | Nit | **Extract a clamp helper (DRY)** | Change (small) |
| **T-gaps** Export edge/ICC, lens lookup plumbing, CLI parse, error paths, GUI state, sidecar ranges (§6) | — | **Add the missing tests** | Change |
| **Positives** (§7) | — | **Keep** (preserve guarding tests) | No change |

**Tally:** 13 Fix (incl. test-coverage work and 2 small nits), 3 Keep (L5, N2, Positives), 1 Covered-elsewhere (M3 → IP-05 §2.3-L1). M3 is the only finding whose remedy lands in another register's plan.

---

## Per-finding decisions

### H1 — GUI renders synchronously on the UI thread · **Move render/export off-thread**
- **Decision:** Run the pipeline render off the egui update thread. At minimum `export` (full resolution, the worst case); ideally `render_preview` too. Keep a "rendering…" / "exporting…" status, and `ctx.request_repaint()` on completion.
- **Why:** `render(&self.full, …)` (`gui.rs:174-178`) and `render(&self.preview, …)` (`gui.rs:157-161`) execute the whole pipeline inline in `update` (`:335-337`, `:390-392`); while running, the window neither repaints nor processes input. On a large RAW the editor appears hung for the entire export. This is the single most user-visible engineering weakness. Spot-check confirmed both `export` and `render_preview` call `render(...)` synchronously and that `export` is full-resolution (`self.full`) while preview is bounded by `PREVIEW_MAX_DIM = 1600`.
- **Action:** `latent-app/src/gui.rs:173-183` (`export`) and `:156-170` (`render_preview`). Spawn a worker (`std::thread` + `std::sync::mpsc`, or `poll_promise`); send the result back as a texture-ready frame; gate re-entry so only one render is in flight; show progress/status. Take an output result through a channel rather than returning it inline.
- **Coordination:** When render moves off-thread, the worker must own/clone the data it needs. `Database` (the lensfun handle) is intentionally **not** `Send`/`Sync` (§3) — do **not** move it onto the render worker; lens-profile lookup stays on its current (pre-window) path. Add the one-line thread-safety doc note requested in §3 so this constraint is explicit for the threading work.
- **UX:** Add a cancellable/"busy" indicator; at least disable a second export while one is running.
- **Pairs with:** L3 (`render_preview` becomes fallible/async → texture handle should be returned, not unwrapped) and L5 (DB-load note).

### H2 — Unknown output extension silently writes an untagged file · **Reject with a typed error**
- **Decision:** Take option (a) from the review: reject unknown/unsupported extensions with a clear, actionable typed error rather than silently falling through to `buf.save(path)` with no ICC. Do **not** keep the silent fallback even with a warning — a color-managed photo developer whose stated contract is "an sRGB ICC profile is embedded" should not emit untagged files at all.
- **Why:** The `_ => buf.save(path)` arm (`export/lib.rs:154`, confirmed) writes with **no ICC profile** for any extension that isn't png/tif/tiff/jpg/jpeg — `.bmp`, `.webp`, or a typo like `.jpe`/no-extension. The color is then untagged and misinterpreted by color-managed viewers, with zero signal to the user. It is reachable directly from the CLI (`latent develop in.raw out.bmp`) and from the GUI export field. Silent color-management loss is the worst failure mode for this app.
- **Action:** Replace the `_` arm at `export/lib.rs:135-155` with a returned `ImageError::Unsupported` carrying an actionable message ("unsupported output extension `<ext>`; use png, tif/tiff, or jpg/jpeg"). Keep the empty/no-extension case in the same error.
- **Surfacing:** CLI already maps `Err` → `error: {e}` + exit 1 (`main.rs:87-90`, verified); GUI already maps `Err` → status line (`gui.rs:179-182`, verified). So a typed error surfaces correctly on both front-ends with no extra plumbing.
- **Tests:** Add a unit test asserting the unsupported-extension error (T-gap §6). Folds in **L4**.

### M1 — `save()`/`save_16()` don't validate dimensions · **Early typed error on degenerate images**
- **Decision:** Early-return a typed `ImageError` when `img.width() == 0 || img.height() == 0`, in both `save` and `save_16`.
- **Why:** Confirmed: `save` builds `RgbImage::from_raw(w, h, to_srgb8(img))` (`export/lib.rs:87-88`); for `w==0||h==0` the buffer length (0) matches 0 pixels, so `from_raw` returns `Some` and the `.expect` does **not** fire — a zero-dimension image is handed to the encoder, with format-dependent, unvalidated behavior (some encoders error, some write a degenerate file, the H2 untagged path also becomes reachable). `save_16` loops `0..0` and writes an empty buffer. Neither is a meaningful output.
- **Action:** Add a guard at the top of `save` (`:86`) and `save_16` (`:95`) returning `ImageError::Parameter`/`Unsupported` ("cannot encode an image with a zero dimension"). Add a 0×0 / 0×N test (T-gap §6).

### M2 — `save()`'s `.expect("buffer length matches …")` reachable via overflow · **Return an error**
- **Decision:** Replace the `.expect` at `export/lib.rs:88` with a mapped `ImageError` so the function honors its `image::ImageResult<()>` contract for all inputs, not just typical RAW sizes.
- **Why:** `to_srgb8` builds a `Vec<u8>` of length `w*h*3`; for pathological dimensions `from_raw`'s internal length check can return `None`, tripping the `expect` — a panic in a library function on a public API instead of a returned `Err`. Practically unreachable for real RAW (OOM would hit first), but the contract wrinkle is real and the fix is trivial.
- **Action:** `export/lib.rs:87-88` — `match RgbImage::from_raw(...) { Some(out) => …, None => Err(ImageError::Parameter(…)) }` (or `.ok_or_else(...)?`). The M1 zero-dimension guard already covers the common degenerate case; this covers the overflow case.
- **Note:** Distinct from **N2** (`srgb_icc`), which is genuinely infallible and kept.

### M3 — Optical-center normalization mismatch · **Covered by IP-05 §2.3-L1** (Initiative C)
- **Decision:** No new plan here — register as **Covered by IP-05 §2.3-L1** (already decided: scale the lensfun center offset to match lensfun's normalization — `1.0` = the maximal image dimension — rather than adding `0.5 + CenterX/Y` directly). This is a lens-stack item under **Initiative C (lensfun faithfulness)**.
- **Why:** `lens/lib.rs:142` does `center = [0.5 + (*lens).CenterX, 0.5 + (*lens).CenterY]`, which assumes lensfun's CenterX/Y are already frame-relative half-units — true only on a square sensor. lensfun normalizes the shift by the **max** image dimension (same divisor for both axes); on a non-square frame the off-center shift is scaled wrong on the longer axis. Invisible for the common centered lens (CenterX/Y = 0), bites only rare off-center calibrations. The correct divisor is the algorithm team's call and is decided in IP-05.
- **Action (tracked under IP-05 §2.3-L1, not duplicated here):** Map explicitly — scale CenterX/Y by the max-dimension factor before the `0.5 +`, and pin the `LensProfile.center` convention with a comment at `lens/lib.rs:142` so the wiring assumption is documented either way. The engineering ask (document/guard the assumption) is satisfied by that work.

### M4 — CA/vignetting coefficients not finiteness-guarded · **Add `is_finite` guards (match distortion)**
- **Decision:** Apply the same `is_finite()` guard (and optionally a plausibility clamp) to the CA (`ca_offsets`) and vignetting (`vignetting_falloff`) outputs that `radial_distortion` already has. Make the defensive posture consistent across all three mappers.
- **Why:** `radial_distortion` (`lens/lib.rs:198`) guards non-finite output, but `ca_offsets` (`:212-218`) and `vignetting_falloff` (`:225-231`) pass interpolated lensfun terms straight through. A NaN/garbage term from a corrupt DB entry would flow into the engine unchecked. Low likelihood (lensfun data is generally clean) but the inconsistency is a real robustness gap — one of three hardened, two not.
- **Action:** `latent-lens/src/lib.rs:212-218` and `:225-231` — guard outputs with `is_finite()` (fall back to the no-op identity: zero CA offset / unity falloff), mirroring the distortion path. A clamp to a plausible range is a reasonable optional addition.
- **Overlap:** Touches the same `lens_to_profile` mappers reworked under **Initiative C** (POLY3 TCA, PA vignetting). Land the guards as part of that lens-stack pass so the mappers are edited once.

### L1 — JPEG quality not configurable · **Higher default + plumb a quality param**
- **Decision:** Stop using image-rs's default quality (75) for an export-quality photo developer. Raise the default (≈92) via `JpegEncoder::new_with_quality`, and thread a quality parameter so it is surfaceable from CLI and GUI.
- **Why:** `JpegEncoder::new(file)` (`export/lib.rs:149`) uses quality 75 — visibly lossy and not surfaceable. For a tool whose point is fidelity, 75 is the wrong silent default.
- **Action:** `export/lib.rs:148-153` — `new_with_quality(file, q)`; default `q = 92`. Add an optional `quality` parameter to the export entry points (defaulted so existing callers are unchanged) and a `--quality` CLI flag / GUI control where JPEG is the chosen format. Pairs with **L2** (depth/format plumbing).

### L2 — 16-bit path unreachable from CLI · **Auto-select depth by format + `--depth` override**
- **Decision:** Make the tested-but-unreachable `save_16` path reachable from the CLI. Auto-select 16-bit for formats that benefit (tiff, png) and 8-bit for jpeg, with an explicit `--depth 8|16` override.
- **Why:** Confirmed: `develop` always calls `latent_export::save` (8-bit) at `main.rs:75`, never `save_16`, even for `.tiff` where 16-bit is the whole point. The 16-bit encoder exists and is tested but is dead from the CLI; the GUI export likewise uses 8-bit (`gui.rs:179`). A wide-gamut, highlight-rolled pipeline bands in 8 bits — the high-quality output is unreachable.
- **Action:** `latent-app/src/main.rs:73-77` (`develop`) and the `Develop` subcommand (`main.rs:25-30`) — add `#[arg(long)] depth: Option<u8>` (or a `Depth` enum); choose `save_16` vs `save` by depth, defaulting to 16 for tif/tiff/png and 8 for jpg/jpeg. Mirror the choice in `gui.rs:179` (`export`). Honors the high-quality option (default to 16-bit where the format supports it). Pairs with **L1** (same Develop-args plumbing) and benefits from **H2** (extension is already validated/known at this point).

### L3 — `self.texture.unwrap()` relies on an unenforced invariant · **Return the texture handle (no unwrap)**
- **Decision:** Remove the implicit coupling: have `render_preview` return (or hand back through the worker channel) the `TextureHandle`, so the call site holds a concrete handle and never unwraps `self.texture`.
- **Why:** `gui.rs:339-340` unwraps `self.texture` after the `if dirty` block; sound today only because the first frame has `dirty = self.texture.is_none()` true (`:188`, confirmed) and `render_preview` always sets the texture. That coupling is implicit — reordering `dirty` or making `render_preview` fallible turns it into a first-frame panic.
- **Action:** `latent-app/src/gui.rs:156-170` (`render_preview`) and `:339-340` (call site). Thread the handle out of `render_preview`'s return; or, as a floor, use `let-else`/`expect` with a message documenting the invariant. This is **required** rather than optional once **H1** makes the render path async/fallible.
- **Pairs with:** H1.

### L4 — Export path is free-text with no extension validation (GUI) · **Validate extension (folds into H2)**
- **Decision:** Validate the output extension when the user edits/commits the export path in the GUI, surfacing the same unsupported-extension message decided in **H2** before the export runs.
- **Why:** `gui.rs:312-318` is a free-text field; an unknown extension currently hits the silent untagged fallback (H2) and an unwritable path surfaces only as a status string. Once H2 returns a typed error, the GUI already surfaces it (status line, `:179-182`); adding an inline validity hint on the field is the small extra UX win.
- **Action:** `latent-app/src/gui.rs:312-318` — check the typed extension against the supported set and show an inline warning/disable-export-button when unsupported. Largely resolved by H2; this is the front-end affordance.

### L5 — Lensfun DB loaded synchronously every editor open · **Keep**
- **Decision:** Keep as-is. Acceptable; note for later.
- **Why:** `auto_lens_profile` / `Database::load()` (`gui.rs:81-94`) runs in `run` **before** the window opens (confirmed by the review), so it does not block an interactive frame — it only adds to startup latency, once per open. Caching/lazy-load is a premature optimization today.
- **Note:** If open latency becomes a concern, cache the loaded DB or lazy-load. Do **not** move it onto a render worker (the handle is not `Send`/`Sync`; see §3 / H1).

### N1 — `examples/lookup.rs:20` `.expect` on DB load · **Fix (friendly message)**
- **Decision:** Make the demo print a friendly line and exit cleanly when the lensfun-data package is absent, rather than panicking. Small, genuine improvement consistent with the crate's graceful "returns `None` when no DB" posture in `find_profile`.
- **Why:** As a manual demo a panic is tolerable, but a friendly message better matches the rest of the crate and is a one-line change.
- **Action:** `latent-lens/examples/lookup.rs:20` — replace `.expect("load the lensfun database …")` with a `match`/`if let Err` that prints "lensfun database not found (install the lensfun-data package)" and returns. Low priority.

### N2 — `srgb_icc()` `.expect` on `moxcms` encode · **Keep (verified safe)**
- **Decision:** No change. The `.expect("encode sRGB ICC profile")` at `export/lib.rs:114` is a pure, input-independent call (`moxcms::ColorProfile::new_srgb().encode()`) that cannot realistically fail; the panic is unreachable.
- **Note:** Recorded only for the panics-in-library-code audit trail. Distinct from **M2**, which is input-dependent and therefore changed.

### N3 — Duplicated `local_sel` clamp · **Extract a helper (DRY)**
- **Decision:** Factor the duplicated defensive clamp after list mutations into a small helper. Genuine (minor) improvement — keeps the two sites from drifting.
- **Why:** `gui.rs:885` and `:908` clamp `*sel` after list mutations (good), but the clamp is duplicated. A `clamp_selection(...)` helper DRYs it and is the natural home for the GUI-state test (§6).
- **Action:** `latent-app/src/gui.rs:885,908` — extract `clamp_selection`. Low priority; pair with the GUI-state tests.

### Test-coverage gaps (§6) · **Add the missing tests**
Existing tests are good (exact-value pins, ICC round-trips, preview-equals-export, lens model-mapping with degenerate guards). Close the gaps the review flags, prioritizing the ones that lock a contract being changed above:

- **Export edge/contract:** 0×0 and 0×N image (locks **M1**); unsupported-extension behavior (locks **H2** — asserts the typed error); a `.jpg`-carries-ICC test (the path is shared with png/tiff but currently only png/tiff are covered). Update/extend the pinned-value export tests for the **L2** 16-bit-default and **L1** JPEG-quality changes.
- **Lens lookup plumbing:** feature-gated test (runs only when `liblensfun-data` is present) or a stub/fake exercising `Database`→`find_profile` (null/empty-list handling, `lf_free` on every path, not-found→`None`) and `lens_to_profile` end-to-end aggregation. The pure mappers are tested; the pointer/free logic and field-routing are not.
- **CLI arg parsing:** `Cli::try_parse_from([...])` per subcommand plus an error case (e.g. missing `OUTPUT`), including the `--gpu`, and the new `--depth`/`--quality` flags.
- **`develop`/`develop_to_image` error path:** cover the "camera color matrix is singular" error (`main.rs:69`) and the unpack-failure path → exit-code behavior, via a fixture or failure-injecting stub.
- **GUI state transitions:** construct a `History<Settings>` directly (as `history.rs` does for `History<i32>`) and unit-test `gesture()` begin/commit/discrete, autosave idle+diff gating, and the extracted `clamp_selection` (**N3**), and variant add/switch.
- **Out-of-range sidecar values:** add a `latent-edit`-side validation/clamp test — `Document::from_ron` does not range-check, so a hand-edited sidecar can inject e.g. `contrast > 1`. This is the **Initiative F (sanitize-on-load)** theme; the risk surfaces in this crate because the GUI assumes clamped values.

### Positives (§7) · **Keep**
- Keep all verified-correct design choices and their guarding tests: single shared output transform (`to_display` → `to_srgb8`) giving structural preview/file agreement; clamp-before-round quantization; per-format ICC embedding (validated `moxcms` profile); the careful, RAII-clean lens FFI (null checks, `lf_free` on every path incl. early returns, `Drop`/`lf_db_destroy`, correct `CString` lifetimes, divide-by-near-zero finiteness guard, accurate SAFETY comments — FFI assessed **sound** in §3); the transactional undo/redo + debounced autosave model; graceful degradation on bad input (missing/corrupt sidecar, bad/unsupported RAW, missing lensfun DB); forward-compatible sidecar + never-overwrite rule; idiomatic CLI (typed errors, stderr + exit-1, GPU-with-CPU-fallback at the composition root).
- One add-on flowing from §3/§5: the **corrupt-sidecar silent drop** (`gui.rs:34`, `from_ron().ok()` → default) never crashes but is silently ignored — surface a status-line warning ("ignored unreadable sidecar; using defaults"). Small UX nicety; covered by the **Initiative F** sanitize-on-load theme.

---

## Resulting implementation notes (derived from the decisions)

Grouped by crate; independent of the image-processing color/lens initiatives except where noted.

**`latent-export`** (most impactful correctness/contract work):
1. **H2** — replace the `_ => buf.save(path)` arm with a typed `ImageError::Unsupported` (`lib.rs:135-155`). This is the headline color-management fix.
2. **M1 + M2** — guard zero dimensions (early `Err`) and replace the `from_raw` `.expect` with a mapped `Err` so `save`/`save_16` honor `ImageResult` for all inputs (`lib.rs:86-106`).
3. **L1** — JPEG `new_with_quality(file, 92)` + an optional quality parameter (`lib.rs:148-153`).
4. Keep **N2** (`srgb_icc` expect) and all §7 positives; extend the export tests for the unsupported-extension error, 0-dim guard, `.jpg` ICC, 16-bit default, and JPEG quality.

**`latent-app`** (UX + reachability):
5. **H1** — move `export` (and ideally `render_preview`) onto a worker thread with status + `request_repaint` (`gui.rs:156-183`, `:335-337`, `:390-392`); keep the lensfun handle off the worker.
6. **L3** — return the `TextureHandle` from `render_preview` (no `self.texture.unwrap()`); required once H1 makes render async (`gui.rs:156-170`, `:339-340`).
7. **L2 + L1** — add `--depth 8|16` (default 16 for tif/tiff/png, 8 for jpeg) and `--quality` to the `Develop` subcommand; route `save` vs `save_16` accordingly in `develop` (`main.rs:25-30`, `:73-77`) and mirror in GUI `export` (`gui.rs:179`).
8. **L4** — validate the export-path extension in the GUI (folds into H2) (`gui.rs:312-318`); **N3** — extract `clamp_selection` (`gui.rs:885,908`); corrupt-sidecar status warning (`gui.rs:34`).
9. CLI tests (`try_parse_from` per subcommand incl. `--gpu`/`--depth`/`--quality`, error cases), `develop` error-path test, and GUI-state unit tests via a directly-constructed `History<Settings>`.
10. **L5** — keep DB-on-open; note caching only if startup latency matters.

**`latent-lens`**:
11. **M4** — add `is_finite()` guards to `ca_offsets` (`lib.rs:212-218`) and `vignetting_falloff` (`:225-231`) to match `radial_distortion`.
12. **N1** — friendly DB-missing message in `examples/lookup.rs:20`.
13. Add a one-line doc note that `Database`'s handle is single-threaded by design (relevant to H1's off-thread render).
14. Lens-lookup plumbing tests (feature-gated or stubbed): `find_profile` null/empty/not-found and `lf_free`-on-every-path; `lens_to_profile` aggregation.

**Cross-refs / overlaps:**
- **M3** → **IP-05 §2.3-L1** (center offset scaled by min(w,h)/2 to match lensfun's max-dimension normalization) — a lens-stack item under **Initiative C (lensfun faithfulness)**; the M4 guards land in the same `lens_to_profile` pass.
- Any export **hue-preserving rolloff / color-transform** concern is **not** re-decided here — it is **IP-01 (#1 sRGB matrix, #7 highlight rolloff)**. This register covers only the export *engineering* (ICC tagging, quantization contract, untagged fallback, depth reachability).
- The **sidecar range/NaN sanitize-on-load** test (out-of-range `contrast`, curve points) is the code-review face of **Initiative F (robustness / sanitize-on-load)**; the validation lives in `latent-edit`, the consuming risk surfaces in `latent-app`.

This register reflects intent only; nothing here has been implemented.
