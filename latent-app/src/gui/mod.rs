//! The egui editor window: open a developed RAW, show it, and edit it live by
//! re-rendering the settings over a downscaled preview. The per-variant
//! [`latent_edit::History`] is the single source of truth — sliders read from
//! and write to the active variant's settings.

mod app;
mod canvas;
mod config;
mod dialogs;
mod icons;
mod panels;
mod state;
mod theme;
mod tools;
mod widgets;

pub use app::{BackendKind, run};

/// Load the persisted application config from the OS config dir (or defaults on a
/// missing/corrupt file). The composition root calls this before [`run`].
pub fn config_load() -> config::Config {
    config::Config::load()
}
