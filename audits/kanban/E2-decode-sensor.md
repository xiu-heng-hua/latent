# E2 — RAW decode & sensor metadata

> *(Independent stream — can start immediately; no dependency on E0/E7.)*
> Makes RAW decode correct on all mainstream Bayer sensors and FFI-sound on untrusted input. Realizes IP-02 (RAW decode & demosaic) and CR-01 (RAW & image foundations). The X-Trans/Foveon guard (C2) is safety-critical and unblocks the level-metadata rework (C3).
>
> **Decision sources:** [IP-02](../decisions/02-raw-decode-and-demosaic.md) · [CR-01](../decisions/code-review/01-raw-and-image-foundations.md).
> **Reference docs:** `docs/demosaic-dng-spec-1.6.0.0.pdf` (Ch. 5 — `BlackLevel`, `BlackLevelRepeatDim`, `WhiteLevel`, `CFAPattern`), `docs/demosaic-malvar-he-cutler-2004.pdf` (Fig. 2 kernels), `docs/color-dng-spec-1.4.0.0.pdf`.
> **FFI fields (confirmed against generated `bindings.rs`):** `color.cblack: [c_uint; 4104]`, `color.black: c_uint`, `color.maximum: c_uint`, `color.linear_max: [c_uint; 4]`, `idata.filters: c_uint`, `idata.colors: c_int`, `sizes.raw_pitch: c_uint`.
>
> **Shared-edit warning:** C1, C2, C3, C4 all touch `read_metadata`/`unpack`/`normalized` in `latent-raw/src/lib.rs`. Land them in the order C1 → C2 → C4 (or C1 → C4 → C2; both touch `unpack`) → C3, then C6/C7 (demosaic), and C8 last (tests over the finished surface). Keep one PR per card but rebase rather than parallel-merge the `lib.rs` cards.

---

### E2-C1 — Extend `read_metadata` (full `cblack` pattern, `linear_max[4]`, `filters`, `colors`)
- Implements: IP-02 §2.1 / §2.2 / §2.8b (the single metadata pass)    Priority: High
- Crates/files: `latent-raw/src/lib.rs` (`Metadata` struct ~106-131; `read_metadata` 504-534)
- Depends on: —                                                       Blocks: E2-C2, E2-C3
- Heads-up:
  - One pass that widens what `read_metadata` captures so C2 (guard) and C3 (normalization) have the data they need. Decode-correctness only; no behavior change to `normalized`/`clip_mask` yet (that is C3).
  - **`cblack` layout** (LibRaw, mirrors DNG `BlackLevel` + `BlackLevelRepeatDim`): the FFI field is `color.cblack: [c_uint; 4104]`. Indices `[0..4]` are the per-CFA-channel black offsets (current code reads only these into `cblack: [u32;4]`). Indices `[4]` and `[5]` are the **pattern dimensions** `W = cblack[4]`, `H = cblack[5]`; indices `[6 ..]` are the `W*H` row-major repeating black pattern (`cblack[6 + r*W + c]`). See `docs/demosaic-dng-spec-1.6.0.0.pdf` Ch. 5 (`BlackLevelRepeatDim` / `BlackLevel` matrix). When `W==0||H==0` there is no 2-D pattern (most Bayer bodies) — that is the common case and must be handled, not assumed.
  - Widen `Metadata`: keep `cblack: [u32; 4]` (per-channel) and **add** `cblack_pattern: { w: u32, h: u32, values: Vec<u32> }` (or `cblack_dims: [u32;2]` + `cblack_grid: Vec<u32>`). Copy `cblack[4]`, `cblack[5]`, and `cblack[6 .. 6 + (w*h)]` defensively (clamp `w*h` so the slice never exceeds the 4104-element array — `6 + w*h <= 4104`; treat an over-range dim as "no pattern").
  - Add `linear_max: [u32; 4]` from `color.linear_max` (per-channel white; `0` means "unset → fall back to `maximum`").
  - Add `filters: u32` from `idata.filters` and `colors: u32` from `idata.colors` (cast the `c_int`); these feed C2's guard.
  - **Belt-and-suspenders CFA clamp (CR-01 H1/L2, realized here):** the `cfa` build loop (512-516) stores `libraw_COLOR()` directly; that returns `6` for Foveon/full-color sensors. Clamp/validate each entry to `0..4` at this FFI boundary — either clamp to a safe sentinel or carry a `cfa_valid: bool` — so the 4-element `cblack`/`gains` array invariant holds defensively even if C2's guard is ever bypassed. (C2 is the primary fix; this is the second line of defense at the single source.)
  - **N3 cleanup (CR-01 N3):** while editing this region, relocate the misplaced `# Safety` doc — `read_metadata`'s `/// # Safety` clause currently sits above `c_str_field` (489-502) instead of above `unsafe fn read_metadata` (504). Move it; give `c_str_field` its own (non-`# Safety`) doc.
- Acceptance:
  - `Metadata` exposes the pattern (`cblack_pattern`/dims+grid), `linear_max[4]`, `filters`, `colors`; `read_metadata` populates them; `cargo build` + existing tests green.
  - New test `read_metadata_captures_cblack_pattern_dims` (or a unit test on a hand-built `cblack` array via a small pure helper): a `W=2,H=2` pattern folds to the right per-pixel offsets; `W==0||H==0` yields the empty/no-pattern case; an over-range `W*H` is rejected (clamped to no-pattern) without panicking.
  - New test `cfa_codes_are_clamped_to_bayer_range`: a CFA entry of `6` (Foveon `libraw_COLOR`) does not survive as `6` into `Metadata.cfa` (clamped or flagged).

---

### E2-C2 — Sensor guard: accept only 2×2 RGB Bayer; reject X-Trans/Foveon before any `cfa` indexing
- Implements: IP-02 §2.8b · CR-01 H1 / M1 (deferred fix realized here)    Priority: **Critical**
- Crates/files: `latent-raw/src/lib.rs` (`is_rgb_bayer` 435-437; the gate in `unpack` 475-477)
- Depends on: E2-C1                                                         Blocks: E2-C3, E2-C8
- Heads-up:
  - **Safety-critical.** Today `is_rgb_bayer` only checks `cdesc == b"RGBG"`, which is true for *every* RGB sensor including X-Trans and Foveon. Two latent panics ride on this:
    - **Foveon / full-color (`filters==0`):** `libraw_COLOR()` returns the special value `6`, so `cfa` becomes `[6,6,6,6]`; the file decodes, then **panics** at `cblack[6]` in `normalized` (164-165) and `gains[6]` in `apply_white_balance` (204-205). A panic on untrusted RAW input violates the decode layer's "never panic, return typed `RawError`" contract (CR-01 H1).
    - **X-Trans (`filters==9`):** a 6×6 CFA reported as `RGBG` is silently demosaiced as 2×2 Bayer → scrambled color, no error (CR-01 M1).
  - **Fix:** rewrite the guard to read C1's `filters`/`colors`. Accept only standard 2×2 RGB Bayer: `colors == 3 && filters != 0 && filters != 9`. Reject X-Trans (`filters == 9`), Foveon/non-Bayer/linear (`filters == 0`), and non-3-color CFAs **explicitly** with `RawError::NoMosaic`. Keep the `cdesc == "RGBG"` check too (it still rejects CYGM/RGBE which have `colors==3, filters!=0,9`), or fold it in — the `filters`/`colors` test is the load-bearing one.
  - **Ordering is the soundness guarantee (CR-01 H1 add-on).** The guard MUST run in `unpack` **before** any `cfa`/`cblack`/`gains`/`channel_at` indexing. The decode order is `from_raw_parts` (470) → `read_metadata` (471, builds `cfa`) → guard (475). `cfa` is only *indexed* in the post-decode pipeline (`normalized`/`apply_white_balance`/`channel_at`), all reached only after `unpack` returns — so gating at line 475 provably prevents the value-`6` index. Do **not** move any `cfa` indexing earlier than this gate. (C1's `cfa∈0..4` clamp is the belt-and-suspenders second line if the guard is ever bypassed.)
  - X-Trans *black-pattern* groundwork from C1 stays dormant: X-Trans is rejected here until a real 6×6 X-Trans demosaic exists.
- Acceptance:
  - `is_rgb_bayer` (or its replacement) accepts `filters` of a real Bayer mask with `colors==3`; rejects `filters==0` and `filters==9` and `colors!=3`.
  - New regression test `foveon_filters_zero_is_rejected_not_panicked`: a `RawImage`/metadata with `filters==0` (and `cfa` containing `6`) returns `Err(RawError::NoMosaic)` from the guard — **today this path panics**; the test must fail before the fix.
  - New test `xtrans_filters_nine_is_rejected`: `filters==9` → `Err(RawError::NoMosaic)`.
  - Existing `only_rgb_bayer_sensors_are_supported` updated to the new signature and still green; CYGM/RGBE still rejected.

---

### E2-C3 — Normalization rework: per-pixel black (pattern), per-channel white (`linear_max`), consistent per-plane scale
- Implements: IP-02 §2.1 / §2.2 / §2.3 · CR-01 L2 (mosaic-len assert)    Priority: High
- Crates/files: `latent-raw/src/lib.rs` (`normalized` 153-173; `clip_mask` 181-186; `apply_white_balance` 193-207 for the assert)
- Depends on: E2-C1, E2-C2                                               Blocks: E2-C7
- Heads-up:
  - Now that C1 supplies the pattern + `linear_max` and C2 guarantees a true Bayer `cfa`, rework the level math. Three interlocking changes — keep them coherent:
  - **Per-pixel black (§2.1):** replace `black = base + cblack[color]` (164-165) with `black = base + cblack[color] + pattern(row,col)`, where `pattern(row,col) = cblack_grid[(row % H) * W + (col % W)]` when `W>0 && H>0`, else `0`. Guard `W==0||H==0` (the common Bayer case → pattern contributes nothing). Index 4 of the raw `cblack` was previously mis-used as a "G2 black" — it is a pattern dimension, not a channel offset; that misread is gone once C1 lands.
  - **Per-channel white (§2.2):** read `linear_max[c]` for the photosite's CFA channel; use it as the per-channel white where `linear_max[c] != 0`, falling back to `maximum` (the current `white`) when `0`. Apply the same per-channel white in `clip_mask` (181-186): a sample is clipped iff `s as u32 >= white_for_its_channel` (make `clip_mask` per-CFA-channel; today it uses the single scalar `white`). This kills magenta blown highlights from a miscalibrated single `maximum`.
  - **Consistent per-plane scale (§2.3):** stop giving each channel a *different* gain via its own `(white − black_c)` as a side effect of per-channel black. Use `(white_c − black_c)` **consistently** with the per-channel white from §2.2 so any per-channel normalization is an intentional colorimetric choice (DNG's single per-plane scale model), not an accident of per-channel black. Keep the `.max(1.0)` guard against a corrupt `white <= black`.
  - **CR-01 L2 asserts:** add `debug_assert_eq!(self.mosaic.len(), w*h)` at the top of `normalized` and `apply_white_balance`, plus a doc line stating the `mosaic.len() == width*height` and `cfa ∈ 0..4` invariants. **Footgun:** the `white_balance_neutralizes_a_gray_patch` fixture (610-638) has `mosaic: vec![]` with `width:2,height:2` — it would trip this assert. Fix that fixture as part of C8 (or here if C8 is later); coordinate so the assert lands with a consistent fixture.
  - WB-before-demosaic (§2.4) and `channel_at` (§2.8a) are verified correct — do not touch.
- Acceptance:
  - Existing `normalize_maps_black_to_zero_and_white_to_one`, `normalize_subtracts_per_channel_black`, `clip_mask_marks_saturated_samples` updated for the new model and green.
  - New test `normalize_folds_2d_black_pattern`: a `W=2,H=2` cblack pattern produces the correct per-pixel pedestal across a ≥4×4 mosaic (each of the four pattern cells removed at its photosites).
  - New test `per_channel_linear_max_drives_white`: `linear_max=[r,0,b,0]` (one channel unset) uses `linear_max[c]` where nonzero and `maximum` where zero, in **both** `normalized` and `clip_mask`; a sample at its channel's `linear_max` lands at 1.0 and is flagged clipped.
  - New test `consistent_plane_scale_keeps_neutral_neutral`: with the new scale, a WB-neutral gray patch normalizes without per-channel tint (no faint mid-tone cast WB can't cancel).

---

### E2-C4 — Honor `raw_pitch` row stride in the `unsafe` unpack load
- Implements: CR-01 H2                                                   Priority: High
- Crates/files: `latent-raw/src/lib.rs` (`unpack` `from_raw_parts` block 461-470; optionally `Metadata` to surface `raw_pitch` for a fixture)
- Depends on: —  *(coordinate with C1, same `unpack`/`read_metadata` region)*    Blocks: E2-C8
- Heads-up:
  - This is the single load-bearing `unsafe` length in the crate. Today (469-470): `len = raw_width * raw_height`, then `from_raw_parts(samples, len)` — assuming `raw_image` is tightly packed at `raw_width*2` bytes/row. **That assumption is unverified and wrong for padded unpackers.** `sizes.raw_pitch` exists precisely because some unpackers pad rows. When `raw_pitch/2 > raw_width`: (a) the last-row read over-reads past `raw_alloc` → **undefined behavior**; (b) every later `y*w+x` index addresses the wrong photosite → **sheared output**.
  - **Fix — copy row-by-row at stride (the correct option, not just an assert):**
    - `let pitch_u16 = { let p = (*raw).sizes.raw_pitch as usize / 2; if p == 0 { width as usize } else { p } };` — `raw_pitch == 0` (some paths leave it unset) falls back to `raw_width` (i.e. `raw_width*2` bytes).
    - Build the source slice as `from_raw_parts(samples, pitch_u16 * height)` (the full padded allocation, not `width*height`).
    - Copy `width` samples from each of `height` rows at `pitch_u16` stride into a tight `width*height` `mosaic`. The destination stays tightly packed so the rest of the pipeline's `y*width+x` indexing is unchanged.
  - Sanity-guard `pitch_u16 >= width` (a pitch smaller than the width is malformed — treat as the tight fallback or a typed error rather than reading garbage).
  - Optionally store `raw_pitch` in `Metadata` so C8 can pin it in a fixture. This is independent of the IP track (IP-02 §2.10 deemed the *numeric* lifecycle correct and explicitly handed `raw_pitch` to the code-review track); only the *file region* overlaps C1 — rebase, don't parallel-merge.
- Acceptance:
  - The `from_raw_parts` load reads `pitch_u16 * height` source samples and copies row-by-row at stride into a tight `width*height` mosaic; `raw_pitch==0` → `width` fallback; clippy/fmt clean.
  - New test `padded_pitch_copies_rows_without_shear`: a synthetic source buffer with `raw_pitch/2 > raw_width` (extra padding columns per row) extracted via the row-copy helper yields the un-sheared `width*height` mosaic (the padding dropped, photosites aligned). Factor the stride-copy into a small pure helper so it is testable without LibRaw.
  - New test/doc `raw_pitch_zero_falls_back_to_tight_width`: `raw_pitch==0` reproduces the old tight-packing behavior exactly.

---

### E2-C5 — Overflow-safe `ImageBuf` (`checked_mul` + `try_new`)
- Implements: CR-01 M2 (pairs with N1 `try_get`/`try_set`, N2 `#[must_use]`)    Priority: Medium
- Crates/files: `latent-image/src/lib.rs` (`new` 18-25; `index` 56-58; `get`/`set` 61-69; accessors 27-48)
- Depends on: —                                                          Blocks: —
- Heads-up:
  - `new` (18-25) and `index` (56-58) both compute `width as usize * height as usize` (product up to ~1.8e19) **unchecked**. `ImageBuf` accepts arbitrary `u32` dims (not just u16 RAW dims), so on **32-bit** the multiply wraps silently → `new()` under-allocates while `index()` computes a different (also-wrapped) offset → logic error / OOB read-write. On 64-bit it can't overflow `usize` but a huge `vec![..; count]` aborts the process ungracefully.
  - **Fix:** compute the element count with `checked_mul`. Add a fallible `try_new(width, height) -> Option<ImageBuf>` (or `Result`) that returns `None` on overflow. Use the **same** checked computation in `index` so `new` and `index` can never disagree. If `new` must stay infallible for ergonomics, route it through `try_new().expect("...")` with a documented panic, and ensure RAW-sized construction (`demosaic_*`, `reconstruct_highlights`) uses the checked path.
  - **N1 (CR-01):** keep panicking `get`/`set` for the hot path but **document the panic contract** on the type and add non-panicking `try_get`/`try_set` returning `Option` for callers handling untrusted dims.
  - **N2 (CR-01):** add `#[must_use]` to the pure accessors (`width`, `height`, `len`, `is_empty`, `pixels`) and the value-returning `Mat3` ops in `color.rs` (`mul_vec`, `mul`, `det`, `row_normalized`, `inverse`).
  - Sanitize-at-boundary: this is the general buffer type — the checked ctor is where dimension trust is established for the whole pipeline.
- Acceptance:
  - `try_new` exists and returns `None`/`Err` on an overflowing `width*height`; `new`'s panic (if retained) is documented.
  - `index` uses the same checked math (no silent wrap).
  - New test `imagebuf_overflow_dims_are_rejected`: `try_new(u32::MAX, u32::MAX)` (or a product overflowing `usize` on the target) returns `None`/`Err` rather than allocating/aborting; a normal `try_new(4,3)` is `Some`.
  - New test `try_get_out_of_bounds_is_none`: `try_get`/`try_set` return `None` past the edge while `get`/`set` still panic (documented).
  - `#[must_use]` present on the named accessors and `Mat3` ops; `cargo clippy --all-targets` clean.

---

### E2-C6 — Mirror/clamp the MHC 5×5 border (no drop to bilinear)
- Implements: IP-02 §2.6                                                 Priority: Low
- Crates/files: `latent-raw/src/lib.rs` (`mhc_pixel` 269-344; `demosaic_mhc` border branch 348-362)
- Depends on: —                                                          Blocks: —
- Heads-up:
  - Today `demosaic_mhc` (353-357) runs the sharper 5×5 MHC filter only where the full window is in bounds (`x>=2 && y>=2 && x+2<w && y+2<h`) and falls back to `bilinear_pixel` on the 2-px frame. **Decision:** run MHC to the very edge by **mirroring/clamping** out-of-bounds taps instead of dropping to bilinear.
  - The convolution in `mhc_pixel` (309-316) indexes `mosaic[(y+dy-2)*w + (x+dx-2)]` assuming in-bounds. Introduce a coordinate-reflecting sampler — clamp (`coord.clamp(0, dim-1)`) or mirror (reflect across the edge) — for each tap so the kernel weights still land on a valid same-phase sample. **CFA-phase caveat:** mirroring must preserve Bayer parity — reflecting a coordinate changes its `(row%2,col%2)` phase unless you reflect by an **even** offset. Clamp-to-edge is simpler and parity-safe at the cost of slight bias; mirror-by-2 (reflect keeping parity) is sharper. Pick one and note which; the MHC kernels (verified correct per §2.5) are unchanged — only the tap fetch changes.
  - This is cosmetic/marginal (2-px frame), so correctness of the existing interior path must not regress.
- Acceptance:
  - `demosaic_mhc` no longer calls `bilinear_pixel`; every pixel uses MHC with a border-safe tap fetch (clamp or parity-preserving mirror).
  - Existing `mhc_beats_bilinear_on_detailed_image` and `all_cfa_phases_reconstruct_equally` still green (no phase regression).
  - New test `mhc_border_uses_mhc_not_bilinear`: on a smooth gradient the border pixels match the MHC reconstruction (and are at least as accurate as the old bilinear-border output — MAE not worse).
  - New test `mhc_border_preserves_cfa_phase`: a colorful image reconstructs at the border without channel swaps (would blow up if the mirror broke parity).

---

### E2-C7 — Spatial highlight propagation (LCH-blend/guided) + widen clip mask to 5×5 MHC support
- Implements: IP-02 §2.7 (L1 fix + spatial feature)                      Priority: Low *(largest single item — a real feature)*
- Crates/files: `latent-raw/src/lib.rs` (`clipped_channels` 369-388; `reconstruct_highlights` 402-416; new propagation stage)
- Depends on: E2-C3  *(corrected white level bounds quality)* · MAY reuse **E4-C1** guided filter (cross-epic)    Blocks: —
- Heads-up:
  - Two parts, building on the conservative core (rebuild ≥2 clipped channels to the pixel's peak, keep measured channels) which stays:
  - **(a) L1 fix — widen the clip-flag support to 5×5.** `clipped_channels` (369-388) currently scans a **3×3** neighborhood for same-color clipped samples, but `demosaic_mhc` draws an interpolated channel from the **5×5** MHC support. That mismatch leaves a faint ring at the edge of MHC-reconstructed regions. Widen the neighborhood loop to 5×5 for the MHC path (match `mhc_pixel`'s footprint). Keep the center (known) channel exact (clipped iff its own photosite saturated, from the raw mask).
  - **(b) Spatial color propagation.** After the per-pixel rebuild, large blown regions go flat at `peak` (no texture/gradient). Add a propagation stage so structure recovers from surrounding unblown pixels: either an **LCh blend** (carry chroma/hue from the region boundary inward while keeping the rebuilt luminance) or a **guided-filter color propagation**. **Cross-epic reuse:** E4-C1 (dehaze) owns the reusable O(N) guided-filter primitive; if the guided approach is chosen, depend on E4-C1 rather than re-implementing (Global rule §5: one owner for the guided filter). If E4-C1 isn't ready, the LCh-blend variant has no cross-epic dependency — prefer it to avoid blocking, and note the reuse hook for later.
  - **Depends on C3:** propagation quality is bounded by `peak`/white-level accuracy — only meaningful once per-channel `linear_max` (C3) makes `peak` trustworthy.
  - This is post-demosaic, in white-balanced camera RGB, before the color matrix (same stage as today's `reconstruct_highlights`).
- Acceptance:
  - `clipped_channels` scans 5×5 for the MHC path; existing `reconstruct_highlights_rebuilds_blown_channels_keeping_measured_ones` still green (conservative core preserved: single-blown-channel colors kept, all-blown → neutral peak).
  - New test `clip_support_matches_mhc_5x5`: a clipped photosite influences the clip flag of interpolated channels out to the 5×5 radius (a pixel 2 px away from a saturated same-color site is flagged), removing the 3×3 ring.
  - New test `highlight_propagation_recovers_gradient`: a large blown region adjacent to a smooth gradient recovers non-flat structure (variance > 0 across the region, hue continuous from the boundary) instead of a flat `peak` plateau.
  - If guided-filter path taken: it calls the E4-C1 primitive (no duplicate guided-filter impl); else the LCh-blend path is self-contained.

---

### E2-C8 — Decode/FFI-error + boundary tests (HSV, Mat3, error paths)
- Implements: CR-01 §4 (test gaps + fixture footgun) · CR-01 L1 (HSV sextant)    Priority: Medium
- Crates/files: `latent-raw/src/lib.rs` (`#[cfg(test)]`); `latent-image/src/color.rs` (`hsv_to_rgb` 211-225 + tests); `latent-image/src/lib.rs` (`downscaled`/`tone` boundary tests)
- Depends on: E2-C2, E2-C4  *(regresses the guard + pitch)*              Blocks: —
- Heads-up:
  - The suite is strong on the happy path but blind to the exact failure modes the reviews found. Add, priority-ordered (CR-01 §4):
    1. **Non-Bayer / value-`6` CFA path (H1/M1):** construct metadata with `filters==0` (and `cfa` containing `6`) and an X-Trans `filters==9` case; assert clean `Err(RawError::NoMosaic)` post-C2 (today panics). **This is the C2 regression** — coordinate so it lives once.
    2. **`raw_pitch` / stride (H2):** the padded-pitch fixture for C4 (row-copy yields un-sheared mosaic; `raw_pitch==0` fallback). **This is the C4 regression.**
    3. **FFI error paths:** `RawError::Unpack`, `RawError::NoMosaic` (both null `raw_image` *and* non-RGBG / rejected-filters `cdesc`), and `RawError::InvalidPath` (path with an interior NUL — trivially testable without LibRaw via `unpack(Path::new("a\0b"))`). Today only `RawError::Open` is exercised (`missing_file_is_a_typed_error`).
    4. **HSV/color extremes (incl. L1 sextant fix):** the `hsv_to_rgb` sextant match (color.rs:216) does `h6 as u32` — `f32::rem_euclid(1.0)` can return *exactly* `1.0` for tiny negative inputs (`1.0−ε` rounds to `1.0` in f32), so `h6==6.0` and `h6 as u32 == 6` is reachable, and `color_mix` feeds slightly-negative hue shifts (249,252). It is currently right only by accident (`x==0` collapses the magenta `_` arm to red). **Fix:** clamp the sextant — `let sextant = (h6 as u32).min(5);` before the match. Add tests: achromatic early-return (`s<=1e-6`), `color_mix` hue at band center / wraparound (the L1 boundary), negative/super-unit value, and NaN/Inf into `rgb_to_hsv` (pin the `f32::max/min` NaN-dropping behavior).
    5. **`Mat3::inverse` near the `1e-12` threshold** (det just above/below the singular cutoff).
    6. **`downscaled` edge cases:** `max_dim==0`, 1-pixel image, extreme aspect ratios, the `span`'s `b.max(a+1)` div-by-zero guard.
    7. **`tone::eval`** lower-extrapolation + `i >= n-1` clamp + exact `lut[n-2]` end-slope (ties to CR-01 L3 — that constructor enforcement is owned by E1, but the boundary tests live here).
    8. **Seeded randomized sweeps** for `hsv` round-trip and `Mat3` inverse (cheap coverage widening).
  - **Fixture footgun (explicit):** `white_balance_neutralizes_a_gray_patch` (610-638) builds a `RawImage` with `width:2, height:2` but `mosaic: vec![]` — **inconsistent**. It would false-positive C3's `debug_assert_eq!(mosaic.len(), w*h)` and break any future method reading `self.mosaic`. Make the fixture's `mosaic`/`width`/`height` consistent (give it 4 samples) as part of this card; coordinate with C3 so the assert and the fix land together.
- Acceptance:
  - Tests 1 (`foveon`/`xtrans` → `Err`) and 2 (padded pitch) exist and regress C2/C4 (fail before those cards, pass after).
  - `InvalidPath`, `Unpack`, `NoMosaic` error paths each have a test; `hsv_to_rgb` sextant clamped with a boundary test (`color_mix` at a band center and at a slightly-negative hue stays correct).
  - `Mat3::inverse` threshold, `downscaled` edge cases, and `tone::eval` extrapolation/end-slope tests present; seeded round-trip sweeps for HSV and `Mat3` present.
  - `white_balance_neutralizes_a_gray_patch` fixture has a consistent `mosaic.len()==4`; `cargo test --workspace` green with C3's `debug_assert` enabled.

---

## Epic done when
- The decoder **accepts only standard 2×2 RGB Bayer** and returns a typed `RawError::NoMosaic` (never panics) for X-Trans (`filters==9`), Foveon/full-color (`filters==0`), and non-3-color/CYGM/RGBE CFAs — the guard running before any `cfa`/`cblack`/`gains` indexing, with a defensive `cfa∈0..4` clamp at the FFI boundary (C1, C2).
- Level metadata is **correct on mainstream Bayer sensors**: full 2-D `cblack` pattern folded into per-pixel black, per-channel `linear_max` driving both the white level and the clip mask, and a consistent per-plane scale that leaves a WB-neutral patch tint-free (C1, C3).
- The load-bearing `unsafe` unpack **honors `raw_pitch`** (row-copy at stride, `0`→tight fallback) — no last-row over-read UB, no sheared output (C4).
- `ImageBuf` construction/indexing is **overflow-safe** (`checked_mul` + `try_new`, matching `index`), with `try_get`/`try_set` and `#[must_use]` on pure accessors/`Mat3` ops (C5).
- Demosaic runs MHC to the **border** (parity-safe mirror/clamp) and highlight reconstruction has **5×5 clip support + spatial propagation** so large blown regions recover structure (C6, C7).
- The test suite regresses every failure mode found: the panic→`Err` guard path, the `raw_pitch` stride, all FFI error paths, the HSV sextant boundary, `Mat3`/`downscaled`/`tone` edges — and the `white_balance_neutralizes_a_gray_patch` fixture is consistent (C8).
- Baseline green throughout: `cargo fmt --check`, `cargo clippy --all-targets` (zero warnings), `cargo test --workspace`.
