# E5 — Spatial filters (denoise / sharpen / clarity)

Implements the Audit-04 spatial decisions (`../decisions/04-spatial-and-frequency.md`)
that live in the **denoise / sharpen / radius** cluster: unify radius semantics
across the spatial filters (**E5-C1**), widen the bilateral spatial Gaussian to
the standard ±3σ support (**E5-C2**), move the capture sharpen into the L\*
perceptual domain (**E5-C3**), and make the denoise chroma metric perceptual
(**E5-C4**). The through-line shared with E1: **the perceptual spatial ops
(sharpen, chroma denoise) move into the shared Lab/LCh / L\* space** instead of
operating in raw linear-RGB. Clarity (`IP-04 2.3`) was **verified correct** and is
left unchanged — do not re-touch the 3-box + midtone path.

**Scope boundaries (do NOT author these here).** The other Audit-04 points are
owned by sibling epics: the **dehaze overhaul** (`IP-04 2.5a/2.5b/2.5c`) is **E4**,
and the **output-sharpening pass** (`IP-04 2.6`) is **E6-C3** — which *reuses the
L\* sharpen form built in E5-C3*. This epic only owns `IP-04 2.1`, `2.2`, `2.4a`,
`2.4b`. **E5-C1 (radius)** does, however, reach *across* into the dehaze patch and
must coordinate with **E4-C4** (the dehaze patch tied to a radius) — that
coordination is spelled out in C1.

**Depends on E0** — specifically **E0-C1** (the `Lab`/`LCh` module: working→XYZ→
Lab→LCh + inverse, D50-referenced, and the exposed `L*` perceptual lightness).
**E5-C3 and E5-C4 cannot start until E0-C1 lands** (they consume L\* / Lab
directly); E5-C1 and E5-C2 do not strictly need it but should sequence after it so
the whole epic shares one perceptual module (global rule "one place per concern").
Do **not** re-derive lightness or a fresh Lab path here — consume the E0-C1 owner.

**Backend / lockstep context (read once, applies to the whole epic).** The
relevant primitives and their current backend state:
- **`blur`** is fully ported: CPU (`latent-cpu/src/lib.rs:65-74`, separable
  `blur_axis`), pipeline reference (`latent-pipeline/src/lib.rs:838-864`,
  non-separable box), and the **WGSL shader** `latent-gpu/src/box_blur.wgsl`
  (driven from `latent-gpu/src/lib.rs:466-502`). **Any radius-semantics change to
  `blur` is a three-backend change** (Global rule #1) and must keep
  `blur_matches_cpu` (`latent-gpu/src/lib.rs:677`) green.
- **`combine`** (the Unsharp / LocalContrast recombine) lives on CPU
  (`latent-cpu/src/lib.rs:76-99`) and the pipeline reference
  (`latent-pipeline/src/lib.rs:866-880`); the **GPU backend delegates `combine`
  to the CPU** (`latent-gpu/src/lib.rs:504-506`) — there is **no combine WGSL
  shader**. So the L\* sharpen recombine (E5-C3) changes CPU + reference; whether
  it needs a WGSL shader is decided in that card (it does not, by delegation —
  but the lockstep obligation that the CPU and reference helper not drift still
  applies).
- **`denoise`** (`bilateral_pixel`) is **CPU-only**: the CPU backend
  (`latent-cpu/src/lib.rs:159-177`) and pipeline reference
  (`latent-pipeline/src/lib.rs:942-953`) both call the shared
  `bilateral_pixel` (`latent-pipeline/src/lib.rs:525-568`); the **GPU backend
  delegates `denoise` to the CPU** (`latent-gpu/src/lib.rs:552-556`: *"isn't
  ported to WGSL yet; delegate to the CPU"*). **Decision for this epic: the
  bilateral stays CPU-only** (E5-C2, E5-C4 change `bilateral_pixel` only) — no
  WGSL mirror is written, and the CPU↔GPU equivalence stays trivially green
  because GPU == CPU by delegation. Each card states the *if/when* a GPU mirror
  becomes owed.

The lockstep obligation that *does* bite this epic: the **sharpen lowering** lives
in `apply_local` (`latent-pipeline/src/lib.rs:433-441`) and the **shared
`bilateral_pixel` / `combine` math** must move together with the CPU backend and
(for `blur`) the WGSL shader — they must not drift.

---

### E5-C1 — Unify radius semantics across blur/denoise/dehaze
- Implements: `IP-04 2.2` (M3, Medium — *Unify radius semantics*)            Priority: High
- Crates/files: `latent-cpu/src/lib.rs:68` (`blur` gate), `latent-cpu/src/lib.rs:163` (`denoise` gate); `latent-pipeline/src/lib.rs:839` (reference `blur` gate), `:526` (`bilateral_pixel` radius), `:943` (reference `denoise` gate); `latent-gpu/src/lib.rs:467` (GPU `blur` gate) + `latent-gpu/src/box_blur.wgsl`; the clarity/sharpen radius gates in `apply_local`/`apply_global` (`latent-pipeline/src/lib.rs:417,435`); the dehaze patch (`latent-pipeline/src/lib.rs:464-468`, **coordinate with E4-C4**)
- Depends on: —            Blocks: E5-C2 (uses the unified radius), E5-C3 (sharpen blur radius), E5-C4 (denoise radius); **coordinate with E4-C4** (dehaze patch tied to radius)
- Heads-up:
  - **This is the foundation card.** Every other spatial card assumes one,
    documented meaning for "radius". Land it first; the others build on the
    convention it pins.
  - **The inconsistency today.** Three filters interpret "radius" three
    different ways:
    - **`blur`**: `let r = radius.round().max(0.0) as i32; if r == 0 { return clone }`
      (`latent-cpu/src/lib.rs:68-71`, pipeline `:839-842`, GPU `:467-470` →
      `max(0.0) as u32`). So a **radius of 0.3 rounds to 0 and is a silent
      identity** — the *exact* surprise the decision calls out. The gate in
      `apply_global`/`apply_local` is a *separate* predicate (`c.radius > 0.0`,
      `s.radius > 0.0` at `:417,435`) that lets `0.3` through to a no-op blur.
    - **`denoise`**: gated by `params.radius.round() < 1.0` (CPU `:163`, pipeline
      `:943`) → identity below 0.5; then **inside** `bilateral_pixel` the radius
      is re-derived as `params.radius.round().max(1.0) as i32`
      (`latent-pipeline/src/lib.rs:526`) — a *different* clamp (floors at 1, not
      0). So the gate and the kernel round the same value with subtly different
      rules.
    - **`dehaze`**: no radius at all — a fixed `pub const DEHAZE_PATCH: i32 = 4`
      (`latent-pipeline/src/lib.rs:468`), a 9×9 window, independent of the others.
  - **Decision — one radius meaning.** Define "radius" once: **the integer
    half-window in pixels**, `r = round(radius)` with a single documented
    rounding rule (round-half-up / `f32::round`, state it), and a single
    identity/skip rule. Reconcile the gate predicates so they agree with the
    kernel's own clamp:
    - A sub-threshold radius is **either** a clean identity **at one consistent
      threshold** (recommend: `round(radius) < 1 ⇒ identity` for *all* of blur,
      denoise, and the clarity/sharpen radius gates) **or** snaps up to the
      minimum — pick one and apply it uniformly. The current mix (blur identity
      at `r==0`, denoise identity at `r<1`, plus a separate `> 0.0` gate in the
      lowering) is what produces the *silent-identity surprise* (blur radius 0.3
      passes the `> 0.0` gate, then rounds to a no-op). Make the lowering gate
      and the primitive's own gate **the same predicate** so a radius that the
      caller thinks is "on" cannot silently no-op.
    - Make `blur`'s internal `r` and `bilateral_pixel`'s internal `r` use the
      **identical** expression (so `round`/clamp match), and have the `denoise`
      gate use that same expression rather than a second hand-rolled
      `round() < 1.0`.
  - **No silent identity.** After this card, a "radius" a user can set must
    either *do something* or be reported/snapped — there must be **no value that
    passes the enable gate yet rounds to a no-op kernel**. That is the concrete
    acceptance bar.
  - **Tie the dehaze patch to a radius (coordinate with E4-C4).** §2.2 says
    *"tie the dehaze patch to a radius"* and §2.5c (E4-C4) scales that patch with
    resolution. This card **owns the radius *convention***; **E4-C4 owns the
    dehaze patch *value*.** Concretely: E4-C4 derives the dehaze patch as a
    *radius* (half-window) using **this card's** rounding/half-window meaning.
    Coordinate so the dehaze patch is expressed as a `radius` in the same units
    as `blur`/`denoise` — not a bespoke `DEHAZE_PATCH` with its own semantics. If
    E4-C4 lands first, retro-fit its radius to this convention; if this lands
    first, leave a documented `radius` helper E4-C4 can call. (E4-C4 already
    carries a `// E5-C1: unify with blur/denoise radius` marker — close it.)
  - **CPU↔GPU↔reference lockstep (mandatory for `blur`).** `blur` is fully
    ported, so a change to its gate/rounding is a **three-backend change**: CPU
    (`latent-cpu/src/lib.rs:68`), pipeline reference
    (`latent-pipeline/src/lib.rs:839`), **and** the GPU path
    (`latent-gpu/src/lib.rs:467` + `box_blur.wgsl` if the rounding moves into the
    shader — currently the host computes `r` and passes a `u32`, so the shader is
    likely untouched, but verify the host-side `r` matches the new convention).
    `blur_matches_cpu` (`latent-gpu/src/lib.rs:677`) must stay green. `denoise`
    is CPU-only (epic header) — CPU + reference only.
  - **Why.** Inconsistent radius rounding means a value that "works" in one tool
    silently does nothing in another, and the dehaze patch can't be tied to the
    same notion of radius that 2.5c needs to scale. One meaning removes the
    surprise and unblocks E4-C4.
- Acceptance:
  - Behavior: `blur`, `denoise`, and the clarity/sharpen radius gates all
    interpret "radius" identically (same `round`, same half-window, same
    identity threshold); **no radius value passes an enable gate yet produces a
    no-op kernel** (the 0.3-blur surprise is gone); the dehaze patch is
    expressible as a radius in the same units (coordinated with E4-C4).
  - Tests (in `latent-cpu` and/or `latent-pipeline`):
    `blur_radius_below_threshold_is_consistent_identity` — a sub-threshold radius
    is identity in `blur` **and** the same threshold gates `denoise` and the
    lowering (no value that the lowering passes then rounds to a no-op).
    `radius_rounding_matches_across_blur_and_denoise` — `round(radius)` produces
    the same integer half-window for `blur` and `bilateral_pixel` across a sweep
    of fractional radii (e.g. 0.3, 0.5, 0.9, 1.4, 2.6).
    `no_radius_passes_gate_then_noops` — the headline regression: for every
    radius the enable gate accepts, the kernel does non-trivial work (assert a
    known non-flat input changes). Keep `blur_radius_zero_is_identity`
    (`latent-cpu/src/lib.rs:381`) green under the unified rule.
    GPU: `blur_matches_cpu` stays green with the reconciled host-side `r`.
    A cross-reference test (or shared const) pins that E4-C4's dehaze patch radius
    uses this convention.

---

### E5-C2 — Bilateral spatial Gaussian σ_s = r/3 (±3σ)
- Implements: `IP-04 2.4a` (H1, High — *Use σ=r/3 (±3σ support)*)            Priority: High
- Crates/files: `latent-pipeline/src/lib.rs:528-529` (`bilateral_pixel` `sigma_s` / `inv_2ss2`, and the σ note in the docstring `:519-520`)
- Depends on: E5-C1 (the radius `r` is the unified half-window)            Blocks: —
- Heads-up:
  - **One-line math change.** In `bilateral_pixel` the spatial Gaussian is
    currently `let sigma_s = r as f32 / 2.0;`
    (`latent-pipeline/src/lib.rs:528`), i.e. **σ_s = r/2**, which truncates the
    `±r` window at **±2σ**. Change it to **σ_s = r/3** so the same `±r` window is
    the standard **±3σ** truncation. Update `inv_2ss2` accordingly (it's derived
    from `sigma_s`, so it follows automatically) and the docstring line that
    currently says *"σ = radius/2 … window 2σ"* (`:519-520`) to describe the
    r/3 / ±3σ rationale.
  - **Same tap count, same window.** The loop bounds (`-r..=r`) **do not
    change** — only the Gaussian's width inside that fixed window. Cost is
    identical; this is purely a weighting fix.
  - **Why.** σ_s = r/2 cuts off at e^(−2) ≈ 0.135, a hard step that drops
    ~4.6%/axis of the Gaussian's mass at the window boundary — a visible
    discontinuity (boundary step) in the spatial weight. σ_s = r/3 puts the
    window edge at 3σ where the Gaussian is ≈ e^(−4.5) ≈ 0.011, a smooth falloff
    with negligible truncation (the standard ±3σ support). This is the textbook
    Gaussian-support convention (Tomasi & Manduchi, ICCV 1998 — the bilateral the
    docstring already cites; see `docs/spatial-bilateral-tomasi-1998.pdf`).
  - **CPU-only — no GPU mirror owed (yet).** The bilateral is CPU-only by
    delegation (epic header: `latent-gpu/src/lib.rs:552-556` delegates `denoise`
    to the CPU). So this change touches **only** the shared `bilateral_pixel`
    (used by both the CPU backend and the pipeline reference) — **no WGSL
    bilateral exists to mirror.** State this explicitly. **If/when** a future
    epic ports the bilateral to WGSL (the GPU `denoise` stops delegating), the σ_s
    = r/3 spatial Gaussian must be mirrored there in lockstep — note that the
    obligation transfers with the port (it does not exist today).
  - **Interacts with E5-C1.** `r` is the unified half-window from E5-C1, so σ_s =
    r/3 is computed off the *unified* radius — land C1 first (the σ change is
    meaningless if `r` itself is ambiguous). Coexists cleanly with the chroma
    metric change (E5-C4) — both edit `bilateral_pixel`; sequence them so they
    don't collide (do C2 then C4, or in one combined pass, but keep the spatial
    σ and the range metric as separate, independently-tested concerns).
- Acceptance:
  - Behavior: the spatial Gaussian's σ is r/3 so the `±r` window is ±3σ; the
    boundary weight is ≈ e^(−4.5) (negligible) instead of e^(−2) (≈0.135); same
    tap count; a flat region still smooths and an edge is still preserved (the
    range term is unchanged).
  - Tests (in `latent-pipeline`):
    `bilateral_spatial_sigma_is_r_over_three` — the spatial weight at the window
    edge (`dx²+dy² = r²`, range term neutralized) equals `exp(−4.5)` within
    tolerance (i.e. 3σ at the boundary), **not** `exp(−2)`; equivalently assert
    the half-mass radius matches σ = r/3.
    `bilateral_boundary_weight_is_negligible` — the spatial weight at the corner
    of the window is far below the ±2σ value (no boundary step). Keep the
    existing denoise behavior tests green: `denoise_luma_smooths_a_flat_tone_but_
    preserves_an_edge` (`latent-cpu/src/lib.rs:496`) and `denoise_color_smooths_
    chroma_independently_of_luma` (`:526`) — the smoother falloff must not break
    edge preservation.

---

### E5-C3 — Perceptual-domain (L\*) luma sharpen (capture)
- Implements: `IP-04 2.1` (N1, Note — *Luma sharpen in a perceptual domain*)            Priority: High
- Crates/files: `latent-pipeline/src/lib.rs` (`CombineKind` `:45-55`, the sharpen lowering in `apply_local` `:433-441`, the `combine` reference helper `:866-880`); `latent-cpu/src/lib.rs:76-99` (`combine` impl); consume E0-C1 `L*` / Lab from `latent-image/src/color.rs`; **no WGSL** (combine delegates — see Heads-up)
- Depends on: **E0-C1** (the shared `L*` / Lab module — hard dependency)            Blocks: reused by **E6-C3** (output sharpen uses this same L\* form)
- Heads-up:
  - **What sharpen is today.** Capture sharpen is lowered in `apply_local` to a
    blur + an **Unsharp recombine** (`latent-pipeline/src/lib.rs:437-440`):
    `let base = backend.blur(&img, s.radius); backend.combine(&mut img, &base,
    &CombineKind::Unsharp { gain: 1.0 + s.amount })`. The `Unsharp` combine runs
    **per channel in linear light**: `out[c] = base[c] + gain·(img[c] −
    base[c])` (CPU `latent-cpu/src/lib.rs:79-85`, reference
    `latent-pipeline/src/lib.rs:868-872`). Two problems: (1) **per-channel** →
    the three channels overshoot independently at an edge, **shifting edge hue
    (color fringing)**; (2) **linear light** → the overshoot is perceptually
    **asymmetric** (the bright-side halo of a dark→light edge is perceptually
    larger than the dark-side undershoot, because equal linear deltas are unequal
    perceptual steps).
  - **The fix (both halves of N1).** Do the unsharp on **L\* only** and
    **reconstruct color around the sharpened L\***:
    1. Compute the **base** as today (`blur` of the image) — the blur stays in
       working/linear; it is just a low-pass reference.
    2. For each pixel, take the input's **L\*** and the base's **L\*** (via the
       E0-C1 Lab path: working→XYZ→Lab, read L\*). Apply the unsharp **on L\***:
       `L*_out = L*_base + gain·(L*_in − L*_base)`.
    3. **Reconstruct color around the sharpened L\*** — keep the pixel's **a\*,
       b\*** (chroma/hue) from the *input* and substitute the sharpened `L*_out`,
       then Lab→XYZ→working back. So only lightness is sharpened; hue and chroma
       are carried from the input untouched. This removes fringing (one luminance
       channel sharpened, not three) **and** makes the overshoot symmetric
       (L\* is perceptually uniform, so equal-magnitude over/undershoot are equal
       perceptual steps — no brighter-side halo bias).
  - **Why L\* (not Rec.709 luma, not linear Y).** §2.1 was revised
    (2026-06-27) from *luma-only-in-linear* to *luma-in-the-L\* perceptual
    domain* specifically so sharpening is consistent with the rest of the
    perceptual grading core (E1 moved tone/saturation/mixer into L\*/LCh) and is
    no longer the lone tool left in linear light. Use the **one** E0-C1 `L*` —
    do not hand-roll a luma or a second Lab path (global rule "one place per
    concern"; the `LUMA_WEIGHTS` blue weight is ~0.0001 and is the wrong tool
    for a perceptual luma — that's the E1-C6 lesson).
  - **Where the change lands — the combine, not the blur.** The cleanest shape
    is a **new `CombineKind` variant** (e.g. `CombineKind::UnsharpLuma { gain }`
    or extend the recombine to carry a "perceptual-luma" flag) that the sharpen
    lowering emits instead of `Unsharp`, and whose `combine` arm does the L\*
    recombine + color reconstruction. Keep the existing linear `Unsharp` variant
    if anything else uses it (grep: clarity uses `LocalContrast`, output sharpen
    is E6-C3) — but the **capture sharpen** lowering at `:438-440` switches to the
    L\* variant. Thread any new variant through the `CombineKind` enum
    (`:45-55`), the CPU `combine` match (`latent-cpu/src/lib.rs:78-98`), and the
    reference `combine` match (`latent-pipeline/src/lib.rs:867-879`).
  - **CPU + reference helper; no WGSL (by delegation).** `combine` is **not** a
    GPU shader — the GPU backend **delegates `combine` to the CPU**
    (`latent-gpu/src/lib.rs:504-506`), so there is no `combine.wgsl` to mirror and
    the CPU↔GPU equivalence stays green by delegation. State this. The lockstep
    obligation that *does* apply (Global rule #1): the CPU `combine`
    (`latent-cpu/src/lib.rs:76-99`) and the pipeline reference `combine`
    (`latent-pipeline/src/lib.rs:866-880`) implement the *same* L\* recombine and
    must not drift — keep them identical. **`blur` is unchanged** by this card
    (still the linear low-pass base), so the `blur` WGSL shader is untouched.
    **Note the future obligation:** if a later epic ports `combine` to WGSL, the
    L\* recombine + the Lab transfer must be mirrored there (the same WGSL Lab
    port E1-C6 writes for saturation) — that obligation transfers with the port;
    it does not exist today.
  - **Coordination with E6-C3 (output sharpen).** E6-C3 (output-sharpening pass,
    `IP-04 2.6`) **reuses this exact L\* sharpen form** post-geometry. Factor the
    L\* recombine so E6-C3 can call it (the `CombineKind` variant + the
    color-reconstruct-around-L\* helper is the shared unit) rather than
    re-implementing. Note the dependency direction: E6-C3 depends on this card.
  - **Headroom.** Sharpen runs after tone; a pixel can carry L\* > 100 (highlight
    headroom). Reconstructing a\*,b\* around an out-of-`[0,100]` L\* must stay
    finite and not clamp the highlight — verify the Lab inverse is sane for
    L\* slightly > 100 (don't hard-clamp L\* to 100 before the inverse).
- Acceptance:
  - Behavior: an edge is sharpened in **L\* only** — no edge color fringing (a
    neutral or single-hue edge stays the same hue after sharpening, the three
    channels no longer overshoot independently); the overshoot is **perceptually
    symmetric** (the bright-side and dark-side excursions are equal in L\*, unlike
    the linear-light version which biases the bright halo); chroma/hue are carried
    from the input; zero amount is identity.
  - Tests (in `latent-pipeline` and/or `latent-cpu`):
    `sharpen_in_lstar_has_no_color_fringing` — a saturated single-hue step edge,
    after capture sharpen, keeps its hue at the overshoot (a\*/b\* ratio
    preserved; the linear per-channel version would shift it) — fails before, passes
    after. `sharpen_overshoot_is_symmetric_in_lstar` — on a symmetric dark↔light
    step, the L\* over- and undershoot magnitudes match within tolerance (the
    linear version's bright-side halo is asymmetric) — the headline N1 regression.
    `sharpen_zero_amount_is_identity`. Re-assert the existing
    `sharpening_overshoots_a_step_edge` (`latent-pipeline/src/lib.rs:1119`) and
    the CPU sharpen behavior tests in the L\* domain (the edge still overshoots —
    just now in L\* and per-luma). CPU↔reference parity: a CPU↔CPU-backend
    regression that both `combine` implementations produce the same L\* recombine
    (no drift). `render_matches_cpu_across_the_pipeline`
    (`latent-gpu/src/lib.rs`) stays green (combine delegates, so GPU==CPU).

---

### E5-C4 — Perceptual chroma denoise metric (Lab/LCh)
- Implements: `IP-04 2.4b` (M1, Medium — *Keep split, perceptual chroma metric*)            Priority: Medium
- Crates/files: `latent-pipeline/src/lib.rs:525-568` (`bilateral_pixel` — the **chroma** range term `:552-562`, the chroma split `:536`, and the docstring `:508-524`); consume E0-C1 Lab/LCh from `latent-image/src/color.rs`
- Depends on: **E0-C1** (the shared Lab/LCh module — hard dependency), E5-C1 (unified radius), and sequences after/with E5-C2 (same function)            Blocks: —
- Heads-up:
  - **Keep the luma/chroma split — this is the right NR variant.** §2.4b is
    explicit: **keep** the two-scale luma/chroma structure (chroma can smooth
    *harder* than luma — color noise is low-frequency blotches, luminance carries
    detail). This card does **not** collapse to a single-distance Tomasi &
    Manduchi filter; **document it as a deliberate luma/chroma *variant*** of the
    bilateral, not the T&M single-distance form. Do **not** remove the separate
    `params.luma` / `params.chroma` scales or the Y / (rgb−Y) split.
  - **What's wrong today — the chroma *metric*.** The chroma component is
    `cc = rgb − Y` (a **linear-RGB offset**, `latent-pipeline/src/lib.rs:536`),
    and the chroma range weight is the squared **linear-RGB** distance between
    those offsets: `dc2 = Σ_k (cc[k] − nc[k])²` → `wc = exp(spatial − dc2 ·
    inv_2sc2)` (`:552-557`). This linear-RGB-offset distance is **not
    perceptual**: equal numeric offsets are unequal perceived color differences,
    and — the L1 finding — **blue's luminance detail ends up governed by the
    chroma scale** because the `rgb−Y` split with the ~0.0001 blue luma weight
    leaves almost all of blue in the "chroma" component, so the chroma smoother
    eats blue *luminance* detail.
  - **The fix — measure chroma distance in a perceptual space (Lab/LCh).**
    Replace the linear-RGB-offset chroma metric with a **perceptual chroma
    distance** via the E0-C1 Lab/LCh path: convert the center and each neighbor
    working→Lab (E0-C1), and let the chroma range term be the distance in the
    **chromatic** coordinates only — i.e. the (a\*, b\*) (or LCh C\*/h)
    difference, `Δ_chroma = (Δa\*)² + (Δb\*)²` — so the range weight stops on
    *perceptual color* difference at constant lightness. The **luma** term stays
    as the L\* / luminance difference (it already stops on luminance; keep it the
    luma channel). Recombine: smooth the chromatic coordinates (a\*,b\*) with the
    chroma weights, keep/lightly-smooth L\* with the luma weights, then Lab→
    working back. This keeps the two-scale split but moves *both* the split and
    the chroma metric into the perceptual space, which also fixes the blue-detail
    leak (lightness lives in L\*, not in the chroma offset).
  - **Why.** A perceptual chroma metric makes "smooth color noise hard, keep
    color edges" actually track perceived color edges (iso-luminant color edges
    are defined perceptually, not by a linear-RGB offset), and removes the
    pathological coupling where blue luminance detail is treated as chroma. Same
    module as E1-C5/C6 and E5-C3 — **one** Lab/LCh owner (E0-C1).
  - **CPU-only — no GPU mirror owed (yet).** Same as E5-C2: the bilateral is
    CPU-only by delegation (`latent-gpu/src/lib.rs:552-556`), so this edits **only**
    the shared `bilateral_pixel` (CPU backend + pipeline reference). **No WGSL
    bilateral exists to mirror.** State this; the obligation to mirror the Lab
    metric transfers *if/when* the bilateral is ported to WGSL (it isn't today).
  - **Sequencing with E5-C2.** Both edit `bilateral_pixel`. E5-C2 changes the
    **spatial** Gaussian (σ_s = r/3); this changes the **chroma range** metric —
    orthogonal concerns. Do them in order (C2 then C4) or one combined pass, but
    keep them independently tested. Land **E0-C1 first** (hard dependency) and
    **E5-C1 first** (the radius `r`).
  - **Performance note (flag, don't over-engineer).** Naively this converts
    center↔neighbor to Lab inside the inner `(2r+1)²` loop — a cube-root per tap.
    For correctness that's fine and the test bar doesn't require optimization;
    but note that precomputing each pixel's Lab once into a scratch buffer (Lab
    of the whole image, then the bilateral reads from it) is the obvious O(N)
    speedup if profiling later demands it. State the precompute option; don't
    block the card on it.
- Acceptance:
  - Behavior: the luma/chroma split is **kept** (documented as a variant, not
    T&M single-distance); the **chroma** range term measures perceptual (Lab/LCh
    a\*,b\* / C\*,h) distance, not linear-RGB offset; color noise still smooths
    hard while **iso-luminant color edges** are preserved; **blue luminance
    detail is no longer eaten by the chroma scale** (the L1 regression).
  - Tests (in `latent-pipeline` and/or `latent-cpu`):
    `chroma_metric_is_perceptual` — two neighbors with equal *linear-RGB* chroma
    offset but unequal *perceptual* color difference get appropriately different
    chroma weights (the old linear metric weighted them equally) — fails before,
    passes after. `denoise_preserves_blue_luminance_detail` — a blue region with
    a luminance step (constant hue, varying lightness) keeps the step after
    chroma NR (the old `rgb−Y` split smoothed it because blue lived in the chroma
    component) — the headline L1 regression. Keep `denoise_color_smooths_chroma_
    independently_of_luma` (`latent-cpu/src/lib.rs:526`) and `denoise_luma_smooths_
    a_flat_tone_but_preserves_an_edge` (`:496`) green (the split is retained). A
    doc/test note pins that this is the *luma/chroma variant*, distinct from the
    single-distance T&M filter.

---

## Epic done when

- **Radius semantics are unified** (E5-C1): `blur`, `denoise`, and the
  clarity/sharpen radius gates interpret "radius" identically (one `round`, one
  half-window, one identity threshold), there is **no value that passes an enable
  gate yet rounds to a no-op kernel** (the blur-radius-0.3 surprise is gone), and
  the dehaze patch is expressible as a radius in the same units — coordinated with
  **E4-C4** (the dehaze patch's `// E5-C1` marker closed).
- The **bilateral spatial Gaussian is σ_s = r/3** (E5-C2) so the `±r` window is
  the standard ±3σ support (smooth falloff, no boundary step), same tap count.
- The **capture sharpen runs in the L\* perceptual domain** (E5-C3): unsharp on
  L\* only with color reconstructed around the sharpened lightness — **no edge
  color fringing** and **perceptually symmetric overshoot** — and the L\* sharpen
  form is factored so **E6-C3** (output sharpen) reuses it.
- The **denoise chroma metric is perceptual** (E5-C4): the luma/chroma split is
  **kept** (documented as the deliberate NR variant, not the T&M single-distance
  filter) and the chroma range term measures Lab/LCh distance, fixing the
  blue-luminance-detail leak (L1).
- **Backend lockstep holds:** `blur`'s unified radius is mirrored across CPU +
  pipeline reference + the `box_blur.wgsl`/GPU path with `blur_matches_cpu` green;
  the `combine` L\* recombine is identical in the CPU backend and the pipeline
  reference (no drift), with the GPU delegating `combine` (no WGSL); the bilateral
  stays **CPU-only** by delegation (no WGSL mirror owed today — each card notes the
  obligation transfers *if/when* the bilateral is ported), so the CPU↔GPU
  equivalence (`render_matches_cpu_across_the_pipeline`) stays green by
  delegation.
- **Clarity is untouched** (`IP-04 2.3` verified correct); the dehaze overhaul
  (E4) and output-sharpening pass (E6-C3) are **not** authored here.
- All four cards consume the **one** E0-C1 Lab/LCh / L\* module (no second
  perceptual path); E5-C3/E5-C4 state the hard E0-C1 dependency.
- Each card's named tests fail before / pass after (Global rule #2); the baseline
  (`cargo fmt --check`, `cargo clippy --all-targets` zero warnings,
  `cargo test --workspace`) is green after every card (Global rule #3).
