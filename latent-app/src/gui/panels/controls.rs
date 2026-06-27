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
                geometry_tools(app, ui);
                dirty |= widgets::straighten_slider(ui, &mut app.variants[active]);
                dirty |= widgets::keystone_block(ui, &mut app.variants[active]);
                crop_aspect_row(app, ui);
                dirty |= widgets::crop_block(ui, &mut app.variants[active]);
                dirty |= widgets::vignette_slider(ui, &mut app.variants[active]);

                ui.separator();
                ui.heading("Local Adjustments");
                dirty |=
                    widgets::local_adjustments(ui, &mut app.variants[active], &mut app.local_sel);
                // The kind of the selected local's first shape drives which canvas
                // tool its handles belong to.
                let shape = app.variants[active]
                    .current()
                    .locals
                    .get(app.local_sel)
                    .and_then(|l| l.mask.shapes.first().cloned());
                use crate::gui::tools::CanvasTool;
                match shape {
                    Some(MaskShape::Brush(_)) => {
                        // Brush tool: only when the selected local is a brush mask.
                        // Dabs are painted on the image using these settings.
                        if ui
                            .selectable_label(app.tool == CanvasTool::Brush, "Paint on canvas")
                            .clicked()
                        {
                            app.tool = CanvasTool::Brush;
                        }
                        ui.add(egui::Slider::new(&mut app.brush_radius, 0.01..=0.5).text("Size"));
                        ui.add(
                            egui::Slider::new(&mut app.brush_feather, 0.0..=0.5).text("Feather"),
                        );
                        ui.checkbox(&mut app.brush_erase, "Erase");
                        ui.label("Drag on the image to paint. [ ] resize, Shift for feather.");
                    }
                    Some(MaskShape::Gradient(_) | MaskShape::Radial(_)) => {
                        // Gradient/radial shapes get on-canvas handles.
                        let active_shape = app.tool == CanvasTool::MaskShape;
                        let clicked = ui
                            .selectable_label(active_shape, "Edit shape on canvas")
                            .clicked();
                        if clicked {
                            app.tool = CanvasTool::MaskShape;
                        }
                    }
                    _ => {}
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

/// The geometry tool activators: selectable labels that switch the canvas to the
/// crop / level / keystone tool so the handles appear. The numeric sliders below
/// stay as a fallback (handles primary, numbers kept).
fn geometry_tools(app: &mut App, ui: &mut egui::Ui) {
    use crate::gui::tools::CanvasTool;
    ui.horizontal(|ui| {
        for (tool, label) in [
            (CanvasTool::Crop, "Crop"),
            (CanvasTool::Straighten, "Level"),
            (CanvasTool::Keystone, "Keystone"),
        ] {
            if ui.selectable_label(app.tool == tool, label).clicked() {
                app.tool = if app.tool == tool {
                    CanvasTool::None
                } else {
                    tool
                };
            }
        }
    });
}

/// The crop aspect-ratio presets + lock toggle. Picking a preset while a crop
/// exists re-fits it to the ratio (centered, clamped); the lock holds the ratio
/// while dragging. Handles are primary; the numeric crop fields remain below.
fn crop_aspect_row(app: &mut App, ui: &mut egui::Ui) {
    use crate::gui::tools::crop;
    let active = app.active;
    let image_aspect = app.displayed_aspect();
    ui.horizontal_wrapped(|ui| {
        ui.label("Aspect:");
        for (ratio, label) in crop::AspectRatio::ALL {
            if ui
                .selectable_label(app.crop_aspect == ratio, label)
                .clicked()
            {
                app.crop_aspect = ratio;
                // Re-fit an existing crop to the newly-chosen ratio.
                if let Some(r) = ratio.visual_ratio(image_aspect) {
                    let current = crop::current_crop(app.variants[active].current());
                    let history = &mut app.variants[active];
                    history.begin();
                    let refit = crop::refit_to_ratio(current, r, image_aspect);
                    crop::write_crop(history, refit);
                    history.commit();
                }
            }
        }
        ui.checkbox(&mut app.crop_aspect_locked, "Lock");
    });
}
