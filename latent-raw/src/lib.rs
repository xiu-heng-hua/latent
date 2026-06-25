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
    /// White (saturation) level — the largest meaningful sample value.
    pub white: u32,
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
    /// Normalize the raw CFA mosaic to linear floats.
    ///
    /// Maps the black level to 0.0 and the white level to 1.0 via
    /// `(sample - black) / (white - black)`. The result is still a mosaic (one
    /// value per photosite) — demosaic happens later. The floor is clamped to
    /// 0, but values above 1.0 (samples past the white level) are kept so
    /// highlight detail survives; saturation is tracked separately by the clip
    /// mask.
    pub fn normalized(&self) -> Vec<f32> {
        let base = self.meta.black as f32;
        let white = self.meta.white as f32;
        let w = self.width as usize;
        self.mosaic
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                // Per-CFA-channel black: the base pedestal plus this channel's
                // `cblack` offset. Many bodies deliver the pedestal here (sometimes
                // with `black == 0`), so ignoring it leaves raised, tinted shadows.
                let color = self.meta.cfa[(i / w % 2) * 2 + (i % w % 2)] as usize;
                let black = base + self.meta.cblack[color] as f32;
                // `.max(1.0)` guards a corrupt `white <= black` against a non-finite
                // or negative scale; each channel is normalized by its own range so
                // a saturated sample lands at 1.0 in every channel.
                let scale = 1.0 / (white - black).max(1.0);
                ((s as f32 - black) * scale).max(0.0)
            })
            .collect()
    }

    /// A per-photosite mask of saturated (clipped) samples: `true` where the
    /// raw sample reached the white level and its true value was lost.
    ///
    /// Computed on the raw integers (before normalization) for an exact test. The
    /// comparison is widened to `u32` so a white level above 65535 (deep-bit or
    /// float sensors) isn't truncated into a false all-clipped mask.
    pub fn clip_mask(&self) -> Vec<bool> {
        self.mosaic
            .iter()
            .map(|&s| s as u32 >= self.meta.white)
            .collect()
    }

    /// Apply white balance to a normalized mosaic in place.
    ///
    /// Each photosite is multiplied by its CFA channel's gain (`cam_mul`
    /// normalized so green = 1.0). Done on the mosaic *before* demosaic because
    /// white balance is a per-CFA-channel property.
    pub fn apply_white_balance(&self, mosaic: &mut [f32]) {
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

    /// Malvar-He-Cutler estimate of the full RGB at one interior pixel.
    ///
    /// Gradient-corrected bilinear: the missing channels come from 5x5 linear
    /// filters that add a correction from the center channel's local gradient,
    /// exploiting inter-channel correlation to cut blur and color fringing.
    /// Caller must guarantee the 5x5 window is in bounds.
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

        let w = self.width as usize;
        let conv = |k: &[[f32; 5]; 5]| -> f32 {
            let mut s = 0.0;
            for (dy, krow) in k.iter().enumerate() {
                for (dx, &coef) in krow.iter().enumerate() {
                    s += coef * mosaic[(y + dy - 2) * w + (x + dx - 2)];
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

    /// Malvar-He-Cutler demosaic: higher quality than bilinear. Interior pixels
    /// use the 5x5 MHC filters; the 2-pixel border falls back to bilinear.
    pub fn demosaic_mhc(&self, mosaic: &[f32]) -> ImageBuf {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut img = ImageBuf::new(self.width, self.height);
        for y in 0..h {
            for x in 0..w {
                let rgb = if x >= 2 && y >= 2 && x + 2 < w && y + 2 < h {
                    self.mhc_pixel(mosaic, x, y)
                } else {
                    self.bilinear_pixel(mosaic, x, y)
                };
                img.set(x as u32, y as u32, rgb);
            }
        }
        img
    }

    /// Which RGB channels at `(x, y)` were reconstructed from a *saturated*
    /// photosite, per the exact raw clip mask. The known (center) channel is exact
    /// — clipped iff its own photosite saturated; an interpolated channel is
    /// treated as clipped if any same-color photosite in the 3x3 neighborhood
    /// saturated, mirroring how demosaic draws that channel from those samples.
    fn clipped_channels(&self, x: usize, y: usize, mask: &[bool]) -> [bool; 3] {
        let (w, h) = (self.width as usize, self.height as usize);
        let center_ch = self.channel_at(x, y);
        let mut clipped = [false; 3];
        clipped[center_ch] = mask[y * w + x];
        for dy in -1_i32..=1 {
            for dx in -1_i32..=1 {
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
    /// flattened to neutral. Finer color propagation can come later.
    pub fn reconstruct_highlights(&self, img: &mut ImageBuf) {
        let mask = self.clip_mask();
        let (w, h) = (self.width as usize, self.height as usize);
        for y in 0..h {
            for x in 0..w {
                let clipped = self.clipped_channels(x, y, &mask);
                if clipped.iter().filter(|&&c| c).count() >= 2 {
                    let px = img.get(x as u32, y as u32);
                    let peak = px[0].max(px[1]).max(px[2]);
                    let rebuilt = std::array::from_fn(|c| if clipped[c] { peak } else { px[c] });
                    img.set(x as u32, y as u32, rebuilt);
                }
            }
        }
    }

    /// The camera → linear-working color matrix built from this file's metadata.
    ///
    /// `cam_xyz` is the XYZ → camera matrix (its first three rows form the 3x3
    /// used here); composing its inverse with XYZ → working lifts demosaiced
    /// camera RGB into the working space. White balance is already applied once on
    /// the mosaic, so the result is row-normalized to keep a neutral input neutral
    /// (no double white-balance). Returns `None` if the matrix is singular.
    pub fn color_matrix(&self) -> Option<Mat3> {
        let x = self.meta.cam_xyz;
        let xyz_to_cam = Mat3([x[0], x[1], x[2]]);
        color::camera_to_working(xyz_to_cam)
    }
}

/// Whether the sensor is a standard RGB Bayer mosaic (`cdesc == "RGBG"`), the
/// only CFA our channel map and demosaic handle. CYGM/RGBE and other layouts have
/// a different `cdesc` and would be mis-colored, so they are rejected at decode.
fn is_rgb_bayer(cdesc: &[u8; 4]) -> bool {
    cdesc == b"RGBG"
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

        let len = width as usize * height as usize;
        let mosaic = std::slice::from_raw_parts(samples, len).to_vec();
        let meta = read_metadata(raw);

        // We only demosaic standard RGB Bayer; reject other CFAs (CYGM, RGBE, …)
        // rather than silently mis-coloring them through the RGB-only channel map.
        if !is_rgb_bayer(&meta.cdesc) {
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

/// Read sensor metadata from an opened+unpacked LibRaw handle.
///
/// # Safety
/// `raw` must be a non-null, successfully unpacked `libraw_data_t`.
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

unsafe fn read_metadata(raw: *mut ffi::libraw_data_t) -> Metadata {
    let color = unsafe { &(*raw).color };
    let idata = unsafe { &(*raw).idata };
    let other = unsafe { &(*raw).other };
    let lens = unsafe { &(*raw).lens };

    // The 2x2 CFA: ask LibRaw which color each of the top-left photosites is.
    let mut cfa = [0_u8; 4];
    for row in 0..2 {
        for col in 0..2 {
            cfa[row * 2 + col] = unsafe { ffi::libraw_COLOR(raw, row as i32, col as i32) } as u8;
        }
    }

    let cam_xyz = std::array::from_fn(|r| std::array::from_fn(|c| color.cam_xyz[r][c]));

    Metadata {
        black: color.black,
        cblack: std::array::from_fn(|i| color.cblack[i]),
        white: color.maximum,
        cam_mul: color.cam_mul,
        cfa,
        cdesc: std::array::from_fn(|i| idata.cdesc[i] as u8),
        cam_xyz,
        make: c_str_field(&idata.make),
        model: c_str_field(&idata.model),
        lens: c_str_field(&lens.Lens),
        focal_len: other.focal_len,
        aperture: other.aperture,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                white,
                cam_mul: [1.0; 4],
                cfa: [0, 1, 1, 2],
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
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

    #[test]
    fn only_rgb_bayer_sensors_are_supported() {
        // The decode guard accepts standard RGB Bayer and rejects other CFAs.
        assert!(is_rgb_bayer(b"RGBG"));
        assert!(!is_rgb_bayer(b"CYGM"));
        assert!(!is_rgb_bayer(b"RGBE"));
    }

    #[test]
    fn white_balance_neutralizes_a_gray_patch() {
        // 2x2 RGGB sensor; cam_mul gains R=2.0, G=1.0, B=1.5.
        let raw = RawImage {
            width: 2,
            height: 2,
            mosaic: vec![],
            meta: Metadata {
                black: 0,
                cblack: [0; 4],
                white: 16383,
                cam_mul: [2.0, 1.0, 1.5, 1.0],
                cfa: [0, 1, 1, 2], // RGGB
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
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
        // A white-balanced neutral patch is camera RGB [v,v,v]; the color matrix
        // must keep it neutral (white balance applied exactly once, not twice).
        for v in [0.25_f32, 0.5, 1.0] {
            let out = m.mul_vec([v, v, v]);
            for c in out {
                assert!((c - v).abs() < 1e-5, "drifted from neutral: {out:?}");
            }
        }
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
                white: 16383,
                cam_mul: [2.0, 1.0, 1.5, 1.0],
                cfa: [0, 1, 1, 2],
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
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
                white: 16383,
                cam_mul: [1.0; 4],
                cfa,
                cdesc: *b"RGBG",
                cam_xyz: [[0.0; 3]; 4],
                make: String::new(),
                model: String::new(),
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
}
