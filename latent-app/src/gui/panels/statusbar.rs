//! The bottom status bar: zoom %, image dimensions, the hover pixel readout, the
//! active backend, and the render/autosave state. Numeric readouts use the
//! monospace style so they don't jitter as digits change width.

use eframe::egui;
use egui::RichText;

use crate::gui::app::{App, BackendKind};

/// Show the status bar.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            // The live zoom percentage (Fit reports its true fitted scale).
            let zoom = match app.zoom {
                crate::gui::canvas::Zoom::Fit => format!("Fit {}%", app.zoom_percent()),
                crate::gui::canvas::Zoom::Percent(_) => format!("{}%", app.zoom_percent()),
            };
            ui.label(RichText::new(zoom).monospace());
            ui.separator();

            // Image dimensions, real now — sourced from the full-resolution base.
            ui.label(
                RichText::new(format!("{} × {}", app.full.width(), app.full.height())).monospace(),
            );
            ui.separator();

            // The pixel under the cursor (sRGB display value), when over the image.
            if let Some(p) = app.pixel_readout {
                ui.label(
                    RichText::new(format!(
                        "{},{}  sRGB {} {} {}",
                        p.x, p.y, p.rgb[0], p.rgb[1], p.rgb[2]
                    ))
                    .monospace(),
                );
                ui.separator();
            }

            // Active backend (CPU/GPU), threaded from `select_backend`.
            let backend = match app.backend_kind {
                BackendKind::Cpu => "CPU",
                BackendKind::Gpu => "GPU",
            };
            ui.label(RichText::new(backend).monospace());
            ui.separator();

            // Render / autosave state.
            if app.render.is_busy() {
                ui.label("Rendering…");
            } else if !app.status.is_empty() {
                ui.label(&app.status);
            } else if app.is_saved() {
                ui.label("Saved");
            } else {
                ui.label("Editing");
            }
        });
    });
}
