//! Mask-overlay visualization: the selected local's mask rendered as a
//! translucent red overlay (or a mask-only view) so the selection is visible.
//!
//! The coverage is computed from [`Mask::weight_at`] over the downscaled preview
//! grid, and — critically — fed the **source/working** pixel at each point, not
//! the display sRGB, so value-driven masks (luminosity, color range) preview the
//! same selection the engine evaluates. It is cached and only recomputed when the
//! mask, the selected local, or the preview base changes; the brush (live
//! coverage) reuses the same compute and cache.

use eframe::egui;
use egui::{Color32, ColorImage};
use latent_edit::Mask;
use latent_image::ImageBuf;

use super::super::app::App;
use super::super::canvas::ViewTransform;

/// The longest side of the coverage grid. Coarser than the preview itself —
/// coverage is a soft visualization, so a smaller grid keeps the compute and the
/// texture cheap without a visible difference under the translucent fill.
const COVERAGE_MAX_DIM: u32 = 400;

/// How the mask overlay is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OverlayMode {
    /// Off — no overlay.
    #[default]
    Off,
    /// A translucent red wash over the selected region (Lightroom-style).
    Color,
    /// A grayscale mask-only view (white = selected, black = not).
    MaskOnly,
}

impl OverlayMode {
    /// Whether any overlay is shown.
    pub(crate) fn is_on(self) -> bool {
        !matches!(self, OverlayMode::Off)
    }
}

/// A computed coverage grid: per-cell mask weight in `[0, 1]`, plus the grid
/// size. Kept separate from the texture so the same coverage can be turned into
/// either overlay mode.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Coverage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) weights: Vec<f32>,
}

/// Build a coverage grid for `mask` over `base` (the working-RGB preview base).
/// Each cell samples the mask weight at its normalized center using the **source
/// pixel** there — the same pixel the engine evaluates the mask against — so a
/// luminosity/color-range mask previews correctly. The grid is the base
/// downscaled to [`COVERAGE_MAX_DIM`] on its longest side.
pub(crate) fn build_coverage(mask: &Mask, base: &ImageBuf) -> Coverage {
    let (bw, bh) = (base.width().max(1), base.height().max(1));
    let scale = (COVERAGE_MAX_DIM as f32 / bw.max(bh) as f32).min(1.0);
    let gw = ((bw as f32 * scale).round() as u32).max(1);
    let gh = ((bh as f32 * scale).round() as u32).max(1);
    let mut weights = vec![0.0_f32; (gw * gh) as usize];
    for gy in 0..gh {
        // Normalized Y at the cell center, and the source row it samples.
        let ny = if gh > 1 {
            gy as f32 / (gh - 1) as f32
        } else {
            0.0
        };
        let sy = (ny * (bh - 1) as f32).round() as u32;
        for gx in 0..gw {
            let nx = if gw > 1 {
                gx as f32 / (gw - 1) as f32
            } else {
                0.0
            };
            let sx = (nx * (bw - 1) as f32).round() as u32;
            // The SOURCE (working linear) pixel — never the display sRGB.
            let pixel = base.get(sx, sy);
            weights[(gy * gw + gx) as usize] = mask.weight_at(nx, ny, pixel).clamp(0.0, 1.0);
        }
    }
    Coverage {
        width: gw,
        height: gh,
        weights,
    }
}

/// Turn a coverage grid into an egui texture image for the given mode. `Color` is
/// a red wash whose alpha tracks the weight; `MaskOnly` is an opaque grayscale.
pub(crate) fn coverage_image(cov: &Coverage, mode: OverlayMode) -> ColorImage {
    let mut pixels = Vec::with_capacity(cov.weights.len());
    for &w in &cov.weights {
        let c = match mode {
            OverlayMode::Color => {
                // A translucent red; the alpha is the weight (capped so even full
                // coverage stays a wash the image reads through).
                Color32::from_rgba_unmultiplied(220, 40, 40, (w * 150.0) as u8)
            }
            OverlayMode::MaskOnly | OverlayMode::Off => {
                let g = (w * 255.0) as u8;
                Color32::from_rgb(g, g, g)
            }
        };
        pixels.push(c);
    }
    ColorImage {
        size: [cov.width as usize, cov.height as usize],
        pixels,
        source_size: egui::vec2(cov.width as f32, cov.height as f32),
    }
}

/// A cached coverage texture plus the key it was built for, so a frame reuses it
/// unless the mask / selected local / preview base changed.
#[derive(Default)]
pub(crate) struct OverlayCache {
    /// The cache key: `(selected local index, mask hash, preview generation)`.
    key: Option<(usize, u64, u64)>,
    /// The uploaded coverage texture for the current key.
    texture: Option<egui::TextureHandle>,
    /// The mode the texture was built for (so a mode switch rebuilds it).
    mode: OverlayMode,
}

impl OverlayCache {
    /// Return the coverage texture for `mask`/`local`/`base`, rebuilding it only
    /// when the key or the mode changed. `generation` is the preview generation counter,
    /// bumped whenever the preview base is replaced.
    pub(crate) fn texture(
        &mut self,
        ctx: &egui::Context,
        mask: &Mask,
        local: usize,
        base: &ImageBuf,
        generation: u64,
        mode: OverlayMode,
    ) -> Option<egui::TextureHandle> {
        let key = (local, mask_hash(mask), generation);
        if self.key != Some(key) || self.mode != mode || self.texture.is_none() {
            let cov = build_coverage(mask, base);
            let image = coverage_image(&cov, mode);
            let tex = ctx.load_texture("mask_overlay", image, egui::TextureOptions::NEAREST);
            self.key = Some(key);
            self.mode = mode;
            self.texture = Some(tex);
        }
        self.texture.clone()
    }

    /// Drop the cached texture (e.g. when the overlay is turned off or the file
    /// changes), so the next show rebuilds from scratch.
    pub(crate) fn clear(&mut self) {
        self.key = None;
        self.texture = None;
    }
}

/// Draw the selected local's mask overlay onto the image, when the overlay is on
/// and a local is selected. Reads the cached coverage texture (rebuilt only when
/// the mask / selected local / preview base changes) and stretches it over the
/// image rect via the transform. Pure paint — the rendered preview and the export
/// are untouched.
pub(crate) fn draw(
    app: &mut App,
    painter: &egui::Painter,
    transform: &ViewTransform,
    active: usize,
    local_sel: usize,
) {
    if !app.overlay_mode.is_on() {
        app.overlay_cache.clear();
        return;
    }
    let Some(local) = app.variants[active].current().locals.get(local_sel) else {
        return;
    };
    let mask = local.mask.clone();
    let mode = app.overlay_mode;
    let generation = app.preview_gen;
    // The coverage samples the working (linear) preview base — never the display
    // sRGB — so value-driven masks preview the selection the engine evaluates.
    let base = std::sync::Arc::clone(&app.preview);
    let Some(tex) =
        app.overlay_cache
            .texture(painter.ctx(), &mask, local_sel, &base, generation, mode)
    else {
        return;
    };
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    painter.image(tex.id(), transform.image_rect(), uv, Color32::WHITE);
}

/// A cheap order-sensitive hash of a mask's shapes/ops/invert, used as the cache
/// key so the overlay rebuilds exactly when the mask changes. Floats are hashed
/// by their bit pattern (the mask carries finite, sanitized values).
fn mask_hash(mask: &Mask) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut f = |x: f32| h.write_u32(x.to_bits());
    for shape in &mask.shapes {
        hash_shape(&mut f, shape);
    }
    for op in &mask.ops {
        h.write_u8(*op as u8);
    }
    h.write_u8(mask.invert as u8);
    h.finish()
}

/// Feed a shape's parameters into the running float hash.
fn hash_shape(f: &mut impl FnMut(f32), shape: &latent_edit::MaskShape) {
    use latent_edit::MaskShape;
    match shape {
        MaskShape::Gradient(g) => {
            for v in [g.x0, g.y0, g.x1, g.y1] {
                f(v);
            }
        }
        MaskShape::Radial(r) => {
            for v in [r.cx, r.cy, r.radius, r.feather] {
                f(v);
            }
        }
        MaskShape::Luminosity(l) => {
            for v in [l.lo, l.hi, l.feather] {
                f(v);
            }
        }
        MaskShape::ColorRange(c) => {
            for v in [c.hue, c.hue_width, c.sat_min, c.feather] {
                f(v);
            }
        }
        MaskShape::Brush(b) => {
            for d in &b.dabs {
                for v in [d.x, d.y, d.radius, d.feather] {
                    f(v);
                }
                f(if d.erase { 1.0 } else { 0.0 });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use latent_edit::{LuminanceRange, MaskShape, Radial};

    #[test]
    fn coverage_uses_the_source_pixel_for_value_driven_masks() {
        // A 2×1 image: a dark pixel on the left, a bright one on the right, both in
        // linear working RGB. A luminosity mask selecting shadows (luma ≤ 0.3) must
        // cover only the dark cell — which is only possible if the builder feeds
        // `weight_at` the SOURCE pixel, not a position.
        let mut base = ImageBuf::new(2, 1);
        base.set(0, 0, [0.05, 0.05, 0.05]); // dark → selected
        base.set(1, 0, [0.9, 0.9, 0.9]); // bright → not selected
        let mask = Mask {
            shapes: vec![MaskShape::Luminosity(LuminanceRange {
                lo: 0.0,
                hi: 0.3,
                feather: 0.05,
            })],
            ops: Vec::new(),
            invert: false,
        };
        let cov = build_coverage(&mask, &base);
        assert_eq!((cov.width, cov.height), (2, 1));
        // The left (dark) cell is selected, the right (bright) one is not.
        assert!(
            cov.weights[0] > 0.9,
            "dark cell selected: {}",
            cov.weights[0]
        );
        assert!(
            cov.weights[1] < 0.1,
            "bright cell rejected: {}",
            cov.weights[1]
        );

        // Feeding display-encoded (brighter) pixels would wrongly reject the dark
        // cell — confirm the same mask on the *display* values selects differently.
        let mut wrong = ImageBuf::new(2, 1);
        wrong.set(0, 0, [0.5, 0.5, 0.5]); // a dark pixel's sRGB is much brighter
        wrong.set(1, 0, [0.96, 0.96, 0.96]);
        let cov_wrong = build_coverage(&mask, &wrong);
        assert!(
            cov_wrong.weights[0] < 0.1,
            "on display values the dark cell would be missed — proving source matters"
        );
    }

    #[test]
    fn coverage_matches_weight_at_for_a_radial() {
        // A position-only radial: the center cell is fully covered, a far corner is
        // not, matching `weight_at` directly.
        let mut base = ImageBuf::new(5, 5);
        for p in base.pixels_mut() {
            *p = [0.5, 0.5, 0.5];
        }
        let r = Radial {
            cx: 0.5,
            cy: 0.5,
            radius: 0.2,
            feather: 0.1,
        };
        let mask = Mask {
            shapes: vec![MaskShape::Radial(r)],
            ops: Vec::new(),
            invert: false,
        };
        let cov = build_coverage(&mask, &base);
        let center = cov.weights[(cov.height / 2 * cov.width + cov.width / 2) as usize];
        assert!((center - mask.weight_at(0.5, 0.5, [0.5; 3])).abs() < 1e-5);
        assert!(cov.weights[0] < 0.5, "the corner is outside the disc");
    }

    #[test]
    fn mask_hash_changes_with_the_mask() {
        let a = Mask {
            shapes: vec![MaskShape::Radial(Radial {
                cx: 0.5,
                cy: 0.5,
                radius: 0.2,
                feather: 0.1,
            })],
            ops: Vec::new(),
            invert: false,
        };
        let mut b = a.clone();
        let MaskShape::Radial(r) = &mut b.shapes[0] else {
            unreachable!()
        };
        r.radius = 0.3;
        assert_ne!(mask_hash(&a), mask_hash(&b));
        // Toggling invert also changes the hash.
        let mut c = a.clone();
        c.invert = true;
        assert_ne!(mask_hash(&a), mask_hash(&c));
        // The same mask hashes the same.
        assert_eq!(mask_hash(&a), mask_hash(&a.clone()));
    }
}
