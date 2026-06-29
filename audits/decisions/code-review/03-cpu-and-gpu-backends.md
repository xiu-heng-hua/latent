# Decision Register — Code Review 03 (CPU and GPU Rendering Backends)

**Source review:** [`../../code-review/03-cpu-and-gpu-backends.md`](../../code-review/03-cpu-and-gpu-backends.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Triaged **autonomously**, in document order, optimizing each finding for quality / correctness / safety / robustness / CPU-GPU parity. Source spot-checked at `latent-gpu/src/{lib.rs,resample.wgsl}` where a decision hinged on the code.

This file records, for every finding of the CPU/GPU backend code review, whether the software will be changed or kept as-is, the rationale, and the concrete action implied. It does **not** itself modify any source. Several findings overlap with the geometry/resampling initiative already registered in [`../05-geometry-and-optics.md`](../05-geometry-and-optics.md) (§2.7, §2.9) — those are marked **Covered by** and re-registered here so the plan is complete, not double-counted.

---

## Decision summary

| Finding | Severity | Decision | Outcome |
|---|---|---|---|
| **F1** — `resample.wgsl` missing `w<=0` guard → CPU/GPU divergence at extreme keystone (NaN at `w==0`) | High | **Add `w<=0` guard** *(Covered by IP-05 §2.9)* | Change |
| **F2** — `floor`-before-`i32` on negative source coords | (note) | **Keep `floor` before `i32` (verified correct)** | No change |
| **F3** — Full-pipeline parity test never drives GPU `resample` with a real homography | Medium | **Add end-to-end + primitive parity tests over GPU resample** | Change (tests) |
| **F4** — Per-primitive full GPU round-trip + per-call buffer allocation | Low | **Keep image resident across GPU primitives; pool buffers** | Change (perf) |
| **F5** — `read_staging` panics on poll/map failure | Low | **Return a recoverable error on device loss** | Change |
| **F6** — `combine`/`apply_radial_gain` cheap-to-port but CPU-only | Low | **Port `combine` + `apply_radial_gain` to WGSL** | Change |
| **F7** — `MapParams.row_stride` is write-only in `map_params`, mutated at dispatch | Nit | **Document "filled in at dispatch"** | Change (doc) |
| **F8** — `box_blur.wgsl` O(radius)/pixel; no running-sum | Nit | **Keep O(radius) (defer sliding-window to both backends)** | No change |
| **F9** — Pad/size invariants on `ResampleParams`/`MapParams` undocumented | Nit | **Add std140-size invariant comments** | Change (doc) |
| **TG** — Test-coverage gaps (border, 2D spill, empty/1px, value-mask, blur border-clamp) | Low–Nit | **Add the targeted parity/edge tests** | Change (tests) |

**Tally:** 8 changes (1 covered by IP-05), 2 keep-as-is. The verdicts confirm the backends are high-quality (no `unsafe`, sound bytemuck/std140 layout, clean rayon, graceful no-device fallback). The substantive work is the **GPU resample correctness + parity** thread (F1/F3, shared with Initiative E/G) and a small set of robustness/perf/port follow-ups (F4–F6).

---

## Per-finding decisions

### F1 — `resample.wgsl` omits the `w<=0` guard · **Add the guard** *(High — Covered by IP-05 §2.9)*
- **Decision:** Mirror `Transform::map`'s `if w <= 0.0 { return (-1.0, -1.0); }` guard inside `resample.wgsl` before the divide, so behind-the-plane output pixels read **black** on the GPU exactly as on the CPU. For `w == 0` this also avoids `inf`/`NaN` source coords feeding `floor`/`i32` (undefined in WGSL → garbage output).
- **Why:** Confirmed at `resample.wgsl:46-48` — `w`, `sx`, `sy` are computed unconditionally with no guard, whereas the CPU reference `Transform::map` (`latent-pipeline/src/lib.rs:133-144`) guards and the CPU `resample` (`latent-cpu/src/lib.rs:101-116`) relies on it. The path is **reachable through the pipeline on GPU**: keystone/straighten with no lens block routes `apply_geometry` → `backend.resample` (the native shader). This is a real correctness/parity defect, not theoretical.
- **Action @ `latent-gpu/src/resample.wgsl:46-48`:** add, before the divide:
  ```wgsl
  if w <= 0.0 {
      let o = (gid.y * p.out_width + gid.x) * 3u;
      dst[o] = 0.0; dst[o + 1u] = 0.0; dst[o + 2u] = 0.0;
      return;
  }
  ```
- **Covered by:** [`../05-geometry-and-optics.md`](../05-geometry-and-optics.md) **§2.9** ("Add w≤0 guard + warp.wgsl"). **Note the scope is larger than the guard:** §2.9 also adds a **`warp.wgsl`** (radial Horner + per-channel CA, mirroring `Warp::map`/`map_channel`) so lens distortion/CA runs on GPU too, and **§2.7** upgrades *both* backends from bilinear to a **higher-order kernel (Lanczos/bicubic) + a minification prefilter**. So the GPU shader work here is substantial: guard **+** new warp shader **+** higher-order interpolator **+** prefilter, all kept bit-aligned with the CPU. Initiative **E** (resampling convergence) + **G** (CPU↔GPU lockstep).
- **Deps/overlaps:** Pairs with F3 (the parity test that locks it in). Must land together with IP-05 §2.7/§2.9 so the two backends never diverge mid-refactor.

### F2 — `floor`-before-`i32` on negative source coords · **Keep (verified correct)** *(note)*
- **Decision:** No change. Keep `x0 = floor(sx); xi = i32(x0)` as written (`resample.wgsl:50-55`); do **not** switch to `i32(sx)`.
- **Why:** The review downgraded its own F2 to a note: `i32(floor(sx))` matches the CPU `x.floor() as i32`, and both `fetch`'s bounds check (`resample.wgsl:31-37`) and the CPU `at` (`latent-cpu/src/lib.rs:279-285`) reject negative indices identically. The only residual difference was NaN/Inf propagation, which F1's guard eliminates. Switching to `i32(sx)` would truncate toward zero and split the half-open pixel cells differently from the CPU — a regression.
- **Action:** None. A one-line comment ("`floor` before `i32` to match CPU `floor as i32`") may be added alongside the F1 edit to prevent a future "simplification."
- **Deps/overlaps:** Resolved by F1.

### F3 — Full-pipeline parity test never drives GPU `resample` · **Add parity tests** *(Medium)*
- **Decision:** Add GPU/CPU parity coverage that actually exercises the native resample shader: (a) a **primitive** parity test on a `w<=0` transform (e.g. `keystone_transform(_, 0.8, 0.8)`), and (b) an **end-to-end** parity case with keystone/straighten geometry and **no lens block**, so `apply_geometry` lowers to GPU `resample` rather than CPU `warp`.
- **Why:** Confirmed: `render_matches_cpu_across_the_pipeline` (`latent-gpu/src/lib.rs:747-842`) always sets a `lens` block, so geometry lowers to `warp` (CPU on the GPU backend) and the GPU resample shader is never reached end-to-end; the standalone resample tests use only mild interior perspective. This is exactly why F1 slipped through — it is the **equivalence-test gap** the brief calls out as a code-review item.
- **Action @ `latent-gpu/src/lib.rs` (tests, near `:747-842`):** add both cases; assert against the CPU backend at the end-to-end tolerance. Build the transforms from the existing `keystone_transform` helper so the `w<=0` corners are exercised.
- **Deps/overlaps:** Locks in F1 and the IP-05 §2.9 guard/warp/interpolator work. This is the test half of Initiatives **E** and **G** — IP-05 §2.9's action already names "add a CPU/GPU equivalence test over perspective + distortion (the existing end-to-end test never exercises the resample shader)". Register here as the code-review owner of that test gap; implement once.

### F4 — Per-primitive full GPU round-trip + per-call allocation · **Keep image resident; pool buffers** *(Low)*
- **Decision:** Fix it. Keep the image resident in a single storage buffer across consecutive GPU primitives and read back only at the end of a GPU run; reuse `data/params/lut/staging` buffers via a small pool instead of allocating fresh ones per call.
- **Why:** Confirmed at `latent-gpu/src/lib.rs:189-260` / `282-347`: every `run_map_pixels`/`run_io` uploads the full buffer, dispatches, and reads it back, allocating new buffers each call. For a typical render (WB → exposure → 4 tone curves → saturation → clarity/sharpen blurs) that is 10+ full PCIe round-trips plus per-call allocation — the upload/readback dominates and erases most of the GPU advantage. Correctness is unaffected, but "Low → Fix" applies: this is a genuine, worthwhile performance win and the review flags it as a known L2/L3 follow-up.
- **Action @ `latent-gpu/src/lib.rs`:** introduce a persistent device-resident image buffer threaded through the GPU primitive sequence; a buffer pool keyed by size; single readback at run end. Sequence after F5 (so the resident-buffer path returns errors recoverably) and after F1/F3 (so parity tests guard the refactor).
- **Deps/overlaps:** Interacts with F6 (more ported primitives → more round-trips eliminated) and Initiative **G** (parity tests must stay green across the buffer-residency change).

### F5 — `read_staging` panics on poll/map failure · **Return a recoverable error** *(Low)*
- **Decision:** Fix it. Surface device loss (`Outdated`/`Lost`) and channel errors as a recoverable error from the GPU primitives instead of `expect`/`panic`; on such an error the backend should fall back to CPU (or propagate `GpuUnavailable`-style) rather than abort the render.
- **Why:** Confirmed at `latent-gpu/src/lib.rs:269-272`: `poll(...).expect("GPU poll failed")`, `rx.recv().expect(...).expect("buffer map")`. A render primitive panicking on a rare-but-real device loss is strictly worse than a recoverable error — robustness, per the brief's explicit ask. The graceful no-device path already exists at init (`GpuUnavailable`, `lib.rs:117-134`); this extends the same posture to mid-render loss.
- **Action @ `latent-gpu/src/lib.rs:263-278`:** change `read_staging` to return `Result<Vec<f32>, _>`; map `PollError`/channel-closed/`BufferAsyncError` to a recoverable backend error; thread the `Result` up through `run_map_pixels`/`run_io`/the `Backend` impls so a lost device degrades to CPU instead of panicking.
- **Deps/overlaps:** Pairs with F4 (the resident-buffer refactor touches the same functions). Initiative **F** (robustness) in spirit, though F is otherwise sidecar-sanitize.

### F6 — `combine`/`apply_radial_gain` CPU-only on GPU backend · **Port to WGSL** *(Low)*
- **Decision:** Fix it. Port `combine` (Unsharp/LocalContrast) and `apply_radial_gain` to WGSL. Both are per-pixel ops on plain f32 buffers and are the cheapest primitives to port; doing so removes two CPU round-trips from the clarity/sharpen path.
- **Why:** Not a bug (delegation is correct and equivalent), but "Low → Fix unless not worth it" — and these *are* worth it: they sit in the hot clarity/sharpen path, are trivial element-wise/radial kernels, and each currently forces a GPU→CPU→GPU bounce. Porting them improves CPU/GPU coverage (Initiative G) and compounds with F4's resident-buffer win.
- **Action @ `latent-gpu/src/lib.rs` + new shaders:** add `combine.wgsl` (two-input element-wise unsharp/local-contrast) and fold radial gain into a small shader (or reuse `map_pixels` machinery); add `combine_matches_cpu` / `apply_radial_gain_matches_cpu` parity tests. `apply_radial_gain` should share the `RadialGain::at` math via the pipeline helper to avoid drift.
- **Deps/overlaps:** Coordinate with IP-05 §2.8 (vignetting/`RadialGain` convention) so the ported radial-gain shader uses the corrected convention, not the current one. Initiatives **E**/**G** (more native parity); compounds with F4.

### F7 — `MapParams.row_stride` write-only in `map_params` · **Document "filled in at dispatch"** *(Nit)*
- **Decision:** Fix as a documentation nit. Keep the field where it is (it must live in the shared uniform), but document that `map_params` sets it to `0` as a placeholder and `run_map_pixels` fills it at dispatch once `gx` is known.
- **Why:** Confirmed: `map_params` always sets `row_stride = 0` (lib.rs:357) and `run_map_pixels` overwrites it with `gx * MAP_WORKGROUP` (lib.rs:196). Harmless but genuinely confusing — a one-line comment is a real readability improvement.
- **Action @ `latent-gpu/src/lib.rs:357` and `:196`:** add a doc-comment on the field / at both sites ("placeholder 0; filled in at dispatch from the 2D grid width").
- **Deps/overlaps:** None.

### F8 — `box_blur.wgsl` O(radius)/pixel · **Keep (defer sliding-window)** *(Nit)*
- **Decision:** Keep as-is for now. Do **not** convert to a running-sum here unilaterally.
- **Why:** Confirmed `box_blur.wgsl:25-34` recomputes the window each iteration, and crucially the CPU `blur_axis` does the same — so they **match**, which is the property we must not break. A sliding-window O(1)/pixel optimization is a real win but **must be applied to both backends together** to preserve equivalence (Initiative G), making it a larger, deliberate task rather than a backend-local nit. Keeping it avoids introducing a CPU/GPU divergence for a perf-only gain.
- **Action:** None now. If pursued, schedule as a paired CPU+GPU change with a re-run of `blur_matches_cpu`.
- **Deps/overlaps:** Initiative **G** (any blur change is dual-backend).

### F9 — Pad/size invariants undocumented · **Add std140 comments** *(Nit)*
- **Decision:** Fix as a documentation nit. Add comments tying `ResampleParams`'s `_pad0/_pad1/_pad2` + Rust `_pad: [f32;3]` (and `MapParams` at 32 B) to the std140 16-byte-multiple uniform requirement, so a future editor doesn't "simplify" the padding away and silently corrupt the layout.
- **Why:** The review's §5 verified the layout is **correct** and deliberately dodges the `array<f32,9>`/`vec3` std140 stride trap (scalars `m0..m8` are 4-aligned and contiguous). The padding is load-bearing but non-obvious; a comment is cheap insurance against a regression that would be hard to debug.
- **Action @ `latent-gpu/src/resample.wgsl:22-24` and the Rust `ResampleParams`/`MapParams` definitions:** add "rounds 52→64 B to satisfy std140 16-byte struct alignment; do not remove" (and the analogous note for `MapParams`).
- **Deps/overlaps:** None. Touch alongside the F1 shader edit since it is the same file.

### TG — Remaining test-coverage gaps · **Add the targeted tests** *(Low–Nit)*
- **Decision:** Fix the worthwhile ones. Beyond F3's resample parity tests, add: (2) a GPU resample border test where output maps **on/over** the source border (bilinear "fade to black" must match — the most fragile region, currently kept interior); (3) a `map_pixels` **2D workgroup-spill** test (`gy > 1`, via a larger image or a forced small `max_dim`) to validate the `row_stride`/`gid.y` index reconstruction against the CPU; (4) **empty** and **1-pixel** image no-op tests for GPU `map_pixels`. Keep (5) backend-level value-driven `eval_mask` and (6) blur border-clamp on a non-uniform edge as **optional** — both are already covered end-to-end in the pipeline crate (low value at the backend level).
- **Why:** Border equivalence and the 2D-spill reconstruction are real, currently-unverified code paths; the empty/1px guard (`is_empty` early-return, lib.rs:450) is a cheap correctness lock. (5)/(6) are genuinely redundant with pipeline coverage, so "Low → Keep, not worth it" applies to those two.
- **Action @ `latent-gpu/src/lib.rs` (tests):** add tests (2)–(4); leave (5)/(6) noted as covered elsewhere.
- **Deps/overlaps:** (2) overlaps F1/F3/IP-05 §2.9 (border behavior changes again under the Lanczos/prefilter upgrade — write the border test against the *final* interpolator, or update it when §2.7 lands). Initiative **G**.

---

## Resulting implementation notes (derived from the decisions)

The backend findings split into one shared correctness/parity thread and a few independent follow-ups. A sensible order:

1. **F5** — make `read_staging` (and the primitives that call it) return a recoverable error; lost device → CPU fallback, not panic. Do first: it changes the signatures the later refactors thread through.
2. **F1 + F3 + TG(2)** — **Covered by IP-05 §2.9 / §2.7.** Add the `w<=0` guard to `resample.wgsl`, then (with §2.9/§2.7) the **`warp.wgsl`** shader and the **higher-order (Lanczos/bicubic) interpolator + minification prefilter** on *both* backends, and the GPU/CPU parity tests (primitive `w<=0` + end-to-end keystone/straighten with no lens + border). This is the heart of the work and the explicit CPU↔GPU lockstep point — keep `blur_matches_cpu`/resample parity green throughout.
3. **F4** — resident image buffer across GPU primitives + buffer pool; single readback per run. Guarded by step 2's parity tests.
4. **F6** — port `combine` + `apply_radial_gain` to WGSL (radial gain using the corrected IP-05 §2.8 convention); compounds with F4.
5. **TG(3)(4)** — 2D-spill and empty/1px GPU tests.
6. **Doc nits F7, F9** — fold into the relevant edits (F9 alongside the F1 shader change; F7 at the `map_params`/dispatch sites). **F8 and F2 are no-ops** (keep; F8 deferred as a dual-backend change, F2 verified correct).

**Cross-refs:**
- **Initiative E (resampling/minification convergence)** and **Initiative G (CPU↔GPU lockstep):** F1, F3, and TG(2) are the code-review face of [`../05-geometry-and-optics.md`](../05-geometry-and-optics.md) **§2.9** (w≤0 guard + `warp.wgsl` + equivalence test) and **§2.7** (higher-order interpolation + prefilter on both backends). The GPU shader work is therefore guard **+** warp shader **+** Lanczos/bicubic **+** prefilter — implement the interpolator + prefilter **once**, mirrored on both backends, and never let the two drift (the render-equivalence test is the guard rail). See [`../README.md`](../README.md) → Initiatives **E** and **G**.
- **Initiative C (lensfun):** F6's `apply_radial_gain` port should adopt the IP-05 §2.8 vignetting convention rather than the current one.
- **Verified-correct positives kept as-is:** no `unsafe`; sound bytemuck `Pod`/`cast_slice`; std140-safe scalar uniform layout; clean disjoint-write rayon (no scatter writes, no shared accumulators, deterministic ordering); graceful no-Vulkan init (`GpuUnavailable`); exact buffer sizing; correct map/unmap order; tail-bounds guards in every shader; faithful single-source-of-truth helper reuse and tone `>1.0` headroom regression test. These are the backbone the changes above must preserve.

This register reflects intent only; nothing here has been implemented.
