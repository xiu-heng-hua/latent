# Decision Register — Audit 02 (RAW Decode & Demosaic)

**Source audit:** [`../image-processing/02-raw-decode-and-demosaic.md`](../image-processing/02-raw-decode-and-demosaic.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Each §2 point reviewed interactively, in document order (2.1→2.10), accept/modify/keep for each.

---

## Decision summary

| Point | Topic (file) | Finding · Severity | Decision | Outcome |
|---|---|---|---|---|
| **2.1** | `cblack` 2-D black pattern (`lib.rs:164,520`) | C1 · Critical | **Read the full cblack pattern** | Change |
| **2.2** | White level / `linear_max` (`lib.rs:181,166`) | H1 · High | **Use per-channel `linear_max`** | Change |
| **2.3** | Normalization denominator (`lib.rs:166-169`) | M1 · Medium | **Use a consistent per-plane scale** | Change |
| **2.4** | WB on mosaic before demosaic (`lib.rs:193`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.5** | MHC 5×5 kernels (`lib.rs:274-342`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.6** | MHC 2-px border fallback (`lib.rs:353`) | L2 · Low | **Mirror/clamp for edge MHC** | Change |
| **2.7** | Highlight reconstruction (`lib.rs:369-416`) | L1 · Low (+caveats) | **Also add spatial propagation** (incl. L1 5×5 fix) | Change (feature) |
| **2.8a** | `channel_at` CFA→RGB mapping (`lib.rs:210`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.8b** | `is_rgb_bayer` sensor guard (`lib.rs:435`) | H2 · High | **Check `filters`/`colors`, reject X-Trans** | Change |
| **2.9** | Bilinear demosaic & borders (`lib.rs:218`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.10** | FFI / unpack lifecycle (`lib.rs:441`) | — · Correct* | **Keep as-is (verified correct)** | No change |

\* §2.10 is "correct" only for *numeric/decode* scope; the **engineering** defects in this same FFI (Foveon OOB panic, ignored `raw_pitch`) live in the code-review track — see [code-review 01](../code-review/01-raw-and-image-foundations.md).

**Tally:** 6 changes (incl. one feature add), 5 keep-as-is. The stance is "make decode correct on all mainstream sensors," fixing every level-metadata gap and upgrading highlight handling.

---

## Per-point decisions

### 2.1 — `cblack` 2-D pattern · **Read the full cblack pattern** *(C1, Critical)*
- **Decision:** Read the complete `color.cblack[]` (the FFI binding is `[c_uint; 4104]`), not just `cblack[0..4]`. Compute per-pixel black as `black + cblack[c] + cblack[6 + (row % H)*W + (col % W)]` with `W=cblack[4]`, `H=cblack[5]`, guarding `W==0||H==0`.
- **Why:** The repeating 2-D black pattern is where several cameras store their pedestal (X-Trans entirely; some Bayer bodies). Ignoring it leaves raised, tinted shadows and a non-zero black floor. Index 4 is currently mis-used as a "G2 black."
- **Action:** Widen `Metadata.cblack` storage, extend `read_metadata` (`lib.rs:520-522`) to copy `cblack[0..6]` + the `W*H` pattern, and update the per-pixel black computation in `normalized` (`lib.rs:164-165`).
- **Note:** Lays groundwork for correct X-Trans black if X-Trans demosaic is ever added — but X-Trans is *rejected* at decode for now (see 2.8b).

### 2.2 — White level · **Use per-channel `linear_max`** *(H1, High)*
- **Decision:** Read `color.linear_max[4]`; where `linear_max[c] != 0`, use it as the per-channel white for **both** the clip mask and the normalization denominator. Keep `color.maximum` as the fallback.
- **Why:** The single scalar `maximum` is a guessed value that can be miscalibrated and cause magenta blown highlights; LibRaw's own remedy is per-channel `linear_max`, available after `unpack()`.
- **Action:** Extend `read_metadata`; make `clip_mask` (`lib.rs:181-186`) per-CFA-channel; thread per-channel white into `normalized` (`lib.rs:166`).
- **Enables:** trustworthy `peak` in `reconstruct_highlights` (2.7 / N2), and the consistent per-plane scale in 2.3.

### 2.3 — Normalization denominator · **Use a consistent per-plane scale** *(M1, Medium)*
- **Decision:** Stop giving each channel a different gain via its own `(white − black_c)`. Adopt DNG's single per-plane scale, or — now that 2.2 lands — use `(linear_max[c] − black_c)` **consistently** so any per-channel normalization is an intentional, colorimetric choice rather than a side effect of per-channel black.
- **Why:** The current per-channel denominator introduces a faint mid-tone tint WB doesn't cancel; small today but real, and grows with large per-channel black offsets.
- **Action:** Edit `normalized` (`lib.rs:166-169`); coordinate with 2.1 (black) and 2.2 (white) so the chosen scale is coherent.

### 2.4 — WB before demosaic · **Keep as-is (verified correct)**
- No change. Matches darktable/dcraw; a linear per-channel gain doesn't violate MHC's gradient assumptions.

### 2.5 — MHC kernels · **Keep as-is (verified correct)**
- No change. All coefficients, the 1/8 normalization, and ROW/COL assignment match ICASSP-2004 Fig. 2 element-by-element.

### 2.6 — MHC border · **Mirror/clamp for edge MHC** *(L2, Low)*
- **Decision:** Replace the 2-px bilinear border fallback with a mirrored/clamped 5×5 window so the sharper MHC filter runs to the very edge.
- **Why:** Marginal sharpness gain on the 2-px frame; cosmetic but cheap.
- **Action:** Edit the border branch in `demosaic_mhc` (`lib.rs:353-357`) to mirror/clamp out-of-bounds taps instead of dropping to `bilinear_pixel`.

### 2.7 — Highlight reconstruction · **Also add spatial propagation** *(L1 Low + enhancement)*
- **Decision:** Keep the conservative "rebuild ≥2 clipped channels to peak, keep measured channels" core, **and**: (a) fix L1 by propagating the clip flag over the **5×5** MHC support (not 3×3); (b) add a **spatial color-propagation** pass (LCH-blend or guided propagation) so large blown regions recover structure instead of going flat at `peak`.
- **Why:** The current method fixes magenta neutrals and preserves genuine single-channel colors, but can't recover texture/gradient in big blown areas, and the 3×3 mask leaves a faint ring at the edge of MHC-reconstructed regions.
- **Action:** Update `clipped_channels` (`lib.rs:374-386`) to 5×5 for the MHC path; add a new reconstruction stage after the per-pixel rebuild. **Depends on 2.2** (peak/white-level accuracy bounds quality). This is the largest single item in this register (a real feature).

### 2.8a — `channel_at` mapping · **Keep as-is (verified correct)**
- No change. Correct for all 4 colors and all 4 Bayer phases.

### 2.8b — `is_rgb_bayer` guard · **Check `filters`/`colors`, reject X-Trans** *(H2, High)*
- **Decision:** Read `idata.filters` and `idata.colors`; accept only standard 2×2 Bayer (e.g. `colors == 3 && filters != 0 && filters != 9`). Reject X-Trans (`filters==9`), Foveon/non-Bayer/linear (`filters==0`), and non-3-color CFAs **explicitly**.
- **Why:** `cdesc=="RGBG"` is true for every RGB sensor incl. X-Trans, so an X-Trans `.RAF` currently passes and is mis-demosaiced as Bayer (scrambled). This also closes the related code-review finding (Foveon `filters==0` → OOB panic).
- **Action:** Extend `read_metadata` to capture `filters`/`colors`; rewrite `is_rgb_bayer` (`lib.rs:435-437`) and the gate at `lib.rs:475-477`.

### 2.9 — Bilinear demosaic · **Keep as-is (verified correct)**
- No change. Textbook, border-safe, exact on smooth content.

### 2.10 — FFI / unpack · **Keep as-is (verified correct, numeric scope)**
- No change *here*. Numeric/decode lifecycle is correct. The engineering issues in this FFI (`raw_pitch`, Foveon panic) are tracked in the code-review decision pass, and the Foveon panic is also addressed structurally by 2.8b's `filters` guard.

---

## Resulting implementation plan (derived from the decisions)

Level metadata first (they interlock), then the demosaic/highlight upgrades:

1. **Extend `read_metadata`** to capture: full `cblack[]` (pattern), `linear_max[4]`, `filters`, `colors`. One pass enables 2.1, 2.2, 2.8b.
2. **2.8b** — tighten the sensor guard (`filters`/`colors`); reject X-Trans/Foveon/non-Bayer explicitly (also fixes the Foveon panic).
3. **2.1 + 2.2 + 2.3** — rework `normalized`/`clip_mask` for per-pixel black (pattern), per-channel white (`linear_max`), and a consistent per-plane/`linear_max` scale.
4. **2.6** — mirror/clamp MHC border.
5. **2.7** — 5×5 clip propagation (L1) + a spatial color-propagation highlight stage (depends on the corrected white level from step 3).
6. **No-ops:** 2.4, 2.5, 2.8a, 2.9, 2.10 confirmed correct — leave unchanged; keep their guarding tests.

**Cross-cutting reminders:**
- Add/extend unit tests: `cblack` pattern folding, per-channel `linear_max` clip/normalize, the X-Trans/Foveon rejection path, mirrored MHC border, and the new highlight propagation.
- The X-Trans *black* (2.1) is groundwork only; X-Trans is rejected at decode (2.8b) until a 6×6 X-Trans demosaic exists.
- This register reflects intent only; nothing here has been implemented.
