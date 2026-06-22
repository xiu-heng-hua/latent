//! Rendering pipeline and the backend abstraction.
//!
//! [`render`] is the fixed, ordered pipeline. It owns the order; a [`Backend`]
//! only provides the pixel-level primitives it calls. Naming the coordinate
//! spaces is deliberate: all adjustments act on the full, uncropped image
//! (SOURCE space), and geometry is the single later step that reframes it
//! (SOURCE → OUTPUT).

use latent_edit::{Adjustments, Geometry, LocalAdjustment, SelectiveTone, Settings};
use latent_image::ImageBuf;
use latent_image::tone::{self, ToneCurve};

/// A data-described per-pixel operation over linear-light RGB pixels.
///
/// Point operations are *data*, not code: the pipeline builds them and each
/// backend gives them meaning (interpreted on the CPU now, dispatched however a
/// future backend likes). Describing them as data — rather than as an opaque
/// closure — is what lets a backend run them anywhere. More variants are added
/// as adjustments land.
#[derive(Debug, Clone, PartialEq)]
pub enum PointOp {
    /// Multiply each channel by its own gain (white balance, exposure).
    Gain([f32; 3]),
    /// Apply a tone curve to each channel, in its perceptual domain.
    Tone(ToneCurve),
    /// Blend each channel between its luma (grayscale) and itself by `amount`
    /// (`0` = grayscale, `1` = unchanged).
    Saturation(f32),
}

/// The pixel-level primitives a rendering backend provides.
///
/// The pipeline calls these in a fixed order; the order lives in [`render`],
/// never in a backend. A backend may implement the primitives however it likes
/// (on the CPU now, elsewhere later) as long as the results match. More
/// primitives are added to this trait as the pipeline grows.
pub trait Backend {
    /// Apply a per-pixel operation to every pixel of the image, in place.
    fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp);
}

/// Render a finished working image from a source image and its settings.
///
/// `source` is the linear working image in SOURCE coordinates — the full,
/// uncropped develop (decode → white balance → demosaic → camera-to-working).
/// The fixed pipeline then applies, in order: global adjustments, local
/// adjustments, and geometry (the single SOURCE → OUTPUT step). The final
/// output encoding happens separately, at export.
///
/// With default (neutral) settings every stage is a no-op, so the source image
/// is returned unchanged.
pub fn render(source: &ImageBuf, settings: &Settings, backend: &dyn Backend) -> ImageBuf {
    let img = source.clone();
    let img = apply_global(img, &settings.global, backend);
    let img = apply_locals(img, &settings.locals, backend);
    apply_geometry(img, &settings.geometry, backend)
}

/// Stage: global adjustments, applied in SOURCE space.
///
/// Each active adjustment is lowered into a per-pixel operation and applied via
/// the backend, in canonical order: white balance, exposure, tone, saturation.
/// An inactive (`None`) adjustment contributes nothing.
fn apply_global(mut img: ImageBuf, global: &Adjustments, backend: &dyn Backend) -> ImageBuf {
    if let Some(wb) = global.white_balance {
        // temp/tint become per-channel gains, with green as the anchor.
        let (rg, gg, bg) = (1.0 + wb.temp, 1.0 - wb.tint, 1.0 - wb.temp);
        backend.map_pixels(&mut img, &PointOp::Gain([rg, gg, bg]));
    }
    if let Some(stops) = global.exposure {
        // In linear light, exposure is a multiply: +1 EV = ×2.
        let gain = 2.0_f32.powf(stops);
        backend.map_pixels(&mut img, &PointOp::Gain([gain, gain, gain]));
    }
    if let Some(t) = global.tone {
        // Each non-neutral tonal control is a shape of the same curve, applied
        // per channel through the same path, in a fixed order.
        for curve in tone_curves(&t) {
            backend.map_pixels(&mut img, &PointOp::Tone(curve));
        }
    }
    if let Some(amount) = global.saturation {
        backend.map_pixels(&mut img, &PointOp::Saturation(amount));
    }
    img
}

/// The active tonal curves of a [`SelectiveTone`], in canonical order. A control
/// at its neutral `0` contributes no curve.
fn tone_curves(t: &SelectiveTone) -> Vec<ToneCurve> {
    let mut curves = Vec::new();
    if t.contrast != 0.0 {
        curves.push(tone::contrast(t.contrast));
    }
    if t.highlights != 0.0 {
        curves.push(tone::highlights(t.highlights));
    }
    if t.shadows != 0.0 {
        curves.push(tone::shadows(t.shadows));
    }
    if t.blacks != 0.0 {
        curves.push(tone::blacks(t.blacks));
    }
    curves
}

/// Stage: local adjustments, applied in SOURCE space.
///
/// Each local adjustment applies a set of adjustments through its mask, blended
/// at its opacity. Masks are not implemented yet, so this returns the image
/// unchanged.
fn apply_locals(img: ImageBuf, _locals: &[LocalAdjustment], _backend: &dyn Backend) -> ImageBuf {
    img
}

/// Stage: geometry — the single SOURCE → OUTPUT resample (crop, straighten).
///
/// Not implemented yet, so this returns the image unchanged.
fn apply_geometry(img: ImageBuf, _geometry: &Geometry, _backend: &dyn Backend) -> ImageBuf {
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::WhiteBalance;
    use latent_image::color::luminance;

    /// A minimal backend so the pipeline can be tested here; the real CPU
    /// backend lives in a crate that depends on this one. It gives each
    /// [`PointOp`] the same meaning the CPU backend does.
    struct TestBackend;

    impl Backend for TestBackend {
        fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp) {
            match op {
                PointOp::Gain(g) => {
                    let g = *g;
                    for px in img.pixels_mut() {
                        *px = [px[0] * g[0], px[1] * g[1], px[2] * g[2]];
                    }
                }
                PointOp::Tone(curve) => {
                    for px in img.pixels_mut() {
                        *px = std::array::from_fn(|c| curve.apply_linear(px[c]));
                    }
                }
                PointOp::Saturation(amount) => {
                    let amount = *amount;
                    for px in img.pixels_mut() {
                        let y = luminance(*px);
                        // Clamp to ≥0 so over-saturation never emits negative light
                        // (mirrors the CPU backend).
                        *px = std::array::from_fn(|c| (y + amount * (px[c] - y)).max(0.0));
                    }
                }
            }
        }
    }

    /// Develop a single pixel through the given global adjustments.
    fn developed(global: Adjustments, px: [f32; 3]) -> [f32; 3] {
        let mut src = ImageBuf::new(1, 1);
        src.set(0, 0, px);
        let settings = Settings {
            global,
            ..Settings::default()
        };
        render(&src, &settings, &TestBackend).get(0, 0)
    }

    #[test]
    fn render_with_default_settings_returns_the_source_unchanged() {
        let mut src = ImageBuf::new(3, 2);
        src.set(0, 0, [0.1, 0.2, 0.3]);
        src.set(2, 1, [0.9, 0.8, 0.7]);
        let out = render(&src, &Settings::default(), &TestBackend);
        assert_eq!(out, src);
    }

    #[test]
    fn exposure_one_ev_doubles_linear_values() {
        let p = developed(
            Adjustments {
                exposure: Some(1.0),
                ..Adjustments::default()
            },
            [0.1, 0.2, 0.3],
        );
        assert!(
            (p[0] - 0.2).abs() < 1e-6 && (p[1] - 0.4).abs() < 1e-6 && (p[2] - 0.6).abs() < 1e-6
        );
    }

    #[test]
    fn white_balance_can_reneutralize_a_gray() {
        // A warm-cast gray (R high, B low): cool it and nudge tint to R==G==B.
        let p = developed(
            Adjustments {
                white_balance: Some(WhiteBalance {
                    temp: -0.2,
                    tint: 0.04,
                }),
                ..Adjustments::default()
            },
            [0.6, 0.5, 0.4],
        );
        assert!(
            (p[0] - p[1]).abs() < 1e-6 && (p[1] - p[2]).abs() < 1e-6,
            "{p:?}"
        );
    }

    #[test]
    fn saturation_zero_is_grayscale_and_one_is_unchanged() {
        let gray = developed(
            Adjustments {
                saturation: Some(0.0),
                ..Adjustments::default()
            },
            [0.6, 0.3, 0.1],
        );
        assert!(
            (gray[0] - gray[1]).abs() < 1e-6 && (gray[1] - gray[2]).abs() < 1e-6,
            "{gray:?}"
        );

        let same = developed(
            Adjustments {
                saturation: Some(1.0),
                ..Adjustments::default()
            },
            [0.6, 0.3, 0.1],
        );
        for c in 0..3 {
            assert!((same[c] - [0.6, 0.3, 0.1][c]).abs() < 1e-6);
        }
    }

    #[test]
    fn contrast_brightens_a_mid_bright_gray() {
        let p = developed(
            Adjustments {
                tone: Some(SelectiveTone {
                    contrast: 0.6,
                    ..SelectiveTone::default()
                }),
                ..Adjustments::default()
            },
            [0.7, 0.7, 0.7],
        );
        assert!(p[0] > 0.7 && p[1] > 0.7 && p[2] > 0.7, "{p:?}");
    }
}
