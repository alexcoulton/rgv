//! Thin `Viewer` wiring for the policies in `shell_state`.

use std::path::PathBuf;
use std::time::Instant;

use rgv_core::{Locus, LocusRange, PanelId, TrackId};

use super::shell_state::{
    EscapeAction, EscapeContext, GateOutcome, GuardedAction, NotificationSeverity, ScrollIntent,
    ScrollModifiers, UnsavedChoice, escape_action, interpret_scroll, pinch_zoom_factor,
};
use super::tasks::SessionLoadKind;
use crate::{UiPanel, Viewer};
use rgv_engine::TrackSearchResults;

/// Whether the annotation-search popup is shown under the locus field.
pub(crate) fn search_popup_visible(
    locus_editor: &str,
    results_query: &str,
    has_hits: bool,
) -> bool {
    let typed = locus_editor.trim();
    // A coordinate locus navigates; a colon alone does not make one, so
    // variant IDs such as `chr1:12345:A:G` keep their search results.
    let coordinates = Locus::parse(typed).is_ok_and(|locus| locus.range != LocusRange::WholeContig);
    has_hits
        && !coordinates
        && !typed.is_empty()
        && results_query.trim().eq_ignore_ascii_case(typed)
}

impl Viewer {
    // ----- Escape (UI-1) ---------------------------------------------------------------------

    /// Handles a pressed Escape key. Returns whether anything changed. Never exits.
    pub(crate) fn escape_pressed(&mut self, repeat: bool, egui_consumed: bool) -> bool {
        let action = escape_action(EscapeContext {
            repeat,
            egui_consumed,
            context_menu_open: self.alignment_context_menu.is_some(),
            unsaved_prompt_open: self.shell.unsaved_gate.prompt().is_some(),
            exit_prompt_open: self.exit_confirmation_open,
            search_popup_open: search_popup_visible(
                &self.locus_editor,
                &self.annotation_search_results.query,
                !self.annotation_search_results.hits.is_empty(),
            ),
            command_palette_open: self.open_panel == Some(UiPanel::CommandPalette),
            notifications_open: self.shell.notifications_open,
            other_panel_open: self
                .open_panel
                .is_some_and(|panel| panel != UiPanel::CommandPalette),
        });
        match action {
            EscapeAction::Nothing => return self.clear_active_sashimi_filters(),
            EscapeAction::CloseContextMenu => {
                self.close_alignment_context_menu("escape");
            }
            EscapeAction::CancelUnsavedPrompt => {
                self.shell.unsaved_gate.resolve(UnsavedChoice::Cancel);
            }
            EscapeAction::CloseExitPrompt => self.exit_confirmation_open = false,
            EscapeAction::CloseSearchPopup => {
                self.annotation_search_results = TrackSearchResults::default();
                self.pending_annotation_navigation = None;
            }
            EscapeAction::CloseNotifications => {
                self.shell.notifications_open = false;
            }
            EscapeAction::ClosePanel => {
                self.open_panel = None;
                self.command_search.clear();
            }
        }
        true
    }

    // ----- Unsaved-changes gate (APP-1) ------------------------------------------------------

    fn request_guarded(&mut self, action: GuardedAction) -> Option<GuardedAction> {
        self.observe_document_changes();
        let dirty = self.session.document_state.is_dirty();
        match self.shell.unsaved_gate.request(action, dirty) {
            GateOutcome::Proceed(action) => Some(action),
            GateOutcome::Prompt | GateOutcome::SaveFirst | GateOutcome::Stay => {
                self.exit_confirmation_open = false;
                self.window.request_redraw();
                None
            }
        }
    }

    /// The window close button. Returns whether the application may exit immediately; otherwise
    /// the Save / Discard / Cancel prompt has been opened.
    pub(crate) fn request_exit(&mut self) -> bool {
        self.request_guarded(GuardedAction::Exit).is_some()
    }

    /// Cmd/Ctrl+W. Unsaved changes open the Save / Discard / Cancel prompt; a clean document
    /// keeps the press-again confirmation. Returns whether the application may exit now.
    pub(crate) fn request_exit_from_shortcut(&mut self) -> bool {
        if self.shell.unsaved_gate.prompt().is_some() {
            // The prompt is already asking; a repeated shortcut must not discard the document.
            return false;
        }
        if self.request_guarded(GuardedAction::Exit).is_none() {
            return false;
        }
        if self.exit_confirmation_open {
            true
        } else {
            self.exit_confirmation_open = true;
            false
        }
    }

    /// Every GUI path that replaces the document with a session file (open, recent, recover,
    /// drag and drop) goes through here.
    pub(crate) fn request_session_load(&mut self, path: PathBuf, kind: SessionLoadKind) {
        if let Some(action) = self.request_guarded(GuardedAction::LoadSession { path, kind }) {
            self.run_guarded_action(action);
        }
    }

    fn run_guarded_action(&mut self, action: GuardedAction) {
        match action {
            GuardedAction::Exit => self.user_exit_requested = true,
            GuardedAction::LoadSession { path, kind } => self.start_session_load(path, kind),
        }
        self.window.request_redraw();
    }

    pub(crate) fn resolve_unsaved_choice(&mut self, choice: UnsavedChoice) {
        match self.shell.unsaved_gate.resolve(choice) {
            GateOutcome::Proceed(action) => self.run_guarded_action(action),
            GateOutcome::SaveFirst => {
                let destination = self.session.current_session_path.clone().or_else(|| {
                    rfd::FileDialog::new()
                        .set_file_name("rgv-project.json")
                        .add_filter("RGV project", &["json"])
                        .save_file()
                });
                let started =
                    destination.is_some_and(|path| match self.try_start_session_save(path, None) {
                        Ok(()) => true,
                        Err(error) => {
                            self.report_error(format!("Could not start project save: {error}"));
                            false
                        }
                    });
                if !started {
                    self.shell.unsaved_gate.save_abandoned();
                }
            }
            GateOutcome::Prompt | GateOutcome::Stay => {}
        }
        self.window.request_redraw();
    }

    /// Called when a manual project save finishes; resumes the action that was waiting for it.
    pub(crate) fn unsaved_gate_save_finished(&mut self, succeeded: bool) {
        if !self.shell.unsaved_gate.awaiting_save() {
            return;
        }
        let still_dirty = self.session.document_state.is_dirty();
        if let GateOutcome::Proceed(action) = self
            .shell
            .unsaved_gate
            .save_finished(succeeded, still_dirty)
        {
            self.run_guarded_action(action);
        }
        self.window.request_redraw();
    }

    // ----- One history entry per gesture (UI-3) ----------------------------------------------

    /// Shows the active viewport in the locus field without recording a recent locus.
    pub(crate) fn sync_locus_editor(&mut self) {
        let panel = self.session.panels.active();
        let interval = panel.viewport.interval();
        self.locus_editor = format!(
            "{}:{}-{}",
            panel.contig_name,
            interval.start() + 1,
            interval.end()
        );
    }

    /// One step of a continuous navigation gesture happened in the active panel.
    pub(crate) fn note_navigation_gesture(&mut self, panel: PanelId) {
        if let Some(previous) = self.session.navigation_gesture.touch(panel, Instant::now()) {
            self.record_panel_navigation(previous);
        }
    }

    /// Records the pending gesture, if any, as one history entry and one recent locus. Called
    /// before every discrete navigation so the gesture's end point is not lost.
    pub(crate) fn commit_navigation_gesture(&mut self) {
        if let Some(panel) = self.session.navigation_gesture.take() {
            self.record_panel_navigation(panel);
        }
    }

    /// The pointer or key driving the gesture was released.
    pub(crate) fn end_navigation_gesture(&mut self) {
        let now = Instant::now();
        self.session.navigation_gesture.end_now(now);
        self.commit_due_navigation_gesture(now);
    }

    pub(crate) fn commit_due_navigation_gesture(&mut self, now: Instant) {
        let pointer_gesture_active = self.dragging
            || self.horizontal_overview_drag.is_some()
            || self.overview_capture.is_some();
        if let Some(panel) = self
            .session
            .navigation_gesture
            .take_due(now, pointer_gesture_active)
        {
            self.record_panel_navigation(panel);
        }
    }

    pub(crate) fn navigation_gesture_deadline(&self) -> Option<Instant> {
        self.session.navigation_gesture.deadline()
    }

    fn record_panel_navigation(&mut self, panel_id: PanelId) {
        let Some(panel) = self.session.panels.get_mut(panel_id) else {
            return;
        };
        let interval = panel.viewport.interval();
        panel.visit_history(interval);
        let locus = format!(
            "{}:{}-{}",
            panel.contig_name,
            interval.start() + 1,
            interval.end()
        );
        self.session.settings.remember_locus(locus);
    }

    // ----- Wheel, trackpad and pinch (UI-2) --------------------------------------------------

    /// Applies one wheel or two-finger scroll event. Returns whether it navigated horizontally
    /// (zoom or pan), so the caller can settle the gesture when the touch phase ends.
    pub(crate) fn apply_scroll(&mut self, delta: winit::event::MouseScrollDelta) -> bool {
        let intent = interpret_scroll(
            delta,
            self.window.scale_factor(),
            ScrollModifiers {
                shift: self.modifiers.shift_key(),
                primary: self.modifiers.control_key() || self.modifiers.super_key(),
            },
            self.cursor_in_genomic_canvas(),
        );
        match intent {
            ScrollIntent::Zoom { factor } => {
                self.zoom_at_cursor(factor);
                true
            }
            ScrollIntent::Pan { points } => {
                if let Ok(viewport) = self
                    .session
                    .panels
                    .active()
                    .viewport
                    .pan_pixels(points, self.session.panels.active().contig_length)
                {
                    self.set_panel_viewport(
                        self.session.panels.active().id,
                        viewport,
                        crate::action::ViewportCommit::Gesture,
                    );
                }
                true
            }
            ScrollIntent::ScrollTracks { points } => {
                self.scroll_tracks_points(points);
                false
            }
            ScrollIntent::Ignore => false,
        }
    }

    /// Applies one step of a trackpad magnification gesture around the cursor.
    pub(crate) fn apply_pinch(&mut self, delta: f64) {
        if let Some(factor) = pinch_zoom_factor(delta) {
            self.zoom_at_cursor(factor);
        }
    }

    // ----- Track ids (APP-14) ----------------------------------------------------------------

    /// A track id that has not been used before in this run, even by a track since removed:
    /// worker events for a removed track may still be in flight.
    pub(crate) fn allocate_track_id(&mut self) -> TrackId {
        self.session.allocate_track_id()
    }

    // ----- Notifications (APP-7) -------------------------------------------------------------

    /// Reports a failure: shown on the status line and kept in the notification log, where a
    /// later progress message cannot overwrite it.
    pub(crate) fn report_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.session
            .notifications
            .push_reported(NotificationSeverity::Error, None, &message);
        self.session.data_status = message;
        self.window.request_redraw();
    }

    /// Reports something the user should know that is not a failure: shown on the status line
    /// and kept in the notification log as a warning.
    pub(crate) fn report_warning(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.session
            .notifications
            .push_reported(NotificationSeverity::Warning, None, &message);
        self.session.data_status = message;
        self.window.request_redraw();
    }

    /// Records a failure of one track (or of the track engine as a whole) without touching the
    /// status line. Used for failures in panels other than the active one, which previously
    /// left no trace at all.
    pub(crate) fn log_track_error(&mut self, track: Option<TrackId>, message: &str) {
        self.session
            .notifications
            .push_reported(NotificationSeverity::Error, track, message);
        self.window.request_redraw();
    }

    /// Routes the current status line into the notification log when it reports a failure.
    /// Called after every batch of work that may have written `data_status`, so failures from
    /// call sites that only set the status text are still retained.
    pub(crate) fn capture_status_notification(&mut self) {
        if self
            .session
            .notifications
            .observe_status(&self.session.data_status)
        {
            self.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_popup_is_visible_only_for_matching_non_empty_results() {
        assert!(!search_popup_visible("TP53", "tp53", false), "no hits");
        assert!(search_popup_visible(" TP53 ", "tp53", true));
        assert!(!search_popup_visible("TP5", "tp53", true), "stale results");
        assert!(!search_popup_visible("chr1:1-2", "chr1:1-2", true));
        assert!(search_popup_visible(
            "chr1:12345:A:G",
            "chr1:12345:a:g",
            true
        ));
        assert!(!search_popup_visible("  ", "", true));
    }
}
