//! The slim icon toolbar beneath the menu bar: undo/redo (sharing the single
//! history path with the menu and keyboard), the variant selector / new-variant
//! button, the on-canvas tool selector, the discrete rotate/flip buttons, the
//! mask-overlay toggle, the before/after toggle, and the zoom controls.

use eframe::egui;
use latent_edit::History;

use crate::gui::app::{App, BeforeAfter};
use crate::gui::icons;
use crate::gui::tools::CanvasTool;
use crate::gui::tools::overlay::OverlayMode;

/// Show the toolbar. `do_undo` / `do_redo` are OR-ed with the toolbar's
/// undo/redo clicks; `dirty` is set when the active variant changes or a new
/// variant is added, so `update` re-renders.
pub(crate) fn show(
    app: &mut App,
    ctx: &egui::Context,
    do_undo: &mut bool,
    do_redo: &mut bool,
    dirty: &mut bool,
) {
    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let can_undo = app.variants[app.active].can_undo();
            let can_redo = app.variants[app.active].can_redo();
            *do_undo |= icons::icon_button(ui, can_undo, "undo", "Undo (Cmd/Ctrl+Z)").clicked();
            *do_redo |=
                icons::icon_button(ui, can_redo, "redo", "Redo (Cmd/Ctrl+Shift+Z)").clicked();

            ui.separator();

            ui.label("Variant:");
            for i in 0..app.variants.len() {
                if ui
                    .selectable_label(i == app.active, format!("{}", i + 1))
                    .clicked()
                {
                    app.active = i;
                    *dirty = true;
                }
            }
            if ui.button("+").on_hover_text("New variant (copy)").clicked() {
                let copy = app.variants[app.active].current().clone();
                app.variants.push(History::new(copy));
                app.active = app.variants.len() - 1;
                *dirty = true;
            }

            ui.separator();

            // On-canvas tool selector. Selecting a tool activates its handles on
            // the image; the same tool also activates when its panel section is
            // open. "View" is the no-tool state (pure pan/zoom).
            tool_selector(app, ui);

            ui.separator();

            // Discrete orientation: rotate 90° CW/CCW and flip H/V, each one undo
            // step. They fold into the same single resample as straighten/crop.
            if ui
                .button("⟳")
                .on_hover_text("Rotate 90° clockwise")
                .clicked()
            {
                app.apply_orientation(|o| o.rotate_cw());
                *dirty = true;
            }
            if ui
                .button("⟲")
                .on_hover_text("Rotate 90° counter-clockwise")
                .clicked()
            {
                app.apply_orientation(|o| o.rotate_ccw());
                *dirty = true;
            }
            if ui.button("⇋").on_hover_text("Flip horizontal").clicked() {
                app.apply_orientation(|o| o.flip_h());
                *dirty = true;
            }
            if ui.button("⇅").on_hover_text("Flip vertical").clicked() {
                app.apply_orientation(|o| o.flip_v());
                *dirty = true;
            }

            ui.separator();

            // Mask-overlay toggle (off / red wash / mask-only). Pure visualization
            // — no render change.
            overlay_toggle(app, ui);

            ui.separator();

            // Before/after: cycle Off → Toggle → Split (also bound to `).
            let before_label = match app.before {
                BeforeAfter::Off => "After",
                BeforeAfter::Toggle => "Before",
                BeforeAfter::Split => "Split",
            };
            if ui
                .selectable_label(app.before != BeforeAfter::Off, before_label)
                .on_hover_text("Before / after (`)")
                .clicked()
            {
                app.before = match app.before {
                    BeforeAfter::Off => BeforeAfter::Toggle,
                    BeforeAfter::Toggle => BeforeAfter::Split,
                    BeforeAfter::Split => BeforeAfter::Off,
                };
            }

            // Zoom controls. Fit / 100% snap the intent; −/+ step the ladder.
            ui.separator();
            if icons::icon_button(ui, true, "zoom_fit", "Zoom to fit (0)").clicked() {
                app.zoom_fit();
            }
            if icons::icon_button(ui, true, "zoom_100", "Zoom to 100% (1)").clicked() {
                app.zoom_actual();
            }
            if icons::icon_button(ui, true, "zoom_out", "Zoom out (−)").clicked() {
                app.zoom_step(-1);
            }
            if icons::icon_button(ui, true, "zoom_in", "Zoom in (+)").clicked() {
                app.zoom_step(1);
            }
            ui.label(format!("{}%", app.zoom_percent()));
        });
    });
}

/// The on-canvas tool selector: a row of selectable labels, one per tool. Only
/// the active tool draws handles and consumes the canvas pointer.
fn tool_selector(app: &mut App, ui: &mut egui::Ui) {
    let tools = [
        (CanvasTool::None, "View"),
        (CanvasTool::Crop, "Crop"),
        (CanvasTool::Straighten, "Level"),
        (CanvasTool::Keystone, "Keystone"),
        (CanvasTool::MaskShape, "Mask"),
        (CanvasTool::Brush, "Brush"),
    ];
    for (tool, label) in tools {
        if ui.selectable_label(app.tool == tool, label).clicked() {
            // Toggle off to View when re-clicking the active tool.
            app.tool = if app.tool == tool {
                CanvasTool::None
            } else {
                tool
            };
        }
    }
}

/// The mask-overlay toggle: cycles Off → red wash → mask-only.
fn overlay_toggle(app: &mut App, ui: &mut egui::Ui) {
    let label = match app.overlay_mode {
        OverlayMode::Off => "Mask: off",
        OverlayMode::Color => "Mask: red",
        OverlayMode::MaskOnly => "Mask: gray",
    };
    if ui
        .selectable_label(app.overlay_mode.is_on(), label)
        .on_hover_text("Show the selected mask as an overlay")
        .clicked()
    {
        app.overlay_mode = match app.overlay_mode {
            OverlayMode::Off => OverlayMode::Color,
            OverlayMode::Color => OverlayMode::MaskOnly,
            OverlayMode::MaskOnly => OverlayMode::Off,
        };
    }
}
