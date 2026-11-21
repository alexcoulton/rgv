//! Annotation, signal, variant and reference track display settings as actions.
//!
//! The track settings window, the metadata window's bulk commands and the automation protocol
//! phrase a change to an annotation or signal track as [`Action::SetTrackConfigs`] (the complete
//! configuration each named track should have afterwards), a rename of an alignment track as
//! [`Action::SetAlignmentTrackLabels`], and a reordering of the track list as
//! [`Action::MoveTrack`]. This module applies them to the session and returns their inverses;
//! the host decides which queries and displays to refresh from what changed.

use super::Action;
use crate::Session;
use crate::panels::Panel;
use anyhow::Result;
use rgv_core::{
    GeneticCode, ReferenceTranslationFrame, TrackConfig, TrackId, TrackKind, TrackSource,
};
use std::fmt::Write as _;

/// An alignment track's display name and sample association.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlignmentTrackLabel {
    pub track_id: TrackId,
    pub name: String,
    pub sample_key: Option<String>,
}

/// How the reference sequence lane is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferencePresentation {
    pub reverse_complement: bool,
    /// Sorted and without repeats.
    pub translation_frames: Vec<ReferenceTranslationFrame>,
    pub genetic_code: GeneticCode,
}

impl ReferencePresentation {
    /// The presentation as written: frames sorted, repeats dropped.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.translation_frames.sort();
        self.translation_frames.dedup();
        self
    }
}

/// One annotation or signal track whose configuration an action changed.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackConfigChange {
    pub before: TrackConfig,
    pub after: TrackConfig,
}

impl TrackConfigChange {
    /// The variant filters or sample selection changed: the variant query must run again.
    pub fn variant_query_changed(&self) -> bool {
        self.before.variant != self.after.variant
    }

    /// The interaction score floor changed: the interaction query must run again.
    pub fn interaction_query_changed(&self) -> bool {
        self.before.interaction.minimum_score != self.after.interaction.minimum_score
    }

    /// The copy-number sort changed: the copy-number query must run again.
    pub fn copy_number_query_changed(&self) -> bool {
        self.before.copy_number.sort_mode != self.after.copy_number.sort_mode
            || self.before.copy_number.sort_locus != self.after.copy_number.sort_locus
    }
}

/// The gesture a configuration edit may be one sample of (see [`continuous_edit_key`]).
pub fn track_configs_coalesce_key(changes: &[TrackConfigChange]) -> Option<String> {
    continuous_edit_key(
        "track-configs",
        changes.iter().map(|change| {
            (
                change.after.id.get(),
                serde_json::to_value(&change.before).unwrap_or_default(),
                serde_json::to_value(&change.after).unwrap_or_default(),
            )
        }),
    )
}

/// The configurations that put `changes` back.
pub fn track_configs_inverse(changes: &[TrackConfigChange]) -> Option<Action> {
    (!changes.is_empty()).then(|| Action::SetTrackConfigs {
        tracks: changes.iter().map(|change| change.before.clone()).collect(),
    })
}

/// What makes a track the track it is; an action may restyle it, never replace it.
fn identity(track: &TrackConfig) -> (TrackId, TrackKind, &TrackSource) {
    (track.id, track.kind, &track.source)
}

impl<P: Panel> Session<P> {
    /// Applies [`Action::SetTrackConfigs`]: each named annotation or signal track takes the
    /// given configuration. Every track is validated before anything changes; a configuration
    /// may not change a track's kind or source. Returns the tracks that changed.
    pub fn apply_track_configs(
        &mut self,
        tracks: &[TrackConfig],
    ) -> Result<Vec<TrackConfigChange>> {
        for track in tracks {
            let current = self
                .tracks
                .tracks()
                .iter()
                .find(|existing| existing.id == track.id)
                .ok_or_else(|| anyhow::anyhow!("track {} was not found", track.id.get()))?;
            anyhow::ensure!(
                identity(current) == identity(track),
                "track {} cannot change its kind or source",
                track.id.get()
            );
        }
        let mut changes = Vec::new();
        for track in tracks {
            let current = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|existing| existing.id == track.id)
                .expect("validated above");
            if current != track {
                changes.push(TrackConfigChange {
                    before: std::mem::replace(current, track.clone()),
                    after: track.clone(),
                });
            }
        }
        Ok(changes)
    }

    /// An alignment track's label as the session document saves it.
    pub fn alignment_track_label(&self, track_id: TrackId) -> Option<AlignmentTrackLabel> {
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .map(|track| AlignmentTrackLabel {
                track_id,
                name: track.config.name.clone(),
                sample_key: track.config.sample_key.clone(),
            })
    }

    /// Applies [`Action::SetAlignmentTrackLabels`]. Returns the labels that changed, as they
    /// were before.
    pub fn apply_alignment_track_labels(
        &mut self,
        labels: &[AlignmentTrackLabel],
    ) -> Result<Vec<AlignmentTrackLabel>> {
        for label in labels {
            anyhow::ensure!(
                self.alignment_track_label(label.track_id).is_some(),
                "alignment track {} was not found",
                label.track_id.get()
            );
        }
        let mut previous = Vec::new();
        for label in labels {
            let before = self
                .alignment_track_label(label.track_id)
                .expect("validated above");
            if before == *label {
                continue;
            }
            let track = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == label.track_id)
                .expect("validated above");
            track.config.name.clone_from(&label.name);
            track.config.sample_key.clone_from(&label.sample_key);
            previous.push(before);
        }
        Ok(previous)
    }

    /// Applies [`Action::MoveTrack`]: moves an annotation or signal track to a position in the
    /// track list. Returns where it was, or `None` when there is no such track.
    pub fn move_auxiliary_track(&mut self, track_id: TrackId, index: usize) -> Option<usize> {
        let previous = self
            .tracks
            .tracks()
            .iter()
            .position(|track| track.id == track_id)?;
        self.tracks.move_track(track_id, index);
        Some(previous)
    }
}

/// Collects the paths of the values that differ between two documents, and whether every
/// difference is a number or a string (what sliders, drag values and text fields edit).
fn changed_leaves(
    left: &serde_json::Value,
    right: &serde_json::Value,
    path: &mut String,
    leaves: &mut Vec<String>,
) -> bool {
    use serde_json::Value;
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let mut continuous = true;
            for (key, value) in left {
                let length = path.len();
                path.push('.');
                path.push_str(key);
                continuous &=
                    changed_leaves(value, right.get(key).unwrap_or(&Value::Null), path, leaves);
                path.truncate(length);
            }
            for key in right.keys().filter(|key| !left.contains_key(*key)) {
                leaves.push(format!("{path}.{key}"));
                continuous = false;
            }
            continuous
        }
        (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
            let mut continuous = true;
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                let length = path.len();
                let _ = write!(path, "[{index}]");
                continuous &= changed_leaves(left, right, path, leaves);
                path.truncate(length);
            }
            continuous
        }
        _ if left == right => true,
        (Value::Number(_), Value::Number(_)) | (Value::String(_), Value::String(_)) => {
            leaves.push(path.clone());
            true
        }
        _ => {
            leaves.push(path.clone());
            false
        }
    }
}

/// The gesture an edit of these documents may be one sample of: a slider drag or typing in one
/// field changes the same numbers or text every frame and is one undo step. A toggle, a choice
/// that adds or removes entries, or an edit of other fields is a step of its own.
pub fn continuous_edit_key(
    kind: &str,
    edits: impl IntoIterator<Item = (u64, serde_json::Value, serde_json::Value)>,
) -> Option<String> {
    let mut key = kind.to_owned();
    let mut any = false;
    for (id, before, after) in edits {
        let mut leaves = Vec::new();
        if !changed_leaves(&before, &after, &mut String::new(), &mut leaves) {
            return None;
        }
        let _ = write!(key, ":{id}");
        for leaf in leaves {
            key.push_str(&leaf);
            key.push(',');
        }
        any = true;
    }
    any.then_some(key)
}
