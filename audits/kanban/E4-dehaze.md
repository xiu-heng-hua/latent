# E4 — Dehaze overhaul (full He et al.)

Independent stream. Brings the dark-channel dehaze up to the full He, Sun & Tang
method: a reusable O(N) guided filter (**E4-C1**), per-channel airlight
estimation (**E4-C2**), guided-filter transmission refinement (**E4-C3**), and a
resolution-scaled patch (**E4-C4**), closed out by the colored-veil/refinement
tests (**E4-C5**). Implements `IP-04 2.5a/2.5b/2.5c`.

**Provides:** the guided-filter primitive (E4-C1) — a Backend candidate that
`E2-C7` (spatial highlight propagation) may also reuse. Other cards depend on it
rather than re-implementing (Global rule #5).

**Backend / lockstep context (read once, applies to the whole epic).** `dehaze`
is a `Backend` method (`latent-pipeline/src/lib.rs:330`), but the GPU backend
**delegates dehaze to the CPU** (`latent-gpu/src/lib.rs:558-561`: *"isn't ported
to WGSL yet; delegate to CPU"*). The actual math lives in the **pipeline
reference helpers** `dehaze_dark_channel` / `dehaze_recover` / `DEHAZE_PATCH` /
`DEHAZE_T0` (`latent-pipeline/src/lib.rs:458-506`), which the CPU backend
(`latent-cpu/src/lib.rs:179-198`) calls directly. **Decision for this epic:
dehaze stays CPU-only** — the overhaul keeps the GPU delegating to CPU, so no
WGSL mirror is written. The CPU↔GPU equivalence test stays trivially green
because GPU == CPU by delegation. The lockstep obligation that *does* apply
(Global rule #1): every change to the math goes in the **pipeline reference
helpers** and the **CPU backend** together — they must not drift. The guided
filter (E4-C1) is a *new* primitive; deciding its home (Backend method vs.
pipeline helper) is part of C1 and determines whether a future GPU port is even
possible — see that card.

---

### E4-C1 — Guided-filter primitive (O(N), reusable)
- Implements: basis for `IP-04 2.5b` (the reusable building block 2.5b/2.5c need)            Priority: High
- Crates/files: `latent-pipeline/src/lib.rs` (new `guided_filter` helper near the dehaze helpers); consumed by E4-C3 and (optionally) E2-C7
- Depends on: —             Blocks: E4-C3 (transmission refinement); reusable by E2-C7
- Heads-up: Implement the **O(N) box-filter-based guided filter** (He & Sun,
  *Guided Image Filtering*, ECCV 2010 — see `docs/spatial-dehaze-he-2011-tpami.pdf`
  §4.2 / the IPOL writeup `docs/spatial-dehaze-ipol-2024.pdf` for the filter
  algorithm). Signature shape: `guided_filter(guide: &[f32], src: &[f32], w, h, radius: usize, eps: f32) -> Vec<f32>` operating on **single-channel** (scalar) buffers — for dehaze the guide is input **luminance** and the filtered signal is the transmission map. Algorithm (linear model `q = a·I + b` per window):
  mean_I, mean_p, corr_I (= mean(I·I)), corr_Ip (= mean(I·p)) via a **box filter**;
  `var_I = corr_I − mean_I²`, `cov_Ip = corr_Ip − mean_I·mean_p`;
  `a = cov_Ip / (var_I + eps)`, `b = mean_p − a·mean_I`; then box-filter `a`,`b`
  and output `q = mean_a·I + mean_b`. The whole thing is **5 box filters** → O(N)
  independent of `radius`. **Implement the box filter as a true running-sum
  (integral-image or sliding-window) so cost is O(N), not O(N·r²)** — this is the
  point of the primitive; a naïve per-window sum defeats it (and would be slower
  than the existing `DEHAZE_PATCH` min on large rasters). Border handling: use a
  **shrinking window** (divide each pixel's box sum by its actual in-bounds tap
  count) so edges are not biased — match the border convention to the existing
  dark-channel clamp behavior at `latent-pipeline/src/lib.rs:480-481` so the two
  passes agree at borders. `eps` is the regularization (smoothing-vs-edge knob);
  pick a small default for transmission (He uses eps≈1e-3 on [0,1] luminance) and
  let C3 pass it.
  **Placement decision (state it in the card):** make it a **free function /
  pipeline helper** in `latent-pipeline`, **not** a `Backend` method. Rationale:
  dehaze is CPU-only by delegation (see epic header), so a Backend method would
  force a WGSL mirror for no benefit and break the "no GPU port" decision; a
  pipeline helper keeps it backend-agnostic and lets E2-C7 call it directly.
  (If a later epic wants a GPU guided filter, promoting it to a `Backend` method
  is a separate decision with its own lockstep cost — note it, don't do it here.)
  Validate `radius`/`eps` are finite and `radius ≥ 1`, `eps ≥ 0` (Global rule #4
  — but these are internal call-sites, so debug-assert is fine).
- Acceptance: a guided filter that **smooths within a region but preserves edges
  defined by the guide**. Named tests:
  `guided_filter_smooths_flat_noise` — a noisy constant signal with a clean
  constant guide is driven toward its mean (variance drops sharply).
  `guided_filter_preserves_guide_edge` — a step in **both** guide and signal is
  kept sharp (output step ≈ input step, no smearing across the edge), whereas a
  step in the signal *only* (flat guide) **is** smoothed — proving edge-awareness
  comes from the guide.
  `guided_filter_is_radius_cheap` — (optional perf guard) result is correct and
  runtime is ~independent of `radius` (asserts the O(N) box-sum path, e.g. by
  checking a large-radius run completes within a generous bound, or by a unit
  test that the box-filter sub-routine matches a brute-force reference on a small
  buffer for several radii).

---

### E4-C2 — Per-channel airlight estimation + `I/A` normalization
- Implements: `IP-04 2.5a` (H2, High — *Estimate airlight A*)            Priority: High
- Crates/files: `latent-pipeline/src/lib.rs` (`dehaze_dark_channel`, `dehaze_recover`, new airlight estimator); `latent-cpu/src/lib.rs:179-198` (`dehaze` impl — thread A through)
- Depends on: —             Blocks: E4-C3, E4-C5
- Heads-up: Today airlight is the literal constant **`A = 1`** baked into
  `dehaze_recover` (`latent-pipeline/src/lib.rs:493-504`). Add a per-image
  **airlight estimator** (He §4.3, `docs/spatial-dehaze-he-2011-tpami.pdf`):
  compute the patch dark channel over the whole image, take the **top 0.1%
  brightest dark-channel pixels**, and among those select the airlight `A`
  per channel (He picks the pixel with the highest *intensity* among that 0.1%;
  a robust per-channel **mean/percentile of that candidate set** is acceptable
  and steadier — state which you use). Result: a per-channel `A = [Ar, Ag, Ab]`.
  This is what neutralizes **colored** haze, which `A = 1` cannot (the *why* for
  the colored-veil test in C5).
  Then **normalize `I/A^c` per channel** before estimating the dark channel and
  during recovery, per the decision in `04-spatial-and-frequency.md` §2.5a: the
  dark-channel prior is computed on `I/A`, transmission `t = 1 − ω·darkchannel(I/A)`,
  and recovery becomes the per-channel `J^c = (I^c − A^c)/clamp(t,t0,1) + A^c`.
  **Signature change (call it out):** `dehaze_recover` gains a per-channel `a: [f32;3]`
  argument (or a small `Airlight([f32;3])` newtype), and `dehaze_dark_channel`
  must see normalized values — either pass `A` in or pre-normalize the buffer.
  Keep both **pipeline helpers and the CPU backend** in lockstep (epic header);
  the CPU `dehaze` computes `A` once up front, then loops as today.
  **Headroom (`I > 1`) must be revisited** — the current code splits each channel
  into `in_range = min(I,1)` + `headroom = max(I−1,0)` and passes headroom
  through *because the old model assumed `I ≤ A = 1`* (`latent-pipeline/src/lib.rs:498-504`).
  Now **`A` can exceed 1**, so the clamp pivot is no longer `1` but `A^c`: split
  at `A^c` (recover the `≤A` part by the model, pass the `>A` excess through), or
  re-derive the headroom rule against the new pivot. Do not silently keep the
  hard-coded `1.0` pivot — that was the whole point of estimating A. Guard
  `A^c > 0` to avoid divide-by-zero in `I/A`.
- Acceptance: airlight is estimated, not assumed; a **colored** veil is
  neutralized (not just a white one). Behavior pinned in C5 by
  `dehaze_neutralizes_a_colored_veil`. Plus a focused unit test
  `airlight_picks_brightest_dark_channel` — on a synthetic image whose hazy
  region is the brightest (highest dark channel) and tinted, the estimated `A`
  matches the veil color within tolerance, and recovery on a `A>1` pixel keeps
  the above-`A` headroom unclipped (`dehaze_headroom_pivots_at_airlight`).

---

### E4-C3 — Transmission refinement via the guided filter
- Implements: `IP-04 2.5b` (H3, High — *Add guided-filter refinement*)            Priority: High
- Crates/files: `latent-pipeline/src/lib.rs` (transmission stage between estimate and recovery); `latent-cpu/src/lib.rs:179-198` (`dehaze` impl — restructure to two passes)
- Depends on: E4-C1 (guided filter), E4-C2 (the `t` to refine is the per-channel-A one)             Blocks: E4-C5
- Heads-up: The current `dehaze` is **single-pass per pixel** — it computes the
  patch dark channel and recovers in one go (`latent-cpu/src/lib.rs:191-195`).
  Refinement needs **two passes**: (1) build the **raw transmission map**
  `t_raw[p] = 1 − ω·darkchannel(I/A)[p]` for the whole image, (2) **refine it with
  the guided filter** (E4-C1) using the **input luminance as the guide**, then
  (3) recover per pixel with the refined `t`. So restructure `dehaze` to
  materialize `t_raw` as a `Vec<f32>`, call `guided_filter(luma, t_raw, w, h,
  radius, eps)`, and feed the refined `t` into `dehaze_recover` (which no longer
  re-clamps a raw patch `t`, but still applies the `DEHAZE_T0` floor). The guide
  luminance should come from the **shared luma weights** (the project's
  `LUMA_WEIGHTS`, per E0) — do not hand-roll a fresh weighting. **Why:** the raw
  patch `t` is piecewise-constant over `(2·patch+1)²` blocks, so it produces
  **block and halo artifacts at depth edges** (He §4.2, `docs/spatial-dehaze-he-2011-tpami.pdf`);
  the guided filter aligns `t` to luminance edges and removes them.
  Pick the guided-filter `radius` larger than the dark-channel patch (He uses a
  guided radius ~ several× the patch) and a small `eps`; expose them as named
  consts next to `DEHAZE_PATCH`/`DEHAZE_T0` so they read together. Keep
  **pipeline helpers ↔ CPU backend** in lockstep; GPU still delegates (epic
  header).
- Acceptance: a sharp depth/transmission edge is recovered **without block or
  halo artifacts**. Named test `dehaze_transmission_refinement_removes_blocks`
  (in C5): an image with a step in haze density (two regions at different
  transmission, sharing a clean luminance edge) dehazes with the recovered
  transmission **following the luminance edge** — adjacent output pixels straddling
  the edge differ sharply (no `2·patch+1`-wide constant block bleeding across),
  and a flat region shows **no patch-grid banding** (variance within a
  uniform-haze region after refinement is below the raw-patch baseline). Confirm
  the existing white-veil recovery (`dehaze_clears_a_synthetic_veil`,
  `latent-pipeline/src/lib.rs:1176`) still passes (uniform veil → uniform `t` → no
  change from refinement).

---

### E4-C4 — Resolution-scaled dark-channel patch
- Implements: `IP-04 2.5c` (M2, Medium — *Scale patch with resolution*)            Priority: Medium
- Crates/files: `latent-pipeline/src/lib.rs:464-486` (replace `DEHAZE_PATCH` const + `dehaze_dark_channel` window); `latent-cpu/src/lib.rs` (call-site)
- Depends on: —             Blocks: E4-C5; **coordinate with E5-C1** (unified radius semantics)
- Heads-up: The patch is the **fixed** `pub const DEHAZE_PATCH: i32 = 4`
  (`latent-pipeline/src/lib.rs:468`) → a **9×9** window
  (`2·4+1`). He, Sun & Tang use **15×15** at their reference scale; on
  high-MP rasters a fixed 9×9 is in the **small-patch over-saturation regime**
  (the decision's *why*, `04-spatial-and-frequency.md` §2.5c). Replace the const
  with a **size derived from image resolution**, targeting **≥ a 15×15-equivalent
  at the reference scale** — e.g. `radius = max(7, round(k · min(w,h)))` scaled so
  a reference image yields ~15×15 (radius 7), larger for higher-MP. Pick the
  reference dimension and constant `k` explicitly and document them next to the
  const; make `dehaze_dark_channel` take the radius (it currently reads the const
  directly at lines 478-479). **Coordinate with E5-C1 (`IP-04 2.2`, unified radius
  semantics):** "radius" must mean the same thing here as in `blur`/`denoise`
  (half-window in pixels, consistent rounding/gate) — when E5-C1 lands, the
  dehaze patch must use the **same** radius convention (see
  `04-spatial-and-frequency.md` §2.2: *"tie the dehaze patch to a radius"*). If
  E5-C1 hasn't landed yet, derive the radius here in a way that E5-C1 can later
  unify without re-meaning it (half-window, round-half-up); leave a `// E5-C1:
  unify with blur/denoise radius` marker. The guided-filter radius (E4-C3) should
  scale alongside (it is conventionally several× the patch). Keep helpers ↔ CPU
  backend in lockstep.
- Acceptance: the patch grows with resolution and meets the 15×15-equivalent
  floor. Named test `dehaze_patch_scales_with_resolution` (in C5): the derived
  radius for a reference-sized image yields **≥ 15×15** (radius ≥ 7), and a
  larger image yields a **strictly larger** patch (monotonic in `min(w,h)`); a
  tiny image still yields the **minimum** (no degenerate 1×1). A regression check
  that the radius convention matches whatever `blur`/`denoise` use once E5-C1 is
  in (cross-referenced from E5-C1).

---

### E4-C5 — Colored-veil + refinement tests
- Implements: `IP-04 2.5a/2.5b/2.5c` test coverage            Priority: High
- Crates/files: `latent-pipeline/src/lib.rs` (tests) and/or `latent-cpu/src/lib.rs` (tests at `latent-cpu/src/lib.rs:323-374`)
- Depends on: E4-C2, E4-C3, E4-C4             Blocks: —
- Heads-up: The **only** existing dehaze recovery test validates a **white veil**
  (`dehaze_clears_a_synthetic_veil`, `latent-pipeline/src/lib.rs:1176`; the CPU
  side has `dehaze_recovers_a_uniform_veil_and_spares_a_clear_region`,
  `dehaze_preserves_a_bright_neutral_subject`,
  `dehaze_passes_highlight_headroom_through` at `latent-cpu/src/lib.rs:323-374`).
  A white veil cannot exercise airlight estimation (with `A=1` it would pass
  anyway), so add the three new behaviors. Build hazy inputs from the scattering
  model `I^c = J^c·t + A^c·(1−t)` so the expected recovery is exact, the same
  technique the existing test uses (`latent-pipeline/src/lib.rs:1183`). **Do not
  delete the white-veil test** — keep it green (regression that `A≈[1,1,1]` still
  works after the overhaul).
- Acceptance: Named tests —
  `dehaze_neutralizes_a_colored_veil` — synthesize a haze with a **tinted**
  airlight (e.g. `A = [0.9, 0.85, 1.0]`) over a known clear pixel at a known `t`;
  full-strength dehaze recovers the clear pixel within tolerance (proves the
  airlight estimate found the tint — would fail under the old `A=1`).
  `dehaze_transmission_refinement_removes_blocks` (per E4-C3) — a haze-density
  step sharing a clean luminance edge dehazes with `t` following the edge: no
  `(2·patch+1)`-wide constant block bleeds across, and within-region variance in a
  uniform-haze patch is below the raw-patch (un-refined) baseline.
  `dehaze_patch_scales_with_resolution` (per E4-C4) — derived radius ≥ 7 at
  reference scale, strictly larger for a larger image, clamped to the minimum for
  a tiny one.
  All three fail before their feature card and pass after (Global rule #2);
  `cargo fmt --check`, `cargo clippy --all-targets` (zero warnings),
  `cargo test --workspace` green (Global rule #3).

---

**Epic done when:** the dehaze path implements the full He, Sun & Tang method —
a reusable O(N) guided filter (E4-C1), per-channel estimated airlight with
`I/A` normalization and airlight-aware headroom (E4-C2), guided-filter-refined
transmission (E4-C3), and a resolution-scaled dark-channel patch (E4-C4) — with
the colored-veil, transmission-refinement, and patch-scaling tests green
(E4-C5), the original white-veil test still passing, the pipeline reference
helpers and CPU backend in lockstep (GPU still delegating to CPU, no WGSL
mirror), and the guided filter exposed as a reusable pipeline helper for E2-C7.
`cargo fmt --check`, `cargo clippy --all-targets` (zero warnings), and
`cargo test --workspace` all pass.
