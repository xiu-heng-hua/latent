//! The top menu bar (File / Edit / View / Help). Many items are deliberate
//! placeholders for functionality that is not wired yet. The items that drive
//! existing behavior — Edit ▸ Undo/Redo, File ▸ Export, File ▸ Quit — share the
//! same code paths the toolbar and keyboard use (the undo/redo flags are folded
//! back into `update`).

use eframe::egui;

use crate::gui::app::App;

/// Show the menu bar. `do_undo` / `do_redo` are set when the Edit menu's
/// Undo/Redo items are clicked, so `update` applies them on the single shared
/// history path.
pub(crate) fn show(app: &mut App, ctx: &egui::Context, do_undo: &mut bool, do_redo: &mut bool) {
    egui::TopBottomPanel::top("menubar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            let can_undo = app.variants[app.active].can_undo();
            let can_redo = app.variants[app.active].can_redo();

            ui.menu_button("File", |ui| {
                // Planned: a native Open… file dialog.
                ui.add_enabled(false, egui::Button::new("Open…"));
                // Planned: a recent-files submenu.
                ui.add_enabled(false, egui::Button::new("Open Recent"));
                // The sidecar autosaves on idle, so an explicit Save is optional;
                // a manual sidecar save may be added here later.
                ui.add_enabled(false, egui::Button::new("Save sidecar"));
                ui.separator();
                // Planned: a full export dialog. For now this triggers the
                // existing full-resolution export to the current output path.
                if ui
                    .add_enabled(!app.render.is_busy(), egui::Button::new("Export…"))
                    .clicked()
                {
                    app.export(ctx);
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            ui.menu_button("Edit", |ui| {
                if ui
                    .add_enabled(can_undo, egui::Button::new("Undo"))
                    .clicked()
                {
                    *do_undo = true;
                    ui.close();
                }
                if ui
                    .add_enabled(can_redo, egui::Button::new("Redo"))
                    .clicked()
                {
                    *do_redo = true;
                    ui.close();
                }
                ui.separator();
                // Planned: copy/paste develop settings.
                ui.add_enabled(false, egui::Button::new("Copy settings"));
                ui.add_enabled(false, egui::Button::new("Paste settings"));
            });

            ui.menu_button("View", |ui| {
                if ui.button("Zoom to fit").clicked() {
                    app.zoom_fit();
                    ui.close();
                }
                if ui.button("Zoom to 100%").clicked() {
                    app.zoom_actual();
                    ui.close();
                }
                if ui.button("Before / After").clicked() {
                    app.before = match app.before {
                        crate::gui::app::BeforeAfter::Off => crate::gui::app::BeforeAfter::Toggle,
                        crate::gui::app::BeforeAfter::Toggle => crate::gui::app::BeforeAfter::Split,
                        crate::gui::app::BeforeAfter::Split => crate::gui::app::BeforeAfter::Off,
                    };
                    ui.close();
                }
                // Planned: scopes (histogram / clipping).
                ui.add_enabled(false, egui::Button::new("Histogram"));
                ui.separator();
                // Planned: panel visibility toggles.
                ui.add_enabled(false, egui::Button::new("Show controls panel"));
            });

            ui.menu_button("Help", |ui| {
                // Planned: a keyboard-shortcut cheat sheet.
                ui.add_enabled(false, egui::Button::new("Keyboard shortcuts"));
                // Planned: an About dialog.
                ui.add_enabled(false, egui::Button::new("About latent"));
            });

            // The open file's title (basename), right-aligned; the full path is
            // on hover. The window title is the single authoritative title; this
            // is just a visible label, not a second `with_title`.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(&app.title).on_hover_text(&app.path);
            });
        });
    });
}
