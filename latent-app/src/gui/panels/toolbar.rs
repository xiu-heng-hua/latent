//! The slim icon toolbar beneath the menu bar: undo/redo (sharing the single
//! history path with the menu and keyboard), the variant selector / new-variant
//! button, the before/after toggle, and the zoom controls (fit / 100% / −/+).

use eframe::egui;
use latent_edit::History;

use crate::gui::app::{App, BeforeAfter};
use crate::gui::icons;

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
