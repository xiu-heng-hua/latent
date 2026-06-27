//! RAW decoding via LibRaw (FFI, unpack-only).

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use latent_image::ImageBuf;
use latent_image::color::{self, Mat3};

/// Bindings generated from `libraw/libraw.h` by bindgen (see build.rs).
#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code
)]
#[allow(unnecessary_transmutes)]
#[allow(clippy::all)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

/// The version string of the LibRaw library we are linked against.
pub fn version() -> String {
    // SAFETY: `libraw_version` returns a pointer to a static, NUL-terminated
    // C string owned by LibRaw; it is valid for the duration of the program.
    let ptr = unsafe { ffi::libraw_version() };
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Something went wrong decoding a RAW file. Errors are typed so callers can
/// react; we never panic across the FFI boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawError {
    /// The path contained an interior NUL byte and can't become a C string.
    InvalidPath,
    /// `libraw_init` returned null.
    Init,
    /// `libraw_open_file` failed (carries LibRaw's error code).
    Open(i32),
    /// `libraw_unpack` failed (carries LibRaw's error code).
    Unpack(i32),
    /// The file unpacked but exposes no Bayer mosaic (unsupported sensor).
    NoMosaic,
}

impl std::fmt::Display for RawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RawError::InvalidPath => write!(f, "path contains a NUL byte"),
            RawError::Init => write!(f, "libraw_init failed"),
            RawError::Open(code) => write!(f, "libraw_open_file failed: {}", strerror(*code)),
            RawError::Unpack(code) => write!(f, "libraw_unpack failed: {}", strerror(*code)),
            RawError::NoMosaic => write!(f, "no Bayer mosaic (unsupported sensor?)"),
        }
    }
}

impl std::error::Error for RawError {}

/// LibRaw's human-readable message for one of its error codes.
fn strerror(code: i32) -> String {
    // SAFETY: libraw_strerror returns a static, NUL-terminated C string.
    let ptr = unsafe { ffi::libraw_strerror(code) };
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// RAII owner of a LibRaw handle: `libraw_close` runs on drop, so the handle is
/// freed exactly once on every path (success or early error return) — no leak,
/// no double-free, no manual cleanup.
struct Handle(*mut ffi::libraw_data_t);

impl Handle {
    fn new() -> Result<Self, RawError> {
        // SAFETY: libraw_init(0) allocates a handle or returns null.
        let ptr = unsafe { ffi::libraw_init(0) };
        if ptr.is_null() {
            Err(RawError::Init)
        } else {
            Ok(Handle(ptr))
        }
    }

    fn as_ptr(&self) -> *mut ffi::libraw_data_t {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid handle from libraw_init, closed once.
        unsafe { ffi::libraw_close(self.0) };
    }
}

/// A repeating 2-D black-level pattern, tiled across the sensor.
///
/// LibRaw stores this in `color.cblack[4..]`: index `4` is the pattern width,
/// index `5` is the height, and indices `6 .. 6 + w*h` are the `w × h` grid in
/// row-major order. It mirrors DNG's `BlackLevel` matrix with `BlackLevelRepeatDim`.
/// When `w == 0 || h == 0` there is no pattern (the common Bayer case) and `grid`
/// is empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CblackPattern {
    /// Pattern width (columns); `0` when there is no 2-D pattern.
    pub w: u32,
    /// Pattern height (rows); `0` when there is no 2-D pattern.
    pub h: u32,
    /// The `w × h` black offsets in row-major order; empty when there is no pattern.
    pub grid: Vec<u32>,
}

impl CblackPattern {
    /// The black offset this pattern contributes at sensor position `(row, col)`,
    /// or `0` when there is no pattern. The grid tiles, so the offset repeats every
    /// `w` columns and `h` rows.
    pub fn offset_at(&self, row: usize, col: usize) -> u32 {
        let (w, h) = (self.w as usize, self.h as usize);
        if w == 0 || h == 0 {
            return 0;
        }
        // `grid.len() == w * h` is established at construction, so this index is in
        // bounds; guard anyway so a hand-built pattern can't panic.
        self.grid
            .get((row % h) * w + (col % w))
            .copied()
            .unwrap_or(0)
    }
}

/// Build a [`CblackPattern`] from the raw `color.cblack[]` FFI array.
///
/// Indices `[4]`/`[5]` are the pattern width/height and `[6 ..]` the row-major
/// grid. The dimensions are validated defensively: an over-range `w * h` (one that
/// would read past the fixed 4104-element array) or a zero dimension yields the
/// empty "no pattern" case rather than reading garbage or panicking.
fn cblack_pattern(cblack: &[u32]) -> CblackPattern {
    let w = cblack.get(4).copied().unwrap_or(0);
    let h = cblack.get(5).copied().unwrap_or(0);
    // The grid lives at `cblack[6 .. 6 + w*h]`; reject any dimensions whose product
    // (or its offset window) would exceed the source array — treat as no pattern.
    let count = match (w as usize).checked_mul(h as usize) {
        Some(n) => n,
        None => return CblackPattern::default(),
    };
    if w == 0 || h == 0 || 6 + count > cblack.len() {
        return CblackPattern::default();
    }
    CblackPattern {
        w,
        h,
        grid: cblack[6..6 + count].to_vec(),
    }
}

/// Sensor metadata read from the RAW file (everything beyond the pixels).
///
/// These values drive later stages: black/white levels normalize the mosaic,
/// `cam_mul` is the as-shot white balance, `cfa`/`cdesc` say which photosite is
/// which color, and `cam_xyz` is the camera→XYZ color matrix.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// Base black level (samples at/below this are "no light").
    pub black: u32,
    /// Per-CFA-channel black offsets, added on top of `black`.
    pub cblack: [u32; 4],
    /// The repeating 2-D black-level pattern (LibRaw's `cblack[4..]`, mirroring
    /// DNG `BlackLevel` + `BlackLevelRepeatDim`). Some bodies store their pedestal
    /// here as a `w × h` grid tiled across the sensor; the per-pixel black at
    /// `(row, col)` adds `grid[(row % h) * w + (col % w)]` on top of [`Self::black`]
    /// and [`Self::cblack`]. `w == 0 || h == 0` means there is no 2-D pattern (the
    /// common Bayer case) and `grid` is empty.
    pub cblack_pattern: CblackPattern,
    /// Per-CFA-channel white (saturation) level — LibRaw's `linear_max`. A `0`
    /// entry means "unset": fall back to [`Self::white`] for that channel.
    pub linear_max: [u32; 4],
    /// White (saturation) level — the largest meaningful sample value.
    pub white: u32,
    /// LibRaw's `filters` code: the CFA bit-mask. `0` is a non-Bayer/linear sensor
    /// (Foveon, full-color); `9` is X-Trans; anything else is a 2×2 Bayer phase.
    pub filters: u32,
    /// Number of distinct sensor colors (LibRaw's `colors`): `3` for RGB Bayer.
    pub colors: u32,
    /// Source row stride of the raw buffer in `u16` samples (LibRaw's `raw_pitch`
    /// in bytes, halved). `0` means the buffer is tightly packed at the raw width.
    pub raw_pitch: u32,
    /// As-shot white-balance multipliers, one per CFA channel (R, G1, B, G2).
    pub cam_mul: [f32; 4],
    /// The 2x2 CFA layout as color indices (into `cdesc`), row-major.
    pub cfa: [u8; 4],
    /// Color descriptor, e.g. `b"RGBG"` — maps a CFA index to a color letter.
    pub cdesc: [u8; 4],
    /// Camera→XYZ color matrix (one row per camera channel).
    pub cam_xyz: [[f32; 3]; 4],
    /// Camera maker, as in EXIF (e.g. `"Canon"`); empty if unknown.
    pub make: String,
    /// Camera model, as in EXIF (e.g. `"Canon EOS 5D Mark III"`); empty if unknown.
    pub model: String,
    /// LibRaw's standardized camera maker, mapping rebrands onto the primary
    /// vendor (e.g. `"NIKON CORPORATION"` → `"Nikon"`); a lens-database lookup aid,
    /// not for display. Empty if unknown.
    pub normalized_make: String,
    /// LibRaw's standardized camera model, a lens-database lookup aid, not for
    /// display. Empty if unknown.
    pub normalized_model: String,
    /// Lens model, as in EXIF (e.g. `"Canon EF 16-35mm f/2.8L II USM"`); empty if unknown.
    pub lens: String,
    /// Focal length in mm at capture, or `0` if unknown.
    pub focal_len: f32,
    /// Aperture (f-number) at capture, or `0` if unknown.
    pub aperture: f32,
}

/// A decoded RAW file that owns its sensor data.
///
/// The mosaic is the data *before* demosaic — one raw u16 sample per photosite,
/// in the camera's CFA layout — plus the [`Metadata`] needed to interpret it.
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    pub mosaic: Vec<u16>,
    pub meta: Metadata,
}

impl RawImage {
    /// The CFA color index (`0..4`) of the photosite at `(row, col)`.
    fn cfa_color(&self, row: usize, col: usize) -> usize {
        self.meta.cfa[(row % 2) * 2 + (col % 2)] as usize
    }

    /// The per-CFA-channel white (saturation) level: the camera's `linear_max[c]`
    /// where the sensor reports one, else the single scalar `maximum`.
    ///
    /// `linear_max` is LibRaw's per-channel saturation calibration; a `0` entry
    /// means "unset", in which case the scalar `maximum` is the best estimate. Using
    /// the per-channel value where available avoids the magenta highlights a single
    /// miscalibrated `maximum` produces.
    fn channel_white(&self, color: usize) -> u32 {
        let lm = self.meta.linear_max[color];
        if lm != 0 { lm } else { self.meta.white }
    }

    /// The per-pixel black pedestal at `(row, col)`: the base black, this channel's
    /// `cblack` offset, and the repeating 2-D `cblack` pattern (when present). Many
    /// bodies deliver the pedestal in these fields (sometimes with `black == 0`), so
    /// ignoring them leaves raised, tinted shadows.
    fn channel_black(&self, color: usize, row: usize, col: usize) -> u32 {
        self.meta.black + self.meta.cblack[color] + self.meta.cblack_pattern.offset_at(row, col)
    }

    /// Normalize the raw CFA mosaic to linear floats.
    ///
    /// Maps each photosite's black level to 0.0 and its channel's white level to
    /// 1.0 via `(sample - black) / (white - black)`. Black is the per-pixel pedestal
    /// (base + per-channel `cblack` + the 2-D `cblack` pattern); white is the
    /// per-channel `linear_max` (falling back to `maximum`). The result is still a
    /// mosaic (one value per photosite) — demosaic happens later. The floor is
    /// clamped to 0, but values above 1.0 (samples past the white level) are kept so
    /// highlight detail survives; saturation is tracked separately by the clip mask.
    ///
    /// Invariants (held by [`unpack`]): `mosaic.len() == width * height` and every
    /// `cfa` entry is in `0..4`.
    pub fn normalized(&self) -> Vec<f32> {
        debug_assert_eq!(
            self.mosaic.len(),
            self.width as usize * self.height as usize
        );
        let w = self.width as usize;
        self.mosaic
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let (row, col) = (i / w, i % w);
                let color = self.cfa_color(row, col);
                let black = self.channel_black(color, row, col) as f32;
                let white = self.channel_white(color) as f32;
                // A single per-channel range `(white_c - black_c)`: white from
                // `linear_max[c]` so any per-channel scale is an intentional
                // colorimetric choice, not a side effect of per-channel black.
                // `.max(1.0)` guards a corrupt `white <= black` against a
                // non-finite or negative scale.
                let scale = 1.0 / (white - black).max(1.0);
                ((s as f32 - black) * scale).max(0.0)
            })
            .collect()
    }

    /// A per-photosite mask of saturated (clipped) samples: `true` where the
    /// raw sample reached its channel's white level and its true value was lost.
    ///
    /// Computed on the raw integers (before normalization) for an exact test, and
    /// per CFA channel — a sample is clipped iff it reaches *its own* channel's white
    /// level ([`Self::channel_white`]). The comparison is widened to `u32` so a white
    /// level above 65535 (deep-bit or float sensors) isn't truncated into a false
    /// all-clipped mask.
    pub fn clip_mask(&self) -> Vec<bool> {
        let w = self.width as usize;
        self.mosaic
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let color = self.cfa_color(i / w, i % w);
                s as u32 >= self.channel_white(color)
            })
            .collect()
    }

    /// Apply white balance to a normalized mosaic in place.
    ///
    /// Each photosite is multiplied by its CFA channel's gain (`cam_mul`
    /// normalized so green = 1.0). Done on the mosaic *before* demosaic because
    /// white balance is a per-CFA-channel property.
    ///
    /// Invariants (held by [`unpack`]): the passed mosaic has `width * height`
    /// samples and every `cfa` entry is in `0..4`.
    pub fn apply_white_balance(&self, mosaic: &mut [f32]) {
        debug_assert_eq!(mosaic.len(), self.width as usize * self.height as usize);
        let m = &self.meta.cam_mul;
        let g = m[1];
        // Second green (G2) sometimes reads 0 in metadata; treat it as green.
        let g2 = if m[3] != 0.0 { m[3] } else { g };
        // Gains indexed by CFA color (0=R, 1=G, 2=B, 3=G2).
        let gains = [m[0] / g, 1.0, m[2] / g, g2 / g];

        let w = self.width as usize;
        for (i, px) in mosaic.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let color = self.meta.cfa[(y % 2) * 2 + (x % 2)] as usize;
            *px *= gains[color];
        }
    }

    /// RGB channel of a photosite at `(x, y)` from the CFA pattern (G2 → green).
    fn channel_at(&self, x: usize, y: usize) -> usize {
        let c = self.meta.cfa[(y % 2) * 2 + (x % 2)];
        (c == 2) as usize * 2 + (c == 1 || c == 3) as usize
    }

    /// Bilinear estimate of the full RGB at one pixel: the known channel is the
    /// sample itself; each missing channel is the average of the same-colored
    /// samples in the 3x3 neighborhood (clipped at the borders).
    fn bilinear_pixel(&self, mosaic: &[f32], x: usize, y: usize) -> [f32; 3] {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut sum = [0.0_f32; 3];
        let mut count = [0_u32; 3];
        for dy in -1_i32..=1 {
            for dx in -1_i32..=1 {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                let ch = self.channel_at(nx, ny);
                sum[ch] += mosaic[ny * w + nx];
                count[ch] += 1;
            }
        }

        let center_ch = self.channel_at(x, y);
        let center = mosaic[y * w + x];
        std::array::from_fn(|ch| {
            if ch == center_ch {
                center // known channel: use the sample directly
            } else if count[ch] > 0 {
                sum[ch] / count[ch] as f32
            } else {
                center // no same-color neighbor (tiny image): fall back
            }
        })
    }

    /// Bilinear demosaic: reconstruct a full RGB image from a prepared mosaic.
    ///
    /// `mosaic` is the normalized, white-balanced single-channel data. Reads the
    /// CFA pattern, so it works for any Bayer phase.
    pub fn demosaic_bilinear(&self, mosaic: &[f32]) -> ImageBuf {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut img = ImageBuf::new(self.width, self.height);
        for y in 0..h {
            for x in 0..w {
                img.set(x as u32, y as u32, self.bilinear_pixel(mosaic, x, y));
            }
        }
        img
    }

    /// Malvar-He-Cutler estimate of the full RGB at one pixel.
    ///
    /// Gradient-corrected bilinear: the missing channels come from 5x5 linear
    /// filters that add a correction from the center channel's local gradient,
    /// exploiting inter-channel correlation to cut blur and color fringing. Taps
    /// that fall outside the image are reflected back in with a parity-preserving
    /// mirror (see [`reflect_parity`]) so the filter runs to the very edge while each
    /// tap still lands on a same-phase sample.
    fn mhc_pixel(&self, mosaic: &[f32], x: usize, y: usize) -> [f32; 3] {
        // Malvar-He-Cutler 5x5 filters (coefficients; divided by 8 below).
        // Values from Malvar, He & Cutler, "High-Quality Linear Interpolation
        // for Demosaicing of Bayer-Patterned Color Images" (ICASSP 2004), Fig. 2.
        #[rustfmt::skip]
        const G_AT_RB: [[f32; 5]; 5] = [
            [0.0, 0.0, -1.0, 0.0, 0.0],
            [0.0, 0.0,  2.0, 0.0, 0.0],
            [-1.0, 2.0, 4.0, 2.0, -1.0],
            [0.0, 0.0,  2.0, 0.0, 0.0],
            [0.0, 0.0, -1.0, 0.0, 0.0],
        ];
        #[rustfmt::skip]
        const DIAG: [[f32; 5]; 5] = [
            [0.0, 0.0, -1.5, 0.0, 0.0],
            [0.0, 2.0,  0.0, 2.0, 0.0],
            [-1.5, 0.0, 6.0, 0.0, -1.5],
            [0.0, 2.0,  0.0, 2.0, 0.0],
            [0.0, 0.0, -1.5, 0.0, 0.0],
        ];
        // Missing color whose same-color samples are horizontal neighbors.
        #[rustfmt::skip]
        const ROW: [[f32; 5]; 5] = [
            [0.0, 0.0,  0.5, 0.0, 0.0],
            [0.0, -1.0, 0.0, -1.0, 0.0],
            [-1.0, 4.0, 5.0, 4.0, -1.0],
            [0.0, -1.0, 0.0, -1.0, 0.0],
            [0.0, 0.0,  0.5, 0.0, 0.0],
        ];
        // ...and the vertical case (transpose of ROW).
        #[rustfmt::skip]
        const COL: [[f32; 5]; 5] = [
            [0.0, 0.0, -1.0, 0.0, 0.0],
            [0.0, -1.0, 4.0, -1.0, 0.0],
            [0.5, 0.0,  5.0, 0.0, 0.5],
            [0.0, -1.0, 4.0, -1.0, 0.0],
            [0.0, 0.0, -1.0, 0.0, 0.0],
        ];

        let (w, h) = (self.width as usize, self.height as usize);
        let conv = |k: &[[f32; 5]; 5]| -> f32 {
            let mut s = 0.0;
            for (dy, krow) in k.iter().enumerate() {
                let ny = reflect_parity(y as i32 + dy as i32 - 2, h);
                for (dx, &coef) in krow.iter().enumerate() {
                    let nx = reflect_parity(x as i32 + dx as i32 - 2, w);
                    s += coef * mosaic[ny * w + nx];
                }
            }
            s / 8.0
        };

        let center_ch = self.channel_at(x, y);
        let mut rgb = [0.0_f32; 3];
        rgb[center_ch] = mosaic[y * w + x];
        match center_ch {
            0 => {
                rgb[1] = conv(&G_AT_RB);
                rgb[2] = conv(&DIAG);
            }
            2 => {
                rgb[1] = conv(&G_AT_RB);
                rgb[0] = conv(&DIAG);
            }
            _ => {
                // Green site: the horizontal neighbor's color is the one whose
                // samples lie along the row (use ROW); the other uses COL.
                if self.channel_at(x + 1, y) == 0 {
                    rgb[0] = conv(&ROW);
                    rgb[2] = conv(&COL);
                } else {
                    rgb[2] = conv(&ROW);
                    rgb[0] = conv(&COL);
                }
            }
        }
        rgb
    }

    /// Malvar-He-Cutler demosaic: higher quality than bilinear. Every pixel uses
    /// the 5x5 MHC filter, including the 2-pixel border, where out-of-bounds taps
    /// are reflected back in with a parity-preserving mirror so the sharper filter
    /// runs to the very edge without a softer bilinear frame.
    pub fn demosaic_mhc(&self, mosaic: &[f32]) -> ImageBuf {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut img = ImageBuf::new(self.width, self.height);
        for y in 0..h {
            for x in 0..w {
                img.set(x as u32, y as u32, self.mhc_pixel(mosaic, x, y));
            }
        }
        img
    }

    /// Which RGB channels at `(x, y)` were reconstructed from a *saturated*
    /// photosite, per the exact raw clip mask. The known (center) channel is exact
    /// — clipped iff its own photosite saturated; an interpolated channel is
    /// treated as clipped if any same-color photosite in the 5x5 neighborhood
    /// saturated. The radius matches the MHC filter's 5x5 support, so a channel an
    /// MHC reconstruction drew from a saturated sample is flagged — closing the faint
    /// ring the narrower 3x3 scan left at the edge of reconstructed regions.
    fn clipped_channels(&self, x: usize, y: usize, mask: &[bool]) -> [bool; 3] {
        let (w, h) = (self.width as usize, self.height as usize);
        let center_ch = self.channel_at(x, y);
        let mut clipped = [false; 3];
        clipped[center_ch] = mask[y * w + x];
        for dy in -2_i32..=2 {
            for dx in -2_i32..=2 {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let (nx, ny) = (nx as usize, ny as usize);
                let ch = self.channel_at(nx, ny);
                if ch != center_ch && mask[ny * w + nx] {
                    clipped[ch] = true;
                }
            }
        }
        clipped
    }

    /// Reconstruct blown highlights in white-balanced camera RGB (post-demosaic,
    /// before the color matrix).
    ///
    /// At the sensor every channel clips at the same white level; white balance
    /// then scales each channel by its CFA gain, so a neutral highlight that
    /// saturated the sensor lands *colored* (typically pink/magenta) because the
    /// boosted channels were capped at different heights. Using the exact raw clip
    /// mask (not a post-demosaic value threshold), where two or more channels are
    /// blown the blown channels are rebuilt up to the pixel's brightest channel —
    /// but any channel that was actually *measured* is kept, so a genuinely
    /// saturated color (a single blown channel) survives intact instead of being
    /// flattened to neutral.
    ///
    /// The per-pixel rebuild flattens large blown regions to a single `peak` value;
    /// a second [`Self::propagate_highlight_color`] stage then carries hue and chroma
    /// from the surrounding unblown pixels inward across those regions so structure
    /// recovers instead of going flat.
    pub fn reconstruct_highlights(&self, img: &mut ImageBuf) {
        let mask = self.clip_mask();
        let (w, h) = (self.width as usize, self.height as usize);
        // `rebuilt[i]` marks a pixel whose >= 2 blown channels were lifted to `peak`
        // — i.e. one with no measured chroma left, the target of color propagation.
        let mut rebuilt = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let clipped = self.clipped_channels(x, y, &mask);
                if clipped.iter().filter(|&&c| c).count() >= 2 {
                    let px = img.get(x as u32, y as u32);
                    let peak = px[0].max(px[1]).max(px[2]);
                    let new_px = std::array::from_fn(|c| if clipped[c] { peak } else { px[c] });
                    img.set(x as u32, y as u32, new_px);
                    rebuilt[y * w + x] = true;
                }
            }
        }
        self.propagate_highlight_color(img, &rebuilt);
    }

    /// Carry hue and chroma from the boundary of a blown region inward, keeping each
    /// rebuilt pixel's reconstructed lightness.
    ///
    /// The per-pixel rebuild lifts every blown channel to the pixel's `peak`, so a
    /// large blown area becomes a flat neutral plateau with no texture. This pass
    /// diffuses the color (a*/b* in CIE [`Lab`]) of the surrounding *unblown* pixels
    /// across the rebuilt region: each rebuilt pixel adopts the average chroma of its
    /// neighbors while holding its own `L*`, iterated so the color flows in from the
    /// region boundary. The lightness — the trustworthy part of the rebuild — is
    /// never changed, so a genuinely neutral blown highlight stays neutral (no
    /// chroma to pull in) while one adjacent to colored, gradient-bearing scene
    /// content recovers a continuous hue. Lab here treats the white-balanced camera
    /// RGB as working-space RGB; the absolute colorimetry is approximate, but the
    /// pass only *blends* existing neighbor chroma, so the relative structure it
    /// restores is what matters.
    fn propagate_highlight_color(&self, img: &mut ImageBuf, rebuilt: &[bool]) {
        let (w, h) = (self.width as usize, self.height as usize);
        if !rebuilt.iter().any(|&r| r) {
            return; // nothing blown enough to need propagation
        }

        // Per-pixel Lab; the rebuilt pixels' L* is fixed, their a*/b* are diffused.
        let mut lab: Vec<color::Lab> = (0..w * h)
            .map(|i| color::Lab::from_working(img.get((i % w) as u32, (i / w) as u32)))
            .collect();
        // The fixed lightness each rebuilt pixel must keep across the diffusion.
        let fixed_l: Vec<f32> = lab.iter().map(|p| p.l).collect();

        // Jacobi diffusion of (a*, b*) into the rebuilt pixels from their 4-neighbors.
        // A handful of sweeps spreads the boundary chroma several pixels inward, far
        // enough to color the modest blown regions this stage targets.
        let sweeps = 16;
        for _ in 0..sweeps {
            let mut next = lab.clone();
            for y in 0..h {
                for x in 0..w {
                    let i = y * w + x;
                    if !rebuilt[i] {
                        continue; // only rebuilt pixels receive propagated chroma
                    }
                    let mut sum = [0.0_f32; 2];
                    let mut n = 0.0_f32;
                    for (dx, dy) in [(-1_i32, 0_i32), (1, 0), (0, -1), (0, 1)] {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let np = lab[ny as usize * w + nx as usize];
                        sum[0] += np.a;
                        sum[1] += np.b;
                        n += 1.0;
                    }
                    if n > 0.0 {
                        next[i] = color::Lab {
                            l: fixed_l[i],
                            a: sum[0] / n,
                            b: sum[1] / n,
                        };
                    }
                }
            }
            lab = next;
        }

        // Write the diffused chroma back at the rebuilt pixels, holding their L*.
        // A pixel whose diffused chroma is still essentially neutral has nothing to
        // recover (no colored neighbor reached it), so leave the per-pixel rebuild's
        // exact value rather than round-tripping it through Lab — a genuinely neutral
        // blown highlight stays bit-for-bit neutral.
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if rebuilt[i] && lab[i].a.hypot(lab[i].b) > 1e-4 {
                    img.set(x as u32, y as u32, lab[i].to_working());
                }
            }
        }
    }

    /// The camera → linear-working color matrix built from this file's metadata.
    ///
    /// `cam_xyz` is the XYZ → camera matrix (its first three rows form the 3x3
    /// used here). [`color::camera_to_working`] inverts it to camera→XYZ and
    /// Bradford-adapts the capture illuminant to the D50 working white (the DNG
    /// color model). White balance is applied once, upstream on the mosaic by
    /// [`apply_white_balance`], so this matrix receives white-balanced camera RGB
    /// and never re-balances it. Returns `None` if the matrix is singular.
    pub fn color_matrix(&self) -> Option<Mat3> {
        let x = self.meta.cam_xyz;
        let xyz_to_cam = Mat3([x[0], x[1], x[2]]);
        color::camera_to_working(xyz_to_cam)
    }
}

/// Whether the sensor is a standard 2×2 RGB Bayer mosaic — the only CFA our
/// channel map and demosaic handle.
///
/// The load-bearing test is on LibRaw's `filters`/`colors`, *not* `cdesc`: every
/// RGB sensor (including X-Trans and Foveon) reports `cdesc == "RGBG"`, so that
/// string alone is not a sufficient guard. We require:
///
/// * `colors == 3` — exactly three sensor colors (rules out 4-color CYGM/RGBE),
/// * `filters != 0` — a real CFA mosaic exists (rules out Foveon / full-color /
///   already-demosaiced linear sensors, whose `libraw_COLOR` returns the sentinel
///   `6` that would index our 4-element arrays out of bounds), and
/// * `filters != 9` — not the 6×6 X-Trans pattern (which a 2×2 demosaic would
///   scramble).
///
/// The `cdesc == "RGBG"` check is kept as an extra screen for any remaining
/// non-RGB three-color layout, but `filters`/`colors` is what makes the guard sound.
fn is_rgb_bayer(filters: u32, colors: u32, cdesc: &[u8; 4]) -> bool {
    colors == 3 && filters != 0 && filters != 9 && cdesc == b"RGBG"
}

/// Copy a padded raw buffer into a tight `width * height` mosaic.
///
/// `source` is the raw allocation laid out at `pitch` samples per row (`pitch`
/// being the byte stride halved, `>= width`). Each of `height` rows contributes
/// its first `width` samples; the trailing `pitch - width` padding samples are
/// dropped. The destination stays tightly packed so the rest of the pipeline's
/// `y * width + x` indexing is unchanged. Factored out of the `unsafe` load so it
/// is testable without LibRaw.
fn copy_rows_at_pitch(source: &[u16], width: usize, height: usize, pitch: usize) -> Vec<u16> {
    let mut mosaic = Vec::with_capacity(width * height);
    for row in 0..height {
        let start = row * pitch;
        mosaic.extend_from_slice(&source[start..start + width]);
    }
    mosaic
}

/// Reflect a coordinate back into `[0, dim - 1]` while preserving its parity.
///
/// Used to fetch the MHC kernel's border taps: a coordinate just outside an edge
/// is mirrored across that edge so the tap stays on a valid sample. Reflection by
/// an even offset (`-c` on the low side, `2*(dim-1) - c` on the high side) keeps
/// `c % 2` unchanged, so each tap lands on a same-CFA-phase photosite — folding the
/// out-of-bounds tap to a same-color sample rather than swapping the Bayer phase.
/// The loop handles a coordinate that overshoots far enough to bounce off both
/// edges (only possible on a sub-5-pixel image), and clamps the degenerate
/// `dim <= 1` case.
fn reflect_parity(coord: i32, dim: usize) -> usize {
    if dim <= 1 {
        return 0;
    }
    let last = dim as i32 - 1;
    let mut c = coord;
    loop {
        if c < 0 {
            c = -c;
        } else if c > last {
            c = 2 * last - c;
        } else {
            return c as usize;
        }
    }
}

/// Open a RAW file and unpack its sensor mosaic (unpack-only: we never run
/// LibRaw's own demosaic/pipeline).
pub fn unpack(path: &Path) -> Result<RawImage, RawError> {
    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| RawError::InvalidPath)?;

    // The handle frees itself on drop, so every early `?` return still closes it.
    let handle = Handle::new()?;
    let raw = handle.as_ptr();

    // SAFETY: `raw` is a valid handle; we follow LibRaw's open → unpack → read
    // lifecycle, checking each return code and pointer before use.
    unsafe {
        let rc = ffi::libraw_open_file(raw, c_path.as_ptr());
        if rc != 0 {
            return Err(RawError::Open(rc));
        }

        let rc = ffi::libraw_unpack(raw);
        if rc != 0 {
            return Err(RawError::Unpack(rc));
        }

        let width = (*raw).sizes.raw_width as u32;
        let height = (*raw).sizes.raw_height as u32;

        let samples = (*raw).rawdata.raw_image;
        if samples.is_null() {
            return Err(RawError::NoMosaic);
        }

        // `raw_image` is laid out at `raw_pitch` bytes per row, which is *not* always
        // a tight `raw_width * 2`: some unpackers pad rows. Honor the stride so we
        // (a) never over-read past the last row of the allocation, and (b) don't
        // shear the image by treating padding columns as photosites.
        let pitch_u16 = {
            let p = (*raw).sizes.raw_pitch as usize / 2;
            // `raw_pitch == 0` (some paths leave it unset) means tight packing; a
            // pitch narrower than the row is malformed — fall back to the tight width
            // rather than reading garbage.
            if p < width as usize {
                width as usize
            } else {
                p
            }
        };
        // SAFETY: `samples` is non-null (checked above) and points at LibRaw's
        // `raw_alloc` of `pitch_u16 * height` `u16` samples — the full padded
        // allocation, the size LibRaw guarantees for `raw_image`. The slice is read
        // only for the duration of the copy below; `to_vec`/the row copy do not
        // outlive `raw`.
        let source = std::slice::from_raw_parts(samples, pitch_u16 * height as usize);
        let mosaic = copy_rows_at_pitch(source, width as usize, height as usize, pitch_u16);
        let meta = read_metadata(raw);

        // We only demosaic standard 2×2 RGB Bayer; reject X-Trans (`filters == 9`),
        // Foveon/full-color (`filters == 0`), and non-3-color CFAs (CYGM, RGBE, …)
        // rather than mis-coloring them or, worse, panicking later on a `cfa` value
        // of `6`. This guard runs before any `cfa`/`cblack`/`gains` indexing — those
        // happen only in the post-decode pipeline (`normalized`/`apply_white_balance`/
        // demosaic), all reached after `unpack` returns — so the out-of-range index
        // is provably unreachable for a rejected sensor.
        if !is_rgb_bayer(meta.filters, meta.colors, &meta.cdesc) {
            return Err(RawError::NoMosaic);
        }

        Ok(RawImage {
            width,
            height,
            mosaic,
            meta,
        })
    }
    // `handle` drops here → libraw_close runs exactly once.
}

/// Read a NUL-terminated fixed C `char` buffer into an owned `String` (lossy on
/// non-UTF-8, trimmed of trailing whitespace).
fn c_str_field(buf: &[std::os::raw::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

/// Clamp a raw CFA color code to the `0..4` range our 4-element `cblack`/`gains`
/// arrays index.
///
/// `libraw_COLOR()` returns the sentinel `6` for Foveon/full-color sensors; left
/// unclamped it would index those arrays out of bounds and panic. The sensor guard
/// in [`unpack`] rejects such files before any indexing happens, so this clamp is a
/// second line of defense at the FFI boundary — if the guard were ever bypassed the
/// 4-element-array invariant still holds. Standard Bayer codes (`0..4`) pass through.
fn clamp_cfa_code(code: u8) -> u8 {
    if (code as usize) < 4 { code } else { 0 }
}

/// Read sensor metadata from an opened+unpacked LibRaw handle.
///
/// # Safety
/// `raw` must be a non-null, successfully unpacked `libraw_data_t`.
unsafe fn read_metadata(raw: *mut ffi::libraw_data_t) -> Metadata {
    let color = unsafe { &(*raw).color };
    let idata = unsafe { &(*raw).idata };
    let sizes = unsafe { &(*raw).sizes };
    let other = unsafe { &(*raw).other };
    let lens = unsafe { &(*raw).lens };

    // The 2x2 CFA: ask LibRaw which color each of the top-left photosites is, then
    // clamp each code into `0..4` so the 4-element `cblack`/`gains` arrays can never
    // be indexed out of bounds even if the sensor guard is bypassed.
    let mut cfa = [0_u8; 4];
    for row in 0..2 {
        for col in 0..2 {
            let code = unsafe { ffi::libraw_COLOR(raw, row as i32, col as i32) } as u8;
            cfa[row * 2 + col] = clamp_cfa_code(code);
        }
    }

    let cam_xyz = std::array::from_fn(|r| std::array::from_fn(|c| color.cam_xyz[r][c]));

    Metadata {
        black: color.black,
        cblack: std::array::from_fn(|i| color.cblack[i]),
        cblack_pattern: cblack_pattern(&color.cblack),
        linear_max: std::array::from_fn(|i| color.linear_max[i]),
        white: color.maximum,
        filters: idata.filters,
        colors: idata.colors as u32,
        raw_pitch: sizes.raw_pitch / 2,
        cam_mul: color.cam_mul,
        cfa,
        cdesc: std::array::from_fn(|i| idata.cdesc[i] as u8),
        cam_xyz,
        make: c_str_field(&idata.make),
        model: c_str_field(&idata.model),
        normalized_make: c_str_field(&idata.normalized_make),
        normalized_model: c_str_field(&idata.normalized_model),
        lens: c_str_field(&lens.Lens),
        focal_len: other.focal_len,
        aperture: other.aperture,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative real Bayer filter mask (RGGB, LibRaw's `0x94949494`): a
    /// nonzero, non-`9` `filters` value the sensor guard must accept.
    const BAYER_FILTERS: u32 = 0x9494_9494;

    #[test]
    fn reports_a_version() {
        let v = version();
        println!("LibRaw version: {v}");
        assert!(v.chars().next().is_some_and(|c| c.is_ascii_digit()));
    }

    fn raw_with(mosaic: Vec<u16>, black: u32, white: u32) -> RawImage {
        RawImage {
            width: mosaic.len() as u32,
            height: 1,
            mosaic,
            meta: Metadata {
                black,
                cblack: [0; 4],
                cblack_pattern: CblackPattern::default(),
                linear_max: [0; 4],
                white,
                filters: BAYER_FILTERS,
                colors: 3,
                raw_pitch: 0,
                cam_mul: [1.0; 4],
                cfa: [0, 1, 1, 2],
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
                normalized_make: String::new(),
                normalized_model: String::new(),
                lens: String::new(),
                focal_len: 0.0,
                aperture: 0.0,
            },
        }
    }

    #[test]
    fn normalize_maps_black_to_zero_and_white_to_one() {
        // black=1000, white=5000 → midpoint 3000 should land at 0.5.
        let raw = raw_with(vec![1000, 3000, 5000, 500], 1000, 5000);
        let norm = raw.normalized();
        assert_eq!(norm[0], 0.0); // black → 0
        assert_eq!(norm[1], 0.5); // mid-gray → 0.5
        assert_eq!(norm[2], 1.0); // white → 1
        assert_eq!(norm[3], 0.0); // below black is clamped to 0
    }

    #[test]
    fn clip_mask_marks_saturated_samples() {
        // white=5000: only samples at/above 5000 are clipped.
        let raw = raw_with(vec![1000, 4999, 5000, 6000], 1000, 5000);
        assert_eq!(raw.clip_mask(), vec![false, false, true, true]);
    }

    #[test]
    fn normalize_subtracts_per_channel_black() {
        // `cblack` adds a per-CFA pedestal on top of the base black; it must be
        // removed, and each channel normalized so its own white level lands at 1.0.
        // cfa [0,1,1,2] over width 4 → photosites R, G, R, G.
        let mut raw = raw_with(vec![1200, 1000, 5000, 5000], 1000, 5000);
        raw.meta.cblack = [200, 0, 0, 0]; // red photosites carry +200 extra black
        let n = raw.normalized();
        assert!(n[0].abs() < 1e-6, "R black removed: {}", n[0]); // (1200-1200)/3800
        assert!(n[1].abs() < 1e-6, "G black removed: {}", n[1]); // (1000-1000)/4000
        assert!((n[2] - 1.0).abs() < 1e-6, "R white→1: {}", n[2]); // 3800/3800
        assert!((n[3] - 1.0).abs() < 1e-6, "G white→1: {}", n[3]); // 4000/4000
    }

    /// A `width × height` RGGB sensor over the given mosaic (one sample per
    /// photosite), with the supplied black/white levels.
    fn raw_2d(width: u32, height: u32, mosaic: Vec<u16>, black: u32, white: u32) -> RawImage {
        assert_eq!(mosaic.len(), (width * height) as usize);
        let mut raw = raw_with(mosaic, black, white);
        raw.width = width;
        raw.height = height;
        raw
    }

    #[test]
    fn cblack_pattern_parses_dims_and_grid() {
        // A 2×2 pattern: dims at [4],[5], grid at [6..10].
        let mut cblack = [0_u32; 4104];
        cblack[4] = 2; // w
        cblack[5] = 2; // h
        cblack[6..10].copy_from_slice(&[10, 20, 30, 40]);
        let p = cblack_pattern(&cblack);
        assert_eq!((p.w, p.h), (2, 2));
        assert_eq!(p.grid, vec![10, 20, 30, 40]);
        // The grid tiles: offset_at folds (row % h, col % w) into the grid.
        assert_eq!(p.offset_at(0, 0), 10);
        assert_eq!(p.offset_at(0, 1), 20);
        assert_eq!(p.offset_at(1, 0), 30);
        assert_eq!(p.offset_at(1, 1), 40);
        assert_eq!(p.offset_at(2, 2), 10); // wraps to (0,0)
        assert_eq!(p.offset_at(3, 5), 40); // (3%2, 5%2) = (1,1)
    }

    #[test]
    fn read_metadata_captures_cblack_pattern_dims() {
        // `W==0 || H==0` is the common Bayer case: no pattern, zero offset.
        let none = cblack_pattern(&[0_u32; 4104]);
        assert_eq!(none, CblackPattern::default());
        assert_eq!(none.offset_at(7, 3), 0);

        // An over-range `W*H` (one that would read past the 4104-element array) is
        // rejected as "no pattern" rather than panicking.
        let mut huge = [0_u32; 4104];
        huge[4] = 5000;
        huge[5] = 5000;
        assert_eq!(cblack_pattern(&huge), CblackPattern::default());

        // A dimension whose window just exceeds the array is also rejected.
        let mut edge = [0_u32; 4104];
        edge[4] = 4100;
        edge[5] = 1; // 6 + 4100 = 4106 > 4104
        assert_eq!(cblack_pattern(&edge), CblackPattern::default());
    }

    #[test]
    fn cfa_codes_are_clamped_to_bayer_range() {
        // The Foveon `libraw_COLOR` sentinel `6` must not survive into a CFA entry
        // that indexes the 4-element `cblack`/`gains` arrays.
        assert_eq!(clamp_cfa_code(6), 0);
        assert_eq!(clamp_cfa_code(4), 0);
        for c in 0..4 {
            assert_eq!(clamp_cfa_code(c), c, "valid Bayer code passes through");
        }
    }

    #[test]
    fn normalize_folds_2d_black_pattern() {
        // A 4×4 RGGB sensor with a W=2,H=2 cblack pattern. Each of the four pattern
        // cells removes a different pedestal at its photosites; after subtracting
        // base black + pattern, every sample lands at 0.
        let pattern = [11_u32, 22, 33, 44]; // (0,0),(0,1),(1,0),(1,1)
        let mut mosaic = vec![0_u16; 16];
        for row in 0..4usize {
            for col in 0..4usize {
                // sample = base black (100) + the pattern cell for this position.
                let cell = pattern[(row % 2) * 2 + (col % 2)];
                mosaic[row * 4 + col] = (100 + cell) as u16;
            }
        }
        let mut raw = raw_2d(4, 4, mosaic, 100, 5000);
        raw.meta.cblack_pattern = CblackPattern {
            w: 2,
            h: 2,
            grid: pattern.to_vec(),
        };
        let n = raw.normalized();
        for (i, v) in n.iter().enumerate() {
            assert!(v.abs() < 1e-6, "pattern pedestal not removed at {i}: {v}");
        }
    }

    #[test]
    fn per_channel_linear_max_drives_white() {
        // cfa [0,1,1,2] over width 4 → photosites R, G, R, G on row 0. linear_max
        // sets red's white explicitly; green's is unset (0) so it falls back to
        // `maximum`. A sample at its channel's white lands at 1.0 and is clipped.
        let mut raw = raw_2d(4, 1, vec![4000, 6000, 4000, 6000], 0, 6000);
        raw.meta.linear_max = [4000, 0, 0, 0]; // R white = 4000; G unset → maximum 6000
        let n = raw.normalized();
        // R sites (cols 0,2) at 4000 = their linear_max → 1.0.
        assert!((n[0] - 1.0).abs() < 1e-6, "R at linear_max → 1.0: {}", n[0]);
        assert!((n[2] - 1.0).abs() < 1e-6, "R at linear_max → 1.0: {}", n[2]);
        // G sites (cols 1,3) at 6000 = maximum → 1.0.
        assert!((n[1] - 1.0).abs() < 1e-6, "G at maximum → 1.0: {}", n[1]);
        // The clip mask is per-channel too: R at 4000 clipped, G at 6000 clipped.
        let mask = raw.clip_mask();
        assert_eq!(mask, vec![true, true, true, true]);
        // A red sample just below its linear_max is *not* clipped, proving the mask
        // uses the per-channel white rather than the scalar maximum.
        let mut raw = raw_2d(4, 1, vec![3999, 100, 3999, 100], 0, 6000);
        raw.meta.linear_max = [4000, 0, 0, 0];
        assert_eq!(raw.clip_mask(), vec![false, false, false, false]);
    }

    #[test]
    fn consistent_plane_scale_keeps_neutral_neutral() {
        // With a consistent per-plane scale (per-channel white from linear_max, per-
        // channel black), a WB-neutral patch — where every channel shares the same
        // black and white — normalizes to the same value with no per-channel tint.
        let mut raw = raw_2d(4, 1, vec![3000, 3000, 3000, 3000], 1000, 5000);
        raw.meta.linear_max = [5000, 5000, 5000, 5000];
        let n = raw.normalized();
        // (3000-1000)/(5000-1000) = 0.5 for every channel — no drift between them.
        for v in &n {
            assert!((v - 0.5).abs() < 1e-6, "neutral picked up a cast: {v}");
        }
        let spread =
            n.iter().cloned().fold(f32::MIN, f32::max) - n.iter().cloned().fold(f32::MAX, f32::min);
        assert!(spread < 1e-6, "channels drifted apart: {spread}");
    }

    #[test]
    fn padded_pitch_copies_rows_without_shear() {
        // A 3-wide, 2-tall sensor whose source rows are padded to a pitch of 5.
        // The row copy must keep the first 3 samples of each row and drop the 2
        // padding samples, yielding the un-sheared tight mosaic.
        let pitch = 5;
        let (w, h) = (3usize, 2usize);
        // Row 0 = [1,2,3, pad, pad]; row 1 = [4,5,6, pad, pad].
        let source = vec![1, 2, 3, 99, 99, 4, 5, 6, 88, 88];
        let mosaic = copy_rows_at_pitch(&source, w, h, pitch);
        assert_eq!(mosaic, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn raw_pitch_zero_falls_back_to_tight_width() {
        // The tight-packing case: pitch == width reproduces the old behavior exactly
        // (no padding to drop). This mirrors the `raw_pitch == 0 → width` fallback in
        // `unpack`, where a tight buffer copies straight through.
        let (w, h) = (4usize, 3usize);
        let source: Vec<u16> = (0..(w * h) as u16).collect();
        let mosaic = copy_rows_at_pitch(&source, w, h, w);
        assert_eq!(mosaic, source);
    }

    #[test]
    fn only_rgb_bayer_sensors_are_supported() {
        // The decode guard accepts standard 2×2 RGB Bayer (3 colors, a real CFA mask
        // that is neither Foveon nor X-Trans) and rejects everything else.
        assert!(is_rgb_bayer(BAYER_FILTERS, 3, b"RGBG"));
        // Foveon / full-color: no CFA mask.
        assert!(!is_rgb_bayer(0, 3, b"RGBG"));
        // X-Trans: the 6×6 pattern.
        assert!(!is_rgb_bayer(9, 3, b"RGBG"));
        // Four-color CFAs still rejected (CYGM/RGBE have `colors == 4` and a
        // non-RGBG descriptor).
        assert!(!is_rgb_bayer(BAYER_FILTERS, 4, b"CYGM"));
        assert!(!is_rgb_bayer(BAYER_FILTERS, 4, b"RGBE"));
    }

    #[test]
    fn foveon_filters_zero_is_rejected_not_panicked() {
        // A Foveon/full-color sensor reports `filters == 0`, and `libraw_COLOR`
        // would yield a CFA full of the sentinel `6`. The guard must reject it with
        // a typed error before any `cfa` indexing — the path that used to panic at
        // `cblack[6]`/`gains[6]`.
        assert!(!is_rgb_bayer(0, 3, b"RGBG"));
        // The clamp is the second line of defense: even a stray `6` can't index out
        // of bounds.
        assert_eq!(clamp_cfa_code(6), 0);
    }

    #[test]
    fn xtrans_filters_nine_is_rejected() {
        // X-Trans (`filters == 9`) reports RGB filters but a 6×6 mosaic; reject it
        // rather than scrambling it through a 2×2 Bayer demosaic.
        assert!(!is_rgb_bayer(9, 3, b"RGBG"));
    }

    #[test]
    fn white_balance_neutralizes_a_gray_patch() {
        // 2x2 RGGB sensor; cam_mul gains R=2.0, G=1.0, B=1.5. The mosaic carries one
        // sample per photosite so `mosaic.len() == width * height` holds.
        let raw = RawImage {
            width: 2,
            height: 2,
            mosaic: vec![0; 4],
            meta: Metadata {
                black: 0,
                cblack: [0; 4],
                cblack_pattern: CblackPattern::default(),
                linear_max: [0; 4],
                white: 16383,
                filters: BAYER_FILTERS,
                colors: 3,
                raw_pitch: 0,
                cam_mul: [2.0, 1.0, 1.5, 1.0],
                cfa: [0, 1, 1, 2], // RGGB
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
                normalized_make: String::new(),
                normalized_model: String::new(),
                lens: String::new(),
                focal_len: 0.0,
                aperture: 0.0,
            },
        };
        // A neutral gray reads unequal per channel (∝ 1/gain): R, G, G, B.
        let mut mosaic = vec![0.25, 0.5, 0.5, 1.0 / 3.0];
        raw.apply_white_balance(&mut mosaic);
        // After WB every photosite should land on the same gray (~0.5).
        for v in mosaic {
            assert!((v - 0.5).abs() < 1e-6, "expected ~0.5, got {v}");
        }
    }

    #[test]
    fn color_matrix_keeps_a_neutral_patch_neutral() {
        // Arbitrary non-singular stand-in for an XYZ→camera matrix (only the
        // first three rows are used by the 3x3 color conversion).
        let mut raw = raw_grid(4, 4);
        raw.meta.cam_xyz = [
            [1.4, -0.3, -0.1],
            [-0.5, 1.6, -0.1],
            [0.0, -0.4, 1.5],
            [0.0; 3],
        ];
        let m = raw.color_matrix().expect("invertible");
        // A white-balanced neutral patch is camera RGB [v,v,v]; the DNG color
        // matrix must keep it neutral (white balance applied exactly once on the
        // mosaic, never re-applied by this matrix).
        for v in [0.25_f32, 0.5, 1.0] {
            let out = m.mul_vec([v, v, v]);
            for c in out {
                assert!((c - v).abs() < 1e-5, "drifted from neutral: {out:?}");
            }
        }
    }

    #[test]
    fn color_matrix_does_not_double_apply_white_balance() {
        // Regression: a neutral that has been white-balanced on the mosaic must not
        // be darkened or tinted by the color matrix re-applying its own implicit
        // balance. A pure-neutral [v,v,v] in, the exact same gray out — and the
        // three channels stay equal to each other to machine precision.
        let mut raw = raw_grid(4, 4);
        raw.meta.cam_xyz = [
            [1.4, -0.3, -0.1],
            [-0.5, 1.6, -0.1],
            [0.0, -0.4, 1.5],
            [0.0; 3],
        ];
        let m = raw.color_matrix().expect("invertible");
        let out = m.mul_vec([0.6, 0.6, 0.6]);
        let spread = out[0].max(out[1]).max(out[2]) - out[0].min(out[1]).min(out[2]);
        assert!(spread < 1e-5, "neutral picked up a tint: {out:?}");
        assert!(
            (out[1] - 0.6).abs() < 1e-5,
            "neutral darkened/lifted: {out:?}"
        );
    }

    #[test]
    fn reconstruct_highlights_rebuilds_blown_channels_keeping_measured_ones() {
        // 2x2 RGGB. WB gains R=2.0, G=1.0, B=1.5; clip detection uses the exact raw
        // mask, not post-demosaic values.
        let mut raw = RawImage {
            width: 2,
            height: 2,
            mosaic: vec![16383; 4], // every photosite saturated → every channel clipped
            meta: Metadata {
                black: 0,
                cblack: [0; 4],
                cblack_pattern: CblackPattern::default(),
                linear_max: [0; 4],
                white: 16383,
                filters: BAYER_FILTERS,
                colors: 3,
                raw_pitch: 0,
                cam_mul: [2.0, 1.0, 1.5, 1.0],
                cfa: [0, 1, 1, 2],
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
                normalized_make: String::new(),
                normalized_model: String::new(),
                lens: String::new(),
                focal_len: 0.0,
                aperture: 0.0,
            },
        };
        // A neutral highlight that blew the sensor demosaics to a colored cast.
        let mut img = ImageBuf::new(2, 2);
        for p in img.pixels_mut() {
            *p = [2.0, 1.0, 1.5];
        }
        raw.reconstruct_highlights(&mut img);
        // All three channels blown → rebuilt neutral at the peak (2.0).
        assert_eq!(img.get(0, 0), [2.0, 2.0, 2.0]);

        // Now only the red photosites saturate; green/blue are measured fine, so a
        // genuine red highlight (one blown channel) must be kept exactly.
        raw.mosaic = vec![16383, 8000, 8000, 8000];
        let mut img = ImageBuf::new(2, 2);
        img.set(0, 0, [2.0, 0.3, 0.4]); // R site: only red clipped
        img.set(1, 0, [0.4, 0.4, 0.4]);
        img.set(0, 1, [0.4, 0.4, 0.4]);
        img.set(1, 1, [0.4, 0.4, 0.4]);
        raw.reconstruct_highlights(&mut img);
        assert_eq!(img.get(0, 0), [2.0, 0.3, 0.4]); // single blown channel → untouched
    }

    /// A sensor of the given size and CFA pattern, with no pixel data (callers
    /// pass the f32 mosaic to demosaic separately).
    fn raw_grid_cfa(width: u32, height: u32, cfa: [u8; 4]) -> RawImage {
        RawImage {
            width,
            height,
            mosaic: vec![],
            meta: Metadata {
                black: 0,
                cblack: [0; 4],
                cblack_pattern: CblackPattern::default(),
                linear_max: [0; 4],
                white: 16383,
                filters: BAYER_FILTERS,
                colors: 3,
                raw_pitch: 0,
                cam_mul: [1.0; 4],
                cfa,
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
                normalized_make: String::new(),
                normalized_model: String::new(),
                lens: String::new(),
                focal_len: 0.0,
                aperture: 0.0,
            },
        }
    }

    /// An RGGB sensor of the given size.
    fn raw_grid(width: u32, height: u32) -> RawImage {
        raw_grid_cfa(width, height, [0, 1, 1, 2])
    }

    #[test]
    fn clip_support_matches_mhc_5x5() {
        // RGGB: a green photosite sits at (row even, col odd) / (row odd, col even).
        // A red center at (2, 2) interpolates its green channel; the green photosite
        // at (row 0, col 1) is dy = -2, dx = -1 from it — inside the 5x5 MHC support
        // but outside the old 3x3 scan. Saturating it must flag the center's green.
        let (w, h) = (5usize, 5usize);
        let raw = raw_2d(w as u32, h as u32, vec![0; w * h], 0, 5000);
        let mut mask = vec![false; w * h];
        mask[1] = true; // green photosite at (row 0, col 1): flat index 0*w + 1
        let clipped = raw.clipped_channels(2, 2, &mask);
        assert!(clipped[1], "green channel flagged via the 5x5 support");
        // The same saturated green is outside the 3x3 scan, so a 3x3-only propagation
        // would have missed it — confirm nothing one ring narrower would catch it by
        // checking a pixel for which that green is more than 2 away is unflagged.
        let far = raw.clipped_channels(4, 4, &mask); // (row 0,col 1) is dy=-4 → outside
        assert!(!far[1], "green not flagged beyond the 5x5 support");
    }

    #[test]
    fn highlight_propagation_recovers_gradient() {
        // A large blown region beside colored, gradient-bearing scene content must
        // recover non-flat structure (variance > 0) and a hue carried in from the
        // boundary, instead of staying a flat neutral peak.
        let (w, h) = (16, 8);
        let raw = raw_2d(w, h, vec![0; (w * h) as usize], 0, 5000);
        // Mark the right half (x >= 8) as a fully-blown region; the left half is
        // unblown colored content with a smooth horizontal hue gradient.
        let mut rebuilt = vec![false; (w * h) as usize];
        let mut img = ImageBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                if x >= 8 {
                    img.set(x, y, [1.0, 1.0, 1.0]); // flat neutral peak plateau
                    rebuilt[(y * w + x) as usize] = true;
                } else {
                    // A colored gradient: red rises, blue falls across x.
                    let t = x as f32 / 7.0;
                    img.set(x, y, [0.3 + 0.5 * t, 0.4, 0.8 - 0.5 * t]);
                }
            }
        }
        raw.propagate_highlight_color(&mut img, &rebuilt);

        // The blown region is no longer a flat plateau: chroma varies across it.
        let mut chroma = Vec::new();
        for y in 0..h {
            for x in 8..w {
                let lch = color::Lab::from_working(img.get(x, y)).to_lch();
                chroma.push(lch.c);
            }
        }
        let mean = chroma.iter().sum::<f32>() / chroma.len() as f32;
        let var = chroma.iter().map(|c| (c - mean).powi(2)).sum::<f32>() / chroma.len() as f32;
        assert!(
            mean > 1e-2,
            "region stayed neutral, no color recovered: {mean}"
        );
        assert!(
            var > 1e-6,
            "region stayed flat, no structure recovered: {var}"
        );
        // The hue is continuous across the boundary: the first blown column's hue is
        // close to the last unblown column's hue (color flowed in, not jumped).
        let boundary_in = color::Lab::from_working(img.get(7, 4)).to_lch().h;
        let boundary_out = color::Lab::from_working(img.get(8, 4)).to_lch().h;
        assert!(
            (boundary_in - boundary_out).abs() < 0.5,
            "hue discontinuous at boundary: {boundary_in} vs {boundary_out}"
        );
    }

    #[test]
    fn demosaic_of_uniform_mosaic_is_uniform_gray() {
        let raw = raw_grid(4, 4);
        let img = raw.demosaic_bilinear(&[0.5; 16]);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(img.get(x, y), [0.5, 0.5, 0.5]);
            }
        }
    }

    #[test]
    fn demosaic_interpolates_missing_channels() {
        // Red photosites = 1.0, every other photosite = 0.0.
        let raw = raw_grid(4, 4);
        let mut m = vec![0.0_f32; 16];
        for y in 0..4 {
            for x in 0..4 {
                if y % 2 == 0 && x % 2 == 0 {
                    m[y * 4 + x] = 1.0;
                }
            }
        }
        let img = raw.demosaic_bilinear(&m);

        // Blue site (1,1): R = average of 4 diagonal reds = 1.0, G = B = 0.
        assert_eq!(img.get(1, 1), [1.0, 0.0, 0.0]);
        // Green site (1,0): R = average of 2 horizontal reds = 1.0.
        assert_eq!(img.get(1, 0)[0], 1.0);
    }

    // --- Synthetic demosaic harness --------------------------------------
    // Mosaic a known RGB image, demosaic it back, and measure the error.
    // This catches *silent* demosaic bugs and lets us compare algorithms.

    /// CFA color index (0=R,1=G,2=B,3=G2) → RGB channel.
    fn cfa_channel(c: u8) -> usize {
        (c == 2) as usize * 2 + (c == 1 || c == 3) as usize
    }

    /// Forward mosaic: keep only one channel per photosite, per the CFA
    /// pattern. The inverse of demosaicing.
    fn mosaic_rgb(img: &ImageBuf, cfa: [u8; 4]) -> Vec<f32> {
        let w = img.width();
        let mut m = vec![0.0_f32; img.len()];
        for y in 0..img.height() {
            for x in 0..w {
                let color = cfa[((y % 2) * 2 + (x % 2)) as usize];
                m[(y * w + x) as usize] = img.get(x, y)[cfa_channel(color)];
            }
        }
        m
    }

    /// Mean absolute error per channel between two equally-sized images.
    fn mean_abs_error(a: &ImageBuf, b: &ImageBuf) -> f32 {
        let mut sum = 0.0;
        for y in 0..a.height() {
            for x in 0..a.width() {
                let (pa, pb) = (a.get(x, y), b.get(x, y));
                sum += (0..3).map(|c| (pa[c] - pb[c]).abs()).sum::<f32>();
            }
        }
        sum / (a.len() * 3) as f32
    }

    /// Build an RGB image from a per-pixel function.
    fn make_image(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 3]) -> ImageBuf {
        let mut img = ImageBuf::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set(x, y, f(x, y));
            }
        }
        img
    }

    #[test]
    fn roundtrip_of_constant_image_is_lossless() {
        let raw = raw_grid(8, 8);
        let original = make_image(8, 8, |_, _| [0.3, 0.6, 0.9]);
        let mosaic = mosaic_rgb(&original, raw.meta.cfa);
        let restored = raw.demosaic_bilinear(&mosaic);
        assert_eq!(mean_abs_error(&original, &restored), 0.0);
    }

    #[test]
    fn bilinear_reconstructs_a_smooth_gradient_well() {
        // A horizontal linear gradient (R=G=B=x/(w-1)): bilinear interpolation
        // is exact for linear signals, so error should be tiny.
        let (w, h) = (16, 16);
        let raw = raw_grid(w, h);
        let original = make_image(w, h, |x, _| {
            let v = x as f32 / (w - 1) as f32;
            [v, v, v]
        });
        let mosaic = mosaic_rgb(&original, raw.meta.cfa);
        let restored = raw.demosaic_bilinear(&mosaic);

        let err = mean_abs_error(&original, &restored);
        println!("bilinear gradient MAE: {err}");
        assert!(
            err < 0.01,
            "expected near-lossless on a gradient, got {err}"
        );
    }

    #[test]
    fn mhc_beats_bilinear_on_detailed_image() {
        use std::f32::consts::PI;
        // A grayscale image with 2-D detail: MHC's gradient correction should
        // reconstruct it more accurately than independent-channel bilinear.
        let (w, h) = (32, 32);
        let raw = raw_grid(w, h);
        let original = make_image(w, h, |x, y| {
            let v = 0.5
                + 0.25 * (2.0 * PI * x as f32 / 8.0).sin()
                + 0.2 * (2.0 * PI * y as f32 / 6.0).cos();
            [v, v, v]
        });
        let mosaic = mosaic_rgb(&original, raw.meta.cfa);

        let bilinear = mean_abs_error(&original, &raw.demosaic_bilinear(&mosaic));
        let mhc = mean_abs_error(&original, &raw.demosaic_mhc(&mosaic));
        println!("detailed image MAE — bilinear: {bilinear}, mhc: {mhc}");
        assert!(
            mhc < bilinear,
            "MHC ({mhc}) should beat bilinear ({bilinear})"
        );
    }

    #[test]
    fn all_cfa_phases_reconstruct_equally() {
        use std::f32::consts::PI;
        let (w, h) = (32, 32);
        // A colorful image so a wrong CFA phase would swap channels and blow up.
        let image = make_image(w, h, |x, y| {
            let fx = x as f32 / (w - 1) as f32;
            let fy = y as f32 / (h - 1) as f32;
            [fx, 0.5 + 0.4 * (2.0 * PI * fx).sin(), fy]
        });

        let phases = [
            ("RGGB", [0_u8, 1, 1, 2]),
            ("BGGR", [2, 1, 1, 0]),
            ("GRBG", [1, 0, 2, 1]),
            ("GBRG", [1, 2, 0, 1]),
        ];
        for (name, cfa) in phases {
            let raw = raw_grid_cfa(w, h, cfa);
            let mosaic = mosaic_rgb(&image, cfa);
            let err = mean_abs_error(&image, &raw.demosaic_mhc(&mosaic));
            println!("{name} MHC MAE: {err}");
            // Every phase reconstructs accurately; a phase bug would be huge.
            assert!(err < 0.03, "{name} reconstructed poorly: {err}");
        }
    }

    #[test]
    fn reflect_parity_mirrors_and_keeps_parity() {
        // In-bounds coordinates pass straight through.
        for c in 0..5 {
            assert_eq!(reflect_parity(c as i32, 5), c);
        }
        // Just past each edge reflects back in while keeping the coordinate's parity
        // (so a Bayer phase is never swapped).
        assert_eq!(reflect_parity(-1, 5), 1);
        assert_eq!(reflect_parity(-2, 5), 2);
        assert_eq!(reflect_parity(5, 5), 3); // 2*4 - 5
        assert_eq!(reflect_parity(6, 5), 2); // 2*4 - 6
        for c in [-2_i32, -1, 5, 6] {
            let r = reflect_parity(c, 5);
            assert_eq!(r % 2, c.rem_euclid(2) as usize, "parity broken at {c}");
        }
        // Degenerate single-column/row clamps to 0.
        assert_eq!(reflect_parity(3, 1), 0);
    }

    /// Mean absolute error over just the 2-pixel border frame of two images.
    fn border_mean_abs_error(a: &ImageBuf, b: &ImageBuf) -> f32 {
        let (w, h) = (a.width(), a.height());
        let mut sum = 0.0;
        let mut n = 0u32;
        for y in 0..h {
            for x in 0..w {
                if x < 2 || y < 2 || x + 2 >= w || y + 2 >= h {
                    let (pa, pb) = (a.get(x, y), b.get(x, y));
                    sum += (0..3).map(|c| (pa[c] - pb[c]).abs()).sum::<f32>();
                    n += 3;
                }
            }
        }
        sum / n as f32
    }

    #[test]
    fn mhc_border_uses_mhc_not_bilinear() {
        // On a smooth 2-D gradient the all-MHC border must reconstruct the true
        // image at least as accurately as the old bilinear-border output (bilinear is
        // exact on a linear signal, MHC on a smooth one should match it). The MAE is
        // measured on the 2-pixel frame only, where the behavior changed.
        let (w, h) = (16, 16);
        let raw = raw_grid(w, h);
        let original = make_image(w, h, |x, y| {
            let v = (x as f32 + y as f32) / (2.0 * (w - 1) as f32);
            [v, v, v]
        });
        let mosaic = mosaic_rgb(&original, raw.meta.cfa);

        let mhc = raw.demosaic_mhc(&mosaic);
        let bilinear = raw.demosaic_bilinear(&mosaic);
        let mhc_border = border_mean_abs_error(&original, &mhc);
        let bilinear_border = border_mean_abs_error(&original, &bilinear);
        println!("border MAE — mhc: {mhc_border}, bilinear: {bilinear_border}");
        // No bilinear softening: MHC's border is not worse than the bilinear border.
        assert!(
            mhc_border <= bilinear_border + 1e-6,
            "MHC border ({mhc_border}) worse than bilinear ({bilinear_border})"
        );
    }

    #[test]
    fn mhc_border_preserves_cfa_phase() {
        // A colorful image: if the parity-preserving mirror were broken, border taps
        // would read the wrong CFA phase and swap channels, blowing up the error.
        use std::f32::consts::PI;
        let (w, h) = (24, 24);
        let original = make_image(w, h, |x, y| {
            let fx = x as f32 / (w - 1) as f32;
            let fy = y as f32 / (h - 1) as f32;
            [fx, 0.5 + 0.4 * (2.0 * PI * fx).sin(), fy]
        });
        for cfa in [[0_u8, 1, 1, 2], [2, 1, 1, 0], [1, 0, 2, 1], [1, 2, 0, 1]] {
            let raw = raw_grid_cfa(w, h, cfa);
            let mosaic = mosaic_rgb(&original, cfa);
            let err = border_mean_abs_error(&original, &raw.demosaic_mhc(&mosaic));
            // A channel swap at the border would push the error far past this; a
            // correct same-phase border stays small.
            assert!(err < 0.05, "border phase broken for {cfa:?}: {err}");
        }
    }

    #[test]
    fn missing_file_is_a_typed_error() {
        let err = match unpack(Path::new("/no/such/file.nef")) {
            Err(e) => e,
            Ok(_) => panic!("expected an error for a missing file"),
        };
        // A non-existent file fails at open, not with a panic.
        assert!(matches!(err, RawError::Open(_)));
        // And it has a readable message.
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn path_with_interior_nul_is_invalid_path() {
        // A path carrying an interior NUL can't become a C string, so `unpack`
        // returns a typed error before touching LibRaw — no panic, no FFI call.
        let err = match unpack(Path::new("a\0b")) {
            Err(e) => e,
            Ok(_) => panic!("expected InvalidPath for an interior NUL"),
        };
        assert_eq!(err, RawError::InvalidPath);
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn every_raw_error_has_a_readable_message() {
        // Each typed error renders a non-empty, distinct message so callers (and logs)
        // can surface the failure mode. This pins the `Unpack`/`NoMosaic`/`Init`
        // variants' Display alongside the I/O paths exercised by the other tests.
        let errors = [
            RawError::InvalidPath,
            RawError::Init,
            RawError::Open(-1),
            RawError::Unpack(-1),
            RawError::NoMosaic,
        ];
        for e in &errors {
            assert!(!e.to_string().is_empty(), "empty message for {e:?}");
        }
        // The two LibRaw-coded variants name their stage distinctly.
        assert_ne!(
            RawError::Open(-1).to_string(),
            RawError::Unpack(-1).to_string()
        );
        // NoMosaic — the variant the sensor guard returns for X-Trans/Foveon/non-RGB
        // — mentions the missing mosaic.
        assert!(RawError::NoMosaic.to_string().contains("mosaic"));
    }
}
