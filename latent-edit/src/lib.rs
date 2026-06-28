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

/// Force a single value finite, then clamp it into `[lo, hi]`.
///
/// Order matters: `f32::clamp` returns `NaN` unchanged for a `NaN` input, so the
/// non-finite scrub to `neutral` must come *first* — only then does the clamp
/// see a real number. The neutral is the field's own default, so scrubbing a
/// corrupt value restores the no-op rather than an arbitrary edge of the range.
fn finite_clamped(x: f32, neutral: f32, lo: f32, hi: f32) -> f32 {
    let scrubbed = if x.is_finite() { x } else { neutral };
    scrubbed.clamp(lo, hi)
}

/// Force a single value finite (to `neutral` if not), leaving its magnitude
/// otherwise untouched — for fields with no documented bound.
fn finite_or(x: f32, neutral: f32) -> f32 {
    if x.is_finite() { x } else { neutral }
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
///
/// The default is an all-zero band (a single point at `0` with no feather),
/// which selects a negligible sliver — a benign no-op, the right neutral when a
/// field is missing from an older sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
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
///
/// The default is all-zero: a zero-width band at hue `0` that, with `sat_min` 0,
/// selects nothing — a benign no-op, the right neutral when a field is missing
/// from an older sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
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
///
/// The default is a zero-length gradient (both endpoints at the origin), which
/// is treated as empty (`weight_at` returns `0`) — a benign no-op, the right
/// neutral when a field is missing from an older sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
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
///
/// The default is all-zero: a zero-radius, zero-feather disc that selects
/// nothing — a benign no-op, the right neutral when a field is missing from an
/// older sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
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
///
/// The default is all-zero: a zero-radius paint dab that covers nothing — a
/// benign no-op, the right neutral when a field is missing from an older sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
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
///
/// `preserve_luminosity` is Photoshop's "preserve luminosity" checkbox (default
/// off): when on, each row is normalized to sum to 1 before applying, so a
/// neutral gray `[v,v,v]` maps to `[v,v,v]` and the mix can't shift overall
/// brightness. Note this preserves a *neutral's value* (rows summing to 1), not
/// true colorimetric luminance across all colors — the standard Photoshop
/// semantic, despite the name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelMixer {
    pub matrix: [[f32; 3]; 3],
    pub preserve_luminosity: bool,
}

impl Default for ChannelMixer {
    fn default() -> Self {
        Self {
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            preserve_luminosity: false,
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
    /// Discrete right-angle re-framing (rotate 90° / flip) the user applies on
    /// top of the developed image, composed into the same single resample as
    /// straighten/keystone. Default is the identity (no turn, no flip).
    pub orientation: Orientation,
    /// Straighten angle in degrees (positive = counter-clockwise); `0` is level.
    pub straighten_degrees: f32,
    /// Keystone (perspective) correction, or `None` for none.
    pub perspective: Option<Perspective>,
    /// Lens correction profile, or `None` for none. Applied in the geometry
    /// stage, composed into the same single resample as straighten/keystone.
    pub lens: Option<LensProfile>,
    /// Creative vignette amount applied after crop (OUTPUT space): negative
    /// darkens the corners, positive lightens them; `None` (or `0`) is none.
    pub vignette: Option<f32>,
    /// Scale the corrected/warped image about the frame center so it fills the
    /// output with no black border wedges (lensfun's `GetAutoScale`). Off by
    /// default, so a distortion/keystone/straighten correction leaves the wedges
    /// for the user to crop; when on it may *minify* the image to fit.
    pub auto_scale: bool,
    /// Output sharpening applied *after* geometry (OUTPUT space, post-resample),
    /// using the same perceptual L\* luma-sharpen as capture sharpen. `None` (or a
    /// `0` amount) is off; distinct from [`Adjustments::sharpen`], which runs in
    /// SOURCE space before geometry.
    pub output_sharpen: Option<Sharpen>,
}

impl Geometry {
    /// True if this geometry changes nothing (no crop, orientation, rotation,
    /// keystone, lens, vignette, auto-scale, or output sharpen).
    pub fn is_identity(&self) -> bool {
        self.crop.is_none()
            && self.orientation.is_identity()
            && self.straighten_degrees == 0.0
            && self.perspective.is_none()
            && self.lens.is_none()
            && self.vignette.is_none()
            && !self.auto_scale
            && self.output_sharpen.is_none()
    }
}

/// Keystone (perspective) correction. `vertical` corrects converging verticals
/// (the camera tilted up or down); `horizontal` corrects converging horizontals
/// (panned left or right). Each is a normalized amount in roughly `[-1, 1]`,
/// where `0` is no correction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Perspective {
    pub vertical: f32,
    pub horizontal: f32,
}

/// A discrete right-angle re-framing the user applies on top of the developed
/// (already display-oriented) image: zero or more 90° turns plus an optional
/// horizontal flip. The eight reachable states form the dihedral group of the
/// square (four rotations × an optional mirror), the same space LibRaw's
/// orientation codes live in, so any rotate/flip the user clicks composes
/// exactly with the next.
///
/// It is modeled as a quarter-turn count (`0..4`, clockwise) followed by an
/// optional left↔right flip. This pair reaches all eight states and composes
/// without a lookup table: rotating advances the count, flipping toggles the
/// mirror and reverses the turn direction so the count and flip stay in a
/// canonical normal form. The default — no turns, no flip — is the identity,
/// the right neutral when an older sidecar omits the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Orientation {
    /// Clockwise quarter-turns, kept in `0..4`.
    pub quarter_turns: u8,
    /// Mirror left↔right, applied after the rotation.
    pub flip: bool,
}

impl Orientation {
    /// The identity re-framing (no turn, no flip).
    pub const IDENTITY: Self = Self {
        quarter_turns: 0,
        flip: false,
    };

    /// Whether this is the identity (changes nothing). The default `Geometry`
    /// stays a no-op when it carries a default `Orientation`.
    pub fn is_identity(self) -> bool {
        self.quarter_turns.is_multiple_of(4) && !self.flip
    }

    /// Whether the re-framing exchanges the width and height axes — true for the
    /// two odd (90°/270°) quarter-turns.
    pub fn swaps_axes(self) -> bool {
        self.quarter_turns % 2 == 1
    }

    /// Add a clockwise 90° turn.
    pub fn rotate_cw(self) -> Self {
        Self {
            quarter_turns: (self.quarter_turns + 1) % 4,
            flip: self.flip,
        }
    }

    /// Add a counter-clockwise 90° turn (three clockwise turns).
    pub fn rotate_ccw(self) -> Self {
        Self {
            quarter_turns: (self.quarter_turns + 3) % 4,
            flip: self.flip,
        }
    }

    /// Toggle a horizontal (left↔right) mirror. Composing the new flip *after*
    /// the existing rotation reverses the stored turn direction, which keeps the
    /// `(quarter_turns, flip)` pair in its canonical normal form so two flips
    /// cancel and `flip ∘ rotate` stays a single reachable state.
    pub fn flip_h(self) -> Self {
        Self {
            quarter_turns: (4 - self.quarter_turns % 4) % 4,
            flip: !self.flip,
        }
    }

    /// Toggle a vertical (top↔bottom) mirror — a horizontal flip plus a 180°
    /// turn, expressed through the same normal form.
    pub fn flip_v(self) -> Self {
        self.flip_h().rotate_cw().rotate_cw()
    }
}

/// The forward distortion model a [`LensProfile`]'s coefficients describe.
///
/// The model determines how the engine *inverts* the forward map `r_d(r_u)` to
/// undistort. The two even-order polynomial models (POLY3, POLY5; Brown 1966)
/// have no closed-form inverse and are solved by Newton iteration; the
/// PanoTools/Hugin "abc" model (PTLENS) keeps the direct radial multiply where
/// it is the defined operation. `None` (an all-zero profile) skips distortion
/// entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DistortionModel {
    /// No distortion model (the coefficients are all zero — a no-op).
    #[default]
    None,
    /// 3rd-order polynomial, `r_d = r_u·(1 + k1·r_u²)` once focal-normalized.
    /// Inverted by Newton iteration.
    Poly3,
    /// 5th-order polynomial, `r_d = r_u·(1 + k1·r_u² + k2·r_u⁴)`. Inverted by
    /// Newton iteration.
    Poly5,
    /// PanoTools/Hugin model, `r_d = r_u·(1 + c·r_u + b·r_u² + a·r_u³)` once
    /// focal-normalized. Applied directly as a radial multiply.
    Ptlens,
}

/// Lens correction profile: the optical parameters that undo a lens's geometric
/// distortion. Coefficients are pure data — a synthetic model in tests, or a
/// lens database (lensfun) at runtime.
///
/// The conventions are pinned here so a database lookup can map its coefficients
/// in without silent mis-scaling. The radius for the distortion and TCA models is
/// normalized by the **focal-scaled half-diagonal** (lensfun's natural unit: `r`
/// is the on-sensor distance in units of one real focal length), built from
/// `crop` and `real_focal`; the `distortion`/`ca` coefficients are stored in that
/// same focal frame. The optical `center` is normalized to the frame, where
/// `(0.5, 0.5)` is the image center and an offset is measured against the
/// **shorter** image side (`min(w, h)/2`, the same divisor on both axes).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensProfile {
    /// Optical center, normalized to the frame (`(0.5, 0.5)` = image center). An
    /// offset from center is in units of half the shorter image side.
    pub center: [f32; 2],
    /// Camera crop factor (sensor diagonal relative to 35 mm full frame). Used,
    /// with `real_focal`, to normalize the radius into lensfun's focal frame.
    pub crop: f32,
    /// Real focal length (mm) the calibration was measured at — lensfun's
    /// `RealFocal`, which can differ from the nominal focal on some lenses.
    pub real_focal: f32,
    /// Which forward distortion model `distortion` describes (selects the
    /// inversion path: Newton for POLY3/POLY5, direct multiply for PTLENS).
    pub model: DistortionModel,
    /// Radial distortion coefficients of the *forward* map, in the focal frame,
    /// laid out by `model`:
    /// - POLY3: `k1` in slot 1 (`r²`), `r_d = r_u·(1 + k1·r_u²)`.
    /// - POLY5: `k1` in slot 1 (`r²`), `k2` in slot 3 (`r⁴`).
    /// - PTLENS: `[c, b, a, 0]` so the direct multiply is
    ///   `r_d = r_u·(1 + c·r + b·r² + a·r³)`.
    ///
    /// All zero (with `model` = `None`) is no distortion.
    pub distortion: [f32; 4],
    /// Lateral (transverse) chromatic aberration as lensfun's POLY3 per-channel
    /// radial scale, for red and blue relative to green (the reference). Channel
    /// `c` samples at radius `r·(b·r² + c·r + v)` where `ca[0] = [b_R, c_R, v_R]`
    /// and `ca[1] = [b_B, c_B, v_B]`. Identity (`[0, 0, 1]`) is no CA; a LINEAR
    /// model is the degenerate `[0, 0, k]`.
    pub ca: [[f32; 3]; 2],
    /// Vignetting falloff (lensfun's PA model): the captured brightness is
    /// `ideal · (1 + v0·r² + v1·r⁴ + v2·r⁶)` at the corner-normalized radius `r`
    /// (the `v`s are negative for darker corners), so correction divides it back
    /// out. All zero is no vignetting.
    pub vignetting: [f32; 3],
}

impl Default for LensProfile {
    /// A centered, distortion-, aberration-, and vignetting-free profile (a no-op).
    fn default() -> Self {
        Self {
            center: [0.5, 0.5],
            crop: 1.0,
            real_focal: 1.0,
            model: DistortionModel::None,
            distortion: [0.0, 0.0, 0.0, 0.0],
            ca: [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
            vignetting: [0.0, 0.0, 0.0],
        }
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

// --- Sanitize-on-load ------------------------------------------------------
//
// A sidecar is untrusted input: RON accepts `NaN`/`inf` as float literals, and a
// hand-edited or corrupt file can carry an out-of-range `opacity` or feather
// verbatim. The documented invariants ("opacity in `[0, 1]`", adjustments
// "roughly `[-1, 1]`", a feather `>= 0`) are otherwise only advisory. The
// `sanitize` pass below makes them real: it walks the whole settings tree and,
// for every float, scrubs any non-finite value to its neutral and clamps every
// documented range. Run once on load (see [`Document::from_ron`]), this lets the
// rest of the codebase — the render math, the tone/curve evaluation — assume
// finite, in-range values. Sanitizing an already-clean `Settings` is a no-op.

impl Settings {
    /// Scrub every non-finite float and clamp every documented range across the
    /// whole settings tree (global + locals + geometry), in place.
    pub fn sanitize(&mut self) {
        self.global.sanitize();
        for local in &mut self.locals {
            local.adjustments.sanitize();
            // Opacity is a blend strength in `[0, 1]`; full opacity is neutral.
            local.opacity = finite_clamped(local.opacity, 1.0, 0.0, 1.0);
            local.mask.sanitize();
        }
        self.geometry.sanitize();
    }
}

impl Adjustments {
    /// Scrub/clamp every adjustment. Used for both the global adjustments and
    /// each local's, so the two cannot drift.
    fn sanitize(&mut self) {
        // Exposure (stops) and saturation are signed/unbounded factors; only
        // finiteness is enforced. Neutral exposure is `0` EV, neutral saturation
        // is `1` (unchanged); saturation has no meaning below `0`.
        if let Some(exposure) = &mut self.exposure {
            *exposure = finite_or(*exposure, 0.0);
        }
        if let Some(saturation) = &mut self.saturation {
            *saturation = finite_clamped(*saturation, 1.0, 0.0, f32::MAX);
        }
        // Dehaze is a strength in `[0, 1]`; `0` (and `None`) is off.
        if let Some(dehaze) = &mut self.dehaze {
            *dehaze = finite_clamped(*dehaze, 0.0, 0.0, 1.0);
        }
        if let Some(wb) = &mut self.white_balance {
            wb.temp = finite_or(wb.temp, 0.0);
            wb.tint = finite_or(wb.tint, 0.0);
        }
        if let Some(tone) = &mut self.tone {
            // Each range is neutral at `0`, documented as roughly `[-1, 1]`.
            tone.contrast = finite_clamped(tone.contrast, 0.0, -1.0, 1.0);
            tone.highlights = finite_clamped(tone.highlights, 0.0, -1.0, 1.0);
            tone.shadows = finite_clamped(tone.shadows, 0.0, -1.0, 1.0);
            tone.blacks = finite_clamped(tone.blacks, 0.0, -1.0, 1.0);
        }
        if let Some(curves) = &mut self.curves {
            curves.sanitize();
        }
        if let Some(hsl) = &mut self.hsl {
            for band in &mut hsl.bands {
                for v in band {
                    *v = finite_or(*v, 0.0);
                }
            }
        }
        if let Some(mixer) = &mut self.channel_mixer {
            for row in &mut mixer.matrix {
                for v in row {
                    *v = finite_or(*v, 0.0);
                }
            }
        }
        if let Some(sharpen) = &mut self.sharpen {
            // Amount is signed-ish strength; radius is a magnitude in pixels.
            sharpen.amount = finite_or(sharpen.amount, 0.0);
            sharpen.radius = finite_clamped(sharpen.radius, 0.0, 0.0, f32::MAX);
        }
        if let Some(clarity) = &mut self.clarity {
            clarity.amount = finite_or(clarity.amount, 0.0);
            clarity.radius = finite_clamped(clarity.radius, 0.0, 0.0, f32::MAX);
        }
        if let Some(nr) = &mut self.noise_reduction {
            // Radius and both strengths are magnitudes (`>= 0`); `0` is off.
            nr.radius = finite_clamped(nr.radius, 0.0, 0.0, f32::MAX);
            nr.luminance = finite_clamped(nr.luminance, 0.0, 0.0, f32::MAX);
            nr.color = finite_clamped(nr.color, 0.0, 0.0, f32::MAX);
        }
    }
}

impl Curves {
    /// Drop any control point with a non-finite coordinate and clamp the
    /// survivors into the `[0, 1]` perceptual domain. The render-side evaluator
    /// stays total regardless, but cleaning the stored points here keeps the data
    /// model honest and the two agree on which points are valid.
    fn sanitize(&mut self) {
        for channel in [
            &mut self.master,
            &mut self.red,
            &mut self.green,
            &mut self.blue,
        ] {
            channel.retain(|(x, y)| x.is_finite() && y.is_finite());
            for (x, y) in channel.iter_mut() {
                *x = x.clamp(0.0, 1.0);
                *y = y.clamp(0.0, 1.0);
            }
        }
    }
}

impl Mask {
    /// Scrub/clamp every shape in the mask, in place.
    fn sanitize(&mut self) {
        for shape in &mut self.shapes {
            shape.sanitize();
        }
    }
}

impl MaskShape {
    /// Scrub/clamp the shape's parameters: positions and centers are coordinates
    /// (finite only), radii and feathers are magnitudes (finite and `>= 0`).
    fn sanitize(&mut self) {
        match self {
            MaskShape::Gradient(g) => {
                g.x0 = finite_or(g.x0, 0.0);
                g.y0 = finite_or(g.y0, 0.0);
                g.x1 = finite_or(g.x1, 0.0);
                g.y1 = finite_or(g.y1, 0.0);
            }
            MaskShape::Radial(r) => {
                r.cx = finite_or(r.cx, 0.0);
                r.cy = finite_or(r.cy, 0.0);
                r.radius = finite_clamped(r.radius, 0.0, 0.0, f32::MAX);
                r.feather = finite_clamped(r.feather, 0.0, 0.0, f32::MAX);
            }
            MaskShape::Luminosity(l) => {
                l.lo = finite_or(l.lo, 0.0);
                l.hi = finite_or(l.hi, 0.0);
                l.feather = finite_clamped(l.feather, 0.0, 0.0, f32::MAX);
            }
            MaskShape::ColorRange(c) => {
                c.hue = finite_or(c.hue, 0.0);
                c.hue_width = finite_clamped(c.hue_width, 0.0, 0.0, f32::MAX);
                c.sat_min = finite_clamped(c.sat_min, 0.0, 0.0, f32::MAX);
                c.feather = finite_clamped(c.feather, 0.0, 0.0, f32::MAX);
            }
            MaskShape::Brush(b) => {
                for dab in &mut b.dabs {
                    dab.x = finite_or(dab.x, 0.0);
                    dab.y = finite_or(dab.y, 0.0);
                    dab.radius = finite_clamped(dab.radius, 0.0, 0.0, f32::MAX);
                    dab.feather = finite_clamped(dab.feather, 0.0, 0.0, f32::MAX);
                }
            }
        }
    }
}

impl Geometry {
    /// Scrub/clamp the geometry: the straighten angle and the keystone/lens/
    /// vignette parameters (finite), and the crop rectangle (finite, with a
    /// non-negative size).
    fn sanitize(&mut self) {
        // Keep the discrete re-framing in its canonical normal form: an
        // out-of-range turn count (a hand-edited sidecar) wraps into `0..4`.
        self.orientation.quarter_turns %= 4;
        self.straighten_degrees = finite_or(self.straighten_degrees, 0.0);
        if let Some(crop) = &mut self.crop {
            crop.x = finite_or(crop.x, 0.0);
            crop.y = finite_or(crop.y, 0.0);
            crop.width = finite_clamped(crop.width, 0.0, 0.0, f32::MAX);
            crop.height = finite_clamped(crop.height, 0.0, 0.0, f32::MAX);
        }
        if let Some(p) = &mut self.perspective {
            p.vertical = finite_or(p.vertical, 0.0);
            p.horizontal = finite_or(p.horizontal, 0.0);
        }
        if let Some(lens) = &mut self.lens {
            // A non-finite optical center falls back to the frame center.
            lens.center[0] = finite_or(lens.center[0], 0.5);
            lens.center[1] = finite_or(lens.center[1], 0.5);
            // The crop/focal divisors must be finite and non-zero or the radius
            // normalization blows up; fall back to the no-op unit scale.
            lens.crop = finite_clamped(lens.crop, 1.0, f32::MIN_POSITIVE, f32::MAX);
            lens.real_focal = finite_clamped(lens.real_focal, 1.0, f32::MIN_POSITIVE, f32::MAX);
            for d in &mut lens.distortion {
                *d = finite_or(*d, 0.0);
            }
            for channel in &mut lens.ca {
                // The radial CA terms default to the green-equivalent identity
                // [b, c, v] = [0, 0, 1] (unit scale, no offset).
                channel[0] = finite_or(channel[0], 0.0);
                channel[1] = finite_or(channel[1], 0.0);
                channel[2] = finite_or(channel[2], 1.0);
            }
            for v in &mut lens.vignetting {
                *v = finite_or(*v, 0.0);
            }
        }
        if let Some(vignette) = &mut self.vignette {
            *vignette = finite_or(*vignette, 0.0);
        }
        if let Some(sharpen) = &mut self.output_sharpen {
            // Same scrub as the global sharpen: amount is a signed-ish strength,
            // radius a non-negative magnitude in pixels.
            sharpen.amount = finite_or(sharpen.amount, 0.0);
            sharpen.radius = finite_clamped(sharpen.radius, 0.0, 0.0, f32::MAX);
        }
        // `auto_scale` is a plain bool — nothing to scrub.
    }
}

/// A saved edit document: a schema version plus one or more variants —
/// independent edits of the same source image. This is what a sidecar stores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Document {
    /// Schema version, so the format can evolve compatibly.
    pub version: u32,
    /// One or more independent edits of the source; always at least one.
    pub variants: Vec<Settings>,
    /// Display names for the variants, parallel to `variants` (`names[i]` names
    /// `variants[i]`). A name is UI metadata, not develop data, so it lives here
    /// rather than on `Settings` — keeping the develop settings and their history
    /// equality untouched. The vector may be shorter than `variants` (or empty): a
    /// missing or empty entry means "unnamed", and the UI then shows a positional
    /// fallback. `#[serde(default)]` lets a sidecar written before names existed
    /// load with this defaulting to empty.
    #[serde(default)]
    pub names: Vec<String>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            variants: vec![Settings::default()],
            names: Vec::new(),
        }
    }
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
    ///
    /// Every loaded variant is then [sanitized](Settings::sanitize): a sidecar is
    /// untrusted input, so any non-finite float is scrubbed to its neutral and
    /// every documented range is clamped before the value is handed back. The
    /// result is always safe, in-range data — or a clean `Err` for a parse
    /// failure — never a panic and never a propagated `NaN`/`inf`.
    pub fn from_ron(text: &str) -> Result<Self, String> {
        let mut doc: Document = ron::from_str(text).map_err(|e| e.to_string())?;
        if doc.version > Self::VERSION {
            return Err(format!(
                "sidecar schema version {} is newer than supported {}",
                doc.version,
                Self::VERSION
            ));
        }
        for variant in &mut doc.variants {
            variant.sanitize();
        }
        Ok(doc)
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
            straighten_degrees: 1.0,
            ..Geometry::default()
        };
        assert!(!tilted.is_identity());
        let keystoned = Geometry {
            perspective: Some(Perspective {
                vertical: 0.2,
                horizontal: 0.0,
            }),
            ..Geometry::default()
        };
        assert!(!keystoned.is_identity());
        let corrected = Geometry {
            lens: Some(LensProfile {
                model: DistortionModel::Poly3,
                distortion: [0.0, -0.1, 0.0, 0.0],
                ..LensProfile::default()
            }),
            ..Geometry::default()
        };
        assert!(!corrected.is_identity());
        let auto_scaled = Geometry {
            auto_scale: true,
            ..Geometry::default()
        };
        assert!(!auto_scaled.is_identity());
        let sharpened = Geometry {
            output_sharpen: Some(Sharpen {
                amount: 1.0,
                radius: 1.0,
            }),
            ..Geometry::default()
        };
        assert!(!sharpened.is_identity());
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
                orientation: Orientation {
                    quarter_turns: 1,
                    flip: false,
                },
                straighten_degrees: 2.5,
                perspective: Some(Perspective {
                    vertical: -0.15,
                    horizontal: 0.1,
                }),
                lens: Some(LensProfile {
                    center: [0.49, 0.51],
                    crop: 1.6,
                    real_focal: 24.0,
                    model: DistortionModel::Poly5,
                    distortion: [0.0, -0.08, 0.0, 0.02],
                    ca: [[0.0, 0.0, 1.001], [0.0, 0.0, 0.998]],
                    vignetting: [-0.2, 0.05, 0.0],
                }),
                vignette: Some(0.3),
                auto_scale: true,
                output_sharpen: Some(Sharpen {
                    amount: 0.6,
                    radius: 1.5,
                }),
            },
        };
        // Two variants: a neutral one and the edited one.
        let d = Document {
            version: Document::VERSION,
            variants: vec![Settings::default(), edited],
            names: Vec::new(),
        };
        assert_eq!(Document::from_ron(&d.to_ron().unwrap()).unwrap(), d);
    }

    #[test]
    fn document_names_round_trip_and_old_sidecar_loads() {
        // Variant names live in a parallel `names` vector on `Document`. A document
        // carrying names round-trips through RON unchanged.
        let doc = Document {
            version: Document::VERSION,
            variants: vec![Settings::default(), Settings::default()],
            names: vec!["Portrait".to_owned(), "Black & White".to_owned()],
        };
        let back = Document::from_ron(&doc.to_ron().unwrap()).unwrap();
        assert_eq!(back, doc);
        assert_eq!(back.names, vec!["Portrait", "Black & White"]);

        // An old sidecar written before `names` existed (no `names` field) still
        // loads, the field defaulting to empty — so the UI falls back to a
        // positional label and nothing breaks. `Document::VERSION` is unchanged.
        let old = "(version: 1, variants: [(global: ()), (global: ())])";
        let loaded = Document::from_ron(old).expect("a sidecar without names should load");
        assert_eq!(loaded.variants.len(), 2);
        assert!(loaded.names.is_empty(), "missing names default to empty");

        // A names vector shorter than variants is allowed (the tail is unnamed) and
        // survives a round-trip.
        let ragged = Document {
            version: Document::VERSION,
            variants: vec![Settings::default(), Settings::default()],
            names: vec!["Only the first".to_owned()],
        };
        let back = Document::from_ron(&ragged.to_ron().unwrap()).unwrap();
        assert_eq!(back, ragged);
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
    fn old_lens_sidecar_loads_with_default_focal_geometry() {
        // A sidecar written before the focal-frame fields existed carries only
        // `center`/`distortion`/`vignetting`. It must still load, the new
        // `crop`/`real_focal`/`model`/`ca` falling back to the no-op defaults
        // (`#[serde(default)]`) so the radius normalization stays well-defined.
        let text = "(version: 1, variants: [(geometry: (lens: Some((\
            center: (0.5, 0.5), distortion: (0.0, -0.1, 0.0, 0.0), \
            vignetting: (-0.2, 0.05, 0.0)))))])";
        let doc = Document::from_ron(text).expect("old lens sidecar should load");
        let lens = doc.variants[0]
            .geometry
            .lens
            .expect("lens should be present");
        assert_eq!(lens.center, [0.5, 0.5]);
        assert_eq!(lens.distortion, [0.0, -0.1, 0.0, 0.0]);
        assert_eq!(lens.vignetting, [-0.2, 0.05, 0.0]);
        // The omitted focal-frame fields take their neutral defaults.
        assert_eq!(lens.crop, 1.0);
        assert_eq!(lens.real_focal, 1.0);
        assert_eq!(lens.model, DistortionModel::None);
        assert_eq!(lens.ca, [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]);
    }

    #[test]
    fn channel_mixer_default_is_off() {
        // The preserve-luminosity toggle defaults off, leaving the raw creative
        // matrix to be applied as authored.
        assert!(!ChannelMixer::default().preserve_luminosity);
    }

    #[test]
    fn channel_mixer_serde_old_sidecar_defaults_off() {
        // A sidecar written before the toggle existed (no `preserve_luminosity`)
        // must load with it off (`#[serde(default)]` + the struct's own Default).
        let text = "(version: 1, variants: [(global: (channel_mixer: Some((\
            matrix: ((0.5, 0.3, 0.2), (0.2, 0.6, 0.2), (0.1, 0.1, 0.8))))))])";
        let doc = Document::from_ron(text).expect("old channel-mixer sidecar should load");
        let cm = doc.variants[0]
            .global
            .channel_mixer
            .expect("channel mixer should be present");
        assert_eq!(
            cm.matrix,
            [[0.5, 0.3, 0.2], [0.2, 0.6, 0.2], [0.1, 0.1, 0.8]]
        );
        assert!(!cm.preserve_luminosity, "omitted toggle must default off");
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        // A sidecar from a future build is refused rather than silently misread.
        let text = "(version: 999, variants: [(global: ())])";
        let err = Document::from_ron(text).expect_err("newer version should fail");
        assert!(err.contains("newer"), "unexpected error: {err}");
    }

    #[test]
    fn sanitize_replaces_non_finite_with_neutral() {
        // NaN/inf anywhere in the tree are scrubbed to each field's neutral.
        let mut s = Settings {
            global: Adjustments {
                exposure: Some(f32::NAN),
                saturation: Some(f32::INFINITY),
                curves: Some(Curves {
                    master: vec![(0.0, 0.0), (f32::NAN, 0.5), (1.0, 1.0)],
                    red: Vec::new(),
                    green: Vec::new(),
                    blue: Vec::new(),
                }),
                ..Adjustments::default()
            },
            locals: vec![LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Radial(Radial {
                        cx: 0.5,
                        cy: 0.5,
                        radius: 0.2,
                        feather: f32::NAN,
                    })],
                    ops: Vec::new(),
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            }],
            geometry: Geometry::default(),
        };
        s.sanitize();

        assert_eq!(s.global.exposure, Some(0.0)); // NaN → neutral exposure
        assert_eq!(s.global.saturation, Some(1.0)); // inf → neutral saturation
        // The NaN-x curve point is dropped; the finite ones survive.
        let master = &s.global.curves.as_ref().unwrap().master;
        assert_eq!(master, &vec![(0.0, 0.0), (1.0, 1.0)]);
        // The NaN feather is scrubbed to its neutral (0 → hard edge), finite.
        let MaskShape::Radial(r) = &s.locals[0].mask.shapes[0] else {
            panic!("expected a radial");
        };
        assert_eq!(r.feather, 0.0);
        assert!(r.feather.is_finite());
    }

    #[test]
    fn sanitize_clamps_opacity_out_of_range() {
        let mut over = LocalAdjustment {
            opacity: 5.0,
            ..LocalAdjustment::default()
        };
        let mut under = LocalAdjustment {
            opacity: -2.0,
            ..LocalAdjustment::default()
        };
        let mut s = Settings {
            locals: vec![over.clone(), under.clone()],
            ..Settings::default()
        };
        s.sanitize();
        assert_eq!(s.locals[0].opacity, 1.0); // 5.0 → 1.0
        assert_eq!(s.locals[1].opacity, 0.0); // -2.0 → 0.0

        // A non-finite opacity scrubs to the neutral (1.0) before clamping.
        over.opacity = f32::NAN;
        under.opacity = f32::NEG_INFINITY;
        let mut s = Settings {
            locals: vec![over, under],
            ..Settings::default()
        };
        s.sanitize();
        assert_eq!(s.locals[0].opacity, 1.0);
        assert_eq!(s.locals[1].opacity, 1.0);
    }

    #[test]
    fn sanitize_clamps_adjustment_ranges() {
        let mut s = Settings {
            global: Adjustments {
                tone: Some(SelectiveTone {
                    contrast: 5.0,    // past the documented [-1, 1]
                    highlights: -3.0, // past the documented [-1, 1]
                    shadows: 0.2,     // in range, untouched
                    blacks: 0.0,
                }),
                dehaze: Some(2.5), // past the documented [0, 1]
                ..Adjustments::default()
            },
            ..Settings::default()
        };
        s.sanitize();
        let tone = s.global.tone.unwrap();
        assert_eq!(tone.contrast, 1.0);
        assert_eq!(tone.highlights, -1.0);
        assert_eq!(tone.shadows, 0.2);
        assert_eq!(s.global.dehaze, Some(1.0));
    }

    #[test]
    fn sanitize_is_a_no_op_on_clean_settings() {
        // A valid, populated document survives sanitize unchanged (idempotence).
        let s = Settings {
            global: Adjustments {
                exposure: Some(0.5),
                saturation: Some(1.2),
                tone: Some(SelectiveTone {
                    contrast: 0.3,
                    highlights: -0.2,
                    shadows: 0.1,
                    blacks: 0.0,
                }),
                dehaze: Some(0.4),
                curves: Some(Curves {
                    master: vec![(0.0, 0.0), (0.5, 0.6), (1.0, 1.0)],
                    ..Curves::default()
                }),
                ..Adjustments::default()
            },
            locals: vec![LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Radial(Radial {
                        cx: 0.5,
                        cy: 0.5,
                        radius: 0.2,
                        feather: 0.1,
                    })],
                    ops: Vec::new(),
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 0.5,
            }],
            geometry: Geometry::default(),
        };
        let mut sanitized = s.clone();
        sanitized.sanitize();
        assert_eq!(sanitized, s);
    }

    #[test]
    fn from_ron_sanitizes_on_load() {
        // A hand-written sidecar with NaN/inf and an out-of-range opacity loads
        // to finite, clamped values rather than carrying the bad data through.
        let text = "(version: 1, variants: [(\
            global: (exposure: Some(NaN)), \
            locals: [(mask: (), adjustments: (), opacity: 5.0)]\
        )])";
        let doc = Document::from_ron(text).expect("malformed-but-parseable sidecar should load");
        let s = &doc.variants[0];
        assert_eq!(s.global.exposure, Some(0.0)); // NaN scrubbed to neutral
        assert!(s.global.exposure.unwrap().is_finite());
        assert_eq!(s.locals[0].opacity, 1.0); // 5.0 clamped to 1.0
    }

    #[test]
    fn garbage_ron_is_a_clean_error_not_a_panic() {
        let err = Document::from_ron("this is not ron at all {{{")
            .expect_err("garbage input should error");
        assert!(!err.is_empty());
    }

    #[test]
    fn mask_shapes_load_with_missing_fields() {
        // Each mask-shape leaf carries `#[serde(default)]`, so a sidecar that
        // omits a field on any shape fills it with the neutral default instead of
        // erroring — the forward-compat guarantee that lets a shape gain fields
        // without breaking previously-saved sidecars.

        // Gradient with `x1` omitted → default 0.0.
        let text = "(version: 1, variants: [(locals: [(\
            mask: (shapes: [Gradient((x0: 0.1, y0: 0.2, y1: 0.4))]), opacity: 1.0)\
        ])])";
        let doc = Document::from_ron(text).expect("gradient with a missing field should load");
        let MaskShape::Gradient(g) = &doc.variants[0].locals[0].mask.shapes[0] else {
            panic!("expected a gradient");
        };
        assert_eq!(g.x1, 0.0);

        // Radial with `feather` omitted → default 0.0.
        let text = "(version: 1, variants: [(locals: [(\
            mask: (shapes: [Radial((cx: 0.5, cy: 0.5, radius: 0.2))]), opacity: 1.0)\
        ])])";
        let doc = Document::from_ron(text).expect("radial with a missing field should load");
        let MaskShape::Radial(r) = &doc.variants[0].locals[0].mask.shapes[0] else {
            panic!("expected a radial");
        };
        assert_eq!(r.feather, 0.0);

        // Dab with `erase` omitted → default false.
        let text = "(version: 1, variants: [(locals: [(\
            mask: (shapes: [Brush((dabs: [(x: 0.5, y: 0.5, radius: 0.2, feather: 0.0)]))]), \
            opacity: 1.0)\
        ])])";
        let doc = Document::from_ron(text).expect("dab with a missing field should load");
        let MaskShape::Brush(b) = &doc.variants[0].locals[0].mask.shapes[0] else {
            panic!("expected a brush");
        };
        assert!(!b.dabs[0].erase);

        // Luminosity range with `hi` omitted → default 0.0.
        let text = "(version: 1, variants: [(locals: [(\
            mask: (shapes: [Luminosity((lo: 0.0, feather: 0.05))]), opacity: 1.0)\
        ])])";
        let doc =
            Document::from_ron(text).expect("luminance range with a missing field should load");
        let MaskShape::Luminosity(l) = &doc.variants[0].locals[0].mask.shapes[0] else {
            panic!("expected a luminosity range");
        };
        assert_eq!(l.hi, 0.0);

        // Color range with `sat_min` omitted → default 0.0.
        let text = "(version: 1, variants: [(locals: [(\
            mask: (shapes: [ColorRange((hue: 0.0, hue_width: 0.05, feather: 0.05))]), \
            opacity: 1.0)\
        ])])";
        let doc = Document::from_ron(text).expect("color range with a missing field should load");
        let MaskShape::ColorRange(c) = &doc.variants[0].locals[0].mask.shapes[0] else {
            panic!("expected a color range");
        };
        assert_eq!(c.sat_min, 0.0);
    }

    #[test]
    fn orientation_default_is_identity() {
        // The default re-framing is a no-op, and a default `Geometry` carrying it
        // still reports identity (so the default render is byte-identical).
        let o = Orientation::default();
        assert!(o.is_identity());
        assert_eq!(o, Orientation::IDENTITY);
        assert!(!o.swaps_axes());
        assert!(Geometry::default().is_identity());
    }

    #[test]
    fn orientation_group_composes() {
        // Four clockwise turns return to identity; two equal a 180°.
        let id = Orientation::IDENTITY;
        assert_eq!(id.rotate_cw().rotate_cw().rotate_cw().rotate_cw(), id);
        let half = id.rotate_cw().rotate_cw();
        assert_eq!(half.quarter_turns, 2);
        assert!(!half.flip);

        // CW then CCW cancels; CCW is three CW turns.
        assert_eq!(id.rotate_cw().rotate_ccw(), id);
        assert_eq!(id.rotate_ccw(), id.rotate_cw().rotate_cw().rotate_cw());

        // A flip is its own inverse, both before and after a rotation.
        assert_eq!(id.flip_h().flip_h(), id);
        let r = id.rotate_cw();
        assert_eq!(r.flip_h().flip_h(), r);

        // Vertical flip equals horizontal flip plus a 180° turn.
        assert_eq!(id.flip_v(), id.flip_h().rotate_cw().rotate_cw());
        // Two vertical flips cancel.
        assert_eq!(id.flip_v().flip_v(), id);

        // Odd turns swap the axes; even turns (and a pure flip) do not.
        assert!(id.rotate_cw().swaps_axes());
        assert!(!id.rotate_cw().rotate_cw().swaps_axes());
        assert!(!id.flip_h().swaps_axes());
    }

    #[test]
    fn orientation_round_trips_and_is_forward_compatible() {
        // A populated orientation survives a RON round-trip.
        let s = Settings {
            geometry: Geometry {
                orientation: Orientation {
                    quarter_turns: 3,
                    flip: true,
                },
                ..Geometry::default()
            },
            ..Settings::default()
        };
        let doc = Document {
            version: Document::VERSION,
            variants: vec![s.clone()],
            names: Vec::new(),
        };
        let text = doc.to_ron().expect("serialize");
        let back = Document::from_ron(&text).expect("round-trip");
        assert_eq!(back.variants[0], s);

        // An older sidecar without the `orientation` field still loads, filling
        // the identity (forward compatibility — a defaulted field is the no-op).
        let text = "(version: 1, variants: [(geometry: (straighten_degrees: 5.0))])";
        let doc = Document::from_ron(text).expect("sidecar without orientation should load");
        assert_eq!(doc.variants[0].geometry.orientation, Orientation::IDENTITY);
        assert_eq!(doc.variants[0].geometry.straighten_degrees, 5.0);

        // A hand-edited out-of-range turn count wraps into 0..4 on load.
        let text = "(version: 1, variants: [(geometry: (orientation: (quarter_turns: 6)))])";
        let doc = Document::from_ron(text).expect("out-of-range turns should sanitize");
        assert_eq!(doc.variants[0].geometry.orientation.quarter_turns, 2);
    }
}
