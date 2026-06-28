//! Reusable slider machinery and the per-block control builders the controls
//! panel composes. Each builder is data-bound to the active variant's history:
//! it reads the current value, renders one or more sliders, and folds the
//! responses into a single begin/commit history transaction per gesture.
//!
//! The shared input control is [`adjust_slider`] / [`opt_adjust_slider`]: a
//! slider with an optional numeric entry, double-click-to-reset, a neutral-value
//! marker, and a concise hover tooltip. Both flavors preserve the begin/commit
//! gesture-to-history contract exactly — one drag (or one discrete click-set, or
//! one double-click reset, or one typed entry) is a single undo step, and a
//! net-zero gesture records nothing — by routing every interaction through the
//! same [`GestureScope`] derivation and the same begin-once/commit-once/
//! write-on-change dance. Multi-field blocks share one [`GestureScope`] across
//! all their sliders, so a block commits as a single transaction.

use eframe::egui;
use latent_edit::{
    Adjustments, Brush, ColorRange, Crop, Curves, Gradient, History, LocalAdjustment,
    LuminanceRange, Mask, MaskShape, Perspective, Radial, SelectiveTone, Settings, WhiteBalance,
};

use super::state::clamp_selection;
use super::theme;

/// Accumulates the [`egui::Response`]s of several sliders so a block of them
/// commits as **one** history transaction. Each [`adjust_slider`] in the block
/// records its response here and writes its own field on change; the block then
/// calls [`GestureScope::finish`] once to derive a single begin/commit over every
/// response — exactly reproducing the hand-rolled `gesture(&[&r0, &r1, …])` the
/// multi-field blocks used before. A single-field control uses the same path with
/// one response in the scope.
#[derive(Default)]
struct GestureScope {
    started: bool,
    stopped: bool,
    changed: bool,
    dragged: bool,
}

impl GestureScope {
    /// Fold one slider response into the running gesture.
    fn record(&mut self, r: &egui::Response) {
        self.started |= r.drag_started();
        self.stopped |= r.drag_stopped();
        self.changed |= r.changed();
        self.dragged |= r.dragged();
    }

    /// Mark a one-shot edit (a double-click reset, or a typed-and-committed entry)
    /// that begins and commits on the same frame — the discrete-click path. The
    /// `prev != current` guard in `commit` still drops a no-op (e.g. resetting an
    /// already-default field), so this never records an empty step.
    fn mark_discrete(&mut self) {
        self.changed = true;
        self.started = true;
        self.stopped = true;
    }

    /// Resolve the accumulated responses into `(begin, commit, changed)` with the
    /// discrete-click handling the hand-rolled blocks used: a value that changed
    /// without a drag begins and commits on the same frame.
    fn resolve(&self) -> (bool, bool, bool) {
        let discrete = self.changed && !self.dragged && !self.started && !self.stopped;
        (
            self.started || discrete,
            self.stopped || discrete,
            self.changed,
        )
    }

    /// Apply the accumulated gesture to `history`: begin once on the first frame
    /// of the gesture and commit once when it ends. The field writes already
    /// happened inside each [`adjust_slider`] (only on change), so `commit`'s
    /// `prev != current` guard records nothing for a net-zero block. Returns
    /// whether anything changed (so the caller can mark the preview dirty).
    fn finish(self, history: &mut History<Settings>) -> bool {
        let (begin, commit, changed) = self.resolve();
        if begin {
            history.begin();
        }
        if commit {
            history.commit();
        }
        changed
    }
}

/// Reset an optional adjustment field to its default: neutral is `None` (the
/// field off). Pure so the reset mapping is unit-testable independently of the
/// display-driven gesture wiring.
fn opt_reset() -> Option<f32> {
    None
}

/// Whether an optional adjustment field is modified from its default — i.e. set
/// to `Some(_)` rather than off. Pure predicate behind the modified indicator.
fn opt_is_modified(value: Option<f32>) -> bool {
    value.is_some()
}

/// Whether a plain `f32` field differs from its default value. Pure predicate
/// behind the modified indicator for non-optional controls.
fn value_is_modified(value: f32, default: f32) -> bool {
    value != default
}

/// Paint a subtle dot at the right edge of `rect`'s row when `modified` is true,
/// marking a control whose value differs from its default. Pure egui paint with
/// no interaction and no history effect.
fn modified_marker(ui: &egui::Ui, rect: egui::Rect, modified: bool) {
    if !modified {
        return;
    }
    let center = egui::pos2(rect.right() - 2.0, rect.center().y);
    ui.painter().circle_filled(center, 2.5, theme::ACCENT);
}

/// Draw a non-interactive tick at the neutral/default position on a slider's
/// track, so the user sees where "off" is. `neutral` is in the slider's value
/// range. Pure painting over the slider rect — no interaction, no history.
fn neutral_marker(
    ui: &egui::Ui,
    rect: egui::Rect,
    range: &std::ops::RangeInclusive<f32>,
    neutral: f32,
) {
    let (lo, hi) = (*range.start(), *range.end());
    if hi <= lo || neutral < lo || neutral > hi {
        return;
    }
    // The slider rail spans the rect's width inset by the handle radius; a small
    // inset keeps the tick over the usable track rather than under the handle.
    let inset = rect.height() * 0.5;
    let left = rect.left() + inset;
    let right = rect.right() - inset - theme::SLIDER_NUMERIC_WIDTH;
    if right <= left {
        return;
    }
    let t = (neutral - lo) / (hi - lo);
    let x = left + t * (right - left);
    let (top, bottom) = (rect.center().y - 4.0, rect.center().y + 4.0);
    ui.painter().line_segment(
        [egui::pos2(x, top), egui::pos2(x, bottom)],
        egui::Stroke::new(1.0, theme::NEUTRAL_MARKER),
    );
}

/// One concise help line for a control: `what` plus the slider's own range, so
/// the tooltip text is derived from the `RangeInclusive` and can't drift from the
/// widget. `neutral` is named as the unchanged value.
fn help_text(what: &str, range: &std::ops::RangeInclusive<f32>, neutral: f32) -> String {
    format!(
        "{what}. {} … {}; {} is unchanged",
        trim_float(*range.start()),
        trim_float(*range.end()),
        trim_float(neutral)
    )
}

/// Format a slider bound for a tooltip without a trailing `.0` on whole numbers
/// (so a range reads `-5 … 5`, not `-5.0 … 5.0`).
fn trim_float(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The static description of one adjust-slider: its label, value range, neutral/
/// default value, and one-line help. Bundling these keeps the builder signatures
/// small and is the single place the tooltip's range text is derived from. Built
/// by the panel and the multi-field blocks; paired with a `get`/`set` closure pair
/// that binds it to one `Settings` field.
pub(crate) struct SliderSpec<'a> {
    pub(crate) label: &'a str,
    pub(crate) range: std::ops::RangeInclusive<f32>,
    /// The neutral (optional flavor) or default (plain flavor) value: where the
    /// marker sits and what a double-click resets toward.
    pub(crate) neutral: f32,
    pub(crate) help: &'a str,
}

/// Render the slider + numeric entry for `spec`, mutating `value`, and return the
/// `(slider, drag, double_clicked)` outcome plus paint the tooltip, neutral
/// marker, and modified dot. Shared by both flavors so the egui plumbing — the
/// `Slider`, the `DragValue`, the markers — lives in one place; the flavors only
/// differ in their `get`/`set` value mapping.
fn paint_slider(
    ui: &mut egui::Ui,
    spec: &SliderSpec,
    value: &mut f32,
    modified: bool,
) -> (egui::Response, egui::Response, bool) {
    let hint = help_text(spec.help, &spec.range, spec.neutral);
    let (slider, drag) = ui
        .horizontal(|ui| {
            let slider = ui.add(
                egui::Slider::new(value, spec.range.clone())
                    .text(spec.label)
                    .clamping(egui::SliderClamping::Always),
            );
            let drag = ui.add(
                egui::DragValue::new(value)
                    .range(spec.range.clone())
                    .speed(drag_speed(&spec.range))
                    .fixed_decimals(2),
            );
            (slider, drag)
        })
        .inner;
    slider.union(drag.clone()).on_hover_text(&hint);
    neutral_marker(ui, slider.rect, &spec.range, spec.neutral);
    modified_marker(ui, slider.rect, modified);
    let reset = slider.double_clicked();
    (slider, drag, reset)
}

/// A slider bound to an optional point adjustment, sharing one `scope` with the
/// rest of its block (pass a fresh scope and call [`GestureScope::finish`] for a
/// single-field control). The slider shows the spec's neutral when the field is
/// `None`; any change sets it to `Some(value)` via `set`. A numeric entry beside
/// it types a precise value, a double-click resets the field to `None`, and a tick
/// marks the neutral position. The numeric entry, double-click, and drag all flow
/// through `scope`, so each interaction is one undo step.
fn opt_adjust_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    scope: &mut GestureScope,
    spec: SliderSpec,
    get: impl Fn(&Settings) -> Option<f32>,
    set: impl Fn(&mut Settings, Option<f32>),
) {
    let mut value = get(history.current()).unwrap_or(spec.neutral);
    let modified = opt_is_modified(get(history.current()));
    let (slider, drag, reset) = paint_slider(ui, &spec, &mut value, modified);

    scope.record(&slider);
    scope.record(&drag);

    if reset {
        // Double-click resets the field to its default (None) as one undo step.
        scope.mark_discrete();
        set(history.current_mut(), opt_reset());
    } else if slider.changed() || drag.changed() {
        set(history.current_mut(), Some(value));
    }
}

/// A slider bound to a plain `f32` field, sharing one `scope` with its block (or
/// a fresh scope for a single control). Mirrors [`opt_adjust_slider`] but for a
/// non-optional value: double-click resets to the spec's default, the numeric
/// entry types a precise value, and the marker sits at the default.
fn adjust_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    scope: &mut GestureScope,
    spec: SliderSpec,
    get: impl Fn(&Settings) -> f32,
    set: impl Fn(&mut Settings, f32),
) {
    let mut value = get(history.current());
    let modified = value_is_modified(value, spec.neutral);
    let (slider, drag, reset) = paint_slider(ui, &spec, &mut value, modified);

    scope.record(&slider);
    scope.record(&drag);

    if reset {
        scope.mark_discrete();
        set(history.current_mut(), spec.neutral);
    } else if slider.changed() || drag.changed() {
        set(history.current_mut(), value);
    }
}

/// A per-step drag speed for the numeric entry: a small fraction of the range so
/// dragging the `DragValue` is fine-grained, independent of the slider's own
/// pixel-driven step. Holding the egui fine modifier slows both further.
fn drag_speed(range: &std::ops::RangeInclusive<f32>) -> f64 {
    let span = (*range.end() - *range.start()).abs().max(f32::EPSILON);
    (span as f64) / 400.0
}

/// A single-field optional slider: the public entry point for a lone optional
/// control. Wraps [`opt_adjust_slider`] in its own one-response scope so the one
/// drag (or click-set, reset, or typed entry) is one undo step.
pub(crate) fn opt_point_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    spec: SliderSpec,
    get: impl Fn(&Settings) -> Option<f32>,
    set: impl Fn(&mut Settings, Option<f32>),
) -> bool {
    let mut scope = GestureScope::default();
    opt_adjust_slider(ui, history, &mut scope, spec, get, set);
    scope.finish(history)
}

/// A single-field plain slider: the public entry point for a lone non-optional
/// control. Wraps [`adjust_slider`] in its own one-response scope.
pub(crate) fn value_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    spec: SliderSpec,
    get: impl Fn(&Settings) -> f32,
    set: impl Fn(&mut Settings, f32),
) -> bool {
    let mut scope = GestureScope::default();
    adjust_slider(ui, history, &mut scope, spec, get, set);
    scope.finish(history)
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
    if ui
        .checkbox(&mut enabled, "Curves")
        .on_hover_text("Master and per-channel tone curves; drag the five points")
        .changed()
    {
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

/// White balance: two sliders (temp/tint) editing one optional adjustment. The
/// two sliders share one gesture scope, so dragging both is one undo step.
pub(crate) fn white_balance_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let wb = history.current().global.white_balance.unwrap_or_default();
    let mut scope = GestureScope::default();
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Temp",
            range: -1.0..=1.0,
            neutral: 0.0,
            help: "Warm/cool shift",
        },
        move |s| s.global.white_balance.unwrap_or(wb).temp,
        move |s, v| {
            let mut now = s.global.white_balance.unwrap_or(wb);
            now.temp = v;
            s.global.white_balance = wb_or_none(now);
        },
    );
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Tint",
            range: -1.0..=1.0,
            neutral: 0.0,
            help: "Green/magenta shift",
        },
        move |s| s.global.white_balance.unwrap_or(wb).tint,
        move |s, v| {
            let mut now = s.global.white_balance.unwrap_or(wb);
            now.tint = v;
            s.global.white_balance = wb_or_none(now);
        },
    );
    scope.finish(history)
}

/// Drop a white balance back to `None` when both channels are neutral, matching
/// the original block's "off when zeroed" behavior.
fn wb_or_none(wb: WhiteBalance) -> Option<WhiteBalance> {
    (wb.temp != 0.0 || wb.tint != 0.0).then_some(wb)
}

/// Selective tone: four sliders editing one optional adjustment, all sharing one
/// gesture scope so the whole block commits as a single undo step.
pub(crate) fn tone_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let t = history.current().global.tone.unwrap_or_default();
    let mut scope = GestureScope::default();
    for (label, help, get, set) in tone_fields(t) {
        adjust_slider(
            ui,
            history,
            &mut scope,
            SliderSpec {
                label,
                range: -1.0..=1.0,
                neutral: 0.0,
                help,
            },
            get,
            set,
        );
    }
    scope.finish(history)
}

/// The four selective-tone fields as `(label, help, get, set)` tuples; each
/// get/set reads and writes one component of the optional [`SelectiveTone`],
/// clearing it back to `None` once every component is neutral.
#[allow(clippy::type_complexity)]
fn tone_fields(
    base: SelectiveTone,
) -> [(
    &'static str,
    &'static str,
    impl Fn(&Settings) -> f32,
    impl Fn(&mut Settings, f32),
); 4] {
    fn field(
        base: SelectiveTone,
        getc: fn(&SelectiveTone) -> f32,
        setc: fn(&mut SelectiveTone, f32),
    ) -> (impl Fn(&Settings) -> f32, impl Fn(&mut Settings, f32)) {
        (
            move |s: &Settings| getc(&s.global.tone.unwrap_or(base)),
            move |s: &mut Settings, v: f32| {
                let mut now = s.global.tone.unwrap_or(base);
                setc(&mut now, v);
                s.global.tone = (now != SelectiveTone::default()).then_some(now);
            },
        )
    }
    let (cg, cs) = field(base, |t| t.contrast, |t, v| t.contrast = v);
    let (hg, hs) = field(base, |t| t.highlights, |t, v| t.highlights = v);
    let (sg, ss) = field(base, |t| t.shadows, |t, v| t.shadows = v);
    let (bg, bs) = field(base, |t| t.blacks, |t, v| t.blacks = v);
    [
        ("Contrast", "Overall contrast", cg, cs),
        ("Highlights", "Recover or lift the brights", hg, hs),
        ("Shadows", "Open or deepen the darks", sg, ss),
        ("Blacks", "The darkest tones", bg, bs),
    ]
}

/// Sharpening: amount/radius sliders editing one optional adjustment, sharing one
/// gesture scope.
pub(crate) fn sharpen_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let s = history.current().global.sharpen.unwrap_or_default();
    let mut scope = GestureScope::default();
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Sharpen amount",
            range: 0.0..=2.0,
            neutral: 0.0,
            help: "Edge sharpening strength",
        },
        move |st| st.global.sharpen.unwrap_or(s).amount,
        move |st, v| {
            let mut now = st.global.sharpen.unwrap_or(s);
            now.amount = v;
            st.global.sharpen = Some(now);
        },
    );
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Sharpen radius",
            range: 1.0..=10.0,
            neutral: s.radius,
            help: "Sharpening blur radius (px)",
        },
        move |st| st.global.sharpen.unwrap_or(s).radius,
        move |st, v| {
            let mut now = st.global.sharpen.unwrap_or(s);
            now.radius = v;
            st.global.sharpen = Some(now);
        },
    );
    scope.finish(history)
}

/// Clarity: midtone local-contrast amount/radius sliders editing one adjustment,
/// sharing one gesture scope.
pub(crate) fn clarity_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let c = history.current().global.clarity.unwrap_or_default();
    let mut scope = GestureScope::default();
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Clarity amount",
            range: -1.0..=1.0,
            neutral: 0.0,
            help: "Midtone local contrast",
        },
        move |st| st.global.clarity.unwrap_or(c).amount,
        move |st, v| {
            let mut now = st.global.clarity.unwrap_or(c);
            now.amount = v;
            st.global.clarity = Some(now);
        },
    );
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Clarity radius",
            range: 5.0..=100.0,
            neutral: c.radius,
            help: "Local-contrast blur radius (px)",
        },
        move |st| st.global.clarity.unwrap_or(c).radius,
        move |st, v| {
            let mut now = st.global.clarity.unwrap_or(c);
            now.radius = v;
            st.global.clarity = Some(now);
        },
    );
    scope.finish(history)
}

/// Noise reduction: independent luminance/color strengths plus a radius, all
/// sharing one gesture scope.
pub(crate) fn noise_reduction_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let nr = history.current().global.noise_reduction.unwrap_or_default();
    let mut scope = GestureScope::default();
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Luminance NR",
            range: 0.0..=0.3,
            neutral: 0.0,
            help: "Luminance noise reduction",
        },
        move |st| st.global.noise_reduction.unwrap_or(nr).luminance,
        move |st, v| {
            let mut now = st.global.noise_reduction.unwrap_or(nr);
            now.luminance = v;
            st.global.noise_reduction = Some(now);
        },
    );
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Color NR",
            range: 0.0..=0.3,
            neutral: 0.0,
            help: "Color noise reduction",
        },
        move |st| st.global.noise_reduction.unwrap_or(nr).color,
        move |st, v| {
            let mut now = st.global.noise_reduction.unwrap_or(nr);
            now.color = v;
            st.global.noise_reduction = Some(now);
        },
    );
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "NR radius",
            range: 1.0..=10.0,
            neutral: nr.radius,
            help: "Noise-reduction neighborhood (px)",
        },
        move |st| st.global.noise_reduction.unwrap_or(nr).radius,
        move |st, v| {
            let mut now = st.global.noise_reduction.unwrap_or(nr);
            now.radius = v;
            st.global.noise_reduction = Some(now);
        },
    );
    scope.finish(history)
}

/// Straighten angle (degrees), applied before the crop.
pub(crate) fn straighten_slider(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    value_slider(
        ui,
        history,
        SliderSpec {
            label: "Angle (°)",
            range: -45.0..=45.0,
            neutral: 0.0,
            help: "Level the horizon",
        },
        |s| s.geometry.straighten_degrees,
        |s, v| s.geometry.straighten_degrees = v,
    )
}

/// Creative vignette applied after the crop: negative darkens the corners,
/// positive lightens them. Zero clears it (back to `None`).
pub(crate) fn vignette_slider(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    opt_point_slider(
        ui,
        history,
        SliderSpec {
            label: "Vignette",
            range: -1.0..=1.0,
            neutral: 0.0,
            help: "Darken or lighten the corners",
        },
        |s| s.geometry.vignette,
        |s, v| s.geometry.vignette = v.filter(|&a| a != 0.0),
    )
}

/// Keystone: two sliders correcting converging verticals and horizontals, sharing
/// one gesture scope. Both at zero clears the correction (back to `None`).
pub(crate) fn keystone_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let p = history
        .current()
        .geometry
        .perspective
        .unwrap_or(Perspective {
            vertical: 0.0,
            horizontal: 0.0,
        });
    let mut scope = GestureScope::default();
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Vertical",
            range: -0.8..=0.8,
            neutral: 0.0,
            help: "Correct converging verticals",
        },
        move |s| s.geometry.perspective.unwrap_or(p).vertical,
        move |s, v| {
            let mut now = s.geometry.perspective.unwrap_or(p);
            now.vertical = v;
            s.geometry.perspective = perspective_or_none(now);
        },
    );
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Horizontal",
            range: -0.8..=0.8,
            neutral: 0.0,
            help: "Correct converging horizontals",
        },
        move |s| s.geometry.perspective.unwrap_or(p).horizontal,
        move |s, v| {
            let mut now = s.geometry.perspective.unwrap_or(p);
            now.horizontal = v;
            s.geometry.perspective = perspective_or_none(now);
        },
    );
    scope.finish(history)
}

/// Drop a keystone correction back to `None` once both axes are neutral, matching
/// the original block's "off when both zero" behavior.
fn perspective_or_none(p: Perspective) -> Option<Perspective> {
    (p.vertical != 0.0 || p.horizontal != 0.0).then_some(p)
}

/// Crop: four sliders for a normalized rectangle, editing one optional crop, all
/// sharing one gesture scope. The full frame `{0, 0, 1, 1}` is shown when there is
/// no crop.
pub(crate) fn crop_block(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
    let c = history.current().geometry.crop.unwrap_or(Crop {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    let mut scope = GestureScope::default();
    for (label, help, default, getc, setc) in [
        (
            "Left",
            "Crop left edge",
            0.0_f32,
            crop_get as fn(&Crop) -> f32,
            crop_set_x as fn(&mut Crop, f32),
        ),
        ("Top", "Crop top edge", 0.0, crop_get_y, crop_set_y),
        ("Width", "Crop width", 1.0, crop_get_w, crop_set_w),
        ("Height", "Crop height", 1.0, crop_get_h, crop_set_h),
    ] {
        adjust_slider(
            ui,
            history,
            &mut scope,
            SliderSpec {
                label,
                range: 0.0..=1.0,
                neutral: default,
                help,
            },
            move |s| getc(&s.geometry.crop.unwrap_or(c)),
            move |s, v| {
                let mut now = s.geometry.crop.unwrap_or(c);
                setc(&mut now, v);
                s.geometry.crop = crop_or_none(now);
            },
        );
    }
    scope.finish(history)
}

fn crop_get(c: &Crop) -> f32 {
    c.x
}
fn crop_get_y(c: &Crop) -> f32 {
    c.y
}
fn crop_get_w(c: &Crop) -> f32 {
    c.width
}
fn crop_get_h(c: &Crop) -> f32 {
    c.height
}
fn crop_set_x(c: &mut Crop, v: f32) {
    c.x = v;
}
fn crop_set_y(c: &mut Crop, v: f32) {
    c.y = v;
}
fn crop_set_w(c: &mut Crop, v: f32) {
    c.width = v;
}
fn crop_set_h(c: &mut Crop, v: f32) {
    c.height = v;
}

/// Drop a crop back to `None` when it spans the full frame, matching the original
/// block's "no crop when full" behavior.
fn crop_or_none(c: Crop) -> Option<Crop> {
    let full = c.x == 0.0 && c.y == 0.0 && c.width == 1.0 && c.height == 1.0;
    (!full).then_some(c)
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
        SliderSpec {
            label: "Opacity",
            range: 0.0..=1.0,
            neutral: 1.0,
            help: "Local adjustment blend strength",
        },
        move |s| s.locals[i].opacity,
        move |s, v| s.locals[i].opacity = v,
    );
    dirty |= opt_point_slider(
        ui,
        history,
        SliderSpec {
            label: "Exposure (EV)",
            range: -5.0..=5.0,
            neutral: 0.0,
            help: "Local brightness in stops",
        },
        move |s| s.locals[i].adjustments.exposure,
        move |s, v| s.locals[i].adjustments.exposure = v,
    );
    dirty |= opt_point_slider(
        ui,
        history,
        SliderSpec {
            label: "Saturation",
            range: 0.0..=2.0,
            neutral: 1.0,
            help: "Local color intensity",
        },
        move |s| s.locals[i].adjustments.saturation,
        move |s, v| s.locals[i].adjustments.saturation = v,
    );

    dirty
}

/// Sliders for the selected local adjustment's first mask shape (gradient
/// endpoints or radial center/radius/feather), in normalized coordinates. Each
/// shape's sliders share one gesture scope, so editing a shape is one undo step.
fn local_shape_block(ui: &mut egui::Ui, history: &mut History<Settings>, i: usize) -> bool {
    match history.current().locals[i].mask.shapes.first().cloned() {
        Some(MaskShape::Gradient(g)) => {
            let mut scope = GestureScope::default();
            for (label, help, default, getc, setc) in [
                (
                    "From X",
                    "Gradient start X",
                    g.x0,
                    grad_x0 as fn(&Gradient) -> f32,
                    grad_set_x0 as fn(&mut Gradient, f32),
                ),
                ("From Y", "Gradient start Y", g.y0, grad_y0, grad_set_y0),
                ("To X", "Gradient end X", g.x1, grad_x1, grad_set_x1),
                ("To Y", "Gradient end Y", g.y1, grad_y1, grad_set_y1),
            ] {
                adjust_slider(
                    ui,
                    history,
                    &mut scope,
                    SliderSpec {
                        label,
                        range: 0.0..=1.0,
                        neutral: default,
                        help,
                    },
                    move |s| match &s.locals[i].mask.shapes[0] {
                        MaskShape::Gradient(g) => getc(g),
                        _ => default,
                    },
                    move |s, v| {
                        if let MaskShape::Gradient(g) = &mut s.locals[i].mask.shapes[0] {
                            setc(g, v);
                        }
                    },
                );
            }
            scope.finish(history)
        }
        Some(MaskShape::Radial(r)) => {
            let mut scope = GestureScope::default();
            for (label, help, default, getc, setc) in [
                (
                    "Center X",
                    "Radial center X",
                    r.cx,
                    rad_cx as fn(&Radial) -> f32,
                    rad_set_cx as fn(&mut Radial, f32),
                ),
                ("Center Y", "Radial center Y", r.cy, rad_cy, rad_set_cy),
                ("Radius", "Radial radius", r.radius, rad_r, rad_set_r),
                ("Feather", "Edge softness", r.feather, rad_f, rad_set_f),
            ] {
                adjust_slider(
                    ui,
                    history,
                    &mut scope,
                    SliderSpec {
                        label,
                        range: 0.0..=1.0,
                        neutral: default,
                        help,
                    },
                    move |s| match &s.locals[i].mask.shapes[0] {
                        MaskShape::Radial(r) => getc(r),
                        _ => default,
                    },
                    move |s, v| {
                        if let MaskShape::Radial(r) = &mut s.locals[i].mask.shapes[0] {
                            setc(r, v);
                        }
                    },
                );
            }
            scope.finish(history)
        }
        Some(MaskShape::Luminosity(l)) => {
            let mut scope = GestureScope::default();
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Range low",
                    range: 0.0..=1.0,
                    neutral: l.lo,
                    help: "Darkest selected tone",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::Luminosity(l) => l.lo,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::Luminosity(l) = &mut s.locals[i].mask.shapes[0] {
                        l.lo = v;
                    }
                },
            );
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Range high",
                    range: 0.0..=1.0,
                    neutral: l.hi,
                    help: "Brightest selected tone",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::Luminosity(l) => l.hi,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::Luminosity(l) = &mut s.locals[i].mask.shapes[0] {
                        l.hi = v;
                    }
                },
            );
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Feather",
                    range: 0.0..=0.5,
                    neutral: l.feather,
                    help: "Edge softness",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::Luminosity(l) => l.feather,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::Luminosity(l) = &mut s.locals[i].mask.shapes[0] {
                        l.feather = v;
                    }
                },
            );
            scope.finish(history)
        }
        Some(MaskShape::ColorRange(c)) => {
            let mut scope = GestureScope::default();
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Hue",
                    range: 0.0..=1.0,
                    neutral: c.hue,
                    help: "Target hue",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::ColorRange(c) => c.hue,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[0] {
                        c.hue = v;
                    }
                },
            );
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Hue width",
                    range: 0.0..=0.5,
                    neutral: c.hue_width,
                    help: "Hue band half-width",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::ColorRange(c) => c.hue_width,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[0] {
                        c.hue_width = v;
                    }
                },
            );
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Min saturation",
                    range: 0.0..=1.0,
                    neutral: c.sat_min,
                    help: "Reject paler colors",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::ColorRange(c) => c.sat_min,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[0] {
                        c.sat_min = v;
                    }
                },
            );
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: "Feather",
                    range: 0.0..=0.5,
                    neutral: c.feather,
                    help: "Edge softness",
                },
                move |s| match &s.locals[i].mask.shapes[0] {
                    MaskShape::ColorRange(c) => c.feather,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[0] {
                        c.feather = v;
                    }
                },
            );
            scope.finish(history)
        }
        Some(MaskShape::Brush(_)) => {
            ui.label("Brush mask — paint on the preview to add, Erase to subtract.");
            false
        }
        None => false,
    }
}

fn grad_x0(g: &Gradient) -> f32 {
    g.x0
}
fn grad_y0(g: &Gradient) -> f32 {
    g.y0
}
fn grad_x1(g: &Gradient) -> f32 {
    g.x1
}
fn grad_y1(g: &Gradient) -> f32 {
    g.y1
}
fn grad_set_x0(g: &mut Gradient, v: f32) {
    g.x0 = v;
}
fn grad_set_y0(g: &mut Gradient, v: f32) {
    g.y0 = v;
}
fn grad_set_x1(g: &mut Gradient, v: f32) {
    g.x1 = v;
}
fn grad_set_y1(g: &mut Gradient, v: f32) {
    g.y1 = v;
}
fn rad_cx(r: &Radial) -> f32 {
    r.cx
}
fn rad_cy(r: &Radial) -> f32 {
    r.cy
}
fn rad_r(r: &Radial) -> f32 {
    r.radius
}
fn rad_f(r: &Radial) -> f32 {
    r.feather
}
fn rad_set_cx(r: &mut Radial, v: f32) {
    r.cx = v;
}
fn rad_set_cy(r: &mut Radial, v: f32) {
    r.cy = v;
}
fn rad_set_r(r: &mut Radial, v: f32) {
    r.radius = v;
}
fn rad_set_f(r: &mut Radial, v: f32) {
    r.feather = v;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_neutral_maps_to_none() {
        // Resetting an optional field yields the off (None) default, and the
        // modified predicate reads Some as modified, None as not.
        assert_eq!(opt_reset(), None);
        assert!(opt_is_modified(Some(0.5)));
        assert!(opt_is_modified(Some(0.0)));
        assert!(!opt_is_modified(None));
    }

    #[test]
    fn double_click_resets_to_default() {
        // The pure reset logic: a plain field resets to its default value and an
        // optional field resets to None. (The begin/commit bracketing of the
        // reset is display-driven and is not exercised headless.)
        let default = 0.0_f32;
        assert!(value_is_modified(1.0, default));
        assert!(!value_is_modified(default, default));
        // Resetting to default makes the field un-modified.
        let reset = default;
        assert!(!value_is_modified(reset, default));
        // The optional flavor resets to None.
        assert_eq!(opt_reset(), None);
    }

    #[test]
    fn help_text_states_range_and_neutral() {
        // The tooltip text is derived from the slider's own range, so it cannot
        // drift from the widget. Whole-number bounds drop the trailing `.0`.
        let h = help_text("Brightness in stops", &(-5.0..=5.0), 0.0);
        assert_eq!(h, "Brightness in stops. -5 … 5; 0 is unchanged");
        // A fractional bound keeps its decimals.
        let f = help_text("Strength", &(0.0..=0.3), 0.0);
        assert_eq!(f, "Strength. 0 … 0.3; 0 is unchanged");
    }

    #[test]
    fn wb_clears_to_none_when_neutral() {
        // The block-local "off when zeroed" helpers reproduce the original
        // sentinel-clearing behavior of the hand-rolled blocks.
        assert_eq!(
            wb_or_none(WhiteBalance {
                temp: 0.0,
                tint: 0.0
            }),
            None
        );
        assert!(
            wb_or_none(WhiteBalance {
                temp: 0.1,
                tint: 0.0
            })
            .is_some()
        );
        assert_eq!(
            perspective_or_none(Perspective {
                vertical: 0.0,
                horizontal: 0.0
            }),
            None
        );
        assert!(
            perspective_or_none(Perspective {
                vertical: 0.2,
                horizontal: 0.0
            })
            .is_some()
        );
        assert_eq!(
            crop_or_none(Crop {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0
            }),
            None
        );
        assert!(
            crop_or_none(Crop {
                x: 0.1,
                y: 0.0,
                width: 0.9,
                height: 1.0
            })
            .is_some()
        );
    }
}
