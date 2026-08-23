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
    pub remote: Option<String>,
    pub layout_cache: String,
    pub gpu_upload: String,
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
                        if projection.is_some_and(|projection| projection.pending_query) =>
                    {
                        "loading".to_owned()
                    }
                    AlignmentTrackLifecycle::Ready => "ready".to_owned(),
                    AlignmentTrackLifecycle::Failed(message) => format!("failed: {message}"),
                }
            };
            let records = projection
                .and_then(|projection| projection.current_query.as_ref())
                .map_or_else(
                    || "no result".to_owned(),
                    |query| {
                        format!(
                            "{}/{} records",
                            query.stats.records_emitted, query.stats.records_eligible
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
        DiagnosticsSnapshot {
            frame: format!(
                "{:.2} ms · {:.0} FPS",
                self.smoothed_frame_ms,
                1_000.0 / self.smoothed_frame_ms.max(0.01)
            ),
            workspace: (!self.alignment_tracks.is_empty()).then(|| {
                format!(
                    "{} alignment tracks · {} visible · {} record ceiling",
                    self.alignment_tracks.len(),
                    self.alignment_tracks
                        .iter()
                        .filter(|track| track.config.visible)
                        .count(),
                    self.workspace_alignment_limit
                )
            }),
            panels: self
                .panels
                .iter()
                .map(|panel| {
                    format!(
                        "panel {} · {}:{}-{} · generation {}{}",
                        panel.id.get(),
                        panel.contig_name,
                        panel.viewport.interval().start().saturating_add(1),
                        panel.viewport.interval().end(),
                        panel.generation_clock.current().get(),
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
                    "workspace layout {rendered} shown · {hidden} hidden · {primitives} primitives"
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
                "GPU upload {} KiB{}",
                upload.uploaded_bytes / 1024,
                if upload.truncated { " · capped" } else { "" }
            ),
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
