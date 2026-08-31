use egui::Context;

use super::super::*;

#[derive(Clone, Debug, Default)]
pub struct DiagnosticsSnapshot {
    pub frame: String,
    pub workspace: Option<String>,
    pub panels: Vec<String>,
    pub alignment_tracks: Vec<String>,
    pub layout: Option<String>,
    pub data_cache: Option<String>,
    pub alignment_scene_budget: Option<String>,
    pub remote: Option<String>,
    pub gpu_upload: String,
    pub navigation: String,
    pub interaction: Option<String>,
    pub landmarks: Option<String>,
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn diagnostics_snapshot(&self) -> DiagnosticsSnapshot {
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
        let alignment_ledger_progress = self.alignment_scene_ledger.progress().ok();
        let alignment_ledger_operations = self.alignment_scene_ledger.operation_counters().ok();
        let read_gpu_operations = self.alignment_gpu_renderer.operation_counters();
        let summary_gpu_operations = self.alignment_summary_gpu_renderer.operation_counters();
        let mut alignment_tracks = Vec::with_capacity(self.alignment_tracks.len());
        for track in &self.alignment_tracks {
            let projection = track.projection(active_panel_id);
            if track.scene_engine.is_some() {
                let Some(projection) = projection else {
                    alignment_tracks.push(format!(
                        "{} · unified scene · no panel transaction",
                        track.config.name
                    ));
                    continue;
                };
                let transaction = projection.navigation.diagnostic();
                let staged_source = projection.scene_frame.as_ref().map_or_else(
                    || "staged source frame absent".to_owned(),
                    |frame| {
                        let records = frame.snapshot.as_ref().map_or(0, |snapshot| {
                            snapshot.reads().evidence().record_count()
                        });
                        format!(
                            "staged source {} records · genomic {:?} · rows {:?}/{:?} · reference {:?} (products current {}) · reads {:?} · vertical {:?}",
                            records,
                            frame.covered_interval,
                            frame.covered_rows,
                            frame.requested_rows,
                            frame.reference_requirement,
                            frame.reference_products_current,
                            frame.individual_reads,
                            frame.vertical_residency,
                        )
                    },
                );
                let committed = self
                    .presented_alignment_lane(active_panel_id, track.config.id)
                    .map_or_else(
                        || "committed presentation unavailable for desired target".to_owned(),
                        |presented| {
                            presented.lane.snapshot.as_ref().map_or_else(
                                || {
                                    format!(
                                        "committed target {} · no semantic snapshot · summaries current {} · reads {:?}",
                                        presented.target_revision,
                                        presented.lane.summary_current(),
                                        presented.source.individual_reads,
                                    )
                                },
                                |snapshot| {
                                    format!(
                                        "committed target {} · scene {:?} · prepared {:?} · {} records / {} atlas rows · read pages {} · summary pages {} (depth {} allele {} junction {} modification {}) · summaries current {} · read pixel authority {} · reads {:?}",
                                        presented.target_revision,
                                        snapshot.revision(),
                                        snapshot.prepared_interval(),
                                        snapshot.reads().evidence().record_count(),
                                        snapshot.reads().atlas().row_count(),
                                        snapshot.reads().pages().len(),
                                        snapshot.summary_pages().len(),
                                        snapshot.summaries().depth_tiles().len(),
                                        snapshot.summaries().allele_tiles().len(),
                                        snapshot.summaries().junction_tiles().len(),
                                        snapshot.summaries().modification_tiles().len(),
                                        presented.lane.summary_current(),
                                        presented.read_authority.is_some(),
                                        presented.source.individual_reads,
                                    )
                                },
                            )
                        },
                    );
                let recent = projection
                    .navigation
                    .journal()
                    .rev()
                    .take(4)
                    .map(|entry| {
                        format!(
                            "p{:?}/track{:?} g{} t{} {:?}->{:?} owner={:?} products={:?} camera={:?}: {}",
                            entry.panel_id,
                            entry.track_id,
                            entry.generation,
                            entry
                                .request_token
                                .map_or(0, AlignmentSceneRequestToken::get),
                            entry.from,
                            entry.to,
                            entry.owner,
                            entry.products,
                            entry.camera.map(|camera| camera.interval()),
                            entry.reason,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                let invariant = transaction
                    .invariant_failure
                    .as_deref()
                    .map_or_else(String::new, |failure| {
                        format!(" · INVARIANT FAILURE: {failure}")
                    });
                alignment_tracks.push(format!(
                    "{} · unified scene · panel {:?} track {:?} · g{} t{} · {:?} ({}) · owner {:?} · products {:?} · camera {:?} · {} · {} · journal {}/64 [{}]{}{}",
                    track.config.name,
                    transaction.panel_id,
                    transaction.track_id,
                    transaction.generation,
                    transaction
                        .request_token
                        .map_or(0, AlignmentSceneRequestToken::get),
                    transaction.phase,
                    transaction.phase.status(),
                    transaction.owner,
                    transaction.products,
                    transaction.desired_camera.map(|camera| camera.interval()),
                    committed,
                    staged_source,
                    transaction.journal_entries,
                    recent,
                    transaction
                        .detail
                        .as_deref()
                        .map_or_else(String::new, |detail| format!(" · {detail}")),
                    invariant,
                ));
                continue;
            }
            alignment_tracks.push(format!(
                "{} · unsupported by unified alignment scene backend · {:?}",
                track.config.name, track.lifecycle,
            ));
        }

        let upload = self.rectangle_renderer.last_prepare_stats();
        let context_gpu = self.context_scene_gpu_renderer.last_prepare_stats();
        let navigation_counters = self.panels.iter().fold([0_u64; 7], |mut counters, panel| {
            let schedule = &panel.navigation_query_schedule;
            counters[0] = counters[0].saturating_add(schedule.input_updates);
            counters[1] = counters[1].saturating_add(schedule.coalesced_updates);
            counters[2] = counters[2].saturating_add(schedule.dispatched_targets);
            counters[3] = counters[3].saturating_add(schedule.gesture_end_dispatches);
            counters[4] = counters[4].saturating_add(schedule.cancelled_targets);
            counters[5] = counters[5].saturating_add(schedule.committed_targets);
            counters[6] = counters[6].max(schedule.last_commit_micros);
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
            latest_result.total_targets,
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
                    let transaction = if panel.navigation_presentation.is_unsettled() {
                        format!(
                            " · target @{} · {} ms · acquiring independent lanes",
                            panel.navigation_presentation.target_generation,
                            panel
                                .navigation_presentation
                                .age(diagnostic_now)
                                .as_millis(),
                        )
                    } else {
                        String::new()
                    };
                    let invalidation = panel
                        .navigation_presentation
                        .invalidation_reason
                        .as_deref()
                        .map_or_else(String::new, |reason| format!(" · invalidated: {reason}"));
                    let contextual = if panel.context_scene.lanes.is_empty() {
                        String::new()
                    } else {
                        let lanes = panel
                            .context_scene
                            .lanes
                            .iter()
                            .map(|(key, state)| {
                                let name = match key {
                                    ContextLaneKey::Reference => "reference".to_owned(),
                                    ContextLaneKey::Auxiliary(track_id) => {
                                        format!("track {}", track_id.get())
                                    }
                                };
                                format!(
                                    "{name}={}/{}@{}{}",
                                    state.readiness.label(),
                                    state.gpu_readiness.label(),
                                    state
                                        .lease
                                        .as_ref()
                                        .map_or(0, |lease| lease.generation),
                                    state
                                        .gpu_failure
                                        .as_deref()
                                        .map_or_else(String::new, |failure| format!("!{failure}")),
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            " · context [{lanes}] · {} coherence groups",
                            panel.context_scene.coherence_groups.len()
                        )
                    };
                    format!(
                        "panel {} · {}:{}-{} · generation {} · {:?}{transaction}{invalidation}{contextual}{}",
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
            layout: Some(format!(
                "workspace layout rev {} · {} recomputes · {} settled resize queries",
                self.workspace_layout.revision,
                self.workspace_layout_recomputations,
                self.resize_query_refreshes,
            )),
            data_cache: None,
            alignment_scene_budget: (!self.alignment_tracks.is_empty()).then(|| {
                let scene = alignment_ledger_progress.map_or_else(
                    || "scene ledger unavailable".to_owned(),
                    |progress| {
                        let retained = progress.retained;
                        let operations = alignment_ledger_operations.unwrap_or_default();
                        format!(
                            "scene ledger {:.1}/{:.1} MiB charged · evidence {:.1} · summaries {:.1} · rows {:.1} · semantic {:.1} · GPU/frames {:.1} · reserved {:.1} · transient {:.1} (peak {:.1}) · pinned {:.1} · replacement {:?} · ledger inventories {} / {} records enumerated · prepared-manifest builds read {} ({} pages), summary {} ({} pages) · bounded prefetch keys inspected read {}, summary {} · indexed reclaim keys inspected read {}, summary {}",
                            progress.charged as f64 / (1024.0 * 1024.0),
                            progress.budget as f64 / (1024.0 * 1024.0),
                            retained.evidence_bytes() as f64 / (1024.0 * 1024.0),
                            retained.summary_bytes() as f64 / (1024.0 * 1024.0),
                            retained.row_atlas_bytes() as f64 / (1024.0 * 1024.0),
                            retained.semantic_scene_bytes() as f64 / (1024.0 * 1024.0),
                            retained.gpu_bytes() as f64 / (1024.0 * 1024.0),
                            progress.reserved as f64 / (1024.0 * 1024.0),
                            progress.transient as f64 / (1024.0 * 1024.0),
                            progress.transient_high_water as f64 / (1024.0 * 1024.0),
                            progress.pinned as f64 / (1024.0 * 1024.0),
                            progress.active_replacement_bundle,
                            operations.full_inventory_snapshots,
                            operations.allocation_records_enumerated,
                            read_gpu_operations.content_manifest_builds,
                            read_gpu_operations.content_manifest_pages_enumerated,
                            summary_gpu_operations.content_manifest_builds,
                            summary_gpu_operations.content_manifest_pages_enumerated,
                            read_gpu_operations.prefetch_authority_keys_inspected,
                            summary_gpu_operations.prefetch_authority_keys_inspected,
                            read_gpu_operations.reclaim_authority_keys_inspected,
                            summary_gpu_operations.reclaim_authority_keys_inspected,
                        )
                    },
                );
                format!(
                    "{scene}\nBudget scope: scene-owned CPU/GPU allocations only; wgpu driver copies, allocator retention, UI, generic tracks, and process runtime memory are outside the ledger and explain any RSS difference."
                )
            }),
            remote: (remote_requests > 0).then(|| {
                format!(
                    "remote {remote_requests} requests · {:.2} MiB · {remote_cache_hits} range-cache hits · {remote_cancellations} cancelled",
                    remote_bytes as f64 / (1024.0 * 1024.0)
                )
            }),
            gpu_upload: format!(
                "GPU upload {} KiB{} · generic slabs {} entries / {} MiB / {} · contextual {} lanes / {:.2} MiB · {} KiB geometry / {} camera writes / {} rejected",
                upload.uploaded_bytes / 1024,
                if upload.truncated { " · capped" } else { "" },
                upload.slab_entries,
                upload.slab_resident_bytes / (1024 * 1024),
                if upload.slab_cache_hit { "hit" } else { "miss" },
                context_gpu.resident_lanes,
                context_gpu.resident_bytes as f64 / (1024.0 * 1024.0),
                context_gpu.uploaded_bytes / 1024,
                context_gpu.camera_writes,
                context_gpu.rejected_lanes,
            ),
            navigation: format!(
                "navigation {} inputs / {} coalesced / {} dispatched / {} release-dispatched / {} cancelled / {} committed · last {:.2} ms · {} stale results rejected · row layout {} active / {} cancelled",
                navigation_counters[0],
                navigation_counters[1],
                navigation_counters[2],
                navigation_counters[3],
                navigation_counters[4],
                navigation_counters[5],
                navigation_counters[6] as f64 / 1_000.0,
                self.navigation_stale_result_rejections,
                0,
                0,
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
            if let Some(cache) = &snapshot.alignment_scene_budget {
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
