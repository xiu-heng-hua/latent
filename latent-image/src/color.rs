//! Color math: 3x3 matrices and color-space conversions.

use crate::ImageBuf;

/// A 3x3 matrix, row-major. Used for color-space conversions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3(pub [[f32; 3]; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    /// Apply the matrix to a column vector: `self * v`.
    pub fn mul_vec(&self, v: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
    }

    /// Matrix product `self * other`.
    pub fn mul(&self, other: &Mat3) -> Mat3 {
        let (a, b) = (&self.0, &other.0);
        Mat3(std::array::from_fn(|r| {
            std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c])
        }))
    }

    /// Determinant (via the rule of Sarrus).
    pub fn det(&self) -> f32 {
        let m = &self.0;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    /// Scale each row so it sums to 1, so a neutral input `[v,v,v]` maps to a
    /// neutral output `[v,v,v]`. Rows that sum to ~0 are left unchanged.
    pub fn row_normalized(&self) -> Mat3 {
        Mat3(std::array::from_fn(|r| {
            let sum: f32 = self.0[r].iter().sum();
            let sum = if sum.abs() < 1e-12 { 1.0 } else { sum };
            std::array::from_fn(|c| self.0[r][c] / sum)
        }))
    }

    /// The inverse, or `None` if the matrix is singular.
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

/// XYZ (D65) → linear sRGB, the standard matrix for sRGB primaries.
///
/// Defined by the sRGB standard, IEC 61966-2-1.
pub const XYZ_TO_LINEAR_SRGB: Mat3 = Mat3([
    [3.2406, -1.5372, -0.4986],
    [-0.9689, 1.8758, 0.0415],
    [0.0557, -0.2040, 1.0570],
]);

/// Convert a device-independent XYZ color into linear-light sRGB primaries.
pub fn xyz_to_linear_srgb(xyz: [f32; 3]) -> [f32; 3] {
    XYZ_TO_LINEAR_SRGB.mul_vec(xyz)
}

/// Build the linear-RGB → XYZ matrix for an RGB space from its primary
/// chromaticities `[r, g, b]` (each `[x, y]`) and white point `[x, y]`.
///
/// Standard construction (SMPTE RP 177): take each primary's XYZ at unit
/// luminance as a column, then scale the columns so unit RGB reproduces the
/// white point. Deriving the matrix from published chromaticities keeps the
/// numbers verifiable (white → neutral, round-trips) rather than transcribed.
fn rgb_to_xyz(primaries: [[f32; 2]; 3], white: [f32; 2]) -> Mat3 {
    let to_xyz = |c: [f32; 2]| [c[0] / c[1], 1.0, (1.0 - c[0] - c[1]) / c[1]];
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
/// CIE D65 white point `(x, y)`.
const D65_WHITE: [f32; 2] = [0.3127, 0.3290];

/// The working space: **linear ProPhoto primaries at D65**. ProPhoto's wide
/// gamut means saturated camera colors stay in-gamut; pinning it to D65 (rather
/// than ProPhoto's nominal D50) matches the camera matrix and the sRGB output,
/// so no chromatic adaptation is needed anywhere in the pipeline.
pub fn linear_working_to_xyz() -> Mat3 {
    rgb_to_xyz(PROPHOTO_PRIMARIES, D65_WHITE)
}

/// XYZ → linear working RGB.
pub fn xyz_to_linear_working() -> Mat3 {
    linear_working_to_xyz()
        .inverse()
        .expect("working primaries are non-singular")
}

/// Linear working RGB → linear sRGB, for the output transform at export.
///
/// Row-normalized so a neutral working gray maps to an *exactly* neutral sRGB
/// gray: the working matrix is derived from chromaticities but
/// [`XYZ_TO_LINEAR_SRGB`] is the published 4-decimal constant, so their product's
/// rows sum to `1 ± ~1e-4` — a sub-8-bit drift that is ~10 LSB at 16-bit.
/// Pinning neutral removes that tint; chromatic colors shift by the same ~1e-4.
pub fn linear_working_to_linear_srgb() -> Mat3 {
    XYZ_TO_LINEAR_SRGB
        .mul(&linear_working_to_xyz())
        .row_normalized()
}

/// Build the combined **camera → linear working** matrix from the file's
/// XYZ→camera matrix, ready to apply to white-balanced camera RGB.
///
/// Composes camera→XYZ→working, then row-normalizes so each row sums to 1. White
/// balance is already applied once on the mosaic; the row-normalization stops
/// this matrix from re-applying its own implicit white balance (the classic
/// double-apply bug). The net effect: a neutral input stays neutral, and the
/// matrix only rotates color. Returns `None` if the input is singular.
pub fn camera_to_working(xyz_to_cam: Mat3) -> Option<Mat3> {
    let cam_to_xyz = camera_to_xyz(xyz_to_cam)?;
    Some(xyz_to_linear_working().mul(&cam_to_xyz).row_normalized())
}

/// Relative-luminance weights for the linear **working** space (the Y row of
/// [`linear_working_to_xyz`]: ProPhoto primaries at D65). Cross-checked against
/// that matrix in the tests. The GPU shader (`map_pixels.wgsl`) hard-codes the
/// same values for its saturation path — keep the two in sync (the GPU/CPU
/// render-equivalence test guards against drift).
///
/// Note the near-zero blue weight (~0.0001): in these wide primaries blue carries
/// almost no luminance, which is colorimetrically correct but means a fully
/// desaturated pure-blue maps to near-black — by design, not a bug (it would be
/// ~0.07 under Rec. 709/sRGB primaries).
pub const LUMA_WEIGHTS: [f32; 3] = [0.27881965, 0.72106725, 0.000113055];

/// Relative luminance of a linear-light RGB pixel — its perceived brightness.
pub fn luminance(rgb: [f32; 3]) -> f32 {
    LUMA_WEIGHTS[0] * rgb[0] + LUMA_WEIGHTS[1] * rgb[1] + LUMA_WEIGHTS[2] * rgb[2]
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
        let d65 = [0.95047, 1.0, 1.08883];
        let rgb = xyz_to_linear_srgb(d65);
        for c in rgb {
            assert!((c - 1.0).abs() < 1e-3, "expected ~1.0, got {c}");
        }
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

        // After white balance the mosaic, a neutral patch is camera RGB [v,v,v];
        // the matrix must keep it neutral (WB applied exactly once, not twice).
        for v in [0.25_f32, 0.5, 1.0] {
            let out = m.mul_vec([v, v, v]);
            assert!((out[0] - v).abs() < 1e-5, "R drifted: {out:?}");
            assert!((out[1] - v).abs() < 1e-5, "G drifted: {out:?}");
            assert!((out[2] - v).abs() < 1e-5, "B drifted: {out:?}");
        }
    }

    #[test]
    fn working_space_white_is_neutral_and_round_trips() {
        // Unit working RGB must be the D65 white in XYZ, and XYZ→working→XYZ
        // must round-trip (so the derived primaries matrix is self-consistent).
        let to_xyz = linear_working_to_xyz();
        let white_xyz = to_xyz.mul_vec([1.0, 1.0, 1.0]);
        let d65 = [0.3127 / 0.3290, 1.0, (1.0 - 0.3127 - 0.3290) / 0.3290];
        for c in 0..3 {
            assert!((white_xyz[c] - d65[c]).abs() < 1e-4, "white: {white_xyz:?}");
        }
        let back = xyz_to_linear_working().mul(&to_xyz);
        assert!(
            approx_eq(&back, &Mat3::IDENTITY, 1e-5),
            "round-trip: {back:?}"
        );
    }

    #[test]
    fn working_to_srgb_keeps_neutral_neutral() {
        // The output transform must leave a neutral gray neutral (both spaces D65).
        let m = linear_working_to_linear_srgb();
        for v in [0.25_f32, 0.5, 1.0] {
            let out = m.mul_vec([v, v, v]);
            for c in out {
                // ~1e-3 tolerance: the published 4-decimal XYZ→sRGB const isn't
                // perfectly consistent with the derived working matrix (drift
                // ≈ 0.04 of an 8-bit level — sub-quantization).
                assert!((c - v).abs() < 1e-3, "neutral drifted: {out:?}");
            }
        }
    }

    #[test]
    fn luma_weights_match_the_working_matrix() {
        // LUMA_WEIGHTS is the Y (second) row of the working RGB→XYZ matrix; this
        // catches any transcription error in the const.
        let y_row = linear_working_to_xyz().0[1];
        for c in 0..3 {
            assert!(
                (LUMA_WEIGHTS[c] - y_row[c]).abs() < 1e-4,
                "LUMA_WEIGHTS{LUMA_WEIGHTS:?} vs Y row {y_row:?}"
            );
        }
        assert!((LUMA_WEIGHTS.iter().sum::<f32>() - 1.0).abs() < 1e-4);
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
    fn row_normalized_rows_sum_to_one() {
        let m = Mat3([[2.0, 1.0, 1.0], [0.0, 3.0, 1.0], [1.0, 1.0, 2.0]]).row_normalized();
        for r in 0..3 {
            assert!((m.0[r].iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
    }
}
