//! The narrow interface between session-level settlement and whatever presents the session.
//!
//! Alignment query dispatch and settlement (`alignment::dispatch`, `alignment::events`,
//! `reference_events`) are session logic, but they interleave with work only a presenter can do:
//! preparing the GPU frame for the scenes just settled, yielding optional GPU residency to a
//! source under memory pressure, observing a cross-owner capacity reclaim, rebuilding display
//! lists, writing diagnostics. [`PresentationHost`] names exactly those effects. The desktop
//! implements it on its window and GPU state; [`RecordingHost`] is the headless implementation,
//! which presents nothing and records each call so tests can assert on the order of effects.
//!
//! The host owns the session (`session()` / `session_mut()`), as the desktop's `Viewer` does, so
//! settlement keeps the exact interleaving it had: every host call happens at the point in the
//! event stream where the desktop used to make it.

use crate::lane_layout::PanelLaneLayout;
use crate::panels::{Panel, PanelState};
use crate::session::Session;
use rgv_alignment_scene::{LedgerError, LedgerProgressEpoch};
use rgv_core::{PanelId, ReferenceDictionary, TrackId};
use rgv_hts::RemoteIoStats;

/// Effects settlement needs from the presenter, and the presenter's view of the layout.
pub trait PresentationHost {
    type Panel: Panel;

    fn session(&self) -> &Session<Self::Panel>;
    fn session_mut(&mut self) -> &mut Session<Self::Panel>;

    // ----- Layout ---------------------------------------------------------------------------

    /// The committed lane layout of `panel_id`, or `None` while the panel has no canvas.
    fn panel_lane_layout(&self, panel_id: PanelId) -> Option<PanelLaneLayout>;

    /// The template hash of the read promoted to the top of `track_id`'s stack, if any.
    fn promoted_read_template(&self, _track_id: TrackId) -> Option<u64> {
        None
    }

    // ----- GPU publication ------------------------------------------------------------------

    /// Captures every visible alignment lane against the current cameras and prepares its
    /// presentation. Reports admission back through
    /// [`Session::commit_alignment_lane_admissions`] and
    /// [`Session::wait_alignment_lane_admissions_for_gpu`].
    fn prepare_alignment_scene_frame(&mut self);

    /// Drops the presenter's copy of one lane's scene for `panel_id`, after its row authority
    /// ended (contig change, disjoint atlas, scroll reset).
    fn clear_alignment_scene_lane(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Result<(), String>;

    /// Gives a foreground source retry priority over optional presenter residency charged to
    /// the shared ledger.
    fn yield_speculation_for_source_pressure(
        &mut self,
        pressure: LedgerError,
    ) -> Result<LedgerProgressEpoch, LedgerError>;

    /// A scene worker finished (or abandoned) its part of a cross-owner capacity reclaim.
    fn observe_capacity_reclaim(
        &mut self,
        track_id: TrackId,
        request_id: u64,
        released_bytes: usize,
        relinquished_cache_entries: usize,
    );

    // ----- Displays and interaction ---------------------------------------------------------

    fn rebuild_all_panel_displays(&mut self);
    fn synchronize_alignment_scene_interactions(&mut self, panel_id: PanelId);
    fn mark_display_dirty(&mut self, reason: &'static str);
    fn request_redraw(&mut self);

    // ----- Shell work the settlement triggers -----------------------------------------------

    /// Requests reference, track and alignment data for every panel.
    fn request_all_panel_queries(&mut self);
    /// Starts the coverage worker of the track at `track_index` when its source has opened.
    fn start_alignment_coverage_engine(&mut self, track_index: usize);
    /// Panels were placed on a newly opened dictionary; finish an imported IGV locus.
    fn finish_imported_locus(&mut self, feature_search: Option<String>);
    /// A reference dictionary opened; re-resolve landmarks against it.
    fn update_landmark_resolution(&mut self, dictionary: &ReferenceDictionary);
    /// Refreshes the focused panel's context (ruler, cytoband) after a dictionary change.
    fn refresh_active_panel_context(&mut self);
    /// Saves settings (remembering the current locus).
    fn persist_settings(&mut self);
    /// Remote transfer statistics reported by the reference engine.
    fn observe_reference_remote_io(&mut self, stats: Option<RemoteIoStats>);

    /// Reports a failure on the status line and in the notification log.
    fn report_error(&mut self, message: String) {
        self.session_mut().report_error(message);
        self.request_redraw();
    }

    /// Reports a warning on the status line and in the notification log.
    fn report_warning(&mut self, message: String) {
        self.session_mut().report_warning(message);
        self.request_redraw();
    }

    // ----- Diagnostics ----------------------------------------------------------------------

    /// Whether diagnostic events are being recorded; payloads are built only when they are.
    fn diagnostics_recording(&self) -> bool {
        false
    }

    fn diagnostic_event(&mut self, _category: &str, _name: &str, _fields: serde_json::Value) {}

    fn alignment_filter_diagnostic_state(
        &self,
        _panel_id: PanelId,
        _track_id: TrackId,
    ) -> serde_json::Value {
        serde_json::Value::Null
    }
}

/// One effect a [`RecordingHost`] was asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum HostCall {
    PrepareAlignmentSceneFrame,
    ClearAlignmentSceneLane(PanelId, TrackId),
    YieldSpeculationForSourcePressure,
    ObserveCapacityReclaim { track_id: TrackId, request_id: u64 },
    RebuildAllPanelDisplays,
    SynchronizeAlignmentSceneInteractions(PanelId),
    MarkDisplayDirty(&'static str),
    RequestRedraw,
    RequestAllPanelQueries,
    StartAlignmentCoverageEngine(usize),
    FinishImportedLocus(Option<String>),
    UpdateLandmarkResolution,
    RefreshActivePanelContext,
    PersistSettings,
    ObserveReferenceRemoteIo,
}

/// A headless host that presents nothing: it records every effect, lays every panel out as one
/// canvas of the configured size with no lanes unless [`RecordingHost::lane_layouts`] says
/// otherwise, and keeps no GPU residency (so a source under pressure has nothing to yield).
pub struct RecordingHost<P: Panel = PanelState> {
    pub session: Session<P>,
    pub calls: Vec<HostCall>,
    /// Per-panel lane layouts; a panel without one has no canvas.
    pub lane_layouts: std::collections::BTreeMap<PanelId, PanelLaneLayout>,
}

impl<P: Panel> RecordingHost<P> {
    pub fn new(session: Session<P>) -> Self {
        Self {
            session,
            calls: Vec::new(),
            lane_layouts: std::collections::BTreeMap::new(),
        }
    }
}

impl<P: Panel> PresentationHost for RecordingHost<P> {
    type Panel = P;

    fn session(&self) -> &Session<P> {
        &self.session
    }

    fn session_mut(&mut self) -> &mut Session<P> {
        &mut self.session
    }

    fn panel_lane_layout(&self, panel_id: PanelId) -> Option<PanelLaneLayout> {
        self.lane_layouts.get(&panel_id).cloned()
    }

    fn prepare_alignment_scene_frame(&mut self) {
        self.calls.push(HostCall::PrepareAlignmentSceneFrame);
    }

    fn clear_alignment_scene_lane(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Result<(), String> {
        self.calls
            .push(HostCall::ClearAlignmentSceneLane(panel_id, track_id));
        Ok(())
    }

    fn yield_speculation_for_source_pressure(
        &mut self,
        _pressure: LedgerError,
    ) -> Result<LedgerProgressEpoch, LedgerError> {
        self.calls.push(HostCall::YieldSpeculationForSourcePressure);
        self.session
            .alignment_scene_ledger
            .progress()
            .map(rgv_alignment_scene::LedgerProgress::progress_epoch)
    }

    fn observe_capacity_reclaim(
        &mut self,
        track_id: TrackId,
        request_id: u64,
        _released_bytes: usize,
        _relinquished_cache_entries: usize,
    ) {
        self.calls.push(HostCall::ObserveCapacityReclaim {
            track_id,
            request_id,
        });
    }

    fn rebuild_all_panel_displays(&mut self) {
        self.calls.push(HostCall::RebuildAllPanelDisplays);
    }

    fn synchronize_alignment_scene_interactions(&mut self, panel_id: PanelId) {
        self.calls
            .push(HostCall::SynchronizeAlignmentSceneInteractions(panel_id));
    }

    fn mark_display_dirty(&mut self, reason: &'static str) {
        self.calls.push(HostCall::MarkDisplayDirty(reason));
    }

    fn request_redraw(&mut self) {
        self.calls.push(HostCall::RequestRedraw);
    }

    fn request_all_panel_queries(&mut self) {
        self.calls.push(HostCall::RequestAllPanelQueries);
    }

    fn start_alignment_coverage_engine(&mut self, track_index: usize) {
        self.calls
            .push(HostCall::StartAlignmentCoverageEngine(track_index));
    }

    fn finish_imported_locus(&mut self, feature_search: Option<String>) {
        self.session.imported_locus = None;
        self.calls
            .push(HostCall::FinishImportedLocus(feature_search));
    }

    fn update_landmark_resolution(&mut self, _dictionary: &ReferenceDictionary) {
        self.calls.push(HostCall::UpdateLandmarkResolution);
    }

    fn refresh_active_panel_context(&mut self) {
        self.calls.push(HostCall::RefreshActivePanelContext);
    }

    fn persist_settings(&mut self) {
        self.calls.push(HostCall::PersistSettings);
    }

    fn observe_reference_remote_io(&mut self, _stats: Option<RemoteIoStats>) {
        self.calls.push(HostCall::ObserveReferenceRemoteIo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{
        ContigId, DesktopSettings, GenomeAssembly, GenomicInterval, ReferenceDictionary, Viewport,
    };
    use std::path::PathBuf;

    fn recording_host() -> RecordingHost {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        let panel = PanelState::new(
            PanelId::new(1),
            Viewport::new(interval, 600.0).unwrap(),
            None,
            "chr1".to_owned(),
            1_000_000,
        );
        RecordingHost::new(Session::new(
            panel,
            GenomeAssembly::from_dictionary("test", dictionary),
            DesktopSettings::default(),
            PathBuf::from("settings.json"),
        ))
    }

    #[test]
    fn default_error_report_reaches_the_session_and_asks_for_a_redraw() {
        let mut host = recording_host();
        host.report_error("boom".to_owned());
        assert_eq!(host.session.data_status, "boom");
        assert_eq!(host.calls, vec![HostCall::RequestRedraw]);
        assert!(!host.diagnostics_recording());
        assert_eq!(
            host.alignment_filter_diagnostic_state(PanelId::new(1), TrackId::new(1)),
            serde_json::Value::Null
        );
    }

    #[test]
    fn scene_identities_are_never_reused() {
        let mut host = recording_host();
        let first = host.session.take_alignment_scene_request_token();
        let second = host.session.take_alignment_scene_request_token();
        assert!(second.get() > first.get());
        let owner = host.session.take_alignment_scene_owner();
        assert_eq!(owner.get(), 1);
    }

    #[test]
    fn a_headless_host_yields_nothing_and_reports_the_ledger_epoch() {
        let mut host = recording_host();
        let expected = host
            .session
            .alignment_scene_ledger
            .progress()
            .unwrap()
            .progress_epoch();
        let pressure = LedgerError::ReservationExceeded {
            requested: 2,
            remaining: 1,
        };
        assert_eq!(
            host.yield_speculation_for_source_pressure(pressure)
                .unwrap(),
            expected
        );
        assert_eq!(
            host.calls,
            vec![HostCall::YieldSpeculationForSourcePressure]
        );
    }
}
