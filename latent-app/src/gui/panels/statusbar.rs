//! The bottom status bar: zoom %, image dimensions, the hover pixel readout, the
//! active backend, and the render/autosave state. Shown only with an open session.
//! Numeric readouts use the monospace style so they don't jitter as digits change
//! width.

use eframe::egui;
use egui::RichText;

use crate::gui::app::App;

/// Show the status bar.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let Some(session) = app.session() else {
                return;
            };
            // The live zoom percentage (Fit reports its true fitted scale).
            let zoom = match session.zoom {
                crate::gui::canvas::Zoom::Fit => format!("Fit {}%", app.zoom_percent()),
                crate::gui::canvas::Zoom::Percent(_) => format!("{}%", app.zoom_percent()),
            };
            ui.label(RichText::new(zoom).monospace());
            ui.separator();

            // Image dimensions, sourced from the full-resolution base.
            ui.label(
                RichText::new(format!(
                    "{} × {}",
                    session.full.width(),
                    session.full.height()
                ))
                .monospace(),
            );
            ui.separator();

            // The pixel under the cursor (sRGB display value), when over the image.
            if let Some(p) = session.pixel_readout {
                ui.label(
                    RichText::new(format!(
                        "{},{}  sRGB {} {} {}",
                        p.x, p.y, p.rgb[0], p.rgb[1], p.rgb[2]
                    ))
                    .monospace(),
                );
                ui.separator();
            }

            // Active backend (CPU/GPU): the one actually rendering, reflecting any
            // GPU→CPU fallback, with a hint that a switch is in flight.
            let backend = app.backend_kind.label();
            let label = if app.pending_backend.is_some() {
                format!("{backend} (switching…)")
            } else {
                backend.to_owned()
            };
            ui.label(RichText::new(label).monospace())
                .on_hover_text("Active rendering backend");
            ui.separator();

            // Render / autosave state.
            if app.render.is_busy() {
                ui.label("Rendering…");
            } else if !app.status.is_empty() {
                ui.label(&app.status);
            } else if session.is_saved() {
                ui.label("Saved");
            } else {
                ui.label("Editing");
            }
        });
    });
}
