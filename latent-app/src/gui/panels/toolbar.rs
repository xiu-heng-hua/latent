//! The slim icon toolbar beneath the menu bar: undo/redo (sharing the single
//! history path with the menu and keyboard), the variant selector / new-variant
//! button, the on-canvas tool selector, the discrete rotate/flip buttons, the
//! mask-overlay toggle, the before/after toggle, and the zoom controls. Shown only
//! with an open session (the welcome state hides it).

use eframe::egui;

use crate::gui::app::App;
use crate::gui::tools::CanvasTool;
use crate::gui::tools::overlay::OverlayMode;

/// Show the toolbar. `do_undo` / `do_redo` are OR-ed with the toolbar's
/// undo/redo clicks; `dirty` is set when the active variant changes or a new
/// variant is added, so `update` re-renders.
pub(crate) fn show(
    app: &mut App,
    ctx: &egui::Context,
    do_undo: &mut bool,
    do_redo: &mut bool,
    dirty: &mut bool,
) {
    egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let Some(session) = app.session.as_mut() else {
                return;
            };
            let can_undo = session.variants[session.active].can_undo();
            let can_redo = session.variants[session.active].can_redo();
            *do_undo |=
                crate::gui::icons::icon_button(ui, can_undo, "undo", "Undo (Cmd/Ctrl+Z)").clicked();
            *do_redo |=
                crate::gui::icons::icon_button(ui, can_redo, "redo", "Redo (Cmd/Ctrl+Shift+Z)")
                    .clicked();

            ui.separator();

            ui.label("Variant:");
            for i in 0..session.variants.len() {
                let label = session.variant_label(i);
                if ui.selectable_label(i == session.active, label).clicked() {
                    session.select_variant(i);
                    *dirty = true;
                }
            }
            if ui.button("+").on_hover_text("New variant (copy)").clicked() {
                session.duplicate_variant(session.active);
                *dirty = true;
            }

            ui.separator();

            // On-canvas tool selector.
            tool_selector(session, ui);

            ui.separator();

            // Discrete orientation: rotate 90° CW/CCW and flip H/V, each one undo
            // step. They fold into the same single resample as straighten/crop.
            // The orientation edit goes through the app (so the begin/commit lives
            // in one place), so it is applied after this closure via the flag set
            // below.
            let mut orient: Option<fn(latent_edit::Orientation) -> latent_edit::Orientation> = None;
            if crate::gui::icons::icon_button(ui, true, "rotate_cw", "Rotate 90° clockwise")
                .clicked()
            {
                orient = Some(|o| o.rotate_cw());
            }
            if crate::gui::icons::icon_button(
                ui,
                true,
                "rotate_ccw",
                "Rotate 90° counter-clockwise",
            )
            .clicked()
            {
                orient = Some(|o| o.rotate_ccw());
            }
            if crate::gui::icons::icon_button(ui, true, "flip_h", "Flip horizontal").clicked() {
                orient = Some(|o| o.flip_h());
            }
            if crate::gui::icons::icon_button(ui, true, "flip_v", "Flip vertical").clicked() {
                orient = Some(|o| o.flip_v());
            }
            if let Some(f) = orient {
                // Apply directly on the session's active history (one undo step).
                let history = &mut session.variants[session.active];
                history.begin();
                let o = history.current().geometry.orientation;
                history.current_mut().geometry.orientation = f(o);
                history.commit();
                *dirty = true;
            }

            ui.separator();

            // Mask-overlay toggle (off / red wash / mask-only). Pure visualization.
            overlay_toggle(session, ui);

            ui.separator();

            // Before/after: cycle Off → Toggle → Split (also bound to `).
            let before_label = match session.before {
                crate::gui::app::BeforeAfter::Off => "After",
                crate::gui::app::BeforeAfter::Toggle => "Before",
                crate::gui::app::BeforeAfter::Split => "Split",
            };
            if ui
                .selectable_label(
                    session.before != crate::gui::app::BeforeAfter::Off,
                    before_label,
                )
                .on_hover_text("Before / after (`)")
                .clicked()
            {
                session.before = session.before.cycled();
            }

            // Zoom controls. Fit / 100% snap the intent; −/+ step the ladder. They
            // go through the app methods (session is reborrowed inside), so end the
            // session borrow first.
            let _ = session;
            ui.separator();
            if crate::gui::icons::icon_button(ui, true, "zoom_fit", "Zoom to fit (0)").clicked() {
                app.zoom_fit();
            }
            if crate::gui::icons::icon_button(ui, true, "zoom_100", "Zoom to 100% (1)").clicked() {
                app.zoom_actual();
            }
            if crate::gui::icons::icon_button(ui, true, "zoom_out", "Zoom out (−)").clicked() {
                app.zoom_step(-1);
            }
            if crate::gui::icons::icon_button(ui, true, "zoom_in", "Zoom in (+)").clicked() {
                app.zoom_step(1);
            }
            ui.label(format!("{}%", app.zoom_percent()));
        });
    });
}

/// The on-canvas tool selector: a row of selectable labels, one per tool. Only
/// the active tool draws handles and consumes the canvas pointer.
fn tool_selector(session: &mut crate::gui::app::Session, ui: &mut egui::Ui) {
    let tools = [
        (CanvasTool::None, "View"),
        (CanvasTool::Crop, "Crop"),
        (CanvasTool::Straighten, "Level"),
        (CanvasTool::Keystone, "Keystone"),
        (CanvasTool::MaskShape, "Mask"),
        (CanvasTool::Brush, "Brush"),
    ];
    for (tool, label) in tools {
        if ui.selectable_label(session.tool == tool, label).clicked() {
            // Toggle off to View when re-clicking the active tool. Route through
            // `set_tool` so the handle-tool gesture brackets (and the geometry view
            // reset) apply the same as every other tool entry point.
            let next = if session.tool == tool {
                CanvasTool::None
            } else {
                tool
            };
            session.set_tool(next);
        }
    }
}

/// The mask-overlay toggle: cycles Off → red wash → mask-only.
fn overlay_toggle(session: &mut crate::gui::app::Session, ui: &mut egui::Ui) {
    let label = match session.overlay_mode {
        OverlayMode::Off => "Mask: off",
        OverlayMode::Color => "Mask: red",
        OverlayMode::MaskOnly => "Mask: gray",
    };
    if ui
        .selectable_label(session.overlay_mode.is_on(), label)
        .on_hover_text("Show the selected mask as an overlay")
        .clicked()
    {
        session.overlay_mode = match session.overlay_mode {
            OverlayMode::Off => OverlayMode::Color,
            OverlayMode::Color => OverlayMode::MaskOnly,
            OverlayMode::MaskOnly => OverlayMode::Off,
        };
    }
}
