use std::{num::NonZeroUsize, ops::Range, sync::Arc};

use rgv_alignment_scene::{
    AlignmentPresentationStore, AlignmentRowActivityProfile, AlignmentSceneSnapshot,
};
use rgv_core::GenomicInterval;

use crate::SourceOwnerId;

use super::{
    ALIGNMENT_ROW_ACTIVITY_BINS, AlignmentCoordinatorFrame, AlignmentIndividualReadCapability,
    AlignmentRowActivityCache, AlignmentSceneSourceLane, AlignmentVerticalResidency,
    alignment_row_activity_satisfies_contract, alignment_row_atlas_satisfies_contract,
    clamp_visible_rows, presentation_core_matches, presentation_owner,
};

impl AlignmentSceneSourceLane {
    pub(crate) fn frame(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> Option<AlignmentCoordinatorFrame> {
        let desired = self.desired.as_ref()?;
        let snapshot = presentation
            .prepared_for(presentation_owner(owner))
            .cloned();
        let requested_rows = snapshot.as_ref().map_or_else(
            || desired.camera.visible_rows(),
            |scene| {
                let row_count =
                    u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
                clamp_visible_rows(desired.camera.visible_rows(), row_count)
            },
        );
        let (covered_interval, covered_rows) = snapshot.as_ref().map_or((None, 0..0), |scene| {
            (
                intersect_intervals(desired.camera.interval(), scene.prepared_interval()),
                intersect_ranges(requested_rows.clone(), scene.reads().prepared_rows()),
            )
        });
        let scene_contracts_current = snapshot.as_ref().is_some_and(|scene| {
            presentation_core_matches(scene.reads().presentation(), self.contract.presentation)
                && self.scene_roots_satisfy_contract(scene)
        });
        let reference_requirement = self.reference_requirement(desired);
        let reference_products_current = snapshot.as_ref().is_some_and(|scene| {
            reference_requirement.is_ready()
                && scene.reads().presentation() == self.contract.presentation
        });
        let visible_complete = scene_contracts_current
            && covered_interval
                .is_some_and(|covered| covered.contains_interval(desired.camera.interval()))
            && covered_rows.start <= requested_rows.start
            && covered_rows.end >= requested_rows.end;
        let row_activity = desired
            .camera
            .has_visible_rows()
            .then(|| {
                snapshot
                    .as_ref()
                    .and_then(|scene| self.row_activity_for_frame(scene, desired.camera.interval()))
            })
            .flatten();
        let buffered_camera_refusal = visible_complete
            .then(|| row_activity.as_ref())
            .flatten()
            .filter(|activity| {
                activity.visible_record_count() > self.contract.complete_read_limit.get()
            })
            .map(|activity| AlignmentIndividualReadCapability::Refused {
                target: desired.camera.interval(),
                complete_read_limit: self.contract.complete_read_limit.get(),
                eligible_records: u64::try_from(activity.visible_record_count())
                    .unwrap_or(u64::MAX),
            });
        let individual_reads = if !desired.camera.has_visible_rows() {
            // This camera requested genomic summary authority only. Do not report an
            // individual-read capability for work the caller deliberately omitted.
            AlignmentIndividualReadCapability::Pending
        } else if let Some(refusal) = buffered_camera_refusal {
            // The immutable read-ahead scene may contain more total records than the viewport
            // limit. Its camera-exact activity index enforces the limit after a contained pan
            // without throwing away the buffer or reopening BAM I/O.
            refusal
        } else if visible_complete {
            AlignmentIndividualReadCapability::Complete {
                scene_revision: snapshot
                    .as_ref()
                    .expect("complete frame coverage requires a scene")
                    .revision(),
            }
        } else if let Some(refusal) = self
            .refusal
            .filter(|refusal| refusal.target == desired.camera.interval())
        {
            AlignmentIndividualReadCapability::Refused {
                target: refusal.target,
                complete_read_limit: refusal.complete_read_limit,
                eligible_records: refusal.eligible_records,
            }
        } else {
            AlignmentIndividualReadCapability::Pending
        };
        let vertical_residency = if !desired.camera.has_visible_rows() {
            AlignmentVerticalResidency::ViewportBounded
        } else {
            snapshot
                .as_ref()
                .map_or(AlignmentVerticalResidency::ViewportBounded, |scene| {
                    self.vertical_residency_state(scene)
                })
        };
        Some(AlignmentCoordinatorFrame {
            camera: desired.camera,
            camera_sequence: desired.camera_sequence,
            snapshot,
            covered_interval,
            requested_rows,
            covered_rows,
            row_activity,
            reference_requirement,
            scene_contracts_current,
            reference_products_current,
            individual_reads,
            vertical_residency,
        })
    }

    pub(crate) fn row_activity_for_frame(
        &self,
        scene: &AlignmentSceneSnapshot,
        camera_interval: GenomicInterval,
    ) -> Option<Arc<AlignmentRowActivityProfile>> {
        if !alignment_row_atlas_satisfies_contract(scene, camera_interval, &self.contract) {
            *self.row_activity_cache.borrow_mut() = None;
            return None;
        }

        let atlas = scene.reads().atlas();
        let atlas_revision = atlas.revision();
        let max_bins = NonZeroUsize::new(ALIGNMENT_ROW_ACTIVITY_BINS)
            .expect("the fixed row-activity bin bound is non-zero");
        let mut cache = self.row_activity_cache.borrow_mut();
        if let Some(cached) = cache.as_mut()
            && cached.atlas_revision == atlas_revision
        {
            if cached.cursor.interval() == camera_interval {
                let mut telemetry = self.row_activity_telemetry.get();
                telemetry.record_cache_hit();
                self.row_activity_telemetry.set(telemetry);
                return Some(Arc::clone(&cached.profile));
            }
            if cached.cursor.update(camera_interval) {
                let stats = cached.cursor.last_update_stats();
                let mut telemetry = self.row_activity_telemetry.get();
                telemetry.record_cursor_update(stats);
                self.row_activity_telemetry.set(telemetry);
                let profile = Arc::new(cached.cursor.profile(max_bins));
                debug_assert!(alignment_row_activity_satisfies_contract(
                    &profile,
                    scene,
                    camera_interval,
                    &self.contract,
                ));
                cached.profile = Arc::clone(&profile);
                return Some(profile);
            }
        }

        let cursor = atlas.activity_cursor(camera_interval)?;
        let stats = cursor.last_update_stats();
        let mut telemetry = self.row_activity_telemetry.get();
        telemetry.record_rebuild(stats);
        self.row_activity_telemetry.set(telemetry);
        let profile = Arc::new(cursor.profile(max_bins));
        debug_assert!(alignment_row_activity_satisfies_contract(
            &profile,
            scene,
            camera_interval,
            &self.contract,
        ));
        *cache = Some(AlignmentRowActivityCache {
            atlas_revision,
            cursor,
            profile: Arc::clone(&profile),
        });
        Some(profile)
    }
}

pub(crate) fn intersect_intervals(
    left: GenomicInterval,
    right: GenomicInterval,
) -> Option<GenomicInterval> {
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

pub(crate) fn intersect_ranges(left: Range<u32>, right: Range<u32>) -> Range<u32> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);
    start..end.max(start)
}
