//! Alignment scene query dispatch lives in `rgv_session::alignment::dispatch`; the desktop is
//! its [`PresentationHost`](rgv_session::presentation_host::PresentationHost).

use crate::Viewer;
use rgv_core::PanelId;

impl Viewer {
    /// Begins the sole liveness transaction for one panel/track target; see
    /// [`rgv_session::Session::queue_alignment_navigation_for_track`].
    pub(crate) fn queue_alignment_navigation_for_track(
        &mut self,
        track_index: usize,
        panel_id: PanelId,
        advance_generation: bool,
        reason: &'static str,
    ) {
        self.session.queue_alignment_navigation_for_track(
            track_index,
            panel_id,
            advance_generation,
            reason,
        );
    }

    /// Sends the UI's current camera to the unified source actor and changes no pixels itself;
    /// see [`rgv_session::alignment::dispatch::dispatch_alignment_query`].
    pub(crate) fn dispatch_alignment_query(&mut self, panel_id: PanelId) {
        rgv_session::alignment::dispatch::dispatch_alignment_query(self, panel_id);
    }
}
