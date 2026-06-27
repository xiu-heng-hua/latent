//! The straighten-by-horizon tool: the user drags a reference line along a
//! horizon (or a vertical), and on release the angle that levels it is written to
//! `geometry.straighten_degrees`. The straighten slider stays as numeric entry.

use eframe::egui;
use egui::{Color32, Pos2, Stroke};

use super::super::canvas::ViewTransform;
use super::draw_handle;

/// The angle (degrees) that levels a reference line drawn between two normalized
/// points, given the displayed image aspect `image_w / image_h`.
///
/// The line's *screen* angle depends on the pixel aspect, so the deltas are
/// scaled by the displayed width/height before the `atan2` — otherwise the
/// horizon would not actually end up level on a non-square frame. The result is
/// the rotation that brings the line to the nearer of horizontal or vertical
/// (so dragging a near-vertical edge plumbs it), folded into `[-45, 45]`, with
/// the sign matching the engine's counter-clockwise-positive convention: a line
/// whose right end sits lower on screen (a clockwise visual tilt) returns a
/// positive angle, which rotates the image counter-clockwise to level it.
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
    deg.clamp(-45.0, 45.0)
}

/// Draw the in-progress horizon line and its two endpoint handles, in screen
/// space via the transform.
pub(crate) fn draw_line(
    painter: &egui::Painter,
    transform: &ViewTransform,
    from: [f32; 2],
    to: [f32; 2],
) {
    let a = transform.image_norm_to_screen(from);
    let b = transform.image_norm_to_screen(to);
    painter.line_segment([a, b], Stroke::new(1.5, Color32::WHITE));
    draw_handle(painter, transform, from);
    draw_handle(painter, transform, to);
}

/// Draw a faint center cross-hair prompt when no horizon is being drawn, so the
/// user knows the canvas is in the straighten tool.
pub(crate) fn draw_prompt(painter: &egui::Painter, transform: &ViewTransform) {
    let c: Pos2 = transform.image_norm_to_screen([0.5, 0.5]);
    let s = 8.0;
    let faint = Stroke::new(1.0, Color32::from_white_alpha(90));
    painter.line_segment([c - egui::vec2(s, 0.0), c + egui::vec2(s, 0.0)], faint);
    painter.line_segment([c - egui::vec2(0.0, s), c + egui::vec2(0.0, s)], faint);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_line_needs_no_rotation() {
        assert!(level_angle([0.1, 0.5], [0.9, 0.5], 1.5).abs() < 1e-4);
    }

    #[test]
    fn a_right_down_horizon_rotates_counter_clockwise() {
        // The right end is lower on screen (a clockwise visual tilt) → positive
        // angle (counter-clockwise rotation levels it).
        let a = level_angle([0.2, 0.4], [0.8, 0.5], 1.0);
        assert!(a > 0.0, "expected a positive (CCW) angle, got {a}");
        // A right-*up* horizon mirrors to a negative angle.
        let b = level_angle([0.2, 0.5], [0.8, 0.4], 1.0);
        assert!(b < 0.0, "expected a negative angle, got {b}");
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
