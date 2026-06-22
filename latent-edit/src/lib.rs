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
pub struct Mask {
    /// Shapes combined into the mask; empty means "selects nothing".
    pub shapes: Vec<MaskShape>,
    /// Apply to the complement of the shapes' region instead.
    pub invert: bool,
}

impl Mask {
    /// The mask weight in `[0, 1]` at a normalized point `(px, py)`.
    pub fn weight_at(&self, px: f32, py: f32) -> f32 {
        let mut w = 0.0;
        for shape in &self.shapes {
            w = f32::max(w, shape.weight_at(px, py));
        }
        if self.invert { 1.0 - w } else { w }
    }
}

/// One masking primitive. More shapes (radial, brush, …) are added over time.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MaskShape {
    /// A linear gradient.
    Gradient(Gradient),
}

impl MaskShape {
    /// The shape's weight in `[0, 1]` at a normalized point.
    pub fn weight_at(&self, px: f32, py: f32) -> f32 {
        match self {
            MaskShape::Gradient(g) => g.weight_at(px, py),
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

/// The catalog of adjustments.
///
/// Each field is optional: `Some` means the adjustment is active with the given
/// parameters, `None` means it is off. The empty (default) value is neutral and
/// changes nothing. There is deliberately no ordering field, because the engine
/// applies adjustments in a fixed order rather than the order they appear in.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Adjustments {
    /// Editable white balance, on top of the as-shot balance applied at decode.
    pub white_balance: Option<WhiteBalance>,
    /// Exposure in stops (EV): a linear multiply by `2^stops`.
    pub exposure: Option<f32>,
    /// Tonal shaping across the contrast/highlights/shadows/blacks ranges.
    pub tone: Option<SelectiveTone>,
    /// Saturation factor: `0` is grayscale, `1` is unchanged, `> 1` is more.
    pub saturation: Option<f32>,
    /// Unsharp-mask sharpening.
    pub sharpen: Option<Sharpen>,
}

/// Editable white balance as a temp/tint pair; both `0` is neutral. Positive
/// `temp` warms (more red, less blue); positive `tint` shifts toward magenta.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct WhiteBalance {
    pub temp: f32,
    pub tint: f32,
}

/// Tonal shaping split across four ranges; all `0` is neutral. Each value is
/// roughly `[-1, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct SelectiveTone {
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub blacks: f32,
}

/// Unsharp-mask sharpening: `amount` is the strength (`0` = off), `radius` the
/// blur radius (in pixels) of the unsharp base.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

/// Framing and orientation of the rendered image: an optional crop and a
/// straighten angle. The default is the identity — no crop, no rotation.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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

    /// Parse from a RON string.
    pub fn from_ron(text: &str) -> Result<Self, String> {
        ron::from_str(text).map_err(|e| e.to_string())
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
        assert_eq!(Mask::default().weight_at(0.5, 0.5), 0.0);
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
            invert: false,
        };
        assert_eq!(mask.weight_at(0.0, 0.5), 0.0);
        assert!((mask.weight_at(0.5, 0.5) - 0.5).abs() < 1e-6);
        assert_eq!(mask.weight_at(1.0, 0.5), 1.0);
        assert_eq!(mask.weight_at(-0.3, 0.5), 0.0); // clamped before the band
        assert_eq!(mask.weight_at(1.3, 0.5), 1.0); // clamped after the band
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
            shapes: vec![g],
            invert: false,
        };
        let inverted = Mask {
            shapes: vec![g],
            invert: true,
        };
        assert!((normal.weight_at(0.25, 0.5) + inverted.weight_at(0.25, 0.5) - 1.0).abs() < 1e-6);
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
                saturation: Some(1.2),
                sharpen: Some(Sharpen {
                    amount: 0.3,
                    radius: 1.5,
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
}
