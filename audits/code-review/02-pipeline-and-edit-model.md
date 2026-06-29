# Code Review: `latent-pipeline` & `latent-edit`

Reviewer: automated code review (Claude Opus). Date: 2026-06-27.
Scope: `latent-pipeline/src/lib.rs`, `latent-edit/src/lib.rs`, `latent-edit/src/history.rs`.
Focus: engineering correctness, robustness, idioms, API/abstraction design, serde/sidecar
forward-compat, undo/redo correctness, and test coverage. Image-processing *algorithm*
correctness is explicitly out of scope (audited by a separate team).

Method: full static reading of all three files plus the dependency surface they touch
(`latent-image` `ImageBuf`/`ToneCurve`, the `latent-cpu` backend). Suspected bugs were
confirmed with throwaway tests before reporting; `cargo clippy -p latent-pipeline -p
latent-edit --all-targets` is clean, so findings go deeper than lint.

---

## 1. Overview & overall quality

This is genuinely high-quality code — clearly written by someone who has built a render
pipeline before. The central abstraction is excellent: `render()` owns the fixed stage
*order* and a `Backend` trait provides only pixel-level primitives, with the order
deliberately kept out of every backend. That seam is honoured consistently, the coordinate
spaces (SOURCE vs. OUTPUT) are named and respected, and the "compose lens + keystone +
straighten into one resample so the image interpolates exactly once" design in
`apply_geometry` is both correct and well-justified. The data model is plain serializable
data with neutral defaults, no stored execution order, and a clean version-gated sidecar.
Documentation is unusually thorough, and the ~40 existing tests cover the headline behaviours
(no-op defaults, exposure, white balance, keystone, lens distortion, CA, vignette, mask
combine ops, serde round-trip, partial-sidecar load, version rejection, undo/redo).

The findings below are mostly robustness edge-cases where degenerate or hostile data reaches
floating-point math, plus forward-compat and API/idiom nits. There is **one confirmed reachable
panic** (a NaN x-coordinate in a curve control point crashes `render()`), one **unbounded
memory growth** in history, and a **forward-compatibility inconsistency** in the sidecar
(adjustment structs are evolvable, mask-shape structs are not). Nothing is a memory-safety
defect — Rust's guarantees hold throughout — and most of the data model is already robust.

### Severity counts

| Severity | Count |
|----------|-------|
| Critical | 0 |
| High     | 2 |
| Medium   | 4 |
| Low      | 4 |
| Nit      | 4 |

---

## 2. Findings by severity

### High

#### H1 — A NaN x-coordinate in a curve control point panics `render()` `[confirmed via test]`
`latent-pipeline/src/lib.rs:604-623` (`point_curve`), panics at **line 618**.

```rust
let i = pts.windows(2).position(|w| t <= w[1].0).unwrap();   // line 618
```

`point_curve` sorts control points with `total_cmp`, which orders `NaN` **last**. The two
early returns guard the ends:

```rust
if t <= pts[0].0 { return pts[0].1; }
if t >= pts[last].0 { return pts[last].1; }   // t >= NaN is always false → bypassed
```

When the largest x is `NaN`, the high-end guard `t >= pts[last].0` is `t >= NaN == false`, so
it never fires. Then for any `t` greater than the largest *real* x, no `windows(2)` element
satisfies `t <= w[1].0` (every comparison against the trailing `NaN` is false), so
`position` returns `None` and `.unwrap()` **panics**. Because `channel_curves` builds the
curve via `ToneCurve::from_fn`, which samples the closure across `[0,1]`, this panics at
*render time* (every sampled `t > max_real_x`).

Confirmed end-to-end: a `Curves { master: vec![(0.0,0.0),(0.3,0.3),(f32::NAN,0.5)], .. }`
driven through `render()` panics with `called Option::unwrap() on a None value` at
`lib.rs:618`. A `NaN` reaches here easily — RON deserialization happily accepts `NaN`/`inf`
floats (confirmed: `(exposure: Some(NaN))` loads fine), so a corrupt or hand-edited sidecar
is a direct crash vector, and so is any programmatic mis-set.

**Impact:** A single bad float in saved/loaded edit data crashes the renderer (DoS on the
editing UI; a corrupt sidecar takes down the open). The same `NaN`/`inf` would also silently
poison exposure/gain/tone math elsewhere without a panic.

**Fix (two layers, both cheap):**
1. Make `point_curve` total. Drop non-finite control points (and clamp x/y to `[0,1]`) before
   sorting, and replace the fragile `position().unwrap()` with a saturating search that cannot
   return `None`:
   ```rust
   pts.retain(|p| p.0.is_finite() && p.1.is_finite());
   if pts.is_empty() { return ToneCurve::identity(); }
   // ...
   let i = pts.windows(2).position(|w| t <= w[1].0).unwrap_or(last - 1);
   ```
2. Sanitize on load: reject (or scrub to neutral) non-finite floats in `Document::from_ron`,
   so `NaN`/`inf` never enter the model. A `validate`/`sanitize` pass over `Settings` is the
   clean home for this and also fixes L1/L2 below.

> Note — the prompt's suspected "duplicate-x divide-by-zero" in `(t-x0)/(x1-x0)` is **NOT a
> bug** (verified, see Section 5). The `windows(2).position(|w| t <= w[1].0)` search never
> selects a zero-width segment: at a duplicate x value `X`, the *earlier* window already
> satisfies `t <= X`, so `position` returns it; any `t > X` skips past both duplicate points.
> A 100k-sample sweep over `[(0,0),(0.3,0.3),(0.3,0.9),(1,1)]` produced zero non-finite
> outputs. The real reachable hazard is `NaN`, above.

#### H2 — Undo history grows unbounded (no capacity cap) `[confirmed via test]`
`latent-edit/src/history.rs:11-56`.

`History` keeps every committed step in `undo: Vec<T>` with no cap; `commit` only ever
`push`es. For `T = Settings` each entry is a full deep clone (every `Vec<LocalAdjustment>`,
brush dab list, etc.). Confirmed: 5,000 (and 10,000) committed edits all retain their undo
entries with no eviction. In a long editing session — where each slider gesture is a commit —
this is unbounded RAM growth proportional to (number of gestures × settings size), and brush
masks make each `Settings` arbitrarily large.

**Impact:** Memory grows without limit over a session; with large brush masks this is a real
leak, not a theoretical one.

**Fix:** Bound the stack. Add a `capacity` (e.g. default 100) and evict the oldest on push:
```rust
const DEFAULT_CAP: usize = 100;
// in commit, after push:
if self.undo.len() > self.capacity {
    self.undo.remove(0); // or use a VecDeque for O(1) pop_front
}
```
A `VecDeque` is the idiomatic backing store here (O(1) front eviction). Note the *oldest*
states being unreachable for undo is the correct trade — losing the ability to undo a
500-step-old edit is acceptable; OOM is not.

---

### Medium

#### M1 — Sidecar forward-compat is inconsistent: mask-shape structs reject new/missing fields `[confirmed via test]`
`latent-edit/src/lib.rs:187, 203, 232, 254, 285` (`LuminanceRange`, `ColorRange`, `Gradient`,
`Radial`, `Dab`).

The adjustment structs (`Adjustments`, `Settings`, `Geometry`, `Sharpen`, …) all carry
`#[serde(default)]`, so an older sidecar that predates a field still loads (confirmed by the
existing `partial_sidecar_fills_missing_fields_with_defaults` test), and an unknown *future*
field is silently ignored (confirmed: `future_widget: Some(3.0)` loads fine). But the
mask-shape leaf structs have **neither** `#[serde(default)]` nor a `Default` impl. A sidecar
missing one of their fields is **rejected**: confirmed error `Unexpected missing field named
`x1` in `Gradient``.

**Impact:** The format's forward/backward compatibility story is only half-applied. The moment
a field is added to `Gradient`/`Radial`/`Dab`/`LuminanceRange`/`ColorRange` (e.g. a brush
hardness, a gradient midpoint), every previously-saved sidecar containing that shape fails to
load — a hard regression the schema-version gate won't catch (it's still version 1 data the
build *should* understand). This asymmetry is a latent compatibility trap.

**Fix:** Give these leaf structs the same treatment as the adjustment structs — derive/impl
`Default` and add `#[serde(default)]` — so a missing field falls back to neutral. Add a
forward-compat test that loads a mask shape with a field omitted.

#### M2 — Non-finite / out-of-range values are accepted into the model with no validation `[confirmed via test]`
`latent-edit/src/lib.rs:593-613` (`Document::to_ron`/`from_ron`) and throughout the data model.

`from_ron` does only version gating; it performs no value validation. Confirmed: `opacity:
5.0` loads as `5.0` (the doc comment says `[0,1]`), and `exposure: Some(NaN)`/`Some(inf)` load
verbatim. Most render paths clamp opacity (`blend`) and tolerate large gains, but combined
with H1 a `NaN` is an outright crash, and `inf`/huge values silently produce black/blown
renders that are hard to diagnose.

**Impact:** The data model's documented invariants (`opacity ∈ [0,1]`, finite floats, roughly
`[-1,1]` adjustment ranges) are advisory only; nothing enforces them, so corrupt or adversarial
files propagate bad values into math. This is the upstream cause of H1.

**Fix:** Add a `Settings::sanitize(&mut self)` (or validate-on-load) that replaces non-finite
floats with their neutral value and clamps the documented ranges, and call it in `from_ron`.
This is the single highest-leverage robustness fix — it neutralizes H1, M2, and L1/L2 at once.

#### M3 — `Backend` is a non-`Send`/`Sync`, allocation-heavy seam with several "must clone" returns
`latent-pipeline/src/lib.rs:297-341` (trait) and `353-358`/`631-638` (call sites).

The trait is otherwise a clean design (see Section 3), but two engineering concerns:
- **Per-local-adjustment full-image clones.** `apply_locals` (line 636) does
  `apply_global(img.clone(), …)` for *every* local adjustment, then blends. With N local
  adjustments on a full-resolution image that is N full-frame allocations + copies per render,
  on top of the initial `source.clone()` at line 354 and the per-stage `blur`/`denoise`/
  `dehaze`/`resample`/`warp` allocations. For a develop pipeline run on every slider tick this
  is a meaningful, avoidable cost. The clone is structurally necessary *given the current
  blend-the-whole-image design*, but it's worth documenting as a known cost and a candidate
  for a future "apply within mask bounds only" optimization (most masks cover a fraction of
  the frame).
- **No `Send + Sync` bound.** `render` takes `&dyn Backend`; the trait has no `Send`/`Sync`
  supertrait. A GPU/threaded backend will likely want these, and adding them later is a
  breaking change to every impl. Consider `pub trait Backend: Send + Sync` now if any backend
  is expected to be used across threads (the CPU backend already parallelizes internally per
  the row-based loops in `latent-cpu`).

**Impact:** Performance (clones) and a probable future breaking-change (missing auto-trait
bounds). Neither is a correctness bug.

**Fix:** Document the clone cost on `apply_locals`; consider `Send + Sync` supertraits;
longer-term, blend only the masked bounding region.

#### M4 — `TestBackend` and the real `CpuBackend` independently reimplement the same primitives — drift risk, not tested for parity
`latent-pipeline/src/lib.rs:796-994` (`TestBackend`) vs. `latent-cpu/src/lib.rs:18+`
(`CpuBackend`).

The shared *math kernels* (`bilateral_pixel`, `dehaze_recover`, `dehaze_dark_channel`,
`midtone_weight`) are correctly factored into `latent-pipeline` as `pub fn`s and reused by
both backends — good. But the *structural* primitives — `map_pixels`, `blur`, `combine`,
`resample`, `warp`, `blend`, `eval_mask` — are each written twice, once per backend, with no
shared conformance suite asserting the two agree. The `TestBackend`'s own doc comment claims
it "gives each `PointOp`/`CombineKind` the same meaning the CPU backend does", but nothing
*verifies* that claim. The `TestBackend` even uses nearest-neighbor resampling where the real
backend is bilinear, so the pipeline tests can pass while masking a real-backend regression.

**Impact:** A behavioural divergence between the reference and production backends would not be
caught by the pipeline tests; the documented "same semantics" contract is unenforced.

**Fix:** Extract a backend-conformance test harness (a set of assertions parameterized over
`&dyn Backend`) and run it against both `TestBackend` and `CpuBackend`. At minimum, add a
comment on `TestBackend` flagging the drift risk and the nearest-vs-bilinear difference.

---

### Low

#### L1 — `Gradient::weight_at` returns `0.0` for a degenerate (zero-length) gradient — silently selects nothing
`latent-edit/src/lib.rs:241-248`.

```rust
let len2 = dx*dx + dy*dy;
if len2 <= 1e-12 { return 0.0; }
```

A gradient whose two handles coincide selects nothing everywhere. That's a safe no-crash
choice, but it's a silent surprise (a user who collapses a gradient gets an all-zero mask with
no signal). Acceptable; worth a doc note that a degenerate gradient is treated as empty.

#### L2 — Several mask/feather params trust the sign of `feather` but not its magnitude or finiteness
`latent-edit/src/lib.rs:148-155` (`band`), `262-270` (`Radial`), `294-312` (`Brush`),
`216-227` (`ColorRange`).

Each does `if feather <= 0.0 { hard } else { (… / feather).clamp(0,1) }`. A `feather` of
`NaN` makes `feather <= 0.0` false (NaN compares false), takes the divide branch, and produces
`NaN` (which `clamp` does **not** sanitize for NaN — `f32::clamp` returns NaN if the input is
NaN). That `NaN` then flows into `weight_at` → `blend`, where it is `clamp(0,1)`'d in both
backends (so no crash), but the selection is silently wrong. Same NaN-survives-clamp issue as
H1's root cause. Covered by the M2 sanitize-on-load fix; otherwise harmless given the
downstream clamp.

#### L3 — `commit` records a step on *any* change, including changes made directly via `current_mut` outside a gesture being lost
`latent-edit/src/history.rs:34, 40-56`.

`current_mut()` hands out a `&mut T` with no `begin` required. If a caller mutates through
`current_mut()` *without* a surrounding `begin`/`commit`, the change is applied but never
recorded as an undo step, and a later unrelated `commit` (with a stale or absent `pending`)
won't capture it. This is an API-shape footgun: the "transaction" discipline is by convention,
not enforced. The existing tests always pair `begin`/`commit`, so the unpaired path is
untested.

**Impact:** Easy to silently lose undo coverage for an edit if a call site forgets `begin`.
**Fix:** Either document `current_mut` loudly as "must be wrapped in begin/commit", or provide
a `mutate(|s| …)` helper that brackets the gesture, or have `current_mut` auto-`begin`.

#### L4 — `keystone_transform`/`lens_radial` guard against zero half-extent but a 1×1 image still yields `inv_norm` from `2.0 / w.min(h)`
`latent-pipeline/src/lib.rs:652-666`, `672-677`.

`keystone_transform` guards `cx > 0.0`/`cy > 0.0` (so a 1-pixel dimension degrades to no
keystone in that axis — good). `lens_radial` computes `inv_norm = 2.0 / w.min(h)`; for a
1×1 image that's `2.0`, finite, fine. But for a `0`-dimension image (possible from an
over-aggressive crop elsewhere? `cropped` floors at 1, so not via crop) `w.min(h) == 0` →
`inv_norm = inf`. Not currently reachable (images are ≥1×1), but a debug assert or a
`.max(1.0)` would make the intent explicit and future-proof.

---

### Nit

- **N1 — `PointOp`/`CombineKind`/`MaskShape` are non-`#[non_exhaustive]` public enums.**
  `latent-pipeline/src/lib.rs:25,46`; `latent-edit/src/lib.rs:99,112`. The doc comments say
  "more variants are added over time", but the enums aren't `#[non_exhaustive]`, so adding a
  variant is a breaking change for any downstream `match`. If these are meant to be
  internal-only, fine; if part of the public API for plugin backends, mark them
  `#[non_exhaustive]` now. (Note: `#[non_exhaustive]` would force the in-crate matches to add
  a wildcard arm, which slightly weakens the nice exhaustiveness checking — so this is a
  deliberate trade, not an automatic win.)

- **N2 — Missing `#[must_use]` on pure constructors/queries.** `Transform::identity`,
  `Transform::rotation`, `Transform::compose`, `Warp::from_transform`, `RadialGain::at`,
  `Mask::weight_at`, `Geometry::is_identity`, `History::can_undo`/`can_redo`/`is_idle` all
  return values with no side effects; `#[must_use]` would catch accidental discards. Low value
  but idiomatic.

- **N3 — `Document::to_ron`/`from_ron` return `Result<_, String>`.** Stringly-typed errors
  lose the structured `ron` error (line/column, kind) that callers might want to surface in a
  UI. A small `enum SidecarError { Parse(ron::error::SpannedError), VersionTooNew { found, max } }`
  with `thiserror` would be more idiomatic and preserve the span. The version-too-new case in
  particular is a *data* condition the UI may want to distinguish from a parse failure.

- **N4 — `select_luma` (Rec. 709) silently differs from `luminance` used elsewhere.**
  `latent-edit/src/lib.rs:142-144` defines a Rec.709 luma for mask selection, while the
  pipeline uses `latent_image::color::luminance` for tone/clarity/denoise. The divergence is
  intentional and documented ("need not match the colorimetric luminance"), but two
  "luminance" notions in adjacent code is a readability trap — consider a more distinct name
  like `select_brightness`.

---

## 3. Backend trait / abstraction design assessment

**Verdict: strong.** The core idea — `render()` owns the pipeline *order*, the `Backend` owns
only stateless pixel primitives — is the right seam and is applied without leaks. Specific
points:

- **Data-as-data dispatch.** `PointOp`/`CombineKind`/`DenoiseParams`/`Transform`/`Warp`/
  `RadialGain` describe operations as serializable data, not closures, which is exactly what
  lets a future GPU backend interpret them. The doc comments justify this well. The shared
  `pub fn` math kernels (`bilateral_pixel`, `dehaze_recover`, `midtone_weight`,
  `dehaze_dark_channel`) let backends reuse the *meaning* without re-deriving it — the right
  factoring to keep CPU and GPU honest.
- **`Warp` vs `Transform` split is well-judged.** `Transform` is the affine/homography fast
  path; `Warp` generalizes to radial + per-channel CA in a single interpolation. The
  `Warp::from_transform` equivalence (and its test) keep the two consistent. The `w <= 0`
  behind-the-plane guard (`map` returns `(-1,-1)`) is a correct, tested defense against the
  perspective-divide singularity — a genuinely good edge-case catch.
- **Concerns:** the missing `Send + Sync` supertrait (M3) is the most likely future
  breaking-change; the per-local `img.clone()` (M3) is the main perf cost; and the absence of
  a shared backend-conformance suite (M4) means the trait's behavioural contract is asserted
  only by prose. The trait is also large (11 methods) and will keep growing per its own doc —
  fine for an internal trait, but each new method is a breaking change for every impl, so
  consider whether some primitives could be default-implemented in terms of others (e.g.
  `resample` as a special case of `warp`) to shrink the required surface.

Extensibility of `PointOp`/`CombineKind`: adding variants is clean *internally* but breaking
*externally* (N1). Default impls (`ChannelMixer`, `LensProfile`, `Sharpen`, `Clarity`,
`NoiseReduction`) are all correct neutral values and well-commented.

---

## 4. Serde / sidecar & undo-redo robustness assessment

**Sidecar (serde):** The versioning strategy is sound — an explicit `version: u32`, a
`from_ron` that *rejects* a newer-than-supported schema (tested) rather than silently
misreading it, and `#[serde(default)]` on the adjustment structs so older files still load
(tested). RON ignores unknown future fields (confirmed), so *adding* adjustment fields is
forward-compatible. Round-tripping works for both empty and fully-populated documents (tested).

The gaps: (a) **M1** — mask-shape leaf structs lack `#[serde(default)]`, so the forward-compat
guarantee is inconsistent and evolving a shape will break old files; (b) **M2** — no value
validation on load, so non-finite/out-of-range data enters the model (and via H1 can crash).
There is no checksum/corruption detection beyond what RON's parser rejects, which is a
reasonable scope boundary for a sidecar. Recommend a `sanitize`-on-load pass and uniform
`#[serde(default)]` coverage.

**Undo/redo (`history.rs`):** The core logic is **correct**. Verified by reading and by the
existing tests: `undo`/`redo` swap through `current` symmetrically via `mem::replace` (no
clone, no index arithmetic, no off-by-one); `commit` records a step **only on a real change**
(`prev != self.current`) and **clears the redo branch** on a new edit (tested:
`a_new_edit_clears_the_redo_branch`); a no-net-change gesture records nothing (tested); `undo`
on an empty stack returns `false` (tested) — no underflow. The `begin`/`commit` transaction
model with `pending` is a nice way to coalesce a drag into one step, and `is_idle` for
"auto-save only between gestures" is a thoughtful touch.

The two robustness gaps are **H2** (unbounded growth — the one real defect here) and **L3**
(the `current_mut` footgun that lets an edit escape the transaction). The let-chain in `commit`
(`if let Some(prev) = self.pending.take() && prev != self.current`) is correct: `take()` runs
regardless (so a no-op gesture still clears `pending`), and the `&&` only gates the record —
no short-circuit bug. Reading it twice to be sure: yes, `take()` is in the `if let` scrutinee
so it always executes; correct.

**Let-chain guards in the pipeline** (`apply_global` lines 395-441, `apply_geometry` filters):
all reviewed. `if let Some(nr) = global.noise_reduction && nr.radius > 0.0 && (nr.luminance >
0.0 || nr.color > 0.0)` and the `clarity`/`sharpen` guards correctly make a zero-amount or
zero-radius control a no-op. The `.filter(|…| predicate)` pattern on `Option` for keystone /
lens / vignette is a clean way to fold "present but neutral" into "absent". No precedence or
short-circuit subtlety found — the `&&` chains read left-to-right with the `if let` binding in
scope for the later conditions, which is the intended Rust 2024 semantics.

---

## 5. Test-coverage gaps

The crates are already well-tested for the *happy path and headline behaviours*. The gaps are
in **degenerate / hostile inputs** and **cross-backend parity**:

1. **`point_curve` degenerate inputs — untested, and one is a live panic (H1).** No tests for:
   empty points (identity — works), a single point (flat clamp — works, verified), unsorted
   points (sorted — works), duplicate x (safe, verified — but *should* have a regression test
   proving it stays safe), and **NaN/inf x or y (panics — H1)**. Add tests for all of these,
   especially a NaN-x regression once H1 is fixed.
2. **Sidecar robustness — partial coverage.** Tested: partial-fill, version-reject, round-trip.
   **Missing:** mask-shape forward-compat (M1, currently *fails*), non-finite/out-of-range
   value handling on load (M2), and a malformed/garbage-RON test (asserting a clean `Err`, not
   a panic).
3. **History — capacity untested (H2).** No test asserts (or enforces) a bound on the undo
   stack; add one after the cap fix. The `current_mut`-without-gesture path (L3) is also
   untested.
4. **Backend conformance — no parity test (M4).** Nothing asserts `TestBackend` and
   `CpuBackend` agree; the nearest-vs-bilinear resample difference means a real-backend
   regression can hide.
5. **`latent-edit` has good unit tests** for masks/geometry/serde, but **no `History` tests
   live in `lib.rs`** (they're correctly in `history.rs`) — fine; just confirming the edit
   crate's coverage is real and not only in the pipeline.
6. **Extreme-geometry math** (giant straighten angle producing a huge output `Extent`,
   `w.min(h)==0`) — untested; L4.

---

## 6. Positives

- **Clean, well-named abstraction seam.** `render()` owns order, `Backend` owns primitives —
  consistently honoured, with coordinate spaces (SOURCE/OUTPUT) named and respected.
- **Operations-as-data** (`PointOp`/`CombineKind`/`Warp`/`Transform`) make backends portable
  by construction, and the shared `pub fn` math kernels keep CPU/GPU semantics from diverging
  on the parts that matter most.
- **Single-interpolation geometry.** Folding lens distortion + keystone + straighten + CA into
  one `Warp` resample (instead of chained passes) is the correct, quality-preserving design,
  and the `w <= 0` perspective-divide guard is a real edge-case caught and tested.
- **Undo/redo logic is correct** — symmetric `mem::replace` swaps, redo cleared on new edit,
  change-detected commits, no off-by-one, empty-stack handled. Only the missing capacity cap
  lets it down.
- **Sidecar versioning is done right** — explicit version, reject-newer, default-fill-older,
  unknown-field-ignore. (Just apply it uniformly — M1.)
- **Neutral-by-default data model.** Every adjustment is `Option` / zero-valued-neutral, with
  correct `Default` impls and a `render`-returns-source-unchanged test that pins the invariant.
- **Documentation quality is excellent** — most non-obvious decisions (clarity's three-pass
  blur, the perceptual midtone window, the lensfun coefficient conventions, the dehaze
  airlight model) are explained with citations, which made this review far faster.
- **Clippy-clean** across `--all-targets`, and the ~40 existing tests are meaningful
  behavioural checks, not coverage padding.

---

### Appendix: verification evidence

Throwaway tests (since removed; tree left clean) confirmed:
- **H1:** `render()` with `Curves { master: [(0,0),(0.3,0.3),(NaN,0.5)] }` panics at
  `lib.rs:618` — `called Option::unwrap() on a None value`.
- **Duplicate-x is NOT a bug:** 100k-sample sweep over `[(0,0),(0.3,0.3),(0.3,0.9),(1,1)]`
  yielded zero non-finite outputs; the `position(|w| t <= w[1].0)` search structurally skips
  zero-width segments.
- **H2:** 10,000 committed edits retained 10,000 undo steps (no eviction).
- **M1:** a `Gradient` RON with `x1` omitted → `Err("Unexpected missing field named `x1` in
  `Gradient`")`; an unknown `future_widget` field on `Adjustments` → loaded (ignored).
- **M2:** `(exposure: Some(NaN))`, `(exposure: Some(inf))`, and `(opacity: 5.0)` all load
  verbatim with no validation.
- Baseline `cargo clippy -p latent-pipeline -p latent-edit --all-targets` is clean.
