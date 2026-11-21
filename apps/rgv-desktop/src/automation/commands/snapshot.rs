use crate::automation::AutomationAuxiliaryOpenState;
use crate::automation::commands::evidence::{
    automation_locus, automation_scene_read_presentation_fields, automation_source_fingerprint,
    bounded_automation_text,
};
use crate::automation::commands::{
    automation_read_evidence_field_is_public, observed_retained_selection,
};
use crate::inspection::auxiliary::{
    copy_number_inspector_fields, feature_inspector_fields, gwas_inspector_fields,
    interaction_inspector_fields, mutation_inspector_fields, quantitative_inspector_fields,
};
use crate::inspection::variant::variant_inspector_fields;
use crate::ui::TrackFrameKey;
use crate::workspace::alignments::{
    AlignmentPanelProjection, AlignmentTrackLifecycle, alignment_effective_view_predicates,
};
use crate::workspace::context_lanes::ContextLaneKey;
use crate::{Viewer, ui};
use rgv_alignment_scene::AlignmentComponent;
use rgv_automation::{
    AUTOMATION_SCHEMA_VERSION, AutomationAlignmentSceneDiagnostics,
    AutomationAlignmentTrackSnapshot, AutomationContextLaneSnapshot,
    AutomationCytobandAvailability, AutomationEvidenceField, AutomationGenomeSnapshot,
    AutomationPaneItemLayoutSnapshot, AutomationPaneLayoutSnapshot, AutomationPanelSnapshot,
    AutomationPresentationCompositeSnapshot, AutomationPresentationGroupSnapshot,
    AutomationPresentationNodeSnapshot, AutomationReadPanelStateSnapshot, AutomationRectSnapshot,
    AutomationRenderLaneSnapshot, AutomationRenderOmissionReason, AutomationSelectionKind,
    AutomationSelectionSnapshot, AutomationTrackGestureKind, AutomationTrackGestureSnapshot,
    AutomationTrackLifecycle, AutomationTrackSnapshot, AutomationWorkspaceLayoutSnapshot,
    AutomationWorkspacePanelLayoutSnapshot, AutomationWorkspaceSnapshot,
    MAXIMUM_AUTOMATION_EVIDENCE_FIELDS, MAXIMUM_AUTOMATION_LOCI,
    MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS,
};
use rgv_core::{DetailLevel, GenomicInterval, PresentationNodeId, TrackId, TrackKind};
use rgv_engine::{AlignmentIndividualReadCapability, AlignmentSceneCoordinatorTelemetry};
use rgv_render::GpuBudgetPressure;
use std::time::Instant;

/// Longest an automation selection snapshot waits for a tile-pipeline record lookup.
const AUTOMATION_TILE_RECORD_LOOKUP_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(500);

impl Viewer {
    pub(super) fn automation_alignment_scene_diagnostics(
        &self,
    ) -> AutomationAlignmentSceneDiagnostics {
        let telemetry = self
            .session
            .alignment_tracks
            .iter()
            .flat_map(|track| track.projections.values())
            .map(|projection| projection.scene_telemetry)
            .fold(
                AlignmentSceneCoordinatorTelemetry::default(),
                |mut total, item| {
                    total.camera_updates = total.camera_updates.saturating_add(item.camera_updates);
                    total.camera_only_updates = total
                        .camera_only_updates
                        .saturating_add(item.camera_only_updates);
                    total.source_acquisitions = total
                        .source_acquisitions
                        .saturating_add(item.source_acquisitions);
                    total.row_atlas_candidates = total
                        .row_atlas_candidates
                        .saturating_add(item.row_atlas_candidates);
                    total.presentation_candidates = total
                        .presentation_candidates
                        .saturating_add(item.presentation_candidates);
                    total.scene_publications = total
                        .scene_publications
                        .saturating_add(item.scene_publications);
                    total.optional_work_pauses = total
                        .optional_work_pauses
                        .saturating_add(item.optional_work_pauses);
                    total.optional_work_resumes = total
                        .optional_work_resumes
                        .saturating_add(item.optional_work_resumes);
                    total.row_activity_queries = total
                        .row_activity_queries
                        .saturating_add(item.row_activity_queries);
                    total.row_activity_rebuilds = total
                        .row_activity_rebuilds
                        .saturating_add(item.row_activity_rebuilds);
                    total.row_activity_intervals_examined = total
                        .row_activity_intervals_examined
                        .saturating_add(item.row_activity_intervals_examined);
                    total.row_activity_intervals_changed = total
                        .row_activity_intervals_changed
                        .saturating_add(item.row_activity_intervals_changed);
                    total
                },
            );
        let ledger = self.session.alignment_scene_ledger.progress().ok();
        let context = self.context_scene_gpu_renderer.last_prepare_stats();
        let generic = self.rectangle_renderer.last_prepare_stats();
        let context_operations = self.context_scene_gpu_renderer.operation_counters();
        let generic_operations = self.rectangle_renderer.operation_counters();
        let context_bytes = self.context_scene_gpu_renderer.resident_bytes();
        let generic_bytes = self.rectangle_renderer.resident_bytes();
        let read = self.alignment_gpu_last_prepare_stats;
        let summary = self.alignment_summary_gpu_last_prepare_stats;
        let read_operations = self.alignment_gpu_renderer.operation_counters();
        let summary_operations = self.alignment_summary_gpu_renderer.operation_counters();
        let (contextual_text_anchors_examined, contextual_text_anchors_available) = self
            .session
            .panels
            .iter()
            .fold((0_usize, 0_usize), |(examined, available), panel| {
                (
                    examined.saturating_add(panel.context_scene.camera_text_anchors_examined),
                    available.saturating_add(panel.context_scene.camera_text_anchors_available),
                )
            });
        let alignment_occupied = ledger.map_or(0, |progress| {
            progress.charged.saturating_add(progress.reserved)
        });
        let pressure = self.alignment_last_gpu_budget_pressure;
        AutomationAlignmentSceneDiagnostics {
            aggregate_cache_budget_bytes: self.aggregate_cache_budget_bytes(),
            aggregate_cache_occupied_bytes: alignment_occupied
                .saturating_add(context_bytes)
                .saturating_add(generic_bytes),
            contextual_gpu_reserved_bytes: context_bytes,
            generic_gpu_reserved_bytes: generic_bytes,
            publication_waiting_for_capacity: self.alignment_gpu_publication_retry.is_some(),
            cross_owner_reclaim_active: self.alignment_capacity_reclaim.is_some(),
            last_capacity_pressure_cause: pressure
                .map_or_else(String::new, |pressure| format!("{:?}", pressure.cause)),
            last_capacity_pressure_requested_bytes: pressure
                .map_or(0, |pressure| pressure.requested_bytes),
            last_capacity_pressure_effective_occupied_bytes: pressure.map_or(0, |pressure| {
                pressure
                    .charged_bytes_at_failure
                    .saturating_add(pressure.reserved_bytes_at_failure)
            }),
            last_capacity_pressure_retirement_credit_bytes: pressure
                .map_or(0, |pressure| pressure.retirement_credit_bytes),
            last_capacity_pressure_limit_bytes: pressure.map_or(0, |pressure| pressure.limit_bytes),
            last_capacity_pressure_required_reclaim_bytes: pressure
                .map_or(0, GpuBudgetPressure::required_reclaim_bytes),
            optional_residency_phase: format!("{:?}", self.alignment_gpu_speculation_gate.phase),
            optional_residency_retention_limited: self
                .alignment_gpu_prefetch
                .read_retention_limited
                || self.alignment_gpu_prefetch.summary_retention_limited,
            camera_updates: telemetry.camera_updates,
            camera_only_updates: telemetry.camera_only_updates,
            source_acquisitions: telemetry.source_acquisitions,
            row_atlas_candidates: telemetry.row_atlas_candidates,
            presentation_candidates: telemetry.presentation_candidates,
            scene_publications: telemetry.scene_publications,
            optional_work_pauses: telemetry.optional_work_pauses,
            optional_work_resumes: telemetry.optional_work_resumes,
            row_activity_queries: telemetry.row_activity_queries,
            row_activity_rebuilds: telemetry.row_activity_rebuilds,
            row_activity_intervals_examined: telemetry.row_activity_intervals_examined,
            row_activity_intervals_changed: telemetry.row_activity_intervals_changed,
            read_content_manifest_builds: read_operations.content_manifest_builds,
            read_content_manifest_pages_enumerated: read_operations
                .content_manifest_pages_enumerated,
            read_prefetch_keys_inspected: read_operations.prefetch_authority_keys_inspected,
            read_reclaim_keys_inspected: read_operations.reclaim_authority_keys_inspected,
            summary_content_manifest_builds: summary_operations.content_manifest_builds,
            summary_content_manifest_pages_enumerated: summary_operations
                .content_manifest_pages_enumerated,
            summary_prefetch_keys_inspected: summary_operations.prefetch_authority_keys_inspected,
            summary_reclaim_keys_inspected: summary_operations.reclaim_authority_keys_inspected,
            read_frame_mandatory_pages: read.mandatory_pages,
            read_frame_body_instances: read.base_instances,
            read_frame_overlay_instances: read.overlay_instances,
            read_frame_labels: read.text_labels,
            read_frame_label_anchors_examined: read.label_anchors_examined,
            read_frame_page_pin_acquisitions: read.page_pin_acquisitions,
            read_frame_camera_bytes: read.camera_uniform_bytes,
            read_frame_immutable_upload_bytes: read.geometry_uploaded_bytes,
            read_frame_cache_hits: read.page_cache_hits,
            read_frame_cache_misses: read.page_cache_misses,
            summary_frame_mandatory_pages: summary.mandatory_pages,
            summary_frame_depth_instances: summary.depth_instances,
            summary_frame_allele_instances: summary.allele_instances,
            summary_frame_junction_instances: summary.junction_instances,
            summary_frame_modification_instances: summary.modification_instances,
            summary_frame_range_queries: summary.range_maximum_queries,
            summary_frame_junction_entries_examined: summary.junction_entries_examined,
            summary_frame_page_pin_acquisitions: summary.page_pin_acquisitions,
            summary_frame_camera_bytes: summary.camera_uniform_bytes,
            summary_frame_immutable_upload_bytes: summary.geometry_uploaded_bytes,
            summary_frame_cache_hits: summary.page_cache_hits,
            summary_frame_cache_misses: summary.page_cache_misses,
            contextual_frame_camera_writes: context.camera_writes,
            contextual_frame_immutable_upload_bytes: context.uploaded_bytes,
            contextual_text_anchors_examined,
            contextual_text_anchors_available,
            contextual_content_preparations: context_operations.content_preparations,
            contextual_camera_capture_attempts: context_operations.camera_capture_attempts,
            contextual_camera_captures: context_operations.camera_captures,
            contextual_content_replacement_requests: context_operations
                .content_replacement_requests,
            contextual_invalid_camera_captures: context_operations.invalid_camera_captures,
            contextual_total_immutable_upload_bytes: context_operations.immutable_upload_bytes,
            contextual_total_camera_writes: context_operations.camera_writes,
            generic_frame_uploaded_bytes: generic.uploaded_bytes,
            generic_frame_slab_cache_hit: generic.slab_cache_hit,
            generic_preparations: generic_operations.preparations,
            generic_instances_examined: generic_operations.instances_examined,
            generic_slab_cache_hits: generic_operations.slab_cache_hits,
            generic_total_uploaded_bytes: generic_operations.uploaded_bytes,
            generic_buffer_reallocations: generic_operations.buffer_reallocations,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn automation_workspace_snapshot(&self) -> AutomationWorkspaceSnapshot {
        let shared_annotation_scroll = self.session.panels.annotation_scroll_points;
        let maximum_shared_annotation_scroll =
            crate::workspace::panels::maximum_annotation_scroll_points(&self.session.panels);
        let mut panels: Vec<_> = self
            .session
            .panels
            .iter()
            .map(|panel| {
                let interval = panel.viewport.interval();
                let maximum_scroll = (panel.content_bottom_points
                    - self.canvas_geometry().viewport.bottom())
                .max(0.0);
                let maximum_annotation_scroll = maximum_shared_annotation_scroll;
                let filters = self.primary_alignment_filters();
                let primary_track = self.session.alignment_tracks.first();
                let alignment_projection =
                    primary_track.and_then(|track| track.projection(panel.id));
                let presented_scene = primary_track
                    .filter(|track| track.scene_engine.is_some())
                    .and_then(|track| self.presented_alignment_lane(panel.id, track.config.id));
                let scene_snapshot = presented_scene
                    .filter(|presented| {
                        presented.lane.summary_current()
                            && presented.source.semantic_target_complete
                    })
                    .and_then(|presented| presented.lane.snapshot.as_ref());
                let scene_records =
                    scene_snapshot.map_or(0, |snapshot| snapshot.reads().evidence().record_count());
                let scene_rows =
                    scene_snapshot.map_or(0, |snapshot| snapshot.reads().atlas().row_count());
                let scene_provenance = scene_snapshot.and_then(|snapshot| {
                    snapshot
                        .summaries()
                        .filter_provenance(snapshot.prepared_interval())
                        .ok()
                });
                let scene_reads_complete = presented_scene.is_some_and(|presented| {
                    matches!(
                        presented.source.individual_reads,
                        AlignmentIndividualReadCapability::Complete { scene_revision }
                            if presented.lane.scene_revision() == Some(scene_revision)
                    ) && presented.read_authority.is_some()
                });
                let scene_reads_refused = presented_scene.is_some_and(|presented| {
                    matches!(
                        presented.source.individual_reads,
                        AlignmentIndividualReadCapability::Refused { .. }
                    )
                });
                let index_only_overview =
                    alignment_projection.is_some_and(|projection| projection.index_only_overview);
                let read_content_visible = self
                    .session
                    .track_presentation
                    .effective_track_visibility(
                        ui::TrackPane::Reads,
                        self.primary_alignment_track_id(),
                    )
                    .unwrap_or(false);
                let reads_rendered = read_content_visible
                    && panel.viewport.detail_level() != DetailLevel::Overview
                    && scene_snapshot.is_some()
                    && scene_reads_complete;
                let data_available = scene_snapshot.is_some();
                let scene_modifications_available = scene_snapshot
                    .is_some_and(|snapshot| snapshot.summaries().contract().collect_modifications);
                let scene_maximum_depth = scene_snapshot
                    .and_then(|snapshot| snapshot.summaries().maximum_depth(interval).ok())
                    .unwrap_or_default();
                let render_lanes = [
                    ("reference", filters.show_reference),
                    ("coverage", filters.show_coverage),
                    ("reads", read_content_visible),
                    ("modifications", filters.show_modified_bases),
                ]
                .into_iter()
                .map(|(lane, configured)| {
                    let rendered = configured
                        && data_available
                        && match lane {
                            "reads" => reads_rendered,
                            "modifications" => scene_modifications_available,
                            _ => true,
                        };
                    let omission_reason = (!rendered).then_some(if !configured {
                        AutomationRenderOmissionReason::Disabled
                    } else if lane == "reads" && scene_reads_refused {
                        AutomationRenderOmissionReason::TooDense
                    } else if index_only_overview {
                        AutomationRenderOmissionReason::OverviewDetailThreshold
                    } else if !data_available {
                        if panel.pending_queries.alignment
                            || alignment_projection
                                .is_some_and(AlignmentPanelProjection::scene_is_checking)
                        {
                            AutomationRenderOmissionReason::Loading
                        } else {
                            AutomationRenderOmissionReason::NoData
                        }
                    } else {
                        AutomationRenderOmissionReason::OverviewDetailThreshold
                    });
                    AutomationRenderLaneSnapshot {
                        lane: lane.to_owned(),
                        configured,
                        rendered,
                        omission_reason,
                        queried_items: scene_records,
                        rendered_items: match lane {
                            "reads" | "modifications" if rendered => scene_records,
                            "coverage" => scene_records,
                            _ => 0,
                        },
                        sampled_items: 0,
                        truncated: false,
                        filtered_unmapped_items: scene_provenance.map_or(0, |provenance| {
                            usize::try_from(provenance.unmapped).unwrap_or(usize::MAX)
                        }),
                        filtered_secondary_items: scene_provenance.map_or(0, |provenance| {
                            usize::try_from(provenance.secondary).unwrap_or(usize::MAX)
                        }),
                        filtered_supplementary_items: scene_provenance.map_or(0, |provenance| {
                            usize::try_from(provenance.supplementary).unwrap_or(usize::MAX)
                        }),
                        filtered_user_items: scene_provenance.map_or(0, |provenance| {
                            usize::try_from(provenance.user_filter).unwrap_or(usize::MAX)
                        }),
                        filtered_duplicate_items: 0,
                        filtered_qc_failed_items: 0,
                        regional_insertions_rendered: 0,
                        regional_insertions_collapsed: 0,
                        regional_insertions_omitted: 0,
                        raw_maximum_coverage: scene_maximum_depth,
                        coverage_exact_complete: scene_snapshot.is_some(),
                        depth_segments_omitted: 0,
                        packed_row_count: scene_rows,
                        visible_occupied_rows: presented_scene.map_or(0, |presented| {
                            presented
                                .lane
                                .requested_rows
                                .end
                                .saturating_sub(presented.lane.requested_rows.start)
                                as usize
                        }),
                        visible_row_span: presented_scene.map_or(0, |presented| {
                            presented
                                .lane
                                .requested_rows
                                .end
                                .saturating_sub(presented.lane.requested_rows.start)
                                as usize
                        }),
                        natural_stack_height: scene_rows,
                    }
                })
                .collect();
                AutomationPanelSnapshot {
                    id: panel.id,
                    locus: automation_locus(&panel.contig_name, interval),
                    contig: panel.contig_name.clone(),
                    start: interval.start(),
                    end: interval.end(),
                    detail_level: panel.viewport.detail_level(),
                    pixel_width: panel.viewport.pixel_width(),
                    bases_per_pixel: panel.viewport.bases_per_pixel(),
                    vertical_scroll_points: panel.vertical_scroll_points,
                    maximum_vertical_scroll_points: maximum_scroll,
                    annotation_scroll_points: shared_annotation_scroll,
                    maximum_annotation_scroll_points: maximum_annotation_scroll,
                    read_stack_scroll_points: panel.read_stack_scroll_points,
                    maximum_read_stack_scroll_points: (panel.pane_layout.reads.content_bottom
                        - panel.pane_layout.reads.pane_rect.bottom())
                    .max(0.0),
                    render_generation: panel.generation_clock.current().get(),
                    settled: !panel.pending_queries.alignment
                        && !panel.pending_queries.reference
                        && !panel.pending_queries.tracks
                        && !panel.navigation_presentation.is_unsettled()
                        && panel.navigation_query_schedule.pending.is_none()
                        && self.session.alignment_tracks.iter().all(|track| {
                            match track.lifecycle {
                                AlignmentTrackLifecycle::Opening => false,
                                AlignmentTrackLifecycle::Failed(_) => true,
                                AlignmentTrackLifecycle::Ready => track
                                    .projection(panel.id)
                                    .is_some_and(AlignmentPanelProjection::scene_is_current),
                            }
                        }),
                    navigation_transaction_active: panel.navigation_presentation.is_unsettled(),
                    navigation_target_locus: panel.navigation_presentation.is_unsettled().then(
                        || {
                            automation_locus(
                                &panel.contig_name,
                                panel.navigation_presentation.target_interval,
                            )
                        },
                    ),
                    navigation_target_generation: panel
                        .navigation_presentation
                        .is_unsettled()
                        .then_some(panel.navigation_presentation.target_generation),
                    navigation_age_ms: panel.navigation_presentation.is_unsettled().then(|| {
                        u64::try_from(
                            panel
                                .navigation_presentation
                                .age(Instant::now())
                                .as_millis(),
                        )
                        .unwrap_or(u64::MAX)
                    }),
                    navigation_invalidation_reason: panel
                        .navigation_presentation
                        .invalidation_reason
                        .clone(),
                    render_lanes,
                    context_lanes: panel
                        .context_scene
                        .lanes
                        .iter()
                        .map(|(key, state)| {
                            let (lane, track_id) = match key {
                                ContextLaneKey::Reference => ("reference".to_owned(), None),
                                ContextLaneKey::Auxiliary(track_id) => {
                                    ("auxiliary".to_owned(), Some(*track_id))
                                }
                            };
                            AutomationContextLaneSnapshot {
                                lane,
                                track_id,
                                source_readiness: state.readiness.label().to_owned(),
                                gpu_readiness: state.gpu_readiness.label().to_owned(),
                                source_locus: state.lease.as_ref().map(|lease| {
                                    automation_locus(
                                        &panel.contig_name,
                                        lease.source_viewport.interval(),
                                    )
                                }),
                                content_revision: state.lease.as_ref().map(|lease| lease.revision),
                                generation: state.lease.as_ref().map(|lease| lease.generation),
                                failure: state.gpu_failure.clone(),
                            }
                        })
                        .collect(),
                    read_panel_states: self
                        .session
                        .alignment_tracks
                        .iter()
                        .map(|track| {
                            let track_id = track.config.id;
                            let state = panel.read_panel_scroll_state(track_id);
                            let clip_bottom = panel
                                .scroll_display_cache
                                .as_ref()
                                .and_then(|cache| {
                                    cache
                                        .read_panels
                                        .iter()
                                        .find(|read_panel| read_panel.track_id == track_id)
                                })
                                .map_or(panel.pane_layout.reads.pane_rect.bottom(), |read_panel| {
                                    read_panel.clip_bottom
                                });
                            AutomationReadPanelStateSnapshot {
                                track_id,
                                scroll_points: state.scroll_points,
                                maximum_scroll_points: (state.content_bottom_points - clip_bottom)
                                    .max(0.0),
                                content_origin_points: state.content_origin_points,
                                latent_scroll_fraction: state.latent_scroll_fraction,
                                content_bottom_points: state.content_bottom_points,
                                density: state.density,
                                effective_density: state
                                    .density
                                    .unwrap_or(track.config.filters.alignment_density),
                            }
                        })
                        .collect(),
                    selected_presentation_nodes: panel.selected_presentation_nodes.clone(),
                    presentation_selection_anchor: panel.presentation_selection_anchor,
                    coverage_sources: self
                        .session
                        .alignment_tracks
                        .iter()
                        .map(|track| rgv_automation::AutomationCoverageSourceSnapshot {
                            track_id: track.config.id,
                            source: self
                                .alignment_coverage_mode(panel.id, track.config.id)
                                .automation_name()
                                .to_owned(),
                        })
                        .collect(),
                }
            })
            .collect();
        let active_scene_presentation_committed = self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive();
        let mut tracks = self
            .session
            .alignment_tracks
            .iter()
            .map(|track| {
                let projection = track.projection(self.session.panels.active().id);
                let presented_scene = track.scene_engine.as_ref().and_then(|_| {
                    self.presented_alignment_lane(self.session.panels.active().id, track.config.id)
                });
                let (lifecycle, failure) = match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => (AutomationTrackLifecycle::Opening, None),
                    AlignmentTrackLifecycle::Ready => (AutomationTrackLifecycle::Ready, None),
                    AlignmentTrackLifecycle::Failed(_) => (
                        AutomationTrackLifecycle::Failed,
                        Some("alignment track failed to open".to_owned()),
                    ),
                };
                let read_presentation = automation_scene_read_presentation_fields(
                    presented_scene,
                    lifecycle == AutomationTrackLifecycle::Opening
                        || !active_scene_presentation_committed
                        || (track.scene_engine.is_some() && presented_scene.is_none()),
                    lifecycle == AutomationTrackLifecycle::Failed || track.scene_engine.is_none(),
                    track.config.maximum_alignments,
                    track.scene_complete_read_limit,
                );
                let annotation = self
                    .session
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Annotation, track.config.id);
                let annotation_visible = annotation.is_none_or(|node| node.content_visible);
                let read_visible = self
                    .session
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Reads, track.config.id)
                    .is_none_or(|node| node.content_visible);
                AutomationTrackSnapshot {
                    id: track.config.id,
                    name: track.config.name.clone(),
                    kind: TrackKind::Alignment,
                    source_fingerprint: automation_source_fingerprint(&track.config.source),
                    visible: annotation_visible,
                    collapsed: !annotation_visible,
                    height: annotation
                        .and_then(|node| node.preferred_height)
                        .unwrap_or(track.config.height),
                    source_available: lifecycle != AutomationTrackLifecycle::Failed,
                    lifecycle,
                    failure,
                    alignment: Some(AutomationAlignmentTrackSnapshot {
                        group_mode: track.config.filters.alignment_group_mode,
                        group_order: track.config.filters.alignment_group_order,
                        sort_mode: track.config.filters.alignment_sort_mode,
                        sort_order: track.config.filters.alignment_sort_order,
                        sort_anchor: track.config.filters.alignment_sort_anchor,
                        filter_expression: track.filter_editor.expression.clone(),
                        view_filter: track.config.filters.alignment_view_filter.clone(),
                        highlight_filter: track.config.filters.alignment_highlight_filter.clone(),
                        view_filter_fingerprint: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| {
                                snapshot
                                    .reads()
                                    .evidence()
                                    .view_filter()
                                    .fingerprint()
                                    .get()
                            }),
                        view_filter_eligible_records: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| {
                                snapshot.reads().evidence().view_filter().eligible_records()
                            }),
                        view_filter_matching_records: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| {
                                snapshot.reads().evidence().view_filter().matching_records()
                            }),
                        view_filter_predicate_matches: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or_else(Vec::new, |snapshot| {
                                snapshot
                                    .reads()
                                    .evidence()
                                    .view_filter()
                                    .predicate_matches()
                                    .to_vec()
                            }),
                        view_filter_effective_predicates: alignment_effective_view_predicates(
                            &track.config.filters,
                            &track.active_filters,
                        ),
                        view_as_pairs: track.config.filters.view_as_pairs,
                        read_stacking_mode: track.config.filters.read_stacking_mode,
                        read_presentation_state: read_presentation.state,
                        requested_complete_read_limit: read_presentation.requested_limit,
                        effective_complete_read_limit: read_presentation.effective_limit,
                        eligible_read_records: read_presentation.eligible_records,
                        complete_read_records: read_presentation.complete_records,
                        refusal_eligible_read_records: read_presentation.refusal_eligible_records,
                        read_refusal_scope: read_presentation.refusal_scope,
                        scene_phase: projection.map_or_else(
                            || "unsupported".to_owned(),
                            |projection| {
                                format!("{:?}", projection.navigation.phase()).to_ascii_lowercase()
                            },
                        ),
                        scene_revision: presented_scene
                            .and_then(|presented| presented.lane.scene_revision())
                            .map(|revision| format!("{:?}", revision.fingerprint())),
                        scene_target_revision: presented_scene
                            .map_or(0, |presented| presented.target_revision),
                        scene_prepared_start: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map(|snapshot| snapshot.prepared_interval().start()),
                        scene_prepared_end: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map(|snapshot| snapshot.prepared_interval().end()),
                        scene_record_count: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| snapshot.reads().evidence().record_count()),
                        scene_row_count: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| snapshot.reads().atlas().row_count()),
                        scene_read_page_count: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| snapshot.reads().pages().len()),
                        scene_summary_page_count: presented_scene
                            .and_then(|presented| presented.lane.snapshot.as_ref())
                            .map_or(0, |snapshot| snapshot.summary_pages().len()),
                        scene_target_complete: presented_scene
                            .is_some_and(|presented| presented.source.semantic_target_complete),
                        scene_reads_complete: presented_scene
                            .is_some_and(|presented| presented.lane.reads_complete()),
                        scene_reference_ready: presented_scene
                            .is_some_and(|presented| presented.source.reference_ready),
                        scene_vertical_residency: presented_scene.map_or_else(
                            || "unavailable".to_owned(),
                            |presented| {
                                format!("{:?}", presented.source.vertical_residency)
                                    .to_ascii_lowercase()
                            },
                        ),
                        scene_camera_updates: projection
                            .map_or(0, |projection| projection.scene_telemetry.camera_updates),
                        scene_camera_only_updates: projection.map_or(0, |projection| {
                            projection.scene_telemetry.camera_only_updates
                        }),
                        scene_source_acquisitions: projection.map_or(0, |projection| {
                            projection.scene_telemetry.source_acquisitions
                        }),
                        scene_source_retargets: projection
                            .map_or(0, |projection| projection.scene_telemetry.source_retargets),
                        scene_source_refusals: projection
                            .map_or(0, |projection| projection.scene_telemetry.source_refusals),
                        scene_publications: projection.map_or(0, |projection| {
                            projection.scene_telemetry.scene_publications
                        }),
                        scene_foreground_submissions: projection.map_or(0, |projection| {
                            projection.scene_telemetry.foreground_submissions
                        }),
                        scene_speculation_submissions: projection.map_or(0, |projection| {
                            projection.scene_telemetry.speculation_submissions
                        }),
                        overlapping_mate_policy: track.config.filters.overlapping_mate_policy,
                        color_mode: track.config.filters.alignment_color_mode,
                        alignment_density: track.config.filters.alignment_density,
                        read_base_display_mode: track.config.filters.read_base_display_mode,
                        shade_read_bases_by_quality: track
                            .config
                            .filters
                            .shade_read_bases_by_quality,
                        show_modified_bases: track.config.filters.show_modified_bases,
                        modified_base_color_mode: track.config.filters.modified_base_color_mode,
                        show_reference: track.config.filters.show_reference,
                        show_coverage: track.config.filters.show_coverage,
                        coverage_scale_transform: track.config.filters.coverage_scale_transform,
                        show_reads: read_visible,
                        include_secondary: track.config.filters.include_secondary,
                        include_supplementary: track.config.filters.include_supplementary,
                    }),
                }
            })
            .collect::<Vec<_>>();
        tracks.extend(self.session.tracks.tracks().iter().map(|track| {
            let (lifecycle, failure) = match self.automation_auxiliary_open_state.get(&track.id) {
                Some(AutomationAuxiliaryOpenState::Pending) => {
                    (AutomationTrackLifecycle::Opening, None)
                }
                Some(AutomationAuxiliaryOpenState::Failed(_)) => (
                    AutomationTrackLifecycle::Failed,
                    Some("auxiliary track failed to open".to_owned()),
                ),
                Some(AutomationAuxiliaryOpenState::Ready) | None => {
                    (AutomationTrackLifecycle::Ready, None)
                }
            };
            let annotation = self
                .session
                .track_presentation
                .pane_track_state(ui::TrackPane::Annotation, track.id);
            let annotation_visible = annotation.is_none_or(|node| node.content_visible);
            AutomationTrackSnapshot {
                id: track.id,
                name: track.name.clone(),
                kind: track.kind,
                source_fingerprint: automation_source_fingerprint(&track.source),
                visible: annotation_visible,
                collapsed: !annotation_visible,
                height: annotation
                    .and_then(|node| node.preferred_height)
                    .unwrap_or(track.height),
                source_available: lifecycle != AutomationTrackLifecycle::Failed,
                lifecycle,
                failure,
                alignment: None,
            }
        }));
        let source_available = |track_id: TrackId| {
            self.session
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)
                .is_some_and(|track| !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_)))
                || self.session.tracks.tracks().iter().any(|track| {
                    track.id == track_id
                        && !matches!(
                            self.automation_auxiliary_open_state.get(&track_id),
                            Some(AutomationAuxiliaryOpenState::Failed(_))
                        )
                })
        };
        let parent_composite = |track_id: TrackId| {
            self.session
                .tracks
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .and_then(|track| track.quantitative.overlay_group)
                .filter(|id| {
                    self.session
                        .track_presentation
                        .composites
                        .iter()
                        .any(|composite| {
                            composite.channel == ui::TrackPane::Annotation && composite.id == *id
                        })
                })
        };
        let composite_visible = |track_id: TrackId| {
            parent_composite(track_id).is_none_or(|id| {
                self.session
                    .track_presentation
                    .composite_content_visible(ui::TrackPane::Annotation, id)
            })
        };
        let mut presentation_nodes = Vec::new();
        for channel in [ui::TrackPane::Annotation, ui::TrackPane::Reads] {
            let mut order_index = 0_usize;
            if channel == ui::TrackPane::Annotation && self.session.reference_engine.is_some() {
                presentation_nodes.push(AutomationPresentationNodeSnapshot {
                    id: PresentationNodeId::Reference,
                    order_index,
                    desired_content_visible: self
                        .session
                        .track_presentation
                        .reference_content_visible,
                    effective_content_visible: self
                        .session
                        .track_presentation
                        .reference_content_visible,
                    preferred_height: self.session.track_presentation.reference_preferred_height,
                    parent_group_id: None,
                    parent_composite_id: None,
                });
                order_index += 1;
            }
            for root in self.session.track_presentation.pane_root_nodes(channel) {
                match root {
                    PresentationNodeId::Group { group_id, .. } => {
                        if let Some(group) = self
                            .session
                            .track_presentation
                            .groups
                            .iter()
                            .find(|group| group.channel == channel && group.id == group_id)
                        {
                            presentation_nodes.push(AutomationPresentationNodeSnapshot {
                                id: root,
                                order_index,
                                desired_content_visible: group.content_visible,
                                effective_content_visible: group.content_visible,
                                preferred_height: None,
                                parent_group_id: None,
                                parent_composite_id: None,
                            });
                            order_index += 1;
                            for track_id in &group.members {
                                let node = self
                                    .session
                                    .track_presentation
                                    .pane_track_state(channel, *track_id);
                                let desired = node.is_some_and(|node| node.content_visible);
                                let source_available = source_available(*track_id);
                                presentation_nodes.push(AutomationPresentationNodeSnapshot {
                                    id: PresentationNodeId::Track {
                                        channel,
                                        track_id: *track_id,
                                    },
                                    order_index,
                                    desired_content_visible: desired,
                                    effective_content_visible: desired
                                        && source_available
                                        && group.content_visible
                                        && group.expanded
                                        && composite_visible(*track_id),
                                    preferred_height: node.and_then(|node| node.preferred_height),
                                    parent_group_id: Some(group_id),
                                    parent_composite_id: parent_composite(*track_id),
                                });
                                order_index += 1;
                            }
                        }
                    }
                    PresentationNodeId::Track { track_id, .. } => {
                        let node = self
                            .session
                            .track_presentation
                            .pane_track_state(channel, track_id);
                        let desired = node.is_some_and(|node| node.content_visible);
                        let source_available = source_available(track_id);
                        presentation_nodes.push(AutomationPresentationNodeSnapshot {
                            id: root,
                            order_index,
                            desired_content_visible: desired,
                            effective_content_visible: desired
                                && source_available
                                && composite_visible(track_id),
                            preferred_height: node.and_then(|node| node.preferred_height),
                            parent_group_id: None,
                            parent_composite_id: parent_composite(track_id),
                        });
                        order_index += 1;
                    }
                    PresentationNodeId::Reference => {}
                }
            }
        }
        let to_rect = |rect: egui::Rect| AutomationRectSnapshot {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
        };
        let pane_layouts = self
            .session
            .panels
            .iter()
            .flat_map(|panel| {
                [&panel.pane_layout.annotation, &panel.pane_layout.reads]
                    .into_iter()
                    .map(|layout| AutomationPaneLayoutSnapshot {
                        panel_id: panel.id,
                        channel: layout.channel,
                        pane: to_rect(layout.pane_rect),
                        content_bottom: layout.content_bottom,
                        scroll_points: layout.scroll_points,
                        stack_scroll_points: layout.stack_scroll_points,
                        items: layout
                            .items
                            .iter()
                            .map(|item| AutomationPaneItemLayoutSnapshot {
                                id: item.id,
                                row: to_rect(item.row_rect),
                                header: to_rect(item.header_rect),
                                content: item.content_rect.map(to_rect),
                                clip: to_rect(item.clip_rect),
                                desired_content_visible: item.desired_content_visible,
                                effective_content_visible: item.effective_content_visible,
                                composite_id: match item.frame.key {
                                    TrackFrameKey::Auxiliary(track_id) => {
                                        parent_composite(track_id)
                                    }
                                    TrackFrameKey::Reference
                                    | TrackFrameKey::Alignment(_)
                                    | TrackFrameKey::Group { .. } => None,
                                },
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let workspace_layout = Some(AutomationWorkspaceLayoutSnapshot {
            revision: self.workspace_layout.revision,
            recomputations: self.workspace_layout_recomputations,
            settled_resize_query_refreshes: self.resize_query_refreshes,
            resolution_query_refresh_pending: self.pending_resolution_query_refresh.is_some(),
            workspace: to_rect(self.workspace_layout.workspace),
            properties: to_rect(self.workspace_layout.properties),
            properties_expanded: self.workspace_layout.properties_expanded,
            properties_resize_active: self.properties_resize_gesture_active,
            display_scale: self.workspace_layout.display_scale,
            accessibility_scale: self.workspace_layout.accessibility_scale,
            genome_context: to_rect(self.workspace_layout.genome_context),
            track_viewport: to_rect(self.workspace_layout.track_viewport),
            context_gutter: to_rect(self.workspace_layout.context_gutter),
            track_gutter: to_rect(self.workspace_layout.track_gutter),
            has_cytoband_row: self.workspace_layout.has_cytoband_row,
            panels: self
                .workspace_layout
                .panels
                .iter()
                .map(|panel| AutomationWorkspacePanelLayoutSnapshot {
                    panel_id: panel.panel_id,
                    cytobands: match panel.cytobands {
                        ui::CytobandAvailability::Loading => {
                            AutomationCytobandAvailability::Loading
                        }
                        ui::CytobandAvailability::Present => {
                            AutomationCytobandAvailability::Present
                        }
                        ui::CytobandAvailability::Absent => AutomationCytobandAvailability::Absent,
                    },
                    column: to_rect(panel.column),
                    header: to_rect(panel.header),
                    header_genomic: to_rect(panel.header_genomic),
                    header_scrollbar: to_rect(panel.header_scrollbar),
                    canvas: to_rect(panel.canvas),
                    scrollbar: to_rect(panel.scrollbar),
                    metadata_row: to_rect(panel.context_rows.metadata),
                    cytoband_row: panel.context_rows.cytoband.map(to_rect),
                    overview_row: to_rect(panel.context_rows.overview),
                    span_row: to_rect(panel.context_rows.span),
                    ruler_row: to_rect(panel.context_rows.ruler),
                })
                .collect(),
        });
        let active_track_gesture = self
            .resizing_annotation_track
            .and_then(|(key, _)| match key {
                TrackFrameKey::Reference => Some(PresentationNodeId::Reference),
                TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
                    Some(PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id,
                    })
                }
                TrackFrameKey::Group { channel, id } => Some(PresentationNodeId::Group {
                    channel,
                    group_id: id,
                }),
            })
            .map(|target| AutomationTrackGestureSnapshot {
                kind: AutomationTrackGestureKind::ResizeNode,
                panel_id: self.session.panels.active().id,
                target: Some(target),
            })
            .or_else(|| {
                self.resizing_read_track
                    .map(|(track_id, _)| AutomationTrackGestureSnapshot {
                        kind: AutomationTrackGestureKind::ResizeNode,
                        panel_id: self.session.panels.active().id,
                        target: Some(PresentationNodeId::Track {
                            channel: ui::TrackPane::Reads,
                            track_id,
                        }),
                    })
            })
            .or_else(|| {
                self.resizing_read_pane
                    .map(|_| AutomationTrackGestureSnapshot {
                        kind: AutomationTrackGestureKind::ResizeReadPane,
                        panel_id: self.session.panels.active().id,
                        target: None,
                    })
            })
            .or_else(|| {
                self.active_stack_scrollbar_gesture
                    .map(|(panel_id, channel)| AutomationTrackGestureSnapshot {
                        kind: match channel {
                            ui::TrackPane::Annotation => {
                                AutomationTrackGestureKind::AnnotationScrollbar
                            }
                            ui::TrackPane::Reads => AutomationTrackGestureKind::ReadStackScrollbar,
                        },
                        panel_id,
                        target: None,
                    })
            })
            .or_else(|| {
                self.vertical_overview_drag
                    .map(|drag| AutomationTrackGestureSnapshot {
                        kind: AutomationTrackGestureKind::ReadScrollbar,
                        panel_id: drag.panel_id,
                        target: Some(PresentationNodeId::Track {
                            channel: ui::TrackPane::Reads,
                            track_id: drag.track_id,
                        }),
                    })
            });
        let truncated = panels.len() > MAXIMUM_AUTOMATION_LOCI
            || tracks.len() > MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS;
        panels.truncate(MAXIMUM_AUTOMATION_LOCI);
        tracks.truncate(MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS);
        let scene_budget = self.session.alignment_scene_budget;
        let alignment_scene_diagnostics = self.automation_alignment_scene_diagnostics();
        AutomationWorkspaceSnapshot {
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version: AUTOMATION_SCHEMA_VERSION,
            application_instance_id: self.application_instance_id.clone(),
            workspace_revision: self.session.workspace_revision,
            genome: AutomationGenomeSnapshot {
                name: self.session.assembly_name.clone(),
                context_label: self.session.assembly.context_label(),
                assembly_identity: self.session.assembly_identity.clone(),
                suggested_assembly_identity: self.session.suggested_assembly_identity.clone(),
                reference_configured: self.session.reference_engine.is_some(),
                reference_ready: self.session.reference_engine.is_some()
                    && self.session.reference_dictionary.is_some(),
                reference_contig_count: self.session.assembly.reference_contig_count,
                navigation_contig_count: self.session.assembly.dictionary.len(),
                reference_is_subset: self.session.assembly_identity.as_ref().is_some_and(
                    |identity| {
                        identity.expected_primary_contigs > 0
                            && self.session.assembly.reference_contig_count
                                < identity.expected_primary_contigs
                    },
                ),
                cytobands_loaded: self.session.assembly.has_cytobands(),
                default_annotation_count: self.session.assembly.default_annotation_count,
                gene_model_track_count: self
                    .session
                    .tracks
                    .tracks()
                    .iter()
                    .filter(|track| track.kind == TrackKind::GeneModel)
                    .count(),
                contig_alias_count: self.session.contig_aliases.len(),
                landmarks: self.automation_landmark_snapshot(),
            },
            settled: self.automation_data_is_settled(),
            active_panel_id: self.session.panels.active().id,
            panels,
            tracks,
            read_browsing_buffer_percent: self.session.settings.read_browsing_buffer_percent,
            total_cache_budget_mib: self.session.settings.cache_budget_mib,
            alignment_scene_budget_bytes: scene_budget.budget_bytes,
            alignment_scene_charged_bytes: scene_budget.charged_bytes,
            alignment_scene_reserved_bytes: scene_budget.reserved_bytes,
            alignment_scene_transition_reserve_bytes: scene_budget.transition_reserve_bytes,
            alignment_scene_transition_reserved_bytes: scene_budget.reserved_transition_bytes,
            alignment_scene_transient_budget_bytes: scene_budget.transient_budget_bytes,
            alignment_scene_transient_bytes: scene_budget.transient_bytes,
            alignment_scene_transient_high_water_bytes: scene_budget.transient_high_water_bytes,
            alignment_scene_pinned_bytes: scene_budget.pinned_bytes,
            alignment_scene_pinned_overage_bytes: scene_budget.pinned_overage_bytes,
            alignment_scene_active_replacement_bundle: scene_budget.active_replacement_bundle,
            alignment_scene_budget_refreshes: scene_budget.refreshes,
            alignment_scene_diagnostics,
            read_pane_height: self.session.track_presentation.read_pane_height,
            annotation_section_expanded: self
                .session
                .track_presentation
                .annotation_section_expanded,
            read_section_expanded: self.session.track_presentation.read_section_expanded,
            presentation_groups: self
                .session
                .track_presentation
                .groups
                .iter()
                .map(|group| AutomationPresentationGroupSnapshot {
                    channel: group.channel,
                    id: group.id,
                    label: group.label.clone(),
                    members: group.members.clone(),
                    expanded: group.expanded,
                    content_visible: group.content_visible,
                })
                .collect(),
            presentation_composites: self
                .session
                .track_presentation
                .composites
                .iter()
                .map(|composite| AutomationPresentationCompositeSnapshot {
                    channel: composite.channel,
                    id: composite.id,
                    content_visible: composite.content_visible,
                })
                .collect(),
            presentation_nodes,
            pane_layouts,
            workspace_layout,
            active_track_gesture,
            selection: self.automation_selection_snapshot(),
            // Free-form status strings can contain local paths. Typed lifecycle
            // and readiness fields above provide safe status instead.
            status: Vec::new(),
            truncated,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn automation_selection_snapshot(&self) -> Option<AutomationSelectionSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        Some(observed_retained_selection(
            &self
                .session
                .panels
                .active()
                .retained_evidence
                .as_ref()?
                .selection,
            self.session.panels.active().viewport.interval(),
            &self.session.panels.active().contig_name,
        ))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn live_automation_selection_snapshot(&self) -> Option<AutomationSelectionSnapshot> {
        const MAXIMUM_FIELD_KEY_BYTES: usize = 256;
        const MAXIMUM_FIELD_VALUE_BYTES: usize = 4_096;

        let panel = self.session.panels.active();
        if !panel.navigation_presentation.is_interactive() {
            return None;
        }
        let bounded_fields = |fields: Vec<(String, String)>| {
            let truncated = fields.len() > MAXIMUM_AUTOMATION_EVIDENCE_FIELDS;
            let fields = fields
                .into_iter()
                .take(MAXIMUM_AUTOMATION_EVIDENCE_FIELDS)
                .map(|(key, value)| AutomationEvidenceField {
                    key: bounded_automation_text(&key, MAXIMUM_FIELD_KEY_BYTES),
                    value: bounded_automation_text(&value, MAXIMUM_FIELD_VALUE_BYTES),
                })
                .collect::<Vec<_>>();
            (fields, truncated)
        };
        let snapshot = |track_id, kind, position, start, end, raw_fields: Vec<(String, String)>| {
            let (fields, truncated) = bounded_fields(raw_fields);
            AutomationSelectionSnapshot {
                selection_id: String::new(),
                evidence_revision: 0,
                workspace_revision_at_selection: self.session.workspace_revision,
                panel_id: panel.id,
                contig: panel.contig_name.clone(),
                track_id,
                kind,
                granularity: None,
                anchor_position: position,
                evidence_start: start,
                evidence_end: end,
                position,
                start,
                end,
                fields,
                provenance: Vec::new(),
                visible_in_active_view: start.zip(end).is_some_and(|(start, end)| {
                    GenomicInterval::new(panel.viewport.interval().contig(), start, end)
                        .is_ok_and(|evidence| panel.viewport.interval().contains_interval(evidence))
                }),
                stale: false,
                stale_reason: None,
                truncated,
                presentation_truncated: false,
            }
        };

        if let Some(selection) = panel.selected_variant
            && let Some(inspector) = self.variant_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Variant,
                Some(record.start),
                Some(record.start),
                Some(record.end),
                variant_inspector_fields(&inspector, record),
            ));
        }
        if let Some(selection) = panel.selected_feature
            && let Some(inspector) = self.feature_inspector_snapshot().map(|inspector| {
                // A tile-pipeline selection may still show the tile's stand-in record; wait
                // briefly for the complete one so automation sees the inspector's fields.
                self.track_tile_feature_inspector_snapshot_blocking(
                    AUTOMATION_TILE_RECORD_LOOKUP_TIMEOUT,
                )
                .unwrap_or(inspector)
            })
        {
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Feature,
                Some(inspector.record.start),
                Some(inspector.record.start),
                Some(inspector.record.end),
                feature_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_gwas
            && let Some(inspector) = self.gwas_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Gwas,
                Some(record.position),
                Some(record.position),
                Some(record.position.saturating_add(1)),
                gwas_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_quantitative
            && let Some(inspector) = self.quantitative_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Quantitative,
                Some(selection.position),
                Some(record.start),
                Some(record.end),
                quantitative_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_copy_number
            && let Some(inspector) = self.copy_number_inspector_snapshot()
        {
            let segment = &inspector.query.segments[inspector.segment_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::CopyNumber,
                Some(segment.start),
                Some(segment.start),
                Some(segment.end),
                copy_number_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_mutation
            && let Some(inspector) = self.mutation_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Mutation,
                Some(record.start),
                Some(record.start),
                Some(record.end),
                mutation_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_interaction
            && let Some(inspector) = self.interaction_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Interaction,
                Some(record.source.start),
                Some(record.source.start),
                Some(record.source.end),
                interaction_inspector_fields(&inspector),
            ));
        }

        let alignment_track = panel
            .selected_alignment_track
            .or(match panel.selected_track {
                Some(TrackFrameKey::Alignment(id)) => Some(id),
                _ => None,
            });
        if let Some(position) = panel.selected_pileup_position
            && let Some(inspector) = self.pileup_inspector_snapshot()
        {
            let scene_evidence = alignment_track.and_then(|track_id| {
                let track = self
                    .session
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == track_id)?;
                track.scene_engine.as_ref()?;
                let presented = self.presented_alignment_lane(panel.id, track_id)?;
                let snapshot = presented.lane.snapshot.as_ref()?;
                snapshot
                    .pileup_site_at(position)
                    .map(|site| (site.evidence.owner_interval, site.evidence))
            });
            let interval = scene_evidence.map(|(interval, _)| interval);
            let mut captured = snapshot(
                alignment_track,
                AutomationSelectionKind::Pileup,
                Some(position),
                interval.map(GenomicInterval::start).or(Some(position)),
                interval
                    .map(GenomicInterval::end)
                    .or(Some(position.saturating_add(1))),
                inspector
                    .document(format!("pileup:{position}"), "Pileup")
                    .flatten_automation_fields(),
            );
            if let Some((_, evidence)) = scene_evidence {
                captured.provenance = vec![
                    AutomationEvidenceField {
                        key: "authority".to_owned(),
                        value: "committed_scene".to_owned(),
                    },
                    AutomationEvidenceField {
                        key: "bin_width_bases".to_owned(),
                        value: "1".to_owned(),
                    },
                    AutomationEvidenceField {
                        key: "eligible_reads".to_owned(),
                        value: evidence.eligible_records.to_string(),
                    },
                    AutomationEvidenceField {
                        key: "all_eligible_reads".to_owned(),
                        value: "true".to_owned(),
                    },
                    AutomationEvidenceField {
                        key: "allele_observations_available".to_owned(),
                        value: evidence.allele_observations_available.to_string(),
                    },
                    AutomationEvidenceField {
                        key: "collapsed_mate_bases".to_owned(),
                        value: evidence.collapsed_mate_bases.to_string(),
                    },
                ];
            }
            return Some(captured);
        }
        if let Some(position) = panel.selected_modification_position
            && let Some(inspector) = self.modification_inspector_snapshot()
        {
            return Some(snapshot(
                alignment_track,
                AutomationSelectionKind::Modification,
                Some(position),
                Some(position),
                Some(position.saturating_add(1)),
                inspector
                    .document(format!("modification:{position}"), "Modification frequency")
                    .flatten_automation_fields(),
            ));
        }
        if let Some((start, end)) = panel.selected_junction
            && let Some(inspector) = self.junction_inspector_snapshot()
        {
            return Some(snapshot(
                alignment_track,
                AutomationSelectionKind::Junction,
                Some(start),
                Some(start),
                Some(end),
                inspector
                    .document(format!("junction:{start}:{end}"), "Splice junction")
                    .flatten_automation_fields(),
            ));
        }
        if let Some(hit) = panel.selected_alignment_hit
            && let Some(inspector) = self.inspector_snapshot()
        {
            let track_id = hit.track;
            let presented = self.presented_alignment_lane(panel.id, track_id)?;
            let snapshot_authority = presented.lane.snapshot.as_ref()?;
            if !snapshot_authority.reads().contains_handle(hit.handle) {
                return None;
            }
            let record = snapshot_authority
                .inspect_retained(hit.handle)
                .ok()
                .flatten()?;
            let fields = inspector
                .document(
                    format!("read:{}:{:?}", track_id.get(), hit.handle),
                    "Alignment segment",
                )
                .flatten_automation_fields()
                .into_iter()
                .filter(|(key, _)| automation_read_evidence_field_is_public(key))
                .collect();
            return Some(snapshot(
                Some(track_id),
                if !matches!(hit.component, AlignmentComponent::ReadSpan) {
                    AutomationSelectionKind::ReadEvent
                } else {
                    AutomationSelectionKind::Read
                },
                Some(record.start),
                Some(record.start),
                Some(record.end),
                fields,
            ));
        }
        match panel.selected_track {
            Some(TrackFrameKey::Alignment(id) | TrackFrameKey::Auxiliary(id)) => Some(snapshot(
                Some(id),
                AutomationSelectionKind::Track,
                None,
                None,
                None,
                Vec::new(),
            )),
            Some(TrackFrameKey::Reference) => Some(snapshot(
                None,
                AutomationSelectionKind::Track,
                None,
                None,
                None,
                Vec::new(),
            )),
            Some(TrackFrameKey::Group { .. }) => None,
            None => None,
        }
    }
}
