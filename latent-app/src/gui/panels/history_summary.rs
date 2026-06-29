//! One-line summaries of what changed between two [`Settings`] snapshots, used to
//! label undo-history steps.
//!
//! The undo history stores full snapshots with no recorded action name, so a
//! step's label is derived after the fact by diffing it against the step before
//! it. Each summary splits into a `title` (the tool/category that changed) and a
//! `detail` (the variable(s) that moved and their new values, in the same units
//! the controls show — EV, Kelvin, degrees, percent). This lives in the UI layer
//! rather than `latent-edit` because it speaks in those presentation units.

use latent_edit::{Adjustments, Geometry, LocalAdjustment, Orientation, Settings, WhiteBalance};

use crate::gui::widgets::wb;

/// A history step's label: a `title` naming the tool/category and a `detail`
/// carrying the variable(s) and their new values (possibly empty).
pub(crate) struct ChangeSummary {
    pub(crate) title: String,
    pub(crate) detail: String,
}

impl ChangeSummary {
    fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
        }
    }
}

/// Summarize the edit that turns `prev` into `next`. When one gesture changed
/// several independent things at once — a pasted look or a preset — the summary
/// counts them rather than listing each; a fully-neutral result reads as "Reset".
pub(crate) fn summarize_change(prev: &Settings, next: &Settings) -> ChangeSummary {
    let mut changes = Vec::new();
    describe_adjustments(&prev.global, &next.global, &mut changes);
    describe_geometry(&prev.geometry, &next.geometry, &mut changes);
    describe_locals(&prev.locals, &next.locals, &mut changes);

    match changes.len() {
        0 => ChangeSummary::new("Edit", ""),
        1 => changes.pop().unwrap(),
        n if *next == Settings::default() => ChangeSummary::new("Reset", format!("{n} cleared")),
        n => ChangeSummary::new("Multiple", format!("{n} changes")),
    }
}

/// "on" / "off" / "edited" — whether an optional adjustment was switched on, off,
/// or had its parameters changed while staying on.
fn toggled(was: bool, now: bool) -> &'static str {
    match (was, now) {
        (false, true) => "on",
        (true, false) => "off",
        _ => "edited",
    }
}

/// The detail for an optional scalar: the formatted value when on, or "off".
fn scalar_detail(next: Option<f32>, fmt: impl Fn(f32) -> String) -> String {
    next.map(fmt).unwrap_or_else(|| "off".to_owned())
}

fn describe_adjustments(prev: &Adjustments, next: &Adjustments, out: &mut Vec<ChangeSummary>) {
    if prev.white_balance != next.white_balance {
        out.push(ChangeSummary::new(
            "White balance",
            white_balance_detail(prev.white_balance, next.white_balance),
        ));
    }
    if prev.exposure != next.exposure {
        out.push(ChangeSummary::new(
            "Exposure",
            scalar_detail(next.exposure, |v| format!("{v:+.2} EV")),
        ));
    }
    if prev.tone != next.tone {
        out.push(ChangeSummary::new("Tone", tone_detail(prev, next)));
    }
    if prev.curves != next.curves {
        out.push(ChangeSummary::new(
            "Curves",
            toggled(prev.curves.is_some(), next.curves.is_some()),
        ));
    }
    if prev.saturation != next.saturation {
        out.push(ChangeSummary::new(
            "Saturation",
            scalar_detail(next.saturation, |v| format!("{v:.2}")),
        ));
    }
    if prev.hsl != next.hsl {
        out.push(ChangeSummary::new(
            "HSL mixer",
            toggled(prev.hsl.is_some(), next.hsl.is_some()),
        ));
    }
    if prev.channel_mixer != next.channel_mixer {
        out.push(ChangeSummary::new(
            "Channel mixer",
            toggled(prev.channel_mixer.is_some(), next.channel_mixer.is_some()),
        ));
    }
    if prev.sharpen != next.sharpen {
        out.push(ChangeSummary::new(
            "Sharpen",
            toggled(prev.sharpen.is_some(), next.sharpen.is_some()),
        ));
    }
    if prev.clarity != next.clarity {
        out.push(ChangeSummary::new(
            "Clarity",
            toggled(prev.clarity.is_some(), next.clarity.is_some()),
        ));
    }
    if prev.dehaze != next.dehaze {
        out.push(ChangeSummary::new(
            "Dehaze",
            scalar_detail(next.dehaze, |v| format!("{:.0}%", v * 100.0)),
        ));
    }
    if prev.noise_reduction != next.noise_reduction {
        out.push(ChangeSummary::new(
            "Noise reduction",
            toggled(
                prev.noise_reduction.is_some(),
                next.noise_reduction.is_some(),
            ),
        ));
    }
}

/// White balance reads as the temperature (in Kelvin) and/or tint that moved,
/// matching the two sliders the user sees.
fn white_balance_detail(prev: Option<WhiteBalance>, next: Option<WhiteBalance>) -> String {
    let Some(n) = next else {
        return "off".to_owned();
    };
    let p = prev.unwrap_or_default();
    let mut parts = Vec::new();
    if p.temp != n.temp {
        parts.push(format!("{:.0} K", wb::temp_to_kelvin(n.temp)));
    }
    if p.tint != n.tint {
        parts.push(format!("Tint {:+.0}", wb::tint_to_slider(n.tint)));
    }
    if parts.is_empty() {
        "on".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Selective tone is presented as four independent sliders (Contrast, Highlights,
/// Shadows, Blacks); a change reads as the single slider that moved, with its
/// value, and falls back to "adjusted" when several moved at once.
fn tone_detail(prev: &Adjustments, next: &Adjustments) -> String {
    let p = prev.tone.unwrap_or_default();
    let Some(n) = next.tone else {
        return "off".to_owned();
    };
    let fields = [
        ("Contrast", p.contrast, n.contrast),
        ("Highlights", p.highlights, n.highlights),
        ("Shadows", p.shadows, n.shadows),
        ("Blacks", p.blacks, n.blacks),
    ];
    let mut changed = fields.iter().filter(|(_, a, b)| a != b);
    match (changed.next(), changed.next()) {
        (Some((name, _, b)), None) => format!("{name} {b:+.2}"),
        _ => "adjusted".to_owned(),
    }
}

fn describe_geometry(prev: &Geometry, next: &Geometry, out: &mut Vec<ChangeSummary>) {
    if prev.crop != next.crop {
        let detail = match next.crop {
            None => "removed".to_owned(),
            Some(c) => format!("{:.0}% × {:.0}%", c.width * 100.0, c.height * 100.0),
        };
        out.push(ChangeSummary::new("Crop", detail));
    }
    if prev.orientation != next.orientation {
        out.push(ChangeSummary::new(
            orientation_title(prev.orientation, next.orientation),
            "",
        ));
    }
    if prev.straighten_degrees != next.straighten_degrees {
        let d = next.straighten_degrees;
        let detail = if d.abs() < 0.05 {
            "0°".to_owned()
        } else {
            format!("{d:+.1}°")
        };
        out.push(ChangeSummary::new("Straighten", detail));
    }
    if prev.perspective != next.perspective {
        out.push(ChangeSummary::new("Keystone", keystone_detail(prev, next)));
    }
    if prev.lens != next.lens {
        out.push(ChangeSummary::new(
            "Lens correction",
            toggled(prev.lens.is_some(), next.lens.is_some()),
        ));
    }
    if prev.vignette != next.vignette {
        out.push(ChangeSummary::new(
            "Vignette",
            scalar_detail(next.vignette, |v| format!("{v:+.2}")),
        ));
    }
    if prev.output_sharpen != next.output_sharpen {
        out.push(ChangeSummary::new(
            "Output sharpening",
            toggled(prev.output_sharpen.is_some(), next.output_sharpen.is_some()),
        ));
    }
    if prev.auto_scale != next.auto_scale {
        out.push(ChangeSummary::new(
            "Auto-scale",
            if next.auto_scale { "on" } else { "off" },
        ));
    }
    if prev.auto_constrain != next.auto_constrain {
        out.push(ChangeSummary::new(
            "Auto-constrain",
            if next.auto_constrain { "on" } else { "off" },
        ));
    }
}

/// Keystone reads as the converging-line correction(s) that moved, with values.
fn keystone_detail(prev: &Geometry, next: &Geometry) -> String {
    let Some(n) = next.perspective else {
        return "off".to_owned();
    };
    // Treat an absent previous keystone as a zeroed one, so enabling it lists only
    // the component the user actually moved, not the untouched zero axis.
    let (pv, ph) = prev
        .perspective
        .map_or((0.0, 0.0), |p| (p.vertical, p.horizontal));
    let mut parts = Vec::new();
    if pv != n.vertical {
        parts.push(format!("V {:+.2}", n.vertical));
    }
    if ph != n.horizontal {
        parts.push(format!("H {:+.2}", n.horizontal));
    }
    if parts.is_empty() {
        "on".to_owned()
    } else {
        parts.join(", ")
    }
}

/// A right-angle re-framing is either a rotate or a flip: only a flip toggles the
/// mirror, while a rotate changes the quarter-turn count with the mirror fixed.
fn orientation_title(prev: Orientation, next: Orientation) -> &'static str {
    if prev.flip != next.flip {
        "Flip"
    } else if prev.quarter_turns != next.quarter_turns {
        "Rotate"
    } else {
        "Orientation"
    }
}

fn describe_locals(
    prev: &[LocalAdjustment],
    next: &[LocalAdjustment],
    out: &mut Vec<ChangeSummary>,
) {
    use std::cmp::Ordering;
    let detail = match next.len().cmp(&prev.len()) {
        Ordering::Greater => "added",
        Ordering::Less => "removed",
        Ordering::Equal if prev != next => "adjusted",
        Ordering::Equal => return,
    };
    out.push(ChangeSummary::new("Mask", detail));
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{Crop, Perspective, SelectiveTone, Sharpen};

    fn with_exposure(ev: f32) -> Settings {
        Settings {
            global: Adjustments {
                exposure: Some(ev),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn summarize(prev: &Settings, next: &Settings) -> (String, String) {
        let s = summarize_change(prev, next);
        (s.title, s.detail)
    }

    #[test]
    fn a_scalar_change_titles_the_tool_and_details_the_value() {
        assert_eq!(
            summarize(&Settings::default(), &with_exposure(0.5)),
            ("Exposure".to_owned(), "+0.50 EV".to_owned())
        );
        assert_eq!(
            summarize(&Settings::default(), &with_exposure(-1.25)),
            ("Exposure".to_owned(), "-1.25 EV".to_owned())
        );
        assert_eq!(
            summarize(&with_exposure(0.5), &Settings::default()),
            ("Exposure".to_owned(), "off".to_owned())
        );
    }

    #[test]
    fn tone_details_the_individual_slider_that_moved() {
        let mut next = Settings::default();
        next.global.tone = Some(SelectiveTone {
            contrast: 0.3,
            ..Default::default()
        });
        assert_eq!(
            summarize(&Settings::default(), &next),
            ("Tone".to_owned(), "Contrast +0.30".to_owned())
        );

        let mut two = Settings::default();
        two.global.tone = Some(SelectiveTone {
            contrast: 0.3,
            blacks: -0.2,
            ..Default::default()
        });
        assert_eq!(
            summarize(&Settings::default(), &two),
            ("Tone".to_owned(), "adjusted".to_owned())
        );
    }

    #[test]
    fn white_balance_details_kelvin_and_tint() {
        // A pure-warm change shows the Kelvin it landed on; the neutral T0 reads as
        // its reference Kelvin.
        let mut warm = Settings::default();
        warm.global.white_balance = Some(WhiteBalance {
            temp: wb::kelvin_to_temp(4000.0),
            tint: 0.0,
        });
        let (title, detail) = summarize(&Settings::default(), &warm);
        assert_eq!(title, "White balance");
        assert_eq!(detail, "4000 K");

        // A pure tint change shows only the tint.
        let mut tinted = Settings::default();
        tinted.global.white_balance = Some(WhiteBalance {
            temp: 0.0,
            tint: wb::slider_to_tint(20.0),
        });
        assert_eq!(summarize(&Settings::default(), &tinted).1, "Tint +20");
    }

    #[test]
    fn toggling_a_tool_details_on_or_off() {
        let mut on = Settings::default();
        on.global.sharpen = Some(Sharpen::default());
        assert_eq!(
            summarize(&Settings::default(), &on),
            ("Sharpen".to_owned(), "on".to_owned())
        );
        assert_eq!(summarize(&on, &Settings::default()).1, "off");
    }

    #[test]
    fn geometry_changes_title_the_tool_with_values() {
        let mut cropped = Settings::default();
        cropped.geometry.crop = Some(Crop {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.6,
        });
        assert_eq!(
            summarize(&Settings::default(), &cropped),
            ("Crop".to_owned(), "80% × 60%".to_owned())
        );
        assert_eq!(summarize(&cropped, &Settings::default()).1, "removed");

        let mut straight = Settings::default();
        straight.geometry.straighten_degrees = 2.5;
        assert_eq!(
            summarize(&Settings::default(), &straight),
            ("Straighten".to_owned(), "+2.5°".to_owned())
        );

        let mut keystone = Settings::default();
        keystone.geometry.perspective = Some(Perspective {
            vertical: 0.3,
            horizontal: 0.0,
        });
        assert_eq!(
            summarize(&Settings::default(), &keystone),
            ("Keystone".to_owned(), "V +0.30".to_owned())
        );

        let mut rotated = Settings::default();
        rotated.geometry.orientation = Orientation::IDENTITY.rotate_cw();
        assert_eq!(summarize(&Settings::default(), &rotated).0, "Rotate");

        let mut flipped = Settings::default();
        flipped.geometry.orientation = Orientation::IDENTITY.flip_h();
        assert_eq!(summarize(&Settings::default(), &flipped).0, "Flip");
    }

    #[test]
    fn multiple_changes_summarize_and_a_neutral_result_resets() {
        let mut multi = with_exposure(0.5);
        multi.global.saturation = Some(1.4);
        assert_eq!(
            summarize(&Settings::default(), &multi),
            ("Multiple".to_owned(), "2 changes".to_owned())
        );
        assert_eq!(summarize(&multi, &Settings::default()).0, "Reset");
    }

    #[test]
    fn local_adjustments_read_as_mask_changes() {
        let one = Settings {
            locals: vec![LocalAdjustment::default()],
            ..Default::default()
        };
        assert_eq!(
            summarize(&Settings::default(), &one),
            ("Mask".to_owned(), "added".to_owned())
        );
        assert_eq!(summarize(&one, &Settings::default()).1, "removed");

        let mut edited = one.clone();
        edited.locals[0].opacity = 0.5;
        assert_eq!(summarize(&one, &edited).1, "adjusted");
    }
}
