# Decision Register — Code Review 01 (RAW & Image Foundations)

**Source doc:** [`../../code-review/01-raw-and-image-foundations.md`](../../code-review/01-raw-and-image-foundations.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Triaged **autonomously**, in finding order (H1→N4 plus §3/§4/§5 cross-cutting items), optimizing for correctness / FFI-`unsafe` soundness / robustness / safety — matching the maintainer's established "highest-quality / most-correct option" pattern from the image-processing decision registers. Spot-checked `latent-raw/src/lib.rs` (decode order, `normalized`/`apply_white_balance` indexing) where soundness hinges on it.

This file records, for every finding of the code review, whether the software will be changed or kept as-is, the rationale, and the concrete action implied. It does **not** itself modify any source. Where a finding overlaps an already-decided image-processing item it is marked **Covered by &lt;ref&gt;** and the work is *not* re-planned here.

---

## Decision summary

| Finding | Severity | Decision | Outcome |
|---|---|---|---|
| **H1** — Foveon/full-color `filters==0` → `cfa=[6,6,6,6]` OOB panic (`lib.rs:164,205,504`) | High | **Fix — covered by IP-02 §2.8b** (+ ordering note) | Change |
| **H2** — `from_raw_parts` ignores `raw_pitch` (`lib.rs:461-470`) | High | **Fix — honor `raw_pitch`** | Change |
| **M1** — X-Trans (`filters==9`) passes Bayer guard, mis-demosaiced (`lib.rs:435,475`) | Medium | **Fix — covered by IP-02 §2.8b** | Change |
| **M2** — unchecked `width*height` in `ImageBuf` (`lib.rs:18,56`) | Medium | **Fix — `checked_mul` fallible ctor** | Change |
| **L1** — `h6 as u32 == 6` sextant boundary in `hsv_to_rgb` (`color.rs:216`) | Low | **Fix — clamp sextant** | Change |
| **L2** — undocumented `mosaic.len()`/`cfa∈0..4` invariants (`lib.rs:153,193`) | Low | **Fix — `debug_assert` + clamp at source** | Change |
| **L3** — `tone::eval` relies on `lut.len() >= 2` (`tone.rs:57`) | Low | **Fix — enforce invariant in ctor** | Change |
| **N1** — `get`/`set` panic-only bounds story, undocumented (`lib.rs:61,66`) | Nit | **Fix — document panic + add `try_get`/`try_set`** | Change |
| **N2** — missing `#[must_use]` on pure accessors / `Mat3` ops (`lib.rs:27,31,36,46`) | Nit | **Fix — add `#[must_use]`** | Change |
| **N3** — `c_str_field` doc under wrong `# Safety` block (`lib.rs:495`) | Nit | **Fix — relocate doc comment** | Change |
| **N4** — runtime `.expect` on compile-time constants (`color.rs:117,143`) | Nit | **Fix — `const`/test-assert construction** | Change |
| **§3** — FFI / `unsafe` soundness assessment | Positive* | **Keep** (gaps = H1/H2, already registered) | No change |
| **§4** — test-coverage gaps (8 items + fixture footgun) | Gaps | **Fix — add the missing tests** | Change (tests) |
| **§5** — positives worth keeping | Positive | **Keep** (guard with tests) | No change |

\* §3 is a *verified-correct* assessment of the FFI surface; its only open items are H2 and H1, both registered above.

**Tally:** 11 findings + §4 test work = **Fix**; **2 Keep** (§3 and §5 — verified-correct / positives). Of the Fixes, **H1 and M1 are Covered by IP-02 §2.8b** (the `filters`/`colors` sensor guard) and are not re-planned here; the rest are net-new engineering/soundness work owned by this register. The throughline is FFI/`unsafe` soundness (H2 `raw_pitch`, the H1 Foveon panic, the `from_raw_parts` length), numeric robustness (M2 overflow, L1 boundary), and closing test gaps.

---

## Per-finding decisions

### H1 — Foveon / full-color `filters==0` OOB panic · **Fix — Covered by IP-02 §2.8b** *(High)*
- **Decision:** Fix by **rejecting non-Bayer layouts at decode**, which is exactly the remedy already chosen in [IP-02 §2.8b](../02-raw-decode-and-demosaic.md) (read `idata.filters`/`idata.colors`; accept only true 2×2 Bayer — `filters != 0 && filters != 9 && colors == 3`). With `filters==0` rejected, `cfa` never becomes `[6,6,6,6]` and the value-`6` index into the 4-element `cblack`/`gains` arrays is unreachable. **Do not plan a separate fix here.**
- **Why:** `libraw_COLOR()` returns the special value `6` when `idata.filters == 0` (Foveon/Sigma, full-color sensors). Today only `cdesc=="RGBG"` is checked, which `filters==0` bodies satisfy, so the `RawImage` decodes successfully and then panics later at `cblack[6]` (`normalized`, `lib.rs:164-165`) / `gains[6]` (`apply_white_balance`, `lib.rs:204-205`). A panic on untrusted RAW input violates the decode layer's "never panic, return a typed `RawError`" contract — a robustness/DoS bug.
- **Action / ordering note (this register's contribution):** The structural fix lives in IP-02 §2.8b, but soundness depends on **placement**: the `filters` guard MUST run in `unpack` *before* any `cfa`/`cblack`/`gains`/`channel_at` indexing — i.e. at or before the current `is_rgb_bayer` gate (`lib.rs:475-477`), which already precedes `normalized`/`apply_white_balance`. The decode order is `from_raw_parts` (470) → `read_metadata` (471, which builds `cfa`) → guard (475); since `cfa` is only *indexed* in the post-decode pipeline, gating at line 475 provably prevents the value-`6` index. **Belt-and-suspenders (own this here):** in `read_metadata` (`lib.rs:512-516`), clamp/validate each `cfa` entry to `0..4` (or treat any out-of-range CFA code as a decode error) so the 4-element-array invariant holds defensively even if the guard is ever bypassed.
- **Overlaps / deps:** **Covered by IP-02 §2.8b.** Also see IP-02 §2.10 / its cross-note, which explicitly hands the Foveon panic to this code-review track while noting §2.8b's `filters` guard fixes it structurally. Test gap §4.1 (below) regresses it.

### H2 — `from_raw_parts` length ignores `raw_pitch` · **Fix — honor `raw_pitch`** *(High)*
- **Decision:** Stop assuming the raw buffer is tightly packed at `raw_width*raw_height` `u16`. Read `sizes.raw_pitch` and **copy row-by-row honoring the stride** (`pitch_u16 = raw_pitch/2`): slice the source as `from_raw_parts(samples, pitch_u16 * raw_height)` and copy `raw_width` samples from each of `raw_height` rows at `pitch_u16` stride into a tight `raw_width*raw_height` mosaic. If `raw_pitch == 0` (some paths leave it unset), fall back to `raw_width*2`. This is the highest-correctness option: it is sound for padded unpackers *and* fixes the sheared-image indexing, versus merely asserting the tight-packing invariant.
- **Why:** This is the single load-bearing `unsafe` length in the crate, and its soundness currently rests on an *unverified* assumption about LibRaw's allocator. `libraw_image_sizes_t::raw_pitch` exists precisely because the row stride of `raw_image` is **not always** `raw_width*2` bytes — some unpackers pad rows. When `raw_pitch/2 > raw_width`: (a) the last-row read can over-read past `raw_alloc` → **undefined behavior**, and (b) every later `y*width+x` index addresses the wrong photosite (sheared output). Honoring the pitch removes both the UB and the shear.
- **Action:** Rework the `from_raw_parts` block in `unpack` (`lib.rs:461-470`). Add `let pitch_u16 = { let p = (*raw).sizes.raw_pitch as usize / 2; if p == 0 { width as usize } else { p } };`, build the source slice with length `pitch_u16 * height`, and copy per row. At minimum (if the row-copy is deferred) add `debug_assert_eq!(raw_pitch as usize, raw_width as usize * 2)` and a typed error on mismatch — but the row-copy is preferred (correct, not just guarded). Capture `raw_pitch` in `Metadata` if useful for a regression fixture.
- **Overlaps / deps:** Independent of the IP track (IP-02 §2.10 deemed the *numeric* lifecycle correct and explicitly defers `raw_pitch` here). Coordinate touch-points with the IP-02 `read_metadata` extension (both edit the same `unpack`/`read_metadata` region) to avoid churn. Test gap §4.2 documents the expected pitch.

### M1 — X-Trans passes the Bayer guard, silently mis-demosaiced · **Fix — Covered by IP-02 §2.8b** *(Medium)*
- **Decision:** Fix via the same `filters` validation as H1 — **Covered by [IP-02 §2.8b](../02-raw-decode-and-demosaic.md)** (`filters != 9` rejects X-Trans explicitly). Reject the file with a clean typed error rather than silently sampling a 2×2 corner of the 6×6 X-Trans pattern. **Do not plan a separate fix here.**
- **Why:** X-Trans (Fuji) sets `filters == 9` and a 6×6 CFA but still reports RGB filters, so `cdesc=="RGBG"` and `is_rgb_bayer` returns `true`; the decoder then demosaics a 6×6 pattern as if 2×2 Bayer → scrambled colors with no error. A clean rejection is correct until a real 6×6 X-Trans demosaic exists.
- **Action:** None new — folded into IP-02 §2.8b's rewrite of `is_rgb_bayer` (`lib.rs:435-437`) and the gate (`lib.rs:475-477`). Test gap §4.1 regresses the rejection path.
- **Overlaps / deps:** **Covered by IP-02 §2.8b** (same single check closes H1 + M1). Note IP-02 §2.1 lays X-Trans *black-pattern* groundwork but X-Trans stays rejected at decode.

### M2 — unchecked `width*height` in `ImageBuf` · **Fix — `checked_mul` fallible ctor** *(Medium)*
- **Decision:** Compute the element count with `checked_mul` and surface a fallible constructor (`try_new` → `Result`/`Option`, or make `ImageBuf::new` return `Result`). Use the *same* checked computation in `index` so `new` and `index` can never disagree. Highest-robustness option over "document 64-bit-only," because `ImageBuf` accepts arbitrary `u32` dims (not just the u16 RAW dims) and is a general buffer type.
- **Why:** `new` (`lib.rs:18-25`) and `index` (`lib.rs:56-58`) both do `width as usize * height as usize` (product up to `~1.8e19`). On **32-bit** the multiply wraps silently → `new()` under-allocates while `index()` computes a different (also-wrapped) offset → logic error / OOB read-write. On 64-bit it can't overflow `usize` but a huge `vec![..; count]` aborts the process ungracefully. A checked count fixes both: a graceful error instead of OOB (32-bit) or abort (64-bit).
- **Action:** Edit `ImageBuf::new` and `index` (`latent-image/src/lib.rs:18,56`). Provide `try_new`; if the existing `new` must stay infallible for ergonomics, have it call `try_new().expect(...)` with a documented panic, and route RAW-sized construction through the checked path. Add a unit test for the overflow boundary.
- **Overlaps / deps:** None in the IP track. Pairs naturally with N1 (the `ImageBuf` bounds/`#[must_use]` cleanup).

### L1 — `h6 as u32 == 6` sextant boundary in `hsv_to_rgb` · **Fix — clamp sextant** *(Low)*
- **Decision:** Make the sextant explicit: `let sextant = (h6 as u32).min(5);` before the match, so hue-0 deterministically lands in arm `0`. Worth fixing despite being "currently correct by accident."
- **Why:** `f32::rem_euclid(1.0)` returns *exactly* `1.0` for tiny negative inputs (`-1e-9`, …) because `1.0-ε` rounds to `1.0` in f32, so `h6 == 6.0` and `h6 as u32 == 6` is reachable — and `color_mix` feeds slightly-negative hue shifts (`color.rs:249,252`). It currently produces the right pixel only because `x==0` at that exact boundary collapses the magenta `_` arm to pure red; any change to the `x` formula or arm order silently breaks it. Clamping removes the latent fragility for a one-line cost.
- **Action:** Edit `hsv_to_rgb` (`latent-image/src/color.rs:212-223`, the match at `:216`). Add the boundary + wraparound test (§4.4).
- **Overlaps / deps:** None (engineering robustness, not algorithm). The HSV color model itself is untouched by the IP color-science redesign.

### L2 — undocumented `mosaic.len()` / `cfa∈0..4` invariants · **Fix — `debug_assert` + clamp at source** *(Low)*
- **Decision:** Document and *enforce* the invariants. The `cfa ∈ 0..4` half is subsumed by H1's belt-and-suspenders clamp in `read_metadata` (validate CFA codes at the FFI boundary, the single source). For `mosaic.len() == width*height`, add a `debug_assert_eq!(mosaic.len(), w*h)` at the top of `normalized`/`apply_white_balance` and a doc line stating the invariant.
- **Why:** `normalized()` (`lib.rs:153`) and `apply_white_balance()` (`lib.rs:193`) silently assume `mosaic.len() == width*height` and `cfa` values in `0..4`; nothing documents or checks it, so a future caller or a malformed `RawImage` corrupts indexing without a clear failure. Cheap guards turn a silent logic error into a loud debug-time failure.
- **Action:** Add `debug_assert`s + doc in `latent-raw/src/lib.rs:153,193`; clamp CFA at `read_metadata` (`lib.rs:512-516`, shared with H1). Fix the §4 fixture footgun (below) so the asserts don't false-positive on the existing test.
- **Overlaps / deps:** Shares the CFA-clamp work with **H1** (IP-02 §2.8b region). The `normalized` body is also edited by IP-02 §2.1/§2.2/§2.3 — coordinate the assert placement with those.

### L3 — `tone::eval` relies on `lut.len() >= 2` · **Fix — enforce invariant in ctor** *(Low)*
- **Decision:** Enforce `lut.len() >= 2` at construction (the only place a `ToneCurve`/LUT is built) — reject or clamp shorter LUTs there — so `eval`'s `lut[n-2]` end-slope read can never panic regardless of future constructors. Prefer enforcing the invariant over scattering bounds checks into the hot `eval` path.
- **Why:** `eval` (`tone.rs:57`) reads `lut[n-2]`, relying on a private, currently-upheld invariant. It's safe today, but a future alternate constructor that admits a 0/1-element LUT would make `eval` panic. Centralizing the invariant at the boundary is the durable fix.
- **Action:** Add the length check/clamp to the LUT constructor in `latent-image/src/tone.rs`; document the `len >= 2` precondition on `eval`. Add the end-slope / lower-extrapolation tests (§4.7).
- **Overlaps / deps:** None in the IP track (tone *curve evaluation* mechanics; distinct from the IP tone-grading algorithm work in IP-03).

### N1 — `get`/`set` panic-only, undocumented bounds story · **Fix — document + add `try_*`** *(Nit)*
- **Decision:** Keep the panicking `get`/`set` for the hot path (reasonable), but (a) document the panic contract on the type, and (b) add non-panicking `try_get`/`try_set` (returning `Option`) so callers that can't guarantee bounds have a checked path. This is a genuine safety/API improvement, not pure style.
- **Why:** Today the panic is the *only* bounds story with no `checked_*` variant and no type-level documentation — a footgun for callers handling untrusted dims.
- **Action:** Edit `latent-image/src/lib.rs:61,66`; add `try_get`/`try_set`, document the panic on `get`/`set`. Pairs with M2 (both are `ImageBuf` robustness).
- **Overlaps / deps:** None in the IP track.

### N2 — missing `#[must_use]` on pure accessors / `Mat3` ops · **Fix — add `#[must_use]`** *(Nit)*
- **Decision:** Add `#[must_use]` to the pure accessors (`width`, `height`, `len`, `is_empty`, `pixels`) and the value-returning `Mat3` ops. Genuine idiom/safety improvement (catches discarded results at compile time).
- **Action:** Annotate in `latent-image/src/lib.rs:27,31,36,46` (and the `Mat3` impls).
- **Overlaps / deps:** None.

### N3 — `c_str_field` doc under wrong `# Safety` block · **Fix — relocate doc comment** *(Nit)*
- **Decision:** Move the doc comment so the `read_metadata` `# Safety` clause documents `read_metadata`, and `c_str_field` gets its own doc. Spot-checked: at `lib.rs:489-502` the `# Safety` block for `read_metadata` is physically split by `c_str_field`'s definition, so the safety note currently reads as if it documents the wrong item.
- **Why:** Misleading safety documentation on `unsafe` code is a real hazard for future maintainers even though it's cosmetic at runtime.
- **Action:** Reorder so `read_metadata`'s `/// # Safety` sits immediately above `unsafe fn read_metadata` (`lib.rs:504`); give `c_str_field` (`lib.rs:495`) its own doc. (`c_str_field` is safe — it takes a slice — so it needs no `# Safety`.)
- **Overlaps / deps:** None.

### N4 — runtime `.expect` on compile-time constants · **Fix — `const`/test-assert construction** *(Nit)*
- **Decision:** Remove the runtime `expect` paths on compile-time-constant matrix construction by either evaluating in `const` context or asserting the inversion once in a unit test, so the unreachable `expect("primaries are linearly independent")` (and sibling) doesn't ship as runtime panic machinery.
- **Why:** Genuine clarity/idiom improvement: an `expect` on a constant is dead runtime code that obscures the fact the value is statically known-good.
- **Action:** `latent-image/src/color.rs:117,143` — prefer a `const`-evaluated build or a test that asserts the matrices invert. Low effort.
- **Overlaps / deps:** This code is **also rebuilt by IP-01 (color science)** — the working-space/sRGB matrices are reconstructed at D50 with Bradford CA. Fold N4 into that rework (build the new constants without runtime `expect`) rather than editing twice. Cross-ref [IP-01 §#1/#2/#3](../01-color-science.md).

### §3 — FFI / `unsafe` soundness assessment · **Keep (verified-correct)**
- **Decision:** No change. The assessment verifies the FFI surface is sound: RAII `Handle` (exactly-once `libraw_close`, no leak/double-free), static-string FFI, `unpack` lifecycle, `libraw_COLOR` raw-coordinate/phase consistency, `c_char→u8` casts, bindgen `const _` size/offset assertions, and the `cblack[0..4]`-within-`[c_uint;4104]` read. Keep these as-is and keep their guarding tests.
- **Open items:** The only soundness gaps it flags are **H2** (`raw_pitch`) and **H1** (the value-`6` CFA path) — both registered above. Nothing else to do.

### §4 — Test-coverage gaps · **Fix — add the missing tests** *(Gaps)*
- **Decision:** Add the missing tests; the suite is strong on the happy path but blind to the exact failure modes this review found. Priority-ordered (matching the doc):
  1. **Non-Bayer / value-`6` CFA path (H1/M1):** construct a `RawImage` with `filters==0`/`cfa` containing `6` (and an X-Trans `filters==9` case) and assert a clean `Err(RawError::NoMosaic)` post-fix (today it panics). **Regresses H1 + M1.**
  2. **`raw_pitch` / stride (H2):** a fixture documenting expected pitch + the `debug_assert`/row-copy behavior.
  3. **FFI error paths:** `Unpack`, `NoMosaic` (null `raw_image` *and* non-RGBG `cdesc`), and `InvalidPath` (interior-NUL path — trivially testable without LibRaw). Only `RawError::Open` is currently exercised.
  4. **HSV/color extremes:** achromatic early-return (`s<=1e-6`), `color_mix` hue at band center / wraparound (the **L1** boundary), negative/super-unit value, and NaN/Inf into `rgb_to_hsv` (pin the `f32::max/min` NaN-dropping behavior).
  5. **`Mat3::inverse` near the `1e-12` threshold** (det just above/below).
  6. **`downscaled` edge cases:** `max_dim==0`, 1-pixel, extreme aspect ratios, the `span` `b.max(a+1)` div-by-zero guard.
  7. **`tone::eval`** lower-extrapolation + `i >= n-1` clamp + exact `lut[n-2]` end-slope (ties to **L3**).
  8. **Seeded randomized sweeps** for `hsv` round-trip and `Mat3` inverse to widen coverage cheaply.
- **Plus the fixture footgun:** fix `white_balance_neutralizes_a_gray_patch` (`lib.rs:610-638`) — its `RawImage` has `width:2, height:2, mosaic: vec![]` (inconsistent), so it'd break L2's `debug_assert` and any future method reading `self.mosaic`. Make the fixture's `mosaic`/`width`/`height` consistent.
- **Overlaps / deps:** Tests 1 regress the IP-02 §2.8b guard; coordinate with the IP-02 test additions (X-Trans/Foveon rejection, `cblack`/`linear_max`).

### §5 — Positives worth keeping · **Keep**
- **Decision:** No change. Preserve (and keep the tests guarding) the RAII `Handle`, typed `RawError`, bindgen `const _` ABI assertions, the `libraw_COLOR`/raw-buffer CFA-phase correctness, the pervasive numerical guards, idiomatic `array::from_fn`/iterator style, the doc comments, and especially the demosaic round-trip / all-CFA-phase test harness. The fixes above must not regress these.

---

## Resulting implementation notes (derived from the decisions)

Sequenced so the shared FFI/decode edits land once:

1. **Sensor guard (H1 + M1) — Covered by [IP-02 §2.8b](../02-raw-decode-and-demosaic.md).** Read `idata.filters`/`idata.colors` in `read_metadata`; reject non-Bayer (`filters==0` Foveon, `filters==9` X-Trans, non-3-color) at the `unpack` gate (`lib.rs:475-477`) **before** any `cfa`/`cblack`/`gains` indexing. *This register's add-ons:* (a) the ordering guarantee (guard precedes `normalized`/`apply_white_balance`), and (b) the belt-and-suspenders `cfa ∈ 0..4` clamp in `read_metadata` (also satisfies **L2**'s CFA half).
2. **H2 — honor `raw_pitch`** in the `from_raw_parts` path (`lib.rs:461-470`): row-by-row copy at `pitch_u16` stride, `raw_pitch==0` → `raw_width*2` fallback. The one genuinely unverified `unsafe` invariant — close it with a correct copy, not just an assert.
3. **M2 — `checked_mul`** element count in `ImageBuf::new`/`index` (fallible `try_new`); pair with **N1** (`try_get`/`try_set` + documented panic) and **N2** (`#[must_use]`).
4. **L1 — clamp the `hsv_to_rgb` sextant** (`color.rs:216`).
5. **L2 — `debug_assert`s + docs** in `normalized`/`apply_white_balance` (CFA-clamp shared with step 1); coordinate placement with IP-02 §2.1/§2.2/§2.3 which rewrite `normalized`.
6. **L3 — enforce `lut.len() >= 2`** in the tone-LUT constructor (`tone.rs`).
7. **N3 — relocate** the `read_metadata` `# Safety` doc (`lib.rs:489-504`); **N4 — drop runtime `expect`** on the color constants (`color.rs:117,143`) — **fold into the IP-01 color-core rebuild**, not a separate edit.
8. **§4 — add the missing tests** (esp. the H1/M1 panic→`Err` regression and the FFI error-path coverage); fix the `white_balance_neutralizes_a_gray_patch` fixture.
9. **No-ops:** **§3** (FFI soundness, modulo H1/H2) and **§5** (positives) confirmed correct — leave unchanged and keep their guarding tests.

**Cross-refs to IP decisions / initiatives:**
- **H1 + M1 → [IP-02 §2.8b](../02-raw-decode-and-demosaic.md)** (sensor guard via `filters`/`colors`) — *the* dedup; the Foveon panic is fixed structurally by that guard (this register only pins the ordering + CFA clamp). See also IP-02 §2.10's cross-note handing these engineering defects to this track.
- Any **`cblack` / `linear_max`** touch-points in `normalized` → **[IP-02 §2.1 / §2.2](../02-raw-decode-and-demosaic.md)** (full `cblack` pattern; per-channel `linear_max`). L2's assert placement and H1's CFA clamp must coordinate with that `normalized` rewrite.
- **N4** color-constant construction → **[IP-01 §#1/#2/#3](../01-color-science.md)** (color core rebuilt at D50 ProPhoto + Bradford CA) — build the new constants without runtime `expect`.
- This register reflects intent only; nothing here has been implemented. Initiative bucket **(F) robustness / sanitize-on-load** is the home for H1/H2/M2/L1/L2/L3; **(B) sensor-metadata decode correctness** is the home for the H1/M1 dedup.
