//! Gradient and radial mask-shape handles. For the selected local's selected
//! shape, the user drags the gradient line endpoints, or the radial center /
//! radius ring / feather ring, directly on the image. Writes the same normalized
//! `mask.shapes[selected]` fields the sliders write; the sliders stay as numeric
//! entry. Luminosity / color-range / brush shapes have no positional handles.

use eframe::egui;
use egui::{Color32, Pos2, Stroke};
use latent_edit::{Gradient, History, MaskShape, Radial, Settings};

use super::super::canvas::ViewTransform;
use super::{HANDLE_HIT_RADIUS, draw_ellipse, draw_handle, draw_line, nearest_handle};

/// Which handle of a mask shape a drag grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShapeGrab {
    /// Gradient first endpoint `(x0, y0)`.
    GradFrom,
    /// Gradient second endpoint `(x1, y1)`.
    GradTo,
    /// Gradient midpoint — translates the whole line.
    GradMid,
    /// Radial center `(cx, cy)`.
    RadialCenter,
    /// Radial radius ring.
    RadialRadius,
    /// Radial outer feather ring (`radius + feather`).
    RadialFeather,
}

/// The selected shape of the selected local, if it is a gradient or radial (the
/// only shapes with positional handles). `shape` is the index within the local's
/// shape list, so the handles follow the shape selected in the panel.
pub(crate) fn selected_shape(settings: &Settings, local: usize, shape: usize) -> Option<MaskShape> {
    let l = settings.locals.get(local)?;
    match l.mask.shapes.get(shape)? {
        s @ (MaskShape::Gradient(_) | MaskShape::Radial(_)) => Some(s.clone()),
        _ => None,
    }
}

/// Convert a normalized pointer to a normalized radius from a center — the
/// screen-pixel-ring → normalized-radius conversion the radial handles use. The
/// radius the engine evaluates is the plain Euclidean distance in normalized
/// space (elliptical on screen because the axes scale differently), so reading
/// the pointer back through the transform and measuring here keeps the on-screen
/// ring and the engine's falloff in agreement. Pure math, unit-tested.
pub(crate) fn radius_from_pointer(center: [f32; 2], pointer_norm: [f32; 2]) -> f32 {
    let dx = pointer_norm[0] - center[0];
    let dy = pointer_norm[1] - center[1];
    (dx * dx + dy * dy).sqrt()
}

/// Hit-test a gradient's handles (the two endpoints and the midpoint).
fn hit_gradient(g: &Gradient, pointer: Pos2, transform: &ViewTransform) -> Option<ShapeGrab> {
    let mid = [(g.x0 + g.x1) / 2.0, (g.y0 + g.y1) / 2.0];
    let anchors = [[g.x0, g.y0], [g.x1, g.y1], mid];
    nearest_handle(&anchors, pointer, transform, HANDLE_HIT_RADIUS).map(|i| match i {
        0 => ShapeGrab::GradFrom,
        1 => ShapeGrab::GradTo,
        _ => ShapeGrab::GradMid,
    })
}

/// Hit-test a radial's handles (center, radius ring, feather ring). The rings are
/// hit by comparing the pointer's normalized radius to each ring's radius within
/// a tolerance converted to normalized units off the transform's length scalar.
fn hit_radial(r: &Radial, pointer: Pos2, transform: &ViewTransform) -> Option<ShapeGrab> {
    // The center handle wins when the pointer is on it.
    if nearest_handle(&[[r.cx, r.cy]], pointer, transform, HANDLE_HIT_RADIUS).is_some() {
        return Some(ShapeGrab::RadialCenter);
    }
    let pn = transform.screen_to_image_norm(pointer);
    let pr = radius_from_pointer([r.cx, r.cy], pn);
    // A screen-pixel tolerance turned into a normalized band (use the x axis as a
    // representative scale — the rings are close enough for a grab tolerance).
    let tol = transform.screen_len_to_norm(HANDLE_HIT_RADIUS).x;
    if (pr - r.radius).abs() <= tol {
        Some(ShapeGrab::RadialRadius)
    } else if (pr - (r.radius + r.feather)).abs() <= tol {
        Some(ShapeGrab::RadialFeather)
    } else {
        None
    }
}

/// Hit-test the selected shape's handles; returns the grab a press starts.
pub(crate) fn hit_test(
    shape: &MaskShape,
    pointer: Pos2,
    transform: &ViewTransform,
) -> Option<ShapeGrab> {
    match shape {
        MaskShape::Gradient(g) => hit_gradient(g, pointer, transform),
        MaskShape::Radial(r) => hit_radial(r, pointer, transform),
        _ => None,
    }
}

/// Apply a drag of `grab` to `shape`, moving it to the normalized pointer `p`,
/// and return the updated shape. Pure transform of the shape's fields.
pub(crate) fn apply_drag(shape: &MaskShape, grab: ShapeGrab, p: [f32; 2]) -> MaskShape {
    let px = p[0].clamp(0.0, 1.0);
    let py = p[1].clamp(0.0, 1.0);
    match (shape, grab) {
        (MaskShape::Gradient(g), ShapeGrab::GradFrom) => MaskShape::Gradient(Gradient {
            x0: px,
            y0: py,
            ..*g
        }),
        (MaskShape::Gradient(g), ShapeGrab::GradTo) => MaskShape::Gradient(Gradient {
            x1: px,
            y1: py,
            ..*g
        }),
        (MaskShape::Gradient(g), ShapeGrab::GradMid) => {
            // Translate the whole line so its midpoint follows the pointer.
            let (mx, my) = ((g.x0 + g.x1) / 2.0, (g.y0 + g.y1) / 2.0);
            let (dx, dy) = (px - mx, py - my);
            MaskShape::Gradient(Gradient {
                x0: (g.x0 + dx).clamp(0.0, 1.0),
                y0: (g.y0 + dy).clamp(0.0, 1.0),
                x1: (g.x1 + dx).clamp(0.0, 1.0),
                y1: (g.y1 + dy).clamp(0.0, 1.0),
            })
        }
        (MaskShape::Radial(r), ShapeGrab::RadialCenter) => MaskShape::Radial(Radial {
            cx: px,
            cy: py,
            ..*r
        }),
        (MaskShape::Radial(r), ShapeGrab::RadialRadius) => {
            let radius = radius_from_pointer([r.cx, r.cy], [px, py]).max(0.0);
            MaskShape::Radial(Radial { radius, ..*r })
        }
        (MaskShape::Radial(r), ShapeGrab::RadialFeather) => {
            // The outer ring is radius + feather; feather is the gap beyond radius.
            let outer = radius_from_pointer([r.cx, r.cy], [px, py]);
            let feather = (outer - r.radius).max(0.0);
            MaskShape::Radial(Radial { feather, ..*r })
        }
        // A grab that doesn't match the shape leaves it unchanged.
        (s, _) => s.clone(),
    }
}

/// Write the updated shape to the selected local's selected shape (mid-drag).
pub(crate) fn write(
    history: &mut History<Settings>,
    local: usize,
    shape_sel: usize,
    shape: MaskShape,
) {
    if let Some(l) = history.current_mut().locals.get_mut(local)
        && let Some(slot) = l.mask.shapes.get_mut(shape_sel)
    {
        *slot = shape;
    }
}

/// Draw the selected shape's handles and guides.
pub(crate) fn draw_overlay(painter: &egui::Painter, transform: &ViewTransform, shape: &MaskShape) {
    match shape {
        MaskShape::Gradient(g) => {
            draw_line(painter, transform, [g.x0, g.y0], [g.x1, g.y1]);
            draw_handle(painter, transform, [g.x0, g.y0]);
            draw_handle(painter, transform, [g.x1, g.y1]);
            draw_handle(
                painter,
                transform,
                [(g.x0 + g.x1) / 2.0, (g.y0 + g.y1) / 2.0],
            );
        }
        MaskShape::Radial(r) => {
            let center = transform.image_norm_to_screen([r.cx, r.cy]);
            let ring = transform.norm_len_to_screen(r.radius);
            let outer = transform.norm_len_to_screen(r.radius + r.feather);
            draw_ellipse(painter, center, ring, Stroke::new(1.5, Color32::WHITE));
            draw_ellipse(
                painter,
                center,
                outer,
                Stroke::new(1.0, Color32::from_white_alpha(120)),
            );
            draw_handle(painter, transform, [r.cx, r.cy]);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_from_pointer_is_normalized_distance() {
        // A pointer 0.3 right and 0.4 down from the center is a 0.5 normalized
        // radius (the engine measures radii in plain normalized distance).
        let r = radius_from_pointer([0.5, 0.5], [0.8, 0.9]);
        assert!((r - 0.5).abs() < 1e-6, "expected 0.5, got {r}");
        // On the center it is zero.
        assert!(radius_from_pointer([0.2, 0.2], [0.2, 0.2]).abs() < 1e-6);
    }

    #[test]
    fn dragging_the_radius_ring_sets_the_radius() {
        let shape = MaskShape::Radial(Radial {
            cx: 0.5,
            cy: 0.5,
            radius: 0.1,
            feather: 0.1,
        });
        // Drag the radius ring to a pointer 0.25 normalized from center.
        let out = apply_drag(&shape, ShapeGrab::RadialRadius, [0.75, 0.5]);
        let MaskShape::Radial(r) = out else {
            panic!("expected radial");
        };
        assert!(
            (r.radius - 0.25).abs() < 1e-6,
            "radius follows the ring: {}",
            r.radius
        );
        assert!((r.feather - 0.1).abs() < 1e-6, "feather untouched");
    }

    #[test]
    fn dragging_the_feather_ring_sets_the_gap_beyond_radius() {
        let shape = MaskShape::Radial(Radial {
            cx: 0.5,
            cy: 0.5,
            radius: 0.2,
            feather: 0.05,
        });
        // Outer ring dragged to 0.35 from center → feather = 0.35 - 0.2 = 0.15.
        let out = apply_drag(&shape, ShapeGrab::RadialFeather, [0.85, 0.5]);
        let MaskShape::Radial(r) = out else {
            panic!("expected radial");
        };
        assert!(
            (r.feather - 0.15).abs() < 1e-6,
            "feather is the gap beyond radius"
        );
        assert!((r.radius - 0.2).abs() < 1e-6, "radius untouched");
    }

    #[test]
    fn dragging_a_gradient_endpoint_moves_only_it() {
        let shape = MaskShape::Gradient(Gradient {
            x0: 0.2,
            y0: 0.2,
            x1: 0.8,
            y1: 0.8,
        });
        let out = apply_drag(&shape, ShapeGrab::GradFrom, [0.1, 0.3]);
        let MaskShape::Gradient(g) = out else {
            panic!("expected gradient");
        };
        assert!(
            (g.x0 - 0.1).abs() < 1e-6 && (g.y0 - 0.3).abs() < 1e-6,
            "from moved"
        );
        assert!(
            (g.x1 - 0.8).abs() < 1e-6 && (g.y1 - 0.8).abs() < 1e-6,
            "to fixed"
        );
    }

    #[test]
    fn dragging_the_gradient_midpoint_translates_the_line() {
        let shape = MaskShape::Gradient(Gradient {
            x0: 0.2,
            y0: 0.4,
            x1: 0.6,
            y1: 0.4,
        });
        // Midpoint is (0.4, 0.4); drag it to (0.5, 0.5) → both ends shift by +0.1.
        let out = apply_drag(&shape, ShapeGrab::GradMid, [0.5, 0.5]);
        let MaskShape::Gradient(g) = out else {
            panic!("expected gradient");
        };
        assert!((g.x0 - 0.3).abs() < 1e-6 && (g.y0 - 0.5).abs() < 1e-6);
        assert!((g.x1 - 0.7).abs() < 1e-6 && (g.y1 - 0.5).abs() < 1e-6);
    }
}
