//! UI icons drawn as glyphs from the embedded Phosphor font (registered as the
//! [`theme::ICON_FAMILY`] family in [`crate::gui::theme`]). One name→codepoint
//! table maps stable names to the font's private-use-area codepoints so call
//! sites never scatter raw `\u{e0xx}` literals, and later panels can add icons
//! by name. The codepoints are sourced from Phosphor's own glyph manifest.

use eframe::egui;
use egui::{FontFamily, FontId, RichText};

use super::theme;

/// Map a stable icon name to its glyph in the icon font. An unknown name returns
/// a visible placeholder rather than panicking, so a typo at a call site shows up
/// as a box instead of crashing the UI.
pub(crate) fn icon(name: &str) -> char {
    match name {
        "open" => '\u{e256}',       // folder-open
        "save" => '\u{e248}',       // floppy-disk
        "export" => '\u{eaf0}',     // export
        "undo" => '\u{e014}',       // arrow-arc-left
        "redo" => '\u{e016}',       // arrow-arc-right
        "rotate_cw" => '\u{e036}',  // arrow-clockwise
        "rotate_ccw" => '\u{e038}', // arrow-counter-clockwise
        "flip_h" => '\u{ed6a}',     // flip-horizontal
        "flip_v" => '\u{ed6c}',     // flip-vertical
        "zoom_in" => '\u{e310}',    // magnifying-glass-plus
        "zoom_out" => '\u{e30e}',   // magnifying-glass-minus
        "zoom_fit" => '\u{e0a2}',   // arrows-out
        "zoom_100" => '\u{e30c}',   // magnifying-glass
        "crop" => '\u{e1d4}',       // crop
        "straighten" => '\u{e6b8}', // ruler
        "keystone" => '\u{ebe6}',   // perspective
        "brush" => '\u{e6f0}',      // paint-brush
        "mask" => '\u{e9f4}',       // mask-happy
        "histogram" => '\u{e150}',  // chart-bar
        "settings" => '\u{e270}',   // gear
        "info" => '\u{e2ce}',       // info
        "image" => '\u{e2ca}',      // image
        "quit" => '\u{e42a}',       // sign-out
        _ => '\u{e2ce}',            // info, as a visible fallback
    }
}

/// The complete set of icon names this module resolves. Kept beside [`icon`] so
/// the consistency test can sweep every mapping.
#[cfg(test)]
const ICON_NAMES: &[&str] = &[
    "open",
    "save",
    "export",
    "undo",
    "redo",
    "rotate_cw",
    "rotate_ccw",
    "flip_h",
    "flip_v",
    "zoom_in",
    "zoom_out",
    "zoom_fit",
    "zoom_100",
    "crop",
    "straighten",
    "keystone",
    "brush",
    "mask",
    "histogram",
    "settings",
    "info",
    "image",
    "quit",
];

/// Build a `RichText` for an icon glyph in the icon family at the icon size.
pub(crate) fn icon_text(name: &str) -> RichText {
    RichText::new(icon(name).to_string()).font(FontId::new(
        theme::ICON_SIZE,
        FontFamily::Name(theme::ICON_FAMILY.into()),
    ))
}

/// A selectable icon: the glyph rendered in the icon family as a toggle that reads
/// as active (highlighted) when `selected`, with a hover tooltip. The icon variant
/// of [`egui::Ui::selectable_label`] for activators that head a control group.
/// Returns the `Response` so the caller wires its action and can place an accent
/// mark relative to it.
pub(crate) fn selectable_icon(
    ui: &mut egui::Ui,
    selected: bool,
    name: &str,
    tooltip: &str,
) -> egui::Response {
    ui.selectable_label(selected, icon_text(name))
        .on_hover_text(tooltip)
}

/// An icon button: the glyph rendered in the icon family, with a hover tooltip
/// and an explicit enabled state (a disabled affordance still shows its glyph and
/// tooltip). Returns the `Response` so the caller wires its action.
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    enabled: bool,
    name: &str,
    tooltip: &str,
) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(icon_text(name)))
        .on_hover_text(tooltip)
}

/// A show/hide toggle drawn as an eye — an open eye (almond outline + pupil) when
/// `shown`, a closed lid (a downward arc with a few lashes) when hidden. Painted
/// directly rather than as a font glyph, so it reads clearly at this small size and
/// the "hidden" state is an unmistakable closed eye. The button chrome (size and
/// hover border) matches [`selectable_icon`], so it sits flush with the tool icons
/// it shares a header row with; only the glyph changes between states, never the
/// footprint, so toggling never shifts the layout. Returns the `Response`; the
/// caller flips the visibility on a click.
pub(crate) fn eye_toggle(ui: &mut egui::Ui, shown: bool, tooltip: &str) -> egui::Response {
    // Match a `selectable_icon`'s footprint: an icon-sized glyph plus the button
    // padding, so the eye sits flush with the tool icons it shares a row with. A
    // representative glyph stands in for the (painter-drawn) eye's metrics.
    let icon_font = FontId::new(
        theme::ICON_SIZE,
        FontFamily::Name(theme::ICON_FAMILY.into()),
    );
    let glyph =
        ui.fonts(|f| f.layout_no_wrap(icon("crop").to_string(), icon_font, egui::Color32::WHITE));
    let pad = ui.spacing().button_padding;
    let size = egui::vec2(
        glyph.size().x + 2.0 * pad.x,
        (glyph.size().y + 2.0 * pad.y).max(ui.spacing().interact_size.y),
    );
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = resp
        .on_hover_text(tooltip)
        .on_hover_cursor(egui::CursorIcon::PointingHand);

    // Render exactly like `selectable_label`: the chrome (fill + the accent hover
    // border) is drawn only on hover/focus, using the same widget visuals the tool
    // icons do, so the eye reads as the same kind of button. The eye carries no
    // persistent "selected" accent — its state is shown by the open/closed glyph,
    // not by a highlight — so the resting row of subsection headers stays calm.
    let vis = ui.style().interact_selectable(&resp, false);
    if resp.hovered() || resp.has_focus() {
        ui.painter().rect(
            rect,
            vis.corner_radius,
            vis.weak_bg_fill,
            vis.bg_stroke,
            egui::StrokeKind::Inside,
        );
    }
    let color = vis.text_color();

    let painter = ui.painter();
    let c = rect.center();
    let hw = theme::ICON_SIZE * 0.42;
    let hh = theme::ICON_SIZE * 0.30;
    let stroke = egui::Stroke::new(1.4, color);
    const N: usize = 9;
    if shown {
        // Almond outline: the top arc left→right, then the bottom arc right→left,
        // closed — plus a pupil in the middle.
        let mut pts = Vec::with_capacity(N * 2);
        for i in 0..N {
            let t = -1.0 + 2.0 * i as f32 / (N - 1) as f32;
            pts.push(egui::pos2(c.x + t * hw, c.y - hh * (1.0 - t * t)));
        }
        for i in 0..N {
            let t = 1.0 - 2.0 * i as f32 / (N - 1) as f32;
            pts.push(egui::pos2(c.x + t * hw, c.y + hh * (1.0 - t * t)));
        }
        painter.add(egui::Shape::closed_line(pts, stroke));
        painter.circle_filled(c, hh * 0.5, color);
    } else {
        // Closed lid: a shallow downward arc with three short lashes below it.
        let lid = |t: f32| egui::pos2(c.x + t * hw, c.y - hh * 0.25 + hh * 0.65 * (1.0 - t * t));
        let pts: Vec<egui::Pos2> = (0..N)
            .map(|i| lid(-1.0 + 2.0 * i as f32 / (N - 1) as f32))
            .collect();
        painter.add(egui::Shape::line(pts, stroke));
        for &t in &[-0.55_f32, 0.0, 0.55] {
            let base = lid(t);
            painter.line_segment([base, base + egui::vec2(0.0, hh * 0.5)], stroke);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_table_is_consistent() {
        // Every mapped codepoint must be in the Unicode private-use area (where
        // icon fonts place their glyphs) and no two names may collide — so a
        // future icon addition can't silently shadow an existing one.
        let mut seen = std::collections::HashMap::new();
        for &name in ICON_NAMES {
            let cp = icon(name) as u32;
            assert!(
                (0xE000..=0xF8FF).contains(&cp),
                "{name} -> {cp:#06x} is not in the private-use area"
            );
            if let Some(prev) = seen.insert(cp, name) {
                panic!("{name} and {prev} share codepoint {cp:#06x}");
            }
        }
    }
}
