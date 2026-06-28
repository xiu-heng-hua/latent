//! The single source of truth for the app's look: a tuned dark, neutral,
//! single-accent [`egui::Visuals`], the spacing/rounding/stroke tokens every
//! panel and widget reads, the embedded typefaces, and the icon font. Apply it
//! once at startup with [`apply`]; nothing in `gui/` should hard-code a
//! `Color32`/`CornerRadius`/size inline when a token here covers it.

use eframe::egui;
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

// ---------------------------------------------------------------------------
// Color tokens — a neutral gray ramp (no blue tint) plus a single accent.
// ---------------------------------------------------------------------------

/// The one accent color, used for selection, links, active widget strokes, and
/// the curve editor. A calm cool gray-blue, deliberately low-chroma so it never
/// competes with the photo. Replaces the stray `LIGHT_BLUE` the editor had.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x9D, 0xC9);

/// The success accent — a muted, desaturated green for a completed export or
/// save. Low-chroma to stay in the neutral chrome; only ever a toast stripe or a
/// status mark, never on the photo.
pub(crate) const SUCCESS: Color32 = Color32::from_rgb(0x5C, 0xA8, 0x6E);

/// The error accent — a muted red for a failed export/save or an unreadable
/// file. Used for the error toast stripe and the error-modal headline; like the
/// other tokens it is desaturated so it warns without shouting.
pub(crate) const ERROR: Color32 = Color32::from_rgb(0xCC, 0x5F, 0x5F);

/// Window background — the darkest neutral.
const WINDOW_FILL: Color32 = Color32::from_gray(24);
/// Panel background (menu bar, toolbar, side panel, status bar).
pub(crate) const PANEL_FILL: Color32 = Color32::from_gray(32);
/// Faint zebra fill for alternating rows etc.
const FAINT_BG: Color32 = Color32::from_gray(38);
/// The darkest sunken background (text edits, slider rails).
const EXTREME_BG: Color32 = Color32::from_gray(16);
/// Resting interactive widget fill (buttons at rest).
const WIDGET_INACTIVE: Color32 = Color32::from_gray(44);
/// Hovered interactive widget fill.
const WIDGET_HOVERED: Color32 = Color32::from_gray(56);
/// Active (pressed/dragged) interactive widget fill.
const WIDGET_ACTIVE: Color32 = Color32::from_gray(70);
/// Normal body text.
const TEXT: Color32 = Color32::from_gray(220);
/// Hairline separators / widget outlines.
const HAIRLINE: Color32 = Color32::from_gray(70);

/// The tick color marking a slider's neutral (unchanged) position on its track.
/// A muted light gray, distinct from the accent so it reads as a reference mark
/// rather than an active selection.
pub(crate) const NEUTRAL_MARKER: Color32 = Color32::from_gray(150);

/// The neutral mid-gray that surrounds the photo in the central panel. A RAW
/// editor's working area must surround the image with a *color-neutral* gray:
/// the channels are deliberately equal (R == G == B) so the surround pushes no
/// tint into the eye's white balance and therefore biases no color or exposure
/// judgement the user makes. `from_gray(118)` reads as a perceptual mid against
/// the already-sRGB-encoded chrome. Keep the channels equal on any future edit —
/// `canvas_surround_is_neutral_gray` guards exactly that.
pub(crate) const CANVAS_SURROUND: Color32 = Color32::from_gray(118);

// ---------------------------------------------------------------------------
// Shape / spacing tokens.
// ---------------------------------------------------------------------------

/// Corner radius applied uniformly to interactive widgets.
pub(crate) const CORNER_RADIUS: CornerRadius = CornerRadius::same(4);
/// Outline / separator stroke width.
const STROKE_WIDTH: f32 = 1.0;

/// Spacing between successive items in a layout.
const ITEM_SPACING: egui::Vec2 = egui::vec2(8.0, 6.0);
/// Inner margin for panels.
pub(crate) const PANEL_MARGIN: i8 = 8;
/// Default width of the right-hand controls side panel.
pub(crate) const SIDE_PANEL_WIDTH: f32 = 280.0;
/// Minimum width the controls side panel can be dragged to — wide enough that the
/// widest slider label plus its numeric entry stays readable rather than clipping.
pub(crate) const SIDE_PANEL_MIN_WIDTH: f32 = 240.0;
/// Maximum width the controls side panel can be dragged to.
pub(crate) const SIDE_PANEL_MAX_WIDTH: f32 = 520.0;
/// Approximate width reserved for a slider's numeric entry, used to keep the
/// neutral-marker tick over the slider track rather than under the entry.
pub(crate) const SLIDER_NUMERIC_WIDTH: f32 = 56.0;
/// The curve editor's drawing area (max width × fixed height).
pub(crate) const CURVE_EDITOR_SIZE: egui::Vec2 = egui::vec2(220.0, 160.0);

// ---------------------------------------------------------------------------
// Window tokens (consumed by `app::run`'s `ViewportBuilder`).
// ---------------------------------------------------------------------------

/// Default window inner size (logical points) — roomy enough for the side panel
/// plus a usable canvas on a photo editor.
pub(crate) const DEFAULT_WINDOW_SIZE: [f32; 2] = [1400.0, 900.0];
/// Minimum window inner size (logical points) that keeps the side panel and a
/// usable canvas visible.
pub(crate) const MIN_WINDOW_SIZE: [f32; 2] = [900.0, 600.0];

// ---------------------------------------------------------------------------
// Typography tokens.
// ---------------------------------------------------------------------------

const HEADING_SIZE: f32 = 18.0;
const BODY_SIZE: f32 = 14.0;
const BUTTON_SIZE: f32 = 14.0;
const SMALL_SIZE: f32 = 12.0;
const MONO_SIZE: f32 = 13.0;

/// The named font family used for icon glyphs (see [`crate::gui::icons`]).
pub(crate) const ICON_FAMILY: &str = "icons";
/// Default icon glyph size for toolbar/menu affordances.
pub(crate) const ICON_SIZE: f32 = 16.0;

// Embedded typefaces (committed under `latent-app/assets/fonts/`, OFL/MIT). They
// are embedded as static bytes rather than pulled in as a crate so the build
// stays hermetic and the dependency list stays flat.
const INTER: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const JETBRAINS_MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const PHOSPHOR: &[u8] = include_bytes!("../../assets/fonts/Phosphor-Regular.ttf");

/// Build the [`egui::FontDefinitions`]: the UI sans (Inter) takes precedence on
/// the proportional family, the mono (JetBrains Mono) on the monospace family —
/// both keeping egui's built-ins as glyph fallback so missing glyphs (e.g. a
/// non-Latin file path in the title) still render rather than showing tofu. The
/// icon font registers under its own named family so its private-use codepoints
/// never shadow text.
fn font_definitions() -> egui::FontDefinitions {
    use std::sync::Arc;
    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "inter".to_owned(),
        Arc::new(egui::FontData::from_static(INTER)),
    );
    fonts.font_data.insert(
        "jetbrains-mono".to_owned(),
        Arc::new(egui::FontData::from_static(JETBRAINS_MONO)),
    );
    fonts.font_data.insert(
        "phosphor".to_owned(),
        Arc::new(egui::FontData::from_static(PHOSPHOR)),
    );

    // Prepend so the embedded faces win, but leave the built-ins as fallback.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "inter".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "jetbrains-mono".to_owned());

    // The icon font is its own family, not mixed into text families.
    fonts.families.insert(
        FontFamily::Name(ICON_FAMILY.into()),
        vec!["phosphor".to_owned()],
    );

    fonts
}

/// The text-style ramp: proportional for headings/body/small/button, monospace
/// for numeric readouts.
fn text_styles() -> std::collections::BTreeMap<TextStyle, FontId> {
    use FontFamily::{Monospace, Proportional};
    [
        (TextStyle::Heading, FontId::new(HEADING_SIZE, Proportional)),
        (TextStyle::Body, FontId::new(BODY_SIZE, Proportional)),
        (TextStyle::Button, FontId::new(BUTTON_SIZE, Proportional)),
        (TextStyle::Small, FontId::new(SMALL_SIZE, Proportional)),
        (TextStyle::Monospace, FontId::new(MONO_SIZE, Monospace)),
    ]
    .into_iter()
    .collect()
}

/// A tuned dark, neutral, single-accent [`egui::Visuals`]. Starts from
/// `Visuals::dark()` and overrides the cool defaults with a strictly neutral
/// gray ramp and one accent on selection/links/active strokes.
fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();

    v.window_fill = WINDOW_FILL;
    v.panel_fill = PANEL_FILL;
    v.faint_bg_color = FAINT_BG;
    v.extreme_bg_color = EXTREME_BG;
    v.window_stroke = Stroke::new(STROKE_WIDTH, HAIRLINE);

    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.linear_multiply(0.4);
    v.selection.stroke = Stroke::new(STROKE_WIDTH, ACCENT);

    let stroke = Stroke::new(STROKE_WIDTH, HAIRLINE);
    let text_stroke = Stroke::new(STROKE_WIDTH, TEXT);

    v.widgets.noninteractive.bg_fill = PANEL_FILL;
    v.widgets.noninteractive.weak_bg_fill = PANEL_FILL;
    v.widgets.noninteractive.bg_stroke = stroke;
    v.widgets.noninteractive.fg_stroke = text_stroke;
    v.widgets.noninteractive.corner_radius = CORNER_RADIUS;

    v.widgets.inactive.bg_fill = WIDGET_INACTIVE;
    v.widgets.inactive.weak_bg_fill = WIDGET_INACTIVE;
    v.widgets.inactive.bg_stroke = stroke;
    v.widgets.inactive.fg_stroke = text_stroke;
    v.widgets.inactive.corner_radius = CORNER_RADIUS;

    v.widgets.hovered.bg_fill = WIDGET_HOVERED;
    v.widgets.hovered.weak_bg_fill = WIDGET_HOVERED;
    v.widgets.hovered.bg_stroke = Stroke::new(STROKE_WIDTH, ACCENT);
    v.widgets.hovered.fg_stroke = Stroke::new(STROKE_WIDTH, Color32::WHITE);
    v.widgets.hovered.corner_radius = CORNER_RADIUS;

    v.widgets.active.bg_fill = WIDGET_ACTIVE;
    v.widgets.active.weak_bg_fill = WIDGET_ACTIVE;
    v.widgets.active.bg_stroke = Stroke::new(STROKE_WIDTH, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(STROKE_WIDTH, Color32::WHITE);
    v.widgets.active.corner_radius = CORNER_RADIUS;

    v.widgets.open.bg_fill = WIDGET_INACTIVE;
    v.widgets.open.weak_bg_fill = WIDGET_INACTIVE;
    v.widgets.open.bg_stroke = stroke;
    v.widgets.open.fg_stroke = text_stroke;
    v.widgets.open.corner_radius = CORNER_RADIUS;

    v
}

/// The tuned [`egui::Style`]: the neutral [`visuals`], the spacing tokens, and
/// the embedded text-style ramp.
fn style() -> egui::Style {
    let mut style = egui::Style {
        visuals: visuals(),
        text_styles: text_styles(),
        ..Default::default()
    };
    style.spacing.item_spacing = ITEM_SPACING;
    style.spacing.slider_width = 160.0;
    style
}

/// Apply the theme — fonts, visuals, spacing, text styles — to `ctx`. Call once
/// at startup (in the `eframe` creation closure). Idempotent: re-applying it
/// simply re-installs the same style and fonts.
pub(crate) fn apply(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.set_style(style());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_surround_is_neutral_gray() {
        // The canvas surround must be a true neutral: equal channels (no color
        // cast that would bias the user's color/exposure judgement) and a
        // mid-range gray. Catches an accidental tint on a future edit.
        assert_eq!(CANVAS_SURROUND.r(), CANVAS_SURROUND.g());
        assert_eq!(CANVAS_SURROUND.g(), CANVAS_SURROUND.b());
        assert!(
            (90..=150).contains(&CANVAS_SURROUND.r()),
            "surround should be a mid gray, got {}",
            CANVAS_SURROUND.r()
        );
    }
}
