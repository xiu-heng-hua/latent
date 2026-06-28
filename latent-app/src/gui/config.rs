//! The small persisted application config: window size, recent files, side-panel
//! width, the last-used open/export directories, the rendering-backend preference,
//! and the theme. It is the backing store several parts of the UI read and write.
//!
//! The file lives in the per-OS config directory (`~/.config/latent/config.ron`
//! on Linux, the platform-appropriate path elsewhere), is loaded once at startup,
//! and is written back when a tracked value changes. Writes are
//! **temp-then-rename** so a failed or interrupted write never truncates the
//! existing file, and every error path is non-fatal: a missing or corrupt file
//! falls back to defaults rather than crashing.
//!
//! Every field is `#[serde(default)]` so an older config that predates a field
//! still loads — the missing field simply takes its default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many recent files to remember. Newest-first, de-duplicated, capped here.
pub(crate) const RECENT_FILES_CAP: usize = 10;

/// The window theme. A small enum so the surface can grow later; the editor's
/// tuned dark visuals are the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum Theme {
    /// The tuned dark visuals.
    #[default]
    Dark,
    /// A light variant (reserved; the dark visuals are what ships today).
    Light,
}

/// The persisted application config. Flat and serde-derived; serialized as RON to
/// match the edit sidecar's format. Every field defaults so a partial or older
/// file still loads.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    /// Last window inner size in logical points, re-applied on launch.
    pub(crate) window_size: Option<(f32, f32)>,
    /// Most-recently-opened files, newest first, de-duplicated and capped.
    pub(crate) recent_files: Vec<PathBuf>,
    /// Remembered width of the right-hand controls side panel.
    pub(crate) side_panel_width: Option<f32>,
    /// The directory the last successful export wrote into, for the next Save.
    pub(crate) last_export_dir: Option<PathBuf>,
    /// The directory the last Open dialog picked from, for the next Open.
    pub(crate) last_open_dir: Option<PathBuf>,
    /// Whether to prefer the GPU backend when a device is available.
    pub(crate) gpu: bool,
    /// The window theme.
    pub(crate) theme: Theme,
    /// Open/closed state of the controls-panel sections, keyed by a stable
    /// section key (not the display label, so renaming a header never orphans
    /// saved state). A missing key falls back to the section's own default-open.
    pub(crate) sections_open: BTreeMap<String, bool>,
}

impl Config {
    /// The config file's path inside the per-OS config dir, or `None` when no
    /// such directory can be resolved (a headless/locked-down environment). The
    /// directory is *not* created here — only on a successful write.
    pub(crate) fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "latent")
            .map(|dirs| dirs.config_dir().join("config.ron"))
    }

    /// Load the config from `path`, falling back to defaults on any error:
    /// a missing file is first-run (defaults, no error), and a corrupt file is
    /// recovered to defaults rather than crashing. Never returns an `Err` —
    /// startup must not be blocked by a bad config.
    pub(crate) fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_ron(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Load the config from the per-OS config dir, or defaults when no dir is
    /// resolvable. The startup entry point.
    pub(crate) fn load() -> Self {
        Self::path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    /// Serialize to RON. Pretty-printed to match the sidecar's on-disk style.
    pub(crate) fn to_ron(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())
    }

    /// Parse from RON. A parse failure is an `Err` the caller maps to defaults.
    pub(crate) fn from_ron(text: &str) -> Result<Self, String> {
        ron::from_str(text).map_err(|e| e.to_string())
    }

    /// Write the config to `path` atomically: serialize, write to a sibling temp
    /// file on the same filesystem, then rename over the real file so a failed or
    /// interrupted write never truncates the existing config. Creates the parent
    /// directory if missing. The error is returned (to be surfaced to the user),
    /// never panicked on.
    pub(crate) fn save_to(&self, path: &Path) -> Result<(), String> {
        let text = self.to_ron()?;
        atomic_write(path, &text)
    }

    /// Write the config to the per-OS config dir, atomically and non-fatally. A
    /// missing config dir (no resolvable path) is treated as success-with-nothing
    /// to do rather than an error, since there is nowhere to persist to.
    pub(crate) fn save(&self) -> Result<(), String> {
        match Self::path() {
            Some(path) => self.save_to(&path),
            None => Ok(()),
        }
    }
}

/// Write `text` to `path` atomically (temp file in the same directory, then
/// rename). The parent directory is created if missing. On the same filesystem a
/// rename is atomic, so the destination is never seen half-written and the old
/// file survives a failed write. Returns a printable error string on failure.
pub(crate) fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    }
    // The temp file sits beside the destination so the rename stays within one
    // filesystem (where it is atomic). A unique-enough suffix avoids colliding
    // with a real file.
    let tmp = path.with_extension("ron.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write temp config: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // Best-effort cleanup of the temp file if the rename failed.
        let _ = std::fs::remove_file(&tmp);
        format!("replace config: {e}")
    })?;
    Ok(())
}

/// Insert `path` at the front of a most-recent-first list: canonicalize it (so the
/// same file reached by different relative paths de-duplicates), remove any earlier
/// copy, push it to the front, and cap the length. Pure — no I/O beyond the
/// canonicalize attempt — so the ordering/dedup/cap is unit-testable.
pub(crate) fn push_recent(list: &mut Vec<PathBuf>, path: PathBuf, cap: usize) {
    // Canonicalize when possible (resolves `.`/`..` and symlinks so the same file
    // de-dups); fall back to the path as given when it can't be resolved.
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    list.retain(|p| p != &canonical);
    list.insert(0, canonical);
    list.truncate(cap);
}

/// Filter a recent-files list down to the entries whose file still exists. Used
/// when building the Open Recent menu and the welcome list so a stale entry is
/// never shown — and a missing file is never a trap. Pure over the input.
pub(crate) fn existing_recents(list: &[PathBuf]) -> Vec<PathBuf> {
    list.iter().filter(|p| p.exists()).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_roundtrips() {
        // A populated config serialized and re-read yields the same values.
        let cfg = Config {
            window_size: Some((1280.0, 720.0)),
            recent_files: vec![PathBuf::from("/a/one.nef"), PathBuf::from("/b/two.cr2")],
            side_panel_width: Some(300.0),
            last_export_dir: Some(PathBuf::from("/exports")),
            last_open_dir: Some(PathBuf::from("/photos")),
            gpu: true,
            theme: Theme::Light,
            sections_open: BTreeMap::from([
                ("basic".to_owned(), true),
                ("color".to_owned(), false),
            ]),
        };
        let text = cfg.to_ron().expect("serialize");
        let back = Config::from_ron(&text).expect("parse");
        assert_eq!(cfg, back);
    }

    #[test]
    fn section_state_round_trips() {
        // A collapsed section persists through a serialize/reload, keyed by a
        // stable section key (not the display label). An older config missing the
        // field defaults to an empty map (each section then uses its own
        // default-open), proving the forward-compatible default.
        let mut cfg = Config::default();
        cfg.sections_open.insert("color".to_owned(), false);
        let text = cfg.to_ron().expect("serialize");
        let back = Config::from_ron(&text).expect("parse");
        assert_eq!(back.sections_open.get("color"), Some(&false));

        // A config that predates the field still loads, with no remembered state.
        let old = "(window_size: Some((800.0, 600.0)))";
        let loaded = Config::from_ron(old).expect("old config should load");
        assert!(loaded.sections_open.is_empty());
    }

    #[test]
    fn corrupt_config_falls_back_to_default() {
        // Garbage bytes (not valid RON) must recover to defaults, never panic or
        // abort startup.
        let dir = std::env::temp_dir().join("latent_config_corrupt_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.ron");
        std::fs::write(&path, b"\x00\x01 this is not ron {{{").unwrap();
        let cfg = Config::load_from(&path);
        assert_eq!(cfg, Config::default());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_config_loads_as_default() {
        // A path that does not exist is first-run: defaults, no error.
        let path = std::env::temp_dir().join("latent_config_missing_test_does_not_exist.ron");
        std::fs::remove_file(&path).ok();
        assert_eq!(Config::load_from(&path), Config::default());
    }

    #[test]
    fn old_config_missing_fields_still_loads() {
        // A config that predates newer fields (here, everything but the window
        // size) still loads — `#[serde(default)]` fills the rest.
        let partial = "(window_size: Some((800.0, 600.0)))";
        let cfg = Config::from_ron(partial).expect("partial config should load");
        assert_eq!(cfg.window_size, Some((800.0, 600.0)));
        // The omitted fields take their defaults.
        assert_eq!(cfg.recent_files, Vec::<PathBuf>::new());
        assert!(!cfg.gpu);
        assert_eq!(cfg.theme, Theme::Dark);
    }

    #[test]
    fn config_write_is_atomic_and_load_recovers() {
        // Writing goes through a `.tmp` then renames over the real file, and the
        // written config reads back identically.
        let dir = std::env::temp_dir().join("latent_config_atomic_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.ron");
        std::fs::remove_file(&path).ok();
        let tmp = path.with_extension("ron.tmp");
        std::fs::remove_file(&tmp).ok();

        let cfg = Config {
            recent_files: vec![PathBuf::from("/x/y.dng")],
            gpu: true,
            ..Config::default()
        };
        cfg.save_to(&path).expect("atomic write");
        // The temp file must not linger after a successful rename.
        assert!(!tmp.exists(), "temp file should be renamed away");
        assert!(path.exists(), "real config file should exist");
        let back = Config::load_from(&path);
        assert_eq!(cfg, back);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_to_unwritable_path_returns_error_not_panic() {
        // A write under a path whose parent cannot be created returns the error
        // (to be surfaced to the user), it does not panic. A NUL byte makes the
        // path unrepresentable on every OS.
        let bad = Path::new("/this/does/not/exist\0/config.ron");
        assert!(atomic_write(bad, "(gpu: false)").is_err());
    }

    #[test]
    fn push_recent_dedups_caps_and_orders() {
        // Pushing moves an existing entry to the front without duplicating, a new
        // entry lands at the front, and exceeding the cap drops the oldest.
        let cap = 3;
        let mut list = Vec::new();
        // Use paths that won't canonicalize (nonexistent) so they pass through
        // unchanged and the ordering logic is what's exercised.
        push_recent(&mut list, PathBuf::from("/r/a.nef"), cap);
        push_recent(&mut list, PathBuf::from("/r/b.nef"), cap);
        push_recent(&mut list, PathBuf::from("/r/c.nef"), cap);
        assert_eq!(
            list,
            vec![
                PathBuf::from("/r/c.nef"),
                PathBuf::from("/r/b.nef"),
                PathBuf::from("/r/a.nef"),
            ]
        );
        // Re-pushing an existing entry moves it to the front, no duplicate.
        push_recent(&mut list, PathBuf::from("/r/a.nef"), cap);
        assert_eq!(
            list,
            vec![
                PathBuf::from("/r/a.nef"),
                PathBuf::from("/r/c.nef"),
                PathBuf::from("/r/b.nef"),
            ]
        );
        // Exceeding the cap drops the oldest (b).
        push_recent(&mut list, PathBuf::from("/r/d.nef"), cap);
        assert_eq!(
            list,
            vec![
                PathBuf::from("/r/d.nef"),
                PathBuf::from("/r/a.nef"),
                PathBuf::from("/r/c.nef"),
            ]
        );
    }

    #[test]
    fn existing_recents_prunes_missing_paths() {
        // A real temp file is kept; a path that does not exist is pruned.
        let real = std::env::temp_dir().join("latent_recent_exists_test.nef");
        std::fs::write(&real, b"x").unwrap();
        let missing = std::env::temp_dir().join("latent_recent_missing_test_nope.nef");
        std::fs::remove_file(&missing).ok();

        let list = vec![real.clone(), missing.clone()];
        let pruned = existing_recents(&list);
        assert_eq!(pruned, vec![real.clone()]);

        std::fs::remove_file(&real).ok();
    }
}
