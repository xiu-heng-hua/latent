//! Output transform and file encoding.

use std::path::Path;

use latent_image::ImageBuf;
use latent_image::color::{self, Mat3};

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

/// The highlight-rolloff knee: values up to here pass through untouched.
const ROLLOFF_KNEE: f32 = 0.98;

/// Compress a single brightness value into display range, **anchored at white**:
/// values up to the knee (just below 1.0) pass through unchanged, and everything
/// from there up — including headroom above 1.0 — is compressed smoothly toward,
/// but never reaching, 1.0.
///
/// The knee sits at 0.98 so display `[0, 1]` stays faithful to within ~1 code
/// (white maps to 254, not 255), while highlights above 1.0 keep a gradient
/// instead of clamping flat. That gradient is necessarily tiny at 8 bits — it
/// lives in the top code or two — but is real at 16 bits ([`save_16`]), which is
/// what it exists for. (You cannot keep white at *exactly* 255 and also show a
/// headroom gradient in a fixed-range output; 0.98 is the
/// faithful-within-tolerance compromise.)
pub fn highlight_rolloff(x: f32) -> f32 {
    if x <= ROLLOFF_KNEE {
        x
    } else {
        let span = 1.0 - ROLLOFF_KNEE;
        let excess = x - ROLLOFF_KNEE;
        ROLLOFF_KNEE + span * (excess / (excess + span))
    }
}

/// Hue-preserving highlight rolloff for a linear RGB triplet: derive **one**
/// compression factor from the brightest channel and scale the whole triplet by
/// it, so the color keeps its channel ratios (its hue) as it compresses into
/// display range.
///
/// Per-channel rolloff would compress only the hot channel of a near-clipped
/// color, dragging its hue toward the secondaries on blown highlights; a single
/// shared factor avoids that. Below the knee (and for a non-positive max) the
/// factor is exactly `1.0`, so every in-range color is untouched and there is no
/// division by zero. A neutral triplet `[v,v,v]` reduces to [`highlight_rolloff`]
/// of `v` on each channel, so the pinned neutral display codes are unchanged.
fn highlight_rolloff_rgb(rgb: [f32; 3]) -> [f32; 3] {
    let m = rgb[0].max(rgb[1]).max(rgb[2]);
    if m <= ROLLOFF_KNEE {
        return rgb;
    }
    let factor = highlight_rolloff(m) / m;
    std::array::from_fn(|c| rgb[c] * factor)
}

/// The full output transform for one working-space pixel: working → linear sRGB,
/// hue-preserving highlight rolloff, sRGB OETF, clamped to display `[0, 1]`.
/// Shared by the 8-bit and 16-bit encoders so they agree exactly bar
/// quantization.
fn to_display(working: [f32; 3], to_srgb: &Mat3) -> [f32; 3] {
    let lin = highlight_rolloff_rgb(to_srgb.mul_vec(working));
    std::array::from_fn(|c| srgb_encode(lin[c]).clamp(0.0, 1.0))
}

/// Apply the output transform to a whole working-space image and return row-major
/// 8-bit sRGB bytes (`RGBRGB…`).
///
/// This is *the* output path — both the saved 8-bit file ([`save`]) and the live
/// editor preview go through it, so the preview matches the file by construction.
pub fn to_srgb8(img: &ImageBuf) -> Vec<u8> {
    let to_srgb = color::linear_working_to_linear_srgb();
    let mut bytes = Vec::with_capacity(img.len() * 3);
    for y in 0..img.height() {
        for x in 0..img.width() {
            for v in to_display(img.get(x, y), &to_srgb) {
                bytes.push((v * 255.0 + 0.5) as u8);
            }
        }
    }
    bytes
}

/// The output extensions we encode with the sRGB ICC profile embedded, lowercased
/// and listed once so the rejection in [`save_buffer_with_icc`] and the depth
/// default in [`recommended_depth`] cannot drift apart.
const SUPPORTED_EXTENSIONS: &[&str] = &["png", "tif", "tiff", "jpg", "jpeg"];

/// The lowercased extension of `path` if it is one we can write (carrying the
/// sRGB profile), else `None` — for an unsupported or missing extension. The one
/// place that classifies an output path by format.
fn supported_extension(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    SUPPORTED_EXTENSIONS.contains(&ext.as_str()).then_some(ext)
}

/// A bit depth for an encoded file: 8 bits ([`save`]) or 16 ([`save_16`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Eight,
    Sixteen,
}

/// The depth that best suits a path's format: 16-bit for the formats whose extra
/// precision the wide-gamut, highlight-rolled pipeline can fill (`tif`/`tiff`,
/// and `png`, which has a real `Rgb16`), 8-bit for JPEG (no 16-bit path).
/// `None` for an unsupported extension — the caller surfaces that as the same
/// rejection [`save_buffer_with_icc`] would raise.
pub fn recommended_depth(path: &Path) -> Option<Depth> {
    supported_extension(path).map(|ext| match ext.as_str() {
        "jpg" | "jpeg" => Depth::Eight,
        _ => Depth::Sixteen,
    })
}

/// A typed error for an image that cannot be encoded because a dimension is zero
/// (`to_srgb8` would yield an empty buffer the encoders mishandle), or because
/// the pixel buffer would overflow the requested dimensions.
fn invalid_dimensions(message: &str) -> image::ImageError {
    image::ImageError::Parameter(image::error::ParameterError::from_kind(
        image::error::ParameterErrorKind::Generic(message.to_owned()),
    ))
}

/// Encode a linear-light **working-space** image to 8-bit sRGB and write it to
/// `path`.
///
/// Converts the working space (wide-gamut linear) to linear sRGB, rolls off
/// highlights into display range, gamma-encodes (sRGB OETF), and quantizes to
/// 8 bits via [`to_srgb8`]. The format is chosen from the path's extension
/// (e.g. `.jpg`, `.png`, `.tiff`); an sRGB ICC profile is embedded. Returns an
/// error for a zero dimension or an unsupported extension — never a degenerate
/// or untagged file.
pub fn save(img: &ImageBuf, path: &Path) -> image::ImageResult<()> {
    if img.width() == 0 || img.height() == 0 {
        return Err(invalid_dimensions(
            "cannot encode an image with a zero dimension",
        ));
    }
    let out = image::RgbImage::from_raw(img.width(), img.height(), to_srgb8(img))
        .ok_or_else(|| invalid_dimensions("image dimensions overflow the pixel buffer"))?;
    save_buffer_with_icc(&out, path)
}

/// Encode the image to **16-bit** sRGB (same output transform as [`save`]) and
/// write it to `path`. The extra depth preserves the gradients a wide-gamut,
/// highlight-rolled working pipeline produces, which 8 bits would band. Returns
/// an error for a zero dimension or an unsupported extension.
pub fn save_16(img: &ImageBuf, path: &Path) -> image::ImageResult<()> {
    if img.width() == 0 || img.height() == 0 {
        return Err(invalid_dimensions(
            "cannot encode an image with a zero dimension",
        ));
    }
    let to_srgb = color::linear_working_to_linear_srgb();
    let mut out = image::ImageBuffer::<image::Rgb<u16>, Vec<u16>>::new(img.width(), img.height());
    for y in 0..img.height() {
        for x in 0..img.width() {
            let d = to_display(img.get(x, y), &to_srgb);
            let rgb = std::array::from_fn(|c| (d[c] * 65535.0 + 0.5) as u16);
            out.put_pixel(x, y, image::Rgb(rgb));
        }
    }
    save_buffer_with_icc(&out, path)
}

/// Encode `img` to `path` at the given depth, or — when `depth` is `None` — at
/// the depth [`recommended_depth`] picks for the path's format. The single entry
/// point both the CLI and the editor route exports through, so they cannot drift
/// on which format gets 16 bits. An unsupported extension is rejected by the
/// underlying [`save`]/[`save_16`] (it reaches [`save_buffer_with_icc`] either
/// way), so it surfaces the same typed error regardless of the chosen depth.
pub fn save_auto(img: &ImageBuf, path: &Path, depth: Option<Depth>) -> image::ImageResult<()> {
    // Default by format; for an unsupported extension fall through to `save`,
    // which routes to `save_buffer_with_icc` and raises the extension error.
    match depth.or_else(|| recommended_depth(path)) {
        Some(Depth::Sixteen) => save_16(img, path),
        _ => save(img, path),
    }
}

/// The sRGB ICC profile bytes for tagging output — our output transform produces
/// sRGB, so this matches the data. Generated by `moxcms` (a real, validated
/// profile, not hand-rolled).
fn srgb_icc() -> Vec<u8> {
    moxcms::ColorProfile::new_srgb()
        .encode()
        .expect("encode sRGB ICC profile")
}

/// Write an 8- or 16-bit RGB buffer to `path` with the sRGB ICC profile embedded
/// via the per-format encoder (the high-level `save` can't carry ICC). Format is
/// chosen by extension. An unknown or missing extension is **rejected** with a
/// typed error rather than written untagged — every file this tool emits carries
/// the sRGB profile, so a color-managed viewer reads it correctly.
fn save_buffer_with_icc<P>(
    buf: &image::ImageBuffer<P, Vec<P::Subpixel>>,
    path: &Path,
) -> image::ImageResult<()>
where
    P: image::PixelWithColorType,
    [P::Subpixel]: image::EncodableLayout,
{
    use image::ImageEncoder;
    let Some(ext) = supported_extension(path) else {
        // The empty/no-extension case lands here too (`""` is not supported).
        let bad = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        return Err(image::ImageError::Parameter(
            image::error::ParameterError::from_kind(image::error::ParameterErrorKind::Generic(
                format!("unsupported output extension '{bad}'; use png, tif/tiff, or jpg/jpeg"),
            )),
        ));
    };
    let icc = srgb_icc();
    match ext.as_str() {
        "png" => {
            let mut enc = image::codecs::png::PngEncoder::new(std::fs::File::create(path)?);
            enc.set_icc_profile(icc)
                .map_err(image::ImageError::Unsupported)?;
            buf.write_with_encoder(enc)
        }
        "tif" | "tiff" => {
            let mut enc = image::codecs::tiff::TiffEncoder::new(std::fs::File::create(path)?);
            enc.set_icc_profile(icc)
                .map_err(image::ImageError::Unsupported)?;
            buf.write_with_encoder(enc)
        }
        // The supported set guarantees this is `jpg`/`jpeg`.
        _ => {
            let mut enc = image::codecs::jpeg::JpegEncoder::new(std::fs::File::create(path)?);
            enc.set_icc_profile(icc)
                .map_err(image::ImageError::Unsupported)?;
            buf.write_with_encoder(enc)
        }
    }
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
    fn highlight_rolloff_is_identity_below_knee_and_monotone_above() {
        // The triplet form on a neutral input reduces to the scalar curve on each
        // channel: identity below the knee, monotone and bounded above it.
        let roll = |v: f32| highlight_rolloff_rgb([v, v, v])[0];
        assert_eq!(roll(0.0), 0.0);
        assert_eq!(roll(0.9), 0.9); // below the 0.98 knee: unchanged
        // From the knee up: strictly increasing, always < 1.0 (room kept for more),
        // and white (1.0) barely moves so display stays faithful.
        let (a, b, c) = (roll(1.0), roll(2.0), roll(8.0));
        assert!(0.98 < a && a < b && b < c && c < 1.0, "{a} {b} {c}");
    }

    #[test]
    fn highlight_rolloff_preserves_hue_on_a_saturated_color() {
        // A hot, near-clipped color: per-channel rolloff would compress only the
        // brightest channel, shifting the channel ratios (its hue). The shared
        // factor scales the whole triplet, so the ratios are preserved.
        let hot = [3.0_f32, 1.5, 0.5];
        let out = highlight_rolloff_rgb(hot);
        // Same compression factor on every channel → ratios unchanged.
        let f = out[0] / hot[0];
        for c in 0..3 {
            assert!((out[c] - hot[c] * f).abs() < 1e-6, "ratio drift: {out:?}");
        }
        // And it genuinely compressed (max came down below 1.0, the rolloff target).
        assert!(out[0] < 1.0 && out[0] > 0.98, "max not rolled: {out:?}");

        // Contrast with the old per-channel form, which would shift the hue: the
        // green/red ratio changes because only the hot channel is compressed.
        let per_channel: [f32; 3] = std::array::from_fn(|c| highlight_rolloff(hot[c]));
        let shared_ratio = out[1] / out[0];
        let per_ratio = per_channel[1] / per_channel[0];
        assert!(
            (shared_ratio - per_ratio).abs() > 1e-3,
            "shared and per-channel should differ on a hot color"
        );
    }

    #[test]
    fn save_writes_a_readable_image() {
        // Neutrals are invariant under the working→sRGB transform, so they
        // exercise the encode + rolloff + file round-trip without depending on the
        // working space's primaries.
        let mut img = ImageBuf::new(2, 2);
        img.set(0, 0, [0.0, 0.0, 0.0]); // black
        img.set(1, 0, [1.0, 1.0, 1.0]); // sensor white (rolled off)
        img.set(0, 1, [0.5, 0.5, 0.5]); // mid-gray (below the knee)
        img.set(1, 1, [2.0, 2.0, 2.0]); // highlight headroom

        let path = std::env::temp_dir().join("latent_export_save_test.png");
        save(&img, &path).expect("save should succeed");

        let loaded = image::open(&path)
            .expect("written file should open")
            .to_rgb8();
        assert_eq!(loaded.dimensions(), (2, 2));
        assert_eq!(loaded.get_pixel(0, 0), &image::Rgb([0, 0, 0])); // black
        // Mid-gray (below the knee): faithful, 0.5 linear → sRGB OETF = 188.
        assert_eq!(loaded.get_pixel(0, 1).0, [188, 188, 188]);
        // Display white (1.0) is faithful to within one code (254, not 255) — the
        // rolloff barely moves it — and the >1.0 headroom maps strictly brighter,
        // to 255. Pinned exactly so a future rolloff change can't silently drift.
        assert_eq!(loaded.get_pixel(1, 0).0[0], 254, "white"); // working 1.0
        assert_eq!(loaded.get_pixel(1, 1).0[0], 255, "headroom"); // working 2.0

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_16_writes_a_true_16bit_file() {
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]);
        img.set(1, 0, [0.5, 0.5, 0.5]);
        let path = std::env::temp_dir().join("latent_export_save16_test.tiff");
        save_16(&img, &path).expect("save_16 should succeed");

        let loaded = image::open(&path).expect("written file should open");
        assert!(
            matches!(loaded.color(), image::ColorType::Rgb16),
            "expected 16-bit, got {:?}",
            loaded.color()
        );
        let px = loaded.to_rgb16();
        assert_eq!(px.get_pixel(0, 0).0, [0, 0, 0]);
        // 0.5 linear → sRGB OETF ≈ 0.7354 → ~48196 at 16 bits — and not the 8-bit
        // value scaled up (188·257), proving real 16-bit precision.
        let gray = px.get_pixel(1, 0).0[0];
        assert!((gray as i32 - 48196).abs() < 200, "gray16: {gray}");
        assert_ne!(gray, 188 * 257, "carries 16-bit precision, not 8-bit");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_embeds_a_readable_icc_profile() {
        use image::ImageDecoder;
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.5, 0.5, 0.5]);
        let path = std::env::temp_dir().join("latent_export_icc_test.png");
        save(&img, &path).expect("save should succeed");

        let mut decoder = image::codecs::png::PngDecoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).unwrap(),
        ))
        .expect("decode png");
        let embedded = decoder.icc_profile().expect("icc read ok");
        assert!(
            embedded.is_some_and(|p| !p.is_empty()),
            "output should carry an embedded ICC profile"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_jpg_embeds_icc() {
        // The JPEG path shares `save_buffer_with_icc` with png/tiff, but only
        // png/tiff were covered; pin that a `.jpg` carries the profile too.
        use image::ImageDecoder;
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.5, 0.5, 0.5]);
        let path = std::env::temp_dir().join("latent_export_icc_jpg_test.jpg");
        save(&img, &path).expect("save should succeed");

        let mut decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).unwrap(),
        ))
        .expect("decode jpeg");
        let embedded = decoder.icc_profile().expect("icc read ok");
        assert!(
            embedded.is_some_and(|p| !p.is_empty()),
            "jpeg output should carry an embedded ICC profile"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_rejects_unknown_extension() {
        // An unsupported or typo'd extension must never write an untagged file;
        // it returns an error naming the bad extension and the supported set.
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.5, 0.5, 0.5]);
        let path = std::env::temp_dir().join("latent_export_unknown_ext_test.bmp");
        std::fs::remove_file(&path).ok();

        let err = save(&img, &path).expect_err("unsupported extension should error");
        let msg = err.to_string();
        assert!(
            msg.contains("bmp"),
            "message names the bad extension: {msg}"
        );
        assert!(
            msg.contains("png"),
            "message names the supported set: {msg}"
        );
        // No file written on the rejected path.
        assert!(
            !path.exists(),
            "no file should be written for a rejected extension"
        );

        // The 16-bit and no-extension paths reject identically.
        assert!(save_16(&img, &path).is_err());
        let no_ext = std::env::temp_dir().join("latent_export_no_ext_test");
        assert!(save(&img, &no_ext).is_err());
        assert!(!no_ext.exists());
    }

    #[test]
    fn save_rejects_zero_dimension() {
        // A zero-dimension image yields an empty buffer that slips past the
        // length check; both encoders must return an error and write nothing.
        let zero_w = ImageBuf::new(0, 4);
        let zero_h = ImageBuf::new(4, 0);
        let zero = ImageBuf::new(0, 0);

        for img in [&zero_w, &zero_h, &zero] {
            let path = std::env::temp_dir().join("latent_export_zero_dim_test.png");
            std::fs::remove_file(&path).ok();
            let err = save(img, &path).expect_err("zero dimension should error");
            assert!(err.to_string().contains("zero dimension"), "message: {err}");
            assert!(
                !path.exists(),
                "no file should be written for a zero dimension"
            );

            let tpath = std::env::temp_dir().join("latent_export_zero_dim16_test.tiff");
            std::fs::remove_file(&tpath).ok();
            assert!(save_16(img, &tpath).is_err());
            assert!(!tpath.exists());
        }
    }

    #[test]
    fn recommended_depth_routes_by_format() {
        // 16-bit for the formats whose precision the pipeline can fill, 8-bit for
        // JPEG, and `None` (deferred to the extension rejection) for the rest.
        assert_eq!(recommended_depth(Path::new("o.tiff")), Some(Depth::Sixteen));
        assert_eq!(recommended_depth(Path::new("o.tif")), Some(Depth::Sixteen));
        assert_eq!(recommended_depth(Path::new("o.png")), Some(Depth::Sixteen));
        assert_eq!(recommended_depth(Path::new("o.jpg")), Some(Depth::Eight));
        assert_eq!(recommended_depth(Path::new("o.jpeg")), Some(Depth::Eight));
        assert_eq!(recommended_depth(Path::new("o.bmp")), None);
        assert_eq!(recommended_depth(Path::new("noext")), None);
    }

    #[test]
    fn save_auto_picks_16bit_for_tiff_and_8bit_for_jpeg() {
        // `save_auto` with no explicit depth must produce a 16-bit TIFF and an
        // 8-bit JPEG, and honor an explicit override.
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]);
        img.set(1, 0, [0.5, 0.5, 0.5]);

        let tiff = std::env::temp_dir().join("latent_export_auto_tiff_test.tiff");
        save_auto(&img, &tiff, None).expect("auto tiff");
        assert!(matches!(
            image::open(&tiff).unwrap().color(),
            image::ColorType::Rgb16
        ));
        std::fs::remove_file(&tiff).ok();

        let jpg = std::env::temp_dir().join("latent_export_auto_jpg_test.jpg");
        save_auto(&img, &jpg, None).expect("auto jpg");
        assert!(matches!(
            image::open(&jpg).unwrap().color(),
            image::ColorType::Rgb8
        ));
        std::fs::remove_file(&jpg).ok();

        // An explicit 8-bit override forces Rgb8 even on a .tiff.
        let tiff8 = std::env::temp_dir().join("latent_export_auto_tiff8_test.tiff");
        save_auto(&img, &tiff8, Some(Depth::Eight)).expect("forced 8-bit tiff");
        assert!(matches!(
            image::open(&tiff8).unwrap().color(),
            image::ColorType::Rgb8
        ));
        std::fs::remove_file(&tiff8).ok();
    }

    #[test]
    fn save_16_embeds_a_readable_icc_profile() {
        // The 16-bit TIFF path must also carry the profile (a different encoder
        // than PNG), so the tag isn't silently dropped for the high-bit export.
        use image::ImageDecoder;
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.5, 0.5, 0.5]);
        let path = std::env::temp_dir().join("latent_export_icc16_test.tiff");
        save_16(&img, &path).expect("save_16 should succeed");

        let mut decoder = image::codecs::tiff::TiffDecoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).unwrap(),
        ))
        .expect("decode tiff");
        let embedded = decoder.icc_profile().expect("icc read ok");
        assert!(
            embedded.is_some_and(|p| !p.is_empty()),
            "16-bit TIFF output should carry an embedded ICC profile"
        );
        std::fs::remove_file(&path).ok();
    }
}
