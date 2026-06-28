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

use latent_edit::Settings;
use serde::{Deserialize, Serialize};

/// How many recent files to remember. Newest-first, de-duplicated, capped here.
pub(crate) const RECENT_FILES_CAP: usize = 10;

/// A named develop preset: a reusable *look* stored in the app config (global to
/// the app, not per-image). It carries only the develop part of a [`Settings`] —
/// the global and local adjustments — with **geometry excluded**: a crop,
/// straighten, keystone, lens, or vignette is image-specific and would mis-apply
/// to a differently-framed image, so a preset never bakes it in. Applying a preset
/// leaves the target's geometry untouched (see the app's apply path).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preset {
    /// The user-chosen preset name (unique within the list; a save under an
    /// existing name overwrites it).
    pub(crate) name: String,
    /// The stored develop settings. Geometry is stripped on save, so this is
    /// always the identity geometry plus the develop adjustments; only the develop
    /// part is meaningful and only it is applied.
    pub(crate) settings: Settings,
}

impl Preset {
    /// Build a preset from a variant's current settings, **stripping geometry** so
    /// only the reusable develop look is stored.
    pub(crate) fn from_settings(name: String, settings: &Settings) -> Self {
        Self {
            name,
            settings: Settings {
                global: settings.global.clone(),
                locals: settings.locals.clone(),
                geometry: Default::default(),
            },
        }
    }
}

/// Insert `preset` into the list, replacing any existing preset of the same name
/// (a save under an existing name overwrites it) and otherwise appending. Pure
/// over the list so the dedup/overwrite is unit-testable.
pub(crate) fn upsert_preset(list: &mut Vec<Preset>, preset: Preset) {
    if let Some(slot) = list.iter_mut().find(|p| p.name == preset.name) {
        *slot = preset;
    } else {
        list.push(preset);
    }
}

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Used when solo (accordion) mode is **off**, where sections open and close
    /// independently. In solo mode the single open section is tracked by
    /// [`Config::open_section`] instead.
    pub(crate) sections_open: BTreeMap<String, bool>,
    /// Whether the controls panel runs in solo (accordion) mode: opening one
    /// section collapses the others, so at most one is open at a time. On by
    /// default. When off, sections open/close independently (the
    /// [`Config::sections_open`] map). Forward-compatible: an older config without
    /// it loads as solo-on.
    #[serde(default = "default_solo_sections")]
    pub(crate) solo_sections: bool,
    /// In solo mode, the stable key of the single currently-open section, or
    /// `None` when the user has collapsed them all. Keyed by the same stable
    /// section key as [`Config::sections_open`] (never the display label), so it
    /// stays an opaque string here. Ignored when solo mode is off. Defaults to the
    /// section open on first run (see [`default_open_section`]); a config that
    /// predates the field takes that same default, so a fresh and an older config
    /// agree on which section greets the user.
    #[serde(default = "default_open_section")]
    pub(crate) open_section: Option<String>,
    /// Shown/hidden state of the toggleable subsections (Cropping, Straighten,
    /// Sharpen, HSL mixer, …), keyed by a stable subsection id (not the display
    /// label). This is purely a UI show/hide of the subsection body and is
    /// independent of whether the effect is enabled. A missing key falls back to
    /// shown (the eye-open default).
    pub(crate) subsections_shown: BTreeMap<String, bool>,
    /// Named develop presets, global to the app and reusable across images. An old
    /// config with no presets loads with an empty list (`#[serde(default)]`).
    pub(crate) presets: Vec<Preset>,
    /// Whether the user has seen the first-run welcome hint (pointing at Open /
    /// drag-drop). Defaults to `false` so the hint shows on a fresh config and is
    /// set `true` once it is dismissed or the first file opens. Forward-compatible:
    /// an older config without it loads as "not yet seen".
    pub(crate) seen_welcome_hint: bool,
}

/// The default for [`Config::solo_sections`]: solo (accordion) mode is **on** by
/// default, so only one controls-panel section is open at a time. A standalone
/// function so the same default seeds both `Config::default()` and a config that
/// predates the field (the serde field-level default).
fn default_solo_sections() -> bool {
    true
}

/// The default for [`Config::open_section`]: the stable key of the section that
/// greets the user on first run under solo mode (the develop sections' default-
/// open one). A standalone function so a fresh config and a config that predates
/// the field agree. Once the user collapses everything this becomes `None`
/// (all-collapsed), which is preserved — only a never-set field takes this seed.
fn default_open_section() -> Option<String> {
    Some("light".to_owned())
}

impl Default for Config {
    /// The default config. Every field is its type's default except
    /// `solo_sections`, which defaults to **on** (see [`default_solo_sections`]),
    /// matching the serde missing-field default so a fresh config and an older
    /// config that predates the field agree.
    fn default() -> Self {
        Self {
            window_size: None,
            recent_files: Vec::new(),
            side_panel_width: None,
            last_export_dir: None,
            last_open_dir: None,
            gpu: false,
            theme: Theme::default(),
            sections_open: BTreeMap::new(),
            solo_sections: default_solo_sections(),
            open_section: default_open_section(),
            subsections_shown: BTreeMap::new(),
            presets: Vec::new(),
            seen_welcome_hint: false,
        }
    }
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
                ("light".to_owned(), true),
                ("color".to_owned(), false),
            ]),
            solo_sections: false,
            open_section: Some("detail".to_owned()),
            subsections_shown: BTreeMap::from([
                ("hsl_mixer".to_owned(), false),
                ("sharpen".to_owned(), true),
            ]),
            presets: vec![Preset::from_settings(
                "Test".to_owned(),
                &Settings::default(),
            )],
            seen_welcome_hint: true,
        };
        let text = cfg.to_ron().expect("serialize");
        let back = Config::from_ron(&text).expect("parse");
        assert_eq!(cfg, back);
    }

    #[test]
    fn preset_excludes_geometry_and_round_trips() {
        use latent_edit::{Adjustments, Crop, Geometry};
        // A variant with both develop adjustments and geometry; the preset keeps
        // only the develop part.
        let settings = Settings {
            global: Adjustments {
                exposure: Some(0.7),
                ..Adjustments::default()
            },
            geometry: Geometry {
                crop: Some(Crop {
                    x: 0.1,
                    y: 0.1,
                    width: 0.8,
                    height: 0.8,
                }),
                straighten_degrees: 4.0,
                ..Geometry::default()
            },
            ..Settings::default()
        };
        let preset = Preset::from_settings("Look".to_owned(), &settings);
        // The develop part is kept…
        assert_eq!(preset.settings.global.exposure, Some(0.7));
        // …but geometry is stripped to the identity (excluded from the preset).
        assert!(preset.settings.geometry.is_identity());

        // A config carrying the preset round-trips through RON unchanged.
        let cfg = Config {
            presets: vec![preset.clone()],
            ..Config::default()
        };
        let back = Config::from_ron(&cfg.to_ron().unwrap()).unwrap();
        assert_eq!(back.presets, vec![preset]);

        // An old config with no presets loads with an empty list.
        let old = "(window_size: Some((800.0, 600.0)))";
        let loaded = Config::from_ron(old).expect("old config should load");
        assert!(loaded.presets.is_empty());
    }

    #[test]
    fn upsert_preset_overwrites_same_name() {
        // Saving under an existing name replaces it; a new name appends.
        let mut list = Vec::new();
        upsert_preset(
            &mut list,
            Preset::from_settings("A".to_owned(), &Settings::default()),
        );
        upsert_preset(
            &mut list,
            Preset::from_settings("B".to_owned(), &Settings::default()),
        );
        assert_eq!(list.len(), 2);
        // Overwrite "A" with a different settings value.
        let mut tweaked = Settings::default();
        tweaked.global.exposure = Some(2.0);
        upsert_preset(&mut list, Preset::from_settings("A".to_owned(), &tweaked));
        assert_eq!(list.len(), 2, "same name does not duplicate");
        let a = list.iter().find(|p| p.name == "A").unwrap();
        assert_eq!(a.settings.global.exposure, Some(2.0));
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
    fn solo_pref_and_open_section_round_trip() {
        // The solo (accordion) preference and the tracked single open section
        // persist through a serialize/reload, keyed by a stable section key. An
        // explicit all-collapsed (`None`) round-trips too, distinct from the seed.
        for open in [Some("geometry".to_owned()), None] {
            let cfg = Config {
                solo_sections: false,
                open_section: open.clone(),
                ..Config::default()
            };
            let text = cfg.to_ron().expect("serialize");
            let back = Config::from_ron(&text).expect("parse");
            assert!(!back.solo_sections, "solo preference round-trips");
            assert_eq!(back.open_section, open, "open section round-trips");
        }

        // A config that predates these fields loads solo-on and is seeded to the
        // first-run section, so a fresh and an older config greet the user the same.
        let old = "(window_size: Some((800.0, 600.0)))";
        let loaded = Config::from_ron(old).expect("old config should load");
        assert!(loaded.solo_sections, "solo defaults on for an older config");
        assert_eq!(
            loaded.open_section.as_deref(),
            Some("light"),
            "older config takes the first-run open-section seed"
        );
        // And a fresh default agrees with the missing-field default.
        assert!(Config::default().solo_sections, "fresh default is solo-on");
        assert_eq!(Config::default().open_section.as_deref(), Some("light"));
    }

    #[test]
    fn subsection_visibility_round_trips() {
        // A hidden subsection persists through a serialize/reload, keyed by a stable
        // subsection id (not the display label). This is UI show/hide only, kept
        // independent of the subsection's enable state. An older config missing the
        // field defaults to an empty map (each subsection then shows by default),
        // proving the forward-compatible default.
        let mut cfg = Config::default();
        cfg.subsections_shown.insert("hsl_mixer".to_owned(), false);
        let text = cfg.to_ron().expect("serialize");
        let back = Config::from_ron(&text).expect("parse");
        assert_eq!(back.subsections_shown.get("hsl_mixer"), Some(&false));

        // A config that predates the field still loads, with no remembered state.
        let old = "(window_size: Some((800.0, 600.0)))";
        let loaded = Config::from_ron(old).expect("old config should load");
        assert!(loaded.subsections_shown.is_empty());
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
        // The first-run hint flag defaults to "not yet seen", so a config that
        // predates it shows the hint once on the next launch.
        assert!(!cfg.seen_welcome_hint);
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
