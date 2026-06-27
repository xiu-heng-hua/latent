//! The editor application: the `App` state, its off-thread render machinery, and
//! the per-frame `update` that lays out the chrome (menu bar, toolbar, status
//! bar, controls) and the central canvas.
//!
//! `App` separates *cross-image* state (the backend, the render worker, the
//! persisted config, the status line) from the *per-image* [`Session`] — a single
//! opened RAW with its developed bases, variants, view, and tool state. The
//! session is an `Option`: `None` is the welcome state, so the window can start
//! with no file and switch files in place without relaunching. Opening a file
//! re-develops off the UI thread through the render worker and installs a fresh
//! session.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

use eframe::egui;
use latent_edit::{Document, History, Settings};
use latent_image::ImageBuf;
use latent_pipeline::Backend;
use latent_pipeline::render;

use super::canvas;
use super::canvas::{PixelReadout, ViewTransform, Zoom};
use super::config::{self, Config};
use super::dialogs::{self, ExportSettings};
use super::panels;
use super::state::{JobKind, RenderJob, RenderOutput, RenderState, SessionData};
use super::theme;
use super::tools::crop::AspectRatio;
use super::tools::overlay::{OverlayCache, OverlayMode};
use super::tools::{CanvasDrag, CanvasTool};

/// Which before/after view the canvas is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BeforeAfter {
    /// The live edit (the normal view).
    #[default]
    Off,
    /// The cached unedited "before" in place of the live edit.
    Toggle,
    /// Before on the left, after on the right, split down the middle.
    Split,
}

impl BeforeAfter {
    /// Advance the before/after view one step: Off → Toggle → Split → Off. The
    /// single home of the cycle the toolbar, menu, and `` ` `` shortcut share.
    pub(crate) fn cycled(self) -> Self {
        match self {
            BeforeAfter::Off => BeforeAfter::Toggle,
            BeforeAfter::Toggle => BeforeAfter::Split,
            BeforeAfter::Split => BeforeAfter::Off,
        }
    }
}

/// Longest side of the interactive preview, in pixels. Keeps re-render cheap
/// during editing; export uses the full-resolution image.
pub(crate) const PREVIEW_MAX_DIM: u32 = 1600;

/// Which rendering backend is active, surfaced in the status bar. Threaded from
/// the composition root (`select_backend`) since the `Arc<dyn Backend>` itself
/// doesn't carry its kind. A future live backend toggle can reuse this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cpu,
    Gpu,
}

/// One opened RAW: the developed working bases, the per-variant edit histories,
/// the view (zoom/pan/before-after) and tool state, and the export defaults. Held
/// as `Option<Session>` on [`App`] — `None` is the welcome state. Opening a file
/// replaces the whole session, so nothing from the previous image leaks onto the
/// new one.
pub(crate) struct Session {
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
    /// Default export file name (the source stem with an image extension).
    pub(crate) output: String,
    /// The live preview texture (uploaded on the egui thread).
    pub(crate) texture: Option<egui::TextureHandle>,
    /// The cached "before" texture: the develop base rendered once with
    /// `Settings::default()`, drawn for the before/after view.
    pub(crate) before_texture: Option<egui::TextureHandle>,
    /// The last rendered preview image, stashed so the hover readout samples the
    /// pixel under the cursor without re-rendering.
    pub(crate) preview_rendered: Option<ImageBuf>,
    /// The current zoom intent (Fit, or a fixed percent).
    pub(crate) zoom: Zoom,
    /// The current pan offset (screen-space).
    pub(crate) pan: egui::Vec2,
    /// The screen↔image transform built on the last frame.
    pub(crate) last_transform: Option<ViewTransform>,
    /// Which before/after view to draw.
    pub(crate) before: BeforeAfter,
    /// The pixel under the cursor this frame, or `None` off the image.
    pub(crate) pixel_readout: Option<PixelReadout>,
    /// Index of the local adjustment selected for editing in the panel.
    pub(crate) local_sel: usize,
    /// Brush tool settings for painting dabs onto a brush mask (normalized).
    pub(crate) brush_radius: f32,
    pub(crate) brush_feather: f32,
    pub(crate) brush_erase: bool,
    /// Which curve channel the editor edits (0 = master, 1/2/3 = R/G/B).
    pub(crate) curve_channel: usize,
    /// The active on-canvas tool.
    pub(crate) tool: CanvasTool,
    /// The in-progress canvas drag.
    pub(crate) drag: Option<CanvasDrag>,
    /// Crop tool UI state.
    pub(crate) crop_aspect: AspectRatio,
    pub(crate) crop_aspect_locked: bool,
    pub(crate) crop_thirds: bool,
    /// Mask-overlay visualization mode and its cached coverage texture.
    pub(crate) overlay_mode: OverlayMode,
    pub(crate) overlay_cache: OverlayCache,
    /// Preview-base generation counter, bumped whenever the preview base is
    /// replaced (a new file), so the mask-overlay cache invalidates with it.
    pub(crate) preview_gen: u64,
    /// The export format/depth/quality choices, defaulted from the source name.
    pub(crate) export: ExportSettings,
}

impl Session {
    /// Build the live session from the worker's developed payload, defaulting the
    /// view, tool, and export state. The textures start empty — the first preview
    /// render uploads them on the egui thread.
    fn from_data(data: SessionData) -> Self {
        let export =
            ExportSettings::for_path(Path::new(&data.output), ExportSettings::default().quality);
        Self {
            full: data.full,
            preview: data.preview,
            variants: data.variants,
            active: 0,
            sidecar: data.sidecar,
            saved: data.saved,
            title: data.title,
            path: data.path,
            output: data.output,
            texture: None,
            before_texture: None,
            preview_rendered: None,
            zoom: Zoom::default(),
            pan: egui::Vec2::ZERO,
            last_transform: None,
            before: BeforeAfter::default(),
            pixel_readout: None,
            local_sel: 0,
            brush_radius: 0.08,
            brush_feather: 0.04,
            brush_erase: false,
            curve_channel: 0,
            tool: CanvasTool::default(),
            drag: None,
            crop_aspect: AspectRatio::default(),
            crop_aspect_locked: false,
            crop_thirds: true,
            overlay_mode: OverlayMode::default(),
            overlay_cache: OverlayCache::default(),
            preview_gen: 0,
            export,
        }
    }

    /// The history of the variant currently being edited.
    pub(crate) fn active_history(&mut self) -> &mut History<Settings> {
        &mut self.variants[self.active]
    }

    /// The displayed image aspect (`width / height`) of the shown texture, for the
    /// aspect-aware tools (crop ratio, straighten angle). Falls back to `1.0`
    /// before a texture/transform exists.
    pub(crate) fn displayed_aspect(&self) -> f32 {
        self.last_transform
            .map(|t| {
                let r = t.image_rect();
                if r.height() > 0.0 {
                    r.width() / r.height()
                } else {
                    1.0
                }
            })
            .unwrap_or(1.0)
    }

    /// Whether every variant equals what's on disk (no unsaved edits).
    pub(crate) fn is_saved(&self) -> bool {
        self.variants.len() == self.saved.len()
            && self
                .variants
                .iter()
                .zip(&self.saved)
                .all(|(h, s)| h.current() == s)
    }
}

/// Open the editor window. With `input`, the first file develops off-thread once
/// the window is up; with `None`, the window opens on the welcome state. The
/// persisted `config` supplies the window size and dialog defaults. Renders with
/// `backend` (whose `kind` is shown in the status bar).
pub fn run(
    input: Option<&Path>,
    backend: Box<dyn Backend>,
    kind: BackendKind,
    config: Config,
) -> Result<(), Box<dyn Error>> {
    // The trait is `Send + Sync`, so the backend can be shared with the worker.
    let backend: Arc<dyn Backend> = Arc::from(backend);
    let input = input.map(|p| p.to_path_buf());

    let icon = load_icon();
    let title = input
        .as_deref()
        .map(window_title)
        .unwrap_or_else(|| "latent".to_owned());
    // The persisted window size overrides the default when present.
    let size = config
        .window_size
        .map(|(w, h)| [w, h])
        .unwrap_or(theme::DEFAULT_WINDOW_SIZE);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
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
            theme::apply(&cc.egui_ctx);
            let mut app = App {
                session: None,
                backend,
                backend_kind: kind,
                config,
                status: String::new(),
                render: RenderState::default(),
                pending_load: false,
            };
            // Kick off the first file's develop on the worker (off the UI thread),
            // so the window paints immediately and the image arrives when ready.
            if let Some(input) = input {
                app.open_path(&cc.egui_ctx, &input);
            }
            Ok(Box::new(app) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| format!("could not start the editor window: {e}"))?;
    Ok(())
}

/// The window title for `input`: `<basename> — latent`, falling back to the full
/// path when there's no file name.
pub(crate) fn window_title(input: &Path) -> String {
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.display().to_string());
    format!("{name} — latent")
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
    /// The open image, or `None` for the welcome state.
    pub(crate) session: Option<Session>,
    /// The rendering backend (CPU, or GPU when selected and available). Shared
    /// with the render worker by `Arc` — the trait is `Send + Sync`.
    pub(crate) backend: Arc<dyn Backend>,
    /// Which backend kind is active, for the status bar.
    pub(crate) backend_kind: BackendKind,
    /// The persisted application config (recent files, last dirs, window size…).
    pub(crate) config: Config,
    /// The transient status line (export result, save error). Replaced by a toast
    /// queue later; a single line is enough for now.
    pub(crate) status: String,
    /// The off-thread render/load in flight (if any) plus a coalescing flag.
    pub(crate) render: RenderState,
    /// Whether the in-flight worker job is a file load (vs a preview/export), so
    /// the UI can show a loading state and gate re-entrant opens.
    pub(crate) pending_load: bool,
}

impl App {
    /// The open session, or `None` on the welcome state. Most per-frame work runs
    /// only when a session is present.
    pub(crate) fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Whether a file load is currently developing on the worker (the welcome /
    /// editor should show a loading state and not start a second open).
    pub(crate) fn is_loading(&self) -> bool {
        self.pending_load
    }

    /// Open `input` into the running app: develop it off the UI thread through the
    /// worker. On success a fresh [`Session`] replaces the current one; on failure
    /// the current session is left untouched and the error lands on the status
    /// line. Re-entrant opens are ignored while one is already loading.
    pub(crate) fn open_path(&mut self, ctx: &egui::Context, input: &Path) {
        if self.is_loading() {
            return;
        }
        // Remember the directory so the next Open dialog starts there.
        if let Some(parent) = input.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.set_last_open_dir(parent.to_path_buf());
        }
        let job = RenderJob {
            base: Arc::clone(self.placeholder_base()),
            settings: Settings::default(),
            backend: Arc::clone(&self.backend),
            kind: JobKind::Load {
                input: input.to_path_buf(),
            },
        };
        self.pending_load = true;
        self.status = format!("Opening {}…", input.display());
        self.spawn(ctx, job);
    }

    /// Open via the native file picker. Blocks while the dialog is up (picking a
    /// path), then develops off-thread. No-op while a load is already in flight.
    pub(crate) fn open_via_dialog(&mut self, ctx: &egui::Context) {
        if self.is_loading() {
            return;
        }
        let start = self.config.last_open_dir.clone();
        if let Some(path) = dialogs::pick_raw_file(start.as_deref()) {
            self.open_path(ctx, &path);
        }
    }

    /// A throwaway 1×1 base for the `Load` job's `RenderJob` (the load develops
    /// its own image; the job's `base`/`settings` are unused by the load arm but
    /// the struct requires them). Kept here so the load path reuses the existing
    /// `RenderJob` plumbing without a special-case spawn.
    fn placeholder_base(&self) -> &Arc<ImageBuf> {
        // A session's preview, or a tiny shared placeholder when none is open.
        static PLACEHOLDER: std::sync::OnceLock<Arc<ImageBuf>> = std::sync::OnceLock::new();
        match &self.session {
            Some(s) => &s.preview,
            None => PLACEHOLDER.get_or_init(|| Arc::new(ImageBuf::new(1, 1))),
        }
    }

    /// Record a successfully opened file in the recent list and the last-open dir,
    /// then persist the config. Called after a load installs a session.
    fn note_opened(&mut self, path: &Path) {
        config::push_recent(
            &mut self.config.recent_files,
            path.to_path_buf(),
            config::RECENT_FILES_CAP,
        );
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.config.last_open_dir = Some(parent.to_path_buf());
        }
        self.save_config();
    }

    /// Set the remembered open directory and persist if it changed.
    fn set_last_open_dir(&mut self, dir: PathBuf) {
        if self.config.last_open_dir.as_deref() != Some(dir.as_path()) {
            self.config.last_open_dir = Some(dir);
            self.save_config();
        }
    }

    /// Persist the config atomically and non-fatally: a failed write raises the
    /// status line and leaves the prior config intact (the temp-then-rename never
    /// truncates), it never crashes.
    pub(crate) fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.status = format!("Config save failed: {e}");
        }
    }

    /// Record the current window inner size for next launch, saving only when it
    /// changes by more than a pixel (so a resize drag doesn't write every frame).
    fn persist_window_size(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|i| i.viewport().inner_rect) else {
            return;
        };
        let (w, h) = (rect.width(), rect.height());
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let changed = match self.config.window_size {
            Some((pw, ph)) => (pw - w).abs() > 1.0 || (ph - h).abs() > 1.0,
            None => true,
        };
        if changed {
            self.config.window_size = Some((w, h));
            self.save_config();
        }
    }

    /// Open a dropped file (the first with a real path), routing into the same
    /// develop path as Open. Extra files in one drop are ignored (single-image).
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            self.open_path(ctx, &path);
        }
    }

    /// Paint a centered "drop to open" banner while a file is dragged over the
    /// window, so the drop target is obvious over an open image as well as the
    /// welcome screen. Pure overlay — drawn on a foreground layer above everything.
    fn show_drop_hint(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("drop_hint"),
        ));
        painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            "Drop a RAW to open",
            egui::FontId::proportional(28.0),
            egui::Color32::WHITE,
        );
        ctx.request_repaint();
    }

    /// Run the active variant's export at full resolution off the UI thread, to
    /// `output` at the chosen `depth`/`quality`. Skipped if a render/load is in
    /// flight (the Export action is disabled in that state).
    pub(crate) fn export_to(&mut self, ctx: &egui::Context, output: PathBuf) {
        if self.render.is_busy() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let depth = session.export.depth;
        let quality = session
            .export
            .format
            .has_quality()
            .then_some(session.export.quality);
        let job = RenderJob {
            base: Arc::clone(&session.full),
            settings: session.variants[session.active].current().clone(),
            backend: Arc::clone(&self.backend),
            kind: JobKind::Export {
                output,
                depth: Some(depth),
                quality,
            },
        };
        self.status = "Exporting…".to_owned();
        self.spawn(ctx, job);
    }

    /// Open the native Save dialog and, on a chosen path, export to it. The dialog
    /// starts in the remembered export dir (or the source's folder), seeded with
    /// the source stem; a chosen path updates the in-app format from its extension
    /// and remembers its directory.
    pub(crate) fn export_via_dialog(&mut self, ctx: &egui::Context) {
        if self.render.is_busy() {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let default_name = export_default_name(&session.output, session.export.format);
        let start = self
            .config
            .last_export_dir
            .clone()
            .or_else(|| Path::new(&session.path).parent().map(|p| p.to_path_buf()));
        let format = session.export.format;
        let Some(path) = dialogs::pick_export_file(&default_name, start.as_deref(), format) else {
            return;
        };
        // The chosen extension drives the format/depth; keep the picked quality.
        let quality = session.export.quality;
        if let Some(session) = &mut self.session {
            session.export = ExportSettings::for_path(&path, quality);
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.config.last_export_dir = Some(parent.to_path_buf());
            self.save_config();
        }
        self.export_to(ctx, path);
    }

    /// Request a preview re-render of the active variant on the worker. Coalesces
    /// to one render at a time (latest-wins). No-op with no session.
    pub(crate) fn render_preview(&mut self, ctx: &egui::Context) {
        if self.render.is_busy() {
            self.render.pending = true;
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        let job = RenderJob {
            base: Arc::clone(&session.preview),
            settings: session.variants[session.active].current().clone(),
            backend: Arc::clone(&self.backend),
            kind: JobKind::Preview,
        };
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

    /// Consume a finished worker job if one has been reported: install a loaded
    /// session, upload a fresh preview, or post an export's status line. Then, if a
    /// preview re-render was coalesced while this one ran, spawn it.
    fn poll_render(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.render.in_flight else {
            return;
        };
        match rx.try_recv() {
            Ok(out) => {
                self.render.in_flight = None;
                match out {
                    RenderOutput::Preview(img) => {
                        if let Some(session) = &mut self.session {
                            session.load_texture(ctx, &img);
                            session.ensure_before_texture(ctx, self.backend.as_ref());
                            session.preview_rendered = Some(img);
                        }
                    }
                    RenderOutput::Export(status) => self.status = status,
                    RenderOutput::Loaded(result) => {
                        self.pending_load = false;
                        match *result {
                            Ok(data) => {
                                let opened = PathBuf::from(&data.path);
                                self.session = Some(Session::from_data(data));
                                self.status.clear();
                                self.note_opened(&opened);
                                // Trigger the first preview render of the new image.
                                self.render_preview(ctx);
                            }
                            Err(e) => {
                                // Leave the current session intact; surface the error.
                                self.status = format!("Open failed: {e}");
                            }
                        }
                    }
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
                self.pending_load = false;
            }
        }
    }

    /// Write the active session's variants to its sidecar if they changed and no
    /// gesture is in progress. Atomic temp-then-rename so a failed write never
    /// truncates the sidecar.
    fn autosave(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        if !session.variants[session.active].is_idle() {
            return;
        }
        let current: Vec<Settings> = session
            .variants
            .iter()
            .map(|h| h.current().clone())
            .collect();
        if current == session.saved {
            return;
        }
        let doc = Document {
            version: Document::VERSION,
            variants: current.clone(),
        };
        match doc.to_ron() {
            Ok(text) => match config::atomic_write(&session.sidecar, &text) {
                Ok(()) => session.saved = current,
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            Err(e) => self.status = format!("Serialize failed: {e}"),
        }
    }

    /// Step the zoom one notch in (`+1`) or out (`−1`) along the ladder.
    pub(crate) fn zoom_step(&mut self, dir: i32) {
        if let Some(session) = &mut self.session {
            let fit = session.last_fit_scale();
            session.zoom = canvas::step_zoom(session.zoom, dir, fit);
        }
    }

    /// Snap the zoom to fit (and reset the pan, which is inert when fitted).
    pub(crate) fn zoom_fit(&mut self) {
        if let Some(session) = &mut self.session {
            session.zoom = Zoom::Fit;
            session.pan = egui::Vec2::ZERO;
        }
    }

    /// Snap the zoom to 100% (one preview-pixel per screen-pixel).
    pub(crate) fn zoom_actual(&mut self) {
        if let Some(session) = &mut self.session {
            session.zoom = Zoom::Percent(1.0);
        }
    }

    /// The current zoom percentage to show in the status bar.
    pub(crate) fn zoom_percent(&self) -> u32 {
        let scale = self
            .session
            .as_ref()
            .and_then(|s| s.last_transform)
            .map(|t| t.displayed_scale())
            .unwrap_or(1.0);
        (scale * 100.0).round().max(1.0) as u32
    }

    /// Apply the view keyboard shortcuts: `+`/`=` zoom in, `−` zoom out, `0` fit,
    /// `1` 100%, `` ` `` cycle the before/after view. Skipped while a panel widget
    /// wants keyboard input, and with no session.
    fn handle_view_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() || self.session.is_none() {
            return;
        }
        let mut zoom_in = false;
        let mut zoom_out = false;
        let mut fit = false;
        let mut actual = false;
        let mut cycle_before = false;
        ctx.input(|i| {
            // Don't collide with Cmd/Ctrl shortcuts (undo/redo etc.).
            if i.modifiers.command {
                return;
            }
            zoom_in = i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals);
            zoom_out = i.key_pressed(egui::Key::Minus);
            fit = i.key_pressed(egui::Key::Num0);
            actual = i.key_pressed(egui::Key::Num1);
            cycle_before = i.key_pressed(egui::Key::Backtick);
        });
        if zoom_in {
            self.zoom_step(1);
        }
        if zoom_out {
            self.zoom_step(-1);
        }
        if fit {
            self.zoom_fit();
        }
        if actual {
            self.zoom_actual();
        }
        if cycle_before && let Some(session) = &mut self.session {
            session.before = session.before.cycled();
            ctx.request_repaint();
        }
    }

    /// The brush keyboard shortcuts: `[` / `]` shrink/grow the brush radius, and
    /// the same with Shift for the feather. Only under the Brush tool, with a
    /// session, and skipped while a panel widget wants the keyboard.
    fn handle_brush_shortcuts(&mut self, ctx: &egui::Context) {
        let Some(session) = &mut self.session else {
            return;
        };
        if session.tool != CanvasTool::Brush || ctx.wants_keyboard_input() {
            return;
        }
        let (mut shrink, mut grow, mut shift) = (false, false, false);
        ctx.input(|i| {
            if i.modifiers.command {
                return;
            }
            shift = i.modifiers.shift;
            shrink = i.key_pressed(egui::Key::OpenBracket);
            grow = i.key_pressed(egui::Key::CloseBracket);
        });
        let step = if shrink {
            Some(1.0 / super::tools::BRUSH_KEY_STEP)
        } else if grow {
            Some(super::tools::BRUSH_KEY_STEP)
        } else {
            None
        };
        if let Some(k) = step {
            if shift {
                session.brush_feather =
                    super::tools::scaled_clamped(session.brush_feather.max(0.005), k, 0.0, 0.5);
            } else {
                session.brush_radius =
                    super::tools::scaled_clamped(session.brush_radius, k, 0.01, 0.5);
            }
            ctx.request_repaint();
        }
    }

    /// Apply the open shortcut (`Cmd`/`Ctrl`+O), opening the native picker. Guards
    /// against re-entrancy while a load is in flight.
    fn handle_open_shortcut(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let open = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O));
        if open {
            self.open_via_dialog(ctx);
        }
    }
}

impl Session {
    /// The fit scale for the last drawn frame.
    fn last_fit_scale(&self) -> f32 {
        self.last_transform.map(|t| t.fit_scale()).unwrap_or(1.0)
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

    /// Build the cached "before" texture once: the develop base rendered with
    /// `Settings::default()`, uploaded as a second texture the before/after view
    /// draws. Cheap — over the downscaled preview, only on the first preview.
    fn ensure_before_texture(&mut self, ctx: &egui::Context, backend: &dyn Backend) {
        if self.before_texture.is_some() {
            return;
        }
        let base = render(&self.preview, &Settings::default(), backend);
        let color = canvas::to_color_image(&base);
        self.before_texture =
            Some(ctx.load_texture("before", color, egui::TextureOptions::default()));
    }
}

/// The export file name to seed the Save dialog with: the source's output stem
/// re-extensioned to the chosen format. Pure so the seeding is testable.
pub(crate) fn export_default_name(output: &str, format: dialogs::ExportFormat) -> String {
    let stem = Path::new(output)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".to_owned());
    format!("{stem}.{}", format.canonical_ext())
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Pick up a finished worker job (loaded session, preview texture, export
        // status) first.
        self.poll_render(ctx);

        // Persist the window size when the user resizes (debounced: saved only when
        // it actually changes from what's stored).
        self.persist_window_size(ctx);

        // Accept a dropped file (the first with a path) as an open.
        self.handle_dropped_files(ctx);

        // Cmd/Ctrl+O opens the file picker, regardless of session.
        self.handle_open_shortcut(ctx);

        // A centered banner while a file is dragged over the window (drawn on a
        // foreground layer, so it sits above whichever state is shown).
        self.show_drop_hint(ctx);

        // With no open image, show the welcome state and skip the editor entirely.
        if self.session.is_none() {
            panels::menubar::show_minimal(self, ctx);
            panels::welcome::show(self, ctx);
            // While a file is developing, keep repainting so the editor appears the
            // moment the session installs.
            if self.is_loading() {
                ctx.request_repaint();
            }
            return;
        }

        // The first frame needs an initial render; once one is in flight or queued
        // we wait for it rather than re-triggering every frame.
        let texture_ready = self
            .session
            .as_ref()
            .map(|s| s.texture.is_some())
            .unwrap_or(true);
        let mut dirty = !texture_ready && !self.render.is_busy() && !self.render.pending;

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

        self.handle_view_shortcuts(ctx);
        self.handle_brush_shortcuts(ctx);

        // Chrome, in panel order: menu bar, toolbar, status bar, controls — then
        // the central canvas last so it takes the remaining space.
        panels::menubar::show(self, ctx, &mut do_undo, &mut do_redo);
        panels::toolbar::show(self, ctx, &mut do_undo, &mut do_redo, &mut dirty);
        panels::statusbar::show(self, ctx);
        dirty |= panels::controls::show(self, ctx);

        if let Some(session) = &mut self.session {
            if do_undo && session.active_history().undo() {
                dirty = true;
            }
            if do_redo && session.active_history().redo() {
                dirty = true;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use latent_cpu::CpuBackend;

    /// A file-less `App` for testing the welcome / no-session paths without a
    /// window. The render worker and config are inert.
    fn fileless_app() -> App {
        App {
            session: None,
            backend: Arc::new(CpuBackend),
            backend_kind: BackendKind::Cpu,
            config: Config::default(),
            status: String::new(),
            render: RenderState::default(),
            pending_load: false,
        }
    }

    #[test]
    fn app_constructs_with_no_session() {
        // The welcome state is representable: an `App` with `session: None` is
        // valid and reports no open image and no loading in flight.
        let app = fileless_app();
        assert!(app.session().is_none());
        assert!(!app.is_loading());
        // The session-gated accessors stay safe with no image.
        assert_eq!(app.zoom_percent(), 100);
    }

    #[test]
    fn export_default_name_reextensions_the_stem() {
        // The Save dialog's seed name takes the source stem and the format's
        // canonical extension.
        assert_eq!(
            export_default_name("photo.jpg", dialogs::ExportFormat::Png),
            "photo.png"
        );
        assert_eq!(
            export_default_name("/a/b/IMG_1234.jpg", dialogs::ExportFormat::Tiff),
            "IMG_1234.tif"
        );
    }

    #[test]
    fn window_title_uses_the_basename() {
        assert_eq!(
            window_title(Path::new("/photos/sunset.nef")),
            "sunset.nef — latent"
        );
    }
}
