//! The empty-state welcome screen, shown in the central panel when no file is
//! open. It teaches the three ways into the editor — a large Open button, the
//! recent-files list, and a drag-drop hint — and shows the app version. Opening
//! any of these kicks off the off-thread develop, which installs a session and
//! flips the next frame into the editor.

use eframe::egui;

use crate::gui::app::App;
use crate::gui::config;
use crate::gui::theme;

/// Show the welcome screen. Reads the recent list (pruned of missing files) and
/// dispatches Open / recent-click through the same open path the menu uses.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    let surround = egui::Frame::central_panel(&ctx.style()).fill(theme::CANVAS_SURROUND);
    egui::CentralPanel::default()
        .frame(surround)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                // Push the content down to roughly the vertical middle.
                ui.add_space(ui.available_height() * 0.18);

                ui.heading(egui::RichText::new("latent").size(40.0).strong());
                ui.label(
                    egui::RichText::new(format!("version {}", env!("CARGO_PKG_VERSION"))).weak(),
                );
                ui.add_space(24.0);

                let opening = app.is_loading();
                let open = ui.add_enabled(
                    !opening,
                    egui::Button::new(egui::RichText::new("Open a RAW…").size(18.0))
                        .min_size(egui::vec2(220.0, 44.0)),
                );
                if open.clicked() {
                    app.open_via_dialog(ctx);
                }
                if opening {
                    ui.add_space(8.0);
                    ui.label("Opening…");
                }

                ui.add_space(20.0);

                // Recent files, newest-first, pruned of missing paths. Each row
                // opens on click via the same develop path.
                let recents = config::existing_recents(&app.config.recent_files);
                if !recents.is_empty() {
                    ui.label(egui::RichText::new("Recent").strong());
                    ui.add_space(4.0);
                    for path in &recents {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string());
                        let parent = path
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default();
                        let row = ui
                            .add_enabled(
                                !opening,
                                egui::Button::new(name).min_size(egui::vec2(240.0, 0.0)),
                            )
                            .on_hover_text(&parent);
                        if row.clicked() {
                            let path = path.clone();
                            app.open_path(ctx, &path);
                        }
                    }
                    ui.add_space(20.0);
                }

                ui.label(egui::RichText::new("…or drop a RAW anywhere on this window").weak());
            });
        });
}
