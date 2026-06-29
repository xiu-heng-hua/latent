//! The top menu bar (File / Edit / View / Help). File ▸ Open (and Ctrl+O), File ▸
//! Open Recent, and File ▸ Export… drive real behavior; the rest of File/Edit
//! share the same code paths the toolbar and keyboard use. A few items are
//! deliberate placeholders for functionality that is not wired yet.

use eframe::egui;

use crate::gui::app::App;
use crate::gui::config;

/// Show the full menu bar (with an open session). `do_undo` / `do_redo` are set
/// when the Edit menu's Undo/Redo items are clicked, so `update` applies them on
/// the single shared history path. `dirty` is set when a menu action (paste, reset)
/// changes the settings, so `update` re-renders.
pub(crate) fn show(
    app: &mut App,
    ctx: &egui::Context,
    do_undo: &mut bool,
    do_redo: &mut bool,
    dirty: &mut bool,
) {
    egui::TopBottomPanel::top("menubar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            let (can_undo, can_redo, title, path) = app
                .session()
                .map(|s| (s.can_undo(), s.can_redo(), s.title.clone(), s.path.clone()))
                .unwrap_or_default();

            ui.menu_button("File", |ui| {
                file_open_items(app, ctx, ui);
                ui.separator();
                // The sidecar autosaves on idle, so an explicit Save is optional.
                ui.add_enabled(false, egui::Button::new("Save sidecar"));
                ui.separator();
                let can_export = app.session().is_some() && !app.render.is_busy();
                // While an export is in flight the action is disabled *and* reads as
                // in-progress ("Exporting…"), so the user sees why it's grayed out
                // rather than a dead button.
                let export_label = if app.exporting {
                    "Exporting…"
                } else {
                    "Export…"
                };
                if ui
                    .add_enabled(can_export, egui::Button::new(export_label))
                    .clicked()
                {
                    app.export_via_dialog(ctx);
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
                // Copy the active variant's settings to the in-app clipboard;
                // Paste applies the develop part (global + locals), keeping the
                // target's geometry. Reset returns the develop to neutral, geometry
                // kept. Each routes through the shared App methods the shortcuts use.
                let has_session = app.session().is_some();
                if ui
                    .add_enabled(has_session, egui::Button::new("Copy settings"))
                    .clicked()
                {
                    app.copy_settings();
                    ui.close();
                }
                if ui
                    .add_enabled(app.can_paste(), egui::Button::new("Paste settings"))
                    .on_hover_text("Apply the copied develop look (keeps this image's geometry)")
                    .clicked()
                {
                    *dirty |= app.paste_settings();
                    ui.close();
                }
                if ui
                    .add_enabled(has_session, egui::Button::new("Reset all develop"))
                    .on_hover_text("Reset develop adjustments to neutral (keeps geometry)")
                    .clicked()
                {
                    *dirty |= app.reset_all_develop();
                    ui.close();
                }
                ui.separator();
                // Apply a saved develop preset (the look only — geometry is kept).
                let has_presets = !app.config.presets.is_empty();
                ui.add_enabled_ui(has_session && has_presets, |ui| {
                    ui.menu_button("Apply preset", |ui| {
                        let mut chosen: Option<usize> = None;
                        for (i, preset) in app.config.presets.iter().enumerate() {
                            if ui.button(&preset.name).clicked() {
                                chosen = Some(i);
                                ui.close();
                            }
                        }
                        if let Some(i) = chosen
                            && let Some(preset) = app.config.presets.get(i).cloned()
                        {
                            *dirty |= app.apply_preset(&preset);
                        }
                    });
                });
            });

            ui.menu_button("View", |ui| {
                let has_session = app.session().is_some();
                if ui
                    .add_enabled(has_session, egui::Button::new("Zoom to fit"))
                    .clicked()
                {
                    app.zoom_fit();
                    ui.close();
                }
                if ui
                    .add_enabled(has_session, egui::Button::new("Zoom to 100%"))
                    .clicked()
                {
                    app.zoom_actual();
                    ui.close();
                }
                if ui
                    .add_enabled(has_session, egui::Button::new("Before / After"))
                    .clicked()
                {
                    if let Some(session) = &mut app.session {
                        session.before = session.before.cycled();
                    }
                    ui.close();
                }
                // Planned: scopes (histogram / clipping).
                ui.add_enabled(false, egui::Button::new("Histogram"));
                ui.separator();
                // Toggle the right-hand controls panel (also bound to Tab).
                let panel_label = if app.panel_visible {
                    "Hide controls panel"
                } else {
                    "Show controls panel"
                };
                if ui.button(panel_label).clicked() {
                    app.panel_visible = !app.panel_visible;
                    ui.close();
                }
                ui.separator();
                // Runtime CPU↔GPU backend toggle. A check next to "Use GPU"
                // reflects the active backend (which shows CPU if GPU init fell
                // back); toggling rebuilds the backend and re-renders. The switch is
                // deferred if a render is in flight, so this stays clickable.
                let mut use_gpu = app.gpu_active();
                if ui
                    .checkbox(&mut use_gpu, "Use GPU")
                    .on_hover_text(
                        "Render on the GPU when a device is available (falls back to CPU)",
                    )
                    .changed()
                {
                    app.request_backend(use_gpu, ctx);
                    ui.close();
                }
            });

            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    app.shortcuts_open = true;
                    ui.close();
                }
                if ui.button("About Latent").clicked() {
                    app.about_open = true;
                    ui.close();
                }
            });

            // The open file's title (basename), right-aligned; the full path is
            // on hover.
            if !title.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&title).on_hover_text(&path);
                });
            }
        });
    });
}

/// A minimal menu bar for the welcome state (no open session): only File ▸ Open /
/// Open Recent / Quit and Help. Keeps Open reachable from the chrome before any
/// file is loaded.
pub(crate) fn show_minimal(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::top("menubar").show(ctx, |ui| {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                file_open_items(app, ctx, ui);
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    app.shortcuts_open = true;
                    ui.close();
                }
                if ui.button("About Latent").clicked() {
                    app.about_open = true;
                    ui.close();
                }
            });
        });
    });
}

/// The File ▸ Open and File ▸ Open Recent items, shared by the full and minimal
/// menu bars. Open is disabled while a file is already loading; Open Recent lists
/// the persisted entries newest-first, pruned of missing files.
fn file_open_items(app: &mut App, ctx: &egui::Context, ui: &mut egui::Ui) {
    if ui
        .add_enabled(!app.is_loading(), egui::Button::new("Open…"))
        .clicked()
    {
        app.open_via_dialog(ctx);
        ui.close();
    }

    let recents = config::existing_recents(&app.config.recent_files);
    ui.add_enabled_ui(!recents.is_empty() && !app.is_loading(), |ui| {
        ui.menu_button("Open Recent", |ui| {
            for path in &recents {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let parent = path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                if ui.button(name).on_hover_text(&parent).clicked() {
                    let path = path.clone();
                    app.open_path(ctx, &path);
                    ui.close();
                }
            }
        });
    });
}
