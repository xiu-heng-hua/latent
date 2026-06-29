//! The variant manager: a list of the open image's variants, each with a small
//! thumbnail, an editable name, and selection. A toolbar row duplicates, deletes
//! (never below one), and reorders the selected variant. Switching the active
//! variant re-renders the preview. The list and names persist through autosave;
//! the thumbnails are cached and rendered through the same transform as the
//! preview, only tiny.

use eframe::egui;

use crate::gui::app::App;

/// The pixel size a variant thumbnail is drawn at in the list.
const THUMB_DISPLAY: f32 = 48.0;

/// Show the variant manager block inside the controls panel. Returns whether the
/// active variant changed (so the caller re-renders the preview). Thumbnails are
/// ensured up to date here, cached so an unchanged variant never re-renders.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) -> bool {
    let backend = app.backend.clone();
    let Some(session) = app.session.as_mut() else {
        return false;
    };
    let mut switched = false;

    let count = session.variants.len();
    // Refresh the thumbnails (cached; only changed variants re-render).
    for i in 0..count {
        session.ensure_thumb(ui.ctx(), i, backend.as_ref());
    }

    // Reorder/duplicate/delete requests, applied after the list loop so the
    // borrow of the list is released first.
    let mut to_select: Option<usize> = None;
    let mut to_duplicate: Option<usize> = None;
    let mut to_delete: Option<usize> = None;
    let mut to_move: Option<(usize, isize)> = None;
    let mut name_edit: Option<(usize, String)> = None;

    for i in 0..count {
        let selected = i == session.active;
        let frame = egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(4));
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                // Thumbnail (clickable to select).
                if let Some(Some(thumb)) = session.thumbs.get(i) {
                    let img = egui::Image::new(&thumb.texture)
                        .fit_to_exact_size(egui::vec2(THUMB_DISPLAY, THUMB_DISPLAY))
                        .sense(egui::Sense::click());
                    if ui.add(img).clicked() {
                        to_select = Some(i);
                    }
                } else {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(THUMB_DISPLAY, THUMB_DISPLAY),
                        egui::Sense::click(),
                    );
                    ui.painter()
                        .rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
                    if resp.clicked() {
                        to_select = Some(i);
                    }
                }

                ui.vertical(|ui| {
                    // An editable name field. Edits are applied after the loop.
                    let mut name = session.names.get(i).cloned().unwrap_or_default();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text(format!("Variant {}", i + 1))
                            .desired_width(140.0),
                    );
                    if resp.changed() {
                        name_edit = Some((i, name));
                    }

                    // The active marker / select button.
                    if ui.selectable_label(selected, "Active").clicked() {
                        to_select = Some(i);
                    }
                });
            });
        });
    }

    ui.horizontal(|ui| {
        let active = session.active;
        if ui
            .button("Duplicate")
            .on_hover_text("Copy the active variant")
            .clicked()
        {
            to_duplicate = Some(active);
        }
        let can_delete = session.variants.len() > 1;
        if ui
            .add_enabled(can_delete, egui::Button::new("Delete"))
            .on_hover_text("Remove the active variant")
            .clicked()
        {
            to_delete = Some(active);
        }
        if ui
            .add_enabled(active > 0, egui::Button::new("↑"))
            .on_hover_text("Move up")
            .clicked()
        {
            to_move = Some((active, -1));
        }
        if ui
            .add_enabled(active + 1 < session.variants.len(), egui::Button::new("↓"))
            .on_hover_text("Move down")
            .clicked()
        {
            to_move = Some((active, 1));
        }
    });

    // Apply the deferred mutations. A rename is not a `Settings` edit, so it never
    // goes through History; it (like reorder/duplicate/delete) is persisted by
    // autosave on the next idle frame.
    if let Some((i, name)) = name_edit
        && let Some(slot) = session.names.get_mut(i)
    {
        *slot = name;
    }
    if let Some(i) = to_select
        && i != session.active
    {
        session.select_variant(i);
        switched = true;
    }
    if let Some(i) = to_duplicate {
        session.duplicate_variant(i);
        switched = true;
    }
    if let Some(i) = to_delete
        && session.delete_variant(i)
    {
        switched = true;
    }
    if let Some((i, delta)) = to_move
        && session.move_variant(i, delta)
    {
        switched = true;
    }

    switched
}
