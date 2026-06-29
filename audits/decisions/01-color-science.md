# Decision Register — Audit 01 (Color Science)

**Source audit:** [`../image-processing/01-color-science.md`](../image-processing/01-color-science.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Each point of §2 of the audit was reviewed interactively, in document order, with an explicit accept/modify/keep choice.
**Revision 2026-06-27 (consistency pass):** #5 reconciled from a Rec.709 luma to **CIE L\*** — a single perceptual-lightness definition shared with Audits 03–04. See [`README.md`](README.md) → *Consistency reconciliation*.

This file records, for every point of the color-science audit, whether the software will be changed or kept as-is, the rationale, and the concrete action implied. It does **not** itself modify any source.

---

## Decision summary

| Point | Topic (file) | Finding · Severity | Decision | Outcome |
|---|---|---|---|---|
| **#1** | XYZ→linear-sRGB matrix (`color.rs:89-93`) | F-3 · Low | **Use full-precision matrix** | Change |
| **#2** | Working space = ROMM primaries @ D65 (`color.rs:125-136`) | F-2 · Medium | **Switch to standard D50 ProPhoto** | Change (redesign) |
| **#3** | `rgb_to_xyz` construction (`color.rs:107-123`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **#4a** | `camera_to_working` row-normalization (`color.rs:166-169`) | F-1 · High | **Adopt full DNG model** | Change (redesign) |
| **#4b** | `working→sRGB` row-normalization (`color.rs:152-156`) | F-4 · Low | **Drop row-norm (after #1 fix)** | Change |
| **#5** | `LUMA_WEIGHTS` for perceptual ops (`color.rs:181`) | F-5 · Medium | **Perceptual lightness (L\*) for perceptual ops** | Change |
| **#6** | sRGB OETF constants (`export/lib.rs:14-29`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **#7** | `highlight_rolloff` per-channel (`export/lib.rs:42-59`) | F-6 · Low | **Roll off on max/luminance** | Change |
| **#8** | DNG XYZ→camera direction (`color.rs:82-84`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **#9** | Chromatic adaptation omitted | — · Correct* | **Add Bradford CA (follows #2/#4a)** | Change (consequence) |

\* Point #9 verified *correct only for the original all-D65 design*; the #2/#4a decisions change that premise (see below).

**Tally:** 7 changes, 3 keep-as-is. Note that #2, #4a, and #9 are mutually reinforcing and amount to a **deliberate redesign of the color core** toward a standard DNG/ICC-style pipeline (D50 reference white + explicit Bradford chromatic adaptation), rather than three independent tweaks.

---

## Per-point decisions

### #1 — XYZ→linear-sRGB matrix · **Use full-precision matrix** *(was F-3, Low)*
- **Decision:** Replace the IEC 4-decimal ("8-bit grade") `XYZ_TO_LINEAR_SRGB` constant with the full-precision matrix — either the IEC-2003 7-decimal matrix or one derived from the Rec.709 primaries via the same `rgb_to_xyz` used for the working space.
- **Why:** The 4-decimal matrix is off by up to `3.7e-4` (~24 of a 16-bit code) and does not map D65 white exactly to neutral; `latent` exports true 16-bit, so the precision matters. It is also the root cause of the #4b row-normalization.
- **Action:** Edit `latent-image/src/color.rs:89-93`. Prefer deriving from primaries (single source of truth, consistent with the working-space construction).
- **Enables:** #4b (drop the working→sRGB row-normalization).

### #2 — Working space · **Switch to standard D50 ProPhoto** *(was F-2, Medium)*
- **Decision:** Use real ROMM/ProPhoto RGB at its standard **D50** reference white instead of ROMM primaries pinned to D65, and add the chromatic adaptation this requires.
- **Why:** The current "ROMM primaries at D65" space has no ICC/ISO identity and is easily confused with ProPhoto. Moving to the standard D50 space makes the working space interoperable and well-defined.
- **Action:** Change `D65_WHITE`→`D50_WHITE` for the working-space construction in `latent-image/src/color.rs:125-136`; update `linear_working_to_xyz`/`xyz_to_linear_working`, `LUMA_WEIGHTS` (Y row will change to the D50 ProPhoto value), and `linear_working_to_linear_srgb` (now D50-working → D65-sRGB, requiring adaptation — see #9). Rename comments accordingly.
- **Trade-off accepted:** Re-introduces chromatic adaptation into the pipeline (the whole point of the original D65 choice was to avoid it). Coupled with #4a and #9.

### #3 — `rgb_to_xyz` construction · **Keep as-is (verified correct)**
- **Decision:** No change. The primaries-as-columns-scaled-to-white construction matches SMPTE RP 177 / Lindbloom and round-trips to `<1e-5`.
- **Note:** This routine is reused by #1 (deriving the sRGB matrix from primaries) and #2 (rebuilding the working matrix at D50), so it stays central.

### #4a — `camera_to_working` row-normalization · **Adopt full DNG model** *(was F-1, High)*
- **Decision:** Replace the "mosaic `cam_mul` + row-normalize the matrix" approach with the full Adobe DNG color model: inverse-`ColorMatrix` (camera→XYZ) plus a **Bradford** chromatic adaptation from the white-balance illuminant to the reference white (the DNG `ForwardMatrix`-equivalent path, with WB applied by scaling camera coordinates).
- **Why:** Row-normalization keeps neutrals correct but is *not* the reference operator; saturated colors drift up to **0.28 in linear working RGB** for a real Canon profile (visible, camera/WB-dependent). The DNG model renders chromatic colors correctly.
- **Action:** Rework `camera_to_working` (`color.rs:152-169`) and the WB handling in `latent-raw/src/lib.rs` (`apply_white_balance` / `color_matrix`, and the decode order in `latent-app/src/main.rs:63-70`). Remove the misleading "double-WB fix" comment at `color.rs:164-165`.
- **Depends on / pairs with:** #2 (reference white now D50) and #9 (Bradford CA must be implemented).
- **Severity:** Highest-impact change in this register.

### #4b — `working→sRGB` row-normalization · **Drop row-norm (after #1 fix)** *(was F-4, Low)*
- **Decision:** Delete the row-normalization in `linear_working_to_linear_srgb`; once #1 supplies the full-precision sRGB matrix, the working→sRGB product has unit row sums natively.
- **Why:** The row-norm was masking the rounded-matrix tint rather than fixing it, and its docstring understated the chromatic shift (~1e-4 claimed vs 3.25e-4 measured).
- **Action:** Edit `color.rs:152-156` and its docstring. **Sequencing:** do #1 first. **Interaction:** with #2, this matrix is now D50-working → D65-sRGB and must include the Bradford adaptation (per #9), so "drop row-norm" means "replace the row-norm hack with the correct adapted matrix," not "just remove a line."

### #5 — Perceptual lightness · **Perceptual lightness (L\*) for perceptual ops** *(was F-5, Medium; revised — see reconciliation)*
- **Decision:** Keep the working-space `LUMA_WEIGHTS` for *colorimetric* uses (exposure / relative luminance), but use **CIE L\*** (the Lab lightness from the shared Lab/LCh module) as the perceptual lightness wherever a *perceptual* operation needs one — clarity's midtone gate, denoise luma weighting, and the perceptual-domain luma sharpen (Audit-04 2.1). Saturation no longer uses a luma at all: it becomes LCh chroma scaling (Audit-03 2.7).
- **Why:** The working-space blue weight (~0.0001, smaller still at D50 ProPhoto) makes blue vanish under perceptual ops. **Reconciliation (T1):** the original choice here was a Rec.709 luma, but once Audits 03/04 move the perceptual operations into Lab/LCh, the natural perceptual lightness is **L\*** from that same Lab pipeline — not a separate Rec.709 luma. Using L\* keeps one perceptual-lightness definition across the whole grading core.
- **Action:** Expose `L*` from the Lab/LCh module (built for Audit-03) and route clarity / denoise-weighting / luma-sharpen to it in `latent-pipeline`/`latent-cpu`/`latent-gpu`; drop the planned separate Rec.709 luma. **Keep the CPU and GPU paths in sync** (the GPU shader hard-codes the luma triple — `color.rs:173-175` note).

### #6 — sRGB OETF · **Keep as-is (verified correct)**
- **Decision:** No change. All constants match IEC 61966-2-1 verbatim; endpoints fixed; round-trip passes.

### #7 — Highlight rolloff · **Roll off on max/luminance** *(was F-6, Low)*
- **Decision:** Change `highlight_rolloff` from independent per-channel compression to a hue-preserving form: compute one compression factor from the max channel (or luminance) and scale the RGB triplet by it. Keep the 0.98 knee (that part was correct-by-design).
- **Why:** Per-channel rolloff compresses only the hot channel of a near-clipped color, shifting hue toward the secondaries in extreme highlights.
- **Action:** Edit `to_display`/`highlight_rolloff` in `latent-export/src/lib.rs:42-59` to apply a shared factor; update the pinned-value tests accordingly.

### #8 — DNG direction convention · **Keep as-is (verified correct)**
- **Decision:** No change. `cam_xyz` is correctly treated as XYZ→camera and inverted; top-3-rows usage is fine for the RGBG-Bayer-only decoder.

### #9 — Chromatic adaptation · **Add Bradford CA (follows #2/#4a)** *(point verified correct only for the old all-D65 design)*
- **Decision:** Implement a Bradford chromatic adaptation. This is the **direct consequence** of #2 and #4a: once the working white is D50 and WB is handled by the DNG model, adaptation is no longer the identity and must be performed explicitly — (a) from the capture/WB illuminant to the reference white in `camera_to_working` (#4a), and (b) from D50-working to D65-sRGB in the output transform (#4b/#2).
- **Why:** Omitting CA was only valid because the original design pinned every stage to D65. The chosen #2/#4a path breaks that premise; without CA a white-point error would be introduced.
- **Action:** Add a Bradford CA helper (the standard cone-response matrix) in `latent-image/src/color.rs`, and wire it into both the camera→working build (#4a) and the working→sRGB output matrix (#2/#4b).

---

## Resulting implementation plan (derived from the decisions)

The changes are interdependent; a sensible order:

1. **Add a Bradford CA helper** (`color.rs`) — needed by #2, #4a, #4b, #9.
2. **#1** — derive the full-precision sRGB matrix from Rec.709 primaries.
3. **#2** — rebuild the working space at standard D50 ProPhoto; update `LUMA_WEIGHTS` (colorimetric) and dependent matrices.
4. **#9 + #4b** — make `linear_working_to_linear_srgb` the correct D50-working→D65-sRGB adapted matrix (full precision, no row-norm hack).
5. **#4a** — replace the camera→working WB/row-norm with the DNG model (inverse-ColorMatrix + Bradford from WB illuminant to D50); fix the misleading comment; revisit `apply_white_balance`/decode order in `latent-raw`/`latent-app`.
6. **#5** — expose **L\*** from the Lab/LCh module and route clarity / denoise-weighting / luma-sharpen to it (saturation uses LCh, not luma); CPU **and** GPU shader in sync. *(Reconciled from Rec.709 luma to L\* — T1.)*
7. **#7** — switch `highlight_rolloff` to a hue-preserving max/luminance form; update tests.
8. **No-ops:** #3, #6, #8 confirmed correct — leave unchanged (and keep their guarding unit tests).

**Cross-cutting reminders:**
- Several changes touch constants duplicated in the **GPU WGSL shaders** (luma weights, any output-transform math) — keep CPU/GPU in sync (the GPU/CPU render-equivalence test guards this).
- Update the affected **unit tests** (`working_space_white_is_neutral_and_round_trips`, `luma_weights_match_the_working_matrix`, the export pinned-value tests) to the new D50 / full-precision / L\* expectations.
- This register reflects intent only; nothing here has been implemented.
