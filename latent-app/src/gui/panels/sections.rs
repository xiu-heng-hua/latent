//! The controls-panel section model: a stable id per collapsible group, its
//! display label and default-open state, the set of `Settings` fields it owns
//! (so a per-section reset clears exactly those, as one undo step), and the
//! "is any field in this section non-default?" predicate behind the modified-dot.
//!
//! Everything here is pure data and pure functions over [`Settings`], so the
//! reset actions and the modified predicates are unit-testable without a window.
//! The display-driven painting (the collapsing header, the dot) lives in the
//! panel; this module is the truth it renders from.

use latent_edit::Settings;

/// A controls-panel section. The variant — **not** its display string — is the
/// stable key persisted in the app config, so renaming a header's label never
/// orphans its saved open/closed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SectionId {
    Basic,
    Tone,
    Color,
    Curves,
    Detail,
    Effects,
    Geometry,
    Masks,
}

impl SectionId {
    /// Every section, in panel order. Used by the consistency tests; the panel
    /// renders each section explicitly (so each owns its own `Settings` borrow).
    #[cfg(test)]
    pub(crate) const ALL: [SectionId; 8] = [
        SectionId::Basic,
        SectionId::Tone,
        SectionId::Color,
        SectionId::Curves,
        SectionId::Detail,
        SectionId::Effects,
        SectionId::Geometry,
        SectionId::Masks,
    ];

    /// The stable persistence key — a short constant string that never changes
    /// even if the display label does. This is what round-trips in the config.
    pub(crate) fn key(self) -> &'static str {
        match self {
            SectionId::Basic => "basic",
            SectionId::Tone => "tone",
            SectionId::Color => "color",
            SectionId::Curves => "curves",
            SectionId::Detail => "detail",
            SectionId::Effects => "effects",
            SectionId::Geometry => "geometry",
            SectionId::Masks => "masks",
        }
    }

    /// The human label shown on the section header. Safe to change without
    /// affecting saved state (which is keyed by [`SectionId::key`]).
    pub(crate) fn label(self) -> &'static str {
        match self {
            SectionId::Basic => "Basic",
            SectionId::Tone => "Tone",
            SectionId::Color => "Color",
            SectionId::Curves => "Curves",
            SectionId::Detail => "Detail",
            SectionId::Effects => "Effects",
            SectionId::Geometry => "Geometry",
            SectionId::Masks => "Masks",
        }
    }

    /// A one-line "what this group does" tooltip for the section header.
    pub(crate) fn help(self) -> &'static str {
        match self {
            SectionId::Basic => "Exposure and white balance",
            SectionId::Tone => "Contrast, highlights, shadows, and blacks",
            SectionId::Color => "Saturation and color grading",
            SectionId::Curves => "Master and per-channel tone curves",
            SectionId::Detail => "Sharpening, clarity, dehaze, and noise reduction",
            SectionId::Effects => "Vignette and creative effects",
            SectionId::Geometry => "Crop, straighten, and perspective",
            SectionId::Masks => "Local, masked adjustments",
        }
    }

    /// Whether the section starts open the first time, before any saved state.
    pub(crate) fn default_open(self) -> bool {
        matches!(self, SectionId::Basic | SectionId::Tone)
    }

    /// Reset exactly this section's `Settings` fields to their default (neutral)
    /// values, leaving every other section untouched. Pure mutation, so the
    /// caller wraps a single `begin`/`commit` around it for one undo step; a
    /// section already at default leaves the value unchanged, so that one commit
    /// records nothing.
    pub(crate) fn reset(self, s: &mut Settings) {
        match self {
            SectionId::Basic => {
                s.global.exposure = None;
                s.global.white_balance = None;
            }
            SectionId::Tone => {
                s.global.tone = None;
            }
            SectionId::Color => {
                s.global.saturation = None;
            }
            SectionId::Curves => {
                s.global.curves = None;
            }
            SectionId::Detail => {
                s.global.sharpen = None;
                s.global.clarity = None;
                s.global.dehaze = None;
                s.global.noise_reduction = None;
            }
            SectionId::Effects => {
                s.geometry.vignette = None;
            }
            SectionId::Geometry => {
                s.geometry.straighten_degrees = 0.0;
                s.geometry.perspective = None;
                s.geometry.crop = None;
            }
            SectionId::Masks => {
                s.locals.clear();
            }
        }
    }

    /// Whether any field this section owns differs from its default — the
    /// section-level modified predicate. Derived purely from the current
    /// `Settings` (no stored dirty flag), so undoing back to default clears it
    /// for free. Equivalent to the OR of the section's control predicates.
    pub(crate) fn is_modified(self, s: &Settings) -> bool {
        let g = &s.global;
        match self {
            SectionId::Basic => g.exposure.is_some() || g.white_balance.is_some(),
            SectionId::Tone => g.tone.is_some(),
            SectionId::Color => g.saturation.is_some(),
            SectionId::Curves => g.curves.is_some(),
            SectionId::Detail => {
                g.sharpen.is_some()
                    || g.clarity.is_some()
                    || g.dehaze.is_some()
                    || g.noise_reduction.is_some()
            }
            SectionId::Effects => s.geometry.vignette.is_some(),
            SectionId::Geometry => {
                s.geometry.straighten_degrees != 0.0
                    || s.geometry.perspective.is_some()
                    || s.geometry.crop.is_some()
            }
            SectionId::Masks => !s.locals.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{Crop, SelectiveTone};

    /// A settings value with one field set in the Basic section and one in the
    /// Geometry section, for the cross-section isolation checks.
    fn populated() -> Settings {
        Settings {
            global: latent_edit::Adjustments {
                exposure: Some(1.5),
                tone: Some(SelectiveTone {
                    contrast: 0.5,
                    ..SelectiveTone::default()
                }),
                ..latent_edit::Adjustments::default()
            },
            geometry: latent_edit::Geometry {
                crop: Some(Crop {
                    x: 0.1,
                    y: 0.1,
                    width: 0.8,
                    height: 0.8,
                }),
                ..latent_edit::Geometry::default()
            },
            ..Settings::default()
        }
    }

    #[test]
    fn section_reset_is_one_undo_step() {
        // A section with several of its fields set, plus a field in another
        // section, resets under one begin/commit. The result: (a) the section's
        // fields are default, (b) the other section is untouched, (c) one undo
        // restores all of them at once — proving the whole reset was a single step,
        // not N.
        use latent_edit::{History, WhiteBalance};
        let mut s = populated();
        // Set a second Basic field so the reset clears more than one.
        s.global.white_balance = Some(WhiteBalance {
            temp: 0.3,
            tint: -0.2,
        });
        let mut h = History::new(s);

        h.begin();
        SectionId::Basic.reset(h.current_mut());
        h.commit();

        // (a) the Basic fields are now default.
        assert_eq!(h.current().global.exposure, None);
        assert_eq!(h.current().global.white_balance, None);
        // (b) other sections are untouched.
        assert!(h.current().global.tone.is_some(), "Tone untouched");
        assert!(h.current().geometry.crop.is_some(), "Geometry untouched");

        // (c) a single undo restores every reset field at once.
        assert!(h.can_undo());
        assert!(h.undo());
        assert_eq!(h.current().global.exposure, Some(1.5));
        assert!(h.current().global.white_balance.is_some());
        // And there is nothing further to undo — it was one step.
        assert!(!h.can_undo());
    }

    #[test]
    fn reset_of_neutral_section_records_nothing() {
        // Resetting an already-default section under one begin/commit records no
        // undo step (the History `prev != current` guard), so the button is a
        // no-op when there's nothing to reset.
        use latent_edit::History;
        let mut h = History::new(Settings::default());
        h.begin();
        SectionId::Detail.reset(h.current_mut());
        h.commit();
        assert!(!h.can_undo(), "no step recorded for a no-op reset");
    }

    #[test]
    fn section_keys_are_stable_and_unique() {
        // No two sections share a persistence key (a collision would conflate
        // their saved state), and every key is a short stable string.
        let mut seen = std::collections::HashSet::new();
        for id in SectionId::ALL {
            assert!(seen.insert(id.key()), "duplicate key {}", id.key());
            assert!(!id.key().is_empty());
        }
        assert_eq!(seen.len(), SectionId::ALL.len());
    }

    #[test]
    fn section_reset_clears_only_its_fields() {
        // Resetting Basic clears its fields but leaves the Geometry crop and the
        // Tone block (other sections) untouched.
        let mut s = populated();
        SectionId::Basic.reset(&mut s);
        assert_eq!(s.global.exposure, None, "Basic field cleared");
        assert!(s.global.tone.is_some(), "Tone untouched");
        assert!(s.geometry.crop.is_some(), "Geometry untouched");

        // Resetting Geometry clears its crop/straighten/perspective but keeps the
        // auto-constrain preference on — it is a setting, not a per-image edit.
        s.geometry.straighten_degrees = 5.0;
        SectionId::Geometry.reset(&mut s);
        assert_eq!(s.geometry.crop, None, "crop cleared");
        assert_eq!(s.geometry.straighten_degrees, 0.0, "straighten cleared");
        assert!(s.geometry.auto_constrain, "auto-constrain preference kept");
    }

    #[test]
    fn reset_of_neutral_section_changes_nothing() {
        // Resetting a section that is already at default leaves the value bit-for-
        // bit identical — so a single begin/commit around it records nothing (the
        // History `prev != current` guard).
        let mut s = Settings::default();
        let before = s.clone();
        SectionId::Detail.reset(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn modified_dot_only_when_non_default() {
        // The section predicate is exactly "any owned field non-default".
        let default = Settings::default();
        for id in SectionId::ALL {
            assert!(!id.is_modified(&default), "{:?} clean on default", id);
        }
        let s = populated();
        assert!(SectionId::Basic.is_modified(&s), "Basic has exposure");
        assert!(SectionId::Tone.is_modified(&s), "Tone has contrast");
        assert!(SectionId::Geometry.is_modified(&s), "Geometry has crop");
        assert!(!SectionId::Detail.is_modified(&s), "Detail untouched");
        assert!(!SectionId::Masks.is_modified(&s), "Masks untouched");
    }

    #[test]
    fn section_dot_is_or_of_controls() {
        // The Detail dot turns on as soon as any one of its several controls is
        // set, and off only when all are default — the OR of its controls.
        let mut s = Settings::default();
        assert!(!SectionId::Detail.is_modified(&s));
        s.global.dehaze = Some(0.4);
        assert!(SectionId::Detail.is_modified(&s));
        s.global.dehaze = None;
        assert!(!SectionId::Detail.is_modified(&s));
        s.global.clarity = Some(latent_edit::Clarity::default());
        assert!(SectionId::Detail.is_modified(&s));
    }
}
