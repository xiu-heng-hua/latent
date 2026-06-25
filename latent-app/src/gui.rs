//! The egui editor window: open a developed RAW, show it, and edit it live by
//! re-rendering the settings over a downscaled preview. The per-variant
//! [`History`] is the single source of truth — sliders read from and write to
//! the active variant's settings.

use std::error::Error;
use std::path::{Path, PathBuf};

use eframe::egui;
use latent_edit::{
    Adjustments, Brush, Clarity, ColorRange, Crop, Curves, Dab, Document, Gradient, History,
    LocalAdjustment, LuminanceRange, Mask, MaskShape, NoiseReduction, Radial, SelectiveTone,
    Settings, Sharpen, WhiteBalance,
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
                brush_radius: 0.08,
                brush_feather: 0.04,
                brush_erase: false,
                curve_channel: 0,
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
    /// Brush tool settings for painting dabs onto a brush mask (normalized).
    brush_radius: f32,
    brush_feather: f32,
    brush_erase: bool,
    /// Which curve channel the editor edits (0 = master, 1/2/3 = R/G/B).
    curve_channel: usize,
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
            dirty |= curves_block(ui, &mut self.variants[active], &mut self.curve_channel);

            ui.separator();
            ui.heading("Detail");
            dirty |= sharpen_block(ui, &mut self.variants[active]);
            dirty |= clarity_block(ui, &mut self.variants[active]);
            dirty |= opt_point_slider(
                ui,
                &mut self.variants[active],
                "Dehaze",
                0.0..=1.0,
                0.0,
                |s| s.global.dehaze,
                |s, v| s.global.dehaze = v,
            );
            dirty |= noise_reduction_block(ui, &mut self.variants[active]);

            ui.separator();
            ui.heading("Geometry");
            dirty |= straighten_slider(ui, &mut self.variants[active]);
            dirty |= crop_block(ui, &mut self.variants[active]);

            ui.separator();
            ui.heading("Local Adjustments");
            dirty |= local_adjustments(ui, &mut self.variants[active], &mut self.local_sel);
            // Brush tool: only when the selected local is a brush mask. Dabs are
            // painted on the image in the central panel using these settings.
            if self.variants[active]
                .current()
                .locals
                .get(self.local_sel)
                .is_some_and(|l| matches!(l.mask.shapes.first(), Some(MaskShape::Brush(_))))
            {
                ui.label("Brush");
                ui.add(egui::Slider::new(&mut self.brush_radius, 0.01..=0.5).text("Size"));
                ui.add(egui::Slider::new(&mut self.brush_feather, 0.0..=0.5).text("Feather"));
                ui.checkbox(&mut self.brush_erase, "Erase");
                ui.label("Drag on the image to paint.");
            }

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

        let tex_id = self.texture.as_ref().unwrap().id();
        let tex_size = self.texture.as_ref().unwrap().size_vec2();
        let mut painted = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| {
                let resp = ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(tex_id, tex_size))
                        .sense(egui::Sense::click_and_drag()),
                );
                // Paint brush dabs when the selected local is a brush mask, one
                // undo step per stroke (begin on press, commit on release).
                let is_brush = self.variants[active]
                    .current()
                    .locals
                    .get(self.local_sel)
                    .is_some_and(|l| matches!(l.mask.shapes.first(), Some(MaskShape::Brush(_))));
                if is_brush {
                    let click = resp.clicked() && !resp.dragged();
                    if resp.drag_started() || click {
                        self.variants[active].begin();
                    }
                    if (resp.dragged() || click)
                        && let Some(pos) = resp.hover_pos()
                    {
                        let r = resp.rect;
                        let nx = ((pos.x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
                        let ny = ((pos.y - r.top()) / r.height().max(1.0)).clamp(0.0, 1.0);
                        if let Some(MaskShape::Brush(b)) =
                            self.variants[active].current_mut().locals[self.local_sel]
                                .mask
                                .shapes
                                .first_mut()
                        {
                            b.dabs.push(Dab {
                                x: nx,
                                y: ny,
                                radius: self.brush_radius,
                                feather: self.brush_feather,
                                erase: self.brush_erase,
                            });
                            painted = true;
                        }
                    }
                    if resp.drag_stopped() || click {
                        self.variants[active].commit();
                    }
                }
            });
        });
        // A painted stroke changed the settings after this frame's render; refresh
        // the preview and repaint so the dab shows up.
        if painted {
            self.render_preview(ctx);
            ctx.request_repaint();
        }
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

/// Evenly-spaced input positions of the curve editor's five control points.
const CURVE_XS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// The control-point list for one channel of a [`Curves`] (0 = master,
/// 1/2/3 = red/green/blue).
fn curve_channel_mut(curves: &mut Curves, channel: usize) -> &mut Vec<(f32, f32)> {
    match channel {
        1 => &mut curves.red,
        2 => &mut curves.green,
        3 => &mut curves.blue,
        _ => &mut curves.master,
    }
}

/// Curve editor: enable curves, pick a channel, then drag the five control
/// points on the graph (the nearest point's output follows the cursor). Feeds
/// the [`Curves`] engine and re-renders live. The drag interaction is
/// display-unverifiable, so it carries no automated test.
fn curves_block(ui: &mut egui::Ui, history: &mut History<Settings>, channel: &mut usize) -> bool {
    let mut dirty = false;

    let mut enabled = history.current().global.curves.is_some();
    if ui.checkbox(&mut enabled, "Curves").changed() {
        history.begin();
        history.current_mut().global.curves = enabled.then(Curves::default);
        history.commit();
        dirty = true;
    }
    if history.current().global.curves.is_none() {
        return dirty;
    }

    ui.horizontal(|ui| {
        for (i, name) in ["Master", "R", "G", "B"].into_iter().enumerate() {
            ui.selectable_value(channel, i, name);
        }
    });

    // Output (y) of each fixed-input point for the selected channel; identity
    // where a point has not been set yet.
    let mut ys: [f32; 5] = {
        let curves = history.current().global.curves.as_ref().unwrap();
        let pts = match *channel {
            1 => &curves.red,
            2 => &curves.green,
            3 => &curves.blue,
            _ => &curves.master,
        };
        std::array::from_fn(|i| {
            pts.iter()
                .find(|(x, _)| (x - CURVE_XS[i]).abs() < 1e-3)
                .map_or(CURVE_XS[i], |&(_, y)| y)
        })
    };

    let size = egui::vec2(ui.available_width().min(220.0), 160.0);
    let (resp, painter) = ui.allocate_painter(size, egui::Sense::click_and_drag());
    let rect = resp.rect;
    let sx = |x: f32| rect.left() + x * rect.width();
    let sy = |y: f32| rect.bottom() - y.clamp(0.0, 1.0) * rect.height();

    if resp.drag_started() {
        history.begin();
    }
    if resp.dragged()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let nx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let ny = ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0);
        let i = (0..5)
            .min_by(|&a, &b| {
                (CURVE_XS[a] - nx)
                    .abs()
                    .total_cmp(&(CURVE_XS[b] - nx).abs())
            })
            .unwrap();
        ys[i] = ny;
        let curves = history.current_mut().global.curves.as_mut().unwrap();
        *curve_channel_mut(curves, *channel) =
            CURVE_XS.iter().zip(ys).map(|(&x, y)| (x, y)).collect();
        dirty = true;
    }
    if resp.drag_stopped() {
        history.commit();
    }

    // Reference diagonal, then the curve and its control points.
    painter.line_segment(
        [egui::pos2(sx(0.0), sy(0.0)), egui::pos2(sx(1.0), sy(1.0))],
        egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
    );
    let pts: Vec<egui::Pos2> = CURVE_XS
        .iter()
        .zip(ys)
        .map(|(&x, y)| egui::pos2(sx(x), sy(y)))
        .collect();
    for w in pts.windows(2) {
        painter.line_segment(
            [w[0], w[1]],
            egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE),
        );
    }
    for p in &pts {
        painter.circle_filled(*p, 3.0, egui::Color32::WHITE);
    }

    dirty
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

/// Clarity: midtone local-contrast amount/radius sliders editing one adjustment.
fn clarity_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let c = history.current().global.clarity.unwrap_or_default();
    let (mut amount, mut radius) = (c.amount, c.radius);
    let ra = ui.add(egui::Slider::new(&mut amount, -1.0..=1.0).text("Clarity amount"));
    let rr = ui.add(egui::Slider::new(&mut radius, 5.0..=100.0).text("Clarity radius"));
    let (begin, commit, changed) = gesture(&[&ra, &rr]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().global.clarity = Some(Clarity { amount, radius });
    }
    if commit {
        history.commit();
    }
    changed
}

/// Noise reduction: independent luminance/color strengths plus a radius.
fn noise_reduction_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let nr = history.current().global.noise_reduction.unwrap_or_default();
    let (mut luminance, mut color, mut radius) = (nr.luminance, nr.color, nr.radius);
    let rl = ui.add(egui::Slider::new(&mut luminance, 0.0..=0.3).text("Luminance NR"));
    let rc = ui.add(egui::Slider::new(&mut color, 0.0..=0.3).text("Color NR"));
    let rr = ui.add(egui::Slider::new(&mut radius, 1.0..=10.0).text("NR radius"));
    let (begin, commit, changed) = gesture(&[&rl, &rc, &rr]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().global.noise_reduction = Some(NoiseReduction {
            radius,
            luminance,
            color,
        });
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
                    ops: Vec::new(),
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
                    ops: Vec::new(),
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            });
            history.commit();
            *sel = history.current().locals.len() - 1;
            dirty = true;
        }
        if ui.button("+ Luminosity").clicked() {
            history.begin();
            history.current_mut().locals.push(LocalAdjustment {
                mask: Mask {
                    // Defaults to the shadows; drag the range to retarget.
                    shapes: vec![MaskShape::Luminosity(LuminanceRange {
                        lo: 0.0,
                        hi: 0.3,
                        feather: 0.1,
                    })],
                    ops: Vec::new(),
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            });
            history.commit();
            *sel = history.current().locals.len() - 1;
            dirty = true;
        }
        if ui.button("+ Color").clicked() {
            history.begin();
            history.current_mut().locals.push(LocalAdjustment {
                mask: Mask {
                    // Defaults to reds; drag the hue to retarget.
                    shapes: vec![MaskShape::ColorRange(ColorRange {
                        hue: 0.0,
                        hue_width: 0.08,
                        sat_min: 0.15,
                        feather: 0.08,
                    })],
                    ops: Vec::new(),
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            });
            history.commit();
            *sel = history.current().locals.len() - 1;
            dirty = true;
        }
        if ui.button("+ Brush").clicked() {
            history.begin();
            history.current_mut().locals.push(LocalAdjustment {
                mask: Mask {
                    shapes: vec![MaskShape::Brush(Brush::default())],
                    ops: Vec::new(),
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
    match history.current().locals[i].mask.shapes.first().cloned() {
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
        Some(MaskShape::Luminosity(l)) => {
            let (mut lo, mut hi, mut feather) = (l.lo, l.hi, l.feather);
            let r0 = ui.add(egui::Slider::new(&mut lo, 0.0..=1.0).text("Range low"));
            let r1 = ui.add(egui::Slider::new(&mut hi, 0.0..=1.0).text("Range high"));
            let r2 = ui.add(egui::Slider::new(&mut feather, 0.0..=0.5).text("Feather"));
            let (begin, commit, changed) = gesture(&[&r0, &r1, &r2]);
            if begin {
                history.begin();
            }
            if changed {
                history.current_mut().locals[i].mask.shapes[0] =
                    MaskShape::Luminosity(LuminanceRange { lo, hi, feather });
            }
            if commit {
                history.commit();
            }
            changed
        }
        Some(MaskShape::ColorRange(c)) => {
            let (mut hue, mut hue_width, mut sat_min, mut feather) =
                (c.hue, c.hue_width, c.sat_min, c.feather);
            let r0 = ui.add(egui::Slider::new(&mut hue, 0.0..=1.0).text("Hue"));
            let r1 = ui.add(egui::Slider::new(&mut hue_width, 0.0..=0.5).text("Hue width"));
            let r2 = ui.add(egui::Slider::new(&mut sat_min, 0.0..=1.0).text("Min saturation"));
            let r3 = ui.add(egui::Slider::new(&mut feather, 0.0..=0.5).text("Feather"));
            let (begin, commit, changed) = gesture(&[&r0, &r1, &r2, &r3]);
            if begin {
                history.begin();
            }
            if changed {
                history.current_mut().locals[i].mask.shapes[0] =
                    MaskShape::ColorRange(ColorRange {
                        hue,
                        hue_width,
                        sat_min,
                        feather,
                    });
            }
            if commit {
                history.commit();
            }
            changed
        }
        Some(MaskShape::Brush(_)) => {
            ui.label("Brush mask — paint on the preview to add, Erase to subtract.");
            false
        }
        None => false,
    }
}

/// Convert a linear working-RGB image to a gamma-encoded egui texture, using the
/// exact output transform export uses ([`latent_export::to_srgb8`] — working→sRGB
/// matrix, highlight rolloff, sRGB OETF) so the preview matches the saved file.
fn to_color_image(img: &ImageBuf) -> egui::ColorImage {
    let bytes = latent_export::to_srgb8(img);
    egui::ColorImage::from_rgb([img.width() as usize, img.height() as usize], &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_color_image_matches_the_export_transform() {
        // The preview must go through the same output transform as a saved file
        // (working→sRGB matrix + highlight rolloff + sRGB OETF). Neutrals stay
        // neutral, so the values match the export tests: 0.5 → 188, and display
        // white 1.0 rolls off to 254 (not a bare 255).
        let mut img = ImageBuf::new(3, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]); // black
        img.set(1, 0, [0.5, 0.5, 0.5]); // mid-gray (below the knee, faithful)
        img.set(2, 0, [1.0, 1.0, 1.0]); // display white (rolled off)

        let ci = to_color_image(&img);
        assert_eq!(ci.size, [3, 1]);
        assert_eq!(ci.pixels[0], egui::Color32::from_rgb(0, 0, 0));
        assert_eq!(ci.pixels[1], egui::Color32::from_rgb(188, 188, 188));
        assert_eq!(ci.pixels[2], egui::Color32::from_rgb(254, 254, 254));
    }
}
