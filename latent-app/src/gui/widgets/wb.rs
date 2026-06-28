//! White-balance tools: a Kelvin + tint UI over the engine's `WhiteBalance`
//! offset, a gray eyedropper, and presets — all writing the single
//! `WhiteBalance { temp, tint }` field the engine already interprets.
//!
//! The engine lowers white balance to per-channel gains with green anchored:
//! `(rg, gg, bg) = (1 + temp, 1 - tint, 1 - temp)`. Everything here is a UI-side
//! conversion onto that one model; no render math changes.
//!
//! ## Kelvin ↔ offset mapping
//!
//! There is no absolute Kelvin in the model, so the UI maps a Kelvin slider onto
//! `temp` with a monotonic, invertible mired-difference curve anchored at the
//! reference `T0` (so `T0` ⇒ `temp = 0`, neutral):
//!
//! ```text
//! temp = KELVIN_GAIN * (1 / T - 1 / T0)      (forward)
//! T    = 1 / (1 / T0 + temp / KELVIN_GAIN)   (inverse)
//! ```
//!
//! Mireds (`1e6 / T`) are perceptually even and the relation is trivially
//! invertible, so a stored `temp` shows a sensible Kelvin back. A warmer (lower
//! Kelvin) target gives a positive `temp` (warmer = more red, less blue), matching
//! the engine's antisymmetric `(1 + temp, …, 1 - temp)`. Tint maps linearly:
//! `tint = TINT_GAIN * slider`, invertibly.

use latent_edit::WhiteBalance;

/// The reference color temperature (Kelvin) that maps to a neutral `temp = 0`
/// — daylight / "as shot" baseline.
pub(crate) const T0: f32 = 5500.0;

/// The slider's Kelvin range.
pub(crate) const KELVIN_MIN: f32 = 2000.0;
pub(crate) const KELVIN_MAX: f32 = 12000.0;

/// Scales the mired difference into `temp`. Chosen so the slider ends land in a
/// sane `temp` range (roughly ±0.5), well inside the unbounded field: at
/// `KELVIN_MIN` the mired difference is `1/2000 - 1/5500 ≈ +3.18e-4`, so with this
/// gain `temp ≈ +0.48` (warm); at `KELVIN_MAX`, `temp ≈ -0.18` (cool).
pub(crate) const KELVIN_GAIN: f32 = 1500.0;

/// The tint slider range (green ↔ magenta) and its linear gain onto `tint`.
pub(crate) const TINT_RANGE: f32 = 150.0;
/// `tint = TINT_GAIN * slider`; chosen so the slider ends reach ±0.3 `tint`.
pub(crate) const TINT_GAIN: f32 = 0.3 / TINT_RANGE;

/// Map a Kelvin temperature to the engine's `temp` offset (mired-difference,
/// anchored at [`T0`]). Monotonic decreasing in Kelvin: warmer (lower K) → warmer
/// (positive `temp`).
pub(crate) fn kelvin_to_temp(kelvin: f32) -> f32 {
    KELVIN_GAIN * (1.0 / kelvin - 1.0 / T0)
}

/// The inverse of [`kelvin_to_temp`]: the Kelvin a stored `temp` corresponds to,
/// clamped into the slider's range so a wild stored offset still shows a usable
/// value. Returns [`T0`] for the degenerate (would-be-infinite) denominator.
pub(crate) fn temp_to_kelvin(temp: f32) -> f32 {
    let denom = 1.0 / T0 + temp / KELVIN_GAIN;
    if denom.abs() < 1e-9 {
        return T0;
    }
    (1.0 / denom).clamp(KELVIN_MIN, KELVIN_MAX)
}

/// Map a tint slider value (green ↔ magenta) to the engine's `tint` offset.
pub(crate) fn slider_to_tint(slider: f32) -> f32 {
    TINT_GAIN * slider
}

/// The inverse of [`slider_to_tint`].
pub(crate) fn tint_to_slider(tint: f32) -> f32 {
    tint / TINT_GAIN
}

/// The **absolute** `WhiteBalance` that neutralizes a sampled patch, given the
/// patch's **post-WB** linear working RGB (the pixel as currently rendered, with
/// the current offset already baked in) and the current offset `current`.
///
/// The engine renders `out = in · (1 + t, 1 - tint, 1 - t)` with green anchored.
/// Because that gain is *linear-antisymmetric* in `temp` (not multiplicative),
/// composing two offsets by multiplying their gains would not preserve the single
/// `temp` shared by red and blue. So we instead recover the original pre-WB pixel
/// by dividing the sample back through the current gain, then solve for the
/// absolute offset that neutralizes *that* pixel directly:
///
/// - `pre = sample / (1 + t, 1 - tint, 1 - t)`.
/// - Balance red against blue: `pre_r · (1 + temp) = pre_b · (1 - temp)` ⇒
///   `temp = (pre_b - pre_r) / (pre_b + pre_r)`.
/// - Match green to the balanced red/blue level `rb = pre_r · (1 + temp)`:
///   `pre_g · (1 - tint) = rb` ⇒ `tint = 1 - rb / pre_g`.
///
/// Guards a non-positive channel sum or green, and a degenerate current gain
/// (`tint = 1` zeroing green), by leaving the offset unchanged.
pub(crate) fn neutralizing_wb(sample: [f32; 3], current: WhiteBalance) -> WhiteBalance {
    let [r, g, b] = sample;
    if !(r.is_finite() && g.is_finite() && b.is_finite()) {
        return current;
    }
    // Undo the current gain to recover the original (pre-WB) pixel.
    let (rg, gg, bg) = (1.0 + current.temp, 1.0 - current.tint, 1.0 - current.temp);
    if rg.abs() < 1e-6 || gg.abs() < 1e-6 || bg.abs() < 1e-6 {
        return current;
    }
    let pre = [r / rg, g / gg, b / bg];
    let sum = pre[0] + pre[2];
    if sum <= 1e-6 || pre[1] <= 1e-6 {
        return current;
    }
    let temp = (pre[2] - pre[0]) / sum;
    let rb = pre[0] * (1.0 + temp); // the common red/blue level after the gain
    let tint = 1.0 - rb / pre[1];
    WhiteBalance {
        temp: if temp.is_finite() { temp } else { current.temp },
        tint: if tint.is_finite() { tint } else { current.tint },
    }
}

/// A named white-balance preset. `As Shot` clears the editable offset to `None`
/// (the as-shot decode balance applies); the rest set a fixed `{temp, tint}`
/// mapped through the same Kelvin function. `Auto` is a gray-world estimate
/// computed from the preview, so it carries no fixed Kelvin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Preset {
    AsShot,
    Auto,
    Daylight,
    Cloudy,
    Shade,
    Tungsten,
    Fluorescent,
    Flash,
}

impl Preset {
    /// Every preset, in panel order.
    pub(crate) const ALL: [Preset; 8] = [
        Preset::AsShot,
        Preset::Auto,
        Preset::Daylight,
        Preset::Cloudy,
        Preset::Shade,
        Preset::Tungsten,
        Preset::Fluorescent,
        Preset::Flash,
    ];

    /// The button label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Preset::AsShot => "As Shot",
            Preset::Auto => "Auto",
            Preset::Daylight => "Daylight",
            Preset::Cloudy => "Cloudy",
            Preset::Shade => "Shade",
            Preset::Tungsten => "Tungsten",
            Preset::Fluorescent => "Fluorescent",
            Preset::Flash => "Flash",
        }
    }

    /// The reference Kelvin for the fixed presets, or `None` for the two dynamic
    /// ones (`As Shot` clears; `Auto` is estimated from the image).
    pub(crate) fn kelvin(self) -> Option<f32> {
        match self {
            Preset::Daylight => Some(5500.0),
            Preset::Cloudy => Some(6500.0),
            Preset::Shade => Some(7500.0),
            Preset::Tungsten => Some(3200.0),
            Preset::Fluorescent => Some(4000.0),
            Preset::Flash => Some(5500.0),
            Preset::AsShot | Preset::Auto => None,
        }
    }
}

/// The estimated gray-world `WhiteBalance` from a preview's average linear RGB:
/// treat the whole-image average as a patch that should be neutral and solve the
/// same neutralization as the eyedropper, composing onto the current offset.
pub(crate) fn auto_wb(mean: [f32; 3], current: WhiteBalance) -> WhiteBalance {
    neutralizing_wb(mean, current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kelvin_round_trips_through_temp() {
        // T0 is neutral, and the forward/inverse map invert across the range so a
        // stored temp shows a sensible Kelvin back.
        assert!(kelvin_to_temp(T0).abs() < 1e-6, "T0 is neutral");
        for &k in &[2000.0_f32, 3200.0, 5500.0, 6500.0, 9000.0, 12000.0] {
            let t = kelvin_to_temp(k);
            let back = temp_to_kelvin(t);
            assert!((back - k).abs() < 1.0, "k={k} round-trips: got {back}");
        }
        // Warmer (lower K) gives a positive (warmer) temp; cooler gives negative.
        assert!(kelvin_to_temp(3200.0) > 0.0, "tungsten is warm");
        assert!(kelvin_to_temp(9000.0) < 0.0, "shade-cool is cool");
    }

    #[test]
    fn temp_stays_in_a_sane_range_over_the_slider() {
        // The slider ends stay well inside the unbounded field (roughly ±0.5).
        let warm = kelvin_to_temp(KELVIN_MIN);
        let cool = kelvin_to_temp(KELVIN_MAX);
        assert!((0.3..0.6).contains(&warm), "warm end: {warm}");
        assert!((-0.3..0.0).contains(&cool), "cool end: {cool}");
    }

    #[test]
    fn tint_slider_round_trips() {
        for s in [-TINT_RANGE, -50.0, 0.0, 50.0, TINT_RANGE] {
            assert!((tint_to_slider(slider_to_tint(s)) - s).abs() < 1e-3);
        }
    }

    #[test]
    fn eyedropper_neutralizes_a_warm_patch_from_neutral() {
        // A warm patch (R high, B low) sampled with no current WB: the computed
        // offset, applied by the engine's gain, drives the patch to neutral
        // (R == G == B).
        let sample = [0.8, 0.6, 0.4];
        let wb = neutralizing_wb(sample, WhiteBalance::default());
        // Apply the engine's gain to the original sample.
        let r = sample[0] * (1.0 + wb.temp);
        let g = sample[1] * (1.0 - wb.tint);
        let b = sample[2] * (1.0 - wb.temp);
        assert!((r - b).abs() < 1e-4, "red==blue: {r} vs {b}");
        assert!((r - g).abs() < 1e-4, "red==green: {r} vs {g}");
    }

    #[test]
    fn eyedropper_composes_onto_a_current_offset() {
        // With a current offset already applied, the sample is the post-WB pixel;
        // the new absolute offset, applied to the *pre-WB* pixel, still neutralizes.
        let pre = [0.7, 0.55, 0.45]; // the true (pre-WB) pixel
        let current = WhiteBalance {
            temp: 0.15,
            tint: -0.05,
        };
        // The post-WB sample the eyedropper actually reads.
        let sample = [
            pre[0] * (1.0 + current.temp),
            pre[1] * (1.0 - current.tint),
            pre[2] * (1.0 - current.temp),
        ];
        let wb = neutralizing_wb(sample, current);
        // The new absolute offset applied to the pre-WB pixel is neutral.
        let r = pre[0] * (1.0 + wb.temp);
        let g = pre[1] * (1.0 - wb.tint);
        let b = pre[2] * (1.0 - wb.temp);
        assert!((r - b).abs() < 1e-4 && (r - g).abs() < 1e-4, "{r} {g} {b}");
    }

    #[test]
    fn eyedropper_guards_degenerate_samples() {
        // A black or zero-green sample leaves the offset unchanged rather than
        // dividing by zero.
        let cur = WhiteBalance {
            temp: 0.2,
            tint: 0.1,
        };
        assert_eq!(neutralizing_wb([0.0, 0.0, 0.0], cur), cur);
        assert_eq!(neutralizing_wb([0.5, 0.0, 0.5], cur), cur);
        assert_eq!(neutralizing_wb([f32::NAN, 0.5, 0.5], cur), cur);
    }

    #[test]
    fn presets_map_to_expected_warmth() {
        // Tungsten (low K) is warm (positive temp); shade (high K) is cool.
        let tungsten = kelvin_to_temp(Preset::Tungsten.kelvin().unwrap());
        let shade = kelvin_to_temp(Preset::Shade.kelvin().unwrap());
        assert!(tungsten > 0.0 && shade < 0.0);
        // Daylight sits at neutral.
        assert!(kelvin_to_temp(Preset::Daylight.kelvin().unwrap()).abs() < 1e-6);
        // The dynamic presets carry no fixed Kelvin.
        assert!(Preset::AsShot.kelvin().is_none());
        assert!(Preset::Auto.kelvin().is_none());
    }
}
