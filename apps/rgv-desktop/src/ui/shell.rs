use super::super::*;

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
        let frame_started = Instant::now();
        self.poll_background_work();
        self.apply_captured_motion();

        self.rebuild_display_list();
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let (full_output, repaint_delay) = self.render_shell_frame(raw_input);
        // Shell layout is resolved by egui. If it changed the committed
        // workspace geometry, rebuild before submitting this same frame so
        // native GPU primitives, egui chrome, scissoring, and hit geometry
        // never describe different layout revisions on screen.
        self.rebuild_display_list();
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
            self.advance_current_alignment_gpu_prefetch();
        }
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
