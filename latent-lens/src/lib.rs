//! A thin, safe wrapper over the system lensfun library (FFI), mirroring the
//! LibRaw boundary in `latent-raw`.
//!
//! lensfun is only the *data source* for lens corrections — the correction math
//! itself is the clean-room engine in `latent-pipeline`. This crate looks a lens
//! up in lensfun's database and maps its model into a [`latent_edit::LensProfile`]
//! the engine can apply. The lensfun *library* is LGPL (linked here); its
//! *database* is CC-BY-SA, installed separately and read at runtime — never
//! vendored into this repository.

use latent_edit::{DistortionModel, LensProfile};
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
    ///
    /// Camera resolution uses lensfun's fuzzy `lf_db_find_cameras_ext`
    /// (`lfDatabase::FindCamerasExt`, scored with `lfFuzzyStrCmp`, best match
    /// first) rather than the exact `lf_db_find_cameras` (`FindCameras`, a byte
    /// `_lf_strcmp` on maker and model). The exact compare fails whenever the EXIF
    /// spelling differs from the database's — a near-universal case, since EXIF
    /// maker/model strings carry vendor suffixes and drop the maker prefix from
    /// the model. The fuzzy matcher absorbs that gap, as lensfun's own `lenstool`
    /// does. When the maker-qualified search comes back empty (a maker suffix the
    /// scorer won't bridge), we retry model-only with a NULL maker, again mirroring
    /// `lenstool`, while keeping the model qualified to avoid a whole-database scan.
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
            let mut cameras =
                ffi::lf_db_find_cameras_ext(self.db, maker.as_ptr(), model.as_ptr(), 0);
            if cameras.is_null() || (*cameras).is_null() {
                // A maker suffix the scorer won't bridge can empty the
                // maker-qualified search; retry model-only (NULL maker) before
                // giving up. Free the first list first so it can't leak.
                if !cameras.is_null() {
                    ffi::lf_free(cameras as *mut _);
                }
                cameras = ffi::lf_db_find_cameras_ext(self.db, std::ptr::null(), model.as_ptr(), 0);
                if cameras.is_null() || (*cameras).is_null() {
                    if !cameras.is_null() {
                        ffi::lf_free(cameras as *mut _);
                    }
                    return None;
                }
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
        // lensfun's CenterX/Y are a shift from the geometric center, measured in
        // half-shorter-side units (the engine scales by `min(w, h)/2`); (0, 0) is
        // centered, the overwhelmingly common case.
        let center = [0.5 + (*lens).CenterX, 0.5 + (*lens).CenterY];

        // The crop factor and aspect ratio come from the lens calibration; the
        // real focal length (which can differ from the nominal `focal` on some
        // lenses) is interpolated separately. Together they normalize the radius
        // into lensfun's focal frame and rescale the Hugin-frame coefficients.
        let crop = (*lens).CropFactor;
        let aspect_ratio = (*lens).AspectRatio;
        let mut real_focal = focal;
        let mut rf = std::mem::zeroed::<ffi::lfLensCalibRealFocal>();
        if ffi::lf_lens_interpolate_real_focal(lens, focal, &mut rf) != 0 {
            real_focal = rf.RealFocal;
        }

        let mut model = DistortionModel::None;
        let mut distortion = [0.0_f32; 4];
        let mut dist = std::mem::zeroed::<ffi::lfLensCalibDistortion>();
        if ffi::lf_lens_interpolate_distortion(lens, focal, &mut dist) != 0 {
            let scaling = hugin_scaling(crop, aspect_ratio, real_focal);
            (model, distortion) = radial_distortion(dist.Model, dist.Terms, scaling);
        }

        let mut ca = [[0.0_f32, 0.0, 1.0], [0.0, 0.0, 1.0]];
        let mut tca = std::mem::zeroed::<ffi::lfLensCalibTCA>();
        if ffi::lf_lens_interpolate_tca(lens, focal, &mut tca) != 0 {
            let scaling = hugin_scaling(crop, aspect_ratio, real_focal);
            ca = ca_offsets(tca.Model, tca.Terms, scaling);
        }

        let mut vignetting = [0.0_f32; 3];
        let mut vig = std::mem::zeroed::<ffi::lfLensCalibVignetting>();
        if ffi::lf_lens_interpolate_vignetting(lens, focal, aperture, distance, &mut vig) != 0 {
            vignetting = vignetting_falloff(vig.Model, vig.Terms);
        }

        LensProfile {
            center,
            crop,
            real_focal,
            model,
            distortion,
            ca,
            vignetting,
        }
    }
}

/// The factor that carries a polynomial coefficient from the Hugin calibration
/// frame (where `r = 1` is the half *long* edge) into lensfun's focal frame
/// (where `r = 1` is one real focal length on the sensor). A radius term of order
/// `n` rescales by this factor to the `n`-th power. Derived from lensfun's
/// `rescale_polynomial_coefficients`:
/// `hugin_scaling = RealFocal / (hypot(36, 24)/Crop/hypot(AspectRatio, 1)/2)`.
fn hugin_scaling(crop: f32, aspect_ratio: f32, real_focal: f32) -> f32 {
    let hugin_scale_in_mm = (36.0_f32).hypot(24.0) / crop / aspect_ratio.hypot(1.0) / 2.0;
    real_focal / hugin_scale_in_mm
}

/// Map a lensfun distortion model into the engine's forward radial coefficients
/// `[d0, d1, d2, d3]`, **rescaled into the focal frame** so they share the radius
/// unit the engine normalizes with (lensfun's `NormScale`). The returned
/// [`DistortionModel`] selects how the engine inverts the forward map.
///
/// lensfun isolates the PT magnification (the `(1 - …)` factor that shrinks the
/// image center) into the coefficients in `rescale_polynomial_coefficients`,
/// after which the transform is focal-length-preserving. With `H` the
/// [`hugin_scaling`] factor:
///
/// - **POLY3** (`r_d = r_u(1 - k1 + k1 r_u²)`) → `k1' = k1·H²/(1 - k1)³` in slot 1
///   (the `r²` term), giving the focal-frame `r_d = r_u(1 + k1' r_u²)`.
/// - **POLY5** (`r_d = r_u(1 + k1 r_u² + k2 r_u⁴)`) → `[0, k1·H², 0, k2·H⁴]`.
/// - **PTLENS** (`r_d = r_u(a r_u³ + b r_u² + c r_u + D)`, `D = 1-a-b-c`) →
///   `[c·H/D², b·H²/D³, a·H³/D⁴, 0]`, giving `r_d = r_u(1 + c' r + b' r² + a' r³)`.
fn radial_distortion(
    model: ffi::lfDistortionModel,
    terms: [f32; 3],
    h: f32,
) -> (DistortionModel, [f32; 4]) {
    let (out_model, out) = if model == ffi::lfDistortionModel_LF_DIST_MODEL_POLY5 {
        let (k1, k2) = (terms[0], terms[1]);
        (
            DistortionModel::Poly5,
            [0.0, k1 * h.powi(2), 0.0, k2 * h.powi(4)],
        )
    } else if model == ffi::lfDistortionModel_LF_DIST_MODEL_POLY3 {
        let k1 = terms[0];
        let d = 1.0 - k1;
        (
            DistortionModel::Poly3,
            [0.0, k1 * h.powi(2) / d.powi(3), 0.0, 0.0],
        )
    } else if model == ffi::lfDistortionModel_LF_DIST_MODEL_PTLENS {
        let (a, b, c) = (terms[0], terms[1], terms[2]);
        let d = 1.0 - a - b - c;
        (
            DistortionModel::Ptlens,
            [
                c * h / d.powi(2),
                b * h.powi(2) / d.powi(3),
                a * h.powi(3) / d.powi(4),
                0.0,
            ],
        )
    } else {
        (DistortionModel::None, [0.0, 0.0, 0.0, 0.0])
    };
    // POLY3 (k1→1) and PTLENS (D→0) can divide by ~0 on pathological data; a
    // non-finite coefficient would corrupt every pixel, so fall back to no-op.
    if out.iter().all(|v| v.is_finite()) {
        (out_model, out)
    } else {
        (DistortionModel::None, [0.0, 0.0, 0.0, 0.0])
    }
}

/// Map a lensfun TCA model into the engine's per-channel radial CA scale
/// `[[b_R, c_R, v_R], [b_B, c_B, v_B]]` (red, blue; green is the reference). The
/// channel samples at radius `r·(b r² + c r + v)`; the radius coefficients are
/// **rescaled into the focal frame** by [`hugin_scaling`] (`b` by `H²`, `c` by
/// `H`, `v` unchanged) so they share the engine's radius unit.
///
/// - **POLY3** carries the full radial scale: `Terms` lead with the per-channel
///   constants `vR, vB`, then `bR, bB, cR, cB`, so the higher-order radius
///   dependence is preserved (not collapsed to the on-axis `v`).
/// - **LINEAR** (`r_d = r_u·k`, `kR, kB`) is the degenerate `[0, 0, k]`.
///
/// A non-finite interpolated term falls back to the green-equivalent identity
/// `[0, 0, 1]` (unit scale, no offset) so a corrupt DB entry cannot poison the
/// channel.
fn ca_offsets(model: ffi::lfTCAModel, terms: [f32; 6], h: f32) -> [[f32; 3]; 2] {
    let out = if model == ffi::lfTCAModel_LF_TCA_MODEL_POLY3 {
        // Interpolated POLY3 order: vR, vB, bR, bB, cR, cB.
        [
            [terms[2] * h * h, terms[4] * h, terms[0]],
            [terms[3] * h * h, terms[5] * h, terms[1]],
        ]
    } else if model == ffi::lfTCAModel_LF_TCA_MODEL_LINEAR {
        [[0.0, 0.0, terms[0]], [0.0, 0.0, terms[1]]]
    } else {
        [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]
    };
    // A non-finite term (corrupt DB entry) falls back to the no-op identity.
    if out.iter().flatten().all(|v| v.is_finite()) {
        out
    } else {
        [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]
    }
}

/// Map a lensfun vignetting model into the engine's falloff polynomial. The PA
/// model's `(1 + k1 r² + k2 r⁴ + k3 r⁶)` is the measured light *falloff* (its
/// `k`s are negative for darker corners): `C_d = C_s / (1 + …)`
/// (`geometry-lensfun-lens-models.html`, PA model), the corrected output `C_d`
/// is the source `C_s` *divided* by the falloff. That is exactly what
/// `LensProfile.vignetting` stores and the engine divides out, so the terms pass
/// straight through. A non-finite term falls back to unity falloff `[0, 0, 0]`.
fn vignetting_falloff(model: ffi::lfVignettingModel, terms: [f32; 3]) -> [f32; 3] {
    if model == ffi::lfVignettingModel_LF_VIGNETTING_MODEL_PA && terms.iter().all(|v| v.is_finite())
    {
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
        // POLY5's k1, k2 are the r² and r⁴ coefficients, each rescaled into the
        // focal frame by a power of the Hugin scaling. With H = 1 they map
        // unchanged into slots 1 and 3.
        let (model, out) = radial_distortion(
            ffi::lfDistortionModel_LF_DIST_MODEL_POLY5,
            [0.01, -0.002, 0.0],
            1.0,
        );
        assert_eq!(model, DistortionModel::Poly5);
        assert_eq!(out, [0.0, 0.01, 0.0, -0.002]);
    }

    #[test]
    fn poly5_rescales_each_order_by_its_power_of_h() {
        // k1 (the r² term) scales by H², k2 (r⁴) by H⁴.
        let h = 1.3_f32;
        let (_, out) = radial_distortion(
            ffi::lfDistortionModel_LF_DIST_MODEL_POLY5,
            [0.01, -0.002, 0.0],
            h,
        );
        assert!((out[1] - 0.01 * h.powi(2)).abs() < 1e-7, "{out:?}");
        assert!((out[3] + 0.002 * h.powi(4)).abs() < 1e-9, "{out:?}");
    }

    #[test]
    fn poly3_rescales_out_the_corner_anchor() {
        // POLY3 folds its (1 - k1) magnification and the focal scaling into k1.
        let k1 = 0.02_f32;
        let h = 1.4_f32;
        let (model, out) = radial_distortion(
            ffi::lfDistortionModel_LF_DIST_MODEL_POLY3,
            [k1, 0.0, 0.0],
            h,
        );
        assert_eq!(model, DistortionModel::Poly3);
        let expected = k1 * h.powi(2) / (1.0 - k1).powi(3);
        assert!((out[1] - expected).abs() < 1e-6, "{out:?}");
        assert_eq!([out[0], out[2], out[3]], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn ptlens_maps_its_abc_into_odd_and_even_terms() {
        // r_d = r(a r³ + b r² + c r + D), D = 1-a-b-c; lensfun folds D and the
        // focal scaling into a, b, c (a/D⁴·H³, b/D³·H², c/D²·H).
        let (a, b, c) = (0.001_f32, -0.02_f32, 0.005_f32);
        let h = 1.2_f32;
        let d = 1.0 - a - b - c;
        let (model, out) =
            radial_distortion(ffi::lfDistortionModel_LF_DIST_MODEL_PTLENS, [a, b, c], h);
        assert_eq!(model, DistortionModel::Ptlens);
        assert!((out[0] - c * h / d.powi(2)).abs() < 1e-7, "{out:?}");
        assert!((out[1] - b * h.powi(2) / d.powi(3)).abs() < 1e-7, "{out:?}");
        assert!((out[2] - a * h.powi(3) / d.powi(4)).abs() < 1e-7, "{out:?}");
        assert_eq!(out[3], 0.0);
    }

    #[test]
    fn degenerate_distortion_falls_back_to_no_op() {
        // PTLENS with D = 1 - a - b - c = 0 would divide by zero; guard to a no-op.
        assert_eq!(
            radial_distortion(
                ffi::lfDistortionModel_LF_DIST_MODEL_PTLENS,
                [0.5, 0.5, 0.0],
                1.0
            ),
            (DistortionModel::None, [0.0, 0.0, 0.0, 0.0])
        );
    }

    #[test]
    fn linear_tca_becomes_channel_offsets() {
        // LINEAR is the degenerate constant scale [0, 0, k] per channel.
        let ca = ca_offsets(
            ffi::lfTCAModel_LF_TCA_MODEL_LINEAR,
            [1.001, 0.999, 0.0, 0.0, 0.0, 0.0],
            1.0,
        );
        assert_eq!(ca, [[0.0, 0.0, 1.001], [0.0, 0.0, 0.999]]);
    }

    #[test]
    fn poly3_tca_keeps_radial_terms() {
        // The full POLY3 TCA retains the radius-dependent b, c terms (rescaled by
        // H², H), not just the on-axis v — the radial dependence the old constant
        // path dropped. Interpolated order is vR, vB, bR, bB, cR, cB.
        let h = 1.5_f32;
        let ca = ca_offsets(
            ffi::lfTCAModel_LF_TCA_MODEL_POLY3,
            [1.0003744, 0.9998434, 1.0e-4, 2.0e-4, 3.46e-5, 5.37e-5],
            h,
        );
        // Red: [bR·H², cR·H, vR].
        assert!((ca[0][0] - 1.0e-4 * h * h).abs() < 1e-9, "{ca:?}");
        assert!((ca[0][1] - 3.46e-5 * h).abs() < 1e-9, "{ca:?}");
        assert!((ca[0][2] - 1.0003744).abs() < 1e-6, "{ca:?}");
        // Blue: [bB·H², cB·H, vB].
        assert!((ca[1][0] - 2.0e-4 * h * h).abs() < 1e-9, "{ca:?}");
        assert!((ca[1][1] - 5.37e-5 * h).abs() < 1e-9, "{ca:?}");
        assert!((ca[1][2] - 0.9998434).abs() < 1e-6, "{ca:?}");
    }

    #[test]
    fn ca_offsets_guards_non_finite() {
        // A non-finite interpolated term (corrupt DB entry) falls back to the
        // green-equivalent identity, not garbage that would poison every pixel.
        let ca = ca_offsets(
            ffi::lfTCAModel_LF_TCA_MODEL_POLY3,
            [f32::NAN, 1.0, 0.0, 0.0, 0.0, 0.0],
            1.0,
        );
        assert_eq!(ca, [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]);
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

    #[test]
    fn vignetting_falloff_guards_non_finite() {
        // A non-finite PA term falls back to unity falloff (a true no-op).
        assert_eq!(
            vignetting_falloff(
                ffi::lfVignettingModel_LF_VIGNETTING_MODEL_PA,
                [f32::NAN, 0.05, 0.11]
            ),
            [0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn hugin_scaling_matches_lensfun() {
        // hugin_scaling = RealFocal / (hypot(36,24)/Crop/hypot(Aspect,1)/2).
        let (crop, aspect, real_focal) = (1.6_f32, 1.5_f32, 24.0_f32);
        let mm = (36.0_f32).hypot(24.0) / crop / aspect.hypot(1.0) / 2.0;
        let expected = real_focal / mm;
        assert!((hugin_scaling(crop, aspect, real_focal) - expected).abs() < 1e-4);
    }

    #[test]
    fn find_profile_returns_none_when_no_match() {
        // Exercises the lookup → free plumbing without depending on a specific
        // lens being installed. When the data package is absent, `load` fails and
        // the crate degrades gracefully (the whole feature is optional); when it
        // is present, a nonsense camera/lens drives the early-return paths in
        // `find_profile` (empty camera list, then `lf_free`) to `None`. Either
        // way the result is the same: no profile, no leak, no crash.
        match Database::load() {
            Err(_) => {
                // No database installed — the documented graceful path.
            }
            Ok(db) => {
                let profile = db.find_profile(
                    "no-such-maker",
                    "no-such-model",
                    "no-such-lens",
                    50.0,
                    8.0,
                    1000.0,
                );
                assert!(profile.is_none(), "an unknown camera should not match");
                // A NUL byte in a field can't form a C string; that path is also
                // `None` (and frees nothing, having allocated nothing).
                assert!(
                    db.find_profile("ma\0ker", "model", "lens", 50.0, 8.0, 1000.0)
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn lens_to_profile_aggregates_terms() {
        // The per-model mappers are unit-tested individually; this pins that the
        // identity/no-calibration case aggregates into a neutral profile (centered
        // optical axis, no distortion model, unit CA scale, unity vignetting) —
        // the field routing `lens_to_profile` performs, exercised without a live
        // lens by checking the assembled defaults each mapper falls back to.
        let center = [0.5, 0.5];
        let (model, distortion) = radial_distortion(
            ffi::lfDistortionModel_LF_DIST_MODEL_NONE,
            [0.0, 0.0, 0.0],
            1.0,
        );
        let ca = ca_offsets(ffi::lfTCAModel_LF_TCA_MODEL_NONE, [0.0; 6], 1.0);
        let vignetting =
            vignetting_falloff(ffi::lfVignettingModel_LF_VIGNETTING_MODEL_NONE, [0.0; 3]);
        let profile = LensProfile {
            center,
            crop: 1.0,
            real_focal: 50.0,
            model,
            distortion,
            ca,
            vignetting,
        };
        assert_eq!(profile.center, [0.5, 0.5]);
        assert_eq!(profile.model, DistortionModel::None);
        assert_eq!(profile.distortion, [0.0; 4]);
        assert_eq!(profile.ca, [[0.0, 0.0, 1.0], [0.0, 0.0, 1.0]]);
        assert_eq!(profile.vignetting, [0.0; 3]);
    }
}
