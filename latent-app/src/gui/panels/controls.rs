//! The right-hand controls side panel: the develop sections (Basic / Tone /
//! Color / Curves / Detail / Effects / Geometry / Masks) plus the export row,
//! laid out as collapsible [`egui::collapsing_header::CollapsingState`] groups
//! inside a [`egui::ScrollArea`]. Each section delegates to a builder in
//! [`crate::gui::widgets`]; the panel wires them to the active variant, folds
//! their `dirty` flags, and decorates each header with a per-section reset
//! affordance and a modified indicator. Section open/closed state and the panel
//! width persist through the app config. Shown only with an open session.

use eframe::egui;
use egui::collapsing_header::CollapsingState;
use latent_edit::{History, MaskShape, Settings};

use crate::gui::app::{App, Session};
use crate::gui::dialogs::ExportFormat;
use crate::gui::panels::sections::SectionId;
use crate::gui::scopes;
use crate::gui::theme;
use crate::gui::widgets;

/// Show the controls panel and return whether the preview needs a redraw.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) -> bool {
    // Nothing to show without an open image, or when the panel is hidden.
    if app.session.is_none() || !app.panel_visible {
        return false;
    }
    let mut dirty = false;
    // Whether the user asked to export this frame (handled after the panel closure
    // so the session borrow is released first).
    let mut do_export = false;
    // Section open-state toggles to persist after the panel closure (so the config
    // write does not borrow `app` while the panel closure still does).
    let mut toggles: Vec<(&'static str, bool)> = Vec::new();

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
        .width_range(theme::SIDE_PANEL_MIN_WIDTH..=theme::SIDE_PANEL_MAX_WIDTH)
        .frame(frame)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // Variant manager, history, and presets sit at the top of the panel.
                // They take `&mut App`, so they are rendered before the session is
                // borrowed for the develop sections below.
                egui::CollapsingHeader::new("Variants")
                    .default_open(false)
                    .show(ui, |ui| {
                        dirty |= crate::gui::panels::variants::show(app, ui);
                    });
                egui::CollapsingHeader::new("History")
                    .default_open(false)
                    .show(ui, |ui| {
                        dirty |= crate::gui::panels::history::show(app, ui);
                    });
                egui::CollapsingHeader::new("Presets")
                    .default_open(false)
                    .show(ui, |ui| {
                        dirty |= crate::gui::panels::presets::show(app, ui);
                    });
                ui.separator();

                // Capture the export-in-flight / busy facts before the session is
                // borrowed, so the Export button can read as in-progress and stay
                // disabled while a render/export runs.
                let exporting = app.exporting;
                let busy = app.render.is_busy();
                let sections_open = &app.config.sections_open;
                let session = app.session.as_mut().expect("session present");
                // The scopes sit above the develop sections. They only paint the
                // cached bins/overlay (computed once per preview), so this never
                // re-renders.
                scopes::scope_block(ui, &mut session.scopes);
                ui.separator();

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Basic,
                    |ui, s| {
                        let mut d = false;
                        d |= widgets::opt_point_slider(
                            ui,
                            &mut s.variants[s.active],
                            widgets::SliderSpec {
                                label: "Exposure (EV)",
                                range: -5.0..=5.0,
                                neutral: 0.0,
                                help: "Brightness in stops",
                            },
                            |st| st.global.exposure,
                            |st, v| st.global.exposure = v,
                        );
                        let mut action = widgets::WbAction::None;
                        d |= widgets::white_balance_block(
                            ui,
                            &mut s.variants[s.active],
                            widgets::GlobalAccess,
                            &mut action,
                        );
                        s.wb_action = action;
                        d
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Tone,
                    |ui, s| {
                        widgets::tone_block(ui, &mut s.variants[s.active], widgets::GlobalAccess)
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Color,
                    |ui, s| {
                        let mut d = false;
                        d |= widgets::opt_point_slider(
                            ui,
                            &mut s.variants[s.active],
                            widgets::SliderSpec {
                                label: "Saturation",
                                range: 0.0..=2.0,
                                neutral: 1.0,
                                help: "Color intensity",
                            },
                            |st| st.global.saturation,
                            |st, v| st.global.saturation = v,
                        );
                        d |= widgets::hsl_block(
                            ui,
                            &mut s.variants[s.active],
                            widgets::GlobalAccess,
                        );
                        d |= widgets::channel_mixer_block(
                            ui,
                            &mut s.variants[s.active],
                            widgets::GlobalAccess,
                        );
                        d
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Curves,
                    |ui, s| {
                        let active = s.active;
                        widgets::curves_block(
                            ui,
                            &mut s.variants[active],
                            &mut s.curve_channel,
                            widgets::GlobalAccess,
                        )
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Detail,
                    |ui, s| {
                        let mut d = false;
                        d |= widgets::sharpen_block(
                            ui,
                            &mut s.variants[s.active],
                            widgets::GlobalAccess,
                        );
                        d |= widgets::clarity_block(
                            ui,
                            &mut s.variants[s.active],
                            widgets::GlobalAccess,
                        );
                        d |= widgets::opt_point_slider(
                            ui,
                            &mut s.variants[s.active],
                            widgets::SliderSpec {
                                label: "Dehaze",
                                range: 0.0..=1.0,
                                neutral: 0.0,
                                help: "Cut atmospheric haze",
                            },
                            |st| st.global.dehaze,
                            |st, v| st.global.dehaze = v,
                        );
                        d |= widgets::noise_reduction_block(
                            ui,
                            &mut s.variants[s.active],
                            widgets::GlobalAccess,
                        );
                        d
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Effects,
                    |ui, s| widgets::vignette_slider(ui, &mut s.variants[s.active]),
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Geometry,
                    |ui, s| {
                        let mut d = false;
                        geometry_tools(s, ui);
                        d |= widgets::straighten_slider(ui, &mut s.variants[s.active]);
                        d |= widgets::keystone_block(ui, &mut s.variants[s.active]);
                        d |= lens_block(s, ui);
                        crop_aspect_row(s, ui);
                        d |= widgets::crop_block(ui, &mut s.variants[s.active]);
                        d
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    SectionId::Masks,
                    |ui, s| {
                        let mut d = false;
                        let mut action = widgets::WbAction::None;
                        d |= widgets::local_adjustments(
                            ui,
                            &mut s.variants[s.active],
                            &mut s.local_sel,
                            &mut s.shape_sel,
                            &mut action,
                        );
                        if action != widgets::WbAction::None {
                            s.wb_action = action;
                        }
                        local_tool_row(s, ui);
                        d
                    },
                );

                ui.separator();
                // → moves to a dialog in a later pass; left as-is for now.
                ui.heading("Export");
                export_section(session, ui, &mut do_export, exporting, busy);
            });
        });

    // Persist any section open/closed toggles the user made this frame.
    for (key, open) in toggles {
        let changed = app.config.sections_open.get(key) != Some(&open);
        if changed {
            app.config.sections_open.insert(key.to_owned(), open);
            app.save_config();
        }
    }

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

/// Render one collapsible section: a custom header carrying the section label, a
/// modified indicator, and a per-section reset button, then the section body. The
/// open/closed state is seeded from the persisted config (falling back to the
/// section's own default-open) and any toggle this frame is recorded in `toggles`
/// for the caller to persist. Returns whether the body marked the preview dirty.
fn section(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    session: &mut Session,
    sections_open: &std::collections::BTreeMap<String, bool>,
    toggles: &mut Vec<(&'static str, bool)>,
    id: SectionId,
    body: impl FnOnce(&mut egui::Ui, &mut Session) -> bool,
) -> bool {
    let default_open = sections_open
        .get(id.key())
        .copied()
        .unwrap_or_else(|| id.default_open());
    let modified = id.is_modified(session.variants[session.active].current());

    let state_id = ui.make_persistent_id(("controls_section", id.key()));
    let state = CollapsingState::load_with_default_open(ctx, state_id, default_open);
    let was_open = state.is_open();

    let mut dirty = false;
    let (_toggle, _header, _body) = state
        .show_header(ui, |ui| {
            ui.label(egui::RichText::new(id.label()).heading())
                .on_hover_text(id.help());
            // Push the reset/indicator to the right edge of the header row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::gui::icons::icon_button(ui, modified, "undo", "Reset this section")
                    .clicked()
                {
                    reset_section(&mut session.variants[session.active], id);
                    dirty = true;
                }
                if modified {
                    // A subtle dot on the header so a collapsed section still
                    // shows it holds edits.
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 3.0, theme::ACCENT);
                }
            });
        })
        .body(|ui| {
            dirty |= body(ui, session);
        });

    // Record an open/closed change for the caller to persist (keyed by the stable
    // section key, never the display label).
    let now_open = CollapsingState::load(ctx, state_id)
        .map(|s| s.is_open())
        .unwrap_or(was_open);
    if now_open != was_open {
        toggles.push((id.key(), now_open));
    }

    dirty
}

/// Reset exactly the section's fields to default, as **one** undo step. The
/// section owns the field set; one begin/commit brackets the whole reset, so it
/// is a single step regardless of how many fields it touched, and a section
/// already at default records nothing (the `prev != current` guard).
fn reset_section(history: &mut History<Settings>, id: SectionId) {
    history.begin();
    id.reset(history.current_mut());
    history.commit();
}

/// The local-adjustment tool row: the brush/shape activator and brush sliders that
/// follow the selected local's selected shape.
fn local_tool_row(session: &mut Session, ui: &mut egui::Ui) {
    use crate::gui::tools::CanvasTool;
    let shape_sel = session.shape_sel;
    let shape = session.variants[session.active]
        .current()
        .locals
        .get(session.local_sel)
        .and_then(|l| l.mask.shapes.get(shape_sel).cloned());
    match shape {
        Some(MaskShape::Brush(_)) => {
            if ui
                .selectable_label(session.tool == CanvasTool::Brush, "Paint on canvas")
                .clicked()
            {
                session.tool = CanvasTool::Brush;
            }
            ui.add(egui::Slider::new(&mut session.brush_radius, 0.01..=0.5).text("Size"))
                .on_hover_text("Brush radius. 0.01 … 0.5; [ ] resize");
            ui.add(egui::Slider::new(&mut session.brush_feather, 0.0..=0.5).text("Feather"))
                .on_hover_text("Brush edge softness. 0 … 0.5; Shift+[ ] resize");
            ui.checkbox(&mut session.brush_erase, "Erase")
                .on_hover_text("Subtract coverage instead of adding it");
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
/// directly (the session borrow is released by the caller first). While a job is
/// in flight the button is disabled and, for an export, reads "Exporting…" with a
/// spinner so the user sees why it's grayed rather than a dead button.
fn export_section(
    session: &mut Session,
    ui: &mut egui::Ui,
    do_export: &mut bool,
    exporting: bool,
    busy: bool,
) {
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

    if exporting {
        // Mid-export: a disabled, in-progress affordance (spinner + label) rather
        // than a dead grey button. The status bar carries the same spinner.
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(14.0));
            ui.add_enabled(false, egui::Button::new("Exporting…"));
        });
    } else if ui
        .add_enabled(!busy, egui::Button::new("Export…"))
        .clicked()
    {
        *do_export = true;
    }
}

/// The lens-correction panel: an enable checkbox over `geometry.lens`, off by
/// default. Enabling detects a profile from the RAW's EXIF on the main thread
/// (the lensfun `Database` is not `Send`, so it never crosses the render worker)
/// and applies it — or reports that none was found and leaves the checkbox off.
/// Disabling clears the correction. Returns whether the preview is now dirty.
fn lens_block(session: &mut Session, ui: &mut egui::Ui) -> bool {
    let active = session.active;
    let mut dirty = false;
    let mut enabled = session.variants[active].current().geometry.lens.is_some();
    let changed = ui
        .checkbox(&mut enabled, "Lens Corrections")
        .on_hover_text("Correct lens distortion/vignetting from the lens profile")
        .changed();
    if changed {
        if enabled {
            // Detect synchronously on the main thread (a one-shot lookup, never a
            // per-frame cost) and apply when a profile is found.
            match crate::gui::state::auto_lens_profile(&session.meta) {
                Some(profile) => {
                    let history = &mut session.variants[active];
                    history.begin();
                    history.current_mut().geometry.lens = Some(profile);
                    history.commit();
                    session.lens_name = Some(lens_display_name(&session.meta));
                    dirty = true;
                }
                None => {
                    // No match: leave the lens off and report it.
                    session.lens_name = None;
                }
            }
        } else {
            let history = &mut session.variants[active];
            history.begin();
            history.current_mut().geometry.lens = None;
            history.commit();
            session.lens_name = None;
            dirty = true;
        }
    }

    if session.variants[active].current().geometry.lens.is_some() {
        let name = session
            .lens_name
            .clone()
            .unwrap_or_else(|| "Lens profile applied".to_owned());
        ui.label(name);
    } else if enabled {
        // The user ticked the box but nothing matched (the box reads back off).
        ui.label("No lens profile found");
    }
    dirty
}

/// A display name for the detected lens: the EXIF lens model when present, else
/// the camera body. Used only for the panel label.
fn lens_display_name(meta: &latent_raw::Metadata) -> String {
    if !meta.lens.is_empty() {
        meta.lens.clone()
    } else if !meta.model.is_empty() {
        meta.model.clone()
    } else {
        "Lens profile applied".to_owned()
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
