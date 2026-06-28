//! Native file dialogs and the export format/depth/quality chooser.
//!
//! The Open and Save pickers are thin wrappers over `rfd` (native, XDG-portal
//! aware on Linux). They block while the user is in the dialog — acceptable for
//! picking a *path* — but the develop and the export that follow run off the UI
//! thread through the render worker.
//!
//! The export chooser is in-app egui state (format, bit depth, JPEG quality)
//! resolved against what each format can encode, mirroring the develop CLI's
//! "16 for tif/png, 8 for jpg" rule.

use std::path::{Path, PathBuf};

use eframe::egui;
use latent_export::Depth;

use crate::gui::shortcuts;

/// Show the shortcuts cheat-sheet modal when `open` is set, rendering every row of
/// the single [`shortcuts::SHORTCUTS`] table (so the help is generated from the
/// same list the input handler dispatches from). Closes on `Esc`, a click on the
/// backdrop, or the Close button, clearing `open`. The `?` key that opens it is
/// dispatched in `app` (guarded against text-field focus).
pub(crate) fn show_shortcuts(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    let modal = egui::Modal::new(egui::Id::new("shortcuts_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Keyboard shortcuts");
        ui.separator();
        egui::Grid::new("shortcuts_grid")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                for (keys, action) in shortcuts::cheat_sheet_rows() {
                    ui.monospace(keys);
                    ui.label(action);
                    ui.end_row();
                }
            });
        ui.separator();
        ui.vertical_centered(|ui| {
            if ui.button("Close").clicked() {
                *open = false;
            }
        });
    });
    if modal.should_close() {
        *open = false;
    }
}

/// The RAW extensions the Open dialog filters to. The real gate is
/// `latent_raw::unpack`, not this filter, so an "All files" entry keeps an
/// unusual extension reachable; this list is a convenience, kept deliberately
/// broad across the common camera makers.
const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", // Nikon
    "cr2", "cr3", "crw", // Canon
    "arw", "sr2", "srf", // Sony
    "dng", // Adobe / open
    "raf", // Fujifilm
    "orf", // Olympus
    "rw2", // Panasonic
    "pef", // Pentax
    "srw", // Samsung
    "raw", "rwl", // Leica / generic
    "iiq", // Phase One
    "3fr", "fff", // Hasselblad
    "x3f", // Sigma
];

/// Open a native file picker filtered to RAW images (plus an All-files escape
/// hatch), starting in `start_dir` when given. Returns the chosen path, or `None`
/// when the user cancels. Blocks while the dialog is open.
pub(crate) fn pick_raw_file(start_dir: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("RAW images", RAW_EXTENSIONS)
        .add_filter("All files", &["*"]);
    if let Some(dir) = start_dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

/// Open a native save picker for the export path. `default_name` seeds the file
/// name (from the source stem), `start_dir` the directory. The format filters are
/// ordered to match the format the caller pre-selected. Returns the chosen path,
/// or `None` on cancel. Blocks while the dialog is open.
pub(crate) fn pick_export_file(
    default_name: &str,
    start_dir: Option<&Path>,
    format: ExportFormat,
) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_file_name(default_name);
    if let Some(dir) = start_dir {
        dialog = dialog.set_directory(dir);
    }
    // List the chosen format's filter first so the dialog defaults to it.
    for f in format.ordered_from() {
        dialog = dialog.add_filter(f.label(), f.extensions());
    }
    dialog.save_file()
}

/// An output image format the export dialog can write. Each maps to a set of file
/// extensions and the bit depths its encoder supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportFormat {
    Jpeg,
    Png,
    Tiff,
}

impl ExportFormat {
    /// All formats, in menu order.
    pub(crate) const ALL: [ExportFormat; 3] =
        [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Tiff];

    /// The human label for the format chooser and the file-dialog filter.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Png => "PNG",
            ExportFormat::Tiff => "TIFF",
        }
    }

    /// The file extensions for this format (first is the canonical one).
    pub(crate) fn extensions(self) -> &'static [&'static str] {
        match self {
            ExportFormat::Jpeg => &["jpg", "jpeg"],
            ExportFormat::Png => &["png"],
            ExportFormat::Tiff => &["tif", "tiff"],
        }
    }

    /// The canonical extension used when forcing a path to this format.
    pub(crate) fn canonical_ext(self) -> &'static str {
        self.extensions()[0]
    }

    /// The format implied by a path's extension, if it is one we recognize.
    /// Lets the Save dialog's chosen extension drive the in-app format selection.
    pub(crate) fn from_path(path: &Path) -> Option<ExportFormat> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        ExportFormat::ALL
            .into_iter()
            .find(|f| f.extensions().contains(&ext.as_str()))
    }

    /// Whether this format's encoder can write at the given bit depth. JPEG is
    /// 8-bit only; PNG and TIFF support both. Keeps a 16-bit-JPEG choice
    /// unreachable in the UI.
    pub(crate) fn supports(self, depth: Depth) -> bool {
        match self {
            ExportFormat::Jpeg => depth == Depth::Eight,
            ExportFormat::Png | ExportFormat::Tiff => true,
        }
    }

    /// The depth that best suits this format — the develop CLI's rule (8 for
    /// JPEG, 16 for PNG/TIFF). Used as the default when the format changes.
    pub(crate) fn recommended_depth(self) -> Depth {
        match self {
            ExportFormat::Jpeg => Depth::Eight,
            ExportFormat::Png | ExportFormat::Tiff => Depth::Sixteen,
        }
    }

    /// Whether this format has a quality control (JPEG only).
    pub(crate) fn has_quality(self) -> bool {
        matches!(self, ExportFormat::Jpeg)
    }

    /// This format followed by the others, so the Save dialog lists the chosen
    /// one's filter first (and therefore defaults to it).
    fn ordered_from(self) -> Vec<ExportFormat> {
        let mut v = vec![self];
        v.extend(ExportFormat::ALL.into_iter().filter(|&f| f != self));
        v
    }
}

/// The export chooser's state: the selected format, bit depth, and (for JPEG) the
/// quality. Held on the running app so the choices persist between exports within
/// a session; the format/depth default from the source's extension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ExportSettings {
    pub(crate) format: ExportFormat,
    pub(crate) depth: Depth,
    /// JPEG quality (1–100); only meaningful when `format` is JPEG.
    pub(crate) quality: u8,
}

impl Default for ExportSettings {
    fn default() -> Self {
        // Default to JPEG/8-bit at a high quality — the common "send me a photo"
        // export. The dialog re-derives this from the source name when opened.
        Self {
            format: ExportFormat::Jpeg,
            depth: Depth::Eight,
            quality: 92,
        }
    }
}

impl ExportSettings {
    /// Select `format`, snapping the depth to that format's recommendation when
    /// the current depth isn't valid for it (so JPEG can never carry 16-bit).
    pub(crate) fn set_format(&mut self, format: ExportFormat) {
        self.format = format;
        if !format.supports(self.depth) {
            self.depth = format.recommended_depth();
        }
    }

    /// Build the export settings for a freshly chosen save `path`: pick the format
    /// from its extension (defaulting to JPEG), and the recommended depth for that
    /// format, carrying the current JPEG quality.
    pub(crate) fn for_path(path: &Path, quality: u8) -> Self {
        let format = ExportFormat::from_path(path).unwrap_or(ExportFormat::Jpeg);
        Self {
            format,
            depth: format.recommended_depth(),
            quality,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_from_path_routes_by_extension() {
        assert_eq!(
            ExportFormat::from_path(Path::new("p.jpg")),
            Some(ExportFormat::Jpeg)
        );
        assert_eq!(
            ExportFormat::from_path(Path::new("p.jpeg")),
            Some(ExportFormat::Jpeg)
        );
        assert_eq!(
            ExportFormat::from_path(Path::new("p.PNG")),
            Some(ExportFormat::Png)
        );
        assert_eq!(
            ExportFormat::from_path(Path::new("p.tiff")),
            Some(ExportFormat::Tiff)
        );
        assert_eq!(ExportFormat::from_path(Path::new("p.bmp")), None);
        assert_eq!(ExportFormat::from_path(Path::new("noext")), None);
    }

    #[test]
    fn depth_support_mirrors_the_encoder() {
        // JPEG is 8-bit only; PNG/TIFF take both. This is what keeps a 16-bit JPEG
        // unreachable in the UI.
        assert!(ExportFormat::Jpeg.supports(Depth::Eight));
        assert!(!ExportFormat::Jpeg.supports(Depth::Sixteen));
        assert!(ExportFormat::Png.supports(Depth::Eight));
        assert!(ExportFormat::Png.supports(Depth::Sixteen));
        assert!(ExportFormat::Tiff.supports(Depth::Eight));
        assert!(ExportFormat::Tiff.supports(Depth::Sixteen));
    }

    #[test]
    fn recommended_depth_matches_the_cli_rule() {
        // 8 for JPEG, 16 for PNG/TIFF — the develop CLI's format-driven default.
        assert_eq!(ExportFormat::Jpeg.recommended_depth(), Depth::Eight);
        assert_eq!(ExportFormat::Png.recommended_depth(), Depth::Sixteen);
        assert_eq!(ExportFormat::Tiff.recommended_depth(), Depth::Sixteen);
    }

    #[test]
    fn set_format_snaps_invalid_depth() {
        // Switching from a 16-bit format to JPEG snaps the depth to 8-bit (the
        // only depth JPEG can encode); switching the other way keeps a valid depth.
        let mut s = ExportSettings {
            format: ExportFormat::Tiff,
            depth: Depth::Sixteen,
            quality: 90,
        };
        s.set_format(ExportFormat::Jpeg);
        assert_eq!(s.depth, Depth::Eight);
        // PNG supports 16, but having just come from 8-bit JPEG we keep 8 (still
        // valid) rather than forcing a change.
        s.set_format(ExportFormat::Png);
        assert_eq!(s.depth, Depth::Eight);
    }

    #[test]
    fn for_path_derives_format_and_depth() {
        let png = ExportSettings::for_path(Path::new("/out/photo.png"), 80);
        assert_eq!(png.format, ExportFormat::Png);
        assert_eq!(png.depth, Depth::Sixteen);
        assert_eq!(png.quality, 80);

        let jpg = ExportSettings::for_path(Path::new("/out/photo.jpg"), 80);
        assert_eq!(jpg.format, ExportFormat::Jpeg);
        assert_eq!(jpg.depth, Depth::Eight);

        // An unknown extension falls back to JPEG/8.
        let unknown = ExportSettings::for_path(Path::new("/out/photo.xyz"), 80);
        assert_eq!(unknown.format, ExportFormat::Jpeg);
        assert_eq!(unknown.depth, Depth::Eight);
    }
}
