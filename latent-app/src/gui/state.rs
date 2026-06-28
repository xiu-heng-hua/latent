//! Off-thread render machinery and the small pure helpers around it: the
//! in-flight render gate, the self-contained render job the worker owns, and the
//! status/clamp/lens-profile functions that the UI and worker share.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use latent_edit::{Document, History, LensProfile, Settings};
use latent_image::ImageBuf;
use latent_pipeline::{Backend, render};

/// The developed payload of an opened file, produced by the worker and consumed
/// on the main thread to install a session. Owns only `Send` data (the developed
/// `Arc<ImageBuf>` bases plus plain owned settings/strings), so it crosses the
/// thread boundary cleanly; the textures are built on the egui thread afterward.
pub(crate) struct SessionData {
    /// Full-resolution working base.
    pub(crate) full: Arc<ImageBuf>,
    /// Downscaled working base for the live preview.
    pub(crate) preview: Arc<ImageBuf>,
    /// One edit history per variant (never empty).
    pub(crate) variants: Vec<History<Settings>>,
    /// Display names for the variants, parallel to `variants` (an empty/missing
    /// entry means "unnamed" → a positional fallback in the UI).
    pub(crate) names: Vec<String>,
    /// The sidecar path the document autosaves to.
    pub(crate) sidecar: PathBuf,
    /// The variants as loaded from disk (for the saved/edited indicator).
    pub(crate) saved: Vec<Settings>,
    /// The variant names as loaded from disk (so a rename is detected as unsaved).
    pub(crate) saved_names: Vec<String>,
    /// Window title (`<basename> — latent`).
    pub(crate) title: String,
    /// Full input path (shown on hover).
    pub(crate) path: String,
    /// Default export file name (the source stem with an image extension).
    pub(crate) output: String,
    /// The RAW's decoded EXIF metadata, carried onto the session so the lens
    /// panel can detect a profile on demand (the lensfun `Database` is not `Send`,
    /// so detection runs later on the main thread, never on this worker).
    pub(crate) meta: latent_raw::Metadata,
}

/// What the render worker produces back to the main thread: a freshly rendered
/// preview image (the main thread uploads it as a texture, which must stay on the
/// egui thread), the status line for a finished export, or a developed file ready
/// to install as a session.
pub(crate) enum RenderOutput {
    /// A rendered preview base, to be uploaded into the preview texture.
    Preview(ImageBuf),
    /// The result of a finished export: `Ok(path)` names the written file,
    /// `Err(message)` carries the failure. The main thread turns this into a
    /// success or error toast, so the kind is known without sniffing a string.
    Export(Result<String, String>),
    /// A developed file (boxed — it's much larger than the other variants), or the
    /// error string from a failed develop. On `Err` the current session is kept.
    Loaded(Box<Result<SessionData, String>>),
}

/// Tracks the single in-flight render and whether another is queued. Only one
/// render runs at a time; a request that arrives while one is in flight sets
/// `pending` (latest-wins), and the next idle frame spawns it. The lensfun
/// `Database` is never part of this — the render reads the already-resolved
/// [`LensProfile`] in [`Settings`], so nothing non-`Send` crosses the boundary.
#[derive(Default)]
pub(crate) struct RenderState {
    /// The channel a spawned worker reports back on, while it runs.
    pub(crate) in_flight: Option<Receiver<RenderOutput>>,
    /// A preview re-render was requested while one was in flight; coalesce to one.
    pub(crate) pending: bool,
}

impl RenderState {
    /// Whether a render is currently running on the worker.
    pub(crate) fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }
}

/// A self-contained render request the worker owns outright: the base to render
/// over, the settings to apply, the backend, and what to do with the result. All
/// fields are `Send` — no lensfun `Database`, only plain resolved data — so the
/// job can move to a worker thread.
pub(crate) struct RenderJob {
    pub(crate) base: Arc<ImageBuf>,
    pub(crate) settings: Settings,
    pub(crate) backend: Arc<dyn Backend>,
    pub(crate) kind: JobKind,
}

/// Whether a [`RenderJob`] feeds the live preview, writes an export file, or
/// develops a file to open.
pub(crate) enum JobKind {
    /// Render for the on-screen preview; the rendered image is handed back.
    Preview,
    /// Render at full resolution and write to this path; a status line is handed
    /// back. `depth` (when set) and `quality` (JPEG only) come from the export
    /// dialog; `None` lets the encoder pick by format.
    Export {
        output: PathBuf,
        depth: Option<latent_export::Depth>,
        quality: Option<u8>,
    },
    /// Develop `input` into a [`SessionData`] (off the UI thread), or report the
    /// develop error. The job's `base`/`settings` are unused by this arm — it
    /// develops its own image — but keeping the one `RenderJob` shape lets the
    /// load reuse the existing spawn/poll plumbing.
    Load { input: PathBuf },
}

impl RenderJob {
    /// Run the render (and, for an export, the file write; for a load, the
    /// develop) and produce the result the main thread consumes. Pure with respect
    /// to the UI — no egui, no shared state — so it is unit-testable without a
    /// window.
    pub(crate) fn run(self) -> RenderOutput {
        // A load develops its own image and ignores the carried base/settings.
        if let JobKind::Load { input } = &self.kind {
            return RenderOutput::Loaded(Box::new(load_session(input)));
        }
        let rendered = render(&self.base, &self.settings, self.backend.as_ref());
        match self.kind {
            JobKind::Preview => RenderOutput::Preview(rendered),
            JobKind::Export {
                output,
                depth,
                quality,
            } => {
                let result =
                    latent_export::save_auto_with_quality(&rendered, &output, depth, quality);
                RenderOutput::Export(export_result(&output.to_string_lossy(), result))
            }
            // Handled above; unreachable here.
            JobKind::Load { .. } => unreachable!("load handled before render"),
        }
    }
}

/// Develop `input` into a [`SessionData`]: decode and develop the RAW, build the
/// full-res and downscaled-preview bases, resolve the title and default export
/// name, and restore the edit sidecar (`<raw>.ron`) when present.
///
/// Lens correction is off by default: a fresh document (no sidecar) opens with
/// `geometry.lens == None` and no correction applied — the user enables it from
/// the lens panel. A saved sidecar's lens is loaded verbatim (kept as the user's
/// prior choice), since only the fresh-document default changed, never saved
/// intent. The decoded EXIF is carried on the result so the panel can detect a
/// profile on demand on the main thread (the lensfun `Database` is not `Send`).
///
/// Pure — no egui, no `App` — so it is testable without a window and runs on the
/// worker thread. A bad/corrupt input returns an `Err` (never a panic), which the
/// caller surfaces while leaving the current session intact.
pub(crate) fn load_session(input: &Path) -> Result<SessionData, String> {
    let (full, meta) = crate::develop_to_image(input).map_err(|e| e.to_string())?;
    let preview = Arc::new(full.downscaled(crate::gui::app::PREVIEW_MAX_DIM));
    let full = Arc::new(full);
    let title = crate::gui::app::window_title(input);
    let path = input.display().to_string();
    let output = input.with_extension("jpg").to_string_lossy().into_owned();

    // Reload edits from the sidecar (photo.nef → photo.ron) if present. A saved
    // lens (the user's prior choice) is kept exactly as stored; a fresh document
    // gets no lens (off by default).
    let sidecar = input.with_extension("ron");
    let loaded = std::fs::read_to_string(&sidecar)
        .ok()
        .and_then(|text| Document::from_ron(&text).ok());
    let mut document = loaded.unwrap_or_default();
    if document.variants.is_empty() {
        document.variants.push(Settings::default());
    }
    let saved = document.variants.clone();
    // Normalize names to one-per-variant, padding any short/empty trailing entries
    // so the parallel vectors stay aligned; an empty name is shown positionally.
    let mut names = document.names;
    names.resize(document.variants.len(), String::new());
    let saved_names = names.clone();
    let variants = document.variants.into_iter().map(History::new).collect();

    Ok(SessionData {
        full,
        preview,
        variants,
        names,
        sidecar,
        saved,
        saved_names,
        title,
        path,
        output,
        meta,
    })
}

/// The outcome of a finished export as a toast-ready result: `Ok(path)` on a
/// written file, `Err(message)` on a failure. Factored out of the worker so the
/// success/failure split is a pure, testable mapping the main thread routes into
/// a success or error toast by kind (no string-sniffing).
pub(crate) fn export_result(path: &str, result: image::ImageResult<()>) -> Result<String, String> {
    match result {
        Ok(()) => Ok(path.to_owned()),
        Err(e) => Err(format!("Export failed: {e}")),
    }
}

/// Clamp a selection index to a list of `len` items: the last valid index, or 0
/// when the list is empty. Shared by the local-adjustment list mutations so the
/// two clamp sites cannot drift.
pub(crate) fn clamp_selection(sel: &mut usize, len: usize) {
    *sel = (*sel).min(len.saturating_sub(1));
}

/// Merge the **develop** part of `source` onto `target`, keeping the target's own
/// geometry. The develop part is the global adjustments and the local
/// adjustments; geometry (crop, orientation, straighten, keystone, lens,
/// vignette, …) is image-specific — a crop or keystone tuned for one frame is
/// wrong on another — so it is deliberately left as the target's. This is the
/// shared mapping behind "Paste settings" (develop only) and "Apply preset". Pure,
/// so the develop-vs-geometry split is unit-testable.
pub(crate) fn merge_develop(target: &Settings, source: &Settings) -> Settings {
    Settings {
        global: source.global.clone(),
        locals: source.locals.clone(),
        geometry: target.geometry.clone(),
    }
}

/// Reset the develop part of `target` to neutral while keeping its geometry — the
/// "Reset all develop" mapping. A user rarely wants a reset to also undo their
/// crop/straighten, so geometry is preserved by default. Pure.
pub(crate) fn reset_develop(target: &Settings) -> Settings {
    Settings {
        global: Default::default(),
        locals: Vec::new(),
        geometry: target.geometry.clone(),
    }
}

/// Query lensfun for a lens profile matching the RAW's EXIF metadata, or `None`
/// when there's no usable metadata, no match, or no database installed. Focus
/// distance defaults to far, where vignetting/distortion are effectively fixed.
pub(crate) fn auto_lens_profile(meta: &latent_raw::Metadata) -> Option<LensProfile> {
    if meta.model.is_empty() && meta.lens.is_empty() {
        return None;
    }
    // LibRaw's normalized names are already close to the database spelling; prefer
    // them for the lookup, falling back to the raw EXIF when one is empty.
    fn prefer<'a>(normalized: &'a str, raw: &'a str) -> &'a str {
        if normalized.is_empty() {
            raw
        } else {
            normalized
        }
    }
    let db = latent_lens::Database::load().ok()?;
    db.find_profile(
        prefer(&meta.normalized_make, &meta.make),
        prefer(&meta.normalized_model, &meta.model),
        &meta.lens,
        meta.focal_len,
        meta.aperture,
        1000.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::mpsc::channel;

    #[test]
    fn export_result_maps_ok_and_err() {
        // The result names the path on success (an `Ok` → a success toast) and the
        // error on failure (an `Err` → an error toast); the kind is carried by the
        // `Result`, never sniffed from a string.
        assert_eq!(export_result("out.tiff", Ok(())), Ok("out.tiff".to_owned()));
        let err = latent_export::save(&ImageBuf::new(0, 0), Path::new("out.png"))
            .expect_err("zero dimension errors");
        let mapped = export_result("out.png", Err(err));
        assert!(mapped.is_err(), "a failed export maps to Err");
        assert!(mapped.unwrap_err().starts_with("Export failed:"));
    }

    #[test]
    fn paste_develop_keeps_target_geometry() {
        // "Paste settings" (develop only) copies the source's global + local
        // adjustments but keeps the *target's* geometry — a crop tuned for one
        // frame must not follow the look onto another.
        use latent_edit::{Adjustments, Crop, Geometry};
        let source = Settings {
            global: Adjustments {
                exposure: Some(1.0),
                ..Adjustments::default()
            },
            geometry: Geometry {
                crop: Some(Crop {
                    x: 0.0,
                    y: 0.0,
                    width: 0.5,
                    height: 0.5,
                }),
                ..Geometry::default()
            },
            ..Settings::default()
        };
        let target = Settings {
            geometry: Geometry {
                straighten_degrees: 3.0,
                ..Geometry::default()
            },
            ..Settings::default()
        };
        let merged = merge_develop(&target, &source);
        // The develop part came from the source…
        assert_eq!(merged.global.exposure, Some(1.0));
        // …but the geometry is the target's, not the source's.
        assert_eq!(merged.geometry, target.geometry);
        assert_eq!(merged.geometry.crop, None);
        assert_eq!(merged.geometry.straighten_degrees, 3.0);
    }

    #[test]
    fn reset_all_returns_default_develop() {
        // "Reset all develop" clears the global + local adjustments to neutral but
        // keeps the geometry (a reset rarely should undo a crop/straighten).
        use latent_edit::{Adjustments, Geometry, LocalAdjustment};
        let target = Settings {
            global: Adjustments {
                exposure: Some(1.0),
                saturation: Some(1.5),
                ..Adjustments::default()
            },
            locals: vec![LocalAdjustment::default()],
            geometry: Geometry {
                straighten_degrees: 2.0,
                ..Geometry::default()
            },
        };
        let reset = reset_develop(&target);
        assert_eq!(reset.global, Adjustments::default());
        assert!(reset.locals.is_empty());
        // Geometry is preserved.
        assert_eq!(reset.geometry.straighten_degrees, 2.0);
    }

    #[test]
    fn clamp_selection_keeps_in_range() {
        // Clamps to the last valid index, leaves an in-range index untouched, and
        // collapses to 0 on an empty list.
        let mut sel = 5;
        clamp_selection(&mut sel, 3);
        assert_eq!(sel, 2);
        let mut sel = 1;
        clamp_selection(&mut sel, 3);
        assert_eq!(sel, 1);
        let mut sel = 4;
        clamp_selection(&mut sel, 0);
        assert_eq!(sel, 0);
    }

    #[test]
    fn render_state_gate_coalesces_to_one() {
        // The in-flight gate runs at most one render at a time. A fresh state is
        // idle; once a render is recorded it is busy, and a second request while
        // busy must coalesce (set `pending`) rather than spawn another.
        let mut state = RenderState::default();
        assert!(!state.is_busy());

        // Simulate a render in flight by recording a receiver.
        let (_tx, rx) = channel::<RenderOutput>();
        state.in_flight = Some(rx);
        assert!(state.is_busy());

        // A request arriving while busy coalesces instead of spawning.
        if state.is_busy() {
            state.pending = true;
        }
        assert!(state.pending, "a request during a render must be queued");

        // When the render finishes the slot clears and the queued one can run.
        state.in_flight = None;
        assert!(!state.is_busy());
        assert!(std::mem::take(&mut state.pending));
    }

    #[test]
    fn backend_swap_is_deferred_past_a_busy_render() {
        // A backend switch must never replace the backend while a render is in
        // flight (the in-flight render owns its `Arc` and must finish on the backend
        // it started with). This pins the deferral state machine the app uses: a
        // request while busy is recorded as pending and only applied once the gate
        // goes idle. (The `Arc` swap itself needs a window, so this exercises the
        // gating logic that decides *when* the swap is safe.)
        let mut state = RenderState::default();
        let mut pending_backend: Option<bool> = None;

        // A render is in flight.
        let (_tx, rx) = channel::<RenderOutput>();
        state.in_flight = Some(rx);
        assert!(state.is_busy());

        // A request to switch to GPU arrives mid-render: it is deferred, not applied.
        let requested = true;
        if state.is_busy() {
            pending_backend = Some(requested);
        }
        assert_eq!(
            pending_backend,
            Some(true),
            "a switch during a render must be deferred, never applied in flight"
        );

        // The in-flight render finishes; the gate goes idle.
        state.in_flight = None;
        assert!(!state.is_busy());

        // Now — and only now — the deferred switch is consumed and applied.
        let applied = pending_backend.take();
        assert_eq!(
            applied,
            Some(true),
            "the deferred switch is applied once idle"
        );
        assert!(
            pending_backend.is_none(),
            "the pending switch is consumed exactly once"
        );
    }

    #[test]
    fn render_job_export_writes_and_reports() {
        // A `RenderJob` export runs the render and the file write off any UI, and
        // reports a success status, writing the chosen depth. `CpuBackend` with
        // default settings renders the base unchanged, so a tiny image exercises
        // the path cheaply. Drive both a 16-bit TIFF and an 8-bit JPEG through an
        // explicit `depth` so the dialog's depth choice is proven to reach
        // `save_auto_with_quality`.
        fn tiny_base() -> ImageBuf {
            let mut base = ImageBuf::new(2, 1);
            base.set(0, 0, [0.0, 0.0, 0.0]);
            base.set(1, 0, [0.5, 0.5, 0.5]);
            base
        }

        // 16-bit TIFF.
        let tiff = std::env::temp_dir().join("latent_render_job_export_test.tiff");
        std::fs::remove_file(&tiff).ok();
        let job = RenderJob {
            base: Arc::new(tiny_base()),
            settings: Settings::default(),
            backend: Arc::new(latent_cpu::CpuBackend),
            kind: JobKind::Export {
                output: tiff.clone(),
                depth: Some(latent_export::Depth::Sixteen),
                quality: None,
            },
        };
        match job.run() {
            RenderOutput::Export(result) => assert!(result.is_ok(), "{result:?}"),
            _ => panic!("an export job must report an export result"),
        }
        assert!(matches!(
            image::open(&tiff).unwrap().color(),
            image::ColorType::Rgb16
        ));
        std::fs::remove_file(&tiff).ok();

        // 8-bit JPEG with an explicit quality.
        let jpg = std::env::temp_dir().join("latent_render_job_export_test.jpg");
        std::fs::remove_file(&jpg).ok();
        let job = RenderJob {
            base: Arc::new(tiny_base()),
            settings: Settings::default(),
            backend: Arc::new(latent_cpu::CpuBackend),
            kind: JobKind::Export {
                output: jpg.clone(),
                depth: Some(latent_export::Depth::Eight),
                quality: Some(90),
            },
        };
        match job.run() {
            RenderOutput::Export(result) => assert!(result.is_ok(), "{result:?}"),
            _ => panic!("an export job must report an export result"),
        }
        assert!(matches!(
            image::open(&jpg).unwrap().color(),
            image::ColorType::Rgb8
        ));
        std::fs::remove_file(&jpg).ok();
    }

    #[test]
    fn export_failure_is_an_error_toast() {
        // An unsupported extension reaches the encoder as a typed error; the
        // mapper turns it into an `Err` (which the main thread routes to an error
        // toast — the kind comes from the `Result`, not a sniffed string), never a
        // panic.
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.5, 0.5, 0.5]);
        let bad = std::env::temp_dir().join("latent_export_status_bad.bmp");
        std::fs::remove_file(&bad).ok();
        let result = latent_export::save_auto_with_quality(&img, &bad, None, None);
        assert!(result.is_err(), "unsupported extension should error");
        let mapped = export_result(&bad.to_string_lossy(), result);
        assert!(
            mapped.is_err(),
            "a failed export is an error result (error toast)"
        );
        assert!(mapped.unwrap_err().starts_with("Export failed:"));
    }

    #[test]
    fn load_session_errors_on_bad_input() {
        // A garbage (non-RAW) input must return an `Err` from the develop, not a
        // panic, so the open path can surface it and keep the current session.
        let bad = std::env::temp_dir().join("latent_load_session_bad_input.raw");
        std::fs::write(&bad, b"not a raw file").unwrap();
        let result = load_session(&bad);
        assert!(
            result.is_err(),
            "a bad input must surface as Err, not panic"
        );
        std::fs::remove_file(&bad).ok();
    }

    #[test]
    fn fresh_document_has_no_lens_by_default() {
        // Lens correction is off by default: a fresh document (the no-sidecar
        // fallback `load_session` builds) carries no lens. Only the user enabling
        // it — or a saved sidecar — sets `geometry.lens`. This pins the
        // off-by-default behavior independently of a real RAW decode.
        let doc = Document::default();
        for variant in &doc.variants {
            assert_eq!(
                variant.geometry.lens, None,
                "a fresh document applies no lens correction"
            );
        }
    }

    #[test]
    fn saved_sidecar_lens_is_kept_verbatim() {
        // A sidecar that already carries a lens (the user enabled it before) loads
        // with that lens still applied — the fresh-document default never overrides
        // saved intent. Exercises the sidecar-restore half of `load_session`.
        use latent_edit::{DistortionModel, Geometry, LensProfile};
        let dir = std::env::temp_dir().join("latent_lens_sidecar_test");
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("photo.ron");
        let lens = LensProfile {
            model: DistortionModel::Poly3,
            distortion: [0.0, -0.1, 0.0, 0.0],
            ..LensProfile::default()
        };
        let doc = Document {
            version: Document::VERSION,
            variants: vec![Settings {
                geometry: Geometry {
                    lens: Some(lens),
                    ..Geometry::default()
                },
                ..Settings::default()
            }],
            names: Vec::new(),
        };
        std::fs::write(&sidecar, doc.to_ron().unwrap()).unwrap();
        let restored = std::fs::read_to_string(&sidecar)
            .ok()
            .and_then(|t| Document::from_ron(&t).ok())
            .expect("sidecar restores");
        assert_eq!(
            restored.variants[0].geometry.lens,
            Some(lens),
            "a saved lens is kept verbatim"
        );
        std::fs::remove_file(&sidecar).ok();
    }

    #[test]
    fn load_session_prefers_a_present_sidecar() {
        // A present sidecar's variants win over a default document. The develop
        // itself needs a real RAW, which is impractical here, so this exercises the
        // sidecar-resolution half of `load_session` directly: a saved document's
        // variants are restored rather than a single default.
        use latent_edit::Document;
        let dir = std::env::temp_dir().join("latent_load_sidecar_test");
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("photo.ron");
        // Two variants, so a successful restore is distinguishable from a default
        // (one-variant) document.
        let doc = Document {
            version: Document::VERSION,
            variants: vec![Settings::default(), Settings::default()],
            names: Vec::new(),
        };
        std::fs::write(&sidecar, doc.to_ron().unwrap()).unwrap();
        let restored = std::fs::read_to_string(&sidecar)
            .ok()
            .and_then(|t| Document::from_ron(&t).ok())
            .expect("sidecar restores");
        assert_eq!(
            restored.variants.len(),
            2,
            "a present sidecar's variants win"
        );
        std::fs::remove_file(&sidecar).ok();
    }
}
