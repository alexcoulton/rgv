//! The display-invalidation flag. Diagnostics observe every mark and clear, but the flag itself
//! is core state and is maintained whether or not diagnostics are compiled in.

use crate::Viewer;

impl Viewer {
    /// Marks the generic display derivative stale. Repeated marks while already dirty are still
    /// reported to diagnostics: they reveal the feedback loop that can re-dirty a just-rebuilt
    /// display every frame.
    #[track_caller]
    pub(crate) fn mark_display_dirty(&mut self, reason: &'static str) {
        let was_already_dirty = self.display_dirty;
        self.display_dirty = true;
        self.diagnostics.display_marked_dirty(
            reason,
            std::panic::Location::caller(),
            was_already_dirty,
        );
    }

    /// Clears display invalidation only after the complete CPU/GPU display derivative has been
    /// rebuilt.
    #[track_caller]
    pub(crate) fn clear_display_dirty(&mut self, reason: &'static str) {
        let was_dirty = self.display_dirty;
        self.display_dirty = false;
        self.diagnostics
            .display_cleared_dirty(reason, std::panic::Location::caller(), was_dirty);
    }
}
