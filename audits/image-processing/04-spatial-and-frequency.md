# Audit 04 — Spatial & Frequency-Domain Filters

**Component:** `latent-pipeline` (filter models / lowering) and `latent-cpu` (the reference implementations)
**Files audited:** `latent-pipeline/src/lib.rs` (lines ~46–567), `latent-cpu/src/lib.rs` (lines ~65–262), with cross-reference to `latent-image/src/color.rs` (`luminance`, `LUMA_WEIGHTS`) and `latent-image/src/tone.rs` (`GAMMA`).
**Scope:** The four spatial primitives that run in the GLOBAL stage, in pipeline order **denoise → dehaze → clarity → sharpen** (all before geometry resample): box blur, the two `CombineKind` recombines (Unsharp / LocalContrast), the split-channel bilateral denoise, and the dark-channel-prior dehaze. Each is checked for correctness against the primary literature.
**Date:** 2026-06-27
**Method:** Code read line-by-line, then every numeric/algorithmic claim verified against primary sources. PDFs downloaded under `docs/` with a `spatial-` prefix:
- `docs/spatial-bilateral-tomasi-1998.pdf` — C. Tomasi, R. Manduchi, *Bilateral Filtering for Gray and Color Images*, ICCV 1998 (8 pp., Stanford author host).
- `docs/spatial-dehaze-he-2011-tpami.pdf` — K. He, J. Sun, X. Tang, *Single Image Haze Removal Using Dark Channel Prior*, **IEEE TPAMI 33(12), 2011** — the extended journal version of the CVPR 2009 paper, by the same authors (13 pp., CUHK mmlab host). All equation numbers below cite this version; the model, prior, $\omega=0.95$, $t_0=0.1$, $15\times15$ patch, and the soft-matting refinement are identical to CVPR 2009.
- `docs/spatial-dehaze-ipol-2024.pdf` — peer-reviewed IPOL reproduction of the method (companion reference).

Numeric claims were checked with throwaway Python (`math` only); the snippets and their output are reproduced inline as evidence.

---

## 1. Summary of Each Filter's Model

### 1.1 Box blur (`latent-cpu` `blur` / `blur_axis`)
A separable box mean: a horizontal 1-D pass of width $2r+1$ followed by a vertical one, $r=\mathrm{round}(\text{radius})$, border **clamp-to-edge** (replicate). For a 2-D box this is exact (the box kernel is rank-1 separable), so the separation is an $O(r)$ optimization with no loss. Radius $0$ returns a clone.

$$ B_r[f](x) = \frac{1}{2r+1}\sum_{d=-r}^{r} f\big(\mathrm{clamp}(x+d)\big), \qquad \text{2-D} = B_r^{\text{vert}}\circ B_r^{\text{horiz}}. $$

### 1.2 Unsharp recombine (`CombineKind::Unsharp{gain}`, lowered by `sharpen`)
`sharpen` blurs once to a base $L=B_r[I]$, sets $\text{gain}=1+\text{amount}$, and recombines

$$ O = L + \text{gain}\cdot(I - L) = L + (1+a)(I-L). $$

### 1.3 Local-contrast recombine (`CombineKind::LocalContrast{amount}`, lowered by `clarity`)
`clarity` builds the base from **three** successive box blurs $L = B_r^{3}[I]$ (central-limit Gaussian approximation), then

$$ O = I + a\cdot m\big(\mathrm{Y}(L)\big)\cdot (I - L), \qquad m(\ell) = 1 - (2b-1)^2,\ \ b = \mathrm{clamp}(\ell)^{1/\gamma},\ \gamma=2.2, $$

where $\mathrm{Y}$ is relative luminance and $m$ is the midtone window evaluated in the perceptual (gamma-2.2) domain on the **base** luminance.

### 1.4 Bilateral denoise (`bilateral_pixel`, lowered by `noise_reduction`)
Each pixel splits into luminance $Y=\mathrm{Y}(\text{rgb})$ and chroma $C = \text{rgb}-Y$ (a per-channel offset). **Two independent** bilateral averages run over the same $\pm r$ window ($r=\mathrm{round}(\text{radius})$), spatial $\sigma_s=r/2$, with **separate** range scales — luma stops on the luma difference, chroma stops on the chroma (vector) difference:

$$ w_s(\Delta) = e^{-\frac{\|\Delta\|^2}{2\sigma_s^2}},\quad
Y_{\text{out}} = \frac{\sum w_s\,e^{-\frac{(Y-Y_n)^2}{2\sigma_l^2}}\,Y_n}{\sum w_s\,e^{-\frac{(Y-Y_n)^2}{2\sigma_l^2}}},\quad
C_{\text{out}} = \frac{\sum w_s\,e^{-\frac{\|C-C_n\|^2}{2\sigma_c^2}}\,C_n}{\sum w_s\,e^{-\frac{\|C-C_n\|^2}{2\sigma_c^2}}}, $$

recombined $O = Y_{\text{out}} + C_{\text{out}}$. A scale of $0$ leaves that component untouched.

### 1.5 Dark-channel dehaze (`dehaze_dark_channel`, `dehaze_recover`, lowered by `dehaze`)
Atmospheric scattering model $I = J\,t + A(1-t)$ with airlight **fixed at $A=1$** (neutral white, *not* estimated). Patch dark channel over a $(2\cdot4+1)^2 = 9\times9$ window (`DEHAZE_PATCH=4`):

$$ \mathrm{dc}(x) = \min_{y\in\Omega(x)}\ \min_{c\in\{r,g,b\}} I^c(y), \qquad
t = \mathrm{clamp}\big(1 - \text{strength}\cdot\mathrm{dc},\ t_0,\ 1\big),\ t_0=0.1, $$

$$ J^c = \frac{I^c - 1}{t} + 1 \quad(\text{for } I^c\le 1),\qquad \text{headroom } (I^c-1)_+\ \text{passed through}. $$

`strength` plays the role of the prior's $\omega$. The raw patch transmission is used directly (no soft-matting / guided-filter refinement).

---

## 2. Point-by-Point Verification

### 2.1 Unsharp mask — formula and algebra

**Code** (`latent-pipeline/src/lib.rs:438-440`, recombine at `latent-cpu/src/lib.rs:79-86`):
```rust
let base = backend.blur(&img, s.radius);
let gain = 1.0 + s.amount;
backend.combine(&mut img, &base, &CombineKind::Unsharp { gain });
// combine: *px = o[c] + gain * (px[c] - o[c])
```

**Standard.** The canonical digital unsharp mask is
$$ \text{sharpened} = \text{original} + \text{amount}\cdot(\text{original} - \text{blurred}). $$
Wikipedia states it verbatim: *"sharpened = original + (original − blurred) × amount"* and defines amount as controlling *"the magnitude of each overshoot (how much darker and how much lighter the edge borders become)."* Imatest gives the equivalent USM form $L_{\text{USM}} = L - k_{\text{USM}}\cdot \text{Blur}$ with a Gaussian blur kernel.

**Algebra** (verified numerically): substituting $\text{gain}=1+a$ into the code form,
$$ L + (1+a)(I-L) = L + (I-L) + a(I-L) = I + a(I-L), $$
which is **exactly** the standard with $\text{amount}=a$. Random-input check (`other + (1+amount)(img-other)` vs `img + amount(img-other)`) agreed to $<10^{-9}$ over many samples.

> **VERDICT: Correct.** The `gain = 1 + amount` parameterization is the standard unsharp mask exactly. The step-edge test (`sharpening_overshoots_a_step_edge`, `lib.rs:1119`) confirms the expected dark-side undershoot and bright-side overshoot.
> **Citations:** Wikipedia, *Unsharp masking* (formula, overshoot/halo semantics); Imatest, *Sharpening* (USM equation, "small overshoots enhance sharpness; large overshoots cause halos").

**Caveat — domain (see §3, Note N1).** The blur and recombine run in **linear light**. Blurring/convolution is *physically* correct in linear light (NVIDIA GPU Gems 3, Ch. 24, *The Importance of Being Linear*: light transport is linear, so filtering/blending/mip generation must be linear to be correct). But unsharp **overshoot is asymmetric in linear light**: because the eye is ~gamma-2.2, equal linear $\pm\delta$ overshoots around a base look like a much larger bright halo than dark halo, so linear-light sharpening tends to produce **brighter, more visible halos on the dark side of edges** than the same operation in a perceptual/luma domain — the opposite of what most camera/raw sharpeners do (they sharpen the gamma-encoded luma/L channel). This is a *design tradeoff consistent with `latent`'s linear-light philosophy*, not a bug, but it should be understood. Sharpening all three RGB channels independently (rather than luma only) can also shift hue/saturation at edges (color fringing), where the convention in raw developers is luminance-only sharpening.

---

### 2.2 Box blur — separability, border, radius/round semantics

**Code** (`latent-cpu/src/lib.rs:65-74, 234-262`): two 1-D passes, each averaging $2r+1$ taps with `clamp(0, dim-1)` indices; `r = radius.round().max(0)`.

**Standard.** A 2-D box kernel is the outer product of two 1-D box kernels, so $B^{\text{2D}}_r = B^{\text{vert}}_r \ast B^{\text{horiz}}_r$ exactly — the separation is an $O(r)$-per-pixel optimization with **no** approximation. Verified by the test `blur_matches_a_box_average_reference` (`lib.rs:408`). Border handling is **clamp-to-edge** (edge-pixel replication), one of the standard pad modes (Wikipedia, *Box blur*, "Extend / Fill in a constant color extending from the last pixel"); it is energy-preserving (the divisor stays $2r+1$, and replicated samples are real edge values), so a uniform image is unchanged (test `blur_leaves_a_uniform_image_unchanged`).

> **VERDICT: Correct.** Separable box blur with clamp-to-edge is a standard, exact, energy-preserving implementation.
> **Citation:** Wikipedia, *Box blur* (separability; border-fill options).

**Radius/round consistency (see §3, Finding M3).** `blur` uses `r = radius.round().max(0)` (so radius `0` → identity, radius `<0.5` → identity); `bilateral_pixel` uses `r = radius.round().max(1)` (window $\pm r$); `dehaze` uses a **constant** `DEHAZE_PATCH=4` independent of any radius. The blur/denoise rounding is *consistent with each other* (both `round`), but the gate predicates differ slightly: `apply_global` gates blur on `radius > 0.0` (so radius `0.3` rounds to `0` → silent no-op clone) while denoise gates on `params.radius.round() >= 1.0`. Harmless, but the dehaze patch size being a fixed constant (not tied to the UI "radius" the other tools expose) is an inconsistency worth a doc note (§3, M2).

---

### 2.3 Clarity — three box blurs as a Gaussian, and midtone weighting

#### 2.3.a Three box blurs ≈ Gaussian (central limit)

**Claim** (`lib.rs:421-423`): three box passes are *"a central-limit approximation of a Gaussian — because a single box kernel rings at the broad clarity radius and would itself create halos."*

**Standard.** By the central limit theorem, repeated box convolution converges to a Gaussian (Wikipedia, *Box blur*: *"By the central limit theorem, repeated application of a box blur will approximate a Gaussian blur"*, citing Getreuer 2013). Three passes is the **textbook count** for a fast Gaussian approximation (Wells 1986, *Efficient synthesis of Gaussian filters by cascaded uniform filters*, PAMI 8(2):234–239 — the canonical 3-to-4-pass box-cascade result; widely repeated, e.g. the CSS/SVG blur spec uses a 3-box approximation).

**Variance / effective $\sigma$.** A single discrete box mean over integer offsets $[-r,r]$ has variance $\sigma_1^2 = \frac{(2r+1)^2-1}{12} = \frac{r(r+1)}{3}$. Independent passes **add** variance, so $K$ boxes give $\sigma_K^2 = K\cdot\frac{r(r+1)}{3}$. For $K=3$:
$$ \sigma_{3\text{box}} = \sqrt{3\cdot\tfrac{r(r+1)}{3}} = \sqrt{r(r+1)} \xrightarrow{r\to\infty} r. $$

**Evidence** (Python, `math` only):
```
== 3-box effective sigma ==
r=  1  sigma_3box=  1.414  s3/r=1.4142
r=  3  sigma_3box=  3.464  s3/r=1.1547
r=  5  sigma_3box=  5.477  s3/r=1.0954
r= 10  sigma_3box= 10.488  s3/r=1.0488
r= 50  sigma_3box= 50.498  s3/r=1.0100   # -> r
```
So **three boxes of radius $r$ give an effective Gaussian $\sigma \approx r$** (exactly $\sqrt{r(r+1)}$, within 1% of $r$ for $r\ge 10$). Useful, sensible scaling for a "clarity radius".

**How close to a true Gaussian?** Kernel-shape comparison of $K$-box cascades against a same-variance sampled Gaussian (max-abs and $L_1$ error over the kernel):
```
r= 5 K=2: L1=0.0937   r= 5 K=3: L1=0.0509   r= 5 K=4: L1=0.0381
r=10 K=2: L1=0.0932   r=10 K=3: L1=0.0510   r=10 K=4: L1=0.0381
excess kurtosis of K-box -> 0:  K=1:-1.200  K=2:-0.600  K=3:-0.400  K=4:-0.300
```
Three boxes give ~5% $L_1$ kernel error and excess kurtosis $-0.4$ (a single box is $-1.2$). The bulk of the improvement is $1\!\to\!3$; $3\!\to\!4$ only trims ~1.3 pts more $L_1$. **Three is the right, conventional count** — a good cost/quality knee. The motivation in the comment (a single box rings/halos at a broad radius) is exactly right: a box kernel has a sinc frequency response with large side-lobes; cascading suppresses them.

> **VERDICT: Correct.** Three-box Gaussian approximation is standard practice and the effective $\sigma\approx r$ is a sound, documented relationship.
> **Citations:** Wells, PAMI 1986 (cascaded uniform filters ≈ Gaussian, 3–4 passes); Wikipedia, *Box blur* (CLT statement, Getreuer 2013); Getreuer, *A Survey of Gaussian Convolution Algorithms*, IPOL 2013.

#### 2.3.b Midtone parabola placement

**Code** (`lib.rs:453-456`): $m(\ell)=1-(2b-1)^2$, $b=\mathrm{clamp}(\ell)^{1/2.2}$, evaluated on the **base (low-frequency) luminance**.

**Claim** (`lib.rs:445-452`): the window peaks on perceptual mid-gray (≈0.18 linear) rather than linear 0.5, "genuinely weights the midtones," and protects highlights/shadows from halos.

**Numeric check** (Python):
```
Parabola peaks (m=1) at b=0.5 => linear luma = 0.5^2.2 = 0.2176
  m(0.18) = 0.9932   (photographic mid-gray ~0.18 linear)
  m(0.5)  = 0.7889   (linear 0.5 sits well off-peak, as intended)
  half-max (m=0.5) at linear luma = 0.0146 and 0.7058
  L*=50 mid-gray linear = 0.1842
```
The peak lands at **linear 0.2176** ($=0.5^{2.2}$), within striking distance of the photographic 18% gray card (0.18) and the perceptual $L^*=50$ mid-gray (0.184). $m(0.18)=0.993$ — essentially full weight at mid-gray — while linear 0.5 is pushed down to $m=0.789$ and the extremes go to $0$. The claim holds quantitatively. Evaluating $m$ on the **blurred base** (not the high-frequency detail) is the right choice: it's the local *tone* that should gate the contrast boost, and it makes the gate spatially smooth so the protection doesn't itself create a hard transition. The half-max points (linear 0.015 and 0.706) show the window genuinely tapers contrast toward both black and white, which is what suppresses clarity halos at the tonal extremes.

> **VERDICT: Correct.** The parabola peaks on perceptual mid-gray as claimed; midtone-gating the local-contrast term is a sound, halo-aware design. The `clarity_boosts_midtone_local_contrast` and `local_contrast_amplifies_midtones_and_protects_the_extremes` tests confirm the behavior.
> **Citation:** Photographic 18%-gray / $L^*=50$ mid-gray conventions (CIE $L^*$, ISO 7589 / standard middle-gray); the parabola $1-(2b-1)^2$ is the unique downward parabola with zeros at $b\in\{0,1\}$ and peak $1$ at $b=0.5$.

---

### 2.4 Bilateral denoise — vs Tomasi & Manduchi (ICCV 1998)

**Reference equations** (`docs/spatial-bilateral-tomasi-1998.pdf`). The combined bilateral filter (their eqs. 5–6) averages each pixel with weights that are the **product** of a domain (spatial) Gaussian and a range (photometric) Gaussian, normalized by the weight sum:
$$ h(x) = k^{-1}(x)\int f(\xi)\,\underbrace{c(\xi,x)}_{\text{domain}}\,\underbrace{s(f(\xi),f(x))}_{\text{range}}\,d\xi,\quad
k(x)=\int c\,s\,d\xi, $$
with the Gaussian case $c=e^{-\frac12(d/\sigma_d)^2}$, $s=e^{-\frac12(\delta/\sigma_r)^2}$, $d=\|\xi-x\|$ (geometric spread $\sigma_d$), $\delta=\|f(\xi)-f(x)\|$ (photometric spread $\sigma_r$).

**Code** (`bilateral_pixel`, `lib.rs:525-568`). Spatial term `spatial = -(dx²+dy²)·inv_2ss2`, range terms subtracted in the exponent: `wl = exp(spatial - dl²·inv_2sl2)`, normalized by `wsum`. This is **exactly** $e^{-\frac{\|d\|^2}{2\sigma_s^2}}\,e^{-\frac{\delta^2}{2\sigma_r^2}}$ as a single `exp(sum)` — algebraically identical to the product of the two Gaussians, normalized by the weight sum. ✔

Point-by-point:

| Aspect | Paper | Code | Verdict |
|---|---|---|---|
| Weight = domain × range | eq. 5–6, product of two Gaussians | `exp(spatial − Δ²·inv2σ²)` = product | **Correct** |
| Normalization by $k=\sum w$ | eq. 6 | `acc/wsum` per component | **Correct** |
| Spatial Gaussian | $\sigma_d$ "based on desired low-pass" | $\sigma_s = r/2$ | **Correct-with-caveats** (truncation, below) |
| Range Gaussian | $\sigma_r$ photometric spread | $\sigma_l$, $\sigma_c$ | **Correct** form |
| Edge preservation | range term rejects cross-edge pixels | same | **Correct** (test `…keeps_an_edge`) |
| Color handling | **single** combined distance, ideally CIE-Lab (§5) | **two separate** Y/chroma filters | **Correct-with-caveats** (variant, below) |

**Caveat 1 — luma/chroma split vs the paper's combined distance (Finding M1).** Tomasi & Manduchi §5 are explicit that for color one should **not** filter R,G,B separately — *"Separate smoothing results in an even more pronounced pink-purple band"* — but instead *"combine the three color bands appropriately, and measuring photometric distances between pixels in the combined space … using Euclidean distance in the CIE-Lab color space … the most natural type of filtering for color images."* i.e. **one** range weight from a single perceptual color distance.

The code instead runs **two** bilateral passes with **two** range weights: luma (scalar $\Delta Y$) and chroma (vector $\Delta C$ in an RGB-minus-luma offset space). This is *not* the per-RGB-channel mistake the paper warns against (that would be three independent scalar filters); it is the **standard luma/chroma denoising decomposition** used throughout raw/photo NR (Lightroom/ACR, dcraw/wavelet NR, "Luminance" + "Color" sliders), and it has a real benefit the paper's single filter lacks: chroma can be smoothed **harder** than luma without destroying luminance detail, because color noise is low-frequency blotch. It is therefore a **legitimate, well-established variant** (close in spirit to YCbCr / cross-component bilateral denoising), but it **departs from the cited paper's formulation** and is not what T&M eq. 5–6 describes. The chroma distance is measured in a linear RGB-offset space, not CIE-Lab, so it is *not* perceptually uniform — a moderate accuracy point, not an error. **Recommendation:** keep the split (it's the right tool), but cite it as a luma/chroma variant rather than implying it is the T&M color filter, and consider that the chroma metric is non-perceptual.

**Caveat 2 — spatial truncation at $\pm 2\sigma$ (Finding H1, the flagged item).** With $\sigma_s = r/2$ and window $\pm r$, the window edge is at exactly **$2\sigma$**. The spatial weight there is $e^{-2}=0.135$ — a *hard* cutoff at 13.5% of peak, not the near-zero a properly supported Gaussian reaches.
```
Spatial Gaussian at window edge (2σ): exp(-2) = 0.1353
At 3σ it would be:                    exp(-4.5) = 0.0111
1D Gaussian mass beyond ±2σ = erfc(2/√2) = 0.0455  (~4.6% per axis)
1D Gaussian mass beyond ±3σ = erfc(3/√2) = 0.0027  (~0.27%)
```
The standard convention is to truncate a Gaussian at **$3\sigma$** (SciPy `gaussian_filter` defaults `truncate=4.0`; common kernel size $2\lceil 3\sigma\rceil+1$; Bart Wronski / general CV practice: radius $2$–$3\sigma$). Truncating at $2\sigma$ drops ~4.6% of the 1-D Gaussian mass per axis (~9% in 2-D) and, more importantly, leaves a **discontinuous step** in the kernel at the window boundary (weight jumps from 0.135 to 0). For an edge-preserving filter this is mostly cosmetic (the range term already gates most far pixels), but it (a) makes the spatial kernel a truncated-Gaussian-with-a-step rather than a smooth one, and (b) means $\sigma_s$ is effectively smaller than the window suggests. **Recommendation:** either set $\sigma_s = r/3$ (window $\pm 3\sigma$, the standard), or keep $\sigma_s=r/2$ but extend the window to $\pm\lceil 3\sigma_s\rceil = \pm\lceil 1.5r\rceil$. The current $\pm 2\sigma$ is *defensible* (it bounds cost and the comment acknowledges the choice — "falls off across the support … rather than behaving like a box") but is **narrower support than the literature standard** and should be flagged.

**Other observations.**
- **Luma weights (ProPhoto-D65).** `LUMA_WEIGHTS = [0.2788, 0.7211, 0.000113]` — the blue weight is ~$10^{-4}$ (`color.rs:177-181` flags this as colorimetrically correct for wide primaries). Consequence for the chroma split: blue contributes essentially **nothing** to $Y$, so the blue chroma offset $C_b = B - Y \approx B$ carries almost the full blue signal. The chroma filter therefore does the heavy lifting on blue. That's internally consistent (luma+chroma still reconstruct RGB exactly: $O=Y_{\text{out}}+C_{\text{out}}$ telescopes back), but it means blue **luminance detail** is effectively governed by the *chroma* scale, not the luma scale — a subtle consequence worth a doc note (Finding L1).
- **Non-separability / performance (Note N2).** The bilateral is genuinely non-separable (the range weight depends on 2-D content), so it is $O(r^2)$ per pixel — correct and unavoidable for the exact filter. The code parallelizes rows via Rayon. No fast-bilateral approximation (bilateral grid, permutohedral) is used; fine for a reference backend, a performance note for large radii.
- **Border.** `clamp(0,dim-1)` index replication, consistent with the box blur. Correct.

> **VERDICT (overall): Correct-with-caveats.** The core bilateral math (product of spatial × range Gaussians, normalized by weight sum, edge-stopping) matches Tomasi & Manduchi exactly. The two departures are (i) the luma/chroma split with separate range weights — a standard denoising variant, but **not** the paper's single-combined-distance color filter, and in a non-perceptual chroma metric; and (ii) the $\pm 2\sigma$ spatial truncation vs the standard $3\sigma$.
> **Citation:** Tomasi & Manduchi, ICCV 1998, eqs. 1–6 (domain/range/combined) and §5 (color: combined distance, CIE-Lab, against per-channel filtering).

---

### 2.5 Dehaze — vs He, Sun & Tang (CVPR 2009 / TPAMI 2011)

**Reference equations** (`docs/spatial-dehaze-he-2011-tpami.pdf`):
- **Model** eq. (1): $I(x) = J(x)\,t(x) + A\,(1-t(x))$. ✔ matches code (`dehaze_recover` doc + impl).
- **Dark channel** eq. (5): $J^{\text{dark}}(x)=\min_{y\in\Omega(x)}(\min_c J^c(y))$. ✔ matches `dehaze_dark_channel` (patch-min of channel-min).
- **Transmission** eq. (12): $\tilde t(x) = 1 - \omega\,\min_{y}\big(\min_c \frac{I^c(y)}{A^c}\big)$, with $\omega=0.95$. The code computes $t = 1 - \text{strength}\cdot\mathrm{dc}$ with $\mathrm{dc}=\min_y\min_c I^c$ and $A=1$, so `strength` is $\omega$ and $\mathrm{dc} = \min_y\min_c I^c/A^c$ holds **only because $A=1$**. ✔ (conditional on $A=1$).
- **Lower bound** $t_0=0.1$ (eq. 22 text: *"A typical value of $t_0$ is 0.1"*). ✔ `DEHAZE_T0=0.1`.
- **Recovery** eq. (22): $J(x) = \frac{I(x)-A}{\max(t(x),t_0)} + A$. ✔ matches `dehaze_recover` `(in_range-1)/t + 1` with $A=1$ and $t$ pre-clamped to $[t_0,1]$.
- **Patch size:** He uses $15\times15$ (§4.5, *"in the remainder of this paper, we use a patch size of $15\times15$"*). Code uses `DEHAZE_PATCH=4` → $9\times9$.

So the **model, prior, transmission form, $t_0$ floor, and recovery equation are transcribed correctly.** Two deliberate simplifications and one parameter difference:

**Simplification 1 — fixed airlight $A=1$ instead of estimating $A$ (Finding H2, flagged).** He §4.3 estimates $A$ from the image: *"We first pick the top 0.1 percent brightest pixels in the dark channel … Among these pixels, the pixels with highest intensity in the input image $I$ are selected as the atmospheric light."* This per-channel $A=(A^r,A^g,A^b)$ is the **key step** that lets the method (a) handle **non-neutral / colored haze** (warm sunset haze, blue distance haze) by normalizing each channel by its own $A^c$ in eq. (7), and (b) place the true white point so recovered colors are correct.

Fixing $A=1$ (neutral white, at the working-space ceiling) means:
- **Color cast in the veil is not removed** — only a neutral gray veil is correctly inverted. Real haze is often slightly blue or warm; with $A=1$ that tint is *baked into* $J$ (the recovery $J=(I-1)/t+1$ shifts all channels by the **same** $t$, so a colored veil stays colored). The `dehaze_clears_a_synthetic_veil` test (`lib.rs:1176`) only validates the **white-airlight** case — exactly the case where $A=1$ is correct — so it does not exercise this gap.
- **Magnitude assumption:** with linear-light headroom, scene values can exceed 1 and a true diffuse-white airlight may be **brighter than 1** (a bright sky), so $A=1$ can *under*-estimate $A$, under-removing haze; or the brightest haze may be **below 1**, so $A=1$ *over*-estimates, pushing recovered darks too far down. The code's headroom passthrough (`I>1` left unchanged) is a reasonable guard but it means the densest/brightest haze region is *not* dehazed at all.
- **Upside:** it removes the fragile, content-dependent $A$ estimation (which can be fooled by white objects — the very failure He §4.3 designs around) and is deterministic/local, which suits a per-pixel GPU-friendly primitive. For a *neutral* veil it is exactly correct.

> **Impact:** Medium-High. For neutral haze the result matches He; for **colored** haze the veil's tint is not neutralized, and for scenes where true airlight $\ne 1$ the strength is mis-scaled. **Recommendation:** estimate $A$ (even a cheap global estimate: per-channel mean/percentile of the brightest dark-channel pixels) and normalize $I/A$ per channel before the dark-channel and recovery, per He eqs. (7),(12),(22). At minimum, document that dehaze assumes a neutral airlight and is only colorimetrically correct for gray veils.

**Simplification 2 — raw patch transmission, no soft-matting / guided-filter refinement (Finding H3, flagged).** He §4.2 refines $\tilde t$ with a soft-matting Laplacian (and, in their later work, the guided filter) because *"the transmission is not always constant in a patch … The main problems are some halos and block artifacts"* (§4.1, Fig. 6). The code uses the **raw patch transmission** directly. Consequence: **block / halo artifacts at depth edges**, exactly as He Fig. 6(b)/Fig. 10(c) show for the un-refined map. The per-pixel patch-min already softens this slightly (each pixel recomputes its own $9\times9$ min, so $t$ varies per pixel rather than per block), which is **better than a true block transmission** but still unrefined — transmission will be over-flat across depth discontinuities, producing the characteristic dark halo around foreground edges against bright haze.

> **Impact:** Medium. Visible as edge halos/blocking at strong settings. **Recommendation:** refine $t$ with a guided filter (He & Sun, *Guided Image Filtering*, ECCV 2010 — their own faster replacement for soft matting), using the input luminance as guide. This is the standard modern fix and is $O(N)$.

**Parameter — $9\times9$ patch vs He's $15\times15$ (Finding M2).** He §4.5 analyzes patch size: too small → oversaturated recovery (dark channel not dark enough, eq. 9 violated); too large → stronger halos. They settle on $15\times15$ for ~$500$–$600$px images. `DEHAZE_PATCH=4` ($9\times9$) is **smaller**, biasing toward He's "oversaturated" regime (the dark channel is less likely to hit a true dark pixel, so $\mathrm{dc}$ is over-estimated, $t$ under-estimated, haze *over*-removed and colors *over*-saturated). It is also a **fixed absolute size**, not scaled to image resolution — on a 24MP raw, $9\times9$ is a much smaller fraction of the frame than $15\times15$ was on He's 0.3MP test images, pushing further into the small-patch regime. **Recommendation:** scale the patch with resolution (or expose it), and consider $\ge 15\times15$-equivalent at He's reference scale to avoid over-saturation; the patch-min already partially compensates by sparing bright neutral subjects (the `dehaze_preserves_a_bright_neutral_subject` test), but the absolute size is small.

> **VERDICT (overall): Correct-with-caveats.** The scattering model, dark-channel prior, transmission, $t_0$ floor, and recovery equation are correct transcriptions of He et al. The implementation deliberately drops the two refinements that make the published method robust on real images: **airlight estimation** (fixed $A=1$) and **transmission refinement** (raw patch $t$), and uses a **smaller-than-recommended, fixed patch**. These are acceptable simplifications for a fast neutral-veil dehaze but should be documented and ideally addressed.
> **Citations:** He, Sun & Tang, TPAMI 2011 / CVPR 2009: eq. (1) model, eq. (5) dark channel, eq. (12) transmission ($\omega=0.95$), §4.3 airlight (top-0.1% dark-channel pixels), eq. (22) recovery ($t_0=0.1$), §4.2 soft matting, §4.5 patch size $15\times15$. Refinement: He & Sun, *Guided Image Filtering*, ECCV 2010.

---

### 2.6 Ordering and resolution (denoise → dehaze → clarity → sharpen, before resample)

**Code** (`apply_global`, `lib.rs:395-441`; geometry runs later in `apply_geometry`, `lib.rs:689`).

**Denoise before sharpen — correct.** Sharpening amplifies high frequencies, including noise; denoising first prevents the unsharp/clarity stages from boosting the very grain the bilateral removed. The comment at `lib.rs:399` states this rationale, and it is the universally recommended order (denoise → enhance). ✔

**Dehaze before clarity/sharpen — reasonable.** Dehaze is a tone/contrast restoration that changes the global contrast envelope; doing local-contrast (clarity) and edge sharpening *after* it operates on the already-restored signal, which is the sensible order (you sharpen what you intend to keep).

**Doing all of this at full source resolution, then resampling later (Finding M4 / Note).** Geometry (rotation, perspective, distortion, scale) resamples **after** all spatial filters. Two consequences:
- **Sharpening then downscaling:** if the geometry stage *downscales* (e.g. fitting to a smaller output, or strong perspective compression), the sharpening was tuned at source resolution and then resampled. Downscaling sharpened content can **alias the overshoots** and waste the sharpening (the high-frequency detail you boosted is partly thrown away or moiré'd by the bilinear resampler, which has no prefilter — see Audit on geometry). The textbook order for output sharpening is **resample first, then sharpen at output resolution** ("output sharpening" / capture-vs-output sharpening, the standard raw-workflow distinction). Running sharpen at source res is "capture sharpening," which is defensible, but if the output is significantly smaller it is suboptimal.
- **Upscaling:** if geometry upscales, sharpening at source res is fine or even preferable (sharpen the real detail before interpolation invents none).
- **Filter radii are in source pixels:** a clarity/denoise radius chosen at source resolution will not correspond to the same *output* feature size after a scale change. For a fixed-pipeline developer this is usually acceptable (the user tunes against the source), but it means "radius" is resolution-dependent.

> **VERDICT: Correct (ordering) / Note (resolution).** The intra-stage order is correct (denoise first, sharpen last). Performing spatial filters at source resolution before geometry is **capture-sharpening** semantics — correct when geometry preserves or increases scale, but suboptimal when geometry **downscales** (overshoots get resampled by an unprefiltered bilinear sampler, risking aliasing and wasted sharpening). Worth a design note; not a correctness bug for the upscale/no-scale common case.
> **Citation:** Standard raw-workflow capture-vs-output sharpening (e.g. Bruce Fraser, *Real World Image Sharpening*); NVIDIA GPU Gems 3 Ch. 24 (filtering correctness under resampling).

---

## 3. Findings by Severity

| ID | Sev | File:line | Issue | Reference | Recommendation |
|----|-----|-----------|-------|-----------|----------------|
| **H1** | High | `latent-pipeline/src/lib.rs:528-529` | Bilateral **spatial Gaussian truncated at $\pm2\sigma$** ($\sigma_s=r/2$, window $\pm r$). Edge weight $e^{-2}=0.135$ (hard step); ~4.6%/axis of Gaussian mass dropped; effective support narrower than the kernel implies. | Standard $3\sigma$ truncation (SciPy `truncate=4.0`; kernel $2\lceil3\sigma\rceil+1$). | Use $\sigma_s=r/3$ (window $=\pm3\sigma$) **or** keep $\sigma_s=r/2$ and widen window to $\pm\lceil1.5r\rceil$. |
| **H2** | High | `latent-pipeline/src/lib.rs:493-505` | **Dehaze fixes airlight $A=1$** instead of estimating per-channel $A$. Colored haze is not neutralized; strength mis-scaled when true airlight $\ne1$. Only correct for a neutral, $\le1$ veil. | He et al. TPAMI 2011, §4.3 (top-0.1% dark-channel pixels → highest-$I$ → $A$); eqs. (7),(12),(22) use $I/A$. | Estimate $A$ (≥ a cheap global per-channel percentile of brightest dark-channel pixels) and normalize $I/A^c$ per channel; or document the neutral-veil limitation. |
| **H3** | High | `latent-pipeline/src/lib.rs:475-487` | **No transmission refinement** — raw per-pixel patch $t$ → block/halo artifacts at depth edges. | He et al. §4.2 (soft matting) / He & Sun, *Guided Image Filtering*, ECCV 2010. | Refine $t$ with an $O(N)$ guided filter (guide = input luma) before recovery. |
| **M1** | Medium | `latent-pipeline/src/lib.rs:525-567` | Denoise is a **luma/chroma split with two separate range weights**, not T&M's single combined-distance (CIE-Lab) color filter; chroma distance is in a **non-perceptual** linear-RGB-offset space. | Tomasi & Manduchi ICCV 1998 §5 (combine bands, CIE-Lab; per-channel filtering is worse). | Keep the split (standard NR variant, intentional luma≠chroma strength), but document it as a luma/chroma variant; consider a perceptual chroma metric. |
| **M2** | Medium | `latent-pipeline/src/lib.rs:468` | Dehaze patch **$9\times9$** (`DEHAZE_PATCH=4`), **fixed absolute size**, vs He's $15\times15$; biases toward the small-patch *over-saturation* regime, worse on high-MP rasters. | He et al. §4.5 (patch-size analysis; $15\times15$ at ~0.3–0.4 MP). | Scale patch with resolution (or expose it); target ≥ He's $15\times15$-equivalent at reference scale. |
| **M3** | Medium | `lib.rs:526`, `latent-cpu/src/lib.rs:68,163` | Radius/round **gate predicates differ**: `blur` gates `radius>0` then `round` (radius 0.3 → silent identity); denoise gates on `round>=1`; dehaze ignores radius entirely (fixed patch). | — (internal consistency) | Unify radius semantics/threshold across blur/denoise/dehaze; tie dehaze patch to a radius. |
| **M4** | Medium | `latent-pipeline/src/lib.rs:355-357` | Spatial filters run at **source resolution before geometry**; if geometry **downscales**, sharpening overshoots are resampled by an **unprefiltered bilinear** sampler → aliasing/wasted sharpening. | Capture-vs-output sharpening (Fraser); GPU Gems 3 Ch. 24. | Consider an output-sharpening pass after resample, or prefilter on downscale. (Fine for no-scale/upscale.) |
| **L1** | Low | `latent-image/src/color.rs:181`; `lib.rs:535-536` | ProPhoto-D65 blue luma weight $\approx10^{-4}$, so blue's **luminance detail** is governed by the **chroma** scale, not luma, in the denoise split. | (consequence of wide-gamut luminance) | Document; consider clamping blue weight or a luma floor if blue detail is over-smoothed. |
| **N1** | Note | `latent-pipeline/src/lib.rs:438-440` | Sharpening/clarity in **linear light**: physically correct for blur, but unsharp **overshoot is asymmetric** perceptually (brighter dark-side halos) vs gamma/luma-domain sharpeners; sharpens all RGB (potential edge color fringing) rather than luma-only. | NVIDIA GPU Gems 3 Ch. 24; Wikipedia/Imatest (overshoot→halos). | Acceptable given linear-light philosophy; document. Optionally sharpen luma only. |
| **N2** | Note | `latent-pipeline/src/lib.rs:525-567` | Bilateral is exact and non-separable → $O(r^2)$/pixel; no fast-bilateral approximation. | Tomasi & Manduchi (non-separable). | Fine as reference; consider bilateral-grid/permutohedral for large radii. |

**No Critical findings.** Every formula transcribed (unsharp, box, midtone parabola, bilateral product-of-Gaussians, dehaze model/transmission/recovery) matches its authoritative source; the issues are deliberate simplifications and support/parameter choices, not transcription errors.

---

## 4. Verdict Summary

| Filter | Verdict |
|---|---|
| Box blur (separable, clamp border) | **Correct** |
| Unsharp mask (`gain=1+amount`) | **Correct** (linear-domain caveat N1) |
| Clarity: 3-box Gaussian approx ($\sigma\approx r$) | **Correct** |
| Clarity: midtone parabola (peak at linear 0.2176) | **Correct** |
| Bilateral denoise (core math) | **Correct** |
| Bilateral: luma/chroma split + $2\sigma$ support | **Correct-with-caveats** (M1, H1) |
| Dehaze: model / DCP / transmission / $t_0$ / recovery | **Correct** |
| Dehaze: fixed $A=1$, no $t$-refinement, $9\times9$ patch | **Correct-with-caveats** (H2, H3, M2) |
| Ordering (denoise→dehaze→clarity→sharpen) | **Correct** |
| Full-res then resample | **Note** (M4) |

---

## 5. References

1. C. Tomasi, R. Manduchi, **Bilateral Filtering for Gray and Color Images**, *Proc. IEEE ICCV 1998*, pp. 839–846. Local copy: `docs/spatial-bilateral-tomasi-1998.pdf`. <https://users.soe.ucsc.edu/~manduchi/Papers/ICCV98.pdf>
2. K. He, J. Sun, X. Tang, **Single Image Haze Removal Using Dark Channel Prior**, *IEEE TPAMI 33(12):2341–2353, 2011* (extended version of *CVPR 2009*, pp. 1956–1963). Local copy: `docs/spatial-dehaze-he-2011-tpami.pdf`. <http://mmlab.ie.cuhk.edu.hk/2011/Haze.pdf>
3. IPOL, **Dehazing with Dark Channel Prior** (peer-reviewed reproduction), *IPOL 2024*. Local copy: `docs/spatial-dehaze-ipol-2024.pdf`. <http://www.ipol.im/pub/art/2024/530/>
4. K. He, J. Sun, X. Tang, **Guided Image Filtering**, *ECCV 2010* (authors' $O(N)$ replacement for soft matting in transmission refinement). <https://kaiminghe.github.io/eccv10/>
5. W. M. Wells, **Efficient Synthesis of Gaussian Filters by Cascaded Uniform Filters**, *IEEE TPAMI 8(2):234–239, 1986* (box-cascade ≈ Gaussian; 3–4 passes).
6. P. Getreuer, **A Survey of Gaussian Convolution Algorithms**, *IPOL 2013* (box-blur CLT / repeated-box Gaussian). <https://www.ipol.im/pub/art/2013/87/>
7. Wikipedia, **Unsharp masking** — `sharpened = original + (original − blurred) × amount`; overshoot/halo. <https://en.wikipedia.org/wiki/Unsharp_masking>
8. Wikipedia, **Box blur** — separability, CLT Gaussian approximation, border-fill modes. <https://en.wikipedia.org/wiki/Box_blur>
9. Imatest, **Sharpening** — USM equation $L_{\text{USM}}=L-k_{\text{USM}}\,\text{Blur}$; "small overshoots enhance sharpness; large overshoots cause halos." <https://www.imatest.com/imaging/sharpening/>
10. L. Gritz, E. d'Eon, **The Importance of Being Linear**, *GPU Gems 3, Ch. 24*, NVIDIA — filtering/blending/mip must be in linear light. <https://developer.nvidia.com/gpugems/gpugems3/part-iv-image-effects/chapter-24-importance-being-linear>
11. SciPy, **`scipy.ndimage.gaussian_filter`** — `truncate=4.0` default (radius $\approx 4\sigma$; $3\sigma$ minimum convention). <https://docs.scipy.org/doc/scipy/reference/generated/scipy.ndimage.gaussian_filter.html>
12. B. Fraser, J. Schewe, **Real World Image Sharpening with Adobe Photoshop, Camera Raw, and Lightroom** — capture vs. output sharpening (sharpen at output resolution).

---

### Appendix A — Numeric evidence (reproducible, `python3`, stdlib `math` only)

**3-box effective $\sigma$** ($\sigma_{3\text{box}}=\sqrt{r(r+1)}\to r$): `r=3 → 3.464`, `r=10 → 10.488`, `r=50 → 50.498`.

**3-box vs true Gaussian** (kernel $L_1$ error): $K{=}2{:}\,0.094$, $K{=}3{:}\,0.051$, $K{=}4{:}\,0.038$; excess kurtosis $K{=}1{:}{-}1.2 \to K{=}3{:}{-}0.4$. Three is the cost/quality knee.

**Midtone parabola peak:** $m$ peaks (=1) at $b{=}0.5 \Rightarrow$ linear luma $=0.5^{2.2}=0.2176$; $m(0.18)=0.993$, $m(0.5)=0.789$; half-max at linear $0.015$ and $0.706$. ($L^*{=}50$ mid-gray $=0.184$.)

**Bilateral $2\sigma$ truncation:** spatial weight at window edge $=e^{-2}=0.135$; 1-D Gaussian mass beyond $\pm2\sigma=4.6\%$, beyond $\pm3\sigma=0.27\%$.

**Unsharp algebra:** $L+(1+a)(I-L)\equiv I+a(I-L)$, verified to $<10^{-9}$ on random inputs.
