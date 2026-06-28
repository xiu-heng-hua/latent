//! The empty-state welcome screen, shown in the central panel when no file is
//! open. It teaches the three ways into the editor — a large Open button, the
//! recent-files list, and a drag-drop hint — and shows the app version. Opening
//! any of these kicks off the off-thread develop, which installs a session and
//! flips the next frame into the editor.
//!
//! While a develop is in flight (the loading state) the central panel shows a
//! spinner and the file name instead, so a slow decode reads as "working", not
//! "frozen".

use std::path::Path;

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

                // The one-time first-run hint, shown until dismissed (remembered in
                // the config). A subtle inline callout, not a focus-trapping modal.
                if app.should_show_hint() {
                    ui.add_space(28.0);
                    first_run_hint(app, ui);
                }
            });
        });
}

/// The subtle, dismissable first-run hint pointing new users at Open / drag-drop.
/// Non-blocking: it sits inline on the welcome screen and a "Got it" button (or
/// the first successful open) retires it for good.
fn first_run_hint(app: &mut App, ui: &mut egui::Ui) {
    egui::Frame::default()
        .fill(theme::PANEL_FILL)
        .stroke(egui::Stroke::new(1.0, theme::ACCENT))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_max_width(360.0);
            ui.label(
                egui::RichText::new("New here? Open a RAW with the button above, File ▸ Open, or just drag one onto the window.")
                    .size(13.0),
            );
            ui.add_space(6.0);
            if ui.button("Got it").clicked() {
                app.dismiss_hint();
            }
        });
}

/// The loading view: a spinner and the developing file's name, centered on the
/// neutral canvas surround. Shown while a develop is in flight so the wait reads
/// as progress rather than a freeze. The window stays fully interactive — this is
/// just the central panel's content for that state.
pub(crate) fn show_loading(ctx: &egui::Context, path: &Path) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let surround = egui::Frame::central_panel(&ctx.style()).fill(theme::CANVAS_SURROUND);
    egui::CentralPanel::default()
        .frame(surround)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.4);
                ui.add(egui::Spinner::new().size(28.0));
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(format!("Developing {name}…"))
                        .size(16.0)
                        .color(egui::Color32::from_gray(40)),
                );
            });
        });
    // Keep animating the spinner while the develop runs.
    ctx.request_repaint();
}
