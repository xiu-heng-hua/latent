# Decision Register — Audit 04 (Spatial & Frequency-Domain Filters)

**Source audit:** [`../image-processing/04-spatial-and-frequency.md`](../image-processing/04-spatial-and-frequency.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Each §2 point reviewed interactively, in document order (2.1→2.6); the multi-finding points (2.4 bilateral, 2.5 dehaze) were split into their distinct decisions.
**Revision 2026-06-27 (consistency pass):** 2.1 upgraded *luma-only-in-linear → luma sharpen in the L\* perceptual domain* (addresses both halves of N1); perceptual-lightness references reconciled to **L\*** (T1). See [`README.md`](README.md) → *Consistency reconciliation*.

---

## Decision summary

| Point | Topic (file) | Finding · Severity | Decision | Outcome |
|---|---|---|---|---|
| **2.1** | Unsharp mask domain (`pipeline:438`) | N1 · Note | **Luma sharpen in a perceptual domain** | Change |
| **2.2** | Radius/round semantics (`cpu:68,163`) | M3 · Medium | **Unify radius semantics** | Change |
| **2.3** | Clarity 3-box + midtone (`pipeline:421-456`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.4a** | Bilateral ±2σ truncation (`pipeline:528`) | H1 · High | **Use σ=r/3 (±3σ support)** | Change |
| **2.4b** | Denoise luma/chroma space (`pipeline:525`) | M1 · Medium | **Keep split, perceptual chroma metric** | Change |
| **2.5a** | Dehaze airlight A=1 (`pipeline:493`) | H2 · High | **Estimate airlight A** | Change |
| **2.5b** | Dehaze transmission refinement (`pipeline:475`) | H3 · High | **Add guided-filter refinement** | Change |
| **2.5c** | Dehaze patch size (`pipeline:468`) | M2 · Medium | **Scale patch with resolution** | Change |
| **2.6** | Sharpen at source res before geometry | M4 · Medium | **Add output-sharpening pass** | Change (architecture) |

**Tally:** 8 changes, 1 keep-as-is. The dehaze decisions (2.5a/b/c) together amount to **bringing dehaze up to the full He et al. method**; 2.6 adds a new post-geometry stage.

---

## Per-point decisions

### 2.1 — Unsharp domain · **Luma sharpen in a perceptual domain** *(N1; revised — higher-quality option)*
- **Decision:** Sharpen the **luminance** channel only **and** do the unsharp recombine in a **perceptual domain (L\*)**, recombining color around the sharpened lightness. This addresses *both* halves of N1: luma-only removes edge color fringing, and the perceptual domain makes the overshoot symmetric (no brighter dark-side halos that linear-light sharpening produces).
- **Why:** Per-channel linear-light unsharp both shifts edge hue (fringing) *and* produces perceptually asymmetric halos. The earlier decision (luma-only, still linear) fixed only the first; revised to luma-in-perceptual so sharpening is consistent with the rest of the perceptual grading core (and no longer the lone tool left in linear light).
- **Action:** Change the `sharpen` lowering / `Unsharp` recombine (`pipeline:438-440`, `cpu:79-86`) to compute the unsharp on **L\*** (from the shared Lab/LCh module, per Audit-01 #5 / Audit-03 2.1) and reconstruct color from the sharpened L\*. **Mirror in the GPU path.** The same perceptual-domain treatment applies to the output-sharpening pass (2.6).

### 2.2 — Radius semantics · **Unify radius semantics** *(M3)*
- **Decision:** Make the radius threshold/rounding consistent across `blur`, `denoise`, and `dehaze`, and tie the dehaze patch to a radius (pairs with 2.5c).
- **Action:** Reconcile the gate predicates (`pipeline:526`, `cpu:68,163`, dehaze patch) so "radius" means the same thing everywhere and there are no silent-identity surprises (e.g. blur radius 0.3).

### 2.3 — Clarity · **Keep as-is (verified correct)**
- No change. 3-box σ≈r and the midtone parabola peak (linear 0.2176 ≈ 18% gray) verified numerically.

### 2.4a — Bilateral support · **Use σ=r/3 (±3σ)** *(H1, High)*
- **Decision:** Set the spatial Gaussian σ_s = r/3 so the ±r window equals the standard ±3σ truncation (smooth falloff, no boundary step).
- **Why:** σ_s=r/2 truncates at ±2σ, a hard cutoff at e⁻²=0.135 dropping ~4.6%/axis of mass.
- **Action:** Change σ_s in `bilateral_pixel` (`pipeline:528-529`). Same tap count. **Mirror in any GPU bilateral** (currently CPU-only).

### 2.4b — Denoise chroma space · **Keep split, perceptual chroma metric** *(M1, Medium)*
- **Decision:** Keep the luma/chroma split (correct, intentional NR variant — chroma can smooth harder than luma) but measure the **chroma distance in a perceptual space (Lab/LCh)** instead of the linear-RGB-offset space; document it as a luma/chroma variant (not the T&M single-distance filter).
- **Why:** The current chroma metric is non-perceptual, and blue's luminance detail ends up governed by the chroma scale (L1).
- **Action:** Rework the chroma range term in `bilateral_pixel` to use the shared Lab/LCh path (same module as Audit-03 2.5/2.7). Keep the two-scale structure.

### 2.5a — Dehaze airlight · **Estimate airlight A** *(H2, High)*
- **Decision:** Estimate per-channel airlight A (≥ a cheap global percentile of the brightest dark-channel pixels, per He §4.3) and normalize I/A^c per channel before the dark channel and recovery.
- **Why:** Fixed A=1 can't neutralize colored haze and mis-scales strength when true airlight ≠ 1.
- **Action:** Add an airlight-estimation step feeding `dehaze_dark_channel`/`dehaze_recover` (`pipeline:475-505`); make recovery per-channel A. Changes the `dehaze` primitive's signature/data.

### 2.5b — Dehaze transmission · **Add guided-filter refinement** *(H3, High)*
- **Decision:** Refine the transmission map with an O(N) **guided filter** (guide = input luminance) before recovery, replacing the raw patch t.
- **Why:** Raw patch t → block/halo artifacts at depth edges (He §4.2).
- **Action:** Add a guided-filter pass (He & Sun, ECCV 2010) between transmission estimation and recovery. New building block (also reusable elsewhere).

### 2.5c — Dehaze patch · **Scale patch with resolution** *(M2, Medium)*
- **Decision:** Scale the dark-channel patch with image resolution (or expose it), targeting ≥ He's 15×15-equivalent at reference scale.
- **Why:** Fixed 9×9 on high-MP rasters sits in the small-patch over-saturation regime.
- **Action:** Replace the `DEHAZE_PATCH=4` constant (`pipeline:468`) with a resolution-derived size; ties to 2.2's unified radius.

### 2.6 — Output sharpening · **Add output-sharpening pass** *(M4, Medium)*
- **Decision:** Add a sharpening pass **after** geometry resample (output sharpening), so overshoots are created at output resolution and not aliased by the resampler. Uses the same **luma-in-perceptual-domain (L\*)** form as the capture sharpen (2.1).
- **Why:** Sharpening at source res then downscaling aliases the overshoots / wastes the sharpening.
- **Action:** Introduce a post-geometry sharpen stage in the pipeline (a new stage after `apply_geometry`), distinct from the existing capture-sharpen. Interacts with the geometry prefilter + higher-order interpolation decision (Audit 05 2.7) and the fixed pipeline order in `render`. **Note (T4):** with Audit-05 2.7 upgrading the resampler to higher-order (Lanczos/bicubic) interpolation, the output is sharper to begin with, so this pass compensates for *aliasing on minification*, not for bilinear softness.

---

## Resulting implementation plan (derived from the decisions)

1. **2.2** — unify radius semantics across blur/denoise/dehaze (foundation for 2.5c).
2. **2.4a** — bilateral σ_s = r/3 (one-line, plus GPU mirror if applicable).
3. **2.1 + 2.4b** — route sharpen (**luma in the L\* perceptual domain**) and the denoise chroma metric through the shared **Lab/LCh** module (built for Audit-01/03); keep CPU/GPU in sync.
4. **Dehaze overhaul (2.5a/b/c)** — airlight estimation, guided-filter transmission refinement, resolution-scaled patch. Largest cluster; effectively the full He et al. pipeline. Build the guided filter as a reusable O(N) primitive.
5. **2.6** — add a post-geometry output-sharpening stage to `render`; coordinate with the geometry resampler prefilter (Audit 05).
6. **No-op:** 2.3 confirmed correct — leave unchanged.

**Cross-cutting reminders:**
- 2.1 (perceptual-domain luma sharpen) and 2.4b (perceptual chroma) depend on the L\* / Lab-LCh work shared with Audits 01 and 03 — sequence after that lands.
- 2.6 changes the fixed pipeline order; update `render` and the pipeline tests; coordinate with Audit 05's resampler prefilter so downscale is handled in exactly one place.
- New tests: dehaze on a **colored** veil (currently only white-veil is tested), guided-filter transmission, σ=r/3 support, luma-in-perceptual-domain sharpen (no edge fringing, symmetric overshoot), output-sharpening after resample.
- This register reflects intent only; nothing here has been implemented.
