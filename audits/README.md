# `latent` — Code Review & Image-Processing Audit

**Subject:** `latent`, a RAW photo-development application (a Lightroom/darktable/RawTherapee-class tool) written in Rust (edition 2024), ~8,400 LOC across a 9-crate Cargo workspace.
**Date:** 2026-06-27
**Commit reviewed:** `4a6b2af` (branch `main`), working tree clean.

This folder holds three deliverables requested together:

1. **A comprehensive code review** (software-engineering quality: correctness, safety, FFI/`unsafe` soundness, robustness, idioms, tests) — [`code-review/`](code-review/).
2. **A comprehensive image-processing audit** (algorithmic & colorimetric correctness verified against the primary literature and standards) — [`image-processing/`](image-processing/), with a compiled PDF at [`latent-image-processing-audit.pdf`](latent-image-processing-audit.pdf).
3. **An annotated list of theory resources** for someone with a strong math/CS/engineering background but no prior RAW-development knowledge — [`../audits/theory-resources.md`](theory-resources.md).

Every reference consulted for the image-processing audit (papers, standards, lensfun source) was downloaded to [`../docs/`](../docs/) so the findings can be checked against the actual source text rather than recollection.

---

## Headline assessment

**This is an unusually disciplined, well-engineered codebase.** It builds clean, `cargo fmt --check` is clean, **`cargo clippy` is clean with zero warnings**, and **all 50 tests pass**. The architecture is principled: a fixed-order `render()` pipeline owns stage ordering while a `Backend` trait exposes only stateless pixel primitives; operations are modelled as *serializable data* (`PointOp`, `Warp`, `Transform`) rather than closures, so a CPU and a GPU backend can share one definition. Doc-comments are excellent and — rare for a hobby imaging project — cite the actual source papers (Malvar–He–Cutler 2004, Tomasi–Manduchi 1998, He–Sun–Tang 2009, Brown 1966). The color pipeline is genuinely linear-light end-to-end with deliberate highlight-headroom handling.

The defects we found are **not architectural**. They cluster into three themes:

- **Sensor-metadata edge cases** that mishandle non-mainstream sensors (Foveon, Fujifilm X-Trans) and cameras that encode their black level as a 2-D pattern — these can *panic* or *silently mis-develop*.
- **A lens-correction convention mismatch** — the radial-distortion coordinate normalization matches *PanoTools*, not *lensfun* (the stated coefficient source), which silently rescales every lens coefficient.
- **Hostile/degenerate input reaching float math** (a `NaN` in a sidecar curve crashes the renderer) and a few **CPU↔GPU divergences** in untested corners.

None block normal use on mainstream Bayer cameras with neutral-to-moderate edits; several should be fixed before the tool is trusted on arbitrary cameras or untrusted sidecar files.

### Verification status (this environment)

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo build --workspace --all-targets` | clean |
| `cargo test --workspace` | **50 passed**, 0 failed (incl. 10 GPU tests run on a software Vulkan device) |
| `cargo clippy --workspace --all-targets` | clean, **0 warnings** |

---

## How this audit was produced

The work was fanned out across **ten parallel sub-agents** (five image-processing domains, four code-review domains, one resource researcher), then synthesized here. Two guard-rails were imposed:

- **Image-processing claims were verified against primary sources, not from memory.** Each auditor was required to fetch and cite the canonical paper/standard/source — e.g. the Malvar–He–Cutler kernels were checked element-by-element against the ICASSP-2004 figure; the lensfun normalization was read out of `modifier.cpp`/`mod-coord.cpp`; the sRGB/ROMM constants against IEC 61966-2-1 / ISO 22028-2.
- **Code-review claims were grounded in the code and, where a bug was suspected, confirmed by a throwaway test** (the `NaN`-curve panic, the unbounded undo history, the GPU `w≤0` divergence, and the `0×0`-image export path were all reproduced before being reported). False-positive candidates the prompts suggested were *disproven* and recorded as such (e.g. the suspected duplicate-`x` divide-by-zero in `point_curve` is structurally impossible; the `hsv_to_rgb` boundary is correct-by-accident, not a live bug).

**Independent corroboration.** Three findings were discovered *separately* by two different agents working from different angles — the strongest possible signal that they are real:

| Finding | Found by (image-processing) | Found by (code review) |
|---|---|---|
| GPU `resample.wgsl` lacks the `w≤0` guard → CPU/GPU divergence | [05 Geometry](image-processing/05-geometry-and-optics.md) **H1** | [03 Backends](code-review/03-cpu-and-gpu-backends.md) **F1** (reproduced numerically) |
| Fujifilm X-Trans passes the "RGB Bayer" guard and is mis-demosaiced | [02 Demosaic](image-processing/02-raw-decode-and-demosaic.md) **H2** | [01 Foundations](code-review/01-raw-and-image-foundations.md) **M1** |
| lensfun optical-center offset normalization mismatch | [05 Geometry](image-processing/05-geometry-and-optics.md) **L1** | [04 Export/Lens/App](code-review/04-export-lens-app.md) **M3** |

---

## Consolidated top findings (severity-ranked, de-duplicated)

Full detail — with code excerpts, citations, and fixes — is in the linked per-domain reports. IDs below are `report:local-id`.

### Critical

| ID | Finding | Where |
|---|---|---|
| **IP-02 C1** | **`cblack` 2-D black-level pattern ignored.** Only `cblack[0..4]` is read, but LibRaw/DNG encode a repeating black-level pattern in `cblack[4]=W`, `cblack[5]=H`, values at `cblack[6..]`. Cameras that store their pedestal there (X-Trans always; some Bayer bodies) get a wrong/too-small black → raised, tinted shadows. Index 4 is even mis-used as a "G2 black." | `latent-raw/src/lib.rs:164,521` |
| **IP-05 C1** | **Lens radius normalization matches PanoTools, not lensfun.** Code normalizes by half the *shorter side* (`inv_norm = 2/min(w,h)`); lensfun normalizes by the focal-scaled *half-diagonal* (`NormScale = hypot(36,24)/Crop/hypot(W+1,H+1)/RealFocal`). Every lensfun-sourced distortion/TCA/vignetting coefficient is applied at the wrong radial scale (off by ~1.3–1.8× per even order, aspect-ratio dependent). The "matching lensfun" doc-comment is false. | `latent-pipeline/src/lib.rs:675`; `latent-edit/src/lib.rs:528` |

### High (selected — 16 total across the two streams)

| ID | Finding | Where |
|---|---|---|
| **CR-01 H1** | **OOB panic on Foveon/full-color sensors.** `libraw_COLOR` returns `6` when `filters==0`, so `cfa=[6,6,6,6]` passes the `cdesc=="RGBG"` guard, then indexes 4-element `cblack`/`gains` → panic. Breaks the "never panic across FFI" contract; reachable from a real file. | `latent-raw/src/lib.rs:164,205` |
| **CR-01 H2** | **`from_raw_parts` ignores `raw_pitch`.** The one load-bearing `unsafe` assumes stride = `raw_width·2`; LibRaw exposes `sizes.raw_pitch` precisely because that isn't always true → potential OOB read / sheared rows. | `latent-raw/src/lib.rs:470` |
| **CR-02 H1** | **A `NaN` in a sidecar curve control point panics `render()`.** `total_cmp` sorts `NaN` last, the high-end early-return never fires, and `windows(2).position(..).unwrap()` hits `None`. RON loads `NaN`/`inf` happily → a corrupt sidecar is a direct crash vector. *(Confirmed end-to-end.)* | `latent-pipeline/src/lib.rs:618` |
| **CR-02 H2** | **Undo history grows unbounded** — no cap; every step clones the whole `Settings` (worse with brush masks). A real per-session memory leak. *(Confirmed at 10k entries.)* | `latent-edit/src/history.rs` |
| **CR-04 H1** | **GUI renders synchronously on the UI thread** — a full-resolution export/render freezes the window; no worker thread or progress. | `latent-app/src/gui.rs:173` |
| **CR-04 H2 / IP-01** | **Silent untagged export fallback** — an unknown extension (`.bmp`, a typo) writes with **no ICC profile** and no warning: silent color-management loss. | `latent-export/src/lib.rs:154` |
| **IP-02 H1** | **Per-channel `linear_max[4]` ignored.** Clip detection and normalization use the single scalar `color.maximum` ("may be changed by automated maximum adjustment"); LibRaw's own remedy for magenta-highlight ("pink clouds") is per-channel `linear_max`, which is available but never read. | `latent-raw/src/lib.rs:169,184` |
| **IP-02 H2 / CR-01 M1** | **X-Trans accepted as Bayer.** `cdesc=="RGBG"` is true for *all* RGB sensors incl. Fuji X-Trans; the pattern is identified by `filters==9`/`xtrans`, never checked → a `.RAF` is mis-demosaiced as 2×2 Bayer. | `latent-raw/src/lib.rs:435` |
| **IP-03 F1** | **`contrast` inverts tones for amount > 1** (monotonicity break: min slope `1−a < 0`). The GUI clamps to ±1, but `SelectiveTone.contrast` is an unclamped `f32` reachable from the API/sidecar, and the function's doc promises monotonicity. *(Confirmed numerically.)* | `latent-image/src/tone.rs:94` |
| **IP-03 F2** | **Positive `contrast`/`highlights` crush the highlight headroom they claim to shape** — endpoint slope → 0 at white, so a linear `8.0` highlight maps to ≈`1.04` (a soft clip), the opposite of the documented intent. Replicated in the GPU shader. | `latent-image/src/tone.rs:94,100` |
| **IP-04 H2** | **Dehaze fixes airlight `A=1`** instead of estimating it (He §4.3); colored haze isn't neutralized and strength is mis-scaled when true airlight ≠ 1. | `latent-pipeline/src/lib.rs:493` |
| **IP-04 H3** | **No dehaze transmission refinement** — raw per-pixel patch `t` with no soft-matting/guided-filter step → block/halo artifacts at depth edges (He §4.2). | `latent-pipeline/src/lib.rs:475` |
| **IP-04 H1** | **Bilateral spatial Gaussian truncated at ±2σ** (`σ=r/2`, window `±r`) — a hard cutoff at `e⁻²≈0.135` drops ~4.6%/axis of kernel mass vs the standard 3σ convention. | `latent-pipeline/src/lib.rs:528` |
| **IP-05 H1 / CR-03 F1** | **GPU `resample.wgsl` lacks the `w≤0` guard** and has **no radial/TCA `warp` shader** — extreme-keystone corners sample real pixels/NaN on GPU instead of black; the `Warp` (distortion/CA) path is CPU-only. *(Divergence reproduced numerically.)* | `latent-gpu/src/resample.wgsl` |
| **IP-01 F1** | **Row-normalizing `camera_to_working` is not colorimetrically equivalent** to the DNG/dcraw reference (fold `diag(cam_mul)⁻¹` into the matrix + Bradford CA). It pins neutrals correctly but bends saturated colors by up to ~0.28 in linear working RGB for a real Canon profile — a visible hue shift, not sub-quantization. | `latent-image/src/color.rs:166` |

### Medium / Low / Nit

A further **~23 Medium**, **~24 Low**, and **~15 Nit** items are documented in the per-domain reports — including the ProPhoto-vs-ROMM-at-D65 naming (IP-01 F2), the luma-blend saturation collapsing blues (IP-01 F5 / IP-03 F3), the "HSL" mixer actually being HSV (IP-03 F4), the per-channel rescale-denominator tint (IP-02 M1), the dehaze 9×9-vs-15×15 patch (IP-04 M2), the un-prefiltered bilinear aliasing on strong warps (IP-05 M3), the inconsistent sidecar forward-compat on mask shapes (CR-02 M1), missing load-time value validation (CR-02 M2), the `TestBackend`/`CpuBackend` parity-test gap (CR-02 M4 / CR-03 F3), and the export dimension-validation/library-panic issues (CR-04 M1/M2).

---

## Suggested remediation order

1. **Stop the crashes and OOB.** Sanitize sidecar values on load (rejects/repairs `NaN`/`inf`/out-of-range — fixes CR-02 H1+M2 and the `contrast>1` API path at once); validate `filters` at decode to reject Foveon and route X-Trans (fixes CR-01 H1 + IP-02 H2); honor `raw_pitch` in the `unsafe` slice (CR-01 H2).
2. **Get the sensor levels right.** Read the `cblack` 2-D pattern and per-channel `linear_max` (IP-02 C1 + H1) — these decide whether shadows and highlights are correct on many real cameras.
3. **Fix the lens convention.** Reconcile the radius normalization with the actual coefficient source (lensfun half-diagonal vs PanoTools half-short-side) and make the doc-comment honest (IP-05 C1); add the GPU `w≤0` guard + a `warp` shader or asserted CPU fallback (IP-05 H1 / CR-03 F1).
4. **Robustness & UX.** Cap the undo history (CR-02 H2); move rendering off the UI thread (CR-04 H1); warn or refuse on untagged export extensions (CR-04 H2).
5. **Quality polish.** The tone-curve headroom crush, dehaze airlight estimation + transmission refinement, bilateral 3σ support, and saturation in a chroma-preserving space are quality upgrades, not bugs.

---

## Deliverable index

### Image-processing audit — [`image-processing/`](image-processing/)
- [`01-color-science.md`](image-processing/01-color-science.md) — camera matrix, ProPhoto/ROMM-D65 working space, sRGB output, ICC, luminance.
- [`02-raw-decode-and-demosaic.md`](image-processing/02-raw-decode-and-demosaic.md) — black/white levels, white balance, bilinear & Malvar–He–Cutler demosaic, highlight reconstruction.
- [`03-tone-and-color-grading.md`](image-processing/03-tone-and-color-grading.md) — perceptual-domain tone curves, HSL/HSV mixer, channel mixer, saturation.
- [`04-spatial-and-frequency.md`](image-processing/04-spatial-and-frequency.md) — unsharp, clarity, bilateral denoise, dark-channel dehaze.
- [`05-geometry-and-optics.md`](image-processing/05-geometry-and-optics.md) — resampling, homography/keystone, Brown–Conrady distortion, lateral CA, vignetting.
- **Compiled PDF:** [`latent-image-processing-audit.pdf`](latent-image-processing-audit.pdf) (all five, with typeset formulas).

### Code review — [`code-review/`](code-review/)
- [`01-raw-and-image-foundations.md`](code-review/01-raw-and-image-foundations.md) — `latent-raw` (LibRaw FFI), `latent-image` (buffers, color, tone).
- [`02-pipeline-and-edit-model.md`](code-review/02-pipeline-and-edit-model.md) — `latent-pipeline` orchestration, `latent-edit` data model + undo/redo.
- [`03-cpu-and-gpu-backends.md`](code-review/03-cpu-and-gpu-backends.md) — `latent-cpu` (rayon), `latent-gpu` (wgpu/WGSL), CPU↔GPU equivalence.
- [`04-export-lens-app.md`](code-review/04-export-lens-app.md) — `latent-export`, `latent-lens` (lensfun FFI), `latent-app` (CLI + egui GUI).

### Theory resources — [`theory-resources.md`](theory-resources.md)
~40 verified resources across 12 categories, with a sequenced reading path, an "if you only read five things" shortlist, and a table mapping each algorithm `latent` implements to its canonical paper/standard.

### Reference library — [`../docs/`](../docs/)
17 downloaded primary sources (papers, standards, lensfun source) cited throughout the audit, prefixed by domain (`color-`, `demosaic-`, `spatial-`, `geometry-`, `tone-`).
