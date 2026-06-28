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
    // Subsection show/hide toggles to persist after the panel closure, collected
    // the same way as the section open-state toggles so neither write borrows
    // `app` while the closure still holds it.
    let mut vis_toggles: Vec<(&'static str, bool)> = Vec::new();

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
                let subsections_shown = &app.config.subsections_shown;
                let session = app.session.as_mut().expect("session present");
                // The per-frame visibility context threaded into each section body:
                // the persisted show/hide map (read) and the toggles to write back.
                let mut vis = VisCtx {
                    shown: subsections_shown,
                    toggles: &mut vis_toggles,
                };
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
                    &mut vis,
                    SectionId::Basic,
                    |ui, s, _vis| {
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
                    &mut vis,
                    SectionId::Tone,
                    |ui, s, _vis| {
                        widgets::tone_block(ui, &mut s.variants[s.active], widgets::GlobalAccess)
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    &mut vis,
                    SectionId::Color,
                    |ui, s, vis| {
                        let access = widgets::GlobalAccess;
                        let h = &mut s.variants[s.active];
                        let mut d = false;
                        // Saturation is an always-on continuous basic — a plain slider.
                        d |= widgets::opt_point_slider(
                            ui,
                            h,
                            widgets::SliderSpec {
                                label: "Saturation",
                                range: 0.0..=2.0,
                                neutral: 1.0,
                                help: "Color intensity",
                            },
                            |st| st.global.saturation,
                            |st, v| st.global.saturation = v,
                        );
                        let (hsl_on, mixer_on) = {
                            let g = &h.current().global;
                            (g.hsl.is_some(), g.channel_mixer.is_some())
                        };
                        d |= toggle_subsection(
                            ui,
                            vis,
                            "hsl_mixer",
                            h,
                            "HSL mixer",
                            hsl_on,
                            |h, on| widgets::set_hsl_enabled(h, access, on),
                            |ui, h| widgets::hsl_body(ui, h, access),
                        );
                        d |= toggle_subsection(
                            ui,
                            vis,
                            "channel_mixer",
                            h,
                            "Channel mixer",
                            mixer_on,
                            |h, on| widgets::set_channel_mixer_enabled(h, access, on),
                            |ui, h| widgets::channel_mixer_body(ui, h, access),
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
                    &mut vis,
                    SectionId::Curves,
                    |ui, s, vis| {
                        let access = widgets::GlobalAccess;
                        let channel = &mut s.curve_channel;
                        let h = &mut s.variants[s.active];
                        let curves_on = h.current().global.curves.is_some();
                        toggle_subsection(
                            ui,
                            vis,
                            "curves",
                            h,
                            "Curves",
                            curves_on,
                            |h, on| widgets::set_curves_enabled(h, access, on),
                            |ui, h| widgets::curves_body(ui, h, channel, access),
                        )
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    &mut vis,
                    SectionId::Detail,
                    |ui, s, vis| {
                        let access = widgets::GlobalAccess;
                        let h = &mut s.variants[s.active];
                        let g = &h.current().global;
                        let (sharpen_on, clarity_on, dehaze_on, nr_on) = (
                            g.sharpen.is_some(),
                            g.clarity.is_some(),
                            g.dehaze.is_some(),
                            g.noise_reduction.is_some(),
                        );
                        let mut d = false;
                        d |= toggle_subsection(
                            ui,
                            vis,
                            "sharpen",
                            h,
                            "Sharpen",
                            sharpen_on,
                            |h, on| widgets::set_sharpen_enabled(h, access, on),
                            |ui, h| widgets::sharpen_block(ui, h, access),
                        );
                        d |= toggle_subsection(
                            ui,
                            vis,
                            "clarity",
                            h,
                            "Clarity",
                            clarity_on,
                            |h, on| widgets::set_clarity_enabled(h, access, on),
                            |ui, h| widgets::clarity_block(ui, h, access),
                        );
                        d |= toggle_subsection(
                            ui,
                            vis,
                            "dehaze",
                            h,
                            "Dehaze",
                            dehaze_on,
                            |h, on| widgets::set_dehaze_enabled(h, access, on),
                            |ui, h| {
                                widgets::opt_point_slider(
                                    ui,
                                    h,
                                    widgets::SliderSpec {
                                        label: "Dehaze",
                                        range: 0.0..=1.0,
                                        neutral: 0.0,
                                        help: "Cut atmospheric haze",
                                    },
                                    |st| st.global.dehaze,
                                    |st, v| st.global.dehaze = v,
                                )
                            },
                        );
                        d |= toggle_subsection(
                            ui,
                            vis,
                            "noise_reduction",
                            h,
                            "Noise reduction",
                            nr_on,
                            |h, on| widgets::set_noise_reduction_enabled(h, access, on),
                            |ui, h| widgets::noise_reduction_block(ui, h, access),
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
                    &mut vis,
                    SectionId::Effects,
                    |ui, s, vis| {
                        let h = &mut s.variants[s.active];
                        let vignette_on = h.current().geometry.vignette.is_some();
                        toggle_subsection(
                            ui,
                            vis,
                            "vignette",
                            h,
                            "Vignette",
                            vignette_on,
                            widgets::set_vignette_enabled,
                            widgets::vignette_body,
                        )
                    },
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    &mut vis,
                    SectionId::Geometry,
                    geometry_body,
                );

                dirty |= section(
                    ui,
                    ctx,
                    session,
                    sections_open,
                    &mut toggles,
                    &mut vis,
                    SectionId::Masks,
                    |ui, s, _vis| {
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

    // Persist any subsection show/hide toggles the user made this frame, keyed by
    // the stable subsection id (never the display label) — the same shape as the
    // section open-state persistence above.
    for (key, shown) in vis_toggles {
        let changed = app.config.subsections_shown.get(key) != Some(&shown);
        if changed {
            app.config.subsections_shown.insert(key.to_owned(), shown);
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
#[allow(clippy::too_many_arguments)]
fn section(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    session: &mut Session,
    sections_open: &std::collections::BTreeMap<String, bool>,
    toggles: &mut Vec<(&'static str, bool)>,
    vis: &mut VisCtx,
    id: SectionId,
    body: impl FnOnce(&mut egui::Ui, &mut Session, &mut VisCtx) -> bool,
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
                // The reset button is enabled only when the section differs from
                // its defaults, so its enabled state is itself the "modified"
                // indicator — no separate dot that would shift the header.
                if crate::gui::icons::icon_button(ui, modified, "undo", "Reset this section")
                    .clicked()
                {
                    reset_section(&mut session.variants[session.active], id);
                    dirty = true;
                }
            });
        })
        .body(|ui| {
            dirty |= body(ui, session, vis);
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

/// The per-frame visibility context threaded into each section body: the persisted
/// subsection show/hide map (read-only) plus the list of changes to write back
/// after the panel closure. Mirrors how the section open-state map and its toggle
/// list are threaded, so neither persistence path borrows `app` while the panel
/// closure still holds it.
struct VisCtx<'a> {
    /// The persisted show/hide state, keyed by a stable subsection id (never the
    /// display label). A missing id means shown (the eye-open default).
    shown: &'a std::collections::BTreeMap<String, bool>,
    /// Show/hide changes made this frame, for the caller to persist.
    toggles: &'a mut Vec<(&'static str, bool)>,
}

impl VisCtx<'_> {
    /// Whether the subsection `id` is currently shown (eye open). Defaults to
    /// shown when nothing is persisted yet.
    fn is_shown(&self, id: &str) -> bool {
        self.shown.get(id).copied().unwrap_or(true)
    }

    /// Render the eye / eye-off button at the right edge of a subsection header and
    /// record any change for the caller to persist. `shown` is the current state;
    /// returns the (possibly flipped) state to use this frame. Pure UI — toggling
    /// it never touches the image. Styled like the per-section reset icon-button.
    fn eye_button(&mut self, ui: &mut egui::Ui, id: &'static str, shown: bool) -> bool {
        let (name, tip) = if shown {
            ("eye", "Hide these controls")
        } else {
            ("eye_off", "Show these controls")
        };
        let mut now = shown;
        if crate::gui::icons::icon_button(ui, true, name, tip).clicked() {
            now = !shown;
            self.toggles.push((id, now));
        }
        now
    }
}

/// A toggleable subgroup bound to one history: the header is `[checkbox] Label`
/// with an eye / eye-off button at the right that shows or hides the body. The
/// checkbox owns the subsection's **enable** — flipping it calls
/// `set_enabled(history, now)`, where the caller flips the underlying field as
/// **one** undo step. The eye button owns the body's **visibility** — pure UI,
/// no image effect — and is independent of the enable: the body is rendered
/// whenever the subsection is shown, and when shown-but-disabled it is greyed and
/// non-interactive (via [`egui::Ui::add_enabled_ui`]) so toggling the checkbox
/// never changes the panel layout. The body (`body(ui, history)`) renders the
/// controls and returns its own dirty flag; the whole call returns whether the
/// body *or* the toggle dirtied the preview. Threading the single `history`
/// through both closures (rather than letting each capture it) keeps the borrow
/// checker happy — the same shape [`toggle_tool_subsection`] uses for the session.
#[allow(clippy::too_many_arguments)]
fn toggle_subsection(
    ui: &mut egui::Ui,
    vis: &mut VisCtx,
    id: &'static str,
    history: &mut History<Settings>,
    label: &str,
    enabled: bool,
    set_enabled: impl FnOnce(&mut History<Settings>, bool) -> bool,
    body: impl FnOnce(&mut egui::Ui, &mut History<Settings>) -> bool,
) -> bool {
    ui.add_space(2.0);
    let mut on = enabled;
    let mut dirty = false;
    let shown = vis.is_shown(id);
    let shown = ui
        .horizontal(|ui| {
            if ui.checkbox(&mut on, label).changed() {
                dirty |= set_enabled(history, on);
            }
            // The eye button sits at the right edge of the header row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                vis.eye_button(ui, id, shown)
            })
            .inner
        })
        .inner;
    if shown {
        dirty |= ui
            .indent(label, |ui| {
                // Disabled-but-shown reads as greyed and non-interactive; nothing
                // in the layout moves when the enable checkbox is toggled.
                ui.add_enabled_ui(on, |ui| body(ui, history)).inner
            })
            .inner;
    }
    dirty
}

/// A toggleable subgroup whose header also carries a canvas-tool activation icon
/// plus the eye / eye-off visibility button:
/// `[checkbox] Label …………… [tool icon] [eye]`. The checkbox owns the **enable**
/// (flipping it calls `on_toggle(now)`, one undo step in the caller); the tool icon
/// sits at the right edge of the same row and reuses the single tool-activation
/// path ([`Session::set_tool`]) — clicking it switches to `tool`, clicking it while
/// already active toggles back to the plain view. The eye button (rightmost) owns the body's
/// **visibility** — pure UI, no image effect — independent of the enable: the body
/// is rendered whenever the subsection is shown, and when shown-but-disabled it is
/// greyed and non-interactive so toggling the checkbox never moves the layout.
/// Returns whether the body or the toggle dirtied the preview.
#[allow(clippy::too_many_arguments)]
fn toggle_tool_subsection(
    session: &mut Session,
    ui: &mut egui::Ui,
    vis: &mut VisCtx,
    id: &'static str,
    label: &str,
    tool: crate::gui::tools::CanvasTool,
    icon: &str,
    enabled: bool,
    on_toggle: impl FnOnce(&mut Session, bool) -> bool,
    body: impl FnOnce(&mut Session, &mut egui::Ui) -> bool,
) -> bool {
    ui.add_space(2.0);
    let mut on = enabled;
    let mut dirty = false;
    let shown = vis.is_shown(id);
    let shown = ui
        .horizontal(|ui| {
            if ui.checkbox(&mut on, label).changed() {
                dirty |= on_toggle(session, on);
            }
            // Push the tool activator and eye button to the right edge of the row.
            // In a right-to-left layout the eye is laid out first so it lands at the
            // far right, next to the tool icon.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let now_shown = vis.eye_button(ui, id, shown);
                let active = session.tool == tool;
                let resp =
                    crate::gui::icons::selectable_icon(ui, active, icon, "Edit this on the image");
                if resp.clicked() {
                    // Re-clicking the active tool returns to the plain view.
                    let next = if active {
                        crate::gui::tools::CanvasTool::None
                    } else {
                        tool
                    };
                    session.set_tool(next);
                }
                now_shown
            })
            .inner
        })
        .inner;
    if shown {
        dirty |= ui
            .indent(label, |ui| {
                // Disabled-but-shown reads as greyed and non-interactive; nothing
                // in the layout moves when the enable checkbox is toggled.
                ui.add_enabled_ui(on, |ui| body(session, ui)).inner
            })
            .inner;
    }
    dirty
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::gui::icons::selectable_icon(
                    ui,
                    session.tool == CanvasTool::Brush,
                    "brush",
                    "Paint on the image",
                )
                .clicked()
                {
                    session.set_tool(CanvasTool::Brush);
                }
            });
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::gui::icons::selectable_icon(
                    ui,
                    active_shape,
                    "mask",
                    "Edit the shape on the image",
                )
                .clicked()
                {
                    session.set_tool(CanvasTool::MaskShape);
                }
            });
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

/// The lens-correction subsection: a single checkbox header `[checkbox] Lens
/// correction` (the enable) with the eye / eye-off visibility button at the right,
/// styled like the section reset icon-button. `geometry.lens` is off by default.
/// Enabling detects a profile from the RAW's EXIF on the main thread (the lensfun
/// `Database` is not `Send`, so it never crosses the render worker) and applies it
/// — or reports that none was found and leaves the checkbox off. Disabling clears
/// the correction. The detected-lens name (or a "none found" note) shows as a small
/// indented body label whenever the subsection is shown; while disabled it reads
/// greyed and non-interactive so the layout does not move when the box is toggled.
/// Returns whether the preview is now dirty.
fn lens_block(session: &mut Session, ui: &mut egui::Ui, vis: &mut VisCtx) -> bool {
    let active = session.active;
    let mut dirty = false;
    let mut enabled = session.variants[active].current().geometry.lens.is_some();
    ui.add_space(2.0);
    let shown = vis.is_shown("lens");
    let shown = ui
        .horizontal(|ui| {
            let changed = ui
                .checkbox(&mut enabled, "Lens correction")
                .on_hover_text("Correct lens distortion/vignetting from the lens profile")
                .changed();
            if changed {
                if enabled {
                    // Detect synchronously on the main thread (a one-shot lookup,
                    // never a per-frame cost) and apply when a profile is found.
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
            // The eye button sits at the right edge of the header row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                vis.eye_button(ui, "lens", shown)
            })
            .inner
        })
        .inner;

    // Show the detected-lens name (or a "none found" note) as the small body
    // whenever shown. When disabled it greys out, so toggling the box never moves
    // the layout.
    if shown {
        let on = session.variants[active].current().geometry.lens.is_some();
        let name = if on {
            session
                .lens_name
                .clone()
                .unwrap_or_else(|| "Lens profile applied".to_owned())
        } else {
            // No profile applied — the user has not enabled it, or nothing matched.
            "No lens profile found".to_owned()
        };
        ui.indent("lens", |ui| {
            ui.add_enabled_ui(on, |ui| ui.label(name));
        });
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

/// The Geometry section body, grouped into per-purpose subsections — Cropping,
/// Straighten, Keystone, and Lens — each carrying the on-canvas tool that edits it
/// at its header. Aspect ratio lives in Cropping (it constrains the crop). The
/// shared auto-constrain toggle, which trims the wedges either a straighten or a
/// keystone leaves, sits at the foot of the section since it spans both. Returns
/// whether any control marked the preview dirty.
fn geometry_body(ui: &mut egui::Ui, session: &mut Session, vis: &mut VisCtx) -> bool {
    use crate::gui::tools::CanvasTool;
    let mut d = false;

    // Cropping — a header checkbox (with the crop tool icon), its aspect-ratio
    // constraint, and the edge sliders. The enable checkbox is itself the
    // "this image is cropped" signal, so no separate indicator is needed.
    let crop_enabled = session.crop_enabled;
    d |= toggle_tool_subsection(
        session,
        ui,
        vis,
        "cropping",
        "Cropping",
        CanvasTool::Crop,
        "crop",
        crop_enabled,
        set_crop_enabled,
        |session, ui| {
            crop_aspect_row(session, ui);
            widgets::crop_block(ui, &mut session.variants[session.active])
        },
    );

    // Straighten — the level tool and its angle.
    let straighten_enabled = session.straighten_enabled;
    d |= toggle_tool_subsection(
        session,
        ui,
        vis,
        "straighten",
        "Straighten",
        CanvasTool::Straighten,
        "straighten",
        straighten_enabled,
        set_straighten_enabled,
        |session, ui| widgets::straighten_slider(ui, &mut session.variants[session.active]),
    );

    // Keystone — the perspective tool and its two correction axes.
    let keystone_enabled = session.keystone_enabled;
    d |= toggle_tool_subsection(
        session,
        ui,
        vis,
        "keystone",
        "Keystone",
        CanvasTool::Keystone,
        "keystone",
        keystone_enabled,
        set_keystone_enabled,
        |session, ui| widgets::keystone_block(ui, &mut session.variants[session.active]),
    );

    // Lens correction — a single-checkbox subsection (no nested enable): the header
    // checkbox is the enable, the detected-lens name shows as a small body label.
    d |= lens_block(session, ui, vis);

    // The auto-constrain toggle spans straighten and keystone, so it stays a
    // section-level control rather than living in either subsection.
    d |= auto_constrain_row(session, ui);
    d
}

/// Enable/disable the crop subsection, keeping the [`Session::crop_enabled`] UI
/// flag in sync with the develop history. The stash/restore round-trip lives in
/// [`toggle_crop`]; this wrapper only mirrors the flag onto the session. Returns
/// whether the preview is now dirty.
fn set_crop_enabled(session: &mut Session, on: bool) -> bool {
    session.crop_enabled = on;
    let history = &mut session.variants[session.active];
    toggle_crop(history, &mut session.crop_stash, on)
}

/// Enable/disable the straighten subsection, mirroring [`Session::straighten_enabled`].
/// The stash/restore lives in [`toggle_straighten`]. Returns dirty.
fn set_straighten_enabled(session: &mut Session, on: bool) -> bool {
    session.straighten_enabled = on;
    let history = &mut session.variants[session.active];
    toggle_straighten(history, &mut session.straighten_stash, on)
}

/// Enable/disable the keystone subsection, mirroring [`Session::keystone_enabled`].
/// The stash/restore lives in [`toggle_keystone`]. Returns dirty.
fn set_keystone_enabled(session: &mut Session, on: bool) -> bool {
    session.keystone_enabled = on;
    let history = &mut session.variants[session.active];
    toggle_keystone(history, &mut session.keystone_stash, on)
}

/// Toggle the crop field on/off as **one** undo step, stashing through `stash` so a
/// toggle is non-destructive within the session. Disabling stashes the current
/// crop and clears the field (the render then shows the full frame); enabling
/// restores the stash (or leaves the full frame when nothing was stashed, so the
/// edge sliders open at `{0,0,1,1}`). The stash is UI state, not history, so it is
/// passed in rather than recorded in a step. Returns whether the field changed.
fn toggle_crop(
    history: &mut History<Settings>,
    stash: &mut Option<latent_edit::Crop>,
    on: bool,
) -> bool {
    if on {
        match stash.take() {
            Some(crop) => {
                history.begin();
                history.current_mut().geometry.crop = Some(crop);
                history.commit();
                true
            }
            None => false,
        }
    } else {
        *stash = history.current().geometry.crop;
        if stash.is_none() {
            return false;
        }
        history.begin();
        history.current_mut().geometry.crop = None;
        history.commit();
        true
    }
}

/// Toggle the straighten angle on/off as one undo step, stashing through `stash`.
/// Disabling stashes the angle and levels it to `0`; enabling restores the stashed
/// angle (a `0` or absent stash is a no-op). Returns whether the angle changed.
fn toggle_straighten(history: &mut History<Settings>, stash: &mut Option<f32>, on: bool) -> bool {
    if on {
        match stash.take() {
            Some(deg) if deg != 0.0 => {
                history.begin();
                history.current_mut().geometry.straighten_degrees = deg;
                history.commit();
                true
            }
            _ => false,
        }
    } else {
        let deg = history.current().geometry.straighten_degrees;
        *stash = Some(deg);
        if deg == 0.0 {
            return false;
        }
        history.begin();
        history.current_mut().geometry.straighten_degrees = 0.0;
        history.commit();
        true
    }
}

/// Toggle the keystone perspective on/off as one undo step, stashing through
/// `stash`. Disabling stashes the perspective and clears it; enabling restores the
/// stash. Returns whether the field changed.
fn toggle_keystone(
    history: &mut History<Settings>,
    stash: &mut Option<latent_edit::Perspective>,
    on: bool,
) -> bool {
    if on {
        match stash.take() {
            Some(p) => {
                history.begin();
                history.current_mut().geometry.perspective = Some(p);
                history.commit();
                true
            }
            None => false,
        }
    } else {
        *stash = history.current().geometry.perspective;
        if stash.is_none() {
            return false;
        }
        history.begin();
        history.current_mut().geometry.perspective = None;
        history.commit();
        true
    }
}

/// Seed each toggleable geometry transform's enable flag from the loaded settings:
/// a transform present in the sidecar opens enabled, an absent one disabled. The
/// single place the seeding rule lives, so [`Session::from_data`] and the test
/// share one definition. Returns `(crop, straighten, keystone)` enabled flags.
pub(crate) fn geometry_enabled_from(geometry: &latent_edit::Geometry) -> (bool, bool, bool) {
    (
        geometry.crop.is_some(),
        geometry.straighten_degrees != 0.0,
        geometry.perspective.is_some(),
    )
}

/// The auto-constrain toggle: trim the straighten/keystone border wedges to the
/// largest valid rectangle. On by default; toggling it is one undo step.
fn auto_constrain_row(session: &mut Session, ui: &mut egui::Ui) -> bool {
    let active = session.active;
    let mut on = session.variants[active].current().geometry.auto_constrain;
    let resp = ui
        .checkbox(&mut on, "Auto-constrain")
        .on_hover_text("Trim the black border wedges a straighten or keystone leaves");
    if resp.changed() {
        let history = &mut session.variants[active];
        history.begin();
        history.current_mut().geometry.auto_constrain = on;
        history.commit();
        return true;
    }
    false
}

/// The crop aspect-ratio presets. The selected ratio constrains the rectangle
/// directly; `Free` leaves it unconstrained (no separate lock).
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{Crop, Geometry, Perspective, Settings};

    /// A settings value carrying every toggleable geometry transform at a non-off
    /// value, for the seeding and stash/restore round-trip tests.
    fn settings_with_all_geometry() -> Settings {
        Settings {
            geometry: Geometry {
                crop: Some(Crop {
                    x: 0.1,
                    y: 0.1,
                    width: 0.8,
                    height: 0.8,
                }),
                straighten_degrees: 5.0,
                perspective: Some(Perspective {
                    vertical: 0.2,
                    horizontal: 0.0,
                }),
                ..Geometry::default()
            },
            ..Settings::default()
        }
    }

    #[test]
    fn geometry_flags_seed_from_loaded_settings() {
        // A sidecar with all three transforms set opens with all three enabled.
        let (c, s, k) = geometry_enabled_from(&settings_with_all_geometry().geometry);
        assert!(c && s && k, "all present transforms seed enabled");
        // A neutral (default) geometry opens with all three disabled.
        let (c, s, k) = geometry_enabled_from(&Geometry::default());
        assert!(!c && !s && !k, "absent transforms seed disabled");
        // Each transform seeds independently of the others.
        let only_crop = Geometry {
            crop: Some(Crop {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            }),
            ..Geometry::default()
        };
        assert_eq!(geometry_enabled_from(&only_crop), (true, false, false));
    }

    #[test]
    fn toggle_crop_stash_restore_is_non_destructive() {
        let mut h = History::new(settings_with_all_geometry());
        let crop = h.current().geometry.crop;
        let mut stash = None;
        // Disabling stashes the crop and clears the field as one undo step.
        assert!(toggle_crop(&mut h, &mut stash, false));
        assert_eq!(h.current().geometry.crop, None, "field cleared");
        assert_eq!(stash, crop, "value stashed");
        assert_eq!(h.undo_len(), 1, "disabling is one undo step");
        // Re-enabling restores the exact stashed value, also one step.
        assert!(toggle_crop(&mut h, &mut stash, true));
        assert_eq!(h.current().geometry.crop, crop, "value restored");
        assert_eq!(stash, None, "stash consumed");
        assert_eq!(h.undo_len(), 2, "enabling is a second undo step");
    }

    #[test]
    fn toggle_straighten_stash_restore_round_trips() {
        let mut h = History::new(settings_with_all_geometry());
        let mut stash = None;
        assert!(toggle_straighten(&mut h, &mut stash, false));
        assert_eq!(h.current().geometry.straighten_degrees, 0.0, "leveled");
        assert_eq!(stash, Some(5.0), "angle stashed");
        assert!(toggle_straighten(&mut h, &mut stash, true));
        assert_eq!(h.current().geometry.straighten_degrees, 5.0, "restored");
        assert_eq!(stash, None);
    }

    #[test]
    fn toggle_keystone_stash_restore_round_trips() {
        let mut h = History::new(settings_with_all_geometry());
        let p = h.current().geometry.perspective;
        let mut stash = None;
        assert!(toggle_keystone(&mut h, &mut stash, false));
        assert_eq!(h.current().geometry.perspective, None, "cleared");
        assert_eq!(stash, p, "stashed");
        assert!(toggle_keystone(&mut h, &mut stash, true));
        assert_eq!(h.current().geometry.perspective, p, "restored");
        assert_eq!(stash, None);
    }

    #[test]
    fn enabling_a_neutral_transform_records_no_step() {
        // Enabling at a neutral value (an empty stash) shows the body but changes
        // no settings, so it records no undo step — the flag alone (UI state) holds
        // the intent. Disabling a neutral straighten likewise records nothing.
        let mut h = History::new(Settings::default());
        let mut crop_stash = None;
        assert!(
            !toggle_crop(&mut h, &mut crop_stash, true),
            "no value to set"
        );
        assert_eq!(h.undo_len(), 0, "enabling at neutral records nothing");
        let mut straight_stash = None;
        assert!(!toggle_straighten(&mut h, &mut straight_stash, false));
        assert_eq!(straight_stash, Some(0.0), "neutral angle stashed");
        assert_eq!(h.undo_len(), 0, "disabling a level angle records nothing");
    }
}
