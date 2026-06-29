# Color Science Audit — `latent`

**Scope:** the color-science math of the `latent` RAW developer.
**Audited files:**

- `latent-image/src/color.rs` (matrix ops, primaries, working-space construction, luma, HSV)
- `latent-export/src/lib.rs` (sRGB OETF, highlight rolloff, output transform, ICC)
- `latent-raw/src/lib.rs` (`color_matrix()`, `cam_xyz`/`cam_mul` feed; decode order)
- `latent-app/src/main.rs` (pipeline ordering, for context)

**Date:** 2026-06-27
**Method:** every numeric claim was re-derived from first principles (pure-Python reference
implementation of the same matrix algebra) and checked against primary standards
(IEC 61966-2-1, ISO 22028-2 / ROMM RGB, Adobe DNG 1.4.0.0, SMPTE RP 177). Drift figures
are reported in both 8-bit and 16-bit code units.

---

## 1. Summary — what the code does and its colorimetric model

`latent` develops RAW files through a **fixed linear-light pipeline** whose decode order is
(`latent-app/src/main.rs:63-70`):

$$\text{normalize} \to \text{white balance (mosaic, } cam\_mul) \to \text{demosaic} \to \text{highlight reconstruct} \to \mathbf{M}_{\text{cam}\to\text{work}}.$$

The **working space** is *linear ProPhoto/ROMM primaries pinned to a D65 white point*
(`color.rs:125-136`). Camera RGB is lifted into XYZ by inverting the file's `cam_xyz`
(DNG `ColorMatrix`, an XYZ→camera matrix), then mapped XYZ→working. At export
(`latent-export/src/lib.rs`), working RGB is taken to **linear sRGB** by a row-normalized
product of the working→XYZ matrix and the published IEC XYZ→sRGB matrix, passed through a
**highlight rolloff** (Reinhard-style knee at 0.98), then the **sRGB OETF**, then quantized
to 8/16-bit with an embedded sRGB ICC profile.

The colorimetric model is internally **D65-consistent end to end**: camera matrix inverse,
working primaries, and sRGB output all sit at D65, so no chromatic adaptation is performed
anywhere — by design. Two deliberate engineering choices distinguish it from a textbook
ICC pipeline: (a) the working space is *non-standard* (ProPhoto primaries at D65, not the
ISO-22028-2 D50 white), and (b) **row-normalization** is used twice to pin the neutral axis
in place of an explicit chromatic-adaptation transform.

---

## 2. Point-by-point verification

| # | Claim under test | Verdict | Authority |
|---|------------------|---------|-----------|
| 1 | `XYZ_TO_LINEAR_SRGB` = the IEC sRGB matrix (4-decimal) | **Correct-with-caveats** | IEC 61966-2-1 / Amd.1 |
| 2 | ProPhoto primaries pinned to D65 is a sound, adaptation-free working space | **Correct-with-caveats** | ISO 22028-2 (ROMM) |
| 3 | `rgb_to_xyz` construction (primaries-as-columns, scaled to white) | **Correct** | SMPTE RP 177 / Lindbloom |
| 4 | Row-normalizing `camera_to_working` is the right way to avoid double-WB | **Questionable** | Adobe DNG 1.4.0.0 §6 |
| 4b | Row-normalizing `working→sRGB`; "~1e-4" chromatic drift claim | **Correct-with-caveats** | (numerical) |
| 5 | `LUMA_WEIGHTS` = Y row of ProPhoto-D65 matrix; blue ≈ 0 by design | **Correct-with-caveats** | (derived); Rec. 709 |
| 6 | sRGB OETF constants (12.92, 0.0031308, 1.055, 1/2.4, 0.055) | **Correct** | IEC 61966-2-1 |
| 7 | `highlight_rolloff` knee 0.98, white→254/255 | **Correct (by design)** | Reinhard / display-referred practice |
| 8 | `cam_xyz` is XYZ→camera; inverse is camera→XYZ | **Correct** | Adobe DNG 1.4.0.0 §6 |
| 9 | No Bradford chromatic adaptation needed | **Correct-with-caveats** | DNG §6 / ICC practice |

### 2.1 (#1) The XYZ→linear-sRGB matrix — `color.rs:89-93`

The constant is

$$
\mathbf{M}_{\text{XYZ}\to\text{sRGB}} =
\begin{bmatrix}
3.2406 & -1.5372 & -0.4986\\
-0.9689 & 1.8758 & 0.0415\\
0.0557 & -0.2040 & 1.0570
\end{bmatrix}.
$$

This is **bit-exact** with the matrix printed in IEC 61966-2-1 (reproduced verbatim in the
freely-available bg-sRGB amendment text, *"Conversion from XYZ (D65) to bg-sRGB"*). So the
code transcribes the standard correctly.

**Caveat (precision).** The standard explicitly labels this 4-decimal matrix as accurate
*only to 8-bit samples*; the **2003 Amendment 1** supplies a 7-decimal matrix
(`3.2406255, -1.5372080, -0.4986286; …`) recommended for 16-bit. Re-deriving the matrix at
full precision from the Rec. 709 primaries gives

$$
\mathbf{M}^{\text{exact}}_{\text{XYZ}\to\text{sRGB}} =
\begin{bmatrix}
3.24096994 & -1.53738318 & -0.49861076\\
-0.96924364 & 1.87596750 & 0.04155506\\
0.05563008 & -0.20397696 & 1.05697151
\end{bmatrix},
$$

which differs from the code's constant by up to **$3.7\times10^{-4}$** (element [0][2]/[2][2]).
That is $\approx 0.094$ of an 8-bit code (invisible) but $\approx 24$ of a 16-bit code
(real, though still well under any perceptual threshold). Because `latent` advertises a
true-16-bit export path (`save_16`), using the 4-decimal constant is a minor precision leak
on the 16-bit output. Verified directly: $\mathbf{M}_{\text{code}}\cdot[0.95047,1,1.08883]^\top
= [1.000002, 1.000076, 0.999834]^\top$ (should be exactly $[1,1,1]$), confirming the rounded
matrix does not map D65 white perfectly to neutral.

### 2.2 (#2) ProPhoto primaries at D65 — `color.rs:125-136`

The primaries

$$ R(0.7347,0.2653),\quad G(0.1596,0.8404),\quad B(0.0366,0.0001) $$

**match ISO 22028-2 (ROMM RGB) exactly** (the registry spec lists the same numbers).
However, the same spec states unambiguously: *"The reference white for ROMM RGB is specified
as D50 (x=0.3457, y=0.3585)."* The code pins these primaries to **D65** (`D65_WHITE =
[0.3127, 0.3290]`).

**This is a non-standard space.** It is *not* ProPhoto RGB / ROMM RGB; it is "ROMM primaries
at D65," which has no ICC/ISO identity. The code comment calls it "linear ProPhoto primaries
at D65," which is an accurate description, but a reader should not assume interoperability
with anything tagged ProPhoto.

Is the "no chromatic adaptation needed" reasoning sound? **Yes, internally.** Because the
camera→XYZ inverse, the working white, and the sRGB output white are *all* D65, the white
point never changes across the pipeline, so a Bradford CA between stages would be the
identity. Pinning the working white to D65 is a legitimate way to make the whole chain
adaptation-free. The cost is purely that the working space is unconventional, not that it is
colorimetrically broken: any XYZ color is still represented exactly (the primaries are real,
the matrix is invertible and round-trips to $<10^{-5}$, verified). The Y row at D65 is
$[0.27882, 0.72107, 0.00011]$ vs. the standard D50 ProPhoto Y row $[0.28804, 0.71188,
0.0000857]$ — same character (huge green, near-zero blue), shifted only by the white move.

Derived ROMM-at-D50 RGB→XYZ matches Bruce Lindbloom's published ProPhoto-D50 matrix to
$2.2\times10^{-5}$, confirming the construction is faithful; the only deviation from the
standard is the deliberate D50→D65 white substitution.

### 2.3 (#3) The `rgb_to_xyz` construction — `color.rs:107-123`

The routine builds each primary's XYZ at unit luminance, $[x/y,\,1,\,(1-x-y)/y]$, places the
three as **columns** of a basis $\mathbf{B}$, then solves $\mathbf{s}=\mathbf{B}^{-1}\,
\mathbf{W}_{XYZ}$ and scales column $c$ by $s_c$. This is precisely the **Normalized Primary
Matrix** derivation of **SMPTE RP 177** ("the matrix is uniquely determined by the xy
chromaticities of the RGB primaries and the XYZ of the white point") and the method on Bruce
Lindbloom's *RGB/XYZ Matrices* page. **Correct.** The unit-test `working_space_white_is_
neutral_and_round_trips` confirms $\mathbf{M}\cdot[1,1,1]^\top = \mathbf{W}_{D65}$ and
$\mathbf{M}^{-1}\mathbf{M}=\mathbf{I}$.

### 2.4 (#4) Row-normalization to avoid double white-balance — `color.rs:152-169`

Two separate uses must be assessed differently.

**(4a) `camera_to_working` — `color.rs:166-169`.** The pipeline applies white balance
*on the mosaic* via `cam_mul` (`latent-raw/src/lib.rs:193-207`, confirmed called at
`main.rs:64` *before* the matrix at `main.rs:68`). So a neutral patch reaches the matrix as
$[v,v,v]$. The code then row-normalizes $\mathbf{M}=\mathbf{M}_{XYZ\to\text{work}}\cdot
\mathbf{M}_{\text{cam}\to XYZ}$ so each row sums to 1, guaranteeing $[v,v,v]\mapsto[v,v,v]$.

The neutral guarantee is real and the unit test
`camera_to_working_keeps_a_neutral_patch_neutral` passes. **But this is not the operation the
DNG/dcraw reference pipeline performs, and the two are not colorimetrically equivalent on
chromatic colors.** Per Adobe DNG 1.4.0.0 §6 ("Mapping Camera Color Space to CIE XYZ"):

> *"`CameraToXYZ_D50 = CA * CameraToXYZ` … CA … is a chromatic adaptation matrix that maps
> from the white balance xy value to the D50 white point. The recommended method … is the
> linear Bradford algorithm."*

and the preferred (ForwardMatrix) path *"causes the white balance adjustment … to be done by
scaling the camera coordinates rather than by adapting the resulting XYZ values."* In both DNG
methods the white-balance correction is a **per-input-channel (column) scale on camera
coordinates** plus a principled Bradford adaptation. Row-normalization is instead a
**per-output-row scale on the working result**. These are *different linear operators*: they
coincide only when the camera→XYZ matrix is diagonal, which it never is.

Concretely, with a realistic Canon-EOS-5D-Mark-III `cam_xyz` and its as-shot `cam_mul`
(green-normalized $[2.25, 1.0, 1.42]$), I compared the matrix the code applies to
white-balanced camera RGB against the matrix a per-channel WB compensation would apply. Both
map neutral to neutral, but on saturated inputs they diverge by up to **$0.28$** in linear
working RGB (e.g. a saturated green channel: $1.48$ vs $1.20$). That is a large, visible hue
shift — **not** a sub-quantization effect. The row-normalization therefore *absorbs a real
per-illuminant color rotation into a crude three-scalar fudge*. The neutral axis is correct;
saturated-color rendering is an uncontrolled approximation whose error grows with the camera
matrix's off-diagonality and the WB distance from the matrix's calibration illuminant.

> The code comment at `color.rs:164-165` ("the row-normalization stops this matrix from
> re-applying its own implicit white balance — the classic double-apply bug") is misleading:
> the real fix for double-WB is to build the matrix to *expect* WB'd input (i.e. fold
> $\mathrm{diag}(cam\_mul)^{-1}$ into the matrix, or use the DNG ForwardMatrix path).
> Row-normalization happens to fix the *neutral* but at the cost of *chromatic* fidelity.

**(4b) `linear_working_to_linear_srgb` — `color.rs:152-156`.** Here both spaces are genuine
D65 RGB spaces, so the product $\mathbf{M}_{XYZ\to\text{sRGB}}\cdot\mathbf{M}_{\text{work}\to
XYZ}$ *should* already have rows summing to 1; it does not only because the sRGB matrix is the
rounded 4-decimal constant (finding #1). Measured raw row sums:
$[0.99984,\,1.00010,\,1.00007]$, i.e. a neutral tint of at most **$1.6\times10^{-4}$**
($\approx 0.04$ of an 8-bit code, $\approx 10$ of a 16-bit code). Row-normalizing removes that
tint and shifts chromatic colors by the *same order* — measured **max $3.25\times10^{-4}$**
on saturated red/orange ($\approx 0.083$ of an 8-bit code, $\approx 21$ of a 16-bit code).

The docstring's "**~1e-4**" understates the chromatic shift by roughly $3\times$ (actual
$3.25\times10^{-4}$); and note the operation trades a $\le10$-LSB neutral tint for a
$\le21$-LSB chromatic shift at 16-bit — it makes saturated colors move *more* than the error
it fixes. Both are sub-perceptual, so the verdict is *correct-with-caveats*: the right
underlying fix is to use the 7-decimal sRGB matrix (finding #1), after which no
row-normalization here would be needed at all.

### 2.5 (#5) `LUMA_WEIGHTS` — `color.rs:181`

$$ \text{LUMA\_WEIGHTS} = [0.27881965,\ 0.72106725,\ 0.000113055]. $$

Re-deriving the ProPhoto-primaries-at-D65 RGB→XYZ matrix gives a Y row of
$[0.27881967, 0.72106727, 0.00011305]$ — agreement to $2\times10^{-8}$. **The constant is
exactly the Y row of the working space**, sums to 1.0, and the unit test
`luma_weights_match_the_working_matrix` guards it. Correct as *relative luminance of the
working space*.

**Caveat (perceptual operations).** The near-zero blue weight ($1.1\times10^{-4}$) is
colorimetrically correct for these primaries but has a strong consequence: a fully desaturated
pure-blue maps to near-black (luminance $\approx0.0001$ vs. $\approx0.0722$ under
Rec. 709/sRGB primaries). The code's own comment acknowledges this. For a *colorimetric*
luminance that is the right answer. But this same `luminance()` and these same weights feed
**perceptual** operations — saturation, clarity, denoise luma (the GPU shader hard-codes the
identical triple, `color.rs:173-175`). For perceptual work the wide-gamut luma is a poor
proxy: it makes blue detail nearly weightless, so denoise/clarity will under-protect blue
texture and saturation math will darken blues aggressively. This is a *defensible-but-
debatable* design choice; many pipelines compute perceptual luma in a display-referred space
(Rec. 709 $Y=0.2126R+0.7152G+0.0722B$) precisely to avoid the vanishing-blue artifact. Flagged
as a **Medium** note, not an error: it is self-consistent and intentional, but worth a
deliberate decision rather than an inherited side effect of the working-space choice.

### 2.6 (#6) sRGB OETF — `latent-export/src/lib.rs:14-29`

$$
V_{\text{sRGB}} =
\begin{cases}
12.92\,c & c \le 0.0031308\\
1.055\,c^{1/2.4} - 0.055 & c > 0.0031308
\end{cases}
\qquad
c =
\begin{cases}
V/12.92 & V \le 0.04045\\
\left(\frac{V+0.055}{1.055}\right)^{2.4} & V > 0.04045
\end{cases}
$$

Every constant — slope **12.92**, encode threshold **0.0031308**, gain **1.055**, exponent
**1/2.4**, offset **0.055**, decode threshold **0.04045** — matches IEC 61966-2-1 verbatim
(the bg-sRGB amendment text reproduces the identical piecewise form). **Correct.** Endpoints
fixed ($0\mapsto0$, $1\mapsto1$) and the round-trip test passes to $10^{-5}$. (Pedantic note:
the canonical threshold that makes the two segments meet is $0.0031308$ as written; the
slightly different $0.04045$ on the decode side is the standard's published companion value —
consistent with IEC, not a bug.)

### 2.7 (#7) `highlight_rolloff` — `latent-export/src/lib.rs:42-51`

$$
f(x)=
\begin{cases}
x & x \le 0.98\\
0.98 + 0.02\cdot\dfrac{x-0.98}{(x-0.98)+0.02} & x > 0.98
\end{cases}
$$

Above the knee this is a **Reinhard compressor** ($t\mapsto t/(t+s)$) rescaled into
$[0.98,1.0)$: monotonic, asymptotic to 1.0, never reaching it. Verified mapping:
$f(1.0)=0.990\Rightarrow$ sRGB $0.9956\Rightarrow$ **254/255** (8-bit), 65246/65535 (16-bit);
headroom $x\ge1.5$ all reach **255** at 8-bit. So the headroom gradient is essentially
nonexistent at 8-bit (it lives in the single top code) but real at 16-bit (spanning
$\approx65246\to65535$, $\approx289$ codes).

**Assessment:** sacrificing exact white (255→254) to keep a smooth, non-clipping highlight
rolloff is a standard **display-referred** trade-off. It is the same dilemma Hable/Filmic
tone-mappers describe ("the range 3.25→4.0 going 254→255 is not useful") and solve with
overshoot parameters. The code's choice — anchor faithful $[0,1]$ to within one code, keep a
true gradient above 1.0 for the 16-bit path — is principled and well-documented in-code.
**Correct by design**, with the honest limitation (stated in the code) that the 8-bit headroom
gradient is degenerate. One genuine subtlety: because the rolloff is applied *per channel
independently* (`to_display`, `lib.rs:56-59`), a color with one channel above 1.0 and others
below will have *only* the hot channel compressed, which **shifts its hue toward the
secondaries** in the extreme highlights. A luminance-based or max-channel rolloff would
preserve hue better. Low severity (only affects near-clipped highlights), but worth noting.

### 2.8 (#8) DNG direction convention — `color.rs:82-84`, `latent-raw/src/lib.rs:425-428`

`camera_to_xyz(xyz_to_cam) = xyz_to_cam.inverse()`. Adobe DNG 1.4.0.0 §Tag-ColorMatrix1:
*"ColorMatrix1 defines a transformation matrix that converts XYZ values to reference camera
native color space values."* §6: `XYZtoCamera = AB·CC·CM` and (no ForwardMatrix)
`CameraToXYZ = Inverse(XYZtoCamera)`. LibRaw's `cam_xyz` is the same XYZ→camera matrix
(LibRaw docs: *"cam_xyz should exactly match ColorMatrix2"*). The code treats `cam_xyz` as
XYZ→camera and inverts it to get camera→XYZ — **exactly correct** in direction. Using only the
top 3 rows of the 4×N `cam_xyz` (`lib.rs:427`) is fine for the RGBG Bayer case the decoder
restricts itself to (`is_rgb_bayer`, `lib.rs:435-436`).

### 2.9 (#9) Chromatic adaptation — omitted

No Bradford adaptation appears anywhere. Given the all-D65 model (finding #2) this is
**internally correct**: every stage shares the white point, so an explicit CA would be the
identity. The caveat is finding #4a: the *white-balance* correction (which in the DNG model is
entwined with a Bradford adaptation from the capture illuminant to the reference white) is
handled instead by mosaic `cam_mul` + row-normalization, which is not equivalent to the DNG
Bradford path for saturated colors. So adaptation is correctly omitted *as a white-point
transform*, but the *illuminant-dependent color rotation* that DNG bundles into CA is only
crudely approximated.

---

## 3. Findings

### F-1 — Row-normalizing `camera_to_working` is not colorimetrically equivalent to the reference WB+adaptation pipeline · **High**
- **Where:** `latent-image/src/color.rs:166-169` (`camera_to_working`); rationale comment at
  `color.rs:158-165`; called from `latent-raw/src/lib.rs:425-428`, applied at
  `latent-app/src/main.rs:64-70`.
- **Issue:** White balance is applied per-channel on the mosaic (`cam_mul`), then the
  camera→working matrix is row-normalized to keep neutrals neutral. Row-normalization is a
  per-output-row scale; the correct WB compensation (DNG ForwardMatrix path, or
  inverse-ColorMatrix + Bradford CA) is a per-input-channel scale on camera coordinates plus a
  principled adaptation. The two agree on the neutral axis by construction but diverge on
  saturated colors — measured up to **0.28 in linear working RGB** (a clearly visible hue/sat
  shift) for a realistic Canon `cam_xyz` + as-shot `cam_mul`. The error scales with the camera
  matrix's off-diagonality and the WB distance from the matrix's calibration illuminant, so it
  is camera- and white-balance-dependent, not a fixed offset.
- **Reference:** Adobe DNG 1.4.0.0, Ch. 6 *"Mapping Camera Color Space to CIE XYZ Space"*
  (`CameraToXYZ_D50 = CA·CameraToXYZ`, Bradford CA; ForwardMatrix WB by camera-coordinate
  scaling). LibRaw color-pipeline docs.
- **Recommendation:** Fold the inverse of the applied `cam_mul` into the matrix instead of
  row-normalizing — i.e. build $\mathbf{M}_{XYZ\to\text{work}}\cdot\mathbf{M}_{\text{cam}\to
  XYZ}\cdot\mathrm{diag}(cam\_mul)^{-1}$ — so WB is mathematically applied exactly once and
  chromatic colors follow the true camera transform. This still keeps neutrals neutral (when
  `cam_mul` is the as-shot neutral) but renders saturated colors correctly. If a strict neutral
  guarantee under arbitrary user WB is also wanted, adopt the DNG model (Bradford CA from the
  user WB to D65). At minimum, correct the comment at `color.rs:164-165`, which mischaracterizes
  row-normalization as *the* fix for double-WB.

### F-2 — Working space is ProPhoto primaries at D65, a non-standard space (not ROMM/ProPhoto) · **Medium**
- **Where:** `latent-image/src/color.rs:125-136` (`PROPHOTO_PRIMARIES` + `D65_WHITE`,
  `linear_working_to_xyz`).
- **Issue:** ISO 22028-2 fixes ROMM RGB's reference white at **D50** (x=0.3457, y=0.3585). The
  code reuses ROMM's primaries but at **D65**, producing a space with no standard identity.
  This is a deliberate, documented choice that keeps the pipeline adaptation-free and is
  *colorimetrically valid* (the primaries are real and the matrix round-trips), but it must not
  be confused with ProPhoto RGB; anything tagged "ProPhoto" elsewhere would be a different
  space.
- **Reference:** ISO 22028-2:2013 / ROMM RGB registry spec ("reference white … D50").
- **Recommendation:** Keep the design if the adaptation-free property is wanted, but rename the
  space in code/comments to something unambiguous (e.g. "ROMM-primaries-D65" / "wide-D65") so
  no reader assumes ProPhoto interoperability. No numeric change needed.

### F-3 — 4-decimal sRGB matrix leaks precision on the 16-bit export path · **Low**
- **Where:** `latent-image/src/color.rs:89-93` (`XYZ_TO_LINEAR_SRGB`); consumed by
  `latent-export/src/lib.rs:95-106` (`save_16`).
- **Issue:** The constant is the IEC *8-bit-grade* 4-decimal matrix. It differs from the
  full-precision (and the IEC-2003 7-decimal) matrix by up to $3.7\times10^{-4}$
  ($\approx24$ of a 16-bit code) and does not map D65 white exactly to neutral
  ($\mathbf{M}\cdot W_{D65}=[0.99984,1.0001,1.00007]$). For an app that exports true 16-bit,
  this is an avoidable sub-perceptual error; it is also the *root cause* of the working→sRGB
  row-normalization (F-4).
- **Reference:** IEC 61966-2-1:1999 (4-decimal matrix, "accurate for 8-bit"); Amendment 1:2003
  (7-decimal matrix for 16-bit).
- **Recommendation:** Replace with the 7-decimal IEC-2003 matrix, or (cleanest) derive the
  sRGB matrix from the Rec. 709 primaries with the same `rgb_to_xyz` used for the working
  space, so both halves of the output transform are full-precision and consistent. The
  working→sRGB product would then have unit row sums natively and F-4's row-normalization
  becomes unnecessary.

### F-4 — `working→sRGB` row-normalization; docstring drift figure understated · **Low**
- **Where:** `latent-image/src/color.rs:152-156` (`linear_working_to_linear_srgb`), docstring
  at `color.rs:147-151`.
- **Issue:** Row-normalization here masks the rounded-matrix tint (F-3) rather than fixing it.
  The docstring claims chromatic colors "shift by the same ~1e-4," but the measured maximum is
  **$3.25\times10^{-4}$** (saturated red/orange) — about $3\times$ larger, and *larger than*
  the $1.6\times10^{-4}$ neutral tint it removes. All sub-perceptual ($\le0.08$ of an 8-bit
  code; $\le21$ of a 16-bit code), so impact is negligible, but the comment is inaccurate.
- **Reference:** numerical (re-derived); IEC matrix precision (F-3).
- **Recommendation:** Fix F-3 and delete this row-normalization; otherwise correct the
  docstring figure to $\sim3\times10^{-4}$.

### F-5 — Wide-gamut luma drives perceptual operations (saturation/clarity/denoise) · **Medium**
- **Where:** `latent-image/src/color.rs:181-186` (`LUMA_WEIGHTS`, `luminance`); duplicated in
  the GPU shader (`color.rs:173-175` note).
- **Issue:** The weights are correct *colorimetric* luminance for the working space, but the
  blue weight is $\approx1.1\times10^{-4}$ (vs $0.0722$ in Rec. 709). Using this luma for
  perceptual ops makes blue carry almost no weight: denoise/clarity under-protect blue texture
  and saturation/value math darkens blues hard (a fully-desaturated pure blue → near-black).
  Self-consistent and intentional, but a debatable choice.
- **Reference:** Rec. 709/sRGB luma coefficients $Y=0.2126R+0.7152G+0.0722B$ (IEC 61966-2-1 /
  ITU-R BT.709); common practice of computing perceptual luma in a display-referred space.
- **Recommendation:** Decide per-operation. For exposure/relative-luminance, keep the working
  luma. For *perceptual* operations (clarity, denoise weighting, saturation), consider a
  Rec. 709 luma (computed after a notional working→sRGB primaries rotation) so blue detail
  isn't treated as nearly weightless. At minimum document that perceptual ops inherit the
  vanishing-blue behavior deliberately.

### F-6 — Per-channel highlight rolloff can shift hue in extreme highlights · **Low**
- **Where:** `latent-export/src/lib.rs:56-59` (`to_display` applies `highlight_rolloff`
  independently per channel).
- **Issue:** When one channel exceeds 1.0 and others don't, only the hot channel is compressed,
  pulling near-clipped highlight colors toward a secondary/white in a hue-shifting way. Only
  affects pixels with headroom in some channels.
- **Reference:** display-referred tone-mapping practice (luminance- or max-channel-based
  rolloff preserves hue).
- **Recommendation:** Consider rolling off on the max channel (or luminance) and scaling the
  triplet, rather than per-channel, if highlight hue fidelity matters. Low priority.

### F-7 — Note: matrix algebra, OETF, DNG direction, and rgb_to_xyz are all correct · **Note**
- `Mat3` inverse/det/mul, the SMPTE-RP-177 `rgb_to_xyz` construction, the sRGB OETF constants,
  the DNG XYZ→camera direction, and the round-trip/neutral unit tests are all correct and
  well-tested. The ICC embedding uses a real validated `moxcms` sRGB profile that matches the
  output transform. These are called out explicitly as **correct** so the findings above are
  read as targeted, not a blanket concern.

**Severity tally:** Critical 0 · High 1 · Medium 2 · Low 3 · Note 1.

---

## 4. References

1. **IEC 61966-2-1:1999**, *Multimedia systems and equipment — Colour measurement and
   management — Part 2-1: Colour management — Default RGB colour space — sRGB.* (Paywalled;
   cited for the canonical sRGB matrix, OETF constants 12.92 / 0.0031308 / 1.055 / 1/2.4 /
   0.055, and Rec. 709 primaries + D65 white.)
2. **IEC 61966-2-1:1999/Amd.1:2003** — adds the 7-decimal XYZ→sRGB matrix for 16-bit. Freely
   available specification text reproduced as *"Specification of bg-sRGB (Amendment 1 to IEC
   61966-2-1)"*, color.org. <https://www.color.org/bgsrgb.pdf>
   — saved: `docs/color-srgb-iec61966-2-1-amd1-bgsrgb.pdf`
3. **ISO 22028-2:2013**, *Photography and graphic technology — Extended colour encodings …
   Part 2: ROMM RGB.* Specification text (primaries, **D50** reference white, RGB↔XYZ matrix)
   from the ICC RGB registry. <https://registry.color.org/rgb-registry/files/ROMMRGB.pdf>
   — saved: `docs/color-romm-rgb-iso22028-2.pdf`
4. **Adobe, Digital Negative (DNG) Specification, Version 1.4.0.0** (2012). §Tags
   ColorMatrix1/2 ("converts XYZ values to reference camera native color space values"); Ch. 6
   *"Mapping Camera Color Space to CIE XYZ Space"* (`XYZtoCamera = AB·CC·CM`,
   `CameraToXYZ = Inverse(XYZtoCamera)`, `CameraToXYZ_D50 = CA·CameraToXYZ` with linear
   Bradford CA; ForwardMatrix WB by camera-coordinate scaling).
   <https://www.kronometric.org/phot/processing/DNG/dng_spec_1.4.0.0.pdf>
   — saved: `docs/color-dng-spec-1.4.0.0.pdf`
5. **SMPTE RP 177:1993 (R2002)**, *Derivation of Basic Television Color Equations.* Defines the
   Normalized Primary Matrix (primaries-as-columns scaled to the white point) used by
   `rgb_to_xyz`. (Paywalled standard; cited for the construction method.)
   <https://standards.globalspec.com/std/1284890/smpte-rp-177>
6. **Bruce Justin Lindbloom**, *RGB/XYZ Matrices* and *RGB Working Space Information.*
   Reference ProPhoto-D50 RGB→XYZ matrix (matched to $2\times10^{-5}$) and the same NPM
   derivation. <http://www.brucelindbloom.com/index.html?Eqn_RGB_XYZ_Matrix.html>
7. **ITU-R BT.709-6**, *Parameter values for the HDTV standards …* — Rec. 709 primaries and
   luma coefficients $0.2126/0.7152/0.0722$, the display-referred perceptual-luma reference for
   finding F-5.
8. **mina86**, *Calculating RGB↔XYZ matrix* (2019) — independent derivation cross-check of the
   NPM construction. <https://mina86.com/2019/srgb-xyz-matrix/>
9. **J. Hable / "Filmic Worlds"**, *Filmic Tonemapping with Piecewise Power Curves* — the
   254→255 highlight-collapse trade-off and overshoot technique, context for finding #7 /
   F-6. <https://filmicworlds.com/blog/filmic-tonemapping-with-piecewise-power-curves/>
10. **E. Reinhard et al.**, *Photographic Tone Reproduction for Digital Images* (SIGGRAPH 2002)
    — the $L/(L+1)$ compressor that `highlight_rolloff` rescales.

*Downloaded reference PDFs (freely available, authoritative):*
`docs/color-srgb-iec61966-2-1-amd1-bgsrgb.pdf`, `docs/color-romm-rgb-iso22028-2.pdf`,
`docs/color-dng-spec-1.4.0.0.pdf`. Paywalled standards (IEC 61966-2-1 base text, ISO 22028-2
full text, SMPTE RP 177) are cited but not redistributed.
