//! Alignment track display and query settings as one action.
//!
//! The settings window, the gutter toggles, the bulk "copy appearance" commands and the
//! automation protocol (`set_option`, `configure`, `group`, `sort`, `filter`) all phrase an
//! alignment settings change as [`Action::SetAlignmentSettings`]: the complete settings each
//! named track should have afterwards, in the form the session document saves them. This module
//! applies that action to the session's alignment state, reports which fields changed so the
//! host can choose the refresh it owes, and returns the inverse (the settings it replaced).
//!
//! Applying an action is pure state: no engine is touched here. Which refresh a change needs
//! (a new scene query, a display rebuild, a highlight refresh) is decided by the host from
//! [`AlignmentSettingsChange`].

use super::Action;
use crate::Session;
use crate::alignment::track::{
    active_filters_from_session, alignment_color_mode_needs_reference,
    expanded_workspace_alignment_limit,
};
use crate::panels::Panel;
use anyhow::Result;
use rgv_core::{SessionFilters, TrackId};
use std::collections::BTreeSet;
use std::fmt::Write as _;

/// The id the primary alignment settings answer to while no alignment source is open (the
/// desktop's synthetic alignment track).
pub const EMPTY_ALIGNMENT_TRACK_ID: TrackId = TrackId::new(0);

/// The range of read-row density (`alignment_density`) the vertical navigator supports.
pub const MINIMUM_ALIGNMENT_DENSITY: f32 = 0.35;
pub const MAXIMUM_ALIGNMENT_DENSITY: f32 = 9.0;

/// One alignment track's settings as the session document saves them.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentSettings {
    pub filters: SessionFilters,
    /// The track's complete-read limit.
    pub maximum_alignments: usize,
}

/// Fields whose change asks the alignment source for different records: a new scene query.
const QUERY_FIELDS: &[&str] = &[
    "minimum_base_quality",
    "overlapping_mate_policy",
    "show_modification_frequency",
    "minimum_junction_flank",
    "minimum_mapping_quality",
    "include_secondary",
    "include_supplementary",
    "auxiliary_predicates",
    "alignment_view_filter",
    "maximum_alignments",
];

/// Read arrangement and colouring fields. The primary track re-runs its query for these, as the
/// settings window always has.
const STYLE_FIELDS: &[&str] = &[
    "alignment_color_mode",
    "alignment_group_mode",
    "alignment_group_order",
    "alignment_sort_mode",
    "alignment_sort_order",
    "alignment_sort_anchor",
    "read_stacking_mode",
    "read_sampling",
    "view_as_pairs",
    "minimum_template_length",
    "maximum_template_length",
    "alignment_highlight_filter",
];

/// Fields a slider or number field edits continuously. A drag over one of them reports a new
/// value every frame and is one undo step (see [`super::history::UNDO_COALESCE_WINDOW`]).
const CONTINUOUS_FIELDS: &[&str] = &[
    "minimum_base_quality",
    "minimum_allele_fraction",
    "minimum_junction_depth",
    "minimum_junction_flank",
    "sashimi_fixed_scale_maximum",
    "coverage_scale_maximum",
    "alignment_density",
    "minimum_indel_size",
    "minimum_event_label_size",
    "minimum_modification_probability",
    "minimum_template_length",
    "maximum_template_length",
    "maximum_alignments",
];

/// The filter clauses the "Apply filter" button commits; the filter editor mirrors them.
const FILTER_CLAUSE_FIELDS: &[&str] = &[
    "minimum_mapping_quality",
    "include_secondary",
    "include_supplementary",
    "auxiliary_predicates",
];

fn filter_fields(filters: &SessionFilters) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(filters) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

/// The names of the settings fields that differ, as the session document spells them.
/// `maximum_alignments` is included when the complete-read limit differs.
pub fn changed_alignment_fields(
    before: &AlignmentSettings,
    after: &AlignmentSettings,
) -> BTreeSet<String> {
    let left = filter_fields(&before.filters);
    let right = filter_fields(&after.filters);
    let mut fields = left
        .iter()
        .filter(|(key, value)| right.get(*key) != Some(value))
        .map(|(key, _)| key.clone())
        .chain(right.keys().filter(|key| !left.contains_key(*key)).cloned())
        .collect::<BTreeSet<_>>();
    if before.maximum_alignments != after.maximum_alignments {
        fields.insert("maximum_alignments".to_owned());
    }
    fields
}

/// `current` with every field the window's draft edited taken from `drafted`. A field the
/// draft did not change keeps its current value, so opening the settings window never
/// rewrites settings the user did not touch. `initial` is the draft as the frame began.
pub fn merge_alignment_settings_draft(
    current: &AlignmentSettings,
    initial: &AlignmentSettings,
    drafted: &AlignmentSettings,
) -> AlignmentSettings {
    let edited = changed_alignment_fields(initial, drafted);
    if edited.is_empty() {
        return current.clone();
    }
    let mut merged = filter_fields(&current.filters);
    let drafted_fields = filter_fields(&drafted.filters);
    for field in &edited {
        if let Some(value) = drafted_fields.get(field) {
            merged.insert(field.clone(), value.clone());
        }
    }
    let mut filters = serde_json::from_value::<SessionFilters>(serde_json::Value::Object(merged))
        .unwrap_or_else(|_| current.filters.clone());
    // Legacy field that is read but never written by the document.
    filters.show_reads = current.filters.show_reads;
    AlignmentSettings {
        filters,
        maximum_alignments: if edited.contains("maximum_alignments") {
            drafted.maximum_alignments
        } else {
            current.maximum_alignments
        },
    }
}

/// What one track's settings change was.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentSettingsChange {
    pub track_id: TrackId,
    /// The first alignment track (or the settings kept while none is open).
    pub primary: bool,
    pub before: AlignmentSettings,
    pub after: AlignmentSettings,
    /// Changed fields, as [`changed_alignment_fields`] names them.
    pub fields: BTreeSet<String>,
}

impl AlignmentSettingsChange {
    pub fn changed(&self, field: &str) -> bool {
        self.fields.contains(field)
    }

    /// The source must be asked again: the records, their admission or their derived summaries
    /// differ.
    pub fn query_changed(&self) -> bool {
        QUERY_FIELDS.iter().any(|field| self.changed(field))
            || alignment_color_mode_needs_reference(self.before.filters.alignment_color_mode)
                != alignment_color_mode_needs_reference(self.after.filters.alignment_color_mode)
            || self.before.filters.read_sampling.is_downsampled()
                != self.after.filters.read_sampling.is_downsampled()
    }

    /// Read arrangement, colouring or highlight changed.
    pub fn style_changed(&self) -> bool {
        STYLE_FIELDS.iter().any(|field| self.changed(field))
    }

    /// Anything beyond the hover-inspector trigger changed, so the track's lanes look different.
    pub fn presentation_changed(&self) -> bool {
        self.fields
            .iter()
            .any(|field| field.as_str() != "inspector_activation")
    }

    /// A boolean field went from shown to hidden.
    pub fn turned_off(&self, field: &str) -> bool {
        self.changed(field)
            && filter_fields(&self.after.filters).get(field)
                == Some(&serde_json::Value::Bool(false))
    }
}

/// The result of applying [`Action::SetAlignmentSettings`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlignmentSettingsApplied {
    /// One entry per track whose settings changed, in the order of the action.
    pub changes: Vec<AlignmentSettingsChange>,
    /// The workspace-wide complete-read limit before, when the action changed it.
    pub previous_workspace_limit: Option<usize>,
}

impl AlignmentSettingsApplied {
    pub fn changed(&self) -> bool {
        !self.changes.is_empty() || self.previous_workspace_limit.is_some()
    }

    /// The action that puts every changed track (and the workspace limit) back.
    pub fn inverse(&self) -> Option<Action> {
        self.changed().then(|| Action::SetAlignmentSettings {
            updates: self
                .changes
                .iter()
                .map(|change| (change.track_id, change.before.clone()))
                .collect(),
            workspace_limit: self.previous_workspace_limit,
        })
    }

    /// The gesture this change may be one sample of: a slider drag over the same continuous
    /// fields of the same tracks. Toggles and choices are complete edits and never coalesce.
    pub fn coalesce_key(&self) -> Option<String> {
        if self.changes.is_empty()
            || !self.changes.iter().all(|change| {
                change
                    .fields
                    .iter()
                    .all(|field| CONTINUOUS_FIELDS.contains(&field.as_str()))
            })
        {
            return None;
        }
        let mut key = "alignment-settings".to_owned();
        for change in &self.changes {
            let _ = write!(key, ":{}", change.track_id.get());
            for field in &change.fields {
                let _ = write!(key, ",{field}");
            }
        }
        Some(key)
    }
}

/// Settings as written: values the controls cannot express are brought into range.
fn normalized(mut settings: AlignmentSettings) -> AlignmentSettings {
    let filters = &mut settings.filters;
    filters.read_sampling = filters.read_sampling.normalized();
    filters.minimum_junction_depth = filters.minimum_junction_depth.max(1);
    filters.sashimi_fixed_scale_maximum = filters.sashimi_fixed_scale_maximum.max(1);
    if let (Some(minimum), Some(maximum)) = (
        filters.minimum_template_length,
        filters.maximum_template_length,
    ) {
        filters.maximum_template_length = Some(maximum.max(minimum));
    }
    settings
}

impl<P: Panel> Session<P> {
    /// The id the primary alignment settings answer to: the first alignment track, or
    /// [`EMPTY_ALIGNMENT_TRACK_ID`] while none is open.
    pub fn primary_alignment_settings_id(&self) -> TrackId {
        self.alignment_tracks
            .first()
            .map_or(EMPTY_ALIGNMENT_TRACK_ID, |track| track.config.id)
    }

    /// One alignment track's settings as the session document saves them. The primary track's
    /// committed filter clauses are the ones in force (the active filters), exactly as
    /// `session_document` writes them. `None` for a track that is not open.
    pub fn alignment_settings(&self, track_id: TrackId) -> Option<AlignmentSettings> {
        let primary = self.primary_alignment_settings_id() == track_id;
        let (mut filters, maximum_alignments, active) = if self.alignment_tracks.is_empty() {
            if track_id != EMPTY_ALIGNMENT_TRACK_ID {
                return None;
            }
            (
                self.empty_alignment.filters.clone(),
                self.empty_alignment.maximum_alignments,
                &self.empty_alignment.active_filters,
            )
        } else {
            let track = self
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)?;
            (
                track.config.filters.clone(),
                track.config.maximum_alignments,
                &track.active_filters,
            )
        };
        if primary {
            filters.minimum_mapping_quality = active.alignment.minimum_mapping_quality;
            filters.include_secondary = active.include_secondary;
            filters.include_supplementary = active.include_supplementary;
        }
        Some(AlignmentSettings {
            filters,
            maximum_alignments,
        })
    }

    /// Applies [`Action::SetAlignmentSettings`]: every named track takes the given settings.
    /// Every track is validated before anything changes. `workspace_limit` sets the
    /// workspace-wide complete-read limit exactly (an undo putting it back); `None` widens it as
    /// far as a requested track limit needs.
    pub fn apply_alignment_settings(
        &mut self,
        updates: &[(TrackId, AlignmentSettings)],
        workspace_limit: Option<usize>,
    ) -> Result<AlignmentSettingsApplied> {
        let mut prepared = Vec::with_capacity(updates.len());
        let mut workspace = workspace_limit
            .unwrap_or(self.workspace_alignment_limit)
            .max(1);
        for (track_id, settings) in updates {
            let before = self.alignment_settings(*track_id).ok_or_else(|| {
                anyhow::anyhow!("alignment track {} was not found", track_id.get())
            })?;
            let mut after = normalized(settings.clone());
            if workspace_limit.is_some() {
                after.maximum_alignments = after
                    .maximum_alignments
                    .clamp(1, rgv_core::MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT)
                    .min(workspace);
            } else {
                let (expanded, limit) =
                    expanded_workspace_alignment_limit(workspace, after.maximum_alignments);
                workspace = expanded;
                after.maximum_alignments = limit;
            }
            let fields = changed_alignment_fields(&before, &after);
            let active = if FILTER_CLAUSE_FIELDS
                .iter()
                .any(|field| fields.contains(*field))
            {
                Some(active_filters_from_session(&after.filters)?)
            } else {
                None
            };
            prepared.push((*track_id, before, after, fields, active));
        }

        let mut applied = AlignmentSettingsApplied::default();
        if workspace != self.workspace_alignment_limit {
            applied.previous_workspace_limit = Some(self.workspace_alignment_limit);
            self.workspace_alignment_limit = workspace;
        }
        let primary_id = self.primary_alignment_settings_id();
        for (track_id, before, after, fields, active) in prepared {
            let (config_filters, maximum_alignments, editor, active_filters) =
                if self.alignment_tracks.is_empty() {
                    let state = &mut self.empty_alignment;
                    (
                        &mut state.filters,
                        &mut state.maximum_alignments,
                        &mut state.filter_editor,
                        &mut state.active_filters,
                    )
                } else {
                    let track = self
                        .alignment_tracks
                        .iter_mut()
                        .find(|track| track.config.id == track_id)
                        .expect("validated above");
                    (
                        &mut track.config.filters,
                        &mut track.config.maximum_alignments,
                        &mut track.filter_editor,
                        &mut track.active_filters,
                    )
                };
            config_filters.clone_from(&after.filters);
            *maximum_alignments = after.maximum_alignments;
            if let Some(active) = active {
                *active_filters = active;
                // The editor mirrors the committed clauses; an unapplied draft is replaced.
                editor.minimum_mapq = after.filters.minimum_mapping_quality.unwrap_or_default();
                editor.include_secondary = after.filters.include_secondary;
                editor.include_supplementary = after.filters.include_supplementary;
                editor.expression = after.filters.auxiliary_predicates.join(";");
                editor.error = None;
            }
            if !fields.is_empty() {
                applied.changes.push(AlignmentSettingsChange {
                    track_id,
                    primary: track_id == primary_id,
                    before,
                    after,
                    fields,
                });
            }
        }
        Ok(applied)
    }
}
