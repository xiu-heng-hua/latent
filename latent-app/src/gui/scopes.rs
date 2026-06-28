//! Image scopes drawn over the **display-referred** preview bytes: a live RGB +
//! luma histogram, a clipping read-out, and an optional column-wise waveform /
//! RGB parade. Everything here reads the exact sRGB8 bytes
//! [`latent_export::to_srgb8`] produces (the same `Vec<u8>` the preview texture
//! is uploaded from), so what a scope shows is what the saved file will be, and a
//! pixel a scope calls "clipped" is a pixel that clips in the export.
//!
//! The bins (and the clip mask) are **cached and recomputed only when a new
//! preview lands** — see the recompute in `Session::load_texture`. The per-frame
//! draw never recomputes; it only paints the cached data. All drawing is
//! hand-rolled with [`egui::Painter`] (no plotting crate), mirroring the curve
//! editor's `allocate_painter` + `line_segment` / `rect_filled` approach.
//!
//! Why display-referred and not the working-linear pixels: the working space is
//! linear-light, wide-gamut, and a working value of `1.0` is **not** the clip
//! point — the output transform applies a working→sRGB matrix, a hue-preserving
//! highlight rolloff, and the sRGB OETF before clamping. A histogram of the
//! working data would mis-place every bin and flag the wrong pixels. The scopes
//! therefore bin the post-transform display bytes so they line up with the file
//! and with each other.

use eframe::egui;
use egui::{Color32, ColorImage, Rect, Stroke};

use super::canvas::ViewTransform;
use super::theme;

/// Rec. 709 luma weights, applied to the **display** sRGB bytes to answer "how
/// bright does this output pixel look". These match
/// [`latent_edit::select_luma`]'s weights deliberately (the "how bright is this
/// pixel" estimate for non-working-space data) — **not**
/// `latent_image::color::LUMA_WEIGHTS`, which is the colorimetric luminance of
/// *linear working* space (its near-zero blue term would read pure display-blue
/// as near-black, wrong for a display-byte histogram).
const LUMA_R: f32 = 0.2126;
const LUMA_G: f32 = 0.7152;
const LUMA_B: f32 = 0.0722;

/// Highlight-clip threshold on the display bytes: a channel is a blown highlight
/// at `>= 254`. Display white rolls off to **254** (not a bare 255) under the
/// output transform's highlight knee, so 254 is the honest "this is as bright as
/// the file goes" code; values of 255 only come from working headroom above 1.0.
/// Matched to the preview/export white code so the overlay and the histogram's
/// top bins agree by construction.
pub(crate) const CLIP_HIGH: u8 = 254;

/// Shadow-clip threshold on the display bytes: a channel is a crushed shadow at
/// `<= 1` (i.e. byte 0 or 1 — pure black plus the first code above it, where
/// detail is gone). Matched to the histogram's bottom bins.
pub(crate) const CLIP_LOW: u8 = 1;

/// Per-channel 256-bin counts over the display sRGB bytes, plus a Rec. 709 luma
/// histogram of the same bytes. Each array is indexed by the byte value (the
/// channel binning is a byte→bin identity: bin `= byte`), so a bin holds the
/// number of pixels whose channel landed on that display code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Histogram {
    pub(crate) r: [u32; 256],
    pub(crate) g: [u32; 256],
    pub(crate) b: [u32; 256],
    /// Rec. 709 luma of the display bytes, rounded to the nearest code.
    pub(crate) luma: [u32; 256],
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            r: [0; 256],
            g: [0; 256],
            b: [0; 256],
            luma: [0; 256],
        }
    }
}

/// Bin the display sRGB bytes (`RGBRGB…`, row-major — exactly what
/// [`latent_export::to_srgb8`] returns) into a [`Histogram`].
///
/// The R/G/B binning is an exact byte→bin identity (`bins[byte] += 1`) — 256
/// display codes map one-to-one to 256 bins, no float math, no scaling. Luma is
/// `round(0.2126·r + 0.7152·g + 0.0722·b)` on the **display** bytes (the Rec. 709
/// weights — the brightness of the *output* value, not a re-linearized
/// luminance), clamped to `0..=255` for the bin index.
///
/// Pure and total: it walks `rgb8.chunks_exact(3)`, so any input — including a
/// length that is not a multiple of 3 — is handled without panicking, and only
/// complete triplets are counted (a stray trailing byte is ignored). The buffer
/// is always `len()·3` from `to_srgb8`, but the guard keeps the function safe for
/// any slice a test throws at it.
pub(crate) fn histogram_bins(rgb8: &[u8]) -> Histogram {
    let mut h = Histogram::default();
    for px in rgb8.chunks_exact(3) {
        let (r, g, b) = (px[0], px[1], px[2]);
        h.r[r as usize] += 1;
        h.g[g as usize] += 1;
        h.b[b as usize] += 1;
        let luma = LUMA_R * r as f32 + LUMA_G * g as f32 + LUMA_B * b as f32;
        let bin = (luma + 0.5) as usize; // round-to-nearest; always in 0..=255
        h.luma[bin.min(255)] += 1;
    }
    h
}

/// Whether a display triplet is a blown highlight: **any** channel at or above
/// [`CLIP_HIGH`]. "Any channel" is the common photo behavior (a clipped red on a
/// hot sky is flagged even though green/blue are not), and it is what makes the
/// overlay population equal the histogram's top bins (`>= CLIP_HIGH`) summed over
/// channels rather than only fully-white pixels.
pub(crate) fn is_highlight_clipped(rgb: [u8; 3]) -> bool {
    rgb[0] >= CLIP_HIGH || rgb[1] >= CLIP_HIGH || rgb[2] >= CLIP_HIGH
}

/// Whether a display triplet is a crushed shadow: **any** channel at or below
/// [`CLIP_LOW`] (the any-channel convention, symmetric with the highlight test).
pub(crate) fn is_shadow_clipped(rgb: [u8; 3]) -> bool {
    rgb[0] <= CLIP_LOW || rgb[1] <= CLIP_LOW || rgb[2] <= CLIP_LOW
}

/// Which scope the panel draws. The histogram is the default; the waveform and
/// the RGB parade are alternate readouts of the same cached display bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScopeKind {
    /// The RGB + luma histogram (the default).
    #[default]
    Histogram,
    /// A column-wise waveform: x = image column, y = brightness, intensity = how
    /// many pixels in that column hit that value.
    Waveform,
    /// Three side-by-side waveforms, one per channel (R, G, B).
    Parade,
}

/// The cached scope state on a [`super::app::Session`]: the binned histogram, the
/// column waveform buckets, the clipping-overlay texture, the chosen scope type,
/// and the two transient clip toggles. The bins/waveform/overlay are rebuilt only
/// when a new preview lands (in `load_texture`); the toggles and the scope type
/// are pure UI state. None of this is persisted (it is view state, not a setting).
#[derive(Default)]
pub(crate) struct Scopes {
    /// The cached histogram for the current preview, or `None` before the first
    /// preview lands.
    pub(crate) histogram: Option<Histogram>,
    /// The cached column waveform for the current preview (built alongside the
    /// histogram). One [`Waveform`] holds all three channels' buckets.
    pub(crate) waveform: Option<Waveform>,
    /// The blown-highlight overlay texture for the current preview: transparent
    /// except the highlight color where a pixel blows a highlight. Built once per
    /// preview and drawn stretched over the image rect only when the highlight
    /// toggle is on — toggling never rebuilds or re-renders.
    pub(crate) highlight_overlay: Option<egui::TextureHandle>,
    /// The crushed-shadow overlay texture for the current preview (transparent
    /// except the shadow color where a pixel crushes a shadow). Same lifecycle as
    /// [`Self::highlight_overlay`].
    pub(crate) shadow_overlay: Option<egui::TextureHandle>,
    /// Which scope the panel shows.
    pub(crate) kind: ScopeKind,
    /// Whether to mark blown highlights on the canvas (right end-cap / checkbox).
    pub(crate) show_highlight_clip: bool,
    /// Whether to mark crushed shadows on the canvas (left end-cap / checkbox).
    pub(crate) show_shadow_clip: bool,
}

impl Scopes {
    /// Recompute the cached scopes from a fresh preview's **display bytes** (the
    /// `to_srgb8` output the texture is uploaded from) — the once-per-preview hook
    /// called from `load_texture`. Rebuilds the histogram, the column waveform,
    /// and the clipping-overlay texture from the *same* byte pass, so all three
    /// agree with the texture and the file. Never called per frame.
    pub(crate) fn recompute(
        &mut self,
        ctx: &egui::Context,
        rgb8: &[u8],
        width: usize,
        height: usize,
    ) {
        self.histogram = Some(histogram_bins(rgb8));
        self.waveform = Some(waveform_buckets(rgb8, width, WAVEFORM_BUCKETS));
        // Two independent overlay textures (highlights, shadows), each built once
        // here from the same display bytes, so a clip toggle only flips which is
        // drawn — never a rebuild or a re-render.
        let (high, low) = clip_overlay_images(rgb8, width, height);
        self.highlight_overlay =
            Some(ctx.load_texture("clip_highlights", high, egui::TextureOptions::NEAREST));
        self.shadow_overlay =
            Some(ctx.load_texture("clip_shadows", low, egui::TextureOptions::NEAREST));
    }

    /// Draw the enabled clipping overlays onto the image through the shared
    /// [`ViewTransform`] (the one screen↔image map — no independent math here).
    /// Pure paint: the rendered preview and the export are untouched, and toggling
    /// an overlay does not re-render — it only changes what is drawn this frame.
    pub(crate) fn draw_clip_overlay(&self, painter: &egui::Painter, transform: &ViewTransform) {
        let rect = transform.image_rect();
        let uv = Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        if self.show_highlight_clip
            && let Some(tex) = &self.highlight_overlay
        {
            painter.image(tex.id(), rect, uv, Color32::WHITE);
        }
        if self.show_shadow_clip
            && let Some(tex) = &self.shadow_overlay
        {
            painter.image(tex.id(), rect, uv, Color32::WHITE);
        }
    }
}

// ---------------------------------------------------------------------------
// Clipping overlay (display-referred mask, built once per preview).
// ---------------------------------------------------------------------------

/// Warning color for blown highlights (saturated red).
const HIGHLIGHT_COLOR: Color32 = Color32::from_rgb(230, 40, 40);
/// Warning color for crushed shadows (saturated blue).
const SHADOW_COLOR: Color32 = Color32::from_rgb(60, 120, 230);

/// Build the two clipping-overlay images at the preview's size from the display
/// bytes: the first is transparent except the highlight color where a pixel blows
/// a highlight; the second is transparent except the shadow color where a pixel
/// crushes a shadow. Two independent textures (rather than one combined) let each
/// overlay be toggled on the canvas without a rebuild — the canvas draws each only
/// when its toggle is on.
fn clip_overlay_images(rgb8: &[u8], width: usize, height: usize) -> (ColorImage, ColorImage) {
    let mut high = vec![Color32::TRANSPARENT; width * height];
    let mut low = vec![Color32::TRANSPARENT; width * height];
    for (i, px) in rgb8.chunks_exact(3).enumerate().take(width * height) {
        let rgb = [px[0], px[1], px[2]];
        if is_highlight_clipped(rgb) {
            high[i] = HIGHLIGHT_COLOR;
        }
        if is_shadow_clipped(rgb) {
            low[i] = SHADOW_COLOR;
        }
    }
    let image = |pixels: Vec<Color32>| ColorImage {
        size: [width, height],
        pixels,
        source_size: egui::vec2(width as f32, height as f32),
    };
    (image(high), image(low))
}

// ---------------------------------------------------------------------------
// Waveform (column-wise, built once per preview).
// ---------------------------------------------------------------------------

/// How many column buckets the waveform accumulates into. The preview is up to
/// 1600px wide and the scope panel is far narrower, so source columns are mapped
/// onto this many buckets to keep the accumulator small (a waveform is a 2-D
/// `[buckets][256]` table — heavier than a 1-D histogram — so it especially must
/// not run per frame).
const WAVEFORM_BUCKETS: usize = 256;

/// A column-wise waveform over the display bytes: for each of `buckets` column
/// buckets, a 256-entry brightness distribution per channel. `cell(bucket, value)`
/// holds how many pixels in that column band landed on that display code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Waveform {
    /// Number of column buckets (x resolution).
    pub(crate) buckets: usize,
    /// `[R, G, B]`, each a flat `buckets * 256` table indexed `bucket * 256 + value`.
    pub(crate) channels: [Vec<u32>; 3],
}

/// Accumulate a column-wise waveform from the display bytes. `width` is the source
/// image width in pixels; source columns are bucketed into `buckets` bands so the
/// accumulator stays small regardless of the preview width.
///
/// Pure and total (walks `chunks_exact(3)`, ignores a ragged tail), so the
/// accumulation is unit-testable without a window — exactly like
/// [`histogram_bins`].
pub(crate) fn waveform_buckets(rgb8: &[u8], width: usize, buckets: usize) -> Waveform {
    let buckets = buckets.max(1);
    let width = width.max(1);
    let mut channels: [Vec<u32>; 3] = std::array::from_fn(|_| vec![0u32; buckets * 256]);
    for (i, px) in rgb8.chunks_exact(3).enumerate() {
        // The pixel's column, then which bucket it falls in.
        let col = i % width;
        let bucket = (col * buckets / width).min(buckets - 1);
        for c in 0..3 {
            let value = px[c] as usize;
            channels[c][bucket * 256 + value] += 1;
        }
    }
    Waveform { buckets, channels }
}

// ---------------------------------------------------------------------------
// Painter draws (per-frame paint only — never a recompute).
// ---------------------------------------------------------------------------

/// Fixed scope-panel height (the drawing area below the controls).
const SCOPE_HEIGHT: f32 = 120.0;
/// Dark surface the scope is drawn on.
const SCOPE_BG: Color32 = Color32::from_gray(18);
/// Width of the clickable end-cap toggle zones on the histogram (left = shadows,
/// right = highlights), in screen pixels.
const END_CAP_WIDTH: f32 = 14.0;

/// The scope area at the top of the controls panel: a scope-type selector, the
/// clip checkboxes, and the chosen scope drawn with the painter. Reads only the
/// cached [`Scopes`] — it never recomputes the bins. Returns nothing; clip
/// toggles are pure view state (no re-render).
pub(crate) fn scope_block(ui: &mut egui::Ui, scopes: &mut Scopes) {
    // Scope-type selector (histogram is the default; waveform/parade are alternate
    // readouts of the same cached display bytes).
    ui.horizontal(|ui| {
        ui.selectable_value(&mut scopes.kind, ScopeKind::Histogram, "Histogram");
        ui.selectable_value(&mut scopes.kind, ScopeKind::Waveform, "Waveform");
        ui.selectable_value(&mut scopes.kind, ScopeKind::Parade, "Parade");
    });

    let size = egui::vec2(ui.available_width(), SCOPE_HEIGHT);
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = resp.rect;
    painter.rect_filled(rect, theme::CORNER_RADIUS, SCOPE_BG);

    match scopes.kind {
        ScopeKind::Histogram => {
            if let Some(h) = &scopes.histogram {
                draw_histogram(&painter, rect, h);
            }
            // The clickable end-caps live only under the histogram (the Lightroom
            // triangle affordance): left toggles shadows, right toggles highlights.
            draw_clip_end_caps(ui, scopes, rect);
        }
        ScopeKind::Waveform => {
            if let Some(w) = &scopes.waveform {
                draw_waveform(&painter, rect, w, &[0, 1, 2]);
            }
        }
        ScopeKind::Parade => {
            if let Some(w) = &scopes.waveform {
                draw_parade(&painter, rect, w);
            }
        }
    }

    // The clip toggles, mirrored as checkboxes (the end-caps are the on-canvas
    // affordance; these are the explicit controls). Both are transient view state.
    ui.horizontal(|ui| {
        ui.checkbox(&mut scopes.show_shadow_clip, "Shadows")
            .on_hover_text("Mark crushed shadows (display ≤ 1) on the image");
        ui.checkbox(&mut scopes.show_highlight_clip, "Highlights")
            .on_hover_text("Mark blown highlights (display ≥ 254) on the image");
    });
}

/// Map a bin count to a bar height fraction with a `√` vertical scale, which reads
/// better than linear for photographic data (the long tail of mid-tones would
/// otherwise vanish next to a peak). `max` is the tallest bin among the drawn
/// channels; a zero `max` (empty image) flattens to the baseline with no
/// division.
fn bar_fraction(count: u32, max: u32) -> f32 {
    if max == 0 {
        return 0.0;
    }
    (count as f32 / max as f32).sqrt()
}

/// Draw the RGB + luma histogram into `rect`: R/G/B as additive translucent fills
/// (so overlapping channels read as their mix, the way photo histograms do) and
/// luma as a light outline polyline. Heights are normalized to the tallest bin
/// across the three color channels, on a `√` scale. Pure paint.
fn draw_histogram(painter: &egui::Painter, rect: Rect, h: &Histogram) {
    // The height scale is the tallest color-channel bin; bin 0/255 spikes (large
    // flat black/white regions) are excluded from the *scale* so the body of the
    // distribution stays visible rather than being crushed by an end spike.
    let max = channel_scale_max(h);

    let fill = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 70);
    draw_channel_fill(
        painter,
        rect,
        &h.r,
        max,
        fill(Color32::from_rgb(220, 60, 60)),
    );
    draw_channel_fill(
        painter,
        rect,
        &h.g,
        max,
        fill(Color32::from_rgb(60, 200, 60)),
    );
    draw_channel_fill(
        painter,
        rect,
        &h.b,
        max,
        fill(Color32::from_rgb(80, 120, 230)),
    );
    // Luma as a light outline over the fills.
    draw_channel_outline(
        painter,
        rect,
        &h.luma,
        max,
        Stroke::new(1.0, Color32::from_gray(210)),
    );
}

/// The bar-height scale for the histogram: the tallest bin across R/G/B, ignoring
/// the two extreme bins (0 and 255) so a large pure-black or pure-white region
/// doesn't flatten the rest of the distribution. Falls back to the full max if
/// the interior is empty.
fn channel_scale_max(h: &Histogram) -> u32 {
    let interior = |bins: &[u32; 256]| bins[1..255].iter().copied().max().unwrap_or(0);
    let m = interior(&h.r).max(interior(&h.g)).max(interior(&h.b));
    if m > 0 {
        m
    } else {
        // Degenerate (all mass in the end bins): fall back to the overall max so
        // the shape is still visible rather than flat.
        h.r.iter()
            .chain(&h.g)
            .chain(&h.b)
            .copied()
            .max()
            .unwrap_or(0)
    }
}

/// Draw one channel as a filled area: a translucent column per bin, rising to the
/// bin's `√`-scaled height. Additive translucent fills let overlapping channels
/// read as their mix.
fn draw_channel_fill(
    painter: &egui::Painter,
    rect: Rect,
    bins: &[u32; 256],
    max: u32,
    color: Color32,
) {
    let bin_w = rect.width() / 256.0;
    for (i, &count) in bins.iter().enumerate() {
        let frac = bar_fraction(count, max);
        if frac <= 0.0 {
            continue;
        }
        let x = rect.left() + i as f32 * bin_w;
        let top = rect.bottom() - frac * rect.height();
        let bar = Rect::from_min_max(
            egui::pos2(x, top),
            egui::pos2(x + bin_w.max(1.0), rect.bottom()),
        );
        painter.rect_filled(bar, 0.0, color);
    }
}

/// Draw one channel as an outline polyline (used for luma), `√`-scaled like the
/// fills so it sits over them at the right height.
fn draw_channel_outline(
    painter: &egui::Painter,
    rect: Rect,
    bins: &[u32; 256],
    max: u32,
    stroke: Stroke,
) {
    let bin_w = rect.width() / 256.0;
    let pts: Vec<egui::Pos2> = bins
        .iter()
        .enumerate()
        .map(|(i, &count)| {
            let x = rect.left() + (i as f32 + 0.5) * bin_w;
            let y = rect.bottom() - bar_fraction(count, max) * rect.height();
            egui::pos2(x, y)
        })
        .collect();
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
}

/// Lay out and handle the two clickable end-cap toggles over the histogram rect:
/// a thin interactive zone at the left edge toggles shadow clipping, one at the
/// right edge toggles highlight clipping. A faint triangle marks each, brighter
/// when its overlay is on. Toggling is pure view state (no render).
fn draw_clip_end_caps(ui: &mut egui::Ui, scopes: &mut Scopes, rect: Rect) {
    let left = Rect::from_min_max(
        rect.left_top(),
        egui::pos2(rect.left() + END_CAP_WIDTH, rect.bottom()),
    );
    let right = Rect::from_min_max(
        egui::pos2(rect.right() - END_CAP_WIDTH, rect.top()),
        rect.right_bottom(),
    );

    let left_resp = ui
        .interact(left, ui.id().with("clip_shadows"), egui::Sense::click())
        .on_hover_text("Toggle crushed-shadow markers on the image");
    if left_resp.clicked() {
        scopes.show_shadow_clip = !scopes.show_shadow_clip;
    }
    let right_resp = ui
        .interact(right, ui.id().with("clip_highlights"), egui::Sense::click())
        .on_hover_text("Toggle blown-highlight markers on the image");
    if right_resp.clicked() {
        scopes.show_highlight_clip = !scopes.show_highlight_clip;
    }

    // A small corner triangle marks each cap, brighter when its overlay is active.
    let painter = ui.painter_at(rect);
    let lo = |on: bool, c: Color32| if on { c } else { c.gamma_multiply(0.4) };
    painter.add(egui::Shape::convex_polygon(
        vec![
            left.left_top(),
            egui::pos2(left.left() + END_CAP_WIDTH, left.top()),
            left.left_top() + egui::vec2(0.0, END_CAP_WIDTH),
        ],
        lo(scopes.show_shadow_clip, SHADOW_COLOR),
        Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            right.right_top(),
            egui::pos2(right.right() - END_CAP_WIDTH, right.top()),
            right.right_top() + egui::vec2(0.0, END_CAP_WIDTH),
        ],
        lo(scopes.show_highlight_clip, HIGHLIGHT_COLOR),
        Stroke::NONE,
    ));
}

/// Draw a waveform for the given `channels` into `rect`, each channel tinted: for
/// every column bucket, plot a translucent dot per occupied brightness value with
/// alpha tracking the (`√`-scaled) count. Multiple channels overlay additively
/// (so the full waveform reads white where R/G/B coincide). Pure paint.
fn draw_waveform(painter: &egui::Painter, rect: Rect, w: &Waveform, channels: &[usize]) {
    let col_w = rect.width() / w.buckets as f32;
    let cell_h = rect.height() / 256.0;
    let max = waveform_max(w, channels);
    let tints = [
        Color32::from_rgb(230, 90, 90),
        Color32::from_rgb(90, 220, 90),
        Color32::from_rgb(110, 140, 240),
    ];
    for &c in channels {
        let table = &w.channels[c];
        for bucket in 0..w.buckets {
            let x = rect.left() + bucket as f32 * col_w;
            for value in 0..256usize {
                let count = table[bucket * 256 + value];
                if count == 0 {
                    continue;
                }
                let a = (bar_fraction(count, max) * 220.0) as u8;
                if a == 0 {
                    continue;
                }
                // y is inverted: bright values sit at the top.
                let y = rect.bottom() - (value as f32 + 0.5) * cell_h;
                let cell = Rect::from_min_size(
                    egui::pos2(x, y - cell_h),
                    egui::vec2(col_w.max(1.0), cell_h.max(1.0)),
                );
                let base = tints[c];
                painter.rect_filled(
                    cell,
                    0.0,
                    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a),
                );
            }
        }
    }
}

/// Draw an RGB parade: three waveforms side by side (R, then G, then B), each in
/// its own third of `rect`.
fn draw_parade(painter: &egui::Painter, rect: Rect, w: &Waveform) {
    let third = rect.width() / 3.0;
    for (i, c) in [0usize, 1, 2].into_iter().enumerate() {
        let sub = Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * third, rect.top()),
            egui::vec2(third, rect.height()),
        );
        draw_waveform(painter, sub, w, &[c]);
    }
}

/// The intensity scale for a waveform draw: the tallest cell count across the
/// drawn channels, so the `√`-scaled alpha spans the data.
fn waveform_max(w: &Waveform, channels: &[usize]) -> u32 {
    channels
        .iter()
        .flat_map(|&c| w.channels[c].iter().copied())
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_bins_counts_channels_and_luma() {
        // One black, one white, one mid-gray, one saturated-red pixel. The R/G/B
        // bins must land in the exact code slots (bin = byte), the luma of pure
        // red is round(0.2126·255) = 54 (the Rec. 709 weight choice — it would
        // differ under the linear-working `color::LUMA_WEIGHTS`), and the totals
        // equal the pixel count.
        #[rustfmt::skip]
        let rgb8: &[u8] = &[
            0, 0, 0,        // black
            255, 255, 255,  // white
            128, 128, 128,  // mid
            255, 0, 0,      // saturated red
        ];
        let h = histogram_bins(rgb8);

        // R: one each at 0, 128, 255 (black, mid) plus white(255) and red(255).
        assert_eq!(h.r[0], 1); // black
        assert_eq!(h.r[128], 1); // mid
        assert_eq!(h.r[255], 2); // white + red
        // G/B: black(0), mid(128), white(255), and red contributes a 0.
        assert_eq!(h.g[0], 2); // black + red
        assert_eq!(h.g[128], 1);
        assert_eq!(h.g[255], 1); // white
        assert_eq!(h.b[0], 2); // black + red
        assert_eq!(h.b[255], 1);

        // Luma: black → 0, white → 255, mid → 128, red → round(0.2126·255) = 54.
        assert_eq!(h.luma[0], 1); // black
        assert_eq!(h.luma[255], 1); // white
        assert_eq!(h.luma[128], 1); // mid (0.2126+0.7152+0.0722)·128 = 128
        assert_eq!(h.luma[54], 1); // saturated red, Rec. 709
        // The Rec. 709 red luma must be 54, not what LUMA_WEIGHTS would give.
        assert_eq!((LUMA_R * 255.0 + 0.5) as usize, 54);

        // Every channel and luma total equals the four pixels.
        assert_eq!(h.r.iter().sum::<u32>(), 4);
        assert_eq!(h.g.iter().sum::<u32>(), 4);
        assert_eq!(h.b.iter().sum::<u32>(), 4);
        assert_eq!(h.luma.iter().sum::<u32>(), 4);
    }

    #[test]
    fn histogram_bins_handles_ragged_input() {
        // A slice whose length is not a multiple of 3 must not panic, and the
        // complete triplets are still counted (the trailing stray bytes ignored).
        let rgb8: &[u8] = &[10, 20, 30, 40, 50, 60, 70, 80]; // two triplets + 2 stray
        let h = histogram_bins(rgb8);
        assert_eq!(h.r.iter().sum::<u32>(), 2);
        assert_eq!(h.g.iter().sum::<u32>(), 2);
        assert_eq!(h.b.iter().sum::<u32>(), 2);
        assert_eq!(h.r[10], 1);
        assert_eq!(h.r[40], 1);
        assert_eq!(h.r[70], 0); // the stray 70 was not a complete triplet
        // An empty slice yields all-zero bins (no panic, no division later).
        let empty = histogram_bins(&[]);
        assert_eq!(empty.r.iter().sum::<u32>(), 0);
        assert_eq!(empty, Histogram::default());
    }

    #[test]
    fn clip_predicates_match_thresholds() {
        // The any-channel clip predicates fire exactly at the documented bytes.
        assert!(is_highlight_clipped([254, 0, 0])); // a single hot channel
        assert!(is_highlight_clipped([255, 255, 255]));
        assert!(!is_highlight_clipped([253, 253, 253])); // just below the knee code
        assert!(is_shadow_clipped([1, 200, 200])); // a single crushed channel
        assert!(is_shadow_clipped([0, 0, 0]));
        assert!(!is_shadow_clipped([2, 2, 2])); // just above the floor
    }

    #[test]
    fn clip_mask_matches_histogram_ends() {
        // The overlay and the histogram read the SAME display-referred bytes: the
        // set of pixels the clip predicate flags must equal the population of the
        // histogram's end bins. A hand-built buffer with known clipped pixels:
        #[rustfmt::skip]
        let rgb8: &[u8] = &[
            255, 255, 255,  // highlight (all channels)
            254, 10, 10,    // highlight (one channel ≥ 254)
            0, 0, 0,        // shadow (all channels)
            5, 1, 200,      // shadow (one channel ≤ 1)
            128, 128, 128,  // neither
            200, 50, 60,    // neither
        ];
        let h = histogram_bins(rgb8);

        // Count pixels each predicate flags by walking the same bytes.
        let mut hi_pixels = 0u32;
        let mut lo_pixels = 0u32;
        for px in rgb8.chunks_exact(3) {
            let rgb = [px[0], px[1], px[2]];
            if is_highlight_clipped(rgb) {
                hi_pixels += 1;
            }
            if is_shadow_clipped(rgb) {
                lo_pixels += 1;
            }
        }
        assert_eq!(hi_pixels, 2);
        assert_eq!(lo_pixels, 2);

        // The histogram's high bins (≥ CLIP_HIGH) over all channels hold exactly
        // the channels that the highlight predicate keys on; for these hand-built
        // pixels (no pixel both clips and isn't flagged) the per-channel high-bin
        // population corresponds to the flagged pixels. Pin the agreement at the
        // bin level: every flagged-high pixel contributes at least one channel in
        // the top bins, and no unflagged pixel does.
        let hi_channel_hits: u32 = (CLIP_HIGH as usize..=255)
            .map(|v| h.r[v] + h.g[v] + h.b[v])
            .sum();
        let lo_channel_hits: u32 = (0..=CLIP_LOW as usize)
            .map(|v| h.r[v] + h.g[v] + h.b[v])
            .sum();
        // pixel 0: 3 hot channels; pixel 1: 1 hot channel → 4 channel-hits.
        assert_eq!(hi_channel_hits, 4);
        // pixel 2: 3 crushed channels; pixel 3: 1 crushed channel (the `1`) → 4.
        assert_eq!(lo_channel_hits, 4);

        // And the overlay images flag the same pixel sets the predicates do.
        let (high_img, low_img) = clip_overlay_images(rgb8, 6, 1);
        let flagged_high = high_img
            .pixels
            .iter()
            .filter(|p| **p != Color32::TRANSPARENT)
            .count();
        let flagged_low = low_img
            .pixels
            .iter()
            .filter(|p| **p != Color32::TRANSPARENT)
            .count();
        assert_eq!(flagged_high as u32, hi_pixels);
        assert_eq!(flagged_low as u32, lo_pixels);
        // The colored pixels are exactly the clipped ones, in order.
        assert_eq!(high_img.pixels[0], HIGHLIGHT_COLOR);
        assert_eq!(high_img.pixels[1], HIGHLIGHT_COLOR);
        assert_eq!(high_img.pixels[2], Color32::TRANSPARENT);
        assert_eq!(low_img.pixels[2], SHADOW_COLOR);
        assert_eq!(low_img.pixels[3], SHADOW_COLOR);
        assert_eq!(high_img.pixels[4], Color32::TRANSPARENT);
        assert_eq!(low_img.pixels[4], Color32::TRANSPARENT);
        assert_eq!(low_img.pixels[5], Color32::TRANSPARENT);
    }

    #[test]
    fn waveform_buckets_accumulate_columns() {
        // A 4-wide, 2-tall image; bucket the 4 columns into 2 buckets, so columns
        // 0,1 → bucket 0 and columns 2,3 → bucket 1. Each pixel adds to its
        // column bucket at its channel value.
        #[rustfmt::skip]
        let rgb8: &[u8] = &[
            // row 0: cols 0..3
            10, 0, 0,   10, 0, 0,   200, 0, 0,   200, 0, 0,
            // row 1: cols 0..3
            10, 0, 0,   10, 0, 0,   200, 0, 0,   200, 0, 0,
        ];
        let w = waveform_buckets(rgb8, 4, 2);
        assert_eq!(w.buckets, 2);
        // `cell(channel, bucket, value)` reads the flat `bucket * 256 + value` slot.
        let cell = |ch: usize, bucket: usize, value: usize| w.channels[ch][bucket * 256 + value];
        // Bucket 0 (left two columns) has four R=10 pixels; bucket 1 four R=200.
        assert_eq!(cell(0, 0, 10), 4);
        assert_eq!(cell(0, 1, 200), 4);
        assert_eq!(cell(0, 0, 200), 0);
        assert_eq!(cell(0, 1, 10), 0);
        // Green/blue are all zero except the value-0 slot (every pixel has G=B=0).
        assert_eq!(cell(1, 0, 0), 4); // bucket 0, value 0
        assert_eq!(cell(1, 1, 0), 4); // bucket 1, value 0
        // Total over a channel equals the pixel count.
        assert_eq!(w.channels[0].iter().sum::<u32>(), 8);

        // Ragged / degenerate inputs don't panic: zero width clamps to 1, and a
        // ragged tail is ignored.
        let _ = waveform_buckets(&[1, 2, 3, 4], 0, 4);
        let _ = waveform_buckets(&[], 4, 4);
    }
}
