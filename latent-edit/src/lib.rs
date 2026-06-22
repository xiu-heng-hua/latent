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
    /// The adjustments to apply where this one acts.
    pub adjustments: Adjustments,
    /// Blend strength in `[0, 1]`; `1.0` applies the adjustments fully.
    pub opacity: f32,
}

impl Default for LocalAdjustment {
    fn default() -> Self {
        Self {
            adjustments: Adjustments::default(),
            opacity: 1.0,
        }
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
