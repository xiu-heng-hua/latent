//! Linear-light image buffers and color math.

pub mod color;
pub mod tone;

/// The image as it flows through the pipeline: always linear-light, f32 RGB.
///
/// Pixels are stored row-major: pixel `(x, y)` is at index `y * width + x`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageBuf {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 3]>,
}

impl ImageBuf {
    /// Allocate a `width` x `height` image with every pixel set to black.
    pub fn new(width: u32, height: u32) -> Self {
        let count = width as usize * height as usize;
        Self {
            width,
            height,
            pixels: vec![[0.0; 3]; count],
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of pixels in the image.
    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    /// True if the image has no pixels.
    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// The pixels as a flat row-major slice (for bulk/parallel processing).
    pub fn pixels(&self) -> &[[f32; 3]] {
        &self.pixels
    }

    /// The pixels as a mutable flat row-major slice.
    pub fn pixels_mut(&mut self) -> &mut [[f32; 3]] {
        &mut self.pixels
    }

    /// Row-major index of pixel `(x, y)`.
    fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    /// Read the pixel at `(x, y)`. Panics if out of bounds.
    pub fn get(&self, x: u32, y: u32) -> [f32; 3] {
        self.pixels[self.index(x, y)]
    }

    /// Write the pixel at `(x, y)`. Panics if out of bounds.
    pub fn set(&mut self, x: u32, y: u32, px: [f32; 3]) {
        let i = self.index(x, y);
        self.pixels[i] = px;
    }

    /// A copy of the rectangular region with top-left `(x, y)` and size `w × h`,
    /// in pixels. The region is clamped to the image bounds (a width/height that
    /// would run past the edge is reduced to fit) and never shrinks below one
    /// pixel, since a zero-area image is meaningless.
    pub fn cropped(&self, x: u32, y: u32, w: u32, h: u32) -> ImageBuf {
        if self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        let w = w.clamp(1, self.width - x);
        let h = h.clamp(1, self.height - y);

        let mut out = ImageBuf::new(w, h);
        for ry in 0..h {
            for rx in 0..w {
                out.set(rx, ry, self.get(x + rx, y + ry));
            }
        }
        out
    }

    /// A copy scaled down so its longest side is at most `max_dim`, by averaging
    /// each source block (area downsampling). Returns a clone if already that
    /// small. Averaging happens in whatever space the pixels are in — call this
    /// on linear-light data so the downsample is physically correct.
    pub fn downscaled(&self, max_dim: u32) -> ImageBuf {
        let longest = self.width.max(self.height);
        if longest == 0 || longest <= max_dim {
            return self.clone();
        }
        let scale = max_dim as f32 / longest as f32;
        let tw = ((self.width as f32 * scale).round() as u32).max(1);
        let th = ((self.height as f32 * scale).round() as u32).max(1);

        // Source range [a, b) covered by target index `t` of `target_len`.
        let span = |t: u32, target_len: u32, src_len: u32| {
            let a = (t as u64 * src_len as u64 / target_len as u64) as u32;
            let b = ((t + 1) as u64 * src_len as u64 / target_len as u64) as u32;
            (a, b.max(a + 1))
        };

        let mut out = ImageBuf::new(tw, th);
        for ty in 0..th {
            let (y0, y1) = span(ty, th, self.height);
            for tx in 0..tw {
                let (x0, x1) = span(tx, tw, self.width);
                let mut sum = [0.0_f32; 3];
                let mut n = 0_u32;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let p = self.get(sx, sy);
                        sum[0] += p[0];
                        sum[1] += p[1];
                        sum[2] += p[2];
                        n += 1;
                    }
                }
                let inv = 1.0 / n as f32;
                out.set(tx, ty, [sum[0] * inv, sum[1] * inv, sum[2] * inv]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_noop_when_already_small() {
        let img = ImageBuf::new(2, 2);
        let small = img.downscaled(10);
        assert_eq!((small.width(), small.height()), (2, 2));
    }

    #[test]
    fn downscale_averages_a_block() {
        // 2x1 black+white → 1x1 mid-gray (average).
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]);
        img.set(1, 0, [1.0, 1.0, 1.0]);
        let small = img.downscaled(1);
        assert_eq!((small.width(), small.height()), (1, 1));
        assert_eq!(small.get(0, 0), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn downscale_preserves_uniform_color_and_fits_max_dim() {
        let mut img = ImageBuf::new(8, 4);
        for y in 0..4 {
            for x in 0..8 {
                img.set(x, y, [0.2, 0.4, 0.6]);
            }
        }
        let small = img.downscaled(4);
        assert_eq!((small.width(), small.height()), (4, 2));
        assert_eq!(small.get(3, 1), [0.2, 0.4, 0.6]);
    }

    #[test]
    fn cropped_extracts_the_region() {
        // 3x2 with a distinct value per pixel; crop the 2x1 block at (1, 0).
        let mut img = ImageBuf::new(3, 2);
        for y in 0..2 {
            for x in 0..3 {
                img.set(x, y, [(x + y * 3) as f32, 0.0, 0.0]);
            }
        }
        let c = img.cropped(1, 0, 2, 1);
        assert_eq!((c.width(), c.height()), (2, 1));
        assert_eq!(c.get(0, 0), [1.0, 0.0, 0.0]);
        assert_eq!(c.get(1, 0), [2.0, 0.0, 0.0]);
    }

    #[test]
    fn cropped_clamps_a_region_past_the_edge() {
        let img = ImageBuf::new(4, 4);
        // Asking for 10x10 from (2,2) clamps to the 2x2 that actually fits.
        let c = img.cropped(2, 2, 10, 10);
        assert_eq!((c.width(), c.height()), (2, 2));
    }

    #[test]
    fn new_is_black_and_correctly_sized() {
        let img = ImageBuf::new(4, 3);
        assert_eq!(img.len(), 12);
        assert_eq!(img.get(0, 0), [0.0, 0.0, 0.0]);
        assert_eq!(img.get(3, 2), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let mut img = ImageBuf::new(2, 2);
        img.set(1, 0, [0.25, 0.5, 1.0]);
        assert_eq!(img.get(1, 0), [0.25, 0.5, 1.0]);
        // A different pixel is untouched.
        assert_eq!(img.get(0, 0), [0.0, 0.0, 0.0]);
    }
}
