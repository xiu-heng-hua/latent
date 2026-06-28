//! The straighten-by-horizon tool: the user drags a reference line along a
//! horizon (or a vertical), and on release the angle that levels it is written to
//! `geometry.straighten_degrees`. The straighten slider stays as numeric entry.

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Stroke};

use super::super::canvas::ViewTransform;
use super::draw_handle;

/// Which end of the two-point reference line a drag controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineEnd {
    From,
    To,
}

/// The default reference line shown when the straighten tool is first entered: a
/// horizontal line across the middle third of the image, so both endpoint handles
/// are immediately visible and draggable rather than hidden behind a prompt.
pub(crate) const DEFAULT_LINE: ([f32; 2], [f32; 2]) = ([0.2, 0.5], [0.8, 0.5]);

/// The endpoint a press should grab: always the nearer of the two, so a drag can
/// never miss. The reference line is persistent — it is only ever refined, never
/// recreated — so a press always grabs an existing endpoint rather than spawning a
/// new line.
pub(crate) fn nearest_end(
    from: [f32; 2],
    to: [f32; 2],
    pointer: Pos2,
    transform: &ViewTransform,
) -> LineEnd {
    let df = transform.image_norm_to_screen(from).distance(pointer);
    let dt = transform.image_norm_to_screen(to).distance(pointer);
    if df <= dt { LineEnd::From } else { LineEnd::To }
}

/// The angle (degrees) that levels a reference line drawn between two normalized
/// points, given the displayed image aspect `image_w / image_h`.
///
/// The line's *screen* angle depends on the pixel aspect, so the deltas are
/// scaled by the displayed width/height before the `atan2` — otherwise the
/// horizon would not actually end up level on a non-square frame. The result is
/// the rotation that brings the line to the nearer of horizontal or vertical
/// (so dragging a near-vertical edge plumbs it), folded into `[-45, 45]`. The
/// engine rotates the image clockwise for a positive `straighten_degrees` (image
/// coordinates have y pointing down), so a line whose right end sits lower on
/// screen (a clockwise visual tilt) returns a negative angle, which rotates the
/// image counter-clockwise to level it.
///
/// Pure math — unit-tested without a window.
pub(crate) fn level_angle(from: [f32; 2], to: [f32; 2], image_aspect: f32) -> f32 {
    // Scale the deltas into a square aspect so the measured angle matches what the
    // eye sees on screen (screen y is down, matching the normalized coordinate).
    let dx = (to[0] - from[0]) * image_aspect;
    let dy = to[1] - from[1];
    if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
        return 0.0;
    }
    let mut deg = dy.atan2(dx).to_degrees();
    // Fold into (-90, 90]: the line and its reverse describe the same level.
    while deg > 90.0 {
        deg -= 180.0;
    }
    while deg <= -90.0 {
        deg += 180.0;
    }
    // If the line is closer to vertical, level it to the vertical (plumb) instead
    // — rotate the smaller way.
    if deg > 45.0 {
        deg -= 90.0;
    } else if deg < -45.0 {
        deg += 90.0;
    }
    // The engine rotates clockwise for a positive angle (image y is down), so the
    // leveling rotation is the negation of the line's measured screen tilt.
    -deg.clamp(-45.0, 45.0)
}

/// Draw the reference line and its two endpoint handles, in screen space via the
/// transform. The line reads as the horizon/vertical the user is leveling.
pub(crate) fn draw_line(
    painter: &egui::Painter,
    transform: &ViewTransform,
    from: [f32; 2],
    to: [f32; 2],
) {
    let a = transform.image_norm_to_screen(from);
    let b = transform.image_norm_to_screen(to);
    painter.line_segment([a, b], Stroke::new(2.0, Color32::WHITE));
    draw_handle(painter, transform, from);
    draw_handle(painter, transform, to);
}

/// Draw a one-line hint near the top of the image telling the user how the tool
/// works (drag a line along a horizon or a vertical to level it). Shown whenever
/// the straighten tool is active, above the reference line.
pub(crate) fn draw_hint(painter: &egui::Painter, transform: &ViewTransform) {
    let at: Pos2 = transform.image_norm_to_screen([0.5, 0.04]);
    let text = "Drag the line along a horizon or a vertical to level it";
    // A subtle shadow plate so the hint reads over any image content.
    let galley = painter.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(13.0),
        Color32::from_white_alpha(230),
    );
    let rect = Align2::CENTER_TOP
        .anchor_size(at, galley.size())
        .expand2(egui::vec2(6.0, 3.0));
    painter.rect_filled(rect, 4.0, Color32::from_black_alpha(140));
    painter.galley(
        rect.center_top() + egui::vec2(-galley.size().x / 2.0, 3.0),
        galley,
        Color32::WHITE,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, Vec2};

    fn transform() -> ViewTransform {
        ViewTransform::new(
            Vec2::new(100.0, 100.0),
            Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 200.0)),
            super::super::super::canvas::Zoom::Fit,
            Vec2::ZERO,
        )
    }

    #[test]
    fn the_default_line_is_level() {
        // The line the tool opens with is horizontal, so merely entering the tool
        // (before any drag) implies no rotation.
        let (from, to) = DEFAULT_LINE;
        assert!(level_angle(from, to, 1.0).abs() < 1e-4);
    }

    #[test]
    fn nearest_end_picks_the_nearer_endpoint() {
        let t = transform();
        let (from, to) = ([0.2, 0.5], [0.8, 0.5]);
        // On (or near) each endpoint grabs that endpoint.
        assert_eq!(
            nearest_end(from, to, t.image_norm_to_screen(from), &t),
            LineEnd::From
        );
        assert_eq!(
            nearest_end(from, to, t.image_norm_to_screen(to), &t),
            LineEnd::To
        );
        // A press left of centre grabs `from`, right of centre grabs `to` — there is
        // no "miss" that would spawn a new line.
        let left = t.image_norm_to_screen([0.4, 0.5]);
        assert_eq!(nearest_end(from, to, left, &t), LineEnd::From);
        let right = t.image_norm_to_screen([0.6, 0.5]);
        assert_eq!(nearest_end(from, to, right, &t), LineEnd::To);
    }

    #[test]
    fn a_level_line_needs_no_rotation() {
        assert!(level_angle([0.1, 0.5], [0.9, 0.5], 1.5).abs() < 1e-4);
    }

    #[test]
    fn a_right_down_horizon_levels_counter_clockwise() {
        // The right end is lower on screen (a clockwise visual tilt). The engine
        // rotates clockwise for a positive angle, so leveling it (a counter-
        // clockwise rotation) is a negative angle.
        let a = level_angle([0.2, 0.4], [0.8, 0.5], 1.0);
        assert!(
            a < 0.0,
            "a right-down horizon levels with a negative angle, got {a}"
        );
        // A right-*up* horizon mirrors to a positive angle.
        let b = level_angle([0.2, 0.5], [0.8, 0.4], 1.0);
        assert!(
            b > 0.0,
            "a right-up horizon levels with a positive angle, got {b}"
        );
        assert!((a + b).abs() < 1e-4, "the mirror should negate the angle");
    }

    #[test]
    fn the_pixel_aspect_changes_the_measured_angle() {
        // The same normalized deltas on a 2:1 wide frame measure a different angle
        // than on a square one (the X delta counts double on the wide frame), so
        // the horizon actually levels rather than over/under-rotating.
        let square = level_angle([0.2, 0.4], [0.7, 0.5], 1.0);
        let wide = level_angle([0.2, 0.4], [0.7, 0.5], 2.0);
        assert!(
            (square - wide).abs() > 1e-3,
            "the aspect must affect the angle: {square} vs {wide}"
        );
    }

    #[test]
    fn a_near_vertical_line_plumbs_to_the_smaller_rotation() {
        // A line dragged mostly downward (a near-vertical reference) levels to the
        // vertical, a small rotation, not a ~90° one.
        let a = level_angle([0.5, 0.1], [0.55, 0.9], 1.0);
        assert!(
            a.abs() <= 45.0,
            "a vertical reference plumbs within ±45°: {a}"
        );
    }
}
