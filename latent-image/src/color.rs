//! Color math: 3x3 matrices and color-space conversions.

use crate::ImageBuf;

/// A 3x3 matrix, row-major. Used for color-space conversions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3(pub [[f32; 3]; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    /// Apply the matrix to a column vector: `self * v`.
    #[must_use]
    pub fn mul_vec(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
    }

    /// Matrix product `self * other`.
    #[must_use]
    pub fn mul(&self, other: &Mat3) -> Mat3 {
        let (a, b) = (&self.0, &other.0);
        Mat3(std::array::from_fn(|r| {
            std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c])
        }))
    }

    /// Determinant (via the rule of Sarrus).
    #[must_use]
    pub fn det(&self) -> f32 {
        let m = &self.0;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    /// Scale each row so it sums to 1, so a neutral input `[v,v,v]` maps to a
    /// neutral output `[v,v,v]`. Rows that sum to ~0 are left unchanged.
    #[must_use]
    pub fn row_normalized(&self) -> Mat3 {
        Mat3(std::array::from_fn(|r| {
            let sum: f32 = self.0[r].iter().sum();
            let sum = if sum.abs() < 1e-12 { 1.0 } else { sum };
            std::array::from_fn(|c| self.0[r][c] / sum)
        }))
    }

    /// The inverse, or `None` if the matrix is singular.
    #[must_use]
    pub fn inverse(&self) -> Option<Mat3> {
        let det = self.det();
        if det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let m = &self.0;
        // Inverse = adjugate / determinant (cofactor C[r][c] placed at [c][r]).
        let cof = |r0: usize, c0: usize, r1: usize, c1: usize| {
            m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
        };
        Some(Mat3([
            [
                cof(1, 1, 2, 2) * inv_det,
                -cof(0, 1, 2, 2) * inv_det,
                cof(0, 1, 1, 2) * inv_det,
            ],
            [
                -cof(1, 0, 2, 2) * inv_det,
                cof(0, 0, 2, 2) * inv_det,
                -cof(0, 0, 1, 2) * inv_det,
            ],
            [
                cof(1, 0, 2, 1) * inv_det,
                -cof(0, 0, 2, 1) * inv_det,
                cof(0, 0, 1, 1) * inv_det,
            ],
        ]))
    }
}

/// Build the **camera → XYZ** matrix from the file's **XYZ → camera** matrix
/// (LibRaw `cam_xyz` / DNG `ColorMatrix`).
///
/// The metadata gives the *forward* direction (what camera RGB a known XYZ
/// color makes); to lift camera RGB into XYZ we need its inverse. Returns
/// `None` if the matrix is singular.
pub fn camera_to_xyz(xyz_to_cam: Mat3) -> Option<Mat3> {
    xyz_to_cam.inverse()
}

/// Rec.709 / sRGB primary chromaticities `(x, y)` — R, G, B (IEC 61966-2-1).
const REC709_PRIMARIES: [[f32; 2]; 3] = [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]];

/// XYZ (D65) → linear sRGB.
///
/// Derived at full precision by inverting [`rgb_to_xyz`] of the Rec.709/sRGB
/// primaries at D65 — the *same* construction used for the working space, so the
/// sRGB and working matrices share one source of truth. This is exact where the
/// published 4-decimal IEC constant rounds (it is off by up to `~3.7e-4`, visible
/// at the 16-bit depth this pipeline exports), and it maps D65 white to neutral
/// to machine precision rather than approximately. A side effect that the output
/// transform relies on: because both ends come from `rgb_to_xyz`, the
/// working→sRGB product has unit row sums natively, with no normalization hack
/// (see [`linear_working_to_linear_srgb`]).
pub fn xyz_to_linear_srgb_matrix() -> Mat3 {
    rgb_to_xyz(REC709_PRIMARIES, D65_WHITE)
        .inverse()
        .expect("sRGB primaries are non-singular")
}

/// Convert a device-independent XYZ color into linear-light sRGB primaries.
pub fn xyz_to_linear_srgb(xyz: [f32; 3]) -> [f32; 3] {
    xyz_to_linear_srgb_matrix().mul_vec(xyz)
}

/// The XYZ of a chromaticity `(x, y)` at unit luminance (`Y = 1`). Used for both
/// primaries and white points so every space references the same construction.
fn chromaticity_to_xyz(c: [f32; 2]) -> [f32; 3] {
    [c[0] / c[1], 1.0, (1.0 - c[0] - c[1]) / c[1]]
}

/// Build the linear-RGB → XYZ matrix for an RGB space from its primary
/// chromaticities `[r, g, b]` (each `[x, y]`) and white point `[x, y]`.
///
/// Standard construction (SMPTE RP 177): take each primary's XYZ at unit
/// luminance as a column, then scale the columns so unit RGB reproduces the
/// white point. Deriving the matrix from published chromaticities keeps the
/// numbers verifiable (white → neutral, round-trips) rather than transcribed.
fn rgb_to_xyz(primaries: [[f32; 2]; 3], white: [f32; 2]) -> Mat3 {
    let to_xyz = chromaticity_to_xyz;
    let (r, g, b) = (
        to_xyz(primaries[0]),
        to_xyz(primaries[1]),
        to_xyz(primaries[2]),
    );
    let basis = Mat3([[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]]);
    let s = basis
        .inverse()
        .expect("primaries are linearly independent")
        .mul_vec(to_xyz(white));
    // Scale column `c` of the basis by `s[c]`.
    Mat3(std::array::from_fn(|row| {
        std::array::from_fn(|col| basis.0[row][col] * s[col])
    }))
}

/// ProPhoto / ROMM RGB primaries (ISO 22028-2) — the wide working gamut.
const PROPHOTO_PRIMARIES: [[f32; 2]; 3] = [[0.7347, 0.2653], [0.1596, 0.8404], [0.0366, 0.0001]];
/// CIE D65 white point `(x, y)` — the reference white of sRGB.
const D65_WHITE: [f32; 2] = [0.3127, 0.3290];
/// CIE D50 white point `(x, y)` — the reference white of the working space (and
/// of CIE Lab), shared by the Lab transform and the chromatic-adaptation targets.
pub const D50_WHITE: [f32; 2] = [0.3457, 0.3585];

/// The working space: **standard ROMM / ProPhoto RGB at its native D50 white**
/// (ISO 22028-2). The wide ProPhoto gamut keeps saturated camera colors in range,
/// and the standard D50 white gives the space a real ICC/ISO identity and lets
/// CIE Lab reference it natively (Lab is a D50 space — see [`Lab`]). The price of
/// a real D50 space is that chromatic adaptation is no longer free: the camera
/// transform adapts from the white-balance illuminant to D50, and the output
/// transform adapts D50→D65 for sRGB. Both are explicit Bradford steps (see
/// [`bradford_adapt`], [`camera_to_working`], [`linear_working_to_linear_srgb`]).
pub fn linear_working_to_xyz() -> Mat3 {
    rgb_to_xyz(PROPHOTO_PRIMARIES, D50_WHITE)
}

/// XYZ → linear working RGB.
pub fn xyz_to_linear_working() -> Mat3 {
    linear_working_to_xyz()
        .inverse()
        .expect("working primaries are non-singular")
}

/// The standard **Bradford** cone-response matrix (XYZ → sharpened LMS), as used
/// by ICC/DNG chromatic-adaptation. Its inverse takes LMS back to XYZ.
const BRADFORD: Mat3 = Mat3([
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
]);

/// The **Bradford chromatic-adaptation** matrix taking colors seen under
/// `src_white` to their appearance under `dst_white`, both given as `(x, y)`
/// chromaticities.
///
/// The whites are lifted to XYZ at unit luminance, projected into Bradford LMS
/// cone space, scaled per-channel by the destination/source cone ratio, and
/// projected back: `M⁻¹ · diag(ρ_dst/ρ_src) · M`. Adapting a white to itself is
/// the identity. This is the operator the camera and output transforms use to
/// reconcile the pipeline's different reference whites (the working space is D50,
/// sRGB is D65, and the capture illuminant is whatever the white balance says).
pub fn bradford_adapt(src_white: [f32; 2], dst_white: [f32; 2]) -> Mat3 {
    bradford_adapt_xyz(
        chromaticity_to_xyz(src_white),
        chromaticity_to_xyz(dst_white),
    )
}

/// [`bradford_adapt`] for whites already expressed in XYZ (used where the source
/// white comes from a matrix, e.g. the white-balanced camera neutral).
fn bradford_adapt_xyz(src_white: [f32; 3], dst_white: [f32; 3]) -> Mat3 {
    let src = BRADFORD.mul_vec(src_white);
    let dst = BRADFORD.mul_vec(dst_white);
    let scale = Mat3([
        [dst[0] / src[0], 0.0, 0.0],
        [0.0, dst[1] / src[1], 0.0],
        [0.0, 0.0, dst[2] / src[2]],
    ]);
    let bradford_inv = BRADFORD
        .inverse()
        .expect("the Bradford cone-response matrix is non-singular");
    bradford_inv.mul(&scale).mul(&BRADFORD)
}

/// Linear working RGB → linear sRGB, for the output transform at export.
///
/// The working space is D50 and sRGB is D65, so this is a genuine adapted
/// transform, composed at full precision: working → XYZ (D50) → Bradford-adapt
/// D50→D65 → XYZ → linear sRGB. Because both end matrices come from the same
/// [`rgb_to_xyz`] primaries construction, the product already has unit row sums,
/// so a neutral working gray lands on an exactly-neutral sRGB gray with no
/// row-normalization hack. (The old 4-decimal sRGB constant left a `~3.25e-4`
/// tint that normalization had to mask; the adapted matrix is correct
/// end-to-end.)
pub fn linear_working_to_linear_srgb() -> Mat3 {
    xyz_to_linear_srgb_matrix()
        .mul(&bradford_adapt(D50_WHITE, D65_WHITE))
        .mul(&linear_working_to_xyz())
}

/// Build the combined **camera → linear working** matrix from the file's
/// XYZ→camera matrix, the Adobe DNG color model.
///
/// Input is *white-balanced* camera RGB (white balance is applied exactly once,
/// upstream on the mosaic, where it is naturally a per-CFA-channel gain). This
/// matrix never re-applies white balance — that was the job of the old
/// row-normalization, which flattened chromatic colors as a side effect; here the
/// adaptation does it correctly instead.
///
/// `xyz_to_cam` is the file's `ColorMatrix` (XYZ→camera); its inverse lifts camera
/// RGB into XYZ. A white-balanced neutral `[1,1,1]` maps, through camera→XYZ, to
/// the XYZ of the capture illuminant; a Bradford step then adapts that illuminant
/// to the D50 working white before XYZ→working. The chain is
/// `working ← XYZ ← Bradford(illuminant→D50) ← camera→XYZ`.
///
/// This is the DNG `ForwardMatrix`-equivalent path: chromatic colors follow the
/// true camera transform (not a row-normalized approximation), while neutrals
/// stay neutral because the illuminant is *adapted* to D50 rather than
/// re-balanced. The capture illuminant is estimated from the white-balanced
/// neutral itself — the file gives no explicit scene-illuminant XYZ — which is
/// the rigorous reading of the available metadata and keeps the neutral invariant
/// exact. Returns `None` if the camera matrix is singular.
pub fn camera_to_working(xyz_to_cam: Mat3) -> Option<Mat3> {
    let cam_to_xyz = camera_to_xyz(xyz_to_cam)?;
    // The capture illuminant in XYZ: where a white-balanced camera neutral lands
    // under camera→XYZ. Adapt it to the D50 working white so a neutral stays
    // neutral without re-balancing, then lift into the working space.
    let illuminant_xyz = cam_to_xyz.mul_vec([1.0, 1.0, 1.0]);
    let d50_xyz = chromaticity_to_xyz(D50_WHITE);
    let adapt = bradford_adapt_xyz(illuminant_xyz, d50_xyz);
    Some(xyz_to_linear_working().mul(&adapt).mul(&cam_to_xyz))
}

/// Relative-luminance weights for the linear **working** space (the Y row of
/// [`linear_working_to_xyz`]: ProPhoto primaries at D50). Cross-checked against
/// that matrix in the tests. The GPU shader (`map_pixels.wgsl`) hard-codes the
/// same values for its saturation path — keep the two in sync (the GPU/CPU
/// render-equivalence test guards against drift).
///
/// These weights are *colorimetric* relative luminance, fine for exposure and
/// neutral grays. They are deliberately **not** the perceptual lightness: the
/// blue weight is near-zero (~0.00009, even smaller at D50 than at D65) because
/// these wide primaries carry almost no luminance in blue, so a fully desaturated
/// pure-blue maps to near-black — colorimetrically correct, but the reason any
/// *perceptual* operation must use [`Lab`]'s `L*` (which is hue-uniform) rather
/// than this luma.
pub const LUMA_WEIGHTS: [f32; 3] = [0.28807107, 0.71184325, 8.56539e-5];

/// Relative luminance of a linear-light RGB pixel — its perceived brightness.
pub fn luminance(rgb: [f32; 3]) -> f32 {
    LUMA_WEIGHTS[0] * rgb[0] + LUMA_WEIGHTS[1] * rgb[1] + LUMA_WEIGHTS[2] * rgb[2]
}

/// The break point of the CIE Lab companding, `δ = 6/29`. Below `δ³` the
/// cube-root is replaced by a linear segment so the transfer stays finite-sloped
/// (and invertible) at the origin.
const LAB_DELTA: f32 = 6.0 / 29.0;

/// The perceptual lightness `L*` of a relative luminance `Y` (`Y = 1` is the
/// working/reference white), the **single** perceptual-lightness definition for
/// the whole grading core. It is the lightness axis of CIE [`Lab`]: a roughly
/// uniform-step scale where the reference white is `L* = 100` and mid-luminance
/// sits near `L* = 50`, unlike the [`LUMA_WEIGHTS`] luma which is hue-skewed in
/// these wide primaries. Perceptual operations (clarity, denoise weighting, the
/// luma-domain sharpen) take their lightness from here so there is one definition
/// across CPU and GPU.
pub fn l_star(y: f32) -> f32 {
    116.0 * lab_f(y) - 16.0
}

/// Inverse of [`l_star`]: the relative luminance of a given `L*`.
pub fn l_star_inv(l: f32) -> f32 {
    lab_f_inv((l + 16.0) / 116.0)
}

/// The Lab forward companding `f(t)`: a cube-root with a linear segment below
/// `δ³` so the slope stays finite at black.
fn lab_f(t: f32) -> f32 {
    if t > LAB_DELTA * LAB_DELTA * LAB_DELTA {
        t.cbrt()
    } else {
        t / (3.0 * LAB_DELTA * LAB_DELTA) + 4.0 / 29.0
    }
}

/// Inverse of [`lab_f`].
fn lab_f_inv(ft: f32) -> f32 {
    if ft > LAB_DELTA {
        ft * ft * ft
    } else {
        3.0 * LAB_DELTA * LAB_DELTA * (ft - 4.0 / 29.0)
    }
}

/// A color in **CIE L\*a\*b\***, referenced to the D50 white of the working space.
///
/// `L*` is perceptual lightness (`0` black … `100` reference white); `a*` runs
/// green→red and `b*` blue→yellow. Lab is the device-independent perceptual space
/// the grading core uses for lightness ([`l_star`]) and, in its polar form
/// [`LCh`], for hue and chroma. It is natively a D50 space, which is exactly the
/// working-space white — so [`Lab::from_working`] needs no extra adaptation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

/// A color in **CIE LCh** — the cylindrical form of [`Lab`]. `l` is the same
/// lightness; `c` is chroma (`hypot(a, b)`, `0` for a neutral gray); `h` is hue
/// in radians (`atan2(b, a)`). LCh is where hue and saturation are graded, since
/// rotating `h` or scaling `c` is a perceptually meaningful move that the
/// Cartesian a*/b* don't expose directly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LCh {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

impl Lab {
    /// XYZ (referenced to `white`, at unit luminance) → Lab. The standard CIE
    /// transform with the `δ = 6/29` linear-segment companding.
    pub fn from_xyz(xyz: [f32; 3], white: [f32; 3]) -> Lab {
        let fx = lab_f(xyz[0] / white[0]);
        let fy = lab_f(xyz[1] / white[1]);
        let fz = lab_f(xyz[2] / white[2]);
        Lab {
            l: 116.0 * fy - 16.0,
            a: 500.0 * (fx - fy),
            b: 200.0 * (fy - fz),
        }
    }

    /// Lab → XYZ (referenced to `white`), the exact inverse of [`Lab::from_xyz`].
    pub fn to_xyz(self, white: [f32; 3]) -> [f32; 3] {
        let fy = (self.l + 16.0) / 116.0;
        let fx = fy + self.a / 500.0;
        let fz = fy - self.b / 200.0;
        [
            white[0] * lab_f_inv(fx),
            white[1] * lab_f_inv(fy),
            white[2] * lab_f_inv(fz),
        ]
    }

    /// Linear **working** RGB → Lab. The working space is D50 and Lab is a D50
    /// space, so this is just working→XYZ followed by the Lab transform at the
    /// shared white — no chromatic adaptation belongs inside Lab.
    pub fn from_working(rgb: [f32; 3]) -> Lab {
        Lab::from_xyz(linear_working_to_xyz().mul_vec(rgb), d50_white_xyz())
    }

    /// Lab → linear **working** RGB, the inverse of [`Lab::from_working`].
    pub fn to_working(self) -> [f32; 3] {
        xyz_to_linear_working().mul_vec(self.to_xyz(d50_white_xyz()))
    }

    /// Polar form: chroma `hypot(a, b)` and hue `atan2(b, a)`. An achromatic
    /// color (`a ≈ b ≈ 0`) yields `c ≈ 0` and a finite hue (`atan2(0, 0) = 0`),
    /// never NaN.
    pub fn to_lch(self) -> LCh {
        LCh {
            l: self.l,
            c: self.a.hypot(self.b),
            h: self.b.atan2(self.a),
        }
    }
}

impl LCh {
    /// Back to Cartesian Lab: `a = c·cos h`, `b = c·sin h`.
    pub fn to_lab(self) -> Lab {
        Lab {
            l: self.l,
            a: self.c * self.h.cos(),
            b: self.c * self.h.sin(),
        }
    }
}

/// The D50 working white in XYZ at unit luminance — the reference white shared by
/// [`linear_working_to_xyz`] and the Lab transform.
fn d50_white_xyz() -> [f32; 3] {
    chromaticity_to_xyz(D50_WHITE)
}

/// Hue (turns, `[0, 1)`), saturation (`[0, 1]`), and value of a linear-RGB
/// pixel. Value is the channel maximum, so it carries highlight headroom (`> 1`)
/// through unchanged — unlike HSL lightness, this keeps the unbounded working
/// range intact across a round trip.
fn rgb_to_hsv(p: [f32; 3]) -> (f32, f32, f32) {
    let max = p[0].max(p[1]).max(p[2]);
    let min = p[0].min(p[1]).min(p[2]);
    let c = max - min;
    let s = if max <= 0.0 { 0.0 } else { c / max };
    let h = if c <= 1e-9 {
        0.0
    } else if max == p[0] {
        ((p[1] - p[2]) / c).rem_euclid(6.0)
    } else if max == p[1] {
        (p[2] - p[0]) / c + 2.0
    } else {
        (p[0] - p[1]) / c + 4.0
    };
    ((h / 6.0).rem_euclid(1.0), s, max)
}

/// Inverse of [`rgb_to_hsv`]. Output channels lie in `[0, v]`, so a value above
/// `1` reconstructs to RGB above `1` (headroom preserved).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = h.rem_euclid(1.0) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h6 % 2.0) - 1.0).abs());
    let m = v - c;
    // `f32::rem_euclid(1.0)` can return *exactly* `1.0` for a tiny negative input
    // (`1.0 - ε` rounds up to `1.0` in f32), making `h6 == 6.0` and `h6 as u32 == 6`
    // reachable — and `color_mix` feeds slightly-negative hue shifts. Clamp the
    // sextant to `5` so hue at the wheel's end deterministically lands in the last
    // (magenta→red) arm instead of relying on `x == 0` to mask the overflow.
    let sextant = (h6 as u32).min(5);
    let (r, g, b) = match sextant {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Apply a per-hue-band color mix to a linear-RGB pixel. `bands` holds eight
/// `[hue, sat, lum]` adjustments for hue centers evenly spaced around the wheel
/// (red … magenta). The pixel's hue picks its two neighbouring bands by linear
/// interpolation, so a color at a band center is driven only by that band; when
/// both neighbouring bands are neutral the pixel is returned unchanged. An
/// achromatic pixel (no saturation) has no hue to grade and is left alone. `hue`
/// shifts the hue (turns); `sat`/`lum` scale saturation/value by `1 + value`.
pub fn color_mix(rgb: [f32; 3], bands: &[[f32; 3]; 8]) -> [f32; 3] {
    let (h, s, v) = rgb_to_hsv(rgb);
    // An achromatic pixel has no hue to grade; leave neutrals exactly alone
    // instead of letting them fall into the red band (where hue 0 lands).
    if s <= 1e-6 {
        return rgb;
    }
    let pos = h * 8.0;
    let i = (pos.floor() as usize) % 8;
    let j = (i + 1) % 8;
    let f = pos - pos.floor();
    let adj: [f32; 3] = std::array::from_fn(|k| bands[i][k] * (1.0 - f) + bands[j][k] * f);
    if adj.iter().all(|&x| x == 0.0) {
        return rgb; // this hue's bands are untouched — leave it exactly alone
    }
    let h2 = h + adj[0];
    let s2 = (s * (1.0 + adj[1])).clamp(0.0, 1.0);
    let v2 = (v * (1.0 + adj[2])).max(0.0);
    hsv_to_rgb(h2, s2, v2)
}

/// Apply a 3x3 color matrix to every pixel of an image, returning a new image.
pub fn apply_matrix(img: &ImageBuf, m: &Mat3) -> ImageBuf {
    let mut out = ImageBuf::new(img.width(), img.height());
    for y in 0..img.height() {
        for x in 0..img.width() {
            out.set(x, y, m.mul_vec(img.get(x, y)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: &Mat3, b: &Mat3, eps: f32) -> bool {
        (0..3).all(|r| (0..3).all(|c| (a.0[r][c] - b.0[r][c]).abs() < eps))
    }

    #[test]
    fn identity_inverse_is_identity() {
        assert_eq!(Mat3::IDENTITY.inverse(), Some(Mat3::IDENTITY));
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        let zero = Mat3([[0.0; 3]; 3]);
        assert_eq!(zero.inverse(), None);
    }

    #[test]
    fn camera_to_xyz_inverts_the_metadata_matrix() {
        // An arbitrary non-singular matrix standing in for an XYZ→camera matrix.
        let xyz_to_cam = Mat3([[2.0, -1.0, 0.0], [-1.0, 2.0, -1.0], [0.0, -1.0, 2.0]]);
        let cam_to_xyz = camera_to_xyz(xyz_to_cam).expect("invertible");
        // Composing the two directions must give the identity, in both orders.
        assert!(approx_eq(
            &xyz_to_cam.mul(&cam_to_xyz),
            &Mat3::IDENTITY,
            1e-5
        ));
        assert!(approx_eq(
            &cam_to_xyz.mul(&xyz_to_cam),
            &Mat3::IDENTITY,
            1e-5
        ));
    }

    #[test]
    fn d65_white_maps_to_neutral_srgb() {
        // The D65 white point in XYZ must become neutral [1,1,1] in linear sRGB.
        // The full-precision matrix maps the chromaticity-derived D65 white to
        // neutral to machine precision (the old 4-decimal const drifted ~3e-4).
        let d65 = chromaticity_to_xyz(D65_WHITE);
        let rgb = xyz_to_linear_srgb(d65);
        for c in rgb {
            assert!((c - 1.0).abs() < 1e-5, "expected ~1.0, got {c}");
        }
    }

    #[test]
    fn xyz_to_srgb_matrix_matches_rec709_primaries() {
        // The matrix must be exactly the inverse of rgb_to_xyz(Rec.709, D65) —
        // the single-source-of-truth derivation — and stay close to the published
        // IEC constant (which is just that inverse, rounded to four decimals).
        let derived = xyz_to_linear_srgb_matrix();
        let reference = rgb_to_xyz(REC709_PRIMARIES, D65_WHITE)
            .inverse()
            .expect("invertible");
        assert!(approx_eq(&derived, &reference, 1e-6), "{derived:?}");
        let iec = Mat3([
            [3.2406, -1.5372, -0.4986],
            [-0.9689, 1.8758, 0.0415],
            [0.0557, -0.2040, 1.0570],
        ]);
        assert!(approx_eq(&derived, &iec, 5e-4), "vs IEC: {derived:?}");
    }

    #[test]
    fn black_maps_to_black() {
        assert_eq!(xyz_to_linear_srgb([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn luminance_weights_sum_to_one_and_favor_green() {
        // White is fully bright; the weights sum to 1 so neutral luma == value.
        assert!((luminance([1.0, 1.0, 1.0]) - 1.0).abs() < 1e-6);
        // Green contributes most, blue least.
        assert!(luminance([0.0, 1.0, 0.0]) > luminance([1.0, 0.0, 0.0]));
        assert!(luminance([1.0, 0.0, 0.0]) > luminance([0.0, 0.0, 1.0]));
    }

    #[test]
    fn camera_to_working_keeps_a_neutral_patch_neutral() {
        // Arbitrary non-singular stand-in for a real XYZ→camera matrix.
        let xyz_to_cam = Mat3([[1.4, -0.3, -0.1], [-0.5, 1.6, -0.1], [0.0, -0.4, 1.5]]);
        let m = camera_to_working(xyz_to_cam).expect("invertible");

        // The mosaic is white-balanced, so a neutral patch is camera RGB [v,v,v];
        // the DNG model adapts the capture illuminant to D50 and keeps it neutral
        // (WB applied exactly once on the mosaic, never re-applied here).
        for v in [0.25_f32, 0.5, 1.0] {
            let out = m.mul_vec([v, v, v]);
            assert!((out[0] - v).abs() < 1e-5, "R drifted: {out:?}");
            assert!((out[1] - v).abs() < 1e-5, "G drifted: {out:?}");
            assert!((out[2] - v).abs() < 1e-5, "B drifted: {out:?}");
        }
    }

    #[test]
    fn camera_to_working_follows_the_camera_transform_for_a_saturated_color() {
        // The DNG model must *not* flatten chromatic colors: a saturated camera
        // color is camera→XYZ→(Bradford illuminant→D50)→working, with no
        // row-normalization. Verify the composed matrix equals that explicit chain
        // and that the old row-normalized operator differs measurably from it.
        let xyz_to_cam = Mat3([[1.4, -0.3, -0.1], [-0.5, 1.6, -0.1], [0.0, -0.4, 1.5]]);
        let m = camera_to_working(xyz_to_cam).expect("invertible");

        let cam_to_xyz = camera_to_xyz(xyz_to_cam).unwrap();
        let illuminant = cam_to_xyz.mul_vec([1.0, 1.0, 1.0]);
        let adapt = bradford_adapt_xyz(illuminant, chromaticity_to_xyz(D50_WHITE));
        let reference = xyz_to_linear_working().mul(&adapt).mul(&cam_to_xyz);
        assert!(approx_eq(&m, &reference, 1e-6), "model: {m:?}");

        // The old approach (row-normalize camera→working) drifts saturated colors.
        let row_normed = xyz_to_linear_working().mul(&cam_to_xyz).row_normalized();
        let color = [0.8_f32, 0.2, 0.1];
        let new = m.mul_vec(color);
        let old = row_normed.mul_vec(color);
        let drift = (0..3).map(|c| (new[c] - old[c]).abs()).fold(0.0, f32::max);
        assert!(
            drift > 1e-3,
            "operator should differ from row-norm: {drift}"
        );
    }

    #[test]
    fn working_space_white_is_neutral_and_round_trips() {
        // Unit working RGB must be the D50 white in XYZ (the working space is now
        // standard D50 ProPhoto), and XYZ→working→XYZ must round-trip.
        let to_xyz = linear_working_to_xyz();
        let white_xyz = to_xyz.mul_vec([1.0, 1.0, 1.0]);
        let d50 = chromaticity_to_xyz(D50_WHITE);
        for c in 0..3 {
            assert!((white_xyz[c] - d50[c]).abs() < 1e-4, "white: {white_xyz:?}");
        }
        let back = xyz_to_linear_working().mul(&to_xyz);
        assert!(
            approx_eq(&back, &Mat3::IDENTITY, 1e-5),
            "round-trip: {back:?}"
        );
    }

    #[test]
    fn working_to_srgb_keeps_neutral_neutral() {
        // The D50-working → D65-sRGB adapted matrix maps a neutral gray to an
        // exactly-neutral sRGB gray (the full-precision derivation needs no
        // row-normalization to pin neutral).
        let m = linear_working_to_linear_srgb();
        for v in [0.25_f32, 0.5, 1.0] {
            let out = m.mul_vec([v, v, v]);
            for c in out {
                assert!((c - v).abs() < 1e-5, "neutral drifted: {out:?}");
            }
        }
    }

    #[test]
    fn working_to_srgb_rows_sum_to_one_without_normalization() {
        // Both ends derive from rgb_to_xyz, so the working→sRGB product has unit
        // row sums natively — the property the dropped row_normalized() faked.
        let m = linear_working_to_linear_srgb();
        for r in 0..3 {
            assert!((m.0[r].iter().sum::<f32>() - 1.0).abs() < 1e-5, "row {r}");
        }
    }

    #[test]
    fn working_to_srgb_adapts_d50_white_to_d65_neutral() {
        // The D50 working white must land on D65-sRGB neutral [1,1,1]: working
        // white is [1,1,1], so the matrix applied to it is the adapted white.
        let out = linear_working_to_linear_srgb().mul_vec([1.0, 1.0, 1.0]);
        for c in out {
            assert!((c - 1.0).abs() < 1e-5, "white not neutral: {out:?}");
        }
    }

    #[test]
    fn luma_weights_match_the_working_matrix() {
        // LUMA_WEIGHTS is the Y (second) row of the working RGB→XYZ matrix; this
        // catches any transcription error in the const.
        let y_row = linear_working_to_xyz().0[1];
        for c in 0..3 {
            assert!(
                (LUMA_WEIGHTS[c] - y_row[c]).abs() < 1e-6,
                "LUMA_WEIGHTS{LUMA_WEIGHTS:?} vs Y row {y_row:?}"
            );
        }
        assert!((LUMA_WEIGHTS.iter().sum::<f32>() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn bradford_self_adaptation_is_identity() {
        // Adapting a white to itself must be the identity (the cheap self-check).
        for w in [D50_WHITE, D65_WHITE] {
            assert!(
                approx_eq(&bradford_adapt(w, w), &Mat3::IDENTITY, 1e-6),
                "self-adapt {w:?}"
            );
        }
    }

    #[test]
    fn bradford_maps_d65_white_to_d50_white() {
        // The whole point of the operator: it moves the D65 white onto the D50
        // white in XYZ.
        let d65 = chromaticity_to_xyz(D65_WHITE);
        let d50 = chromaticity_to_xyz(D50_WHITE);
        let out = bradford_adapt(D65_WHITE, D50_WHITE).mul_vec(d65);
        for c in 0..3 {
            assert!((out[c] - d50[c]).abs() < 1e-4, "D65→D50: {out:?}");
        }
    }

    #[test]
    fn bradford_roundtrips() {
        // D50→D65→D50 returns to the identity.
        let round = bradford_adapt(D65_WHITE, D50_WHITE).mul(&bradford_adapt(D50_WHITE, D65_WHITE));
        assert!(approx_eq(&round, &Mat3::IDENTITY, 1e-5), "round: {round:?}");
    }

    #[test]
    fn lab_roundtrips_working_to_lab_and_back() {
        // working → Lab → working round-trips for a spread of in-gamut colors.
        let colors = [
            [0.5_f32, 0.5, 0.5],
            [0.8, 0.2, 0.1],
            [0.1, 0.6, 0.3],
            [0.2, 0.3, 0.9],
            [0.05, 0.05, 0.05],
        ];
        for px in colors {
            let back = Lab::from_working(px).to_working();
            for c in 0..3 {
                assert!((back[c] - px[c]).abs() < 1e-4, "{back:?} vs {px:?}");
            }
        }
    }

    #[test]
    fn working_white_has_l_star_100_and_neutral_ab() {
        // The working white is L* = 100 with a* = b* = 0 (Lab is D50-referenced,
        // exactly the working white).
        let lab = Lab::from_working([1.0, 1.0, 1.0]);
        assert!((lab.l - 100.0).abs() < 1e-3, "L*: {lab:?}");
        assert!(lab.a.abs() < 1e-3 && lab.b.abs() < 1e-3, "a*/b*: {lab:?}");
        // And the standalone l_star agrees with the Lab L of unit luminance.
        assert!((l_star(1.0) - 100.0).abs() < 1e-4);
        // Mid-luminance L* sits near 50 (perceptual, not linear): Y = 0.18 → ~49.5.
        assert!((l_star(0.18) - 49.496).abs() < 1e-2, "{}", l_star(0.18));
        // l_star inverts.
        assert!((l_star_inv(l_star(0.42)) - 0.42).abs() < 1e-5);
    }

    #[test]
    fn lab_matches_a_known_reference_color() {
        // sRGB primary red (linear [1,0,0]) has a well-known Lab under D50:
        // L*≈54.3, a*≈80.8, b*≈69.9 (Lindbloom). Build its XYZ from the sRGB
        // primaries at D65, Bradford-adapt to D50, then take the Lab transform —
        // exercising from_xyz against a published value.
        let red_xyz_d65 = rgb_to_xyz(REC709_PRIMARIES, D65_WHITE).mul_vec([1.0, 0.0, 0.0]);
        let red_xyz_d50 = bradford_adapt(D65_WHITE, D50_WHITE).mul_vec(red_xyz_d65);
        let lab = Lab::from_xyz(red_xyz_d50, chromaticity_to_xyz(D50_WHITE));
        assert!((lab.l - 54.29).abs() < 0.2, "L*: {lab:?}");
        assert!((lab.a - 80.80).abs() < 0.3, "a*: {lab:?}");
        assert!((lab.b - 69.89).abs() < 0.3, "b*: {lab:?}");
    }

    #[test]
    fn lch_is_polar_form_of_lab_and_roundtrips() {
        // LCh is the polar form of (a*, b*); Lab→LCh→Lab round-trips.
        let lab = Lab {
            l: 60.0,
            a: 30.0,
            b: -40.0,
        };
        let lch = lab.to_lch();
        assert!((lch.c - 50.0).abs() < 1e-4, "C* = hypot(30,40): {lch:?}");
        assert!((lch.h - (-40.0_f32).atan2(30.0)).abs() < 1e-6, "{lch:?}");
        let back = lch.to_lab();
        assert!((back.l - lab.l).abs() < 1e-4, "{back:?}");
        assert!((back.a - lab.a).abs() < 1e-4, "{back:?}");
        assert!((back.b - lab.b).abs() < 1e-4, "{back:?}");
    }

    #[test]
    fn achromatic_pixel_has_zero_chroma_and_finite_hue() {
        // A neutral gray has C* ≈ 0 and a finite (non-NaN) hue — atan2(0,0) = 0.
        let lch = Lab::from_working([0.4, 0.4, 0.4]).to_lch();
        assert!(lch.c < 1e-3, "chroma: {lch:?}");
        assert!(lch.h.is_finite(), "hue must be finite: {lch:?}");
    }

    #[test]
    fn apply_matrix_transforms_each_pixel() {
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [1.0, 2.0, 3.0]);
        img.set(1, 0, [0.0, 0.0, 0.0]);
        // Swap R and B: rows pick out the other channel.
        let swap = Mat3([[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
        let out = apply_matrix(&img, &swap);
        assert_eq!(out.get(0, 0), [3.0, 2.0, 1.0]);
        assert_eq!(out.get(1, 0), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn apply_identity_matrix_is_unchanged() {
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.1, 0.7, 0.4]);
        assert_eq!(
            apply_matrix(&img, &Mat3::IDENTITY).get(0, 0),
            [0.1, 0.7, 0.4]
        );
    }

    #[test]
    fn color_mix_neutral_bands_are_identity() {
        // All-zero bands leave every pixel exactly unchanged, including headroom.
        let bands = [[0.0_f32; 3]; 8];
        for px in [[0.8, 0.1, 0.1], [0.1, 0.8, 0.8], [0.5; 3], [2.0, 0.3, 0.0]] {
            assert_eq!(color_mix(px, &bands), px, "{px:?}");
        }
    }

    #[test]
    fn color_mix_leaves_neutral_grays_alone() {
        // A neutral pixel has no hue, so even a fully-dialed band must not touch
        // it — grays would otherwise fall into the red band at hue 0.
        let mut bands = [[0.0_f32; 3]; 8];
        bands[0] = [0.2, 0.5, 1.0]; // red band: hue, sat, and lum all pushed
        for g in [[0.0; 3], [0.5; 3], [1.0; 3], [2.0; 3]] {
            assert_eq!(color_mix(g, &bands), g, "neutral untouched: {g:?}");
        }
    }

    #[test]
    fn color_mix_one_band_leaves_the_others_alone() {
        // Desaturate only the red band (band 0). A pure-red pixel (hue 0) goes
        // gray; a pure-cyan pixel (hue 0.5, band 4) is untouched.
        let mut bands = [[0.0_f32; 3]; 8];
        bands[0] = [0.0, -1.0, 0.0]; // red band: saturation ×0
        let red = color_mix([0.8, 0.1, 0.1], &bands);
        assert!(
            (red[0] - red[1]).abs() < 1e-6 && (red[1] - red[2]).abs() < 1e-6,
            "red desaturated: {red:?}"
        );
        let cyan = [0.1, 0.8, 0.8];
        assert_eq!(color_mix(cyan, &bands), cyan, "cyan untouched");
    }

    #[test]
    fn color_mix_value_preserves_highlight_headroom() {
        // Boosting the red band's luminance scales value (max channel) above 1,
        // proving the HSV round trip carries the unbounded range through.
        let mut bands = [[0.0_f32; 3]; 8];
        bands[0] = [0.0, 0.0, 1.0]; // red band: value ×2
        let out = color_mix([0.8, 0.1, 0.1], &bands);
        assert!((out[0] - 1.6).abs() < 1e-5, "value doubled: {out:?}");
    }

    #[test]
    fn hsv_round_trips_a_saturated_color() {
        let px = [0.8, 0.3, 0.1];
        let (h, s, v) = rgb_to_hsv(px);
        let back = hsv_to_rgb(h, s, v);
        for c in 0..3 {
            assert!((back[c] - px[c]).abs() < 1e-6, "{back:?} vs {px:?}");
        }
    }

    #[test]
    fn row_normalized_rows_sum_to_one() {
        let m = Mat3([[2.0, 1.0, 1.0], [0.0, 3.0, 1.0], [1.0, 1.0, 2.0]]).row_normalized();
        for r in 0..3 {
            assert!((m.0[r].iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn hsv_achromatic_returns_zero_hue_and_saturation() {
        // A pure gray has no chroma: saturation 0, a finite (0) hue, value = the
        // gray. The `c <= 1e-9` / `max <= 0.0` branches must not produce NaN.
        let (h, s, v) = rgb_to_hsv([0.4, 0.4, 0.4]);
        assert_eq!((h, s, v), (0.0, 0.0, 0.4));
        // Pure black: max == 0 → saturation 0, no division by zero.
        let (h, s, v) = rgb_to_hsv([0.0, 0.0, 0.0]);
        assert!(h.is_finite() && s == 0.0 && v == 0.0, "{h} {s} {v}");
    }

    #[test]
    fn hsv_to_rgb_sextant_is_clamped_at_the_wheel_end() {
        // A hue that lands exactly at (or just past) the wheel end must stay valid.
        // `h6 as u32` could reach 6 for a slightly-negative hue; the clamp keeps it
        // in the last arm. The reconstructed color is finite and on the gray axis for
        // s = 0, and a pure-red hue (0) reconstructs to red.
        let red = hsv_to_rgb(0.0, 1.0, 1.0);
        assert!(
            (red[0] - 1.0).abs() < 1e-6 && red[1] < 1e-6 && red[2] < 1e-6,
            "{red:?}"
        );
        // A tiny-negative hue (which rem_euclid can round to exactly 1.0) must not
        // panic or jump channels — it stays adjacent to pure red.
        let nearly = hsv_to_rgb(-1e-9, 1.0, 1.0);
        for c in nearly {
            assert!(c.is_finite(), "non-finite at wheel end: {nearly:?}");
        }
        assert!((nearly[0] - 1.0).abs() < 1e-3, "should be ~red: {nearly:?}");
    }

    #[test]
    fn color_mix_at_a_band_center_uses_only_that_band() {
        // Hue 0 is the center of band 0; an adjustment there is driven only by band 0
        // (interpolation weight 1 on band 0, 0 on its neighbor).
        let mut bands = [[0.0_f32; 3]; 8];
        bands[0] = [0.0, -1.0, 0.0]; // desaturate band 0
        let red = color_mix([0.8, 0.1, 0.1], &bands);
        assert!(
            (red[0] - red[1]).abs() < 1e-6 && (red[1] - red[2]).abs() < 1e-6,
            "band-center red desaturated: {red:?}"
        );
    }

    #[test]
    fn color_mix_hue_at_wraparound_stays_correct() {
        // A pixel whose hue is just below 1.0 sits between the last band (7) and band
        // 0 (the wraparound `j = (i+1) % 8`). A shift that pushes the hue past 1.0
        // must reconstruct without a channel jump (this exercises the negative/over-
        // unit hue path into hsv_to_rgb).
        let mut bands = [[0.0_f32; 3]; 8];
        bands[7] = [0.05, 0.0, 0.0]; // nudge hue forward at band 7
        bands[0] = [0.05, 0.0, 0.0];
        // A magenta-ish pixel near hue ~0.92.
        let px = [0.8, 0.1, 0.5];
        let out = color_mix(px, &bands);
        for c in out {
            assert!(c.is_finite() && c >= 0.0, "wraparound produced {out:?}");
        }
    }

    #[test]
    fn color_mix_preserves_super_unit_and_clamps_value_floor() {
        // Value above 1 (headroom) survives the round trip; a strong negative lum
        // adjustment can't drive value below 0.
        let mut bands = [[0.0_f32; 3]; 8];
        bands[0] = [0.0, 0.0, -2.0]; // value ×(1 - 2) = ×-1 → clamped to 0
        let out = color_mix([0.9, 0.1, 0.1], &bands);
        for c in out {
            assert!(c >= 0.0 && c.is_finite(), "value floor not held: {out:?}");
        }
    }

    #[test]
    fn rgb_to_hsv_handles_non_finite_inputs() {
        // Pin the `f32::max`/`min` NaN-dropping behavior. For `[NaN, 0.2, 0.1]`,
        // `max`/`min` drop the NaN, so `max == 0.2` (the green channel) and
        // `min == 0.1`; the saturation `c / max` is therefore finite. The hue formula
        // still references the NaN red channel, so hue is NaN — documenting that the
        // max/min are robust to NaN while the per-channel arithmetic is not. The key
        // contract is that the function returns rather than panicking.
        let (_h, s, v) = rgb_to_hsv([f32::NAN, 0.2, 0.1]);
        assert!(
            s.is_finite(),
            "max/min should drop NaN, leaving finite sat: {s}"
        );
        assert_eq!(v, 0.2, "value is the NaN-dropped max");
        // Inf in the dominant channel: max is Inf, so saturation `c / max` is the
        // Inf/Inf indeterminate (NaN) — but the call still returns; value carries Inf.
        let (_h, _s, v) = rgb_to_hsv([f32::INFINITY, 0.0, 0.0]);
        assert!(v.is_infinite(), "value should carry the Inf: {v}");
    }

    #[test]
    fn inverse_just_above_singular_threshold_inverts() {
        // A determinant just above the 1e-12 cutoff still inverts (composing both
        // directions is near-identity); just below it returns None.
        let above = Mat3([[1e-3, 0.0, 0.0], [0.0, 1e-3, 0.0], [0.0, 0.0, 1e-3]]);
        // det = 1e-9 > 1e-12 → invertible.
        let inv = above.inverse().expect("det 1e-9 is above the cutoff");
        assert!(approx_eq(&above.mul(&inv), &Mat3::IDENTITY, 1e-3));

        // det just below the cutoff (a near-zero diagonal) → singular.
        let below = Mat3([[1e-4, 0.0, 0.0], [0.0, 1e-4, 0.0], [0.0, 0.0, 1e-5]]);
        // det = 1e-13 < 1e-12 → None.
        assert!((below.det()).abs() < 1e-12);
        assert_eq!(below.inverse(), None);
    }

    /// A tiny xorshift PRNG for deterministic, dependency-free sweeps.
    fn lcg(state: &mut u64) -> f32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        // Map to [0, 1).
        (*state >> 40) as f32 / (1u64 << 24) as f32
    }

    #[test]
    fn hsv_round_trip_seeded_sweep() {
        // Round-trip a spread of saturated colors; HSV → RGB → HSV → RGB must return
        // the same RGB to f32 precision.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..2000 {
            let px = [lcg(&mut seed), lcg(&mut seed), lcg(&mut seed)];
            let (h, s, v) = rgb_to_hsv(px);
            let back = hsv_to_rgb(h, s, v);
            for c in 0..3 {
                assert!(
                    (back[c] - px[c]).abs() < 1e-5,
                    "round-trip {back:?} vs {px:?}"
                );
            }
        }
    }

    #[test]
    fn mat3_inverse_seeded_sweep() {
        // For random matrices that are comfortably non-singular, A·A⁻¹ ≈ I.
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        let mut checked = 0;
        for _ in 0..2000 {
            let m = Mat3(std::array::from_fn(|_| {
                std::array::from_fn(|_| lcg(&mut seed) * 2.0 - 1.0)
            }));
            if m.det().abs() < 1e-2 {
                continue; // skip near-singular matrices (ill-conditioned in f32)
            }
            let inv = m.inverse().expect("non-singular");
            assert!(approx_eq(&m.mul(&inv), &Mat3::IDENTITY, 1e-2), "{m:?}");
            checked += 1;
        }
        assert!(checked > 100, "sweep covered too few matrices: {checked}");
    }
}
