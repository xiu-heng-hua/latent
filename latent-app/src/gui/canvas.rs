//! The central canvas: the output transform that turns a rendered image into an
//! egui texture, the single screen↔image [`ViewTransform`] every on-canvas
//! feature reads, and the `CentralPanel` that fits the photo onto the neutral
//! surround, handles zoom/pan, draws an optional before/after, samples the pixel
//! under the cursor, and paints brush dabs.
//!
//! Resolution note: the displayed texture is the 1600px downscaled preview, so
//! "100%" is one preview-pixel per screen-pixel — **not** true 1:1 sensor-pixel
//! peeping, and above 100% the preview is upscaled (soft). True pixel-peeping
//! would need an on-demand full-resolution tile of the visible region; that is
//! out of scope here. The hover readout samples the same downscaled preview.

use eframe::egui;
use egui::{Color32, Pos2, Rect, Vec2};
use latent_image::ImageBuf;

use super::app::{App, BeforeAfter, Session};
use super::theme;
use super::tools;

/// Discrete zoom ladder (percent). `+`/`−` step along it; the wheel snaps to the
/// nearest neighbour in the gesture direction. `Fit` is handled separately since
/// it tracks the panel rather than being a fixed level.
pub(crate) const ZOOM_LADDER: &[f32] = &[0.25, 0.33, 0.5, 0.67, 1.0, 1.5, 2.0, 4.0, 8.0];

/// The current zoom intent. `Fit` recomputes its scale from the live panel every
/// frame (so it stays fitted on resize); `Percent` pins a fixed displayed scale.
/// The transform is rebuilt per frame from this intent — no pixel offsets are
/// held in state, only the intent and the pan.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum Zoom {
    /// Fit the whole image in the panel (the default).
    #[default]
    Fit,
    /// A fixed displayed scale, where `1.0` is "100%" (one preview-pixel per
    /// screen-pixel — see the module note on what 100% means here).
    Percent(f32),
}

impl Zoom {
    /// Step to the next ladder level in `dir` (`+1` in, `−1` out), clamped at the
    /// ends. From `Fit`, stepping in jumps onto the ladder near the current fit
    /// scale; here we step relative to 100% as the conventional anchor.
    fn stepped(self, dir: i32, fit_scale: f32) -> Zoom {
        let current = match self {
            Zoom::Fit => fit_scale,
            Zoom::Percent(p) => p,
        };
        // Find where `current` sits on the ladder and move one notch.
        let idx = ZOOM_LADDER
            .iter()
            .position(|&l| l >= current - 1e-4)
            .unwrap_or(ZOOM_LADDER.len() - 1);
        let next = if dir > 0 {
            // Stepping in: if `current` is below this ladder stop, land on it;
            // otherwise advance one.
            if ZOOM_LADDER[idx] > current + 1e-4 {
                idx
            } else {
                (idx + 1).min(ZOOM_LADDER.len() - 1)
            }
        } else {
            idx.saturating_sub(1)
        };
        Zoom::Percent(ZOOM_LADDER[next])
    }
}

/// The single owner of the screen↔image coordinate mapping: a small `Copy` value
/// rebuilt each frame from the texture size, the panel rect, the zoom, and the
/// pan. Every on-canvas feature — brush, before/after alignment, pixel readout,
/// and later tools/overlays — reads this and never recomputes screen↔image math.
///
/// Normalized image space is `[0, 1] × [0, 1]` over the (oriented) image, so it
/// is resolution-independent: the same normalized point maps to the preview, the
/// full-res export, and the source-space mask evaluation alike.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ViewTransform {
    /// Texture (image) size in pixels.
    image_size: Vec2,
    /// The letterbox-fit scale: `min(panel.w / img.w, panel.h / img.h)`.
    fit_scale: f32,
    /// The user zoom multiplier on top of `fit_scale` (`1.0` for plain fit).
    zoom: f32,
    /// Top-left of the drawn image rect, in screen space (centering + pan baked
    /// in).
    offset: Vec2,
}

impl ViewTransform {
    /// Build the transform for an image of `image_size` pixels shown in `panel`,
    /// at the given `zoom` (`Zoom::Fit` ⇒ scale `1.0` over the fit) and `pan`
    /// (screen-space offset, zero when fitted).
    pub(crate) fn new(image_size: Vec2, panel: Rect, zoom: Zoom, pan: Vec2) -> Self {
        let fit_scale = Self::compute_fit_scale(image_size, panel.size());
        let zoom_mul = match zoom {
            Zoom::Fit => 1.0,
            // The displayed scale is the requested percent, expressed relative to
            // fit so `displayed_scale = fit_scale * zoom`.
            Zoom::Percent(p) => p / fit_scale.max(f32::EPSILON),
        };
        let displayed = fit_scale * zoom_mul;
        let drawn = image_size * displayed;
        // Center the drawn image in the panel, then offset by the pan.
        let offset = panel.center().to_vec2() - drawn / 2.0 + pan;
        Self {
            image_size,
            fit_scale,
            zoom: zoom_mul,
            offset,
        }
    }

    /// The letterbox-fit scale for `image_size` inside `panel_size` (longest side
    /// bounded, aspect preserved). Public so callers that need the fit percent
    /// (the status bar, the zoom ladder) don't reimplement it.
    pub(crate) fn compute_fit_scale(image_size: Vec2, panel_size: Vec2) -> f32 {
        if image_size.x <= 0.0 || image_size.y <= 0.0 {
            return 1.0;
        }
        (panel_size.x / image_size.x).min(panel_size.y / image_size.y)
    }

    /// The displayed scale (preview-pixels → screen-pixels): `fit_scale * zoom`.
    pub(crate) fn displayed_scale(&self) -> f32 {
        self.fit_scale * self.zoom
    }

    /// The letterbox-fit scale this transform was built with — the anchor the
    /// zoom ladder steps relative to.
    pub(crate) fn fit_scale(&self) -> f32 {
        self.fit_scale
    }

    /// The on-screen rect the image is drawn into (top-left at `offset`, sized by
    /// the displayed scale).
    pub(crate) fn image_rect(&self) -> Rect {
        let size = self.image_size * self.displayed_scale();
        Rect::from_min_size(Pos2::new(self.offset.x, self.offset.y), size)
    }

    /// Map a normalized `[0, 1]` image point to a screen position. Used to *draw*
    /// handles and overlays.
    pub(crate) fn image_norm_to_screen(&self, norm: [f32; 2]) -> Pos2 {
        let rect = self.image_rect();
        Pos2::new(
            rect.min.x + norm[0] * rect.width(),
            rect.min.y + norm[1] * rect.height(),
        )
    }

    /// Map a screen position to a normalized `[0, 1]` image point — the inverse of
    /// [`Self::image_norm_to_screen`]. Used to *read* the pointer (brush,
    /// eyedropper, handle drags). The raw mapping is returned unclamped so a
    /// caller can detect "off-image" (a coord outside `[0, 1]`); clamp at the call
    /// site that needs it.
    pub(crate) fn screen_to_image_norm(&self, pos: Pos2) -> [f32; 2] {
        let rect = self.image_rect();
        let w = rect.width().max(f32::EPSILON);
        let h = rect.height().max(f32::EPSILON);
        [(pos.x - rect.min.x) / w, (pos.y - rect.min.y) / h]
    }

    /// How many screen pixels a normalized length spans along each axis. A
    /// normalized length is a fraction of the image width (x) or height (y); the
    /// drawn image is non-square, so the two axes scale differently. Used to draw a
    /// radial ring or brush ring at the right on-screen size from a normalized
    /// radius (the engine measures radii in normalized units — elliptical on a
    /// non-square frame — so the on-screen ring is an ellipse with these two
    /// half-axes).
    pub(crate) fn norm_len_to_screen(&self, norm_len: f32) -> Vec2 {
        let rect = self.image_rect();
        Vec2::new(norm_len * rect.width(), norm_len * rect.height())
    }

    /// The inverse of [`Self::norm_len_to_screen`]: convert a screen-pixel length
    /// to a normalized length on each axis. Used to turn a screen-pixel hit
    /// tolerance (or a screen-pixel ring drag) back into image space without
    /// re-measuring off a `Response::rect`.
    pub(crate) fn screen_len_to_norm(&self, screen_len: f32) -> Vec2 {
        let rect = self.image_rect();
        Vec2::new(
            screen_len / rect.width().max(f32::EPSILON),
            screen_len / rect.height().max(f32::EPSILON),
        )
    }
}

/// Step `zoom` one notch in (`+1`) or out (`−1`) along the ladder, anchored at
/// `fit_scale` (so stepping in from `Fit` lands sensibly). The single home of the
/// ladder-stepping logic, shared by the keyboard, toolbar, and wheel paths.
pub(crate) fn step_zoom(zoom: Zoom, dir: i32, fit_scale: f32) -> Zoom {
    zoom.stepped(dir, fit_scale)
}

/// Convert a normalized `[0, 1]` image coordinate to a pixel index in an image of
/// `(w, h)` pixels, clamped to the last valid index. The shared
/// normalized→pixel-index conversion the hover readout (and later the clipping
/// read-back and the WB eyedropper) reuse.
pub(crate) fn norm_to_pixel(norm: [f32; 2], w: u32, h: u32) -> (u32, u32) {
    let nx = norm[0].clamp(0.0, 1.0);
    let ny = norm[1].clamp(0.0, 1.0);
    let px = (nx * (w.saturating_sub(1)) as f32).round() as u32;
    let py = (ny * (h.saturating_sub(1)) as f32).round() as u32;
    (px.min(w.saturating_sub(1)), py.min(h.saturating_sub(1)))
}

/// Convert a linear working-RGB image to a gamma-encoded egui texture, using the
/// exact output transform export uses ([`latent_export::to_srgb8`] — working→sRGB
/// matrix, highlight rolloff, sRGB OETF) so the preview matches the saved file.
pub(crate) fn to_color_image(img: &ImageBuf) -> egui::ColorImage {
    let bytes = latent_export::to_srgb8(img);
    egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], &bytes)
}

/// Show the central canvas. Until the first preview texture arrives, paints a
/// placeholder; once it's ready, fits the photo onto the neutral surround,
/// handles zoom/pan and brush painting (one undo step per stroke), optionally
/// shows the before/after, and samples the pixel under the cursor. The surround
/// changes only the area *around* the photo — the texture bytes are drawn
/// unaltered.
pub(crate) fn show(app: &mut App, ctx: &egui::Context) {
    let surround = egui::Frame::central_panel(&ctx.style()).fill(theme::CANVAS_SURROUND);

    // The canvas runs only with an open session (the welcome state owns the
    // central panel otherwise).
    let Some(session) = app.session.as_mut() else {
        return;
    };

    // The preview renders off-thread, so the texture is not ready on the first
    // frame(s). Until it arrives, show a placeholder rather than unwrapping a
    // `None` texture, and keep waiting for the worker.
    let Some(texture) = &session.texture else {
        egui::CentralPanel::default()
            .frame(surround)
            .show(ctx, |ui| {
                ui.centered_and_justified(|ui| ui.label("Rendering…"));
            });
        ctx.request_repaint();
        return;
    };
    let tex_id = texture.id();
    let tex_size = texture.size_vec2();
    let before_id = session.before_texture.as_ref().map(|t| t.id());
    let active = session.active;
    let local_sel = session.local_sel;
    let mut painted = false;
    // Cleared each frame; re-set below when the cursor is over the image.
    session.pixel_readout = None;

    egui::CentralPanel::default()
        .frame(surround)
        .show(ctx, |ui| {
            let panel = ui.available_rect_before_wrap();
            // A click-and-drag sense so the brush and the pan gestures both get
            // pointer events over the whole canvas.
            let resp = ui.allocate_rect(panel, egui::Sense::click_and_drag());

            // Pan and zoom run before the transform is built so this frame draws
            // at the updated view. Both only request a repaint — never a render.
            handle_pan(session, &resp);
            handle_zoom(session, ui, &resp, tex_size, panel);

            let transform = ViewTransform::new(tex_size, panel, session.zoom, session.pan);
            session.last_transform = Some(transform);

            // Paint the surround fill (the photo's neutral border) across the
            // whole panel, then draw the image fitted into its sub-rect.
            let painter = ui.painter_at(panel);
            painter.rect_filled(panel, 0.0, theme::CANVAS_SURROUND);
            let image_rect = transform.image_rect();
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));

            match (session.before, before_id) {
                // Split: clip the image rect down the middle and draw before on
                // the left half, after on the right. Both go through the one
                // transform, so a feature lines up across the seam.
                (BeforeAfter::Split, Some(bid)) => {
                    let mid = image_rect.center().x;
                    let left = image_rect.intersect(Rect::everything_left_of(mid));
                    let right = image_rect.intersect(Rect::everything_right_of(mid));
                    let left_uv = Rect::from_min_max(uv.min, Pos2::new(0.5, 1.0));
                    let right_uv = Rect::from_min_max(Pos2::new(0.5, 0.0), uv.max);
                    painter.image(bid, left, left_uv, Color32::WHITE);
                    painter.image(tex_id, right, right_uv, Color32::WHITE);
                    // The divider line down the seam.
                    painter.vline(
                        mid,
                        image_rect.y_range(),
                        egui::Stroke::new(1.0, theme::ACCENT),
                    );
                }
                // Toggle: draw the cached "before" in place of the live edit.
                (BeforeAfter::Toggle, Some(bid)) => {
                    painter.image(bid, image_rect, uv, Color32::WHITE);
                }
                // Off, or no before cached yet: the live preview.
                _ => {
                    painter.image(tex_id, image_rect, uv, Color32::WHITE);
                }
            }

            // The mask overlay (translucent red coverage), drawn over the image
            // but under the tool handles, so the user sees the selection. Pure
            // paint — never baked into the texture.
            tools::overlay::draw(session, &painter, &transform, active, local_sel);

            // Pixel readout: sample the rendered preview under the cursor when it
            // is over the image (not the gray surround). The shared pick-pixel
            // path the clipping read-back and the WB eyedropper reuse.
            if session.before == BeforeAfter::Off
                && let Some(pos) = resp.hover_pos()
            {
                sample_pixel_readout(session, &transform, pos);
            }

            // Route the pointer to the active tool: it draws its handles/guides
            // and consumes the drag when it grabs one, falling through to pan
            // otherwise. One undo step per drag (begin on grab, commit on
            // release). `dirty` is set when the tool changed the settings.
            let changed = tools::interact(session, &resp, &painter, &transform, active, local_sel);
            painted |= changed;
        });

    // Whether a tool is active, captured before the session borrow is released so
    // the render request below can reborrow the app.
    let tool_active = session.tool != tools::CanvasTool::None;

    // A tool gesture changed the settings after this frame's render; refresh the
    // preview and repaint so the edit shows up.
    if painted {
        app.render_preview(ctx);
        ctx.request_repaint();
    } else if tool_active {
        // A tool is active (drawing handles/cursor) — keep repainting so the
        // overlay tracks the pointer even when nothing changed this frame.
        ctx.request_repaint();
    }
}

/// Whether a pan gesture is currently active: middle-mouse drag, or space held
/// while left-dragging. Used to suppress brush painting during a pan.
fn panning(resp: &egui::Response) -> bool {
    let space = resp.ctx.input(|i| i.key_down(egui::Key::Space));
    resp.dragged_by(egui::PointerButton::Middle) || (space && resp.dragged())
}

/// Apply a pan gesture (middle-drag or space+left-drag) to the view pan. Pan is a
/// pure view change: it requests a repaint, never a render.
fn handle_pan(session: &mut Session, resp: &egui::Response) {
    // Pan is inert when the whole image fits — there's nothing off-screen to
    // bring into view.
    if matches!(session.zoom, Zoom::Fit) {
        session.pan = Vec2::ZERO;
        return;
    }
    if panning(resp) {
        session.pan += resp.drag_delta();
        resp.ctx.request_repaint();
    }
}

/// Handle wheel-zoom (anchored at the cursor) over the canvas. Stepping the zoom
/// adjusts the pan so the image point under the cursor stays under the cursor.
/// Like pan, this only requests a repaint — the texture is unchanged, so no
/// render is spawned.
fn handle_zoom(
    session: &mut Session,
    ui: &egui::Ui,
    resp: &egui::Response,
    tex_size: Vec2,
    panel: Rect,
) {
    if !resp.hovered() {
        return;
    }
    let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
    if scroll_y.abs() < 0.5 {
        return;
    }
    let Some(cursor) = resp.hover_pos() else {
        return;
    };
    // Capture the normalized image point under the cursor before the zoom change.
    let before = ViewTransform::new(tex_size, panel, session.zoom, session.pan);
    let anchor = before.screen_to_image_norm(cursor);
    let fit_scale = ViewTransform::compute_fit_scale(tex_size, panel.size());

    let dir = if scroll_y > 0.0 { 1 } else { -1 };
    session.zoom = step_zoom(session.zoom, dir, fit_scale);
    // Re-anchor: set the pan so the same normalized point maps back to the cursor.
    let after = ViewTransform::new(tex_size, panel, session.zoom, session.pan);
    let landed = after.image_norm_to_screen(anchor);
    session.pan += cursor - landed;
    ui.ctx().request_repaint();
}

/// Sample the rendered-preview pixel under `pos` (when over the image) into the
/// app's pixel readout, reading through the one transform and from the stashed
/// preview `ImageBuf` — the shared pick-pixel substrate.
fn sample_pixel_readout(session: &mut Session, transform: &ViewTransform, pos: Pos2) {
    let norm = transform.screen_to_image_norm(pos);
    // Off-image (over the gray surround): leave the readout cleared.
    if norm[0] < 0.0 || norm[0] > 1.0 || norm[1] < 0.0 || norm[1] > 1.0 {
        return;
    }
    let Some(img) = &session.preview_rendered else {
        return;
    };
    let (px, py) = norm_to_pixel(norm, img.width(), img.height());
    let Some(linear) = img.try_get(px, py) else {
        return;
    };
    // Convert the linear working pixel to the same sRGB bytes the texture shows,
    // so the readout matches the screen exactly.
    let mut one = ImageBuf::new(1, 1);
    one.set(0, 0, linear);
    let rgb = latent_export::to_srgb8(&one);
    session.pixel_readout = Some(PixelReadout {
        x: px,
        y: py,
        rgb: [rgb[0], rgb[1], rgb[2]],
    });
}

/// The pixel sampled under the cursor, surfaced in the status bar. `(x, y)` is the
/// preview-pixel index; `rgb` is the sRGB display value the user sees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PixelReadout {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) rgb: [u8; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_color_image_matches_the_export_transform() {
        // The preview must go through the same output transform as a saved file
        // (working→sRGB matrix + highlight rolloff + sRGB OETF). Neutrals stay
        // neutral, so the values match the export tests: 0.5 → 188, and display
        // white 1.0 rolls off to 254 (not a bare 255).
        let mut img = ImageBuf::new(3, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]); // black
        img.set(1, 0, [0.5, 0.5, 0.5]); // mid-gray (below the knee, faithful)
        img.set(2, 0, [1.0, 1.0, 1.0]); // display white (rolled off)

        let ci = to_color_image(&img);
        assert_eq!(ci.size, [3, 1]);
        assert_eq!(ci.pixels[0], egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(ci.pixels[1], egui::Color32::from_rgb(188, 188, 188));
        assert_eq!(ci.pixels[2], egui::Color32::from_rgb(254, 254, 254));
    }

    #[test]
    fn view_transform_round_trips() {
        // For a few image/panel shapes, screen↔image must invert within f32
        // epsilon, the image corners must map to the fitted rect's corners, and a
        // point in the gray surround must map outside [0, 1].
        let cases = [
            // (image, panel) — square in a wide panel, portrait in a square, and
            // landscape in a tall panel, so letterboxing happens on both axes.
            (
                Vec2::new(100.0, 100.0),
                Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(400.0, 200.0)),
            ),
            (
                Vec2::new(60.0, 90.0),
                Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(300.0, 300.0)),
            ),
            (
                Vec2::new(160.0, 90.0),
                Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(200.0, 400.0)),
            ),
        ];
        for (image, panel) in cases {
            let t = ViewTransform::new(image, panel, Zoom::Fit, Vec2::ZERO);
            for &p in &[[0.0, 0.0], [1.0, 1.0], [0.3, 0.7], [0.85, 0.15]] {
                let screen = t.image_norm_to_screen(p);
                let back = t.screen_to_image_norm(screen);
                assert!(
                    (back[0] - p[0]).abs() < 1e-4 && (back[1] - p[1]).abs() < 1e-4,
                    "round-trip failed for {p:?}: got {back:?}"
                );
            }
            // Corners map to the fitted rect's corners.
            let rect = t.image_rect();
            let tl = t.image_norm_to_screen([0.0, 0.0]);
            let br = t.image_norm_to_screen([1.0, 1.0]);
            assert!((tl - rect.min).length() < 1e-3);
            assert!((br - rect.max).length() < 1e-3);
            // The fitted rect stays inside the panel (letterboxed, not cropped).
            assert!(panel.contains_rect(rect.shrink(0.01)));
        }

        // Letterbox: a point in the surround maps to a normalized coord outside
        // [0, 1]. A square image in a wide panel leaves gray on the left/right;
        // a point at the panel's left edge is left of the image.
        let image = Vec2::new(100.0, 100.0);
        let panel = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(400.0, 200.0));
        let t = ViewTransform::new(image, panel, Zoom::Fit, Vec2::ZERO);
        let surround_pt = Pos2::new(panel.min.x + 1.0, panel.center().y);
        let n = t.screen_to_image_norm(surround_pt);
        assert!(n[0] < 0.0, "a surround point should map left of the image");
    }

    #[test]
    fn wheel_zoom_keeps_cursor_anchored() {
        // Zooming toward the cursor must keep the normalized image point under the
        // cursor fixed: capture it before, step the zoom, re-anchor the pan, and
        // assert it maps back to (about) the same screen position.
        let image = Vec2::new(160.0, 90.0);
        let panel = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 600.0));
        let cursor = Pos2::new(520.0, 410.0);

        let mut zoom = Zoom::Fit;
        let mut pan = Vec2::ZERO;
        let fit_scale = ViewTransform::compute_fit_scale(image, panel.size());

        let before = ViewTransform::new(image, panel, zoom, pan);
        let anchor = before.screen_to_image_norm(cursor);

        // Step the zoom in and re-anchor exactly as `handle_zoom` does.
        zoom = zoom.stepped(1, fit_scale);
        let after = ViewTransform::new(image, panel, zoom, pan);
        let landed = after.image_norm_to_screen(anchor);
        pan += cursor - landed;

        let anchored = ViewTransform::new(image, panel, zoom, pan);
        let back = anchored.image_norm_to_screen(anchor);
        assert!(
            (back - cursor).length() < 1e-2,
            "cursor anchor drifted: {back:?} vs {cursor:?}"
        );
    }

    #[test]
    fn norm_to_pixel_round_trips() {
        // Normalized [0, 1] → preview-pixel index: the corners hit the first and
        // last pixel, the center lands mid-image, and the conversion is clamped to
        // the last valid index (never out of bounds).
        let (w, h) = (1600, 900);
        assert_eq!(norm_to_pixel([0.0, 0.0], w, h), (0, 0));
        assert_eq!(norm_to_pixel([1.0, 1.0], w, h), (w - 1, h - 1));
        assert_eq!(norm_to_pixel([0.5, 0.5], w, h), (800, 450)); // rounds to nearest
        // Out-of-range input clamps rather than overflowing.
        assert_eq!(norm_to_pixel([2.0, -1.0], w, h), (w - 1, 0));
        // A 1×1 image never produces a nonzero index.
        assert_eq!(norm_to_pixel([0.9, 0.9], 1, 1), (0, 0));
    }

    #[test]
    fn zoom_ladder_steps_and_clamps() {
        // Stepping in from 100% goes to 150%, out goes to 67%; the ends clamp.
        assert_eq!(Zoom::Percent(1.0).stepped(1, 0.5), Zoom::Percent(1.5));
        assert_eq!(Zoom::Percent(1.0).stepped(-1, 0.5), Zoom::Percent(0.67));
        assert_eq!(Zoom::Percent(8.0).stepped(1, 0.5), Zoom::Percent(8.0));
        assert_eq!(Zoom::Percent(0.25).stepped(-1, 0.5), Zoom::Percent(0.25));
    }
}
