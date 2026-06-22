//! CPU rendering backend.

use latent_image::ImageBuf;
use latent_pipeline::{Backend, PointOp};

/// A rendering backend that runs every primitive on the CPU.
///
/// This is the always-available backend and the correctness reference: other
/// backends may accelerate some primitives and fall back to this one.
pub struct CpuBackend;

impl Backend for CpuBackend {
    fn map_pixels(&self, img: &mut ImageBuf, op: &PointOp) {
        // The operation is matched once, outside the per-pixel loop.
        match *op {
            PointOp::Gain(g) => {
                for px in img.pixels_mut() {
                    *px = [px[0] * g[0], px[1] * g[1], px[2] * g[2]];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_pixels_identity_leaves_the_image_unchanged() {
        let mut img = ImageBuf::new(2, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        img.set(1, 1, [0.4, 0.5, 0.6]);
        let before = img.clone();
        CpuBackend.map_pixels(&mut img, &PointOp::Gain([1.0, 1.0, 1.0]));
        assert_eq!(img, before);
    }

    #[test]
    fn map_pixels_applies_the_function_to_every_pixel() {
        let mut img = ImageBuf::new(1, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        img.set(0, 1, [0.4, 0.5, 0.6]);
        CpuBackend.map_pixels(&mut img, &PointOp::Gain([2.0, 2.0, 2.0]));
        assert_eq!(img.get(0, 0), [0.2, 0.4, 0.6]);
        assert_eq!(img.get(0, 1), [0.8, 1.0, 1.2]);
    }
}
