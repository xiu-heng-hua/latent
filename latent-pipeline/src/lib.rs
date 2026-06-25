//! Rendering pipeline and the backend abstraction.
//!
//! [`render`] is the fixed, ordered pipeline. It owns the order; a [`Backend`]
//! only provides the pixel-level primitives it calls. Naming the coordinate
//! spaces is deliberate: all adjustments act on the full, uncropped image
//! (SOURCE space), and geometry is the single later step that reframes it
//! (SOURCE → OUTPUT).

use latent_edit::{
    Adjustments, Crop, Curves, Geometry, LocalAdjustment, Mask, SelectiveTone, Settings,
};
use latent_image::ImageBuf;
use latent_image::color::luminance;
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
    /// Apply a per-channel tone curve `[r, g, b]`, each in its perceptual domain.
    Curves([ToneCurve; 3]),
    /// Per-hue-band color mix: eight `[hue, sat, lum]` band adjustments.
    ColorMix([[f32; 3]; 8]),
    /// Linearly remix channels by a 3x3 matrix (output channel = M · input).
    Matrix([[f32; 3]; 3]),
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
    /// Midtone-weighted local contrast (clarity): `img + amount·m·(img − other)`,
    /// where `m` is a midtone window of the base (`other`) luminance — full in the
    /// midtones, zero at black/white. Adds broad local contrast without haloing
    /// the tonal extremes; `amount` 0 is a no-op, negative softens.
    LocalContrast { amount: f32 },
}

/// Parameters for the edge-preserving denoise primitive (a bilateral filter),
/// split into independent luminance and chroma channels.
///
/// `radius` is the spatial extent (in pixels) of the neighborhood averaged.
/// `luma` and `chroma` are the range (edge-stopping) scales for the luminance and
/// color components respectively: neighbors differing by much more than the scale
/// are excluded, so edges survive while same-value noise averages out. A scale of
/// `0` leaves that component untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DenoiseParams {
    pub radius: f32,
    pub luma: f32,
    pub chroma: f32,
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

    /// Denoise the image with an edge-preserving (bilateral) filter, returning a
    /// new image. Unlike [`Self::blur`], the averaging is edge-aware: it smooths
    /// noise within a tone but does not bleed across luminance edges.
    fn denoise(&self, img: &ImageBuf, params: DenoiseParams) -> ImageBuf;

    /// Remove an estimated atmospheric haze veil, returning a new image.
    /// `strength` in `[0, 1]` is the dark-channel prior's `ω`. The veil is
    /// estimated from a *patch* dark channel (a neighborhood min), so a bright
    /// neutral object with darker surroundings is recognized as haze-free rather
    /// than crushed — see [`dehaze_recover`].
    fn dehaze(&self, img: &ImageBuf, strength: f32) -> ImageBuf;

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
    if let Some(c) = &global.curves {
        backend.map_pixels(&mut img, &PointOp::Curves(channel_curves(c)));
    }
    if let Some(amount) = global.saturation {
        backend.map_pixels(&mut img, &PointOp::Saturation(amount));
    }
    if let Some(hsl) = &global.hsl {
        backend.map_pixels(&mut img, &PointOp::ColorMix(hsl.bands));
    }
    if let Some(cm) = &global.channel_mixer {
        backend.map_pixels(&mut img, &PointOp::Matrix(cm.matrix));
    }
    if let Some(nr) = global.noise_reduction
        && nr.radius > 0.0
        && (nr.luminance > 0.0 || nr.color > 0.0)
    {
        // Denoise before the contrast/sharpening tools so they don't amplify the
        // noise the bilateral filter is removing.
        img = backend.denoise(
            &img,
            DenoiseParams {
                radius: nr.radius,
                luma: nr.luminance,
                chroma: nr.color,
            },
        );
    }
    if let Some(strength) = global.dehaze
        && strength > 0.0
    {
        img = backend.dehaze(&img, strength);
    }
    if let Some(c) = global.clarity
        && c.amount != 0.0
        && c.radius > 0.0
    {
        // Clarity is unsharp at a broad radius with midtone weighting: the same
        // recombine as sharpening, but the added local contrast tapers off toward
        // black/white so it doesn't halo. The base is three box-blur passes — a
        // central-limit approximation of a Gaussian — because a single box kernel
        // rings at the broad clarity radius and would itself create halos.
        let mut base = backend.blur(&img, c.radius);
        base = backend.blur(&base, c.radius);
        base = backend.blur(&base, c.radius);
        backend.combine(
            &mut img,
            &base,
            &CombineKind::LocalContrast { amount: c.amount },
        );
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

/// A midtone window for clarity: `1` at mid-gray, falling smoothly to `0` at
/// black and white (a parabola). The window is evaluated in the **perceptual**
/// (gamma) domain the tone system uses, so its peak lands on perceptual mid-gray
/// (≈0.18 in linear light) rather than linear 0.5 (≈0.73 perceptually) — i.e. it
/// genuinely weights the midtones instead of skewing into the highlights.
/// Weighting the added local contrast by this protects the highlights and
/// shadows from halos. Public so a backend computing the
/// [`CombineKind::LocalContrast`] recombine reuses the identical window.
pub fn midtone_weight(base_luma: f32) -> f32 {
    let b = base_luma.clamp(0.0, 1.0).powf(1.0 / tone::GAMMA);
    1.0 - (2.0 * b - 1.0) * (2.0 * b - 1.0)
}

/// Transmission floor for dehazing: the smallest transmission allowed, so the
/// recovery never divides by ~0 in the densest haze. From the dark-channel
/// dehazing method (He, Sun & Tang, *Single Image Haze Removal Using Dark Channel
/// Prior*, CVPR 2009), which uses `t0 = 0.1`.
const DEHAZE_T0: f32 = 0.1;

/// Radius (pixels) of the dark-channel patch. He, Sun & Tang take the dark
/// channel over a local *patch*, not a single pixel: that is what lets a bright
/// neutral object (which has darker pixels nearby) be told apart from a uniformly
/// bright haze veil, so the former is preserved instead of crushed to black.
pub const DEHAZE_PATCH: i32 = 4;

/// The patch dark channel at `(x, y)`: the minimum, over the surrounding
/// `(2·DEHAZE_PATCH+1)²` window (clamped at the borders), of each pixel's
/// smallest channel. High for uniform bright haze, low wherever any nearby pixel
/// is dark — so a bright neutral subject with darker surroundings reads as
/// haze-free. Public so a backend evaluating dehaze reuses the identical estimate.
pub fn dehaze_dark_channel(img: &ImageBuf, x: u32, y: u32) -> f32 {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let mut dc = f32::INFINITY;
    for dy in -DEHAZE_PATCH..=DEHAZE_PATCH {
        for dx in -DEHAZE_PATCH..=DEHAZE_PATCH {
            let sx = (x as i32 + dx).clamp(0, w - 1) as u32;
            let sy = (y as i32 + dy).clamp(0, h - 1) as u32;
            let p = img.get(sx, sy);
            dc = dc.min(p[0].min(p[1]).min(p[2]));
        }
    }
    dc
}

/// Recover one dehazed linear-RGB pixel from its value and patch dark channel.
///
/// The atmospheric scattering model is `I = J·t + A·(1 − t)`: the observed pixel
/// `I` is the clear radiance `J` attenuated by transmission `t`, plus airlight
/// `A`. With a neutral unit airlight (`A = 1`) the dark-channel prior gives
/// `t = 1 − strength·dc`, and inverting the model recovers
/// `J = (I − A)/clamp(t, t0, 1) + A`. `strength` in `[0, 1]` is the prior's `ω`.
/// A clear pixel (`dc ≈ 0`) has `t ≈ 1` and is left unchanged; removing the gray
/// veil restores contrast (deeper blacks) and saturation at once. Highlight
/// headroom (`I > 1`) is passed through, since the model assumes `I ≤ A`.
pub fn dehaze_recover(rgb: [f32; 3], dc: f32, strength: f32) -> [f32; 3] {
    let t = (1.0 - strength * dc.clamp(0.0, 1.0)).clamp(DEHAZE_T0, 1.0);
    std::array::from_fn(|c| {
        let in_range = rgb[c].min(1.0);
        let headroom = (rgb[c] - 1.0).max(0.0);
        ((in_range - 1.0) / t + 1.0).max(0.0) + headroom
    })
}

/// One output pixel of the bilateral denoise filter at `(x, y)`.
///
/// Each pixel splits into luminance `Y` and chroma `rgb − Y`, which are denoised
/// on **separate** range scales and recombined: luminance carries the detail (so
/// `params.luma` is kept gentle) while color noise is low-frequency blotches that
/// `params.chroma` can smooth hard. Each component is a bilateral average over the
/// `±radius` neighborhood — the weight is a spatial Gaussian times a range
/// Gaussian on that component's own difference, so an edge (a large luminance
/// *or* chroma step) gets a near-zero weight and is not blurred across. Stopping
/// chroma on chroma difference preserves iso-luminant *color* edges; stopping
/// luma on luma difference preserves luminance detail. Bilateral filtering:
/// Tomasi & Manduchi, ICCV 1998. The spatial Gaussian uses `σ = radius/2` so it
/// falls off across the support (window `2σ`) rather than behaving like a box. A
/// component whose scale is `0` is left untouched. The caller guarantees
/// `radius >= 1` and at least one positive scale.
///
/// Public so a backend evaluating the filter itself reuses the identical kernel.
pub fn bilateral_pixel(img: &ImageBuf, x: u32, y: u32, params: DenoiseParams) -> [f32; 3] {
    let r = params.radius.round().max(1.0) as i32;
    let (w, h) = (img.width() as i32, img.height() as i32);
    let sigma_s = r as f32 / 2.0;
    let inv_2ss2 = 1.0 / (2.0 * sigma_s * sigma_s); // spatial (σ = radius/2)
    let (do_luma, do_chroma) = (params.luma > 0.0, params.chroma > 0.0);
    let inv_2sl2 = 1.0 / (2.0 * params.luma * params.luma); // luminance range
    let inv_2sc2 = 1.0 / (2.0 * params.chroma * params.chroma); // chroma range

    let c = img.get(x, y);
    let cy = luminance(c);
    let cc: [f32; 3] = std::array::from_fn(|k| c[k] - cy);
    let (mut acc_y, mut wsum_y) = (0.0_f32, 0.0_f32);
    let (mut acc_c, mut wsum_c) = ([0.0_f32; 3], 0.0_f32);
    for dy in -r..=r {
        for dx in -r..=r {
            let sx = (x as i32 + dx).clamp(0, w - 1) as u32;
            let sy = (y as i32 + dy).clamp(0, h - 1) as u32;
            let n = img.get(sx, sy);
            let ny = luminance(n);
            let spatial = -((dx * dx + dy * dy) as f32) * inv_2ss2;
            if do_luma {
                let dl = cy - ny;
                let wl = (spatial - dl * dl * inv_2sl2).exp();
                acc_y += wl * ny;
                wsum_y += wl;
            }
            if do_chroma {
                let nc: [f32; 3] = std::array::from_fn(|k| n[k] - ny);
                let dc2 = (0..3)
                    .map(|k| (cc[k] - nc[k]) * (cc[k] - nc[k]))
                    .sum::<f32>();
                let wc = (spatial - dc2 * inv_2sc2).exp();
                for k in 0..3 {
                    acc_c[k] += wc * nc[k];
                }
                wsum_c += wc;
            }
        }
    }
    let yout = if do_luma { acc_y / wsum_y } else { cy };
    let cout: [f32; 3] = std::array::from_fn(|k| if do_chroma { acc_c[k] / wsum_c } else { cc[k] });
    std::array::from_fn(|k| yout + cout[k])
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

/// Lower per-channel [`Curves`] to three effective tone curves `[r, g, b]`, each
/// the channel's curve composed after the master, interpolated from its control
/// points. Reuses [`ToneCurve`] so curves share the existing perceptual path.
fn channel_curves(c: &Curves) -> [ToneCurve; 3] {
    let master = point_curve(&c.master);
    let compose = |points: &[(f32, f32)]| {
        let channel = point_curve(points);
        ToneCurve::from_fn(|t| channel.eval(master.eval(t)))
    };
    [compose(&c.red), compose(&c.green), compose(&c.blue)]
}

/// A tone curve interpolated (piecewise-linear) through `(input, output)` control
/// points in the perceptual `[0, 1]` domain, clamped flat past the ends. No
/// points gives the identity.
fn point_curve(points: &[(f32, f32)]) -> ToneCurve {
    if points.is_empty() {
        return ToneCurve::identity();
    }
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let last = pts.len() - 1;
    ToneCurve::from_fn(move |t| {
        if t <= pts[0].0 {
            return pts[0].1;
        }
        if t >= pts[last].0 {
            return pts[last].1;
        }
        let i = pts.windows(2).position(|w| t <= w[1].0).unwrap();
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[i + 1];
        y0 + (t - x0) / (x1 - x0) * (y1 - y0)
    })
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
    use latent_edit::{
        Clarity, Gradient, Hsl, LuminanceRange, MaskShape, NoiseReduction, Sharpen, WhiteBalance,
    };
    use latent_image::color::{Mat3, color_mix, luminance};

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
                PointOp::Curves(curves) => {
                    for px in img.pixels_mut() {
                        *px = std::array::from_fn(|c| curves[c].apply_linear(px[c]));
                    }
                }
                PointOp::ColorMix(bands) => {
                    for px in img.pixels_mut() {
                        *px = color_mix(*px, bands);
                    }
                }
                PointOp::Matrix(m) => {
                    let m = Mat3(*m);
                    for px in img.pixels_mut() {
                        *px = m.mul_vec(*px);
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
                CombineKind::LocalContrast { amount } => {
                    for (px, o) in img.pixels_mut().iter_mut().zip(other.pixels().iter()) {
                        let k = amount * midtone_weight(luminance(*o));
                        *px = std::array::from_fn(|c| px[c] + k * (px[c] - o[c]));
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

        fn denoise(&self, img: &ImageBuf, params: DenoiseParams) -> ImageBuf {
            if params.radius.round() < 1.0 || (params.luma <= 0.0 && params.chroma <= 0.0) {
                return img.clone();
            }
            let mut out = ImageBuf::new(img.width(), img.height());
            for y in 0..img.height() {
                for x in 0..img.width() {
                    out.set(x, y, bilateral_pixel(img, x, y, params));
                }
            }
            out
        }

        fn dehaze(&self, img: &ImageBuf, strength: f32) -> ImageBuf {
            if strength <= 0.0 {
                return img.clone();
            }
            let mut out = ImageBuf::new(img.width(), img.height());
            for y in 0..img.height() {
                for x in 0..img.width() {
                    let dc = dehaze_dark_channel(img, x, y);
                    out.set(x, y, dehaze_recover(img.get(x, y), dc, strength));
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
    fn hsl_mixer_grades_one_band_and_spares_the_others() {
        // Desaturate only the red band via the color mixer. A red pixel goes
        // gray; a cyan pixel (a different band) is left exactly alone — the
        // selectivity that defines the tool, reached through apply_global.
        let mut bands = [[0.0_f32; 3]; 8];
        bands[0] = [0.0, -1.0, 0.0]; // red band: saturation ×0
        let red = developed(
            Adjustments {
                hsl: Some(Hsl { bands }),
                ..Adjustments::default()
            },
            [0.8, 0.1, 0.1],
        );
        assert!(
            (red[0] - red[1]).abs() < 1e-6 && (red[1] - red[2]).abs() < 1e-6,
            "red desaturated: {red:?}"
        );
        let cyan = developed(
            Adjustments {
                hsl: Some(Hsl { bands }),
                ..Adjustments::default()
            },
            [0.1, 0.8, 0.8],
        );
        assert_eq!(cyan, [0.1, 0.8, 0.8], "cyan untouched");
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
    fn noise_reduction_smooths_a_tone_but_keeps_an_edge() {
        // A noisy dark region beside a bright one. The bilateral filter pulls the
        // noisy midtone pixel toward its like-toned neighbors, while the bright
        // pixel at the edge keeps its value — its dark neighbor across the edge is
        // rejected by the range term. Wired through apply_global.
        let mut src = ImageBuf::new(5, 1);
        for (x, v) in [0.20, 0.25, 0.20, 0.80, 0.80].into_iter().enumerate() {
            src.set(x as u32, 0, [v; 3]);
        }
        let settings = Settings {
            global: Adjustments {
                noise_reduction: Some(NoiseReduction {
                    radius: 1.0,
                    luminance: 0.1,
                    color: 0.1,
                }),
                ..Adjustments::default()
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        let smoothed = out.get(1, 0)[0];
        assert!(
            smoothed > 0.20 && smoothed < 0.25,
            "noise smoothed toward neighbors: {smoothed}"
        );
        assert!(
            (out.get(3, 0)[0] - 0.80).abs() < 1e-3,
            "edge preserved: {:?}",
            out.get(3, 0)
        );
    }

    #[test]
    fn dehaze_clears_a_synthetic_veil() {
        // Veil a saturated clear pixel (one channel at 0, so the dark-channel prior
        // holds) with white airlight at transmission 0.5, then dehaze it. Full
        // strength inverts the model and recovers the clear pixel; the lowering
        // wires it through apply_global.
        let clear = [0.8, 0.2, 0.0];
        let t = 0.5;
        let hazy: [f32; 3] = std::array::from_fn(|c| clear[c] * t + (1.0 - t));
        let out = developed(
            Adjustments {
                dehaze: Some(1.0),
                ..Adjustments::default()
            },
            hazy,
        );
        for (c, &want) in clear.iter().enumerate() {
            assert!(
                (out[c] - want).abs() < 1e-5,
                "recovered {out:?} vs {clear:?}"
            );
        }
    }

    #[test]
    fn clarity_boosts_midtone_local_contrast() {
        // A midtone step. Clarity (broad blur + midtone-weighted recombine) lifts
        // local contrast: the dark side goes down and the bright side up. The
        // radius is kept small here only so the blurred base is predictable; all
        // values sit in the midtones, where the weight is ~1, so it stays active.
        let mut src = ImageBuf::new(5, 1);
        for (x, v) in [0.4, 0.4, 0.4, 0.6, 0.6].into_iter().enumerate() {
            src.set(x as u32, 0, [v; 3]);
        }
        let settings = Settings {
            global: Adjustments {
                clarity: Some(Clarity {
                    amount: 1.0,
                    radius: 1.0,
                }),
                ..Adjustments::default()
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert!(
            out.get(2, 0)[0] < 0.4,
            "dark side deepened: {:?}",
            out.get(2, 0)
        );
        assert!(
            out.get(3, 0)[0] > 0.6,
            "bright side lifted: {:?}",
            out.get(3, 0)
        );
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
                ops: Vec::new(),
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
                ops: Vec::new(),
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
