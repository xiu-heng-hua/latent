# Decision Register — Code Review 02 (Pipeline & Edit Model)

**Source review:** [`../../code-review/02-pipeline-and-edit-model.md`](../../code-review/02-pipeline-and-edit-model.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Triaged **autonomously**, in severity order, optimizing for correctness / safety / robustness. Each finding has an explicit Fix/Keep decision, rationale, and concrete action at `file:line`. Findings that overlap already-decided image-processing registers are marked **Covered by …** and cross-referenced (registered here, not re-planned).

This file records, for every finding of the pipeline/edit-model code review, whether the software will change or stay as-is, why, and the concrete action implied. It does **not** itself modify any source. Scope is engineering correctness, robustness, serde/forward-compat, undo/redo, abstraction design, and test coverage — *image-processing algorithm* correctness is out of scope here (see Registers 01–05).

---

## Decision summary

| Finding | Severity | Decision | Outcome |
|---|---|---|---|
| **H1** — NaN-x curve point panics `render()` (`pipeline:604-623`, panic @618) | High | **Make `point_curve` total + sanitize on load** | Change |
| **H2** — Undo history grows unbounded (`history.rs:11-56`) | High | **Bound the stack (VecDeque + cap)** | Change |
| **M1** — Mask-shape structs reject new/missing fields (`edit:187,203,232,254,285`) | Medium | **Uniform `#[serde(default)]` + `Default`** | Change |
| **M2** — No value validation on load (`edit:593-613`) | Medium | **`Settings::sanitize` on load (Initiative F)** | Change |
| **M3** — `Backend` non-`Send`/`Sync`, per-local full-image clones (`pipeline:297-341,636`) | Medium | **Add `Send+Sync` now; document clone cost; defer masked-bounds opt** | Change (partial) |
| **M4** — `TestBackend` vs `CpuBackend` unparity (`pipeline:796-994` vs `cpu:18+`) | Medium | **Shared conformance harness over `&dyn Backend`** | Change |
| **L1** — Degenerate (zero-length) gradient selects nothing (`edit:241-248`) | Low | **Keep behavior; add doc note** | Change (doc) |
| **L2** — Feather params trust sign not finiteness (`edit:148,216,262,294`) | Low | **Covered by M2 sanitize; add local NaN guard** | Change |
| **L3** — `current_mut` lets an edit escape the transaction (`history.rs:34`) | Low | **Add `mutate(|s| …)` helper + loud doc** | Change |
| **L4** — `lens_radial` `inv_norm` from `2.0/w.min(h)` (`pipeline:672-677`) | Low | **`.max(1.0)` + debug-assert** | Change |
| **N1** — Public enums not `#[non_exhaustive]` (`pipeline:25,46`; `edit:99,112`) | Nit | **Mark `#[non_exhaustive]` (keep internal wildcard discipline)** | Change |
| **N2** — Missing `#[must_use]` on pure ctors/queries | Nit | **Add `#[must_use]`** | Change |
| **N3** — `to_ron`/`from_ron` stringly-typed errors (`edit:593-613`) | Nit | **Typed `SidecarError` (preserve span + version case)** | Change |
| **N4** — `select_luma` vs `luminance` name clash (`edit:142-144`) | Nit | **Rename `select_brightness`** | Change |

**Tally:** **14 Fix, 0 pure-Keep.** Two findings (L1, M3) are "Fix" only in the doc/partial sense — the runtime behavior they describe is deliberately kept, but each still gets a concrete change (doc note / `Send+Sync` bound). Verified-correct items from §6 Positives are affirmed as **Keep** in the notes section but are not findings to action.

**Overlaps with image-processing registers (deduped, not re-planned):**
- **H1 / M2 / L2** ↔ **IP-03 §2.2** (sanitize `SelectiveTone` on load) and **Initiative F** (robustness / sanitize-on-load). The broad "validate/sanitize *all* settings on deserialization" fix is owned **here** (M2); IP-03 only sanitizes the tone struct.
- **H1 interpolation rewrite** ↔ **IP-03 §2.8** (`point_curve` → monotone-cubic/PCHIP). Same function (`pipeline:604-623`). The *safety* half of H1 (finite-only points, total search) is owned here; the *interpolation-quality* half is owned by §2.8. **Do them in one edit.**
- **M4 parity** ↔ **IP-05 §2.9** (CPU/GPU resample equivalence) and **Initiative E/G** (resampling + GPU parity). The `TestBackend`↔`CpuBackend` conformance harness is the in-pipeline counterpart of the CPU↔GPU equivalence test.

---

## Per-finding decisions

### H1 — NaN-x curve point panics `render()` · **Make `point_curve` total + sanitize on load** *(High)*
- **Decision:** **Fix, two layers.** (1) Make `point_curve` *total*: drop non-finite control points and clamp x/y to `[0,1]` before sorting; if empty after the drop, return `ToneCurve::identity()`; replace the fragile `windows(2).position(|w| t <= w[1].0).unwrap()` with a saturating search (`.unwrap_or(last - 1)`) that can never return `None`. (2) Stop non-finite floats entering the model at all — see **M2**.
- **Why:** Confirmed reachable panic at `pipeline:618`. `total_cmp` orders `NaN` last, so the high-end guard `t >= pts[last].0` is `t >= NaN == false` and never fires; for `t` past the largest *real* x, `position` returns `None` and `.unwrap()` crashes at render time (sampled across `[0,1]` by `ToneCurve::from_fn`). A `NaN` reaches here trivially — RON accepts `NaN`/`inf` verbatim, so a corrupt or hand-edited sidecar is a direct DoS on opening the editor. Defense in depth (total function *and* sanitize-on-load) is correct: the function must not panic even if a caller mis-sets a point programmatically.
- **Action @ `latent-pipeline/src/lib.rs:604-623`:** rewrite `point_curve` per above. **Overlap — do in one edit with IP-03 §2.8:** §2.8 already replaces this interpolator (piecewise-linear → monotone-cubic/PCHIP); fold the finiteness/clamp/total-search guards into that same rewrite. Keep clamp-flat-past-ends and empty-is-identity. **Tests:** empty, single, unsorted, duplicate-x (regression: must stay finite), and **NaN/inf x or y** (regression: no panic).
- **Note (not a bug):** the prompt's suspected duplicate-x divide-by-zero in `(t-x0)/(x1-x0)` is **verified not reachable** — the `position(|w| t <= w[1].0)` search selects the *earlier* window at a duplicate x, never a zero-width segment (100k-sample sweep produced zero non-finite outputs). No action for that; keep a duplicate-x regression test to pin it.
- **Deps/overlaps:** Covered-by-companion **IP-03 §2.8** (interpolation), **IP-03 §2.2 / M2 / Initiative F** (sanitize-on-load second layer).

### H2 — Undo history grows unbounded · **Bound the stack (VecDeque + cap)** *(High)*
- **Decision:** **Fix.** Back `undo` with a `VecDeque<T>` and a `capacity` (default 100); on `commit`, evict the oldest (`pop_front`) once over capacity. Losing the ability to undo a ~100-step-old edit is the correct trade; unbounded RAM is not.
- **Why:** Confirmed — 10,000 commits retain 10,000 deep-cloned `Settings` with no eviction. Each entry clones every `Vec<LocalAdjustment>` / brush-dab list, so with large brush masks this is a real per-gesture leak, not theoretical, over a long session.
- **Action @ `latent-edit/src/history.rs:11-56`:** change `undo: Vec<T>` → `undo: VecDeque<T>`; add `capacity: usize` (constructor default `DEFAULT_CAP = 100`, plus a `with_capacity` ctor for callers that want more); in `commit`, after `push_back`, `while self.undo.len() > self.capacity { self.undo.pop_front(); }`. `undo()`/`redo()`/`current` logic is otherwise correct (verified) and unchanged. **Test:** commit `> capacity` edits, assert the stack is bounded and the oldest is evicted (and that undo still works for the most-recent `capacity` steps).
- **Deps/overlaps:** none. Pure code-review item.

### M1 — Sidecar mask-shape structs reject new/missing fields · **Uniform `#[serde(default)]` + `Default`** *(Medium)*
- **Decision:** **Fix.** Give the mask-shape leaf structs the same forward-compat treatment as the adjustment structs: derive or impl `Default` (with neutral values) and add `#[serde(default)]` on each field (or `#[serde(default)]` on the struct where supported), so a sidecar missing a field falls back to neutral instead of erroring.
- **Why:** Confirmed asymmetry — `Adjustments`/`Settings`/`Geometry`/… carry `#[serde(default)]` and tolerate missing *and* unknown-future fields, but `LuminanceRange`/`ColorRange`/`Gradient`/`Radial`/`Dab` have neither, so a `Gradient` with `x1` omitted hard-errors (`Unexpected missing field named x1`). The day any of these shapes gains a field (brush hardness, gradient midpoint), every previously-saved sidecar containing that shape fails to load — a silent regression the version gate won't catch (still version-1 data the build should understand). A half-applied forward-compat story is a latent trap.
- **Action @ `latent-edit/src/lib.rs:187,203,232,254,285`** (`LuminanceRange`, `ColorRange`, `Gradient`, `Radial`, `Dab`): add `Default` + `#[serde(default)]`. Choose neutral defaults deliberately (e.g. a `Gradient`/`Radial` default that selects nothing or a benign full-frame, documented). **Test:** load each shape with a field omitted; assert it fills the default rather than erroring.
- **Deps/overlaps:** none new; complements M2 (which sanitizes the *values* these defaults produce).

### M2 — No value validation on load · **`Settings::sanitize` on load** *(Medium)* — **Covered by Initiative F (broadest owner is here)**
- **Decision:** **Fix — and this register owns the broad version.** Add a `Settings::sanitize(&mut self)` that (a) replaces every non-finite float with its neutral value, and (b) clamps documented ranges (`opacity ∈ [0,1]`, adjustment ranges ≈ `[-1,1]`, feather `≥ 0`, etc.), recursing into locals, masks, dabs, and curve points. Call it from `Document::from_ron` after version gating, over every `Settings` in `variants`.
- **Why:** Confirmed — `from_ron` does version gating only; `opacity: 5.0`, `exposure: Some(NaN)`, `Some(inf)` all load verbatim. The documented invariants are advisory; corrupt/adversarial files propagate bad values into math. This is the upstream cause of **H1** and **L2**, and `inf`/huge values otherwise silently produce black/blown renders that are hard to diagnose. A single sanitize pass is the highest-leverage robustness fix in this register — it neutralizes H1's second layer, M2, and L2 at once.
- **Action @ `latent-edit/src/lib.rs:593-613`** (`from_ron`) + a new `sanitize` over the `Settings` tree. Make `point_curve` still total (H1 layer 1) so a programmatic mis-set after load is also safe — sanitize-on-load and the total function are complementary, not redundant. **Tests:** non-finite → neutral; out-of-range `opacity`/adjustments → clamped; a malformed/garbage-RON test asserting a clean `Err` (no panic).
- **Deps/overlaps:** **Covered by Initiative F** and **IP-03 §2.2** (which sanitizes only `SelectiveTone`). This finding is the *superset*: sanitize/validate **all** settings on deserialization, not just the tone struct. IP-03 §2.2 should defer to this pass for its NaN/inf handling rather than implementing a second one.

### M3 — `Backend` non-`Send`/`Sync` + per-local full-image clones · **Add `Send+Sync` now; document clone cost; defer masked-bounds opt** *(Medium)*
- **Decision:** **Fix (partial), in three parts.**
  1. **Add `pub trait Backend: Send + Sync`** now. Adding it later is a breaking change to every impl; a GPU/threaded backend will want it, and `CpuBackend` already parallelizes internally — so the bound is correct and free today.
  2. **Document the per-local `img.clone()` cost** on `apply_locals` as a known, structurally-necessary cost of the current blend-the-whole-image design.
  3. **Defer** the "blend within the mask's bounding region only" optimization — it's a real win (most masks cover a fraction of the frame) but a non-trivial design change; capture it as a tracked follow-up, not part of this triage.
- **Why:** The `Send+Sync` omission is the most likely future *breaking* change and costs nothing to fix now (the engineering-correct call). The clones are a genuine per-tick cost (`N` full-frame allocations + `source.clone()` + per-stage buffers) but not a correctness bug, and reducing them properly means scoping work to mask bounds — worth doing, not worth rushing under a robustness triage.
- **Action @ `latent-pipeline/src/lib.rs:297-341`** (add supertrait), **`:636`** (doc the clone), follow-up issue for masked-bounds blending. Re-run `cargo check` across all backend impls after adding the supertrait.
- **Deps/overlaps:** the masked-bounds optimization touches the same `apply_locals`/`blend` seam as future perf work; keep it independent of M4.

### M4 — `TestBackend` / `CpuBackend` unparity · **Shared conformance harness** *(Medium)* — **pairs with IP-05 §2.9 / Initiative E**
- **Decision:** **Fix.** Extract a backend-conformance test harness — a set of assertions parameterized over `&dyn Backend` — and run it against **both** `TestBackend` and `CpuBackend`. Cover the structural primitives each currently reimplements: `map_pixels`, `blur`, `combine`, `resample`, `warp`, `blend`, `eval_mask`. Where exact agreement is intended, assert equality; where `TestBackend` deliberately simplifies (nearest-neighbor resample vs the real bilinear/Lanczos), either upgrade `TestBackend` to match or assert with a documented tolerance — and flag the difference in `TestBackend`'s doc comment either way.
- **Why:** The shared *math kernels* (`bilateral_pixel`, `dehaze_recover`, `dehaze_dark_channel`, `midtone_weight`) are correctly factored and reused — good — but the *structural* primitives are written twice with nothing verifying the "same semantics" claim in `TestBackend`'s own doc. The nearest-vs-bilinear gap means pipeline tests can pass while masking a real-backend regression. The contract is asserted only in prose today.
- **Action:** add the harness in `latent-pipeline` (or a shared test module), run over both backends; minimum bar is a doc comment on `TestBackend` (`pipeline:796-994`) flagging the drift risk and the nearest-vs-bilinear difference. Coordinate with **IP-05 §2.9**'s CPU/GPU equivalence test so the *three* backends (Test, CPU, GPU) share one conformance contract once the GPU `warp.wgsl` lands.
- **Deps/overlaps:** **Pairs with IP-05 §2.9 / Initiative E/G** (CPU↔GPU resample equivalence). The interpolator upgrade (§2.7) will change what "agree" means for `resample`/`warp`, so build the harness to take a tolerance and land it alongside, not before, the §2.7/§2.9 interpolation change.

### L1 — Degenerate zero-length gradient selects nothing · **Keep behavior; add doc note** *(Low)*
- **Decision:** **Keep the runtime behavior** (a collapsed gradient → all-zero mask is a safe, no-crash choice), **but add a doc note** that a degenerate gradient is treated as empty so the silent all-zero result is documented, not surprising.
- **Why:** The `len2 <= 1e-12 → 0.0` guard is correct and intentional; the only gap is discoverability. Not worth changing behavior (returning full-weight or erroring would both be more surprising).
- **Action @ `latent-edit/src/lib.rs:241-248`:** add the doc note to `Gradient::weight_at` / `Gradient`. Sanitize-on-load (M2) is unaffected — a zero-length gradient is finite.
- **Deps/overlaps:** none.

### L2 — Feather params trust sign not finiteness · **Covered by M2; add local NaN guard** *(Low)*
- **Decision:** **Fix via M2** (sanitize-on-load makes `feather` finite before it reaches these branches), **plus** a cheap belt-and-braces NaN guard at the use sites so a programmatically-injected `NaN` can't slip the `feather <= 0.0` test (NaN compares false → divide branch → `NaN`, which `f32::clamp` does **not** sanitize).
- **Why:** Same NaN-survives-clamp root cause as H1. Downstream `blend` clamps to `[0,1]` in both backends so there's no crash today, but the selection is silently wrong. M2 is the primary fix; a local `if !feather.is_finite() || feather <= 0.0 { hard }` at each site is a one-line defense that keeps these functions correct regardless of caller.
- **Action @ `latent-edit/src/lib.rs:148-155` (`band`), `262-270` (`Radial`), `294-312` (`Brush`), `216-227` (`ColorRange`):** widen the `feather <= 0.0` test to also catch non-finite. **Test:** NaN feather → hard edge, not NaN weight.
- **Deps/overlaps:** **Covered by M2 / Initiative F.**

### L3 — `current_mut` lets an edit escape the transaction · **Add `mutate(...)` helper + loud doc** *(Low)*
- **Decision:** **Fix.** Add a `mutate(&mut self, f: impl FnOnce(&mut T))` helper that brackets the gesture (`begin` → `f(&mut current)` → `commit`), and document `current_mut` loudly as "must be wrapped in `begin`/`commit`; prefer `mutate`." Prefer the explicit helper over auto-`begin` inside `current_mut` (auto-`begin` would make every read-modify accidentally open a gesture, which is its own footgun).
- **Why:** `current_mut` hands out `&mut T` with no `begin` required; an unbracketed mutation is applied but never recorded as undo, and a later `commit` with stale/absent `pending` won't capture it. The transaction discipline is convention-only and the unpaired path is untested. A `mutate` helper makes the correct path the easy path without removing the escape hatch.
- **Action @ `latent-edit/src/history.rs:34,40-56`:** add `mutate`; expand the `current_mut` doc. **Test:** `mutate` records exactly one step iff the closure changed state; document/test the unbracketed `current_mut` path's behavior.
- **Deps/overlaps:** none.

### L4 — `lens_radial` `inv_norm` from `2.0/w.min(h)` · **`.max(1.0)` + debug-assert** *(Low)*
- **Decision:** **Fix (defensive).** Clamp the divisor with `w.min(h).max(1.0)` (or `.max(1)` on the integer dimension) and add a `debug_assert!` that dimensions are `≥ 1`, making the ≥1×1 invariant explicit and future-proofing against a `0`-dimension image (`inv_norm = inf`).
- **Why:** Not currently reachable (`cropped` floors at 1, images are ≥1×1), but the intent is implicit and a future code path could produce a 0 dimension; the guard is free and documents the assumption. `keystone_transform` already guards `cx/cy > 0.0` — this makes `lens_radial` consistent with it.
- **Action @ `latent-pipeline/src/lib.rs:672-677`** (and check the analogous `keystone_transform` half-extent at `:652-666`). **Test:** a 1×1 image renders without `inf`/NaN through the geometry stage.
- **Deps/overlaps:** none.

### N1 — Public enums not `#[non_exhaustive]` · **Mark `#[non_exhaustive]`** *(Nit)*
- **Decision:** **Fix.** Mark `PointOp`, `CombineKind`, `MaskShape` `#[non_exhaustive]`. Their own doc comments say "more variants are added over time," and these are the operations-as-data vocabulary a future plugin/GPU backend would `match` on — so adding a variant should not be a breaking change downstream.
- **Why:** The deliberate trade is acknowledged: `#[non_exhaustive]` forces in-crate matches to add a wildcard arm, slightly weakening exhaustiveness checking. Given the documented intent to grow these enums and the goal of supporting external backends, forward-compat wins. Keep internal matches exhaustive-by-discipline (handle every known variant explicitly, then a `_ => unreachable!()`/neutral arm) so a *new* variant still surfaces in review.
- **Action @ `latent-pipeline/src/lib.rs:25,46`** and **`latent-edit/src/lib.rs:99,112`:** add the attribute; add wildcard arms where the compiler now requires them.
- **Deps/overlaps:** touches the same enums the M4 conformance harness exercises — land N1 first so the harness matches on the `#[non_exhaustive]` shape.

### N2 — Missing `#[must_use]` on pure ctors/queries · **Add `#[must_use]`** *(Nit)*
- **Decision:** **Fix.** Add `#[must_use]` to the pure, side-effect-free constructors/queries: `Transform::{identity,rotation,compose}`, `Warp::from_transform`, `RadialGain::at`, `Mask::weight_at`, `Geometry::is_identity`, `History::{can_undo,can_redo,is_idle}`.
- **Why:** Low value but idiomatic and free; catches accidental discards of a computed value (a real class of bug for query methods like `is_identity`/`can_undo`).
- **Action:** annotate the listed methods across `latent-pipeline`/`latent-edit`.
- **Deps/overlaps:** none.

### N3 — Stringly-typed sidecar errors · **Typed `SidecarError`** *(Nit)*
- **Decision:** **Fix.** Replace `Result<_, String>` on `to_ron`/`from_ron` with a typed `enum SidecarError { Parse(ron::error::SpannedError), VersionTooNew { found: u32, max: u32 } }` (via `thiserror`). Preserves the parse span (line/column/kind) for UI surfacing and lets the UI distinguish a *data* condition (version-too-new) from a *parse* failure.
- **Why:** The version-too-new case in particular is a recoverable data condition a UI may want to message differently ("saved by a newer version") versus "corrupt file." Stringly-typed errors throw that structure away. Small, idiomatic improvement that improves the editor's error UX.
- **Action @ `latent-edit/src/lib.rs:593-613`:** define `SidecarError`; change the two signatures; update call sites. Sequence with **M2** (the sanitize pass can report what it scrubbed) and **M1**, since both touch `from_ron`.
- **Deps/overlaps:** shares `from_ron` with M1, M2.

### N4 — `select_luma` vs `luminance` name clash · **Rename `select_brightness`** *(Nit)*
- **Decision:** **Fix.** Rename the Rec.709 mask-selection luma (`select_luma`) to `select_brightness` so the two distinct "luminance" notions in adjacent code don't read as the same thing.
- **Why:** The divergence (Rec.709 select-luma vs colorimetric `latent_image::color::luminance`) is intentional and documented, but two "luminance" names in neighboring code is a readability trap. A distinct name removes the trap with zero behavior change.
- **Action @ `latent-edit/src/lib.rs:142-144`** and its call sites (`LuminanceRange::weight_at` at `:196`, the doc cross-ref at `:186`). Pure rename. **Note:** if Register 01 §5 / Initiative A reworks the colorimetric `luminance` to L\*, the *select* brightness here is independent and stays Rec.709 by design — the rename makes that independence legible.
- **Deps/overlaps:** light cross-ref with **IP Register 01 §5** (perceptual-lightness) — no shared code, just naming clarity.

---

## Resulting implementation notes

The robustness fixes cluster around two seams (the curve evaluator + sidecar load, and the history stack); the rest are localized.

**Suggested order:**
1. **M2 `Settings::sanitize` on load** — the highest-leverage robustness fix; neutralizes H1's second layer and L2, and is the in-pipeline home of **Initiative F**. Land it with M1 and N3 (all touch `from_ron`).
2. **H1 `point_curve` total + finiteness guard** — fold into **IP-03 §2.8**'s monotone-cubic rewrite of the same function (one edit). Keeps the function panic-free even if a caller mis-sets a point after load.
3. **M1 uniform `#[serde(default)]` + `Default`** on mask-shape leaf structs; add forward-compat tests.
4. **H2 bounded history** (`VecDeque` + cap) — independent; do early, it's self-contained.
5. **M3** add `Backend: Send + Sync`; document the `apply_locals` clone; file the masked-bounds optimization as follow-up.
6. **M4 conformance harness** over `&dyn Backend` — build to take a tolerance and land **alongside IP-05 §2.9 / §2.7** so Test/CPU(/GPU) share one contract once the interpolator upgrade and `warp.wgsl` exist.
7. **L1/L3/L4** doc-and-guard fixes; **N1/N2/N3/N4** idiom nits — batch as low-risk cleanup.

**Cross-refs (deduped — registered here, planned elsewhere):**
- **Initiative F (sanitize-on-load):** M2 owns the broad "validate/sanitize *all* settings on deserialization" pass; **IP-03 §2.2** sanitizes only `SelectiveTone` and should defer to M2's pass. H1 layer 2 and L2 are subsumed by it.
- **IP-03 §2.8 (`point_curve` → monotone-cubic):** same function as H1; do the safety guards and the interpolation upgrade in one rewrite (`pipeline:604-623`).
- **IP-05 §2.9 / Initiative E/G (CPU↔GPU resample equivalence):** M4's backend-conformance harness is the in-pipeline counterpart; converge so Test/CPU/GPU share one parity contract after the §2.7 interpolator change.

**Verified-correct (kept, affirming §6 Positives — no action):**
- Undo/redo core logic — symmetric `mem::replace` swaps, redo cleared on new edit, change-detected commits, no off-by-one, empty-stack handled. (Only the missing cap — **H2** — is a defect.)
- The `commit` let-chain (`if let Some(prev) = self.pending.take() && prev != self.current`) — `take()` always runs (clears `pending` on a no-op); `&&` only gates the record. Correct.
- Sidecar versioning (explicit version, reject-newer, default-fill-older, unknown-field-ignore) — correct; **M1** just applies it uniformly.
- Single-interpolation geometry (`apply_geometry` folds lens + keystone + straighten + CA into one `Warp`) and the `w <= 0` perspective-divide guard — correct, tested.
- `Warp` vs `Transform` split and `Warp::from_transform` equivalence — correct, tested.
- Duplicate-x in `point_curve` is **not** a divide-by-zero bug (verified) — keep a regression test, no fix.
- The shared `pub fn` math kernels (`bilateral_pixel`, `dehaze_recover`, `dehaze_dark_channel`, `midtone_weight`) — correctly factored and reused by both backends.

**Out of scope here:** the `cblack[c]` duplicate-subtraction belongs to the raw-decode register (IP-02), not this pipeline/edit-model triage. Image-processing algorithm correctness is owned by Registers 01–05.

This register reflects intent only; nothing here has been implemented.
