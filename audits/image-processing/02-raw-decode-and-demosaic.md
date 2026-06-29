# Audit 02 — RAW Decode & Demosaic

**Component:** `latent-raw`
**Files audited:** `latent-raw/src/lib.rs`, `latent-raw/build.rs` (with cross-reference to `latent-image/src/color.rs`)
**Scope:** RAW unpack via LibRaw (unpack-only), black/white normalization, per-CFA white balance on the mosaic, bilinear and Malvar–He–Cutler (MHC) demosaic, highlight reconstruction, CFA handling, and the camera→working color matrix entry point.
**Date:** 2026-06-27
**Method:** Code read line-by-line, then every numeric/algorithmic claim verified against primary sources (Malvar–He–Cutler ICASSP 2004 paper Fig. 2 read element-by-element from the rendered PDF, the IPOL reproduction, the Adobe DNG Specification 1.6.0.0 Chapter 5 and tag definitions, and the LibRaw API/forum documentation). PDFs cited are downloaded under `docs/`.

---

## 1. Summary of the Decode / Demosaic Model

`latent` uses LibRaw purely as an **unpacker** (`libraw_open_file` → `libraw_unpack`), reading the raw Bayer mosaic (`rawdata.raw_image`, `u16`) and metadata (`color`, `idata`, `other`, `lens`), then runs its **own** pipeline. The model, in order:

1. **Normalize** (`RawImage::normalized`): per photosite, look up its CFA color $c \in \{0,1,2,3\}$ from the 2×2 `cfa[]` pattern and map
   $$ \tilde{s} = \max\!\left(0,\ \frac{s - (\text{black} + \text{cblack}[c])}{\max(1,\ \text{white} - (\text{black}+\text{cblack}[c]))}\right), $$
   where `black = color.black`, `cblack[c] = color.cblack[c]`, and `white = color.maximum`. Values above $1.0$ are **kept** (highlight headroom); only the floor is clamped.
2. **Clip mask** (`clip_mask`): boolean per photosite, `raw_sample as u32 >= color.maximum`, computed on the raw integers.
3. **White balance on the mosaic** (`apply_white_balance`): `cam_mul` normalized so green $=1$, applied per CFA color **before** demosaic.
4. **Demosaic**: `demosaic_bilinear` (3×3 same-color averaging) or `demosaic_mhc` (5×5 MHC interior, 2-pixel bilinear border).
5. **Highlight reconstruction** (`reconstruct_highlights`): post-demosaic, in white-balanced camera RGB; where $\ge 2$ channels are flagged clipped (via the exact raw mask propagated through the 3×3 neighborhood), rebuild the clipped channels up to the pixel's brightest channel, keeping measured channels.
6. **Color matrix** (`color_matrix` → `color::camera_to_working`): invert `cam_xyz` (XYZ→camera), compose with XYZ→linear-ProPhoto-D65, row-normalize so neutral stays neutral.

Working space is linear ProPhoto primaries pinned to D65; the pipeline is linear-light `f32`.

The data path is sound and the matrix/WB plumbing avoids the classic double-white-balance bug (via row-normalization). The MHC kernels are transcribed **correctly**. The principal correctness gaps are in **level metadata**: the code ignores the 2-D `cblack` black-level *pattern* (`cblack[4..]`) and the per-channel white level (`color.linear_max[]`), and it uses a per-channel rescale denominator that differs from the DNG model. It also accepts X-Trans files that its 2×2 Bayer demosaic cannot handle.

---

## 2. Point-by-Point Verification

### 2.1 Black-level handling — `cblack` pattern ignored

**Code** (`lib.rs:164-165`):
```rust
let color = self.meta.cfa[(i / w % 2) * 2 + (i % w % 2)] as usize;
let black = base + self.meta.cblack[color] as f32;   // base = color.black
```
`read_metadata` (`lib.rs:521-522`) copies only `color.black` and `color.cblack[0..4]`.

**Authoritative layout.** LibRaw's `libraw_colordata_t.cblack[]` is documented as: *"Per-channel black level correction. First 4 values are per-channel correction, next two are black level pattern block size, than `cblack[4]*cblack[5]` correction values (for indexes `[6 … 6+cblack[4]*cblack[5])`."* (LibRaw, *Data Structures and Constants*.) Equivalently, the per-pixel black level is
$$ \text{black}_{\text{pixel}} = \text{black} + \text{cblack}[c] + \text{cblack}\big[6 + (r \bmod H_b)\,W_b + (k \bmod W_b)\big], $$
with $W_b=\text{cblack}[4]$, $H_b=\text{cblack}[5]$. This mirrors the DNG model exactly: the `BlackLevel` tag is *"the zero light … encoding level, as a repeating pattern,"* sized by `BlackLevelRepeatDim` (`BlackLevelRepeatRows × BlackLevelRepeatCols`), origin top-left of `ActiveArea`, stored row-column-sample (DNG 1.6.0.0, Chapter 5 and the `BlackLevel`/`BlackLevelRepeatDim` tag definitions). The DNG black level per pixel is the **sum** `BlackLevel + BlackLevelDeltaH + BlackLevelDeltaV`.

The code reads `cblack[0..4]` only and **never** consults `cblack[4]`, `cblack[5]`, or the pattern values at `cblack[6..]`. For bodies that deliver their pedestal via the pattern (LibRaw's confirmed example: **Fujifilm X-Trans stores ALL black in the pattern**, `cblack[4]==cblack[5]==6`, with `cblack[0..3]` and even `black` possibly $0$; many CFA bodies populate a 2×2 pattern), the subtracted black is wrong — typically too small, leaving **raised, tinted shadows** and a non-zero black floor. Note `cblack[4]` is *reinterpreted* by this code as "the G2 channel's per-channel black," which is semantically wrong: `cblack[4]` is the pattern **width**, not a 5th channel. (The code's `cblack` array is `[u32;4]`, so it cannot even hold index 4 — it is structurally incapable of reading the pattern.)

> Note on LibRaw's own normalization: LibRaw's `subtract_black()`/`raw2image` *does* fold the pattern into a per-pixel black before scaling. Because this code is unpack-only and re-implements normalization, it must replicate that folding itself.

**VERDICT: Incorrect (per-channel scalar path) / Incomplete (pattern path).** The scalar `black + cblack[c]` term is correct as far as it goes, but omitting the `cblack[4..]` 2-D pattern mishandles every camera that encodes black there (notably X-Trans, and a number of Bayer bodies).
**Citations:** LibRaw *Data Structures and Constants* (cblack layout); LibRaw issue #107 (X-Trans black detected as 0; pattern usage); DNG 1.6.0.0 §"Black Subtraction" + `BlackLevel`/`BlackLevelRepeatDim` tags.

---

### 2.2 White level — scalar `color.maximum`, per-channel `linear_max` ignored

**Code** (`lib.rs:181-186`, `522`): `clip_mask` and `normalized` both use the single scalar `white = color.maximum`.

**Authoritative semantics.** LibRaw exposes three distinct fields (LibRaw *Data Structures and Constants*; forum *Maximum/saturation pixel value*, *color.maximum and camera white level*):
- `color.maximum` — *"Maximum pixel value. Calculated from the data for most cameras, hardcoded for others … may be changed on postprocessing … and by automated maximum adjustment."* It is a **format/guessed** saturation, available right after `unpack()`. It can be optimistic or conservative.
- `color.linear_max[4]` — *"Per-channel linear data maximum read from file metadata … set to zero"* if absent. This is the **vendor "specular white"** saturation, **per channel**. (Documented example: Nikon D70s `maximum = 4095`, `linear_max = {3827,3827,3827,3827}` — a ~7% difference.)
- `color.data_maximum` — actual per-file max, only filled at `raw2image`/`dcraw_process`, i.e. **not** available in this unpack-only flow.

**Why this matters (magenta highlights).** At the sensor all channels clip at (approximately) the same level, but per-channel saturation can differ, and the white-balance gains differ per channel. Using a *single* `maximum` that is **too high** lets a channel that actually saturated slip below the clip threshold, so it is treated as valid and not reconstructed; after per-channel WB the channels land at different heights and a blown neutral renders **pink/magenta**. The LibRaw author's explicit recommendation for exactly this "pink clouds" failure is to **"use camera-provided `linear_max` values (if any)"** and to derive the saturation from a robust histogram that ignores hot pixels, rather than trusting a single inflated `maximum` (LibRaw forum *Pink Clouds with LibRaw 0.20.0*).

The DNG model is also explicitly **per-channel**: the `WhiteLevel` tag has `Count = SamplesPerPixel`, i.e. one white level per CFA plane.

**VERDICT: Questionable.** A single scalar `maximum` is the easy choice and is *adequate when `maximum` is accurate and channels share a saturation*, but it ignores LibRaw's own recommended `linear_max[4]` (which is per-channel and available after `unpack()`), and it can directly cause magenta highlight casts when `maximum` is miscalibrated. Both the clip mask and the normalization denominator should prefer per-channel `linear_max[c]` when non-zero.
**Citations:** LibRaw *Data Structures and Constants*; LibRaw forum nodes 2238, 2473, 2605 (pink clouds); DNG 1.6.0.0 `WhiteLevel` tag (Count = SamplesPerPixel).

---

### 2.3 Normalization formula vs. DNG linearization

**Code** (`lib.rs:166-170`): `scale = 1 / max(1, white - black_c)`, then `(s - black_c) * scale` floored at 0; ceiling **not** clamped (headroom kept). Per channel, `black_c = black + cblack[c]`.

**DNG model** (1.6.0.0, Chapter 5, "Mapping Raw Values to Linear Reference Values"): Linearization (LUT) → Black Subtraction (`BlackLevel + DeltaH + DeltaV`) → **Rescaling** with *"scale factor … the inverse of the difference between the value specified in the `WhiteLevel` tag and the **maximum computed black level for the sample plane**"* → Clipping to $[0,1]$.

Two deviations:
1. **Per-channel denominator vs. DNG's single per-plane denominator.** The code rescales each channel by its *own* $(\text{white} - \text{black}_c)$. The DNG formula uses one denominator per sample plane built from the **maximum** black level. With different `cblack[c]` per channel, the code gives each channel a *slightly different gain* even before white balance: a red site with `cblack=+200` is scaled by $1/(W-B-200)$ while green is scaled by $1/(W-B)$. For a saturated photosite this is *intentional and arguably desirable* (the code's own test `normalize_subtracts_per_channel_black` shows every channel's white lands at exactly $1.0$), but for **mid-tones it introduces a small per-channel gain difference** (a faint tint) that the subsequent `cam_mul` white balance does not exactly cancel, because WB multiplies by a constant gain, not by a per-channel range ratio. In practice `cblack[c]` spreads are tiny (tens of DN out of thousands), so the tint is sub-LSB at 8-bit; it is a real but negligible deviation, *as long as `cblack` spreads stay small.* It would matter more for sensors with large per-channel black offsets.
2. **No upper clip.** Keeping values $>1$ is a deliberate, defensible choice for a floating-point pipeline with separate highlight handling (and is *more* information-preserving than DNG's clip-to-1). This is correct for the architecture; saturation is tracked by `clip_mask` instead. Fine.
3. **Black subtraction vs. linearization order:** the code has no `LinearizationTable` step. LibRaw applies the linearization curve during `unpack()`, so `raw_image` is already linearized — this is correct and not a gap.

**VERDICT: Correct-with-caveats.** The structure (per-channel black subtract → rescale → floor, headroom preserved) matches standard raw linearization. The per-channel *denominator* diverges from DNG's single per-plane scale and can introduce a slight mid-tone tint when per-channel black offsets are large; negligible for typical bodies.
**Citation:** DNG 1.6.0.0, Chapter 5 (Black Subtraction / Rescaling / Clipping).

---

### 2.4 White balance on the mosaic, before demosaic

**Code** (`lib.rs:193-207`): `cam_mul` normalized to green $=1$ (with a G2-zero fallback to G1), applied per CFA color **before** demosaic. Gains `[m0/g, 1, m2/g, g2/g]`.

**Standard order.** Applying as-shot WB multipliers to the mosaic *before* demosaic is standard and is exactly what **darktable** does: *"the white balance as reported by the camera is used in processing modules before input color profile … before … demosaic … so that demosaic works correctly"* (darktable manual, white-balance module). dcraw's `scale_colors()` likewise pre-multiplies the mosaic before interpolation. (RawTherapee computes *auto*-WB statistics after demosaic, but the as-shot multipliers are still applied to the mosaic; the post-demosaic step there is an analysis pass, not where the camera multipliers are applied.)

**Does WB-before-MHC violate MHC's assumptions?** MHC's gradient correction assumes inter-channel correlation (a sharp luminance edge appears in all channels). A **constant per-channel scalar** gain preserves edge *positions* and the proportionality the Laplacian correction exploits — it only rescales amplitudes. Because the correction term for, e.g., G-at-R is $\alpha \cdot \Delta_R$ where $\Delta_R$ is a Laplacian of the **same** (red) channel that is being WB-scaled together with the green bilinear estimate, a uniform per-channel scale does **not** break the design (the well-known caveat is the opposite ordering issue — clipping/non-linearity before demosaic, not linear WB). darktable's choice to WB before demosaic confirms this is accepted practice and, if anything, improves highlight/CA behavior. One genuine subtlety: WB pushes some channels well above 1.0, so the linear MHC filter can produce overshoot near clipped edges — but that is inherent to linear demosaic and handled downstream by highlight reconstruction.

**VERDICT: Correct.** WB on the mosaic before demosaic is the standard order; a linear per-channel gain does not violate MHC's gradient assumptions.
**Citations:** darktable manual (white balance / demosaic ordering); dcraw `scale_colors`.

---

### 2.5 Malvar–He–Cutler 5×5 kernels — element-by-element

Verified against **Malvar, He & Cutler, ICASSP 2004, Fig. 2** (read directly from the rendered PDF at 400 DPI) and cross-checked against the IPOL reproduction. All filters are divided by 8 (the paper states coefficients are scaled by 8; gains $\alpha=1/2,\ \beta=5/8,\ \gamma=3/4$).

**(a) G at R/B locations** — code `G_AT_RB` (`lib.rs:274-280`). Paper "G at R locations" / "G at B locations" cross:
$$
\frac{1}{8}\begin{bmatrix}
0&0&-1&0&0\\
0&0&2&0&0\\
-1&2&4&2&-1\\
0&0&2&0&0\\
0&0&-1&0&0
\end{bmatrix}
$$
Center $4$, axis-1 neighbors $2$, axis-2 neighbors $-1$. **Matches.**

**(b) R/B at green, same-row case** — code `ROW` (`lib.rs:291-297`). Paper "R at green in R row, B column" (and symmetric "B at green in B row, R column"):
$$
\frac{1}{8}\begin{bmatrix}
0&0&\tfrac12&0&0\\
0&-1&0&-1&0\\
-1&4&5&4&-1\\
0&-1&0&-1&0\\
0&0&\tfrac12&0&0
\end{bmatrix}
$$
Center $5$, horizontal-1 $=+4$, four diagonal-adjacent $=-1$, horizontal-2 $=-1$, vertical-2 $=+\tfrac12$. **Matches** (confirmed against IPOL: center +5, ±1 horiz +4, diagonals −1, horiz dist-2 −1, vert dist-2 +1/2).

**(c) R/B at green, same-column case** — code `COL` (`lib.rs:300-306`). Paper "R at green in B row, R column" (transpose of (b)):
$$
\frac{1}{8}\begin{bmatrix}
0&0&-1&0&0\\
0&-1&4&-1&0\\
\tfrac12&0&5&0&\tfrac12\\
0&-1&4&-1&0\\
0&0&-1&0&0
\end{bmatrix}
$$
**Matches** — exact transpose of `ROW`, as the code comment claims.

**(d) R at B / B at R (diagonal / checkerboard)** — code `DIAG` (`lib.rs:282-288`). Paper "R at blue in B row, B column" (and symmetric "B at red"):
$$
\frac{1}{8}\begin{bmatrix}
0&0&-\tfrac32&0&0\\
0&2&0&2&0\\
-\tfrac32&0&6&0&-\tfrac32\\
0&2&0&2&0\\
0&0&-\tfrac32&0&0
\end{bmatrix}
$$
Center $6$, four diagonal-1 $=+2$, axis-2 $=-\tfrac32$. Code uses $-1.5 = -\tfrac32$. **Matches.**

**Assignment logic** (`lib.rs:319-342`). `center_ch` comes from `channel_at`. For R sites: $G\!=\!G\_AT\_RB$, $B\!=\!DIAG$. For B sites: $G\!=\!G\_AT\_RB$, $R\!=\!DIAG$. For green sites the code inspects the **right** neighbor: if `channel_at(x+1,y)==0` (red is the horizontal neighbor, i.e. the green sits in a red row), then $R$ uses `ROW` and $B$ uses `COL`; else swapped. This is exactly the paper's "R at green in **R row**" → ROW vs. "R at green in **B row**" → COL distinction, applied symmetrically to B. The `x+1` read is in-bounds because MHC only runs when `x+2 < w`. **Correct.**

**VERDICT: Correct.** Coefficients, the $1/8$ normalization, the ROW/COL row-vs-column assignment, and the diagonal R↔B filter all match the source element-by-element. Independently corroborated by the synthetic round-trip test `mhc_beats_bilinear_on_detailed_image` and `all_cfa_phases_reconstruct_equally`.
**Citations:** Malvar–He–Cutler 2004 Fig. 2 (`docs/demosaic-malvar-he-cutler-2004.pdf`); IPOL *Malvar-He-Cutler Linear Image Demosaicking*.

---

### 2.6 2-pixel MHC border fallback to bilinear

**Code** (`lib.rs:353-357`): pixels with the full 5×5 window out of bounds (`x<2 || y<2 || x+2>=w || y+2>=h`) fall back to `bilinear_pixel`.

A 5×5 filter genuinely cannot be evaluated within 2 px of the edge without padding. Bilinear there is the standard, conservative choice (dcraw/RawTherapee similarly degrade or mirror at borders). Two minor notes: (1) MHC implementations sometimes *mirror/clamp* the window to keep the higher-quality filter to the very edge; bilinear is simpler and a touch softer on a 2-px frame — cosmetically invisible on real images. (2) The bilinear fallback itself handles borders correctly (see 2.9).

**VERDICT: Correct-with-caveats (acceptable).** A 2-px bilinear frame is a reasonable, common fallback; a mirrored-edge MHC would be marginally sharper but is not required for correctness.

---

### 2.7 Highlight reconstruction

**Code** (`reconstruct_highlights` + `clipped_channels`, `lib.rs:369-416`): using the **exact raw clip mask** (not a post-demosaic value threshold), a channel at a pixel is "clipped" if its own photosite saturated (center channel) or any same-color photosite in the 3×3 neighborhood saturated (interpolated channels). Where $\ge 2$ channels are clipped, the clipped channels are raised to the pixel's **brightest** channel (`peak = max(R,G,B)`); measured channels are kept untouched.

**Comparison to established methods.** dcraw's `-H` ladder is the canonical reference (dcraw man page; LibRaw *Understanding highlight modes*): `0=clip` (all to white), `1=unclip` (leave pink), `2=blend`, `3..9=rebuild` (low favors white, high favors color). The dcraw LCH-blend (`-G`) sets L and H from the unclipped neighborhood and C from the clipped pixel. RawTherapee offers Luminance Recovery (gray fill), CIELab blending, and **Color Propagation** (bleeds surrounding known color into the clipped region) (RawPedia *Exposure*; RawTherapee issue #3311).

This code's method is a **single-channel-preserving "rebuild toward neutral peak"** — close in spirit to dcraw rebuild modes with a *low* color-favoring number, with one genuinely good property: by gating on **the exact raw mask** and on **$\ge 2$ clipped channels**, it (a) avoids flattening a genuinely saturated single-channel color (a real red light keeps its hue — verified by the test `reconstruct_highlights_rebuilds_blown_channels_keeping_measured_ones`), and (b) fixes the dominant artifact, neutral highlights going magenta. That is a sensible, conservative default.

**Limitations** (all "the more advanced methods do more"):
- **No spatial propagation.** It reconstructs each pixel from its own channels only; it cannot recover *texture/gradient* in a fully-blown region the way Color Propagation or guided/LCH-blend methods do. A large blown cloud becomes a flat plateau at `peak`.
- **`peak = max channel` can be wrong** when the brightest channel is itself clipped at a per-channel-too-low white level (ties into 2.2): if `maximum` is conservative, `peak` undershoots and highlights render dim/gray; if optimistic, residual color remains. Using per-channel `linear_max` would make `peak` more trustworthy.
- **The 3×3 mask propagation** for interpolated channels is a reasonable approximation of "this channel was drawn from a saturated same-color sample," but it does not match the **5×5** MHC support — an MHC-interpolated channel can draw from a clipped sample up to 2 px away that the 3×3 test misses, so a thin ring of MHC pixels just outside the 3×3 reach can keep a faint cast. Minor.
- It runs in **white-balanced camera RGB before the color matrix**, which is the correct stage (the clipping geometry is per-CFA there).

**VERDICT: Correct-with-caveats.** A defensible, conservative reconstruction that correctly targets the magenta-highlight problem and preserves genuine saturated colors; weaker than color-propagation/LCH methods for recovering structure in large blown regions, and its quality is bounded by the white-level accuracy from 2.2. The code's own comment ("Finer color propagation can come later") acknowledges this.
**Citations:** dcraw man page (`-H`, `-G`); LibRaw *Understanding highlight modes* (node 2694); RawPedia *Exposure*; dcraw LCH-blend patch.

---

### 2.8 CFA handling

**`is_rgb_bayer`** (`lib.rs:435-437`): accepts iff `cdesc == b"RGBG"`.

**Problem — X-Trans (and any RGB non-Bayer) passes this guard.** Per LibRaw (Alex Tutubalin, forum node 2561): *"cdesc is not a pattern, it is just an indication (RGB or CMYK or CMYG, etc.), so for all four R-G-B-G2 orders possible cdesc will be set to RGBG."* X-Trans sensors are **also R/G/B**, so their `cdesc` is **likewise `"RGBG"`**. X-Trans is distinguished by `idata.filters == 9` and a 6×6 `idata.xtrans[][]` pattern — **not** by `cdesc`. Consequently a Fujifilm X-Trans `.RAF` would pass `is_rgb_bayer`, and then be demosaiced by 2×2-Bayer logic that reads `cfa[(y%2)*2 + (x%2)]` — producing a **scrambled, mis-colored** image rather than the intended clean rejection. The decode guard's stated intent ("reject CYGM/RGBE/X-Trans") is only met for CYGM/RGBE (whose `cdesc` differs); **X-Trans is not actually rejected.**

**`channel_at`** (`lib.rs:210-213`): `c = cfa[(y%2)*2 + (x%2)]`, then `(c==2)*2 + (c==1||c==3)`. Mapping:

| CFA index $c$ | meaning | result | RGB channel |
|---|---|---|---|
| 0 | R | $0\cdot2+0$ | 0 (R) ✓ |
| 1 | G | $0\cdot2+1$ | 1 (G) ✓ |
| 2 | B | $1\cdot2+0$ | 2 (B) ✓ |
| 3 | G2 | $0\cdot2+1$ | 1 (G) ✓ |

Correct for all four colors, and because it reads the *actual* `cfa[]` (filled by `libraw_COLOR(raw, row, col)` in `read_metadata`, `lib.rs:512-516`), it is correct for **all four Bayer phases** (RGGB / BGGR / GRBG / GBRG). This is empirically confirmed by `all_cfa_phases_reconstruct_equally` (every phase MAE < 0.03). G2→green is handled correctly everywhere.

**VERDICT (channel mapping & phases): Correct.**
**VERDICT (sensor-type guard): Incorrect** — `is_rgb_bayer` lets X-Trans through; the guard should also require a Bayer `filters` value (e.g. `filters != 9` and `filters != 0`) and/or `colors == 3` with a 2×2 layout. (`read_metadata` does not currently read `idata.filters`/`idata.colors`.)
**Citations:** LibRaw forum node 2561 (cdesc is RGBG for all Bayer orders); LibRaw *Data Structures and Constants* (`filters==9` ⇒ X-Trans, `idata.xtrans`); LibRaw node 2301.

---

### 2.9 Bilinear demosaic quality

**Code** (`bilinear_pixel`, `lib.rs:218-246`): known channel = the sample; each missing channel = mean of same-color samples in the 3×3 neighborhood, clamped at borders; a pixel with no same-color neighbor (degenerate tiny image) falls back to the center value.

This is textbook separable bilinear demosaic (the paper's Eqs. (1) with the cross/diagonal neighbor sets). It is **exact for constant and linear signals** (the round-trip tests `roundtrip_of_constant_image_is_lossless` and `bilinear_reconstructs_a_smooth_gradient_well` confirm MAE 0 and <0.01). Border handling is correct: the `nx<0 || ny<0 || nx>=w || ny>=h` guard simply averages the *available* same-color neighbors, which is the standard "shrink the window at the edge" behavior — no out-of-bounds reads, no wraparound. The only quality limitation is the well-known one (zipper/false-color on high-frequency edges) inherent to channel-independent bilinear, which is precisely why MHC exists and is the default-quality path.

**VERDICT: Correct.** Standard bilinear; border-safe; exact on smooth content; the expected (acceptable) softness/artifacts on detail.

---

### 2.10 FFI / build (`build.rs`, unpack)

Not a numeric-correctness item, but in scope. `build.rs` links LibRaw via `pkg-config` (dynamic — LGPL-clean) and allowlists only `libraw_*` symbols. `unpack` follows LibRaw's `open → unpack` lifecycle with checked return codes, a RAII `Handle` (`libraw_close` on drop on every path), and a null check on `rawdata.raw_image`. Width/height use `sizes.raw_width/raw_height` (the **full** unpacked buffer including masked/optical-black borders), and the mosaic length is `raw_width*raw_height` — consistent with indexing `raw_image` directly. **Note:** because dimensions are the *raw* (not *visible/active*) area, the CFA phase from `libraw_COLOR(raw,0,0)` and the `cfa[]` lookup are anchored at the raw top-left, which is the correct origin for `raw_image`. No correctness issue spotted here; this matches LibRaw's documented `raw_image` semantics.

**VERDICT: Correct.**

---

## 3. Findings by Severity

### CRITICAL

**C1 — `cblack` 2-D black-level pattern (`cblack[4..]`) is ignored.**
- **Where:** `lib.rs:520-522` (`read_metadata` copies only `cblack[0..4]`; `Metadata.cblack` is `[u32;4]`, while the FFI field is `cblack: [c_uint; 4104]`), consumed at `lib.rs:164-165`.
- **Issue:** LibRaw/DNG encode a repeating 2-D black pattern in `cblack[4]=W`, `cblack[5]=H`, values at `cblack[6 .. 6+W*H]`. The code never reads it; index 4 is *mis-used* as a "G2 channel black." Cameras that store their pedestal in the pattern (confirmed for **Fujifilm X-Trans**, where `cblack[0..3]` and `black` may be 0 and *all* black is in the 6×6 pattern; also various Bayer bodies with 2×2 patterns) get the wrong (usually too small) black subtracted ⇒ raised, color-tinted shadows and a non-zero black floor.
- **Reference:** LibRaw *Data Structures and Constants* (cblack layout); DNG 1.6.0.0 Ch.5 + `BlackLevel`/`BlackLevelRepeatDim` tags; LibRaw issue #107.
- **Recommendation:** Read the full `color.cblack[]` (the generated binding is `cblack: [c_uint; 4104]` — confirmed in `OUT_DIR/bindings.rs`; the code currently widens only indices `0..4` into its `[u32;4]`). Copy `cblack[0..6]` plus the `W*H` pattern values. Compute per-pixel black as `black + cblack[c] + cblack[6 + (row%H)*W + (col%W)]` (guarding `W==0||H==0`). At minimum, detect `cblack[4]*cblack[5] > 0` and fold the pattern in; this also unblocks correct X-Trans black if/when X-Trans demosaic is added.

### HIGH

**H1 — Per-channel white level (`color.linear_max[4]`) ignored; scalar `maximum` can cause magenta highlights.**
- **Where:** `lib.rs:181-186` (`clip_mask`), `lib.rs:166` & `522` (`normalized` / `read_metadata`).
- **Issue:** Both clip detection and normalization use only `color.maximum`, a single format/guessed value that *"may be changed by automated maximum adjustment"* and can be optimistic. LibRaw's own remedy for pink/magenta blown highlights is to prefer per-channel `linear_max[]`. With a single wrong `maximum`, a truly-saturated channel can evade the clip mask and, after per-channel WB, render the highlight magenta.
- **Reference:** LibRaw *Data Structures and Constants*; LibRaw forum nodes 2238, 2473, **2605 (Pink Clouds)**; DNG 1.6.0.0 `WhiteLevel` (Count = SamplesPerPixel).
- **Recommendation:** Read `color.linear_max[4]`; where `linear_max[c] != 0`, use it as the per-channel white for **both** the clip mask and the normalization denominator (the clip test then becomes per-CFA-channel). Keep `maximum` as the fallback. This also makes `reconstruct_highlights`' `peak` trustworthy (see N2).

**H2 — `is_rgb_bayer` accepts X-Trans, which the 2×2 demosaic cannot handle.**
- **Where:** `lib.rs:435-437`, gate at `lib.rs:475-477`.
- **Issue:** `cdesc == "RGBG"` is true for **every** RGB sensor including **X-Trans** (cdesc is a color *indication*, not a pattern). X-Trans is identified by `idata.filters == 9` / `idata.xtrans`, which the code never reads. An X-Trans `.RAF` passes the guard and is then mis-demosaiced as Bayer ⇒ scrambled color, contradicting the guard's documented intent.
- **Reference:** LibRaw forum node 2561 (cdesc=RGBG for all Bayer orders); LibRaw *Data Structures and Constants* (`filters==9` ⇒ X-Trans).
- **Recommendation:** Read `idata.filters` and `idata.colors`; accept only standard 2×2 Bayer (e.g. `colors == 3 && filters != 0 && filters != 9`). Reject `filters == 9` (X-Trans), `filters == 0` (non-Bayer/Fovean/linear), and ≠3-color CFAs explicitly.

### MEDIUM

**M1 — Per-channel rescale denominator deviates from the DNG single-per-plane scale.**
- **Where:** `lib.rs:166-169`.
- **Issue:** Normalizing each channel by its own `(white − black_c)` (rather than DNG's single `1/(WhiteLevel − max_black)` per plane) gives channels slightly different gains; for mid-tones this is a faint tint that WB does not exactly cancel. Negligible for small `cblack` spreads, larger if a body has big per-channel black offsets.
- **Reference:** DNG 1.6.0.0 Ch.5 ("Rescaling": inverse of `WhiteLevel − maximum computed black level for the sample plane").
- **Recommendation:** Either adopt DNG's per-plane scale (`white − max_c black_c`) for all channels, or — if per-channel white is implemented (H1) — use `(linear_max[c] − black_c)` consistently so the per-channel normalization is colorimetrically intentional rather than a side effect of per-channel black.

### LOW

**L1 — MHC highlight-clip mask uses 3×3 propagation but MHC support is 5×5.**
- **Where:** `clipped_channels` (`lib.rs:374-386`) vs. `mhc_pixel` 5×5 support.
- **Issue:** An MHC-interpolated channel can draw from a saturated same-color sample 2 px away that the 3×3 clip-propagation misses, leaving a faint cast on a thin ring of MHC pixels at the edge of blown regions.
- **Recommendation:** For the MHC path, propagate the clip flag over the same 5×5 same-color support (or simply over a 5×5 window); cheap and tightens highlight recovery.

**L2 — MHC border could mirror/clamp instead of dropping to bilinear.**
- **Where:** `lib.rs:353-357`. A 2-px bilinear frame is slightly softer than mirrored-edge MHC. Cosmetic; optional.

### NOTE

**N1 — Headroom (values > 1 kept) is correct and intentional** for an FP pipeline with a separate clip mask; not a bug (it is *more* faithful than DNG clip-to-1). Documented as such in code.

**N2 — `reconstruct_highlights` quality is bounded by white-level accuracy.** `peak = max(R,G,B)` is only trustworthy if the brightest channel's white level is right (ties to H1). The method is otherwise a sound, conservative default that correctly preserves genuine single-channel saturated colors and fixes magenta neutrals; it intentionally does **not** do spatial color propagation (the code comment says so). Consider, later, an LCH-blend or guided color-propagation pass for structure recovery in large blown regions.

**N3 — WB G2 fallback** (`g2 = m[3] != 0 ? m[3] : g`, `lib.rs:196`) and the `(white−black).max(1.0)` guard (`lib.rs:169`) are sensible defensive defaults; correct.

**N4 — `data_maximum` is correctly NOT used** here: it is only populated at `raw2image`/`dcraw_process`, which this unpack-only flow never calls; relying on it would read a stale/zero value.

---

## 4. Verdict Summary

| # | Item | Verdict |
|---|---|---|
| 2.1 | Black level: `cblack` pattern | **Incorrect / Incomplete** (pattern ignored) |
| 2.2 | White level: scalar `maximum` vs `linear_max` | **Questionable** |
| 2.3 | Normalization formula | **Correct-with-caveats** |
| 2.4 | WB before demosaic | **Correct** |
| 2.5 | MHC 5×5 kernels (element-by-element) | **Correct** |
| 2.6 | 2-px MHC bilinear border | **Correct-with-caveats** |
| 2.7 | Highlight reconstruction | **Correct-with-caveats** |
| 2.8a | `channel_at` mapping / all 4 phases | **Correct** |
| 2.8b | `is_rgb_bayer` sensor-type guard | **Incorrect** (X-Trans accepted) |
| 2.9 | Bilinear demosaic & borders | **Correct** |
| 2.10 | FFI / unpack lifecycle | **Correct** |

---

## 5. References

**Primary papers / specs (downloaded):**
- Malvar, He & Cutler, *High-Quality Linear Interpolation for Demosaicing of Bayer-Patterned Color Images*, IEEE ICASSP 2004. Fig. 2 verified element-by-element. → `docs/demosaic-malvar-he-cutler-2004.pdf` (source: <https://stanford.edu/class/ee367/reading/Demosaicing_ICASSP04.pdf>; Microsoft Research listing: <https://www.microsoft.com/en-us/research/publication/high-quality-linear-interpolation-for-demosaicing-of-bayer-patterned-color-images/>)
- Adobe, *Digital Negative (DNG) Specification, Version 1.6.0.0*, December 2021. Chapter 5 "Mapping Raw Values to Linear Reference Values"; `BlackLevel`, `BlackLevelRepeatDim`, `WhiteLevel` tag definitions. → `docs/demosaic-dng-spec-1.6.0.0.pdf` (source: <https://paulbourke.net/dataformats/dng/dng_spec_1_6_0_0.pdf>)

**LibRaw:**
- *Data Structures and Constants* (libraw_colordata_t: cblack layout, black, maximum, linear_max, data_maximum, cam_mul, cam_xyz; filters==9 ⇒ X-Trans): <https://www.libraw.org/docs/API-datastruct-eng.html>
- Forum — *Maximum/saturation pixel value*: <https://www.libraw.org/node/2238>
- Forum — *color.maximum and camera white level*: <https://www.libraw.org/node/2473>
- Forum — *Pink Clouds with LibRaw 0.20.0* (per-channel white level ↔ magenta highlights; "use linear_max"): <https://www.libraw.org/node/2605>
- Forum — *Questions about black-level and white-levels in colordata*: <https://www.libraw.org/node/2565>
- Forum — *Determining CFA Pattern from cdesc and filters* (cdesc=="RGBG" for all Bayer orders): <https://www.libraw.org/node/2561>
- Forum — *Understanding highlight modes* (dcraw -H ladder): <https://www.libraw.org/node/2694>
- Issue #107 — *Black level of FUJIFILM X-Trans detected as 0* (black in cblack pattern): <https://github.com/LibRaw/LibRaw/issues/107>
- Forum — *Fujifilm Pattern with rawpy*: <https://www.libraw.org/node/2301>

**Demosaic / highlight references:**
- IPOL, *Malvar-He-Cutler Linear Image Demosaicking* (independent kernel reproduction): <http://www.ipol.im/pub/art/2011/g_mhcd/revisions/2011-08-14/g_mhcd.htm>
- darktable manual — white balance module (WB applied before demosaic): <https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/white-balance/>
- darktable manual — demosaic module: <https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/demosaic/>
- RawPedia — *Exposure* (Highlight Reconstruction: Luminance Recovery, CIELab, Color Propagation): <https://rawpedia.rawtherapee.com/Exposure>
- RawTherapee issue #3311 — Color Propagation highlights / purple fringing: <https://github.com/Beep6581/RawTherapee/issues/3311>
- dcraw man page (-H 0..9, -G LCH blend): <https://www.dechifro.org/dcraw/dcraw.1.html>
- dcraw LCH-blend highlight recovery patch: <http://people.zoy.org/~cyril/dcraw_lchblend/highlight_recovery_dcraw_lch_patch.html>
