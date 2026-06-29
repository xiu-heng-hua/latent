# Audit 05 — Geometry & Optics

**Component:** `latent-pipeline` (geometry stage), `latent-cpu` (resampler), `latent-gpu` (GPU resampler), `latent-lens` (lensfun → profile mapping)
**Files audited:** `latent-pipeline/src/lib.rs`, `latent-cpu/src/lib.rs`, `latent-gpu/src/resample.wgsl`, `latent-lens/src/lib.rs` (with cross-reference to `latent-edit/src/lib.rs` for `LensProfile`).
**Scope:** the single SOURCE→OUTPUT geometry stage — radial lens distortion, lateral chromatic aberration (TCA), keystone (perspective) homography, straighten (rotation) with bounding-box expansion, the single bilinear resample, crop, lens-vignetting correction (pre-resample flat-field), and the creative vignette (post-crop gain); plus the lensfun coefficient → `LensProfile` conventions.
**Date:** 2026-06-27
**Method:** Each source file read line-by-line, then every model/coefficient convention verified against primary sources: lensfun's `lfDistortionModel`/`lfTCAModel`/`lfVignettingModel` API docs, the lensfun "How the corrections work" page, and the **lensfun C++ source** (`libs/lensfun/modifier.cpp`, `libs/lensfun/mod-coord.cpp`) for the exact `NormScale` normalization and the forward-vs-backward direction; the PanoTools/Hugin lens-correction-model wiki for the "abc" model; and Szeliski, *Computer Vision: Algorithms and Applications* (§3.6) for resampling/warping best practice. Reference docs are downloaded under `docs/` with a `geometry-` prefix.

---

## 1. Summary of the Geometry Model and the Single-Resample Design

`latent` folds every geometric correction into **one** coordinate map and interpolates **exactly once**. The map is OUTPUT→SOURCE (inverse mapping). In `apply_geometry` (`latent-pipeline/src/lib.rs:689`) the order is:

1. **Lens vignetting correction** — a SOURCE-space radial-gain *multiply* (`RadialGain`, `reciprocal = true`) applied **before** any resample (matching lensfun's vignetting→geometry order). A flat-field, not an interpolation.
2. **Build one homography** = `keystone ∘ straighten` (matrix product via `Transform::compose`, output canvas = straighten's expanded bounding box).
3. **Resample once** through a `Warp`: homography (with perspective divide), then a radial distortion $s(r)=1+d_0 r+d_1 r^2+d_2 r^3+d_3 r^4$ about the optical center, then a per-channel radial scale for TCA. If there is no lens term it degrades to a plain homography `resample`. Bilinear, in linear light.
4. **Crop** — an exact integer clip of the resampled result.
5. **Creative vignette** — a radial-gain *multiply* about the post-crop center, corners normalized to $r=1$.

The single-resample principle is the headline design strength and is correct (Szeliski §3.6.1: composing transforms and resampling once avoids the blur of repeated interpolation). The resampler is a 2-tap bilinear with no prefilter; the geometry stage is intended for ~unit-scale maps (crop/rotate/mild distortion), with downscaling handled elsewhere by area-averaging (`ImageBuf::downscaled`, per the `sample_bilinear` comment at `latent-cpu/src/lib.rs:264`).

**The single most important conclusion of this audit:** the *direction* (forward formula used as a backward map) is defensible and matches the lensfun convention, and the *vignetting* and *TCA* conventions match; **but the distortion/vignetting/TCA radius normalization unit does not match lensfun's.** The code normalizes by **half the shorter side** ($\texttt{inv\_norm}=2/\min(w,h)$); lensfun normalizes by the **half-diagonal** (a focal-length-scaled 35 mm-equivalent unit that, for the common case, puts $r\approx1$ at the image *corner*, not the short edge). Feeding real lensfun coefficients through the current normalization silently rescales every distortion/TCA/vignetting coefficient. This is the highest-risk correctness gap.

---

## 2. Point-by-Point Verification

### 2.1 Radial distortion model & coefficient form — `Warp::map`, `radial_distortion`

**Code** (`latent-pipeline/src/lib.rs:264-267`):
```rust
// s(r) = 1 + d0·r + d1·r² + d2·r³ + d3·r⁴ (Horner in r).
let [d0, d1, d2, d3] = self.radial;
let s = 1.0 + r * (d0 + r * (d1 + r * (d2 + r * d3)));
(self.center[0] + dx * s, self.center[1] + dy * s)
```
Applied as $p' = c + (p-c)\,s(r)$, i.e. $r_{\text{src}} = r\,s(r)$.

**Authoritative models** (lensfun `lfDistortionModel`, `docs/geometry-lensfun-lens-models.html`):
- **POLY3:** $r_d = r_u(1 - k_1 + k_1 r_u^2)$.
- **POLY5:** $r_d = r_u(1 + k_1 r_u^2 + k_2 r_u^4)$.
- **PTLENS** (PanoTools/Hugin): $r_d = r_u(a\,r_u^3 + b\,r_u^2 + c\,r_u + 1 - a - b - c)$.

(a) **Brown–Conrady radial is even-powered** — confirmed. Brown's radial distortion is $r(1 + k_1 r^2 + k_2 r^4 + \dots)$; lensfun's POLY3/POLY5 are exactly this even subset. (b) **PanoTools is the cubic abc form** — confirmed: PanoTools/Hugin "Lens correction model" gives $r_{\text{src}} = (a\,r_d^3 + b\,r_d^2 + c\,r_d + d)\,r_d$ with $d = 1-(a+b+c)$ "to keep the same image size" (`docs/geometry-panotools-lens-correction-model.html`). The code's general $1+d_0 r+d_1 r^2+d_2 r^3+d_3 r^4$ (note: $d_0,d_2$ odd, $d_1,d_3$ even) is a strict superset that can express all three; the even-only subset is Brown–Conrady and the odd terms enable PTLens. The mapping in `radial_distortion` (`latent-lens/src/lib.rs:183`) is algebraically correct:
- POLY5 $\to [0, k_1, 0, k_2]$ (exact).
- POLY3 $\to [0, k_1/(1-k_1), 0, 0]$ — factors out POLY3's built-in $(1-k_1)$ corner-anchor magnification.
- PTLENS $\to [c/D, b/D, a/D, 0]$, $D=1-a-b-c$ — factors out the $D$ magnification.

**The magnification-factoring is a real, deliberate semantic change** and is sound *for line-straightening*: POLY3's $(1-k_1)$ and PTLens's $D$ both scale the whole image so the corner stays put; dividing them out yields a pure $1+\dots$ polynomial that straightens identically and differs only by an overall zoom a later crop absorbs. The doc comments state this. **Caveat:** because the magnification is dropped, the corrected image is at a *different* scale than lensfun would produce, so a side-by-side pixel comparison against lensfun/darktable will not match without re-zoom; and there is **no auto-scale-to-fill** (the stage keeps the source frame size — noted at `lib.rs:687`), so the residual zoom shows as black borders the user must crop. The `is_finite` guard against $k_1\to1$ / $D\to0$ is appropriate.

**VERDICT: Correct (model algebra and the superset polynomial).** The coefficient form and the POLY3/POLY5/PTLENS → `[d0..d3]` mapping are right. Caveat: the magnification factoring intentionally differs from lensfun's absolute scale; combined with the normalization-unit issue (§2.3) the absolute geometry will not match lensfun.
**Citations:** lensfun `lfDistortionModel` (`docs/geometry-lensfun-lens-models.html`); PanoTools/Hugin lens correction model (`docs/geometry-panotools-lens-correction-model.html`); Brown, *Decentering Distortion of Lenses*, Photogrammetric Engineering 32(3), 1966.

---

### 2.2 Distortion **direction** — forward formula used as the backward (output→source) map

This is one of the two highest-risk questions, and the answer is **the code is correct *given its coefficient mapping*, by the same reasoning lensfun documents.**

**lensfun's own statement** ("How the corrections work", `docs/geometry-lensfun-corrections.html`): the model formulae "map the *undistorted* coordinate to the *distorted* coordinate … This seems to be wrong at first because we want to undistort after all. But given how undistortion is actually done, it makes sense." Undistortion is done by **inverse mapping**: for each *output (undistorted)* pixel you need the *source (distorted)* coordinate to sample — which is exactly the forward (undistorted→distorted) formula.

**Crucial subtlety — lensfun does NOT apply the formula directly for `poly3`/`poly5`/`ptlens` distortion.** In `mod-coord.cpp` the correction path (`Reverse`/`UnDist`) computes the *distorted* radius by **numerically inverting** the model with Newton's method:
```
// ModifyCoord_UnDist_Poly3:  Original function: Rd = k1_ * Ru^3 + Ru
//                            Target function:   k1_ * Ru^3 + Ru - Rd = 0   (solve for Ru by Newton)
```
(`docs/geometry-lensfun-mod-coord.cpp:582-583`, and similarly `_Poly5`, `_PTLens`.) lensfun treats the *output* pixel's radius as $R_d$ (the *measured/distorted* radius, because lensfun's pipeline runs in the *captured* frame), solves for $R_u$, and the forward `Dist` functions (used for the opposite direction) apply $R_d = R_u(1+\dots)$ directly.

`latent` instead treats the *output* pixel's radius as $r$ and **multiplies by $s(r)$ directly** to get the source radius (`Warp::map`). These are *opposite* algebraic operations on the *same* polynomial — but they correspond to *opposite definitions of which radius is the polynomial's input*. `latent`'s choice is internally consistent and is the standard "PanoTools forward model used as the inverse-mapping lookup" (PanoTools' $r_{\text{src}} = (\dots)r_{\text{dest}}$ is literally output→source). **The risk is not a sign error; it is that the polynomial input convention must match the coefficients' convention.** PanoTools/Hugin coefficients are defined with $r_{\text{dest}}$ (output) as the polynomial argument — so `latent`'s direct multiply is correct for PTLens-style coefficients. For lensfun POLY3/POLY5 coefficients, lensfun *defines* them with $r_u$ (undistorted) as the argument and *inverts numerically*; applying them directly (no Newton inversion) is the **first-order-correct approximation** (it differs only at order $k^2$ and above), not the exact inverse. For the small $k$ of real lenses this is a sub-pixel difference, but it is **not** bit-exact to lensfun.

**VERDICT: Correct-with-caveats.** Using the forward $s(r)$ as the OUTPUT→SOURCE map is the right *kind* of operation (it undistorts), and matches the PanoTools convention exactly. For lensfun POLY3/POLY5 it is a first-order approximation of lensfun's Newton-inverted correction, not the exact same curve — acceptable for typical coefficients, worth a comment. There is **no direction (sign) bug**: a barrel lens ($k_1<0$ for POLY) pulls the corners in, straightening them, as the `lens_distortion_straightens_a_barrel_grid` test demonstrates.
**Citations:** lensfun "How the corrections work" (`docs/geometry-lensfun-corrections.html`); `ModifyCoord_UnDist_{Poly3,Poly5,PTLens}` Newton inversion (`docs/geometry-lensfun-mod-coord.cpp:560-741`); PanoTools/Hugin model (`docs/geometry-panotools-lens-correction-model.html`).

---

### 2.3 Radius **normalization unit** — `lens_radial`: `inv_norm = 2/min(w,h)` vs lensfun's half-diagonal

**This is the single highest-risk silent-wrongness finding.**

**Code** (`latent-pipeline/src/lib.rs:672-677`):
```rust
let inv_norm = 2.0 / w.min(h);   // r = 1 at half the SHORTER side
```
and `latent-edit/src/lib.rs:528-531` documents this as "*the radius … normalized by **half the shorter image side** (so `r = 1` at the midpoint of the short edges, **matching lensfun**)*."

**lensfun's actual normalization** (`libs/lensfun/modifier.cpp:79`, downloaded `docs/geometry-lensfun-modifier.cpp`):
```cpp
NormScale = hypot (36.0, 24.0) / Crop / hypot (Width + 1.0, Height + 1.0) / RealFocal;
```
This is **not** $2/\min(w,h)$. Reading the geometry from `GetAutoScale` (`mod-coord.cpp:492-495`), the normalized distances of the eight test points are:
- corners (4 of them): $\tfrac12\sqrt{W^2+H^2}\cdot\texttt{NormScale}$
- mid short-edge: $\tfrac{H}{2}\cdot\texttt{NormScale}$, mid long-edge: $\tfrac{W}{2}\cdot\texttt{NormScale}$.

Substituting `NormScale`, the **corner** normalized radius is
$$ r_{\text{corner}} = \tfrac12\sqrt{W^2+H^2}\cdot\frac{\sqrt{36^2+24^2}}{\text{Crop}\cdot\sqrt{(W{+}1)^2+(H{+}1)^2}\cdot\text{RealFocal}} \;\approx\; \frac{1}{2}\cdot\frac{d_{35}}{\text{Crop}\cdot\text{RealFocal}}, $$
where $d_{35}=43.27$ mm is the 35 mm diagonal. lensfun's unit is therefore the **half-diagonal in focal-length-scaled 35 mm-equivalent units** — pixel-count-independent, anchored to the *diagonal* (corners), and *focal-length dependent*. In the canonical case it places $r\approx1$ near the **image corner**, and the short-edge midpoint at $r = H/\sqrt{W^2+H^2} < 1$.

`latent`'s $2/\min(w,h)$ instead places $r=1$ at the **short-edge midpoint** and the corner at $r=\sqrt{W^2+H^2}/\min(W,H)$ (e.g. $1.80$ for 3:2, $1.60$ for 4:3). So for the *same* physical point, lensfun's $r$ and `latent`'s $r$ differ by a fixed ratio $k=\frac{\min(W,H)}{\text{diag-unit}}$. Because the distortion polynomial is in powers of $r$, every coefficient is silently rescaled by $k^{-n}$: $d_1$ (the $r^2$ term) is off by $k^2$ (≈1.3× for 3:2 even before the focal-length factor), $d_3$ by $k^4$, etc. The result is **systematic over- or under-correction** that grows with radius and changes with aspect ratio — exactly the "silently rescales all coefficients" failure the doc comment claims to avoid.

Two compounding issues:
- The doc comment's claim "matching lensfun" is **false**: lensfun uses the half-diagonal (focal-scaled), not half the shorter side. (Half-the-shorter-side *is* the **PanoTools/Hugin/PTGui** convention — "radius=1.0 is half the smaller side" — so the code matches *PanoTools*, not lensfun. If coefficients are sourced from Hugin's `abc` directly this is correct; if from lensfun's database it is wrong.)
- lensfun's `NormScale` includes `/Crop/RealFocal`, i.e. a **focal-length scaling** the code has no analogue for. Even switching to half-diagonal would not reproduce lensfun without the `RealFocal`/`Crop` factor (which for rectilinear lenses where `RealFocal≈Crop·f_nominal` largely cancels against the sensor-vs-frame ratio, but not exactly, and notably not for fisheyes).

**VERDICT: Questionable / Incorrect for lensfun-sourced coefficients.** The normalization unit does not match lensfun; it matches PanoTools. Any coefficient pulled from the lensfun database (the stated runtime source) will be applied at the wrong radial scale, producing aspect-ratio-dependent over/under-correction. For coefficients authored against the PanoTools "half shorter side" convention it is correct.
**Citations:** lensfun `NormScale` (`docs/geometry-lensfun-modifier.cpp:79`, `geometry-lensfun-mod-coord.cpp:492-495,521`); PanoTools normalization "radius 1.0 is half the smaller side" (`docs/geometry-panotools-lens-correction-model.html`).

> **Note on the optical center.** lensfun scales the center offset by `min(Width,Height)/2 · NormScale` (`modifier.cpp:83-85`), i.e. lensfun's `CenterX/Y` are a *shift from geometric center in the same normalized units*. `latent-lens` maps `center = [0.5 + CenterX, 0.5 + CenterY]` (`latent-lens/src/lib.rs:142`) and `lens_radial` denormalizes to pixels via `center[0]*(w-1)`. This treats `CenterX/Y` as a *fraction of the full width/height*, whereas lensfun's offset is a fraction of `min(w,h)/2`. For the overwhelmingly common centered case (`CenterX=CenterY=0`) this is exactly `(0.5,0.5)→center` and correct; for off-center calibrations it is mis-scaled by the same family of factors as §2.3. Low severity (rarely non-zero).

---

### 2.4 Lateral chromatic aberration (TCA) — per-channel radial scale, green fixed

**Code** (`latent-pipeline/src/lib.rs:744`, `Warp::map_channel:272`): `channel_scale = [1 + ca[0], 1, 1 + ca[1]]` (R and B scaled about the center, G = 1). `ca_offsets` (`latent-lens/src/lib.rs:212`) maps lensfun LINEAR/POLY3 TCA `[kR, kB]` → `[kR-1, kB-1]`.

**Authoritative** (lensfun `lfTCAModel`, `docs/geometry-lensfun-lens-models.html`): LINEAR is $r_{d,R}=r_{u,R}\,k_R$, $r_{d,B}=r_{u,B}\,k_B$, with $k_R,k_B$ defaulting to **1**. So the per-channel scale is $k_R$, and `latent`'s "$1+ca[0]$" with $ca[0]=k_R-1$ reproduces $k_R$ exactly. POLY3 TCA is $r_{d,R}=r_{u,R}(b_R r^2 + c_R r + v_R)$; `ca_offsets` keeps only the constant $v_R$ (the leading interpolated term) and drops the radial dependence — an explicit, documented approximation.

(a) **Lateral CA correction *is* a per-channel radial scaling** — confirmed; this is the textbook TCA model and exactly lensfun's LINEAR model. (b) **Green-as-reference is standard** — confirmed: green is the demosaic luminance reference and lensfun calibrates R and B *relative to* green (the TCA terms are $k_R, k_B$; there is no $k_G$). Fixing $G=1$ and scaling R/B is correct. The same normalization-unit issue (§2.3) applies to *where* the scaling happens radially, but since the LINEAR scale is *constant* in $r$, a normalization mismatch does **not** affect a pure-LINEAR TCA correction (a constant scale about the center is unit-independent). For POLY3 TCA the dropped radial terms *would* be unit-sensitive, but they are dropped anyway.

**VERDICT: Correct (LINEAR TCA) / Correct-with-caveats (POLY3 reduced to its on-axis constant).** Convention and green-reference match lensfun. The single-constant-scale-per-channel can leave residual CA on lenses whose CA varies strongly with radius (POLY3 calibrations), which the doc comment acknowledges.
**Citations:** lensfun `lfTCAModel` LINEAR/POLY3 (`docs/geometry-lensfun-lens-models.html`).

---

### 2.5 Keystone / perspective homography — `keystone_transform`

**Code** (`latent-pipeline/src/lib.rs:652-666`): centered keystone $K$ with bottom row $[a,b,1]$, $a=\text{horizontal}/c_x$, $b=\text{vertical}/c_y$, sandwiched as $T(c)\,K\,T(-c)$ and pre-baked into the explicit matrix. The output→source homography is
$$ M = \begin{bmatrix} 1+c_x a & c_x b & -c_x k \\ c_y a & 1+c_y b & -c_y k \\ a & b & 1-k \end{bmatrix},\qquad k = a c_x + b c_y. $$

**Verification.** This is a valid projective transform: bottom row $[a,b,1-k]$ is non-degenerate for the slider range, and the perspective divide $w = a x + b y + (1-k)$ varies the effective horizontal scale linearly with $y$ (and vertical with $x$), which is precisely what converts a converging pencil of source lines into parallel output lines — the definition of keystone correction. The centering $T(c)\,K\,T(-c)$ keeps the frame center a fixed point (at $(c_x,c_y)$, $w=1-k+ac_x+bc_y=1$, mapping to itself). The unit tests confirm: `keystone_straightens_converging_verticals` lifts two converging source points onto one output column; `keystone_zero_is_a_no_op` gives identity. The math is a standard 2-DOF perspective (one for each of vertical/horizontal convergence), equivalent to the "keystone" controls in Lightroom/Hugin.

The **$w \le 0$ guard** (`map`, `lib.rs:137-142`; `Warp::map:252`) returns $(-1,-1)$ (outside the source → black). At extreme keystone a source plane point can pass through/behind the projection plane ($w\le0$), where $sx/w, sy/w$ would sign-flip or be $0/0$ (NaN). Returning a guaranteed-outside coordinate is the correct, standard handling (it is the projective "point at/behind infinity" case). `extreme_keystone_behind_the_plane_maps_outside` exercises it. **Minor caveat:** $(-1,-1)$ is only *just* outside; with bilinear it still reads pixel $(0,0)$ at weight 0 via the `at()` bounds check → black, so it is safe in practice, but a value like `f32::NAN` or a sentinel far outside would be more obviously-outside. As written it is correct because the samplers bounds-check each tap.

**VERDICT: Correct.** Valid projective keystone, center-preserving, with correct behind-the-plane handling.
**Citations:** Szeliski §2.1.2 (projective/homography transforms) and §3.6 (parametric warps), `docs/geometry-szeliski-03-image-processing.pdf`; Hardy/standard keystone derivation.

---

### 2.6 Rotation / straighten bounding-box expansion — `Transform::rotation`

**Code** (`latent-pipeline/src/lib.rs:110-128`): new canvas $nw = \lceil w|\cos|+h|\sin|\rceil$, $nh=\lceil w|\sin|+h|\cos|\rceil$; the output→source matrix is the inverse rotation $\begin{bmatrix}\cos&\sin\\-\sin&\cos\end{bmatrix}$ with a translation that maps the output center back to the source center.

**Verification.** The expanded-canvas formula $w|\cos\theta|+h|\sin\theta|$, $w|\sin\theta|+h|\cos\theta|$ is the **standard** axis-aligned bounding box of a rotated $w\times h$ rectangle (the extent of the rotated rectangle projected onto each axis) — correct, and it guarantees no content is clipped (the corner wedges fall outside the source and read black). The matrix is the *inverse* rotation (output→source for an inverse-mapped resample), with the bottom row $[0,0,1]$ (affine) — confirmed by `affine_constructors_have_a_unit_bottom_row`. The center-mapping offsets $m_{02},m_{12}$ are built so the output center $(dc_x,dc_y)$ maps to the source center $(sc_x,sc_y)$; the "$+0.5$ / $-0.5$" terms correctly account for pixel-center sampling (integer index = pixel center). `straighten_expands_the_canvas_and_keeps_the_center` and `resample_rotation_expands_and_keeps_the_center` verify both the growth and that the center pixel is preserved to $<10^{-4}$.

**VERDICT: Correct.** Standard rotated-bounding-box expansion and a correct center-preserving inverse-rotation map with proper pixel-center offsets.
**Citations:** Szeliski §3.6.1 (rotation as a 2-D parametric transform; inverse warping), `docs/geometry-szeliski-03-image-processing.pdf`.

---

### 2.7 Resampling / interpolation — `sample_bilinear`, `warp`, `resample`

**(a) Bilinear in LINEAR light — correct.** Resampling is a reconstruction of the continuous signal; the signal whose samples are physically meaningful to average is **linear radiance**, not a gamma-encoded value. Averaging gamma-encoded values darkens edges and shifts hue. The whole pipeline is linear-light `f32` and the resample inherits that. This is the correct choice and matches the consensus (Szeliski §3.6.1 treats resampling as filtering the image *signal*; gamma must be undone first — `latent` is already in the signal domain). **VERDICT: Correct.**

**(b) Bilinear has no prefilter → aliasing on DOWNSCALE.** `sample_bilinear` is a 2-tap interpolator: it reconstructs well for magnification and ~unit-scale maps but, on minification, it reads only the 4 nearest source pixels and **skips** the source pixels between output samples → undersampling/aliasing (moiré, jaggies). The code's own comment (`latent-cpu/src/lib.rs:269-272`) acknowledges this and says downscaling is done separately by area-averaging (`ImageBuf::downscaled`). **The gap:** the geometry stage's warps are *not* guaranteed unit-scale. A barrel-distortion correction *minifies* the corners (pulls them inward), a pincushion correction minifies the center, and a keystone correction strongly minifies the "far" (converging) edge — all done *through this same un-prefiltered bilinear*. There is no per-region MIP/area prefilter for the locally-minifying parts of a distortion/keystone map, so strongly-corrected regions will alias. Szeliski §3.5.2/§3.6 is explicit that **decimation must low-pass filter (prefilter) before subsampling**; a higher-order/anisotropic resampling filter (e.g. EWA, or at least a MIP-mapped trilinear) is the standard remedy for spatially-varying minification. **VERDICT: Correct-with-caveats (a real quality gap for strong distortion/keystone, not just whole-image downscale).**

**(c) Single resample (compose all warps, interpolate once) — correct and best practice.** Folding homography + radial + TCA into one `Warp::map` and sampling once is exactly Szeliski's recommendation: chaining separate resampling passes compounds the interpolation blur, so production warpers compose the transform and resample once. **VERDICT: Correct.** This is the strongest part of the design.

**(d) Border sampling reads black.** `sample_bilinear`'s `at()` returns `[0,0,0]` for any out-of-bounds tap (`latent-cpu/src/lib.rs:280`), the WGSL `fetch` does the same, and the nearest-neighbor reference backends match. So sampling past the border fades to black — appropriate for geometry (rotated/distorted wedges and keystone "sky" regions should be empty, to be cropped). One subtlety: a tap *one pixel outside* a valid edge blends edge-color toward black, producing a 1-px dark fringe at the image boundary. This is standard for "zero" border mode; "clamp/replicate" would avoid the fringe but is a defensible style choice (and the wedges are cropped anyway). **VERDICT: Correct (zero-border is a valid, intentional choice).**
**Citations:** Szeliski, *Computer Vision: Algorithms and Applications*, §3.5.2 (decimation/prefiltering) and §3.6.1 (forward vs inverse warping, resampling, compositing transforms), `docs/geometry-szeliski-03-image-processing.pdf`; Heckbert, *Fundamentals of Texture Mapping and Image Warping* (MSc thesis, UC Berkeley 1989), §3–4 on resampling filters and prefiltering for minification.

---

### 2.8 Vignetting — correction (pre-resample reciprocal) and creative vignette (post-crop gain)

**Code:** correction at `apply_geometry` (`lib.rs:697-708`) builds `RadialGain { poly: l.vignetting, reciprocal: true }` and applies it **before** the resample. `RadialGain::at` (`lib.rs:193`) computes $p = 1 + g_0 r^2 + g_1 r^4 + g_2 r^6$ and returns $1/p$ when `reciprocal`. `vignetting_falloff` (`latent-lens/src/lib.rs:225`) passes lensfun PA terms straight through.

**Authoritative** (lensfun `LF_VIGNETTING_MODEL_PA`, `docs/geometry-lensfun-lens-models.html`): "Pablo D'Angelo vignetting model (a more general variant of the $\cos^4$ law): $C_d = C_s(1 + k_1 r^2 + k_2 r^4 + k_3 r^6)$" — where $C_d$ is *corrected* and $C_s$ is *source*. **Important nuance:** lensfun's documented formula has the polynomial as a *multiplier on the source to get the corrected* value (the $k$ are positive to brighten corners), i.e. lensfun's "correction" multiplies by $(1+k_1r^2+\dots)$. `latent` instead treats `vignetting` as the **measured falloff** (the captured brightness $=\text{ideal}\cdot(1+v r^2+\dots)$ with $v<0$ for darker corners) and **divides** it out (`reciprocal: true`). These are reciprocal conventions: dividing by $(1+vr^2)$ vs multiplying by $(1+k r^2)$. To first order $1/(1+vr^2)\approx 1-vr^2$, so with $k=-v$ the two agree to first order but **not exactly** at higher order. The `latent-edit` doc comment (`lib.rs:546-549`) explicitly defines its convention as "captured brightness $=$ ideal $\cdot(1+v\dots)$, $v$ negative, correction divides it out" — which is self-consistent and matches the *sign* lensfun's measured falloff has, but the *operation* (divide vs lensfun-doc's multiply) differs at second order.

The model family ($\cos^4$/polynomial in $r^2$) is correct: the PA model is the standard generalization of $\cos^4$ falloff and uses exactly $1+k_1r^2+k_2r^4+k_3r^6$ — matching the code's `poly` in $r^2$.

**Pre-resample correction is correct.** Vignetting is a property of the *captured* (pre-geometry) frame; flat-fielding must happen in source coordinates before any geometric resampling moves pixels — which is what lensfun does (its pipeline order is colour/vignetting then geometry) and what `apply_geometry` does (vignetting multiply, then warp). Doing it as a *multiply* (not an interpolation) is also correct — it adds no blur. **VERDICT: Correct (model family, sign, pre-resample placement); Correct-with-caveats on the divide-vs-multiply convention** — verify whether lensfun's interpolated PA terms are meant to be *divided out* (as `latent` does) or *multiplied in*. If the `Terms` are the falloff, divide is right; if they are already the correction gain, `latent` would be inverting twice. The unit-normalization issue (§2.3) also rescales $r$ here.

**Creative vignette** (`lib.rs:756-767`): a `RadialGain` about the post-crop center with `inv_norm = 2/sqrt(w²+h²)` (so $r=1$ at the **corners** — the diagonal), `poly = [amount,0,0]`, `reciprocal: false` (a direct multiply). This is geometrically right for a creative vignette (corners reach full effect, $\cos^4$-like $r^2$ falloff), and `creative_vignette_darkens_corners_and_keeps_center` confirms center untouched / corners darkened. Worth noting the **internal inconsistency**: the *creative* vignette normalizes to the **half-diagonal** (corners at $r=1$) — which is the *correct lensfun-style* normalization — while the *lens distortion/vignetting correction* normalizes to the half-shorter-side (§2.3). The creative one is self-contained so this is fine, but it shows the half-diagonal unit was available and is arguably what §2.3 should also use.

**VERDICT: Correct (creative vignette; correction model family & placement) / Correct-with-caveats (divide-vs-multiply convention for the PA correction).**
**Citations:** lensfun `lfVignettingModel` PA (`docs/geometry-lensfun-lens-models.html`); D'Angelo, *Radiometric alignment and vignetting calibration*, ICVS 2007.

---

### 2.9 GPU vs CPU resample equivalence — `resample.wgsl`

**Code:** the WGSL compute shader (`latent-gpu/src/resample.wgsl`) applies the same row-major homography with the perspective divide (`w = m6·ox + m7·oy + m8; sx = (...)/w`), the same `floor`/frac bilinear with `mix`, and the same out-of-bounds→black `fetch`.

**Verification.** Same inverse map, same bilinear weights, same zero-border. **Two discrepancies vs the CPU/`Warp` path:**
1. **No `w ≤ 0` guard.** `Transform::map` and `Warp::map` return $(-1,-1)$ when $w\le0$ (`lib.rs:137`, `:252`); the shader divides unconditionally (`sx = (...)/w`). For $w<0$ the GPU gets a sign-flipped (possibly in-bounds!) coordinate and may sample *real* source pixels instead of black; for $w=0$ it produces $\pm\infty$/NaN, and `i32(floor(inf))` is UB-ish → likely reads black, but not guaranteed to match the CPU's deterministic $(-1,-1)$. On extreme keystone the GPU and CPU outputs can **differ** at the behind-the-plane corners.
2. **The WGSL is `resample` only — it has no `warp` (radial distortion / TCA) analogue.** So whenever a lens distortion or CA term is present, the GPU path either falls back to CPU or is simply not covered here; the shader handles only the pure-homography case. (Confirm the dispatch falls back to CPU `warp` for lens terms — if it silently runs the homography-only shader, lens distortion/CA would be dropped on GPU.)

Otherwise the homography resample matches the CPU bit-for-bit modulo float-order (same operations).

**VERDICT: Correct-with-caveats.** The homography bilinear resample matches CPU. Missing: the $w\le0$ guard (extreme-keystone divergence) and any radial/TCA warp shader (the `Warp` path is CPU-only here). The header comment claims parity with the CPU `resample`; that parity holds only for $w>0$ homographies.
**Citations:** comparison of `latent-gpu/src/resample.wgsl:46-48` vs `latent-pipeline/src/lib.rs:137-142,251-255`.

---

## 3. Findings by Severity

### CRITICAL

**C1 — Distortion/vignetting/TCA radius normalization unit does not match lensfun (silent coefficient rescale).**
`latent-pipeline/src/lib.rs:675` (`inv_norm = 2.0 / w.min(h)`) and the convention asserted at `latent-edit/src/lib.rs:528-531` ("half the shorter image side … matching lensfun"). lensfun normalizes by the **half-diagonal** in focal-scaled 35 mm units (`NormScale = hypot(36,24)/Crop/hypot(W+1,H+1)/RealFocal`, `docs/geometry-lensfun-modifier.cpp:79`; corner at $r\approx1$ per `mod-coord.cpp:492-495`), **not** half the shorter side. Half-the-shorter-side is the **PanoTools/Hugin** convention. Result: every lensfun-sourced distortion coefficient is applied at the wrong radial scale (off by $k^n$ where $k=\min(w,h)/\text{diag-unit}$, ≈1.3–1.8× per even order for common aspect ratios), giving aspect-ratio-dependent over/under-correction that worsens toward the corners. The vignetting correction (§2.8) and POLY3-TCA radial terms inherit the same rescale.
**Reference:** `docs/geometry-lensfun-modifier.cpp:79`, `geometry-lensfun-mod-coord.cpp:492-495,521`; PanoTools "radius 1.0 is half the smaller side" (`docs/geometry-panotools-lens-correction-model.html`).
**Recommendation:** Decide the source-of-truth convention and make the doc comment honest. If coefficients come from **lensfun**: normalize by the half-diagonal and incorporate the `RealFocal`/`Crop` scaling (set `inv_norm = 2.0/hypot(w,h)` as the geometric baseline, and ideally fold the focal-length factor so it matches `NormScale`). If coefficients come from **PanoTools/Hugin `abc`** directly, the current `2/min(w,h)` is correct and only the "matching lensfun" comment is wrong. Add a round-trip test that straightens a known-distorted grid using a real lensfun `<distortion>` entry and checks residual line-straightness vs lensfun's own output.

### HIGH

**H1 — GPU `resample.wgsl` lacks the `w ≤ 0` guard and has no radial/TCA `warp` shader.**
`latent-gpu/src/resample.wgsl:46-48` divides by `w` unconditionally (CPU returns $(-1,-1)$ for $w\le0$, `lib.rs:137`), so extreme-keystone corners can sample real pixels or NaN on GPU and diverge from CPU; and the shader implements only the homography `resample`, not the `Warp` (distortion + CA) path. If the dispatcher does not fall back to CPU `warp` when a lens term is present, lens distortion/CA would be silently dropped on GPU.
**Reference:** §2.9; `resample.wgsl` vs `latent-pipeline/src/lib.rs:137-142,251-267,272-282`.
**Recommendation:** Add `if (w <= 0.0) { out = black; return; }` to the shader; add a `warp.wgsl` mirroring `Warp::map`/`map_channel` (radial Horner + per-channel scale), or assert a CPU fallback for any non-trivial `Warp`. Add a CPU/GPU equivalence test over a perspective + distortion case.

### MEDIUM

**M1 — Forward distortion formula applied directly is only first-order-equivalent to lensfun's Newton-inverted correction (for POLY3/POLY5).**
`latent-pipeline/src/lib.rs:264-267` multiplies the output radius by $s(r)$; lensfun's `UnDist_{Poly3,Poly5}` solve $R_d=R_u(1+\dots)$ for $R_u$ by Newton (`docs/geometry-lensfun-mod-coord.cpp:582-583`). For PanoTools/PTLens coefficients the direct multiply is the *defined* operation (output→source), so this is exact; for lensfun POLY coefficients it differs at $O(k^2)$. Sub-pixel for real lenses but not bit-exact.
**Reference:** §2.2.
**Recommendation:** Note this in the doc comment; if exactness vs lensfun is wanted for POLY models, Newton-invert (2–3 iterations) instead of the direct multiply.

**M2 — Vignetting correction divides by the falloff polynomial where lensfun's PA doc multiplies (reciprocal-convention ambiguity).**
`latent-pipeline/src/lib.rs:705` (`reciprocal: true`) divides by $(1+v r^2+\dots)$; lensfun's documented PA correction is $C_d=C_s(1+k_1r^2+\dots)$ (a multiply). The two agree only to first order. `vignetting_falloff` passes lensfun's interpolated `Terms` straight through (`latent-lens/src/lib.rs:225`) — so the question is whether those `Terms` are the *falloff* (divide is right) or the *correction gain* (then `latent` double-inverts).
**Reference:** §2.8; `docs/geometry-lensfun-lens-models.html` (PA model).
**Recommendation:** Confirm against lensfun's `ModifyColor`/`vignetting` apply code whether the stored PA `k` are falloff or gain, and align the divide/multiply (and sign) accordingly. Add a flat-field round-trip test against a known lensfun vignetting entry.

**M3 — Un-prefiltered bilinear aliases on the locally-minifying parts of distortion/keystone maps.**
`latent-cpu/src/lib.rs:273` (`sample_bilinear`). The comment scopes bilinear to ~unit-scale and defers downscale to area-averaging, but distortion/keystone warps minify locally (corners under barrel correction, the converging edge under keystone) through this same 2-tap path → aliasing with no prefilter.
**Reference:** §2.7(b); Szeliski §3.5.2/§3.6 (prefilter before subsampling).
**Recommendation:** For strong corrections, use a MIP-mapped/trilinear or EWA sampler in the warp, or at minimum a small adaptive box prefilter sized by the local Jacobian of the map. Low urgency if corrections are mild, but document the limitation in `warp`.

### LOW

**L1 — Optical-center offset scaling differs from lensfun for off-center calibrations.**
`latent-lens/src/lib.rs:142` maps `center = 0.5 + CenterX` (fraction of full dimension); lensfun's `CenterX/Y` are offsets in `min(w,h)/2 · NormScale` units (`modifier.cpp:83-85`). Exact for the common centered case (`CenterX=0`), mis-scaled otherwise.
**Recommendation:** Scale the center offset by `min(w,h)/2` to match lensfun if off-center profiles are ever used.

**L2 — `w ≤ 0` sentinel `(-1,-1)` is only marginally outside.**
`latent-pipeline/src/lib.rs:141,254`. Safe because every sampler bounds-checks each tap, but a more obviously-outside sentinel (e.g. a large negative) would be more robust to future sampler changes.
**Recommendation:** Optional; keep the bounds-checks as the real guarantee.

### NOTE

**N1 — No auto-scale-to-fill.** `apply_geometry` keeps the source frame size (`lib.rs:687`), so distortion/keystone/straighten leave black wedges to be cropped. Intentional and documented; lensfun offers `GetAutoScale` (`mod-coord.cpp:469`) for this — a future addition.

**N2 — Magnification factored out of POLY3/PTLens.** `latent-lens/src/lib.rs:178-182` deliberately drops POLY3's $(1-k_1)$ and PTLens's $D$ overall scale. Correct for line-straightening; means the absolute output scale differs from lensfun (a crop/zoom absorbs it). Documented.

**N3 — Single-resample design is exemplary.** Folding homography ∘ radial ∘ per-channel-CA into one `Warp` and interpolating once (`lib.rs:726-746`, `latent-cpu/src/lib.rs:118-143`) is the correct best practice (Szeliski §3.6.1) and the strongest part of the geometry stage.

---

## 4. References

**Primary — lensfun (downloaded under `docs/`):**
- lensfun API, *Structures and functions for lenses* — `lfDistortionModel` (POLY3/POLY5/PTLENS), `lfTCAModel` (LINEAR/POLY3), `lfVignettingModel` (PA). `docs/geometry-lensfun-lens-models.html` — https://lensfun.github.io/manual/latest/group__Lens.html
- lensfun, *How the corrections work* (forward-formula / backward-mapping direction). `docs/geometry-lensfun-corrections.html` — https://lensfun.github.io/manual/latest/corrections.html
- lensfun, *Lens calibration data format*. `docs/geometry-lensfun-calibration-format.html` — https://lensfun.github.io/manual/latest/elem_calibration.html
- lensfun source, `libs/lensfun/modifier.cpp` — `NormScale = hypot(36,24)/Crop/hypot(W+1,H+1)/RealFocal` and center scaling. `docs/geometry-lensfun-modifier.cpp` — https://github.com/lensfun/lensfun/blob/master/libs/lensfun/modifier.cpp
- lensfun source, `libs/lensfun/mod-coord.cpp` — `ModifyCoord_UnDist_{Poly3,Poly5,PTLens}` (Newton inversion), `GetAutoScale`, `ApplyGeometryDistortion`. `docs/geometry-lensfun-mod-coord.cpp` — https://github.com/lensfun/lensfun/blob/master/libs/lensfun/mod-coord.cpp

**Primary — PanoTools / resampling:**
- PanoTools / Hugin, *Lens correction model* (the abc model, $d=1-(a+b+c)$, "radius 1.0 is half the smaller side"). `docs/geometry-panotools-lens-correction-model.html` — https://hugin.sourceforge.io/docs/manual/Lens_correction_model.html
- R. Szeliski, *Computer Vision: Algorithms and Applications*, §3.5.2 (decimation/prefiltering), §3.6.1 (forward vs inverse warping, resampling, compositing transforms). `docs/geometry-szeliski-03-image-processing.pdf` — http://mesh.brown.edu/engn1610/szeliski/03-ImageProcessing.pdf (2nd ed. full text: https://szeliski.org/Book/)
- P. Heckbert, *Fundamentals of Texture Mapping and Image Warping*, MSc thesis, UC Berkeley, 1989 (resampling filters; prefiltering for minification; EWA) — https://www.cs.cmu.edu/~ph/texfund/texfund.pdf

**Background:**
- D. C. Brown, *Decentering Distortion of Lenses*, Photogrammetric Engineering 32(3):444–462, 1966 (even-powered radial model).
- P. D'Angelo, *Radiometric alignment and vignetting calibration*, ICVS 2007 (the PA vignetting model).
