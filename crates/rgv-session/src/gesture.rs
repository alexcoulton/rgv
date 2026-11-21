//! One navigation-history entry per gesture (UI-3).

use rgv_core::PanelId;
use std::time::{Duration, Instant};

/// Quiet period after which a burst of wheel ticks, key repeats or slider changes is one gesture.
pub const NAVIGATION_GESTURE_DEBOUNCE: Duration = Duration::from_millis(600);

/// Tracks a continuous navigation gesture whose final viewport has not yet been recorded in the
/// navigation history or the recent-locus list.
#[derive(Debug, Default)]
pub struct NavigationGestureCommit {
    pending: Option<(PanelId, Instant)>,
}

impl NavigationGestureCommit {
    /// Note one more step of a gesture in `panel`. Returns a different panel whose gesture must
    /// be committed first, because a gesture belongs to exactly one panel.
    pub fn touch(&mut self, panel: PanelId, now: Instant) -> Option<PanelId> {
        let previous = self
            .pending
            .map(|(pending, _)| pending)
            .filter(|pending| *pending != panel);
        self.pending = Some((panel, now + NAVIGATION_GESTURE_DEBOUNCE));
        previous
    }

    /// The gesture ended explicitly (pointer or key released): commit at the next opportunity.
    pub fn end_now(&mut self, now: Instant) {
        if let Some((_, deadline)) = &mut self.pending {
            *deadline = now;
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending.map(|(_, deadline)| deadline)
    }

    /// Takes the panel to commit when the quiet period has elapsed and no pointer gesture is
    /// still being held.
    pub fn take_due(&mut self, now: Instant, pointer_gesture_active: bool) -> Option<PanelId> {
        match self.pending {
            Some((panel, deadline)) if !pointer_gesture_active && deadline <= now => {
                self.pending = None;
                Some(panel)
            }
            _ => None,
        }
    }

    /// Takes the pending panel unconditionally, before a discrete navigation is recorded.
    pub fn take(&mut self) -> Option<PanelId> {
        self.pending.take().map(|(panel, _)| panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_steps_is_one_gesture_committed_after_the_quiet_period() {
        let mut commit = NavigationGestureCommit::default();
        let panel = PanelId::new(1);
        let start = Instant::now();
        for tick in 0..20 {
            assert_eq!(
                commit.touch(panel, start + Duration::from_millis(tick * 30)),
                None
            );
            assert_eq!(
                commit.take_due(start + Duration::from_millis(tick * 30 + 29), false),
                None,
                "no entry is recorded between steps of one gesture"
            );
        }
        let last = start + Duration::from_millis(19 * 30);
        assert_eq!(commit.deadline(), Some(last + NAVIGATION_GESTURE_DEBOUNCE));
        assert_eq!(
            commit.take_due(last + NAVIGATION_GESTURE_DEBOUNCE, false),
            Some(panel)
        );
        assert_eq!(commit.take_due(last + Duration::from_secs(10), false), None);
        assert_eq!(commit.deadline(), None);
    }

    #[test]
    fn held_pointer_gestures_commit_on_release_not_on_a_pause() {
        let mut commit = NavigationGestureCommit::default();
        let panel = PanelId::new(3);
        let start = Instant::now();
        commit.touch(panel, start);
        let paused = start + Duration::from_secs(5);
        assert_eq!(commit.take_due(paused, true), None);
        commit.end_now(paused);
        assert_eq!(commit.take_due(paused, false), Some(panel));
    }

    #[test]
    fn a_gesture_in_another_panel_and_discrete_navigation_flush_the_pending_one() {
        let mut commit = NavigationGestureCommit::default();
        let now = Instant::now();
        assert_eq!(commit.touch(PanelId::new(1), now), None);
        assert_eq!(commit.touch(PanelId::new(2), now), Some(PanelId::new(1)));
        assert_eq!(commit.take(), Some(PanelId::new(2)));
        assert_eq!(commit.take(), None);
        commit.end_now(now);
        assert_eq!(commit.deadline(), None);
    }
}
