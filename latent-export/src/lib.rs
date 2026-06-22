//! Output transform and file encoding.

use std::path::Path;

use latent_image::ImageBuf;

/// sRGB OETF: encode a linear-light value into gamma-encoded sRGB.
///
/// A near-2.2 power curve with a small linear segment near black. Applied last,
/// on the way out, to turn linear-light pixels into display/file values.
///
/// Transfer function defined by the sRGB standard, IEC 61966-2-1.
pub fn srgb_encode(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Inverse sRGB OETF: decode a gamma-encoded sRGB value back to linear light.
pub fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Encode a linear-light image to 8-bit sRGB and write it to `path`.
///
/// Each channel is gamma-encoded (sRGB OETF), clamped to [0,1], and quantized
/// to 8 bits. The file format is chosen from the path's extension (e.g. `.jpg`,
/// `.png`, `.tiff`) by the `image` crate.
pub fn save(img: &ImageBuf, path: &Path) -> image::ImageResult<()> {
    let mut out = image::RgbImage::new(img.width(), img.height());
    for y in 0..img.height() {
        for x in 0..img.width() {
            let px = img.get(x, y);
            let rgb =
                std::array::from_fn(|c| (srgb_encode(px[c]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.put_pixel(x, y, image::Rgb(rgb));
        }
    }
    out.save(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_fixed() {
        assert_eq!(srgb_encode(0.0), 0.0);
        assert!((srgb_encode(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn encode_then_decode_round_trips() {
        for &v in &[0.0, 0.001, 0.0031308, 0.05, 0.18, 0.5, 0.9, 1.0] {
            let back = srgb_decode(srgb_encode(v));
            assert!((back - v).abs() < 1e-5, "round-trip drift at {v}: {back}");
        }
    }

    #[test]
    fn encoding_brightens_midtones() {
        // Gamma encoding lifts mid-gray well above its linear value.
        assert!(srgb_encode(0.18) > 0.4);
    }

    #[test]
    fn save_writes_a_readable_image() {
        // A 2x2 image with known corners.
        let mut img = ImageBuf::new(2, 2);
        img.set(0, 0, [0.0, 0.0, 0.0]); // black
        img.set(1, 0, [1.0, 1.0, 1.0]); // white
        img.set(0, 1, [1.0, 0.0, 0.0]); // red
        img.set(1, 1, [0.0, 0.0, 1.0]); // blue

        let path = std::env::temp_dir().join("latent_export_save_test.png");
        save(&img, &path).expect("save should succeed");

        let loaded = image::open(&path)
            .expect("written file should open")
            .to_rgb8();
        assert_eq!(loaded.dimensions(), (2, 2));
        assert_eq!(loaded.get_pixel(0, 0), &image::Rgb([0, 0, 0]));
        assert_eq!(loaded.get_pixel(1, 0), &image::Rgb([255, 255, 255]));
        assert_eq!(loaded.get_pixel(0, 1), &image::Rgb([255, 0, 0]));

        std::fs::remove_file(&path).ok();
    }
}
