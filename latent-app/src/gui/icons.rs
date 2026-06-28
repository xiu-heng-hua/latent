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
        "eye" => '\u{e1fc}',        // eye
        "eye_off" => '\u{e200}',    // eye-slash
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
    "eye",
    "eye_off",
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
