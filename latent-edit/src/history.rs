//! Undo/redo history over an edit value.

use std::collections::VecDeque;

/// Undo/redo history over a value (e.g. a [`crate::Settings`]).
///
/// An edit is a transaction: [`begin`](Self::begin) snapshots the state before a
/// gesture and [`commit`](Self::commit) records an undo step **only if the state
/// actually changed** — so a gesture that nets no change (a slider dragged back
/// to where it started, or the same value re-entered) creates no history.
/// Calling `begin`/`commit` once per gesture (not per frame) keeps a drag a
/// single undo step.
///
/// The undo stack is **bounded**: each step deep-clones the whole value (every
/// `Vec<LocalAdjustment>`, every brush-dab list), so an unbounded stack is a slow
/// memory leak over a long session. Past [`capacity`](Self::with_capacity) steps
/// the oldest is evicted, so only the most-recent `capacity` edits stay undoable.
/// The redo stack needs no cap: it is cleared on every new committed edit, so it
/// can never exceed the undo depth.
#[derive(Debug, Clone)]
pub struct History<T> {
    current: T,
    undo: VecDeque<T>,
    redo: Vec<T>,
    /// The most-recent edits to retain; older steps are evicted on `commit`.
    capacity: usize,
    /// Pre-gesture snapshot, set by `begin`, consumed by `commit`.
    pending: Option<T>,
}

impl<T: Clone + PartialEq> History<T> {
    /// The default number of undo steps retained (see [`Self::with_capacity`]).
    pub const DEFAULT_CAP: usize = 100;

    /// A history with the [default capacity](Self::DEFAULT_CAP).
    pub fn new(initial: T) -> Self {
        Self::with_capacity(initial, Self::DEFAULT_CAP)
    }

    /// A history that retains at least the most-recent `capacity` undo steps.
    ///
    /// `capacity` is floored at `1` (a zero cap would discard every step the
    /// moment it is recorded, making undo useless).
    pub fn with_capacity(initial: T, capacity: usize) -> Self {
        Self {
            current: initial,
            undo: VecDeque::new(),
            redo: Vec::new(),
            capacity: capacity.max(1),
            pending: None,
        }
    }

    pub fn current(&self) -> &T {
        &self.current
    }

    pub fn current_mut(&mut self) -> &mut T {
        &mut self.current
    }

    /// Begin an edit gesture: snapshot the current state (once) so `commit` can
    /// tell whether anything changed. A no-op if a gesture is already open.
    pub fn begin(&mut self) {
        if self.pending.is_none() {
            self.pending = Some(self.current.clone());
        }
    }

    /// End an edit gesture: if the state changed since `begin`, record an undo
    /// step and clear the redo branch; otherwise discard the snapshot so a no-op
    /// gesture leaves no trace.
    ///
    /// Eviction happens strictly after a step is recorded, so it never perturbs
    /// the no-op path or the redo invalidation: only a real recorded edit can
    /// push the stack over capacity, and only then is the oldest step dropped.
    pub fn commit(&mut self) {
        if let Some(prev) = self.pending.take()
            && prev != self.current
        {
            self.undo.push_back(prev);
            self.redo.clear();
            while self.undo.len() > self.capacity {
                self.undo.pop_front();
            }
        }
    }

    /// Restore the previous checkpoint; returns false if there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop_back() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut self.current, prev));
                true
            }
            None => false,
        }
    }

    /// Re-apply an undone state; returns false if there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo
                    .push_back(std::mem::replace(&mut self.current, next));
                true
            }
            None => false,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// True when no edit gesture is in progress (nothing pending a commit).
    /// Used to auto-save only after a gesture completes, not mid-drag.
    pub fn is_idle(&self) -> bool {
        self.pending.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: apply one committed edit.
    fn edit(h: &mut History<i32>, value: i32) {
        h.begin();
        *h.current_mut() = value;
        h.commit();
    }

    #[test]
    fn undo_redo_round_trips() {
        let mut h = History::new(0);
        assert!(!h.can_undo() && !h.can_redo());

        edit(&mut h, 1);
        edit(&mut h, 2);
        assert_eq!(*h.current(), 2);

        assert!(h.undo());
        assert_eq!(*h.current(), 1);
        assert!(h.undo());
        assert_eq!(*h.current(), 0);
        assert!(!h.undo()); // nothing left

        assert!(h.redo());
        assert_eq!(*h.current(), 1);
        assert!(h.redo());
        assert_eq!(*h.current(), 2);
    }

    #[test]
    fn a_new_edit_clears_the_redo_branch() {
        let mut h = History::new(0);
        edit(&mut h, 1);
        h.undo(); // back to 0, redo has [1]
        assert!(h.can_redo());

        edit(&mut h, 9); // new edit
        assert!(!h.can_redo()); // redo branch discarded
    }

    #[test]
    fn an_empty_history_undoes_and_redoes_to_nothing() {
        // A fresh history has nothing to undo or redo: both return false, no panic.
        let mut h = History::new(0);
        assert!(!h.undo());
        assert!(!h.redo());
        assert_eq!(*h.current(), 0);
    }

    #[test]
    fn history_caps_undo_depth() {
        // Commit more distinct edits than the default cap. The stack must stay
        // bounded, and undoing all the way back lands on the value that was
        // current `DEFAULT_CAP` steps ago — not the original, whose step was
        // evicted as the oldest.
        let cap = History::<i32>::DEFAULT_CAP;
        let mut h = History::new(0);
        let extra = 5;
        for v in 1..=(cap + extra) as i32 {
            edit(&mut h, v);
        }
        assert_eq!(*h.current(), (cap + extra) as i32);

        // Undo as far as possible; count the steps to confirm the bound.
        let mut steps = 0;
        while h.undo() {
            steps += 1;
        }
        assert_eq!(steps, cap, "exactly the most-recent `cap` steps remain");
        // The oldest `extra` steps were evicted, so we cannot reach `0`; the
        // earliest reachable state is the value that was current `cap` steps back.
        assert_eq!(*h.current(), extra as i32);
    }

    #[test]
    fn with_capacity_respects_a_custom_cap() {
        let mut h = History::with_capacity(0, 3);
        for v in 1..=10 {
            edit(&mut h, v);
        }
        let mut steps = 0;
        while h.undo() {
            steps += 1;
        }
        assert_eq!(steps, 3, "custom cap bounds the stack");
        // Three steps back from 10 (the last recorded `prev` values are 9, 8, 7).
        assert_eq!(*h.current(), 7);

        // A zero cap is floored to 1, so at least the last step stays undoable.
        let mut floored = History::with_capacity(0, 0);
        edit(&mut floored, 1);
        edit(&mut floored, 2);
        assert!(floored.undo());
        assert_eq!(*floored.current(), 1);
        assert!(!floored.undo(), "only one step retained at the floored cap");
    }

    #[test]
    fn a_gesture_with_no_net_change_records_nothing() {
        let mut h = History::new(5);

        // Begin, move away, move back to the start, commit → no undo step.
        h.begin();
        *h.current_mut() = 8;
        *h.current_mut() = 5;
        h.commit();
        assert!(!h.can_undo());

        // Begin/commit with no change at all → nothing recorded.
        h.begin();
        h.commit();
        assert!(!h.can_undo());
    }
}
