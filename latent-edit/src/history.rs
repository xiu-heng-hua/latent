//! Undo/redo history over an edit value.

/// Undo/redo history over a value (e.g. a [`crate::Settings`]).
///
/// An edit is a transaction: [`begin`](Self::begin) snapshots the state before a
/// gesture and [`commit`](Self::commit) records an undo step **only if the state
/// actually changed** — so a gesture that nets no change (a slider dragged back
/// to where it started, or the same value re-entered) creates no history.
/// Calling `begin`/`commit` once per gesture (not per frame) keeps a drag a
/// single undo step.
#[derive(Debug, Clone)]
pub struct History<T> {
    current: T,
    undo: Vec<T>,
    redo: Vec<T>,
    /// Pre-gesture snapshot, set by `begin`, consumed by `commit`.
    pending: Option<T>,
}

impl<T: Clone + PartialEq> History<T> {
    pub fn new(initial: T) -> Self {
        Self {
            current: initial,
            undo: Vec::new(),
            redo: Vec::new(),
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
    pub fn commit(&mut self) {
        if let Some(prev) = self.pending.take()
            && prev != self.current
        {
            self.undo.push(prev);
            self.redo.clear();
        }
    }

    /// Restore the previous checkpoint; returns false if there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
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
                self.undo.push(std::mem::replace(&mut self.current, next));
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
