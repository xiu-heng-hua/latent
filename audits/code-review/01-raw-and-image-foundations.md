# Code Review 01 — RAW & Image Foundations

**Scope:** `latent-raw` (LibRaw FFI: `src/lib.rs`, `build.rs`) and `latent-image`
(`src/lib.rs`, `src/color.rs`, `src/tone.rs`).

**Lens:** software-engineering correctness — bugs, panics, overflow, FFI/`unsafe`
soundness, error handling, API design, edge cases, test coverage, performance,
maintainability. Image-processing *algorithm* correctness is out of scope (a
separate audit covers it under `audits/image-processing/`).

**Environment verified against:** LibRaw 0.22.1 (`/usr/include/libraw`), bindgen
0.72.1, target `x86_64` (`c_char = i8`), `cargo` workspace edition 2024.

**Baseline:** `cargo clippy -p latent-raw -p latent-image --all-targets` is clean
(zero warnings); `cargo test -p latent-raw -p latent-image` passes (35 + 15 = 50
tests). All findings below are *beyond* what those gates catch.

---

## 1. Overview & overall quality assessment

These two crates are **well-engineered and unusually well-documented for
foundational code**. The FFI layer is small and disciplined: a single RAII
`Handle` guarantees `libraw_close` runs exactly once on every path, every LibRaw
return code is checked, the `raw_image` pointer is null-checked before use, and
no panic is allowed to cross the FFI boundary (errors are a typed `RawError`).
The image/color/tone math is clean, idiomatic (heavy, correct use of
`std::array::from_fn`), and backed by a genuinely good test suite — including a
forward-mosaic→demosaic round-trip harness and an all-CFA-phases test that would
catch whole classes of silent regressions. Numerical guards (`(white-black).max(1.0)`,
the `1e-12` singular thresholds, `rem_euclid`, row-normalization) show real care.

The defects that exist cluster around **one root cause: the decode path validates
the CFA *descriptor* (`cdesc == "RGBG"`) but never validates LibRaw's `filters`
field.** Because of that single gap, two distinct non-Bayer sensor families
(Foveon / full-color, and Fuji X-Trans) slip past the guard — one leading to an
**out-of-bounds panic** in a later pipeline stage, the other to silent
mis-demosaicing. There is also a real **FFI soundness assumption** about the
`raw_image` buffer length that the code never verifies (`raw_pitch` is ignored),
and a verified-but-currently-benign floating-point fragility in `hsv_to_rgb`.

Overall grade: **high quality with a small number of real, fixable correctness
gaps**, two of which are reachable from untrusted RAW input.

---

## 2. Findings by severity

| # | Sev | Location | Summary |
|---|-----|----------|---------|
| H1 | **High** | `latent-raw/src/lib.rs:164,205` (+`504`) | Foveon/full-color sensor (`filters==0`) yields `cfa=[6,6,6,6]`; `cblack[6]`/`gains[6]` panic (index OOB) in `normalized()`/`apply_white_balance()`. Reachable from input — `cdesc` guard does not catch it. |
| H2 | **High** | `latent-raw/src/lib.rs:470` | `from_raw_parts(samples, raw_width*raw_height)` ignores `sizes.raw_pitch`; if an unpacker pads rows (`raw_pitch != raw_width*2`), this is an OOB read and/or misaligned rows. Soundness rests on an unverified assumption. |
| M1 | **Med** | `latent-raw/src/lib.rs:435,475` | X-Trans sensors (`filters==9`) report `cdesc=="RGBG"` and pass `is_rgb_bayer`, but the 2×2 CFA sampling captures only a corner of the 6×6 pattern → silently mis-colored output instead of a clean rejection. |
| M2 | **Med** | `latent-image/src/lib.rs:18,56` | `ImageBuf::new` / `index` compute `width as usize * height as usize` with no overflow check. On a 32-bit target an attacker-sized image wraps `usize` → under-allocation vs. later index recompute → OOB. (64-bit: aborts on huge alloc instead — safe but ungraceful.) |
| L1 | **Low** | `latent-image/src/color.rs:216` | `h6 as u32` can equal `6` when `h.rem_euclid(1.0)` returns exactly `1.0` (verified for tiny-negative `h`, e.g. `-1e-9`). Falls into the `_` arm; *currently* correct only because `x==0` at that boundary. Fragile latent edge. |
| L2 | **Low** | `latent-raw/src/lib.rs:153,193` | `normalized()` / `apply_white_balance()` silently assume `mosaic.len() == width*height` and `cfa` values are in `0..4`; no debug assert documents the invariant. |
| L3 | **Low** | `latent-image/src/tone.rs:57` | `eval` reads `lut[n-2]`; relies on the (private, currently-upheld) invariant `lut.len() >= 2`. A future alternate constructor would make it panic. |
| N1 | Nit | `latent-image/src/lib.rs:61,66` | `get`/`set` panic on OOB rather than returning `Option`; reasonable for a hot path, but the panic contract is the *only* bounds story (no `checked_*` variant) and is undocumented at the type level. |
| N2 | Nit | `latent-image/src/lib.rs:27,31,36,46` | Several pure accessors (`width`, `height`, `len`, `is_empty`, `pixels`) and `Mat3` ops lack `#[must_use]`. |
| N3 | Nit | `latent-raw/src/lib.rs:495` | `c_str_field`'s doc comment is physically attached under `read_metadata`'s `# Safety` block; the safety note documents the wrong item (cosmetic, but misleading). |
| N4 | Nit | `latent-image/src/color.rs:117,143` | `.expect("primaries are linearly independent")` etc. are on compile-time constants — unreachable, but a `const`-evaluated or test-asserted construction would remove the runtime `expect` entirely. |

### Detailed findings

#### H1 — OOB panic on Foveon / full-color sensors (`filters == 0`)
`read_metadata` builds `cfa` by calling `libraw_COLOR(raw, row, col)` for the 2×2
top-left (lib.rs:512-516). LibRaw's `COLOR()` returns the **special value `6`**
when `imgdata.idata.filters == 0` (verified in `/usr/include/libraw/libraw.h:303-304`:
`if (!imgdata.idata.filters) return 6;`). That is the case for Foveon (Sigma) and
other full-color sensors, where `filters==0` and `cdesc` defaults to `"RGBG"`.

So `cfa` becomes `[6,6,6,6]`, and `unpack` still passes the only guard,
`is_rgb_bayer(&meta.cdesc)` (lib.rs:475), because that checks `cdesc`, not `cfa`/`filters`.
The `RawImage` is returned successfully. Then, when the pipeline calls:

```rust
// normalized(), lib.rs:164-165
let color = self.meta.cfa[(i / w % 2) * 2 + (i % w % 2)] as usize; // == 6
let black = base + self.meta.cblack[color] as f32;                 // cblack[6] on [u32;4] → PANIC
```
```rust
// apply_white_balance(), lib.rs:204-205
let color = self.meta.cfa[(y % 2) * 2 + (x % 2)] as usize;         // == 6
*px *= gains[color];                                                // gains[6] on [f32;4] → PANIC
```

both index a 4-element array with `6` → **index-out-of-bounds panic**, reachable
from a crafted/legitimate-but-unsupported RAW. (Even where it doesn't panic,
`channel_at` (lib.rs:210-213) silently maps `c==6` to channel 0/red, mis-coloring.)

**Why it matters:** a panic on untrusted input is a robustness/DoS bug; the decode
layer's whole contract is "never panic, return a typed error."

**Fix:** reject non-Bayer layouts at decode by validating `filters`. Read
`idata.filters` into `Metadata`, and in `unpack` require a real Bayer pattern
(`filters >= 1000` and `filters != 9`), e.g.:
```rust
if meta.filters == 9 || meta.filters < 1000 || !is_rgb_bayer(&meta.cdesc) {
    return Err(RawError::NoMosaic);
}
```
This single check resolves H1 **and** M1. (Belt-and-suspenders: also clamp/validate
each `cfa` entry to `0..4` in `read_metadata`.)

#### H2 — `from_raw_parts` length ignores `raw_pitch`
```rust
// lib.rs:461-470
let width  = (*raw).sizes.raw_width  as u32;
let height = (*raw).sizes.raw_height as u32;
let samples = (*raw).rawdata.raw_image;          // *mut ushort
let len = width as usize * height as usize;
let mosaic = std::slice::from_raw_parts(samples, len).to_vec();
```
`libraw_image_sizes_t` carries a distinct **`raw_pitch`** field (bytes per raw row;
bindings.rs:667, header `/usr/include/libraw/libraw_types.h:219`). It exists
precisely because the row stride of `raw_image` is **not always** `raw_width*2`
bytes — some unpackers allocate padded rows. The code assumes the tight
`raw_width*raw_height` packing and never consults `raw_pitch`.

If `raw_pitch/2 > raw_width` for a given file, two things break: (a) the slice
length `raw_width*raw_height` *under*-counts the buffer (benign over-read avoided,
but rows are then misaligned), or for the last row the assumption could *over*-read
past `raw_alloc` — **undefined behavior in `unsafe`**; and (b) every later
`y*width+x` index addresses the wrong photosite (sheared image).

**Why it matters:** this is the single load-bearing `unsafe` length in the crate.
Its soundness currently rests on an unstated invariant about LibRaw's allocator.

**Fix:** read `raw_pitch`, assert/handle the stride explicitly. Either (a) verify
`raw_pitch as usize == width as usize * 2` and return a typed error otherwise, or
(b) copy row-by-row honoring the pitch:
```rust
let pitch_u16 = (*raw).sizes.raw_pitch as usize / 2;
// build mosaic by copying width samples from each of `height` rows at `pitch_u16` stride,
// from a base slice of len `pitch_u16 * height`.
```
At minimum, document the invariant and add a `debug_assert_eq!(raw_pitch, raw_width*2)`.

#### M1 — X-Trans passes the Bayer guard and is silently mis-demosaiced
X-Trans (Fuji) sensors set `filters == 9` (`LIBRAW_XTRANS`,
`/usr/include/libraw/libraw_const.h:715`) and a 6×6 CFA, but still describe RGB
filters, so `cdesc == "RGBG"` and `is_rgb_bayer` returns `true` (lib.rs:435). The
decoder then samples only a 2×2 corner of the 6×6 pattern via
`libraw_COLOR(raw, 0..2, 0..2)` and demosaics as if Bayer — producing wrong colors
with no error. **Fix:** same `filters` validation as H1 (`filters != 9`).

#### M2 — unchecked `width*height` in `ImageBuf`
`ImageBuf::new` (lib.rs:18-25) and `index` (lib.rs:56-58) both do
`width as usize * height as usize`. `ImageBuf` accepts arbitrary `u32` dims (not
just the u16 RAW dims), so the product can reach `~1.8e19`. On 64-bit this can't
overflow `usize` but `vec![..; count]` will attempt an enormous allocation and
abort the process. On a **32-bit** target the multiply wraps silently, so
`new()` under-allocates while `index()` computes a *different* (also-wrapped) offset
→ logic error / OOB. **Fix:** compute the count with `checked_mul` and return a
`Result`/`Option` from a fallible constructor, or document/assert a 64-bit-only
contract. Lower priority if 32-bit is officially unsupported.

#### L1 — `h6 as u32 == 6` boundary in `hsv_to_rgb`
`hsv_to_rgb` (color.rs:212-223) computes `h6 = h.rem_euclid(1.0) * 6.0` then matches
`h6 as u32`. I verified empirically that `f32::rem_euclid(1.0)` returns **exactly
`1.0`** for tiny negative inputs (`-1e-9`, `-1e-12`, `-f32::MIN_POSITIVE`), because
`1.0 - ε` rounds to `1.0` in f32. `color_mix` feeds `hsv_to_rgb(h + adj[0], …)`
(color.rs:249,252) where `adj[0]` is a hue shift that can be slightly negative, so
`h6 == 6.0` and `h6 as u32 == 6` is reachable. That falls into the `_` arm
(`(c, 0.0, x)`), which is the *magenta* assignment — **but** at `h6 == 6.0`,
`x = c*(1 - |(6%2)-1|) = c*(1-1) = 0`, so the output collapses to `(c,0,0)` = pure
red, which is correct for hue 0. So this is **currently correct by accident**, not a
live bug — but it is fragile (any change to the `x` formula or the arm order breaks
it). **Fix:** make the intent explicit, e.g. `let sextant = (h6 as u32).min(5);`
before the match, so hue-0 wraps to arm `0` deterministically.

---

## 3. FFI / `unsafe` soundness assessment

The FFI surface is **small, auditable, and mostly sound.** Verified item by item:

- **`Handle` RAII (lib.rs:75-98):** `libraw_init(0)` is null-checked; `Drop` calls
  `libraw_close(self.0)` exactly once. There is no `Clone`/`Copy`, no public
  constructor that bypasses the null check, and every `?` in `unpack` still drops
  the handle. **No leak, no double-free.** Solid.
- **Static-string FFI (`version`, `strerror`, lib.rs:24-31,64-70):** both wrap
  pointers documented (and verified in the header) to be static NUL-terminated C
  strings; `CStr::from_ptr` + `to_string_lossy`. Sound. (`version()` does not even
  need a `Handle`, correctly.)
- **`unpack` lifecycle (lib.rs:441-487):** correct open→unpack→read order, each
  return code checked, `raw_image` null-checked before deref (`NoMosaic`).
- **`libraw_COLOR` coordinate consistency (verified positive):** the CFA pattern is
  sampled with `libraw_COLOR(raw, row, col)` in **raw-buffer** coordinates, and the
  mosaic read from `rawdata.raw_image` is *also* in raw-buffer coordinates (origin
  includes `top/left_margin`). LibRaw's `FC(row,col)` (header:312-315) is a pure
  function of raw row/col with period 2, so the sampled 2×2 phase and the
  `(y%2)*2+(x%2)` indexing **agree**. There is *no* margin/phase mismatch here —
  this is a correct and easy-to-get-wrong detail done right.
- **`c_char → u8` casts (lib.rs:499,526):** on x86_64 `c_char == i8`; `c as u8`
  reinterprets the bit pattern, so high-bit bytes survive (`-1i8 → 255u8`) and
  `String::from_utf8_lossy` handles the rest. **Sound and portable** (works
  identically where `c_char == u8`). `cdesc` (`[c_char;5]`, NUL-terminated) is read
  as the first 4 bytes — correct for the `b"RGBG"` compare.
- **bindgen layout checks:** the generated bindings embed `size_of`/`offset_of`
  `const _` assertions for every struct, so a header/ABI mismatch is a **compile
  error**, not silent UB. Good defensive posture.
- **`cblack` field width (verified, fine here):** the real field is
  `[c_uint; 4104]` (bindings.rs:1966), not `[_;4]`. The code reads only
  `cblack[0..4]` into `Metadata.cblack: [u32;4]` (lib.rs:522). That stays in-bounds
  of the FFI array; whether the per-channel pedestal *also* lives in the 2-D pattern
  tail (`cblack[4..]`) is an **algorithm** question (out of scope), not a memory bug.

**Open soundness gaps:** **H2** (`raw_pitch` ignored — the one genuinely
unverified `unsafe` invariant) and **H1** (the value-`6` CFA path, which is a logic
panic rather than UB but originates in an FFI return value the code doesn't
anticipate). `read_metadata` is `unsafe fn` and its `# Safety` clause is correct,
though see N3 on the misplaced doc comment.

---

## 4. Test-coverage gaps

The suite is strong on the happy path and on algorithm round-trips. Gaps, roughly
in priority order:

1. **No test exercises the value-`6` / non-Bayer CFA path (H1).** No unit test
   constructs a `RawImage` with `cfa` containing `6` (or `filters==0`) and calls
   `normalized()`/`apply_white_balance()`. Such a test would currently *panic*,
   demonstrating the bug. Add it (and make it assert a clean `Err`, post-fix).
2. **No `raw_pitch` / stride test (H2).** Hard to unit-test without a real padded
   file, but a `debug_assert` + a fixture documenting expected pitch would help.
3. **FFI error paths under-tested.** Only `RawError::Open` is exercised
   (`missing_file_is_a_typed_error`, lib.rs:894). `Unpack`, `NoMosaic` (null
   `raw_image` *and* non-RGBG `cdesc`), and `InvalidPath` (path with interior NUL)
   have no tests. `InvalidPath` in particular is trivially testable without LibRaw.
4. **HSV/color extremes untested.** No test for: achromatic input
   (`s <= 1e-6` early return), `color_mix` hue exactly at a band center / wraparound
   (the L1 boundary), negative/super-unit value handling, or NaN/Inf input to
   `rgb_to_hsv` (I confirmed `f32::max`/`min` *drop* NaN, so `rgb_to_hsv([NaN,0,0])`
   yields `0`-ish rather than propagating — worth pinning with a test).
5. **`Mat3::inverse` near-singular.** Tested only at exactly-singular (`zero`) and
   well-conditioned. No test around the `1e-12` threshold (det just above/below) to
   pin the boundary behavior.
6. **`downscaled` edge cases.** `max_dim == 0` (the `longest <= max_dim` /
   `longest == 0` guards), 1-pixel images, and extreme aspect ratios are untested;
   the `span` `b.max(a+1)` div-by-zero guard is never directly exercised.
7. **`tone::eval` lower extrapolation & LUT-end slope.** `eval(-1.0)`→0 is tested;
   the `i >= n-1` clamp branch and the exact `lut[n-2]` end-slope arithmetic are
   only indirectly covered.
8. **Round-trip/property tests are deterministic single-point.** Consider a small
   randomized (seeded) sweep for `hsv` round-trip and `Mat3` inverse to widen
   coverage cheaply.

One nit on an existing test: `white_balance_neutralizes_a_gray_patch`
(lib.rs:610-638) builds a `RawImage` with `mosaic: vec![]` but then operates on a
*separate* local `mosaic` vector — fine, but the struct's own `mosaic`/`width`/
`height` are inconsistent (width 2, height 2, empty mosaic), so the fixture
wouldn't survive any method that reads `self.mosaic`. It tests what it claims, but
the fixture is a footgun for future edits.

---

## 5. Positives worth keeping

- **RAII `Handle`** is the textbook-correct way to wrap a C resource — exactly-once
  cleanup on all paths, no manual `close`, no leak/double-free.
- **Typed, `Display`+`Error` `RawError`** carrying LibRaw's own codes/messages; the
  "never panic across FFI" discipline is real (modulo H1, which is *post*-decode).
- **bindgen `const _` size/offset assertions** turn ABI drift into compile errors.
- **CFA-phase correctness** between `libraw_COLOR` sampling and raw-buffer indexing
  (see §3) — a subtle margin/phase trap, avoided.
- **Numerical guards everywhere:** `(white-black).max(1.0)`, `1e-12` singular
  thresholds with `Option` returns, `rem_euclid`, `row_normalized` neutral-pinning,
  the widened-`u32` `clip_mask` comparison (deliberately avoiding a u16 truncation).
- **Idiomatic Rust:** pervasive correct `std::array::from_fn`, iterator pipelines,
  `is_empty` alongside `len`, encapsulated `ImageBuf` fields with accessor methods.
- **Excellent doc comments** explaining *why* (headroom preservation, double-WB
  avoidance, per-channel black) — rare and valuable in foundational code.
- **The demosaic test harness** (forward-mosaic → demosaic → MAE, all four CFA
  phases, MHC-beats-bilinear) is a model for catching silent regressions.

---

### Suggested remediation order
1. **H1 + M1** together via a single `filters` validation in `unpack` (one change
   closes both, plus a regression test that today panics).
2. **H2** — honor/verify `raw_pitch` in the `from_raw_parts` path.
3. **M2** — `checked_mul` in `ImageBuf` construction (or document 64-bit-only).
4. **L1** — clamp the `hsv_to_rgb` sextant; **L2/L3/N\*** as cleanup.
