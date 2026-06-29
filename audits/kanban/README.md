# Implementation Plan — Kanban Board

This board turns every **Fix** decision (image-processing: [`../decisions/`](../decisions/); code review: [`../decisions/code-review/`](../decisions/code-review/)) into an executable backlog. Work is decomposed into **9 independent epics**; each epic file holds **cards**, and every card carries a **heads-up** — the context an implementer (here, a subagent) needs to pick it up cold.

**Status:** planning artifact — **no source code has been written yet.** Cards reference the decisions they implement; the decisions hold the *why* and the authoritative `file:line` pointers.

## How to use this board
- An epic is a self-contained stream; epics can largely proceed in parallel (dependencies are listed per epic and per card).
- A card is one cohesive unit of work — small enough for one subagent, with explicit acceptance/tests.
- Each card's **heads-up** names the approach, the gotchas, the references in [`../../docs/`](../../docs/), and the cross-backend obligations.
- Pick cards in an epic top-to-bottom unless a `Depends on` says otherwise.

## Card format
```
### EN-Ck — <title>
- Implements: <decision refs>            Priority: <Critical|High|Medium|Low>
- Crates/files: <paths>
- Depends on: <card ids | —>             Blocks: <card ids | —>
- Heads-up: <approach, gotchas, the why, cross-backend/test obligations>
- Acceptance: <what proves it done — behavior + tests>
```

---

## Global rules (apply to every card)
1. **CPU ↔ GPU lockstep (Initiative G).** Any change to a `Backend` primitive (`map_pixels`/tone/saturation/`combine`/`blur`/`resample`/`warp`/`apply_radial_gain`/denoise) must update the CPU backend, the matching WGSL shader **and** the `latent-pipeline` reference helper, and keep the GPU/CPU render-equivalence test green. New perceptual math (L\*, LCh) added on CPU must be mirrored in WGSL.
2. **A test pins every change.** No card is done without a unit/integration test that fails before and passes after. Re-verify the numeric audits' claims in the new domain where a domain changed (e.g. tone in L\*).
3. **Baseline stays green.** `cargo fmt --check`, `cargo clippy --all-targets` (zero warnings), and `cargo test --workspace` must pass after each card.
4. **Sanitize at the boundary, trust within.** Untrusted input (sidecars, RAW metadata, lens DB) is validated/clamped on the way in (E7-C1, E2); internal code may then assume finiteness/ranges.
5. **One place per concern.** The minification interpolation+prefilter (E6), the perceptual-lightness L\* (E0-C1), and the guided filter (E4-C1) each have a single owner; other cards depend on them rather than re-implementing.

---

## Epics & dependency order

```
            ┌────────── E7 Robustness/data-model (do early; independent) ──────────┐
            │                                                                       │
  E0 Color core ──► E1 Tone & grading ─┐                                            │
   (Lab/LCh, L*,     (L*, LCh, curves) │                                            │
    D50, DNG,                          ├─► E6 Resampling & GPU parity ──► (board complete)
    Bradford,        E5 Spatial ───────┘        ▲            ▲
    rolloff)         (sharpen/denoise) ─────────┘            │
                                                             │
  E2 Decode/sensor (independent) ───────────────────────────┤
  E3 Lens / lensfun (independent) ──────────────► (warp) ────┘
  E4 Dehaze (independent; provides guided filter) ──► (E2-C7 reuse)
  E8 Export/CLI/app (needs E0 output transform) ─────────────────────
```

**Recommended sequencing**
1. **E7** (sanitize-on-load, bounded undo, `Send+Sync`) and **E0** (color core + Lab/LCh + L\*) — foundational; start first.
2. In parallel, independent streams: **E2** (decode/sensor), **E3** (lens), **E4** (dehaze).
3. After E0: **E1** (tone/grading) and **E5** (spatial) — both consume L\*/Lab.
4. **E6** (resampling + GPU parity + output sharpening + auto-scale) — after E3 (warp) and E5 (sharpen form); converges all minification.
5. **E8** (export/CLI/app) — after E0's output transform (E0-C6/C7) is stable.

---

## Epic summaries & card lists

> Full cards (with heads-ups + acceptance) are in each epic file. Decision refs: `IP-0X §p` = image-processing register; `CR-0X <id>` = code-review register.

### [E0 — Color-management core & perceptual module](E0-color-core.md)  *(foundational)*
The standard-conformant color rebuild + the shared `Lab/LCh` + `L*` module everything perceptual depends on.
- **E0-C1** Lab/LCh module (working→XYZ→Lab→LCh + inverse, D50) and exposed `L*` perceptual lightness — `IP-01 #5`
- **E0-C2** Full-precision XYZ→linear-sRGB matrix (derive from Rec.709 primaries) — `IP-01 #1`
- **E0-C3** D50 ProPhoto working space + rebuilt matrices + `LUMA_WEIGHTS` — `IP-01 #2`
- **E0-C4** Bradford chromatic-adaptation helper — `IP-01 #9`
- **E0-C5** Full DNG camera→working model (inverse-ColorMatrix + Bradford WB; drop row-norm + comment; fix decode order) — `IP-01 #4a`
- **E0-C6** Working→sRGB output matrix (D50→D65 adapted, full precision, no row-norm) — `IP-01 #4b`
- **E0-C7** Hue-preserving highlight rolloff (max/luminance shared factor) — `IP-01 #7`

### [E1 — Tone & creative color grading](E1-tone-grading.md)  *(dep: E0)*
- **E1-C1** L\* tone domain (encode/decode, re-derive shape pivots, LUT) — `IP-03 2.1`
- **E1-C2** Always-monotone S-curve contrast — `IP-03 2.2`
- **E1-C3** Headroom pass-through with unit slope (+ GPU mirror, fix comments) — `IP-03 2.3`
- **E1-C4** Monotone-cubic (PCHIP) `point_curve`, made total (NaN/inf-safe) — `IP-03 2.8` + `CR-02 H1`
- **E1-C5** LCh 8-band hue mixer (retire/guard HSV path) — `IP-03 2.5` + `2.6` + `CR-01 L1`
- **E1-C6** Chroma-preserving saturation (LCh) — `IP-03 2.7`
- **E1-C7** Channel-mixer preserve-luminosity toggle — `IP-03 2.9`

### [E2 — RAW decode & sensor metadata](E2-decode-sensor.md)  *(independent)*
- **E2-C1** Extend `read_metadata` (full `cblack` pattern, `linear_max[4]`, `filters`, `colors`) — `IP-02 2.1/2.2` + `2.8b`
- **E2-C2** Sensor guard — accept only 2×2 RGB Bayer; reject X-Trans/Foveon **before** any `cfa` indexing; `cfa ∈ 0..4` clamp — `IP-02 2.8b` + `CR-01 H1/M1`
- **E2-C3** Normalization rework — per-pixel black (pattern), per-channel white (`linear_max`), consistent per-plane scale — `IP-02 2.1/2.2/2.3`
- **E2-C4** Honor `raw_pitch` row stride in the `unsafe` unpack load — `CR-01 H2`
- **E2-C5** Overflow-safe `ImageBuf` (`checked_mul` + `try_new`) — `CR-01 M2`
- **E2-C6** Mirror/clamp MHC 5×5 border — `IP-02 2.6`
- **E2-C7** Spatial highlight propagation (LCH-blend/guided) + 5×5 clip mask — `IP-02 2.7` *(may reuse E4-C1 guided filter)*
- **E2-C8** Decode/FFI-error + boundary tests (HSV, Mat3, error paths) — `CR-01 §4`

### [E3 — Lens correction (lensfun fidelity)](E3-lens.md)  *(independent)*
- **E3-C1** Thread lensfun geometry through `latent-lens` (`RealFocal`, `Crop`, model, POLY3 TCA, center) + finiteness guards — `IP-05 2.3/2.4` + `CR-04 M4`
- **E3-C2** Half-diagonal (focal-scaled) radius normalization + center-offset scaling; fix the false "matching lensfun" doc — `IP-05 2.3` + `2.3-L1` + `CR-04 M3`
- **E3-C3** Model-aware Newton inversion for POLY3/POLY5 — `IP-05 2.2`
- **E3-C4** Full POLY3 radial TCA in `Warp::map_channel` — `IP-05 2.4`
- **E3-C5** Verify & align PA vignetting convention (divide vs multiply) + flat-field test — `IP-05 2.8`
- **E3-C6** lensfun round-trip tests (distortion grid, TCA target, vignetting)

### [E4 — Dehaze overhaul (full He et al.)](E4-dehaze.md)  *(independent; provides the guided filter)*
- **E4-C1** Guided-filter primitive (O(N), reusable) — basis for `IP-04 2.5b`
- **E4-C2** Per-channel airlight estimation + `I/A` normalization — `IP-04 2.5a`
- **E4-C3** Transmission refinement via the guided filter — `IP-04 2.5b`
- **E4-C4** Resolution-scaled dark-channel patch — `IP-04 2.5c`
- **E4-C5** Colored-veil + refinement tests

### [E5 — Spatial filters (denoise / sharpen / clarity)](E5-spatial.md)  *(dep: E0)*
- **E5-C1** Unify radius semantics across blur/denoise/dehaze — `IP-04 2.2`
- **E5-C2** Bilateral spatial Gaussian σ_s = r/3 (±3σ) — `IP-04 2.4a`
- **E5-C3** Perceptual-domain (L\*) luma sharpen (capture) — `IP-04 2.1`
- **E5-C4** Perceptual chroma denoise metric (Lab/LCh) — `IP-04 2.4b`

### [E6 — Resampling, geometry GPU parity & convergence](E6-resampling-gpu.md)  *(dep: E3, E5)*
- **E6-C1** Higher-order interpolation (Lanczos/bicubic) + minification prefilter (CPU) — `IP-05 2.7`
- **E6-C2** GPU resample `w≤0` guard + `warp.wgsl` + matching interpolation/prefilter — `IP-05 2.9` + `CR-03 F1`
- **E6-C3** Output-sharpening pass (post-geometry, perceptual luma) — `IP-04 2.6`
- **E6-C4** Auto-scale-to-fill option — `IP-05 N1`
- **E6-C5** Port `combine`/`apply_radial_gain` to WGSL (radial gain via corrected vignetting) — `CR-03 F6`
- **E6-C6** CPU/GPU + Test/CPU/GPU equivalence harness; close the test gap — `CR-03 F3` + `CR-02 M4`
- **E6-C7** GPU robustness/perf — `read_staging` recoverable error + CPU fallback; keep image resident + pool buffers — `CR-03 F4/F5`

### [E7 — Edit data model & robustness](E7-data-model.md)  *(independent; do early)*
- **E7-C1** Broad `Settings::sanitize` on load (non-finite scrub + range clamp) — `CR-02 M2` (owns `IP-03 2.2` subset)
- **E7-C2** Mask-shape `#[serde(default)]` / forward-compat — `CR-02 M1`
- **E7-C3** Bounded undo (`VecDeque` cap) — `CR-02 H2`
- **E7-C4** `Backend: Send + Sync` — `CR-02 M3`
- **E7-C5** Edit-model tests (sanitize, serde round-trip/forward-compat, history)

### [E8 — Export, CLI & app](E8-export-app.md)  *(dep: E0 output transform)*
- **E8-C1** Reject unknown export extensions with a typed error — `CR-04 H2`
- **E8-C2** `save`/`save_16` honor `ImageResult` + zero-dimension guard — `CR-04 M1/M2`
- **E8-C3** Reachable 16-bit export (`--depth`, auto for tiff/png) — `CR-04 L2`
- **E8-C4** Render/export off the egui UI thread (worker + progress) — `CR-04 H1`
- **E8-C5** CLI exit codes, lens-lookup/CLI tests, nits

---

## Coverage check
Every **Fix** decision maps to exactly one card (overlaps are implemented once and cross-referenced): IP-01 → E0; IP-02 → E2; IP-03 → E1; IP-04 → E5 (+ dehaze E4, output-sharpen E6); IP-05 → E3 (lens) + E6 (resampling/GPU); CR-01 → E2; CR-02 → E7 (+ E1-C4, E6-C6); CR-03 → E6; CR-04 → E8 (+ E3-C1/C2). The four deferred code-review findings (X-Trans/Foveon, point_curve NaN, GPU w≤0, lens center) are realized inside E2-C2, E1-C4, E6-C2, and E3-C2 respectively.
