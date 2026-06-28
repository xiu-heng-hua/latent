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

/// The no-correction keystone parameters, the absolute-mapping reference a corner
/// drag reduces the pointer against.
pub(crate) const ZERO: Perspective = Perspective {
    vertical: 0.0,
    horizontal: 0.0,
};

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

/// Where the four corner handles sit for the keystone parameters `p`, as
/// normalized anchors in [`CORNERS`] order — the inverse of [`corner_to_params`]
/// about a zero start. So the handles *move* off the frame corners to show the
/// pending correction (e.g. a positive `vertical` pulls the two top corners
/// inward), and the quad they span is the pending corrected outline.
pub(crate) fn corner_positions(p: Perspective) -> [[f32; 2]; 4] {
    let mut out = CORNERS;
    for (i, anchor) in out.iter_mut().enumerate() {
        let is_top = i == 0 || i == 1;
        let is_left = i == 0 || i == 3;
        let v_dir = if is_top { 1.0 } else { -1.0 };
        let h_dir = if is_left { 1.0 } else { -1.0 };
        // Recover the displacement that maps to these params (inverse of the
        // forward reduction in `corner_to_params`).
        let inward_x = p.vertical / (v_dir * GAIN);
        let dx = if is_left { inward_x } else { -inward_x };
        let inward_y = p.horizontal / (h_dir * GAIN);
        let dy = if is_top { inward_y } else { -inward_y };
        anchor[0] = (CORNERS[i][0] + dx).clamp(-0.5, 1.5);
        anchor[1] = (CORNERS[i][1] + dy).clamp(-0.5, 1.5);
    }
    out
}

/// The current keystone parameters, or both-zero when there is no correction.
pub(crate) fn current(settings: &Settings) -> Perspective {
    settings.geometry.perspective.unwrap_or(ZERO)
}

/// Hit-test the four corner handles at their current (params-displaced)
/// positions; returns the corner index a press grabs.
pub(crate) fn hit_test(p: Perspective, pointer: Pos2, transform: &ViewTransform) -> Option<usize> {
    nearest_handle(&corner_positions(p), pointer, transform, HANDLE_HIT_RADIUS)
}

/// Write the keystone parameters (mid-drag), clearing to `None` when both reach
/// zero (the same normalize the slider block does).
pub(crate) fn write(history: &mut History<Settings>, p: Perspective) {
    history.current_mut().geometry.perspective =
        (p.vertical != 0.0 || p.horizontal != 0.0).then_some(p);
}

/// Draw the keystone overlay: the un-warped frame (a faint reference rectangle)
/// and, over it, the pending corrected quad — the four corner handles at their
/// params-displaced positions joined by an accent outline — so the handles move
/// to show the correction while the image underneath stays put.
pub(crate) fn draw_overlay(painter: &egui::Painter, transform: &ViewTransform, p: Perspective) {
    // The static frame reference (where the corners start).
    let frame: Vec<Pos2> = CORNERS
        .iter()
        .map(|&c| transform.image_norm_to_screen(c))
        .collect();
    for w in 0..4 {
        painter.line_segment(
            [frame[w], frame[(w + 1) % 4]],
            Stroke::new(1.0, Color32::from_white_alpha(70)),
        );
    }
    // The pending corrected quad and its draggable corner handles.
    let corners = corner_positions(p);
    let quad: Vec<Pos2> = corners
        .iter()
        .map(|&c| transform.image_norm_to_screen(c))
        .collect();
    for w in 0..4 {
        painter.line_segment(
            [quad[w], quad[(w + 1) % 4]],
            Stroke::new(1.5, Color32::WHITE),
        );
    }
    for &c in &corners {
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
    fn corner_positions_invert_corner_to_params() {
        // The drawn handle positions are the inverse of the param reduction: a
        // press at a handle's drawn position (against a zero start) reproduces the
        // very params it was drawn from — so the grabbed handle tracks the pointer
        // with no jump. Exercise both axes at once.
        let p = Perspective {
            vertical: 0.3,
            horizontal: -0.2,
        };
        let positions = corner_positions(p);
        for (corner, pos) in positions.iter().enumerate() {
            let back = corner_to_params(corner, *pos, zero());
            assert!(
                (back.vertical - p.vertical).abs() < 1e-4
                    && (back.horizontal - p.horizontal).abs() < 1e-4,
                "corner {corner} round-trip: {back:?} vs {p:?}"
            );
        }
        // Zero params leave the handles on the frame corners.
        assert_eq!(corner_positions(zero()), CORNERS);
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
