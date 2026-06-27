//! Rendering pipeline and the backend abstraction.
//!
//! [`render`] is the fixed, ordered pipeline. It owns the order; a [`Backend`]
//! only provides the pixel-level primitives it calls. Naming the coordinate
//! spaces is deliberate: all adjustments act on the full, uncropped image
//! (SOURCE space), and geometry is the single later step that reframes it
//! (SOURCE → OUTPUT).

use latent_edit::{
    Adjustments, Crop, Curves, DistortionModel, Geometry, LensProfile, LocalAdjustment, Mask,
    Perspective, SelectiveTone, Settings,
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

/// A projective (homography) map from OUTPUT pixel coordinates to SOURCE pixel
/// coordinates, plus the size of the output image.
///
/// The geometry stage resamples by inverse-mapping each output pixel through
/// this and sampling the source — so the map runs output → source. Keeping it
/// an explicit value (rather than baking rotation into the backend) is what lets
/// perspective compose here ([`Self::compose`]), distortion compose later, and
/// mask reprojection remain possible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Size of the output image.
    pub output: Extent,
    /// Row-major 3x3 homography mapping an output pixel `(x, y, 1)` to a source
    /// coordinate via the perspective divide (see [`Self::map`]). An affine
    /// transform is the special case whose bottom row is `[0, 0, 1]`.
    pub m: [[f32; 3]; 3],
}

impl Transform {
    /// The identity transform for an image of the given size: every output pixel
    /// maps to the same source pixel, so resampling is a no-op.
    pub fn identity(extent: Extent) -> Self {
        Self {
            output: extent,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
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
            m: [[cos, sin, m02], [-sin, cos, m12], [0.0, 0.0, 1.0]],
        }
    }

    /// The source coordinate an output pixel `(x, y)` maps to, after the
    /// perspective divide by the homogeneous weight `w`. For an affine transform
    /// (`w ≡ 1`) this is the plain matrix-vector product.
    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let sx = self.m[0][0] * x + self.m[0][1] * y + self.m[0][2];
        let sy = self.m[1][0] * x + self.m[1][1] * y + self.m[1][2];
        let w = self.m[2][0] * x + self.m[2][1] * y + self.m[2][2];
        if w <= 0.0 {
            // Behind the projection plane (only reachable at extreme keystone):
            // no valid source, so map outside the frame to read as black rather
            // than a sign-flipped or NaN (0/0) coordinate.
            return (-1.0, -1.0);
        }
        (sx / w, sy / w)
    }

    /// Compose two homographies: `self.compose(other).map(p)` equals
    /// `self.map(other.map(p))` (up to the perspective scale). The matrix is the
    /// product `self.m · other.m`; the result carries `self.output` as the final
    /// output extent.
    pub fn compose(&self, other: &Transform) -> Transform {
        let (a, b) = (&self.m, &other.m);
        let mut m = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
            }
        }
        Transform {
            output: self.output,
            m,
        }
    }
}

/// The normalized radial distance of a point from `center`, scaled by
/// `inv_norm` (the reciprocal of the normalization radius). Shared by the radial
/// warp and the radial-gain (vignette) steps so they agree on one geometry.
pub fn normalized_radius(x: f32, y: f32, center: [f32; 2], inv_norm: f32) -> f32 {
    let dx = x - center[0];
    let dy = y - center[1];
    (dx * dx + dy * dy).sqrt() * inv_norm
}

/// A radial gain field — a per-pixel multiplier varying with the normalized
/// distance from `center`. Shared by lens-vignetting *correction* (in SOURCE,
/// `reciprocal` of the measured falloff) and the *creative* vignette (in OUTPUT);
/// the model is `1 + g0·r² + g1·r⁴ + g2·r⁶`, optionally reciprocated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadialGain {
    /// Center of the radial field, in the image's pixel coordinates.
    pub center: [f32; 2],
    /// Reciprocal of the radius normalization.
    pub inv_norm: f32,
    /// Polynomial coefficients in `r²`.
    pub poly: [f32; 3],
    /// Divide by the polynomial instead of multiplying (lens correction is the
    /// reciprocal of the measured falloff).
    pub reciprocal: bool,
}

impl RadialGain {
    /// The gain multiplier at point `(x, y)`.
    pub fn at(&self, x: f32, y: f32) -> f32 {
        let r2 = {
            let r = normalized_radius(x, y, self.center, self.inv_norm);
            r * r
        };
        let p = 1.0 + self.poly[0] * r2 + self.poly[1] * r2 * r2 + self.poly[2] * r2 * r2 * r2;
        if self.reciprocal { 1.0 / p } else { p }
    }
}

/// A general (possibly non-affine) OUTPUT → SOURCE map for a single resample: a
/// homography applied first, then a radial distortion about a center, then an
/// optional per-channel radial scale (lateral chromatic aberration).
///
/// Composing all of these into one coordinate lookup is what keeps the geometry
/// stage to a *single* interpolation — a separate distortion pass followed by a
/// separate perspective pass would interpolate twice and soften the image. With
/// an all-zero `radial` and unit `channel_scale` this is exactly the homography
/// [`Transform`] of the same matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Warp {
    /// Size of the output image.
    pub output: Extent,
    /// Homography (output → rectilinear source coordinates), as in [`Transform`].
    pub m: [[f32; 3]; 3],
    /// Center of the radial term, in source pixel coordinates.
    pub center: [f32; 2],
    /// Reciprocal of the radius normalization (lensfun's focal-scaled
    /// half-diagonal `NormScale`), so radius math is a multiply.
    pub inv_norm: f32,
    /// Which forward distortion model `radial` describes — it selects how the
    /// output → source (undistort) map inverts the forward `r_d(r_u)`: Newton
    /// iteration for the even POLY3/POLY5 models, a direct radial multiply for
    /// the PanoTools/Hugin PTLENS model.
    pub model: DistortionModel,
    /// Forward radial distortion coefficients in the focal-normalized radius `r`,
    /// laid out by `model` (see [`LensProfile::distortion`]): POLY3 carries `k1`
    /// in slot 1; POLY5 carries `k1`, `k2` in slots 1 and 3; PTLENS carries
    /// `[c, b, a, 0]`. All-zero (with `model` = `None`) is no radial term.
    pub radial: [f32; 4],
    /// Per-channel `[r, g, b]` radial scale of the offset from `center`, for
    /// lateral chromatic aberration: each channel `c` samples at radius
    /// `r·(b_c·r² + c_c·r + v_c)` where `channel_scale[c] = [b_c, c_c, v_c]`.
    /// Green is the reference `[0, 0, 1]`; when all three are the identity the
    /// channels share one coordinate and resample in a single pass.
    pub channel_scale: [[f32; 3]; 3],
}

impl Warp {
    /// The pure-homography warp of a [`Transform`] — no radial term and no CA, so
    /// it resamples identically to [`Backend::resample`] of the same transform.
    pub fn from_transform(t: &Transform) -> Self {
        Self {
            output: t.output,
            m: t.m,
            center: [0.0, 0.0],
            inv_norm: 0.0,
            model: DistortionModel::None,
            radial: [0.0, 0.0, 0.0, 0.0],
            channel_scale: [CA_IDENTITY; 3],
        }
    }

    /// The source coordinate an output pixel `(x, y)` maps to: the homography
    /// (with perspective divide), then the radial distortion about `center`. This
    /// is the geometric (CA-free) coordinate shared by all channels.
    ///
    /// The geometry stage runs output → source: an output (corrected) pixel is
    /// inverse-mapped to the source (uncorrected raw) pixel to sample. This is
    /// lensfun's `UnDist` step — solving `r_d(r_u) = r_out` for the source radius
    /// `r_u` to look up. For the even POLY3/POLY5 models that has no closed form,
    /// so it is solved by Newton iteration exactly as lensfun does; PTLENS keeps
    /// the direct multiply where it is the defined operation.
    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let w = self.m[2][0] * x + self.m[2][1] * y + self.m[2][2];
        if w <= 0.0 {
            // Behind the projection plane (extreme keystone) — sample outside.
            return (-1.0, -1.0);
        }
        let ix = (self.m[0][0] * x + self.m[0][1] * y + self.m[0][2]) / w;
        let iy = (self.m[1][0] * x + self.m[1][1] * y + self.m[1][2]) / w;
        if self.model == DistortionModel::None {
            return (ix, iy);
        }
        let (dx, dy) = (ix - self.center[0], iy - self.center[1]);
        // `r_out` is the corrected-output radius (focal-normalized); `s` carries
        // the offset onto the source radius that distorts back to it.
        let r_out = (dx * dx + dy * dy).sqrt() * self.inv_norm;
        let s = self.undistort_ratio(r_out);
        (self.center[0] + dx * s, self.center[1] + dy * s)
    }

    /// The ratio `r_src / r_out` mapping a corrected-output radius `r_out` to the
    /// source radius that distorts back to it (lensfun's `UnDist`; both in the
    /// focal-normalized frame). For the even models this solves `r_out = r_src·(1 +
    /// …)` for `r_src` by a few Newton steps — the radius is monotone over the
    /// image, so it converges to sub-pixel in two or three. PTLENS instead applies
    /// the forward (`Dist`) multiply directly, the register's decision for it.
    fn undistort_ratio(&self, r_out: f32) -> f32 {
        if r_out == 0.0 {
            return 1.0;
        }
        match self.model {
            DistortionModel::None => 1.0,
            // POLY3 focal form: r_out = r_src + k1·r_src³. Solve for r_src.
            DistortionModel::Poly3 => {
                let k1 = self.radial[1];
                let mut ru = r_out;
                for _ in 0..NEWTON_STEPS {
                    let f = ru + k1 * ru * ru * ru - r_out;
                    ru -= f / (1.0 + 3.0 * k1 * ru * ru);
                }
                ru / r_out
            }
            // POLY5 focal form: r_out = r_src(1 + k1·r_src² + k2·r_src⁴).
            DistortionModel::Poly5 => {
                let (k1, k2) = (self.radial[1], self.radial[3]);
                let mut ru = r_out;
                for _ in 0..NEWTON_STEPS {
                    let ru2 = ru * ru;
                    let f = ru * (1.0 + k1 * ru2 + k2 * ru2 * ru2) - r_out;
                    ru -= f / (1.0 + 3.0 * k1 * ru2 + 5.0 * k2 * ru2 * ru2);
                }
                ru / r_out
            }
            // PTLENS keeps the direct radial multiply: s = 1 + c·r + b·r² + a·r³
            // evaluated at the output radius (Horner), no Newton inversion.
            DistortionModel::Ptlens => {
                let [c, b, a, _] = self.radial;
                1.0 + r_out * (c + r_out * (b + r_out * a))
            }
        }
    }

    /// The source coordinate channel `c` samples from: [`Self::map`] for the
    /// shared geometry, then the per-channel radial CA scale applied to the
    /// offset from `center`. The scale `s_c(r) = b·r² + c·r + v` is evaluated at
    /// the (post-distortion) radius the channel samples at — one resample, no
    /// second CA pass.
    pub fn map_channel(&self, x: f32, y: f32, c: usize) -> (f32, f32) {
        let (bx, by) = self.map(x, y);
        let [b, cc, v] = self.channel_scale[c];
        if b == 0.0 && cc == 0.0 && v == 1.0 {
            return (bx, by);
        }
        let (dx, dy) = (bx - self.center[0], by - self.center[1]);
        let r = (dx * dx + dy * dy).sqrt() * self.inv_norm;
        let s = v + r * (cc + r * b);
        (self.center[0] + dx * s, self.center[1] + dy * s)
    }

    /// Whether any channel has a non-identity CA scale (so channels must be
    /// sampled separately rather than in one shared lookup). Green is the
    /// reference identity `[0, 0, 1]`.
    pub fn has_chromatic(&self) -> bool {
        self.channel_scale.iter().any(|s| *s != CA_IDENTITY)
    }
}

/// The green-reference CA scale `[b, c, v] = [0, 0, 1]`: a unit radial scale with
/// no offset, so the channel samples at the shared (distortion-only) coordinate.
const CA_IDENTITY: [f32; 3] = [0.0, 0.0, 1.0];

/// Newton iterations for the even-model radius inversion. The radius map is
/// monotone over the image, so a fixed small count converges to well below a
/// pixel at the corners; a fixed count (no tolerance branch) keeps the hot path
/// branch-free and trivially portable to a GPU warp shader.
const NEWTON_STEPS: usize = 3;

/// The pixel-level primitives a rendering backend provides.
///
/// The pipeline calls these in a fixed order; the order lives in [`render`],
/// never in a backend. A backend may implement the primitives however it likes
/// (on the CPU now, elsewhere later) as long as the results match. More
/// primitives are added to this trait as the pipeline grows.
///
/// The trait is `Send + Sync` so a backend can be moved to or shared across a
/// worker thread (rendering and export off the UI thread): every implementation
/// is either stateless or holds only thread-safe device handles, so the bound is
/// satisfied today and pinned here against a future implementation that is not.
pub trait Backend: Send + Sync {
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

    /// Resample through a general [`Warp`] — a homography composed with a radial
    /// distortion — in a single interpolation. With an all-zero radial term this
    /// matches [`Self::resample`] of the same homography.
    fn warp(&self, img: &ImageBuf, warp: &Warp) -> ImageBuf;

    /// Multiply each pixel by a radial gain field (see [`RadialGain`]), in place.
    fn apply_radial_gain(&self, img: &mut ImageBuf, gain: &RadialGain);

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

/// Build the keystone (perspective-correction) transform for an image of size
/// `extent`: an output → source homography correcting converging verticals
/// (`vertical`) and horizontals (`horizontal`). The frame center is fixed and
/// each amount is normalized to the half-extent, so it is the shift in the
/// projective weight from center to edge. Both amounts `0` is the identity.
///
/// Derived as `T(c) · K · T(-c)` with the centered keystone `K` carrying the
/// perspective term in its bottom row `[a, b, 1]` (`a = horizontal/cx`,
/// `b = vertical/cy`), so the divide varies the horizontal scale with `y` (and
/// vice versa) — turning a converging source line into a straight output one.
pub fn keystone_transform(extent: Extent, vertical: f32, horizontal: f32) -> Transform {
    let cx = (extent.width as f32 - 1.0) / 2.0;
    let cy = (extent.height as f32 - 1.0) / 2.0;
    let a = if cx > 0.0 { horizontal / cx } else { 0.0 };
    let b = if cy > 0.0 { vertical / cy } else { 0.0 };
    let k = a * cx + b * cy;
    Transform {
        output: extent,
        m: [
            [1.0 + cx * a, cx * b, -cx * k],
            [cy * a, 1.0 + cy * b, -cy * k],
            [a, b, 1.0 - k],
        ],
    }
}

/// The radial component of a [`LensProfile`] for an image of size `extent`, as
/// the `(center, inv_norm, model, radial)` fields of a [`Warp`].
///
/// The radius is normalized exactly as lensfun does — by the **focal-scaled
/// half-diagonal**, so `r = 1` is one real focal length on the sensor (lensfun's
/// natural unit), not the half-short-edge of the PanoTools/Hugin convention:
///
/// ```text
/// NormScale = hypot(36, 24) / Crop / hypot(W + 1, H + 1) / RealFocal
/// ```
///
/// where `W = width - 1`, `H = height - 1` are measured at the pixel centers.
/// The distortion coefficients are carried in this same focal frame (rescaled at
/// lookup), so the radius unit and the coefficients agree. The optical center
/// offset is measured against the **shorter** side: `lens.center` is a fraction
/// where `0.5` is the image center and a unit offset spans `min(w, h)/2` pixels.
fn lens_radial(extent: Extent, lens: &LensProfile) -> ([f32; 2], f32, DistortionModel, [f32; 4]) {
    let (w, h) = (extent.width as f32, extent.height as f32);
    let center = optical_center(extent, lens);
    let inv_norm = (36.0_f32).hypot(24.0) / lens.crop / (w + 1.0).hypot(h + 1.0) / lens.real_focal;
    (center, inv_norm, lens.model, lens.distortion)
}

/// The optical center in source pixels. The `lens.center` fraction is anchored at
/// the frame center (`0.5`); an offset is measured in half-shorter-side units,
/// matching lensfun's `min(W, H)/2` divisor (the same on both axes), so an
/// off-center calibration is right on a non-square frame.
fn optical_center(extent: Extent, lens: &LensProfile) -> [f32; 2] {
    let cap_w = (extent.width as f32 - 1.0).max(1.0);
    let cap_h = (extent.height as f32 - 1.0).max(1.0);
    let half_short = cap_w.min(cap_h) / 2.0;
    [
        cap_w / 2.0 + (lens.center[0] - 0.5) * 2.0 * half_short,
        cap_h / 2.0 + (lens.center[1] - 0.5) * 2.0 * half_short,
    ]
}

/// The corner-anchored radius normalization for the PA vignetting model, whose
/// `r = 1` is the image **corner** (unlike distortion/TCA, whose `r = 1` is one
/// focal length). Returns `(center, inv_norm)`: the optical center and the
/// reciprocal of the half-diagonal in pixels, so an image corner sits at `r = 1`.
/// Vignetting is measured about the optical axis, so it shares the center offset.
fn lens_vignetting_radial(extent: Extent, lens: &LensProfile) -> ([f32; 2], f32) {
    let (w, h) = (extent.width as f32, extent.height as f32);
    let center = optical_center(extent, lens);
    let inv_norm = 2.0 / (w * w + h * h).sqrt();
    (center, inv_norm)
}

/// Stage: geometry — the single SOURCE → OUTPUT step.
///
/// Lens distortion, keystone, and straighten all compose into one coordinate map
/// so the image is interpolated *exactly once*; then crop is an exact clip of the
/// result. All are reversible: they only change what the *output* contains, never
/// the source. The default geometry leaves the image untouched.
///
/// The output keeps the source frame size — there is no auto-scale-to-fill, so a
/// strong distortion or keystone correction can leave black borders the user
/// crops away (an auto-scale would be a later addition).
fn apply_geometry(mut img: ImageBuf, geometry: &Geometry, backend: &dyn Backend) -> ImageBuf {
    let extent = Extent {
        width: img.width(),
        height: img.height(),
    };
    // Lens vignetting correction is a SOURCE-space radial gain applied *before*
    // any resample (matching lensfun's vignetting → geometry order): a flat-field
    // multiply, not an interpolation. The PA model's radius is corner-anchored
    // (r = 1 at the image corner), a different unit from distortion/TCA, so it
    // gets its own normalization; `reciprocal: true` divides the source by the
    // measured falloff `1 + k1 r² + …` (lensfun's `C_d = C_s / (1 + …)`).
    if let Some(l) = geometry.lens.filter(|l| l.vignetting != [0.0, 0.0, 0.0]) {
        let (center, inv_norm) = lens_vignetting_radial(extent, &l);
        backend.apply_radial_gain(
            &mut img,
            &RadialGain {
                center,
                inv_norm,
                poly: l.vignetting,
                reciprocal: true,
            },
        );
    }
    let straighten = (geometry.straighten_degrees != 0.0)
        .then(|| Transform::rotation(extent, geometry.straighten_degrees.to_radians()));
    let keystone = geometry
        .perspective
        .filter(|p: &Perspective| p.vertical != 0.0 || p.horizontal != 0.0)
        .map(|p| keystone_transform(extent, p.vertical, p.horizontal));
    // Compose straighten and keystone into one homography (output → rectilinear
    // source). The output canvas is the straighten's bounding box when present.
    let homography = match (straighten, keystone) {
        (Some(s), Some(k)) => Some(Transform {
            output: s.output,
            ..k.compose(&s)
        }),
        (Some(s), None) => Some(s),
        (None, Some(k)) => Some(k),
        (None, None) => None,
    };
    // Fold lens distortion and chromatic aberration into the *same* resample:
    // homography, then the radial term, then a per-channel scale — one
    // interpolation, never a second warp pass.
    let lens = geometry.lens.filter(|l| {
        l.model != DistortionModel::None || l.ca[0] != CA_IDENTITY || l.ca[1] != CA_IDENTITY
    });
    match (homography, lens) {
        (h, Some(l)) => {
            let base = h.unwrap_or_else(|| Transform::identity(extent));
            let (center, inv_norm, model, radial) = lens_radial(extent, &l);
            img = backend.warp(
                &img,
                &Warp {
                    output: base.output,
                    m: base.m,
                    center,
                    inv_norm,
                    model,
                    radial,
                    // Green is the reference identity; red/blue carry their POLY3
                    // radial CA scale [b, c, v].
                    channel_scale: [l.ca[0], CA_IDENTITY, l.ca[1]],
                },
            );
        }
        (Some(t), None) => img = backend.resample(&img, &t),
        (None, None) => {}
    }
    if let Some(crop) = geometry.crop {
        img = crop_image(&img, crop);
    }
    // Creative vignette: a radial gain about the *output* (post-crop) frame
    // center, normalized so the corners sit at r = 1. A gain, not a resample.
    if let Some(amount) = geometry.vignette.filter(|a| *a != 0.0) {
        let (w, h) = (img.width() as f32, img.height() as f32);
        backend.apply_radial_gain(
            &mut img,
            &RadialGain {
                center: [(w - 1.0) / 2.0, (h - 1.0) / 2.0],
                inv_norm: 2.0 / (w * w + h * h).sqrt(),
                poly: [amount, 0.0, 0.0],
                reciprocal: false,
            },
        );
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

        fn warp(&self, img: &ImageBuf, wp: &Warp) -> ImageBuf {
            // Nearest-neighbor sampling through the general warp coordinate map,
            // per channel when chromatic aberration is present.
            let (w, h) = (img.width() as i32, img.height() as i32);
            let sample = |sx: f32, sy: f32| {
                let (xi, yi) = (sx.round() as i32, sy.round() as i32);
                if xi >= 0 && yi >= 0 && xi < w && yi < h {
                    img.get(xi as u32, yi as u32)
                } else {
                    [0.0; 3]
                }
            };
            let chromatic = wp.has_chromatic();
            let mut out = ImageBuf::new(wp.output.width, wp.output.height);
            for oy in 0..wp.output.height {
                for ox in 0..wp.output.width {
                    let px = if chromatic {
                        std::array::from_fn(|c| {
                            let (sx, sy) = wp.map_channel(ox as f32, oy as f32, c);
                            sample(sx, sy)[c]
                        })
                    } else {
                        let (sx, sy) = wp.map(ox as f32, oy as f32);
                        sample(sx, sy)
                    };
                    out.set(ox, oy, px);
                }
            }
            out
        }

        fn apply_radial_gain(&self, img: &mut ImageBuf, gain: &RadialGain) {
            for y in 0..img.height() {
                for x in 0..img.width() {
                    let g = gain.at(x as f32, y as f32);
                    let p = img.get(x, y);
                    img.set(x, y, [p[0] * g, p[1] * g, p[2] * g]);
                }
            }
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
                perspective: None,
                lens: None,
                vignette: None,
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
                perspective: None,
                lens: None,
                vignette: None,
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

    #[test]
    fn affine_constructors_have_a_unit_bottom_row() {
        let ext = Extent {
            width: 4,
            height: 3,
        };
        assert_eq!(Transform::identity(ext).m[2], [0.0, 0.0, 1.0]);
        assert_eq!(Transform::rotation(ext, 0.3).m[2], [0.0, 0.0, 1.0]);
        // Identity still maps every point to itself.
        assert_eq!(Transform::identity(ext).map(2.0, 1.0), (2.0, 1.0));
    }

    #[test]
    fn homography_applies_the_perspective_divide() {
        // A pure perspective in x: w = 0.1·x + 1, so output (10, 20) maps to
        // (10/2, 20/2) = (5, 10) after the divide.
        let t = Transform {
            output: Extent {
                width: 16,
                height: 16,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.0, 1.0]],
        };
        let (sx, sy) = t.map(10.0, 20.0);
        assert!(
            (sx - 5.0).abs() < 1e-6 && (sy - 10.0).abs() < 1e-6,
            "{sx}, {sy}"
        );
    }

    #[test]
    fn compose_equals_sequential_mapping() {
        // A perspective B then an affine translation A. Composing the matrices
        // must equal mapping through B, then A, point for point.
        let ext = Extent {
            width: 16,
            height: 16,
        };
        let a = Transform {
            output: ext,
            m: [[1.0, 0.0, 3.0], [0.0, 1.0, -2.0], [0.0, 0.0, 1.0]],
        };
        let b = Transform {
            output: ext,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.0, 1.0]],
        };
        let direct = a.compose(&b).map(10.0, 20.0);
        let (bx, by) = b.map(10.0, 20.0);
        let seq = a.map(bx, by);
        assert!((direct.0 - seq.0).abs() < 1e-6 && (direct.1 - seq.1).abs() < 1e-6);
        assert!(
            (direct.0 - 8.0).abs() < 1e-6 && (direct.1 - 8.0).abs() < 1e-6,
            "{direct:?}"
        );
    }

    #[test]
    fn testbackend_resamples_through_a_homography() {
        // Per-pixel marker r = x + 8y; the perspective samples through the divide.
        // Output (4, 0) → (4/1.4, 0) ≈ (2.86, 0) → nearest source (3, 0).
        let mut src = ImageBuf::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                src.set(x, y, [(x + 8 * y) as f32, 0.0, 0.0]);
            }
        }
        let t = Transform {
            output: Extent {
                width: 8,
                height: 8,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.0, 1.0]],
        };
        let out = TestBackend.resample(&src, &t);
        assert_eq!(out.get(4, 0), [3.0, 0.0, 0.0]);
        assert_eq!(out.get(0, 4), [32.0, 0.0, 0.0]); // w = 1 → (0, 4) exact
    }

    #[test]
    fn keystone_straightens_converging_verticals() {
        // Two bright source pixels lie on a line that converges toward the top
        // (top point at x=8, lower point at x=6). A vertical keystone must lift
        // them onto a single output column — i.e. straighten the vertical.
        let mut src = ImageBuf::new(9, 9);
        src.set(8, 0, [1.0, 1.0, 1.0]);
        src.set(6, 6, [1.0, 1.0, 1.0]);
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: Some(Perspective {
                    vertical: 0.3,
                    horizontal: 0.0,
                }),
                lens: None,
                vignette: None,
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert_eq!((out.width(), out.height()), (9, 9));
        assert_eq!(out.get(7, 1), [1.0, 1.0, 1.0]); // was source (8, 0)
        assert_eq!(out.get(7, 7), [1.0, 1.0, 1.0]); // was source (6, 6)
    }

    #[test]
    fn keystone_zero_is_a_no_op() {
        let mut src = ImageBuf::new(6, 4);
        for (i, p) in src.pixels_mut().iter_mut().enumerate() {
            *p = [i as f32, 0.0, 0.0];
        }
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: Some(Perspective {
                    vertical: 0.0,
                    horizontal: 0.0,
                }),
                lens: None,
                vignette: None,
            },
            ..Settings::default()
        };
        assert_eq!(render(&src, &settings, &TestBackend), src);
    }

    #[test]
    fn warp_with_zero_radial_equals_the_homography() {
        let t = Transform {
            output: Extent {
                width: 8,
                height: 8,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.1, 0.0, 1.0]],
        };
        let w = Warp::from_transform(&t);
        assert_eq!(w.map(3.0, 5.0), t.map(3.0, 5.0));
    }

    #[test]
    fn warp_map_composes_homography_then_radial() {
        // PTLENS keeps the direct radial multiply. Identity homography, unit
        // normalization, a b-term (r²) of 0.1: the point (3, 4) is r = 5 (r² = 25)
        // from the origin, so it scales by 1 + 0.1·25 = 3.5.
        let w = Warp {
            output: Extent {
                width: 8,
                height: 8,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [0.0, 0.0],
            inv_norm: 1.0,
            model: DistortionModel::Ptlens,
            radial: [0.0, 0.1, 0.0, 0.0],
            channel_scale: [CA_IDENTITY; 3],
        };
        let (sx, sy) = w.map(3.0, 4.0);
        assert!(
            (sx - 10.5).abs() < 1e-5 && (sy - 14.0).abs() < 1e-5,
            "{sx}, {sy}"
        );
    }

    #[test]
    fn warp_map_handles_odd_radial_powers() {
        // The PTLENS direct multiply carries the odd `r` term Brown–Conrady could
        // not hold (the c-coefficient). (3, 4) is r = 5 from the origin, so a c of
        // 0.1 scales by 1 + 0.1·5 = 1.5.
        let w = Warp {
            output: Extent {
                width: 8,
                height: 8,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [0.0, 0.0],
            inv_norm: 1.0,
            model: DistortionModel::Ptlens,
            radial: [0.1, 0.0, 0.0, 0.0],
            channel_scale: [CA_IDENTITY; 3],
        };
        let (sx, sy) = w.map(3.0, 4.0);
        assert!(
            (sx - 4.5).abs() < 1e-5 && (sy - 6.0).abs() < 1e-5,
            "{sx}, {sy}"
        );
    }

    #[test]
    fn extreme_keystone_behind_the_plane_maps_outside() {
        // Both keystone amounts at the slider max put the homography weight w ≤ 0
        // at a corner; the guard maps it outside the source (black), not to a
        // sign-flipped or NaN coordinate.
        let t = keystone_transform(
            Extent {
                width: 9,
                height: 9,
            },
            0.8,
            0.8,
        );
        assert_eq!(t.map(0.0, 0.0), (-1.0, -1.0));
    }

    #[test]
    fn lens_distortion_straightens_a_barrel_grid() {
        // Three bright source pixels bow outward at the middle (the barrel
        // signature): columns 15, 16, 15 at rows 6, 10, 14. The POLY5 distortion
        // correction (Newton-inverted at lensfun's focal-scaled normalization)
        // must pull them onto one straight output column (16). The crop/real_focal
        // pick the focal-scaled `NormScale`; under the old half-short-edge scale
        // the same coefficient would over- or under-correct.
        let mut src = ImageBuf::new(21, 21);
        src.set(16, 10, [1.0, 1.0, 1.0]);
        src.set(15, 6, [1.0, 1.0, 1.0]);
        src.set(15, 14, [1.0, 1.0, 1.0]);
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: Some(LensProfile {
                    crop: 1.0,
                    real_focal: 9.03,
                    model: DistortionModel::Poly5,
                    distortion: [0.0, 0.09, 0.0, 0.0],
                    ..LensProfile::default()
                }),
                vignette: None,
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert_eq!((out.width(), out.height()), (21, 21));
        assert_eq!(out.get(16, 10), [1.0, 1.0, 1.0]); // was source (16, 10)
        assert_eq!(out.get(16, 6), [1.0, 1.0, 1.0]); // was source (15, 6)
        assert_eq!(out.get(16, 14), [1.0, 1.0, 1.0]); // was source (15, 14)
    }

    #[test]
    fn empty_lens_profile_is_a_no_op() {
        let mut src = ImageBuf::new(6, 4);
        for (i, p) in src.pixels_mut().iter_mut().enumerate() {
            *p = [i as f32, 0.0, 0.0];
        }
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: Some(LensProfile::default()),
                vignette: None,
            },
            ..Settings::default()
        };
        assert_eq!(render(&src, &settings, &TestBackend), src);
    }

    #[test]
    fn chromatic_aberration_recombines_a_split_target() {
        // A laterally split feature: red fringed outward (x=16), green centered
        // (x=15), blue inward (x=14). The per-channel (constant LINEAR) CA scale
        // samples each back onto one output pixel, recombining them to white. With
        // unit normalization the on-axis scale is the bare `v` term.
        let mut src = ImageBuf::new(17, 17);
        src.set(16, 8, [1.0, 0.0, 0.0]);
        src.set(15, 8, [0.0, 1.0, 0.0]);
        src.set(14, 8, [0.0, 0.0, 1.0]);
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: Some(LensProfile {
                    crop: 1.0,
                    real_focal: 1.0,
                    ca: [[0.0, 0.0, 1.1], [0.0, 0.0, 0.9]],
                    ..LensProfile::default()
                }),
                vignette: None,
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        assert_eq!(out.get(15, 8), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn lens_vignetting_flattens_a_radial_falloff() {
        // The captured image carries the lens's PA falloff (1 + k·r², k < 0, so
        // corners are darker), measured with the corner-anchored radius (r = 1 at
        // the corner). Correction divides it back out at the same corner
        // normalization, flattening it.
        let mut src = ImageBuf::new(9, 9);
        for p in src.pixels_mut() {
            *p = [0.5, 0.5, 0.5];
        }
        let (center, inv_norm) = lens_vignetting_radial(
            Extent {
                width: 9,
                height: 9,
            },
            &LensProfile::default(),
        );
        let falloff = RadialGain {
            center,
            inv_norm,
            poly: [-0.5, 0.0, 0.0],
            reciprocal: false,
        };
        TestBackend.apply_radial_gain(&mut src, &falloff);
        assert!(src.get(0, 0)[0] < src.get(4, 4)[0], "corners darkened");
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: Some(LensProfile {
                    vignetting: [-0.5, 0.0, 0.0],
                    ..LensProfile::default()
                }),
                vignette: None,
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        for p in out.pixels() {
            assert!((p[0] - 0.5).abs() < 1e-5, "flattened: {p:?}");
        }
    }

    #[test]
    fn neutral_ca_leaves_color_unchanged() {
        // ca = identity → all channels share one coordinate; with no distortion
        // the color image is untouched (the single-sample fast path).
        let mut src = ImageBuf::new(8, 8);
        for (i, p) in src.pixels_mut().iter_mut().enumerate() {
            *p = [i as f32, (2 * i) as f32, (3 * i) as f32];
        }
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: Some(LensProfile::default()),
                vignette: None,
            },
            ..Settings::default()
        };
        assert_eq!(render(&src, &settings, &TestBackend), src);
    }

    #[test]
    fn creative_vignette_darkens_corners_and_keeps_center() {
        let mut src = ImageBuf::new(11, 11);
        for p in src.pixels_mut() {
            *p = [0.6, 0.6, 0.6];
        }
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: None,
                vignette: Some(-0.5),
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        // Center is untouched; corners darken by the modeled radial gain.
        assert_eq!(out.get(5, 5), [0.6, 0.6, 0.6]);
        let g = RadialGain {
            center: [5.0, 5.0],
            inv_norm: 2.0 / (11.0_f32 * 11.0 + 11.0 * 11.0).sqrt(),
            poly: [-0.5, 0.0, 0.0],
            reciprocal: false,
        };
        let expected = 0.6 * g.at(0.0, 0.0);
        assert!(out.get(0, 0)[0] < 0.6, "corner darkened");
        assert!(
            (out.get(0, 0)[0] - expected).abs() < 1e-5,
            "{:?}",
            out.get(0, 0)
        );
    }

    #[test]
    fn no_vignette_leaves_the_image_unchanged() {
        let mut src = ImageBuf::new(8, 8);
        for (i, p) in src.pixels_mut().iter_mut().enumerate() {
            *p = [i as f32, 0.0, 0.0];
        }
        let settings = Settings {
            geometry: Geometry {
                crop: None,
                straighten_degrees: 0.0,
                perspective: None,
                lens: None,
                vignette: None,
            },
            ..Settings::default()
        };
        assert_eq!(render(&src, &settings, &TestBackend), src);
    }

    #[test]
    fn backend_is_send_and_sync() {
        // The `Send + Sync` supertrait is what lets a backend move to or be
        // shared across a worker thread (off-thread render/export). Pin it at the
        // type level: this fails to compile if a backend impl ever loses the
        // bound. The `dyn Backend` line confirms a shared trait object carries it
        // too — a future non-`Send` impl could not satisfy the supertrait.
        fn assert_send_sync<T: ?Sized + Send + Sync>() {}
        assert_send_sync::<TestBackend>();
        assert_send_sync::<dyn Backend>();
    }

    // --- Lens normalization, inversion, TCA, and vignetting (lensfun fidelity) -

    /// The analytic forward distortion `r_d / r_u = s_fwd(r_u)` for a model, in
    /// the focal-normalized frame — the reference the engine's `Warp::map` (which
    /// runs output → distorted source) must reproduce.
    fn forward_ratio(model: DistortionModel, radial: [f32; 4], ru: f32) -> f32 {
        match model {
            DistortionModel::None => 1.0,
            DistortionModel::Poly3 => 1.0 + radial[1] * ru * ru,
            DistortionModel::Poly5 => 1.0 + radial[1] * ru * ru + radial[3] * ru.powi(4),
            DistortionModel::Ptlens => {
                let [c, b, a, _] = radial;
                1.0 + ru * (c + ru * (b + ru * a))
            }
        }
    }

    #[test]
    fn norm_scale_matches_lensfun() {
        // inv_norm is lensfun's focal-scaled half-diagonal NormScale, not the old
        // half-short-edge 2/min(w,h).
        let extent = Extent {
            width: 6000,
            height: 4000,
        };
        let lens = LensProfile {
            crop: 1.6,
            real_focal: 24.0,
            model: DistortionModel::Poly5,
            distortion: [0.0, 0.01, 0.0, 0.0],
            ..LensProfile::default()
        };
        let (_, inv_norm, _, _) = lens_radial(extent, &lens);
        let expected = (36.0_f32).hypot(24.0) / 1.6 / (6001.0_f32).hypot(4001.0) / 24.0;
        assert!(
            (inv_norm - expected).abs() < 1e-9,
            "{inv_norm} vs {expected}"
        );
        // And it is *not* the old PanoTools unit (a clear, large divergence).
        assert!((inv_norm - 2.0 / 4000.0).abs() > 1e-4);
    }

    #[test]
    fn off_center_scales_by_min_dimension() {
        // A non-zero CenterX on a non-square frame shifts the optical center by
        // CenterX·min(w-1, h-1)/2 px — the same divisor on both axes (lensfun),
        // not a fraction of each full dimension (which would differ per axis).
        let extent = Extent {
            width: 101,
            height: 61,
        };
        let lens = LensProfile {
            // 0.5 + 0.1 in x means a +0.1 offset in half-shorter-side units.
            center: [0.6, 0.5],
            ..LensProfile::default()
        };
        let (center, _, _, _) = lens_radial(extent, &lens);
        let (cap_w, cap_h) = (100.0_f32, 60.0_f32);
        let half_short = cap_w.min(cap_h) / 2.0;
        assert!((center[0] - (cap_w / 2.0 + 0.1 * 2.0 * half_short)).abs() < 1e-4);
        assert!((center[1] - cap_h / 2.0).abs() < 1e-4);
        // The old "fraction of full width" mapping would have put it at
        // 0.6·(w-1) = 60.0, a different (wrong) place on the long axis.
        assert!((center[0] - 0.6 * cap_w).abs() > 1.0);
    }

    #[test]
    fn vignetting_radius_is_corner_normalized() {
        // The PA vignetting radius is r = 1 at the image corner, unlike the
        // focal-scaled distortion normalization.
        let extent = Extent {
            width: 21,
            height: 13,
        };
        let (center, inv_norm) = lens_vignetting_radial(extent, &LensProfile::default());
        // The outer rim of the corner pixel (half a pixel past its center) sits at
        // r = 1: distance hypot(w, h)/2 from center, times inv_norm = 2/hypot(w, h).
        let rim_r = normalized_radius(-0.5, -0.5, center, inv_norm);
        assert!((rim_r - 1.0).abs() < 1e-5, "corner rim r = {rim_r}");
    }

    /// The source radius (in the focal-normalized frame) a `Warp::map` produced
    /// for the output point `(px, py)`, and the output radius it came from.
    fn mapped_radii(w: &Warp, px: f32, py: f32) -> (f32, f32) {
        let (sx, sy) = w.map(px, py);
        let out_r = ((px - w.center[0]).powi(2) + (py - w.center[1]).powi(2)).sqrt() * w.inv_norm;
        let src_r = ((sx - w.center[0]).powi(2) + (sy - w.center[1]).powi(2)).sqrt() * w.inv_norm;
        (out_r, src_r)
    }

    #[test]
    fn newton_inverts_poly3_to_subpixel() {
        // The geometry stage maps a corrected output radius back to the smaller
        // distorted source radius — lensfun's UnDist direction. Newton solves
        // `r_out = r_src·(1 + k1·r_src²)` for `r_src`; forward-applying the model
        // to the recovered source radius must return the output radius to
        // sub-pixel (the round-trip the direct multiply cannot close exactly).
        let (cx, cy) = (200.0_f32, 150.0_f32);
        let inv_norm = 1.0 / 250.0; // r ≈ 1 near the corner.
        let radial = [0.0, 0.05, 0.0, 0.0];
        let w = Warp {
            output: Extent {
                width: 400,
                height: 300,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [cx, cy],
            inv_norm,
            model: DistortionModel::Poly3,
            radial,
            channel_scale: [CA_IDENTITY; 3],
        };
        let (out_r, src_r) = mapped_radii(&w, cx + 200.0, cy + 120.0);
        let recovered = src_r * forward_ratio(DistortionModel::Poly3, radial, src_r);
        let err_px = (recovered - out_r).abs() / inv_norm;
        assert!(err_px < 0.01, "poly3 round-trip off by {err_px} px");
    }

    #[test]
    fn newton_inverts_poly5_to_subpixel() {
        let (cx, cy) = (200.0_f32, 150.0_f32);
        let inv_norm = 1.0 / 250.0;
        let radial = [0.0, 0.03, 0.0, 0.01];
        let w = Warp {
            output: Extent {
                width: 400,
                height: 300,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [cx, cy],
            inv_norm,
            model: DistortionModel::Poly5,
            radial,
            channel_scale: [CA_IDENTITY; 3],
        };
        let (out_r, src_r) = mapped_radii(&w, cx + 200.0, cy + 120.0);
        let recovered = src_r * forward_ratio(DistortionModel::Poly5, radial, src_r);
        let err_px = (recovered - out_r).abs() / inv_norm;
        assert!(err_px < 0.01, "poly5 round-trip off by {err_px} px");
    }

    #[test]
    fn ptlens_uses_the_direct_multiply() {
        // PTLENS keeps the direct radial multiply (no Newton): the source radius
        // is exactly r_d = r·(1 + c·r + b·r² + a·r³) evaluated at the output
        // radius. This pins the register's decision to leave PTLENS direct.
        let (cx, cy) = (100.0_f32, 100.0_f32);
        let inv_norm = 1.0 / 100.0;
        let radial = [0.02, -0.01, 0.005, 0.0];
        let w = Warp {
            output: Extent {
                width: 200,
                height: 200,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [cx, cy],
            inv_norm,
            model: DistortionModel::Ptlens,
            radial,
            channel_scale: [CA_IDENTITY; 3],
        };
        let (px, py) = (cx + 60.0, cy + 80.0); // r = 100 px → r_d unit = 1.0
        let (sx, sy) = w.map(px, py);
        let r = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() * inv_norm;
        let s = 1.0 + r * (radial[0] + r * (radial[1] + r * radial[2]));
        assert!((sx - (cx + (px - cx) * s)).abs() < 1e-3, "{sx}");
        assert!((sy - (cy + (py - cy) * s)).abs() < 1e-3, "{sy}");
    }

    #[test]
    fn poly3_tca_corrects_radial_term() {
        // The per-channel radial CA scale s_c(r) = b·r² + c·r + v is applied at the
        // sampled radius — the full radius dependence, not just the on-axis v.
        let (cx, cy) = (50.0_f32, 50.0_f32);
        let inv_norm = 1.0 / 50.0;
        let red = [0.01_f32, 0.02, 1.0]; // [b, c, v]
        let w = Warp {
            output: Extent {
                width: 100,
                height: 100,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [cx, cy],
            inv_norm,
            model: DistortionModel::None,
            radial: [0.0, 0.0, 0.0, 0.0],
            channel_scale: [red, CA_IDENTITY, CA_IDENTITY],
        };
        for &(px, py) in &[(cx + 10.0, cy), (cx + 30.0, cy), (cx + 40.0, cy + 20.0)] {
            let (rx, _ry) = w.map_channel(px, py, 0);
            let r = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() * inv_norm;
            let s = red[2] + r * (red[1] + r * red[0]);
            let expected_x = cx + (px - cx) * s;
            assert!(
                (rx - expected_x).abs() < 1e-4,
                "r={r}: {rx} vs {expected_x}"
            );
        }
    }

    #[test]
    fn linear_tca_is_constant_scale() {
        // LINEAR TCA is the degenerate [0, 0, k]: a constant radial scale at every
        // radius (no b/c radius dependence).
        let (cx, cy) = (50.0_f32, 50.0_f32);
        let inv_norm = 1.0 / 50.0;
        let red = [0.0_f32, 0.0, 1.05];
        let w = Warp {
            output: Extent {
                width: 100,
                height: 100,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [cx, cy],
            inv_norm,
            model: DistortionModel::None,
            radial: [0.0, 0.0, 0.0, 0.0],
            channel_scale: [red, CA_IDENTITY, CA_IDENTITY],
        };
        assert!(w.has_chromatic());
        for &(px, py) in &[(cx + 10.0, cy), (cx + 40.0, cy + 25.0)] {
            let (rx, _ry) = w.map_channel(px, py, 0);
            assert!((rx - (cx + (px - cx) * 1.05)).abs() < 1e-4);
        }
        // Green is the untouched reference.
        let green_w = Warp {
            channel_scale: [CA_IDENTITY; 3],
            ..w
        };
        assert!(!green_w.has_chromatic());
    }

    #[test]
    fn pa_vignetting_flattens_known_falloff() {
        // A flat field with a multi-term PA falloff baked in (corner-normalized,
        // negative k's so the corners darken) returns to flat when the lens
        // vignetting correction divides the same falloff back out.
        let extent = Extent {
            width: 17,
            height: 11,
        };
        let mut src = ImageBuf::new(extent.width, extent.height);
        for p in src.pixels_mut() {
            *p = [0.5, 0.5, 0.5];
        }
        let terms = [-0.45_f32, 0.12, -0.05];
        let (center, inv_norm) = lens_vignetting_radial(extent, &LensProfile::default());
        TestBackend.apply_radial_gain(
            &mut src,
            &RadialGain {
                center,
                inv_norm,
                poly: terms,
                reciprocal: false,
            },
        );
        // The corners really are darker than the center before correction.
        assert!(src.get(0, 0)[0] < src.get(8, 5)[0]);
        let settings = Settings {
            geometry: Geometry {
                lens: Some(LensProfile {
                    vignetting: terms,
                    ..LensProfile::default()
                }),
                ..Geometry::default()
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        for p in out.pixels() {
            assert!((p[0] - 0.5).abs() < 1e-5, "flattened: {p:?}");
        }
    }

    #[test]
    fn distortion_grid_straightens_to_lensfun() {
        // For each model, a straight output grid maps (output → distorted source)
        // to exactly the forward-distortion the lensfun model defines, at lensfun's
        // focal-scaled normalization — proving the C2 scale and C3 inversion. An
        // off-center, non-square frame exercises the center scaling (a square frame
        // hides the min-dimension bug).
        let extent = Extent {
            width: 160,
            height: 100,
        };
        let cases = [
            (DistortionModel::Poly3, [0.0_f32, 0.06, 0.0, 0.0]),
            (DistortionModel::Poly5, [0.0, 0.04, 0.0, 0.015]),
            (DistortionModel::Ptlens, [0.01, -0.02, 0.008, 0.0]),
        ];
        for (model, radial) in cases {
            let lens = LensProfile {
                center: [0.55, 0.48], // off-center
                crop: 1.5,
                real_focal: 20.0,
                model,
                distortion: radial,
                ..LensProfile::default()
            };
            let (center, inv_norm, m, r) = lens_radial(extent, &lens);
            let w = Warp {
                output: extent,
                m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                center,
                inv_norm,
                model: m,
                radial: r,
                channel_scale: [CA_IDENTITY; 3],
            };
            // Sample a grid of straight output points spanning to the corners. For
            // each, `map` lands on the distorted source; forward-applying the
            // lensfun model to that source radius must return to the straight
            // output point (sub-pixel). PTLENS closes this exactly via the direct
            // multiply, POLY3/POLY5 via the Newton inverse.
            for &gx in &[10.0_f32, 60.0, 110.0, 155.0] {
                for &gy in &[5.0_f32, 40.0, 75.0, 95.0] {
                    let (sx, sy) = w.map(gx, gy);
                    let (sdx, sdy) = (sx - center[0], sy - center[1]);
                    let src_r = (sdx * sdx + sdy * sdy).sqrt() * inv_norm;
                    if src_r == 0.0 {
                        continue;
                    }
                    // Forward-distort the source point back toward the output.
                    let fwd = forward_ratio(model, radial, src_r);
                    let (ex, ey) = (center[0] + sdx * fwd, center[1] + sdy * fwd);
                    let err = ((gx - ex).powi(2) + (gy - ey).powi(2)).sqrt();
                    assert!(err < 0.25, "{model:?} at ({gx},{gy}): off by {err} px");
                }
            }
        }
    }

    #[test]
    fn tca_target_recombines_to_lensfun() {
        // A point split into R/G/B by a POLY3 TCA forward fringe is recombined onto
        // one output pixel by the per-channel radial correction. The red channel's
        // corrected sample radius must equal r·s_red(r) at on-axis and corner
        // radii — proving the radial term, not just the on-axis v.
        let (cx, cy) = (80.0_f32, 60.0_f32);
        let inv_norm = 1.0 / 100.0;
        let red = [0.02_f32, 0.015, 1.0]; // [b, c, v]
        let blue = [-0.018_f32, -0.012, 1.0];
        let w = Warp {
            output: Extent {
                width: 160,
                height: 120,
            },
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            center: [cx, cy],
            inv_norm,
            model: DistortionModel::None,
            radial: [0.0, 0.0, 0.0, 0.0],
            channel_scale: [red, CA_IDENTITY, blue],
        };
        for &(px, py) in &[(cx + 5.0, cy + 2.0), (cx + 70.0, cy + 45.0)] {
            let r = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() * inv_norm;
            for (chan, poly) in [(0, red), (2, blue)] {
                let (rx, ry) = w.map_channel(px, py, chan);
                let s = poly[2] + r * (poly[1] + r * poly[0]);
                let sample_r = (((rx - cx).powi(2) + (ry - cy).powi(2)).sqrt() * inv_norm) / r;
                assert!(
                    (sample_r - s).abs() < 1e-4,
                    "chan {chan}: {sample_r} vs {s}"
                );
            }
        }
    }

    #[test]
    fn vignetting_flat_field_recovers_uniform() {
        // A PA-darkened flat returns to uniform across center, edges, and corners
        // — including an off-center optical axis on a non-square frame.
        let extent = Extent {
            width: 23,
            height: 15,
        };
        let mut src = ImageBuf::new(extent.width, extent.height);
        for p in src.pixels_mut() {
            *p = [0.4, 0.4, 0.4];
        }
        let terms = [-0.4_f32, 0.1, -0.03];
        // Off-center, non-square: the falloff is baked about the optical axis, and
        // the correction must divide it out about the same off-center axis.
        let profile = LensProfile {
            center: [0.52, 0.47],
            vignetting: terms,
            ..LensProfile::default()
        };
        let (center, inv_norm) = lens_vignetting_radial(extent, &profile);
        TestBackend.apply_radial_gain(
            &mut src,
            &RadialGain {
                center,
                inv_norm,
                poly: terms,
                reciprocal: false,
            },
        );
        let settings = Settings {
            geometry: Geometry {
                lens: Some(profile),
                ..Geometry::default()
            },
            ..Settings::default()
        };
        let out = render(&src, &settings, &TestBackend);
        for &(x, y) in &[
            (0, 0),
            (22, 0),
            (0, 14),
            (22, 14),
            (11, 7),
            (22, 7),
            (11, 0),
        ] {
            assert!(
                (out.get(x, y)[0] - 0.4).abs() < 1e-5,
                "({x},{y}) = {:?}",
                out.get(x, y)
            );
        }
    }
}
