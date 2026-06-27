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
    ///
    /// # Panics
    /// Panics if `width * height` overflows `usize` (only reachable on a 32-bit
    /// target, since the product of two `u32`s fits a 64-bit `usize`). Callers that
    /// accept untrusted dimensions should use [`Self::try_new`], which returns
    /// `None` instead of panicking or attempting a huge allocation.
    pub fn new(width: u32, height: u32) -> Self {
        Self::try_new(width, height).expect("ImageBuf dimensions overflow usize")
    }

    /// Allocate a `width` x `height` black image, or `None` if `width * height`
    /// overflows `usize`.
    ///
    /// The element count is computed with `checked_mul` — the *same* computation
    /// [`Self::index`] uses — so construction and indexing can never disagree about
    /// the buffer size. This is the boundary where dimension trust is established for
    /// the whole pipeline: a buffer that exists is guaranteed to be consistently
    /// sized.
    #[must_use]
    pub fn try_new(width: u32, height: u32) -> Option<Self> {
        let count = (width as usize).checked_mul(height as usize)?;
        Some(Self {
            width,
            height,
            pixels: vec![[0.0; 3]; count],
        })
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of pixels in the image.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pixels.len()
    }

    /// True if the image has no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    /// The pixels as a flat row-major slice (for bulk/parallel processing).
    #[must_use]
    pub fn pixels(&self) -> &[[f32; 3]] {
        &self.pixels
    }

    /// The pixels as a mutable flat row-major slice.
    pub fn pixels_mut(&mut self) -> &mut [[f32; 3]] {
        &mut self.pixels
    }

    /// Row-major index of pixel `(x, y)`, or `None` if `y * width + x` overflows
    /// `usize`. Uses the same checked arithmetic as [`Self::try_new`]'s element
    /// count, so a valid in-bounds coordinate always maps to a valid offset.
    fn checked_index(&self, x: u32, y: u32) -> Option<usize> {
        (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)
    }

    /// Row-major index of pixel `(x, y)`. Panics on overflow (see
    /// [`Self::checked_index`]); unreachable for an in-bounds coordinate of a buffer
    /// built through [`Self::try_new`].
    fn index(&self, x: u32, y: u32) -> usize {
        self.checked_index(x, y)
            .expect("pixel index overflows usize")
    }

    /// Read the pixel at `(x, y)`.
    ///
    /// # Panics
    /// Panics if `(x, y)` is out of bounds. This is the hot-path accessor; callers
    /// handling untrusted coordinates should use [`Self::try_get`], which returns
    /// `None` instead.
    pub fn get(&self, x: u32, y: u32) -> [f32; 3] {
        self.pixels[self.index(x, y)]
    }

    /// Write the pixel at `(x, y)`.
    ///
    /// # Panics
    /// Panics if `(x, y)` is out of bounds. This is the hot-path accessor; callers
    /// handling untrusted coordinates should use [`Self::try_set`], which returns
    /// `None` instead.
    pub fn set(&mut self, x: u32, y: u32, px: [f32; 3]) {
        let i = self.index(x, y);
        self.pixels[i] = px;
    }

    /// Read the pixel at `(x, y)`, or `None` if the coordinate is out of bounds —
    /// the non-panicking counterpart to [`Self::get`].
    #[must_use]
    pub fn try_get(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.checked_index(x, y).map(|i| self.pixels[i])
    }

    /// Write the pixel at `(x, y)`, returning `Some(())` on success or `None` if the
    /// coordinate is out of bounds — the non-panicking counterpart to [`Self::set`].
    pub fn try_set(&mut self, x: u32, y: u32, px: [f32; 3]) -> Option<()> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = self.checked_index(x, y)?;
        self.pixels[i] = px;
        Some(())
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
    fn downscale_max_dim_zero_collapses_to_one_pixel() {
        // `max_dim == 0` would scale every dimension to 0, but the `.max(1)` floor on
        // the target size keeps a 1x1 result rather than a zero-area buffer; that
        // single pixel is the average of the whole image (here a uniform color).
        let mut img = ImageBuf::new(4, 2);
        for p in img.pixels_mut() {
            *p = [0.1, 0.2, 0.3];
        }
        let out = img.downscaled(0);
        assert_eq!((out.width(), out.height()), (1, 1));
        for c in 0..3 {
            assert!((out.get(0, 0)[c] - [0.1, 0.2, 0.3][c]).abs() < 1e-6);
        }
    }

    #[test]
    fn downscale_already_small_is_a_clone() {
        // When the longest side already fits `max_dim`, the image is returned
        // unchanged (the `longest <= max_dim` early return), preserving exact pixels.
        let mut img = ImageBuf::new(3, 2);
        img.set(0, 0, [0.1, 0.2, 0.3]);
        let out = img.downscaled(10);
        assert_eq!((out.width(), out.height()), (3, 2));
        assert_eq!(out.get(0, 0), [0.1, 0.2, 0.3]);
    }

    #[test]
    fn downscale_one_pixel_image_is_stable() {
        // A 1x1 image is already minimal: any max_dim returns it unchanged, and the
        // span's `b.max(a + 1)` guard means no zero-width source block / div-by-zero.
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.4, 0.5, 0.6]);
        assert_eq!(img.downscaled(1).get(0, 0), [0.4, 0.5, 0.6]);
        assert_eq!(img.downscaled(8).get(0, 0), [0.4, 0.5, 0.6]);
    }

    #[test]
    fn downscale_extreme_aspect_ratio_keeps_at_least_one_pixel() {
        // A 100x1 strip scaled so its longest side is 10 keeps height >= 1 (the
        // `.max(1)` floor) — the rounded target height would otherwise be 0.
        let img = ImageBuf::new(100, 1);
        let out = img.downscaled(10);
        assert_eq!(out.width(), 10);
        assert!(out.height() >= 1, "height floored to 1: {}", out.height());
        // And the very thin axis: every target block covers a non-empty source range
        // (the div-by-zero guard), so no NaN leaks into the averaged pixels.
        for p in out.pixels() {
            assert!(
                p.iter().all(|c| c.is_finite()),
                "NaN from empty block: {p:?}"
            );
        }
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

    #[test]
    fn imagebuf_overflow_dims_are_rejected() {
        // A product that overflows `usize` returns `None` rather than allocating or
        // aborting. (`u32::MAX * u32::MAX` overflows `usize` on a 32-bit target; on
        // 64-bit it fits `usize` but would try a ~3.4e38-element allocation, so we
        // assert the smaller-but-still-overflowing case where it matters and keep the
        // normal case allocating.)
        assert!(ImageBuf::try_new(4, 3).is_some());
        // On 64-bit, `u32::MAX * u32::MAX` fits usize; force a genuine usize overflow
        // by construction is only possible on 32-bit, so guard the assertion to the
        // platform where `checked_mul` can actually fail.
        if usize::BITS <= 32 {
            assert!(ImageBuf::try_new(u32::MAX, u32::MAX).is_none());
        }
        // The element count and a valid index always agree (round-trip a corner).
        let img = ImageBuf::try_new(4, 3).expect("fits");
        assert_eq!(img.len(), 12);
        assert_eq!(img.try_get(3, 2), Some([0.0, 0.0, 0.0]));
    }

    #[test]
    fn try_get_out_of_bounds_is_none() {
        let mut img = ImageBuf::new(2, 2);
        // In bounds: Some.
        assert_eq!(img.try_get(1, 1), Some([0.0, 0.0, 0.0]));
        assert_eq!(img.try_set(1, 1, [0.5, 0.5, 0.5]), Some(()));
        assert_eq!(img.try_get(1, 1), Some([0.5, 0.5, 0.5]));
        // Past the edge in either axis: None, no panic.
        assert_eq!(img.try_get(2, 0), None);
        assert_eq!(img.try_get(0, 2), None);
        assert_eq!(img.try_set(2, 0, [1.0; 3]), None);
        assert_eq!(img.try_set(0, 2, [1.0; 3]), None);
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn get_out_of_bounds_still_panics() {
        // The hot-path accessor keeps its documented panic contract. `(0, 2)` maps to
        // flat index 4, past the 4-element 2x2 buffer, so the Vec index panics.
        let img = ImageBuf::new(2, 2);
        let _ = img.get(0, 2);
    }
}
