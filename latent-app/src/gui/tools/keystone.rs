//! The keystone tool: four corner handles the user pulls to square up converging
//! verticals/horizontals. The displacement of the corners is reduced to the two
//! symmetric parameters the engine consumes — `vertical` and `horizontal` — never
//! a new perspective model. The two keystone sliders stay as numeric entry.

use eframe::egui;
use egui::{Color32, Pos2, Stroke};
use latent_edit::{History, Perspective, Settings};

use super::super::canvas::ViewTransform;
use super::{HANDLE_HIT_RADIUS, draw_handle, nearest_handle};

/// The slider range the engine expects for each keystone parameter; a corner pull
/// is clamped into it so the handles can't drive the warp past the usable range.
const PARAM_LIMIT: f32 = 0.8;

/// How strongly a corner's normalized displacement maps onto the symmetric
/// keystone parameters. A full corner pull from the frame edge to the center
/// (displacement `0.5`) reaches near the parameter limit.
const GAIN: f32 = 1.6;

/// The four corner anchors (normalized), in TL, TR, BR, BL order.
pub(crate) const CORNERS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// Reduce a dragged corner to the two symmetric keystone parameters.
///
/// `corner` is which corner moved (0=TL, 1=TR, 2=BR, 3=BL) and `p` its new
/// normalized position. The displacement from the corner's home position drives:
/// - `vertical` (converging verticals) from how far the corner moved *horizontally*
///   inward/outward — a top corner pulled in widens the top relative to the bottom;
/// - `horizontal` (converging horizontals) from how far it moved *vertically*.
///
/// The mapping is symmetric (one parameter per axis), so it never tries to move
/// the four corners independently — it writes only the two fields the engine has.
/// Pure math, so it is unit-tested without a window.
pub(crate) fn corner_to_params(corner: usize, p: [f32; 2], start: Perspective) -> Perspective {
    let home = CORNERS[corner.min(3)];
    let dx = p[0] - home[0];
    let dy = p[1] - home[1];
    let is_top = corner == 0 || corner == 1;
    let is_left = corner == 0 || corner == 3;

    // A top corner pulled inward (dx toward center) converges the top; sign it so
    // the same physical gesture on any top corner moves `vertical` the same way.
    let v_dir = if is_top { 1.0 } else { -1.0 };
    let inward_x = if is_left { dx } else { -dx };
    let vertical = (start.vertical + v_dir * inward_x * GAIN).clamp(-PARAM_LIMIT, PARAM_LIMIT);

    let h_dir = if is_left { 1.0 } else { -1.0 };
    let inward_y = if is_top { dy } else { -dy };
    let horizontal = (start.horizontal + h_dir * inward_y * GAIN).clamp(-PARAM_LIMIT, PARAM_LIMIT);

    Perspective {
        vertical,
        horizontal,
    }
}

/// The current keystone parameters, or both-zero when there is no correction.
pub(crate) fn current(settings: &Settings) -> Perspective {
    settings.geometry.perspective.unwrap_or(Perspective {
        vertical: 0.0,
        horizontal: 0.0,
    })
}

/// Hit-test the four corner handles; returns the corner index a press grabs.
pub(crate) fn hit_test(pointer: Pos2, transform: &ViewTransform) -> Option<usize> {
    nearest_handle(&CORNERS, pointer, transform, HANDLE_HIT_RADIUS)
}

/// Write the keystone parameters (mid-drag), clearing to `None` when both reach
/// zero (the same normalize the slider block does).
pub(crate) fn write(history: &mut History<Settings>, p: Perspective) {
    history.current_mut().geometry.perspective =
        (p.vertical != 0.0 || p.horizontal != 0.0).then_some(p);
}

/// Draw the keystone overlay: the four corner handles and the edges between them.
pub(crate) fn draw_overlay(painter: &egui::Painter, transform: &ViewTransform) {
    let pts: Vec<Pos2> = CORNERS
        .iter()
        .map(|&c| transform.image_norm_to_screen(c))
        .collect();
    for w in 0..4 {
        painter.line_segment(
            [pts[w], pts[(w + 1) % 4]],
            Stroke::new(1.0, Color32::from_white_alpha(120)),
        );
    }
    for &c in &CORNERS {
        draw_handle(painter, transform, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero() -> Perspective {
        Perspective {
            vertical: 0.0,
            horizontal: 0.0,
        }
    }

    #[test]
    fn pulling_a_top_corner_inward_sets_vertical() {
        // Pull the top-left corner inward in X (toward center). `vertical` moves
        // off zero; `horizontal` stays put (no Y displacement).
        let p = corner_to_params(0, [0.2, 0.0], zero());
        assert!(
            p.vertical > 0.0,
            "top-left inward → positive vertical: {p:?}"
        );
        assert!(
            (p.horizontal - 0.0).abs() < 1e-6,
            "no Y move → horizontal zero"
        );

        // The opposite-side top corner pulled inward moves `vertical` the same way.
        let q = corner_to_params(1, [0.8, 0.0], zero());
        assert!(
            q.vertical > 0.0,
            "top-right inward → positive vertical too: {q:?}"
        );
    }

    #[test]
    fn pulling_a_corner_vertically_sets_horizontal() {
        // Move the top-left corner down in Y (inward): `horizontal` responds.
        let p = corner_to_params(0, [0.0, 0.2], zero());
        assert!(
            p.horizontal > 0.0,
            "left corner inward Y → positive horizontal: {p:?}"
        );
        assert!((p.vertical - 0.0).abs() < 1e-6, "no X move → vertical zero");
    }

    #[test]
    fn params_stay_in_engine_range() {
        // A wild pull (corner dragged across the frame) clamps to the engine range.
        let p = corner_to_params(2, [-1.0, -1.0], zero());
        assert!(p.vertical.abs() <= PARAM_LIMIT + 1e-6);
        assert!(p.horizontal.abs() <= PARAM_LIMIT + 1e-6);
    }

    #[test]
    fn zero_displacement_keeps_the_start() {
        // Releasing a corner exactly on its home leaves the parameters unchanged.
        let start = Perspective {
            vertical: 0.3,
            horizontal: -0.1,
        };
        let p = corner_to_params(3, CORNERS[3], start);
        assert!((p.vertical - 0.3).abs() < 1e-6 && (p.horizontal + 0.1).abs() < 1e-6);
    }
}
