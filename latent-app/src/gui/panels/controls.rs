//! The right-hand controls side panel: the full stack of develop sections
//! (light, color, detail, geometry, local adjustments) plus the export row. Each
//! section delegates to a builder in [`crate::gui::widgets`]; the panel only
//! wires them to the active variant and folds their `dirty` flags.

use eframe::egui;
use latent_edit::MaskShape;

use crate::gui::app::App;
use crate::gui::theme;
use crate::gui::widgets;

/// Show the controls panel and return whether the preview needs a redraw.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) -> bool {
    let active = app.active;
    let mut dirty = false;

    let frame = egui::Frame::side_top_panel(&ctx.style())
        .inner_margin(egui::Margin::same(theme::PANEL_MARGIN));
    egui::SidePanel::right("controls")
        .default_width(theme::SIDE_PANEL_WIDTH)
        .frame(frame)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Light");
                dirty |= widgets::opt_point_slider(
                    ui,
                    &mut app.variants[active],
                    "Exposure (EV)",
                    -5.0..=5.0,
                    0.0,
                    |s| s.global.exposure,
                    |s, v| s.global.exposure = v,
                );
                dirty |= widgets::tone_block(ui, &mut app.variants[active]);

                ui.separator();
                ui.heading("Color");
                dirty |= widgets::white_balance_block(ui, &mut app.variants[active]);
                dirty |= widgets::opt_point_slider(
                    ui,
                    &mut app.variants[active],
                    "Saturation",
                    0.0..=2.0,
                    1.0,
                    |s| s.global.saturation,
                    |s, v| s.global.saturation = v,
                );
                dirty |=
                    widgets::curves_block(ui, &mut app.variants[active], &mut app.curve_channel);

                ui.separator();
                ui.heading("Detail");
                dirty |= widgets::sharpen_block(ui, &mut app.variants[active]);
                dirty |= widgets::clarity_block(ui, &mut app.variants[active]);
                dirty |= widgets::opt_point_slider(
                    ui,
                    &mut app.variants[active],
                    "Dehaze",
                    0.0..=1.0,
                    0.0,
                    |s| s.global.dehaze,
                    |s, v| s.global.dehaze = v,
                );
                dirty |= widgets::noise_reduction_block(ui, &mut app.variants[active]);

                ui.separator();
                ui.heading("Geometry");
                dirty |= widgets::straighten_slider(ui, &mut app.variants[active]);
                dirty |= widgets::keystone_block(ui, &mut app.variants[active]);
                dirty |= widgets::crop_block(ui, &mut app.variants[active]);
                dirty |= widgets::vignette_slider(ui, &mut app.variants[active]);

                ui.separator();
                ui.heading("Local Adjustments");
                dirty |=
                    widgets::local_adjustments(ui, &mut app.variants[active], &mut app.local_sel);
                // Brush tool: only when the selected local is a brush mask. Dabs
                // are painted on the image in the central panel using these
                // settings.
                if app.variants[active]
                    .current()
                    .locals
                    .get(app.local_sel)
                    .is_some_and(|l| matches!(l.mask.shapes.first(), Some(MaskShape::Brush(_))))
                {
                    ui.label("Brush");
                    ui.add(egui::Slider::new(&mut app.brush_radius, 0.01..=0.5).text("Size"));
                    ui.add(egui::Slider::new(&mut app.brush_feather, 0.0..=0.5).text("Feather"));
                    ui.checkbox(&mut app.brush_erase, "Erase");
                    ui.label("Drag on the image to paint.");
                }

                ui.separator();
                ui.heading("Export");
                ui.horizontal(|ui| {
                    ui.label("Path:");
                    ui.text_edit_singleline(&mut app.output);
                });
                // Disable Export while a render/export is in flight (one at a time).
                if ui
                    .add_enabled(
                        !app.render.is_busy(),
                        egui::Button::new("Export (full resolution)"),
                    )
                    .clicked()
                {
                    app.export(ctx);
                }
                if !app.status.is_empty() {
                    ui.label(&app.status);
                }
            });
        });

    dirty
}
