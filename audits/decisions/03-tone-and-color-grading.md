# Decision Register — Audit 03 (Tone & Color-Grading Math)

**Source audit:** [`../image-processing/03-tone-and-color-grading.md`](../image-processing/03-tone-and-color-grading.md)
**Decided:** 2026-06-27
**Status:** Decisions recorded — **pending implementation** (no code changed yet).
**How decided:** Each §2 point reviewed interactively, in document order (2.1→2.9), accept/modify/keep for each.
**Revision 2026-06-27 (consistency pass):** 2.2 upgraded *clamp → always-monotone S-curve*; 2.8 upgraded *keep → monotone-cubic point curves*; perceptual-lightness references reconciled to **L\*** (T1) and the luma-blend saturation dropped in favour of LCh (T2). See [`README.md`](README.md) → *Consistency reconciliation*.

---

## Decision summary

| Point | Topic (file) | Finding · Severity | Decision | Outcome |
|---|---|---|---|---|
| **2.1** | Perceptual domain = γ2.2 (`tone.rs:16-26`) | F6 · Note | **Use true perceptual L\*** | Change (redesign) |
| **2.2** | `contrast` monotonicity (`tone.rs:94`) | F1 · High | **Always-monotone S-curve** | Change |
| **2.3** | Headroom extrapolation (`tone.rs:53-59`) | F2 · High | **Pass L>1 with unit slope** | Change |
| **2.4** | 256-entry LUT (`tone.rs`) | — · Correct | **Keep as-is (verified correct)** | No change |
| **2.5** | `color_mix` "HSL" mixer (`color.rs:234`) | F4 · Medium | **Reimplement in CIE LCh** | Change (redesign) |
| **2.6** | HSV round-trip / sector cast (`color.rs:192,216`) | F7 · Note | **Add explicit sector clamp** | Change (nit) |
| **2.7** | Luma-blend saturation (`pipeline:386`) | F3 · Medium | **Chroma-preserving (LCh/ICtCp)** | Change (redesign) |
| **2.8** | Curves: composition + interpolation (`pipeline:592,604`) | — · Correct (comp.) | **Monotone-cubic point curves** | Change |
| **2.9** | Channel mixer raw 3×3 (`pipeline:392`) | — · Correct | **Add preserve-luminosity toggle** | Change (UX) |

**Tally:** 8 changes, 1 keep-as-is.

> **Overarching theme — move the color-grading core into perceptually-uniform spaces.** Decisions 2.1 (L\* tone domain), 2.5 (LCh HSL mixer), and 2.7 (LCh/ICtCp saturation) together replace the ad-hoc per-channel-power / HSV / luma-lerp approximations with proper perceptual spaces. This is a coherent redesign, not three tweaks — and it dovetails with the Audit-01 decisions (#2 D50 ProPhoto working space + Bradford CA): **CIE Lab/LCh is natively D50-referenced**, so the working→XYZ→Lab path is well-defined once Audit-01's color core lands.

---

## Per-point decisions

### 2.1 — Perceptual domain · **Use true perceptual L\*** *(was F6 Note; upgrade)*
- **Decision:** Replace the pure γ2.2 encode/decode used for tone shaping with a true perceptual space — **CIE L\***.
- **Why:** γ2.2 is only "comparable to" L\*'s cube root and isn't perceptually uniform; moving to L\* makes the contrast/highlights/shadows/blacks pivots and step-uniformity perceptually correct.
- **Action:** Replace `tone_encode`/`tone_decode` (`tone.rs:16-26`) with an L\* encode/decode (via XYZ Y), and re-evaluate the four shape polynomials in the L\* domain (mid-gray now lands at L\*≈0.5 rather than γ2.2's 0.466 — the shapes' pivots shift). Touches the LUT build and `apply_linear`.
- **Depends on:** a working→XYZ path (already present); reference white D50 once Audit-01 #2 lands (Lab is D50-native). **Mirror in the GPU shader.**

### 2.2 — `contrast` monotonicity · **Always-monotone S-curve** *(F1, High; revised — higher-quality option)*
- **Decision:** Replace the smoothstep contrast with a **strictly-monotone S-curve family** (e.g. a logistic / power-pivot contrast) whose slope stays positive for *all* amounts — so the curve never inverts even past `amount = 1`, and the tool keeps its full range instead of being clamped to `[-1,1]`.
- **Why:** The smoothstep `contrast` inverts tones for `amount>1`; clamping fixes the inversion but caps the control. A monotone-by-construction curve fixes the inversion *and* preserves capability — the higher-quality choice (revised from the earlier "clamp" decision).
- **Action:** Implement the new contrast curve in `tone::contrast` (`tone.rs:94`); it operates in the L\* domain (per 2.1). Still **sanitize `SelectiveTone` on load** for NaN/inf (the code-review "sanitize sidecar" theme) even though range no longer needs clamping. Re-verify monotonicity numerically across the amount range (the audit's Python harness).

### 2.3 — Headroom extrapolation · **Pass L>1 with unit slope** *(F2, High)*
- **Decision:** Apply the contrast/highlights bend only on `[0,1]` and pass highlight headroom (`L>1`) through with **unit slope**, preserving it as the docs intend.
- **Why:** The current LUT end-slope extrapolation uses `f'(1)=1−a→0`, soft-clipping headroom (linear 8.0 → ~1.04) — the opposite of the stated "shape, don't crush."
- **Action:** Change `eval`'s `>1` branch (`tone.rs:53-59`) and/or how the shapes are applied above 1; fix the misleading comments; **mirror in `map_pixels.wgsl`**. Note: interacts with 2.1 (the `>1` regime is now in L\* terms).

### 2.4 — 256-entry LUT · **Keep as-is (verified correct)**
- No change. Sub-LSB error at 16-bit, measured. (Stays valid after the 2.1 domain change — the LUT is domain-agnostic; just re-verify the error bound in L\*.)

### 2.5 — "HSL" mixer · **Reimplement in CIE LCh** *(F4, Medium)*
- **Decision:** Reimplement the 8-band mixer in **CIE LCh** (hue-uniform shifts, true lightness), matching darktable color zones — instead of HSV.
- **Why:** The tool is labeled HSL but implemented in HSV (value=max channel); HSV hue isn't perceptually uniform (worst in blue), and "lum" scales HSV value, not lightness.
- **Action:** Replace the HSV path in `color_mix` (`color.rs:234-253`) with a working→Lab→LCh conversion, band the **hue angle** in LCh, and apply Δhue/Δchroma/Δlightness there. Keep the band interpolation / wraparound / neutral-skip logic (those were correct). Reword the F8 "band center" doc. Shares the Lab/LCh machinery with 2.1/2.7.

### 2.6 — HSV sector cast · **Add explicit sector clamp** *(F7, Note)*
- **Decision:** Clamp the sector index to `0..=5` (`(h6 as u32).min(5)`) so correctness is explicit, not coincidental.
- **Action:** One-line change in `hsv_to_rgb` (`color.rs:216`). (If 2.5's LCh reimplementation removes the HSV path entirely, fold/retire this; keep it if `rgb_to_hsv`/`hsv_to_rgb` survive for other uses.)

### 2.7 — Saturation · **Chroma-preserving (LCh/ICtCp)** *(F3, Medium)*
- **Decision:** Replace luma-blend saturation with a **chroma-preserving** operation — scale chroma at constant lightness in Lab/LCh (or scale Ct,Cp at constant I in ICtCp) — so desaturation goes to mid-gray and saturation changes don't shift blue lightness.
- **Why:** The blue luma weight (~0.0001) makes desaturating blue collapse it to near-black.
- **Action:** Reimplement the `Saturation` op in `pipeline`/CPU backend/GPU (`lib.rs:386-388`, backend `:810-817`, GPU `:75-76`) via the shared Lab/LCh path (chroma scaling at constant L\*). **Keep CPU/GPU in sync.** Consistent with 2.1/2.5 and Audit-01 #5. **Reconciliation (T2):** this LCh saturation *supersedes* the luma-blend saturation entirely — the separate "Rec.709 luma for saturation" idea from Audit-01 #5 is dropped (saturation uses LCh chroma, no luma blend).

### 2.8 — Curves: composition + interpolation · **Monotone-cubic point curves** *(composition Correct; interpolation upgraded)*
- **Decision:** Keep the composition order (master-then-channel, `channel ∘ master` — verified correct), but upgrade `point_curve` from C0 piecewise-linear interpolation to **monotonicity-preserving cubic splines** (Fritsch–Carlson / PCHIP) for smoother (C1), shape-preserving curves without overshoot.
- **Why:** Piecewise-linear control-point interpolation has slope kinks at every control point; monotone-cubic gives a smooth curve while *guaranteeing* no spurious oscillation/inversion between points (unlike plain Catmull–Rom).
- **Action:** Replace the interpolation in `point_curve` (`pipeline:604-623`); used by both the master and per-channel curves. Keep the clamp-flat-past-ends and empty-is-identity behavior. Add tests that the spline stays monotone for monotone control points and matches the endpoints.

### 2.9 — Channel mixer · **Add preserve-luminosity toggle** *(was Correct; UX add)*
- **Decision:** Keep the raw un-normalized 3×3 (correct creative primitive) and add an optional **"preserve luminosity" / normalize-rows toggle** (defaulting off), as Photoshop offers.
- **Action:** Add the toggle to `ChannelMixer` (`latent-edit`) + UI; when on, row-normalize the matrix before applying. The op math itself is unchanged.

---

## Resulting implementation plan (derived from the decisions)

The three perceptual-space items share infrastructure, so build that once:

1. **Add a Lab / LCh module** (working→XYZ→Lab→LCh and back), D50-referenced (aligns with Audit-01 #2). This underpins 2.1, 2.5, 2.7.
2. **2.1** — switch the tone domain to L\*; re-derive/verify the four shapes' behavior and re-check the LUT error (2.4) in L\*.
3. **2.3** — make headroom (L>1) pass through with unit slope; fix comments; GPU mirror.
4. **2.2** — replace `contrast` with an always-monotone S-curve (no range cap); sanitize `SelectiveTone` for NaN/inf on load.
5. **2.5** — reimplement the 8-band mixer in LCh (keep band/wraparound/neutral logic); reword band-center doc.
6. **2.7** — reimplement saturation as chroma-preserving in LCh/ICtCp (CPU + GPU); drops the luma-blend path (T2).
7. **2.6** — explicit HSV sector clamp (or retire HSV if 2.5 removes it — T3).
8. **2.8** — upgrade `point_curve` to monotone-cubic (PCHIP) interpolation.
9. **2.9** — add the preserve-luminosity toggle to the channel mixer.
10. **No-op:** 2.4 confirmed correct — leave unchanged (re-verify its LUT error in L\*).

**Cross-cutting reminders:**
- 2.1/2.3/2.7 touch the **GPU WGSL** (`map_pixels.wgsl`) — keep CPU/GPU in lockstep (the equivalence test guards this).
- Update the numeric tone tests (monotonicity, headroom, LUT error) to the L\* domain; add tests for the LCh mixer/saturation and the contrast clamp.
- Aligns with Audit-01 (#2 D50 ProPhoto + Bradford CA, #5 perceptual lightness = **L\*** from this same Lab/LCh module) — sequence the Lab/LCh work after Audit-01's color-core change so the reference white is settled.
- This register reflects intent only; nothing here has been implemented.
