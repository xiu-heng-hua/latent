//! The crop tool: a draggable rectangle with eight edge/corner handles plus an
//! interior move, aspect-ratio presets with a lock, a rule-of-thirds overlay,
//! and dimming outside the kept region. It writes `geometry.crop` (a full-frame
//! rect normalizes back to `None`); the numeric crop fields stay as a fallback.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Stroke};
use latent_edit::{Crop, History, Settings};

use super::super::canvas::ViewTransform;
use super::draw_handle;

/// The smallest a crop edge may shrink to (normalized), so the rectangle stays
/// grabbable and never collapses to a zero-area sliver.
const MIN_SIZE: f32 = 0.02;

/// How close (screen px) a pointer must be to a crop *corner* to grab it. Generous
/// so a corner is easy to catch even when aimed at roughly.
const CORNER_HIT_RADIUS: f32 = 14.0;

/// How close (screen px) a pointer must be to a crop *edge* line — anywhere along
/// its span, not just a midpoint handle — to grab that edge.
const EDGE_HIT_RADIUS: f32 = 8.0;

/// An aspect-ratio constraint for the crop. The numeric ratios are *visual*
/// width:height; `Free` is unconstrained and `Original` matches the displayed
/// image's own aspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AspectRatio {
    #[default]
    Free,
    Original,
    Square,
    ThreeTwo,
    FourThree,
    SixteenNine,
}

impl AspectRatio {
    /// The presets in display order, with their labels.
    pub(crate) const ALL: [(AspectRatio, &'static str); 6] = [
        (AspectRatio::Free, "Free"),
        (AspectRatio::Original, "Original"),
        (AspectRatio::Square, "1:1"),
        (AspectRatio::ThreeTwo, "3:2"),
        (AspectRatio::FourThree, "4:3"),
        (AspectRatio::SixteenNine, "16:9"),
    ];

    /// The target *visual* width:height ratio for this preset, given the displayed
    /// image's pixel aspect (`image_w / image_h`). `Free` returns `None` (no
    /// constraint); `Original` is the image's own aspect.
    pub(crate) fn visual_ratio(self, image_aspect: f32) -> Option<f32> {
        match self {
            AspectRatio::Free => None,
            AspectRatio::Original => Some(image_aspect),
            AspectRatio::Square => Some(1.0),
            AspectRatio::ThreeTwo => Some(3.0 / 2.0),
            AspectRatio::FourThree => Some(4.0 / 3.0),
            AspectRatio::SixteenNine => Some(16.0 / 9.0),
        }
    }
}

/// Which part of the crop rectangle a drag grabbed. The eight handles are the
/// four corners and the four edge midpoints; `Interior` translates the whole
/// rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CropGrab {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Interior,
}

/// The current crop rectangle as a normalized `Crop` — the stored crop, or the
/// full frame when there is none.
pub(crate) fn current_crop(settings: &Settings) -> Crop {
    settings.geometry.crop.unwrap_or(Crop {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    })
}

/// The eight handle anchors (normalized) for a crop rectangle, in draw order:
/// TL, T, TR, R, BR, B, BL, L.
pub(crate) fn handle_anchors(c: Crop) -> [[f32; 2]; 8] {
    let (x0, y0) = (c.x, c.y);
    let (x1, y1) = (c.x + c.width, c.y + c.height);
    let (mx, my) = (c.x + c.width / 2.0, c.y + c.height / 2.0);
    [
        [x0, y0],
        [mx, y0],
        [x1, y0],
        [x1, my],
        [x1, y1],
        [mx, y1],
        [x0, y1],
        [x0, my],
    ]
}

/// Whether a crop is (essentially) the full frame, so it can normalize back to
/// `None`. A tiny tolerance absorbs rounding from a drag that lands at the edges.
pub(crate) fn is_full_frame(c: Crop) -> bool {
    c.x <= 1e-4 && c.y <= 1e-4 && c.width >= 1.0 - 1e-4 && c.height >= 1.0 - 1e-4
}

/// Apply a drag to the crop: move the grabbed handle (or the interior) to the
/// normalized pointer `p`, honoring the aspect `ratio` (a *visual* width:height,
/// already accounting for the image's pixel aspect) when one is set, and clamp
/// the result into `[0, 1]` with a minimum size. Pure math, so the corner
/// anchoring and the ratio constraint are unit-testable without a window.
///
/// `image_aspect` is the displayed `image_w / image_h`, needed to convert a
/// *visual* ratio into the normalized-space width:height the rect stores (a 1:1
/// visual square is `width == height · (image_h / image_w)` in normalized units).
pub(crate) fn apply_drag(
    start: Crop,
    grab: CropGrab,
    p: [f32; 2],
    ratio: Option<f32>,
    image_aspect: f32,
) -> Crop {
    let px = p[0].clamp(0.0, 1.0);
    let py = p[1].clamp(0.0, 1.0);
    let (mut x0, mut y0) = (start.x, start.y);
    let (mut x1, mut y1) = (start.x + start.width, start.y + start.height);

    match grab {
        CropGrab::Interior => {
            // Translate, keeping the size, clamped so the rect stays on-frame.
            let w = start.width;
            let h = start.height;
            let nx = (start.x + (px - (start.x + w / 2.0))).clamp(0.0, 1.0 - w);
            let ny = (start.y + (py - (start.y + h / 2.0))).clamp(0.0, 1.0 - h);
            return Crop {
                x: nx,
                y: ny,
                width: w,
                height: h,
            };
        }
        CropGrab::Left | CropGrab::TopLeft | CropGrab::BottomLeft => x0 = px.min(x1 - MIN_SIZE),
        CropGrab::Right | CropGrab::TopRight | CropGrab::BottomRight => x1 = px.max(x0 + MIN_SIZE),
        _ => {}
    }
    match grab {
        CropGrab::Top | CropGrab::TopLeft | CropGrab::TopRight => y0 = py.min(y1 - MIN_SIZE),
        CropGrab::Bottom | CropGrab::BottomLeft | CropGrab::BottomRight => {
            y1 = py.max(y0 + MIN_SIZE)
        }
        _ => {}
    }

    let mut c = Crop {
        x: x0,
        y: y0,
        width: (x1 - x0).max(MIN_SIZE),
        height: (y1 - y0).max(MIN_SIZE),
    };

    if let Some(r) = ratio {
        c = constrain_ratio(start, grab, c, r, image_aspect);
    }
    clamp_crop(c)
}

/// Re-fit the dragged rectangle `c` to the visual aspect `ratio`, anchored at the
/// corner/edge *opposite* the one being dragged so the rect grows from the side
/// the user is not moving. The normalized width:height is `ratio · image_h /
/// image_w` (the visual ratio expressed in the non-square normalized space).
fn constrain_ratio(start: Crop, grab: CropGrab, c: Crop, ratio: f32, image_aspect: f32) -> Crop {
    // Normalized width:height that yields the requested *visual* ratio.
    let norm_ratio = (ratio / image_aspect).max(1e-4);

    // Which dimension the drag drives, and the anchor (the fixed opposite edge).
    let (anchor_right, anchor_bottom) = match grab {
        CropGrab::TopLeft => (true, true),
        CropGrab::TopRight => (false, true),
        CropGrab::BottomLeft => (true, false),
        CropGrab::BottomRight | CropGrab::Interior => (false, false),
        CropGrab::Left => (true, false),
        CropGrab::Right => (false, false),
        CropGrab::Top => (false, true),
        CropGrab::Bottom => (false, false),
    };
    let _ = start;

    // A top/bottom edge drives the height; a left/right edge drives the width; a
    // corner follows the pointer's dominant axis (the dragged free rect's aspect vs
    // the target), so the corner tracks the cursor in both directions. Driving a
    // corner from the width alone would leave a vertical drag — whose width barely
    // changes — looking stuck. Whichever drives, derive the other from `norm_ratio`,
    // then if that overruns the frame clamp it and re-derive so the ratio stays exact.
    let height_driven = match grab {
        CropGrab::Top | CropGrab::Bottom => true,
        CropGrab::Left | CropGrab::Right => false,
        _ => c.width / c.height <= norm_ratio,
    };
    let (mut w, mut h) = if height_driven {
        (c.height * norm_ratio, c.height)
    } else {
        (c.width, c.width / norm_ratio)
    };
    if w > 1.0 {
        w = 1.0;
        h = w / norm_ratio;
    }
    if h > 1.0 {
        h = 1.0;
        w = h * norm_ratio;
    }

    // Anchor the fixed edge: grow from the opposite side.
    let x = if anchor_right { c.x + c.width - w } else { c.x };
    let y = if anchor_bottom {
        c.y + c.height - h
    } else {
        c.y
    };
    Crop {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Clamp a crop into `[0, 1]` with a minimum size: keep the size (capped at the
/// frame) and slide the origin so the rect stays fully on-frame.
pub(crate) fn clamp_crop(c: Crop) -> Crop {
    let w = c.width.clamp(MIN_SIZE, 1.0);
    let h = c.height.clamp(MIN_SIZE, 1.0);
    let x = c.x.clamp(0.0, 1.0 - w);
    let y = c.y.clamp(0.0, 1.0 - h);
    Crop {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Re-fit an existing crop to a newly-chosen aspect ratio, centered on the
/// current rectangle's center and clamped to the frame. Used when the user picks
/// a preset while a crop exists.
pub(crate) fn refit_to_ratio(c: Crop, ratio: f32, image_aspect: f32) -> Crop {
    let norm_ratio = (ratio / image_aspect).max(1e-4);
    let (cx, cy) = (c.x + c.width / 2.0, c.y + c.height / 2.0);
    // Start from the current width and derive the matching height, shrinking if
    // it would leave the frame.
    let mut w = c.width;
    let mut h = w / norm_ratio;
    if h > 1.0 {
        h = 1.0;
        w = h * norm_ratio;
    }
    if w > 1.0 {
        w = 1.0;
        h = w / norm_ratio;
    }
    clamp_crop(Crop {
        x: cx - w / 2.0,
        y: cy - h / 2.0,
        width: w,
        height: h,
    })
}

/// Hit-test the crop border for a pointer, in screen space. The whole border is
/// grabbable, not just eight handle dots: a press within [`CORNER_HIT_RADIUS`] of a
/// corner grabs that corner; otherwise a press within [`EDGE_HIT_RADIUS`] of an
/// edge line (anywhere along its span) grabs that edge; otherwise a press inside
/// the rectangle moves it; otherwise nothing. Corners win over edges (they sit at
/// the ends of two), so a corner is always resizable in both axes.
pub(crate) fn hit_test(c: Crop, pointer: Pos2, transform: &ViewTransform) -> Option<CropGrab> {
    let rect = crop_screen_rect(transform, c);

    // Corners first — the nearest within the (generous) corner tolerance.
    let corners = [
        (CropGrab::TopLeft, rect.left_top()),
        (CropGrab::TopRight, rect.right_top()),
        (CropGrab::BottomRight, rect.right_bottom()),
        (CropGrab::BottomLeft, rect.left_bottom()),
    ];
    let mut best: Option<(CropGrab, f32)> = None;
    for (g, p) in corners {
        let d = p.distance(pointer);
        if d <= CORNER_HIT_RADIUS && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((g, d));
        }
    }
    if let Some((g, _)) = best {
        return Some(g);
    }

    // Edges: near an edge line and within its span (with a little overhang past the
    // corners so the whole side is catchable).
    let span_x =
        pointer.x >= rect.left() - EDGE_HIT_RADIUS && pointer.x <= rect.right() + EDGE_HIT_RADIUS;
    let span_y =
        pointer.y >= rect.top() - EDGE_HIT_RADIUS && pointer.y <= rect.bottom() + EDGE_HIT_RADIUS;
    if span_y && (pointer.x - rect.left()).abs() <= EDGE_HIT_RADIUS {
        return Some(CropGrab::Left);
    }
    if span_y && (pointer.x - rect.right()).abs() <= EDGE_HIT_RADIUS {
        return Some(CropGrab::Right);
    }
    if span_x && (pointer.y - rect.top()).abs() <= EDGE_HIT_RADIUS {
        return Some(CropGrab::Top);
    }
    if span_x && (pointer.y - rect.bottom()).abs() <= EDGE_HIT_RADIUS {
        return Some(CropGrab::Bottom);
    }

    // Interior.
    let n = transform.screen_to_image_norm(pointer);
    let inside = n[0] >= c.x && n[0] <= c.x + c.width && n[1] >= c.y && n[1] <= c.y + c.height;
    inside.then_some(CropGrab::Interior)
}

/// Write `crop` to the active variant's settings (mid-drag), normalizing a
/// full-frame rect back to `None`.
pub(crate) fn write_crop(history: &mut History<Settings>, crop: Crop) {
    history.current_mut().geometry.crop = (!is_full_frame(crop)).then_some(crop);
}

/// Draw the crop overlay: dim outside the kept region, outline the rectangle,
/// draw a rule-of-thirds grid inside it, and place the eight handles. Pure egui
/// paint — no pixel change.
pub(crate) fn draw_overlay(
    painter: &egui::Painter,
    transform: &ViewTransform,
    c: Crop,
    show_thirds: bool,
) {
    let rect = crop_screen_rect(transform, c);
    let image = transform.image_rect();

    // Dim the four border regions outside the crop with a translucent dark fill.
    let dim = Color32::from_black_alpha(140);
    // Top, bottom, left, right of the crop, clipped to the image rect.
    let top = Rect::from_min_max(image.min, Pos2::new(image.max.x, rect.min.y));
    let bottom = Rect::from_min_max(Pos2::new(image.min.x, rect.max.y), image.max);
    let left = Rect::from_min_max(
        Pos2::new(image.min.x, rect.min.y),
        Pos2::new(rect.min.x, rect.max.y),
    );
    let right = Rect::from_min_max(
        Pos2::new(rect.max.x, rect.min.y),
        Pos2::new(image.max.x, rect.max.y),
    );
    for r in [top, bottom, left, right] {
        if r.is_positive() {
            painter.rect_filled(r, 0.0, dim);
        }
    }

    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.5, Color32::WHITE),
        egui::StrokeKind::Inside,
    );

    if show_thirds {
        let thin = Stroke::new(1.0, Color32::from_white_alpha(120));
        for k in 1..3 {
            let fx = rect.min.x + rect.width() * k as f32 / 3.0;
            let fy = rect.min.y + rect.height() * k as f32 / 3.0;
            painter.vline(fx, rect.y_range(), thin);
            painter.hline(rect.x_range(), fy, thin);
        }
    }

    for a in handle_anchors(c) {
        draw_handle(painter, transform, a);
    }
}

/// The screen rect for a normalized crop, via the transform.
fn crop_screen_rect(transform: &ViewTransform, c: Crop) -> Rect {
    Rect::from_min_max(
        transform.image_norm_to_screen([c.x, c.y]),
        transform.image_norm_to_screen([c.x + c.width, c.y + c.height]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_drag_moves_two_edges_and_clamps() {
        // A full-frame crop, drag the bottom-right corner inward.
        let start = Crop {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        let c = apply_drag(start, CropGrab::BottomRight, [0.6, 0.7], None, 1.0);
        assert!(
            (c.x - 0.0).abs() < 1e-6 && (c.y - 0.0).abs() < 1e-6,
            "TL stays put"
        );
        assert!((c.width - 0.6).abs() < 1e-6, "width follows the corner");
        assert!((c.height - 0.7).abs() < 1e-6, "height follows the corner");

        // Dragging the corner off-frame clamps into [0, 1].
        let c = apply_drag(start, CropGrab::BottomRight, [1.5, -0.5], None, 1.0);
        assert!(c.x + c.width <= 1.0 + 1e-6 && c.y + c.height <= 1.0 + 1e-6);
        assert!(c.width >= MIN_SIZE && c.height >= MIN_SIZE);
    }

    #[test]
    fn edge_drag_moves_one_edge() {
        let start = Crop {
            x: 0.2,
            y: 0.2,
            width: 0.6,
            height: 0.6,
        };
        // Drag the left edge right; only x/width change, the right edge is fixed.
        let c = apply_drag(start, CropGrab::Left, [0.4, 0.5], None, 1.0);
        assert!((c.x - 0.4).abs() < 1e-6);
        assert!(((c.x + c.width) - 0.8).abs() < 1e-6, "right edge fixed");
        assert!(
            (c.y - 0.2).abs() < 1e-6 && (c.height - 0.6).abs() < 1e-6,
            "y/height untouched"
        );
    }

    #[test]
    fn interior_drag_translates_keeping_size() {
        let start = Crop {
            x: 0.2,
            y: 0.2,
            width: 0.4,
            height: 0.4,
        };
        let c = apply_drag(start, CropGrab::Interior, [0.6, 0.6], None, 1.0);
        assert!(
            (c.width - 0.4).abs() < 1e-6 && (c.height - 0.4).abs() < 1e-6,
            "size kept"
        );
        // Center followed the pointer (0.6, 0.6) → origin 0.4, 0.4.
        assert!((c.x - 0.4).abs() < 1e-6 && (c.y - 0.4).abs() < 1e-6);

        // A move that would leave the frame clamps the origin.
        let c = apply_drag(start, CropGrab::Interior, [2.0, 2.0], None, 1.0);
        assert!(c.x + c.width <= 1.0 + 1e-6 && c.y + c.height <= 1.0 + 1e-6);
    }

    #[test]
    fn aspect_lock_holds_the_visual_ratio_through_the_pixel_aspect() {
        // A 2:1 wide image (image_aspect = 2.0). Lock to a 1:1 *visual* square and
        // drag the bottom-right corner: the normalized width:height must be 1:2
        // (because the image is twice as wide as tall), so a visual square is half
        // as wide as it is tall in normalized units.
        let start = Crop {
            x: 0.0,
            y: 0.0,
            width: 0.8,
            height: 0.8,
        };
        let image_aspect = 2.0;
        let c = apply_drag(
            start,
            CropGrab::BottomRight,
            [0.8, 0.8],
            Some(1.0),
            image_aspect,
        );
        // norm_ratio = ratio / image_aspect = 0.5, so width = height * 0.5.
        assert!(
            (c.width - c.height * 0.5).abs() < 1e-4,
            "normalized w:h must be 0.5 for a 1:1 visual on a 2:1 frame: {c:?}"
        );
        // Anchored at the top-left (the opposite corner) — it stays put.
        assert!((c.x - 0.0).abs() < 1e-6 && (c.y - 0.0).abs() < 1e-6);
    }

    #[test]
    fn locked_corner_follows_a_vertical_drag() {
        // With a 1:1 lock on a square frame, dragging the bottom-right corner mostly
        // downward (height grows, width barely) must still resize the rectangle to
        // follow the cursor — driving from width alone would leave it stuck.
        let start = Crop {
            x: 0.0,
            y: 0.0,
            width: 0.4,
            height: 0.4,
        };
        let c = apply_drag(start, CropGrab::BottomRight, [0.45, 0.8], Some(1.0), 1.0);
        // The free drag is taller than wide, so the height drives: the square grows
        // to ~the dragged height, not the near-unchanged width.
        assert!(
            (c.width - c.height).abs() < 1e-4,
            "ratio held (square): {c:?}"
        );
        assert!(
            c.height > 0.7,
            "the corner followed the vertical drag, not stuck near 0.4: {c:?}"
        );
        // Anchored at the top-left corner.
        assert!((c.x - 0.0).abs() < 1e-6 && (c.y - 0.0).abs() < 1e-6);
    }

    #[test]
    fn full_frame_normalizes_to_none() {
        assert!(is_full_frame(Crop {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        }));
        assert!(!is_full_frame(Crop {
            x: 0.1,
            y: 0.0,
            width: 0.8,
            height: 1.0,
        }));
    }

    #[test]
    fn refit_centers_and_clamps() {
        let start = Crop {
            x: 0.1,
            y: 0.1,
            width: 0.8,
            height: 0.8,
        };
        // Refit a square image's crop to 16:9: the height shrinks, the center holds.
        let c = refit_to_ratio(start, 16.0 / 9.0, 1.0);
        let (cx0, cy0) = (start.x + start.width / 2.0, start.y + start.height / 2.0);
        let (cx1, cy1) = (c.x + c.width / 2.0, c.y + c.height / 2.0);
        assert!(
            (cx0 - cx1).abs() < 1e-4 && (cy0 - cy1).abs() < 1e-4,
            "center kept"
        );
        assert!(
            (c.width / c.height - 16.0 / 9.0).abs() < 1e-3,
            "ratio applied"
        );
        // Stays on-frame.
        assert!(c.x >= -1e-6 && c.y >= -1e-6 && c.x + c.width <= 1.0 + 1e-6);
    }

    fn test_transform() -> ViewTransform {
        use super::super::super::canvas::Zoom;
        // A 1000×800 image fit into a 500×400 panel at the origin: image_rect is
        // (0,0)–(500,400), so a normalized point maps to screen at scale 500×400.
        ViewTransform::new(
            egui::Vec2::new(1000.0, 800.0),
            Rect::from_min_size(Pos2::ZERO, egui::Vec2::new(500.0, 400.0)),
            Zoom::Fit,
            egui::Vec2::ZERO,
        )
    }

    #[test]
    fn hit_test_grabs_corners_and_edges_generously() {
        let t = test_transform();
        // A centered sub-rect crop with margin around it.
        let c = Crop {
            x: 0.25,
            y: 0.25,
            width: 0.5,
            height: 0.5,
        };
        let rect = crop_screen_rect(&t, c);

        // A press a few px inside a corner still grabs that corner (not the
        // interior) — the symptom of the old eight-dots-only hit-test was a corner
        // aim landing on the interior and moving the rectangle.
        let near_tl = rect.left_top() + egui::Vec2::new(6.0, 6.0);
        assert_eq!(hit_test(c, near_tl, &t), Some(CropGrab::TopLeft));
        let near_br = rect.right_bottom() - egui::Vec2::new(5.0, 5.0);
        assert_eq!(hit_test(c, near_br, &t), Some(CropGrab::BottomRight));

        // A press anywhere along an edge (not just its midpoint) grabs that edge.
        let on_left_edge = Pos2::new(rect.left(), rect.top() + rect.height() * 0.2);
        assert_eq!(hit_test(c, on_left_edge, &t), Some(CropGrab::Left));
        let on_bottom_edge = Pos2::new(rect.left() + rect.width() * 0.8, rect.bottom());
        assert_eq!(hit_test(c, on_bottom_edge, &t), Some(CropGrab::Bottom));

        // Well inside moves the rectangle; well outside grabs nothing.
        assert_eq!(hit_test(c, rect.center(), &t), Some(CropGrab::Interior));
        let outside = rect.left_top() - egui::Vec2::new(40.0, 40.0);
        assert_eq!(hit_test(c, outside, &t), None);
    }

    #[test]
    fn a_crop_drag_is_one_undo_step() {
        // The begin/commit-per-drag gesture the canvas runs (begin on grab, write
        // each move, commit on release) must produce exactly one undo step, and an
        // undo must restore the prior crop. Simulated here on the history directly,
        // since the egui drag surface itself is display-unverifiable.
        let mut history = History::new(Settings::default());
        assert_eq!(history.current().geometry.crop, None);

        // The gesture: grab the bottom-right corner of the full frame, drag it in
        // over a couple of frames, release.
        let start = current_crop(history.current());
        history.begin();
        for p in [[0.8, 0.8], [0.7, 0.6]] {
            let c = apply_drag(start, CropGrab::BottomRight, p, None, 1.0);
            write_crop(&mut history, c);
        }
        history.commit();

        // The crop is set, and a single undo clears it back to the prior state.
        assert!(history.current().geometry.crop.is_some());
        assert!(history.undo());
        assert_eq!(history.current().geometry.crop, None);
        // Only one step: a second undo has nothing left.
        assert!(!history.undo());
    }
}
