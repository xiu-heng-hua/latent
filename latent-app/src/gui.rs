//! The egui editor window: open a developed RAW, show it, and edit it live by
//! re-rendering the settings over a downscaled preview. The per-variant
//! [`History`] is the single source of truth — sliders read from and write to
//! the active variant's settings.

use std::error::Error;
use std::path::{Path, PathBuf};

use eframe::egui;
use latent_edit::{
    Adjustments, Crop, Document, Gradient, History, LocalAdjustment, Mask, MaskShape, Radial,
    SelectiveTone, Settings, Sharpen, WhiteBalance,
};
use latent_image::ImageBuf;
use latent_pipeline::{Backend, render};

/// Longest side of the interactive preview, in pixels. Keeps re-render cheap
/// during editing; export uses the full-resolution image.
const PREVIEW_MAX_DIM: u32 = 1600;

/// Develop `input` and open the editor window, rendering with `backend`.
pub fn run(input: &Path, backend: Box<dyn Backend>) -> Result<(), Box<dyn Error>> {
    // Develop once at full res; the preview re-renders over a downscaled copy.
    let full = crate::develop_to_image(input)?;
    let preview = full.downscaled(PREVIEW_MAX_DIM);
    let title = format!("{}  ({}x{})", input.display(), full.width(), full.height());
    let output = input.with_extension("jpg").to_string_lossy().into_owned();

    // Reload edits from the sidecar (photo.nef → photo.ron) if present.
    let sidecar = input.with_extension("ron");
    let mut document = std::fs::read_to_string(&sidecar)
        .ok()
        .and_then(|text| Document::from_ron(&text).ok())
        .unwrap_or_default();
    if document.variants.is_empty() {
        document.variants.push(Settings::default());
    }
    let saved = document.variants.clone();
    let variants = document.variants.into_iter().map(History::new).collect();

    eframe::run_native(
        "latent",
        eframe::NativeOptions::default(),
        Box::new(move |_cc| {
            Ok(Box::new(App {
                full,
                preview,
                variants,
                active: 0,
                sidecar,
                saved,
                title,
                output,
                status: String::new(),
                texture: None,
                local_sel: 0,
                backend,
            }) as Box<dyn eframe::App>)
        }),
    )
    .map_err(|e| format!("could not start the editor window: {e}"))?;
    Ok(())
}

struct App {
    /// Full-resolution working base, rendered over for export.
    full: ImageBuf,
    /// Downscaled working base, rendered over for the live preview.
    preview: ImageBuf,
    /// One independent edit history per variant; never empty.
    variants: Vec<History<Settings>>,
    /// Index of the variant currently being edited and previewed.
    active: usize,
    /// Sidecar path (`<raw>.ron`) the document auto-saves to.
    sidecar: PathBuf,
    /// Last variants written to the sidecar, to avoid redundant writes.
    saved: Vec<Settings>,
    title: String,
    /// Export destination path (editable in the UI).
    output: String,
    status: String,
    texture: Option<egui::TextureHandle>,
    /// Index of the local adjustment selected for editing in the panel.
    local_sel: usize,
    /// The rendering backend (CPU, or GPU when selected and available).
    backend: Box<dyn Backend>,
}

impl App {
    /// The history of the variant currently being edited.
    fn active_history(&mut self) -> &mut History<Settings> {
        &mut self.variants[self.active]
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

    /// Re-render the active variant over the preview base and refresh the texture.
    fn render_preview(&mut self, ctx: &egui::Context) {
        let rendered = render(
            &self.preview,
            self.variants[self.active].current(),
            self.backend.as_ref(),
        );
        let color = to_color_image(&rendered);
        match &mut self.texture {
            Some(tex) => tex.set(color, egui::TextureOptions::default()),
            None => {
                self.texture =
                    Some(ctx.load_texture("preview", color, egui::TextureOptions::default()));
            }
        }
    }

    /// Render the active variant at full resolution and write it to `self.output`.
    fn export(&mut self) {
        let rendered = render(
            &self.full,
            self.variants[self.active].current(),
            self.backend.as_ref(),
        );
        self.status = match latent_export::save(&rendered, Path::new(&self.output)) {
            Ok(()) => format!("Exported to {}", self.output),
            Err(e) => format!("Export failed: {e}"),
        };
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut dirty = self.texture.is_none();

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

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.title);
                do_undo |= ui
                    .add_enabled(
                        self.variants[self.active].can_undo(),
                        egui::Button::new("Undo"),
                    )
                    .clicked();
                do_redo |= ui
                    .add_enabled(
                        self.variants[self.active].can_redo(),
                        egui::Button::new("Redo"),
                    )
                    .clicked();
            });
            ui.horizontal(|ui| {
                ui.label("Variant:");
                for i in 0..self.variants.len() {
                    if ui
                        .selectable_label(i == self.active, format!("{}", i + 1))
                        .clicked()
                    {
                        self.active = i;
                        dirty = true;
                    }
                }
                if ui.button("+").on_hover_text("New variant (copy)").clicked() {
                    let copy = self.variants[self.active].current().clone();
                    self.variants.push(History::new(copy));
                    self.active = self.variants.len() - 1;
                    dirty = true;
                }
            });
        });

        let active = self.active;
        egui::SidePanel::right("controls").show(ctx, |ui| {
            ui.heading("Light");
            dirty |= opt_point_slider(
                ui,
                &mut self.variants[active],
                "Exposure (EV)",
                -5.0..=5.0,
                0.0,
                |s| s.global.exposure,
                |s, v| s.global.exposure = v,
            );
            dirty |= tone_block(ui, &mut self.variants[active]);

            ui.separator();
            ui.heading("Color");
            dirty |= white_balance_block(ui, &mut self.variants[active]);
            dirty |= opt_point_slider(
                ui,
                &mut self.variants[active],
                "Saturation",
                0.0..=2.0,
                1.0,
                |s| s.global.saturation,
                |s, v| s.global.saturation = v,
            );

            ui.separator();
            ui.heading("Detail");
            dirty |= sharpen_block(ui, &mut self.variants[active]);

            ui.separator();
            ui.heading("Geometry");
            dirty |= straighten_slider(ui, &mut self.variants[active]);
            dirty |= crop_block(ui, &mut self.variants[active]);

            ui.separator();
            ui.heading("Local Adjustments");
            dirty |= local_adjustments(ui, &mut self.variants[active], &mut self.local_sel);

            ui.separator();
            ui.heading("Export");
            ui.horizontal(|ui| {
                ui.label("Path:");
                ui.text_edit_singleline(&mut self.output);
            });
            if ui.button("Export (full resolution)").clicked() {
                self.export();
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });

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

        let texture = self.texture.as_ref().unwrap();
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                ui.image(egui::load::SizedTexture::new(
                    texture.id(),
                    texture.size_vec2(),
                ));
            });
        });
    }
}

/// Whether a set of slider responses begins / commits an edit gesture, and
/// whether any value changed. One transaction per gesture: begin on the first
/// drag (or a discrete click-set), commit when it ends. `commit` only records
/// an undo step if the value actually changed.
fn gesture(responses: &[&egui::Response]) -> (bool, bool, bool) {
    let started = responses.iter().any(|r| r.drag_started());
    let stopped = responses.iter().any(|r| r.drag_stopped());
    let changed = responses.iter().any(|r| r.changed());
    let dragged = responses.iter().any(|r| r.dragged());
    let discrete = changed && !dragged && !started && !stopped;
    (started || discrete, stopped || discrete, changed)
}

/// A slider bound to an optional point adjustment. The slider shows `neutral`
/// when the field is `None`; any change sets it to `Some(value)`.
fn opt_point_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    label: &str,
    range: std::ops::RangeInclusive<f32>,
    neutral: f32,
    get: impl Fn(&Settings) -> Option<f32>,
    set: impl Fn(&mut Settings, Option<f32>),
) -> bool {
    let mut value = get(history.current()).unwrap_or(neutral);
    let r = ui.add(egui::Slider::new(&mut value, range).text(label));
    let (begin, commit, changed) = gesture(&[&r]);
    if begin {
        history.begin();
    }
    if changed {
        set(history.current_mut(), Some(value));
    }
    if commit {
        history.commit();
    }
    changed
}

/// White balance: two sliders (temp/tint) editing one optional adjustment.
fn white_balance_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let wb = history.current().global.white_balance.unwrap_or_default();
    let (mut temp, mut tint) = (wb.temp, wb.tint);
    let rt = ui.add(egui::Slider::new(&mut temp, -1.0..=1.0).text("Temp"));
    let ru = ui.add(egui::Slider::new(&mut tint, -1.0..=1.0).text("Tint"));
    let (begin, commit, changed) = gesture(&[&rt, &ru]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().global.white_balance = Some(WhiteBalance { temp, tint });
    }
    if commit {
        history.commit();
    }
    changed
}

/// Selective tone: four sliders editing one optional adjustment.
fn tone_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let t = history.current().global.tone.unwrap_or_default();
    let (mut contrast, mut highlights, mut shadows, mut blacks) =
        (t.contrast, t.highlights, t.shadows, t.blacks);
    let rc = ui.add(egui::Slider::new(&mut contrast, -1.0..=1.0).text("Contrast"));
    let rh = ui.add(egui::Slider::new(&mut highlights, -1.0..=1.0).text("Highlights"));
    let rs = ui.add(egui::Slider::new(&mut shadows, -1.0..=1.0).text("Shadows"));
    let rb = ui.add(egui::Slider::new(&mut blacks, -1.0..=1.0).text("Blacks"));
    let (begin, commit, changed) = gesture(&[&rc, &rh, &rs, &rb]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().global.tone = Some(SelectiveTone {
            contrast,
            highlights,
            shadows,
            blacks,
        });
    }
    if commit {
        history.commit();
    }
    changed
}

/// Sharpening: amount/radius sliders editing one optional adjustment.
fn sharpen_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let s = history.current().global.sharpen.unwrap_or_default();
    let (mut amount, mut radius) = (s.amount, s.radius);
    let ra = ui.add(egui::Slider::new(&mut amount, 0.0..=2.0).text("Sharpen amount"));
    let rr = ui.add(egui::Slider::new(&mut radius, 1.0..=10.0).text("Sharpen radius"));
    let (begin, commit, changed) = gesture(&[&ra, &rr]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().global.sharpen = Some(Sharpen { amount, radius });
    }
    if commit {
        history.commit();
    }
    changed
}

/// Straighten angle (degrees), applied before the crop.
fn straighten_slider(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let mut angle = history.current().geometry.straighten_degrees;
    let r = ui.add(egui::Slider::new(&mut angle, -45.0..=45.0).text("Angle (°)"));
    let (begin, commit, changed) = gesture(&[&r]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().geometry.straighten_degrees = angle;
    }
    if commit {
        history.commit();
    }
    changed
}

/// Crop: four sliders for a normalized rectangle, editing one optional crop.
/// The full frame `{0, 0, 1, 1}` is shown when there is no crop.
fn crop_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let c = history.current().geometry.crop.unwrap_or(Crop {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    let (mut x, mut y, mut w, mut h) = (c.x, c.y, c.width, c.height);
    let rx = ui.add(egui::Slider::new(&mut x, 0.0..=1.0).text("Left"));
    let ry = ui.add(egui::Slider::new(&mut y, 0.0..=1.0).text("Top"));
    let rw = ui.add(egui::Slider::new(&mut w, 0.0..=1.0).text("Width"));
    let rh = ui.add(egui::Slider::new(&mut h, 0.0..=1.0).text("Height"));
    let (begin, commit, changed) = gesture(&[&rx, &ry, &rw, &rh]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().geometry.crop = Some(Crop {
            x,
            y,
            width: w,
            height: h,
        });
    }
    if commit {
        history.commit();
    }
    changed
}

/// A slider bound to a plain `f32` field of the active settings (begin/commit
/// as one gesture, like [`opt_point_slider`] but for a non-optional value).
fn value_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    label: &str,
    range: std::ops::RangeInclusive<f32>,
    get: impl Fn(&Settings) -> f32,
    set: impl Fn(&mut Settings, f32),
) -> bool {
    let mut value = get(history.current());
    let r = ui.add(egui::Slider::new(&mut value, range).text(label));
    let (begin, commit, changed) = gesture(&[&r]);
    if begin {
        history.begin();
    }
    if changed {
        set(history.current_mut(), value);
    }
    if commit {
        history.commit();
    }
    changed
}

/// The Local Adjustments panel: add/select/delete masked adjustments and edit
/// the selected one. `sel` is the selected index (UI state). Returns whether
/// the preview needs a redraw.
fn local_adjustments(ui: &mut egui::Ui, history: &mut History<Settings>, sel: &mut usize) -> bool {
    let mut dirty = false;

    ui.horizontal(|ui| {
        if ui.button("+ Graduated").clicked() {
            history.begin();
            history.current_mut().locals.push(LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Gradient(Gradient {
                        x0: 0.5,
                        y0: 0.0,
                        x1: 0.5,
                        y1: 1.0,
                    })],
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            });
            history.commit();
            *sel = history.current().locals.len() - 1;
            dirty = true;
        }
        if ui.button("+ Radial").clicked() {
            history.begin();
            history.current_mut().locals.push(LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Radial(Radial {
                        cx: 0.5,
                        cy: 0.5,
                        radius: 0.25,
                        feather: 0.25,
                    })],
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            });
            history.commit();
            *sel = history.current().locals.len() - 1;
            dirty = true;
        }
    });

    if history.current().locals.is_empty() {
        ui.label("(none)");
        return dirty;
    }
    *sel = (*sel).min(history.current().locals.len() - 1);

    ui.horizontal(|ui| {
        let count = history.current().locals.len();
        for i in 0..count {
            if ui
                .selectable_label(i == *sel, format!("{}", i + 1))
                .clicked()
            {
                *sel = i;
            }
        }
        if ui.button("Delete").clicked() {
            history.begin();
            history.current_mut().locals.remove(*sel);
            history.commit();
            dirty = true;
        }
    });

    if history.current().locals.is_empty() {
        return dirty;
    }
    *sel = (*sel).min(history.current().locals.len() - 1);
    let i = *sel;

    dirty |= local_shape_block(ui, history, i);

    let mut invert = history.current().locals[i].mask.invert;
    if ui.checkbox(&mut invert, "Invert mask").changed() {
        history.begin();
        history.current_mut().locals[i].mask.invert = invert;
        history.commit();
        dirty = true;
    }

    dirty |= value_slider(
        ui,
        history,
        "Opacity",
        0.0..=1.0,
        move |s| s.locals[i].opacity,
        move |s, v| s.locals[i].opacity = v,
    );
    dirty |= opt_point_slider(
        ui,
        history,
        "Exposure (EV)",
        -5.0..=5.0,
        0.0,
        move |s| s.locals[i].adjustments.exposure,
        move |s, v| s.locals[i].adjustments.exposure = v,
    );
    dirty |= opt_point_slider(
        ui,
        history,
        "Saturation",
        0.0..=2.0,
        1.0,
        move |s| s.locals[i].adjustments.saturation,
        move |s, v| s.locals[i].adjustments.saturation = v,
    );

    dirty
}

/// Sliders for the selected local adjustment's first mask shape (gradient
/// endpoints or radial center/radius/feather), in normalized coordinates.
fn local_shape_block(ui: &mut egui::Ui, history: &mut History<Settings>, i: usize) -> bool {
    match history.current().locals[i].mask.shapes.first().copied() {
        Some(MaskShape::Gradient(g)) => {
            let (mut x0, mut y0, mut x1, mut y1) = (g.x0, g.y0, g.x1, g.y1);
            let r0 = ui.add(egui::Slider::new(&mut x0, 0.0..=1.0).text("From X"));
            let r1 = ui.add(egui::Slider::new(&mut y0, 0.0..=1.0).text("From Y"));
            let r2 = ui.add(egui::Slider::new(&mut x1, 0.0..=1.0).text("To X"));
            let r3 = ui.add(egui::Slider::new(&mut y1, 0.0..=1.0).text("To Y"));
            let (begin, commit, changed) = gesture(&[&r0, &r1, &r2, &r3]);
            if begin {
                history.begin();
            }
            if changed {
                history.current_mut().locals[i].mask.shapes[0] =
                    MaskShape::Gradient(Gradient { x0, y0, x1, y1 });
            }
            if commit {
                history.commit();
            }
            changed
        }
        Some(MaskShape::Radial(r)) => {
            let (mut cx, mut cy, mut radius, mut feather) = (r.cx, r.cy, r.radius, r.feather);
            let r0 = ui.add(egui::Slider::new(&mut cx, 0.0..=1.0).text("Center X"));
            let r1 = ui.add(egui::Slider::new(&mut cy, 0.0..=1.0).text("Center Y"));
            let r2 = ui.add(egui::Slider::new(&mut radius, 0.0..=1.0).text("Radius"));
            let r3 = ui.add(egui::Slider::new(&mut feather, 0.0..=1.0).text("Feather"));
            let (begin, commit, changed) = gesture(&[&r0, &r1, &r2, &r3]);
            if begin {
                history.begin();
            }
            if changed {
                history.current_mut().locals[i].mask.shapes[0] = MaskShape::Radial(Radial {
                    cx,
                    cy,
                    radius,
                    feather,
                });
            }
            if commit {
                history.commit();
            }
            changed
        }
        None => false,
    }
}

/// Convert a linear working-RGB image to a gamma-encoded egui texture, using the
/// same sRGB encoding as export so the preview matches the saved file.
fn to_color_image(img: &ImageBuf) -> egui::ColorImage {
    let mut bytes = Vec::with_capacity(img.len() * 3);
    for y in 0..img.height() {
        for x in 0..img.width() {
            for v in img.get(x, y) {
                bytes.push((latent_export::srgb_encode(v).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            }
        }
    }
    egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_color_image_gamma_encodes_pixels() {
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]);
        img.set(1, 0, [1.0, 1.0, 1.0]);

        let ci = to_color_image(&img);
        assert_eq!(ci.size, [2, 1]);
        assert_eq!(ci.pixels[0], egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(ci.pixels[1], egui::Color32::from_rgb(255, 255, 255));
    }
}
