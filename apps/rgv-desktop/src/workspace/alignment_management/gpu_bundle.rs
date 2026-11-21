use crate::workspace::alignment_management::gpu_publication::{
    AlignmentGpuBundleFailure, AlignmentGpuCameraCaptureOutcome, AlignmentGpuCompositeRetirement,
    OwnedAlignmentGpuRequest, OwnedAlignmentSummaryGpuRequest,
    alignment_read_camera_capture_blocker, alignment_summary_camera_capture_blocker,
    settle_alignment_gpu_bundle,
};
use crate::workspace::alignment_management::scene_authority::AlignmentSemanticEpochRetirement;
use crate::workspace::alignment_management::{
    alignment_clip_gpu_scissor, alignment_scene_covered_clip, alignment_scene_gpu_scissor,
};
use crate::{
    AlignmentCommittedLaneAuthority, AlignmentGpuFrameBundle, AlignmentGpuPreparedAuthority,
    AlignmentNativeReadAuthority, Viewer, summary_junction_component,
};
use rgv_alignment_scene::{
    AlignmentDerivedLaneOptions, AlignmentInteractionState, AlignmentPanelSceneFrame,
    AlignmentPanelSceneStore, AllocationBundleActual, AllocationBundleError, AllocationBundleSlot,
    AllocationBundleSpec, AllocationRetirementToken, CpuSummaryCamera,
    build_alignment_semantic_retirement_claim_set,
};
use rgv_core::PanelId;
use rgv_engine::AlignmentIndividualReadCapability;
use rgv_render::{
    AlignmentGpuCameraCaptureError, AlignmentGpuFrame, AlignmentGpuRetirement,
    AlignmentGpuSceneRequest, AlignmentSummaryFrameOverlays, AlignmentSummaryGpuCameraCaptureError,
    AlignmentSummaryGpuFrame, AlignmentSummaryGpuLaneOptions, AlignmentSummaryGpuRetirement,
    AlignmentSummaryGpuSceneRequest, AlignmentSummaryJunctionInteraction,
};
use std::collections::HashMap;
use std::sync::Arc;

impl Viewer {
    pub(super) fn collect_alignment_gpu_requests(
        &self,
        panel_frames: &HashMap<PanelId, AlignmentPanelSceneFrame>,
        lane_authorities: &[AlignmentCommittedLaneAuthority],
        publication_attempt_sequence: u64,
        scale: f32,
    ) -> (
        Vec<OwnedAlignmentGpuRequest>,
        Vec<OwnedAlignmentSummaryGpuRequest>,
        usize,
    ) {
        let mut reads = Vec::new();
        let mut summaries = Vec::new();
        let unsupported_interpretations = 0_usize;
        for (panel_id, frame) in panel_frames {
            for lane in frame.lanes.iter() {
                let Some(source_authority) = lane_authorities.iter().find(|authority| {
                    authority.panel_id == *panel_id
                        && authority.track_id == lane.track_id
                        && authority.target_revision == frame.camera_revision
                        && authority.source_camera == lane.preparation_camera
                }) else {
                    continue;
                };
                if lane.scene_contracts_current
                    && !matches!(
                        source_authority.individual_reads,
                        AlignmentIndividualReadCapability::Refused { .. }
                    )
                    && let (Some(snapshot), Some(camera)) = (&lane.snapshot, lane.read_camera)
                    && let Some(live_clip) = lane.covered_interval.and_then(|covered| {
                        alignment_scene_covered_clip(camera.viewport(), camera.clip(), covered)
                    })
                    && let Some(scissor) = alignment_scene_gpu_scissor(
                        camera,
                        scale,
                        self.surface_config.width,
                        self.surface_config.height,
                    )
                {
                    let interaction = self
                        .alignment_tracks
                        .iter()
                        .find(|track| track.config.id == lane.track_id)
                        .and_then(|track| track.projection(*panel_id))
                        .map_or_else(AlignmentInteractionState::default, |projection| {
                            projection.scene_interaction.clone()
                        });
                    reads.push(OwnedAlignmentGpuRequest {
                        panel_id: *panel_id,
                        track_id: lane.track_id,
                        scene: Arc::clone(snapshot.reads()),
                        camera_sequence: publication_attempt_sequence,
                        camera,
                        scissor,
                        interaction,
                        live_clip: Some(live_clip),
                    });
                }

                if !lane.scene_contracts_current
                    || !lane
                        .covered_interval
                        .is_some_and(|covered| covered.overlaps(lane.preparation_camera.interval()))
                {
                    continue;
                }
                let Some(scene) = lane.snapshot.as_ref() else {
                    continue;
                };
                debug_assert_eq!(source_authority.source_camera, lane.preparation_camera);
                for capture in lane.derived_lanes.iter() {
                    let Ok(camera) = CpuSummaryCamera::from_viewport(
                        lane.preparation_camera.viewport(),
                        capture.clip,
                    ) else {
                        continue;
                    };
                    let Some(scissor) = alignment_clip_gpu_scissor(
                        capture.clip,
                        scale,
                        self.surface_config.width,
                        self.surface_config.height,
                    ) else {
                        continue;
                    };
                    if lane
                        .covered_interval
                        .and_then(|covered| {
                            alignment_scene_covered_clip(
                                lane.preparation_camera.viewport(),
                                capture.clip,
                                covered,
                            )
                        })
                        .is_none()
                    {
                        continue;
                    }
                    let (options, selected_position) = match &capture.options {
                        AlignmentDerivedLaneOptions::Coverage(options) => {
                            // Depth is reference-independent. While the optional FASTA product
                            // is pending, publish exact depth alone; the same immutable lane is
                            // upgraded with allele/substitution pages when reference arrives.
                            let mut available = *options;
                            if scene.reads().presentation().reference.is_none() {
                                available.alleles = None;
                            }
                            (
                                AlignmentSummaryGpuLaneOptions::Coverage(available),
                                options.depth.selected_position,
                            )
                        }
                        AlignmentDerivedLaneOptions::Modifications(options) => (
                            AlignmentSummaryGpuLaneOptions::Modifications(options.clone()),
                            options.selected_position,
                        ),
                        AlignmentDerivedLaneOptions::Sashimi(options) => {
                            (AlignmentSummaryGpuLaneOptions::Sashimi(*options), None)
                        }
                    };
                    let panel = self.panels.get(*panel_id);
                    let hovered_junction = panel
                        .and_then(|panel| panel.hovered_junction_arc)
                        .filter(|hover| hover.key.track_id == lane.track_id)
                        .map(|hover| AlignmentSummaryJunctionInteraction {
                            start: hover.key.start,
                            end: hover.key.end,
                            component: summary_junction_component(hover.key.component),
                        });
                    let selected_junction = panel
                        .filter(|panel| panel.selected_alignment_track == Some(lane.track_id))
                        .and_then(|panel| {
                            panel
                                .selected_junction
                                .zip(panel.selected_junction_component)
                        })
                        .map(
                            |((start, end), component)| AlignmentSummaryJunctionInteraction {
                                start,
                                end,
                                component: summary_junction_component(component),
                            },
                        );
                    summaries.push(OwnedAlignmentSummaryGpuRequest {
                        panel_id: *panel_id,
                        track_id: lane.track_id,
                        scene: Arc::clone(scene),
                        camera,
                        scissor,
                        options,
                        overlays: AlignmentSummaryFrameOverlays {
                            selected_position,
                            hovered_junction,
                            selected_junction,
                        },
                    });
                }
            }
        }
        (reads, summaries, unsupported_interpretations)
    }

    /// Attempts a camera-only capture over the immutable content authority of the currently
    /// presented epoch. Both renderer legs must succeed before either candidate becomes visible.
    pub(super) fn capture_alignment_gpu_bundle_from_resident_content(
        &mut self,
        read_requests: &[AlignmentGpuSceneRequest<'_>],
        summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    ) -> AlignmentGpuCameraCaptureOutcome {
        let Some(previous) = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
        else {
            return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
        };
        let read_authority = match (read_requests.is_empty(), previous.reads.as_ref()) {
            (true, None) => None,
            (false, Some(frame)) => Some(frame.content_authority().clone()),
            (true, Some(_)) | (false, None) => {
                return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
            }
        };
        let summary_authority = match (summary_requests.is_empty(), previous.summaries.as_ref()) {
            (true, None) => None,
            (false, Some(frame)) => Some(frame.content_authority().clone()),
            (true, Some(_)) | (false, None) => {
                return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
            }
        };

        let reads = match read_authority {
            Some(authority) => match self.alignment_gpu_renderer.capture_camera_frame(
                &self.device,
                &self.queue,
                &authority,
                read_requests,
                &self.alignment_scene_ledger,
            ) {
                Ok((frame, stats)) => {
                    self.alignment_gpu_last_prepare_stats = stats;
                    Some(frame)
                }
                Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired { .. }) => {
                    return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
                }
                Err(AlignmentGpuCameraCaptureError::FrameSlotBusy { slots }) => {
                    return AlignmentGpuCameraCaptureOutcome::Terminal(format!(
                        "internal read-camera slot invariant failed: all {slots} provisioned slots were retained"
                    ));
                }
                Err(error) => {
                    return if let Some(blocker) = alignment_read_camera_capture_blocker(&error) {
                        AlignmentGpuCameraCaptureOutcome::Deferred(blocker)
                    } else {
                        AlignmentGpuCameraCaptureOutcome::Terminal(error.to_string())
                    };
                }
            },
            None => None,
        };
        let summaries = match summary_authority {
            Some(authority) => match self.alignment_summary_gpu_renderer.capture_camera_frame(
                &self.device,
                &self.queue,
                &authority,
                summary_requests,
                &self.alignment_scene_ledger,
            ) {
                Ok((frame, stats)) => {
                    self.alignment_summary_gpu_last_prepare_stats = stats;
                    Some(frame)
                }
                Err(AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    ..
                }) => {
                    return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
                }
                Err(AlignmentSummaryGpuCameraCaptureError::FrameSlotBusy { slots }) => {
                    return AlignmentGpuCameraCaptureOutcome::Terminal(format!(
                        "internal summary-camera slot invariant failed: all {slots} provisioned slots were retained"
                    ));
                }
                Err(error) => {
                    return if let Some(blocker) = alignment_summary_camera_capture_blocker(&error) {
                        AlignmentGpuCameraCaptureOutcome::Deferred(blocker)
                    } else {
                        AlignmentGpuCameraCaptureOutcome::Terminal(error.to_string())
                    };
                }
            },
            None => None,
        };
        AlignmentGpuCameraCaptureOutcome::Captured { reads, summaries }
    }

    /// Detaches the complete old semantic desktop authority before aggregate admission. Nothing
    /// outside this synchronous transaction can observe the temporary empty slots. The returned
    /// token either rejoins the exact old epoch on rollback or is consumed alongside the GPU
    /// retirement at successful bundle settlement.
    pub(super) fn detach_alignment_semantic_epoch_retirement(
        &mut self,
    ) -> std::result::Result<Option<AlignmentSemanticEpochRetirement>, AllocationBundleError> {
        let Some(epoch) = self.alignment_presented_epoch.as_mut() else {
            debug_assert!(self.alignment_panel_scene_stores.is_empty());
            return Ok(None);
        };
        let panel_frames = std::mem::take(&mut epoch.panel_frames);
        let panel_scene_stores = std::mem::take(&mut self.alignment_panel_scene_stores);
        let planned = build_alignment_semantic_retirement_claim_set(
            &self.alignment_scene_ledger,
            panel_frames
                .values()
                .flat_map(|frame| frame.lanes.iter())
                .filter_map(|lane| lane.snapshot.as_ref())
                .chain(
                    panel_scene_stores
                        .values()
                        .flat_map(AlignmentPanelSceneStore::snapshots),
                ),
        );
        let (claim_set, stats) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                epoch.panel_frames = panel_frames;
                self.alignment_panel_scene_stores = panel_scene_stores;
                return Err(error);
            }
        };
        tracing::debug!(
            snapshot_occurrences = stats.snapshot_occurrences,
            unique_snapshots = stats.unique_snapshots,
            exclusively_owned_snapshots = stats.exclusively_owned_snapshots,
            exclusively_owned_read_scenes = stats.exclusively_owned_read_scenes,
            exclusively_owned_evidence_roots = stats.exclusively_owned_evidence_roots,
            exclusively_owned_row_atlases = stats.exclusively_owned_row_atlases,
            exclusively_owned_summary_roots = stats.exclusively_owned_summary_roots,
            exclusively_owned_read_pages = stats.exclusively_owned_read_pages,
            exclusively_owned_summary_pages = stats.exclusively_owned_summary_pages,
            owned_references_examined = stats.owned_references_examined,
            claimed_semantic_allocations = stats.claimed_allocations,
            "detached semantic component of alignment epoch retirement"
        );
        Ok(Some(AlignmentSemanticEpochRetirement {
            claim_set,
            stats,
            panel_frames,
            panel_scene_stores,
        }))
    }

    pub(super) fn restore_alignment_semantic_epoch_retirement(
        &mut self,
        retirement: AlignmentSemanticEpochRetirement,
    ) -> std::result::Result<(), String> {
        let Some(epoch) = self.alignment_presented_epoch.as_mut() else {
            return Err("semantic rollback has no prior presented epoch".into());
        };
        if !epoch.panel_frames.is_empty() || !self.alignment_panel_scene_stores.is_empty() {
            return Err("semantic rollback would overwrite an occupied presentation slot".into());
        }
        tracing::debug!(
            claimed_semantic_allocations = retirement.stats.claimed_allocations,
            "restoring detached semantic alignment epoch"
        );
        epoch.panel_frames = retirement.panel_frames;
        self.alignment_panel_scene_stores = retirement.panel_scene_stores;
        Ok(())
    }

    pub(super) fn try_publish_alignment_gpu_bundle(
        &mut self,
        target_revision: u64,
        publication_attempt_sequence: u64,
        read_requests: &[AlignmentGpuSceneRequest<'_>],
        summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        lane_authorities: Vec<AlignmentCommittedLaneAuthority>,
        read_authorities: Vec<AlignmentNativeReadAuthority>,
    ) -> std::result::Result<AlignmentGpuFrameBundle, AlignmentGpuBundleFailure> {
        let progress = self
            .alignment_scene_ledger
            .progress()
            .map_err(|error| AlignmentGpuBundleFailure::terminal(error.to_string()))?;
        let mut next_slot = AllocationBundleSlot::new(0);
        let read_plan = if read_requests.is_empty() {
            None
        } else {
            let plan = self
                .alignment_gpu_renderer
                .plan_replacement(read_requests, &progress, next_slot)
                .map_err(AlignmentGpuBundleFailure::read_replacement)?;
            next_slot = plan.next_bundle_slot();
            Some(plan)
        };
        let summary_plan = if summary_requests.is_empty() {
            None
        } else {
            let plan = self
                .alignment_summary_gpu_renderer
                .plan_replacement(summary_requests, &progress, next_slot)
                .map_err(AlignmentGpuBundleFailure::summary_replacement)?;
            Some(plan)
        };
        let prepared_authority = AlignmentGpuPreparedAuthority {
            read_pages: read_plan.as_ref().map_or_else(
                || Arc::from([]),
                |plan| Arc::clone(plan.prepared_page_keys()),
            ),
            summary_pages: summary_plan.as_ref().map_or_else(
                || Arc::from([]),
                |plan| Arc::clone(plan.prepared_page_keys()),
            ),
        };
        let mut demands = Vec::new();
        if let Some(plan) = &read_plan {
            demands.extend_from_slice(plan.demands());
        }
        if let Some(plan) = &summary_plan {
            demands.extend_from_slice(plan.demands());
        }

        let semantic_retirement = self
            .detach_alignment_semantic_epoch_retirement()
            .map_err(AlignmentGpuBundleFailure::bundle)?;

        let mut prior_bundle = self
            .alignment_presented_epoch
            .as_mut()
            .and_then(|epoch| epoch.gpu_bundle.take());
        let mut prior_reads = prior_bundle.as_mut().and_then(|bundle| bundle.reads.take());
        let mut prior_summaries = prior_bundle
            .as_mut()
            .and_then(|bundle| bundle.summaries.take())
            .into_iter()
            .collect::<Vec<_>>();

        let read_retirement = if let Some(plan) = &read_plan {
            match self.alignment_gpu_renderer.detach_replacement_retirement(
                &self.alignment_scene_ledger,
                plan,
                &mut prior_reads,
            ) {
                Ok(retirement) => Some(retirement),
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        None,
                        None,
                    )?;
                    return Err(AlignmentGpuBundleFailure::read_replacement(error));
                }
            }
        } else {
            None
        };
        let summary_retirement = if let Some(plan) = &summary_plan {
            match self
                .alignment_summary_gpu_renderer
                .detach_replacement_retirement(
                    &self.alignment_scene_ledger,
                    plan,
                    &mut prior_summaries,
                ) {
                Ok(retirement) => Some(retirement),
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        read_retirement,
                        None,
                    )?;
                    return Err(AlignmentGpuBundleFailure::summary_replacement(error));
                }
            }
        } else {
            None
        };
        let claim_set = {
            let mut claim_sets = Vec::with_capacity(3);
            if let Some(retirement) = &semantic_retirement {
                claim_sets.push(retirement.retirement_claim_set());
            }
            if let Some(retirement) = &read_retirement {
                claim_sets.push(retirement.retirement_claim_set());
            }
            if let Some(retirement) = &summary_retirement {
                claim_sets.push(retirement.retirement_claim_set());
            }
            match self
                .alignment_scene_ledger
                .combine_retirement_claim_sets(claim_sets)
            {
                Ok(claim_set) => claim_set,
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        read_retirement,
                        summary_retirement,
                    )?;
                    return Err(AlignmentGpuBundleFailure::bundle(error));
                }
            }
        };
        let reservation =
            match self
                .alignment_scene_ledger
                .reserve_replacement_bundle(AllocationBundleSpec::new(
                    progress.policy_revision,
                    demands,
                    claim_set.clone(),
                )) {
                Ok(reservation) => reservation,
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        read_retirement,
                        summary_retirement,
                    )?;
                    return Err(AlignmentGpuBundleFailure::bundle(error));
                }
            };
        let mut composite = Some(AlignmentGpuCompositeRetirement {
            claim_set,
            semantic: semantic_retirement,
            reads: read_retirement,
            summaries: summary_retirement,
        });

        let staged_reads = if let Some(plan) = read_plan {
            match self.alignment_gpu_renderer.stage_replacement(
                &self.device,
                &self.queue,
                read_requests,
                plan,
                &reservation,
                &self.alignment_scene_ledger,
                publication_attempt_sequence,
            ) {
                Ok(staged) => Some(staged),
                Err(error) => {
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::read_replacement(error));
                }
            }
        } else {
            None
        };
        let staged_summaries = if let Some(plan) = summary_plan {
            match self.alignment_summary_gpu_renderer.stage_replacement(
                &self.device,
                &self.queue,
                summary_requests,
                plan,
                &reservation,
                &self.alignment_scene_ledger,
                publication_attempt_sequence,
            ) {
                Ok(staged) => Some(staged),
                Err(error) => {
                    drop(staged_reads);
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::summary_replacement(error));
                }
            }
        } else {
            None
        };
        let ready_reads = if let Some(staged) = staged_reads {
            match self
                .alignment_gpu_renderer
                .preflight_replacement_publication(staged)
            {
                Ok(ready) => Some(ready),
                Err(error) => {
                    drop(staged_summaries);
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::read_replacement(error));
                }
            }
        } else {
            None
        };
        let ready_summaries = if let Some(staged) = staged_summaries {
            match self
                .alignment_summary_gpu_renderer
                .preflight_replacement_publication(staged)
            {
                Ok(ready) => Some(ready),
                Err(error) => {
                    drop(ready_reads);
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::summary_replacement(error));
                }
            }
        } else {
            None
        };

        let mut actuals = Vec::<AllocationBundleActual>::new();
        if let Some(ready) = &ready_reads {
            actuals.extend_from_slice(ready.actuals());
        }
        if let Some(ready) = &ready_summaries {
            actuals.extend_from_slice(ready.actuals());
        }
        let commit = match reservation.commit(actuals) {
            Ok(commit) => commit,
            Err(error) => {
                drop(ready_summaries);
                drop(ready_reads);
                self.restore_alignment_gpu_composite(
                    prior_bundle,
                    prior_reads,
                    prior_summaries,
                    composite.take(),
                )?;
                return Err(AlignmentGpuBundleFailure::bundle(error));
            }
        };
        let mut settled_leases = match settle_alignment_gpu_bundle(
            commit,
            composite
                .take()
                .expect("atomic candidate must own its retirement token"),
        ) {
            Ok(leases) => leases,
            Err((error, retirement)) => {
                drop(ready_summaries);
                drop(ready_reads);
                self.restore_alignment_gpu_composite(
                    prior_bundle,
                    prior_reads,
                    prior_summaries,
                    Some(retirement),
                )?;
                return Err(AlignmentGpuBundleFailure::bundle(error));
            }
        };

        // Every recoverable operation is now complete. Retirement, lease binding, and both
        // renderer cache mutations form an infallible suffix; the Viewer bundle is assigned by
        // the caller only after both publications finish.
        let publishable_reads = ready_reads.map(|ready| {
            self.alignment_gpu_renderer
                .bind_replacement_leases(ready, &mut settled_leases)
        });
        let publishable_summaries = ready_summaries.map(|ready| {
            self.alignment_summary_gpu_renderer
                .bind_replacement_leases(ready, &mut settled_leases)
        });
        assert!(
            settled_leases.is_empty(),
            "atomic alignment publication left an unclaimed bundle lease"
        );
        let reads = publishable_reads.map(|publishable| {
            let (frame, stats, _) = self.alignment_gpu_renderer.publish_replacement(publishable);
            self.alignment_gpu_last_prepare_stats = stats;
            frame
        });
        let summaries = publishable_summaries.map(|publishable| {
            let (frame, stats, _) = self
                .alignment_summary_gpu_renderer
                .publish_replacement(publishable);
            self.alignment_summary_gpu_last_prepare_stats = stats;
            frame
        });
        drop(prior_bundle);
        Ok(AlignmentGpuFrameBundle {
            target_revision,
            publication_attempt_sequence,
            prepared_authority,
            reads,
            summaries,
            lane_authorities,
            read_authorities,
        })
    }

    pub(super) fn restore_alignment_gpu_composite(
        &mut self,
        prior_bundle: Option<AlignmentGpuFrameBundle>,
        prior_reads: Option<AlignmentGpuFrame>,
        prior_summaries: Vec<AlignmentSummaryGpuFrame>,
        composite: Option<AlignmentGpuCompositeRetirement>,
    ) -> std::result::Result<(), AlignmentGpuBundleFailure> {
        let (semantic_retirement, read_retirement, summary_retirement) = composite
            .map_or((None, None, None), |composite| {
                (composite.semantic, composite.reads, composite.summaries)
            });
        self.restore_alignment_gpu_replacement(
            prior_bundle,
            prior_reads,
            prior_summaries,
            semantic_retirement,
            read_retirement,
            summary_retirement,
        )
    }

    pub(super) fn restore_alignment_gpu_replacement(
        &mut self,
        mut prior_bundle: Option<AlignmentGpuFrameBundle>,
        mut prior_reads: Option<AlignmentGpuFrame>,
        mut prior_summaries: Vec<AlignmentSummaryGpuFrame>,
        semantic_retirement: Option<AlignmentSemanticEpochRetirement>,
        read_retirement: Option<AlignmentGpuRetirement>,
        summary_retirement: Option<AlignmentSummaryGpuRetirement>,
    ) -> std::result::Result<(), AlignmentGpuBundleFailure> {
        let restored = (|| -> std::result::Result<(), String> {
            if let Some(retirement) = summary_retirement {
                prior_summaries = self
                    .alignment_summary_gpu_renderer
                    .restore_replacement_retirement(retirement)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(retirement) = read_retirement {
                prior_reads = self
                    .alignment_gpu_renderer
                    .restore_replacement_retirement(retirement)
                    .map_err(|error| error.to_string())?;
            }
            if prior_summaries.len() > 1 {
                return Err(format!(
                    "rollback recovered {} summary frames for a single atomic bundle",
                    prior_summaries.len()
                ));
            }
            if prior_bundle.is_none() && (prior_reads.is_some() || !prior_summaries.is_empty()) {
                return Err("rollback recovered frames without prior bundle metadata".into());
            }
            if let Some(bundle) = prior_bundle.as_mut() {
                bundle.reads = prior_reads.take();
                bundle.summaries = prior_summaries.pop();
            }
            match (self.alignment_presented_epoch.as_mut(), prior_bundle.take()) {
                (Some(epoch), bundle) => epoch.gpu_bundle = bundle,
                (None, None) => {}
                (None, Some(_)) => {
                    return Err("rollback recovered bundle metadata without its panel epoch".into());
                }
            }
            if let Some(retirement) = semantic_retirement {
                self.restore_alignment_semantic_epoch_retirement(retirement)?;
            }
            Ok(())
        })();
        if let Err(error) = restored {
            self.alignment_presented_epoch = None;
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_renderer.discard_device_resources();
            self.alignment_summary_gpu_renderer
                .discard_device_resources();
            return Err(AlignmentGpuBundleFailure::terminal(format!(
                "alignment GPU rollback failed closed: {error}"
            )));
        }
        Ok(())
    }
}
