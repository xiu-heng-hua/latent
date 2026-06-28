//! The develop-presets block: save the active variant's current develop look as a
//! named preset, and apply or delete a saved one. Presets live in the app config
//! (global to the app, reusable across images), store only the develop part —
//! **geometry is excluded** so a saved look never mis-applies a crop or keystone —
//! and apply as a single undo step that re-renders. The geometry exclusion is
//! stated here in the UI and in the config's `Preset` doc.

use eframe::egui;

use crate::gui::app::App;

/// Show the presets block. Returns whether applying a preset marked the preview
/// dirty (so the caller re-renders).
pub(crate) fn show(app: &mut App, ui: &mut egui::Ui) -> bool {
    let mut dirty = false;
    let has_session = app.session.is_some();

    // Save row: a name field and a Save button (writes to the app config).
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.preset_name_input)
                .hint_text("Preset name")
                .desired_width(140.0),
        );
        let can_save = has_session && !app.preset_name_input.trim().is_empty();
        if ui
            .add_enabled(can_save, egui::Button::new("Save"))
            .on_hover_text("Save the current develop look (geometry excluded)")
            .clicked()
        {
            let name = std::mem::take(&mut app.preset_name_input);
            app.save_preset(name);
        }
    });

    // Apply / delete rows, one per saved preset. Collect actions, then apply after
    // the loop so the config borrow is released first.
    let mut to_apply: Option<usize> = None;
    let mut to_delete: Option<usize> = None;
    if app.config.presets.is_empty() {
        ui.label(egui::RichText::new("No presets saved").weak());
    } else {
        for (i, preset) in app.config.presets.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_session, egui::Button::new(&preset.name))
                    .on_hover_text("Apply this develop look (keeps the image's geometry)")
                    .clicked()
                {
                    to_apply = Some(i);
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("Delete preset")
                    .clicked()
                {
                    to_delete = Some(i);
                }
            });
        }
    }

    if let Some(i) = to_apply
        && let Some(preset) = app.config.presets.get(i).cloned()
    {
        dirty |= app.apply_preset(&preset);
    }
    if let Some(i) = to_delete
        && i < app.config.presets.len()
    {
        app.config.presets.remove(i);
        app.save_config();
    }

    dirty
}
