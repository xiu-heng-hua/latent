//! The history panel: a clickable list of the active variant's edit steps with
//! the current position marked. Clicking a step navigates straight to it — a run
//! of the same `undo`/`redo` the toolbar buttons and `Ctrl+Z` use, so a panel
//! click and a keyboard undo converge on the same history state. A jump sets the
//! preview dirty so it re-renders. The panel always reflects the *active*
//! variant's history, so switching variants re-targets it with no extra state.

use eframe::egui;
use egui::{Color32, Sense, StrokeKind, TextStyle};

use crate::gui::app::App;
use crate::gui::theme;

/// Horizontal padding inside a step card (screen px), left and right of the label.
const CARD_PAD_X: f32 = 10.0;
/// Vertical padding inside a step card (screen px), above and below the label.
const CARD_PAD_Y: f32 = 6.0;

/// Show the history list. Returns whether a jump moved the position (so the caller
/// re-renders the preview). The steps are full snapshots with no stored action
/// name, so each step's label is derived by diffing it against the step before it
/// (naming the tool and, for single-value controls, its value); the original is
/// marked "Open". The current position is highlighted, and steps ahead of it (the
/// undone branch reachable by redo) are dimmed.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) -> bool {
    let Some(session) = app.session.as_mut() else {
        return false;
    };
    let history = &mut session.variants[session.active];
    let len = history.len();
    let position = history.position();

    let mut target: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(180.0)
        // Fill the panel width (so the cards span it) but only take the height the
        // steps need, up to the cap above.
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for index in 0..len {
                // Index 0 is the original state ("Open"); later indices are labelled
                // by what changed from the step before them.
                let label = if index == 0 {
                    "Open".to_owned()
                } else {
                    match (history.snapshot(index - 1), history.snapshot(index)) {
                        (Some(prev), Some(next)) => latent_edit::describe_change(prev, next),
                        _ => format!("Step {index}"),
                    }
                };
                if step_card(ui, &label, index == position, index > position).clicked() {
                    target = Some(index);
                }
            }
        });

    if let Some(index) = target {
        // A jump is purely a run of undo/redo — the same single navigation path the
        // toolbar and keyboard use — so parity is automatic.
        return history.jump_to(index);
    }
    false
}

/// Draw one history step as a full-width card: a rounded, filled row spanning the
/// panel width. The current step carries the accent fill/stroke, a hovered step
/// brightens, and an undone step (ahead of the current position) dims so the
/// timeline reads at a glance. Returns the click response.
fn step_card(ui: &mut egui::Ui, label: &str, selected: bool, undone: bool) -> egui::Response {
    let pad = egui::vec2(CARD_PAD_X, CARD_PAD_Y);
    let width = ui.available_width();
    let font = TextStyle::Body.resolve(ui.style());
    let wrap = (width - 2.0 * pad.x).max(1.0);
    // Lay out with a placeholder color so the final color — which depends on the
    // selected/hovered/undone state resolved below — is applied at paint time
    // without laying the text out twice.
    let galley = ui
        .painter()
        .layout(label.to_owned(), font, Color32::PLACEHOLDER, wrap);
    let size = egui::vec2(width, galley.size().y + 2.0 * pad.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let hovered = response.hovered();
        let (fill, stroke) = if selected {
            (v.selection.bg_fill, v.selection.stroke)
        } else if hovered {
            (v.widgets.hovered.bg_fill, v.widgets.hovered.bg_stroke)
        } else {
            (v.widgets.inactive.bg_fill, v.widgets.inactive.bg_stroke)
        };
        let text_color = if selected || hovered {
            v.strong_text_color()
        } else if undone {
            v.weak_text_color()
        } else {
            v.widgets.inactive.fg_stroke.color
        };
        let text_pos = egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y / 2.0);

        ui.painter().rect_filled(rect, theme::CORNER_RADIUS, fill);
        ui.painter()
            .rect_stroke(rect, theme::CORNER_RADIUS, stroke, StrokeKind::Inside);
        ui.painter().galley(text_pos, galley, text_color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
