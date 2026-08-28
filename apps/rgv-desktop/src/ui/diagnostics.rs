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
    pub remote: Option<String>,
    pub layout_cache: String,
    pub gpu_upload: String,
    pub navigation: String,
    pub interaction: Option<String>,
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
                        "checking".to_owned()
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
            alignment_tracks.push(format!(
                "{} · {} · {} · {}",
                track.config.name, lifecycle, records, cache
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
        DiagnosticsSnapshot {
            frame: format!(
                "{:.2} ms · {:.0} FPS",
                self.smoothed_frame_ms,
                1_000.0 / self.smoothed_frame_ms.max(0.01)
            ),
            workspace: (!self.alignment_tracks.is_empty()).then(|| {
                format!(
                    "{} alignment tracks · {} visible · {} complete-read ceiling",
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
                    self.workspace_alignment_limit
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
                "GPU upload {} KiB{} · preview {}x{} px @ {:.2}x · {} MiB · {} captures / {} composites / {} invalidations / {} fallbacks · horizon {} checks / {} hits / {} misses / {} budget fallbacks · {} µs / {} rectangles",
                upload.uploaded_bytes / 1024,
                if upload.truncated { " · capped" } else { "" },
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
                "navigation {} inputs / {} coalesced / {} dispatched / {} release-dispatched / {} cancelled / {} committed · {} preview frames / {} neutral-after-commit · last {} frames / {:.2} ms · {} stale results rejected",
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
            ),
            interaction: self.interaction_trace.snapshot().map(|trace| {
                let maximum = trace
                    .events
                    .iter()
                    .filter_map(|event| event.duration_ms)
                    .fold(0.0_f64, f64::max);
                format!(
                    "scroll gesture {} · {} events{} · longest phase {:.2} ms",
                    trace.gesture_id,
                    trace.events.len(),
                    if trace.active { " · recording" } else { "" },
                    maximum,
                )
            }),
        }
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
