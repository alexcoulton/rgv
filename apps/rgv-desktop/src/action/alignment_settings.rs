//! The desktop half of `Action::SetAlignmentSettings`: the session writes the settings, this
//! shell runs the refresh each changed field needs.
//!
//! The refresh is chosen per field, exactly as the settings window chose it before the action
//! existed, so the window, the gutter toggles, undo and automation all produce the same scene
//! contract change:
//!
//! - a query field (base quality, mate policy, filter clauses, view filter, read limit, …) asks
//!   the source again. For the primary track that is the full query-options refresh the window
//!   always ran; another track advances its own scene generations and re-requests alignments;
//! - on the primary track a read arrangement or colouring change also re-runs the query;
//! - every other field invalidates the navigation contract and rebuilds the displays, which
//!   keeps the incumbent scene until its replacement settles.

use crate::Viewer;
use crate::action::{Action, ActionOutcome};
use crate::display::alignment::persisted_style_tag;
use crate::workspace::alignments::AlignmentTrackLifecycle;
use anyhow::Result;
use rgv_core::TrackId;
use rgv_session::action::AlignmentSettings;
use rgv_session::action::alignment_settings::AlignmentSettingsApplied;

impl Viewer {
    /// Gives alignment tracks new settings through the action path, so the edit is undoable.
    /// The entry point the window, the gutter and the bulk commands share.
    pub(crate) fn set_alignment_settings(
        &mut self,
        updates: Vec<(TrackId, AlignmentSettings)>,
    ) -> Result<ActionOutcome> {
        self.apply_action(Action::SetAlignmentSettings {
            updates,
            workspace_limit: None,
        })
    }

    /// The implementation behind `Action::SetAlignmentSettings`; only the action path calls it.
    /// Returns the outcome, the inverse and the slider-drag coalescing key.
    pub(super) fn perform_alignment_settings(
        &mut self,
        updates: &[(TrackId, AlignmentSettings)],
        workspace_limit: Option<usize>,
    ) -> Result<(ActionOutcome, Vec<Action>, Option<String>)> {
        let applied = self
            .session
            .apply_alignment_settings(updates, workspace_limit)?;
        self.run_alignment_settings_effects(&applied);
        Ok((
            ActionOutcome {
                changed: applied.changed(),
                ..ActionOutcome::default()
            },
            applied.inverse().into_iter().collect(),
            applied.coalesce_key(),
        ))
    }

    fn run_alignment_settings_effects(&mut self, applied: &AlignmentSettingsApplied) {
        let mut refresh_query_options = false;
        let mut request_alignments = false;
        let mut query_changed = false;
        let mut presentation_changed = false;
        let mut sashimi_presentation_changed = false;
        for change in &applied.changes {
            let track_id = change.track_id;
            let query = change.query_changed();
            {
                // Selections that pointed into a lane the change hid or rebuilt.
                let panel = self.session.panels.active_mut();
                if panel
                    .selected_alignment_track
                    .is_none_or(|selected| selected == track_id)
                {
                    if change.turned_off("show_coverage") {
                        panel.selected_pileup_position = None;
                    }
                    if change.turned_off("show_junctions") {
                        panel.selected_junction = None;
                        panel.selected_junction_component = None;
                    }
                    if change.changed("show_modification_frequency") {
                        panel.selected_modification_position = None;
                    }
                }
                if change.changed("inspector_activation") {
                    panel.hover_inspector_target = None;
                }
            }
            if change.changed("alignment_density") {
                // A new track density replaces every panel's own density for the track.
                self.set_alignment_density_for_track(
                    track_id,
                    change.after.filters.alignment_density,
                );
                self.invalidate_navigation_contract("read density changed");
            }
            if change.changed("alignment_highlight_filter") {
                self.refresh_alignment_highlight_interactions(&[track_id]);
            }
            if change.primary
                && [
                    "alignment_color_mode",
                    "alignment_group_mode",
                    "alignment_sort_mode",
                ]
                .iter()
                .any(|field| change.changed(field))
            {
                let filters = &change.after.filters;
                self.style_tag_editor = persisted_style_tag(
                    filters.alignment_color_mode,
                    filters.alignment_group_mode,
                    filters.alignment_sort_mode,
                );
            }
            presentation_changed |= change.presentation_changed();
            sashimi_presentation_changed |= change.sashimi_presentation_changed();
            query_changed |= query;
            if change.primary {
                refresh_query_options |= query || change.style_changed();
            } else if query {
                if let Some(track) = self
                    .session
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track.config.id == track_id)
                    && !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                {
                    for projection in track.projections.values_mut() {
                        projection.advance_scene_generation();
                    }
                }
                request_alignments = true;
            }
        }
        if presentation_changed {
            self.invalidate_navigation_contract("alignment presentation changed");
        }
        if query_changed {
            self.automation_alignment_handles.clear();
            self.mark_retained_evidence_stale(
                "alignment query settings changed after this evidence was captured",
            );
        }
        if refresh_query_options {
            self.query_options_changed();
        }
        if request_alignments {
            self.session.panels.active_mut().pending_queries.alignment = true;
            self.request_all_alignment_queries();
        }
        if applied.changed() {
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
        }
        if sashimi_presentation_changed {
            self.prepare_alignment_scene_frame();
        }
    }
}
