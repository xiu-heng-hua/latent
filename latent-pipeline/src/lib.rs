//! Rendering pipeline and the backend abstraction.
//!
//! [`render`] is the fixed, ordered pipeline. It owns the order; a [`Backend`]
//! only provides the pixel-level primitives it calls. Naming the coordinate
//! spaces is deliberate: all adjustments act on the full, uncropped image
//! (SOURCE space), and geometry is the single later step that reframes it
//! (SOURCE → OUTPUT).

use latent_edit::{Adjustments, Geometry, LocalAdjustment, Settings};
use latent_image::ImageBuf;

/// A data-described per-pixel operation over linear-light RGB pixels.
///
/// Point operations are *data*, not code: the pipeline builds them and each
/// backend gives them meaning (interpreted on the CPU now, dispatched however a
/// future backend likes). Describing them as data — rather than as an opaque
/// closure — is what lets a backend run them anywhere. More variants are added
/// as adjustments land.
#[derive(Debug, Clone, PartialEq)]
pub enum PointOp {
    /// Multiply each channel by its own gain.
    Gain([f32; 3]),
}

/// The pixel-level primitives a rendering backend provides.
///
/// The pipeline calls these in a fixed order; the order lives in [`render`],
/// never in a backend. A backend may implement the primitives however it likes
/// (on the CPU now, elsewhere later) as long as the results match. More
/// primitives are added to this trait as the pipeline grows.
pub trait Backend {
    /// Apply a per-pixel operation to every pixel of the image, in place.
    fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp);
}

/// Render a finished working image from a source image and its settings.
///
/// `source` is the linear working image in SOURCE coordinates — the full,
/// uncropped develop (decode → white balance → demosaic → camera-to-working).
/// The fixed pipeline then applies, in order: global adjustments, local
/// adjustments, and geometry (the single SOURCE → OUTPUT step). The final
/// output encoding happens separately, at export.
///
/// With default (neutral) settings every stage is a no-op, so the source image
/// is returned unchanged.
pub fn render(source: &ImageBuf, settings: &Settings, backend: &dyn Backend) -> ImageBuf {
    let img = source.clone();
    let img = apply_global(img, &settings.global, backend);
    let img = apply_locals(img, &settings.locals, backend);
    apply_geometry(img, &settings.geometry, backend)
}

/// Stage: global adjustments, applied in SOURCE space.
///
/// Each active adjustment is lowered into backend primitives in a fixed order.
/// No adjustments are defined yet, so this returns the image unchanged.
fn apply_global(img: ImageBuf, _global: &Adjustments, _backend: &dyn Backend) -> ImageBuf {
    img
}

/// Stage: local adjustments, applied in SOURCE space.
///
/// Each local adjustment applies a set of adjustments through its mask, blended
/// at its opacity. Masks are not implemented yet, so this returns the image
/// unchanged.
fn apply_locals(img: ImageBuf, _locals: &[LocalAdjustment], _backend: &dyn Backend) -> ImageBuf {
    img
}

/// Stage: geometry — the single SOURCE → OUTPUT resample (crop, straighten).
///
/// Not implemented yet, so this returns the image unchanged.
fn apply_geometry(img: ImageBuf, _geometry: &Geometry, _backend: &dyn Backend) -> ImageBuf {
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal backend so the pipeline can be tested here; the real CPU
    /// backend lives in a crate that depends on this one.
    struct TestBackend;

    impl Backend for TestBackend {
        fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp) {
            match *op {
                PointOp::Gain(g) => {
                    for px in img.pixels_mut() {
                        *px = [px[0] * g[0], px[1] * g[1], px[2] * g[2]];
                    }
                }
            }
        }
    }

    #[test]
    fn render_with_default_settings_returns_the_source_unchanged() {
        let mut src = ImageBuf::new(3, 2);
        src.set(0, 0, [0.1, 0.2, 0.3]);
        src.set(2, 1, [0.9, 0.8, 0.7]);
        let out = render(&src, &Settings::default(), &TestBackend);
        assert_eq!(out, src);
    }
}
