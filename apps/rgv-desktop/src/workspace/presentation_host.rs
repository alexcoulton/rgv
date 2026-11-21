//! The desktop as the session's [`PresentationHost`]: every GPU, display and diagnostics effect
//! session settlement asks for lands on the desktop's existing implementation of it.

use crate::Viewer;
use crate::workspace::panels::ViewPanelController;
use rgv_alignment_scene::{LedgerError, LedgerProgressEpoch};
use rgv_core::{PanelId, ReferenceDictionary, TrackId};
use rgv_hts::RemoteIoStats;
use rgv_session::Session;
use rgv_session::lane_layout::PanelLaneLayout;
use rgv_session::presentation_host::PresentationHost;

impl PresentationHost for Viewer {
    type Panel = ViewPanelController;

    fn session(&self) -> &Session<ViewPanelController> {
        &self.session
    }

    fn session_mut(&mut self) -> &mut Session<ViewPanelController> {
        &mut self.session
    }

    fn panel_lane_layout(&self, panel_id: PanelId) -> Option<PanelLaneLayout> {
        Self::panel_lane_layout(self, panel_id)
    }

    fn promoted_read_template(&self, track_id: TrackId) -> Option<u64> {
        self.promoted_read
            .filter(|(candidate, _)| *candidate == track_id)
            .map(|(_, hash)| hash)
    }

    fn prepare_alignment_scene_frame(&mut self) {
        Self::prepare_alignment_scene_frame(self);
    }

    fn clear_alignment_scene_lane(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Result<(), String> {
        match self.alignment_panel_scene_stores.get_mut(&panel_id) {
            Some(store) => store
                .clear_lane(track_id)
                .map(|_| ())
                .map_err(|error| error.to_string()),
            None => Ok(()),
        }
    }

    fn yield_speculation_for_source_pressure(
        &mut self,
        pressure: LedgerError,
    ) -> Result<LedgerProgressEpoch, LedgerError> {
        self.yield_alignment_gpu_speculation_for_source_pressure(pressure)
    }

    fn observe_capacity_reclaim(
        &mut self,
        track_id: TrackId,
        request_id: u64,
        released_bytes: usize,
        relinquished_cache_entries: usize,
    ) {
        self.observe_alignment_capacity_reclaim(
            track_id,
            request_id,
            released_bytes,
            relinquished_cache_entries,
        );
    }

    fn rebuild_all_panel_displays(&mut self) {
        Self::rebuild_all_panel_displays(self);
    }

    fn synchronize_alignment_scene_interactions(&mut self, panel_id: PanelId) {
        Self::synchronize_alignment_scene_interactions(self, panel_id);
    }

    #[track_caller]
    fn mark_display_dirty(&mut self, reason: &'static str) {
        Self::mark_display_dirty(self, reason);
    }

    fn request_redraw(&mut self) {
        self.window.request_redraw();
    }

    fn request_all_panel_queries(&mut self) {
        Self::request_all_panel_queries(self);
    }

    fn start_alignment_coverage_engine(&mut self, track_index: usize) {
        Self::start_alignment_coverage_engine(self, track_index);
    }

    fn finish_imported_locus(&mut self, feature_search: Option<String>) {
        Self::finish_imported_locus(self, feature_search);
    }

    fn update_landmark_resolution(&mut self, dictionary: &ReferenceDictionary) {
        self.update_landmark_resolution_from_dictionary(dictionary);
    }

    fn refresh_active_panel_context(&mut self) {
        Self::refresh_active_panel_context(self);
    }

    fn persist_settings(&mut self) {
        Self::persist_settings(self);
    }

    fn observe_reference_remote_io(&mut self, stats: Option<RemoteIoStats>) {
        self.reference_remote_io_stats = stats;
    }

    fn report_error(&mut self, message: String) {
        Self::report_error(self, message);
    }

    fn report_warning(&mut self, message: String) {
        Self::report_warning(self, message);
    }

    fn diagnostics_recording(&self) -> bool {
        self.diagnostics.recording()
    }

    fn diagnostic_event(&mut self, category: &str, name: &str, fields: serde_json::Value) {
        self.diagnostics.event(category, name, fields);
    }

    fn alignment_filter_diagnostic_state(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> serde_json::Value {
        Self::alignment_filter_diagnostic_state(self, panel_id, track_id)
    }
}
