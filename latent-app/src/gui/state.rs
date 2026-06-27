//! Off-thread render machinery and the small pure helpers around it: the
//! in-flight render gate, the self-contained render job the worker owns, and the
//! status/clamp/lens-profile functions that the UI and worker share.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use latent_edit::{LensProfile, Settings};
use latent_image::ImageBuf;
use latent_pipeline::{Backend, render};

/// What the render worker produces back to the main thread: a freshly rendered
/// preview image (the main thread uploads it as a texture, which must stay on the
/// egui thread), or the status line for a finished export.
pub(crate) enum RenderOutput {
    /// A rendered preview base, to be uploaded into the preview texture.
    Preview(ImageBuf),
    /// The result of a finished export, already formatted for the status line.
    Export(String),
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

/// Whether a [`RenderJob`] feeds the live preview or writes an export file.
pub(crate) enum JobKind {
    /// Render for the on-screen preview; the rendered image is handed back.
    Preview,
    /// Render at full resolution and write to this path; a status line is handed
    /// back. The bit depth follows the file format (16-bit for tif/tiff/png).
    Export { output: PathBuf },
}

impl RenderJob {
    /// Run the render (and, for an export, the file write) and produce the result
    /// the main thread consumes. Pure with respect to the UI — no egui, no
    /// shared state — so it is unit-testable without a window.
    pub(crate) fn run(self) -> RenderOutput {
        let rendered = render(&self.base, &self.settings, self.backend.as_ref());
        match self.kind {
            JobKind::Preview => RenderOutput::Preview(rendered),
            JobKind::Export { output } => {
                let result = latent_export::save_auto(&rendered, &output, None);
                RenderOutput::Export(export_status(&output.to_string_lossy(), result))
            }
        }
    }
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
        // reports a success status. `CpuBackend` with default settings renders
        // the base unchanged, so a tiny image exercises the path cheaply.
        let mut base = ImageBuf::new(2, 1);
        base.set(0, 0, [0.0, 0.0, 0.0]);
        base.set(1, 0, [0.5, 0.5, 0.5]);
        let out = std::env::temp_dir().join("latent_render_job_export_test.tiff");
        std::fs::remove_file(&out).ok();

        let job = RenderJob {
            base: Arc::new(base),
            settings: Settings::default(),
            backend: Arc::new(latent_cpu::CpuBackend),
            kind: JobKind::Export {
                output: out.clone(),
            },
        };
        match job.run() {
            RenderOutput::Export(status) => {
                assert!(status.starts_with("Exported to"), "{status}");
            }
            RenderOutput::Preview(_) => panic!("an export job must report an export status"),
        }
        // A .tiff export is 16-bit by the format default.
        assert!(matches!(
            image::open(&out).unwrap().color(),
            image::ColorType::Rgb16
        ));
        std::fs::remove_file(&out).ok();
    }
}
