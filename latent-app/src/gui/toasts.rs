//! A small egui-native transient-notification queue: stacking, auto-dismissing
//! "toasts" for one-shot outcomes (an export finished, a save failed, a backend
//! fallback). Steady-state ("Rendering…", the active backend, "Saved") stays in
//! the status bar; this queue is only for things that *happen*.
//!
//! The queue logic is pure: pushing records a toast with a creation time and a
//! per-kind time-to-live, and [`Toasts::retain_unexpired`] drops the expired ones
//! given the current clock — so expiry is unit-testable without an egui
//! `Context`. The per-frame [`Toasts::ui`] only draws the surviving toasts and
//! schedules the next repaint so the stack animates without a busy-loop.

use eframe::egui;
use egui::{Align2, Color32, FontId, Frame, Order, RichText};

use super::theme;

/// How long an informational/success toast stays up, in seconds.
const INFO_TTL: f32 = 3.5;
/// How long an error toast stays up — longer, since a failure the user should
/// actually notice shouldn't vanish in a couple of seconds.
const ERROR_TTL: f32 = 8.0;
/// The fade-out duration at the end of a toast's life, in seconds.
const FADE: f32 = 0.4;
/// At most this many toasts are drawn at once; older ones beyond the cap are
/// still kept in the queue (and surface as the visible ones expire) but a burst
/// never fills the screen.
const MAX_VISIBLE: usize = 4;

/// The severity of a toast, which picks its accent color and time-to-live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastKind {
    /// A successful outcome (export written).
    Success,
    /// A neutral notice (a backend switch, a non-error fallback).
    Info,
    /// A failure the user should see (export failed, save failed).
    Error,
}

impl ToastKind {
    /// The time-to-live for this kind, in seconds.
    fn ttl(self) -> f32 {
        match self {
            ToastKind::Error => ERROR_TTL,
            ToastKind::Success | ToastKind::Info => INFO_TTL,
        }
    }

    /// The accent stripe color for this kind.
    fn color(self) -> Color32 {
        match self {
            ToastKind::Success => theme::SUCCESS,
            ToastKind::Info => theme::ACCENT,
            ToastKind::Error => theme::ERROR,
        }
    }
}

/// One transient notification: its severity, message, the time it was created,
/// and how long it lives.
#[derive(Debug, Clone)]
pub(crate) struct Toast {
    kind: ToastKind,
    text: String,
    /// `ctx.input(|i| i.time)` at creation — the same clock `retain_unexpired`
    /// and the fade animation compare against.
    shown_at: f64,
    /// Seconds this toast lives before it is dropped.
    ttl: f32,
}

impl Toast {
    /// Whether this toast is still alive at `now` (its age is within its ttl).
    fn alive(&self, now: f64) -> bool {
        (now - self.shown_at) <= self.ttl as f64
    }

    /// The fade opacity `[0, 1]` at `now`: full until the last [`FADE`] seconds,
    /// then ramping to zero so the toast dissolves rather than blinking out.
    fn opacity(&self, now: f64) -> f32 {
        let remaining = self.ttl as f64 - (now - self.shown_at);
        if remaining >= FADE as f64 {
            1.0
        } else {
            (remaining / FADE as f64).clamp(0.0, 1.0) as f32
        }
    }
}

/// The toast queue, newest pushed to the back and drawn at the top of the stack.
#[derive(Default)]
pub(crate) struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    /// Push a toast of `kind` carrying `text`, stamped with `now` (the egui clock
    /// at push time). Pure so a test can drive the queue without a `Context`.
    pub(crate) fn push_at(&mut self, kind: ToastKind, text: impl Into<String>, now: f64) {
        self.items.push(Toast {
            kind,
            text: text.into(),
            shown_at: now,
            ttl: kind.ttl(),
        });
    }

    /// Drop every toast whose life has elapsed at `now`, keeping the rest in
    /// order. Pure (no `Context`), so toast expiry is unit-testable.
    pub(crate) fn retain_unexpired(&mut self, now: f64) {
        self.items.retain(|t| t.alive(now));
    }

    /// Whether any toast is still queued — the per-frame draw uses this to keep
    /// requesting repaints so the stack animates and expires on time.
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Expire and draw the queue once this frame: a bottom-right stack of panels,
    /// newest on top, each tinted by its kind and fading out at the end of its
    /// life. Schedules the next repaint so the animation runs without a busy-loop.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.retain_unexpired(now);
        if self.is_empty() {
            return;
        }

        // Draw the newest few, newest at the top of the stack.
        let visible: Vec<Toast> = self.items.iter().rev().take(MAX_VISIBLE).cloned().collect();

        let screen = ctx.screen_rect();
        let margin = 12.0;
        let mut y = screen.max.y - margin;
        let mut soonest_repaint = f32::INFINITY;
        for (i, toast) in visible.iter().enumerate() {
            let opacity = toast.opacity(now);
            let area = egui::Area::new(egui::Id::new(("toast", i)))
                .order(Order::Foreground)
                .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-margin, y - screen.max.y));
            area.show(ctx, |ui| {
                ui.set_max_width(360.0);
                let stripe = toast.kind.color().gamma_multiply(opacity);
                let bg = Color32::from_rgba_unmultiplied(28, 28, 28, (235.0 * opacity) as u8);
                Frame::default()
                    .fill(bg)
                    .stroke(egui::Stroke::new(1.5, stripe))
                    .corner_radius(theme::CORNER_RADIUS)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        let text_col = Color32::from_gray(225).gamma_multiply(opacity);
                        ui.label(
                            RichText::new(&toast.text)
                                .color(text_col)
                                .font(FontId::proportional(14.0)),
                        );
                    });
            });
            // Stack upward: subtract this toast's height plus a gap for the next.
            let height = ctx
                .memory(|m| m.area_rect(egui::Id::new(("toast", i))).map(|r| r.height()))
                .unwrap_or(40.0);
            y -= height + 8.0;

            // Repaint when this toast next needs a visual change (fade start or
            // expiry), whichever is sooner.
            let age = (now - toast.shown_at) as f32;
            let until_fade = (toast.ttl - FADE - age).max(0.0);
            let until_expire = (toast.ttl - age).max(0.0);
            soonest_repaint = soonest_repaint.min(if until_fade > 0.0 {
                until_fade
            } else {
                until_expire
            });
        }
        if soonest_repaint.is_finite() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(soonest_repaint));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_queue_expires_after_ttl() {
        // A toast pushed at t=0 survives the expiry pass until its ttl elapses,
        // then is removed; a fresher one pushed later is kept across the same pass.
        let mut toasts = Toasts::default();
        toasts.push_at(ToastKind::Info, "first", 0.0);
        // Before the ttl: still present.
        toasts.retain_unexpired(1.0);
        assert_eq!(toasts.items.len(), 1, "an unexpired toast survives");

        // A second toast pushed just before the first expires.
        toasts.push_at(ToastKind::Info, "second", INFO_TTL as f64 - 0.5);

        // Advance past the first's ttl but not the second's: only the first drops.
        toasts.retain_unexpired(INFO_TTL as f64 + 0.1);
        assert_eq!(
            toasts.items.len(),
            1,
            "the expired toast is removed, the fresh one survives"
        );
        assert_eq!(toasts.items[0].text, "second");

        // Advance past the second's ttl too: the queue empties.
        toasts.retain_unexpired(2.0 * INFO_TTL as f64);
        assert!(toasts.is_empty(), "all expired toasts are removed");
    }

    #[test]
    fn error_toasts_live_longer_than_info() {
        // An error toast should outlive an info toast so a failure the user must
        // see doesn't vanish as quickly as a routine notice.
        assert!(ToastKind::Error.ttl() > ToastKind::Info.ttl());
        assert_eq!(ToastKind::Success.ttl(), ToastKind::Info.ttl());
    }

    #[test]
    fn opacity_full_then_fades_to_zero() {
        // Opacity is full for most of the life and ramps to zero over the last
        // `FADE` seconds (a dissolve, not a blink-out).
        let toast = Toast {
            kind: ToastKind::Info,
            text: "x".to_owned(),
            shown_at: 0.0,
            ttl: 4.0,
        };
        assert_eq!(toast.opacity(0.0), 1.0, "full at birth");
        assert_eq!(toast.opacity(3.0), 1.0, "still full before the fade window");
        // Halfway through the fade window (ttl - FADE/2) ≈ half opacity.
        let half = 4.0 - FADE as f64 / 2.0;
        assert!((toast.opacity(half) - 0.5).abs() < 0.05, "mid-fade ~ 0.5");
        assert_eq!(toast.opacity(4.0), 0.0, "zero at end of life");
    }
}
