use crate::diagnostics::object_fields;
use crate::workspace::alignment_management::gpu_publication::{
    AlignmentGpuBundleFailure, AlignmentGpuCameraCaptureOutcome, AlignmentGpuReclaimOutcome,
    begin_alignment_gpu_publication_attempt,
};
use crate::workspace::alignment_management::scene_authority::{
    AlignmentPanelSceneEpochCaptureFailure, CapturedAlignmentLaneSource,
    CapturedAlignmentPanelSceneEpoch,
};
use crate::workspace::alignment_management::{
    alignment_scene_progress_frame_defers_to_incumbent,
    alignment_scene_snapshot_satisfies_contract, alignment_scene_staging_frame_is_current,
    alignment_scene_viewport_for_layout_width, retained_alignment_scene_can_present,
};
use crate::workspace::alignments::{ALIGNMENT_SASHIMI_CPM_GROUP, ALIGNMENT_SASHIMI_RAW_GROUP};
use crate::{
    AlignmentCommittedLaneAuthority, AlignmentGpuFrameBundle, AlignmentGpuPreparedAuthority,
    AlignmentNativeReadAuthority, AlignmentSceneTargetIdentity, PresentedAlignmentEpoch, Viewer,
};
use rgv_alignment_scene::{
    AlignmentCoherenceGroupKind, AlignmentCoherenceGroupSpec, AlignmentDerivedLaneOptions,
    AlignmentPanelLanePublication, AlignmentPanelSceneAdmission, AlignmentSashimiLaneOptions,
    AlignmentSashimiScalePolicy, AlleleSummaryDetail,
};
use rgv_engine::{
    AlignmentIndividualReadCapability, AlignmentVerticalResidency,
    alignment_row_activity_satisfies_contract,
};
use rgv_render::{AlignmentGpuRenderer, AlignmentGpuVisibleAdmission, AlignmentSummaryGpuRenderer};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

impl Viewer {
    pub(super) fn alignment_filter_authorities_for_trace(
        &self,
        authorities: &[AlignmentCommittedLaneAuthority],
    ) -> Vec<serde_json::Value> {
        if !self.diagnostics.recording() {
            return Vec::new();
        }
        authorities
            .iter()
            .map(|authority| {
                let projection = self
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == authority.track_id)
                    .and_then(|track| track.projection(authority.panel_id));
                let presented = self
                    .presented_alignment_lane(authority.panel_id, authority.track_id)
                    .and_then(|presented| presented.lane.snapshot.as_ref())
                    .map(|snapshot| {
                        let filter = snapshot.reads().evidence().view_filter();
                        serde_json::json!({
                            "scene_revision": {
                                "high": snapshot.revision().fingerprint().high(),
                                "low": snapshot.revision().fingerprint().low(),
                            },
                            "filter_evaluation_fingerprint": filter.fingerprint().get(),
                            "eligible_records": filter.eligible_records(),
                            "matching_records": filter.matching_records(),
                            "predicate_matches": filter.predicate_matches(),
                            "row_atlas_filter_evaluation_fingerprint": snapshot.reads().atlas().view_filter().get(),
                            "summary_evidence_coherent": snapshot.summaries().evidence_revision()
                                == snapshot.reads().evidence().revision(),
                        })
                    });
                serde_json::json!({
                    "panel_id": authority.panel_id.get(),
                    "track_id": authority.track_id.get(),
                    "source_request_token": authority.source_request_token.get(),
                    "desired_request_token": authority.desired_request_token.get(),
                    "source_acquisitions_after": projection.map_or(0, |projection| projection.scene_telemetry.source_acquisitions),
                    "presented": presented,
                })
            })
            .collect()
    }

    pub(super) fn prepare_alignment_scene_frame_atomic(&mut self) {
        // Capture first, then expose the desired target before fallible publication. The prior
        // committed epoch retains ownership for rollback, but render/hit/live-region consumers
        // stale-gate it immediately when the actual camera or layout target changed.
        let scale_factor = self.window.scale_factor();
        let capture_started = Instant::now();
        let candidate_epoch = self.capture_alignment_panel_scene_epoch(0);
        let capture_elapsed = capture_started.elapsed();
        let mut candidate_epoch = match candidate_epoch {
            Ok(candidate) => candidate,
            Err(mut failure) => {
                failure.fields.insert(
                    "capture_elapsed_us".to_owned(),
                    serde_json::json!(capture_elapsed.as_micros()),
                );
                failure.fields.insert(
                    "publication_disposition".to_owned(),
                    serde_json::json!(
                        "rejected before target authority advanced; committed epoch retained"
                    ),
                );
                self.report_error(failure.message.clone());
                self.queue_manual_diagnostic_trace_failure(
                    "alignment_publication",
                    "panel_scene_capture_failed",
                    &failure.message,
                    failure.fields,
                );
                return;
            }
        };
        let uncovered_vertical_lanes =
            self.uncovered_vertical_candidate_lane_count(&candidate_epoch);
        if uncovered_vertical_lanes > 0 {
            let lane_diagnostics = candidate_epoch
                .panel_frames
                .iter()
                .flat_map(|(panel_id, frame)| {
                    frame.lanes.iter().map(|lane| {
                        serde_json::json!({
                            "panel_id": panel_id.get(),
                            "track_id": lane.track_id.get(),
                            "camera_revision": frame.camera_revision,
                            "preparation_interval": format!("{:?}", lane.preparation_camera.interval()),
                            "read_camera_present": lane.read_camera.is_some(),
                            "requested_rows": {
                                "start": lane.requested_rows.start,
                                "end": lane.requested_rows.end,
                            },
                            "covered_rows": {
                                "start": lane.covered_rows.start,
                                "end": lane.covered_rows.end,
                            },
                            "reads_complete": lane.reads_complete(),
                        })
                    })
                })
                .collect::<Vec<_>>();
            self.diagnostics.event(
                "alignment_publication",
                "uncovered_vertical_candidate_retained",
                serde_json::json!({
                    "lane_count": uncovered_vertical_lanes,
                    "capture_elapsed_us": capture_elapsed.as_micros(),
                    "reason": "requested vertical rows have no prepared intersection; retaining the coherent committed epoch",
                    "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                    "committed_target_revision": self.alignment_scene_publication_authority.target_revision,
                    "candidate_lanes": lane_diagnostics,
                    "scroll_cache_authorities": self.manual_scroll_cache_diagnostic_state(self.panels.active().id),
                }),
            );
            return;
        }
        let target = AlignmentSceneTargetIdentity::from_panel_frames(
            self.workspace_layout.revision,
            [self.surface_config.width, self.surface_config.height],
            scale_factor,
            &candidate_epoch.panel_frames,
        );
        let retry_was_pending = self.alignment_gpu_publication_retry.is_some();
        let attempt = if retry_was_pending {
            let snapshot = match self.alignment_gpu_ledger.progress() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.alignment_gpu_publication_retry = None;
                    self.data_status =
                        format!("Could not inspect deferred alignment GPU capacity: {error}");
                    return;
                }
            };
            begin_alignment_gpu_publication_attempt(
                &mut self.alignment_scene_publication_authority,
                &mut self.alignment_gpu_publication_retry,
                &target,
                &snapshot,
            )
        } else {
            Some(
                self.alignment_scene_publication_authority
                    .begin_attempt(&target),
            )
        };
        let Some(attempt) = attempt else {
            self.diagnostics.event(
                "alignment_publication",
                "same_target_retry_coalesced",
                serde_json::json!({
                    "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                    "publication_attempt_sequence": self.alignment_scene_publication_authority.attempt_sequence(),
                    "capture_elapsed_us": capture_elapsed.as_micros(),
                    "panel_frames": candidate_epoch.panel_frames.len(),
                    "lane_sources": candidate_epoch.lane_sources.len(),
                }),
            );
            tracing::trace!(
                target_revision = self
                    .alignment_scene_publication_authority
                    .desired_target_revision(),
                publication_attempt_sequence = self
                    .alignment_scene_publication_authority
                    .attempt_sequence(),
                "coalescing same-target alignment publication behind unchanged ledger pressure"
            );
            return;
        };
        if self
            .alignment_capacity_reclaim
            .as_ref()
            .is_some_and(|cycle| {
                cycle.target_revision != attempt.target_revision
                    || cycle.attempt_sequence != attempt.sequence
            })
        {
            self.alignment_capacity_reclaim = None;
        }
        let target_revision = attempt.target_revision;
        let publication_attempt_sequence = attempt.sequence;
        self.diagnostics.event(
            "alignment_publication",
            "atomic_prepare_started",
            serde_json::json!({
                "target_revision": target_revision,
                "publication_attempt_sequence": publication_attempt_sequence,
                "target_changed": attempt.target_changed,
                "retry_was_pending": retry_was_pending,
                "capture_elapsed_us": capture_elapsed.as_micros(),
                "panel_frames": candidate_epoch.panel_frames.len(),
                "lane_sources": candidate_epoch.lane_sources.len(),
                "workspace_layout_revision": self.workspace_layout.revision,
                "surface": [self.surface_config.width, self.surface_config.height],
                "scale_factor": scale_factor,
            }),
        );
        tracing::trace!(
            target_revision,
            publication_attempt_sequence,
            target_changed = attempt.target_changed,
            "preparing atomic alignment publication"
        );
        for frame in candidate_epoch.panel_frames.values_mut() {
            frame.camera_revision = target_revision;
        }
        let lane_authorities = candidate_epoch
            .lane_sources
            .iter()
            .map(|source| AlignmentCommittedLaneAuthority {
                panel_id: source.panel_id,
                track_id: source.track_id,
                target_revision,
                source_request_token: source.semantic_request_token,
                desired_request_token: source.desired_request_token,
                source_camera: source.camera,
                individual_reads: source.individual_reads,
                semantic_target_complete: source.semantic_target_complete,
                reference_ready: source.reference_ready,
                vertical_residency: source.vertical_residency,
            })
            .collect::<Vec<_>>();
        // Retain a small identity-only copy for exact transaction settlement even when the
        // renderer publication consumes the candidate bundle vectors below.
        let navigation_candidates = lane_authorities.clone();
        if retry_was_pending {
            self.resume_alignment_navigation_transactions_after_gpu_progress(
                &navigation_candidates,
            );
        }

        let scale = scale_factor as f32;
        let screen_size_points = [
            self.surface_config.width as f32 / scale.max(f32::EPSILON),
            self.surface_config.height as f32 / scale.max(f32::EPSILON),
        ];
        let (mut owned_reads, mut owned_summaries, unsupported_interpretations) = self
            .collect_alignment_gpu_requests(
                &candidate_epoch.panel_frames,
                &lane_authorities,
                publication_attempt_sequence,
                scale,
            );
        owned_reads.sort_by_key(|request| (request.panel_id, request.track_id));
        owned_summaries.sort_by_key(|request| (request.panel_id, request.track_id));
        if unsupported_interpretations > 0 {
            self.report_error(format!(
                "{unsupported_interpretations} coverage lane{} unavailable: this allele interpretation is not yet supported by the unified GPU renderer",
                if unsupported_interpretations == 1 {
                    " is"
                } else {
                    "s are"
                }
            ));
        }

        let read_requests = owned_reads
            .iter()
            .map(|request| request.borrowed(screen_size_points))
            .collect::<Vec<_>>();
        let summary_requests = owned_summaries
            .iter()
            .map(|request| request.borrowed(publication_attempt_sequence, screen_size_points))
            .collect::<Vec<_>>();
        self.diagnostics.event(
            "alignment_publication",
            "gpu_requests_collected",
            serde_json::json!({
                "target_revision": target_revision,
                "publication_attempt_sequence": publication_attempt_sequence,
                "read_requests": read_requests.len(),
                "summary_requests": summary_requests.len(),
                "unsupported_interpretations": unsupported_interpretations,
                "lane_authorities": lane_authorities.len(),
            }),
        );
        let read_authorities = owned_reads
            .iter()
            .filter_map(|request| {
                let source = lane_authorities.iter().find(|authority| {
                    authority.panel_id == request.panel_id && authority.track_id == request.track_id
                })?;
                request.live_clip.map(|clip| AlignmentNativeReadAuthority {
                    panel_id: request.panel_id,
                    track_id: request.track_id,
                    target_revision,
                    source_request_token: source.source_request_token,
                    desired_request_token: source.desired_request_token,
                    source_camera: source.source_camera,
                    scene_revision: request.scene.revision(),
                    materialized_row_ranges:
                        crate::workspace::panels::normalize_materialized_row_ranges(
                            request.scene.pages().iter().map(|page| page.row_range()),
                        ),
                    clip,
                })
            })
            .collect::<Vec<_>>();

        if read_requests.is_empty() && summary_requests.is_empty() {
            let unchanged_authoritative_empty = !attempt.target_changed
                && lane_authorities.is_empty()
                && self
                    .alignment_presented_epoch
                    .as_ref()
                    .and_then(|epoch| epoch.gpu_bundle.as_ref())
                    .is_some_and(|bundle| {
                        bundle.reads.is_none()
                            && bundle.summaries.is_none()
                            && bundle.lane_authorities.is_empty()
                    });
            if unchanged_authoritative_empty {
                self.diagnostics.event(
                    "alignment_publication",
                    "unchanged_empty_epoch_retained",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "reason": "the already committed target has no alignment pixel demand",
                    }),
                );
                return;
            }
            let bundle = AlignmentGpuFrameBundle {
                target_revision,
                publication_attempt_sequence,
                prepared_authority: AlignmentGpuPreparedAuthority {
                    read_pages: Arc::from([]),
                    summary_pages: Arc::from([]),
                },
                reads: None,
                summaries: None,
                lane_authorities,
                read_authorities: Vec::new(),
            };
            assert!(
                self.alignment_scene_publication_authority
                    .commit_attempt(&attempt),
                "alignment publication attempt was superseded during synchronous preparation"
            );
            self.commit_alignment_navigation_transactions(&bundle);
            self.alignment_panel_scene_stores = candidate_epoch.panel_scene_stores;
            self.alignment_presented_epoch = Some(PresentedAlignmentEpoch::new(
                candidate_epoch.panel_frames,
                bundle,
            ));
            self.resolve_pending_alignment_scene_selections();
            self.invalidate_stale_alignment_scroll_cache();
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate.reset();
            self.diagnostics.event(
                "alignment_publication",
                "empty_epoch_committed",
                serde_json::json!({
                    "target_revision": target_revision,
                    "publication_attempt_sequence": publication_attempt_sequence,
                    "panel_frames": self.alignment_presented_epoch.as_ref().map_or(0, |epoch| epoch.panel_frames.len()),
                    "reason": "no native read or summary GPU requests were captured",
                }),
            );
            return;
        }

        let current_prepared_authority = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .map(|bundle| bundle.prepared_authority.clone());
        match self
            .capture_alignment_gpu_bundle_from_resident_content(&read_requests, &summary_requests)
        {
            AlignmentGpuCameraCaptureOutcome::Captured { reads, summaries } => {
                let bundle = AlignmentGpuFrameBundle {
                    target_revision,
                    publication_attempt_sequence,
                    prepared_authority: current_prepared_authority.expect(
                        "camera-only capture requires the previously committed content authority",
                    ),
                    reads,
                    summaries,
                    lane_authorities,
                    read_authorities,
                };
                assert!(
                    self.alignment_scene_publication_authority
                        .commit_attempt(&attempt),
                    "alignment publication attempt was superseded during synchronous preparation"
                );
                self.commit_alignment_navigation_transactions(&bundle);
                self.alignment_panel_scene_stores = candidate_epoch.panel_scene_stores;
                self.alignment_presented_epoch = Some(PresentedAlignmentEpoch::new(
                    candidate_epoch.panel_frames,
                    bundle,
                ));
                self.resolve_pending_alignment_scene_selections();
                self.invalidate_stale_alignment_scroll_cache();
                // A capture over a partially resident presentation is partial too.
                self.record_alignment_gpu_partial_residency();
                // Camera capture preserves the immutable prepared authority. Keep any in-flight
                // cursor and its pins instead of restarting complete-scene discovery on every
                // pan/scroll frame.
                if self.alignment_gpu_prefetch.pending() {
                    self.window.request_redraw();
                }
                let filter_authorities =
                    self.alignment_filter_authorities_for_trace(&navigation_candidates);
                self.diagnostics.event(
                    "alignment_publication",
                    "camera_only_capture_committed",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                        "filter_authorities": filter_authorities,
                    }),
                );
                return;
            }
            AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired => {
                self.diagnostics.event(
                    "alignment_publication",
                    "content_replacement_required",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                    }),
                );
            }
            AlignmentGpuCameraCaptureOutcome::Deferred(blocker) => {
                // Camera-ring or other non-content pressure is not evidence that the source or
                // immutable scene failed. Retain the coherent prior epoch for a later retry.
                tracing::debug!(?blocker, "deferring alignment camera-only capture");
                self.diagnostics.event(
                    "alignment_publication",
                    "camera_capture_deferred",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "blocker": format!("{blocker:?}"),
                    }),
                );
                self.wait_alignment_navigation_transactions_for_gpu(&navigation_candidates);
                self.defer_alignment_gpu_publication(&attempt, blocker);
                return;
            }
            AlignmentGpuCameraCaptureOutcome::Terminal(error) => {
                self.alignment_gpu_prefetch.clear();
                self.fail_alignment_navigation_gpu_candidates(&navigation_candidates, &error);
                self.data_status =
                    format!("Could not capture the current alignment camera presentation: {error}");
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "camera_capture_terminal",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "error": error,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                    })),
                );
                return;
            }
        }

        let protected_read_pages = match AlignmentGpuRenderer::mandatory_page_keys(&read_requests) {
            Ok(keys) => keys,
            Err(error) => {
                self.data_status =
                    format!("Could not inventory mandatory alignment GPU pages: {error}");
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "read_page_inventory_failed",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "error": error.to_string(),
                        "read_requests": read_requests.len(),
                    })),
                );
                return;
            }
        };
        let protected_summary_pages =
            match AlignmentSummaryGpuRenderer::mandatory_page_keys(&summary_requests) {
                Ok(keys) => keys,
                Err(error) => {
                    self.data_status =
                        format!("Could not inventory mandatory summary GPU pages: {error}");
                    let message = self.data_status.clone();
                    self.freeze_manual_diagnostic_trace(
                        "alignment_publication",
                        "summary_page_inventory_failed",
                        &message,
                        object_fields(serde_json::json!({
                            "target_revision": target_revision,
                            "publication_attempt_sequence": publication_attempt_sequence,
                            "error": error.to_string(),
                            "summary_requests": summary_requests.len(),
                        })),
                    );
                    return;
                }
            };
        self.diagnostics.event(
            "alignment_publication",
            "mandatory_pages_inventoried",
            serde_json::json!({
                "target_revision": target_revision,
                "publication_attempt_sequence": publication_attempt_sequence,
                "read_pages": protected_read_pages.len(),
                "summary_pages": protected_summary_pages.len(),
            }),
        );
        let budget_bytes = match self.alignment_gpu_ledger.progress() {
            Ok(snapshot) => snapshot.budget,
            Err(error) => {
                self.report_error(format!("Could not inspect alignment GPU budget: {error}"));
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "ledger_inspection_failed",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "error": error.to_string(),
                    })),
                );
                return;
            }
        };
        // Camera capture already proved that immutable content changed. Old all-scene residency
        // and cursor pins are speculative, so release them before the new atomic plan reserves
        // capacity. The replacement itself will mint and publish the successor authority once.
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();

        let mut planned_read_page_bytes = 0_usize;
        let first = self.try_publish_alignment_gpu_bundle(
            target_revision,
            publication_attempt_sequence,
            &read_requests,
            &summary_requests,
            lane_authorities.clone(),
            read_authorities.clone(),
            AlignmentGpuVisibleAdmission::Complete,
            &mut planned_read_page_bytes,
        );
        let published = match first {
            Ok(bundle) => Ok(bundle),
            Err(failure) if failure.blocker.is_some() => Err(failure),
            Err(failure) => match failure.pressure {
                Some(pressure) => {
                    self.alignment_last_gpu_budget_pressure = Some(pressure);
                    match self.reclaim_alignment_gpu_caches_for_foreground(
                        pressure,
                        &protected_read_pages,
                        &protected_summary_pages,
                    ) {
                        Ok(AlignmentGpuReclaimOutcome::Satisfied { .. }) => self
                            .try_publish_alignment_gpu_bundle(
                                target_revision,
                                publication_attempt_sequence,
                                &read_requests,
                                &summary_requests,
                                lane_authorities.clone(),
                                read_authorities.clone(),
                                AlignmentGpuVisibleAdmission::Complete,
                                &mut planned_read_page_bytes,
                            ),
                        // Reclaim could not make room for the complete candidate. Under the
                        // bounded policy draw what fits; only when that is impossible too does
                        // the strict settlement run.
                        Ok(outcome) => {
                            if let Some(bundle) = self
                                .publish_alignment_gpu_bundle_drawing_what_fits(
                                    target_revision,
                                    publication_attempt_sequence,
                                    &read_requests,
                                    &summary_requests,
                                    &lane_authorities,
                                    &read_authorities,
                                    pressure,
                                    planned_read_page_bytes,
                                )
                            {
                                Ok(bundle)
                            } else if let AlignmentGpuReclaimOutcome::Infeasible {
                                required_bytes,
                                ..
                            } = outcome
                            {
                                self.wait_alignment_navigation_transactions_for_gpu(
                                    &navigation_candidates,
                                );
                                let message = format!(
                                    "Complete alignment presentation is cache-budget limited: the minimum exact presentation exceeds the configured capacity by {} MiB",
                                    required_bytes.saturating_add((1 << 20) - 1) >> 20,
                                );
                                self.settle_alignment_navigation_gpu_budget_limited(&message);
                                self.report_warning(message);
                                self.mark_display_dirty("explicit display invalidation");
                                self.window.request_redraw();
                                return;
                            } else {
                                self.defer_alignment_gpu_publication_for_cross_owner_reclaim(
                                    &attempt,
                                    &navigation_candidates,
                                    pressure,
                                );
                                return;
                            }
                        }
                        Err(error) => Err(AlignmentGpuBundleFailure::terminal(format!(
                            "could not coordinate alignment GPU cache reclaim: {error}"
                        ))),
                    }
                }
                None => Err(failure),
            },
        };

        let published = match published {
            Err(failure) if failure.pressure.is_some() => {
                let pressure = failure
                    .pressure
                    .expect("guarded aggregate GPU failure retains typed pressure");
                self.alignment_last_gpu_budget_pressure = Some(pressure);
                // The retry after a satisfied reclaim was refused as well: draw what fits
                // before handing the shortfall to the other cache owners.
                if let Some(bundle) = self.publish_alignment_gpu_bundle_drawing_what_fits(
                    target_revision,
                    publication_attempt_sequence,
                    &read_requests,
                    &summary_requests,
                    &lane_authorities,
                    &read_authorities,
                    pressure,
                    planned_read_page_bytes,
                ) {
                    Ok(bundle)
                } else {
                    self.defer_alignment_gpu_publication_for_cross_owner_reclaim(
                        &attempt,
                        &navigation_candidates,
                        pressure,
                    );
                    return;
                }
            }
            outcome => outcome,
        };

        match published {
            Ok(bundle) => {
                self.alignment_gpu_speculation_gate
                    .observe_foreground(bundle.prepared_authority.clone(), budget_bytes);
                // The semantic panel epoch and its native derivative become observable together.
                // A failed candidate leaves the previous (now stale-gated) pair untouched.
                assert!(
                    self.alignment_scene_publication_authority
                        .commit_attempt(&attempt),
                    "alignment publication attempt was superseded during synchronous preparation"
                );
                self.commit_alignment_navigation_transactions(&bundle);
                self.alignment_panel_scene_stores = candidate_epoch.panel_scene_stores;
                self.alignment_presented_epoch = Some(PresentedAlignmentEpoch::new(
                    candidate_epoch.panel_frames,
                    bundle,
                ));
                self.resolve_pending_alignment_scene_selections();
                self.invalidate_stale_alignment_scroll_cache();
                let deferred_read_pages = self.record_alignment_gpu_partial_residency();
                if deferred_read_pages > 0 {
                    // Visible pages are still waiting for GPU budget: speculative warm-up of
                    // pages outside the view must not compete with them.
                    self.alignment_gpu_prefetch.clear();
                    self.alignment_gpu_speculation_gate
                        .suppress_for_foreground_pressure();
                    self.window.request_redraw();
                } else {
                    // Speculation must never delay publication of the frame that satisfied the
                    // foreground camera. The render loop advances this authority only after
                    // that frame has been submitted and presented.
                    self.alignment_gpu_prefetch
                        .reset(!read_requests.is_empty(), !summary_requests.is_empty());
                    if self.alignment_gpu_prefetch.pending() {
                        self.window.request_redraw();
                    }
                }
                let filter_authorities =
                    self.alignment_filter_authorities_for_trace(&navigation_candidates);
                self.diagnostics.event(
                    "alignment_publication",
                    "content_replacement_committed",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                        "deferred_read_pages": deferred_read_pages,
                        "prefetch_reads": self.alignment_gpu_prefetch.reads,
                        "prefetch_summaries": self.alignment_gpu_prefetch.summaries,
                        "filter_authorities": filter_authorities,
                    }),
                );
            }
            Err(failure) => {
                self.alignment_gpu_prefetch.clear();
                if let Some(blocker) = failure.blocker {
                    tracing::debug!(
                        ?blocker,
                        "deferring alignment replacement until the active ledger transition settles"
                    );
                    self.wait_alignment_navigation_transactions_for_gpu(&navigation_candidates);
                    self.defer_alignment_gpu_publication(&attempt, blocker);
                    return;
                }
                self.alignment_gpu_speculation_gate
                    .suppress_for_foreground_pressure();
                let message = format!(
                    "Could not prepare one atomic alignment GPU presentation: {}",
                    failure.message
                );
                self.fail_alignment_navigation_gpu_candidates(&navigation_candidates, &message);
                self.report_error(message);
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "content_replacement_terminal",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "failure": failure.message,
                        "pressure": failure.pressure.map(|pressure| format!("{pressure:?}")),
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                    })),
                );
            }
        }
    }

    pub(super) fn capture_alignment_panel_scene_epoch(
        &mut self,
        camera_revision: u64,
    ) -> std::result::Result<CapturedAlignmentPanelSceneEpoch, AlignmentPanelSceneEpochCaptureFailure>
    {
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut captured = HashMap::new();
        let mut lane_sources = Vec::new();
        let mut critical_capture_failure = None;
        // The complete semantic panel authority remains private until native GPU publication
        // succeeds. A rejected/superseded candidate drops this clone and leaves the committed
        // store byte-for-byte unchanged.
        let mut panel_scene_stores = self.alignment_panel_scene_stores.clone();
        for panel_id in panel_ids {
            let Some((stored_panel_viewport, contig_name)) = self
                .panels
                .get(panel_id)
                .map(|panel| (panel.viewport, panel.contig_name.clone()))
            else {
                continue;
            };
            let Some(layout_width) = self
                .panel_column_rect(panel_id)
                .map(|rect| f64::from(rect.width()))
                .filter(|width| width.is_finite() && *width > 0.0)
            else {
                continue;
            };
            let Some(panel_viewport) =
                alignment_scene_viewport_for_layout_width(stored_panel_viewport, layout_width)
            else {
                let message = format!(
                    "Could not capture alignment scene: panel {} has an invalid live layout width",
                    panel_id.get()
                );
                critical_capture_failure.get_or_insert_with(|| {
                    (
                        message,
                        object_fields(serde_json::json!({
                            "panel_id": panel_id.get(),
                            "panel_contig": contig_name,
                            "stored_pixel_width": stored_panel_viewport.pixel_width(),
                            "layout_pixel_width": layout_width,
                            "camera_revision": camera_revision,
                            "workspace_layout_revision": self.workspace_layout.revision,
                            "surface": [self.surface_config.width, self.surface_config.height],
                        })),
                    )
                });
                continue;
            };
            let mut lane_captures = Vec::new();
            let mut panel_lane_sources = Vec::new();
            // Progress frames which would withdraw a lane whose replacement is still owned by a
            // live navigation transaction. They are held back so the committed incumbent keeps
            // presenting until the replacement publishes atomically.
            let mut panel_deferred_sources = Vec::new();
            let mut capture_error = None;
            for track in &self.alignment_tracks {
                let Some(projection) = track.projection(panel_id) else {
                    continue;
                };
                if projection.index_only_overview || projection.scene_owner.is_none() {
                    continue;
                }
                let Some(metadata) = track.scene_metadata.as_ref() else {
                    continue;
                };
                let Some(contig) = metadata.dictionary.id_by_name(&contig_name) else {
                    continue;
                };
                let Some(contig_length) =
                    metadata.dictionary.get(contig).map(|entry| entry.length())
                else {
                    continue;
                };
                let reference_backed_alleles =
                    projection.scene_contract.as_ref().is_some_and(|contract| {
                        contract.summaries.allele_detail != AlleleSummaryDetail::None
                            && contract.presentation.reference.is_some()
                    });
                match self.alignment_scene_lane_capture_for_panel_track(
                    panel_id,
                    track.config.id,
                    contig,
                    contig_length,
                    reference_backed_alleles,
                ) {
                    Ok(Some(mut capture)) => {
                        let desired_token = projection.navigation.request_token();
                        let staged_token = projection.scene_frame_request_token;
                        if let (
                            Some(frame),
                            Some(semantic_request_token),
                            Some(desired_request_token),
                        ) = (projection.scene_frame.as_ref(), staged_token, desired_token)
                        {
                            let exact_staging = alignment_scene_staging_frame_is_current(
                                desired_token,
                                staged_token,
                                projection.navigation.desired_camera(),
                                frame.camera,
                                capture.preparation_camera,
                            );
                            // Semantic compatibility and visible page readiness are independent.
                            // A progressive snapshot remains the truthful source for the new
                            // camera even when its newly requested row block has not arrived yet;
                            // the panel frame records the covered row intersection explicitly.
                            let snapshot_contract_current =
                                frame.snapshot.as_ref().is_some_and(|snapshot| {
                                    projection.scene_contract.as_ref().is_some_and(|contract| {
                                        alignment_scene_snapshot_satisfies_contract(
                                            snapshot, contract,
                                        )
                                    })
                                });
                            let snapshot = snapshot_contract_current
                                .then(|| frame.snapshot.as_ref().map(Arc::clone))
                                .flatten();
                            let row_activity =
                                projection.scene_contract.as_ref().and_then(|contract| {
                                    let snapshot = frame.snapshot.as_ref()?;
                                    let profile = frame.row_activity.as_ref()?;
                                    alignment_row_activity_satisfies_contract(
                                        profile,
                                        snapshot,
                                        capture.preparation_camera.interval(),
                                        contract,
                                    )
                                    .then(|| Arc::clone(profile))
                                });
                            capture = capture.with_row_activity(row_activity);
                            // An old worker camera may lend its immutable, contract-matching
                            // snapshot and genomic row profile to the live camera. Its completion
                            // status remains Pending until the exact desired request-and-camera
                            // pair publishes; missing visible row pages remain explicit coverage,
                            // not a reason to revoke the scrollbar's independent row authority.
                            let candidate_contracts_current = snapshot_contract_current
                                && (exact_staging && frame.scene_contracts_current
                                    || !exact_staging);
                            let defers_to_incumbent =
                                alignment_scene_progress_frame_defers_to_incumbent(
                                    exact_staging,
                                    candidate_contracts_current,
                                    projection.navigation.phase(),
                                );
                            if exact_staging || snapshot.is_some() {
                                let sources = if defers_to_incumbent {
                                    &mut panel_deferred_sources
                                } else {
                                    &mut panel_lane_sources
                                };
                                sources.push(CapturedAlignmentLaneSource {
                                    panel_id,
                                    track_id: track.config.id,
                                    semantic_request_token,
                                    desired_request_token,
                                    camera: capture.preparation_camera,
                                    individual_reads: if exact_staging {
                                        frame.individual_reads
                                    } else {
                                        AlignmentIndividualReadCapability::Pending
                                    },
                                    semantic_target_complete: exact_staging
                                        && (frame.visible_complete()
                                            || frame.individual_reads_refused_for_camera()),
                                    reference_ready: exact_staging
                                        && frame.reference_requirement.is_ready(),
                                    vertical_residency: if exact_staging {
                                        frame.vertical_residency
                                    } else {
                                        AlignmentVerticalResidency::ViewportBounded
                                    },
                                    camera_sequence: frame.camera_sequence,
                                    snapshot,
                                    scene_contracts_current: candidate_contracts_current,
                                });
                            }
                        }
                        lane_captures.push(capture);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        capture_error = Some(error.to_string());
                        break;
                    }
                }
            }
            if let Some(error) = capture_error {
                self.report_error(format!("Could not resolve alignment scene lanes: {error}"));
                critical_capture_failure.get_or_insert_with(|| {
                    (
                        self.data_status.clone(),
                        object_fields(serde_json::json!({
                            "panel_id": panel_id.get(),
                            "panel_contig": contig_name,
                            "panel_viewport": {
                                "start": panel_viewport.interval().start(),
                                "end": panel_viewport.interval().end(),
                                "exact_start": panel_viewport.exact_start(),
                                "span": panel_viewport.span(),
                                "pixel_width": panel_viewport.pixel_width(),
                            },
                            "camera_revision": camera_revision,
                            "workspace_layout_revision": self.workspace_layout.revision,
                            "surface": [self.surface_config.width, self.surface_config.height],
                            "lane_capture_count": lane_captures.len(),
                        })),
                    )
                });
                continue;
            }
            let track_ids = lane_captures
                .iter()
                .map(|capture| capture.track_id)
                .collect::<Vec<_>>();
            let coherence_groups = [ALIGNMENT_SASHIMI_RAW_GROUP, ALIGNMENT_SASHIMI_CPM_GROUP]
                .into_iter()
                .filter_map(|group_id| {
                    let members = lane_captures
                        .iter()
                        .filter(|capture| {
                            capture.derived_lanes.iter().any(|lane| {
                                matches!(
                                    lane.options,
                                    AlignmentDerivedLaneOptions::Sashimi(
                                        AlignmentSashimiLaneOptions {
                                            scale: AlignmentSashimiScalePolicy::Shared(id),
                                            ..
                                        }
                                    ) if id == group_id
                                )
                            })
                        })
                        .map(|capture| capture.track_id)
                        .collect::<Vec<_>>();
                    (!members.is_empty()).then(|| {
                        AlignmentCoherenceGroupSpec::new(
                            group_id,
                            AlignmentCoherenceGroupKind::SashimiScale,
                            members,
                        )
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>();
            let coherence_groups = match coherence_groups {
                Ok(groups) => groups,
                Err(error) => {
                    self.data_status =
                        format!("Could not resolve alignment coherence groups: {error}");
                    continue;
                }
            };
            let store = panel_scene_stores.entry(panel_id).or_default();
            store.replace_visible_tracks(track_ids.iter().copied());
            if let Err(error) = store.set_coherence_groups(coherence_groups) {
                self.report_error(format!("Could not set alignment coherence groups: {error}"));
                continue;
            }
            let mut admitted_panel_sources = Vec::new();
            for source in panel_lane_sources {
                let track_id = source.track_id;
                if source.snapshot.is_none() {
                    if let Err(error) = store.clear_lane(track_id) {
                        self.report_error(format!("Could not clear alignment scene: {error}"));
                        continue;
                    }
                    admitted_panel_sources.push(source);
                    continue;
                }
                let publication = AlignmentPanelLanePublication::new(
                    source.camera_sequence,
                    source.snapshot.clone(),
                    source.scene_contracts_current,
                );
                match store.admit(track_id, publication) {
                    Ok(AlignmentPanelSceneAdmission::Stale) => {}
                    Ok(_) => admitted_panel_sources.push(source),
                    Err(error) => {
                        self.report_error(format!("Could not admit alignment scene: {error}"));
                    }
                }
            }
            let diagnostic_lanes = self.diagnostics.recording().then(|| {
                lane_captures
                    .iter()
                    .map(|capture| {
                        let preparation = capture.preparation_camera.viewport();
                        serde_json::json!({
                            "track_id": capture.track_id.get(),
                            "preparation_viewport": {
                                "start": preparation.interval().start(),
                                "end": preparation.interval().end(),
                                "exact_start": preparation.exact_start(),
                                "span": preparation.span(),
                                "pixel_width": preparation.pixel_width(),
                            },
                            "visible_rows": {
                                "start": capture.preparation_camera.visible_rows().start,
                                "end": capture.preparation_camera.visible_rows().end,
                            },
                            "read_camera": capture.read_camera.map(|camera| {
                                let viewport = camera.viewport();
                                serde_json::json!({
                                    "start": viewport.interval().start(),
                                    "end": viewport.interval().end(),
                                    "exact_start": viewport.exact_start(),
                                    "span": viewport.span(),
                                    "pixel_width": viewport.pixel_width(),
                                    "clip": format!("{:?}", camera.clip()),
                                })
                            }),
                            "derived_lane_count": capture.derived_lanes.len(),
                        })
                    })
                    .collect::<Vec<_>>()
            });
            match store.capture(panel_id, camera_revision, panel_viewport, lane_captures) {
                Ok(frame) => {
                    // A worker candidate can be temporarily absent while the immutable scene
                    // already committed for this lane still overlaps the live camera. Preserve
                    // that truthful overlap and recapture it with the live camera; background
                    // completion remains pending and may replace it later. Without this bridge,
                    // an ordinary source checkpoint becomes an empty GPU epoch and blanks the
                    // complete lane despite reusable scene authority still being resident.
                    for lane in frame.lanes.iter() {
                        if admitted_panel_sources
                            .iter()
                            .any(|source| source.track_id == lane.track_id)
                            || !retained_alignment_scene_can_present(
                                lane.scene_contracts_current,
                                lane.snapshot.is_some(),
                                lane.covered_interval,
                                lane.preparation_camera.interval(),
                            )
                        {
                            continue;
                        }
                        let desired_request_token = self
                            .alignment_tracks
                            .iter()
                            .find(|track| track.config.id == lane.track_id)
                            .and_then(|track| track.projection(panel_id))
                            .and_then(|projection| projection.navigation.request_token());
                        let Some(desired_request_token) = desired_request_token else {
                            continue;
                        };
                        let prior_source = self
                            .alignment_presented_epoch
                            .as_ref()
                            .and_then(|epoch| {
                                let prior_lane =
                                    epoch.panel_frames.get(&panel_id)?.lane(lane.track_id)?;
                                let candidate_revision = lane.snapshot.as_ref()?.revision();
                                (prior_lane.snapshot.as_ref()?.revision() == candidate_revision)
                                    .then_some(epoch)
                            })
                            .and_then(|epoch| epoch.gpu_bundle.as_ref())
                            .and_then(|bundle| {
                                bundle.lane_authorities.iter().find(|authority| {
                                    authority.panel_id == panel_id
                                        && authority.track_id == lane.track_id
                                })
                            })
                            .copied();
                        let Some(prior_source) = prior_source else {
                            continue;
                        };
                        admitted_panel_sources.push(CapturedAlignmentLaneSource {
                            panel_id,
                            track_id: lane.track_id,
                            semantic_request_token: prior_source.source_request_token,
                            desired_request_token,
                            camera: lane.preparation_camera,
                            individual_reads: AlignmentIndividualReadCapability::Pending,
                            semantic_target_complete: false,
                            reference_ready: false,
                            vertical_residency: prior_source.vertical_residency,
                            camera_sequence: lane.source_camera_sequence,
                            snapshot: lane.snapshot.clone(),
                            scene_contracts_current: true,
                        });
                    }
                    // A held-back progress frame whose lane has no committed incumbent to
                    // bridge still names the lane's pending source authority, exactly as before.
                    for source in panel_deferred_sources {
                        if !admitted_panel_sources
                            .iter()
                            .any(|admitted| admitted.track_id == source.track_id)
                        {
                            admitted_panel_sources.push(source);
                        }
                    }
                    captured.insert(panel_id, frame);
                    lane_sources.extend(admitted_panel_sources);
                }
                Err(error) => {
                    self.report_error(format!("Could not capture alignment scene: {error}"));
                    critical_capture_failure.get_or_insert_with(|| {
                        (
                            self.data_status.clone(),
                            object_fields(serde_json::json!({
                                "error": format!("{error:?}"),
                                "panel_id": panel_id.get(),
                                "panel_contig": contig_name,
                                "panel_viewport": {
                                    "start": panel_viewport.interval().start(),
                                    "end": panel_viewport.interval().end(),
                                    "exact_start": panel_viewport.exact_start(),
                                    "span": panel_viewport.span(),
                                    "pixel_width": panel_viewport.pixel_width(),
                                },
                                "camera_revision": camera_revision,
                                "workspace_layout_revision": self.workspace_layout.revision,
                                "surface": [self.surface_config.width, self.surface_config.height],
                                "lanes": diagnostic_lanes.unwrap_or_default(),
                            })),
                        )
                    });
                }
            }
        }
        if let Some((message, fields)) = critical_capture_failure {
            return Err(AlignmentPanelSceneEpochCaptureFailure { message, fields });
        }
        Ok(CapturedAlignmentPanelSceneEpoch {
            panel_frames: captured,
            lane_sources,
            panel_scene_stores,
        })
    }
}
