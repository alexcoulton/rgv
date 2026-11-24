use crate::Viewer;
use crate::diagnostics::ManualDiagnosticFramePhase;
use std::time::Instant;

const fn alignment_scene_prefetch_pending(reads: bool, summaries: bool) -> bool {
    reads || summaries
}

const fn alignment_scene_post_submit_prefetch(
    publication_blocked: bool,
    reads: bool,
    summaries: bool,
) -> bool {
    !publication_blocked && alignment_scene_prefetch_pending(reads, summaries)
}

impl Viewer {
    /// Poll, prepare, compose, and submit one application frame.
    pub(crate) fn render(&mut self) {
        // Check the device before preparing anything: a frame laid out and uploaded against a
        // lost device is wasted work. `submit_ui_frame` checks again for a loss during the frame.
        if !self.ensure_gpu_device() {
            return;
        }
        let frame_started = Instant::now();
        self.begin_manual_diagnostic_frame();
        self.alignment_frame_work.open_frame();
        let phase_started = Instant::now();
        self.poll_background_work();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::PollBackground,
            phase_started.elapsed(),
        );
        let phase_started = Instant::now();
        // Captured overview-drag motion is this frame's gesture step. Together with the input
        // handled since the last frame it owes at most one dispatch per panel and one scene
        // capture; run them once, before the display list is rebuilt from that capture.
        self.alignment_frame_work.begin_input();
        self.apply_captured_motion();
        self.flush_alignment_frame_work();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ApplyCapturedMotion,
            phase_started.elapsed(),
        );

        let phase_started = Instant::now();
        let first_egui_frame = self.egui_context.cumulative_frame_nr() == 0;
        if !first_egui_frame {
            self.rebuild_display_list();
            // Feature labels of routed lanes are painted by the shell frame below.
            self.plan_track_tile_lanes();
        }
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::FirstDisplayRebuild,
            phase_started.elapsed(),
        );
        self.alignment_frame_work.close_reuse_window();
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let phase_started = Instant::now();
        let (full_output, repaint_delay) = self.render_shell_frame(raw_input);
        self.diagnostics
            .frame_phase(ManualDiagnosticFramePhase::Shell, phase_started.elapsed());
        // Shell layout is resolved by egui. If it changed the committed
        // workspace geometry, rebuild before submitting this same frame so
        // native GPU primitives, egui chrome, scissoring, and hit geometry
        // never describe different layout revisions on screen.
        let phase_started = Instant::now();
        self.rebuild_display_list();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::SecondDisplayRebuild,
            phase_started.elapsed(),
        );
        if first_egui_frame {
            // Feature-label layout measures real egui fonts. Those fonts are initialized by the
            // first `run_ui` above, so the pre-shell rebuild must be skipped exactly once. The
            // post-shell rebuild now has valid font authority; request one immediate follow-up so
            // its genomic text joins the already coherent native geometry without waiting for an
            // unrelated input event.
            self.window.request_redraw();
        }
        let prepared_stats = std::mem::take(&mut self.display_upload_pending_metrics)
            .then(|| self.rectangle_renderer.last_prepare_stats());
        self.submit_ui_frame(full_output, prepared_stats, frame_started, repaint_delay);
        // Foreground pixels reach the surface before any speculative upload begins. Each
        // bounded quantum revalidates the currently published authority and schedules another
        // redraw only while useful pages remain.
        let prefetch = alignment_scene_post_submit_prefetch(
            self.alignment_gpu_publication_retry.is_some(),
            self.alignment_gpu_prefetch.reads,
            self.alignment_gpu_prefetch.summaries,
        );
        if prefetch {
            let phase_started = Instant::now();
            self.advance_current_alignment_gpu_prefetch();
            self.diagnostics.frame_phase(
                ManualDiagnosticFramePhase::Prefetch,
                phase_started.elapsed(),
            );
        }
        self.finish_manual_diagnostic_frame(frame_started.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::{alignment_scene_post_submit_prefetch, alignment_scene_prefetch_pending};

    #[test]
    fn collapsed_reads_do_not_stall_summary_only_prefetch() {
        assert!(alignment_scene_prefetch_pending(false, true));
        assert!(alignment_scene_prefetch_pending(true, false));
        assert!(!alignment_scene_prefetch_pending(false, false));
    }

    #[test]
    fn deferred_publication_suppresses_prefetch_without_a_post_submit_retry_loop() {
        assert!(!alignment_scene_post_submit_prefetch(true, true, true));
        assert!(alignment_scene_post_submit_prefetch(false, true, false));
    }
}
