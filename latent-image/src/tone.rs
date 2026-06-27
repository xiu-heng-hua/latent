//! 1-D tone curves, evaluated in a perceptual (non-linear) domain.
//!
//! A tone curve maps an input tone to an output tone. It is stored as a lookup
//! table (LUT) so per-pixel evaluation is a cheap interpolated lookup rather
//! than recomputing a function. Curves are applied in the perceptual **L\***
//! domain, not raw linear light: equal steps in linear energy are not equal
//! *perceived* steps, so shaping contrast directly in linear gives harsh
//! results. `apply_linear` handles the encode → curve → decode round-trip.
//!
//! The perceptual domain is CIE **L\*** (a cube-root-shaped lightness that is
//! roughly uniform per perceived step), applied to each channel independently:
//! a channel's value is encoded as `L*(value)/100` so the reference white is
//! `1.0` and perceptual mid-gray (linear 0.18) lands near `0.5`. This is a
//! perceptual *encoding* — the L\* transfer per channel — not a move to grading
//! the pixel's colorimetric lightness (saturation and the hue mixer do that in
//! true LCh). L\* is a genuinely uniform space (γ2.2 only approximates its
//! cube-root toe), so the contrast/highlight/shadow/black pivots sit where they
//! should perceptually.

use crate::color::{l_star, l_star_inv};

/// Number of samples in a curve's lookup table.
pub const LUT_SIZE: usize = 256;

/// Perceptual mid-gray in the L\* tone domain: the encoded position of linear
/// mid-gray (`0.18`). L\* of 0.18·white is ≈49.5, so this is ≈0.495 — the pivot
/// the contrast S-curve and the shape polynomials key off, rather than a bare
/// `0.5` that would only be right by coincidence. Shared so every shape pivots
/// on the same perceptual middle.
pub const MID_GRAY: f32 = 0.494_961_1; // l_star(0.18) / 100

/// Encode a linear-light channel value into the perceptual L\* tone domain
/// (`[0, 1]` over `[black, reference white]`). Above `1.0` the encode keeps
/// climbing (headroom is not clipped here); the `eval` headroom branch then
/// passes those values through with unit slope.
///
/// Public so a backend that evaluates the curve itself (e.g. on the GPU) and the
/// clarity midtone window use the *same* encode, keeping the perceptual middle
/// consistent across the pipeline.
pub fn encode(linear: f32) -> f32 {
    l_star(linear.max(0.0)) / 100.0
}

/// Inverse of [`encode`]: decode a perceptual L\* tone position back to
/// linear-light proportional value.
pub fn decode(encoded: f32) -> f32 {
    l_star_inv(encoded.max(0.0) * 100.0)
}

/// A 1-D tone curve backed by a uniformly-sampled lookup table over `[0, 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ToneCurve {
    /// Output values sampled at inputs `i / (LUT_SIZE - 1)`.
    lut: Vec<f32>,
}

impl ToneCurve {
    /// Build a curve by sampling `f` (an input→output map on `[0, 1]`).
    pub fn from_fn(f: impl Fn(f32) -> f32) -> Self {
        let lut = (0..LUT_SIZE)
            .map(|i| f(i as f32 / (LUT_SIZE - 1) as f32))
            .collect();
        Self { lut }
    }

    /// The identity curve (output = input): a no-op.
    pub fn identity() -> Self {
        Self::from_fn(|t| t)
    }

    /// Evaluate the curve at `t` (clamped below at `0`), linearly interpolating
    /// between the two nearest table entries.
    ///
    /// The bend is applied only on `[0, 1]`. Above `1.0` — highlight headroom in
    /// the perceptual domain — the curve passes the value through with **unit
    /// slope** (`eval(1) + (t - 1)`): headroom is preserved (shape, don't crush),
    /// not flattened to the curve's end slope. For the identity curve this is a
    /// straight pass-through; for a shaped (e.g. contrast) curve the bend simply
    /// stops at white instead of soft-clipping the highlights.
    pub fn eval(&self, t: f32) -> f32 {
        let n = self.lut.len();
        if t > 1.0 {
            return self.lut[n - 1] + (t - 1.0);
        }
        let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = pos.floor() as usize;
        if i >= n - 1 {
            return self.lut[n - 1];
        }
        let frac = pos - i as f32;
        self.lut[i] * (1.0 - frac) + self.lut[i + 1] * frac
    }

    /// Apply the curve to a linear-light value: move into the perceptual L\*
    /// domain, apply the curve, then return to linear. This is where "curves act
    /// on perceived tone, not raw linear energy" actually happens.
    pub fn apply_linear(&self, linear: f32) -> f32 {
        decode(self.eval(encode(linear)))
    }

    /// The lookup table: `LUT_SIZE` output samples at inputs `i / (LUT_SIZE - 1)`
    /// in the perceptual L\* domain. A backend that evaluates the curve itself
    /// (e.g. on the GPU) uploads this and reproduces [`Self::eval`]'s
    /// interpolation.
    pub fn lut(&self) -> &[f32] {
        &self.lut
    }
}

/// How fast `contrast`'s steepness grows with the amount, per unit. The S-curve
/// raises each side of the pivot to the power `exp(±CONTRAST_RATE·amount)`; this
/// sets that exponent's growth so the slider's useful range gives a strong but
/// natural contrast push without needing to be clamped.
const CONTRAST_RATE: f32 = 1.2;

// The following four controls are all shapes of the same tone curve. Each takes
// an `amount` (0 = no change) and returns a strictly increasing (monotone) curve.

/// Contrast: an S-curve pivoting around perceptual mid-gray ([`MID_GRAY`]).
/// Positive pushes tones away from the middle (more contrast), negative pulls
/// them toward it.
///
/// Built as a **power-pivot** curve, monotone for *every* amount by
/// construction — so it never inverts and needs no range clamp (unlike a
/// smoothstep blend, whose slope goes negative past `amount = 1`). Each side of
/// the pivot `p` is remapped through `x^k` with `k = exp(CONTRAST_RATE·amount)`
/// and rescaled to fix the endpoints and the pivot: `k > 1` steepens the middle
/// (more contrast), `k < 1` flattens it. Since `x^k` is strictly increasing for
/// any `k > 0` and `exp(·) > 0` always, `f' > 0` everywhere and the full slider
/// range stays usable.
pub fn contrast(amount: f32) -> ToneCurve {
    let p = MID_GRAY;
    let k = (CONTRAST_RATE * amount).exp();
    ToneCurve::from_fn(move |t| {
        if t <= p {
            // Lower half: map [0, p] through x^k, keeping 0→0 and p→p.
            p * (t / p).powf(k)
        } else {
            // Upper half: mirror it so the curve is point-symmetric about (p, p),
            // keeping p→p and 1→1.
            1.0 - (1.0 - p) * ((1.0 - t) / (1.0 - p)).powf(k)
        }
    })
}

/// Highlights: lift or recover the bright tones (upper mids), leaving the
/// endpoints fixed. Positive brightens, negative recovers.
pub fn highlights(amount: f32) -> ToneCurve {
    ToneCurve::from_fn(|t| (t + amount * t * t * (1.0 - t)).clamp(0.0, 1.0))
}

/// Shadows: lift or deepen the dark tones (lower mids), endpoints fixed.
/// Positive lifts, negative deepens.
pub fn shadows(amount: f32) -> ToneCurve {
    ToneCurve::from_fn(|t| (t + amount * t * (1.0 - t) * (1.0 - t)).clamp(0.0, 1.0))
}

/// Blacks: move the black point — the deepest tones (the toe). Positive lifts
/// the floor, negative crushes it.
pub fn blacks(amount: f32) -> ToneCurve {
    ToneCurve::from_fn(|t| (t + amount * 0.25 * (1.0 - t).powi(4)).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_eval_returns_input() {
        let c = ToneCurve::identity();
        for &t in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!((c.eval(t) - t).abs() < 1e-6, "eval({t}) = {}", c.eval(t));
        }
    }

    #[test]
    fn eval_interpolates_a_known_curve() {
        let c = ToneCurve::from_fn(|t| t * t);
        assert!((c.eval(0.5) - 0.25).abs() < 1e-3);
        assert!((c.eval(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(c.eval(0.0), 0.0);
    }

    #[test]
    fn eval_passes_headroom_with_unit_slope() {
        // Below 0 clamps to the floor; above 1.0 every curve passes through with
        // unit slope (eval(1) + (t - 1)) — headroom is preserved, never crushed.
        let id = ToneCurve::from_fn(|t| t);
        assert_eq!(id.eval(-1.0), 0.0);
        assert!(
            (id.eval(2.0) - 2.0).abs() < 1e-3,
            "headroom: {}",
            id.eval(2.0)
        );

        // A shaped (contrast) curve must NOT soft-clip past 1.0: with unit slope,
        // eval(2.0) ≈ eval(1.0) + 1.0, not the flattened end-slope value the old
        // extrapolation produced (which crushed a highlight toward ~1.04).
        let c = contrast(0.6);
        let at_one = c.eval(1.0);
        assert!(
            (c.eval(2.0) - (at_one + 1.0)).abs() < 1e-4,
            "contrast headroom not unit-slope: eval(2)={}, eval(1)+1={}",
            c.eval(2.0),
            at_one + 1.0
        );
    }

    #[test]
    fn apply_linear_preserves_headroom_and_stays_monotonic() {
        // Identity leaves a >1.0 highlight essentially unchanged (round-trips).
        let id = ToneCurve::identity();
        assert!(
            (id.apply_linear(1.5) - 1.5).abs() < 1e-3,
            "{}",
            id.apply_linear(1.5)
        );
        // A real curve still maps a rising highlight ramp to a rising output
        // (gradient kept, no flat-white plateau) and, because the bend stops at
        // white and headroom passes with unit slope, a strong highlight stays a
        // strong highlight rather than soft-clipping toward white.
        let c = contrast(0.6);
        let ramp: Vec<f32> = [1.0, 1.3, 1.7, 2.5]
            .iter()
            .map(|&x| c.apply_linear(x))
            .collect();
        for w in ramp.windows(2) {
            assert!(w[1] > w[0], "highlight ramp not increasing: {ramp:?}");
        }
        // A bright highlight (linear 4.0) survives — far above 1, not clipped near
        // white. The L* round-trip is exact for the unit-slope headroom region.
        assert!(
            c.apply_linear(4.0) > 3.0,
            "headroom crushed: {}",
            c.apply_linear(4.0)
        );
    }

    #[test]
    fn zero_amount_controls_are_identity() {
        for c in [contrast(0.0), highlights(0.0), shadows(0.0), blacks(0.0)] {
            for &t in &[0.0, 0.3, 0.6, 1.0] {
                assert!((c.eval(t) - t).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn contrast_pushes_tones_away_from_mid() {
        let c = contrast(0.6);
        assert!(c.eval(0.8) > 0.8); // a highlight gets brighter
        assert!(c.eval(0.2) < 0.2); // a shadow gets darker
        // The pivot is perceptual mid-gray (L*≈0.5), not a bare 0.5: it maps to
        // itself, the fixed point the S-curve rotates the tones around.
        assert!(
            (c.eval(MID_GRAY) - MID_GRAY).abs() < 1e-4,
            "pivot moved: {}",
            c.eval(MID_GRAY)
        );
    }

    #[test]
    fn tone_domain_is_lstar() {
        // The tone domain is L*: linear mid-gray (0.18) encodes to ≈0.5 (the pivot),
        // black and white pin the ends, and the round-trip is the identity.
        assert!((encode(0.18) - MID_GRAY).abs() < 1e-5, "{}", encode(0.18));
        assert!(
            (MID_GRAY - 0.5).abs() < 0.01,
            "mid-gray near 0.5: {MID_GRAY}"
        );
        assert!(encode(0.0).abs() < 1e-6);
        assert!((encode(1.0) - 1.0).abs() < 1e-6);
        for &x in &[0.0, 0.05, 0.18, 0.5, 0.9, 1.0, 2.5] {
            assert!((decode(encode(x)) - x).abs() < 1e-4, "round-trip x={x}");
        }
    }

    #[test]
    fn contrast_is_monotone_for_all_amounts() {
        // The power-pivot S-curve is monotone by construction for every amount —
        // it never inverts, even far past the old [-1, 1] clamp. Sweep a wide range
        // and assert the densely-sampled curve never decreases. (At extreme
        // positive amounts the toe values round to equal in f32, so the guarantee
        // is non-decreasing; the slope is strictly positive in exact arithmetic.)
        for a in [-3.0, -2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0, 3.0] {
            let c = contrast(a);
            let lut = c.lut();
            for w in lut.windows(2) {
                assert!(w[1] >= w[0], "contrast({a}) inverted: {} -> {}", w[0], w[1]);
            }
            // Endpoints and pivot stay pinned for every amount.
            assert!(lut[0].abs() < 1e-6, "contrast({a}) floor: {}", lut[0]);
            assert!(
                (lut[lut.len() - 1] - 1.0).abs() < 1e-5,
                "contrast({a}) ceil: {}",
                lut[lut.len() - 1]
            );
            // The pivot is exact in the function; eval samples the LUT, so at
            // steep amounts the pivot (which falls between grid points) carries a
            // small interpolation error — a few×1e-3, not an inversion.
            assert!(
                (c.eval(MID_GRAY) - MID_GRAY).abs() < 3e-3,
                "contrast({a}) pivot: {}",
                c.eval(MID_GRAY)
            );
        }
    }

    #[test]
    fn contrast_is_strictly_increasing_in_the_useful_range() {
        // Over the slider's useful range the curve is strictly increasing (no
        // plateau): every successive LUT sample is greater than the last.
        for a in [-1.0, -0.5, 0.5, 1.0] {
            let c = contrast(a);
            for w in c.lut().windows(2) {
                assert!(w[1] > w[0], "contrast({a}) not strictly rising");
            }
        }
    }

    #[test]
    fn contrast_extends_past_unit_amount() {
        // An amount beyond the old [-1, 1] clamp still yields a valid monotone
        // curve that pushes contrast harder than amount = 1 — the range is no
        // longer capped. A highlight at amount 2 is brighter than at amount 1.
        let strong = contrast(2.0);
        let unit = contrast(1.0);
        assert!(
            strong.eval(0.8) > unit.eval(0.8),
            "amount 2 should out-contrast amount 1"
        );
        for w in strong.lut().windows(2) {
            assert!(w[1] >= w[0], "contrast(2.0) inverted");
        }
    }

    #[test]
    fn highlights_move_the_bright_tones() {
        let c = highlights(0.6);
        assert!(c.eval(0.75) > 0.75); // bright tone lifts
        assert_eq!(c.eval(0.0), 0.0); // black untouched
        assert!((c.eval(1.0) - 1.0).abs() < 1e-6); // white endpoint fixed
    }

    #[test]
    fn shadows_move_the_dark_tones() {
        let c = shadows(0.6);
        assert!(c.eval(0.25) > 0.25); // dark tone lifts
        assert!((c.eval(1.0) - 1.0).abs() < 1e-6); // white untouched
    }

    #[test]
    fn blacks_move_the_floor() {
        let lifted = blacks(0.6);
        assert!(lifted.eval(0.0) > 0.0); // black point lifts
        assert!((lifted.eval(1.0) - 1.0).abs() < 1e-6); // white untouched
    }

    #[test]
    fn identity_apply_linear_is_a_noop() {
        // Even with the encode/decode round-trip, identity leaves values alone.
        let c = ToneCurve::identity();
        for &x in &[0.1, 0.3, 0.5, 0.9] {
            assert!((c.apply_linear(x) - x).abs() < 1e-5, "x={x}");
        }
    }

    #[test]
    fn eval_below_zero_clamps_to_the_floor() {
        // Inputs below 0 clamp to the first table entry rather than extrapolating
        // downward (the lower end is the toe; there is no headroom below black).
        let c = ToneCurve::from_fn(|t| t * t);
        assert_eq!(c.eval(-0.5), c.lut()[0]);
        assert_eq!(c.eval(-100.0), c.lut()[0]);
        // The first table entry of `t*t` is 0.
        assert_eq!(c.eval(-1.0), 0.0);
    }

    #[test]
    fn eval_at_one_hits_the_last_entry_via_the_clamp() {
        // Exactly 1.0 maps to `pos == n - 1`, so the `i >= n - 1` clamp returns the
        // final table entry exactly (no out-of-range `lut[i + 1]` read).
        let c = ToneCurve::from_fn(|t| 0.5 * t + 0.25);
        let last = *c.lut().last().unwrap();
        assert_eq!(c.eval(1.0), last);
        assert!((last - 0.75).abs() < 1e-6, "last entry: {last}");
    }

    #[test]
    fn eval_high_extrapolation_uses_unit_slope_not_the_end_slope() {
        // Above 1.0 the curve passes through with unit slope: `eval(1) + (t - 1)`,
        // independent of the curve's shape near white. A steep curve whose end
        // slope is 2 must NOT extrapolate at slope 2 — headroom passes at slope 1.
        let c = ToneCurve::from_fn(|t| 2.0 * t.min(0.5)); // saturates to 1.0 at t≥0.5
        // eval(1.0) is the last entry (1.0); eval(1.5) is 1.0 + 0.5 = 1.5.
        assert!(
            (c.eval(1.5) - (c.eval(1.0) + 0.5)).abs() < 1e-4,
            "headroom not unit-slope: {}",
            c.eval(1.5)
        );
        // The identity curve passes >1 straight through.
        let id = ToneCurve::identity();
        assert!(
            (id.eval(3.0) - 3.0).abs() < 1e-4,
            "identity headroom: {}",
            id.eval(3.0)
        );
    }

    #[test]
    fn lut_error_bounded_in_lstar() {
        // The 256-entry LUT interpolates a smooth curve; re-measure its
        // interpolation error in the L* domain (the cube-root toe is steeper than
        // γ2.2 near black, so this is the worst case). For a representative shaped
        // curve, the linear interpolation between table entries stays well under
        // an LSB at 16-bit depth (1/65535 ≈ 1.5e-5) of the reference function —
        // i.e. the LUT is indistinguishable from evaluating the function directly.
        let amount = 0.6_f32;
        let p = MID_GRAY;
        let k = (CONTRAST_RATE * amount).exp();
        let reference = |t: f32| {
            if t <= p {
                p * (t / p).powf(k)
            } else {
                1.0 - (1.0 - p) * ((1.0 - t) / (1.0 - p)).powf(k)
            }
        };
        let c = contrast(amount);
        let mut max_err = 0.0_f32;
        // Sample densely, including points between table entries where the linear
        // interpolation deviates most from the true (curved) function.
        for i in 0..=100_000 {
            let t = i as f32 / 100_000.0;
            max_err = max_err.max((c.eval(t) - reference(t)).abs());
        }
        assert!(
            max_err < 1.0 / 65535.0,
            "LUT interpolation error {max_err} exceeds a 16-bit LSB in L*"
        );
    }
}
