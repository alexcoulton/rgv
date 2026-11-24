//! Rolling per-frame CPU timings of the alignment scene, reported by `describe_workspace` under
//! `alignment_scene_diagnostics`: the last value and the median and 95th percentile over the
//! most recent [`FRAME_TIMING_WINDOW`] samples of each phase. Recording is a constant-time push
//! into a fixed ring; the percentiles are computed only when a snapshot asks for them.

use std::collections::VecDeque;
use std::time::Duration;

use rgv_render::{AlignmentGpuPrepareStats, AlignmentSummaryGpuPrepareStats};

/// Frames each rolling window spans (about two seconds at 60 Hz).
pub(crate) const FRAME_TIMING_WINDOW: usize = 120;

/// The most recent [`FRAME_TIMING_WINDOW`] samples of one timing, in microseconds.
#[derive(Clone, Debug, Default)]
pub(crate) struct RollingMicros {
    samples: VecDeque<u64>,
}

/// Last value and rolling median and 95th percentile of one timing, in microseconds.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RollingMicrosSummary {
    pub(crate) last: u64,
    pub(crate) p50: u64,
    pub(crate) p95: u64,
    pub(crate) samples: usize,
}

impl RollingMicros {
    pub(crate) fn push(&mut self, micros: u64) {
        if self.samples.len() == FRAME_TIMING_WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(micros);
    }

    pub(crate) fn push_duration(&mut self, duration: Duration) {
        self.push(u64::try_from(duration.as_micros()).unwrap_or(u64::MAX));
    }

    /// Nearest-rank percentiles over the window; all zero before the first sample.
    pub(crate) fn summary(&self) -> RollingMicrosSummary {
        let mut sorted = self.samples.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        let rank = |percent: usize| {
            if sorted.is_empty() {
                return 0;
            }
            let index = (percent * sorted.len()).div_ceil(100).saturating_sub(1);
            sorted[index.min(sorted.len() - 1)]
        };
        RollingMicrosSummary {
            last: self.samples.back().copied().unwrap_or(0),
            p50: rank(50),
            p95: rank(95),
            samples: sorted.len(),
        }
    }
}

/// The alignment scene's per-frame phases.
#[derive(Clone, Debug, Default)]
pub(crate) struct AlignmentFrameTimings {
    /// Text label projection of each read frame.
    pub(crate) label_projection: RollingMicros,
    /// The rest of each read frame's preparation: culling, interaction flags, uniforms.
    pub(crate) instance_preparation: RollingMicros,
    /// Coverage/modification/sashimi overlay preparation of each summary frame.
    pub(crate) overlay_preparation: RollingMicros,
    /// Time blocked in `Surface::get_current_texture` waiting for a drawable.
    pub(crate) surface_acquire_wait: RollingMicros,
}

impl AlignmentFrameTimings {
    pub(crate) fn record_reads(&mut self, stats: &AlignmentGpuPrepareStats) {
        self.label_projection.push(stats.label_projection_micros);
        self.instance_preparation
            .push(stats.instance_preparation_micros);
    }

    pub(crate) fn record_summaries(&mut self, stats: &AlignmentSummaryGpuPrepareStats) {
        self.overlay_preparation
            .push(stats.overlay_preparation_micros);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_window_reports_last_median_and_tail_of_the_recent_frames_only() {
        let mut timing = RollingMicros::default();
        assert_eq!(timing.summary(), RollingMicrosSummary::default());
        for micros in 1..=100 {
            timing.push(micros);
        }
        let summary = timing.summary();
        assert_eq!((summary.last, summary.p50, summary.p95), (100, 50, 95));
        assert_eq!(summary.samples, 100);
        // Old frames leave the window: after 120 more samples of 7 nothing else remains.
        for _ in 0..FRAME_TIMING_WINDOW {
            timing.push(7);
        }
        let summary = timing.summary();
        assert_eq!((summary.last, summary.p50, summary.p95), (7, 7, 7));
        assert_eq!(summary.samples, FRAME_TIMING_WINDOW);
        timing.push_duration(Duration::from_millis(3));
        assert_eq!(timing.summary().last, 3_000);
    }

    /// The timing and culling fields reach `describe_workspace` under
    /// `alignment_scene_diagnostics`, and a snapshot from before they existed still decodes.
    #[test]
    fn workspace_diagnostics_carry_the_frame_timing_fields() {
        let diagnostics =
            serde_json::to_value(rgv_automation::AutomationAlignmentSceneDiagnostics::default())
                .unwrap();
        for field in [
            "read_frame_body_instances",
            "read_frame_unculled_instances",
            "read_frame_draw_ranges",
            "read_frame_label_anchors_examined",
            "read_frame_label_projection_micros",
            "read_frame_label_projection_micros_p50",
            "read_frame_label_projection_micros_p95",
            "read_frame_instance_preparation_micros",
            "read_frame_instance_preparation_micros_p50",
            "read_frame_instance_preparation_micros_p95",
            "summary_frame_overlay_preparation_micros",
            "summary_frame_overlay_preparation_micros_p50",
            "summary_frame_overlay_preparation_micros_p95",
            "surface_acquire_wait_micros",
            "surface_acquire_wait_micros_p50",
            "surface_acquire_wait_micros_p95",
            "frame_timing_window_samples",
        ] {
            assert!(diagnostics.get(field).is_some(), "missing {field}");
        }
        let legacy: rgv_automation::AutomationAlignmentSceneDiagnostics =
            serde_json::from_value(serde_json::json!({ "read_frame_body_instances": 7 })).unwrap();
        assert_eq!(legacy.read_frame_body_instances, 7);
        assert_eq!(legacy.surface_acquire_wait_micros_p95, 0);
    }

    #[test]
    fn frame_stats_feed_their_phases() {
        let mut timings = AlignmentFrameTimings::default();
        timings.record_reads(&AlignmentGpuPrepareStats {
            label_projection_micros: 40,
            instance_preparation_micros: 9,
            ..AlignmentGpuPrepareStats::default()
        });
        timings.record_summaries(&AlignmentSummaryGpuPrepareStats {
            overlay_preparation_micros: 5,
            ..AlignmentSummaryGpuPrepareStats::default()
        });
        assert_eq!(timings.label_projection.summary().last, 40);
        assert_eq!(timings.instance_preparation.summary().last, 9);
        assert_eq!(timings.overlay_preparation.summary().last, 5);
        assert_eq!(timings.surface_acquire_wait.summary().samples, 0);
    }
}
