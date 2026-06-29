# E7 — Edit data model & robustness

Independent stream — **do early**, it is foundational safety. The edit model
(`latent-edit`) and the backend trait (`latent-pipeline`) carry the untrusted
boundary (sidecar load) and the long-session state (undo history). This epic
hardens both: a broad sanitize-on-load (**E7-C1**, the keystone), uniform
forward-compat on the mask-shape leaves (**E7-C2**), a bounded undo stack
(**E7-C3**), the `Send + Sync` backend bound (**E7-C4**), and the edit-model
tests that pin all of it (**E7-C5**). Implements the code-review register
`CR-02` (`audits/decisions/code-review/02-pipeline-and-edit-model.md`): findings
**M2** (E7-C1), **M1** (E7-C2), **H2** (E7-C3), **M3** (E7-C4), plus the test
coverage CR-02 calls for (E7-C5).

**This epic owns Global rule #4 — "sanitize at the boundary, trust within."**
E7-C1 is what *makes that rule true*: once every `Settings` is scrubbed and
clamped on the way in from a sidecar, internal code (the pipeline, the tone/curve
math) may assume finiteness and in-range values. Two companions live in other
epics and **defer to this pass** rather than duplicating it:
- **E1-C4** (`IP-03 2.8` + `CR-02 H1`) makes `point_curve` *total* (drops
  non-finite control points, total/saturating search) so a programmatic mis-set
  *after* load also can't panic. That is the in-function half; E7-C1 is the
  on-load half. They are complementary, not redundant — note the boundary in
  both cards (defense in depth).
- **IP-03 §2.2** (the always-monotone contrast S-curve, owned by **E1-C2**)
  sanitizes only the `SelectiveTone` struct's NaN/inf. Per `CR-02 M2`, that is a
  **subset of E7-C1's broad pass**; IP-03 2.2 should defer to E7-C1's sanitize
  for its NaN/inf handling rather than implementing a second one. E7-C1 owns the
  broad version.

**No GPU / backend-math lockstep in this epic.** E7 touches data structures
(serde, history) and one trait *bound* — not any `Backend` primitive's math — so
Global rule #1 (CPU↔GPU lockstep) does not bite here. The one exception is
E7-C4, which adds a supertrait every backend impl must satisfy: that is a
compile-time obligation across `latent-cpu`, `latent-gpu`, and the in-pipeline
`TestBackend`, not a math change. Global rules #2 (a test pins every change),
#3 (baseline stays green), and #4 (sanitize at the boundary) are the live ones.

---

### E7-C1 — Broad `Settings::sanitize` on load (non-finite scrub + range clamp)
- Implements: `CR-02 M2` (Medium; the broad owner of Initiative F sanitize-on-load) — owns the `IP-03 2.2` NaN/inf subset that E1-C2 defers to            Priority: High
- Crates/files: `latent-edit/src/lib.rs` (new `Settings::sanitize`, called from `Document::from_ron` at `:602-612`; recurses the whole settings tree)
- Depends on: —             Blocks: E7-C5 (sanitize tests); companions E1-C2 (`IP-03 2.2`) and E1-C4 (`CR-02 H1`) defer to this pass
- Heads-up: This is the **keystone** of the epic and the highest-leverage
  robustness fix in `CR-02`. Today `Document::from_ron` (`latent-edit/src/lib.rs:602`)
  does **version gating only** — `opacity: 5.0`, `exposure: Some(NaN)`,
  `Some(inf)`, an out-of-range feather all load **verbatim** (RON accepts
  `NaN`/`inf` as literals), and the documented invariants ("opacity in `[0,1]`",
  adjustments "roughly `[-1,1]`", feather "`>= 0`") are advisory only. A corrupt
  or hand-edited sidecar then propagates bad values into the render math: `inf`
  silently blows or blackens the image (hard to diagnose), and a `NaN` curve
  point is a *direct panic* at render (the upstream cause of `CR-02 H1`, see
  E1-C4).
  **Approach.** Add `pub fn sanitize(&mut self)` on `Settings`, and call it from
  `from_ron` **after the version gate**, over **every** `Settings` in
  `variants` (loop `for s in &mut doc.variants { s.sanitize(); }`). Recurse the
  *whole* tree, not just adjustments:
  - **Non-finite scrub first, then clamp.** For every `f32`, if `!x.is_finite()`
    replace it with that field's **neutral** value (its `Default`), *then* clamp
    to its documented range. Order matters: clamp alone does **not** fix NaN
    (`f32::clamp` with a NaN input returns NaN — same root cause as `CR-02 L2`),
    so scrub-to-neutral must precede the clamp.
  - **`global` + every `locals[i].adjustments`** (same `Adjustments` type — factor
    an `Adjustments::sanitize` and call it for both): `exposure`/`saturation`/
    `dehaze` finite (dehaze clamp `[0,1]`); `SelectiveTone` four fields finite
    (this is the `IP-03 2.2` subset — clamp to the documented `[-1,1]`-ish range
    that E1-C2's monotone curve no longer *needs* but the data model still
    bounds); `WhiteBalance` temp/tint finite; `Hsl` all 24 band values finite;
    `ChannelMixer` 9 matrix entries finite; `Sharpen`/`Clarity`/`NoiseReduction`
    radius/amount/strengths finite and `>= 0` where the field is a magnitude;
    **`Curves`** — drop non-finite `(x, y)` control points and clamp survivors to
    `[0,1]` (coordinate with E1-C4: E7-C1 cleans the *stored* points on load,
    E1-C4 makes the *evaluator* total regardless — do the point-drop the same way
    so they agree; see that card).
  - **`locals[i].opacity`** finite then clamp `[0,1]` (the canonical example for
    the test); **`mask`** — recurse every `MaskShape`: `Gradient` x/y endpoints
    finite; `Radial`/`Dab` cx/cy/x/y/radius finite, radius `>= 0`, **feather**
    finite and `>= 0` (this is `CR-02 L2` — sanitize makes feather finite before
    it reaches the `feather <= 0.0` branch); `LuminanceRange` lo/hi/feather
    finite; `ColorRange` hue/hue_width/sat_min/feather finite; iterate
    `Brush.dabs`.
  - **`geometry`**: `straighten_degrees` finite; `Crop` x/y/width/height finite
    (and a sane non-negative size); `Perspective` vertical/horizontal finite;
    `LensProfile` center/distortion/ca/vignetting all finite (center neutral
    `[0.5,0.5]` if not); `vignette` finite.
  Pick the **neutral** for each non-finite field deliberately (its `Default`),
  and document the clamp ranges inline so the invariants stop being advisory.
  **Gotcha:** `Option<f32>` fields — sanitize the `Some(_)` payload in place;
  leave `None` as-is (off is valid). **Why defense-in-depth, not either/or:**
  E7-C1 stops bad values entering the model; E1-C4 keeps `point_curve` panic-free
  even if a caller mis-sets a point programmatically after load. Cross-reference
  both. **Coordination:** sequence with E7-C2 (both touch `from_ron`; M1 supplies
  the defaults this pass *produces* when a field is missing) — land C2's defaults
  first or together so sanitize never sees a half-deserialized shape. The future
  typed `SidecarError` (`CR-02 N3`) also lands at this seam; keep the signature
  change additive if it arrives in the same window.
- Acceptance: a malformed/adversarial sidecar loads to a **safe, in-range**
  `Settings` (or a clean `Err` for a parse failure — never a panic, never a
  propagated NaN/inf). Named tests (in E7-C5): `sanitize_replaces_non_finite_with_neutral`
  (NaN/inf exposure/saturation/curve-point/feather → neutral), `sanitize_clamps_opacity_out_of_range`
  (`opacity: 5.0` → `1.0`, `-2.0` → `0.0`), `sanitize_clamps_adjustment_ranges`,
  and `from_ron_sanitizes_on_load` (a RON string with `exposure: Some(NaN)` and
  `opacity: 5.0` loads to finite, clamped values). `cargo fmt --check`, `cargo
  clippy --all-targets` (zero warnings), `cargo test --workspace` green.

---

### E7-C2 — Mask-shape `#[serde(default)]` / forward-compat
- Implements: `CR-02 M1` (Medium)            Priority: Medium
- Crates/files: `latent-edit/src/lib.rs` — `LuminanceRange` (`:187-192`), `ColorRange` (`:203-213`), `Gradient` (`:232-238`), `Radial` (`:254-260`), `Dab` (`:285-292`)
- Depends on: —             Blocks: E7-C5 (forward-compat test); pairs with E7-C1 (which sanitizes the *values* these defaults produce)
- Heads-up: There is a **forward-compat asymmetry** today. `Settings`,
  `Adjustments`, `Geometry`, `Mask`, `Brush`, `Curves`, `Hsl`, etc. all carry
  `#[serde(default)]` (struct-level) and tolerate **missing *and* unknown-future**
  fields — but the five mask-shape *leaf* structs `LuminanceRange`, `ColorRange`,
  `Gradient`, `Radial`, `Dab` derive **neither `Default` nor `#[serde(default)]`**.
  So a sidecar with, say, a `Gradient` whose `x1` is omitted **hard-errors**
  (`Unexpected missing field named x1`). The trap is latent: the day any of these
  shapes gains a field (brush hardness, a gradient midpoint, a radial roundness)
  **every previously-saved sidecar containing that shape stops loading** — a
  silent regression the version gate won't catch, because it is still version-1
  data this build should understand. The current `partial_sidecar_fills_missing_fields_with_defaults`
  test (`latent-edit/src/lib.rs:998`) proves the *adjustment* side works; the
  shape side has no such guarantee.
  **Approach.** Give all five leaves the **same** treatment as the adjustment
  structs: derive (or impl) `Default` and add **struct-level `#[serde(default)]`**
  (matching the `#[derive(... Default ...)] #[serde(default)]` pattern already on
  `WhiteBalance`/`SelectiveTone` at `:393-394`/`:402-403`). Most can simply add
  `Default` to the derive list; `Gradient`/`Radial` may want a **deliberate,
  documented neutral** rather than all-zeros — a zero-length `Gradient` already
  reads as "selects nothing" (the `len2 <= 1e-12 -> 0.0` guard at `:244`, and
  `CR-02 L1` keeps that behavior), and an all-zero `Radial`/`Dab` (radius 0)
  selects nothing too — both benign no-ops, which is the right default for "a
  field was missing." State the chosen default and *why it is benign* in the doc
  comment so the silent all-zero result is documented, not surprising.
  **Gotcha:** these are `Copy` structs — adding `Default` to the derive is clean,
  but if you hand-impl `Default` keep the `#[serde(default)]` so a missing field
  (not just a missing struct) falls back. **Coordination with E7-C1:** the
  defaults this card introduces are exactly the neutral values E7-C1's sanitize
  also produces for non-finite fields — they must agree (a defaulted feather and a
  scrubbed-from-NaN feather should land on the same value). Land C2 before or with
  C1 so sanitize never observes a field that failed to deserialize.
- Acceptance: a sidecar that **omits a field on any mask shape** loads with that
  field filled to its neutral default instead of erroring. Named tests (in E7-C5):
  `gradient_loads_with_missing_field` (a `Gradient` with `x1` omitted → default,
  no error), and one analogous case per shape (`radial_loads_with_missing_field`,
  `dab_loads_with_missing_field`, `luminance_range_loads_with_missing_field`,
  `color_range_loads_with_missing_field`) — or one consolidated
  `mask_shapes_load_with_missing_fields` exercising all five. `cargo fmt --check`,
  `cargo clippy --all-targets` (zero warnings), `cargo test --workspace` green.

---

### E7-C3 — Bounded undo (`VecDeque` cap)
- Implements: `CR-02 H2` (High)            Priority: High
- Crates/files: `latent-edit/src/history.rs` (`History<T>` — `undo` field `:14`, ctor `:21-28`, `commit` `:49-56`)
- Depends on: —             Blocks: E7-C5 (history tests)
- Heads-up: `History`'s `undo: Vec<T>` (`latent-edit/src/history.rs:14`) **grows
  unbounded** — 10,000 commits retain 10,000 deep-cloned `Settings`, never
  evicted. Each entry clones every `Vec<LocalAdjustment>` and every brush
  `Vec<Dab>`, so with large brush masks this is a **real per-gesture memory leak**
  over a long editing session, not a theoretical one.
  **Approach.** Back `undo` with a **`VecDeque<T>`** plus a `capacity: usize`:
  - Change `undo: Vec<T>` → `undo: VecDeque<T>` (add `use std::collections::VecDeque;`).
  - Add a `capacity` field; constructor default `DEFAULT_CAP = 100` (a `pub const`
    on `History`), plus a `pub fn with_capacity(initial: T, capacity: usize)` ctor
    for callers who want a deeper stack. Existing `new(initial)` delegates with
    `DEFAULT_CAP`. (Guard `capacity >= 1` — a zero cap would discard every step;
    `.max(1)` it.)
  - In `commit`, after recording: push with `push_back`, then evict the oldest —
    `while self.undo.len() > self.capacity { self.undo.pop_front(); }`.
  - `undo()`/`redo()` use `pop_back`/`push_back` (was `Vec::pop`/`push` — same
    LIFO end). `redo` can stay a `Vec` (it is cleared on every new edit, so it
    never grows unbounded across independent gestures — but if you prefer
    symmetry, a `VecDeque` is fine; do **not** cap redo, it is naturally bounded
    by the undo depth).
  **Preserve the verified-correct semantics (do not change them):** `CR-02` §6
  affirms the undo/redo core is correct — symmetric `mem::replace` swaps, **redo
  cleared on every new committed edit** (`commit` → `self.redo.clear()`),
  **change-detected commit** (the `if let Some(prev) = self.pending.take() &&
  prev != self.current` let-chain: `take()` always clears `pending` even on a
  no-op, `&&` only gates the *record*), no off-by-one, and **empty-stack safety**
  (`pop` → `None` → returns `false`). Only the missing cap is the defect; keep
  everything else byte-for-byte. The eviction must not perturb redo invalidation
  or the change-detection — it happens strictly *after* the `push_back`, only on a
  real recorded step.
  **Gotcha:** when you evict via `pop_front`, the **most-recent `capacity` steps
  must still undo correctly** — that is the whole point (lose the oldest, keep the
  newest). Pin that in the test. `#[must_use]` on `can_undo`/`can_redo`/`is_idle`
  is a separate nit (`CR-02 N2`), out of scope here unless trivially co-located.
- Acceptance: the undo stack is **bounded** at the cap; the oldest step is
  evicted once over capacity; the most-recent `capacity` steps still undo
  correctly; redo and change-detection semantics are unchanged. Named tests (in
  E7-C5): `history_caps_undo_depth` (commit `> capacity` distinct edits; assert
  `len <= capacity` and that undoing all the way back lands on the value that was
  *current `capacity` steps ago*, **not** the original — the oldest was evicted),
  `with_capacity_respects_a_custom_cap`, and re-affirm the existing
  `undo_redo_round_trips` / `a_new_edit_clears_the_redo_branch` /
  `a_gesture_with_no_net_change_records_nothing` still pass unchanged. `cargo fmt
  --check`, `cargo clippy --all-targets` (zero warnings), `cargo test --workspace`
  green.

---

### E7-C4 — `Backend: Send + Sync`
- Implements: `CR-02 M3` (Medium; the `Send + Sync` part — the clone-cost doc and the masked-bounds optimization are deferred per the decision)            Priority: Medium
- Crates/files: `latent-pipeline/src/lib.rs` (the `Backend` trait, `:297`); `cargo check` across `latent-cpu`, `latent-gpu`, and the in-pipeline `TestBackend` (`:796+`)
- Depends on: —             Blocks: enables E8-C4 (off-thread render/export) and E6-C6 (the `&dyn Backend` conformance harness)
- Heads-up: The `Backend` trait (`latent-pipeline/src/lib.rs:297`) is **not
  `Send + Sync`**. Adding that bound **later** is a breaking change to every impl;
  adding it **now is free** — the existing impls are **stateless** (`CpuBackend`
  is a unit/zero-field struct that already parallelizes internally; `TestBackend`
  likewise; the GPU backend holds device handles that are themselves `Send +
  Sync`). This is the most likely future *breaking* change in `CR-02`, and it is
  the engineering-correct call to take it while it costs nothing.
  **Approach.** Change the trait declaration to
  `pub trait Backend: Send + Sync { ... }`. Then **`cargo check` every impl** —
  `latent-cpu`, `latent-gpu`, and the in-pipeline `TestBackend` — to confirm each
  already satisfies the bound (they should, given statelessness; if the GPU
  backend wraps anything non-`Sync` such as a `Rc`/`Cell`, that surfaces here and
  must be addressed, e.g. `Arc`/an interior-mutability swap — flag it, don't paper
  over it). `&dyn Backend` references used in the pipeline are unaffected; any
  place that wants to *share* a backend across threads (E8-C4) will now also need
  `+ 'static` / `Arc<dyn Backend>` at that call-site — that is the consumer's
  concern, not this card's.
  **Why now, and what it unblocks:** a GPU/threaded backend wants the bound; the
  CPU backend already parallelizes; so the bound is correct today and removes a
  future breaking change. Concretely it **enables E8-C4** (render/export off the
  egui UI thread — a worker needs `Backend: Send` to move the backend, `Sync` to
  share `&dyn Backend`) and **E6-C6** (the shared conformance harness over `&dyn
  Backend` runs the same assertions across Test/CPU(/GPU) and may fan out across
  threads). Name both in the card so the downstream cards can rely on the bound.
  **Scope note (per `CR-02 M3`, partial fix):** the per-local `img.clone()` in
  `apply_locals` (`latent-pipeline/src/lib.rs:636`) is a genuine but
  *structurally-necessary* per-tick cost of the blend-the-whole-image design — its
  doc note and the "blend within the mask bounding-box only" optimization are
  **deferred** (a tracked follow-up, not this triage). This card adds **only** the
  supertrait. Do not attempt the masked-bounds optimization here.
- Acceptance: `Backend: Send + Sync` compiles across **all** impls with no other
  change; a static check enforces the bound at the type level. Named test (in
  E7-C5, or a `latent-pipeline` doc/static test): `backend_is_send_and_sync` — a
  `fn assert_send_sync<T: Send + Sync>() {}` instantiated as
  `assert_send_sync::<CpuBackend>()` (and `TestBackend`), plus a
  `fn _assert_dyn(_: &(dyn Backend + Send + Sync)) {}` style compile assertion so a
  future non-`Send` impl fails to build. `cargo check`/`cargo test --workspace`
  green across `latent-cpu`, `latent-gpu`, `latent-pipeline`; `cargo fmt --check`,
  `cargo clippy --all-targets` (zero warnings).

---

### E7-C5 — Edit-model tests (sanitize, serde round-trip/forward-compat, history)
- Implements: `CR-02 M2/M1/H2/M3` test coverage            Priority: High
- Crates/files: `latent-edit/src/lib.rs` (tests, alongside the existing module at `:624-1019`), `latent-edit/src/history.rs` (tests, at `:95-154`), and a `Send + Sync` static check in `latent-pipeline`
- Depends on: E7-C1, E7-C2, E7-C3, E7-C4             Blocks: —
- Heads-up: This card consolidates the test obligations of the epic (Global rule
  #2 — a test pins every change; each must **fail before** its feature card and
  **pass after**). The existing `latent-edit` test module already covers serde
  **round-trip** (`empty_document_round_trips` `:904`, `populated_document_round_trips`
  `:910`), **partial-fill for adjustments** (`partial_sidecar_fills_missing_fields_with_defaults`
  `:998`), and **version rejection** (`newer_schema_version_is_rejected` `:1013`);
  `history.rs` covers `undo_redo_round_trips`, `a_new_edit_clears_the_redo_branch`,
  `a_gesture_with_no_net_change_records_nothing` (`:106-153`). **Keep all of these
  green** — this epic must not regress them — and **add**:
  - **Sanitize (E7-C1).** `sanitize_replaces_non_finite_with_neutral` (NaN and
    inf in exposure, saturation, a curve-point x/y, a feather → each becomes its
    neutral), `sanitize_clamps_opacity_out_of_range` (`5.0 -> 1.0`, `-2.0 -> 0.0`),
    `sanitize_clamps_adjustment_ranges` (a `SelectiveTone`/dehaze value past its
    documented bound is clamped), `from_ron_sanitizes_on_load` (a hand-written RON
    string carrying `exposure: Some(NaN)` and `opacity: 5.0` loads to finite,
    clamped values via `from_ron` — the boundary test). Also a **garbage-RON**
    case asserting a clean `Err` (no panic) for a genuinely malformed string.
  - **Forward-compat (E7-C2).** A field omitted on **each** mask shape still loads
    to its default: `mask_shapes_load_with_missing_fields` (or one test per shape
    — `gradient_loads_with_missing_field`, etc., per E7-C2). Build the RON by hand
    (as `partial_sidecar_…` does at `:1002`) with a shape missing one field;
    assert it loads and the field holds the neutral default.
  - **History (E7-C3).** `history_caps_undo_depth` (commit `> DEFAULT_CAP` distinct
    edits; assert the stack is bounded and the **oldest** is evicted while the
    most-recent `capacity` steps still undo correctly), `with_capacity_respects_a_custom_cap`,
    and an explicit **empty-stack** assertion (`undo()`/`redo()` on a fresh
    `History` return `false`, no panic — pin the verified-correct behavior).
  - **Send + Sync (E7-C4).** `backend_is_send_and_sync` — the
    `assert_send_sync::<CpuBackend>()` / `TestBackend` static check.
  Use the round-trip → sanitize idempotence trick where useful: sanitizing an
  already-clean `Settings` must be a **no-op** (assert `s == s.clone();
  s2.sanitize()`), so a valid populated document survives unchanged.
- Acceptance: all named tests above pass; each new test **fails before** its
  feature card lands and **passes after**; every pre-existing `latent-edit` /
  `history.rs` test still passes (no regression). `cargo fmt --check`, `cargo
  clippy --all-targets` (zero warnings), `cargo test --workspace` all green
  (Global rules #2, #3).

---

**Epic done when:** the edit data model is robust at the untrusted boundary and
bounded in memory — `Settings::sanitize` scrubs every non-finite float and clamps
every documented range across the whole settings tree on sidecar load (E7-C1,
the broad owner that E1-C2's `IP-03 2.2` clamp and E1-C4's total `point_curve`
defer to); the five mask-shape leaf structs carry uniform `Default` +
`#[serde(default)]` so evolving any shape never breaks an old sidecar (E7-C2);
the undo `History` is a capped `VecDeque` that evicts the oldest step past the
cap while preserving the verified-correct redo-invalidation, change-detection,
and empty-stack semantics (E7-C3); `Backend: Send + Sync` is in place across all
impls, unblocking off-thread render/export (E8-C4) and the `&dyn Backend`
conformance harness (E6-C6) (E7-C4); and the sanitize, serde
round-trip/forward-compat, and history tests pin every behavior, with all
pre-existing edit-model tests still green (E7-C5). `cargo fmt --check`, `cargo
clippy --all-targets` (zero warnings), and `cargo test --workspace` all pass.
