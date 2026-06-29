//! The history panel: a clickable list of the active variant's edit steps, each a
//! card with a thumbnail preview, a title (the tool that changed), and the detail
//! (the variable(s) and their new values). Clicking a step navigates straight to
//! it — a run of the same `undo`/`redo` the toolbar buttons and `Ctrl+Z` use, so a
//! panel click and a keyboard undo converge on the same history state. A jump sets
//! the preview dirty so it re-renders. The panel always reflects the *active*
//! variant's history, so switching variants re-targets it with no extra state.

use eframe::egui;
use egui::{Color32, Rect, Sense, StrokeKind, TextStyle};

use crate::gui::app::App;
use crate::gui::panels::history_summary::{ChangeSummary, summarize_change};
use crate::gui::theme;

/// Padding inside a step card (screen px).
const CARD_PAD: f32 = 8.0;
/// Thumbnail box size inside a card (screen px).
const THUMB_W: f32 = 56.0;
const THUMB_H: f32 = 42.0;
/// Gap between the thumbnail and the text (screen px).
const TEXT_GAP: f32 = 8.0;

/// Show the history list. Returns whether a jump moved the position (so the caller
/// re-renders the preview). Each step's title/detail is derived by diffing it
/// against the step before it; the original is titled "Open". The current position
/// is highlighted, and steps ahead of it (the undone branch reachable by redo) are
/// dimmed. Thumbnails are rendered lazily, only for the visible steps.
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) -> bool {
    let backend = app.backend.clone();
    let ctx = ui.ctx().clone();
    let Some(session) = app.session.as_mut() else {
        return false;
    };
    let active = session.active;
    let len = session.variants[active].len();
    let position = session.variants[active].position();
    session.trim_step_thumbs(len);
    let aspect =
        session.thumb_base.width().max(1) as f32 / session.thumb_base.height().max(1) as f32;

    let mut target: Option<usize> = None;
    egui::ScrollArea::vertical()
        .max_height(240.0)
        // Fill the panel width (so the cards span it) but only take the height the
        // steps need, up to the cap above.
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for index in 0..len {
                // Index 0 is the original state ("Open"); later steps are titled by
                // what changed from the step before them.
                let summary = if index == 0 {
                    ChangeSummary {
                        title: "Open".to_owned(),
                        detail: String::new(),
                    }
                } else {
                    let h = &session.variants[active];
                    match (h.snapshot(index - 1), h.snapshot(index)) {
                        (Some(prev), Some(next)) => summarize_change(prev, next),
                        _ => ChangeSummary {
                            title: format!("Step {index}"),
                            detail: String::new(),
                        },
                    }
                };
                // The step's settings drive its preview thumbnail (rendered lazily
                // inside `ensure_step_thumb`, cached by content).
                let settings = session.variants[active].snapshot(index).cloned();
                let thumb = settings
                    .as_ref()
                    .map(|s| session.ensure_step_thumb(&ctx, index, s, backend.as_ref()));
                if step_card(
                    ui,
                    thumb,
                    aspect,
                    &summary,
                    index == position,
                    index > position,
                )
                .clicked()
                {
                    target = Some(index);
                }
            }
        });

    if let Some(index) = target {
        // A jump is purely a run of undo/redo — the same single navigation path the
        // toolbar and keyboard use — so parity is automatic.
        return session.variants[active].jump_to(index);
    }
    false
}

/// Draw one history step as a full-width card: a thumbnail preview on the left, the
/// title and detail stacked on the right. The current step carries the accent
/// fill/stroke, a hovered step brightens, and an undone step (ahead of the current
/// position) dims so the timeline reads at a glance. Returns the click response.
fn step_card(
    ui: &mut egui::Ui,
    thumb: Option<egui::TextureId>,
    aspect: f32,
    summary: &ChangeSummary,
    selected: bool,
    undone: bool,
) -> egui::Response {
    let width = ui.available_width();
    let height = THUMB_H + 2.0 * CARD_PAD;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());

    if ui.is_rect_visible(rect) {
        let hovered = response.hovered();
        let (fill, stroke, title_color, detail_color, backdrop) = {
            let v = ui.visuals();
            let (fill, stroke) = if selected {
                (v.selection.bg_fill, v.selection.stroke)
            } else if hovered {
                (v.widgets.hovered.bg_fill, v.widgets.hovered.bg_stroke)
            } else {
                (v.widgets.inactive.bg_fill, v.widgets.inactive.bg_stroke)
            };
            let title_color = if selected || hovered {
                v.strong_text_color()
            } else if undone {
                v.weak_text_color()
            } else {
                v.widgets.inactive.fg_stroke.color
            };
            let detail_color = if selected || hovered {
                Color32::from_gray(205)
            } else {
                v.weak_text_color()
            };
            (fill, stroke, title_color, detail_color, v.extreme_bg_color)
        };
        let title_font = TextStyle::Body.resolve(ui.style());
        let detail_font = TextStyle::Small.resolve(ui.style());

        let painter = ui.painter();
        painter.rect_filled(rect, theme::CORNER_RADIUS, fill);
        painter.rect_stroke(rect, theme::CORNER_RADIUS, stroke, StrokeKind::Inside);

        // Thumbnail box on the left: a sunken backdrop with the preview fit inside,
        // preserving the image's aspect (letterboxed).
        let thumb_box = Rect::from_min_size(
            egui::pos2(rect.left() + CARD_PAD, rect.top() + CARD_PAD),
            egui::vec2(THUMB_W, THUMB_H),
        );
        painter.rect_filled(thumb_box, 3.0, backdrop);
        if let Some(id) = thumb {
            let uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(id, fit_rect(thumb_box, aspect), uv, Color32::WHITE);
        }

        // Title over detail, the pair vertically centered against the card.
        let text_x = thumb_box.right() + TEXT_GAP;
        let text_w = (rect.right() - CARD_PAD - text_x).max(1.0);
        let title = painter.layout(summary.title.clone(), title_font, title_color, text_w);
        let detail = (!summary.detail.is_empty())
            .then(|| painter.layout(summary.detail.clone(), detail_font, detail_color, text_w));

        let title_h = title.size().y;
        let gap_y = if detail.is_some() { 2.0 } else { 0.0 };
        let detail_h = detail.as_ref().map_or(0.0, |g| g.size().y);
        let block_top = rect.center().y - (title_h + gap_y + detail_h) / 2.0;
        painter.galley(egui::pos2(text_x, block_top), title, title_color);
        if let Some(g) = detail {
            painter.galley(
                egui::pos2(text_x, block_top + title_h + gap_y),
                g,
                detail_color,
            );
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The largest rectangle of the given `aspect` (width / height) that fits inside
/// `box_rect`, centered — so a preview is letterboxed rather than stretched.
fn fit_rect(box_rect: Rect, aspect: f32) -> Rect {
    let (bw, bh) = (box_rect.width(), box_rect.height());
    let size = if bw / bh > aspect {
        egui::vec2(bh * aspect, bh)
    } else {
        egui::vec2(bw, bw / aspect)
    };
    Rect::from_center_size(box_rect.center(), size)
}
