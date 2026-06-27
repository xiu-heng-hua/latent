//! Reusable slider machinery and the per-block control builders the controls
//! panel composes. Each builder is data-bound to the active variant's history:
//! it reads the current value, renders one or more sliders, and folds the
//! responses into a single begin/commit history transaction per gesture.

use eframe::egui;
use latent_edit::{
    Adjustments, Brush, Clarity, ColorRange, Crop, Curves, Gradient, History, LocalAdjustment,
    LuminanceRange, Mask, MaskShape, NoiseReduction, Perspective, Radial, SelectiveTone, Settings,
    Sharpen, WhiteBalance,
};

use super::state::clamp_selection;
use super::theme;

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
pub(crate) fn opt_point_slider(
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
pub(crate) fn curves_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    channel: &mut usize,
) -> bool {
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

    let size = egui::vec2(
        ui.available_width().min(theme::CURVE_EDITOR_SIZE.x),
        theme::CURVE_EDITOR_SIZE.y,
    );
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
        painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, theme::ACCENT));
    }
    for p in &pts {
        painter.circle_filled(*p, 3.0, egui::Color32::WHITE);
    }

    dirty
}

/// White balance: two sliders (temp/tint) editing one optional adjustment.
pub(crate) fn white_balance_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
pub(crate) fn tone_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
pub(crate) fn sharpen_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
pub(crate) fn clarity_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
pub(crate) fn noise_reduction_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
pub(crate) fn straighten_slider(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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

/// Creative vignette applied after the crop: negative darkens the corners,
/// positive lightens them. Zero clears it (back to `None`).
pub(crate) fn vignette_slider(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let mut amount = history.current().geometry.vignette.unwrap_or(0.0);
    let r = ui.add(egui::Slider::new(&mut amount, -1.0..=1.0).text("Vignette"));
    let (begin, commit, changed) = gesture(&[&r]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().geometry.vignette = (amount != 0.0).then_some(amount);
    }
    if commit {
        history.commit();
    }
    changed
}

/// Keystone: two sliders correcting converging verticals and horizontals.
/// Both at zero clears the correction (back to `None`).
pub(crate) fn keystone_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let p = history
        .current()
        .geometry
        .perspective
        .unwrap_or(Perspective {
            vertical: 0.0,
            horizontal: 0.0,
        });
    let (mut v, mut h) = (p.vertical, p.horizontal);
    let rv = ui.add(egui::Slider::new(&mut v, -0.8..=0.8).text("Vertical"));
    let rh = ui.add(egui::Slider::new(&mut h, -0.8..=0.8).text("Horizontal"));
    let (begin, commit, changed) = gesture(&[&rv, &rh]);
    if begin {
        history.begin();
    }
    if changed {
        history.current_mut().geometry.perspective =
            (v != 0.0 || h != 0.0).then_some(Perspective {
                vertical: v,
                horizontal: h,
            });
    }
    if commit {
        history.commit();
    }
    changed
}

/// Crop: four sliders for a normalized rectangle, editing one optional crop.
/// The full frame `{0, 0, 1, 1}` is shown when there is no crop.
pub(crate) fn crop_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
pub(crate) fn value_slider(
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
pub(crate) fn local_adjustments(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    sel: &mut usize,
) -> bool {
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
    clamp_selection(sel, history.current().locals.len());

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
    clamp_selection(sel, history.current().locals.len());
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
