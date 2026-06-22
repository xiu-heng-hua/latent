//! 1-D tone curves, evaluated in a perceptual (non-linear) domain.
//!
//! A tone curve maps an input tone to an output tone. It is stored as a lookup
//! table (LUT) so per-pixel evaluation is a cheap interpolated lookup rather
//! than recomputing a function. Curves are applied in a gamma (perceptual)
//! domain, not raw linear light: equal steps in linear energy are not equal
//! *perceived* steps, so shaping contrast directly in linear gives harsh
//! results. `apply_linear` handles the encode → curve → decode round-trip.

/// Number of samples in a curve's lookup table.
pub const LUT_SIZE: usize = 256;

/// Approximate perceptual domain: a plain gamma. A 2.2 gamma is a simple,
/// reasonable perceptual space for tone shaping.
const GAMMA: f32 = 2.2;

fn tone_encode(linear: f32) -> f32 {
    linear.clamp(0.0, 1.0).powf(1.0 / GAMMA)
}

fn tone_decode(encoded: f32) -> f32 {
    encoded.clamp(0.0, 1.0).powf(GAMMA)
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

    /// Evaluate the curve at `t` (clamped to `[0, 1]`), linearly interpolating
    /// between the two nearest table entries.
    pub fn eval(&self, t: f32) -> f32 {
        let n = self.lut.len();
        let pos = t.clamp(0.0, 1.0) * (n - 1) as f32;
        let i = pos.floor() as usize;
        if i >= n - 1 {
            return self.lut[n - 1];
        }
        let frac = pos - i as f32;
        self.lut[i] * (1.0 - frac) + self.lut[i + 1] * frac
    }

    /// Apply the curve to a linear-light value: move into the perceptual domain,
    /// apply the curve, then return to linear. This is where "curves act on
    /// perceived tone, not raw linear energy" actually happens.
    pub fn apply_linear(&self, linear: f32) -> f32 {
        tone_decode(self.eval(tone_encode(linear)))
    }
}

/// A smooth S-shaped ramp through (0,0), (0.5,0.5), (1,1) with flat ends.
fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

// The following four controls are all shapes of the same tone curve. Each takes
// an `amount` (roughly [-1, 1], 0 = no change) and returns a monotonic curve.

/// Contrast: an S-curve pivoting around mid-gray. Positive pushes tones away
/// from the middle (more contrast), negative pulls them toward it.
pub fn contrast(amount: f32) -> ToneCurve {
    ToneCurve::from_fn(|t| (t + amount * (smoothstep(t) - t)).clamp(0.0, 1.0))
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
    fn eval_clamps_out_of_range_inputs() {
        let c = ToneCurve::from_fn(|t| t);
        assert_eq!(c.eval(-1.0), 0.0);
        assert_eq!(c.eval(2.0), 1.0);
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
        assert!((c.eval(0.5) - 0.5).abs() < 1e-6); // mid-gray is the pivot
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
}
