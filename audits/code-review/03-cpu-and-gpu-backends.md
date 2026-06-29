# Code Review 03 — CPU and GPU Rendering Backends

Scope: `latent-cpu/src/lib.rs`, `latent-gpu/src/lib.rs`,
`latent-gpu/src/{map_pixels,box_blur,resample}.wgsl`, cross-checked against the
reference helpers and `Backend` contract in `latent-pipeline/src/lib.rs`.

Focus: correctness, CPU/GPU result equivalence, rayon concurrency safety, wgpu
resource management & `unsafe`, WGSL shader correctness, bytemuck/WGSL layout,
numerical consistency, edge cases, tests. Algorithm theory is out of scope.

---

## 1. Overview & overall quality

The two backends are **well-engineered and unusually disciplined about staying in
sync**. The pipeline deliberately exposes reference helpers
(`midtone_weight`, `bilateral_pixel`, `dehaze_dark_channel`, `dehaze_recover`,
`normalized_radius`, `ToneCurve::lut`, `LUMA_WEIGHTS`, `tone::GAMMA`) and the CPU
backend reuses them verbatim, so the CPU backend is a faithful, complete
implementation of the contract and a genuine correctness reference.

The GPU backend is a **partial accelerator with an honest CPU fallback**: only
`map_pixels` (Gain/Tone/Saturation), `blur`, and `resample` run natively in WGSL;
everything else (and the Curves/ColorMix/Matrix point ops) delegates to an
embedded `CpuBackend`. The fallback is wired correctly and the backend is always
complete. Device acquisition fails gracefully (`GpuUnavailable`), GPU is optional,
and the tests skip rather than fail when no adapter is present.

Concurrency is clean: every rayon parallel section writes to **disjoint** output
slices (`par_iter_mut` over the output, or `par_chunks_mut` over output rows) and
only ever *reads* shared input — there are no scatter writes, no shared mutable
accumulators, and no `unsafe`. GPU resource management is correct: buffers are
sized from the data, staging readback is properly mapped/unmapped, and the
bytemuck `Pod` structs match their WGSL `std140` uniform layouts (the author
sidestepped the classic `vec3`/array-stride traps by using flat scalar fields).

The one material correctness gap is a **CPU/GPU divergence in `resample.wgsl`: it
omits the `w <= 0` guard** that `Transform::map` has on the CPU, so extreme
keystone corners (reachable through the pipeline) render differently — and no test
covers it. There is also a smaller `floor`-of-negative integer-truncation
divergence in the same shader. Everything else is matched or correctly delegated.

Overall quality: **high**. One High finding (resample guard), one Medium
(negative-coordinate truncation), the rest are Low/Nit or test-coverage gaps.

---

## 2. Backend primitive coverage matrix

| `Backend` primitive | CPU | GPU native? | GPU path | Verified equivalent? |
|---|---|---|---|---|
| `map_pixels` Gain | rayon `par_iter_mut` | **yes** (`map_pixels.wgsl` op 0) | WGSL | yes — `map_pixels_gain_matches_cpu` (tol 1e-6) |
| `map_pixels` Tone | rayon | **yes** (op 1, LUT upload) | WGSL | yes — `map_pixels_tone_matches_cpu` + `_above_one_` (tol 1e-3) |
| `map_pixels` Saturation | rayon | **yes** (op 2) | WGSL | yes — `map_pixels_saturation_matches_cpu` (tol 1e-5) |
| `map_pixels` Curves | rayon | **no** → CPU fallback | CPU | n/a (same code) |
| `map_pixels` ColorMix | rayon | **no** → CPU fallback | CPU | n/a |
| `map_pixels` Matrix | rayon | **no** → CPU fallback | CPU | n/a |
| `blur` (box, separable) | rayon, 2-pass | **yes** (`box_blur.wgsl`, 2 dispatches) | WGSL | yes — `blur_matches_cpu` (tol 1e-5) |
| `combine` Unsharp/LocalContrast | rayon | **no** → CPU fallback | CPU | n/a |
| `resample` (homography, bilinear) | rayon `par_chunks_mut` | **yes** (`resample.wgsl`) | WGSL | **partial — interior only**; `w<=0` & negative-coord paths untested & divergent (see F1, F2) |
| `warp` (radial/CA) | rayon | **no** → CPU fallback | CPU | n/a (CPU) |
| `apply_radial_gain` | rayon | **no** → CPU fallback | CPU | n/a |
| `denoise` (bilateral) | rayon | **no** → CPU fallback | CPU | n/a |
| `dehaze` (dark-channel) | rayon | **no** → CPU fallback | CPU | n/a |
| `eval_mask` | rayon | **no** → CPU fallback | CPU | n/a |
| `blend` | rayon | **no** → CPU fallback | CPU | n/a |

GPU implements **3 of 11** primitives natively (map_pixels for 3 of 6 point ops,
blur, resample); the rest delegate to `CpuBackend`. The only native primitive that
is **not fully** equivalence-verified is `resample` (interior agreement is tested,
but the `w<=0` and out-of-frame paths diverge from the CPU and are untested).

---

## 3. Findings by severity

### High

**F1 — `resample.wgsl` omits the `w <= 0` guard, diverging from `Transform::map`.**
- `latent-gpu/src/resample.wgsl:46-48` computes
  `w = m6·ox + m7·oy + m8` then `sx = (...)/w`, `sy = (...)/w` **unconditionally**.
- The CPU reference, `Transform::map` (`latent-pipeline/src/lib.rs:133-144`),
  guards `if w <= 0.0 { return (-1.0, -1.0); }` — mapping behind-the-plane output
  pixels *outside* the source so they read as **black**. The CPU `resample`
  (`latent-cpu/src/lib.rs:101-116`) relies on this guard; it does not re-check `w`.
- **Impact (CPU/GPU divergence):** for an output pixel where `w < 0` (extreme
  keystone — the same `w <= 0` corner the pipeline test
  `extreme_keystone_behind_the_plane_maps_outside` documents, lib.rs:1552-1566),
  the GPU produces a *sign-flipped finite source coordinate* and samples real
  pixel data instead of black. For `w == 0` exactly, `sx/sy` are `inf`/`NaN`,
  `floor(inf)`/`i32(inf)` are undefined in WGSL, and the output pixel is garbage.
  This path **is reachable through the pipeline on the GPU**: when geometry has a
  keystone/straighten but no lens distortion/CA, `apply_geometry` routes to
  `backend.resample` (lib.rs:748), which on the GPU is the native shader. (With
  lens distortion present it routes to `warp`, which the GPU delegates to CPU, so
  that path is safe.)
- **Why untested:** the standalone GPU resample tests use only mild *interior*
  perspective (`m6 = 0.01`, samples kept inside) and the end-to-end
  `render_matches_cpu_across_the_pipeline` includes lens distortion, so it goes
  through `warp` (CPU) and never exercises GPU resample with `w<=0`.
- **Fix:** mirror the guard in the shader before the divide:
  ```wgsl
  if w <= 0.0 {
      // Behind the projection plane: sample outside → black, matching CPU.
      let o = (gid.y * p.out_width + gid.x) * 3u;
      dst[o] = 0.0; dst[o + 1u] = 0.0; dst[o + 2u] = 0.0;
      return;
  }
  ```
  and add a GPU/CPU parity test built from `keystone_transform(_, 0.8, 0.8)` (a
  transform with `w<=0` corners) to lock it in.

### Medium

**F2 — `resample.wgsl` truncates negative source coordinates toward zero, not
toward −∞, diverging from the CPU `floor` on the boundary.**
- `resample.wgsl:50-55`: `x0 = floor(sx); ... xi = i32(x0)`. This is correct —
  `i32(floor(sx))` matches the CPU `x.floor() as i32`. **However**, the `fetch`
  bounds check (`resample.wgsl:31-37`) and the CPU `at` (`latent-cpu/src/lib.rs:279-285`)
  both reject negative indices, so the floor is consistent. On closer reading the
  floor handling itself is fine.
  The real residual difference is **NaN/Inf propagation only**, already captured by
  F1 (when `w<=0`). No separate defect here once F1 is fixed. *Downgraded to a
  note: keep `floor` before `i32` (as written) — do not switch to `i32(sx)`, which
  would truncate toward zero and split the half-open pixel cells differently from
  the CPU.*

**F3 — GPU/CPU equivalence is asserted only at the primitive level for a subset of
transforms; the full-pipeline parity test never drives GPU `resample` with a
non-trivial homography.**
- `render_matches_cpu_across_the_pipeline` (`latent-gpu/src/lib.rs:747-842`)
  always sets `lens: Some(... distortion/ca ...)`, so geometry is lowered to
  `warp` (CPU on GPU backend) — the GPU `resample` shader is never reached in the
  end-to-end test. The `straighten_degrees: 3.0` + `perspective` are folded into
  the warp homography, not a standalone resample.
- **Impact:** the GPU `resample` path that *is* used for keystone/straighten-only
  edits has no end-to-end coverage and only interior, small-perspective unit
  coverage. F1 slipped through precisely because of this gap.
- **Fix:** add a parity case with a keystone/straighten geometry and **no** lens
  block, so geometry lowers to `resample` and the GPU shader runs end-to-end.

### Low

**F4 — `map_pixels` Tone forces a CPU round-trip of the whole image buffer per
op even when the image is already on no GPU-resident texture.** Each
`run_map_pixels` (and each `run_io`) uploads the full buffer, dispatches, and
reads it back, allocating fresh `data/params/lut/staging` buffers every call
(`latent-gpu/src/lib.rs:189-260`, `282-347`). For the pipeline's typical sequence
(WB → exposure → 4 tone curves → saturation → clarity blurs → sharpen blur) this
is ~10+ full round-trips of the image over PCIe per render, plus per-call buffer
creation. **Impact:** correctness is fine; performance leaves most of the GPU's
advantage on the table (the upload/readback dominates). **Fix (later):** keep the
image resident in a single storage buffer across consecutive GPU primitives and
only read back at the end of a GPU run; reuse buffers via a small pool. Noted as a
known L2/L3 follow-up in the code comments, not a defect.

**F5 — `read_staging` uses `expect`/`panic` on poll and channel errors.**
`latent-gpu/src/lib.rs:269-272`: `poll(...).expect("GPU poll failed")`,
`rx.recv().expect(...)`, `.expect("buffer map")`. A device loss mid-render
(`Outdated`/`Lost`) becomes a panic rather than a recoverable error or CPU
fallback. **Impact:** low (device loss is rare and the app can restart), but a
render primitive panicking is worse than returning an error. **Fix:** surface a
recoverable error from the GPU primitives, or document that device loss aborts.

**F6 — `combine` is CPU-only on the GPU backend although both its inputs are
already plain f32 buffers.** Not a bug (delegation is correct and equivalent), but
`combine` (Unsharp/LocalContrast) and `apply_radial_gain` are the cheapest
primitives to port and would remove two of the CPU round-trips in the
clarity/sharpen path. Listed as a coverage/perf opportunity, not a defect.

### Nit

**F7 — `MapParams.row_stride` is mutated inside `run_map_pixels`
(`lib.rs:196`) after being built by `map_params`,** which always sets it to `0`
(lib.rs:357). Harmless (the value is only meaningful once `gx` is known), but the
field is effectively write-only in `map_params` and could be documented as
"filled in at dispatch" or moved out of the shared `map_params` builder.

**F8 — `box_blur.wgsl` recomputes `sx/sy` and re-reads the unchanged axis each
loop iteration** (`box_blur.wgsl:25-34`). The CPU `blur_axis` does the same, so
they match; a running-sum (sliding window) would be O(1)/pixel instead of
O(radius), but that optimization must be applied to *both* backends to preserve
equivalence. Perf nit only.

**F9 — `resample.wgsl`'s three `_pad0/_pad1/_pad2` fields and the Rust
`_pad: [f32; 3]`** are correct and necessary (round 52→64 bytes), but a comment
tying the 64-byte size to the uniform 16-byte-multiple requirement would help the
next editor not "simplify" them away. Same for `MapParams` at 32 bytes.

---

## 4. Concurrency-safety assessment (rayon)

**Verdict: safe. No data races, no nondeterminism, no panics-in-closures risk.**

Every parallel section was checked for disjoint writes and shared-mutable state:

- `map_pixels` (all ops), `combine`, `blend` — `pixels_mut().par_iter_mut()`
  (optionally `.zip(other.pixels().par_iter())`): each closure owns **one** output
  pixel `&mut [f32;3]`; inputs are read-only shared borrows. Disjoint. The `zip`
  of two `par_iter` keeps element `i` aligned with element `i`. (cpu/lib.rs:19-99,
  216-228.)
- `resample`, `warp`, `apply_radial_gain`, `denoise`, `dehaze`, `blur_axis` —
  `out.pixels_mut().par_chunks_mut(stride)`: each closure owns one **row** of the
  output; the source `img`/`in_px` is read-only. No two rows alias. The row index
  is recovered from `.enumerate()`, deterministic. (cpu/lib.rs:101-198, 234-261.)
- `eval_mask` — `weights.par_iter_mut().enumerate()`: writes element `i` from a
  read of `pixels[i]`; disjoint, deterministic. (cpu/lib.rs:200-214.)
- **No scatter writes** anywhere (no `out[some_computed_index] = ...` in a parallel
  closure), so no index-aliasing hazard.
- **No shared accumulators**: the bilateral `wsum`/`acc` reductions live entirely
  inside `bilateral_pixel` on the **stack** of a single pixel's computation
  (pipeline/lib.rs:525-568), so they are not cross-thread reductions at all —
  there is no float-add-order nondeterminism across threads.
- **No `unsafe`** in `latent-cpu`.
- Output ordering is fully determined by output index, so results are
  **deterministic** regardless of thread scheduling (important for the GPU/CPU
  parity tests to be stable).

One observation (not a defect): `apply_locals` calls `img.clone()` per local
adjustment (pipeline/lib.rs:636) — sequential, not a concurrency issue.

---

## 5. GPU unsafe / resource-management assessment

**Verdict: correct. No raw `unsafe`; bytemuck casts are sound; layouts match.**

- **No `unsafe` blocks** in `latent-gpu`. All GPU FFI risk is encapsulated in
  wgpu; bytemuck provides the only "transmute" and it is checked via `Pod`.
- **bytemuck `Pod`/`Zeroable` structs:** `MapParams` (8 × 4-byte scalars = 32 B,
  `#[repr(C)]`), `BlurParams` (4 × u32 = 16 B), `ResampleParams` (4×u32 + [f32;9] +
  [f32;3] = 64 B). All fields are 4-byte `u32`/`f32`, so there is **no internal
  padding** and `Pod` is correctly derivable. `cast_slice(img.pixels())` casts
  `&[[f32;3]]`→`&[f32]`: `[f32;3]` has size 12, align 4, no padding, so the cast is
  size-exact and alignment-safe (verified `ImageBuf.pixels: Vec<[f32;3]>`,
  `latent-image/src/lib.rs:13`). Readback `cast_slice_mut(img.pixels_mut())
  .copy_from_slice(&result)` is size-checked by `copy_from_slice` (panics on
  mismatch, but sizes are equal by construction). **Sound.**
- **std140 uniform layout match (the classic trap — checked closely):**
  - `MapParams` ↔ WGSL `Params` (map_pixels.wgsl:9-18): 8 scalars, contiguous at
    offsets 0,4,…,28; std140 rounds struct size up to 16 → 32 (already a multiple).
    **Match.**
  - `BlurParams` ↔ WGSL `Params` (box_blur.wgsl:6-11): 4 × u32 = 16 B. **Match.**
  - `ResampleParams` ↔ WGSL `Params` (resample.wgsl:6-25): the WGSL declares the
    homography as **nine separate `f32` scalar fields `m0..m8`**, *not* an
    `array<f32,9>` or three `vec3`. This is the key correctness choice: scalars are
    4-aligned and contiguous, so they map exactly onto the Rust `m: [f32; 9]` at
    offset 16 (out_*=16 B, m0@16…m8@48, _pad@52..64). Had they used `array<f32,9>`
    in a uniform, std140 would pad each element to a 16-byte stride and **silently
    corrupt** the matrix — they correctly avoided it. **Match.** (Worth a comment;
    see F9.)
- **Buffer sizing:** `data_buf`/`src_buf` sized from `cast_slice(...).len()` bytes;
  `dst`/`staging` sized `out_floats * 4`; `staging` for map sized `bytes.len()`.
  All exact. (lib.rs:198-225, 297-316.)
- **Usages:** storage buffers carry `STORAGE | COPY_SRC`; staging carries
  `MAP_READ | COPY_DST`; uniforms `UNIFORM`. `copy_buffer_to_buffer` then
  `map_async(Read)`. Correct and minimal.
- **Mapping lifecycle:** `read_staging` maps, polls `wait_indefinitely`, reads via
  `get_mapped_range`, **`drop(mapped)` before `staging.unmap()`** — correct order
  (unmapping while a mapped range is alive would panic). (lib.rs:263-278.)
- **Resource lifetime:** per-call buffers are dropped at function end (after the
  blocking readback completes), so nothing is freed while in flight. No leaks.
- **Workgroup rounding & bounds guards:** dispatch counts use `div_ceil`
  (lib.rs:192-196, 473, 527), so the grid covers non-multiple dimensions, and
  **every shader entry guards the tail**: `map_pixels.wgsl:56-59`
  (`i >= n_pixels`), `box_blur.wgsl:19-21` (`gid.x>=width || gid.y>=height`),
  `resample.wgsl:41-43` (same). The 2D-spill index reconstruction
  (`i = gid.y*row_stride + gid.x`, with `row_stride = gx*64`) is consistent
  between the dispatch math and the shader. **No out-of-bounds writes.**
- **Device init / no-Vulkan:** `Instance::new` is infallible in wgpu 29;
  `request_adapter` and `request_device` return `Result`, both mapped to
  `GpuUnavailable` (lib.rs:117-134). No panic on a headless/no-Vulkan host. GPU is
  optional and tests `gpu_or_skip!`. **Graceful.**

Only resource-side concern is the **per-call allocation + full round-trip** (F4)
and the **panic-on-poll-failure** (F5) — performance/robustness, not safety.

---

## 6. Test gaps

1. **(High, ties to F1/F3)** No GPU/CPU parity test drives `resample.wgsl` with a
   `w<=0` (extreme keystone) transform, nor with any transform whose output maps
   *outside* the source frame. Both the standalone resample tests and the
   end-to-end render test avoid that region (the latter routes geometry through
   `warp`/CPU via the lens block). Add: (a) a primitive parity test on
   `keystone_transform(_, 0.8, 0.8)`; (b) an end-to-end parity case with
   keystone/straighten and **no** lens, so geometry lowers to GPU `resample`.
2. **(Medium)** No GPU test feeds `resample` an output pixel that lands on/over the
   source border, where bilinear's out-of-bounds "fade to black" must match. The
   existing tests deliberately keep samples interior; border equivalence
   (the most fragile part) is untested.
3. **(Low)** No test exercises the **2D workgroup spill** path in `map_pixels`
   (`gx = max_dim`, `gy > 1`): all test images are tiny (≤ 40×30), so
   `row_stride`/`gid.y` reconstruction is never validated against a CPU reference
   on a buffer large enough to require `gy > 1`. A medium image (or a forced small
   `max_dim`) would cover it.
4. **(Low)** No test asserts the GPU `map_pixels` no-ops correctly on an **empty**
   image (`is_empty` early-return, lib.rs:450) or on a 1-pixel image.
5. **(Low)** The CPU backend has no test for `eval_mask` with a **value-driven**
   mask (luminosity/hue) at the backend level — only the position-only gradient
   (`eval_mask_produces_a_weight_ramp`). The pipeline crate covers the value path
   end-to-end, so this is minor.
6. **(Nit)** No parity test for `blur` at radius large enough to clamp the border
   on a *non-uniform* image edge (existing `blur_matches_cpu` uses a smooth ramp at
   radius 3; border-clamp agreement is implicitly covered but not targeted).

---

## 7. Positives

- **Deliberate single-source-of-truth design:** the pipeline exports the exact
  helpers backends must reuse (`bilateral_pixel`, `dehaze_*`, `midtone_weight`,
  `RadialGain::at`, `Warp::map*`, `ToneCurve::lut`, `LUMA_WEIGHTS`, `GAMMA`). The
  CPU backend reuses them verbatim, so the CPU never drifts from the contract.
- **No hard-coded-constant drift in the shaders that *are* ported:** `map_pixels.wgsl`
  hard-codes `LUMA = vec3(0.27881965, 0.72106725, 0.000113055)` which **matches
  `LUMA_WEIGHTS` bit-for-bit** (latent-image/src/color.rs:181), and reuses
  `tone::GAMMA` via the uploaded uniform. The tone path uploads `ToneCurve::lut()`
  and reproduces `eval`'s interpolation *and* the `>1.0` end-slope extrapolation
  (the highlight-headroom branch) faithfully — and there is a dedicated regression
  test for the >1.0 case (`map_pixels_tone_above_one_matches_cpu`), which the
  comment notes caught a real prior divergence.
- **Honest partial-port architecture:** unported point ops `unreachable!()` in the
  param builder *and* are intercepted by an explicit `matches!` CPU-fallback in
  `map_pixels` (lib.rs:453-461), so the `unreachable!` can never actually fire —
  defense in depth, not a latent panic.
- **Correct separable box blur** on both backends (horizontal then vertical,
  O(radius)/pixel, edge-clamp border), with matching semantics and a parity test.
- **Clean rayon usage**: disjoint output writes throughout, read-only shared input,
  no `unsafe`, deterministic output ordering.
- **Correct, careful wgpu plumbing**: graceful no-device fallback, exact buffer
  sizing, proper map/unmap ordering, tail-bounds guards in every shader, and
  std140-safe uniform structs that dodge the array-stride trap.
- **Good equivalence-test scaffolding** (`max_abs_diff`, `assert_map_matches`,
  `gpu_or_skip!`) with sensibly tiered tolerances (1e-6 gain, 1e-5 saturation/blur,
  1e-3 tone for `pow` last-bit differences, 5e-3 end-to-end).

---

### Summary of findings

| Sev | ID | Finding |
|---|---|---|
| High | F1 | `resample.wgsl` missing `w<=0` guard → CPU/GPU divergence at extreme keystone (and NaN at `w==0`), reachable & untested |
| Medium | F3 | Full-pipeline parity test never drives GPU `resample` with a real homography (routes through CPU `warp`); border/`w<=0` paths uncovered |
| Low | F4 | Per-primitive full GPU round-trip + per-call buffer allocation (perf) |
| Low | F5 | `read_staging` panics on poll/map failure instead of recoverable error |
| Low | F6 | `combine`/`apply_radial_gain` cheap-to-port but CPU-only (perf/coverage) |
| Nit | F2,F7,F8,F9 | floor-before-i32 note; `row_stride` write-only in builder; box-blur O(radius); document pad/size invariants |
