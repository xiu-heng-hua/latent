//! CPU rendering backend.

use latent_edit::Mask;
use latent_image::ImageBuf;
use latent_image::color::{Mat3, color_mix, luminance};
use latent_pipeline::{Backend, CombineKind, PointOp, Transform};
use rayon::prelude::*;

/// A rendering backend that runs every primitive on the CPU.
///
/// This is the always-available backend and the correctness reference: other
/// backends may accelerate some primitives and fall back to this one.
pub struct CpuBackend;

impl Backend for CpuBackend {
    fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp) {
        // The operation is matched once, outside the per-pixel loop; each pixel
        // depends only on its own value, so the work is data-parallel.
        match op {
            PointOp::Gain(g) => {
                let g = *g;
                img.pixels_mut()
                    .par_iter_mut()
                    .for_each(|px| *px = [px[0] * g[0], px[1] * g[1], px[2] * g[2]]);
            }
            PointOp::Tone(curve) => {
                img.pixels_mut()
                    .par_iter_mut()
                    .for_each(|px| *px = std::array::from_fn(|c| curve.apply_linear(px[c])));
            }
            PointOp::Saturation(amount) => {
                let amount = *amount;
                img.pixels_mut().par_iter_mut().for_each(|px| {
                    let y = luminance(*px);
                    // Clamp the result to ≥0: an over-saturation (amount > 1) can
                    // push the weakest channel negative, which would otherwise twist
                    // hue when later stages clamp it asymmetrically. Headroom (the
                    // brightened channel) is left unbounded.
                    *px = std::array::from_fn(|c| (y + amount * (px[c] - y)).max(0.0));
                });
            }
            PointOp::Curves(curves) => {
                img.pixels_mut()
                    .par_iter_mut()
                    .for_each(|px| *px = std::array::from_fn(|c| curves[c].apply_linear(px[c])));
            }
            PointOp::ColorMix(bands) => {
                let bands = *bands;
                img.pixels_mut()
                    .par_iter_mut()
                    .for_each(|px| *px = color_mix(*px, &bands));
            }
            PointOp::Matrix(m) => {
                let m = Mat3(*m);
                img.pixels_mut()
                    .par_iter_mut()
                    .for_each(|px| *px = m.mul_vec(*px));
            }
        }
    }

    fn blur(&self, img: &ImageBuf, radius: f32) -> ImageBuf {
        // A box blur is separable: a horizontal 1-D pass then a vertical one,
        // so the cost is O(radius) per pixel rather than O(radius²).
        let r = radius.round().max(0.0) as i32;
        if r == 0 {
            return img.clone();
        }
        let horizontal = blur_axis(img, r, false);
        blur_axis(&horizontal, r, true)
    }

    fn combine(&self, img: &mut ImageBuf, other: &ImageBuf, kind: &CombineKind) {
        // Pixelwise zip of two equally-sized images, per the combine kind.
        match *kind {
            CombineKind::Unsharp { gain } => {
                img.pixels_mut()
                    .par_iter_mut()
                    .zip(other.pixels().par_iter())
                    .for_each(|(px, o)| {
                        *px = std::array::from_fn(|c| o[c] + gain * (px[c] - o[c]))
                    });
            }
        }
    }

    fn resample(&self, img: &ImageBuf, t: &Transform) -> ImageBuf {
        // Inverse mapping: trace each output pixel back into the source and
        // bilinearly sample. Output rows are independent → run them in parallel.
        let mut out = ImageBuf::new(t.output.width, t.output.height);
        let stride = t.output.width as usize;
        out.pixels_mut()
            .par_chunks_mut(stride)
            .enumerate()
            .for_each(|(oy, row)| {
                for ox in 0..t.output.width {
                    let (sx, sy) = t.map(ox as f32, oy as f32);
                    row[ox as usize] = sample_bilinear(img, sx, sy);
                }
            });
        out
    }

    fn eval_mask(&self, mask: &Mask, source: &ImageBuf) -> Vec<f32> {
        // One weight per pixel, from the mask evaluated at the pixel's center in
        // normalized coordinates and the source pixel there (for value-driven
        // shapes). Pixels are independent → parallel.
        let (w, h) = (source.width(), source.height());
        let (wf, hf) = (w as f32, h as f32);
        let pixels = source.pixels();
        let mut weights = vec![0.0_f32; (w as usize) * (h as usize)];
        weights.par_iter_mut().enumerate().for_each(|(i, out)| {
            let x = (i as u32 % w) as f32;
            let y = (i as u32 / w) as f32;
            *out = mask.weight_at((x + 0.5) / wf, (y + 0.5) / hf, pixels[i]);
        });
        weights
    }

    fn blend(&self, base: &mut ImageBuf, top: &ImageBuf, weights: &[f32], opacity: f32) {
        // Lerp each pixel from base toward top by its mask weight × opacity.
        base.pixels_mut()
            .par_iter_mut()
            .zip(top.pixels().par_iter())
            .zip(weights.par_iter())
            .for_each(|((b, t), &wt)| {
                let a = (wt * opacity).clamp(0.0, 1.0);
                for c in 0..3 {
                    b[c] += a * (t[c] - b[c]);
                }
            });
    }
}

/// One 1-D box-average pass, along columns (`vertical`) or rows. Each output
/// pixel is the mean over a `2*radius+1` window; the border clamps (replicates
/// the edge pixel). Rows are independent, so they run in parallel.
fn blur_axis(src: &ImageBuf, radius: i32, vertical: bool) -> ImageBuf {
    let (w, h) = (src.width() as i32, src.height() as i32);
    let stride = src.width() as usize;
    let in_px = src.pixels();
    let mut out = ImageBuf::new(src.width(), src.height());
    let n = (2 * radius + 1) as f32;

    out.pixels_mut()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let mut sum = [0.0_f32; 3];
                for d in -radius..=radius {
                    let (sx, sy) = if vertical {
                        (x, (y as i32 + d).clamp(0, h - 1))
                    } else {
                        ((x + d).clamp(0, w - 1), y as i32)
                    };
                    let p = in_px[sy as usize * stride + sx as usize];
                    sum[0] += p[0];
                    sum[1] += p[1];
                    sum[2] += p[2];
                }
                row[x as usize] = [sum[0] / n, sum[1] / n, sum[2] / n];
            }
        });
    out
}

/// Bilinear sample of `img` at the fractional coordinate `(x, y)`, where integer
/// coordinates address pixel centers. Blends the four surrounding pixels; any
/// neighbor outside the image contributes black, so sampling past the border
/// fades to black.
///
/// This is a 2-tap interpolator with no prefilter, so it assumes a transform of
/// roughly unit scale (the geometry stage uses it only for crop/rotation).
/// Minifying through it would undersample and alias — downscaling is done
/// separately by area-averaging (`ImageBuf::downscaled`), not here.
fn sample_bilinear(img: &ImageBuf, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);

    let at = |xi: i32, yi: i32| -> [f32; 3] {
        if xi < 0 || yi < 0 || xi >= w || yi >= h {
            [0.0; 3]
        } else {
            img.get(xi as u32, yi as u32)
        }
    };
    let (p00, p10) = (at(x0, y0), at(x0 + 1, y0));
    let (p01, p11) = (at(x0, y0 + 1), at(x0 + 1, y0 + 1));

    std::array::from_fn(|c| {
        let top = p00[c] * (1.0 - fx) + p10[c] * fx;
        let bot = p01[c] * (1.0 - fx) + p11[c] * fx;
        top * (1.0 - fy) + bot * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{Adjustments, ChannelMixer, Curves, Gradient, MaskShape, Settings};
    use latent_pipeline::{Extent, render};

    #[test]
    fn map_pixels_identity_leaves_the_image_unchanged() {
        let mut img = ImageBuf::new(2, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        img.set(1, 1, [0.4, 0.5, 0.6]);
        let before = img.clone();
        CpuBackend.map_pixels(&mut img, &PointOp::Gain([1.0, 1.0, 1.0]));
        assert_eq!(img, before);
    }

    #[test]
    fn map_pixels_applies_the_function_to_every_pixel() {
        let mut img = ImageBuf::new(1, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        img.set(0, 1, [0.4, 0.5, 0.6]);
        CpuBackend.map_pixels(&mut img, &PointOp::Gain([2.0, 2.0, 2.0]));
        assert_eq!(img.get(0, 0), [0.2, 0.4, 0.6]);
        assert_eq!(img.get(0, 1), [0.8, 1.0, 1.2]);
    }

    #[test]
    fn blur_radius_zero_is_identity() {
        let mut img = ImageBuf::new(2, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        img.set(1, 1, [0.4, 0.5, 0.6]);
        assert_eq!(CpuBackend.blur(&img, 0.0), img);
    }

    #[test]
    fn blur_leaves_a_uniform_image_unchanged() {
        let mut img = ImageBuf::new(4, 4);
        for p in img.pixels_mut() {
            *p = [0.5, 0.2, 0.7];
        }
        let out = CpuBackend.blur(&img, 2.0);
        for y in 0..4 {
            for x in 0..4 {
                let p = out.get(x, y);
                assert!(
                    (p[0] - 0.5).abs() < 1e-6
                        && (p[1] - 0.2).abs() < 1e-6
                        && (p[2] - 0.7).abs() < 1e-6
                );
            }
        }
    }

    #[test]
    fn blur_matches_a_box_average_reference() {
        // 3x1 row; radius 1 with edge clamp. The vertical pass is a no-op (h=1).
        let mut img = ImageBuf::new(3, 1);
        img.set(0, 0, [0.0; 3]);
        img.set(1, 0, [0.3; 3]);
        img.set(2, 0, [0.9; 3]);
        let out = CpuBackend.blur(&img, 1.0);
        // (0,0): (0+0+0.3)/3, (1,0): (0+0.3+0.9)/3, (2,0): (0.3+0.9+0.9)/3
        assert!((out.get(0, 0)[0] - 0.1).abs() < 1e-6);
        assert!((out.get(1, 0)[0] - 0.4).abs() < 1e-6);
        assert!((out.get(2, 0)[0] - 0.7).abs() < 1e-6);
    }

    #[test]
    fn combine_unsharp_recombines_pixelwise() {
        // Unsharp with gain g: out = other + g·(img − other) = g·img − (g−1)·other.
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [0.5, 0.25, 0.0]);
        img.set(1, 0, [0.2, 1.0, 0.5]);
        let mut base = ImageBuf::new(2, 1);
        base.set(0, 0, [0.5, 0.25, 1.0]);
        base.set(1, 0, [0.0, 0.0, 0.5]);
        CpuBackend.combine(&mut img, &base, &CombineKind::Unsharp { gain: 2.0 });
        // out = 2·img − base, pixelwise.
        assert_eq!(img.get(0, 0), [0.5, 0.25, -1.0]);
        assert_eq!(img.get(1, 0), [0.4, 2.0, 0.5]);
    }

    #[test]
    fn unsharp_overshoots_a_step_edge() {
        // A dark→bright step. Unsharp (blur to a base, then amplify the detail)
        // should push the dark side below its original and the bright side above.
        let mut img = ImageBuf::new(5, 1);
        for (x, v) in [0.0, 0.0, 0.0, 1.0, 1.0].into_iter().enumerate() {
            img.set(x as u32, 0, [v; 3]);
        }
        let base = CpuBackend.blur(&img, 1.0);
        CpuBackend.combine(&mut img, &base, &CombineKind::Unsharp { gain: 2.0 });
        assert!(img.get(2, 0)[0] < 0.0, "dark side should undershoot");
        assert!(img.get(3, 0)[0] > 1.0, "bright side should overshoot");
        assert!(img.get(0, 0)[0].abs() < 1e-6, "flat region unchanged");
    }

    #[test]
    fn resample_identity_reproduces_the_image() {
        let mut img = ImageBuf::new(3, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        img.set(2, 1, [0.7, 0.8, 0.9]);
        let t = Transform::identity(Extent {
            width: 3,
            height: 2,
        });
        assert_eq!(CpuBackend.resample(&img, &t), img);
    }

    #[test]
    fn resample_rotation_expands_and_keeps_the_center() {
        let mut img = ImageBuf::new(20, 20);
        for p in img.pixels_mut() {
            *p = [0.4, 0.6, 0.8];
        }
        let t = Transform::rotation(
            Extent {
                width: 20,
                height: 20,
            },
            20.0_f32.to_radians(),
        );
        let out = CpuBackend.resample(&img, &t);
        assert!(out.width() > 20 && out.height() > 20, "canvas should grow");
        let center = out.get(out.width() / 2, out.height() / 2);
        assert!(
            (center[0] - 0.4).abs() < 1e-4
                && (center[1] - 0.6).abs() < 1e-4
                && (center[2] - 0.8).abs() < 1e-4,
            "center preserved, got {center:?}"
        );
        assert_eq!(out.get(0, 0), [0.0, 0.0, 0.0]); // corner outside source → black
    }

    #[test]
    fn eval_mask_produces_a_weight_ramp() {
        // Horizontal gradient over a 4x1 source: weights increase left to right.
        // The gradient is position-only, so the source pixels' values don't matter.
        let mask = Mask {
            shapes: vec![MaskShape::Gradient(Gradient {
                x0: 0.0,
                y0: 0.5,
                x1: 1.0,
                y1: 0.5,
            })],
            ops: Vec::new(),
            invert: false,
        };
        let w = CpuBackend.eval_mask(&mask, &ImageBuf::new(4, 1));
        assert_eq!(w.len(), 4);
        assert!(w[0] < w[1] && w[1] < w[2] && w[2] < w[3], "ramp: {w:?}");
        assert!(w.iter().all(|&v| (0.0..=1.0).contains(&v)));
    }

    #[test]
    fn blend_lerps_by_weight_and_opacity() {
        let mut base = ImageBuf::new(2, 1);
        base.set(0, 0, [0.0, 0.0, 0.0]);
        base.set(1, 0, [0.0, 0.0, 0.0]);
        let mut top = ImageBuf::new(2, 1);
        top.set(0, 0, [1.0, 1.0, 1.0]);
        top.set(1, 0, [1.0, 1.0, 1.0]);
        // weight 0 → unchanged; weight 1 at half opacity → halfway.
        CpuBackend.blend(&mut base, &top, &[0.0, 1.0], 0.5);
        assert_eq!(base.get(0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(base.get(1, 0), [0.5, 0.5, 0.5]);
    }

    /// Render a uniform mid-gray pixel with the given global curves; `render`
    /// lowers them through `apply_global` to a per-channel `PointOp::Curves`.
    fn render_gray_with(curves: Curves) -> [f32; 3] {
        let mut src = ImageBuf::new(1, 1);
        src.set(0, 0, [0.3, 0.3, 0.3]);
        let settings = Settings {
            global: Adjustments {
                curves: Some(curves),
                ..Adjustments::default()
            },
            ..Settings::default()
        };
        render(&src, &settings, &CpuBackend).get(0, 0)
    }

    #[test]
    fn per_channel_curve_grades_only_that_channel() {
        // A red curve lifting the mid-tones; green/blue stay at identity.
        let out = render_gray_with(Curves {
            red: vec![(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)],
            ..Curves::default()
        });
        assert!(out[0] > 0.3, "red lifted: {out:?}");
        assert!((out[1] - 0.3).abs() < 1e-5, "green untouched: {out:?}");
        assert!((out[2] - 0.3).abs() < 1e-5, "blue untouched: {out:?}");
    }

    #[test]
    fn master_curve_applies_uniformly_and_identity_is_a_noop() {
        // A master curve shapes every channel identically (like the tone path).
        let lifted = render_gray_with(Curves {
            master: vec![(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)],
            ..Curves::default()
        });
        assert!(lifted[0] > 0.3, "master lifts: {lifted:?}");
        assert_eq!(lifted[0], lifted[1]);
        assert_eq!(lifted[1], lifted[2]);
        // An identity master (endpoints only) round-trips the perceptual path
        // unchanged — the same no-op the existing tone curve gives.
        let same = render_gray_with(Curves {
            master: vec![(0.0, 0.0), (1.0, 1.0)],
            ..Curves::default()
        });
        for c in same {
            assert!((c - 0.3).abs() < 1e-5, "identity master noop: {same:?}");
        }
    }

    /// Render a single pixel through a global channel mixer matrix.
    fn render_pixel_with_matrix(px: [f32; 3], matrix: [[f32; 3]; 3]) -> [f32; 3] {
        let mut src = ImageBuf::new(1, 1);
        src.set(0, 0, px);
        let settings = Settings {
            global: Adjustments {
                channel_mixer: Some(ChannelMixer { matrix }),
                ..Adjustments::default()
            },
            ..Settings::default()
        };
        render(&src, &settings, &CpuBackend).get(0, 0)
    }

    #[test]
    fn channel_mixer_monochrome_makes_gray() {
        // Identical rows (a luma-style mix) collapse any pixel to neutral gray.
        let w = [0.3, 0.6, 0.1];
        let out = render_pixel_with_matrix([0.8, 0.4, 0.2], [w, w, w]);
        assert!(
            (out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6,
            "monochrome: {out:?}"
        );
    }

    #[test]
    fn channel_mixer_can_swap_channels() {
        // A swap matrix sends [r, g, b] -> [b, g, r].
        let out = render_pixel_with_matrix(
            [0.1, 0.2, 0.3],
            [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]],
        );
        assert_eq!(out, [0.3, 0.2, 0.1]);
    }
}
