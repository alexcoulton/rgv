use crate::inspection::rna::selected_supplementary_chain;
use crate::ui::TrackFrameKey;
use crate::workspace::alignments::{
    AlignmentHighlightCacheKey, alignment_highlight_filter_contract,
};
use crate::workspace::tracks::replace_panel_track_selection;
use crate::{PresentedAlignmentLane, Viewer, alignment_gpu_bundle_is_current, ui};
use rgv_alignment_scene::{
    AlignmentComponent, AlignmentHandle, AlignmentHit, AlignmentInteractionState,
    AlignmentRowActivityProfile, AlignmentSceneSnapshot, evaluate_alignment_filter,
};
use rgv_core::{PanelId, TrackId};
use rgv_engine::{AlignmentSceneRequestToken, SourceOwnerId};
use rgv_hts::AlignmentRecord;
use std::collections::BTreeSet;
use std::sync::Arc;

impl Viewer {
    pub(crate) fn synchronize_alignment_scene_interactions(&mut self, panel_id: PanelId) {
        let (hovered, selected) = self
            .session
            .panels
            .get(panel_id)
            .map_or((None, None), |panel| {
                (panel.hovered_alignment_hit, panel.selected_alignment_hit)
            });
        let track_ids = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        for track_id in track_ids {
            let highlighted = self.alignment_highlight_handles(panel_id, track_id);
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            else {
                continue;
            };
            let hovered = hovered
                .filter(|hit| hit.track == track.config.id)
                .map(|hit| hit.handle);
            let selected = selected
                .filter(|hit| hit.track == track.config.id)
                .map(|hit| hit.handle);
            track.projection_mut(panel_id).scene_interaction =
                AlignmentInteractionState::new(hovered, selected, std::iter::empty())
                    .with_highlighted(highlighted.iter().copied());
        }
    }

    /// Invalidates only the derived highlight selection, then immediately rebinds every affected
    /// panel's interaction overlay from its currently available scene/reference authority.
    /// Highlighting never changes evidence membership, row assignment, or summaries.
    pub(crate) fn refresh_alignment_highlight_interactions(&mut self, track_ids: &[TrackId]) {
        let panel_ids = self
            .alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .flat_map(|track| track.projections.keys().copied())
            .collect::<BTreeSet<_>>();
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            for projection in track.projections.values_mut() {
                projection.invalidate_scene_highlight();
            }
        }
        for panel_id in panel_ids {
            self.synchronize_alignment_scene_interactions(panel_id);
        }
    }

    pub(super) fn alignment_highlight_handles(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Arc<[AlignmentHandle]> {
        let Some(track_index) = self
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == track_id)
        else {
            return Arc::from([]);
        };
        let Some(metadata) = self.alignment_tracks[track_index].scene_metadata.as_ref() else {
            return Arc::from([]);
        };
        let contract = alignment_highlight_filter_contract(
            &self.alignment_tracks[track_index].config.filters,
            &metadata.dictionary,
        );
        if contract.is_empty() {
            let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
            projection.invalidate_scene_highlight();
            return Arc::clone(&projection.scene_highlighted);
        }
        let Some(snapshot) = self.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_frame.as_ref())
            .and_then(|frame| frame.snapshot.as_ref())
            .cloned()
        else {
            return Arc::from([]);
        };
        let compatible_revision =
            self.compatible_alignment_reference_revision(&metadata.dictionary);
        let target = snapshot.prepared_interval();
        let reference = self.session.panels.get(panel_id).and_then(|panel| {
            let revision = panel.reference_slice_revision?;
            let slice = panel.reference_slice.as_deref()?;
            (Some(revision) == compatible_revision).then_some((revision, slice))
        });
        let key = AlignmentHighlightCacheKey {
            evidence_revision: snapshot.reads().evidence().revision(),
            target,
            filter: contract.fingerprint(),
            reference: contract
                .requires_reference()
                .then(|| reference.map(|(revision, slice)| (revision, slice.interval)))
                .flatten(),
        };
        if let Some(projection) = self.alignment_tracks[track_index].projection(panel_id)
            && projection.scene_highlight_key == Some(key)
        {
            return Arc::clone(&projection.scene_highlighted);
        }
        let reference = reference
            .map(|(_, slice)| slice)
            .filter(|slice| slice.interval.contains_interval(target));
        let evaluation =
            evaluate_alignment_filter(snapshot.reads().evidence(), target, &contract, reference);
        let (highlighted, status): (Arc<[AlignmentHandle]>, Option<String>) = match evaluation {
            Ok(evaluation) => (evaluation.into_matching_handles().into(), None),
            Err(error) => (
                Arc::from([]),
                Some(format!("Highlights unavailable: {error}")),
            ),
        };
        let matching_handles = highlighted.len();
        let diagnostic_status = status.clone();
        let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
        projection.scene_highlight_key = Some(key);
        projection.scene_highlighted = Arc::clone(&highlighted);
        projection.scene_highlight_status = status;
        self.diagnostics.event(
            "alignment_filter",
            "highlight_evaluated",
            serde_json::json!({
                "panel_id": panel_id.get(),
                "track_id": track_id.get(),
                "evidence_revision": {
                    "high": key.evidence_revision.fingerprint().high(),
                    "low": key.evidence_revision.fingerprint().low(),
                },
                "target": {
                    "contig": key.target.contig().get(),
                    "start": key.target.start(),
                    "end": key.target.end(),
                },
                "filter_contract_fingerprint": key.filter.get(),
                "reference_revision": key.reference.map(|(revision, _)| serde_json::json!({
                    "high": revision.fingerprint().high(),
                    "low": revision.fingerprint().low(),
                })),
                "reference_interval": key.reference.map(|(_, interval)| serde_json::json!({
                    "contig": interval.contig().get(),
                    "start": interval.start(),
                    "end": interval.end(),
                })),
                "matching_handles": matching_handles,
                "status": diagnostic_status,
            }),
        );
        highlighted
    }

    pub(crate) fn alignment_scene_snapshot_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<&Arc<AlignmentSceneSnapshot>> {
        let presented = self.presented_alignment_lane(panel_id, track_id)?;
        let snapshot = presented.lane.snapshot.as_ref()?;
        presented.read_authority.map(|_| snapshot)
    }

    /// Binds cross-panel/name-based read intent to the stable handle in the newly committed scene.
    /// Query-local record indexes are never introduced on this path. Resolution is deliberately
    /// performed after the atomic epoch commit, so selection can only name evidence that render,
    /// picking, and inspection can all observe together.
    pub(super) fn resolve_pending_alignment_scene_selections(&mut self) {
        let Some(epoch) = self.alignment_presented_epoch.as_ref() else {
            return;
        };
        let resolutions = self
            .session
            .panels
            .iter()
            .filter_map(|panel| {
                let pending = panel.pending_read_selection.clone()?;
                let lane = epoch.panel_frames.get(&panel.id)?.lane(pending.track_id)?;
                let snapshot = lane.snapshot.as_ref()?;
                let (handle, record) = pending.matching_scene_record(snapshot.reads())?;
                let chain = selected_supplementary_chain(&record, &panel.contig_name);
                let (retained, reveal) = pending.retained_after_resolution(Some(&record));
                Some((
                    panel.id,
                    retained.track_id,
                    snapshot.revision(),
                    handle,
                    chain,
                    retained,
                    reveal,
                ))
            })
            .collect::<Vec<_>>();
        if resolutions.is_empty() {
            return;
        }
        let mut resolved_panels = Vec::with_capacity(resolutions.len());
        for (panel_id, track_id, scene_revision, handle, chain, retained, reveal) in resolutions {
            let Some(panel) = self.session.panels.get_mut(panel_id) else {
                continue;
            };
            panel.pending_read_selection = Some(retained);
            panel.selected_alignment_track = Some(track_id);
            panel.selected_alignment_hit = Some(AlignmentHit {
                track: track_id,
                scene_revision,
                handle,
                component: AlignmentComponent::ReadSpan,
            });
            panel.selected_chain = chain;
            if reveal {
                panel.pending_alignment_reveal = Some((track_id, handle));
            }
            replace_panel_track_selection(
                panel,
                TrackFrameKey::Alignment(track_id),
                ui::TrackPane::Reads,
            );
            resolved_panels.push(panel_id);
        }
        resolved_panels.sort_unstable();
        resolved_panels.dedup();
        for panel_id in resolved_panels {
            self.synchronize_alignment_scene_interactions(panel_id);
        }
        self.mark_display_dirty("explicit display invalidation");
        self.window.request_redraw();
    }

    pub(crate) fn presented_alignment_lane(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<PresentedAlignmentLane<'_>> {
        let epoch = self.alignment_presented_epoch.as_ref()?;
        let bundle = epoch.gpu_bundle.as_ref()?;
        if !alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority) {
            return None;
        }
        let panel_frame = epoch.panel_frames.get(&panel_id)?;
        if panel_frame.camera_revision != bundle.target_revision {
            return None;
        }
        let lane = panel_frame.lane(track_id)?;
        let source = bundle.lane_authorities.iter().find(|authority| {
            authority.panel_id == panel_id
                && authority.track_id == track_id
                && authority.target_revision == bundle.target_revision
                && authority.source_camera == lane.preparation_camera
        })?;
        let read_authority = lane.snapshot.as_ref().and_then(|snapshot| {
            bundle.read_authorities.iter().find(|authority| {
                authority.panel_id == panel_id
                    && authority.track_id == track_id
                    && authority.target_revision == bundle.target_revision
                    && authority.source_request_token == source.source_request_token
                    && authority.desired_request_token == source.desired_request_token
                    && authority.source_camera == source.source_camera
                    && authority.scene_revision == snapshot.reads().revision()
            })
        });
        Some(PresentedAlignmentLane {
            target_revision: bundle.target_revision,
            lane,
            source,
            read_authority,
        })
    }

    /// Returns committed genomic row authority while a vertically changed pixel target is still
    /// being prepared. Unlike `presented_alignment_lane`, this accessor deliberately does not
    /// require the old GPU bundle to match the complete desired camera: row extent is independent
    /// of visible-row page readiness. The publication authority first proves that the committed
    /// and desired lanes still describe the same genomic interval.
    pub(crate) fn committed_alignment_row_activity_for_desired_target(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<(u64, Arc<AlignmentRowActivityProfile>)> {
        if !self
            .alignment_scene_publication_authority
            .committed_row_target_matches_desired(panel_id, track_id)
        {
            return None;
        }
        let epoch = self.alignment_presented_epoch.as_ref()?;
        let bundle = epoch.gpu_bundle.as_ref()?;
        if bundle.target_revision != self.alignment_scene_publication_authority.target_revision {
            return None;
        }
        let panel_frame = epoch.panel_frames.get(&panel_id)?;
        if panel_frame.camera_revision != bundle.target_revision {
            return None;
        }
        let lane = panel_frame.lane(track_id)?;
        bundle.lane_authorities.iter().find(|authority| {
            authority.panel_id == panel_id
                && authority.track_id == track_id
                && authority.target_revision == bundle.target_revision
                && authority.source_camera == lane.preparation_camera
        })?;
        let profile = lane.row_activity.as_ref()?;
        if profile.interval() != lane.preparation_camera.interval() {
            return None;
        }
        Some((bundle.target_revision, Arc::clone(profile)))
    }

    pub(crate) fn alignment_scene_record_for_hit(
        &self,
        panel_id: PanelId,
        hit: AlignmentHit,
    ) -> Option<AlignmentRecord> {
        let snapshot = self.alignment_scene_snapshot_for_panel_track(panel_id, hit.track)?;
        snapshot.inspect_retained(hit.handle).ok().flatten()
    }

    pub(super) fn take_alignment_scene_owner(&mut self) -> SourceOwnerId {
        self.alignment_scene_owner_clock = self
            .alignment_scene_owner_clock
            .checked_add(1)
            .expect("alignment scene owner identity space was exhausted");
        SourceOwnerId::new(self.alignment_scene_owner_clock)
    }

    pub(crate) fn take_alignment_scene_request_token(&mut self) -> AlignmentSceneRequestToken {
        self.alignment_scene_request_clock = self
            .alignment_scene_request_clock
            .checked_add(1)
            .expect("alignment scene request identity space was exhausted");
        AlignmentSceneRequestToken::new(self.alignment_scene_request_clock)
    }
}
