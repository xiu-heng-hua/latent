//! Edit settings: adjustments, geometry, and the document model.
//!
//! The whole edit state for one image is a [`Settings`] value. It is plain,
//! serializable data: there is no execution order stored here — the engine
//! applies the parts in a fixed order. A [`Document`] is what a sidecar stores:
//! a schema version plus one or more variants of the same source.

use serde::{Deserialize, Serialize};

pub mod history;
pub use history::History;

/// The complete edit state for one image.
///
/// Three separated parts: the one global development applied everywhere, any
/// local adjustments layered on top, and the geometry (framing/orientation)
/// applied to the result. The default value is neutral — it develops the image
/// without changing it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The global adjustments, applied to the whole image.
    pub global: Adjustments,
    /// Localized adjustments layered on top of the global ones.
    pub locals: Vec<LocalAdjustment>,
    /// Framing and orientation of the rendered image.
    pub geometry: Geometry,
}

/// A localized adjustment: a set of [`Adjustments`] blended over the image at a
/// given opacity. The same `Adjustments` type is used here as globally — a
/// local adjustment is the same kind of edit, scoped to part of the image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalAdjustment {
    /// The region this adjustment acts on (in original-image coordinates).
    pub mask: Mask,
    /// The adjustments to apply where the mask is.
    pub adjustments: Adjustments,
    /// Blend strength in `[0, 1]`; `1.0` applies the adjustments fully.
    pub opacity: f32,
}

impl Default for LocalAdjustment {
    fn default() -> Self {
        Self {
            mask: Mask::default(),
            adjustments: Adjustments::default(),
            opacity: 1.0,
        }
    }
}

/// The region of a local adjustment: one or more shapes combined (by taking the
/// strongest weight), optionally inverted. Coordinates are normalized `[0, 1]`
/// over the original image, so a mask is resolution-independent and lives in
/// SOURCE space — it is evaluated before geometry, never reprojected.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    /// Shapes combined into the mask; empty means "selects nothing".
    pub shapes: Vec<MaskShape>,
    /// How each shape combines with the running result, parallel to `shapes`:
    /// `ops[i]` applies to `shapes[i]`. The first shape is always the base (its
    /// op is ignored) and a missing entry defaults to [`MaskOp::Add`], so an
    /// empty `ops` reproduces the plain union of all shapes.
    pub ops: Vec<MaskOp>,
    /// Apply to the complement of the shapes' region instead.
    pub invert: bool,
}

impl Mask {
    /// The mask weight in `[0, 1]` at a normalized point `(px, py)`, given the
    /// SOURCE pixel `pixel` there (so value-driven shapes — luminosity, hue — can
    /// select on image content, not just position; position-only shapes ignore it).
    pub fn weight_at(&self, px: f32, py: f32, pixel: [f32; 3]) -> f32 {
        let mut w = 0.0_f32;
        for (i, shape) in self.shapes.iter().enumerate() {
            let s = shape.weight_at(px, py, pixel);
            // The first shape is the base; later shapes combine via their op (a
            // missing op — including an empty list — defaults to Add/union).
            let op = if i == 0 {
                MaskOp::Add
            } else {
                self.ops.get(i).copied().unwrap_or(MaskOp::Add)
            };
            w = match op {
                MaskOp::Add => w.max(s),
                MaskOp::Subtract => w * (1.0 - s),
                MaskOp::Intersect => w.min(s),
            };
        }
        if self.invert { 1.0 - w } else { w }
    }
}

/// How a [`MaskShape`] combines with the shapes before it in a [`Mask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MaskOp {
    /// Union — take the stronger of the running weight and this shape.
    #[default]
    Add,
    /// Carve this shape out of the running weight (smoothly).
    Subtract,
    /// Keep only where both the running weight and this shape are present.
    Intersect,
}

/// One masking primitive. More shapes are added over time. Not `Copy`: the
/// brush carries a `Vec` of dabs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MaskShape {
    /// A linear gradient (position).
    Gradient(Gradient),
    /// A radial (elliptical) falloff (position).
    Radial(Radial),
    /// A luminosity range (value): selects a band of brightness.
    Luminosity(LuminanceRange),
    /// A hue/saturation range (value): selects a band of color.
    ColorRange(ColorRange),
    /// A freehand brush (position): painted and erased dabs.
    Brush(Brush),
}

impl MaskShape {
    /// The shape's weight in `[0, 1]` at a normalized point, given the SOURCE
    /// pixel there. Position-only shapes ignore `pixel`; value-driven ones use it.
    pub fn weight_at(&self, px: f32, py: f32, pixel: [f32; 3]) -> f32 {
        match self {
            MaskShape::Gradient(g) => g.weight_at(px, py),
            MaskShape::Radial(r) => r.weight_at(px, py),
            MaskShape::Luminosity(l) => l.weight_at(pixel),
            MaskShape::ColorRange(c) => c.weight_at(pixel),
            MaskShape::Brush(b) => b.weight_at(px, py),
        }
    }
}

/// A relative-luminance estimate used only for mask *selection* (Rec. 709
/// weights). It need not match the working space's colorimetric luminance — it
/// just answers "how bright is this pixel" for picking a tonal range.
fn select_luma(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// A smooth band: `1` for `v` in `[lo, hi]`, ramping to `0` over `feather` on
/// each side. `feather <= 0` gives a hard band.
fn band(v: f32, lo: f32, hi: f32, feather: f32) -> f32 {
    if feather <= 0.0 {
        return if v >= lo && v <= hi { 1.0 } else { 0.0 };
    }
    let rising = ((v - (lo - feather)) / feather).clamp(0.0, 1.0);
    let falling = (((hi + feather) - v) / feather).clamp(0.0, 1.0);
    rising.min(falling)
}

/// Hue (in turns, `[0, 1)`) and saturation (`[0, 1]`) of a linear RGB pixel,
/// for hue-range selection. Hue is the HSV hue; saturation is `(max-min)/max`.
fn hue_sat(p: [f32; 3]) -> (f32, f32) {
    let max = p[0].max(p[1]).max(p[2]);
    let min = p[0].min(p[1]).min(p[2]);
    let chroma = max - min;
    let sat = if max <= 0.0 { 0.0 } else { chroma / max };
    if chroma <= 1e-9 {
        return (0.0, sat);
    }
    let h = if max == p[0] {
        (p[1] - p[2]) / chroma
    } else if max == p[1] {
        (p[2] - p[0]) / chroma + 2.0
    } else {
        (p[0] - p[1]) / chroma + 4.0
    };
    ((h / 6.0).rem_euclid(1.0), sat)
}

/// Shortest distance between two hues on the `[0, 1)` circle (so `0.95` and
/// `0.05` are `0.1` apart, not `0.9`).
fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs();
    d.min(1.0 - d)
}

/// A luminosity-range selection: full weight where the pixel's brightness is in
/// `[lo, hi]`, feathered on each side. Position-independent — it selects tones
/// wherever they occur. (See [`select_luma`].)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LuminanceRange {
    pub lo: f32,
    pub hi: f32,
    pub feather: f32,
}

impl LuminanceRange {
    fn weight_at(&self, pixel: [f32; 3]) -> f32 {
        band(select_luma(pixel), self.lo, self.hi, self.feather)
    }
}

/// A hue-range selection: full weight where the pixel's hue is within
/// `hue_width` of `hue` (on the color wheel) and at least `sat_min` saturated,
/// feathered over `feather`. Position-independent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorRange {
    /// Target hue in turns, `[0, 1)`.
    pub hue: f32,
    /// Half-width of the accepted hue band, in turns.
    pub hue_width: f32,
    /// Minimum saturation to be selected (rejects near-neutral pixels).
    pub sat_min: f32,
    /// Hue feather, in turns.
    pub feather: f32,
}

impl ColorRange {
    fn weight_at(&self, pixel: [f32; 3]) -> f32 {
        let (h, s) = hue_sat(pixel);
        if s < self.sat_min {
            return 0.0;
        }
        let over = (hue_distance(h, self.hue) - self.hue_width).max(0.0);
        if self.feather <= 0.0 {
            if over <= 0.0 { 1.0 } else { 0.0 }
        } else {
            (1.0 - over / self.feather).clamp(0.0, 1.0)
        }
    }
}

/// A linear gradient: weight ramps `0 → 1` from the line through `(x0, y0)` to
/// the line through `(x1, y1)`, clamped flat outside that band. Normalized.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Gradient {
    fn weight_at(&self, px: f32, py: f32) -> f32 {
        let (dx, dy) = (self.x1 - self.x0, self.y1 - self.y0);
        let len2 = dx * dx + dy * dy;
        if len2 <= 1e-12 {
            return 0.0;
        }
        (((px - self.x0) * dx + (py - self.y0) * dy) / len2).clamp(0.0, 1.0)
    }
}

/// A radial mask: weight `1` within `radius` of the center `(cx, cy)`, fading to
/// `0` over `feather`, in normalized coordinates. (Distance is measured in
/// normalized units, so the falloff is elliptical on non-square images.)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Radial {
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
    pub feather: f32,
}

impl Radial {
    fn weight_at(&self, px: f32, py: f32) -> f32 {
        let d = ((px - self.cx).powi(2) + (py - self.cy).powi(2)).sqrt();
        if self.feather <= 0.0 {
            if d <= self.radius { 1.0 } else { 0.0 }
        } else {
            (1.0 - (d - self.radius) / self.feather).clamp(0.0, 1.0)
        }
    }
}

/// A freehand brush mask: an ordered list of [`Dab`]s. Painting dabs add
/// coverage (union); erasing dabs carve it back out. Order matters — a later
/// erase removes earlier paint. Normalized, position-based (ignores pixel value).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Brush {
    pub dabs: Vec<Dab>,
}

/// One brush stamp: a soft disc at `(x, y)` of `radius`, fading to `0` over
/// `feather` (normalized units, like [`Radial`]). `erase` subtracts coverage
/// instead of adding it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub feather: f32,
    pub erase: bool,
}

impl Brush {
    fn weight_at(&self, px: f32, py: f32) -> f32 {
        let mut w = 0.0_f32;
        for dab in &self.dabs {
            let d = ((px - dab.x).powi(2) + (py - dab.y).powi(2)).sqrt();
            let cov = if dab.feather <= 0.0 {
                if d <= dab.radius { 1.0 } else { 0.0 }
            } else {
                (1.0 - (d - dab.radius) / dab.feather).clamp(0.0, 1.0)
            };
            // Paint unions coverage; erase carves it back out (smoothly).
            w = if dab.erase {
                w * (1.0 - cov)
            } else {
                w.max(cov)
            };
        }
        w
    }
}

/// The catalog of adjustments.
///
/// Each field is optional: `Some` means the adjustment is active with the given
/// parameters, `None` means it is off. The empty (default) value is neutral and
/// changes nothing. There is deliberately no ordering field, because the engine
/// applies adjustments in a fixed order rather than the order they appear in.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Adjustments {
    /// Editable white balance, on top of the as-shot balance applied at decode.
    pub white_balance: Option<WhiteBalance>,
    /// Exposure in stops (EV): a linear multiply by `2^stops`.
    pub exposure: Option<f32>,
    /// Tonal shaping across the contrast/highlights/shadows/blacks ranges.
    pub tone: Option<SelectiveTone>,
    /// Master + per-channel tone curves, edited as control points.
    pub curves: Option<Curves>,
    /// Saturation factor: `0` is grayscale, `1` is unchanged, `> 1` is more.
    pub saturation: Option<f32>,
    /// Per-hue-band hue/saturation/luminance mixer.
    pub hsl: Option<Hsl>,
    /// Channel mixer: each output channel is a linear mix of the input channels.
    pub channel_mixer: Option<ChannelMixer>,
    /// Unsharp-mask sharpening.
    pub sharpen: Option<Sharpen>,
    /// Midtone local contrast ("clarity").
    pub clarity: Option<Clarity>,
    /// Dehaze strength in `[0, 1]`: removes an estimated atmospheric veil.
    pub dehaze: Option<f32>,
    /// Edge-preserving noise reduction.
    pub noise_reduction: Option<NoiseReduction>,
}

/// Master + per-channel tone curves, as control points `(input, output)` in the
/// perceptual `[0, 1]` domain. `master` shapes all channels; `red`/`green`/`blue`
/// add per-channel grading on top. An empty point list is the identity.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Curves {
    pub master: Vec<(f32, f32)>,
    pub red: Vec<(f32, f32)>,
    pub green: Vec<(f32, f32)>,
    pub blue: Vec<(f32, f32)>,
}

/// A per-hue-band color mixer (the "HSL" tool): eight evenly-spaced hue bands
/// around the wheel — red, orange, yellow, green, aqua, blue, purple, magenta —
/// each with a `[hue, sat, lum]` adjustment. `hue` shifts the band's hue (in
/// turns), `sat` and `lum` scale its saturation and lightness by `1 + value`.
/// All-zero is neutral. A pixel is influenced by its two nearest bands, so a
/// color at a band center is driven only by that band.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hsl {
    /// Per-band `[hue, sat, lum]`, indexed red (0) … magenta (7).
    pub bands: [[f32; 3]; 8],
}

/// A channel mixer: each output channel is a linear combination of the input
/// channels — a 3x3 matrix whose rows are the output R/G/B and columns the input
/// R/G/B. The default is the identity (no change). Also the natural home for
/// fixed color matrices such as a monochrome conversion or a channel swap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelMixer {
    pub matrix: [[f32; 3]; 3],
}

impl Default for ChannelMixer {
    fn default() -> Self {
        Self {
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }
}

/// Editable white balance as a temp/tint pair; both `0` is neutral. Positive
/// `temp` warms (more red, less blue); positive `tint` shifts toward magenta.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WhiteBalance {
    pub temp: f32,
    pub tint: f32,
}

/// Tonal shaping split across four ranges; all `0` is neutral. Each value is
/// roughly `[-1, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SelectiveTone {
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub blacks: f32,
}

/// Unsharp-mask sharpening: `amount` is the strength (`0` = off), `radius` the
/// blur radius (in pixels) of the unsharp base.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharpen {
    pub amount: f32,
    pub radius: f32,
}

impl Default for Sharpen {
    fn default() -> Self {
        // A sensible default radius so a freshly-enabled slider has somewhere
        // to sit; `amount` 0 keeps it a no-op until the user raises it.
        Self {
            amount: 0.0,
            radius: 2.0,
        }
    }
}

/// Clarity: midtone local contrast. An unsharp recombine over a *broad* base,
/// weighted toward the midtones so it adds punch without haloing the highlights
/// or crushing the shadows. `amount` is the strength (`0` = off, positive adds
/// contrast, negative softens); `radius` is the blur radius (in pixels) of the
/// low-frequency base — large, because clarity shapes broad local contrast
/// rather than fine detail (that is what sharpening does).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clarity {
    pub amount: f32,
    pub radius: f32,
}

impl Default for Clarity {
    fn default() -> Self {
        // A broad default radius so a freshly-enabled slider reads as local
        // contrast, not sharpening; `amount` 0 keeps it a no-op until raised.
        Self {
            amount: 0.0,
            radius: 40.0,
        }
    }
}

/// Edge-preserving noise reduction (a bilateral filter), with **independent
/// luminance and color strengths** — the two kinds of sensor noise behave
/// differently, so they are denoised separately. `radius` is the spatial
/// neighborhood (in pixels). `luminance` is the luma range scale: keep it gentle,
/// since luminance carries the detail. `color` is the chroma range scale: it can
/// be stronger, because color noise is low-frequency blotches that smooth away
/// without costing perceived detail. Both `0` is off (a no-op).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseReduction {
    pub radius: f32,
    pub luminance: f32,
    pub color: f32,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        // A small default radius so a freshly-enabled control has somewhere to
        // sit; both strengths 0 keep it a no-op until the user raises them.
        Self {
            radius: 2.0,
            luminance: 0.0,
            color: 0.0,
        }
    }
}

/// Framing and orientation of the rendered image: an optional crop and a
/// straighten angle. The default is the identity — no crop, no rotation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Geometry {
    /// Crop rectangle in normalized coordinates, or `None` for the full frame.
    pub crop: Option<Crop>,
    /// Straighten angle in degrees (positive = counter-clockwise); `0` is level.
    pub straighten_degrees: f32,
}

impl Geometry {
    /// True if this geometry changes nothing (no crop, no rotation).
    pub fn is_identity(&self) -> bool {
        self.crop.is_none() && self.straighten_degrees == 0.0
    }
}

/// A crop rectangle in normalized `[0, 1]` coordinates relative to the source
/// image: `(x, y)` is the top-left corner and `(width, height)` the size.
///
/// Normalized so the crop is independent of resolution (it applies the same to
/// a preview and a full-size render).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A saved edit document: a schema version plus one or more variants —
/// independent edits of the same source image. This is what a sidecar stores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Schema version, so the format can evolve compatibly.
    pub version: u32,
    /// One or more independent edits of the source; always at least one.
    pub variants: Vec<Settings>,
}

impl Document {
    /// Current sidecar schema version.
    pub const VERSION: u32 = 1;

    /// Serialize to a (pretty) RON string for the sidecar file.
    pub fn to_ron(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())
    }

    /// Parse from a RON string. A sidecar whose `version` is newer than this
    /// build understands is rejected (rather than silently misreading a future
    /// schema); an older one still loads, since `#[serde(default)]` on the
    /// settings structs fills any fields it predates.
    pub fn from_ron(text: &str) -> Result<Self, String> {
        let doc: Document = ron::from_str(text).map_err(|e| e.to_string())?;
        if doc.version > Self::VERSION {
            return Err(format!(
                "sidecar schema version {} is newer than supported {}",
                doc.version,
                Self::VERSION
            ));
        }
        Ok(doc)
    }
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            variants: vec![Settings::default()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_neutral() {
        let s = Settings::default();
        assert_eq!(s.global, Adjustments::default());
        assert_eq!(s.global.exposure, None);
        assert_eq!(s.global.sharpen, None);
        assert!(s.locals.is_empty());
        assert!(s.geometry.is_identity());
    }

    #[test]
    fn default_geometry_is_identity_and_a_change_is_not() {
        assert!(Geometry::default().is_identity());
        let tilted = Geometry {
            crop: None,
            straighten_degrees: 1.0,
        };
        assert!(!tilted.is_identity());
    }

    #[test]
    fn local_adjustment_defaults_to_full_opacity() {
        assert!((LocalAdjustment::default().opacity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn empty_mask_selects_nothing() {
        assert_eq!(Mask::default().weight_at(0.5, 0.5, [0.0; 3]), 0.0);
    }

    #[test]
    fn gradient_ramps_from_zero_to_one_across_the_band() {
        // Horizontal gradient: 0 at the left edge, 1 at the right edge.
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
        assert_eq!(mask.weight_at(0.0, 0.5, [0.0; 3]), 0.0);
        assert!((mask.weight_at(0.5, 0.5, [0.0; 3]) - 0.5).abs() < 1e-6);
        assert_eq!(mask.weight_at(1.0, 0.5, [0.0; 3]), 1.0);
        assert_eq!(mask.weight_at(-0.3, 0.5, [0.0; 3]), 0.0); // clamped before the band
        assert_eq!(mask.weight_at(1.3, 0.5, [0.0; 3]), 1.0); // clamped after the band
    }

    #[test]
    fn radial_is_full_at_center_and_zero_far_away() {
        let mask = Mask {
            shapes: vec![MaskShape::Radial(Radial {
                cx: 0.5,
                cy: 0.5,
                radius: 0.2,
                feather: 0.1,
            })],
            ops: Vec::new(),
            invert: false,
        };
        assert_eq!(mask.weight_at(0.5, 0.5, [0.0; 3]), 1.0); // center: inside radius
        assert_eq!(mask.weight_at(0.5, 0.3, [0.0; 3]), 1.0); // distance 0.2 = the radius edge
        assert_eq!(mask.weight_at(0.5, 0.05, [0.0; 3]), 0.0); // distance 0.45 > radius + feather
        let mid = mask.weight_at(0.5, 0.5 - 0.25, [0.0; 3]); // within the feather band
        assert!(
            (0.0..=1.0).contains(&mid) && mid > 0.0 && mid < 1.0,
            "{mid}"
        );
    }

    #[test]
    fn invert_flips_the_weight() {
        let g = MaskShape::Gradient(Gradient {
            x0: 0.0,
            y0: 0.5,
            x1: 1.0,
            y1: 0.5,
        });
        let normal = Mask {
            shapes: vec![g.clone()],
            ops: Vec::new(),
            invert: false,
        };
        let inverted = Mask {
            shapes: vec![g],
            ops: Vec::new(),
            invert: true,
        };
        assert!(
            (normal.weight_at(0.25, 0.5, [0.0; 3]) + inverted.weight_at(0.25, 0.5, [0.0; 3]) - 1.0)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn luminosity_selects_a_brightness_band_regardless_of_position() {
        // Select shadows: luma in [0, 0.3], small feather. Value-driven, so the
        // (px, py) point doesn't matter — only the pixel's brightness.
        let mask = Mask {
            shapes: vec![MaskShape::Luminosity(LuminanceRange {
                lo: 0.0,
                hi: 0.3,
                feather: 0.05,
            })],
            ops: Vec::new(),
            invert: false,
        };
        let dark = mask.weight_at(0.1, 0.9, [0.1, 0.1, 0.1]); // luma 0.1 → in band
        let bright = mask.weight_at(0.1, 0.9, [0.9, 0.9, 0.9]); // luma 0.9 → out
        assert!((dark - 1.0).abs() < 1e-6, "dark selected: {dark}");
        assert_eq!(bright, 0.0, "bright rejected");
    }

    #[test]
    fn color_range_selects_a_hue_and_rejects_neutrals() {
        // Select reds (hue ~0), needing some saturation.
        let mask = Mask {
            shapes: vec![MaskShape::ColorRange(ColorRange {
                hue: 0.0,
                hue_width: 0.05,
                sat_min: 0.2,
                feather: 0.05,
            })],
            ops: Vec::new(),
            invert: false,
        };
        let red = mask.weight_at(0.5, 0.5, [0.8, 0.1, 0.1]); // hue ~0, saturated
        let blue = mask.weight_at(0.5, 0.5, [0.1, 0.1, 0.8]); // hue ~0.67
        let gray = mask.weight_at(0.5, 0.5, [0.5, 0.5, 0.5]); // unsaturated
        assert!((red - 1.0).abs() < 1e-6, "red selected: {red}");
        assert_eq!(blue, 0.0, "blue rejected");
        assert_eq!(gray, 0.0, "neutral rejected (below sat_min)");
    }

    #[test]
    fn brush_paints_a_disc_and_erase_carves_it_out() {
        // One paint dab makes a hard disc; a later erase dab removes its center,
        // leaving a ring. Brush is position-based, so the pixel value is ignored.
        let mut brush = Brush {
            dabs: vec![Dab {
                x: 0.5,
                y: 0.5,
                radius: 0.2,
                feather: 0.0,
                erase: false,
            }],
        };
        let painted = MaskShape::Brush(brush.clone());
        assert_eq!(painted.weight_at(0.5, 0.5, [0.0; 3]), 1.0, "painted center");
        assert_eq!(
            painted.weight_at(0.9, 0.9, [0.0; 3]),
            0.0,
            "outside the dab"
        );

        brush.dabs.push(Dab {
            x: 0.5,
            y: 0.5,
            radius: 0.1,
            feather: 0.0,
            erase: true,
        });
        let carved = MaskShape::Brush(brush);
        assert_eq!(carved.weight_at(0.5, 0.5, [0.0; 3]), 0.0, "center erased");
        assert_eq!(
            carved.weight_at(0.65, 0.5, [0.0; 3]),
            1.0,
            "ring still painted"
        );
    }

    #[test]
    fn mask_ops_subtract_and_intersect_combine_shapes() {
        // Two overlapping hard discs. A is left, B is right; they overlap in the
        // middle. Subtract carves B out of A; Intersect keeps only the overlap;
        // empty ops unions (the prior behavior).
        let a = MaskShape::Radial(Radial {
            cx: 0.4,
            cy: 0.5,
            radius: 0.2,
            feather: 0.0,
        });
        let b = MaskShape::Radial(Radial {
            cx: 0.6,
            cy: 0.5,
            radius: 0.2,
            feather: 0.0,
        });
        let only_a = (0.3, 0.5);
        let overlap = (0.5, 0.5);
        let only_b = (0.7, 0.5);

        let union = Mask {
            shapes: vec![a.clone(), b.clone()],
            ops: Vec::new(), // empty → all Add: the union, unchanged from before
            invert: false,
        };
        assert_eq!(union.weight_at(only_a.0, only_a.1, [0.0; 3]), 1.0);
        assert_eq!(union.weight_at(only_b.0, only_b.1, [0.0; 3]), 1.0);

        let subtract = Mask {
            shapes: vec![a.clone(), b.clone()],
            ops: vec![MaskOp::Add, MaskOp::Subtract],
            invert: false,
        };
        assert_eq!(
            subtract.weight_at(only_a.0, only_a.1, [0.0; 3]),
            1.0,
            "A kept"
        );
        assert_eq!(
            subtract.weight_at(overlap.0, overlap.1, [0.0; 3]),
            0.0,
            "B carved"
        );

        let intersect = Mask {
            shapes: vec![a, b],
            ops: vec![MaskOp::Add, MaskOp::Intersect],
            invert: false,
        };
        assert_eq!(
            intersect.weight_at(overlap.0, overlap.1, [0.0; 3]),
            1.0,
            "overlap"
        );
        assert_eq!(
            intersect.weight_at(only_a.0, only_a.1, [0.0; 3]),
            0.0,
            "A-only dropped"
        );
        assert_eq!(
            intersect.weight_at(only_b.0, only_b.1, [0.0; 3]),
            0.0,
            "B-only dropped"
        );
    }

    #[test]
    fn default_document_has_one_neutral_variant() {
        let d = Document::default();
        assert_eq!(d.version, Document::VERSION);
        assert_eq!(d.variants, vec![Settings::default()]);
    }

    #[test]
    fn empty_document_round_trips() {
        let d = Document::default();
        assert_eq!(Document::from_ron(&d.to_ron().unwrap()).unwrap(), d);
    }

    #[test]
    fn populated_document_round_trips() {
        let edited = Settings {
            global: Adjustments {
                white_balance: Some(WhiteBalance {
                    temp: 0.1,
                    tint: -0.2,
                }),
                exposure: Some(0.5),
                tone: Some(SelectiveTone {
                    contrast: 0.2,
                    highlights: 0.0,
                    shadows: 0.1,
                    blacks: 0.0,
                }),
                curves: Some(Curves {
                    master: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
                    red: vec![(0.0, 0.1)],
                    green: Vec::new(),
                    blue: Vec::new(),
                }),
                saturation: Some(1.2),
                hsl: Some(Hsl {
                    bands: {
                        let mut b = [[0.0_f32; 3]; 8];
                        b[5] = [0.02, 0.3, -0.1]; // tweak the blue band
                        b
                    },
                }),
                channel_mixer: Some(ChannelMixer::default()),
                sharpen: Some(Sharpen {
                    amount: 0.3,
                    radius: 1.5,
                }),
                clarity: Some(Clarity {
                    amount: 0.4,
                    radius: 30.0,
                }),
                dehaze: Some(0.6),
                noise_reduction: Some(NoiseReduction {
                    radius: 2.0,
                    luminance: 0.05,
                    color: 0.1,
                }),
            },
            locals: vec![LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Gradient(Gradient {
                        x0: 0.0,
                        y0: 0.5,
                        x1: 1.0,
                        y1: 0.5,
                    })],
                    ops: Vec::new(),
                    invert: true,
                },
                adjustments: Adjustments::default(),
                opacity: 0.5,
            }],
            geometry: Geometry {
                crop: Some(Crop {
                    x: 0.1,
                    y: 0.1,
                    width: 0.8,
                    height: 0.8,
                }),
                straighten_degrees: 2.5,
            },
        };
        // Two variants: a neutral one and the edited one.
        let d = Document {
            version: Document::VERSION,
            variants: vec![Settings::default(), edited],
        };
        assert_eq!(Document::from_ron(&d.to_ron().unwrap()).unwrap(), d);
    }

    #[test]
    fn partial_sidecar_fills_missing_fields_with_defaults() {
        // An older/minimal sidecar that omits fields added later must still load,
        // the missing pieces falling back to their neutral defaults
        // (`#[serde(default)]`) — so the format can evolve without breaking files.
        let text = "(version: 1, variants: [(global: (exposure: Some(0.5)))])";
        let doc = Document::from_ron(text).expect("partial sidecar should load");
        let s = &doc.variants[0];
        assert_eq!(s.global.exposure, Some(0.5));
        assert_eq!(s.global.white_balance, None); // omitted → default
        assert_eq!(s.global.sharpen, None); // omitted → default
        assert!(s.locals.is_empty()); // omitted → default
        assert!(s.geometry.is_identity()); // omitted → default
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        // A sidecar from a future build is refused rather than silently misread.
        let text = "(version: 999, variants: [(global: ())])";
        let err = Document::from_ron(text).expect_err("newer version should fail");
        assert!(err.contains("newer"), "unexpected error: {err}");
    }
}
