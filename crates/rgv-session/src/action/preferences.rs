//! The user's preferences as an action.
//!
//! The Settings window and the automation protocol's preference settings
//! (`read_browsing_buffer_percent`, `total_cache_budget_mib`, `track_pipeline`) phrase a change
//! as [`Action::SetPreferences`]: the complete preferences afterwards. The host applies the
//! live effect of each changed preference (cache budget, theme, autosave schedule, pipeline)
//! and saves the settings file.
//!
//! Preferences are not an undo step. They belong to the installation, not to the document:
//! they are written to the settings file at once and apply to every session, so undoing them
//! with the document's edits would let Cmd+Z in one project silently change another project's
//! cache budget or theme.

use rgv_core::{DesktopSettings, InterfaceTheme, TrackPipeline};

/// The preferences the Settings window edits.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub cache_budget_mib: usize,
    pub read_browsing_buffer_percent: u16,
    pub diagnostics_opt_in: bool,
    pub autosave_enabled: bool,
    pub autosave_interval_seconds: u64,
    pub autosave_retention: usize,
    pub interface_scale_percent: u16,
    pub high_contrast: bool,
    pub theme: InterfaceTheme,
    pub reduced_motion: bool,
    pub coverage_cache_enabled: bool,
    pub track_pipeline: TrackPipeline,
}

impl Preferences {
    pub const fn of(settings: &DesktopSettings) -> Self {
        Self {
            cache_budget_mib: settings.cache_budget_mib,
            read_browsing_buffer_percent: settings.read_browsing_buffer_percent,
            diagnostics_opt_in: settings.diagnostics_opt_in,
            autosave_enabled: settings.autosave_enabled,
            autosave_interval_seconds: settings.autosave_interval_seconds,
            autosave_retention: settings.autosave_retention,
            interface_scale_percent: settings.interface_scale_percent,
            high_contrast: settings.high_contrast,
            theme: settings.theme,
            reduced_motion: settings.reduced_motion,
            coverage_cache_enabled: settings.coverage_cache_enabled,
            track_pipeline: settings.track_pipeline,
        }
    }

    pub const fn write_to(self, settings: &mut DesktopSettings) {
        settings.cache_budget_mib = self.cache_budget_mib;
        settings.read_browsing_buffer_percent = self.read_browsing_buffer_percent;
        settings.diagnostics_opt_in = self.diagnostics_opt_in;
        settings.autosave_enabled = self.autosave_enabled;
        settings.autosave_interval_seconds = self.autosave_interval_seconds;
        settings.autosave_retention = self.autosave_retention;
        settings.interface_scale_percent = self.interface_scale_percent;
        settings.high_contrast = self.high_contrast;
        settings.theme = self.theme;
        settings.reduced_motion = self.reduced_motion;
        settings.coverage_cache_enabled = self.coverage_cache_enabled;
        settings.track_pipeline = self.track_pipeline;
    }

    /// The autosave timer must be rescheduled.
    pub const fn autosave_schedule_differs(&self, other: &Self) -> bool {
        self.autosave_enabled != other.autosave_enabled
            || self.autosave_interval_seconds != other.autosave_interval_seconds
    }

    /// The interface scale, contrast, theme or motion changed.
    pub fn accessibility_differs(&self, other: &Self) -> bool {
        self.interface_scale_percent != other.interface_scale_percent
            || self.high_contrast != other.high_contrast
            || self.theme != other.theme
            || self.reduced_motion != other.reduced_motion
    }
}
