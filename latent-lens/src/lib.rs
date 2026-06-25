//! A thin, safe wrapper over the system lensfun library (FFI), mirroring the
//! LibRaw boundary in `latent-raw`.
//!
//! lensfun is only the *data source* for lens corrections — the correction math
//! itself is the clean-room engine in `latent-pipeline`. This crate looks a lens
//! up in lensfun's database and maps its model into a [`latent_edit::LensProfile`]
//! the engine can apply. The lensfun *library* is LGPL (linked here); its
//! *database* is CC-BY-SA, installed separately and read at runtime — never
//! vendored into this repository.

use latent_edit::LensProfile;
use std::ffi::CString;

mod ffi {
    #![allow(
        non_upper_case_globals,
        non_camel_case_types,
        non_snake_case,
        dead_code
    )]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

/// An error talking to lensfun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The database handle could not be allocated.
    Alloc,
    /// No lens database could be loaded (the data package is not installed).
    NoDatabase,
    /// A database file was malformed.
    WrongFormat,
}

/// The lensfun library version this build links against, as
/// `(major, minor, micro, bugfix)`.
pub fn version() -> (u32, u32, u32, u32) {
    (
        ffi::LF_VERSION_MAJOR,
        ffi::LF_VERSION_MINOR,
        ffi::LF_VERSION_MICRO,
        ffi::LF_VERSION_BUGFIX,
    )
}

/// An owned handle to a loaded lensfun database (RAII — destroyed on drop).
pub struct Database {
    db: *mut ffi::lfDatabase,
}

impl Database {
    /// Create a database handle and load the system lens-profile data.
    pub fn load() -> Result<Self, Error> {
        // SAFETY: `lf_db_new` allocates a fresh database we exclusively own and
        // destroy in `Drop`; we check for a null allocation.
        let db = unsafe { ffi::lf_db_new() };
        if db.is_null() {
            return Err(Error::Alloc);
        }
        // SAFETY: `db` is a valid handle just returned by `lf_db_new`.
        let code = unsafe { ffi::lf_db_load(db) };
        if code != ffi::lfError_LF_NO_ERROR {
            // SAFETY: destroy the handle we own before returning the error.
            unsafe { ffi::lf_db_destroy(db) };
            return Err(if code == ffi::lfError_LF_WRONG_FORMAT {
                Error::WrongFormat
            } else {
                Error::NoDatabase
            });
        }
        Ok(Self { db })
    }

    /// Look a lens up by camera and lens metadata and build a [`LensProfile`]
    /// interpolated for the shot's `focal` (mm), `aperture` (f-number), and focus
    /// `distance` (m). Returns `None` if the camera or lens is not in the
    /// database. The live query is exercised manually — the database is external,
    /// like real-RAW decode — while the model→profile mapping is unit-tested.
    pub fn find_profile(
        &self,
        camera_maker: &str,
        camera_model: &str,
        lens_model: &str,
        focal: f32,
        aperture: f32,
        distance: f32,
    ) -> Option<LensProfile> {
        let maker = CString::new(camera_maker).ok()?;
        let model = CString::new(camera_model).ok()?;
        let lens = CString::new(lens_model).ok()?;
        // SAFETY: `self.db` is valid for the handle's lifetime; the C strings
        // outlive each call; the returned arrays are NULL-terminated and owned by
        // lensfun (freed with `lf_free`, their elements owned by the database).
        unsafe {
            let cameras = ffi::lf_db_find_cameras(self.db, maker.as_ptr(), model.as_ptr());
            if cameras.is_null() || (*cameras).is_null() {
                if !cameras.is_null() {
                    ffi::lf_free(cameras as *mut _);
                }
                return None;
            }
            let camera = *cameras;
            let lenses =
                ffi::lf_db_find_lenses_hd(self.db, camera, std::ptr::null(), lens.as_ptr(), 0);
            ffi::lf_free(cameras as *mut _);
            if lenses.is_null() || (*lenses).is_null() {
                if !lenses.is_null() {
                    ffi::lf_free(lenses as *mut _);
                }
                return None;
            }
            let profile = lens_to_profile(*lenses, focal, aperture, distance);
            ffi::lf_free(lenses as *mut _);
            Some(profile)
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // SAFETY: `self.db` came from `lf_db_new` and is destroyed exactly once.
        unsafe { ffi::lf_db_destroy(self.db) };
    }
}

/// Interpolate the lens's calibration at the shot parameters and map each model
/// into a [`LensProfile`].
///
/// # Safety
/// `lens` must be a valid `lfLens` pointer owned by a live database.
unsafe fn lens_to_profile(
    lens: *const ffi::lfLens,
    focal: f32,
    aperture: f32,
    distance: f32,
) -> LensProfile {
    // SAFETY: `lens` is a valid lens owned by a live database (caller contract);
    // the interpolate calls fill the out-params they return true for.
    unsafe {
        // lensfun's CenterX/Y are a shift from the geometric center; (0, 0) is
        // centered, the overwhelmingly common case.
        let center = [0.5 + (*lens).CenterX, 0.5 + (*lens).CenterY];

        let mut distortion = [0.0_f32; 4];
        let mut dist = std::mem::zeroed::<ffi::lfLensCalibDistortion>();
        if ffi::lf_lens_interpolate_distortion(lens, focal, &mut dist) != 0 {
            distortion = radial_distortion(dist.Model, dist.Terms);
        }

        let mut ca = [0.0_f32; 2];
        let mut tca = std::mem::zeroed::<ffi::lfLensCalibTCA>();
        if ffi::lf_lens_interpolate_tca(lens, focal, &mut tca) != 0 {
            ca = ca_offsets(tca.Model, tca.Terms);
        }

        let mut vignetting = [0.0_f32; 3];
        let mut vig = std::mem::zeroed::<ffi::lfLensCalibVignetting>();
        if ffi::lf_lens_interpolate_vignetting(lens, focal, aperture, distance, &mut vig) != 0 {
            vignetting = vignetting_falloff(vig.Model, vig.Terms);
        }

        LensProfile {
            center,
            distortion,
            ca,
            vignetting,
        }
    }
}

/// Map a lensfun distortion model into the engine's general radial polynomial
/// `[d0, d1, d2, d3]`, where the corrected radius maps to
/// `r·(1 + d0·r + d1·r² + d2·r³ + d3·r⁴)`.
///
/// - **POLY5** (`r_d = r_u(1 + k1 r_u² + k2 r_u⁴)`) → `[0, k1, 0, k2]` (exact).
/// - **POLY3** (`r_d = r_u(1 - k1 + k1 r_u²)`) anchors the corner at `r = 1`;
///   factoring out its `(1 - k1)` scale gives `[0, k1/(1 - k1), 0, 0]` (same
///   line-straightening, off only by an overall magnification a later crop
///   absorbs).
/// - **PTLENS** (`r_d = r_u(a r_u³ + b r_u² + c r_u + D)`, `D = 1-a-b-c`; the
///   PanoTools/Hugin model) factors out its `D` scale → `[c/D, b/D, a/D, 0]`,
///   the odd powers the even-only Brown–Conrady form could not hold.
fn radial_distortion(model: ffi::lfDistortionModel, terms: [f32; 3]) -> [f32; 4] {
    let out = if model == ffi::lfDistortionModel_LF_DIST_MODEL_POLY5 {
        [0.0, terms[0], 0.0, terms[1]]
    } else if model == ffi::lfDistortionModel_LF_DIST_MODEL_POLY3 {
        let k1 = terms[0];
        [0.0, k1 / (1.0 - k1), 0.0, 0.0]
    } else if model == ffi::lfDistortionModel_LF_DIST_MODEL_PTLENS {
        let (a, b, c) = (terms[0], terms[1], terms[2]);
        let d = 1.0 - a - b - c;
        [c / d, b / d, a / d, 0.0]
    } else {
        [0.0, 0.0, 0.0, 0.0]
    };
    // POLY3 (k1→1) and PTLENS (D→0) can divide by ~0 on pathological data; a
    // non-finite coefficient would corrupt every pixel, so fall back to no-op.
    if out.iter().all(|v| v.is_finite()) {
        out
    } else {
        [0.0, 0.0, 0.0, 0.0]
    }
}

/// Map a lensfun TCA model into the engine's per-channel CA offsets `[r, b]`,
/// where red samples at radius `×(1 + r)` and blue at `×(1 + b)`.
///
/// Both supported models keep the per-channel on-axis radial scale in the first
/// two interpolated `Terms` (`kR, kB` for LINEAR; `vR, vB` for POLY3) — the
/// engine's CA is a single constant scale, so a POLY3 lens is approximated by
/// that on-axis term and its higher-order radius dependence is dropped.
fn ca_offsets(model: ffi::lfTCAModel, terms: [f32; 6]) -> [f32; 2] {
    if model == ffi::lfTCAModel_LF_TCA_MODEL_LINEAR || model == ffi::lfTCAModel_LF_TCA_MODEL_POLY3 {
        [terms[0] - 1.0, terms[1] - 1.0]
    } else {
        [0.0, 0.0]
    }
}

/// Map a lensfun vignetting model into the engine's falloff polynomial. The PA
/// model's `(1 + k1 r² + k2 r⁴ + k3 r⁶)` is the measured light *falloff* (its
/// `k`s are negative for darker corners), which is exactly what
/// `LensProfile.vignetting` stores and the engine divides out — so the terms pass
/// straight through.
fn vignetting_falloff(model: ffi::lfVignettingModel, terms: [f32; 3]) -> [f32; 3] {
    if model == ffi::lfVignettingModel_LF_VIGNETTING_MODEL_PA {
        terms
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_linked_version() {
        // The compile-time version macros were bound (not all zero).
        let (a, b, c, d) = version();
        assert!(a + b + c + d > 0, "linked lensfun version {a}.{b}.{c}.{d}");
    }

    #[test]
    fn poly5_maps_to_the_even_radial_terms() {
        // POLY5's k1, k2 are the r³ and r⁵ terms of r·s(r).
        assert_eq!(
            radial_distortion(
                ffi::lfDistortionModel_LF_DIST_MODEL_POLY5,
                [0.01, -0.002, 0.0]
            ),
            [0.0, 0.01, 0.0, -0.002]
        );
    }

    #[test]
    fn poly3_rescales_out_the_corner_anchor() {
        let k1 = 0.02_f32;
        let out = radial_distortion(ffi::lfDistortionModel_LF_DIST_MODEL_POLY3, [k1, 0.0, 0.0]);
        assert!((out[1] - k1 / (1.0 - k1)).abs() < 1e-7, "{out:?}");
        assert_eq!([out[0], out[2], out[3]], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn ptlens_maps_its_abc_into_odd_and_even_terms() {
        // r_d = r(a r³ + b r² + c r + D), D = 1-a-b-c; factor out D.
        let (a, b, c) = (0.001_f32, -0.02_f32, 0.005_f32);
        let d = 1.0 - a - b - c;
        let out = radial_distortion(ffi::lfDistortionModel_LF_DIST_MODEL_PTLENS, [a, b, c]);
        assert!((out[0] - c / d).abs() < 1e-7, "{out:?}");
        assert!((out[1] - b / d).abs() < 1e-7, "{out:?}");
        assert!((out[2] - a / d).abs() < 1e-7, "{out:?}");
        assert_eq!(out[3], 0.0);
    }

    #[test]
    fn degenerate_distortion_falls_back_to_no_op() {
        // PTLENS with D = 1 - a - b - c = 0 would divide by zero; guard to a no-op.
        assert_eq!(
            radial_distortion(ffi::lfDistortionModel_LF_DIST_MODEL_PTLENS, [0.5, 0.5, 0.0]),
            [0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn linear_tca_becomes_channel_offsets() {
        let ca = ca_offsets(
            ffi::lfTCAModel_LF_TCA_MODEL_LINEAR,
            [1.001, 0.999, 0.0, 0.0, 0.0, 0.0],
        );
        assert!(
            (ca[0] - 0.001).abs() < 1e-6 && (ca[1] + 0.001).abs() < 1e-6,
            "{ca:?}"
        );
    }

    #[test]
    fn poly3_tca_uses_the_on_axis_scale() {
        // lensfun's interpolated POLY3 TCA terms lead with the per-channel
        // constant scale vR, vB (the on-axis scale), not the attribute order.
        let ca = ca_offsets(
            ffi::lfTCAModel_LF_TCA_MODEL_POLY3,
            [1.0003744, 0.9998434, 0.0, 0.0, 3.46e-5, 5.37e-5],
        );
        assert!(
            (ca[0] - 0.0003744).abs() < 1e-6 && (ca[1] + 0.0001566).abs() < 1e-6,
            "{ca:?}"
        );
    }

    #[test]
    fn pa_vignetting_passes_through_as_the_falloff() {
        assert_eq!(
            vignetting_falloff(
                ffi::lfVignettingModel_LF_VIGNETTING_MODEL_PA,
                [-0.64, 0.05, 0.11]
            ),
            [-0.64, 0.05, 0.11]
        );
    }
}
