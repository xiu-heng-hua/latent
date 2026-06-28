//! Human-readable one-line summaries of what changed between two [`Settings`]
//! snapshots.
//!
//! The undo history stores full snapshots with no recorded action name, so a
//! step's label is derived after the fact by diffing it against the step before
//! it. This keeps the editing code free of any labelling concern — every kind of
//! edit is described by the same comparison — and the label always reflects what
//! actually differs rather than what a call site claimed it would.

use crate::{Adjustments, Geometry, Orientation, Settings, WhiteBalance};

/// A short label for the edit that turns `prev` into `next` — naming the tool and,
/// for the single-value controls, the value it landed on (e.g. "Exposure +0.50
/// EV", "Contrast +0.30", "Crop", "Rotate"). When one gesture changed several
/// independent things at once — a pasted look or a preset — the label summarizes
/// rather than listing each. A fully-neutral result reads as "Reset".
pub fn describe_change(prev: &Settings, next: &Settings) -> String {
    let mut changes = Vec::new();
    describe_adjustments(&prev.global, &next.global, &mut changes);
    describe_geometry(&prev.geometry, &next.geometry, &mut changes);
    describe_locals(&prev.locals, &next.locals, &mut changes);

    match changes.len() {
        0 => "Edit".to_owned(),
        1 => changes.pop().unwrap(),
        _ if *next == Settings::default() => "Reset".to_owned(),
        _ => "Multiple adjustments".to_owned(),
    }
}

/// "<name> on" / "<name> off" when an optional adjustment is switched on or off,
/// or plain "<name>" when its parameters merely changed.
fn toggle(name: &str, was: bool, now: bool) -> String {
    match (was, now) {
        (false, true) => format!("{name} on"),
        (true, false) => format!("{name} off"),
        _ => name.to_owned(),
    }
}

/// The value-carrying label for an optional scalar: the formatted value when it is
/// on, or "<name> off" when it was switched off.
fn scalar(name: &str, next: Option<f32>, fmt: impl Fn(f32) -> String) -> String {
    match next {
        Some(v) => fmt(v),
        None => format!("{name} off"),
    }
}

fn describe_adjustments(prev: &Adjustments, next: &Adjustments, out: &mut Vec<String>) {
    if prev.white_balance != next.white_balance {
        out.push(describe_white_balance(
            prev.white_balance,
            next.white_balance,
        ));
    }
    if prev.exposure != next.exposure {
        out.push(scalar("Exposure", next.exposure, |v| {
            format!("Exposure {v:+.2} EV")
        }));
    }
    if prev.tone != next.tone {
        out.push(describe_tone(prev, next));
    }
    if prev.curves != next.curves {
        out.push(toggle(
            "Curves",
            prev.curves.is_some(),
            next.curves.is_some(),
        ));
    }
    if prev.saturation != next.saturation {
        out.push(scalar("Saturation", next.saturation, |v| {
            format!("Saturation {v:.2}")
        }));
    }
    if prev.hsl != next.hsl {
        out.push(toggle("HSL mixer", prev.hsl.is_some(), next.hsl.is_some()));
    }
    if prev.channel_mixer != next.channel_mixer {
        out.push(toggle(
            "Channel mixer",
            prev.channel_mixer.is_some(),
            next.channel_mixer.is_some(),
        ));
    }
    if prev.sharpen != next.sharpen {
        out.push(toggle(
            "Sharpen",
            prev.sharpen.is_some(),
            next.sharpen.is_some(),
        ));
    }
    if prev.clarity != next.clarity {
        out.push(toggle(
            "Clarity",
            prev.clarity.is_some(),
            next.clarity.is_some(),
        ));
    }
    if prev.dehaze != next.dehaze {
        out.push(scalar("Dehaze", next.dehaze, |v| {
            format!("Dehaze {:.0}%", v * 100.0)
        }));
    }
    if prev.noise_reduction != next.noise_reduction {
        out.push(toggle(
            "Noise reduction",
            prev.noise_reduction.is_some(),
            next.noise_reduction.is_some(),
        ));
    }
}

/// White balance reads as the individual slider when only one of temp/tint moved
/// (matching the two controls the user sees), and as the whole tool otherwise.
fn describe_white_balance(prev: Option<WhiteBalance>, next: Option<WhiteBalance>) -> String {
    match next {
        None => "White balance off".to_owned(),
        Some(n) => {
            let p = prev.unwrap_or_default();
            match (p.temp != n.temp, p.tint != n.tint) {
                (true, false) => "Temperature".to_owned(),
                (false, true) => "Tint".to_owned(),
                _ => "White balance".to_owned(),
            }
        }
    }
}

/// Selective tone is presented as four independent sliders (Contrast, Highlights,
/// Shadows, Blacks) with no wrapping toggle, so a change reads as the single slider
/// that moved — with its value — and falls back to "Tone" when several moved at
/// once.
fn describe_tone(prev: &Adjustments, next: &Adjustments) -> String {
    let p = prev.tone.unwrap_or_default();
    let Some(n) = next.tone else {
        return "Tone off".to_owned();
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
        _ => "Tone".to_owned(),
    }
}

fn describe_geometry(prev: &Geometry, next: &Geometry, out: &mut Vec<String>) {
    if prev.crop != next.crop {
        out.push(
            match (prev.crop.is_some(), next.crop.is_some()) {
                (true, false) => "Remove crop",
                _ => "Crop",
            }
            .to_owned(),
        );
    }
    if prev.orientation != next.orientation {
        out.push(describe_orientation(prev.orientation, next.orientation));
    }
    if prev.straighten_degrees != next.straighten_degrees {
        let d = next.straighten_degrees;
        out.push(if d.abs() < 0.05 {
            "Straighten 0°".to_owned()
        } else {
            format!("Straighten {d:+.1}°")
        });
    }
    if prev.perspective != next.perspective {
        out.push(toggle(
            "Keystone",
            prev.perspective.is_some(),
            next.perspective.is_some(),
        ));
    }
    if prev.lens != next.lens {
        out.push(toggle(
            "Lens correction",
            prev.lens.is_some(),
            next.lens.is_some(),
        ));
    }
    if prev.vignette != next.vignette {
        out.push(toggle(
            "Vignette",
            prev.vignette.is_some(),
            next.vignette.is_some(),
        ));
    }
    if prev.output_sharpen != next.output_sharpen {
        out.push(toggle(
            "Output sharpening",
            prev.output_sharpen.is_some(),
            next.output_sharpen.is_some(),
        ));
    }
    if prev.auto_scale != next.auto_scale {
        out.push(format!("Auto-scale {}", on_off(next.auto_scale)));
    }
    if prev.auto_constrain != next.auto_constrain {
        out.push(format!("Auto-constrain {}", on_off(next.auto_constrain)));
    }
}

/// A right-angle re-framing is either a rotate or a flip: only a flip toggles the
/// mirror, while a rotate changes the quarter-turn count with the mirror fixed, so
/// the two are told apart without a lookup table.
fn describe_orientation(prev: Orientation, next: Orientation) -> String {
    if prev.flip != next.flip {
        "Flip".to_owned()
    } else if prev.quarter_turns != next.quarter_turns {
        "Rotate".to_owned()
    } else {
        "Orientation".to_owned()
    }
}

fn describe_locals(
    prev: &[crate::LocalAdjustment],
    next: &[crate::LocalAdjustment],
    out: &mut Vec<String>,
) {
    use std::cmp::Ordering;
    match next.len().cmp(&prev.len()) {
        Ordering::Greater => out.push("Add mask".to_owned()),
        Ordering::Less => out.push("Remove mask".to_owned()),
        Ordering::Equal if prev != next => out.push("Local adjustment".to_owned()),
        Ordering::Equal => {}
    }
}

fn on_off(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Crop, LocalAdjustment, SelectiveTone, Sharpen};

    /// A settings value with the global exposure set, as a concise base for tests.
    fn with_exposure(ev: f32) -> Settings {
        Settings {
            global: Adjustments {
                exposure: Some(ev),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn a_single_scalar_change_names_the_tool_and_value() {
        let prev = Settings::default();
        let next = with_exposure(0.5);
        assert_eq!(describe_change(&prev, &next), "Exposure +0.50 EV");
        // Negative values keep their sign; turning it back off reads as "off".
        assert_eq!(
            describe_change(&prev, &with_exposure(-1.25)),
            "Exposure -1.25 EV"
        );
        assert_eq!(describe_change(&with_exposure(0.5), &prev), "Exposure off");
    }

    #[test]
    fn tone_reads_as_the_individual_slider_that_moved() {
        let mut next = Settings::default();
        next.global.tone = Some(SelectiveTone {
            contrast: 0.3,
            ..Default::default()
        });
        assert_eq!(
            describe_change(&Settings::default(), &next),
            "Contrast +0.30"
        );

        // Two tone sliders at once fall back to the group name.
        let mut two = Settings::default();
        two.global.tone = Some(SelectiveTone {
            contrast: 0.3,
            blacks: -0.2,
            ..Default::default()
        });
        assert_eq!(describe_change(&Settings::default(), &two), "Tone");
    }

    #[test]
    fn enabling_and_disabling_a_tool_reads_as_on_off() {
        let mut on = Settings::default();
        on.global.sharpen = Some(Sharpen::default());
        assert_eq!(describe_change(&Settings::default(), &on), "Sharpen on");
        assert_eq!(describe_change(&on, &Settings::default()), "Sharpen off");
    }

    #[test]
    fn geometry_changes_name_the_geometry_tool() {
        let mut cropped = Settings::default();
        cropped.geometry.crop = Some(Crop {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
        });
        assert_eq!(describe_change(&Settings::default(), &cropped), "Crop");
        assert_eq!(
            describe_change(&cropped, &Settings::default()),
            "Remove crop"
        );

        let mut straight = Settings::default();
        straight.geometry.straighten_degrees = 2.5;
        assert_eq!(
            describe_change(&Settings::default(), &straight),
            "Straighten +2.5°"
        );

        let mut rotated = Settings::default();
        rotated.geometry.orientation = Orientation::IDENTITY.rotate_cw();
        assert_eq!(describe_change(&Settings::default(), &rotated), "Rotate");

        let mut flipped = Settings::default();
        flipped.geometry.orientation = Orientation::IDENTITY.flip_h();
        assert_eq!(describe_change(&Settings::default(), &flipped), "Flip");
    }

    #[test]
    fn a_multi_field_change_summarizes_and_a_neutral_result_resets() {
        // Two independent tools at once → a generic summary.
        let mut multi = with_exposure(0.5);
        multi.global.saturation = Some(1.4);
        assert_eq!(
            describe_change(&Settings::default(), &multi),
            "Multiple adjustments"
        );

        // Clearing several things back to neutral reads as a reset.
        assert_eq!(describe_change(&multi, &Settings::default()), "Reset");
    }

    #[test]
    fn local_adjustments_read_as_mask_changes() {
        let one = Settings {
            locals: vec![LocalAdjustment::default()],
            ..Default::default()
        };
        assert_eq!(describe_change(&Settings::default(), &one), "Add mask");
        assert_eq!(describe_change(&one, &Settings::default()), "Remove mask");

        let mut edited = one.clone();
        edited.locals[0].opacity = 0.5;
        assert_eq!(describe_change(&one, &edited), "Local adjustment");
    }
}
