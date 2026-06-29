# Audit 03 — Tone & Color-Grading Math

**Component:** `latent-image` (tone/color), `latent-pipeline` (lowering)
**Files audited:** `latent-image/src/tone.rs`, `latent-image/src/color.rs`, `latent-pipeline/src/lib.rs` (`tone_curves`, `channel_curves`, `point_curve`, `apply_global`), with cross-reference to `latent-edit/src/lib.rs` (parameter types/ranges), `latent-app/src/gui.rs` (slider ranges), and `latent-gpu/src/map_pixels.wgsl` (GPU tone/saturation path).
**Date:** 2026-06-27
**Method:** Each file read line-by-line. Every curve polynomial and the LUT evaluator were **re-implemented in standalone Python** and evaluated on fine grids to *measure* (not assert) monotonicity, endpoint slopes, headroom extrapolation, and LUT-interpolation error; the analytic derivatives were derived by hand and cross-checked against the numeric slopes. Color-science claims were verified against Poynton's *Gamma FAQ* and *Color FAQ* (read element-by-element from the downloaded PDFs), the darktable and Lightroom documentation, and the ICtCp/IPT literature. PDFs cited are downloaded under `docs/` with a `tone-` prefix.

---

## 1. Summary of the Tone / Color Model

`latent` shapes tone in a **perceptual domain** built from a plain power law. A linear-light value $L$ is encoded with $E(L)=L^{1/\gamma}$, $\gamma=2.2$ (`tone_encode`, no upper clamp so headroom $L>1$ survives), a 1-D curve is applied, then decoded with $D(E)=E^{\gamma}$ (`apply_linear`). Curves are stored as a **256-entry LUT** sampled uniformly over $[0,1]$; `eval` does linear interpolation inside $[0,1]$, clamps the low end to $0$, and **extrapolates above $1$ with the LUT's end slope** so highlight headroom is shaped rather than flattened.

Four parametric tonal shapes, each $\texttt{clamp}_{[0,1]}$ of a base polynomial with amount $a$:

$$
\text{contrast}(t)=t+a\big(\mathrm{ss}(t)-t\big),\quad
\text{highlights}(t)=t+a\,t^2(1-t),
$$
$$
\text{shadows}(t)=t+a\,t(1-t)^2,\quad
\text{blacks}(t)=t+\tfrac{a}{4}(1-t)^4,
$$

where $\mathrm{ss}(t)=t^2(3-2t)$ is the cubic smoothstep. The pipeline composes the active shapes in the fixed order contrast → highlights → shadows → blacks, each applied per channel through the same encode/curve/decode path (`tone_curves`, `apply_global`).

Color tools operate in the linear working space (linear ProPhoto primaries at D65):

- **Saturation:** luma-blend $C'=Y+a\,(C-Y)$ per channel, clamped $\ge 0$, with $Y=\langle 0.2788, 0.7211, 0.000113\rangle\!\cdot\!C$ (the Y row of the working RGB→XYZ matrix).
- **HSL mixer (`color_mix`):** convert to HSV with $V=\max$ channel, pick the two nearest of 8 evenly-spaced hue bands by linear interpolation, skip achromatic pixels, and apply $h'=h+\Delta h$, $s'=s(1+\Delta s)$, $v'=v(1+\Delta v)$.
- **Channel curves:** per-channel $=$ channel-curve $\circ$ master-curve, both piecewise-linear interpolations of control points in the perceptual domain.
- **Channel mixer:** a raw $3\times3$ matrix (no row normalization at the op level).

**Bottom line.** The tone/color math is broadly sound and the design choices (perceptual-domain shaping, value=max HSV for headroom, derived luma weights) are defensible and well-cited in the code comments. The headroom *plumbing* is real but the shaped curves do **not** all preserve headroom equally: positive **contrast** and **highlights** flatten to a near-zero top slope, so highlights above $1$ are compressed toward white (a soft clip), the opposite of the design's stated intent for those values. The four shapes are monotone for the documented amount range $[-1,1]$, but **contrast is non-monotone for $a>1$** — unreachable from the UI slider (clamped to $\pm1$) yet reachable through the public API / sidecar, where the type is an unclamped `f32`. Secondary issues: luma-blend saturation in wide D65-ProPhoto primaries drives saturated blues toward black; the HSV hue interpolation and `hsv_to_rgb` boundary work but are not hue-uniform.

---

## 2. Point-by-Point Verification

### 2.1 Gamma 2.2 as the "perceptual domain" for tone shaping

**Code** (`tone.rs:16-26`, `69-74`): $E(L)=\max(0,L)^{1/2.2}$, $D=E^{2.2}$, curve applied between them.

**Authoritative position.** Poynton defines lightness as the CIE $L^*$ function
$$ L^* = 116\left(\frac{Y}{Y_n}\right)^{1/3} - 16, \qquad \frac{Y}{Y_n} > 0.008856, $$
and states directly: *"Video systems approximate the lightness response of vision using $R'G'B'$ signals that are each subject to a 0.45 power function. This is comparable to the $1/3$ power function defined by $L^*$."* (Color FAQ Q4.) On whether to process in a nonlinear domain at all: *"if your computation involves human perception, a nonlinear representation may be required … you ought to use nonlinear coding that exhibits perceptual uniformity, because you wish to minimize the perceptibility of the errors that will be introduced during quantization."* (Gamma FAQ Q19.) And *"gamma correction in video effectively codes into a perceptually uniform domain."* (Gamma FAQ Q14.)

So a $\sim 0.45$ encode (display $\gamma\approx 2.2$) is the *standard* perceptual proxy, explicitly "comparable to" $L^*$'s cube root. Shaping contrast/tone in this domain rather than linear light is therefore correct practice, and matches what display-referred photo tools do (Lightroom's parametric tone curve and point curve act on gamma-encoded values; darktable's legacy `tone curve` and `color zones` operate on non-linear Lab/encoded data). The one nuance Poynton flags (Gamma FAQ Q6) is that a **pure power** differs from the sRGB/Rec.709 OETF by the latter's *linear toe* near black — sRGB is a $\sim 1/2.4$ power with a linear segment below $\approx 0.0031$, asymptotically matching an overall $\approx 2.2$. The difference vs a pure $2.2$ is confined to deep shadows and is small for tone-shaping; it is not a true perceptual space (neither is sRGB — $L^*$ is the reference), but it is the conventional and adequate choice.

**Artifacts of the choice.** Shaping in $\gamma 2.2$ vs linear: contrast/S-curves in linear light crush shadows and blow highlights non-perceptually (equal linear steps are unequal perceptual steps) — the code comment's justification is correct. Shaping in $\gamma 2.2$ vs true $L^*$: the pivot of an S-curve and the placement of "midtones" differ slightly (mid-gray $0.18$ lin $\to 0.466$ at $\gamma2.2$ vs $0.5$ at $L^*$), and saturated hues can shift because per-channel power curves are not luminance/hue-preserving — but these are second-order and shared by virtually all per-channel tone tools.

**VERDICT: Correct-with-caveats.** Gamma 2.2 is a legitimate, standard perceptual proxy for tone shaping (Poynton: comparable to $L^*$'s cube root); it is not perceptually *uniform* the way $L^*$/ICtCp are, and it omits sRGB's near-black linear toe, but those gaps are minor for the contrast/HLS shapes used here.
**Citations:** Poynton, *Color FAQ* Q4 (definition of $L^*$; "comparable to the $1/3$ power"), Q6; *Gamma FAQ* Q6 (Rec.709 OETF with toe), Q14, Q19.

---

### 2.2 Monotonicity of the four shapes over the documented amount range

A tone curve that decreases anywhere inverts tones locally — a real bug. The base polynomials' derivatives (before the clamp) are:

$$
\text{contrast}'(t)=1+a\big(6t-6t^2-1\big),\quad
\text{highlights}'(t)=1+a\,(2t-3t^2),
$$
$$
\text{shadows}'(t)=1+a\,(1-4t+3t^2),\quad
\text{blacks}'(t)=1-a(1-t)^3.
$$

Minimizing each bracket over $[0,1]$ gives the monotonicity threshold:

| shape | $\min/\max$ of the bracket on $[0,1]$ | monotone iff |
|---|---|---|
| contrast | bracket $\in[-1,\,0.5]$ (min $-1$ at $t=0,1$) | $-2\le a\le 1$ |
| highlights | bracket $\in[-1,\,1/3]$ (min $-1$ at $t=1$) | $-3\le a\le 1$ |
| shadows | bracket $\in[-1/3,\,1]$ (min $-1/3$ at $t=2/3$) | $-1\le a\le 3$ |
| blacks | $(1-t)^3\in[0,1]$ | $a\le 1$ (and $a\ge$ any neg) |

**Numeric confirmation** (Python re-implementation, 2001-point grid, reporting the minimum forward difference of the *clamped* shape; a negative value means a local inversion):

```
contrast    a=-1.00: min_slope=+2.50e-04    a=+1.00: min_slope=+7.50e-07
highlights  a=-1.00: min_slope=+3.33e-04    a=+1.00: min_slope=+5.00e-07
shadows     a=-1.00: min_slope=+5.00e-07    a=+1.00: min_slope=+3.33e-04
blacks      a=-1.00: min_slope=0 (clamp)    a=+1.00: min_slope=+3.75e-07
```

All four are **monotone (non-decreasing) for every $a\in[-1,1]$** — the documented range (`SelectiveTone` doc: *"each value is roughly $[-1,1]$"*). At the endpoints $a=\pm1$ the slope touches $\approx 0$ but never goes negative, so there is no inversion within range.

**Past the documented range — contrast inverts for $a>1$:**

```
contrast a=+1.00: raw_min_slope=+0.000000  mono=True
contrast a=+1.50: raw_min_slope=-0.000125  mono=False   <-- inversion
contrast a=+2.00: raw_min_slope=-0.000250  mono=False
contrast a=+2.50: raw_min_slope=-0.000375  mono=False
```

This matches the analytic threshold exactly: $\text{contrast}'(t)=1+a(6t-6t^2-1)$, whose minimum over $[0,1]$ is $1-a$ (at $t=0,1$), so the curve is monotone iff $a\le 1$. For $a>1$ the curve **decreases near both endpoints** — i.e. raising contrast past $1$ makes near-black get *brighter* and near-white get *darker*, a tone inversion. The GUI slider is clamped to `-1.0..=1.0` (`latent-app/src/gui.rs:569`), so this is not reachable interactively, **but** `SelectiveTone.contrast` is an unclamped `f32` deserialized from sidecars / set via the public API, and `tone::contrast`'s doc comment promises it "returns a monotonic curve." The clamp lives in the UI, not in the math.

**Endpoints / C1.** $\mathrm{ss}(0)=0,\mathrm{ss}(1)=1$ and the highlights/shadows/blacks terms vanish at the relevant endpoint, so $f(0)=0$ (except blacks, which intentionally lifts/lowers the floor) and $f(1)=1$ — endpoints behave as documented. Each base polynomial is a smooth ($C^\infty$) polynomial, so individually $C^1$; but the **clamp** introduces a slope discontinuity wherever the curve hits $0$ or $1$ (e.g. `blacks(-1)` shows `min_slope=0` exactly because the clamp flattens the toe), and **composing** several shapes in series, plus the LUT's piecewise-linear interpolation, makes the realized transfer function only $C^0$ overall. For tone shaping this is acceptable (the kink is gentle and far from typical operating points), but it is not the $C^1$ that "smoothstep" might suggest end-to-end.

**VERDICT: Correct (within documented $[-1,1]$) / Questionable at the boundary (contrast, $a>1$).** No inversion can occur for any in-range amount; the only inversion is contrast for $a>1$, which the UI prevents but the type system and `tone::contrast`'s contract do not.
**Citations:** standard monotone-tone-curve requirement (a tone curve must be non-decreasing); analytic derivatives above, confirmed numerically.

---

### 2.3 Headroom: end-slope extrapolation of the LUT above 1.0

`eval` extrapolates linearly past $t=1$ using the top LUT slope $s=(\text{lut}[255]-\text{lut}[254])\cdot 255$ (`tone.rs:53-59`). `apply_linear` reaches this whenever $L>1$, since $E(L)=L^{1/2.2}>1$. The realized highlight behavior depends entirely on each shape's **slope at $t=1$**:

$$
\text{contrast}'(1)=1-a,\quad \text{highlights}'(1)=1-a,\quad \text{shadows}'(1)=1,\quad \text{blacks}'(1)=1.
$$

**Measured top LUT slope and `apply_linear` on a highlight ramp** (linear inputs $1.0,1.5,2.5,8.0$):

```
shape       a      top_slope    apply_linear(1.0, 1.5, 2.5, 8.0)        mono  neg
contrast   +1.00    +0.0117     1.0000  1.0052  1.0134  1.0411          yes   no
highlights +1.00    +0.0078     1.0000  1.0035  1.0089  1.0273          yes   no
contrast   -1.00    +1.9883     1.0000  2.1043  4.7335  22.6294         yes   no
highlights -1.00    +1.9922     1.0000  2.1069  4.7438  22.7036         yes   no
shadows    +1.00    +0.9961     1.0000  1.4978  2.4927  7.9580          yes   no
shadows    -1.00    +1.0039     1.0000  1.5022  2.5073  8.0421          yes   no
blacks     +/-1.00  +1.0000     1.0000  1.5000  2.5000  8.0000          yes   no
```

Findings:

1. **No non-monotonicity, no negatives, no runaway** for any shape at $a\in[-1,1]$ on the headroom range — the extrapolation is well-behaved in sign and order. Good.
2. **Positive contrast / highlights crush the headroom they are supposed to shape.** Because $f'(1)=1-a\to 0$ as $a\to 1$, the top LUT slope is $\approx 0.01$, so a linear value of $8.0$ maps to only $\approx 1.04$ — a *soft clip* of all highlight headroom to just above white. This is the **opposite** of the module's stated design: `tone_encode`'s comment says headroom "stays above 1.0 … so the curve can shape them instead of crushing to white," and `eval`'s comment says headroom is "shaped, not flattened." For positive contrast/highlights it is in fact flattened. (For *negative* contrast/highlights, $f'(1)=1-a=2$, so headroom is *expanded* $\sim2\times$ — also a strong, possibly surprising effect: $8.0\to 22.6$.)
3. **Slope-1 shapes pass headroom through faithfully.** shadows and blacks have $f'(1)=1$, so $L=8\to\approx 8$ — headroom preserved as intended.

The root cause is that the extrapolation uses the **local slope at the very top of $[0,1]$**, which for highlight-targeting shapes is exactly where the curve has been bent toward flat. A more headroom-faithful design would extrapolate with the curve's *average* high-end slope, or apply the highlight/contrast bend only up to $1$ and pass $L>1$ through with unit slope, or shape headroom with an explicit highlight-rolloff rather than a linear extension of a near-zero slope.

**VERDICT: Correct (numerically safe — monotone, non-negative, bounded) / Questionable (semantically).** The extrapolation never produces an inverted, negative, or runaway value, so it is a *sound* mechanism; but for positive contrast and positive highlights it compresses highlight headroom to a near-flat plateau, contradicting the documented "shape, don't crush" intent. The same logic is replicated in the GPU shader (`map_pixels.wgsl:38-40`), so both backends behave identically.
**Citations:** module doc comments (`tone.rs:18-22,53-59`); numeric evidence above; analytic endpoint slopes.

---

### 2.4 256-sample LUT with linear interpolation — banding / quantization

**Measured maximum interpolation error** of the 256-entry LUT vs the exact shape (worst amount $a=1$, sampled at 20 001 points), reported in 16-bit and 8-bit levels:

```
shape       max |LUT - exact|     16-bit levels   8-bit levels
contrast    1.149e-05             0.753           0.0029
highlights  7.667e-06             0.502           0.0020
shadows     7.667e-06             0.502           0.0020
blacks      5.744e-06             0.376           0.0015
identity    2.2e-16               ~0              ~0
```

The worst-case error (contrast, $\approx 0.75$ of a 16-bit level) is **below 1 LSB at 16-bit** and ~$1/340$ of an 8-bit level. With $N=256$ samples a piecewise-linear LUT has error $\le \tfrac{1}{8}\max|f''|\,h^2$ with $h=1/255$; the smooth shapes have small $|f''|$, so the bound is tiny. Poynton notes 8-bit *linear* coding bands near black, but here the LUT is in the **already-perceptual** domain and feeds 16-bit output, so the perceptual bit-budget is well spent (Gamma FAQ Q13: nonlinear coding needs ~9 bits for smooth black-to-white; 16-bit output has ample margin). One caveat: the LUT samples are stored as `f32` and interpolated in `f32`, so there is no added quantization beyond the $0.75$-LSB geometric error — banding risk is negligible.

**VERDICT: Correct.** 256 entries with linear interpolation in the perceptual domain is sufficient for 16-bit output; worst-case error is sub-LSB at 16-bit.
**Citations:** Poynton, *Gamma FAQ* Q13 (bit-depth for smooth shading); piecewise-linear interpolation error bound; numeric evidence above.

---

### 2.5 `color_mix` — 8-band HSV hue grading

**Code** (`color.rs:234-253`). For a chromatic pixel: $\text{pos}=8h$, $i=\lfloor\text{pos}\rfloor\bmod 8$, $j=(i+1)\bmod 8$, $f=\text{frac}$, $\text{adj}=\text{bands}[i](1-f)+\text{bands}[j]\,f$; then $h'=h+\Delta h$, $s'=(s(1+\Delta s))_{[0,1]}$, $v'=(v(1+\Delta v))_{\ge0}$.

**Soundness.** Two-neighbor linear interpolation over evenly-spaced band centers is exactly how Lightroom's HSL/Color mixer behaves ("the target tool … will adjust sliders in both colors at the same time if you click on part of your image that contains both yellows and greens"; 8 bands red/orange/yellow/green/aqua/blue/purple/magenta). **Wraparound** band $7\leftrightarrow0$ is handled by the `%8` on $j$ (`color.rs:243`): at $h=0.9375$ the weights split $0.5/0.5$ between band 7 and band 0 — correct and continuous across the seam (confirmed numerically). Achromatic pixels ($s\le10^{-6}$) are skipped so neutrals don't fall into the red band at $h=0$ — a correct and deliberate choice (tested). The all-zero-adjacent-bands early-out keeps untouched hues exact.

**Two real caveats:**

1. **Band centers sit at $h=i/8$ (the *lower edge*), not $(i+0.5)/8$.** With $\text{pos}=8h$ and $i=\lfloor\text{pos}\rfloor$, band $i$ has full weight ($f=0$) exactly at $h=i/8$. So band 0 ("red") is centered on pure red ($h=0$), band 4 on cyan/aqua ($h=0.5$), etc. The doc says *"a color at a band center is driven only by that band,"* which is true at $h=i/8$ — consistent, but worth stating explicitly since "band center" could be misread as the middle of a sector.
2. **HSV (value = max channel) is not a hue-preserving / perceptual space.** Poynton is blunt: of HSI/HSV/HLS, *"no reference is made to the nonlinearity in the primary components … these systems make poor measures of perceptual quantities"* (Gamma FAQ Q20), and the saturation/value of HSV are not the CIE attributes. Scaling HSV "value" (the max channel) as a luminance proxy will shift perceived lightness differently for different hues, and HSV hue is not perceptually uniform (equal $\Delta h$ are unequal perceptual hue steps, worst in blues). The professional comparison point is **darktable color zones**, which works in **CIE LCh** ("selectively adjusts the lightness, chroma and hue … working in CIE LCh"), a hue-preserving space; Lightroom's calibration/HSL is a proprietary perceptual model. Using HSV is the *common, cheap* choice and is internally consistent, but it is a "HSL mixer" in name more than in colorimetric behavior. Note also the tool is labeled **HSL** in the UI/type (`Hsl`) yet implemented in **HSV** — the "lum" control scales HSV *value* (max channel), not HSL *lightness* $(\max+\min)/2$; these diverge for saturated colors.

**VERDICT: Correct-with-caveats.** The band interpolation, wraparound, and neutral-skip are sound and match Lightroom practice; the space is HSV-not-HSL and not hue-uniform (vs darktable's LCh), so hue/lightness behavior is approximate.
**Citations:** Poynton, *Gamma FAQ* Q20 (HSI/HSV lack perceptual basis); darktable *color zones* (CIE LCh); Lightroom Color Mixer docs (8 bands, two-band interpolation).

---

### 2.6 `rgb_to_hsv` / `hsv_to_rgb` correctness and round-trip (with headroom)

**Code** (`color.rs:192-225`). Standard hexagonal HSV: $C=\max-\min$, $S=C/\max$, the six-sector hue formula, and the inverse with $X=C(1-|h_6\bmod2-1|)$, $m=V-C$.

**Numeric round-trip** (re-implemented), including $V>1$:

```
[0.8,0.3,0.1] -> hsv(0.0476,0.8750,0.8000) -> [0.8000,0.3000,0.1000]  err 5.6e-17
[2.0,0.3,0.0] -> hsv(0.0250,1.0000,2.0000) -> [2.0000,0.3000,0.0000]  err 5.6e-17
[1.6,0.2,1.6] -> hsv(0.8333,0.8750,1.6000) -> [1.6000,0.2000,1.6000]  err 5.6e-17
[3.0,3.0,1.0] -> hsv(0.1667,0.6667,3.0000) -> [3.0000,3.0000,1.0000]  err 0
[0.0,0.0,0.0] -> hsv(0,0,0)               -> [0,0,0]                  err 0
```

Round-trip is exact to float epsilon, and **$V>1$ headroom reconstructs faithfully** because $V=\max$ carries the unbounded range and the inverse scales by $V$. This is the correct design choice for a headroom pipeline (HSL lightness $(\max+\min)/2$ would also carry headroom but distorts saturation differently; the code comment's justification is right). The forward formula uses `rem_euclid` so negative-ish hues wrap correctly; the $c\le10^{-9}$ guard avoids a divide-by-zero on neutrals.

**One latent boundary subtlety (benign).** `hsv_to_rgb` casts `h6 as u32` to pick the sector (`color.rs:216`). At exactly $h=1.0$, $h_6=6.0$, `6 as u32 = 6`, which falls to the `_` arm $(c,0,x)$; there $h_6\bmod2=0\Rightarrow X=0$, giving $(c,0,0)$ — identical to sector-0's $(c,x,0)=(c,0,0)$, so the result is correct **by coincidence** (verified: $h=1.0\to[1,0,0]$). The `rem_euclid(1.0)` on the way in normally keeps $h\in[0,1)$, so $h_6\in[0,6)$ and the `_` arm is only legitimately reached for $h_6\in[5,6)$; the $h_6=6.0$ case is reachable only if a caller passes an un-normalized hue and the float lands exactly on $1.0$. No bug, but the cast relies on this coincidence rather than an explicit clamp to sector $0..5$.

**VERDICT: Correct.** Standard hexagonal HSV, exact round-trip, headroom preserved via $V=\max$. The $h=1.0$ sector cast is coincidentally correct and never wrong in practice.
**Citations:** standard HSV/HSV-inverse formulas (hexagonal model); code round-trip test corroborated numerically.

---

### 2.7 Saturation op — luma-blend with working-space weights

**Code** (`pipeline lib.rs:386-388`, backend `:810-817`; GPU `:75-76`): $C'_k=\max\!\big(0,\;Y+a(C_k-Y)\big)$, $Y=\langle0.2788,0.7211,0.000113\rangle\!\cdot\!C$.

**Is luma-blend saturation standard?** Yes — interpolating between luminance and color ($a=0$ gray, $a=1$ identity, $a>1$ more saturated) is the textbook "saturation as a lerp toward gray" and is what many engines do (it is the same construction as a saturation matrix). Using **true relative-luminance weights** for the *working* primaries (the Y row of working→XYZ, cross-checked in tests) is more correct than the common shortcut of reusing Rec.709 weights in a non-709 space. So far, correct.

**The blue problem.** Poynton: *"all saturated blue colors are quite dark … the blue coefficient will be the smallest of the three"* (Color FAQ Q9), and in these *wide ProPhoto* primaries the blue luminance weight is $\approx 0.000113$ — essentially zero (the code comment acknowledges this). Consequences, confirmed numerically:

```
pure blue [0,0,1], amount=0 (desaturate) -> luma 0.00011 -> [0.0001,0.0001,0.0001]  (~BLACK)
pure blue [0,0,1], amount=2 (boost)      -> [0.0000,0.0000,1.99989]
[0.1,0.1,1.0],     amount=1.5            -> [0.0999,0.0999,1.4500]
```

So **desaturating a pure blue sends it to near-black**, not to a mid-gray, because the gray it blends toward is $Y\approx0$. Under sRGB/Rec.709 primaries the same blue would desaturate to $\approx0.0722$ (a dark gray), and in a chroma-preserving space (Lab/LCh: drop chroma at constant $L$, or ICtCp: scale $C_t,C_p$ at constant $I$) it would desaturate to a *mid* gray at the blue's own lightness. The code comment frames this as "by design, not a bug … colorimetrically correct," and it *is* colorimetrically correct as a luminance — but as a **saturation control** it is perceptually wrong: lowering saturation should not also crater lightness. Note also that for $a>1$ the op can push channels below $0$ (hence the $\max(0,\cdot)$ clamp), which itself shifts hue/luminance — boosting saturation of an already-saturated color clips and desaturates the *other* channels asymmetrically.

This is the same class of issue darktable/Capture One avoid by doing saturation/vibrance in a chroma-preserving space rather than a luma lerp; it is the most defensible "Medium" finding here because the visual result (blues going muddy/black when desaturated, and the global saturation slider darkening blue-heavy images) is real and user-visible.

**VERDICT: Correct-with-caveats.** Luma-blend saturation with derived working-space weights is a standard, internally-consistent construction; but the near-zero blue weight of wide D65-ProPhoto primaries makes desaturation collapse blues toward black and saturation changes shift blue lightness — a chroma-preserving space (LCh/ICtCp) would avoid the coupling.
**Citations:** Poynton, *Color FAQ* Q6 (saturation = colorfulness in proportion to brightness), Q9-Q10 (blue carries least luminance; blue contouring); `LUMA_WEIGHTS` comment (`color.rs:177-181`).

---

### 2.8 `channel_curves` — composition order

**Code** (`pipeline lib.rs:592-599`): `ToneCurve::from_fn(|t| channel.eval(master.eval(t)))`.

This evaluates **master first, then the per-channel curve** on the master's output: $\text{eff} = \text{channel}\circ\text{master}$, i.e. $\text{eff}(t)=\text{channel}(\text{master}(t))$. The doc states the per-channel curve is *"composed after the master"* and *"master then per-channel"* — function composition $g\circ f$ means "apply $f$ ( master) then $g$ (channel)," so the code matches the documented intent exactly. This mirrors Photoshop/Lightroom Curves, where the RGB (master) curve applies and the per-channel curves act on the result. Both `point_curve`s are clamped-flat piecewise-linear interpolations of sorted control points (`lib.rs:604-623`), $C^0$, identity when empty — standard and correct. (Piecewise-linear point curves are only $C^0$, so they produce slope discontinuities at control points — acceptable for a curves tool and matching most implementations' linear/`as-shot` interpolation; a Catmull-Rom/monotone-cubic spline would be smoother but is a different design choice.)

**VERDICT: Correct.** Composition order is master-then-channel, matching both the doc and standard Curves behavior.
**Citations:** function-composition semantics; Adobe Curves master+per-channel model.

---

### 2.9 Channel mixer — raw 3×3, not row-normalized at the op

**Code** (`pipeline lib.rs:392-393`, op `Matrix`): applies `cm.matrix` directly; `ChannelMixer::default` is identity (`latent-edit/src/lib.rs:383-388`). A `row_normalized()` exists in `Mat3` but is **not** applied here (it is used for the *camera→working* and *working→sRGB* matrices, where neutral-preservation is required).

**Neutral-shift behavior.** A channel mixer whose rows do **not** each sum to 1 will shift a neutral input: $[v,v,v]\mapsto[v\Sigma_R, v\Sigma_G, v\Sigma_B]$ where $\Sigma_c$ is row $c$'s sum. This is the **standard, intended** behavior of a creative channel mixer — Photoshop/Lightroom expose a per-row "constant"/total and let the user create tints or a custom monochrome by deliberately un-balancing rows; row sums $\ne1$ are how you warm/cool or build a B&W mix. So *not* normalizing is correct for a creative tool (unlike the color-management matrices, which must stay neutral). The only caveat worth a Note: the default-on UX should make clear that a row summing to $\ne1$ tints neutrals, and a "preserve luminosity"/normalize toggle (as Photoshop offers) would be a friendly addition. The op itself is a plain, correct $M\cdot v$ (verified by the swap/identity tests).

**VERDICT: Correct.** A raw, un-normalized 3×3 is the right primitive for a creative channel mixer; the neutral shift for non-unit row sums is the expected, desired behavior, distinct from the color-management matrices that *are* row-normalized.
**Citations:** Photoshop / GIMP Channel Mixer semantics (per-channel totals, optional "monochrome"/preserve-luminosity); `Mat3::row_normalized` usage in `color.rs` (only for CM matrices).

---

## 3. Findings by Severity

| # | Severity | Location | Issue |
|---|---|---|---|
| F1 | **High** | `tone.rs:94-95` (`contrast`); `latent-edit/src/lib.rs:405` (`SelectiveTone.contrast: f32`) | `contrast` is **non-monotone for $a>1$** (tone inversion near black and white). The UI clamps to $\pm1$ (`gui.rs:569`) but the type/API/sidecar are unclamped, and `tone::contrast`'s doc promises a monotonic curve. |
| F2 | **High** | `tone.rs:53-59` (`eval` extrapolation) + `tone.rs:100-101` (`highlights`), `94-95` (`contrast`); GPU `map_pixels.wgsl:38-40` | Positive **contrast/highlights have $f'(1)=1-a\to0$**, so the LUT end-slope extrapolation **compresses highlight headroom to a near-flat plateau** ($L=8\to\approx1.03$–$1.04$). This contradicts the documented "shape, don't crush" headroom intent for exactly the highlight-targeting controls. Numerically safe (monotone, non-negative) but semantically a soft clip. |
| F3 | Medium | `color.rs:181-186`, pipeline `lib.rs:386-388`, backend `:810-817`, GPU `:75-76` | **Luma-blend saturation in wide D65-ProPhoto:** blue luma weight $\approx0.0001$, so **desaturating blue → near-black** and saturation changes shift blue lightness. Colorimetrically a correct luminance, but perceptually wrong for a saturation control. |
| F4 | Medium | `color.rs:211-253`; type `Hsl` (`latent-edit/src/lib.rs:368`) | **"HSL" tool is implemented in HSV** (value = max channel), and the space is not hue-uniform. The "lum" slider scales HSV *value*, not HSL *lightness*; hue steps are non-uniform (worst in blue). darktable's equivalent uses CIE LCh. |
| F5 | Low | `tone.rs:95,101,107,113` (clamp) + composition in `tone_curves`/`channel_curves` | Realized transfer is only **$C^0$**, not $C^1$: the `clamp(0,1)`, the piecewise-linear LUT, and piecewise-linear `point_curve`s introduce slope kinks (e.g. `blacks(-1)` flattens at the floor). Gentle and far from typical operating points, but "smoothstep" implies more smoothness than the end-to-end curve has. |
| F6 | Note | `tone.rs:18-22` (pure $\gamma2.2$) | The perceptual domain is a **pure power**, omitting sRGB/Rec.709's near-black **linear toe**; differs from sRGB only in deep shadows. Adequate, but not identical to the standard OETF and not perceptually uniform like $L^*$. |
| F7 | Note | `color.rs:216` (`h6 as u32`) | `hsv_to_rgb` sector cast is **coincidentally correct at $h=1.0$** (falls to the `_` arm, which equals sector 0 there). Relies on `rem_euclid` + the $X=0$ coincidence rather than an explicit $0..5$ clamp. |
| F8 | Note | `color.rs:241-245` | `color_mix` **band centers sit at $h=i/8$** (the lower sector edge), so "band center" means the band's pure hue, not the middle of a $1/8$-turn sector. Consistent with the doc but easy to misread. |

### Recommendations

- **F1 (High):** Clamp/guard `contrast` (and ideally all four shapes) to their monotone range inside `tone::contrast` — e.g. `let amount = amount.clamp(-2.0, 1.0);` — or document the contract as "monotone for $a\in[-1,1]$" and clamp on deserialization of `SelectiveTone`. The math contract should not depend on the UI slider's range. A non-clamping alternative for $a>1$ is to switch to a strictly-monotone S-curve family (e.g. a logistic or a power-pivot contrast) whose slope stays positive for all $a$.
- **F2 (High):** Decide what positive contrast/highlights should do above white. Options: (a) apply the contrast/highlights bend only on $[0,1]$ and pass $L>1$ through with **unit slope** (preserve headroom); (b) extrapolate with the curve's **high-end average slope** rather than the very-top local slope; (c) replace the linear extension with an explicit highlight **rolloff** so headroom compresses smoothly and intentionally. At minimum, correct the `tone.rs` comments, which currently claim headroom is "shaped, not flattened" when positive contrast/highlights flatten it.
- **F3 (Medium):** Offer (or switch to) a **chroma-preserving** saturation — scale chroma at constant lightness in Lab/LCh, or scale $C_t,C_p$ at constant $I$ in ICtCp — so desaturation goes to mid-gray and saturation does not shift blue lightness. If the luma-lerp is kept for speed, document the blue-darkening explicitly in user-facing terms (it is currently only an internal comment).
- **F4 (Medium):** Either rename the tool/type to "HSV mixer," or reimplement the lightness axis in true HSL lightness (or LCh) so "lum" matches its label; consider an LCh hue path for hue-uniform shifts. At minimum, document the HSV-not-HSL choice in the user-facing tool.
- **F5/F7/F8 (Low/Note):** Optional. For F7, clamp the sector index to `0..=5` (`(h6 as u32).min(5)`) to make correctness explicit rather than coincidental. For F8, reword "band center" to "band hue ($h=i/8$)."

---

## 4. References

1. **Charles Poynton, *Frequently Asked Questions about Gamma*** — pure-power vs Rec.709 OETF and the near-black linear toe (Q6); bit-depth for smooth shading / banding (Q13); "gamma correction … codes into a perceptually uniform domain" (Q14); process in linear vs nonlinear domain (Q19); HSI/HSV/HLS lack a perceptual basis (Q20). Downloaded: `docs/tone-poynton-gamma-faq.pdf`. <https://www.poynton.ca/faq/gammafaq/GammaFAQ.pdf>
2. **Charles Poynton, *Frequently Asked Questions about Color*** — definition of lightness $L^*=116(Y/Y_n)^{1/3}-16$ and "a 0.45 power function … comparable to the $1/3$ power function defined by $L^*$" (Q4); saturation as colourfulness in proportion to brightness (Q6); luma/luminance weights, blue carries least luminance and blue contouring (Q9–Q11). Downloaded: `docs/tone-poynton-color-faq.pdf`. <https://www.poynton.ca/pdf/ColourFAQ.pdf>
3. **IEC 61966-2-1 (sRGB)** — the sRGB OETF: $\sim1/2.4$ power with a linear segment below $\approx0.0031$, overall $\approx2.2$ (the "near-black linear toe" of F6). <https://www.color.org/srgb.pdf>
4. **darktable — *color zones* module** — selective lightness/chroma/hue grading in **CIE LCh** (the hue-preserving reference for F4). <https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/color-zones/>
5. **darktable — *filmic rgb*** — scene-referred linear tone mapping context (S-curve from virtual nodes; logarithmic encode); contrast for display-referred power-domain shaping. <https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/filmic-rgb/>
6. **Adobe Lightroom — HSL / Color Mixer panel** — eight hue bands (red, orange, yellow, green, aqua, blue, purple, magenta) with two-band interpolation between sliders (corroborates §2.5). <https://www.capturelandscapes.com/hsl-color-panel-in-lightroom/>
7. **GIMP — Channel Mixer filter** (same creative channel-mixer model as Photoshop): per-output-channel linear mix of inputs with a monochrome/"preserve luminosity" option; un-normalized rows deliberately tint/brighten (corroborates §2.9). <https://docs.gimp.org/2.10/en/gimp-filter-channel-mixer.html>
8. **ITU-R BT.2100 / ICtCp; Ebner & Fairchild, IPT** — hue-linear, more perceptually uniform color-difference spaces (the "proper chroma-preserving space" reference for F3/F4). <https://en.wikipedia.org/wiki/ICtCp>
9. **G. M. Johnson et al. / I. Lissner & P. Urban, "A Uniform and Hue-Linear Color Space"** — comparison of CIELAB/CAM16/ICtCp/Jzazbz hue linearity (blue-hue shifts), motivating hue-preserving grading. <https://library.imaging.org/cic/articles/25/1/art00043>

---

### Appendix A — Reproducing the numeric checks

The verification scripts are standalone Python (no dependencies), re-implementing `tone_encode/decode`, the 256-entry LUT `build`/`eval` (including the $>1$ end-slope extrapolation) and `apply_linear`, and the HSV/saturation/`color_mix` math exactly as in the Rust source. They print: (A) clamped min-slope of each shape on $[0,1]$ for $a\in\{-1,-0.5,0.5,1\}$ (all $\ge0$); (B) contrast raw min-slope vs $a$ (negative for $a>1$, threshold $a=1$ matching $\text{contrast}'=1-a$); (C) `apply_linear` on the headroom ramp $\{1,1.5,2.5,8\}$ and the top LUT slopes ($1-a$ for contrast/highlights, $1$ for shadows/blacks); (D) the analytic endpoint slopes; (E) the 256-LUT max interpolation error in 16-/8-bit levels (sub-LSB at 16-bit); plus the HSV round-trip with $V>1$, the saturation blue-collapse, and the band-center / wraparound positions. All numeric outputs quoted above were produced by these scripts.
