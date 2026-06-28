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
use super::scopes::Scopes;
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

/// Longest side of a variant thumbnail, in pixels. Tiny, so rendering one is
/// cheap and a list of them never starves the live preview.
pub(crate) const THUMB_MAX_DIM: u32 = 96;

/// A cached variant thumbnail: the uploaded texture plus the settings it was
/// rendered for, so an unchanged variant is not re-rendered and a changed one is
/// invalidated by comparing the stored settings against the variant's current.
pub(crate) struct VariantThumb {
    pub(crate) texture: egui::TextureHandle,
    /// The settings the thumbnail was rendered from; the cache is valid only while
    /// this equals the variant's current settings.
    pub(crate) rendered_for: Settings,
}

/// Which rendering backend is active, surfaced in the status bar. Threaded from
/// the composition root (`select_backend`) since the `Arc<dyn Backend>` itself
/// doesn't carry its kind. The live backend toggle reuses this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Cpu,
    Gpu,
}

impl BackendKind {
    /// The label shown in the status bar.
    pub(crate) fn label(self) -> &'static str {
        match self {
            BackendKind::Cpu => "CPU",
            BackendKind::Gpu => "GPU",
        }
    }
}

/// Pick a rendering backend. With `use_gpu`, try the GPU backend and fall back to
/// the CPU one if no device is available; otherwise use the always-available CPU
/// backend. Returns the backend and which kind it actually is (so a caller that
/// requested GPU can tell whether it fell back). Shared by the launch path and the
/// in-app toggle so both build the backend the exact same way.
pub fn select_backend(use_gpu: bool) -> (Box<dyn Backend>, BackendKind) {
    if use_gpu {
        match latent_gpu::GpuBackend::new() {
            Ok(gpu) => {
                eprintln!("using GPU backend");
                return (Box::new(gpu), BackendKind::Gpu);
            }
            Err(e) => eprintln!("GPU unavailable ({e}); using CPU backend"),
        }
    }
    (Box::new(latent_cpu::CpuBackend), BackendKind::Cpu)
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
    /// A very small working base, rendered over for the variant thumbnails (so a
    /// thumbnail render is trivially cheap and never contends with the preview).
    pub(crate) thumb_base: Arc<ImageBuf>,
    /// One independent edit history per variant; never empty.
    pub(crate) variants: Vec<History<Settings>>,
    /// Display names for the variants, parallel to `variants`. An empty entry is
    /// "unnamed" and the UI shows "Variant N" instead.
    pub(crate) names: Vec<String>,
    /// Cached tiny thumbnail per variant, parallel to `variants`. `None` until
    /// rendered; invalidated (set back to `None`) when a variant's settings change.
    pub(crate) thumbs: Vec<Option<VariantThumb>>,
    /// Index of the variant currently being edited and previewed.
    pub(crate) active: usize,
    /// Sidecar path (`<raw>.ron`) the document auto-saves to.
    pub(crate) sidecar: PathBuf,
    /// Last variants written to the sidecar, to avoid redundant writes.
    pub(crate) saved: Vec<Settings>,
    /// Last variant names written to the sidecar, so a rename autosaves.
    pub(crate) saved_names: Vec<String>,
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
    /// Index of the mask shape (within the selected local) being edited — the one
    /// the on-canvas handles and the brush follow.
    pub(crate) shape_sel: usize,
    /// A white-balance action the panel requested this frame (eyedropper / auto),
    /// handled after the panel closure releases the session borrow.
    pub(crate) wb_action: crate::gui::widgets::WbAction,
    /// The RAW's decoded EXIF metadata, for on-demand lens detection on the main
    /// thread (the lensfun `Database` is not `Send`).
    pub(crate) meta: latent_raw::Metadata,
    /// The detected lens's display name once the user enables lens correction, for
    /// the panel label. `None` until a successful detection.
    pub(crate) lens_name: Option<String>,
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
    /// The cached image scopes (histogram, waveform, clipping overlay) plus the
    /// chosen scope type and clip toggles. Recomputed once per preview in
    /// [`Self::load_texture`] from the same display bytes the texture is built
    /// from; the per-frame draw only paints the cached data.
    pub(crate) scopes: Scopes,
}

impl Session {
    /// Build the live session from the worker's developed payload, defaulting the
    /// view, tool, and export state. The textures start empty — the first preview
    /// render uploads them on the egui thread.
    fn from_data(data: SessionData) -> Self {
        let export =
            ExportSettings::for_path(Path::new(&data.output), ExportSettings::default().quality);
        let thumb_base = Arc::new(data.preview.downscaled(THUMB_MAX_DIM));
        let variant_count = data.variants.len();
        Self {
            full: data.full,
            preview: data.preview,
            thumb_base,
            variants: data.variants,
            names: data.names,
            thumbs: (0..variant_count).map(|_| None).collect(),
            active: 0,
            sidecar: data.sidecar,
            saved: data.saved,
            saved_names: data.saved_names,
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
            shape_sel: 0,
            wb_action: crate::gui::widgets::WbAction::None,
            meta: data.meta,
            lens_name: None,
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
            scopes: Scopes::default(),
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

    /// Whether every variant (and its name) equals what's on disk — no unsaved
    /// edits or renames.
    pub(crate) fn is_saved(&self) -> bool {
        self.variants.len() == self.saved.len()
            && self
                .variants
                .iter()
                .zip(&self.saved)
                .all(|(h, s)| h.current() == s)
            && self.names == self.saved_names
    }

    /// The display name for variant `i`: its stored name, or a positional
    /// "Variant N" fallback when unnamed.
    pub(crate) fn variant_label(&self, i: usize) -> String {
        match self.names.get(i) {
            Some(name) if !name.is_empty() => name.clone(),
            _ => format!("Variant {}", i + 1),
        }
    }

    /// Duplicate variant `i`: a fresh history seeded with its current settings, a
    /// copied name (suffixed " copy"), and an empty thumbnail slot, inserted right
    /// after it and selected. A UI-state mutation (not a `Settings` edit), so it
    /// does not go through `History`; it is persisted by autosave.
    pub(crate) fn duplicate_variant(&mut self, i: usize) {
        let copy = self.variants[i].current().clone();
        let base_name = self.variant_label(i);
        let new = (i + 1).min(self.variants.len());
        self.variants.insert(new, History::new(copy));
        self.names.insert(new, format!("{base_name} copy"));
        self.thumbs.insert(new, None);
        self.active = new;
    }

    /// Delete variant `i`, keeping at least one variant (deleting the last is
    /// refused). Returns whether a variant was removed. Clamps `active` to follow.
    pub(crate) fn delete_variant(&mut self, i: usize) -> bool {
        if self.variants.len() <= 1 || i >= self.variants.len() {
            return false;
        }
        self.variants.remove(i);
        self.names.remove(i);
        self.thumbs.remove(i);
        super::state::clamp_selection(&mut self.active, self.variants.len());
        true
    }

    /// Move variant `i` by `delta` (`-1` up, `+1` down), carrying its name and
    /// cached thumbnail in lockstep and re-pointing `active` to follow the moved
    /// item. Returns whether anything moved. A UI-state mutation, persisted by
    /// autosave.
    pub(crate) fn move_variant(&mut self, i: usize, delta: isize) -> bool {
        let len = self.variants.len() as isize;
        let j = i as isize + delta;
        if i >= self.variants.len() || j < 0 || j >= len {
            return false;
        }
        let j = j as usize;
        self.variants.swap(i, j);
        self.names.swap(i, j);
        self.thumbs.swap(i, j);
        if self.active == i {
            self.active = j;
        } else if self.active == j {
            self.active = i;
        }
        true
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
                panel_visible: true,
                shortcuts_open: false,
                pending_backend: None,
                clipboard: None,
                preset_name_input: String::new(),
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
    /// Whether the right-hand controls panel is shown (toggled with `Tab` for a
    /// full-bleed canvas). Defaults to shown.
    pub(crate) panel_visible: bool,
    /// Whether the keyboard-shortcuts cheat-sheet modal is open (toggled with `?`).
    pub(crate) shortcuts_open: bool,
    /// A requested backend switch (the desired GPU on/off) that could not be
    /// applied immediately because a render was in flight. Consumed in
    /// [`Self::poll_render`] once the worker goes idle, so the in-flight render
    /// always finishes on the backend it started with. `None` when no switch is
    /// pending.
    pub(crate) pending_backend: Option<bool>,
    /// The develop-settings clipboard: the last copied variant settings, applied by
    /// Paste. Process-local UI state — not the OS clipboard, not persisted.
    pub(crate) clipboard: Option<Settings>,
    /// The in-progress preset name being typed in the presets block. Held here so
    /// the field keeps its text across frames; cleared after a save.
    pub(crate) preset_name_input: String,
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
                // The worker is now idle. A backend switch deferred while it ran is
                // safe to apply here, before any coalesced re-render is spawned, so
                // the next render uses the new backend. The swap itself triggers a
                // re-render, which also satisfies a coalesced request.
                if self.pending_backend.is_some() {
                    self.render.pending = false;
                    self.apply_pending_backend(ctx);
                } else if std::mem::take(&mut self.render.pending) {
                    // A request that arrived mid-render coalesced to one; run it now.
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
        if current == session.saved && session.names == session.saved_names {
            return;
        }
        let names = session.names.clone();
        let doc = Document {
            version: Document::VERSION,
            variants: current.clone(),
            names: names.clone(),
        };
        match doc.to_ron() {
            Ok(text) => match config::atomic_write(&session.sidecar, &text) {
                Ok(()) => {
                    session.saved = current;
                    session.saved_names = names;
                }
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

    /// Dispatch the keyboard shortcuts for this frame from the single
    /// [`shortcuts`](super::shortcuts) table: the dispatcher and the cheat-sheet
    /// read the same list. Each fired binding is applied through the same `App`
    /// method its button/menu uses, so a shortcut and a click never diverge.
    /// Bare-letter bindings are suppressed while a text field / numeric entry /
    /// name field holds keyboard focus (so typing a name never switches tools);
    /// command-modified bindings stay live. Returns whether the preview is now
    /// dirty (a paste/reset/undo/redo changed the settings).
    fn dispatch_shortcuts(&mut self, ctx: &egui::Context) -> bool {
        // A focused text widget (a TextEdit / DragValue being typed into) gates the
        // bare bindings; this is the load-bearing typing-collision guard.
        let focused = ctx.memory(|m| m.focused()).is_some();
        let actions = ctx.input(|i| super::shortcuts::fired_actions(i, focused));
        // The brush feather modifier is a live read at apply time (Shift+`[`).
        let shift = ctx.input(|i| i.modifiers.shift);
        let mut dirty = false;
        for action in actions {
            dirty |= self.apply_action(action, shift, ctx);
        }
        dirty
    }

    /// Apply one shortcut [`Action`](super::shortcuts::Action), returning whether it
    /// dirtied the preview. The single place a shortcut turns into behavior — every
    /// arm calls the same method the matching button/menu calls.
    fn apply_action(
        &mut self,
        action: super::shortcuts::Action,
        shift: bool,
        ctx: &egui::Context,
    ) -> bool {
        use super::shortcuts::Action;
        match action {
            Action::Open => {
                self.open_via_dialog(ctx);
                false
            }
            Action::Export => {
                self.export_via_dialog(ctx);
                false
            }
            Action::Undo => self
                .session
                .as_mut()
                .is_some_and(|s| s.active_history().undo()),
            Action::Redo => self
                .session
                .as_mut()
                .is_some_and(|s| s.active_history().redo()),
            Action::Copy => {
                self.copy_settings();
                false
            }
            Action::Paste => self.paste_settings(),
            Action::ResetAll => self.reset_all_develop(),
            Action::ZoomFit => {
                self.zoom_fit();
                false
            }
            Action::ZoomActual => {
                self.zoom_actual();
                false
            }
            Action::ZoomIn => {
                self.zoom_step(1);
                false
            }
            Action::ZoomOut => {
                self.zoom_step(-1);
                false
            }
            Action::BeforeAfter => {
                if let Some(session) = &mut self.session {
                    session.before = session.before.cycled();
                }
                false
            }
            Action::TogglePanel => {
                self.panel_visible = !self.panel_visible;
                false
            }
            Action::ToggleHelp => {
                self.shortcuts_open = !self.shortcuts_open;
                false
            }
            Action::BrushSmaller => {
                self.nudge_brush(1.0 / super::tools::BRUSH_KEY_STEP, shift);
                false
            }
            Action::BrushLarger => {
                self.nudge_brush(super::tools::BRUSH_KEY_STEP, shift);
                false
            }
            Action::NextVariant => {
                self.cycle_variant(1);
                false
            }
            Action::PrevVariant => {
                self.cycle_variant(-1);
                false
            }
            Action::ToolCrop => {
                self.select_tool(CanvasTool::Crop);
                false
            }
            Action::ToolBrush => {
                self.select_tool(CanvasTool::Brush);
                false
            }
        }
    }

    /// Toggle the active on-canvas `tool` (re-selecting the current one returns to
    /// the pure-view tool), matching the toolbar's selectable behavior. No-op
    /// without a session.
    fn select_tool(&mut self, tool: CanvasTool) {
        if let Some(session) = &mut self.session {
            session.tool = if session.tool == tool {
                CanvasTool::None
            } else {
                tool
            };
        }
    }

    /// Cycle the active variant by `delta` (`+1` next, `-1` previous), wrapping.
    /// No-op without a session or with a single variant.
    fn cycle_variant(&mut self, delta: isize) {
        if let Some(session) = &mut self.session {
            let len = session.variants.len();
            if len > 1 {
                let next = (session.active as isize + delta).rem_euclid(len as isize) as usize;
                session.active = next;
            }
        }
    }

    /// Scale the brush radius (or, with `shift`, the feather) by `factor`, clamped
    /// to the brush range — the `[` / `]` (and `Shift+[`/`]`) behavior, only under
    /// the Brush tool.
    fn nudge_brush(&mut self, factor: f32, shift: bool) {
        if let Some(session) = &mut self.session
            && session.tool == CanvasTool::Brush
        {
            if shift {
                session.brush_feather = super::tools::scaled_clamped(
                    session.brush_feather.max(0.005),
                    factor,
                    0.0,
                    0.5,
                );
            } else {
                session.brush_radius =
                    super::tools::scaled_clamped(session.brush_radius, factor, 0.01, 0.5);
            }
        }
    }

    /// Apply a white-balance action the panel requested this frame: activating the
    /// gray eyedropper (a canvas tool), or running a gray-world Auto estimate from
    /// the preview's average linear RGB. Both write only the global
    /// `WhiteBalance { temp, tint }` field; Auto is one undo step. Returns whether
    /// the preview is now dirty.
    fn handle_wb_action(&mut self) -> bool {
        use crate::gui::widgets::{WbAction, wb};
        let Some(session) = &mut self.session else {
            return false;
        };
        let action = std::mem::take(&mut session.wb_action);
        match action {
            WbAction::None => false,
            WbAction::PickGray => {
                // Arm the eyedropper; the pick is consumed on the next canvas click.
                session.tool = CanvasTool::WbPick;
                false
            }
            WbAction::Auto => {
                let Some(mean) = session.preview_rendered.as_ref().and_then(image_mean_rgb) else {
                    return false;
                };
                let history = &mut session.variants[session.active];
                let current = history.current().global.white_balance.unwrap_or_default();
                let estimated = wb::auto_wb(mean, current);
                history.begin();
                history.current_mut().global.white_balance =
                    crate::gui::widgets::wb_or_none(estimated);
                history.commit();
                true
            }
        }
    }

    /// Whether the GPU backend is currently the active one.
    pub(crate) fn gpu_active(&self) -> bool {
        self.backend_kind == BackendKind::Gpu
    }

    /// Copy the active variant's settings into the in-app clipboard. Process-local
    /// UI state — not the OS clipboard, not persisted. A no-op with no session.
    pub(crate) fn copy_settings(&mut self) {
        if let Some(session) = &self.session {
            self.clipboard = Some(session.variants[session.active].current().clone());
        }
    }

    /// Whether a Paste is available (the clipboard holds settings and a session is
    /// open).
    pub(crate) fn can_paste(&self) -> bool {
        self.clipboard.is_some() && self.session.is_some()
    }

    /// Apply the clipboard's **develop** settings onto the active variant, keeping
    /// the target's geometry, as **one** undo step. Pasting settings identical to
    /// the target records no step (the History `prev != current` guard). Returns
    /// whether the preview is now dirty.
    pub(crate) fn paste_settings(&mut self) -> bool {
        let Some(clip) = self.clipboard.clone() else {
            return false;
        };
        let Some(session) = &mut self.session else {
            return false;
        };
        let history = &mut session.variants[session.active];
        let merged = super::state::merge_develop(history.current(), &clip);
        history.begin();
        *history.current_mut() = merged;
        history.commit();
        true
    }

    /// Reset the active variant's develop settings to neutral, keeping its
    /// geometry, as **one** undo step. Returns whether the preview is now dirty.
    pub(crate) fn reset_all_develop(&mut self) -> bool {
        let Some(session) = &mut self.session else {
            return false;
        };
        let history = &mut session.variants[session.active];
        let reset = super::state::reset_develop(history.current());
        history.begin();
        *history.current_mut() = reset;
        history.commit();
        true
    }

    /// Apply a develop preset's settings onto the active variant, keeping the
    /// target's geometry, as **one** undo step (sharing the paste path). The
    /// preset's settings are sanitized first, since a hand-edited config is
    /// untrusted. Returns whether the preview is now dirty.
    pub(crate) fn apply_preset(&mut self, preset: &config::Preset) -> bool {
        let Some(session) = &mut self.session else {
            return false;
        };
        let mut clean = preset.settings.clone();
        clean.sanitize();
        let history = &mut session.variants[session.active];
        let merged = super::state::merge_develop(history.current(), &clean);
        history.begin();
        *history.current_mut() = merged;
        history.commit();
        true
    }

    /// Save the active variant's current develop settings as a named preset in the
    /// app config (geometry stripped, since a preset is a reusable *look*) and
    /// persist. A no-op with no session or an empty name.
    pub(crate) fn save_preset(&mut self, name: String) {
        let Some(session) = &self.session else {
            return;
        };
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        let settings = session.variants[session.active].current();
        let preset = config::Preset::from_settings(name, settings);
        config::upsert_preset(&mut self.config.presets, preset);
        self.save_config();
    }

    /// Request a switch to the GPU (`true`) or CPU (`false`) backend at runtime.
    /// A no-op when the requested kind is already active. The swap must never
    /// replace the backend while a render is in flight (the in-flight render owns
    /// its `Arc` and must finish on the backend it started with), so when the
    /// worker is busy the request is deferred to the next idle point in
    /// [`Self::poll_render`]; when idle it is applied immediately.
    pub(crate) fn request_backend(&mut self, use_gpu: bool, ctx: &egui::Context) {
        if use_gpu == self.gpu_active() {
            self.pending_backend = None;
            return;
        }
        if self.render.is_busy() {
            // Defer; the worker is mid-render. Applied once it goes idle.
            self.pending_backend = Some(use_gpu);
            self.status = "Switching backend…".to_owned();
            ctx.request_repaint();
        } else {
            self.swap_backend(use_gpu, ctx);
        }
    }

    /// Build the requested backend via the shared [`select_backend`] and install
    /// it, persist the preference, and trigger a re-render. GPU init can fail and
    /// fall back to CPU; the status line and the status-bar kind reflect the
    /// **actually-active** backend, not the requested one. Only ever called when
    /// the worker is idle.
    fn swap_backend(&mut self, use_gpu: bool, ctx: &egui::Context) {
        let (backend, kind) = select_backend(use_gpu);
        self.backend = Arc::from(backend);
        self.backend_kind = kind;
        // The before texture was rendered on the previous backend; CPU/GPU are
        // pixel-equivalent, so this is not required for correctness, but it lets the
        // before/after view re-render through the new backend for consistency.
        if let Some(session) = &mut self.session {
            session.before_texture = None;
        }
        // Persist the *preference* the user asked for, even if GPU init fell back —
        // a stored "GPU on" still gracefully degrades on a device-less next launch.
        if self.config.gpu != use_gpu {
            self.config.gpu = use_gpu;
            self.save_config();
        }
        // Report a GPU fallback distinctly from a clean switch.
        self.status = if use_gpu && kind == BackendKind::Cpu {
            "GPU unavailable, using CPU".to_owned()
        } else {
            format!("Using {} backend", kind.label())
        };
        self.render_preview(ctx);
    }

    /// Apply a deferred backend switch once the worker is idle. Called from
    /// [`Self::poll_render`] right after the in-flight render is consumed and before
    /// any coalesced re-render is spawned, so the next render uses the new backend.
    fn apply_pending_backend(&mut self, ctx: &egui::Context) {
        if let Some(use_gpu) = self.pending_backend.take() {
            self.swap_backend(use_gpu, ctx);
        }
    }
}

/// The average linear RGB over a whole image, or `None` for an empty image. Used
/// by the gray-world Auto white balance as the patch to neutralize.
fn image_mean_rgb(img: &ImageBuf) -> Option<[f32; 3]> {
    let (w, h) = (img.width(), img.height());
    let count = (w as u64) * (h as u64);
    if count == 0 {
        return None;
    }
    let mut sum = [0.0_f64; 3];
    for y in 0..h {
        for x in 0..w {
            let p = img.get(x, y);
            sum[0] += p[0] as f64;
            sum[1] += p[1] as f64;
            sum[2] += p[2] as f64;
        }
    }
    let n = count as f64;
    Some([
        (sum[0] / n) as f32,
        (sum[1] / n) as f32,
        (sum[2] / n) as f32,
    ])
}

impl Session {
    /// The fit scale for the last drawn frame.
    fn last_fit_scale(&self) -> f32 {
        self.last_transform.map(|t| t.fit_scale()).unwrap_or(1.0)
    }

    /// Upload a rendered preview image into the preview texture (creating it on
    /// the first frame) and refresh the cached scopes from the **same** display
    /// bytes. The texture upload must run on the egui thread.
    ///
    /// This is the single once-per-preview hook: the output transform
    /// ([`latent_export::to_srgb8`]) runs **once** here, and the resulting bytes
    /// feed both the texture and the scopes (histogram, waveform, clip overlay),
    /// so the scopes match the texture and the file by construction and never
    /// recompute on a per-frame paint.
    fn load_texture(&mut self, ctx: &egui::Context, img: &ImageBuf) {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let bytes = latent_export::to_srgb8(img);
        let color = canvas::color_image_from_srgb8(w, h, &bytes);
        match &mut self.texture {
            Some(tex) => tex.set(color, egui::TextureOptions::default()),
            None => {
                self.texture =
                    Some(ctx.load_texture("preview", color, egui::TextureOptions::default()));
            }
        }
        // Refresh the scopes from the same display bytes (not per frame).
        self.scopes.recompute(ctx, &bytes, w, h);
    }

    /// Ensure variant `i` has an up-to-date thumbnail texture, rendering one only
    /// when the slot is empty or the cached settings no longer match the variant's
    /// current settings. The thumbnail goes through the **same** `render` /
    /// `to_color_image` transform as the live preview, only over a tiny base
    /// ([`THUMB_MAX_DIM`]), so it is trivially cheap and pixel-consistent. Cached,
    /// so an unchanged variant is never re-rendered.
    pub(crate) fn ensure_thumb(&mut self, ctx: &egui::Context, i: usize, backend: &dyn Backend) {
        let Some(history) = self.variants.get(i) else {
            return;
        };
        let current = history.current();
        let fresh = self
            .thumbs
            .get(i)
            .and_then(|t| t.as_ref())
            .is_some_and(|t| &t.rendered_for == current);
        if fresh {
            return;
        }
        let settings = current.clone();
        let rendered = render(&self.thumb_base, &settings, backend);
        let color = canvas::to_color_image(&rendered);
        let texture = ctx.load_texture(
            format!("variant_thumb_{i}"),
            color,
            egui::TextureOptions::default(),
        );
        if let Some(slot) = self.thumbs.get_mut(i) {
            *slot = Some(VariantThumb {
                texture,
                rendered_for: settings,
            });
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

        // A centered banner while a file is dragged over the window (drawn on a
        // foreground layer, so it sits above whichever state is shown).
        self.show_drop_hint(ctx);

        // With no open image, show the welcome state and skip the editor entirely.
        if self.session.is_none() {
            // The file-open and help shortcuts are reachable from the welcome state
            // too (`Cmd/Ctrl+O` to open, `?`/Help ▸ Keyboard shortcuts for help).
            self.dispatch_shortcuts(ctx);
            dialogs::show_shortcuts(ctx, &mut self.shortcuts_open);
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

        // All keyboard shortcuts (undo/redo, zoom, brush, tools, copy/paste/reset,
        // variant cycling, panel/help) dispatch from the single shortcuts table.
        dirty |= self.dispatch_shortcuts(ctx);

        // The toolbar/menu Undo/Redo buttons set these so the button and the
        // shortcut land on the same single history path.
        let (mut do_undo, mut do_redo) = (false, false);

        // The shortcuts cheat-sheet modal (opened with `?`), drawn over the chrome.
        dialogs::show_shortcuts(ctx, &mut self.shortcuts_open);

        // Chrome, in panel order: menu bar, toolbar, status bar, controls — then
        // the central canvas last so it takes the remaining space.
        panels::menubar::show(self, ctx, &mut do_undo, &mut do_redo, &mut dirty);
        panels::toolbar::show(self, ctx, &mut do_undo, &mut do_redo, &mut dirty);
        panels::statusbar::show(self, ctx);
        dirty |= panels::controls::show(self, ctx);

        // Apply any white-balance action the panel requested (eyedropper / auto),
        // released from the panel's session borrow.
        dirty |= self.handle_wb_action();

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
            panel_visible: true,
            shortcuts_open: false,
            pending_backend: None,
            clipboard: None,
            preset_name_input: String::new(),
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
