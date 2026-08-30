use std::{collections::BTreeMap, ops::Range, sync::Arc};

use rgv_core::{GenomicInterval, PanelId, TrackId};
use thiserror::Error;

use crate::{AlignmentCamera, AlignmentSceneSnapshot, SceneRevision};

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
    SceneReplaced,
}

#[derive(Clone, Debug)]
struct ResidentAlignmentLane {
    source_camera_sequence: u64,
    snapshot: Option<Arc<AlignmentSceneSnapshot>>,
}

/// One render-and-interaction capture for one alignment track. The camera is live UI state; the
/// scene is the latest immutable worker publication. Their truthful intersection is explicit.
#[derive(Clone, Debug)]
pub struct AlignmentPanelLaneFrame {
    pub track_id: TrackId,
    pub camera: AlignmentCamera,
    pub source_camera_sequence: u64,
    pub snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    pub covered_interval: Option<GenomicInterval>,
    pub requested_rows: Range<u32>,
    pub covered_rows: Range<u32>,
}

impl AlignmentPanelLaneFrame {
    #[must_use]
    pub fn visible_complete(&self) -> bool {
        self.covered_interval
            .is_some_and(|covered| covered.contains_interval(self.camera.interval()))
            && self.covered_rows.start <= self.requested_rows.start
            && self.covered_rows.end >= self.requested_rows.end
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
    pub visible_complete: bool,
}

/// Immutable frame-boundary capture consumed by rendering and interaction together. Capturing a
/// new live camera clones only small lane descriptors and scene `Arc`s; it performs no source,
/// evidence, layout, page-build, or GPU work.
#[derive(Clone, Debug)]
pub struct AlignmentPanelSceneFrame {
    pub panel_id: PanelId,
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
        source_camera_sequence: u64,
        snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    ) -> Result<AlignmentPanelSceneAdmission, AlignmentPanelSceneError> {
        let lane = self
            .lanes
            .get_mut(&track_id)
            .ok_or(AlignmentPanelSceneError::UnknownTrack(track_id))?;
        if source_camera_sequence < lane.source_camera_sequence {
            return Ok(AlignmentPanelSceneAdmission::Stale);
        }
        let replacement = snapshot.as_ref().is_some_and(|candidate| {
            lane.snapshot
                .as_ref()
                .is_none_or(|current| current.revision() != candidate.revision())
        });
        lane.source_camera_sequence = source_camera_sequence;
        if let Some(snapshot) = snapshot {
            lane.snapshot = Some(snapshot);
        }
        if !replacement {
            return Ok(AlignmentPanelSceneAdmission::CameraAdvanced);
        }
        self.advance_authority();
        for group in &mut self.coherence_groups {
            if group.spec.members().contains(&track_id) {
                group.authority_revision = self.authority_revision;
            }
        }
        Ok(AlignmentPanelSceneAdmission::SceneReplaced)
    }

    /// Ends one lane's snapshot authority without removing its visible track slot. This is used
    /// for discontinuous locus transitions: a new source lane may restart its camera sequence,
    /// so retaining the prior sequence or row atlas would incorrectly reject the new scene.
    pub fn clear_lane(&mut self, track_id: TrackId) -> Result<bool, AlignmentPanelSceneError> {
        let lane = self
            .lanes
            .get_mut(&track_id)
            .ok_or(AlignmentPanelSceneError::UnknownTrack(track_id))?;
        let changed = lane.source_camera_sequence != 0 || lane.snapshot.take().is_some();
        lane.source_camera_sequence = 0;
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
        cameras: impl IntoIterator<Item = (TrackId, AlignmentCamera)>,
    ) -> Result<AlignmentPanelSceneFrame, AlignmentPanelSceneError> {
        let mut camera_map = BTreeMap::new();
        for (track_id, camera) in cameras {
            if camera_map.insert(track_id, camera).is_some() {
                return Err(AlignmentPanelSceneError::DuplicateCamera(track_id));
            }
        }
        let mut cameras = camera_map;
        if cameras.len() != self.lanes.len() {
            if let Some(track_id) = self
                .lanes
                .keys()
                .find(|track_id| !cameras.contains_key(track_id))
            {
                return Err(AlignmentPanelSceneError::MissingCamera(*track_id));
            }
            if let Some(track_id) = cameras
                .keys()
                .find(|track_id| !self.lanes.contains_key(track_id))
            {
                return Err(AlignmentPanelSceneError::UnknownTrack(*track_id));
            }
        }
        let mut lanes = Vec::with_capacity(self.lanes.len());
        for (track_id, resident) in &self.lanes {
            let camera = cameras
                .remove(track_id)
                .ok_or(AlignmentPanelSceneError::MissingCamera(*track_id))?;
            let requested_rows = resident.snapshot.as_ref().map_or_else(
                || camera.visible_rows(),
                |scene| {
                    let row_count =
                        u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
                    let visible = camera.visible_rows();
                    visible.start.min(row_count)..visible.end.min(row_count)
                },
            );
            let (covered_interval, covered_rows) =
                resident.snapshot.as_ref().map_or((None, 0..0), |scene| {
                    (
                        intersect(camera.interval(), scene.prepared_interval()),
                        intersect_ranges(requested_rows.clone(), scene.reads().prepared_rows()),
                    )
                });
            lanes.push(AlignmentPanelLaneFrame {
                track_id: *track_id,
                camera,
                source_camera_sequence: resident.source_camera_sequence,
                snapshot: resident.snapshot.clone(),
                covered_interval,
                requested_rows,
                covered_rows,
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
                let visible_complete = members.iter().all(|track_id| {
                    lanes
                        .iter()
                        .find(|lane| lane.track_id == *track_id)
                        .is_some_and(AlignmentPanelLaneFrame::visible_complete)
                });
                AlignmentCoherenceGroupFrame {
                    id: group.spec.id(),
                    kind: group.spec.kind(),
                    authority_revision: group.authority_revision,
                    members: members.into(),
                    scene_revisions: scene_revisions.into(),
                    visible_complete,
                }
            })
            .collect::<Vec<_>>();
        Ok(AlignmentPanelSceneFrame {
            panel_id,
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

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum AlignmentPanelSceneError {
    #[error("alignment panel scene does not contain track {0:?}")]
    UnknownTrack(TrackId),
    #[error("alignment panel scene capture has no live camera for track {0:?}")]
    MissingCamera(TrackId),
    #[error("alignment panel scene capture contains more than one camera for track {0:?}")]
    DuplicateCamera(TrackId),
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

    #[test]
    fn capture_requires_exactly_one_live_camera_per_visible_track() {
        let tracks = [TrackId::new(1), TrackId::new(2)];
        let store = AlignmentPanelSceneStore::new(tracks);
        assert!(matches!(
            store.capture(PanelId::new(1), 1, [(tracks[0], camera(10))]),
            Err(AlignmentPanelSceneError::MissingCamera(track)) if track == tracks[1]
        ));
        let frame = store
            .capture(
                PanelId::new(1),
                2,
                [(tracks[0], camera(10)), (tracks[1], camera(20))],
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
            store.admit(track, 3, None).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        assert_eq!(
            store.admit(track, 2, None).unwrap(),
            AlignmentPanelSceneAdmission::Stale
        );
        let frame = store
            .capture(PanelId::new(1), 5, [(track, camera(100))])
            .unwrap();
        assert_eq!(frame.lanes[0].source_camera_sequence, 3);
    }

    #[test]
    fn clearing_a_lane_resets_its_source_sequence_for_a_new_authority() {
        let track = TrackId::new(7);
        let mut store = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            store.admit(track, 99, None).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        assert!(store.clear_lane(track).unwrap());
        assert_eq!(
            store.admit(track, 1, None).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        let frame = store
            .capture(PanelId::new(1), 1, [(track, camera(100))])
            .unwrap();
        assert_eq!(frame.lanes[0].source_camera_sequence, 1);
    }

    #[test]
    fn removing_a_renderable_lane_retires_its_resident_authority() {
        let track = TrackId::new(7);
        let mut store = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            store.admit(track, 99, None).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );

        store.replace_visible_tracks([]);
        let overview = store
            .capture(PanelId::new(1), 1, std::iter::empty())
            .unwrap();
        assert!(overview.lanes.is_empty());

        store.replace_visible_tracks([track]);
        assert_eq!(
            store.admit(track, 1, None).unwrap(),
            AlignmentPanelSceneAdmission::CameraAdvanced
        );
        let detailed = store
            .capture(PanelId::new(1), 2, [(track, camera(100))])
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
                [(tracks[0], camera(10)), (tracks[1], camera(10))],
            )
            .unwrap();
        assert_eq!(frame.coherence_groups.len(), 1);
        assert_eq!(frame.coherence_groups[0].members.as_ref(), tracks);
        assert!(!frame.coherence_groups[0].visible_complete);
    }
}
