//! The right-hand controls side panel: the full stack of develop sections
//! (light, color, detail, geometry, local adjustments) plus the export row. Each
//! section delegates to a builder in [`crate::gui::widgets`]; the panel only
//! wires them to the active variant and folds their `dirty` flags. Shown only with
//! an open session.

use eframe::egui;
use latent_edit::MaskShape;

use crate::gui::app::{App, Session};
use crate::gui::dialogs::ExportFormat;
use crate::gui::theme;
use crate::gui::widgets;

/// Show the controls panel and return whether the preview needs a redraw.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) -> bool {
    // Nothing to show without an open image.
    if app.session.is_none() {
        return false;
    }
    let mut dirty = false;
    // Whether the user asked to export this frame (handled after the panel closure
    // so the session borrow is released first).
    let mut do_export = false;

    let frame = egui::Frame::side_top_panel(&ctx.style())
        .inner_margin(egui::Margin::same(theme::PANEL_MARGIN));
    // Restore the persisted panel width when present, else the default.
    let default_width = app
        .config
        .side_panel_width
        .unwrap_or(theme::SIDE_PANEL_WIDTH);
    let panel = egui::SidePanel::right("controls")
        .resizable(true)
        .default_width(default_width)
        .frame(frame)
        .show(ctx, |ui| {
            let session = app.session.as_mut().expect("session present");
            let active = session.active;
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Light");
                dirty |= widgets::opt_point_slider(
                    ui,
                    &mut session.variants[active],
                    "Exposure (EV)",
                    -5.0..=5.0,
                    0.0,
                    |s| s.global.exposure,
                    |s, v| s.global.exposure = v,
                );
                dirty |= widgets::tone_block(ui, &mut session.variants[active]);

                ui.separator();
                ui.heading("Color");
                dirty |= widgets::white_balance_block(ui, &mut session.variants[active]);
                dirty |= widgets::opt_point_slider(
                    ui,
                    &mut session.variants[active],
                    "Saturation",
                    0.0..=2.0,
                    1.0,
                    |s| s.global.saturation,
                    |s, v| s.global.saturation = v,
                );
                dirty |= widgets::curves_block(
                    ui,
                    &mut session.variants[active],
                    &mut session.curve_channel,
                );

                ui.separator();
                ui.heading("Detail");
                dirty |= widgets::sharpen_block(ui, &mut session.variants[active]);
                dirty |= widgets::clarity_block(ui, &mut session.variants[active]);
                dirty |= widgets::opt_point_slider(
                    ui,
                    &mut session.variants[active],
                    "Dehaze",
                    0.0..=1.0,
                    0.0,
                    |s| s.global.dehaze,
                    |s, v| s.global.dehaze = v,
                );
                dirty |= widgets::noise_reduction_block(ui, &mut session.variants[active]);

                ui.separator();
                ui.heading("Geometry");
                geometry_tools(session, ui);
                dirty |= widgets::straighten_slider(ui, &mut session.variants[active]);
                dirty |= widgets::keystone_block(ui, &mut session.variants[active]);
                crop_aspect_row(session, ui);
                dirty |= widgets::crop_block(ui, &mut session.variants[active]);
                dirty |= widgets::vignette_slider(ui, &mut session.variants[active]);

                ui.separator();
                ui.heading("Local Adjustments");
                dirty |= widgets::local_adjustments(
                    ui,
                    &mut session.variants[active],
                    &mut session.local_sel,
                );
                local_tool_row(session, ui);

                ui.separator();
                ui.heading("Export");
                export_section(session, ui, &mut do_export);
            });
        });

    // Persist the panel width when the user resizes it (debounced to ±1px so a
    // resize drag doesn't write the config every frame).
    let width = panel.response.rect.width();
    let changed = match app.config.side_panel_width {
        Some(w) => (w - width).abs() > 1.0,
        None => (width - theme::SIDE_PANEL_WIDTH).abs() > 1.0,
    };
    if changed {
        app.config.side_panel_width = Some(width);
        app.save_config();
    }

    if do_export && !app.render.is_busy() {
        app.export_via_dialog(ctx);
    }

    dirty
}

/// The local-adjustment tool row: the brush/shape activator and brush sliders that
/// follow the selected local's first shape.
fn local_tool_row(session: &mut Session, ui: &mut egui::Ui) {
    use crate::gui::tools::CanvasTool;
    let shape = session.variants[session.active]
        .current()
        .locals
        .get(session.local_sel)
        .and_then(|l| l.mask.shapes.first().cloned());
    match shape {
        Some(MaskShape::Brush(_)) => {
            if ui
                .selectable_label(session.tool == CanvasTool::Brush, "Paint on canvas")
                .clicked()
            {
                session.tool = CanvasTool::Brush;
            }
            ui.add(egui::Slider::new(&mut session.brush_radius, 0.01..=0.5).text("Size"));
            ui.add(egui::Slider::new(&mut session.brush_feather, 0.0..=0.5).text("Feather"));
            ui.checkbox(&mut session.brush_erase, "Erase");
            ui.label("Drag on the image to paint. [ ] resize, Shift for feather.");
        }
        Some(MaskShape::Gradient(_) | MaskShape::Radial(_)) => {
            let active_shape = session.tool == CanvasTool::MaskShape;
            if ui
                .selectable_label(active_shape, "Edit shape on canvas")
                .clicked()
            {
                session.tool = CanvasTool::MaskShape;
            }
        }
        _ => {}
    }
}

/// The export section: the format/depth/quality chooser and the Export button.
/// The bare path field is gone — the destination is chosen in a native Save
/// dialog when Export is clicked. Sets `do_export` rather than calling the app
/// directly (the session borrow is released by the caller first).
fn export_section(session: &mut Session, ui: &mut egui::Ui, do_export: &mut bool) {
    use latent_export::Depth;

    // Format chooser.
    ui.horizontal(|ui| {
        ui.label("Format:");
        for format in ExportFormat::ALL {
            if ui
                .selectable_label(session.export.format == format, format.label())
                .clicked()
            {
                session.export.set_format(format);
            }
        }
    });

    // Bit depth, constrained to what the chosen format can encode.
    ui.horizontal(|ui| {
        ui.label("Depth:");
        for (depth, label) in [(Depth::Eight, "8-bit"), (Depth::Sixteen, "16-bit")] {
            let supported = session.export.format.supports(depth);
            ui.add_enabled_ui(supported, |ui| {
                if ui
                    .selectable_label(session.export.depth == depth, label)
                    .clicked()
                {
                    session.export.depth = depth;
                }
            });
        }
    });

    // JPEG quality, shown only for JPEG.
    if session.export.format.has_quality() {
        ui.add(egui::Slider::new(&mut session.export.quality, 1..=100).text("Quality"));
    }

    if ui.button("Export…").clicked() {
        *do_export = true;
    }
}

/// The geometry tool activators: selectable labels that switch the canvas to the
/// crop / level / keystone tool so the handles appear.
fn geometry_tools(session: &mut Session, ui: &mut egui::Ui) {
    use crate::gui::tools::CanvasTool;
    ui.horizontal(|ui| {
        for (tool, label) in [
            (CanvasTool::Crop, "Crop"),
            (CanvasTool::Straighten, "Level"),
            (CanvasTool::Keystone, "Keystone"),
        ] {
            if ui.selectable_label(session.tool == tool, label).clicked() {
                session.tool = if session.tool == tool {
                    CanvasTool::None
                } else {
                    tool
                };
            }
        }
    });
}

/// The crop aspect-ratio presets + lock toggle.
fn crop_aspect_row(session: &mut Session, ui: &mut egui::Ui) {
    use crate::gui::tools::crop;
    let active = session.active;
    let image_aspect = session.displayed_aspect();
    ui.horizontal_wrapped(|ui| {
        ui.label("Aspect:");
        for (ratio, label) in crop::AspectRatio::ALL {
            if ui
                .selectable_label(session.crop_aspect == ratio, label)
                .clicked()
            {
                session.crop_aspect = ratio;
                // Re-fit an existing crop to the newly-chosen ratio.
                if let Some(r) = ratio.visual_ratio(image_aspect) {
                    let current = crop::current_crop(session.variants[active].current());
                    let history = &mut session.variants[active];
                    history.begin();
                    let refit = crop::refit_to_ratio(current, r, image_aspect);
                    crop::write_crop(history, refit);
                    history.commit();
                }
            }
        }
        ui.checkbox(&mut session.crop_aspect_locked, "Lock");
    });
}
