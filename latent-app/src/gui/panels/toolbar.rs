//! The slim icon toolbar beneath the menu bar: undo/redo (sharing the single
//! history path with the menu and keyboard) and the variant selector / new-
//! variant button. Tool and zoom affordances are placeholders to be wired up
//! later.

use eframe::egui;
use latent_edit::History;

use crate::gui::app::App;
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

            // Planned: zoom controls and tool selection (crop / brush / mask).
            // Placeholders kept disabled so the toolbar reads as the future home
            // for those affordances without faking behavior.
            ui.separator();
            icons::icon_button(ui, false, "zoom_fit", "Zoom to fit");
            icons::icon_button(ui, false, "zoom_100", "Zoom to 100%");
        });
    });
}
