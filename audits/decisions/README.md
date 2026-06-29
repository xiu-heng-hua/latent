# Decision Registers — Image-Processing Audits

This folder records, point-by-point, what to **change** vs **keep as-is** for every finding in the five image-processing audits ([`../image-processing/`](../image-processing/)). Each decision was made interactively, in each audit's document order, and captures the rationale, the concrete action, and cross-dependencies.

**These registers record intent only — no source code has been modified.**

| Register | Source audit | Points | Changes | Keep |
|---|---|---|---|---|
| [`01-color-science.md`](01-color-science.md) | Color science | 10 | 7 | 3 |
| [`02-raw-decode-and-demosaic.md`](02-raw-decode-and-demosaic.md) | RAW decode & demosaic | 11 | 6 | 5 |
| [`03-tone-and-color-grading.md`](03-tone-and-color-grading.md) | Tone & color grading | 9 | 8 | 1 |
| [`04-spatial-and-frequency.md`](04-spatial-and-frequency.md) | Spatial & frequency | 9 | 8 | 1 |
| [`05-geometry-and-optics.md`](05-geometry-and-optics.md) | Geometry & optics | 11 | 8 | 3 |
| **Total** | | **50** | **37** | **13** |

The overall posture was **"make it correct / standard-conformant"**: nearly every actionable finding was accepted for fixing, and several were upgraded to the more thorough option (full perceptual spaces, full He et al. dehaze, full lensfun fidelity, full POLY3 TCA, GPU parity, auto-scale-to-fill).

> Scope note: the four **code-review** documents are a separate track, now triaged in [`code-review/`](code-review/) (~48 Fix / ~7 Keep, autonomous). Four of their findings are remedied by image-processing decisions and cross-referenced there. The combined implementation plan for **both** tracks lives in [`../kanban/`](../kanban/).

---

## Cross-cutting initiatives

Most of the 37 changes are not independent — they cluster into a handful of larger efforts that share infrastructure. Build the shared pieces once.

### A. Perceptual color core *(the biggest thread)*
A new **CIE Lab/LCh module** (D50-referenced) and a **Bradford chromatic-adaptation** path underpin a large fraction of the decisions:
- **01**: #2 D50 ProPhoto working space, #4a full DNG model, #9 Bradford CA, #1 full-precision sRGB matrix, #4b drop row-norm.
- **03**: 2.1 L\* tone domain, 2.5 LCh HSL mixer, 2.7 chroma-preserving saturation, 2.8 monotone-cubic curves.
- **04**: 2.1 perceptual-domain (L\*) luma sharpen, 2.4b perceptual chroma denoise metric.
- **01 #5**: perceptual lightness = **L\*** (a single shared definition — reconciled from a Rec.709 luma; see *Consistency reconciliation* below).

→ **Lab/LCh is natively D50**, so it falls out of 01 #2 cleanly. Do the color core (01) and the Lab/LCh module first; 03 and 04's perceptual items follow.

### B. Sensor-metadata correctness (decode)
One extended `read_metadata` pass in `latent-raw` enables several **02** fixes at once: full `cblack` pattern (2.1), per-channel `linear_max` (2.2), consistent rescale (2.3), and the `filters`/`colors` sensor guard (2.8b, which also fixes the code-review Foveon panic).

### C. Lensfun faithfulness (lens stack)
Thread real lensfun geometry (`RealFocal`, `Crop`, model type, POLY3 TCA, center offset) through `latent-lens`, then fix `lens_radial`/`Warp`: **05** half-diagonal normalization (2.3-C1, the Critical), center scaling (2.3-L1), Newton inversion (2.2), full POLY3 TCA (2.4), and the PA vignetting convention (2.8).

### D. Dehaze overhaul → full He et al.
**04** 2.5a (estimate airlight A), 2.5b (guided-filter transmission refinement), 2.5c (resolution-scaled patch) together rebuild dehaze into the complete published method. The guided filter is a reusable O(N) primitive.

### E. Resampling / minification convergence
**04** 2.6 (output-sharpening pass) + **05** 2.7 (**higher-order Lanczos/bicubic interpolation + minification prefilter**), 2.9 (GPU parity: w≤0 guard + `warp.wgsl`), and N1 (auto-scale-to-fill) all touch downscale/resampling. Converge them so the interpolator + prefilter is implemented once, on both backends.

### F. Robustness / sanitize-on-load
**03** 2.2 replaces `contrast` with an always-monotone S-curve (no inversion at any amount) and still sanitizes `SelectiveTone` on load for NaN/inf — the same "sanitize sidecar values on load" theme the code-review track raises (NaN/inf curve points, out-of-range opacity).

### G. CPU ↔ GPU lockstep *(continuous constraint)*
Many changes touch the WGSL shaders (tone headroom 03 2.3, luma weights, saturation 03 2.7, resample/warp 05 2.9, bilateral 04 2.4a). The GPU/CPU render-equivalence test must be kept green throughout.

---

## Consistency reconciliation (2026-06-27)

A cross-register consistency pass confirmed **no hard contradictions** and that the decisions optimize for quality consistently. It applied four quality upgrades and resolved five overlaps so two decisions never fight:

**Quality upgrades** (each replaced a lighter pick with the higher-quality one):
- **03 2.2** — *clamp* → **always-monotone S-curve** (fixes the inversion *and* keeps the tool's full range).
- **03 2.8** — *keep C0 point curves* → **monotone-cubic (PCHIP)** interpolation (smooth, no overshoot).
- **04 2.1** — *luma-only in linear light* → **luma sharpen in the L\* perceptual domain** (fixes both color fringing *and* the perceptual overshoot asymmetry).
- **05 2.7** — *prefilter only (kept bilinear)* → **prefilter + higher-order (Lanczos/bicubic) interpolation**.

**Reconciliations:**
- **T1 — one perceptual lightness.** 01 #5's *Rec.709 luma* is dropped in favour of **CIE L\*** from the shared Lab pipeline, so the whole grading core uses a single perceptual-lightness definition (clarity gate, denoise weighting, luma sharpen).
- **T2 — saturation decided once.** The LCh chroma-preserving saturation (03 2.7) *supersedes* the luma-blend; the "Rec.709 luma for saturation" idea is gone.
- **T3 — HSV clamp is conditional.** 03 2.6's sector clamp applies only if the HSV path survives 03 2.5's LCh reimplementation; otherwise it retires with HSV.
- **T4 — sharpen vs interpolation.** With 05 2.7 now using Lanczos/bicubic, the 04 2.6 output-sharpening pass compensates for *minification aliasing*, not bilinear softness.
- **T5 — minification handled once.** 04 2.6 + 05 2.7/2.9/N1 converge on a single higher-order interpolation + prefilter path across CPU/GPU (see Initiative E).

These changes are reflected in the individual registers (each carries a *Revision 2026-06-27* note).

---

## Suggested global build order

1. **Color core + Lab/LCh module + Bradford CA** (Initiative A; Register 01, then 03/04's perceptual items). Settles the reference white and the perceptual spaces everything else builds on.
2. **Sensor metadata** (Initiative B; Register 02) — independent, can run in parallel.
3. **Lensfun stack** (Initiative C; Register 05 lens items) — independent of A/B.
4. **Dehaze overhaul** (Initiative D; Register 04 dehaze) — uses the guided filter.
5. **Resampling/minification + GPU parity** (Initiative E; Registers 04 2.6, 05 2.7/2.9/N1) — converge last so all minifying paths share one prefilter.
6. **Robustness sanitize-on-load** (Initiative F) — small, do alongside the color/tone work.
7. **Keep-as-is points** (13 total) — leave unchanged; preserve their guarding unit tests, and re-verify the few whose domain changes (e.g. 03 2.4 LUT error re-checked in L\*).

Every change should land with a test that pins the new behavior (lensfun round-trips, colored-veil dehaze, monotonicity/headroom in L\*, CPU/GPU parity over perspective+distortion, etc.).
