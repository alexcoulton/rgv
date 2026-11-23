//! Deferred alignment, reference and auxiliary track setting changes.

use crate::action::{Action, ReferencePresentation};
use crate::display::alignment::parse_style_tag;
use crate::display::auxiliary::{alignment_sort_position, normalize_quantitative_overlay_settings};
use crate::ui::AlignmentSubtrack;
use crate::ui::alignment_settings::modification_code_label;
use crate::ui::frame::{AlignmentSettingsFrame, ShellFrame};
use crate::workspace::alignments::FilterEditor;
use crate::workspace::lists::apply_compatible_auxiliary_appearance;
use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY, QuantitativeBulkAction, Viewer};
use rgv_core::{TrackConfig, TrackId, Viewport};
use rgv_hts::AuxPredicate;
use rgv_layout::{AlignmentColorMode, AlignmentGroupMode, AlignmentSortMode};
use rgv_session::action::AlignmentSettings;
use rgv_session::action::alignment_settings::merge_alignment_settings_draft;
use std::collections::BTreeMap;

/// The settings the alignment controls of `align` ask for, as the settings action carries
/// them. The filter clauses are left as they were: only "Apply filter" commits those.
fn drafted_alignment_settings(
    align: &mut AlignmentSettingsFrame,
    viewport: Viewport,
) -> AlignmentSettings {
    if let Some(tag) = parse_style_tag(&align.style_tag_editor) {
        if matches!(align.color_mode, AlignmentColorMode::Tag(_)) {
            align.color_mode = AlignmentColorMode::Tag(tag);
        }
        if matches!(align.group_mode, AlignmentGroupMode::Tag(_)) {
            align.group_mode = AlignmentGroupMode::Tag(tag);
        }
        if matches!(align.sort_mode, AlignmentSortMode::Tag(_)) {
            align.sort_mode = AlignmentSortMode::Tag(tag);
        }
    }
    let position_grouping = |mode| {
        matches!(
            mode,
            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
        )
    };
    let previous = &align.settings_filters;
    // A base sort or position grouping keeps the anchor it started with; a new one anchors at
    // the centre of the focused view.
    let alignment_sort_anchor = (align.sort_mode == AlignmentSortMode::Base
        || position_grouping(align.group_mode))
    .then(|| {
        (previous.alignment_sort_mode == AlignmentSortMode::Base
            || position_grouping(previous.alignment_group_mode))
        .then_some(previous.alignment_sort_anchor)
        .flatten()
        .or_else(|| alignment_sort_position(AlignmentSortMode::Base, None, viewport))
    })
    .flatten();
    let mut filters = align.settings_filters.clone();
    filters.alignment_color_mode = align.color_mode;
    filters.alignment_group_mode = align.group_mode;
    filters.alignment_group_order = align.group_order;
    filters.alignment_sort_mode = align.sort_mode;
    filters.alignment_sort_order = align.sort_order;
    filters.alignment_sort_anchor = alignment_sort_anchor;
    filters.read_stacking_mode = align.read_stacking_mode;
    filters.read_sampling = align.read_sampling.normalized();
    filters.view_as_pairs = align.view_as_pairs;
    filters.minimum_template_length = align
        .override_pair_bounds
        .then_some(align.minimum_template_length);
    filters.maximum_template_length = align.override_pair_bounds.then_some(
        align
            .maximum_template_length
            .max(align.minimum_template_length),
    );
    filters.overlapping_mate_policy = align.overlapping_mate_policy;
    filters.minimum_base_quality = align.minimum_base_quality;
    filters.minimum_allele_fraction = align.minimum_allele_fraction;
    filters.quality_weighted_alleles = align.quality_weighted_alleles;
    filters.show_reference = align.show_reference;
    filters.show_coverage = align.show_coverage;
    filters.show_junctions = align.show_junctions;
    filters.minimum_junction_depth = align.minimum_junction_depth.max(1);
    filters.minimum_junction_flank = align.minimum_junction_flank;
    filters.junction_display_mode = align.junction_display_mode;
    filters.sashimi_scale_mode = align.sashimi_scale_mode;
    filters.sashimi_fixed_scale_maximum = align.sashimi_fixed_scale_maximum.max(1);
    filters.sashimi_count_mode = align.sashimi_count_mode;
    filters.show_junction_labels = align.show_junction_labels;
    filters.coverage_scale_maximum = align
        .use_fixed_coverage_scale
        .then_some(align.coverage_scale_maximum.max(1));
    filters.coverage_scale_transform = align.coverage_scale_transform;
    filters.show_coverage_center_line = align.show_coverage_center_line;
    filters.alignment_density = align
        .alignment_density
        .clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
    filters.inspector_activation = align.inspector_activation;
    filters.read_base_display_mode = align.read_base_display_mode;
    filters.shade_read_bases_by_quality = align.shade_read_bases_by_quality;
    filters.minimum_indel_size = align.minimum_indel_size;
    filters.minimum_event_label_size = align.minimum_event_label_size;
    filters.show_modified_bases = align.show_modified_bases;
    filters.show_modification_frequency = align.show_modification_frequency;
    filters.minimum_modification_probability = align.minimum_modification_probability;
    filters.modified_base_color_mode = align.modified_base_color_mode;
    filters.hidden_modification_codes = align
        .hidden_modification_codes
        .iter()
        .copied()
        .map(modification_code_label)
        .collect();
    AlignmentSettings {
        filters,
        maximum_alignments: align.complete_read_limit as usize,
    }
}

/// The filter clauses the editor commits: the mapping-quality floor and the tag predicates.
fn committed_filter_clauses(
    align: &AlignmentSettingsFrame,
) -> Result<(Option<u8>, Vec<String>), String> {
    let expressions = align
        .filter_expression
        .split(';')
        .map(str::trim)
        .filter(|expression| !expression.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    for expression in &expressions {
        expression
            .parse::<AuxPredicate>()
            .map_err(|error| error.to_string())?;
    }
    Ok((
        (align.minimum_mapq > 0).then_some(align.minimum_mapq),
        expressions,
    ))
}

/// A gutter lane toggle as the settings it leaves. `None` for the reads lane, which is a
/// track-arrangement edit.
pub(crate) fn toggled_alignment_lane(
    current: &AlignmentSettings,
    subtrack: AlignmentSubtrack,
) -> Option<AlignmentSettings> {
    let mut next = current.clone();
    let filters = &mut next.filters;
    match subtrack {
        AlignmentSubtrack::Reference => filters.show_reference = !filters.show_reference,
        AlignmentSubtrack::Coverage => filters.show_coverage = !filters.show_coverage,
        AlignmentSubtrack::Sashimi => filters.show_junctions = !filters.show_junctions,
        AlignmentSubtrack::Modifications => {
            filters.show_modification_frequency = !filters.show_modification_frequency;
        }
        AlignmentSubtrack::Reads => return None,
    }
    Some(next)
}

impl Viewer {
    pub(super) fn apply_reference_presentation(&mut self, st: &mut ShellFrame) {
        let requested = ReferencePresentation {
            reverse_complement: st.align.reference_reverse_complement,
            translation_frames: std::mem::take(&mut st.align.reference_translation_frames),
            genetic_code: st.align.reference_genetic_code,
        }
        .normalized();
        if requested != self.reference_presentation()
            && let Err(error) = self.apply_action(Action::SetReferencePresentation(requested))
        {
            self.report_error(error.to_string());
        }
    }

    /// Commits the annotation and signal track controls (names, sample key, the settings of
    /// each kind) as one track settings action, and a track-list move as another.
    pub(super) fn apply_auxiliary_track_settings(&mut self, st: &mut ShellFrame) {
        let mut edited = BTreeMap::<TrackId, TrackConfig>::new();
        let tracks = self.session.tracks.tracks();
        let mut edit = |id: TrackId, apply: &mut dyn FnMut(&mut TrackConfig)| {
            if let Some(config) = edited.get_mut(&id) {
                apply(config);
            } else if let Some(track) = tracks.iter().find(|track| track.id == id) {
                let mut config = track.clone();
                apply(&mut config);
                edited.insert(id, config);
            }
        };
        if let Some(id) = st.align.editing_auxiliary_id {
            let sample_key = st.align.edited_track_sample_key.clone();
            edit(id, &mut |track| track.sample_key.clone_from(&sample_key));
        }
        for control in std::mem::take(&mut st.tracks.track_controls) {
            let name = control.name;
            edit(control.id, &mut |track| track.name.clone_from(&name));
        }
        for (id, settings) in std::mem::take(&mut st.tracks.feature_settings_controls) {
            edit(id, &mut |track| track.feature = settings.clone());
        }
        for (id, settings) in std::mem::take(&mut st.tracks.variant_settings_controls) {
            edit(id, &mut |track| track.variant = settings.clone());
        }
        for (id, settings) in std::mem::take(&mut st.tracks.gwas_settings_controls) {
            if settings.is_valid() {
                edit(id, &mut |track| track.gwas = settings.clone());
            }
        }
        normalize_quantitative_overlay_settings(
            &mut st.tracks.quantitative_settings_controls,
            st.align.editing_auxiliary_id,
        );
        for (id, settings) in std::mem::take(&mut st.tracks.quantitative_settings_controls) {
            if settings.is_valid() {
                edit(id, &mut |track| track.quantitative = settings.clone());
            }
        }
        for (id, settings) in std::mem::take(&mut st.tracks.copy_number_settings_controls) {
            if settings.is_valid() {
                edit(id, &mut |track| track.copy_number = settings.clone());
            }
        }
        for (id, settings) in std::mem::take(&mut st.tracks.mutation_settings_controls) {
            if settings.is_valid() {
                edit(id, &mut |track| track.mutation = settings.clone());
            }
        }
        for (id, settings) in std::mem::take(&mut st.tracks.interaction_settings_controls) {
            if settings.is_valid() {
                edit(id, &mut |track| track.interaction = settings.clone());
            }
        }
        let tracks = self.session.tracks.tracks();
        let changed = edited
            .into_values()
            .filter(|config| {
                tracks
                    .iter()
                    .find(|track| track.id == config.id)
                    .is_some_and(|track| track != config)
            })
            .collect::<Vec<_>>();
        if !changed.is_empty()
            && let Err(error) = self.apply_action(Action::SetTrackConfigs { tracks: changed })
        {
            self.report_error(error.to_string());
        }
        if let Some((track_id, index)) = st.tracks.track_move
            && let Err(error) = self.apply_action(Action::MoveTrack { track_id, index })
        {
            self.report_error(error.to_string());
        }
    }

    /// Commits the "Track settings" draft of the alignment track it shows (the primary track
    /// when the window is closed) as one alignment settings action. Only fields the window
    /// edited this frame are taken from the draft, so an untouched window never writes.
    pub(super) fn commit_alignment_settings(&mut self, st: &mut ShellFrame) {
        let track_id = st.align.settings_alignment_id;
        let viewport = self.session.panels.active().viewport;
        let drafted = drafted_alignment_settings(&mut st.align, viewport);
        self.commit_filter_editor_draft(track_id, &st.align);
        let Some(current) = self.session.alignment_settings(track_id) else {
            return;
        };
        let mut next = if drafted == current {
            current.clone()
        } else {
            // What the controls showed before the user touched them: values the controls
            // cannot express (an unparsed modification code, a one-sided pair bound) are not
            // edits and must not be written back, or undo would be undone by the next frame.
            let mut baseline_frame = self.prepare_alignment_settings();
            let baseline = if baseline_frame.settings_alignment_id == track_id {
                drafted_alignment_settings(&mut baseline_frame, viewport)
            } else {
                current.clone()
            };
            merge_alignment_settings_draft(&current, &baseline, &drafted)
        };
        if !(st.align.typed_alignment_predicates_changed
            || st.align.alignment_highlight_predicates_changed)
        {
            next.filters
                .alignment_view_filter
                .clone_from(&current.filters.alignment_view_filter);
            next.filters
                .alignment_highlight_filter
                .clone_from(&current.filters.alignment_highlight_filter);
        }
        if st.align.apply_filter {
            match committed_filter_clauses(&st.align) {
                Ok((minimum_mapping_quality, auxiliary_predicates)) => {
                    next.filters.minimum_mapping_quality = minimum_mapping_quality;
                    next.filters.include_secondary = st.align.include_secondary;
                    next.filters.include_supplementary = st.align.include_supplementary;
                    next.filters.auxiliary_predicates = auxiliary_predicates;
                    if let Some(editor) = self.alignment_filter_editor_mut(track_id) {
                        editor.error = None;
                    }
                }
                Err(error) => {
                    if let Some(editor) = self.alignment_filter_editor_mut(track_id) {
                        editor.error = Some(error);
                    }
                }
            }
        }
        if next != current
            && let Err(error) = self.set_alignment_settings(vec![(track_id, next)])
        {
            self.report_error(error.to_string());
        }
        if track_id == st.align.primary_alignment_id {
            self.style_tag_editor.clone_from(&st.align.style_tag_editor);
        }
    }

    /// The filter editor of an alignment track (the primary settings while none is open).
    fn alignment_filter_editor_mut(&mut self, track_id: TrackId) -> Option<&mut FilterEditor> {
        if track_id == self.primary_alignment_track_id() {
            return Some(self.primary_filter_editor_mut());
        }
        self.session
            .alignment_tracks
            .iter_mut()
            .find(|track| track.config.id == track_id)
            .map(|track| &mut track.filter_editor)
    }

    /// The filter editor holds what the user typed until "Apply filter" commits it.
    fn commit_filter_editor_draft(&mut self, track_id: TrackId, align: &AlignmentSettingsFrame) {
        if let Some(editor) = self.alignment_filter_editor_mut(track_id) {
            editor.minimum_mapq = align.minimum_mapq;
            editor.include_secondary = align.include_secondary;
            editor.include_supplementary = align.include_supplementary;
            editor.expression.clone_from(&align.filter_expression);
        }
    }

    /// The gutter's lane toggles: reference, coverage, junctions and modifications are
    /// alignment settings; the reads lane is a track-arrangement edit.
    fn toggle_alignment_lane(&mut self, track_id: TrackId, subtrack: AlignmentSubtrack) {
        let Some(current) = self.session.alignment_settings(track_id) else {
            return;
        };
        let Some(next) = toggled_alignment_lane(&current, subtrack) else {
            self.toggle_alignment_subtrack(track_id, subtrack);
            return;
        };
        if let Err(error) = self.set_alignment_settings(vec![(track_id, next)]) {
            self.report_error(error.to_string());
        }
    }

    pub(super) fn apply_bulk_and_deferred_track_actions(
        &mut self,
        st: &mut ShellFrame,
        alignment_subtracks_to_toggle: Vec<(TrackId, AlignmentSubtrack)>,
        alignment_track_to_remove: Option<TrackId>,
        metadata_selection_changed: bool,
    ) {
        if st.align.compact_repack_requested {
            self.query_options_changed();
        }
        if st.align.apply_bulk_alignment_appearance
            && let Err(error) = self.apply_bulk_alignment_appearance(
                st.align.settings_alignment_id,
                &st.align.bulk_alignment_ids,
            )
        {
            self.report_error(error.to_string());
        }
        for (track_id, subtrack) in alignment_subtracks_to_toggle {
            self.toggle_alignment_lane(track_id, subtrack);
        }
        self.metadata_bulk_height = st.lists.metadata_bulk_height;
        self.metadata_remove_armed = st.lists.metadata_remove_armed && !metadata_selection_changed;
        let metadata_selected = self.metadata_selected_tracks.clone();
        if st.lists.metadata_bulk_visibility.is_some()
            || st.lists.metadata_bulk_collapsed.is_some()
            || st.lists.metadata_bulk_height_requested
            || st.lists.metadata_bulk_rename_requested
        {
            self.apply_metadata_bulk_state(
                &metadata_selected,
                st.lists.metadata_bulk_visibility,
                st.lists.metadata_bulk_collapsed,
                st.lists
                    .metadata_bulk_height_requested
                    .then_some(st.lists.metadata_bulk_height),
                st.lists.metadata_bulk_rename_requested,
            );
        }
        if st.lists.metadata_bulk_alignment_settings_requested
            && let Err(error) = self.apply_metadata_bulk_alignment_settings(&metadata_selected)
        {
            self.report_error(error.to_string());
        }
        if st.lists.metadata_bulk_auxiliary_appearance_requested {
            let mut tracks = self.session.tracks.tracks().to_vec();
            if apply_compatible_auxiliary_appearance(&mut tracks, &metadata_selected) {
                self.set_changed_track_configs(tracks);
            }
        }
        if st.lists.metadata_bulk_quantitative_autoscale_requested {
            self.apply_metadata_quantitative_action(
                &metadata_selected,
                QuantitativeBulkAction::Autoscale,
            );
        } else if st.lists.metadata_bulk_quantitative_overlay_requested {
            self.apply_metadata_quantitative_action(
                &metadata_selected,
                QuantitativeBulkAction::Overlay,
            );
        } else if st.lists.metadata_bulk_quantitative_separate_requested {
            self.apply_metadata_quantitative_action(
                &metadata_selected,
                QuantitativeBulkAction::Separate,
            );
        }
        if st.lists.metadata_bulk_remove_requested {
            match self.remove_tracks(&metadata_selected) {
                Ok(removed) => {
                    self.session.data_status = format!("Removed {removed} selected tracks");
                }
                Err(error) => self.report_error(error.to_string()),
            }
        }
        if let Some(id) = alignment_track_to_remove
            && let Err(error) = self.remove_tracks(&std::collections::BTreeSet::from([id]))
        {
            self.report_error(error.to_string());
        }
        if let Some(panel_id) = st.sashimi_clear_filters.take() {
            self.clear_sashimi_filters(panel_id);
        }
        if let Some(target) = st.gene_transcripts_action.take() {
            self.set_gene_transcripts_expanded(target.track_id, &target.gene_key, !target.expanded);
        }
        if let Some((target, action)) = st.alignment_context_action.take() {
            self.apply_alignment_context_action(target, action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_automation::{AutomationCommand, AutomationSetting};
    use rgv_core::{ContigId, DesktopSettings, GenomeAssembly, GenomicInterval, PanelId};
    use rgv_session::action::Action;
    use rgv_session::action::alignment_settings::{
        EMPTY_ALIGNMENT_TRACK_ID, MAXIMUM_ALIGNMENT_DENSITY, MINIMUM_ALIGNMENT_DENSITY,
    };
    use rgv_session::action::translate::automation_alignment_settings_actions;
    use rgv_session::panels::PanelState;

    fn session() -> rgv_session::Session {
        let dictionary =
            rgv_core::ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        rgv_session::Session::new(
            PanelState::new(
                PanelId::new(1),
                Viewport::new(interval, 600.0).unwrap(),
                None,
                "chr1".to_owned(),
                1_000_000,
            ),
            GenomeAssembly::from_dictionary("test", dictionary),
            DesktopSettings::default(),
            std::path::PathBuf::from("settings.json"),
        )
    }

    fn apply(session: &mut rgv_session::Session, actions: Vec<Action>) {
        for action in actions {
            let Action::SetAlignmentSettings {
                updates,
                workspace_limit,
            } = action
            else {
                panic!("an alignment settings action");
            };
            session
                .apply_alignment_settings(&updates, workspace_limit)
                .unwrap();
        }
    }

    fn document(session: &rgv_session::Session) -> String {
        serde_json::to_string(
            &session
                .alignment_settings(EMPTY_ALIGNMENT_TRACK_ID)
                .unwrap()
                .filters,
        )
        .unwrap()
    }

    #[test]
    fn the_session_validates_density_against_the_navigator_range() {
        assert!((MINIMUM_ALIGNMENT_DENSITY - MIN_VERTICAL_DENSITY).abs() < f32::EPSILON);
        assert!((MAXIMUM_ALIGNMENT_DENSITY - MAX_VERTICAL_DENSITY).abs() < f32::EPSILON);
    }

    #[test]
    fn gutter_lane_toggles_and_their_settings_leave_the_same_document() {
        let cases = [
            (
                AlignmentSubtrack::Coverage,
                AutomationSetting::ShowCoverage { value: false },
            ),
            (
                AlignmentSubtrack::Reference,
                AutomationSetting::ShowReference { value: false },
            ),
            (
                AlignmentSubtrack::Sashimi,
                AutomationSetting::ShowJunctions { value: false },
            ),
        ];
        for (subtrack, setting) in cases {
            let mut by_automation = session();
            let mut by_gutter = session();
            let before = document(&by_automation);
            let command = AutomationCommand::Configure { setting };
            let actions = automation_alignment_settings_actions(&by_automation, &command)
                .unwrap()
                .unwrap();
            apply(&mut by_automation, actions);
            let current = by_gutter
                .alignment_settings(EMPTY_ALIGNMENT_TRACK_ID)
                .unwrap();
            let toggled = toggled_alignment_lane(&current, subtrack).unwrap();
            apply(
                &mut by_gutter,
                vec![Action::SetAlignmentSettings {
                    updates: vec![(EMPTY_ALIGNMENT_TRACK_ID, toggled)],
                    workspace_limit: None,
                }],
            );
            assert_eq!(
                document(&by_automation),
                document(&by_gutter),
                "{subtrack:?}"
            );
            assert_ne!(document(&by_automation), before, "{subtrack:?}");
        }
        assert!(
            toggled_alignment_lane(
                &session()
                    .alignment_settings(EMPTY_ALIGNMENT_TRACK_ID)
                    .unwrap(),
                AlignmentSubtrack::Reads
            )
            .is_none(),
            "the reads lane is an arrangement edit"
        );
    }
}
