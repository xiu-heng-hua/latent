//! The controls-panel section model: a stable id per collapsible group, its
//! display label and default-open state, the set of `Settings` fields it owns
//! (so a per-section reset clears exactly those, as one undo step), and the
//! "is any field in this section non-default?" predicate behind the modified-dot.
//!
//! Everything here is pure data and pure functions over [`Settings`], so the
//! reset actions and the modified predicates are unit-testable without a window.
//! The display-driven painting (the collapsing header, the dot) lives in the
//! panel; this module is the truth it renders from.
//!
//! There are five top-level sections, each owning the union of its members'
//! fields: Light (exposure, tone, curves), Color (white balance, saturation, HSL
//! and channel mixers), Detail (sharpen, clarity, dehaze, noise reduction),
//! Geometry (crop, straighten, keystone, lens, vignette), and Masks (the local
//! adjustments).

use latent_edit::Settings;

/// A controls-panel section. The variant — **not** its display string — is the
/// stable key persisted in the app config, so renaming a header's label never
/// orphans its saved open/closed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SectionId {
    Light,
    Color,
    Detail,
    Geometry,
    Masks,
}

impl SectionId {
    /// Every section, in panel order. Backs [`SectionId::key_from_str`] and the
    /// consistency tests; the panel still renders each section explicitly (so each
    /// owns its own `Settings` borrow).
    pub(crate) const ALL: [SectionId; 5] = [
        SectionId::Light,
        SectionId::Color,
        SectionId::Detail,
        SectionId::Geometry,
        SectionId::Masks,
    ];

    /// The stable persistence key — a short constant string that never changes
    /// even if the display label does. This is what round-trips in the config.
    pub(crate) fn key(self) -> &'static str {
        match self {
            SectionId::Light => "light",
            SectionId::Color => "color",
            SectionId::Detail => "detail",
            SectionId::Geometry => "geometry",
            SectionId::Masks => "masks",
        }
    }

    /// Resolve a persisted (opaque) section key string back to the matching
    /// section's `'static` key, or `None` if it names no current section (e.g. an
    /// old config that stored one of the pre-merge keys). Lets the panel drive the
    /// solo open-state from a `'static` key without borrowing the config string.
    pub(crate) fn key_from_str(s: &str) -> Option<&'static str> {
        SectionId::ALL
            .into_iter()
            .map(SectionId::key)
            .find(|&key| key == s)
    }

    /// The human label shown on the section header. Safe to change without
    /// affecting saved state (which is keyed by [`SectionId::key`]).
    pub(crate) fn label(self) -> &'static str {
        match self {
            SectionId::Light => "Light",
            SectionId::Color => "Color",
            SectionId::Detail => "Detail",
            SectionId::Geometry => "Geometry",
            SectionId::Masks => "Masks",
        }
    }

    /// A one-line "what this group does" tooltip for the section header.
    pub(crate) fn help(self) -> &'static str {
        match self {
            SectionId::Light => "Exposure, contrast, highlights, shadows, blacks, and tone curves",
            SectionId::Color => "White balance, saturation, and color grading",
            SectionId::Detail => "Sharpening, clarity, dehaze, and noise reduction",
            SectionId::Geometry => "Crop, straighten, perspective, lens correction, and vignette",
            SectionId::Masks => "Local, masked adjustments",
        }
    }

    /// Whether the section starts open the first time, before any saved state.
    /// Only Light opens by default — with solo (accordion) mode on by default,
    /// exactly one section is open at a time.
    pub(crate) fn default_open(self) -> bool {
        matches!(self, SectionId::Light)
    }

    /// Reset exactly this section's `Settings` fields to their default (neutral)
    /// values, leaving every other section untouched. Pure mutation, so the
    /// caller wraps a single `begin`/`commit` around it for one undo step; a
    /// section already at default leaves the value unchanged, so that one commit
    /// records nothing.
    pub(crate) fn reset(self, s: &mut Settings) {
        match self {
            SectionId::Light => {
                s.global.exposure = None;
                s.global.tone = None;
                s.global.curves = None;
            }
            SectionId::Color => {
                s.global.white_balance = None;
                s.global.saturation = None;
                s.global.hsl = None;
                s.global.channel_mixer = None;
            }
            SectionId::Detail => {
                s.global.sharpen = None;
                s.global.clarity = None;
                s.global.dehaze = None;
                s.global.noise_reduction = None;
            }
            SectionId::Geometry => {
                s.geometry.straighten_degrees = 0.0;
                s.geometry.perspective = None;
                s.geometry.crop = None;
                s.geometry.lens = None;
                s.geometry.vignette = None;
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
            SectionId::Light => g.exposure.is_some() || g.tone.is_some() || g.curves.is_some(),
            SectionId::Color => {
                g.white_balance.is_some()
                    || g.saturation.is_some()
                    || g.hsl.is_some()
                    || g.channel_mixer.is_some()
            }
            SectionId::Detail => {
                g.sharpen.is_some()
                    || g.clarity.is_some()
                    || g.dehaze.is_some()
                    || g.noise_reduction.is_some()
            }
            SectionId::Geometry => {
                s.geometry.straighten_degrees != 0.0
                    || s.geometry.perspective.is_some()
                    || s.geometry.crop.is_some()
                    || s.geometry.lens.is_some()
                    || s.geometry.vignette.is_some()
            }
            SectionId::Masks => !s.locals.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{Crop, SelectiveTone};

    /// A settings value with one field set in the Light section and one in the
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
        use latent_edit::History;
        let mut s = populated();
        // Set a second Light field so the reset clears more than one.
        s.global.curves = Some(latent_edit::Curves::default());
        let mut h = History::new(s);

        h.begin();
        SectionId::Light.reset(h.current_mut());
        h.commit();

        // (a) the Light fields are now default.
        assert_eq!(h.current().global.exposure, None);
        assert_eq!(h.current().global.tone, None);
        assert_eq!(h.current().global.curves, None);
        // (b) other sections are untouched.
        assert!(h.current().geometry.crop.is_some(), "Geometry untouched");

        // (c) a single undo restores every reset field at once.
        assert!(h.can_undo());
        assert!(h.undo());
        assert_eq!(h.current().global.exposure, Some(1.5));
        assert!(h.current().global.tone.is_some());
        assert!(h.current().global.curves.is_some());
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
    fn section_keys_are_the_new_five() {
        // The five stable keys are exactly the merged set, in panel order.
        let keys: Vec<&str> = SectionId::ALL.iter().map(|id| id.key()).collect();
        assert_eq!(keys, ["light", "color", "detail", "geometry", "masks"]);
    }

    #[test]
    fn key_from_str_resolves_current_keys_only() {
        // A current key resolves to the matching `'static` key; a pre-merge or
        // unknown key resolves to `None` (so the panel falls back to all-collapsed
        // rather than opening a section that no longer exists).
        for id in SectionId::ALL {
            assert_eq!(SectionId::key_from_str(id.key()), Some(id.key()));
        }
        // Pre-merge keys that no longer name a section.
        for old in ["basic", "tone", "curves", "effects", "nope"] {
            assert_eq!(SectionId::key_from_str(old), None, "{old} is not a section");
        }
    }

    #[test]
    fn section_reset_clears_only_its_fields() {
        // Resetting Light clears its fields but leaves the Geometry crop (another
        // section) untouched.
        let mut s = populated();
        SectionId::Light.reset(&mut s);
        assert_eq!(s.global.exposure, None, "Light exposure cleared");
        assert_eq!(s.global.tone, None, "Light tone cleared");
        assert!(s.geometry.crop.is_some(), "Geometry untouched");

        // Resetting Geometry clears its crop/straighten/perspective/lens/vignette
        // but keeps the auto-constrain preference on — it is a setting, not a
        // per-image edit.
        s.geometry.straighten_degrees = 5.0;
        SectionId::Geometry.reset(&mut s);
        assert_eq!(s.geometry.crop, None, "crop cleared");
        assert_eq!(s.geometry.straighten_degrees, 0.0, "straighten cleared");
        assert!(s.geometry.auto_constrain, "auto-constrain preference kept");
    }

    #[test]
    fn each_owned_field_drives_exactly_its_section() {
        // Field-ownership check: setting exactly one field flips exactly one
        // section's modified predicate and is cleared by exactly that section's
        // reset — proving each new section owns the union of its members' fields
        // with no overlap.
        use latent_edit::{
            ChannelMixer, Clarity, Curves, Hsl, NoiseReduction, Perspective, SelectiveTone,
            Sharpen, WhiteBalance,
        };

        // One case: a section paired with a closure that sets exactly one field
        // that section owns.
        type Case = (SectionId, fn(&mut Settings));
        // (section, mutate-one-field, the field is owned by `section` alone)
        let cases: Vec<Case> = vec![
            (SectionId::Light, |s| s.global.exposure = Some(1.0)),
            (SectionId::Light, |s| {
                s.global.tone = Some(SelectiveTone::default())
            }),
            (SectionId::Light, |s| {
                s.global.curves = Some(Curves::default())
            }),
            (SectionId::Color, |s| {
                s.global.white_balance = Some(WhiteBalance {
                    temp: 0.2,
                    tint: 0.0,
                })
            }),
            (SectionId::Color, |s| s.global.saturation = Some(1.2)),
            (SectionId::Color, |s| s.global.hsl = Some(Hsl::default())),
            (SectionId::Color, |s| {
                s.global.channel_mixer = Some(ChannelMixer::default())
            }),
            (SectionId::Detail, |s| {
                s.global.sharpen = Some(Sharpen::default())
            }),
            (SectionId::Detail, |s| {
                s.global.clarity = Some(Clarity::default())
            }),
            (SectionId::Detail, |s| s.global.dehaze = Some(0.3)),
            (SectionId::Detail, |s| {
                s.global.noise_reduction = Some(NoiseReduction::default())
            }),
            (SectionId::Geometry, |s| s.geometry.straighten_degrees = 3.0),
            (SectionId::Geometry, |s| {
                s.geometry.perspective = Some(Perspective {
                    vertical: 0.1,
                    horizontal: 0.0,
                })
            }),
            (SectionId::Geometry, |s| {
                s.geometry.crop = Some(Crop {
                    x: 0.0,
                    y: 0.0,
                    width: 0.5,
                    height: 1.0,
                })
            }),
            (SectionId::Geometry, |s| s.geometry.vignette = Some(-0.3)),
        ];

        for (owner, mutate) in cases {
            let mut s = Settings::default();
            mutate(&mut s);
            // Exactly the owner section reads modified; no other does.
            for id in SectionId::ALL {
                assert_eq!(
                    id.is_modified(&s),
                    id == owner,
                    "{:?} modified should be {} after mutating a {:?} field",
                    id,
                    id == owner,
                    owner
                );
            }
            // The owner's reset returns everything to default.
            owner.reset(&mut s);
            assert_eq!(s, Settings::default(), "{:?} reset restores default", owner);
        }
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
        assert!(SectionId::Light.is_modified(&s), "Light has exposure/tone");
        assert!(SectionId::Geometry.is_modified(&s), "Geometry has crop");
        assert!(!SectionId::Color.is_modified(&s), "Color untouched");
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
