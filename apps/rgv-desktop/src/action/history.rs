//! The undo history: a bounded stack of inverse actions.
//!
//! Each entry holds the actions that were applied and the actions that put the document back.
//! Inverse actions were chosen over whole-document snapshots because restoring a snapshot means
//! re-opening every source: undoing a hidden track would rebuild every engine and drop every
//! cache. An inverse touches only what the action touched. Where the natural inverse is "put
//! this small piece of state back", the inverse action carries that piece (the track
//! arrangement, a closed panel's identity and locus, a removed track's configuration), never an
//! engine handle, so an entry that falls off the stack releases nothing but memory.
//!
//! Navigation is not recorded. Each panel already has Back/Forward with one entry per gesture;
//! recording pans and zooms here as well would bury every edit under viewport noise and make
//! Undo and Back disagree about what "the previous place" is.

use super::Action;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Most undo steps kept. Entries are a few hundred bytes to a few kilobytes each.
pub(crate) const MAXIMUM_UNDO_ENTRIES: usize = 128;

/// Edits of the same kind and target closer together than this are one gesture: a divider
/// drag reports a new height every frame and must be one undo step.
pub(crate) const UNDO_COALESCE_WINDOW: Duration = Duration::from_millis(800);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UndoEntry {
    /// Shown in the Edit menu.
    pub(crate) label: &'static str,
    /// What was applied, in order.
    pub(crate) redo: Vec<Action>,
    /// What puts the document back, in order.
    pub(crate) undo: Vec<Action>,
    /// Identity of the gesture this edit may belong to; `None` never coalesces.
    pub(crate) coalesce: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct UndoHistory {
    undo: VecDeque<(UndoEntry, Option<Instant>)>,
    redo: Vec<UndoEntry>,
}

impl UndoHistory {
    /// Records a fresh edit. Any redo branch is discarded: the document has moved on.
    pub(crate) fn record(&mut self, entry: UndoEntry, now: Instant) {
        self.redo.clear();
        self.push(entry, now);
    }

    /// Records a redone edit, keeping the rest of the redo branch.
    pub(crate) fn record_redone(&mut self, mut entry: UndoEntry) {
        // A redone step is complete; a later edit of the same kind is a new step.
        entry.coalesce = None;
        self.push_bounded(entry, None);
    }

    fn push(&mut self, entry: UndoEntry, now: Instant) {
        if let Some((previous, Some(at))) = self.undo.back_mut()
            && entry.coalesce.is_some()
            && previous.coalesce == entry.coalesce
            && now.saturating_duration_since(*at) <= UNDO_COALESCE_WINDOW
        {
            // Same gesture: keep the oldest "before", take the newest "after".
            previous.redo = entry.redo;
            *at = now;
            return;
        }
        self.push_bounded(entry, Some(now));
    }

    fn push_bounded(&mut self, entry: UndoEntry, at: Option<Instant>) {
        if self.undo.len() == MAXIMUM_UNDO_ENTRIES {
            self.undo.pop_front();
        }
        self.undo.push_back((entry, at));
    }

    pub(crate) fn take_undo(&mut self) -> Option<UndoEntry> {
        self.undo.pop_back().map(|(entry, _)| entry)
    }

    pub(crate) fn push_redo(&mut self, entry: UndoEntry) {
        self.redo.push(entry);
    }

    pub(crate) fn take_redo(&mut self) -> Option<UndoEntry> {
        self.redo.pop()
    }

    pub(crate) fn undo_label(&self) -> Option<&'static str> {
        self.undo.back().map(|(entry, _)| entry.label)
    }

    pub(crate) fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|entry| entry.label)
    }

    /// Forgets everything: the document was replaced (new workspace, session opened).
    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    #[cfg(test)]
    pub(crate) fn undo_len(&self) -> usize {
        self.undo.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::PanelId;

    fn entry(panel: u64, coalesce: Option<&str>) -> UndoEntry {
        UndoEntry {
            label: "move panel",
            redo: vec![Action::MovePanel {
                panel_id: PanelId::new(panel),
                index: 1,
            }],
            undo: vec![Action::MovePanel {
                panel_id: PanelId::new(panel),
                index: 0,
            }],
            coalesce: coalesce.map(str::to_owned),
        }
    }

    #[test]
    fn the_stack_is_bounded_and_drops_the_oldest_step() {
        let mut history = UndoHistory::default();
        let now = Instant::now();
        for panel in 1..=(MAXIMUM_UNDO_ENTRIES as u64 + 5) {
            history.record(entry(panel, None), now);
        }
        assert_eq!(history.undo_len(), MAXIMUM_UNDO_ENTRIES);
        let newest = history.take_undo().unwrap();
        assert_eq!(newest, entry(MAXIMUM_UNDO_ENTRIES as u64 + 5, None));
    }

    #[test]
    fn one_gesture_is_one_step_and_keeps_its_first_before_state() {
        let mut history = UndoHistory::default();
        let start = Instant::now();
        let mut first = entry(1, Some("resize:7"));
        first.undo = vec![Action::ActivatePanel {
            panel_id: PanelId::new(9),
        }];
        history.record(first.clone(), start);
        history.record(
            entry(2, Some("resize:7")),
            start + Duration::from_millis(16),
        );
        history.record(
            entry(3, Some("resize:7")),
            start + Duration::from_millis(32),
        );
        assert_eq!(history.undo_len(), 1);
        let merged = history.take_undo().unwrap();
        assert_eq!(
            merged.undo, first.undo,
            "undo returns to before the gesture"
        );
        assert_eq!(
            merged.redo,
            entry(3, None).redo,
            "redo lands where it ended"
        );
    }

    #[test]
    fn a_pause_or_a_different_target_starts_a_new_step() {
        let mut history = UndoHistory::default();
        let start = Instant::now();
        history.record(entry(1, Some("resize:7")), start);
        history.record(
            entry(2, Some("resize:7")),
            start + UNDO_COALESCE_WINDOW + Duration::from_millis(1),
        );
        history.record(entry(3, Some("resize:8")), start + UNDO_COALESCE_WINDOW);
        history.record(entry(4, None), start + UNDO_COALESCE_WINDOW);
        assert_eq!(history.undo_len(), 4);
    }

    #[test]
    fn a_new_edit_discards_the_redo_branch_but_a_redone_edit_keeps_it() {
        let mut history = UndoHistory::default();
        let now = Instant::now();
        history.record(entry(1, None), now);
        history.record(entry(2, None), now);
        let second = history.take_undo().unwrap();
        history.push_redo(second);
        let first = history.take_undo().unwrap();
        history.push_redo(first);
        assert_eq!(history.redo_label(), Some("move panel"));

        let redone = history.take_redo().unwrap();
        history.record_redone(redone);
        assert!(
            history.take_redo().is_some(),
            "the later redo step survives"
        );

        history.push_redo(entry(2, None));
        history.record(entry(5, None), now);
        assert!(history.take_redo().is_none());
    }
}
