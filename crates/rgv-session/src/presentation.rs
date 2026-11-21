//! [`PanelPresentation`]: what one panel shows, described without a device. The session builds it
//! from its own state and the presenter's lane layout; a presenter draws it.
//!
//! Every heavy payload is an `Arc` (scene snapshots, reference slices) or a cheap clone of the
//! retained query answer, so building a presentation copies descriptors, never evidence.

use crate::alignment::coverage::AlignmentCoverageMode;
use crate::alignment::navigation::AlignmentNavigationPhase;
use crate::alignment::track::AlignmentTrackLifecycle;
use crate::lane_layout::PanelLaneLayout;
use crate::panels::Panel;
use crate::session::Session;
use rgv_alignment_scene::AlignmentSceneSnapshot;
use rgv_core::{GenomicInterval, PanelId, TrackId, TrackKind, Viewport};
use rgv_engine::TrackSnapshot;
use rgv_hts::ReferenceSlice;
use std::sync::Arc;

/// The alignment lanes of one track in one panel.
#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct AlignmentLanePresentation {
    pub track_id: TrackId,
    pub name: String,
    /// Whether the track's source is open, opening or failed (with the failure).
    pub source_failure: Option<String>,
    pub source_opening: bool,
    /// The panel's navigation transaction for this track.
    pub phase: AlignmentNavigationPhase,
    /// The current request has settled (committed, budget limited, overview, unavailable or
    /// failed).
    pub settled: bool,
    /// The latest immutable scene the source published for this panel: reads, the row atlas and
    /// the summaries (coverage, alleles, junctions, modifications) derived from the same
    /// evidence.
    pub scene: Option<Arc<AlignmentSceneSnapshot>>,
    /// The scene satisfies the current filter, colour, sort and layout contracts.
    pub scene_contracts_current: bool,
    /// The scene covers the whole requested camera, rows included.
    pub visible_complete: bool,
    /// Individual reads were refused for this camera (complete-read limit).
    pub reads_refused: bool,
    /// The view is at overview detail and shows the index, not records.
    pub index_only_overview: bool,
    /// Why the lane shows no scene, when it shows none for a stated reason.
    pub unavailable: Option<String>,
    /// The coverage-pyramid mode the lane was last presented with, if the pyramid was used.
    pub coverage_mode: Option<AlignmentCoverageMode>,
}

/// One annotation track's lane in one panel.
#[derive(Clone, Debug)]
pub struct TrackLanePresentation {
    pub track_id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub visible: bool,
    /// The panel's retained answer for this track, if any has arrived.
    pub snapshot: Option<TrackSnapshot>,
    /// The buffered interval that answer covers.
    pub interval: Option<GenomicInterval>,
}

/// Which of a panel's data dependencies are still outstanding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PendingPresentation {
    pub alignment: bool,
    pub reference: bool,
    pub tracks: bool,
}

impl PendingPresentation {
    pub const fn any(self) -> bool {
        self.alignment || self.reference || self.tracks
    }
}

/// Everything one panel presents, without a device.
#[derive(Clone, Debug)]
pub struct PanelPresentation {
    pub panel_id: PanelId,
    /// The panel camera: interval and pixel width.
    pub viewport: Viewport,
    pub contig_name: String,
    pub contig_length: u64,
    /// The presenter's lane layout the presentation was built against, if the panel has one.
    pub lanes: Option<PanelLaneLayout>,
    pub alignment: Vec<AlignmentLanePresentation>,
    pub tracks: Vec<TrackLanePresentation>,
    /// The reference bases the panel holds for its current generation.
    pub reference: Option<Arc<ReferenceSlice>>,
    pub pending: PendingPresentation,
    /// The status line as the session currently reads.
    pub status: String,
}

impl PanelPresentation {
    /// The alignment lane of `track_id`.
    pub fn alignment_lane(&self, track_id: TrackId) -> Option<&AlignmentLanePresentation> {
        self.alignment.iter().find(|lane| lane.track_id == track_id)
    }

    /// The annotation lane of `track_id`.
    pub fn track_lane(&self, track_id: TrackId) -> Option<&TrackLanePresentation> {
        self.tracks.iter().find(|lane| lane.track_id == track_id)
    }
}

impl<P: Panel> Session<P> {
    /// Builds the presentation of `panel_id` against `lanes`, or `None` for an unknown panel.
    pub fn panel_presentation(
        &self,
        panel_id: PanelId,
        lanes: Option<PanelLaneLayout>,
    ) -> Option<PanelPresentation> {
        let panel = self.panels.state(panel_id)?;
        let alignment = self
            .alignment_tracks
            .iter()
            .map(|track| {
                let projection = track.projection(panel_id);
                let frame = projection.and_then(|projection| projection.scene_frame.as_ref());
                let (source_failure, source_opening) = match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => (None, true),
                    AlignmentTrackLifecycle::Ready => (None, false),
                    AlignmentTrackLifecycle::Failed(message) => (Some(message.clone()), false),
                };
                AlignmentLanePresentation {
                    track_id: track.config.id,
                    name: track.config.name.clone(),
                    source_failure,
                    source_opening,
                    phase: projection.map_or(AlignmentNavigationPhase::Dormant, |projection| {
                        projection.navigation.phase()
                    }),
                    settled: projection.is_some_and(|projection| {
                        projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    }),
                    scene: frame.and_then(|frame| frame.snapshot.clone()),
                    scene_contracts_current: frame
                        .is_some_and(|frame| frame.scene_contracts_current),
                    visible_complete: frame
                        .is_some_and(rgv_engine::AlignmentCoordinatorFrame::visible_complete),
                    reads_refused: frame.is_some_and(
                        rgv_engine::AlignmentCoordinatorFrame::individual_reads_refused_for_camera,
                    ),
                    index_only_overview: projection
                        .is_some_and(|projection| projection.index_only_overview),
                    unavailable: projection.and_then(|projection| projection.unavailable.clone()),
                    coverage_mode: projection
                        .and_then(|projection| projection.coverage.presented.as_ref())
                        .map(|(mode, _)| *mode),
                }
            })
            .collect();
        let tracks = self
            .tracks
            .tracks()
            .iter()
            .map(|config| TrackLanePresentation {
                track_id: config.id,
                name: config.name.clone(),
                kind: config.kind,
                visible: config.visible,
                snapshot: panel
                    .track_snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == config.id)
                    .cloned(),
                interval: panel.track_snapshot_intervals.get(&config.id).copied(),
            })
            .collect();
        Some(PanelPresentation {
            panel_id,
            viewport: panel.viewport,
            contig_name: panel.contig_name.clone(),
            contig_length: panel.contig_length,
            lanes,
            alignment,
            tracks,
            reference: panel.reference_slice.clone(),
            pending: PendingPresentation {
                alignment: panel.pending_queries.alignment,
                reference: panel.pending_queries.reference,
                tracks: panel.pending_queries.tracks,
            },
            status: self.data_status.clone(),
        })
    }
}
