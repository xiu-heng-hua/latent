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
    Adjustments, Brush, ChannelMixer, Clarity, ColorRange, Crop, Curves, Gradient, History, Hsl,
    LocalAdjustment, LuminanceRange, Mask, MaskOp, MaskShape, NoiseReduction, Perspective, Radial,
    SelectiveTone, Settings, Sharpen, WhiteBalance,
};

use super::state::clamp_selection;
use super::theme;

pub(crate) mod wb;

/// A pair of accessors that bind a control block to *one* [`Adjustments`] inside a
/// [`Settings`] — either the global develop (`|s| &s.global` / `|s| &mut s.global`)
/// or one local's (`|s| &s.locals[i].adjustments`). The blocks below build their
/// per-field get/set closures by composing through these, so the same block edits
/// the global panel or a local sub-panel with no behavioral change to either.
///
/// Both closures must be `Copy` so a block can hand them to several per-field
/// closures; a closure that captures only `Copy` data (a local index `usize`, or
/// nothing) is itself `Copy`, which both call sites satisfy.
pub(crate) trait AdjustAccess: Copy {
    /// Borrow the bound adjustments out of the settings.
    fn get<'a>(&self, s: &'a Settings) -> &'a Adjustments;
    /// Borrow the bound adjustments mutably out of the settings.
    fn get_mut<'a>(&self, s: &'a mut Settings) -> &'a mut Adjustments;
}

/// The global develop adjustments (`s.global`).
#[derive(Clone, Copy)]
pub(crate) struct GlobalAccess;

impl AdjustAccess for GlobalAccess {
    fn get<'a>(&self, s: &'a Settings) -> &'a Adjustments {
        &s.global
    }
    fn get_mut<'a>(&self, s: &'a mut Settings) -> &'a mut Adjustments {
        &mut s.global
    }
}

/// One local's adjustments (`s.locals[i].adjustments`). Carries the local index by
/// value, so it is `Copy` and safe to `move` into the per-field closures.
#[derive(Clone, Copy)]
pub(crate) struct LocalAccess {
    pub(crate) index: usize,
}

impl AdjustAccess for LocalAccess {
    fn get<'a>(&self, s: &'a Settings) -> &'a Adjustments {
        &s.locals[self.index].adjustments
    }
    fn get_mut<'a>(&self, s: &'a mut Settings) -> &'a mut Adjustments {
        &mut s.locals[self.index].adjustments
    }
}

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

/// Render the slider for `spec`, mutating `value`, and return the
/// `(slider, double_clicked)` outcome plus paint the tooltip and neutral marker.
/// Shared by both flavors so the egui plumbing — the `Slider` and the marker —
/// lives in one place; the flavors only differ in their `get`/`set` value mapping.
/// The slider's own value display is click-to-edit, so a precise value is typed
/// there directly — no separate numeric field is rendered. Whether a value differs
/// from its default is shown by the section reset button's enabled state, not a
/// per-control dot.
fn paint_slider(ui: &mut egui::Ui, spec: &SliderSpec, value: &mut f32) -> (egui::Response, bool) {
    let hint = help_text(spec.help, &spec.range, spec.neutral);
    let slider = ui.add(
        egui::Slider::new(value, spec.range.clone())
            .text(spec.label)
            .clamping(egui::SliderClamping::Always),
    );
    slider.clone().on_hover_text(&hint);
    neutral_marker(ui, slider.rect, &spec.range, spec.neutral);
    let reset = slider.double_clicked();
    (slider, reset)
}

/// A slider bound to an optional point adjustment, sharing one `scope` with the
/// rest of its block (pass a fresh scope and call [`GestureScope::finish`] for a
/// single-field control). The slider shows the spec's neutral when the field is
/// `None`; any change sets it to `Some(value)` via `set`. The slider's own value
/// display is click-to-edit, so a precise value is typed there; a double-click
/// resets the field to `None`, and a tick marks the neutral position. The drag,
/// the typed entry, and the double-click all flow through `scope`, so each
/// interaction is one undo step.
fn opt_adjust_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    scope: &mut GestureScope,
    spec: SliderSpec,
    get: impl Fn(&Settings) -> Option<f32>,
    set: impl Fn(&mut Settings, Option<f32>),
) {
    let mut value = get(history.current()).unwrap_or(spec.neutral);
    let (slider, reset) = paint_slider(ui, &spec, &mut value);

    scope.record(&slider);

    if reset {
        // Double-click resets the field to its default (None) as one undo step.
        scope.mark_discrete();
        set(history.current_mut(), opt_reset());
    } else if slider.changed() {
        set(history.current_mut(), Some(value));
    }
}

/// A slider bound to a plain `f32` field, sharing one `scope` with its block (or
/// a fresh scope for a single control). Mirrors [`opt_adjust_slider`] but for a
/// non-optional value: double-click resets to the spec's default, the slider's
/// own value display types a precise value, and the marker sits at the default.
fn adjust_slider(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    scope: &mut GestureScope,
    spec: SliderSpec,
    get: impl Fn(&Settings) -> f32,
    set: impl Fn(&mut Settings, f32),
) {
    let mut value = get(history.current());
    let (slider, reset) = paint_slider(ui, &spec, &mut value);

    scope.record(&slider);

    if reset {
        scope.mark_discrete();
        set(history.current_mut(), spec.neutral);
    } else if slider.changed() {
        set(history.current_mut(), value);
    }
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
    access: impl AdjustAccess,
) -> bool {
    let mut dirty = false;

    let mut enabled = access.get(history.current()).curves.is_some();
    if ui
        .checkbox(&mut enabled, "Curves")
        .on_hover_text("Master and per-channel tone curves; drag the five points")
        .changed()
    {
        dirty |= set_curves_enabled(history, access, enabled);
    }
    // Local surface: the checkbox alone gates the body — an unchecked effect hides
    // its controls (there is no eye button here). Only render the editor when on.
    if enabled {
        dirty |= curves_body(ui, history, channel, access);
    }
    dirty
}

/// Flip `Adjustments.curves` on (a neutral identity [`Curves`]) or off (`None`) as
/// **one** undo step. The single place the curves enable is written, shared by the
/// inline checkbox (the local surface) and the controls-panel header toggle.
pub(crate) fn set_curves_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).curves = enabled.then(Curves::default);
    history.commit();
    true
}

/// The curve editor controls only (channel picker + the draggable graph). Split
/// from [`curves_block`] so the controls-panel header can own the enable checkbox
/// while the body renders the same editor. When curves are off (`None`) it renders
/// the identity [`Curves::default`] so the controls are still visible — the caller
/// greys them via `add_enabled_ui` and a disabled (non-interactive) editor never
/// drags, so it writes nothing; only a `Some` curve is ever mutated.
pub(crate) fn curves_body(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    channel: &mut usize,
    access: impl AdjustAccess,
) -> bool {
    let mut dirty = false;

    ui.horizontal(|ui| {
        for (i, name) in ["Master", "R", "G", "B"].into_iter().enumerate() {
            ui.selectable_value(channel, i, name);
        }
    });

    // Output (y) of each fixed-input point for the selected channel; identity
    // where a point has not been set yet. Off (`None`) reads as the identity.
    let mut ys: [f32; 5] = {
        let curves = access
            .get(history.current())
            .curves
            .clone()
            .unwrap_or_default();
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
        // Only a `Some` curve is written. A disabled (off) editor is greyed
        // non-interactive by the caller, so it never drags here; the guard keeps
        // the write panic-free even so.
        if let Some(curves) = access.get_mut(history.current_mut()).curves.as_mut() {
            *curve_channel_mut(curves, *channel) =
                CURVE_XS.iter().zip(ys).map(|(&x, y)| (x, y)).collect();
            dirty = true;
        }
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

/// Drop a white balance back to `None` when both channels are neutral, matching
/// the original block's "off when zeroed" behavior.
pub(crate) fn wb_or_none(wb: WhiteBalance) -> Option<WhiteBalance> {
    (wb.temp != 0.0 || wb.tint != 0.0).then_some(wb)
}

/// Selective tone: four sliders editing one optional adjustment, all sharing one
/// gesture scope so the whole block commits as a single undo step. Bound to the
/// `access`-selected adjustments (global or a local).
pub(crate) fn tone_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let t = access.get(history.current()).tone.unwrap_or_default();
    let mut scope = GestureScope::default();
    for (label, help, get, set) in tone_fields(t, access) {
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
/// get/set reads and writes one component of the optional [`SelectiveTone`]
/// behind `access`, clearing it back to `None` once every component is neutral.
#[allow(clippy::type_complexity)]
fn tone_fields(
    base: SelectiveTone,
    access: impl AdjustAccess,
) -> [(
    &'static str,
    &'static str,
    impl Fn(&Settings) -> f32,
    impl Fn(&mut Settings, f32),
); 4] {
    fn field(
        base: SelectiveTone,
        access: impl AdjustAccess,
        getc: fn(&SelectiveTone) -> f32,
        setc: fn(&mut SelectiveTone, f32),
    ) -> (impl Fn(&Settings) -> f32, impl Fn(&mut Settings, f32)) {
        (
            move |s: &Settings| getc(&access.get(s).tone.unwrap_or(base)),
            move |s: &mut Settings, v: f32| {
                let mut now = access.get(s).tone.unwrap_or(base);
                setc(&mut now, v);
                access.get_mut(s).tone = (now != SelectiveTone::default()).then_some(now);
            },
        )
    }
    let (cg, cs) = field(base, access, |t| t.contrast, |t, v| t.contrast = v);
    let (hg, hs) = field(base, access, |t| t.highlights, |t, v| t.highlights = v);
    let (sg, ss) = field(base, access, |t| t.shadows, |t, v| t.shadows = v);
    let (bg, bs) = field(base, access, |t| t.blacks, |t, v| t.blacks = v);
    [
        ("Contrast", "Overall contrast", cg, cs),
        ("Highlights", "Recover or lift the brights", hg, hs),
        ("Shadows", "Open or deepen the darks", sg, ss),
        ("Blacks", "The darkest tones", bg, bs),
    ]
}

/// Flip `Adjustments.sharpen` on (a [`Sharpen`] default — amount `0`, a sensible
/// radius, so it is a no-op until raised) or off (`None`) as **one** undo step.
/// The single place the controls-panel header toggle writes the sharpen enable.
pub(crate) fn set_sharpen_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).sharpen = enabled.then(Sharpen::default);
    history.commit();
    true
}

/// Flip `Adjustments.clarity` on (a [`Clarity`] default — amount `0`, a broad
/// radius, a no-op until raised) or off (`None`) as **one** undo step.
pub(crate) fn set_clarity_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).clarity = enabled.then(Clarity::default);
    history.commit();
    true
}

/// Flip `Adjustments.dehaze` on (`Some(0.0)`, a no-op until raised) or off (`None`)
/// as **one** undo step.
pub(crate) fn set_dehaze_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).dehaze = enabled.then_some(0.0);
    history.commit();
    true
}

/// Flip `Adjustments.noise_reduction` on (a [`NoiseReduction`] default — both
/// strengths `0`, a small radius, a no-op until raised) or off (`None`) as **one**
/// undo step.
pub(crate) fn set_noise_reduction_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).noise_reduction = enabled.then(NoiseReduction::default);
    history.commit();
    true
}

/// Sharpening: amount/radius sliders editing one optional adjustment, sharing one
/// gesture scope. Bound to the `access`-selected adjustments (global or a local).
pub(crate) fn sharpen_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let s = access.get(history.current()).sharpen.unwrap_or_default();
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
        move |st| access.get(st).sharpen.unwrap_or(s).amount,
        move |st, v| {
            let mut now = access.get(st).sharpen.unwrap_or(s);
            now.amount = v;
            access.get_mut(st).sharpen = Some(now);
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
        move |st| access.get(st).sharpen.unwrap_or(s).radius,
        move |st, v| {
            let mut now = access.get(st).sharpen.unwrap_or(s);
            now.radius = v;
            access.get_mut(st).sharpen = Some(now);
        },
    );
    scope.finish(history)
}

/// Clarity: midtone local-contrast amount/radius sliders editing one adjustment,
/// sharing one gesture scope. Bound to the `access`-selected adjustments.
pub(crate) fn clarity_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let c = access.get(history.current()).clarity.unwrap_or_default();
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
        move |st| access.get(st).clarity.unwrap_or(c).amount,
        move |st, v| {
            let mut now = access.get(st).clarity.unwrap_or(c);
            now.amount = v;
            access.get_mut(st).clarity = Some(now);
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
        move |st| access.get(st).clarity.unwrap_or(c).radius,
        move |st, v| {
            let mut now = access.get(st).clarity.unwrap_or(c);
            now.radius = v;
            access.get_mut(st).clarity = Some(now);
        },
    );
    scope.finish(history)
}

/// Noise reduction: independent luminance/color strengths plus a radius, all
/// sharing one gesture scope. Bound to the `access`-selected adjustments.
pub(crate) fn noise_reduction_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let nr = access
        .get(history.current())
        .noise_reduction
        .unwrap_or_default();
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
        move |st| access.get(st).noise_reduction.unwrap_or(nr).luminance,
        move |st, v| {
            let mut now = access.get(st).noise_reduction.unwrap_or(nr);
            now.luminance = v;
            access.get_mut(st).noise_reduction = Some(now);
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
        move |st| access.get(st).noise_reduction.unwrap_or(nr).color,
        move |st, v| {
            let mut now = access.get(st).noise_reduction.unwrap_or(nr);
            now.color = v;
            access.get_mut(st).noise_reduction = Some(now);
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
        move |st| access.get(st).noise_reduction.unwrap_or(nr).radius,
        move |st, v| {
            let mut now = access.get(st).noise_reduction.unwrap_or(nr);
            now.radius = v;
            access.get_mut(st).noise_reduction = Some(now);
        },
    );
    scope.finish(history)
}

/// The eight HSL bands' names and their representative hue (in degrees) for the
/// swatch, in index order red (0) … magenta (7) — matching the engine's band
/// layout (`Hsl::bands`).
const HSL_BANDS: [(&str, f32); 8] = [
    ("Red", 0.0),
    ("Orange", 30.0),
    ("Yellow", 60.0),
    ("Green", 120.0),
    ("Aqua", 180.0),
    ("Blue", 240.0),
    ("Purple", 270.0),
    ("Magenta", 300.0),
];

/// A fully-saturated, full-value [`egui::Color32`] for a hue in degrees, for the
/// HSL band swatches. A small HSV→RGB so the swatch shows the band's color
/// without pulling in a color crate.
fn hue_swatch(hue_deg: f32) -> egui::Color32 {
    let h = (hue_deg.rem_euclid(360.0)) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    egui::Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

/// One HSL band row: a small color swatch in the band's representative hue, then
/// Hue / Sat / Lum [`adjust_slider`]s bound to that band's `[hue, sat, lum]`. The
/// row shares the block's `scope`, so a drag across any band is one undo step. The
/// `hue` slider *shifts* the band (small range), while `sat`/`lum` *scale* it by
/// `1 + value` (so `0` is unchanged) — matching the engine's interpretation.
fn hsl_band_row(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    scope: &mut GestureScope,
    access: impl AdjustAccess,
    band: usize,
) {
    let (name, hue_deg) = HSL_BANDS[band];
    let base = access.get(history.current()).hsl.unwrap_or_default();
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, hue_swatch(hue_deg));
        ui.label(name);
    });
    // `hue` shifts in turns (a gentle range); `sat`/`lum` scale by `1 + value`.
    let specs = [
        ("Hue", -0.1_f32..=0.1_f32, "Shift this band's hue", 0_usize),
        ("Sat", -1.0..=1.0, "Scale this band's saturation", 1),
        ("Lum", -1.0..=1.0, "Scale this band's lightness", 2),
    ];
    for (label, range, help, channel) in specs {
        adjust_slider(
            ui,
            history,
            scope,
            SliderSpec {
                label,
                range,
                neutral: 0.0,
                help,
            },
            move |s| access.get(s).hsl.unwrap_or(base).bands[band][channel],
            move |s, v| {
                let mut now = access.get(s).hsl.unwrap_or(base);
                now.bands[band][channel] = v;
                access.get_mut(s).hsl = Some(now);
            },
        );
    }
}

/// The HSL mixer: an enable checkbox over `Adjustments.hsl` (unchecked → `None`,
/// checked → a neutral all-zero [`Hsl`]), then eight band rows of Hue/Sat/Lum
/// sliders with hue swatches. All the band sliders share one gesture scope, so a
/// drag is one undo step. Bound to the `access`-selected adjustments.
pub(crate) fn hsl_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let mut dirty = false;
    let mut enabled = access.get(history.current()).hsl.is_some();
    if ui
        .checkbox(&mut enabled, "HSL Mixer")
        .on_hover_text("Per-hue-band hue/saturation/lightness")
        .changed()
    {
        dirty |= set_hsl_enabled(history, access, enabled);
    }
    // Local surface: the checkbox alone gates the body — render the band rows only
    // when enabled, so an unchecked effect hides its controls (no eye button here).
    if enabled {
        dirty |= hsl_body(ui, history, access);
    }
    dirty
}

/// Flip `Adjustments.hsl` on (a neutral all-zero [`Hsl`]) or off (`None`) as **one**
/// undo step. The single place the HSL enable is written, shared by the inline
/// checkbox (the local surface) and the controls-panel header toggle.
pub(crate) fn set_hsl_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).hsl = enabled.then(Hsl::default);
    history.commit();
    true
}

/// The eight HSL band rows. Split from [`hsl_block`] so the controls-panel header
/// can own the enable checkbox. When HSL is off (`None`) the rows render the neutral
/// [`Hsl::default`] (each band already reads `unwrap_or_default`), so the controls
/// are still visible; the caller greys them via `add_enabled_ui`, and a
/// non-interactive slider never `.changed()`, so a disabled body writes nothing.
pub(crate) fn hsl_body(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let mut scope = GestureScope::default();
    for band in 0..8 {
        hsl_band_row(ui, history, &mut scope, access, band);
    }
    scope.finish(history)
}

/// The channel mixer: an enable checkbox over `Adjustments.channel_mixer`
/// (unchecked → `None`, checked → the identity [`ChannelMixer`], a no-op), then
/// three output-channel groups (Red / Green / Blue output) each with three
/// input-weight sliders, plus the preserve-luminosity toggle. All nine matrix
/// sliders share one gesture scope. Each cell's neutral is its identity entry
/// (diagonal `1`, off-diagonal `0`), so a double-click resets to identity. Bound
/// to the `access`-selected adjustments.
pub(crate) fn channel_mixer_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let mut dirty = false;
    let mut enabled = access.get(history.current()).channel_mixer.is_some();
    if ui
        .checkbox(&mut enabled, "Channel Mixer")
        .on_hover_text("Mix each output channel from the input R/G/B")
        .changed()
    {
        dirty |= set_channel_mixer_enabled(history, access, enabled);
    }
    // Local surface: the checkbox alone gates the body — render the matrix only when
    // enabled, so an unchecked effect hides its controls (no eye button here).
    if enabled {
        dirty |= channel_mixer_body(ui, history, access);
    }
    dirty
}

/// Flip `Adjustments.channel_mixer` on (the identity [`ChannelMixer`], a no-op) or
/// off (`None`) as **one** undo step. The single place the channel-mixer enable is
/// written, shared by the inline checkbox (the local surface) and the
/// controls-panel header toggle.
pub(crate) fn set_channel_mixer_enabled(
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    enabled: bool,
) -> bool {
    history.begin();
    access.get_mut(history.current_mut()).channel_mixer = enabled.then(ChannelMixer::default);
    history.commit();
    true
}

/// The channel-mixer matrix controls only (the nine input-weight sliders plus the
/// preserve-luminosity toggle). Split from [`channel_mixer_block`] so the
/// controls-panel header can own the enable checkbox. When the mixer is off
/// (`None`) it renders the identity [`ChannelMixer::default`] so the controls are
/// still visible; the caller greys them via `add_enabled_ui`, and a non-interactive
/// control never `.changed()`, so a disabled body writes nothing.
pub(crate) fn channel_mixer_body(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
) -> bool {
    let mut dirty = false;
    let base = access
        .get(history.current())
        .channel_mixer
        .unwrap_or_default();

    let mut scope = GestureScope::default();
    for (out, out_name) in ["Red output", "Green output", "Blue output"]
        .into_iter()
        .enumerate()
    {
        ui.label(out_name);
        for (inp, in_name) in ["from R", "from G", "from B"].into_iter().enumerate() {
            // Identity neutral: 1 on the diagonal, 0 off it, so a reset returns
            // the cell to the no-op identity, not a flat zero.
            let neutral = if out == inp { 1.0 } else { 0.0 };
            adjust_slider(
                ui,
                history,
                &mut scope,
                SliderSpec {
                    label: in_name,
                    range: -2.0..=2.0,
                    neutral,
                    help: "Input channel weight",
                },
                move |s| access.get(s).channel_mixer.unwrap_or(base).matrix[out][inp],
                move |s, v| {
                    let mut now = access.get(s).channel_mixer.unwrap_or(base);
                    now.matrix[out][inp] = v;
                    access.get_mut(s).channel_mixer = Some(now);
                },
            );
        }
    }

    let mut preserve = base.preserve_luminosity;
    if ui
        .checkbox(&mut preserve, "Preserve luminosity")
        .on_hover_text("Normalize each row to sum 1 so a neutral gray stays put")
        .changed()
    {
        history.begin();
        let mut now = access.get(history.current()).channel_mixer.unwrap_or(base);
        now.preserve_luminosity = preserve;
        access.get_mut(history.current_mut()).channel_mixer = Some(now);
        history.commit();
        dirty = true;
    }

    dirty | scope.finish(history)
}

/// What a white-balance block asks the caller to do beyond the in-place slider/
/// preset edits it already applied: activate the gray eyedropper (a canvas tool
/// the panel can't reach), or run the gray-world Auto estimate (which needs the
/// preview image the panel doesn't hold). `None` means the block handled
/// everything itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum WbAction {
    #[default]
    None,
    /// Activate the eyedropper (pick a neutral pixel on the canvas).
    PickGray,
    /// Estimate a gray-world white balance from the preview.
    Auto,
}

/// White balance over `WhiteBalance { temp, tint }`: a Kelvin slider + a tint
/// slider (mapped through the documented invertible [`wb`] functions), an
/// eyedropper button, and the presets. Writes the single `WhiteBalance` field —
/// no second representation. Returns whether the preview is dirty and, via
/// `action`, any canvas/preview work only the caller can do. Bound to the
/// `access`-selected adjustments.
pub(crate) fn white_balance_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: impl AdjustAccess,
    action: &mut WbAction,
) -> bool {
    let wb = access
        .get(history.current())
        .white_balance
        .unwrap_or_default();
    let mut scope = GestureScope::default();
    // Kelvin slider, mapped to `temp` through the mired-difference curve.
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Temp (K)",
            range: wb::KELVIN_MIN..=wb::KELVIN_MAX,
            neutral: wb::T0,
            help: "Color temperature in Kelvin",
        },
        move |s| wb::temp_to_kelvin(access.get(s).white_balance.unwrap_or(wb).temp),
        move |s, k| {
            let mut now = access.get(s).white_balance.unwrap_or(wb);
            now.temp = wb::kelvin_to_temp(k);
            access.get_mut(s).white_balance = wb_or_none(now);
        },
    );
    // Tint slider, mapped linearly to `tint`.
    adjust_slider(
        ui,
        history,
        &mut scope,
        SliderSpec {
            label: "Tint",
            range: -wb::TINT_RANGE..=wb::TINT_RANGE,
            neutral: 0.0,
            help: "Green/magenta shift",
        },
        move |s| wb::tint_to_slider(access.get(s).white_balance.unwrap_or(wb).tint),
        move |s, t| {
            let mut now = access.get(s).white_balance.unwrap_or(wb);
            now.tint = wb::slider_to_tint(t);
            access.get_mut(s).white_balance = wb_or_none(now);
        },
    );
    let mut dirty = scope.finish(history);

    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Pick gray")
            .on_hover_text("Click a neutral patch on the image to set the balance")
            .clicked()
        {
            *action = WbAction::PickGray;
        }
        for preset in wb::Preset::ALL {
            if ui.button(preset.label()).clicked() {
                match preset.kelvin() {
                    Some(kelvin) => {
                        history.begin();
                        access.get_mut(history.current_mut()).white_balance =
                            wb_or_none(WhiteBalance {
                                temp: wb::kelvin_to_temp(kelvin),
                                tint: 0.0,
                            });
                        history.commit();
                        dirty = true;
                    }
                    None if preset == wb::Preset::AsShot => {
                        // As Shot clears the editable offset to None; the as-shot
                        // decode balance applies underneath.
                        history.begin();
                        access.get_mut(history.current_mut()).white_balance = None;
                        history.commit();
                        dirty = true;
                    }
                    // Auto needs the preview; defer to the caller.
                    None => *action = WbAction::Auto,
                }
            }
        }
    });
    dirty
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

/// Flip `geometry.vignette` on (`Some(0.0)`, a no-op until dragged) or off (`None`)
/// as **one** undo step. The single place the controls-panel header toggle writes
/// the vignette enable.
pub(crate) fn set_vignette_enabled(history: &mut History<Settings>, enabled: bool) -> bool {
    history.begin();
    history.current_mut().geometry.vignette = enabled.then_some(0.0);
    history.commit();
    true
}

/// The vignette amount slider only; the header checkbox owns the on/off. Negative
/// darkens the corners, positive lightens them. The header owns `None`, so the
/// slider keeps the value `Some` even at the neutral `0` rather than clearing it
/// (which would fight the checkbox). When off (`None`) the slider renders at the
/// neutral `0` (`opt_point_slider` shows `neutral` for `None`) so the control is
/// still visible; the caller greys it via `add_enabled_ui`, and a non-interactive
/// slider never `.changed()`, so a disabled body writes nothing.
pub(crate) fn vignette_body(ui: &mut egui::Ui, history: &mut History<Settings>) -> bool {
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
        // A double-click resets the slider to None (off); a plain drag keeps it
        // Some even at 0, so the header checkbox stays checked while dragging
        // through neutral.
        |s, v| s.geometry.vignette = v,
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

/// The display unit of one crop field. A crop is always stored normalized `[0, 1]`;
/// this only chooses how a cell *shows and edits* that value. `Px` (the default)
/// reads in image pixels, `Pct` in percent of the dimension. Pure UI state — it
/// never changes the stored crop or the render — so it lives on the session, not
/// in the edit history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CropUnit {
    /// Image pixels along the field's dimension (width for Left/Width, height for
    /// Top/Height).
    #[default]
    Px,
    /// Percent of the field's dimension (`normalized × 100`).
    Pct,
}

impl CropUnit {
    /// The short button label naming this unit.
    fn label(self) -> &'static str {
        match self {
            CropUnit::Px => "px",
            CropUnit::Pct => "%",
        }
    }

    /// The other unit, for the toggle button.
    fn toggled(self) -> Self {
        match self {
            CropUnit::Px => CropUnit::Pct,
            CropUnit::Pct => CropUnit::Px,
        }
    }
}

/// Convert a normalized `[0, 1]` crop value into pixels along a `dim`-pixel axis,
/// rounded to a whole pixel (crop cells show integer pixels). Pure for testing.
fn norm_to_px(norm: f32, dim: u32) -> f32 {
    (norm * dim as f32).round()
}

/// Convert an entered pixel value along a `dim`-pixel axis back into a normalized
/// `[0, 1]` value (`px / dim`), clamped to `[0, 1]`. A zero dimension maps to `0`.
/// Pure for testing.
fn px_to_norm(px: f32, dim: u32) -> f32 {
    if dim == 0 {
        return 0.0;
    }
    (px / dim as f32).clamp(0.0, 1.0)
}

/// Convert a normalized `[0, 1]` crop value into percent (`norm × 100`). Pure for
/// testing.
fn norm_to_pct(norm: f32) -> f32 {
    norm * 100.0
}

/// Convert an entered percent back into a normalized `[0, 1]` value (`pct / 100`),
/// clamped to `[0, 1]`. Pure for testing.
fn pct_to_norm(pct: f32) -> f32 {
    (pct / 100.0).clamp(0.0, 1.0)
}

/// One crop cell's static descriptor: its label, the normalized get/set on the
/// [`Crop`], and the pixel length of its axis (image width for Left/Width, height
/// for Top/Height). The grid lists four of these.
type CropCell = (&'static str, fn(&Crop) -> f32, fn(&mut Crop, f32), u32);

/// One crop cell: a label, a numeric [`egui::DragValue`] in the field's current
/// unit, and a fixed-width px/% toggle button. Editing the number writes the same
/// normalized `geometry.crop` the on-canvas crop tool writes (so dragging the
/// rectangle and typing stay in sync), clamped through [`crop::clamp_crop`] to keep
/// the minimum size and stay on-frame, and folds into `scope` so the cell is one
/// undo step. The toggle flips only `*unit` (pure UI state, no history). `dim` is
/// the pixel length of this field's axis (image width for Left/Width, height for
/// Top/Height).
#[allow(clippy::too_many_arguments)]
fn crop_cell(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    scope: &mut GestureScope,
    base: Crop,
    label: &str,
    unit: &mut CropUnit,
    dim: u32,
    getc: fn(&Crop) -> f32,
    setc: fn(&mut Crop, f32),
) {
    use crate::gui::tools::crop;

    let norm = getc(&history.current().geometry.crop.unwrap_or(base));
    // The value shown/edited in this cell's current unit.
    let mut shown = match *unit {
        CropUnit::Px => norm_to_px(norm, dim),
        CropUnit::Pct => norm_to_pct(norm),
    };
    ui.label(label);
    // Range and precision follow the unit: whole pixels up to the dimension, or a
    // single-decimal percent up to 100. A fixed-width field keeps the grid columns
    // aligned and stops the number jittering as its digit count changes.
    let dv = match *unit {
        CropUnit::Px => egui::DragValue::new(&mut shown)
            .range(0.0..=dim as f32)
            .speed(1.0)
            .fixed_decimals(0),
        CropUnit::Pct => egui::DragValue::new(&mut shown)
            .range(0.0..=100.0)
            .speed(0.25)
            .fixed_decimals(1),
    };
    let field_size = egui::vec2(theme::CROP_FIELD_WIDTH, ui.spacing().interact_size.y);
    let drag = ui.add_sized(field_size, dv);
    // A fixed-width unit button so toggling "px"↔"%" never shifts the layout
    // (FRONTEND.md: the UI must not move when a button is clicked).
    if ui
        .add(
            egui::Button::new(unit.label())
                .min_size(egui::vec2(theme::CROP_UNIT_BUTTON_WIDTH, 0.0)),
        )
        .on_hover_text("Toggle pixels / percent")
        .clicked()
    {
        *unit = unit.toggled();
    }

    scope.record(&drag);
    if drag.changed() {
        let entered = match *unit {
            CropUnit::Px => px_to_norm(shown, dim),
            CropUnit::Pct => pct_to_norm(shown),
        };
        let mut now = history.current().geometry.crop.unwrap_or(base);
        setc(&mut now, entered);
        // Reuse the on-canvas crop invariants: keep the minimum size and stay
        // on-frame, then normalize a full-frame rect back to `None`.
        let clamped = crop::clamp_crop(now);
        history.current_mut().geometry.crop = (!crop::is_full_frame(clamped)).then_some(clamped);
    }
}

/// Crop: a 2×2 numeric grid for a normalized rectangle — Left/Top on the first
/// row, Width/Height on the second — each cell a numeric entry with a per-field
/// px/% unit toggle (no slider). People drag the rectangle on the canvas; the grid
/// is for typing a precise value, which they think of in pixels (the default unit).
/// All four cells share one gesture scope, so a typed edit is one undo step, and
/// they write the same normalized `geometry.crop` the on-canvas tool writes. The
/// full frame `{0, 0, 1, 1}` is shown when there is no crop. `units` is the
/// per-field unit state (Left/Top/Width/Height); `image_w`/`image_h` size the px
/// conversion. Returns whether the preview is dirty.
pub(crate) fn crop_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    units: &mut [CropUnit; 4],
    image_w: u32,
    image_h: u32,
) -> bool {
    let base = history.current().geometry.crop.unwrap_or(Crop {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    let mut scope = GestureScope::default();
    // The four cells, by `units` index. Left/Width measure along the image width;
    // Top/Height along the height.
    let cells: [CropCell; 4] = [
        ("Left", |c| c.x, |c, v| c.x = v, image_w),
        ("Top", |c| c.y, |c, v| c.y = v, image_h),
        ("Width", |c| c.width, |c, v| c.width = v, image_w),
        ("Height", |c| c.height, |c, v| c.height = v, image_h),
    ];
    // A real grid (Left|Top on the first row, Width|Height on the second) so the
    // label, number, and unit columns line up; centered in the panel. Each cell
    // emits three widgets (label, number, unit) = six aligned columns.
    ui.vertical_centered(|ui| {
        egui::Grid::new("crop_grid")
            .num_columns(6)
            .spacing(egui::vec2(8.0, 4.0))
            .show(ui, |ui| {
                for row in 0..2 {
                    for col in 0..2 {
                        let i = row * 2 + col;
                        let (label, getc, setc, dim) = cells[i];
                        crop_cell(
                            ui,
                            history,
                            &mut scope,
                            base,
                            label,
                            &mut units[i],
                            dim,
                            getc,
                            setc,
                        );
                    }
                    ui.end_row();
                }
            });
    });
    scope.finish(history)
}

/// The default mask shape for each "add shape" button, by kind. The first shape
/// in a fresh local is the base; later shapes append with an [`MaskOp::Add`].
fn default_shape(kind: ShapeKind) -> MaskShape {
    match kind {
        ShapeKind::Gradient => MaskShape::Gradient(Gradient {
            x0: 0.5,
            y0: 0.0,
            x1: 0.5,
            y1: 1.0,
        }),
        ShapeKind::Radial => MaskShape::Radial(Radial {
            cx: 0.5,
            cy: 0.5,
            radius: 0.25,
            feather: 0.25,
        }),
        // Defaults to the shadows; drag the range to retarget.
        ShapeKind::Luminosity => MaskShape::Luminosity(LuminanceRange {
            lo: 0.0,
            hi: 0.3,
            feather: 0.1,
        }),
        // Defaults to reds; drag the hue to retarget.
        ShapeKind::ColorRange => MaskShape::ColorRange(ColorRange {
            hue: 0.0,
            hue_width: 0.08,
            sat_min: 0.15,
            feather: 0.08,
        }),
        ShapeKind::Brush => MaskShape::Brush(Brush::default()),
    }
}

/// The five mask-shape kinds the add buttons create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapeKind {
    Gradient,
    Radial,
    Luminosity,
    ColorRange,
    Brush,
}

/// The short label naming a shape's kind in the shape list.
fn shape_kind_label(shape: &MaskShape) -> &'static str {
    match shape {
        MaskShape::Gradient(_) => "Gradient",
        MaskShape::Radial(_) => "Radial",
        MaskShape::Luminosity(_) => "Luminosity",
        MaskShape::ColorRange(_) => "Color",
        MaskShape::Brush(_) => "Brush",
    }
}

/// Append a new shape (with a default [`MaskOp::Add`], kept parallel to `shapes`)
/// to the selected local's mask, select it, and record one undo step. The first
/// shape pushes an `Add` too — its op is ignored but keeping `ops.len() ==
/// shapes.len()` keeps the two vectors aligned, the invariant `weight_at` relies
/// on.
fn add_shape(
    history: &mut History<Settings>,
    local: usize,
    kind: ShapeKind,
    shape_sel: &mut usize,
) {
    history.begin();
    let mask = &mut history.current_mut().locals[local].mask;
    mask.shapes.push(default_shape(kind));
    mask.ops.push(MaskOp::Add);
    history.commit();
    *shape_sel = history.current().locals[local].mask.shapes.len() - 1;
}

/// The Local Adjustments panel: add/select/delete masked adjustments, edit the
/// selected local's mask shape list (add/remove shapes, per-shape combine op,
/// invert), and edit the local's full `Adjustments` surface. `sel` is the
/// selected local, `shape_sel` the selected shape within it (UI state). Any white
/// balance the local block needs the caller to act on is reported through
/// `wb_action`. Returns whether the preview needs a redraw.
pub(crate) fn local_adjustments(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    sel: &mut usize,
    shape_sel: &mut usize,
    wb_action: &mut WbAction,
) -> bool {
    let mut dirty = false;

    // The add-local buttons each create a one-shape mask of the chosen kind.
    ui.horizontal_wrapped(|ui| {
        for (label, kind) in [
            ("+ Graduated", ShapeKind::Gradient),
            ("+ Radial", ShapeKind::Radial),
            ("+ Luminosity", ShapeKind::Luminosity),
            ("+ Color", ShapeKind::ColorRange),
            ("+ Brush", ShapeKind::Brush),
        ] {
            if ui.button(label).clicked() {
                history.begin();
                history.current_mut().locals.push(LocalAdjustment {
                    mask: Mask {
                        shapes: vec![default_shape(kind)],
                        ops: vec![MaskOp::Add],
                        invert: false,
                    },
                    adjustments: Adjustments::default(),
                    opacity: 1.0,
                });
                history.commit();
                *sel = history.current().locals.len() - 1;
                *shape_sel = 0;
                dirty = true;
            }
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
                *shape_sel = 0;
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

    ui.separator();
    dirty |= mask_shape_list(ui, history, i, shape_sel);
    dirty |= local_shape_block(ui, history, i, *shape_sel);

    let mut invert = history.current().locals[i].mask.invert;
    if ui
        .checkbox(&mut invert, "Invert mask")
        .on_hover_text("Invert the combined mask")
        .changed()
    {
        history.begin();
        history.current_mut().locals[i].mask.invert = invert;
        history.commit();
        dirty = true;
    }

    ui.separator();
    let access = LocalAccess { index: i };
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
    dirty |= local_adjustment_surface(ui, history, access, wb_action);

    dirty
}

/// The full per-local adjustment surface — the **same** control blocks the global
/// panel uses, bound to `locals[i].adjustments` via [`LocalAccess`]. So a local
/// carries exposure, white balance, tone, curves, saturation, HSL, the channel
/// mixer, sharpen, clarity, dehaze, and noise reduction — all rendered within its
/// mask at its opacity by the unchanged engine. The curve channel reuses a
/// transient id (a local's curve editor doesn't persist its channel choice across
/// reselection, which is acceptable for a sub-panel).
fn local_adjustment_surface(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    access: LocalAccess,
    wb_action: &mut WbAction,
) -> bool {
    let mut dirty = false;
    let i = access.index;
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
    dirty |= white_balance_block(ui, history, access, wb_action);
    dirty |= tone_block(ui, history, access);
    let mut channel = 0_usize;
    dirty |= curves_block(ui, history, &mut channel, access);
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
    dirty |= hsl_block(ui, history, access);
    dirty |= channel_mixer_block(ui, history, access);
    dirty |= sharpen_block(ui, history, access);
    dirty |= clarity_block(ui, history, access);
    dirty |= opt_point_slider(
        ui,
        history,
        SliderSpec {
            label: "Dehaze",
            range: 0.0..=1.0,
            neutral: 0.0,
            help: "Cut atmospheric haze",
        },
        move |s| s.locals[i].adjustments.dehaze,
        move |s, v| s.locals[i].adjustments.dehaze = v,
    );
    dirty |= noise_reduction_block(ui, history, access);
    dirty
}

/// The mask shape list for the selected local: a selectable row per shape labeled
/// by kind, with a per-shape combine op (the base shape shows no op), an add menu,
/// and a remove button (which drops the matching `ops` entry too, keeping the two
/// vectors parallel). The selected shape drives the on-canvas handles. Each list
/// mutation is one undo step.
fn mask_shape_list(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    local: usize,
    shape_sel: &mut usize,
) -> bool {
    let mut dirty = false;
    let count = history.current().locals[local].mask.shapes.len();
    clamp_selection(shape_sel, count.max(1));

    ui.label("Mask shapes:");
    for s in 0..count {
        ui.horizontal(|ui| {
            let label = {
                let shape = &history.current().locals[local].mask.shapes[s];
                format!("{}. {}", s + 1, shape_kind_label(shape))
            };
            if ui.selectable_label(*shape_sel == s, label).clicked() {
                *shape_sel = s;
            }
            // The base shape's op is ignored; later shapes pick how they combine.
            if s > 0 {
                let mut op = history.current().locals[local]
                    .mask
                    .ops
                    .get(s)
                    .copied()
                    .unwrap_or(MaskOp::Add);
                let before = op;
                egui::ComboBox::from_id_salt(("mask_op", local, s))
                    .selected_text(op_label(op))
                    .show_ui(ui, |ui| {
                        for choice in [MaskOp::Add, MaskOp::Subtract, MaskOp::Intersect] {
                            ui.selectable_value(&mut op, choice, op_label(choice));
                        }
                    });
                if op != before {
                    history.begin();
                    let ops = &mut history.current_mut().locals[local].mask.ops;
                    // Keep ops parallel before indexing (older masks may be short).
                    while ops.len() <= s {
                        ops.push(MaskOp::Add);
                    }
                    ops[s] = op;
                    history.commit();
                    dirty = true;
                }
            } else {
                ui.label("(base)");
            }
            // Remove this shape (and its op), keeping the two vectors parallel.
            if count > 1 && ui.button("✕").on_hover_text("Remove shape").clicked() {
                history.begin();
                let mask = &mut history.current_mut().locals[local].mask;
                mask.shapes.remove(s);
                if s < mask.ops.len() {
                    mask.ops.remove(s);
                }
                history.commit();
                dirty = true;
            }
        });
    }

    ui.horizontal_wrapped(|ui| {
        ui.label("Add:");
        for (label, kind) in [
            ("Gradient", ShapeKind::Gradient),
            ("Radial", ShapeKind::Radial),
            ("Luminosity", ShapeKind::Luminosity),
            ("Color", ShapeKind::ColorRange),
            ("Brush", ShapeKind::Brush),
        ] {
            if ui.button(label).clicked() {
                add_shape(history, local, kind, shape_sel);
                dirty = true;
            }
        }
    });

    // The remove above may have shrunk the list; re-clamp the selection.
    let now = history.current().locals[local].mask.shapes.len();
    clamp_selection(shape_sel, now.max(1));
    dirty
}

/// The combine-op label shown in the op selector.
fn op_label(op: MaskOp) -> &'static str {
    match op {
        MaskOp::Add => "Add",
        MaskOp::Subtract => "Subtract",
        MaskOp::Intersect => "Intersect",
    }
}

/// Sliders for the selected local adjustment's selected mask shape (gradient
/// endpoints or radial center/radius/feather, etc.), in normalized coordinates.
/// Each shape's sliders share one gesture scope, so editing a shape is one undo
/// step. `shape` is the index of the shape being edited within the mask.
fn local_shape_block(
    ui: &mut egui::Ui,
    history: &mut History<Settings>,
    i: usize,
    shape: usize,
) -> bool {
    match history.current().locals[i].mask.shapes.get(shape).cloned() {
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
                    move |s| match &s.locals[i].mask.shapes[shape] {
                        MaskShape::Gradient(g) => getc(g),
                        _ => default,
                    },
                    move |s, v| {
                        if let MaskShape::Gradient(g) = &mut s.locals[i].mask.shapes[shape] {
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
                    move |s| match &s.locals[i].mask.shapes[shape] {
                        MaskShape::Radial(r) => getc(r),
                        _ => default,
                    },
                    move |s, v| {
                        if let MaskShape::Radial(r) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::Luminosity(l) => l.lo,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::Luminosity(l) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::Luminosity(l) => l.hi,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::Luminosity(l) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::Luminosity(l) => l.feather,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::Luminosity(l) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::ColorRange(c) => c.hue,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::ColorRange(c) => c.hue_width,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::ColorRange(c) => c.sat_min,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[shape] {
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
                move |s| match &s.locals[i].mask.shapes[shape] {
                    MaskShape::ColorRange(c) => c.feather,
                    _ => 0.0,
                },
                move |s, v| {
                    if let MaskShape::ColorRange(c) = &mut s.locals[i].mask.shapes[shape] {
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
    use std::cell::RefCell;

    /// Run `f` once inside a headless egui frame, so a control builder can be
    /// exercised without a display. The closure may write through a `RefCell` it
    /// captures; `__run_test_ui` calls it once per frame.
    fn in_test_ui(f: impl Fn(&mut egui::Ui)) {
        egui::__run_test_ui(f);
    }

    #[test]
    fn set_curves_enabled_round_trips_as_one_step() {
        // Enabling installs the identity Curves; disabling clears it back to None;
        // each flip is exactly one undo step. Same shape for the global panel and a
        // local — here exercised on the global.
        let mut h = History::new(Settings::default());
        let access = GlobalAccess;
        assert!(h.current().global.curves.is_none(), "off by default");
        assert!(set_curves_enabled(&mut h, access, true));
        assert_eq!(
            h.current().global.curves,
            Some(Curves::default()),
            "enable installs the identity"
        );
        assert_eq!(h.undo_len(), 1, "enabling is one step");
        assert!(set_curves_enabled(&mut h, access, false));
        assert!(h.current().global.curves.is_none(), "disable clears it");
        assert_eq!(h.undo_len(), 2, "disabling is a second step");
    }

    #[test]
    fn set_hsl_enabled_round_trips_as_one_step() {
        let mut h = History::new(Settings::default());
        let access = GlobalAccess;
        assert!(set_hsl_enabled(&mut h, access, true));
        assert_eq!(h.current().global.hsl, Some(Hsl::default()));
        assert_eq!(h.undo_len(), 1);
        assert!(set_hsl_enabled(&mut h, access, false));
        assert!(h.current().global.hsl.is_none());
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn set_channel_mixer_enabled_round_trips_as_one_step() {
        let mut h = History::new(Settings::default());
        let access = GlobalAccess;
        assert!(set_channel_mixer_enabled(&mut h, access, true));
        assert_eq!(
            h.current().global.channel_mixer,
            Some(ChannelMixer::default()),
            "enable installs the identity mixer"
        );
        assert_eq!(h.undo_len(), 1);
        assert!(set_channel_mixer_enabled(&mut h, access, false));
        assert!(h.current().global.channel_mixer.is_none());
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn set_vignette_enabled_round_trips_as_one_step() {
        let mut h = History::new(Settings::default());
        assert!(set_vignette_enabled(&mut h, true));
        assert_eq!(
            h.current().geometry.vignette,
            Some(0.0),
            "enable installs the neutral amount"
        );
        assert_eq!(h.undo_len(), 1);
        assert!(set_vignette_enabled(&mut h, false));
        assert!(h.current().geometry.vignette.is_none());
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn local_blocks_do_not_render_or_edit_when_unchecked() {
        // On the local surface there is no eye button: an unchecked (None) effect
        // hides its controls and must edit nothing. Drive each inline-checkbox block
        // headlessly with the field off and assert it stays off with no undo step —
        // the body is gated on the checkbox, so it never runs (and never panics on a
        // None field).
        let h = RefCell::new(History::new(Settings::default()));
        let channel = RefCell::new(0_usize);
        let access = GlobalAccess;
        in_test_ui(|ui| {
            let mut hist = h.borrow_mut();
            let mut ch = channel.borrow_mut();
            assert!(!curves_block(ui, &mut hist, &mut ch, access));
            assert!(!hsl_block(ui, &mut hist, access));
            assert!(!channel_mixer_block(ui, &mut hist, access));
        });
        let hist = h.borrow();
        assert!(hist.current().global.curves.is_none(), "curves stay off");
        assert!(hist.current().global.hsl.is_none(), "hsl stays off");
        assert!(
            hist.current().global.channel_mixer.is_none(),
            "mixer stays off"
        );
        assert_eq!(hist.undo_len(), 0, "an unchecked local edits nothing");
    }

    #[test]
    fn disabled_bodies_render_default_without_panicking_or_editing() {
        // The global panel renders the body whenever the eye says "shown", even when
        // the effect is disabled (None). With no user interaction the default-valued
        // controls draw and write nothing — and reading a None field as its default
        // must not panic. Drives each body on a None field.
        let h = RefCell::new(History::new(Settings::default()));
        let channel = RefCell::new(0_usize);
        let access = GlobalAccess;
        in_test_ui(|ui| {
            let mut hist = h.borrow_mut();
            let mut ch = channel.borrow_mut();
            assert!(!curves_body(ui, &mut hist, &mut ch, access));
            assert!(!hsl_body(ui, &mut hist, access));
            assert!(!channel_mixer_body(ui, &mut hist, access));
            assert!(!vignette_body(ui, &mut hist));
        });
        let hist = h.borrow();
        assert!(hist.current().global.curves.is_none());
        assert!(hist.current().global.hsl.is_none());
        assert!(hist.current().global.channel_mixer.is_none());
        assert!(hist.current().geometry.vignette.is_none());
        assert_eq!(
            hist.undo_len(),
            0,
            "a shown-but-disabled body edits nothing"
        );
    }

    #[test]
    fn add_shape_keeps_ops_parallel_and_appends() {
        // Appending a shape pushes a parallel default Add op and selects the new
        // shape, keeping `ops.len() == shapes.len()` — the invariant `weight_at`
        // relies on. The base shape's op is present but ignored by the engine.
        let mut h = History::new(Settings {
            locals: vec![LocalAdjustment {
                mask: Mask {
                    shapes: vec![default_shape(ShapeKind::Radial)],
                    ops: vec![MaskOp::Add],
                    invert: false,
                },
                adjustments: Adjustments::default(),
                opacity: 1.0,
            }],
            ..Settings::default()
        });
        let mut shape_sel = 0;
        add_shape(&mut h, 0, ShapeKind::Luminosity, &mut shape_sel);
        let mask = &h.current().locals[0].mask;
        assert_eq!(mask.shapes.len(), 2, "shape appended");
        assert_eq!(mask.ops.len(), mask.shapes.len(), "ops stay parallel");
        assert_eq!(shape_sel, 1, "new shape selected");
        assert!(matches!(mask.shapes[1], MaskShape::Luminosity(_)));
        // One undo removes the whole append (one step).
        assert!(h.undo());
        assert_eq!(h.current().locals[0].mask.shapes.len(), 1);
    }

    #[test]
    fn local_access_binds_writes_to_the_right_local() {
        // The `LocalAccess` get/set reach into `locals[i].adjustments`, not the
        // global — so a block bound to a local edits only that local's catalog,
        // proving the reusable surface is correctly parameterized.
        let mut s = Settings {
            locals: vec![LocalAdjustment::default(), LocalAdjustment::default()],
            ..Settings::default()
        };
        let access = LocalAccess { index: 1 };
        access.get_mut(&mut s).exposure = Some(1.5);
        assert_eq!(s.locals[1].adjustments.exposure, Some(1.5), "local 1 set");
        assert_eq!(s.locals[0].adjustments.exposure, None, "local 0 untouched");
        assert_eq!(s.global.exposure, None, "global untouched");
        assert_eq!(access.get(&s).exposure, Some(1.5));
        // GlobalAccess reaches the global catalog.
        GlobalAccess.get_mut(&mut s).saturation = Some(0.8);
        assert_eq!(s.global.saturation, Some(0.8));
        assert_eq!(s.locals[1].adjustments.saturation, None);
    }

    #[test]
    fn opt_reset_yields_none() {
        // Resetting an optional field yields the off (None) default. (Whether a
        // value differs from default is shown by the section reset button's enabled
        // state, not a per-control predicate.)
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
    }

    #[test]
    fn crop_px_round_trips_through_normalized() {
        // A whole-pixel value survives the px → normalized → px round-trip exactly,
        // for both axes (Left/Width use the width, Top/Height the height).
        let (w, h) = (4000_u32, 3000_u32);
        for (px, dim) in [(1000.0, w), (2500.0, h), (0.0, w), (4000.0, w)] {
            let norm = px_to_norm(px, dim);
            assert_eq!(
                norm_to_px(norm, dim),
                px,
                "px {px} round-trips on dim {dim}"
            );
        }
        // 25% of a 4000px width is pixel 1000; half of a 3000px height is 1500.
        assert_eq!(norm_to_px(0.25, w), 1000.0);
        assert_eq!(norm_to_px(0.5, h), 1500.0);
    }

    #[test]
    fn crop_px_clamps_out_of_range_and_handles_zero_dim() {
        let dim = 2000_u32;
        // An entered pixel past the dimension clamps the normalized value to 1.0,
        // and a negative entry clamps to 0.0.
        assert_eq!(px_to_norm(5000.0, dim), 1.0);
        assert_eq!(px_to_norm(-50.0, dim), 0.0);
        // A degenerate zero-pixel dimension can't divide, so it maps to 0.
        assert_eq!(px_to_norm(123.0, 0), 0.0);
        assert_eq!(norm_to_px(0.5, 0), 0.0);
    }

    #[test]
    fn crop_pct_round_trips_and_clamps() {
        // A percent value survives the % → normalized → % round-trip exactly.
        for pct in [0.0_f32, 12.5, 50.0, 100.0] {
            let norm = pct_to_norm(pct);
            assert!(
                (norm_to_pct(norm) - pct).abs() < 1e-4,
                "pct {pct} round-trips"
            );
        }
        // normalized × 100 is the percent; editing past the ends clamps to [0, 1].
        assert_eq!(norm_to_pct(0.25), 25.0);
        assert_eq!(pct_to_norm(150.0), 1.0);
        assert_eq!(pct_to_norm(-10.0), 0.0);
    }

    #[test]
    fn crop_unit_toggles_between_px_and_pct() {
        // The unit defaults to pixels and the toggle flips it back and forth.
        assert_eq!(CropUnit::default(), CropUnit::Px);
        assert_eq!(CropUnit::Px.toggled(), CropUnit::Pct);
        assert_eq!(CropUnit::Pct.toggled(), CropUnit::Px);
        assert_eq!(CropUnit::Px.label(), "px");
        assert_eq!(CropUnit::Pct.label(), "%");
    }
}
