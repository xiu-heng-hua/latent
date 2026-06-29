# Decision Register — Audit 05 (Geometry & Optics)

**Source audit:** [`../image-processing/05-geometry-and-optics.md`](../image-processing/05-geometry-and-optics.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Each §2 point reviewed interactively, in document order (2.1→2.9), plus the N1 feature note; multi-finding points split into their distinct decisions.
**Revision 2026-06-27 (consistency pass):** 2.7 upgraded *prefilter-only (kept bilinear) → prefilter + higher-order (Lanczos/bicubic) interpolation*; 2.9 updated so the GPU warp matches that interpolation. See [`README.md`](README.md) → *Consistency reconciliation*.

---

## Decision summary

| Point | Topic (file) | Finding · Severity | Decision | Outcome |
|---|---|---|---|---|
| **2.1** | Radial distortion model (`pipeline:264`) | N2 · Correct | **Keep as-is (verified correct)** | No change |
| **2.2** | Distortion direction (`pipeline:264`) | M1 · Medium | **Newton-invert for lensfun POLY** | Change |
| **2.3** | Radius normalization unit (`pipeline:675`) | C1 · Critical | **Switch to lensfun half-diagonal norm** | Change |
| **2.3-L1** | Optical-center offset (`lens:142`) | L1 · Low | **Scale center offset to match lensfun** | Change |
| **2.4** | Lateral CA / TCA (`pipeline:744`) | — · Correct-w-caveats | **Add POLY3 radial TCA** | Change |
| **2.5** | Keystone homography (`pipeline:652`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.6** | Rotation/straighten bbox (`pipeline:110`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.7** | Resampling: prefilter + interpolation (`cpu:273`/gpu) | M3 · Medium | **Prefilter + higher-order (Lanczos) interp** | Change |
| **2.8** | Vignetting divide-vs-multiply (`pipeline:705`) | M2 · Medium | **Verify lensfun PA convention & align** | Change (verify) |
| **2.9** | GPU resample vs CPU (`resample.wgsl`) | H1 · High | **Add w≤0 guard + warp.wgsl** | Change |
| **N1** | Auto-scale-to-fill (`pipeline:687`) | N1 · Note | **Add auto-scale-to-fill option** | Change (feature) |

**Tally:** 8 changes (incl. 1 feature), 3 keep-as-is.

> **Overarching theme — make the lens stack fully lensfun-faithful and close the GPU gap.** Decisions 2.2 (Newton inversion), 2.3-C1 (half-diagonal normalization), 2.3-L1 (center scaling), 2.4 (full POLY3 TCA), and 2.8 (PA convention) together make `latent` apply lensfun-database coefficients *exactly* as lensfun does — fixing the headline Critical mis-scaling. 2.9 brings the GPU backend to parity (guard + warp shader), and 2.7 adds the missing minification prefilter **and upgrades the interpolator to higher-order (Lanczos/bicubic)**. The `lens_radial`/`Warp` math is where most of this lands.

---

## Per-point decisions

### 2.1 — Distortion model · **Keep as-is (verified correct)**
- No change. The `s(r)` polynomial is a correct superset of Brown–Conrady + PanoTools; the POLY3/POLY5/PTLENS→`[d0..d3]` mapping is right; magnification-factoring (N2) is intentional and documented.

### 2.2 — Distortion direction · **Newton-invert for lensfun POLY** *(M1, Medium)*
- **Decision:** For lensfun POLY3/POLY5 coefficients, solve `Rd = Ru(1+…)` for `Ru` by 2–3 Newton iterations (matching lensfun exactly); keep the direct `s(r)` multiply for PTLens (where it's the defined operation).
- **Why:** The direct multiply is only first-order-correct for lensfun POLY models (sub-pixel, but not bit-exact).
- **Action:** Add a model-aware inversion path in `Warp::map` / the lens lowering (`pipeline:264-267`, `lens:183`). Pairs with 2.3 (correct normalization first, or the inversion operates at the wrong scale).

### 2.3 — Radius normalization · **Switch to lensfun half-diagonal norm** *(C1, CRITICAL)*
- **Decision:** Normalize the radius by the **focal-scaled half-diagonal** (incorporating `RealFocal`/`Crop`, per lensfun's `NormScale`) instead of `2/min(w,h)`, so lensfun-database coefficients apply at the correct radial scale.
- **Why:** `2/min(w,h)` is the PanoTools convention, not lensfun's; every lensfun coefficient is currently applied off by ~1.3–1.8× per even order (aspect-dependent), worsening toward the corners. The "matching lensfun" doc comment is false.
- **Action:** Rewrite `lens_radial` (`pipeline:672-677`); thread `RealFocal`/`Crop` from the lensfun profile through `latent-lens`. Fix the doc at `latent-edit:528`. **Add a round-trip test** straightening a known-distorted grid with a real lensfun `<distortion>` entry vs lensfun's own output.

### 2.3-L1 — Optical-center offset · **Scale center offset to match lensfun** *(L1, Low)*
- **Decision:** Scale the center offset by `min(w,h)/2` (and the chosen normalization) so off-center calibrations are correct, instead of treating `CenterX/Y` as a fraction of the full dimension.
- **Action:** Fix `center` mapping in `latent-lens:142`. Also closes code-review 04 **M3**. Pairs with 2.3-C1.

### 2.4 — Lateral CA · **Add POLY3 radial TCA** *(Correct-with-caveats → upgrade)*
- **Decision:** Implement the full lensfun **POLY3** per-channel radial TCA scale (radius-dependent), not just the on-axis constant term.
- **Why:** Reducing POLY3 to its constant leaves residual CA on lenses with strong radius-dependent CA.
- **Action:** Extend `Warp::map_channel` (`pipeline:272`) and `ca_offsets` (`lens:212`) to carry per-channel radial polynomials; keep green as reference (correct). Keeps the LINEAR model as the degenerate case.

### 2.5 — Keystone · **Keep as-is (verified correct)**
- No change. Valid center-preserving projective transform; correct behind-plane guard.

### 2.6 — Rotation/straighten · **Keep as-is (verified correct)**
- No change. Standard rotated-bbox expansion and center-preserving inverse-rotation map.

### 2.7 — Resampling: prefilter + interpolation · **Prefilter + higher-order (Lanczos) interp** *(M3, Medium; revised — higher-quality option)*
- **Decision:** Do **both**: (a) upgrade the interpolator from 2-tap **bilinear** to a **higher-order kernel (Lanczos / bicubic)** for sharper magnification and general resampling quality, and (b) add a **prefilter** for the locally-minifying parts of the warp (MIP/trilinear, EWA, or a box sized by the local map Jacobian) so distortion/keystone corners don't alias.
- **Why:** Bilinear is the soft, low-quality interpolator; Lanczos/bicubic is standard in quality RAW developers (RawTherapee/darktable). Higher-order interpolation alone does **not** fix minification aliasing — hence the prefilter too. (Earlier decision was prefilter-only, keeping bilinear; revised to also upgrade interpolation.)
- **Action:** Replace the bilinear sampler in `warp`/`resample` (`latent-cpu:273`) with Lanczos/bicubic, and add the minification prefilter; mirror in the GPU resampler/warp (with 2.9). Coordinates with **Audit 04 2.6** (output sharpening — see T4: Lanczos reduces the softness that pass was compensating for) and **N1** (auto-scale). (Single-resample and zero-border remain as-is — verified correct; linear-light sampling unchanged.)

### 2.8 — Vignetting convention · **Verify lensfun PA convention & align** *(M2, Medium)*
- **Decision:** Confirm against lensfun's vignetting apply code whether the stored PA terms are the **falloff** (current `divide` is right) or the **correction gain** (then `latent` double-inverts), and align the divide/multiply and sign exactly.
- **Action:** Read lensfun's `ModifyColor`/vignetting code; adjust `RadialGain { reciprocal }` usage (`pipeline:705`) / `vignetting_falloff` (`lens:225`) if needed. Add a flat-field round-trip test vs a known lensfun vignetting entry. (Model family, sign, and pre-resample placement are already correct.)

### 2.9 — GPU resample · **Add w≤0 guard + warp.wgsl** *(H1, High)*
- **Decision:** Add the `w≤0` guard to `resample.wgsl`, and write a `warp.wgsl` mirroring `Warp::map`/`map_channel` (radial Horner + per-channel CA), so the full geometry runs on GPU and matches CPU.
- **Why:** The shader currently diverges from CPU at extreme-keystone corners (samples real pixels/NaN instead of black) and has no warp path (lens distortion/CA is CPU-only). Same as code-review 03 **F1**.
- **Action:** Edit `latent-gpu/src/resample.wgsl`; add `warp.wgsl` + dispatch; **match the CPU's higher-order (Lanczos/bicubic) interpolation and prefilter from 2.7** so the backends agree; add a CPU/GPU equivalence test over perspective + distortion (the existing end-to-end test never exercises the resample shader because it always routes through `warp`/CPU). Subsumes the minor L2 sentinel nit.

### N1 — Auto-scale-to-fill · **Add auto-scale-to-fill option** *(feature)*
- **Decision:** Add an optional auto-scale-to-fill (compute the fill scale à la lensfun `GetAutoScale`) so distortion/keystone/straighten don't leave black border wedges.
- **Action:** Add a `GetAutoScale`-equivalent in the geometry stage (`apply_geometry`, `pipeline:687`) and expose it as a toggle. Interacts with 2.7 (the auto-scale may *minify*, so the prefilter must cover it) and crop.

---

## Resulting implementation plan (derived from the decisions)

The lens-faithfulness items share `lens_radial`/`Warp`, so do them together:

1. **Thread lensfun geometry through `latent-lens`** — `RealFocal`, `Crop`, model type, full POLY3 TCA, center offset. Foundation for 2.2/2.3/2.3-L1/2.4.
2. **2.3-C1 + 2.3-L1** — half-diagonal (focal-scaled) normalization and correct center scaling in `lens_radial`; fix the false doc; add the lensfun round-trip test.
3. **2.2** — model-aware Newton inversion for POLY3/POLY5 (after the scale is correct).
4. **2.4** — full POLY3 radial TCA in `Warp::map_channel`.
5. **2.8** — verify and align the vignetting PA convention; add the flat-field test.
6. **2.7 + 2.9** — upgrade the interpolator to higher-order (Lanczos/bicubic) and add the minification prefilter (CPU + GPU), and bring the GPU to parity (w≤0 guard + `warp.wgsl` + equivalence test). Do these together so CPU/GPU stay in lockstep.
7. **N1** — auto-scale-to-fill option (uses the prefilter from 2.7).
8. **No-ops:** 2.1, 2.5, 2.6 confirmed correct — leave unchanged; keep their tests.

**Cross-cutting reminders:**
- 2.7/2.9/N1 and **Audit 04 2.6** (output sharpening) all touch minification/resampling — converge them so the higher-order interpolation + prefilter is implemented once and downscale is handled in exactly one place, on both backends.
- Every lens change needs a round-trip/equivalence test against real lensfun output (distortion grid, TCA target, vignetting flat-field) and a CPU/GPU parity test.
- This register reflects intent only; nothing here has been implemented.
