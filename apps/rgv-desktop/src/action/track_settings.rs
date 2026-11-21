//! The desktop half of the annotation, signal, alignment-label and reference display actions:
//! the session writes the configuration, this shell runs the refresh the settings window always
//! ran for the same change.

use crate::action::{Action, ActionOutcome, AlignmentTrackLabel, ReferencePresentation};
use crate::{Viewer, auxiliary_query_track_ids};
use anyhow::Result;
use rgv_core::{TrackConfig, TrackId};
use rgv_session::action::track_settings::{
    continuous_edit_key, track_configs_coalesce_key, track_configs_inverse,
};
use std::collections::HashSet;

type Performed = (ActionOutcome, Vec<Action>, Option<String>);

fn changed(changed: bool) -> ActionOutcome {
    ActionOutcome {
        changed,
        ..ActionOutcome::default()
    }
}

impl Viewer {
    /// The reference lane's presentation as the session document saves it.
    pub(crate) fn reference_presentation(&self) -> ReferencePresentation {
        ReferencePresentation {
            reverse_complement: self.reference_reverse_complement,
            translation_frames: self.reference_translation_frames.clone(),
            genetic_code: self.reference_genetic_code,
        }
    }

    pub(super) fn perform_track_configs(&mut self, tracks: &[TrackConfig]) -> Result<Performed> {
        let membership_before = auxiliary_query_track_ids(self.session.tracks.tracks())
            .into_iter()
            .collect::<HashSet<_>>();
        let changes = self.session.apply_track_configs(tracks)?;
        if !changes.is_empty() {
            self.invalidate_navigation_contract("auxiliary track presentation changed");
            self.reconcile_track_presentation();
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
            let membership_after = auxiliary_query_track_ids(self.session.tracks.tracks())
                .into_iter()
                .collect::<HashSet<_>>();
            if membership_before != membership_after {
                self.invalidate_and_request_all_auxiliary_track_queries();
            } else if changes.iter().any(|change| {
                change.variant_query_changed()
                    || change.interaction_query_changed()
                    || change.copy_number_query_changed()
            }) {
                self.request_all_auxiliary_track_queries();
            }
        }
        Ok((
            changed(!changes.is_empty()),
            track_configs_inverse(&changes).into_iter().collect(),
            track_configs_coalesce_key(&changes),
        ))
    }

    pub(super) fn perform_alignment_track_labels(
        &mut self,
        labels: &[AlignmentTrackLabel],
    ) -> Result<Performed> {
        let previous = self.session.apply_alignment_track_labels(labels)?;
        if !previous.is_empty() {
            self.invalidate_navigation_contract("alignment track layout changed");
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
        }
        // Typing a name renames once per keystroke; one pause-free burst is one step.
        let coalesce = continuous_edit_key(
            "alignment-labels",
            previous.iter().map(|before| {
                let after = labels
                    .iter()
                    .find(|label| label.track_id == before.track_id)
                    .expect("a changed label was requested");
                (
                    before.track_id.get(),
                    serde_json::json!([before.name, before.sample_key]),
                    serde_json::json!([after.name, after.sample_key]),
                )
            }),
        );
        Ok((
            changed(!previous.is_empty()),
            if previous.is_empty() {
                Vec::new()
            } else {
                vec![Action::SetAlignmentTrackLabels { labels: previous }]
            },
            coalesce,
        ))
    }

    pub(super) fn perform_track_move(&mut self, track_id: TrackId, index: usize) -> Performed {
        let Some(previous) = self.session.move_auxiliary_track(track_id, index) else {
            return (changed(false), Vec::new(), None);
        };
        // The settings window has always refreshed on a move request, moved or not.
        self.invalidate_navigation_contract("auxiliary track presentation changed");
        self.reconcile_track_presentation();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        let moved = self
            .session
            .tracks
            .tracks()
            .iter()
            .position(|track| track.id == track_id)
            != Some(previous);
        (
            changed(moved),
            if moved {
                vec![Action::MoveTrack {
                    track_id,
                    index: previous,
                }]
            } else {
                Vec::new()
            },
            None,
        )
    }

    pub(super) fn perform_reference_presentation(
        &mut self,
        presentation: &ReferencePresentation,
    ) -> Performed {
        let before = self.reference_presentation();
        let next = presentation.clone().normalized();
        if before == next {
            return (changed(false), Vec::new(), None);
        }
        self.reference_reverse_complement = next.reverse_complement;
        self.reference_translation_frames = next.translation_frames;
        self.reference_genetic_code = next.genetic_code;
        self.invalidate_navigation_contract("reference presentation changed");
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        (
            changed(true),
            vec![Action::SetReferencePresentation(before)],
            None,
        )
    }
}
