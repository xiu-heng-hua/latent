//! The history panel: a clickable list of the active variant's edit steps with
//! the current position marked. Clicking a step navigates straight to it — a run
//! of the same `undo`/`redo` the toolbar buttons and `Ctrl+Z` use, so a panel
//! click and a keyboard undo converge on the same history state. A jump sets the
//! preview dirty so it re-renders. The panel always reflects the *active*
//! variant's history, so switching variants re-targets it with no extra state.

use eframe::egui;

use crate::gui::app::App;

/// Show the history list. Returns whether a jump moved the position (so the caller
/// re-renders the preview). The steps are full snapshots with no stored action
/// name, so each step's label is derived by diffing it against the step before it
/// (naming the tool and, for single-value controls, its value); the original is
/// marked "Open". The current position is highlighted.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) -> bool {
    let Some(session) = app.session.as_mut() else {
        return false;
    };
    let history = &mut session.variants[session.active];
    let len = history.len();
    let position = history.position();

    let mut target: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(160.0)
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
                if ui.selectable_label(index == position, label).clicked() {
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
