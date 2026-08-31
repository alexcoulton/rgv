use std::{collections::BTreeMap, ops::Range, sync::Arc};

use rgv_core::{GenomicInterval, ModifiedBaseColorMode, PanelId, TrackId, Viewport};
use thiserror::Error;

use crate::{
    AlignmentCamera, AlignmentRowActivityProfile, AlignmentSceneCamera, AlignmentSceneSnapshot,
    CpuAlleleProjectionOptions, CpuClipRect, CpuDepthProjectionOptions,
    CpuJunctionProjectionOptions, CpuModificationProjectionOptions, SceneRevision,
};

const CAMERA_EQUIVALENCE_EPSILON: f64 = 1.0e-9;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentCoherenceGroupId(u64);

impl AlignmentCoherenceGroupId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentCoherenceGroupKind {
    SashimiScale,
    JunctionAnnotation,
    Comparison,
}

/// Stable semantic identity for a scene-owned alignment-derived lane. Layout rectangles and
/// presentation options are captured once per panel epoch rather than rediscovered from labels or
/// reread from mutable session settings by each renderer and picker.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlignmentDerivedLaneKind {
    Coverage,
    Sashimi,
    Modifications,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentCoverageLaneOptions {
    pub depth: CpuDepthProjectionOptions,
    pub alleles: Option<CpuAlleleProjectionOptions>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AlignmentSashimiScalePolicy {
    Automatic,
    Fixed(f64),
    Shared(AlignmentCoherenceGroupId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentSashimiLaneOptions {
    pub projection: CpuJunctionProjectionOptions,
    pub scale: AlignmentSashimiScalePolicy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentModificationLaneOptions {
    pub projection: CpuModificationProjectionOptions,
    pub color_mode: ModifiedBaseColorMode,
    pub selected_position: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AlignmentDerivedLaneOptions {
    Coverage(AlignmentCoverageLaneOptions),
    Sashimi(AlignmentSashimiLaneOptions),
    Modifications(AlignmentModificationLaneOptions),
}

impl AlignmentDerivedLaneOptions {
    #[must_use]
    pub const fn kind(&self) -> AlignmentDerivedLaneKind {
        match self {
            Self::Coverage(_) => AlignmentDerivedLaneKind::Coverage,
            Self::Sashimi(_) => AlignmentDerivedLaneKind::Sashimi,
            Self::Modifications(_) => AlignmentDerivedLaneKind::Modifications,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentDerivedLaneCapture {
    pub clip: CpuClipRect,
    pub options: AlignmentDerivedLaneOptions,
}

impl AlignmentDerivedLaneCapture {
    pub fn new(
        clip: CpuClipRect,
        options: AlignmentDerivedLaneOptions,
    ) -> Result<Self, AlignmentPanelSceneError> {
        if !valid_clip(clip) {
            return Err(AlignmentPanelSceneError::InvalidDerivedLaneClip(
                options.kind(),
            ));
        }
        Ok(Self { clip, options })
    }

    #[must_use]
    pub const fn kind(&self) -> AlignmentDerivedLaneKind {
        self.options.kind()
    }
}

/// All live presentation state for one alignment track at a panel frame boundary.
///
/// `preparation_camera` is coordinator demand and exists whenever any read-derived product is
/// visible. `read_camera` is the actual read-pixel lane and is absent when Reads is hidden. This
/// distinction prevents collapsing Reads from also suppressing coverage or Sashimi acquisition.
#[derive(Clone, Debug)]
pub struct AlignmentPanelLaneCapture {
    pub track_id: TrackId,
    pub preparation_camera: AlignmentSceneCamera,
    pub read_camera: Option<AlignmentCamera>,
    /// Exact genomic-camera row authority captured beside the camera. It is independent of the
    /// visible row range: vertical scrolling changes page readiness, not row identity or extent.
    /// A missing profile is deliberately different from an empty profile and means the row
    /// extent is not yet known for this genomic presentation epoch.
    pub row_activity: Option<Arc<AlignmentRowActivityProfile>>,
    pub derived_lanes: Arc<[AlignmentDerivedLaneCapture]>,
}

impl AlignmentPanelLaneCapture {
    pub fn new(
        track_id: TrackId,
        preparation_camera: impl Into<AlignmentSceneCamera>,
        read_camera: Option<AlignmentCamera>,
        derived_lanes: impl IntoIterator<Item = AlignmentDerivedLaneCapture>,
    ) -> Result<Self, AlignmentPanelSceneError> {
        let preparation_camera = preparation_camera.into();
        if read_camera.is_some_and(|camera| {
            !viewports_are_equivalent(camera.viewport(), preparation_camera.viewport())
        }) {
            return Err(AlignmentPanelSceneError::ReadCameraMismatch(track_id));
        }
        let mut derived_lanes = derived_lanes.into_iter().collect::<Vec<_>>();
        derived_lanes.sort_unstable_by_key(AlignmentDerivedLaneCapture::kind);
        for pair in derived_lanes.windows(2) {
            if pair[0].kind() == pair[1].kind() {
                return Err(AlignmentPanelSceneError::DuplicateDerivedLane {
                    track_id,
                    kind: pair[0].kind(),
                });
            }
        }
        let horizontal_x = read_camera
            .map(|camera| camera.clip().x)
            .or_else(|| derived_lanes.first().map(|lane| lane.clip.x));
        for lane in &derived_lanes {
            if horizontal_x.is_some_and(|x| !approximately_equal(lane.clip.x, x))
                || !approximately_equal(
                    lane.clip.width,
                    preparation_camera.viewport().pixel_width(),
                )
            {
                return Err(AlignmentPanelSceneError::DerivedLaneCameraMismatch {
                    track_id,
                    kind: lane.kind(),
                });
            }
        }
        Ok(Self {
            track_id,
            preparation_camera,
            read_camera,
            row_activity: None,
            derived_lanes: derived_lanes.into(),
        })
    }

    /// Attaches the exact genomic row profile to this camera capture. Callers retain the option
    /// to construct a pending capture without inventing a zero-row extent.
    #[must_use]
    pub fn with_row_activity(
        mut self,
        row_activity: Option<Arc<AlignmentRowActivityProfile>>,
    ) -> Self {
        self.row_activity = row_activity;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentCoherenceGroupSpec {
    id: AlignmentCoherenceGroupId,
    kind: AlignmentCoherenceGroupKind,
    members: Arc<[TrackId]>,
}

impl AlignmentCoherenceGroupSpec {
    pub fn new(
        id: AlignmentCoherenceGroupId,
        kind: AlignmentCoherenceGroupKind,
        members: impl IntoIterator<Item = TrackId>,
    ) -> Result<Self, AlignmentPanelSceneError> {
        let mut members = members.into_iter().collect::<Vec<_>>();
        members.sort_unstable();
        let original_len = members.len();
        members.dedup();
        if members.is_empty() {
            return Err(AlignmentPanelSceneError::EmptyCoherenceGroup(id));
        }
        if members.len() != original_len {
            return Err(AlignmentPanelSceneError::DuplicateCoherenceMember(id));
        }
        Ok(Self {
            id,
            kind,
            members: members.into(),
        })
    }

    #[must_use]
    pub const fn id(&self) -> AlignmentCoherenceGroupId {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> AlignmentCoherenceGroupKind {
        self.kind
    }

    #[must_use]
    pub fn members(&self) -> &[TrackId] {
        &self.members
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentPanelSceneAdmission {
    Stale,
    CameraAdvanced,
    ContractsChanged,
    SceneReplaced,
}

/// One coordinator publication offered to the panel-scene store. Keeping the semantic-contract
/// state beside the snapshot prevents a geometrically overlapping retained scene from being
/// admitted as current after the desired summary, layout, or presentation contract changes.
#[derive(Clone, Debug)]
pub struct AlignmentPanelLanePublication {
    source_camera_sequence: u64,
    snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    scene_contracts_current: bool,
}

impl AlignmentPanelLanePublication {
    #[must_use]
    pub const fn new(
        source_camera_sequence: u64,
        snapshot: Option<Arc<AlignmentSceneSnapshot>>,
        scene_contracts_current: bool,
    ) -> Self {
        Self {
            source_camera_sequence,
            snapshot,
            scene_contracts_current,
        }
    }

    #[must_use]
    pub const fn source_camera_sequence(&self) -> u64 {
        self.source_camera_sequence
    }

    #[must_use]
    pub const fn snapshot(&self) -> Option<&Arc<AlignmentSceneSnapshot>> {
        self.snapshot.as_ref()
    }

    #[must_use]
    pub const fn scene_contracts_current(&self) -> bool {
        self.scene_contracts_current
    }
}

#[derive(Clone, Debug)]
struct ResidentAlignmentLane {
    source_camera_sequence: u64,
    snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    scene_contracts_current: bool,
}

/// One render-and-interaction capture for one alignment track. The camera is live UI state; the
/// scene is the latest immutable worker publication. Their truthful intersection is explicit.
#[derive(Clone, Debug)]
pub struct AlignmentPanelLaneFrame {
    pub track_id: TrackId,
    /// Coordinator/source demand. Its genomic viewport is shared by reads and every derived lane.
    pub preparation_camera: AlignmentSceneCamera,
    /// Actual read-pixel camera. Hidden/collapsed Reads deliberately has no render camera while
    /// summaries may remain current and visible.
    pub read_camera: Option<AlignmentCamera>,
    /// Genomic-camera-exact row extent and scrollbar-density authority committed with this panel
    /// frame. It remains exact across vertical-only camera movement even while requested row
    /// pages are pending. Rendering consumers must not substitute an unrelated newer
    /// coordinator/staging profile.
    pub row_activity: Option<Arc<AlignmentRowActivityProfile>>,
    pub derived_lanes: Arc<[AlignmentDerivedLaneCapture]>,
    pub source_camera_sequence: u64,
    pub snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    pub covered_interval: Option<GenomicInterval>,
    pub requested_rows: Range<u32>,
    pub covered_rows: Range<u32>,
    /// Whether the retained snapshot satisfies the coordinator's currently desired summary,
    /// layout, and presentation contracts. Interval overlap alone is not semantic readiness.
    pub scene_contracts_current: bool,
}

impl AlignmentPanelLaneFrame {
    /// Whether exact summary authority is current and complete for the live genomic camera. This
    /// deliberately ignores read-row coverage: coverage, alleles, junctions, and modifications
    /// do not become stale merely because a vertical read page is still being prepared.
    #[must_use]
    pub fn summary_current(&self) -> bool {
        self.scene_contracts_current
            && self.covered_interval.is_some_and(|covered| {
                covered.contains_interval(self.preparation_camera.interval())
            })
    }

    #[must_use]
    pub fn visible_complete(&self) -> bool {
        self.summary_current() && self.reads_complete()
    }

    /// Read-row readiness is independent of summary readiness. A hidden read lane has no visible
    /// row demand and is therefore complete without manufacturing a render clip.
    #[must_use]
    pub fn reads_complete(&self) -> bool {
        self.read_camera.is_none()
            || (self.covered_rows.start <= self.requested_rows.start
                && self.covered_rows.end >= self.requested_rows.end)
    }

    #[must_use]
    pub fn scene_revision(&self) -> Option<SceneRevision> {
        self.snapshot.as_ref().map(|snapshot| snapshot.revision())
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentCoherenceGroupFrame {
    pub id: AlignmentCoherenceGroupId,
    pub kind: AlignmentCoherenceGroupKind,
    pub authority_revision: u64,
    pub members: Arc<[TrackId]>,
    pub scene_revisions: Arc<[Option<SceneRevision>]>,
    pub summary_complete: bool,
    pub reads_complete: bool,
    /// Compatibility aggregate: all visible derived and read products in this group are current.
    pub visible_complete: bool,
}

/// Immutable frame-boundary capture consumed by rendering and interaction together. Capturing a
/// new live camera clones only small lane descriptors and scene `Arc`s; it performs no source,
/// evidence, layout, page-build, or GPU work.
#[derive(Clone, Debug)]
pub struct AlignmentPanelSceneFrame {
    pub panel_id: PanelId,
    /// Workspace-contig viewport captured once for every lane and consumer in this epoch.
    pub viewport: Viewport,
    pub camera_revision: u64,
    pub authority_revision: u64,
    pub lanes: Arc<[AlignmentPanelLaneFrame]>,
    pub coherence_groups: Arc<[AlignmentCoherenceGroupFrame]>,
}

impl AlignmentPanelSceneFrame {
    #[must_use]
    pub fn lane(&self, track_id: TrackId) -> Option<&AlignmentPanelLaneFrame> {
        self.lanes.iter().find(|lane| lane.track_id == track_id)
    }
}

#[derive(Clone, Debug)]
struct CoherenceGroupState {
    spec: AlignmentCoherenceGroupSpec,
    authority_revision: u64,
}

/// UI-thread resident scene leases for one panel. It never owns or queries a source. Worker
/// publications replace lane snapshots monotonically; pointer movement captures those snapshots
/// against the newest live camera immediately.
#[derive(Clone, Debug, Default)]
pub struct AlignmentPanelSceneStore {
    lanes: BTreeMap<TrackId, ResidentAlignmentLane>,
    coherence_groups: Vec<CoherenceGroupState>,
    authority_revision: u64,
}

impl AlignmentPanelSceneStore {
    #[must_use]
    pub fn new(tracks: impl IntoIterator<Item = TrackId>) -> Self {
        let mut store = Self::default();
        store.replace_visible_tracks(tracks);
        store
    }

    pub fn replace_visible_tracks(&mut self, tracks: impl IntoIterator<Item = TrackId>) {
        let tracks = tracks
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if self.lanes.keys().copied().eq(tracks.iter().copied()) {
            return;
        }
        let mut next = tracks
            .into_iter()
            .map(|track_id| {
                let resident = self
                    .lanes
                    .remove(&track_id)
                    .unwrap_or(ResidentAlignmentLane {
                        source_camera_sequence: 0,
                        snapshot: None,
                        scene_contracts_current: false,
                    });
                (track_id, resident)
            })
            .collect::<BTreeMap<_, _>>();
        std::mem::swap(&mut self.lanes, &mut next);
        self.coherence_groups.retain(|group| {
            group
                .spec
                .members()
                .iter()
                .all(|track_id| self.lanes.contains_key(track_id))
        });
        self.advance_authority();
    }

    /// Enumerates the exact strong snapshot references owned by this resident panel store.
    /// Replacement retirement uses occurrences rather than unique identities so it can compare
    /// them with `Arc::strong_count` without manufacturing another semantic owner.
    pub fn snapshots(&self) -> impl Iterator<Item = &Arc<AlignmentSceneSnapshot>> {
        self.lanes
            .values()
            .filter_map(|lane| lane.snapshot.as_ref())
    }

    pub fn set_coherence_groups(
        &mut self,
        groups: impl IntoIterator<Item = AlignmentCoherenceGroupSpec>,
    ) -> Result<(), AlignmentPanelSceneError> {
        let mut groups = groups.into_iter().collect::<Vec<_>>();
        groups.sort_unstable_by_key(|group| group.id());
        for pair in groups.windows(2) {
            if pair[0].id() == pair[1].id() {
                return Err(AlignmentPanelSceneError::DuplicateCoherenceGroup(
                    pair[0].id(),
                ));
            }
        }
        for group in &groups {
            if let Some(track_id) = group
                .members()
                .iter()
                .find(|track_id| !self.lanes.contains_key(track_id))
            {
                return Err(AlignmentPanelSceneError::UnknownTrack(*track_id));
            }
        }
        if groups
            .iter()
            .eq(self.coherence_groups.iter().map(|group| &group.spec))
        {
            return Ok(());
        }
        self.advance_authority();
        let revision = self.authority_revision;
        self.coherence_groups = groups
            .into_iter()
            .map(|spec| CoherenceGroupState {
                spec,
                authority_revision: revision,
            })
            .collect();
        Ok(())
    }

    pub fn admit(
        &mut self,
        track_id: TrackId,
        publication: AlignmentPanelLanePublication,
    ) -> Result<AlignmentPanelSceneAdmission, AlignmentPanelSceneError> {
        let lane = self
            .lanes
            .get_mut(&track_id)
            .ok_or(AlignmentPanelSceneError::UnknownTrack(track_id))?;
        if publication.source_camera_sequence < lane.source_camera_sequence {
            return Ok(AlignmentPanelSceneAdmission::Stale);
        }
        let replacement = publication.snapshot.as_ref().is_some_and(|candidate| {
            lane.snapshot
                .as_ref()
                .is_none_or(|current| current.revision() != candidate.revision())
        });
        let contracts_changed = lane.scene_contracts_current != publication.scene_contracts_current;
        lane.source_camera_sequence = publication.source_camera_sequence;
        lane.scene_contracts_current = publication.scene_contracts_current;
        if let Some(snapshot) = publication.snapshot {
            lane.snapshot = Some(snapshot);
        }
        if !replacement && !contracts_changed {
            return Ok(AlignmentPanelSceneAdmission::CameraAdvanced);
        }
        self.advance_authority();
        for group in &mut self.coherence_groups {
            if group.spec.members().contains(&track_id) {
                group.authority_revision = self.authority_revision;
            }
        }
        Ok(if replacement {
            AlignmentPanelSceneAdmission::SceneReplaced
        } else {
            AlignmentPanelSceneAdmission::ContractsChanged
        })
    }

    /// Ends one lane's snapshot authority without removing its visible track slot. This is used
    /// for discontinuous locus transitions: a new source lane may restart its camera sequence,
    /// so retaining the prior sequence or row atlas would incorrectly reject the new scene.
    pub fn clear_lane(&mut self, track_id: TrackId) -> Result<bool, AlignmentPanelSceneError> {
        let lane = self
            .lanes
            .get_mut(&track_id)
            .ok_or(AlignmentPanelSceneError::UnknownTrack(track_id))?;
        let changed = lane.source_camera_sequence != 0
            || lane.snapshot.take().is_some()
            || lane.scene_contracts_current;
        lane.source_camera_sequence = 0;
        lane.scene_contracts_current = false;
        if changed {
            self.advance_authority();
            for group in &mut self.coherence_groups {
                if group.spec.members().contains(&track_id) {
                    group.authority_revision = self.authority_revision;
                }
            }
        }
        Ok(changed)
    }

    pub fn capture(
        &self,
        panel_id: PanelId,
        camera_revision: u64,
        viewport: Viewport,
        captures: impl IntoIterator<Item = AlignmentPanelLaneCapture>,
    ) -> Result<AlignmentPanelSceneFrame, AlignmentPanelSceneError> {
        let mut capture_map = BTreeMap::new();
        for capture in captures {
            let track_id = capture.track_id;
            if !viewports_are_equivalent(viewport, capture.preparation_camera.viewport()) {
                return Err(AlignmentPanelSceneError::PanelCameraMismatch(track_id));
            }
            if capture_map.insert(track_id, capture).is_some() {
                return Err(AlignmentPanelSceneError::DuplicateCamera(track_id));
            }
        }
        let mut captures = capture_map;
        if captures.len() != self.lanes.len() {
            if let Some(track_id) = self
                .lanes
                .keys()
                .find(|track_id| !captures.contains_key(track_id))
            {
                return Err(AlignmentPanelSceneError::MissingCamera(*track_id));
            }
            if let Some(track_id) = captures
                .keys()
                .find(|track_id| !self.lanes.contains_key(track_id))
            {
                return Err(AlignmentPanelSceneError::UnknownTrack(*track_id));
            }
        }
        let mut lanes = Vec::with_capacity(self.lanes.len());
        for (track_id, resident) in &self.lanes {
            let capture = captures
                .remove(track_id)
                .ok_or(AlignmentPanelSceneError::MissingCamera(*track_id))?;
            let requested_rows = if capture.preparation_camera.has_visible_rows() {
                resident.snapshot.as_ref().map_or_else(
                    || capture.preparation_camera.visible_rows(),
                    |scene| {
                        let row_count =
                            u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
                        let visible = capture.preparation_camera.visible_rows();
                        visible.start.min(row_count)..visible.end.min(row_count)
                    },
                )
            } else {
                0..0
            };
            let (covered_interval, covered_rows) =
                resident.snapshot.as_ref().map_or((None, 0..0), |scene| {
                    (
                        intersect(
                            capture.preparation_camera.interval(),
                            scene.prepared_interval(),
                        ),
                        intersect_ranges(requested_rows.clone(), scene.reads().prepared_rows()),
                    )
                });
            lanes.push(AlignmentPanelLaneFrame {
                track_id: *track_id,
                preparation_camera: capture.preparation_camera,
                read_camera: capture.read_camera,
                row_activity: capture.row_activity,
                derived_lanes: capture.derived_lanes,
                source_camera_sequence: resident.source_camera_sequence,
                snapshot: resident.snapshot.clone(),
                covered_interval,
                requested_rows,
                covered_rows,
                scene_contracts_current: resident.scene_contracts_current,
            });
        }
        let coherence_groups = self
            .coherence_groups
            .iter()
            .map(|group| {
                let members = group.spec.members().to_vec();
                let scene_revisions = members
                    .iter()
                    .map(|track_id| {
                        lanes
                            .iter()
                            .find(|lane| lane.track_id == *track_id)
                            .and_then(AlignmentPanelLaneFrame::scene_revision)
                    })
                    .collect::<Vec<_>>();
                let (summary_complete, reads_complete) =
                    coherence_group_completeness(&lanes, &members);
                AlignmentCoherenceGroupFrame {
                    id: group.spec.id(),
                    kind: group.spec.kind(),
                    authority_revision: group.authority_revision,
                    members: members.into(),
                    scene_revisions: scene_revisions.into(),
                    summary_complete,
                    reads_complete,
                    visible_complete: summary_complete && reads_complete,
                }
            })
            .collect::<Vec<_>>();
        Ok(AlignmentPanelSceneFrame {
            panel_id,
            viewport,
            camera_revision,
            authority_revision: self.authority_revision,
            lanes: lanes.into(),
            coherence_groups: coherence_groups.into(),
        })
    }

    #[must_use]
    pub fn track_count(&self) -> usize {
        self.lanes.len()
    }

    #[must_use]
    pub const fn authority_revision(&self) -> u64 {
        self.authority_revision
    }

    fn advance_authority(&mut self) {
        self.authority_revision = self.authority_revision.saturating_add(1).max(1);
    }
}

fn intersect(left: GenomicInterval, right: GenomicInterval) -> Option<GenomicInterval> {
    if left.contig() != right.contig() {
        return None;
    }
    let start = left.start().max(right.start());
    let end = left.end().min(right.end());
    (start < end).then(|| {
        GenomicInterval::new(left.contig(), start, end)
            .expect("intersection of valid intervals is valid")
    })
}

fn intersect_ranges(left: Range<u32>, right: Range<u32>) -> Range<u32> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);
    start.min(end)..end
}

fn coherence_group_completeness(
    lanes: &[AlignmentPanelLaneFrame],
    members: &[TrackId],
) -> (bool, bool) {
    let summary_complete = members.iter().all(|track_id| {
        lanes
            .iter()
            .find(|lane| lane.track_id == *track_id)
            .is_some_and(AlignmentPanelLaneFrame::summary_current)
    });
    let reads_complete = members.iter().all(|track_id| {
        lanes
            .iter()
            .find(|lane| lane.track_id == *track_id)
            .is_some_and(AlignmentPanelLaneFrame::reads_complete)
    });
    (summary_complete, reads_complete)
}

fn valid_clip(clip: CpuClipRect) -> bool {
    clip.x.is_finite()
        && clip.y.is_finite()
        && clip.width.is_finite()
        && clip.width > 0.0
        && clip.height.is_finite()
        && clip.height > 0.0
}

fn approximately_equal(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= CAMERA_EQUIVALENCE_EPSILON * scale
}

/// Panel and source dictionaries may assign different numeric contig IDs to the same contig.
/// Camera equivalence therefore compares the exact scalar transform and integer enclosure, but
/// deliberately leaves dictionary identity to the desktop's explicit contig remapping step.
fn viewports_are_equivalent(left: Viewport, right: Viewport) -> bool {
    left.interval().start() == right.interval().start()
        && left.interval().end() == right.interval().end()
        && approximately_equal(left.exact_start(), right.exact_start())
        && approximately_equal(left.span(), right.span())
        && approximately_equal(left.pixel_width(), right.pixel_width())
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentPanelSceneError {
    #[error("alignment panel scene does not contain track {0:?}")]
    UnknownTrack(TrackId),
    #[error("alignment panel scene capture has no live camera for track {0:?}")]
    MissingCamera(TrackId),
    #[error("alignment panel scene capture contains more than one camera for track {0:?}")]
    DuplicateCamera(TrackId),
    #[error(
        "alignment panel scene preparation camera for track {0:?} does not match the panel viewport"
    )]
    PanelCameraMismatch(TrackId),
    #[error(
        "alignment panel scene read camera for track {0:?} does not match its preparation camera"
    )]
    ReadCameraMismatch(TrackId),
    #[error(
        "alignment panel scene {kind:?} lane for track {track_id:?} does not share the preparation camera's horizontal geometry"
    )]
    DerivedLaneCameraMismatch {
        track_id: TrackId,
        kind: AlignmentDerivedLaneKind,
    },
    #[error(
        "alignment panel scene capture contains more than one {kind:?} lane for track {track_id:?}"
    )]
    DuplicateDerivedLane {
        track_id: TrackId,
        kind: AlignmentDerivedLaneKind,
    },
    #[error("alignment panel scene {0:?} lane has an invalid clip rectangle")]
    InvalidDerivedLaneClip(AlignmentDerivedLaneKind),
    #[error("coherence group {0:?} has no members")]
    EmptyCoherenceGroup(AlignmentCoherenceGroupId),
    #[error("coherence group {0:?} contains one track more than once")]
    DuplicateCoherenceMember(AlignmentCoherenceGroupId),
    #[error("coherence group {0:?} is declared more than once")]
    DuplicateCoherenceGroup(AlignmentCoherenceGroupId),
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;
    use crate::CpuClipRect;

    fn camera(start: u64) -> AlignmentCamera {
        AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), start, start + 10).unwrap(),
            0.0,
            12.0,
            9.0,
            CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 120.0,
            },
        )
        .unwrap()
    }

    fn lane_capture(track_id: TrackId, start: u64) -> AlignmentPanelLaneCapture {
        let camera = camera(start);
        AlignmentPanelLaneCapture::new(track_id, camera, Some(camera), []).unwrap()
    }

    fn publication(
        source_camera_sequence: u64,
        scene_contracts_current: bool,
    ) -> AlignmentPanelLanePublication {
        AlignmentPanelLanePublication::new(source_camera_sequence, None, scene_contracts_current)
    }

    #[test]
    fn capture_requires_exactly_one_live_camera_per_visible_track() {
        let tracks = [TrackId::new(1), TrackId::new(2)];
        let store = AlignmentPanelSceneStore::new(tracks);
        assert!(matches!(
            store.capture(
                PanelId::new(1),
                1,
                camera(10).viewport(),
                [lane_capture(tracks[0], 10)]
            ),
            Err(AlignmentPanelSceneError::MissingCamera(track)) if track == tracks[1]
        ));
        let frame = store
            .capture(
                PanelId::new(1),
                2,
                camera(10).viewport(),
                [lane_capture(tracks[0], 10), lane_capture(tracks[1], 10)],
            )
            .unwrap();
        assert_eq!(frame.camera_revision, 2);
        assert_eq!(frame.lanes.len(), 2);
        assert!(frame.lanes.iter().all(|lane| lane.snapshot.is_none()));
    }

    #[test]
    fn absent_new_snapshot_advances_camera_without_erasing_resident_authority() {
        let track = TrackId::new(7);
        let mut store = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            store.admit(track, publication(3, false)).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        assert_eq!(
            store.admit(track, publication(2, false)).unwrap(),
            AlignmentPanelSceneAdmission::Stale
        );
        let frame = store
            .capture(
                PanelId::new(1),
                5,
                camera(100).viewport(),
                [lane_capture(track, 100)],
            )
            .unwrap();
        assert_eq!(frame.lanes[0].source_camera_sequence, 3);
    }

    #[test]
    fn contract_freshness_is_monotonic_with_the_admitted_camera_publication() {
        let track = TrackId::new(7);
        let mut store = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            store.admit(track, publication(3, true)).unwrap(),
            AlignmentPanelSceneAdmission::ContractsChanged
        );
        assert_eq!(
            store.admit(track, publication(2, false)).unwrap(),
            AlignmentPanelSceneAdmission::Stale
        );
        let current = store
            .capture(
                PanelId::new(1),
                1,
                camera(100).viewport(),
                [lane_capture(track, 100)],
            )
            .unwrap();
        assert!(current.lanes[0].scene_contracts_current);

        assert_eq!(
            store.admit(track, publication(4, false)).unwrap(),
            AlignmentPanelSceneAdmission::ContractsChanged
        );
        let stale_contract = store
            .capture(
                PanelId::new(1),
                2,
                camera(100).viewport(),
                [lane_capture(track, 100)],
            )
            .unwrap();
        assert!(!stale_contract.lanes[0].scene_contracts_current);
        assert!(!stale_contract.lanes[0].summary_current());
        assert!(!stale_contract.lanes[0].visible_complete());
    }

    #[test]
    fn summary_readiness_rejects_stale_contracts_before_row_coverage() {
        let camera = camera(100);
        let stale_contract = AlignmentPanelLaneFrame {
            track_id: TrackId::new(7),
            preparation_camera: camera.into(),
            read_camera: Some(camera),
            row_activity: None,
            derived_lanes: Arc::new([]),
            source_camera_sequence: 3,
            snapshot: None,
            covered_interval: Some(camera.interval()),
            requested_rows: 100..110,
            covered_rows: 100..110,
            scene_contracts_current: false,
        };
        assert!(!stale_contract.summary_current());
        assert!(!stale_contract.visible_complete());

        let current_summary_with_rows_pending = AlignmentPanelLaneFrame {
            scene_contracts_current: true,
            covered_rows: 0..0,
            ..stale_contract
        };
        assert!(current_summary_with_rows_pending.summary_current());
        assert!(!current_summary_with_rows_pending.visible_complete());
    }

    #[test]
    fn clearing_a_lane_resets_its_source_sequence_for_a_new_authority() {
        let track = TrackId::new(7);
        let mut store = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            store.admit(track, publication(99, false)).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        assert!(store.clear_lane(track).unwrap());
        assert_eq!(
            store.admit(track, publication(1, false)).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        let frame = store
            .capture(
                PanelId::new(1),
                1,
                camera(100).viewport(),
                [lane_capture(track, 100)],
            )
            .unwrap();
        assert_eq!(frame.lanes[0].source_camera_sequence, 1);
    }

    #[test]
    fn removing_a_renderable_lane_retires_its_resident_authority() {
        let track = TrackId::new(7);
        let mut store = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            store.admit(track, publication(99, false)).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );

        store.replace_visible_tracks([]);
        let overview = store
            .capture(PanelId::new(1), 1, camera(0).viewport(), std::iter::empty())
            .unwrap();
        assert!(overview.lanes.is_empty());

        store.replace_visible_tracks([track]);
        assert_eq!(
            store.admit(track, publication(1, false)).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        let detailed = store
            .capture(
                PanelId::new(1),
                2,
                camera(100).viewport(),
                [lane_capture(track, 100)],
            )
            .unwrap();
        assert_eq!(detailed.lanes[0].source_camera_sequence, 1);
    }

    #[test]
    fn coherence_groups_are_captured_as_explicit_atomic_memberships() {
        let tracks = [TrackId::new(1), TrackId::new(2)];
        let mut store = AlignmentPanelSceneStore::new(tracks);
        let group = AlignmentCoherenceGroupSpec::new(
            AlignmentCoherenceGroupId::new(9),
            AlignmentCoherenceGroupKind::SashimiScale,
            tracks,
        )
        .unwrap();
        store.set_coherence_groups([group]).unwrap();
        let frame = store
            .capture(
                PanelId::new(4),
                1,
                camera(10).viewport(),
                [lane_capture(tracks[0], 10), lane_capture(tracks[1], 10)],
            )
            .unwrap();
        assert_eq!(frame.coherence_groups.len(), 1);
        assert_eq!(frame.coherence_groups[0].members.as_ref(), tracks);
        assert!(!frame.coherence_groups[0].visible_complete);
    }

    #[test]
    fn identical_coherence_group_specs_preserve_captured_authority() {
        let tracks = [TrackId::new(1), TrackId::new(2), TrackId::new(3)];
        let mut store = AlignmentPanelSceneStore::new(tracks);
        store
            .set_coherence_groups([
                AlignmentCoherenceGroupSpec::new(
                    AlignmentCoherenceGroupId::new(11),
                    AlignmentCoherenceGroupKind::SashimiScale,
                    [tracks[0], tracks[1]],
                )
                .unwrap(),
                AlignmentCoherenceGroupSpec::new(
                    AlignmentCoherenceGroupId::new(12),
                    AlignmentCoherenceGroupKind::Comparison,
                    [tracks[1], tracks[2]],
                )
                .unwrap(),
            ])
            .unwrap();
        let captured = store
            .capture(
                PanelId::new(4),
                1,
                camera(10).viewport(),
                tracks.map(|track_id| lane_capture(track_id, 10)),
            )
            .unwrap();

        store
            .set_coherence_groups([
                AlignmentCoherenceGroupSpec::new(
                    AlignmentCoherenceGroupId::new(12),
                    AlignmentCoherenceGroupKind::Comparison,
                    [tracks[2], tracks[1]],
                )
                .unwrap(),
                AlignmentCoherenceGroupSpec::new(
                    AlignmentCoherenceGroupId::new(11),
                    AlignmentCoherenceGroupKind::SashimiScale,
                    [tracks[1], tracks[0]],
                )
                .unwrap(),
            ])
            .unwrap();

        assert_eq!(store.authority_revision(), captured.authority_revision);
        let recaptured = store
            .capture(
                PanelId::new(4),
                2,
                camera(10).viewport(),
                tracks.map(|track_id| lane_capture(track_id, 10)),
            )
            .unwrap();
        assert_eq!(recaptured.authority_revision, captured.authority_revision);
        assert_eq!(
            recaptured
                .coherence_groups
                .iter()
                .map(|group| group.authority_revision)
                .collect::<Vec<_>>(),
            captured
                .coherence_groups
                .iter()
                .map(|group| group.authority_revision)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn changed_coherence_group_specs_invalidate_captured_authority() {
        let tracks = [TrackId::new(1), TrackId::new(2)];
        let mut store = AlignmentPanelSceneStore::new(tracks);
        let group_id = AlignmentCoherenceGroupId::new(9);
        store
            .set_coherence_groups([AlignmentCoherenceGroupSpec::new(
                group_id,
                AlignmentCoherenceGroupKind::SashimiScale,
                tracks,
            )
            .unwrap()])
            .unwrap();
        let captured = store
            .capture(
                PanelId::new(4),
                1,
                camera(10).viewport(),
                tracks.map(|track_id| lane_capture(track_id, 10)),
            )
            .unwrap();

        store
            .set_coherence_groups([AlignmentCoherenceGroupSpec::new(
                group_id,
                AlignmentCoherenceGroupKind::JunctionAnnotation,
                tracks,
            )
            .unwrap()])
            .unwrap();

        assert_ne!(store.authority_revision(), captured.authority_revision);
        let recaptured = store
            .capture(
                PanelId::new(4),
                2,
                camera(10).viewport(),
                tracks.map(|track_id| lane_capture(track_id, 10)),
            )
            .unwrap();
        assert_eq!(recaptured.authority_revision, store.authority_revision());
        assert_eq!(
            recaptured.coherence_groups[0].authority_revision,
            recaptured.authority_revision
        );
        assert_eq!(
            recaptured.coherence_groups[0].kind,
            AlignmentCoherenceGroupKind::JunctionAnnotation
        );
    }

    #[test]
    fn capture_preserves_fractional_panel_viewport_across_dictionary_remap() {
        let track = TrackId::new(3);
        let panel_viewport = Viewport::new(
            GenomicInterval::new(ContigId::new(1), 100, 200).unwrap(),
            500.0,
        )
        .unwrap()
        .pan_pixels(0.75, 10_000)
        .unwrap();
        let source_viewport = panel_viewport
            .remapped_contig(ContigId::new(19), 10_000)
            .unwrap();
        let source_camera = AlignmentCamera::from_viewport(
            source_viewport,
            0.0,
            12.0,
            9.0,
            CpuClipRect {
                x: 17.0,
                y: 100.0,
                width: 500.0,
                height: 120.0,
            },
        )
        .unwrap();
        let capture =
            AlignmentPanelLaneCapture::new(track, source_camera, Some(source_camera), []).unwrap();
        let store = AlignmentPanelSceneStore::new([track]);
        let frame = store
            .capture(PanelId::new(2), 9, panel_viewport, [capture])
            .unwrap();
        assert_eq!(frame.viewport, panel_viewport);
        assert!(approximately_equal(
            frame.lanes[0].preparation_camera.exact_start(),
            panel_viewport.exact_start()
        ));
        assert!(approximately_equal(
            frame.lanes[0].preparation_camera.span(),
            panel_viewport.span()
        ));
    }

    #[test]
    fn duplicate_or_horizontally_misaligned_derived_lanes_are_rejected() {
        let track = TrackId::new(4);
        let camera = camera(100);
        let coverage = |x| {
            AlignmentDerivedLaneCapture::new(
                CpuClipRect {
                    x,
                    y: 20.0,
                    width: camera.clip().width,
                    height: 40.0,
                },
                AlignmentDerivedLaneOptions::Coverage(AlignmentCoverageLaneOptions {
                    depth: CpuDepthProjectionOptions::default(),
                    alleles: Some(CpuAlleleProjectionOptions::default()),
                }),
            )
            .unwrap()
        };
        assert!(matches!(
            AlignmentPanelLaneCapture::new(track, camera, None, [coverage(0.0), coverage(0.0)]),
            Err(AlignmentPanelSceneError::DuplicateDerivedLane {
                track_id,
                kind: AlignmentDerivedLaneKind::Coverage
            }) if track_id == track
        ));
        let modifications = AlignmentDerivedLaneCapture::new(
            CpuClipRect {
                x: 1.0,
                y: 70.0,
                width: camera.clip().width,
                height: 30.0,
            },
            AlignmentDerivedLaneOptions::Modifications(AlignmentModificationLaneOptions {
                projection: CpuModificationProjectionOptions::default(),
                color_mode: ModifiedBaseColorMode::ByType,
                selected_position: None,
            }),
        )
        .unwrap();
        assert!(matches!(
            AlignmentPanelLaneCapture::new(track, camera, None, [coverage(0.0), modifications]),
            Err(AlignmentPanelSceneError::DerivedLaneCameraMismatch {
                track_id,
                kind: AlignmentDerivedLaneKind::Modifications
            }) if track_id == track
        ));
    }

    #[test]
    fn collapsed_reads_keep_derived_summary_authority_without_row_demand() {
        let camera = camera(100);
        let lane = AlignmentPanelLaneFrame {
            track_id: TrackId::new(7),
            preparation_camera: camera.into(),
            read_camera: None,
            row_activity: None,
            derived_lanes: Arc::new([]),
            source_camera_sequence: 1,
            snapshot: None,
            covered_interval: Some(camera.interval()),
            requested_rows: 0..0,
            covered_rows: 0..0,
            scene_contracts_current: true,
        };
        assert!(lane.summary_current());
        assert!(lane.reads_complete());
        assert!(lane.visible_complete());
    }

    #[test]
    fn coherence_summary_completeness_is_independent_of_visible_read_rows() {
        let camera = camera(100);
        let current = |track_id, covered_rows| AlignmentPanelLaneFrame {
            track_id,
            preparation_camera: camera.into(),
            read_camera: Some(camera),
            row_activity: None,
            derived_lanes: Arc::new([]),
            source_camera_sequence: 1,
            snapshot: None,
            covered_interval: Some(camera.interval()),
            requested_rows: 10..20,
            covered_rows,
            scene_contracts_current: true,
        };
        let lanes = [
            current(TrackId::new(1), 10..20),
            current(TrackId::new(2), 10..15),
        ];
        let (summary_complete, reads_complete) =
            coherence_group_completeness(&lanes, &[TrackId::new(1), TrackId::new(2)]);
        assert!(summary_complete);
        assert!(!reads_complete);
    }
}
