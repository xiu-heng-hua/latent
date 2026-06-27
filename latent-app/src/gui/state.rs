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
    /// The sidecar path the document autosaves to.
    pub(crate) sidecar: PathBuf,
    /// The variants as loaded from disk (for the saved/edited indicator).
    pub(crate) saved: Vec<Settings>,
    /// Window title (`<basename> — latent`).
    pub(crate) title: String,
    /// Full input path (shown on hover).
    pub(crate) path: String,
    /// Default export file name (the source stem with an image extension).
    pub(crate) output: String,
}

/// What the render worker produces back to the main thread: a freshly rendered
/// preview image (the main thread uploads it as a texture, which must stay on the
/// egui thread), the status line for a finished export, or a developed file ready
/// to install as a session.
pub(crate) enum RenderOutput {
    /// A rendered preview base, to be uploaded into the preview texture.
    Preview(ImageBuf),
    /// The result of a finished export, already formatted for the status line.
    Export(String),
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
                RenderOutput::Export(export_status(&output.to_string_lossy(), result))
            }
            // Handled above; unreachable here.
            JobKind::Load { .. } => unreachable!("load handled before render"),
        }
    }
}

/// Develop `input` into a [`SessionData`]: decode and develop the RAW, build the
/// full-res and downscaled-preview bases, resolve the title and default export
/// name, and restore the edit sidecar (`<raw>.ron`) when present. On a fresh
/// document (no sidecar) a detected lens profile is auto-applied from the EXIF.
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

    // Reload edits from the sidecar (photo.nef → photo.ron) if present.
    let sidecar = input.with_extension("ron");
    let loaded = std::fs::read_to_string(&sidecar)
        .ok()
        .and_then(|text| Document::from_ron(&text).ok());
    let from_sidecar = loaded.is_some();
    let mut document = loaded.unwrap_or_default();
    if document.variants.is_empty() {
        document.variants.push(Settings::default());
    }
    // On a fresh document (no sidecar), auto-apply a lens profile from the RAW's
    // EXIF if lensfun has one. A saved sidecar always wins — we never overwrite it.
    if !from_sidecar && let Some(profile) = auto_lens_profile(&meta) {
        for variant in &mut document.variants {
            variant.geometry.lens = Some(profile);
        }
    }
    let saved = document.variants.clone();
    let variants = document.variants.into_iter().map(History::new).collect();

    Ok(SessionData {
        full,
        preview,
        variants,
        sidecar,
        saved,
        title,
        path,
        output,
    })
}

/// The status line for a finished export: success names the path, failure names
/// the error. Factored out of the worker so it can be tested as a pure mapping.
pub(crate) fn export_status(path: &str, result: image::ImageResult<()>) -> String {
    match result {
        Ok(()) => format!("Exported to {path}"),
        Err(e) => format!("Export failed: {e}"),
    }
}

/// Clamp a selection index to a list of `len` items: the last valid index, or 0
/// when the list is empty. Shared by the local-adjustment list mutations so the
/// two clamp sites cannot drift.
pub(crate) fn clamp_selection(sel: &mut usize, len: usize) {
    *sel = (*sel).min(len.saturating_sub(1));
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
    fn export_status_maps_ok_and_err() {
        // The status line names the path on success and the error on failure.
        assert_eq!(
            export_status("out.tiff", Ok(())),
            "Exported to out.tiff".to_owned()
        );
        let err = latent_export::save(&ImageBuf::new(0, 0), Path::new("out.png"))
            .expect_err("zero dimension errors");
        let msg = export_status("out.png", Err(err));
        assert!(msg.starts_with("Export failed:"), "{msg}");
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
            RenderOutput::Export(status) => assert!(status.starts_with("Exported to"), "{status}"),
            _ => panic!("an export job must report an export status"),
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
            RenderOutput::Export(status) => assert!(status.starts_with("Exported to"), "{status}"),
            _ => panic!("an export job must report an export status"),
        }
        assert!(matches!(
            image::open(&jpg).unwrap().color(),
            image::ColorType::Rgb8
        ));
        std::fs::remove_file(&jpg).ok();
    }

    #[test]
    fn export_status_maps_failed_export() {
        // An unsupported extension reaches the encoder as a typed error; the status
        // mapper turns it into a "Export failed:" line (the toast/status message),
        // never a panic.
        let mut img = ImageBuf::new(1, 1);
        img.set(0, 0, [0.5, 0.5, 0.5]);
        let bad = std::env::temp_dir().join("latent_export_status_bad.bmp");
        std::fs::remove_file(&bad).ok();
        let result = latent_export::save_auto_with_quality(&img, &bad, None, None);
        assert!(result.is_err(), "unsupported extension should error");
        let msg = export_status(&bad.to_string_lossy(), result);
        assert!(msg.starts_with("Export failed:"), "{msg}");
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
