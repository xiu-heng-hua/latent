//! The editor application: the `App` state, its off-thread render machinery, and
//! the per-frame `update` that lays out the chrome (menu bar, toolbar, status
//! bar, controls) and the central canvas. `run` develops the RAW, restores the
//! sidecar, and opens the window.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

use eframe::egui;
use latent_edit::{Document, History, Settings};
use latent_image::ImageBuf;
use latent_pipeline::Backend;

use super::canvas;
use super::panels;
use super::state::{RenderJob, RenderOutput, RenderState, auto_lens_profile};
use super::theme;

/// Longest side of the interactive preview, in pixels. Keeps re-render cheap
/// during editing; export uses the full-resolution image.
const PREVIEW_MAX_DIM: u32 = 1600;

/// Which rendering backend is active, surfaced in the status bar. Threaded from
/// the composition root (`select_backend`) since the `Arc<dyn Backend>` itself
/// doesn't carry its kind. A future live backend toggle can reuse this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cpu,
    Gpu,
}

/// Develop `input` and open the editor window, rendering with `backend` (whose
/// `kind` is shown in the status bar).
pub fn run(
    input: &Path,
    backend: Box<dyn Backend>,
    kind: BackendKind,
) -> Result<(), Box<dyn Error>> {
    // Develop once at full res; the preview re-renders over a downscaled copy.
    // The bases are read-only during a render and are shared with the render
    // worker by `Arc`, so a full-res export never deep-copies the image.
    let (full, meta) = crate::develop_to_image(input)?;
    let preview = Arc::new(full.downscaled(PREVIEW_MAX_DIM));
    let full = Arc::new(full);
    // The trait is `Send + Sync`, so the backend can be shared with the worker.
    let backend: Arc<dyn Backend> = Arc::from(backend);
    // Basename for the window title; the full path stays reachable on hover.
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.display().to_string());
    let title = format!("{name} — latent");
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

    let icon = load_icon();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(theme::DEFAULT_WINDOW_SIZE)
        .with_min_inner_size(theme::MIN_WINDOW_SIZE)
        .with_title(&title);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "latent",
        options,
        Box::new(move |cc| {
            // Apply the theme (visuals, spacing, fonts, icons) once at startup.
            // The native scale factor (`pixels_per_point`) is left to eframe's
            // default so HiDPI displays get a correctly-scaled, crisp window. A
            // persisted scale/zoom override could be applied here in the future.
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(App {
                full,
                preview,
                variants,
                active: 0,
                sidecar,
                saved,
                title,
                path,
                output,
                status: String::new(),
                texture: None,
                render: RenderState::default(),
                local_sel: 0,
                brush_radius: 0.08,
                brush_feather: 0.04,
                brush_erase: false,
                curve_channel: 0,
                backend,
                backend_kind: kind,
            }) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| format!("could not start the editor window: {e}"))?;
    Ok(())
}

/// Decode the committed app icon into eframe's `IconData`. The PNG is decoded
/// once at startup with the already-present `image` crate (no new dependency);
/// a decode failure simply opens the window without a custom icon.
fn load_icon() -> Option<egui::IconData> {
    const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");
    let image = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

pub struct App {
    /// Full-resolution working base, rendered over for export. Shared with the
    /// render worker by `Arc` (read-only during a render).
    pub(crate) full: Arc<ImageBuf>,
    /// Downscaled working base, rendered over for the live preview.
    pub(crate) preview: Arc<ImageBuf>,
    /// One independent edit history per variant; never empty.
    pub(crate) variants: Vec<History<Settings>>,
    /// Index of the variant currently being edited and previewed.
    pub(crate) active: usize,
    /// Sidecar path (`<raw>.ron`) the document auto-saves to.
    pub(crate) sidecar: PathBuf,
    /// Last variants written to the sidecar, to avoid redundant writes.
    pub(crate) saved: Vec<Settings>,
    /// Window title (`<filename> — latent`).
    pub(crate) title: String,
    /// Full input path, surfaced on hover (the title shows only the basename).
    pub(crate) path: String,
    /// Export destination path (editable in the UI).
    pub(crate) output: String,
    pub(crate) status: String,
    pub(crate) texture: Option<egui::TextureHandle>,
    /// The off-thread render in flight (if any) plus a coalescing flag.
    pub(crate) render: RenderState,
    /// Index of the local adjustment selected for editing in the panel.
    pub(crate) local_sel: usize,
    /// Brush tool settings for painting dabs onto a brush mask (normalized).
    pub(crate) brush_radius: f32,
    pub(crate) brush_feather: f32,
    pub(crate) brush_erase: bool,
    /// Which curve channel the editor edits (0 = master, 1/2/3 = R/G/B).
    pub(crate) curve_channel: usize,
    /// The rendering backend (CPU, or GPU when selected and available). Shared
    /// with the render worker by `Arc` — the trait is `Send + Sync`.
    pub(crate) backend: Arc<dyn Backend>,
    /// Which backend kind is active, for the status bar.
    pub(crate) backend_kind: BackendKind,
}

impl App {
    /// The history of the variant currently being edited.
    fn active_history(&mut self) -> &mut History<Settings> {
        &mut self.variants[self.active]
    }

    /// Whether every variant is currently equal to what's on disk (no unsaved
    /// edits). Drives the status bar's saved/editing indicator.
    pub(crate) fn is_saved(&self) -> bool {
        self.variants.len() == self.saved.len()
            && self
                .variants
                .iter()
                .zip(&self.saved)
                .all(|(h, s)| h.current() == s)
    }

    /// Write all variants to the sidecar if they changed and no gesture is in
    /// progress (so we save once per completed edit, not mid-drag).
    fn autosave(&mut self) {
        if !self.variants[self.active].is_idle() {
            return;
        }
        let current: Vec<Settings> = self.variants.iter().map(|h| h.current().clone()).collect();
        if current == self.saved {
            return;
        }
        let doc = Document {
            version: Document::VERSION,
            variants: current.clone(),
        };
        match doc.to_ron() {
            Ok(text) => match std::fs::write(&self.sidecar, text) {
                Ok(()) => self.saved = current,
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            Err(e) => self.status = format!("Serialize failed: {e}"),
        }
    }

    /// Request a preview re-render of the active variant. Spawns the render on a
    /// worker thread so the UI keeps repainting; if one is already in flight the
    /// request is coalesced (latest-wins) and spawned when the current one
    /// finishes. The worker calls `ctx.request_repaint()` on completion so the
    /// main thread wakes to upload the result.
    pub(crate) fn render_preview(&mut self, ctx: &egui::Context) {
        if self.render.is_busy() {
            self.render.pending = true;
            return;
        }
        let job = RenderJob {
            base: Arc::clone(&self.preview),
            settings: self.variants[self.active].current().clone(),
            backend: Arc::clone(&self.backend),
            kind: super::state::JobKind::Preview,
        };
        self.spawn(ctx, job);
    }

    /// Render the active variant at full resolution and write it to `self.output`,
    /// off the UI thread. While it runs the window keeps repainting; the result
    /// lands on the status line. Skipped if a render is already in flight (the
    /// Export button is disabled in that state).
    pub(crate) fn export(&mut self, ctx: &egui::Context) {
        if self.render.is_busy() {
            return;
        }
        let job = RenderJob {
            base: Arc::clone(&self.full),
            settings: self.variants[self.active].current().clone(),
            backend: Arc::clone(&self.backend),
            kind: super::state::JobKind::Export {
                output: PathBuf::from(&self.output),
            },
        };
        self.status = "Exporting…".to_owned();
        self.spawn(ctx, job);
    }

    /// Spawn `job` on a worker thread, recording the receiver as the in-flight
    /// render. The worker requests a repaint when done so the main thread wakes
    /// and consumes the result in [`Self::poll_render`].
    fn spawn(&mut self, ctx: &egui::Context, job: RenderJob) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let out = job.run();
            // If the main thread has dropped the receiver (window closing), the
            // send simply fails; nothing to clean up.
            let _ = tx.send(out);
            ctx.request_repaint();
        });
        self.render.in_flight = Some(rx);
    }

    /// Consume a finished render if the worker has reported one: upload a fresh
    /// preview as the texture, or post an export's status line. Then, if a preview
    /// re-render was coalesced while this one ran, spawn it. Called once per frame.
    fn poll_render(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.render.in_flight else {
            return;
        };
        match rx.try_recv() {
            Ok(out) => {
                self.render.in_flight = None;
                match out {
                    RenderOutput::Preview(img) => self.load_texture(ctx, &img),
                    RenderOutput::Export(status) => self.status = status,
                }
                // A request that arrived mid-render coalesced to one; run it now.
                if std::mem::take(&mut self.render.pending) {
                    self.render_preview(ctx);
                }
            }
            // Still running, or the worker vanished; either way leave the slot.
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.render.in_flight = None;
            }
        }
    }

    /// Upload a rendered preview image into the preview texture (creating it on
    /// the first frame). The texture upload must run on the egui thread.
    fn load_texture(&mut self, ctx: &egui::Context, img: &ImageBuf) {
        let color = canvas::to_color_image(img);
        match &mut self.texture {
            Some(tex) => tex.set(color, egui::TextureOptions::default()),
            None => {
                self.texture =
                    Some(ctx.load_texture("preview", color, egui::TextureOptions::default()));
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pick up a finished render (preview texture or export status) first.
        self.poll_render(ctx);

        // The first frame needs an initial render; once one is in flight or
        // queued we wait for it rather than re-triggering every frame.
        let mut dirty = self.texture.is_none() && !self.render.is_busy() && !self.render.pending;

        // Keyboard: Cmd/Ctrl+Z undo, Cmd/Ctrl+Shift+Z or Cmd/Ctrl+Y redo.
        let (mut do_undo, mut do_redo) = (false, false);
        ctx.input(|i| {
            let cmd = i.modifiers.command;
            if cmd && i.key_pressed(egui::Key::Z) {
                if i.modifiers.shift {
                    do_redo = true;
                } else {
                    do_undo = true;
                }
            }
            if cmd && i.key_pressed(egui::Key::Y) {
                do_redo = true;
            }
        });

        // Chrome, in panel order: menu bar, toolbar, status bar, controls — then
        // the central canvas last so it takes the remaining space.
        panels::menubar::show(self, ctx, &mut do_undo, &mut do_redo);
        panels::toolbar::show(self, ctx, &mut do_undo, &mut do_redo, &mut dirty);
        panels::statusbar::show(self, ctx);
        dirty |= panels::controls::show(self, ctx);

        if do_undo && self.active_history().undo() {
            dirty = true;
        }
        if do_redo && self.active_history().redo() {
            dirty = true;
        }

        // Persist edits to the sidecar once a gesture completes.
        self.autosave();

        // Re-render only when something changed (or on the first frame).
        if dirty {
            self.render_preview(ctx);
        }

        // The central canvas (image + brush) is added last.
        canvas::show(self, ctx);
    }
}
