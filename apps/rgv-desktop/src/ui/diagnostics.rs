use egui::Context;

use super::super::*;
use crate::display::alignment;

#[derive(Clone, Debug, Default)]
pub struct DiagnosticsSnapshot {
    pub frame: String,
    pub workspace: Option<String>,
    pub panels: Vec<String>,
    pub alignment_tracks: Vec<String>,
    pub layout: Option<String>,
    pub data_cache: Option<String>,
    pub cache_coordinator: Option<String>,
    pub remote: Option<String>,
    pub layout_cache: String,
    pub gpu_upload: String,
    pub navigation: String,
    pub interaction: Option<String>,
    pub landmarks: Option<String>,
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn diagnostics_snapshot(&self) -> DiagnosticsSnapshot {
        let mut rendered = 0_usize;
        let mut hidden = 0_usize;
        let mut primitives = 0_usize;
        let mut cache_bytes = 0_usize;
        let mut cache_hits = 0_u64;
        let mut cache_misses = 0_u64;
        let mut remote_requests = 0_u64;
        let mut remote_bytes = 0_u64;
        let mut remote_cache_hits = 0_u64;
        let mut remote_cancellations = 0_u64;
        if let Some(stats) = self.reference_remote_io_stats {
            remote_requests = stats.requests;
            remote_bytes = stats.response_bytes;
            remote_cache_hits = stats.cache_hits;
            remote_cancellations = stats.cancellations;
        }

        let active_panel_id = self.panels.active().id;
        let mut alignment_tracks = Vec::with_capacity(self.alignment_tracks.len());
        for track in &self.alignment_tracks {
            let projection = track.projection(active_panel_id);
            if let Some(stats) = projection
                .and_then(|projection| projection.cached_layout.as_ref())
                .map(|(_, layout)| &layout.stats)
            {
                rendered = rendered.saturating_add(stats.rendered_alignments);
                hidden = hidden.saturating_add(stats.hidden_alignments);
                primitives = primitives.saturating_add(stats.primitive_count);
            }
            if let Some(stats) = track.cache_stats {
                cache_bytes = cache_bytes.saturating_add(stats.resident_weight);
                cache_hits = cache_hits.saturating_add(stats.hits);
                cache_misses = cache_misses.saturating_add(stats.misses);
            }
            if let Some(stats) = track.remote_io_stats {
                remote_requests = remote_requests.saturating_add(stats.requests);
                remote_bytes = remote_bytes.saturating_add(stats.response_bytes);
                remote_cache_hits = remote_cache_hits.saturating_add(stats.cache_hits);
                remote_cancellations = remote_cancellations.saturating_add(stats.cancellations);
            }
            let lifecycle = if let Some(message) =
                projection.and_then(|projection| projection.unavailable.as_deref())
            {
                format!("unavailable: {message}")
            } else {
                match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => "opening".to_owned(),
                    AlignmentTrackLifecycle::Ready
                        if projection.is_some_and(AlignmentPanelProjection::query_is_checking) =>
                    {
                        format!(
                            "checking ({:?})",
                            projection.expect("checking projection exists").query_phase
                        )
                    }
                    AlignmentTrackLifecycle::Ready => "ready".to_owned(),
                    AlignmentTrackLifecycle::Failed(message) => format!("failed: {message}"),
                }
            };
            let records = projection.map_or_else(
                || "no result".to_owned(),
                |projection| {
                    if projection.query_is_checking() {
                        return "checking current viewport".to_owned();
                    }
                    let Some(query) = projection.current_query.as_deref() else {
                        return "no result".to_owned();
                    };
                    let layout_query = projection.layout_query.as_deref().unwrap_or(query);
                    alignment::individual_read_refusal(
                        query,
                        layout_query,
                        track.config.filters.read_stacking_mode,
                    )
                    .map_or_else(
                        || {
                            format!(
                                "complete · {} records · limit {}",
                                query.stats.records_eligible, query.stats.complete_read_limit
                            )
                        },
                        |refusal| {
                            format!(
                                "too dense · {} eligible · limit {} · 0 individual records",
                                refusal.eligible_records, refusal.limit
                            )
                        },
                    )
                },
            );
            let cache = track.cache_stats.map_or_else(
                || "cache n/a".to_owned(),
                |stats| format!("cache {} MiB", stats.resident_weight / (1024 * 1024)),
            );
            let working_set = projection.map_or_else(
                || "working set n/a".to_owned(),
                |projection| {
                    let stats = projection.working_set;
                    let resident = stats.resident_start.zip(stats.resident_end).map_or_else(
                        || "resident none".to_owned(),
                        |(start, end)| {
                            format!(
                                "resident {}-{} · {} records · {:.1} MiB",
                                start.saturating_add(1),
                                end,
                                stats.resident_records,
                                stats.resident_bytes as f64 / (1024.0 * 1024.0),
                            )
                        },
                    );
                    format!(
                        "{:?} · {resident} · buffer L {}/{} bp R {}/{} bp{} · derive lookup/projection/summary {}/{}/{} µs · summary tiles {} hit / {:.1} MiB · source queries {} foreground / {} speculative / {} total · extensions {}/{} complete, {} cancelled, {} dense, {} failed · {} source queries avoided · {:.1} MiB decode avoided",
                        stats.cache_outcome,
                        stats.effective_margin_left,
                        stats.requested_margin_left,
                        stats.effective_margin_right,
                        stats.requested_margin_right,
                        stats.clamp_reason.map_or_else(String::new, |reason| format!(" · clamp {reason:?}")),
                        stats.lookup_micros,
                        stats.projection_micros,
                        stats.summary_micros,
                        stats.summary_tile_hits,
                        stats.summary_index_bytes as f64 / (1024.0 * 1024.0),
                        stats.foreground_source_queries,
                        stats.speculative_source_queries,
                        stats.source_queries,
                        stats.extensions_completed,
                        stats.extensions_requested,
                        stats.extensions_cancelled,
                        stats.extensions_too_dense,
                        stats.extensions_failed,
                        stats.source_queries_avoided,
                        stats.decoded_bytes_avoided as f64 / (1024.0 * 1024.0),
                    )
                },
            );
            let row_scenes = projection.map_or_else(
                || "row scenes n/a".to_owned(),
                |projection| {
                    let bytes = AlignmentRowScene::estimated_resident_bytes(
                        projection.row_scenes.iter().map(Arc::as_ref),
                    );
                    let reconciliation = projection
                        .alignment_rows
                        .as_ref()
                        .map(|rows| rows.reconciliation)
                        .unwrap_or_default();
                    format!(
                        "row scenes {} / {:.1} MiB · {}{} · {}{}{}{} · {} µs",
                        projection.row_scenes.len(),
                        bytes as f64 / (1024.0 * 1024.0),
                        if projection.resident_query.as_ref().is_some_and(|resident| {
                            projection
                                .row_scenes
                                .iter()
                                .any(|scene| scene.interval().contains_interval(resident.interval))
                        }) {
                            "resident prepared"
                        } else {
                            "resident unprepared"
                        },
                        if self.alignment_row_scene_tasks.iter().any(|task| {
                            task.track_id == track.config.id
                                && task.panel_id == self.panels.active().id
                        }) {
                            " (preparing)"
                        } else {
                            ""
                        },
                        if reconciliation.scene_hit {
                            "hit"
                        } else {
                            "miss"
                        },
                        if reconciliation.scene_physical_hit {
                            " physical"
                        } else {
                            ""
                        },
                        if reconciliation.scene_extended {
                            " extended"
                        } else {
                            ""
                        },
                        if reconciliation.exact_camera_hit {
                            " exact-camera"
                        } else {
                            ""
                        },
                        reconciliation.scene_projection_micros,
                    )
                },
            );
            alignment_tracks.push(format!(
                "{} · {} · {} · {} · {} · {}",
                track.config.name, lifecycle, records, cache, working_set, row_scenes
            ));
        }

        let layout_cache = self.layout_cache.stats();
        let upload = self.rectangle_renderer.last_prepare_stats();
        let navigation_preview = self.navigation_preview_renderer.stats();
        let navigation_counters = self.panels.iter().fold([0_u64; 10], |mut counters, panel| {
            let schedule = &panel.navigation_query_schedule;
            counters[0] = counters[0].saturating_add(schedule.input_updates);
            counters[1] = counters[1].saturating_add(schedule.coalesced_updates);
            counters[2] = counters[2].saturating_add(schedule.dispatched_targets);
            counters[3] = counters[3].saturating_add(schedule.gesture_end_dispatches);
            counters[4] = counters[4].saturating_add(schedule.cancelled_targets);
            counters[5] = counters[5].saturating_add(schedule.committed_targets);
            counters[6] = counters[6].saturating_add(schedule.preview_frames);
            counters[7] = counters[7].saturating_add(schedule.neutral_frames_after_commit);
            counters[8] = counters[8].max(schedule.last_transaction_preview_frames);
            counters[9] = counters[9].max(schedule.last_commit_micros);
            counters
        });
        let diagnostic_now = Instant::now();
        let (sashimi_median, sashimi_p95, sashimi_maximum) = self
            .sashimi_interaction_diagnostics
            .duration_summary_micros();
        let latest_sashimi = &self.sashimi_interaction_diagnostics;
        let latest_result = latest_sashimi.latest_result;
        let pointer = latest_sashimi.latest_pointer.map_or_else(
            || "cursor n/a".to_owned(),
            |pointer| format!("cursor {:.1},{:.1}", pointer.x, pointer.y),
        );
        let phase = latest_sashimi.latest_phase.map_or_else(
            || "phase n/a".to_owned(),
            |phase| format!("phase {phase:?}"),
        );
        let lane = latest_result.lane_track_id.map_or_else(
            || "lane none".to_owned(),
            |track_id| format!("lane track {}", track_id.get()),
        );
        let distance = match (
            latest_result.nearest_curve_distance,
            latest_result.nearest_acquisition_radius,
        ) {
            (Some(distance), Some(radius)) => {
                format!("nearest {distance:.2} pt / radius {radius:.2} pt")
            }
            _ => "nearest n/a".to_owned(),
        };
        let acquired = latest_result.hover.map_or_else(
            || "acquired none".to_owned(),
            |hover| {
                format!(
                    "acquired track {} [{}..{}) {:?}",
                    hover.key.track_id.get(),
                    hover.key.start,
                    hover.key.end,
                    hover.key.component,
                )
            },
        );
        let latest_sashimi_probe = format!(
            "Sashimi latest · {pointer} · {phase} · gate {} · {lane} · targets {} total / {} lane / {} current revision · {} bounds / {} evaluated · {distance} · {acquired} · {} hover overlays",
            latest_result.gate.label(),
            latest_result.total_targets,
            latest_result.track_targets,
            latest_result.current_revision_targets,
            latest_result.bounds_candidates,
            latest_result.curve_evaluations,
            latest_sashimi.latest_hover_overlay_count,
        );
        let sashimi_interaction = format!(
            "{latest_sashimi_probe}\nSashimi totals · {} targets · {} hit tests / {} bounds candidates / {} curve evaluations · {} acquisitions / {} transfers / {} clears / {} hysteresis retentions / {} stale rejected · {} hover redraws / {} hover layout rebuilds · hit median/p95/max {}/{}/{} µs",
            self.panels.active().junction_arc_targets.len(),
            self.sashimi_interaction_diagnostics.hit_tests,
            self.sashimi_interaction_diagnostics.bounds_candidates,
            self.sashimi_interaction_diagnostics.curve_evaluations,
            self.sashimi_interaction_diagnostics.acquisitions,
            self.sashimi_interaction_diagnostics.transfers,
            self.sashimi_interaction_diagnostics.clears,
            self.sashimi_interaction_diagnostics.hysteresis_retentions,
            self.sashimi_interaction_diagnostics
                .stale_revision_rejections,
            self.sashimi_interaction_diagnostics.hover_redraws,
            self.sashimi_interaction_diagnostics.hover_layout_rebuilds,
            sashimi_median,
            sashimi_p95,
            sashimi_maximum,
        );
        DiagnosticsSnapshot {
            frame: format!(
                "{:.2} ms · {:.0} FPS",
                self.smoothed_frame_ms,
                1_000.0 / self.smoothed_frame_ms.max(0.01)
            ),
            workspace: (!self.alignment_tracks.is_empty()).then(|| {
                format!(
                    "{} alignment tracks · {} visible · {} complete-read ceiling · {:.2}× browsing buffer/side",
                    self.alignment_tracks.len(),
                    self.alignment_tracks
                        .iter()
                        .filter(|track| {
                            self.track_presentation
                                .track_content_exposed(
                                    ui::TrackPane::Annotation,
                                    track.config.id,
                                )
                                .unwrap_or(false)
                                || self
                                    .track_presentation
                                    .track_content_exposed(
                                        ui::TrackPane::Reads,
                                        track.config.id,
                                    )
                                    .unwrap_or(false)
                        })
                        .count(),
                    self.workspace_alignment_limit,
                    f32::from(self.settings.read_browsing_buffer_percent) / 100.0,
                )
            }),
            panels: self
                .panels
                .iter()
                .map(|panel| {
                    let source = panel
                        .navigation_presentation
                        .source
                        .as_ref()
                        .map(|source| {
                            format!(
                                " · source {}:{}-{}@{}",
                                panel.contig_name,
                                source.interval.start().saturating_add(1),
                                source.interval.end(),
                                source.generation,
                            )
                        })
                        .unwrap_or_default();
                    let transaction = if panel.navigation_presentation.is_unsettled() {
                        format!(
                            " · target @{} · {} ms · {}",
                            panel.navigation_presentation.target_generation,
                            panel
                                .navigation_presentation
                                .age(diagnostic_now)
                                .as_millis(),
                            panel
                                .navigation_presentation
                                .preview_kind
                                .map_or("neutral", NavigationPreviewKind::label),
                        )
                    } else {
                        String::new()
                    };
                    let invalidation = panel
                        .navigation_presentation
                        .invalidation_reason
                        .as_deref()
                        .map_or_else(String::new, |reason| format!(" · invalidated: {reason}"));
                    format!(
                        "panel {} · {}:{}-{} · generation {} · {:?}{source}{transaction}{invalidation}{}",
                        panel.id.get(),
                        panel.contig_name,
                        panel.viewport.interval().start().saturating_add(1),
                        panel.viewport.interval().end(),
                        panel.generation_clock.current().get(),
                        panel.navigation_presentation.phase,
                        if panel.id == active_panel_id {
                            " · active"
                        } else {
                            ""
                        },
                    )
                })
                .collect(),
            alignment_tracks,
            layout: (rendered > 0 || hidden > 0).then(|| {
                format!(
                    "workspace layout rev {} · {} recomputes · {} settled resize queries · {rendered} shown · {hidden} hidden · {primitives} primitives",
                    self.workspace_layout.revision,
                    self.workspace_layout_recomputations,
                    self.resize_query_refreshes,
                )
            }),
            data_cache: (!self.alignment_tracks.is_empty()).then(|| {
                format!(
                    "alignment caches {} MiB · {} hits / {} misses",
                    cache_bytes / (1024 * 1024),
                    cache_hits,
                    cache_misses
                )
            }),
            cache_coordinator: (!self.alignment_tracks.is_empty()).then(|| {
                let cache = self.cache_coordinator;
                format!(
                    "cache coordinator {:.1}/{:.1} MiB · decoded {:.1}/{:.1} · layout {:.1}/{:.1} · rows {:.1}/{:.1} · GPU {:.1}/{:.1}{} · {} rebalances",
                    cache.measured_resident_bytes as f64 / (1024.0 * 1024.0),
                    cache.total_budget_bytes as f64 / (1024.0 * 1024.0),
                    cache.decoded_resident_bytes as f64 / (1024.0 * 1024.0),
                    cache.decoded_allowance_bytes as f64 / (1024.0 * 1024.0),
                    cache.layout_resident_bytes as f64 / (1024.0 * 1024.0),
                    cache.layout_allowance_bytes as f64 / (1024.0 * 1024.0),
                    cache.row_scene_resident_bytes as f64 / (1024.0 * 1024.0),
                    cache.row_scene_allowance_bytes as f64 / (1024.0 * 1024.0),
                    cache.gpu_slab_resident_bytes as f64 / (1024.0 * 1024.0),
                    cache.gpu_slab_allowance_bytes as f64 / (1024.0 * 1024.0),
                    if cache.over_budget_bytes > 0 {
                        format!(
                            " · {:.1} MiB over ({})",
                            cache.over_budget_bytes as f64 / (1024.0 * 1024.0),
                            cache.clamp_reason.unwrap_or("foreground pinned")
                        )
                    } else {
                        String::new()
                    },
                    cache.rebalances,
                )
            }),
            remote: (remote_requests > 0).then(|| {
                format!(
                    "remote {remote_requests} requests · {:.2} MiB · {remote_cache_hits} range-cache hits · {remote_cancellations} cancelled",
                    remote_bytes as f64 / (1024.0 * 1024.0)
                )
            }),
            layout_cache: format!(
                "layout cache {} MiB · {} hits / {} misses",
                layout_cache.resident_weight / (1024 * 1024),
                layout_cache.hits,
                layout_cache.misses
            ),
            gpu_upload: format!(
                "GPU upload {} KiB{} · slabs {} entries / {} MiB / {} · preview {}x{} px @ {:.2}x · {} MiB · {} captures / {} composites / {} invalidations / {} fallbacks · horizon {} checks / {} hits / {} misses / {} budget fallbacks · {} µs / {} rectangles",
                upload.uploaded_bytes / 1024,
                if upload.truncated { " · capped" } else { "" },
                upload.slab_entries,
                upload.slab_resident_bytes / (1024 * 1024),
                if upload.slab_cache_hit { "hit" } else { "miss" },
                navigation_preview.width,
                navigation_preview.height,
                self.window.scale_factor(),
                navigation_preview.allocated_bytes / (1024 * 1024),
                navigation_preview.texture_captures,
                navigation_preview.texture_composites,
                navigation_preview.texture_invalidations,
                navigation_preview.texture_fallbacks,
                self.navigation_horizon_preview_stats.eligibility_checks,
                self.navigation_horizon_preview_stats.hits,
                self.navigation_horizon_preview_stats.misses,
                self.navigation_horizon_preview_stats.budget_fallbacks,
                self.navigation_horizon_preview_stats.last_build_micros,
                self.navigation_horizon_preview_stats.last_rectangle_count,
            ),
            navigation: format!(
                "navigation {} inputs / {} coalesced / {} dispatched / {} release-dispatched / {} cancelled / {} committed · {} preview frames / {} neutral-after-commit · last {} frames / {:.2} ms · {} stale results rejected · row layout {} active / {} cancelled",
                navigation_counters[0],
                navigation_counters[1],
                navigation_counters[2],
                navigation_counters[3],
                navigation_counters[4],
                navigation_counters[5],
                navigation_counters[6],
                navigation_counters[7],
                navigation_counters[8],
                navigation_counters[9] as f64 / 1_000.0,
                self.navigation_stale_result_rejections,
                self.alignment_row_layout_tasks.len(),
                self.alignment_row_layout_cancellations,
            ),
            interaction: Some(self.interaction_trace.snapshot().map_or(
                sashimi_interaction.clone(),
                |trace| {
                    let maximum = trace
                        .events
                        .iter()
                        .filter_map(|event| event.duration_ms)
                        .fold(0.0_f64, f64::max);
                    format!(
                        "scroll gesture {} · {} events{} · longest phase {:.2} ms · {sashimi_interaction}",
                        trace.gesture_id,
                        trace.events.len(),
                        if trace.active { " · recording" } else { "" },
                        maximum,
                    )
                },
            )),
            landmarks: self.landmark_diagnostics_text(),
        }
    }

    fn landmark_diagnostics_text(&self) -> Option<String> {
        let snapshot = self.automation_landmark_snapshot();
        if !snapshot.resource_configured
            && snapshot.catalogue_schema_version.is_none()
            && self.landmark_catalogue_error.is_none()
        {
            return None;
        }
        let root = self.landmark_resource_root.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |path| path.display().to_string(),
        );
        let resolution = self.landmark_resolution.as_ref();
        let bundle = snapshot
            .matched_bundle_id
            .as_deref()
            .map_or("none", |value| value);
        let version = snapshot
            .matched_bundle_version
            .as_deref()
            .map_or("none", |value| value);
        let checksum = snapshot
            .gene_asset_sha256
            .as_deref()
            .map_or("none", |value| value);
        let error = self
            .landmark_catalogue_error
            .as_deref()
            .map_or("none", |value| value);
        Some(format!(
            "root {root}\nschema {:?} · resolution {:?} · evidence {:?} · subset {}\nbundle {bundle} · version {version} · integrity {:?}\nautomatic {:?} · loaded {} · visible {} · suppressed {} · search {}\ngene format {:?} · SHA-256 {checksum}\nreason {}\nerror {error}",
            snapshot.catalogue_schema_version,
            snapshot.resolution_status,
            snapshot.evidence,
            snapshot.subset,
            snapshot.integrity,
            snapshot.automatic_track_state,
            snapshot.automatic_track_loaded,
            snapshot.automatic_track_visible,
            snapshot.suppressed,
            snapshot.search_ready,
            snapshot.gene_asset_format,
            resolution.map_or("not resolved", |value| value.reason.as_str()),
        ))
    }
}

pub fn show_diagnostics(context: &Context, open: &mut bool, snapshot: &DiagnosticsSnapshot) {
    egui::Window::new("Diagnostics")
        .open(open)
        .default_width(420.0)
        .show(context, |ui| {
            ui.monospace(&snapshot.frame);
            if let Some(workspace) = &snapshot.workspace {
                ui.monospace(workspace);
            }
            if let Some(layout) = &snapshot.layout {
                ui.monospace(layout);
            }
            if let Some(cache) = &snapshot.data_cache {
                ui.monospace(cache);
            }
            if let Some(cache) = &snapshot.cache_coordinator {
                ui.monospace(cache);
            }
            if let Some(remote) = &snapshot.remote {
                ui.monospace(remote);
            }
            if !snapshot.panels.is_empty() {
                ui.separator();
                ui.strong("View panels");
                for panel in &snapshot.panels {
                    ui.monospace(panel);
                }
            }
            if !snapshot.alignment_tracks.is_empty() {
                ui.separator();
                ui.strong("Alignment tracks");
                for track in &snapshot.alignment_tracks {
                    ui.monospace(track);
                }
            }
            if let Some(landmarks) = &snapshot.landmarks {
                ui.separator();
                ui.strong("Genome landmarks");
                ui.monospace(landmarks);
            }
            ui.monospace(&snapshot.layout_cache);
            ui.monospace(&snapshot.gpu_upload);
            ui.monospace(&snapshot.navigation);
            if let Some(interaction) = &snapshot.interaction {
                ui.monospace(interaction);
            }
        });
}

pub const fn toggled_diagnostics(open: bool) -> bool {
    !open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_visibility_toggles_without_enabling_collection() {
        assert!(toggled_diagnostics(false));
        assert!(!toggled_diagnostics(true));
    }
}
