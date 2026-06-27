//! The egui editor window: open a developed RAW, show it, and edit it live by
//! re-rendering the settings over a downscaled preview. The per-variant
//! [`latent_edit::History`] is the single source of truth — sliders read from
//! and write to the active variant's settings.

mod app;
mod canvas;
mod icons;
mod panels;
mod state;
mod theme;
mod tools;
mod widgets;

pub use app::{BackendKind, run};
