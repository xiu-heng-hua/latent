//! Rendering pipeline and the backend abstraction.
//!
//! [`render`] is the fixed, ordered pipeline. It owns the order; a [`Backend`]
//! only provides the pixel-level primitives it calls. Naming the coordinate
//! spaces is deliberate: all adjustments act on the full, uncropped image
//! (SOURCE space), and geometry is the single later step that reframes it
//! (SOURCE → OUTPUT).

use latent_edit::{Adjustments, Crop, Geometry, LocalAdjustment, Mask, SelectiveTone, Settings};
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

/// A data-described operation combining an image with a second one pixelwise.
///
/// Like [`PointOp`], this is data rather than a closure so any backend can run
/// it. The second image is supplied to [`Backend::combine`] alongside the kind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CombineKind {
    /// Unsharp recombine: `other + gain·(img − other)`. With `other` the blurred
    /// base, this amplifies the detail the image holds over its blur.
    Unsharp { gain: f32 },
}

/// Pixel dimensions of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub width: u32,
    pub height: u32,
}

/// An affine map from OUTPUT pixel coordinates to SOURCE pixel coordinates,
/// plus the size of the output image.
///
/// The geometry stage resamples by inverse-mapping each output pixel through
/// this and sampling the source — so the map runs output → source. Keeping it
/// an explicit value (rather than baking rotation into the backend) is what
/// will later let perspective/distortion compose here, and mask reprojection
/// remain possible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Size of the output image.
    pub output: Extent,
    /// Row-major 2x3 affine: `src = (m[0]·(x, y, 1), m[1]·(x, y, 1))`.
    pub m: [[f32; 3]; 2],
}

impl Transform {
    /// The identity transform for an image of the given size: every output pixel
    /// maps to the same source pixel, so resampling is a no-op.
    pub fn identity(extent: Extent) -> Self {
        Self {
            output: extent,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        }
    }

    /// A rotation about the image center by `angle` radians, expanding the
    /// output to the rotated bounding box so no content is lost (the corner
    /// wedges fall outside the source and sample as black).
    pub fn rotation(src: Extent, angle: f32) -> Self {
        let (w, h) = (src.width as f32, src.height as f32);
        let (sin, cos) = angle.sin_cos();
        let nw = (w * cos.abs() + h * sin.abs()).ceil().max(1.0);
        let nh = (w * sin.abs() + h * cos.abs()).ceil().max(1.0);
        let (scx, scy) = (w / 2.0, h / 2.0);
        let (dcx, dcy) = (nw / 2.0, nh / 2.0);
        // Map an output pixel center back through the inverse rotation into the
        // source, in pixel-index coordinates (pixel centers at integers).
        let m02 = cos * (0.5 - dcx) + sin * (0.5 - dcy) + scx - 0.5;
        let m12 = -sin * (0.5 - dcx) + cos * (0.5 - dcy) + scy - 0.5;
        Self {
            output: Extent {
                width: nw as u32,
                height: nh as u32,
            },
            m: [[cos, sin, m02], [-sin, cos, m12]],
        }
    }

    /// The source coordinate an output pixel `(x, y)` maps to.
    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.m[0][0] * x + self.m[0][1] * y + self.m[0][2],
            self.m[1][0] * x + self.m[1][1] * y + self.m[1][2],
        )
    }
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

    /// Blur the image with a box blur of the given radius (in pixels).
    fn blur(&self, img: &ImageBuf, radius: f32) -> ImageBuf;

    /// Combine `img` with `other` pixelwise in place, per `kind`. The two images
    /// must have the same dimensions.
    fn combine(&self, img: &mut ImageBuf, other: &ImageBuf, kind: &CombineKind);

    /// Resample the image into a new one by inverse-mapping each output pixel
    /// through `transform` and sampling the source (bilinear).
    fn resample(&self, img: &ImageBuf, transform: &Transform) -> ImageBuf;

    /// Evaluate a mask to a per-pixel weight buffer in `[0, 1]`, row-major, sized
    /// to and reading from `source` (SOURCE coordinates) — so value-driven shapes
    /// (luminosity, hue) can select on pixel content, not just position.
    fn eval_mask(&self, mask: &Mask, source: &ImageBuf) -> Vec<f32>;

    /// Blend `top` over `base` in place by `weights[p] * opacity`:
    /// `base[p] = base[p] + weights[p]*opacity*(top[p] - base[p])`. The weight
    /// buffer must match the image's pixel count.
    fn blend(&self, base: &mut ImageBuf, top: &ImageBuf, weights: &[f32], opacity: f32);
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
/// Each active adjustment is lowered into backend primitives and applied in
/// canonical order: white balance, exposure, tone, saturation, sharpening. An
/// inactive (`None`) adjustment contributes nothing.
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
    if let Some(s) = global.sharpen
        && s.amount > 0.0
        && s.radius > 0.0
    {
        // Unsharp mask: blur to a base, then amplify the detail (img − base).
        let base = backend.blur(&img, s.radius);
        let gain = 1.0 + s.amount;
        backend.combine(&mut img, &base, &CombineKind::Unsharp { gain });
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
/// Each local adjustment reuses the global lowering — its adjustments are
/// applied to the whole image, then that result is blended back through the
/// mask (weight × opacity). Reusing [`apply_global`] is the point: a local
/// adjustment is the same edit as a global one, just scoped by a mask.
fn apply_locals(mut img: ImageBuf, locals: &[LocalAdjustment], backend: &dyn Backend) -> ImageBuf {
    for local in locals {
        // The mask is evaluated on the current image, so value-driven shapes see
        // the developed pixels they select on.
        let weights = backend.eval_mask(&local.mask, &img);
        let adjusted = apply_global(img.clone(), &local.adjustments, backend);
        backend.blend(&mut img, &adjusted, &weights, local.opacity);
    }
    img
}

/// Stage: geometry — the single SOURCE → OUTPUT step.
///
/// Straighten first (a resample about the center, expanding the canvas), then
/// crop (an exact clip of the result). Both are reversible: they only change
/// what the *output* contains, never the source. The default geometry leaves
/// the image untouched.
fn apply_geometry(mut img: ImageBuf, geometry: &Geometry, backend: &dyn Backend) -> ImageBuf {
    if geometry.straighten_degrees != 0.0 {
        let extent = Extent {
            width: img.width(),
            height: img.height(),
        };
        let t = Transform::rotation(extent, geometry.straighten_degrees.to_radians());
        img = backend.resample(&img, &t);
    }
    if let Some(crop) = geometry.crop {
        img = crop_image(&img, crop);
    }
    img
}

/// Clip `img` to a normalized crop rectangle. Fractions become pixels at this
/// image's resolution, so the same crop applies to a preview and a full render.
fn crop_image(img: &ImageBuf, c: Crop) -> ImageBuf {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x = (c.x.clamp(0.0, 1.0) * w).round() as u32;
    let y = (c.y.clamp(0.0, 1.0) * h).round() as u32;
    let cw = (c.width.clamp(0.0, 1.0) * w).round().max(1.0) as u32;
    let ch = (c.height.clamp(0.0, 1.0) * h).round().max(1.0) as u32;
    img.cropped(x, y, cw, ch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{Gradient, LuminanceRange, MaskShape, Sharpen, WhiteBalance};
    use latent_image::color::luminance;

    /// A minimal backend so the pipeline can be tested here; the real CPU
    /// backend lives in a crate that depends on this one. It gives each
    /// [`PointOp`]/[`CombineKind`] the same meaning the CPU backend does, with
    /// simple (and sequential) reference implementations.
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

        fn blur(&self, img: &ImageBuf, radius: f32) -> ImageBuf {
            let r = radius.round().max(0.0) as i32;
            if r == 0 {
                return img.clone();
            }
            let (w, h) = (img.width() as i32, img.height() as i32);
            let mut out = ImageBuf::new(img.width(), img.height());
            for y in 0..h {
                for x in 0..w {
                    let mut sum = [0.0_f32; 3];
                    let mut n = 0.0;
                    for dy in -r..=r {
                        for dx in -r..=r {
                            let sx = (x + dx).clamp(0, w - 1) as u32;
                            let sy = (y + dy).clamp(0, h - 1) as u32;
                            let p = img.get(sx, sy);
                            sum[0] += p[0];
                            sum[1] += p[1];
                            sum[2] += p[2];
                            n += 1.0;
                        }
                    }
                    out.set(x as u32, y as u32, [sum[0] / n, sum[1] / n, sum[2] / n]);
                }
            }
            out
        }

        fn combine(&self, img: &mut ImageBuf, other: &ImageBuf, kind: &CombineKind) {
            match *kind {
                CombineKind::Unsharp { gain } => {
                    for (px, o) in img.pixels_mut().iter_mut().zip(other.pixels().iter()) {
                        *px = std::array::from_fn(|c| o[c] + gain * (px[c] - o[c]));
                    }
                }
            }
        }

        fn resample(&self, img: &ImageBuf, t: &Transform) -> ImageBuf {
            // Nearest-neighbor is enough to exercise the geometry stage here.
            let (w, h) = (img.width() as i32, img.height() as i32);
            let mut out = ImageBuf::new(t.output.width, t.output.height);
            for oy in 0..t.output.height {
                for ox in 0..t.output.width {
                    let (sx, sy) = t.map(ox as f32, oy as f32);
                    let (xi, yi) = (sx.round() as i32, sy.round() as i32);
                    let px = if xi >= 0 && yi >= 0 && xi < w && yi < h {
                        img.get(xi as u32, yi as u32)
                    } else {
                        [0.0; 3]
                    };
                    out.set(ox, oy, px);
                }
            }
            out
        }

        fn eval_mask(&self, mask: &Mask, source: &ImageBuf) -> Vec<f32> {
            let (w, h) = (source.width(), source.height());
            let (wf, hf) = (w as f32, h as f32);
            (0..w * h)
                .map(|i| {
                    let x = (i % w) as f32;
                    let y = (i / w) as f32;
                    mask.weight_at((x + 0.5) / wf, (y + 0.5) / hf, source.pixels()[i as usize])
                })
                .collect()
        }

        fn blend(&self, base: &mut ImageBuf, top: &ImageBuf, weights: &[f32], opacity: f32) {
            for ((b, t), &wt) in base
                .pixels_mut()
                .iter_mut()
                .zip(top.pixels().iter())
                .zip(weights.iter())
            {
                let a = (wt * opacity).clamp(0.0, 1.0);
                for c in 0..3 {
                    b[c] += a * (t[c] - b[c]);
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

    #[test]
    fn sharpening_overshoots_a_step_edge() {
        // Sharpening is lowered to blur + recombine; on a step edge it should
        // push the dark side below and the bright side above their originals.
        let mut src = ImageBuf::new(5, 1);
        for (x, v) in [0.0, 0.0, 0.0, 1.0, 1.0].into_iter().enumerate() {
            src.set(x as u32, 0, [v; 3]);
        }
        let settings = Settings {
            global: Adjustments {
                sharpen: Some(Sharpen {
                    amount: 1.0,
                    radius: 1.0,
                }),
                ..Adjustments::default()
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert!(out.get(2, 0)[0] < 0.0, "dark side: {:?}", out.get(2, 0));
        assert!(out.get(3, 0)[0] > 1.0, "bright side: {:?}", out.get(3, 0));
    }

    #[test]
    fn crop_reduces_dimensions_and_keeps_the_region() {
        // 4x2 with a per-pixel marker; crop the right half.
        let mut src = ImageBuf::new(4, 2);
        for y in 0..2 {
            for x in 0..4 {
                src.set(x, y, [(x + y * 4) as f32, 0.0, 0.0]);
            }
        }
        let settings = Settings {
            geometry: Geometry {
                crop: Some(Crop {
                    x: 0.5,
                    y: 0.0,
                    width: 0.5,
                    height: 1.0,
                }),
                straighten_degrees: 0.0,
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert_eq!((out.width(), out.height()), (2, 2));
        assert_eq!(out.get(0, 0), [2.0, 0.0, 0.0]); // old (2, 0)
        assert_eq!(out.get(1, 1), [7.0, 0.0, 0.0]); // old (3, 1)
    }

    #[test]
    fn straighten_expands_the_canvas_and_keeps_the_center() {
        let mut src = ImageBuf::new(20, 20);
        for p in src.pixels_mut() {
            *p = [0.4, 0.6, 0.8];
        }
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 20.0,
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert!(out.width() > 20 && out.height() > 20, "canvas should grow");
        let center = out.get(out.width() / 2, out.height() / 2);
        assert!((center[0] - 0.4).abs() < 1e-4, "center kept: {center:?}");
        assert_eq!(out.get(0, 0), [0.0, 0.0, 0.0]); // corner outside source → black
    }

    #[test]
    fn masked_local_adjustment_affects_only_the_masked_side() {
        // A flat gray, a horizontal gradient mask (0 left → 1 right), and a
        // local +1 EV. The right side should brighten toward the adjusted value
        // while the left stays near the original — and it reuses apply_global.
        let mut src = ImageBuf::new(8, 1);
        for x in 0..8 {
            src.set(x, 0, [0.4, 0.4, 0.4]);
        }
        let local = LocalAdjustment {
            mask: Mask {
                shapes: vec![MaskShape::Gradient(Gradient {
                    x0: 0.0,
                    y0: 0.5,
                    x1: 1.0,
                    y1: 0.5,
                })],
                invert: false,
            },
            adjustments: Adjustments {
                exposure: Some(1.0), // doubles → adjusted value 0.8
                ..Adjustments::default()
            },
            opacity: 1.0,
        };
        let settings = Settings {
            locals: vec![local],
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        let left = out.get(0, 0)[0];
        let right = out.get(7, 0)[0];
        assert!(
            right > left,
            "masked side brighter: left {left}, right {right}"
        );
        assert!(left < 0.5, "left near original 0.4: {left}");
        assert!(right > 0.6, "right near adjusted 0.8: {right}");
    }

    #[test]
    fn luminosity_masked_local_affects_only_the_selected_tones() {
        // Two pixels — dark and bright. A local +1 EV masked to shadows
        // (luma ≤ 0.3) must brighten only the dark one, proving the source pixel
        // reaches the mask through `eval_mask` (the N1 seam) and drives selection.
        let mut src = ImageBuf::new(2, 1);
        src.set(0, 0, [0.1, 0.1, 0.1]); // dark → selected
        src.set(1, 0, [0.8, 0.8, 0.8]); // bright → not selected
        let local = LocalAdjustment {
            mask: Mask {
                shapes: vec![MaskShape::Luminosity(LuminanceRange {
                    lo: 0.0,
                    hi: 0.3,
                    feather: 0.02,
                })],
                invert: false,
            },
            adjustments: Adjustments {
                exposure: Some(1.0), // ×2
                ..Adjustments::default()
            },
            opacity: 1.0,
        };
        let settings = Settings {
            locals: vec![local],
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert!(
            out.get(0, 0)[0] > 0.18,
            "dark brightened: {:?}",
            out.get(0, 0)
        );
        assert!(
            (out.get(1, 0)[0] - 0.8).abs() < 1e-6,
            "bright unchanged: {:?}",
            out.get(1, 0)
        );
    }
}
