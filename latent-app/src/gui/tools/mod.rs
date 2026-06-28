//! The on-canvas interaction framework and the tools built on it.
//!
//! One layer owns handle drawing, hit-testing, the grabbed-handle drag state,
//! and the begin/commit-per-drag history wiring; every tool (crop, straighten,
//! keystone, mask shapes, brush) plugs into it instead of re-rolling pointer
//! math. All screen↔image conversion goes through the canvas
//! [`ViewTransform`](super::canvas::ViewTransform) — nothing here recomputes it.
//!
//! Coordinate convention: a *handle* is a normalized `[0, 1]` anchor on the
//! oriented image. To draw it, map it forward through the transform; to read a
//! drag, map the pointer back. A hit-test compares in screen space after mapping
//! the handle forward, so the grab tolerance is a fixed number of screen pixels
//! at any zoom.

pub(crate) mod crop;
pub(crate) mod keystone;
pub(crate) mod mask_shape;
pub(crate) mod overlay;
pub(crate) mod straighten;

use eframe::egui;
use egui::{Color32, Painter, Pos2, Stroke, Vec2};

use super::canvas::ViewTransform;
use super::theme;

/// Which on-canvas tool is active. Only the active tool draws handles and
/// consumes pointer input on the canvas, so e.g. crop handles never intercept a
/// brush stroke. [`CanvasTool::None`] leaves the canvas to pure pan/zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CanvasTool {
    /// No tool — the canvas is pure view (pan/zoom).
    #[default]
    None,
    /// The crop rectangle with its edge/corner handles.
    Crop,
    /// Drag a line on the image to level the horizon.
    Straighten,
    /// Drag corner handles to set the keystone correction.
    Keystone,
    /// Drag the selected mask shape's gradient/radial handles.
    MaskShape,
    /// Paint brush dabs onto a brush mask.
    Brush,
    /// Pick a neutral pixel for white balance (wired by a later white-balance
    /// tool; reserved here so the tool routing already has a slot for it). The
    /// pointer routing already has a no-op arm for it, so a later tool only has to
    /// fill in the pick.
    #[allow(dead_code)]
    WbPick,
}

/// The active grab of an in-progress canvas drag, held across frames so a drag
/// that starts on a handle keeps editing *that* handle even as the pointer moves
/// off it. Each variant carries the start-of-gesture snapshot the tool needs to
/// compute the constrained result (e.g. the crop's opposite-corner anchor). The
/// history `begin()`/`commit()` brackets the gesture's lifetime — one undo step
/// per drag.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CanvasDrag {
    /// A crop handle (or interior), with the crop at the moment of the grab.
    Crop(crop::CropGrab, latent_edit::Crop),
    /// A keystone corner (0..4), with the perspective at the grab.
    Keystone(usize, latent_edit::Perspective),
    /// A mask-shape handle, with the shape at the grab.
    MaskShape(mask_shape::ShapeGrab, latent_edit::MaskShape),
    /// The straighten horizon line, carrying its first endpoint; the second
    /// endpoint follows the pointer.
    Straighten([f32; 2]),
    /// A brush stroke in progress (no discrete handle).
    Brush,
}

/// Screen-pixel radius within which a handle is considered grabbed. Sized so a
/// handle stays comfortably clickable without overlapping its neighbours.
pub(crate) const HANDLE_HIT_RADIUS: f32 = 10.0;

/// Screen-pixel radius the handle dot is drawn at.
const HANDLE_DRAW_RADIUS: f32 = 5.0;

/// The multiplicative step the brush size keys apply, so a `[`/`]` press feels
/// even across the whole range rather than a fixed additive nudge.
pub(crate) const BRUSH_KEY_STEP: f32 = 1.2;

/// Scale `value` by `factor` and clamp it into `[lo, hi]` — the shared math the
/// brush `[`/`]` size and feather keys run, so the key path and its test agree.
pub(crate) fn scaled_clamped(value: f32, factor: f32, lo: f32, hi: f32) -> f32 {
    (value * factor).clamp(lo, hi)
}

/// The index of the handle (from a list of normalized anchors) nearest the
/// pointer and within `tol_px` screen pixels, or `None` when the pointer is not
/// over any handle. The comparison is done in screen space, after mapping each
/// anchor forward through the transform — so the tolerance is a true screen-pixel
/// radius regardless of zoom or the image's pixel aspect.
pub(crate) fn nearest_handle(
    anchors: &[[f32; 2]],
    pointer: Pos2,
    transform: &ViewTransform,
    tol_px: f32,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &a) in anchors.iter().enumerate() {
        let screen = transform.image_norm_to_screen(a);
        let d = screen.distance(pointer);
        if d <= tol_px && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Draw a grab handle: a filled accent dot with a contrasting outline at the
/// screen position of a normalized anchor. Sized in screen pixels so it stays
/// grabbable at any zoom.
pub(crate) fn draw_handle(painter: &Painter, transform: &ViewTransform, anchor: [f32; 2]) {
    let p = transform.image_norm_to_screen(anchor);
    painter.circle_filled(p, HANDLE_DRAW_RADIUS, theme::ACCENT);
    painter.circle_stroke(p, HANDLE_DRAW_RADIUS, Stroke::new(1.5, Color32::WHITE));
}

/// Draw a guide line between two normalized anchors, in screen space.
pub(crate) fn draw_line(painter: &Painter, transform: &ViewTransform, a: [f32; 2], b: [f32; 2]) {
    painter.line_segment(
        [
            transform.image_norm_to_screen(a),
            transform.image_norm_to_screen(b),
        ],
        Stroke::new(1.5, theme::ACCENT),
    );
}

/// Whether a pan gesture (middle-drag, or space held while left-dragging) is
/// active — the active tool must not also consume it, so the pointer falls
/// through to pan.
fn pan_gesture(resp: &egui::Response) -> bool {
    let space = resp.ctx.input(|i| i.key_down(egui::Key::Space));
    resp.dragged_by(egui::PointerButton::Middle) || (space && resp.dragged())
}

/// Route the canvas pointer to the active tool. The tool draws its handles and,
/// when it grabs one, consumes the drag as one undo step (begin on grab, write
/// the field each move, commit on release); when no handle is hit — or the tool
/// is [`CanvasTool::None`] — the drag falls through to pan. Returns whether the
/// tool changed the settings this frame (so the canvas re-renders).
///
/// All pointer mapping goes through the [`ViewTransform`]; nothing here
/// recomputes screen↔image math.
pub(crate) fn interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    painter: &Painter,
    transform: &ViewTransform,
    active: usize,
    local_sel: usize,
    shape_sel: usize,
) -> bool {
    // A pan gesture always wins, and `None` is pure view.
    if session.tool == CanvasTool::None || pan_gesture(resp) {
        return false;
    }
    match session.tool {
        CanvasTool::Crop => crop_interact(session, resp, painter, transform, active),
        CanvasTool::Keystone => keystone_interact(session, resp, painter, transform, active),
        CanvasTool::MaskShape => mask_interact(
            session, resp, painter, transform, active, local_sel, shape_sel,
        ),
        CanvasTool::Straighten => straighten_interact(session, resp, painter, transform, active),
        CanvasTool::Brush => brush_interact(
            session, resp, painter, transform, active, local_sel, shape_sel,
        ),
        CanvasTool::WbPick => wb_pick_interact(session, resp, transform, active),
        CanvasTool::None => false,
    }
}

/// The gray eyedropper: a click on the image samples the linear working pixel
/// there and sets the global white balance so that patch renders neutral, then
/// disarms the tool (one click, one undo step). The sampled pixel is the
/// **post-WB** preview pixel (the image as currently rendered), so the
/// neutralizing math composes the delta onto the current offset. Off-image clicks
/// are ignored.
fn wb_pick_interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    transform: &ViewTransform,
    active: usize,
) -> bool {
    use super::widgets::{wb, wb_or_none};
    if !resp.clicked() {
        return false;
    }
    let Some(pos) = resp.interact_pointer_pos() else {
        return false;
    };
    let norm = transform.screen_to_image_norm(pos);
    if norm[0] < 0.0 || norm[0] > 1.0 || norm[1] < 0.0 || norm[1] > 1.0 {
        return false;
    }
    let Some(img) = &session.preview_rendered else {
        return false;
    };
    let (px, py) = super::canvas::norm_to_pixel(norm, img.width(), img.height());
    let Some(sample) = img.try_get(px, py) else {
        return false;
    };
    let history = &mut session.variants[active];
    let current = history.current().global.white_balance.unwrap_or_default();
    let neutralized = wb::neutralizing_wb(sample, current);
    history.begin();
    history.current_mut().global.white_balance = wb_or_none(neutralized);
    history.commit();
    // One pick is enough; return to plain view so the next click doesn't re-pick.
    session.tool = CanvasTool::None;
    true
}

/// The normalized pointer position for a drag this frame, if any.
fn pointer_norm(resp: &egui::Response, transform: &ViewTransform) -> Option<[f32; 2]> {
    resp.interact_pointer_pos()
        .or_else(|| resp.hover_pos())
        .map(|p| transform.screen_to_image_norm(p))
}

/// The crop tool gesture + overlay.
fn crop_interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    painter: &Painter,
    transform: &ViewTransform,
    active: usize,
) -> bool {
    let current = crop::current_crop(session.variants[active].current());
    let mut changed = false;

    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(grab) = crop::hit_test(current, pos, transform)
    {
        session.variants[active].begin();
        session.drag = Some(CanvasDrag::Crop(grab, current));
    }
    if resp.dragged()
        && let Some(CanvasDrag::Crop(grab, start)) = session.drag
        && let Some(p) = pointer_norm(resp, transform)
    {
        let ratio = session
            .crop_aspect_locked
            .then(|| session.crop_aspect.visual_ratio(session.displayed_aspect()))
            .flatten();
        let c = crop::apply_drag(start, grab, p, ratio, session.displayed_aspect());
        crop::write_crop(&mut session.variants[active], c);
        changed = true;
    }
    if resp.drag_stopped() && matches!(session.drag, Some(CanvasDrag::Crop(..))) {
        session.variants[active].commit();
        session.drag = None;
    }

    crop::draw_overlay(painter, transform, current, session.crop_thirds);
    changed
}

/// The keystone tool gesture + overlay.
fn keystone_interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    painter: &Painter,
    transform: &ViewTransform,
    active: usize,
) -> bool {
    let mut changed = false;
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(corner) = keystone::hit_test(pos, transform)
    {
        session.variants[active].begin();
        let start = keystone::current(session.variants[active].current());
        session.drag = Some(CanvasDrag::Keystone(corner, start));
    }
    if resp.dragged()
        && let Some(CanvasDrag::Keystone(corner, start)) = session.drag
        && let Some(p) = pointer_norm(resp, transform)
    {
        let params = keystone::corner_to_params(corner, p, start);
        keystone::write(&mut session.variants[active], params);
        changed = true;
    }
    if resp.drag_stopped() && matches!(session.drag, Some(CanvasDrag::Keystone(..))) {
        session.variants[active].commit();
        session.drag = None;
    }
    keystone::draw_overlay(painter, transform);
    changed
}

/// The mask-shape (gradient/radial) tool gesture + overlay.
fn mask_interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    painter: &Painter,
    transform: &ViewTransform,
    active: usize,
    local_sel: usize,
    shape_sel: usize,
) -> bool {
    let Some(shape) =
        mask_shape::selected_shape(session.variants[active].current(), local_sel, shape_sel)
    else {
        return false;
    };
    let mut changed = false;
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(grab) = mask_shape::hit_test(&shape, pos, transform)
    {
        session.variants[active].begin();
        session.drag = Some(CanvasDrag::MaskShape(grab, shape.clone()));
    }
    if resp.dragged()
        && let Some(CanvasDrag::MaskShape(grab, start)) = session.drag.clone()
        && let Some(p) = pointer_norm(resp, transform)
    {
        let updated = mask_shape::apply_drag(&start, grab, p);
        mask_shape::write(&mut session.variants[active], local_sel, shape_sel, updated);
        changed = true;
    }
    if resp.drag_stopped() && matches!(session.drag, Some(CanvasDrag::MaskShape(..))) {
        session.variants[active].commit();
        session.drag = None;
    }
    mask_shape::draw_overlay(painter, transform, &shape);
    changed
}

/// The straighten-by-horizon tool gesture + overlay.
fn straighten_interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    painter: &Painter,
    transform: &ViewTransform,
    active: usize,
) -> bool {
    let mut changed = false;
    if resp.drag_started()
        && let Some(p) = pointer_norm(resp, transform)
    {
        session.variants[active].begin();
        session.drag = Some(CanvasDrag::Straighten(p));
    }
    if let Some(CanvasDrag::Straighten(from)) = session.drag {
        if let Some(to) = pointer_norm(resp, transform) {
            straighten::draw_line(painter, transform, from, to);
            if resp.dragged() {
                let angle = straighten::level_angle(from, to, session.displayed_aspect());
                session.variants[active]
                    .current_mut()
                    .geometry
                    .straighten_degrees = angle;
                changed = true;
            }
        }
        if resp.drag_stopped() {
            session.variants[active].commit();
            session.drag = None;
        }
    } else {
        straighten::draw_prompt(painter, transform);
    }
    changed
}

/// The brush tool: keep the per-stroke dab painting (one undo step per stroke)
/// and the live coverage overlay, routed through the transform — no ad-hoc rect
/// math. A two-ring cursor at the pointer shows the radius and feather.
fn brush_interact(
    session: &mut super::app::Session,
    resp: &egui::Response,
    painter: &Painter,
    transform: &ViewTransform,
    active: usize,
    local_sel: usize,
    shape_sel: usize,
) -> bool {
    use latent_edit::{Dab, MaskShape};
    // Only paint when the selected shape of the selected local is a brush mask.
    let is_brush = session.variants[active]
        .current()
        .locals
        .get(local_sel)
        .and_then(|l| l.mask.shapes.get(shape_sel))
        .is_some_and(|s| matches!(s, MaskShape::Brush(_)));
    if !is_brush {
        return false;
    }

    let mut painted = false;
    let click = resp.clicked() && !resp.dragged();
    if resp.drag_started() || click {
        session.variants[active].begin();
        session.drag = Some(CanvasDrag::Brush);
    }
    if (resp.dragged() || click)
        && let Some(p) = pointer_norm(resp, transform)
    {
        let nx = p[0].clamp(0.0, 1.0);
        let ny = p[1].clamp(0.0, 1.0);
        if let Some(MaskShape::Brush(b)) = session.variants[active].current_mut().locals[local_sel]
            .mask
            .shapes
            .get_mut(shape_sel)
        {
            b.dabs.push(Dab {
                x: nx,
                y: ny,
                radius: session.brush_radius,
                feather: session.brush_feather,
                erase: session.brush_erase,
            });
            painted = true;
        }
    }
    if resp.drag_stopped() || click {
        session.variants[active].commit();
        session.drag = None;
    }

    // The two-ring brush cursor (radius + feather) at the pointer, in screen
    // space via the transform so it tracks the painted size at any zoom.
    if let Some(pos) = resp.hover_pos() {
        let inner = transform.norm_len_to_screen(session.brush_radius);
        let outer = transform.norm_len_to_screen(session.brush_radius + session.brush_feather);
        let color = if session.brush_erase {
            egui::Color32::from_rgb(230, 120, 120)
        } else {
            egui::Color32::WHITE
        };
        draw_ellipse(
            painter,
            pos,
            outer,
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(120)),
        );
        draw_ellipse(painter, pos, inner, egui::Stroke::new(1.5, color));
    }
    painted
}

/// Draw an axis-aligned ellipse outline centered on a normalized point, with the
/// given screen-pixel half-axes. Egui has no ellipse primitive, so it is traced
/// as a closed polyline — used for the radial mask rings and the brush cursor,
/// which are circular in *normalized* space and therefore elliptical on screen
/// when the image is non-square.
pub(crate) fn draw_ellipse(painter: &Painter, center: Pos2, half: Vec2, stroke: Stroke) {
    const SEGMENTS: usize = 48;
    let pts: Vec<Pos2> = (0..=SEGMENTS)
        .map(|i| {
            let t = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            Pos2::new(center.x + half.x * t.cos(), center.y + half.y * t.sin())
        })
        .collect();
    painter.add(egui::Shape::line(pts, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Rect;

    fn transform() -> ViewTransform {
        // A 100×100 image fitted in a 200×200 panel: displayed scale 2.0, so a
        // normalized step of 0.1 is 20 screen pixels.
        ViewTransform::new(
            Vec2::new(100.0, 100.0),
            Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 200.0)),
            super::super::canvas::Zoom::Fit,
            Vec2::ZERO,
        )
    }

    #[test]
    fn nearest_handle_picks_the_closest_within_tolerance() {
        let t = transform();
        let anchors = [[0.0, 0.0], [1.0, 0.0], [0.5, 0.5]];
        // Pointer right on the center handle (normalized 0.5,0.5 → screen 100,100).
        let on_center = t.image_norm_to_screen([0.5, 0.5]);
        assert_eq!(
            nearest_handle(&anchors, on_center, &t, HANDLE_HIT_RADIUS),
            Some(2)
        );

        // A pointer 6px from the top-left handle (within the 10px tolerance) but
        // far from the others picks the top-left.
        let near_tl = t.image_norm_to_screen([0.0, 0.0]) + Vec2::new(6.0, 0.0);
        assert_eq!(
            nearest_handle(&anchors, near_tl, &t, HANDLE_HIT_RADIUS),
            Some(0)
        );
    }

    #[test]
    fn nearest_handle_misses_outside_tolerance() {
        let t = transform();
        let anchors = [[0.0, 0.0], [1.0, 1.0]];
        // Pointer in the middle of the image, far from any corner handle.
        let middle = t.image_norm_to_screen([0.5, 0.5]);
        assert_eq!(
            nearest_handle(&anchors, middle, &t, HANDLE_HIT_RADIUS),
            None
        );
        // An empty handle list never hits.
        assert_eq!(nearest_handle(&[], middle, &t, HANDLE_HIT_RADIUS), None);
    }

    #[test]
    fn brush_size_step_scales_and_clamps() {
        // Growing scales up, shrinking scales down, and both clamp to the slider
        // range so the keys never push the brush outside its numeric domain.
        let grown = scaled_clamped(0.1, BRUSH_KEY_STEP, 0.01, 0.5);
        assert!((grown - 0.12).abs() < 1e-6);
        let shrunk = scaled_clamped(0.1, 1.0 / BRUSH_KEY_STEP, 0.01, 0.5);
        assert!(shrunk < 0.1 && (0.01..0.1).contains(&shrunk));
        // At the top of the range, growing clamps rather than overshooting.
        assert_eq!(scaled_clamped(0.49, BRUSH_KEY_STEP, 0.01, 0.5), 0.5);
        // At the bottom, shrinking clamps to the floor.
        assert_eq!(scaled_clamped(0.011, 1.0 / BRUSH_KEY_STEP, 0.01, 0.5), 0.01);
    }
}
