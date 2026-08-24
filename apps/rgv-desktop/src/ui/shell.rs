use super::super::*;

impl Viewer {
    /// Poll, prepare, compose, and submit one application frame.
    pub(crate) fn render(&mut self) {
        let frame_started = Instant::now();
        self.poll_background_work();
        self.apply_captured_motion();

        self.rebuild_display_list();
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let (full_output, repaint_delay) = self.render_shell_frame(raw_input);
        let prepared_stats = std::mem::take(&mut self.display_upload_pending_metrics)
            .then(|| self.rectangle_renderer.last_prepare_stats());
        self.submit_ui_frame(full_output, prepared_stats, frame_started, repaint_delay);
    }
}
