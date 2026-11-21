//! The desktop half of `Action::SetPreferences`: each changed preference takes its live effect
//! (cache budget, coverage cache, track pipeline, theme and accessibility, autosave schedule)
//! and the settings file is saved. Not an undo step; see `rgv_session::action::preferences`.

use crate::action::{ActionOutcome, Preferences};
use crate::{Viewer, apply_accessibility_preferences};

impl Viewer {
    pub(super) fn perform_preferences(&mut self, next: Preferences) -> ActionOutcome {
        let before = Preferences::of(&self.session.settings);
        if before == next {
            return ActionOutcome::default();
        }
        next.write_to(&mut self.session.settings);
        if before.coverage_cache_enabled != next.coverage_cache_enabled {
            self.apply_coverage_cache_setting();
        }
        if before.track_pipeline != next.track_pipeline {
            self.apply_track_pipeline_setting();
        }
        if before.accessibility_differs(&next) {
            apply_accessibility_preferences(&self.egui_context, &self.session.settings);
            self.apply_theme_to_renderers();
            self.mark_display_dirty("explicit display invalidation");
        }
        if before.autosave_schedule_differs(&next) {
            self.schedule_next_autosave();
        }
        let cache_budget_changed = before.cache_budget_mib != next.cache_budget_mib;
        if cache_budget_changed {
            self.apply_live_cache_budget(next.cache_budget_mib);
        }
        if cache_budget_changed
            || before.read_browsing_buffer_percent != next.read_browsing_buffer_percent
        {
            self.request_all_panel_queries();
        }
        self.persist_settings();
        ActionOutcome {
            changed: true,
            ..ActionOutcome::default()
        }
    }
}
