//! Deferred alignment, reference and auxiliary track setting changes.

use super::*;

impl Viewer {
    pub(super) fn apply_primary_alignment_query_settings(
        &mut self,
        st: &mut ShellFrame,
        editing_primary_settings: bool,
    ) {
        if editing_primary_settings && st.align.typed_alignment_predicates_changed {
            self.primary_alignment_filters_mut().alignment_view_filter =
                st.align.settings_filters.alignment_view_filter.clone();
            self.primary_alignment_filters_mut()
                .alignment_highlight_filter =
                st.align.settings_filters.alignment_highlight_filter.clone();
            if st.align.alignment_highlight_predicates_changed {
                self.refresh_alignment_highlight_interactions(&[st.align.settings_alignment_id]);
            }
            self.query_options_changed();
        }
        if editing_primary_settings {
            self.primary_filter_editor_mut().minimum_mapq = st.align.minimum_mapq;
            self.primary_filter_editor_mut().include_secondary = st.align.include_secondary;
            self.primary_filter_editor_mut().include_supplementary = st.align.include_supplementary;
            self.primary_filter_editor_mut()
                .expression
                .clone_from(&st.align.filter_expression);
        }
        if editing_primary_settings
            && st.align.minimum_base_quality
                != self.primary_alignment_filters().minimum_base_quality
        {
            self.primary_alignment_filters_mut().minimum_base_quality =
                st.align.minimum_base_quality;
            self.query_options_changed();
        }
        if editing_primary_settings
            && st.align.minimum_junction_flank
                != self.primary_alignment_filters().minimum_junction_flank
        {
            self.primary_alignment_filters_mut().minimum_junction_flank =
                st.align.minimum_junction_flank;
            self.query_options_changed();
        }
        if editing_primary_settings
            && st.align.overlapping_mate_policy
                != self.primary_alignment_filters().overlapping_mate_policy
        {
            self.primary_alignment_filters_mut().overlapping_mate_policy =
                st.align.overlapping_mate_policy;
            self.query_options_changed();
        }
        if editing_primary_settings
            && st.align.show_modification_frequency
                != self.primary_alignment_filters().show_modification_frequency
        {
            self.primary_alignment_filters_mut()
                .show_modification_frequency = st.align.show_modification_frequency;
            self.panels.active_mut().selected_modification_position = None;
            self.query_options_changed();
        }
    }

    pub(super) fn apply_reference_presentation(&mut self, st: &mut ShellFrame) {
        st.align.reference_translation_frames.sort();
        st.align.reference_translation_frames.dedup();
        if st.align.reference_reverse_complement != self.reference_reverse_complement
            || st.align.reference_translation_frames != self.reference_translation_frames
            || st.align.reference_genetic_code != self.reference_genetic_code
        {
            self.reference_reverse_complement = st.align.reference_reverse_complement;
            self.reference_translation_frames =
                std::mem::take(&mut st.align.reference_translation_frames);
            self.reference_genetic_code = st.align.reference_genetic_code;
            self.invalidate_navigation_contract("reference presentation changed");
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_primary_alignment_presentation(
        &mut self,
        st: &mut ShellFrame,
        editing_primary_settings: bool,
        requested_coverage_scale: Option<u32>,
    ) {
        if editing_primary_settings
            && st.align.inspector_activation
                != self.primary_alignment_filters().inspector_activation
        {
            self.primary_alignment_filters_mut().inspector_activation =
                st.align.inspector_activation;
            self.panels.active_mut().hover_inspector_target = None;
        }
        if editing_primary_settings
            && ((st.align.minimum_allele_fraction
                - self.primary_alignment_filters().minimum_allele_fraction)
                .abs()
                > f32::EPSILON
                || st.align.quality_weighted_alleles
                    != self.primary_alignment_filters().quality_weighted_alleles
                || st.align.show_reference != self.primary_alignment_filters().show_reference
                || st.align.show_coverage != self.primary_alignment_filters().show_coverage
                || st.align.show_junctions != self.primary_alignment_filters().show_junctions
                || st.align.minimum_junction_depth
                    != self.primary_alignment_filters().minimum_junction_depth
                || st.align.junction_display_mode
                    != self.primary_alignment_filters().junction_display_mode
                || st.align.sashimi_scale_mode
                    != self.primary_alignment_filters().sashimi_scale_mode
                || st.align.sashimi_fixed_scale_maximum
                    != self.primary_alignment_filters().sashimi_fixed_scale_maximum
                || st.align.sashimi_count_mode
                    != self.primary_alignment_filters().sashimi_count_mode
                || st.align.show_junction_labels
                    != self.primary_alignment_filters().show_junction_labels
                || requested_coverage_scale
                    != self.primary_alignment_filters().coverage_scale_maximum
                || st.align.coverage_scale_transform
                    != self.primary_alignment_filters().coverage_scale_transform
                || st.align.show_coverage_center_line
                    != self.primary_alignment_filters().show_coverage_center_line
                || (st.align.alignment_density
                    - self.primary_alignment_filters().alignment_density)
                    .abs()
                    > f32::EPSILON
                || st.align.read_base_display_mode
                    != self.primary_alignment_filters().read_base_display_mode
                || st.align.shade_read_bases_by_quality
                    != self.primary_alignment_filters().shade_read_bases_by_quality
                || st.align.minimum_indel_size
                    != self.primary_alignment_filters().minimum_indel_size
                || st.align.minimum_event_label_size
                    != self.primary_alignment_filters().minimum_event_label_size
                || st.align.show_modified_bases
                    != self.primary_alignment_filters().show_modified_bases
                || st.align.minimum_modification_probability
                    != self
                        .primary_alignment_filters()
                        .minimum_modification_probability
                || st.align.modified_base_color_mode
                    != self.primary_alignment_filters().modified_base_color_mode
                || st.align.hidden_modification_codes != self.primary_hidden_modification_codes())
        {
            self.primary_alignment_filters_mut().minimum_allele_fraction =
                st.align.minimum_allele_fraction;
            self.primary_alignment_filters_mut()
                .quality_weighted_alleles = st.align.quality_weighted_alleles;
            self.primary_alignment_filters_mut().show_reference = st.align.show_reference;
            if !st.align.show_coverage {
                self.panels.active_mut().selected_pileup_position = None;
            }
            self.primary_alignment_filters_mut().show_coverage = st.align.show_coverage;
            if !st.align.show_junctions {
                self.panels.active_mut().selected_junction = None;
                self.panels.active_mut().selected_junction_component = None;
            }
            self.primary_alignment_filters_mut().show_junctions = st.align.show_junctions;
            self.primary_alignment_filters_mut().minimum_junction_depth =
                st.align.minimum_junction_depth.max(1);
            self.primary_alignment_filters_mut().junction_display_mode =
                st.align.junction_display_mode;
            self.primary_alignment_filters_mut().sashimi_scale_mode = st.align.sashimi_scale_mode;
            self.primary_alignment_filters_mut()
                .sashimi_fixed_scale_maximum = st.align.sashimi_fixed_scale_maximum.max(1);
            self.primary_alignment_filters_mut().sashimi_count_mode = st.align.sashimi_count_mode;
            self.primary_alignment_filters_mut().show_junction_labels =
                st.align.show_junction_labels;
            self.primary_alignment_filters_mut().coverage_scale_maximum = requested_coverage_scale;
            self.primary_alignment_filters_mut()
                .coverage_scale_transform = st.align.coverage_scale_transform;
            self.primary_alignment_filters_mut()
                .show_coverage_center_line = st.align.show_coverage_center_line;
            self.set_alignment_density_for_track(
                st.align.primary_alignment_id,
                st.align.alignment_density,
            );
            self.primary_alignment_filters_mut().read_base_display_mode =
                st.align.read_base_display_mode;
            self.primary_alignment_filters_mut()
                .shade_read_bases_by_quality = st.align.shade_read_bases_by_quality;
            self.primary_alignment_filters_mut().minimum_indel_size = st.align.minimum_indel_size;
            self.primary_alignment_filters_mut()
                .minimum_event_label_size = st.align.minimum_event_label_size;
            self.primary_alignment_filters_mut().show_modified_bases = st.align.show_modified_bases;
            self.primary_alignment_filters_mut()
                .minimum_modification_probability = st.align.minimum_modification_probability;
            self.primary_alignment_filters_mut()
                .modified_base_color_mode = st.align.modified_base_color_mode;
            self.primary_alignment_filters_mut()
                .hidden_modification_codes = st
                .align
                .hidden_modification_codes
                .iter()
                .copied()
                .map(modification_code_label)
                .collect();
            self.invalidate_navigation_contract("alignment presentation changed");
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_auxiliary_track_settings(&mut self, st: &mut ShellFrame) {
        let previous_auxiliary_query_membership = auxiliary_query_track_ids(self.tracks.tracks())
            .into_iter()
            .collect::<HashSet<_>>();
        let mut track_configuration_changed = false;
        if let Some(id) = st.align.editing_auxiliary_id
            && let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
            && track.sample_key != st.align.edited_track_sample_key
        {
            track
                .sample_key
                .clone_from(&st.align.edited_track_sample_key);
            track_configuration_changed = true;
        }
        for control in std::mem::take(&mut st.tracks.track_controls) {
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == control.id)
                && track.name != control.name
            {
                track.name = control.name;
                track_configuration_changed = true;
            }
        }
        for (id, settings) in std::mem::take(&mut st.tracks.feature_settings_controls) {
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.feature != settings
            {
                track.feature = settings;
                track_configuration_changed = true;
            }
        }
        let mut variant_query_requested = false;
        for (id, settings) in std::mem::take(&mut st.tracks.variant_settings_controls) {
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.variant != settings
            {
                track.variant = settings;
                variant_query_requested = true;
                track_configuration_changed = true;
            }
        }
        for (id, settings) in std::mem::take(&mut st.tracks.gwas_settings_controls) {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.gwas != settings
            {
                track.gwas = settings;
                track_configuration_changed = true;
            }
        }
        normalize_quantitative_overlay_settings(
            &mut st.tracks.quantitative_settings_controls,
            st.align.editing_auxiliary_id,
        );
        for (id, settings) in std::mem::take(&mut st.tracks.quantitative_settings_controls) {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.quantitative != settings
            {
                track.quantitative = settings;
                track_configuration_changed = true;
            }
        }
        let mut copy_number_query_requested = false;
        for (id, settings) in std::mem::take(&mut st.tracks.copy_number_settings_controls) {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.copy_number != settings
            {
                copy_number_query_requested |= track.copy_number.sort_mode != settings.sort_mode
                    || track.copy_number.sort_locus != settings.sort_locus;
                track.copy_number = settings;
                track_configuration_changed = true;
            }
        }
        for (id, settings) in std::mem::take(&mut st.tracks.mutation_settings_controls) {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.mutation != settings
            {
                track.mutation = settings;
                track_configuration_changed = true;
            }
        }
        let mut interaction_query_requested = false;
        for (id, settings) in std::mem::take(&mut st.tracks.interaction_settings_controls) {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.interaction != settings
            {
                interaction_query_requested |=
                    track.interaction.minimum_score != settings.minimum_score;
                track.interaction = settings;
                track_configuration_changed = true;
            }
        }
        if let Some((id, new_index)) = st.tracks.track_move {
            track_configuration_changed |= self.tracks.move_track(id, new_index);
        }
        let auxiliary_query_membership_changed = previous_auxiliary_query_membership
            != auxiliary_query_track_ids(self.tracks.tracks())
                .into_iter()
                .collect::<HashSet<_>>();
        if track_configuration_changed {
            self.invalidate_navigation_contract("auxiliary track presentation changed");
            self.reconcile_track_presentation();
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
        if auxiliary_query_membership_changed {
            self.invalidate_and_request_all_auxiliary_track_queries();
        } else if variant_query_requested
            || interaction_query_requested
            || copy_number_query_requested
        {
            self.request_all_auxiliary_track_queries();
        }
    }

    /// Colour/group/sort/stacking/pairing changes for the primary track. Returns the resolved
    /// base-sort anchor, which secondary tracks reuse.
    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_primary_alignment_style(
        &mut self,
        st: &mut ShellFrame,
        editing_primary_settings: bool,
    ) -> Option<u64> {
        if let Some(tag) = parse_style_tag(&st.align.style_tag_editor) {
            if matches!(st.align.color_mode, AlignmentColorMode::Tag(_)) {
                st.align.color_mode = AlignmentColorMode::Tag(tag);
            }
            if matches!(st.align.group_mode, AlignmentGroupMode::Tag(_)) {
                st.align.group_mode = AlignmentGroupMode::Tag(tag);
            }
            if matches!(st.align.sort_mode, AlignmentSortMode::Tag(_)) {
                st.align.sort_mode = AlignmentSortMode::Tag(tag);
            }
        }
        let position_grouping = matches!(
            st.align.group_mode,
            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
        );
        let previous_position_grouping = matches!(
            st.align.settings_filters.alignment_group_mode,
            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
        );
        let alignment_sort_anchor = (st.align.sort_mode == AlignmentSortMode::Base
            || position_grouping)
            .then(|| {
                (st.align.settings_filters.alignment_sort_mode == AlignmentSortMode::Base
                    || previous_position_grouping)
                    .then_some(st.align.settings_filters.alignment_sort_anchor)
                    .flatten()
                    .or_else(|| {
                        alignment_sort_position(
                            AlignmentSortMode::Base,
                            None,
                            self.panels.active().viewport,
                        )
                    })
            })
            .flatten();
        if editing_primary_settings
            && (st.align.color_mode != self.primary_alignment_filters().alignment_color_mode
                || st.align.group_mode != self.primary_alignment_filters().alignment_group_mode
                || st.align.group_order != self.primary_alignment_filters().alignment_group_order
                || st.align.sort_mode != self.primary_alignment_filters().alignment_sort_mode
                || st.align.sort_order != self.primary_alignment_filters().alignment_sort_order
                || alignment_sort_anchor != self.primary_alignment_filters().alignment_sort_anchor
                || st.align.read_stacking_mode
                    != self.primary_alignment_filters().read_stacking_mode
                || st.align.read_sampling != self.primary_alignment_filters().read_sampling
                || st.align.view_as_pairs != self.primary_alignment_filters().view_as_pairs
                || (st
                    .align
                    .override_pair_bounds
                    .then_some(st.align.minimum_template_length)
                    != self.primary_alignment_filters().minimum_template_length)
                || (st
                    .align
                    .override_pair_bounds
                    .then_some(st.align.maximum_template_length)
                    != self.primary_alignment_filters().maximum_template_length))
        {
            let reference_query_changed =
                alignment_color_mode_needs_reference(
                    self.primary_alignment_filters().alignment_color_mode,
                ) != alignment_color_mode_needs_reference(st.align.color_mode);
            self.primary_alignment_filters_mut().alignment_color_mode = st.align.color_mode;
            self.primary_alignment_filters_mut().alignment_group_mode = st.align.group_mode;
            self.primary_alignment_filters_mut().alignment_group_order = st.align.group_order;
            self.primary_alignment_filters_mut().alignment_sort_mode = st.align.sort_mode;
            self.primary_alignment_filters_mut().alignment_sort_order = st.align.sort_order;
            self.primary_alignment_filters_mut().alignment_sort_anchor = alignment_sort_anchor;
            self.primary_alignment_filters_mut().read_stacking_mode = st.align.read_stacking_mode;
            self.primary_alignment_filters_mut().read_sampling =
                st.align.read_sampling.normalized();
            self.primary_alignment_filters_mut().view_as_pairs = st.align.view_as_pairs;
            if st.align.override_pair_bounds {
                st.align.maximum_template_length = st
                    .align
                    .maximum_template_length
                    .max(st.align.minimum_template_length);
                self.primary_alignment_filters_mut().minimum_template_length =
                    Some(st.align.minimum_template_length);
                self.primary_alignment_filters_mut().maximum_template_length =
                    Some(st.align.maximum_template_length);
            } else {
                self.primary_alignment_filters_mut().minimum_template_length = None;
                self.primary_alignment_filters_mut().maximum_template_length = None;
            }
            if reference_query_changed {
                self.query_options_changed();
            } else {
                self.query_options_changed();
            }
        }
        if editing_primary_settings {
            self.style_tag_editor.clone_from(&st.align.style_tag_editor);
        }
        alignment_sort_anchor
    }

    pub(super) fn apply_primary_alignment_filters(
        &mut self,
        st: &mut ShellFrame,
        editing_primary_settings: bool,
    ) {
        if editing_primary_settings && st.align.apply_filter {
            let editor = self.primary_filter_editor().clone();
            let expressions = editor
                .expression
                .split(';')
                .map(str::trim)
                .filter(|expression| !expression.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let parsed = expressions
                .iter()
                .map(|expression| expression.parse::<AuxPredicate>())
                .collect::<Result<Vec<_>, _>>();
            match parsed {
                Ok(auxiliary) => {
                    let active = ActiveFilters {
                        alignment: AlignmentFilter {
                            minimum_mapping_quality: (editor.minimum_mapq > 0)
                                .then_some(editor.minimum_mapq),
                            auxiliary,
                            ..AlignmentFilter::default()
                        },
                        include_secondary: editor.include_secondary,
                        include_supplementary: editor.include_supplementary,
                    };
                    let filters = self.primary_alignment_filters_mut();
                    filters.minimum_mapping_quality = active.alignment.minimum_mapping_quality;
                    filters.include_secondary = active.include_secondary;
                    filters.include_supplementary = active.include_supplementary;
                    filters.auxiliary_predicates = expressions;
                    *self.primary_active_filters_mut() = active;
                    self.primary_filter_editor_mut().error = None;
                    self.query_options_changed();
                }
                Err(error) => self.primary_filter_editor_mut().error = Some(error.to_string()),
            }
        }
        if editing_primary_settings
            && st.align.complete_read_limit
                != u32::try_from(self.primary_alignment_limit()).unwrap_or(u32::MAX)
        {
            self.set_primary_alignment_limit(st.align.complete_read_limit as usize);
            self.query_options_changed();
        }
    }

    pub(super) fn commit_secondary_alignment_settings(
        &mut self,
        st: &mut ShellFrame,
        editing_primary_settings: bool,
        requested_coverage_scale: Option<u32>,
        alignment_sort_anchor: Option<u64>,
    ) {
        if !editing_primary_settings && st.align.editing_alignment_id.is_some() {
            let mut filters = std::mem::take(&mut st.align.settings_filters);
            filters.alignment_color_mode = st.align.color_mode;
            filters.alignment_group_mode = st.align.group_mode;
            filters.alignment_group_order = st.align.group_order;
            filters.alignment_sort_mode = st.align.sort_mode;
            filters.alignment_sort_order = st.align.sort_order;
            filters.alignment_sort_anchor = alignment_sort_anchor;
            filters.read_stacking_mode = st.align.read_stacking_mode;
            filters.read_sampling = st.align.read_sampling.normalized();
            filters.view_as_pairs = st.align.view_as_pairs;
            filters.minimum_template_length = st
                .align
                .override_pair_bounds
                .then_some(st.align.minimum_template_length);
            filters.maximum_template_length = st.align.override_pair_bounds.then_some(
                st.align
                    .maximum_template_length
                    .max(st.align.minimum_template_length),
            );
            filters.overlapping_mate_policy = st.align.overlapping_mate_policy;
            filters.minimum_base_quality = st.align.minimum_base_quality;
            filters.minimum_allele_fraction = st.align.minimum_allele_fraction;
            filters.quality_weighted_alleles = st.align.quality_weighted_alleles;
            filters.show_reference = st.align.show_reference;
            filters.show_coverage = st.align.show_coverage;
            filters.show_junctions = st.align.show_junctions;
            filters.minimum_junction_depth = st.align.minimum_junction_depth.max(1);
            filters.minimum_junction_flank = st.align.minimum_junction_flank;
            filters.junction_display_mode = st.align.junction_display_mode;
            filters.sashimi_scale_mode = st.align.sashimi_scale_mode;
            filters.sashimi_fixed_scale_maximum = st.align.sashimi_fixed_scale_maximum.max(1);
            filters.sashimi_count_mode = st.align.sashimi_count_mode;
            filters.show_junction_labels = st.align.show_junction_labels;
            filters.coverage_scale_maximum = requested_coverage_scale;
            filters.coverage_scale_transform = st.align.coverage_scale_transform;
            filters.show_coverage_center_line = st.align.show_coverage_center_line;
            filters.alignment_density = st
                .align
                .alignment_density
                .clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
            filters.inspector_activation = st.align.inspector_activation;
            filters.read_base_display_mode = st.align.read_base_display_mode;
            filters.shade_read_bases_by_quality = st.align.shade_read_bases_by_quality;
            filters.minimum_indel_size = st.align.minimum_indel_size;
            filters.minimum_event_label_size = st.align.minimum_event_label_size;
            filters.show_modified_bases = st.align.show_modified_bases;
            filters.show_modification_frequency = st.align.show_modification_frequency;
            filters.minimum_modification_probability = st.align.minimum_modification_probability;
            filters.modified_base_color_mode = st.align.modified_base_color_mode;
            filters.hidden_modification_codes = st
                .align
                .hidden_modification_codes
                .iter()
                .copied()
                .map(modification_code_label)
                .collect();
            self.apply_secondary_alignment_settings(
                st.align.settings_alignment_id,
                AlignmentSettingsDraft {
                    filters,
                    filter_editor: FilterEditor {
                        minimum_mapq: st.align.minimum_mapq,
                        include_secondary: st.align.include_secondary,
                        include_supplementary: st.align.include_supplementary,
                        expression: std::mem::take(&mut st.align.filter_expression),
                        error: st.align.filter_error.take(),
                    },
                    maximum_alignments: st.align.complete_read_limit as usize,
                    apply_filter: st.align.apply_filter,
                },
            );
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
            self.data_status = error.to_string();
        }
        for (track_id, subtrack) in alignment_subtracks_to_toggle {
            self.toggle_alignment_subtrack(track_id, subtrack);
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
            self.data_status = error.to_string();
        }
        if st.lists.metadata_bulk_auxiliary_appearance_requested
            && apply_compatible_auxiliary_appearance(self.tracks.tracks_mut(), &metadata_selected)
        {
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
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
            match self.remove_metadata_selected_tracks(&metadata_selected) {
                Ok(removed) => {
                    self.data_status = format!("Removed {removed} selected tracks");
                }
                Err(error) => self.data_status = error.to_string(),
            }
        }
        if let Some(id) = alignment_track_to_remove
            && let Err(error) = self.remove_alignment_track(id)
        {
            self.data_status = error.to_string();
        }
        if let Some((target, action)) = st.alignment_context_action.take() {
            self.apply_alignment_context_action(target, action);
        }
    }
}
