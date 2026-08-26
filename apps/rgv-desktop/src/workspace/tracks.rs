use super::super::*;

fn updated_track_pane_selection(
    track_order: &[TrackId],
    selected: &[TrackId],
    anchor: Option<TrackId>,
    target: TrackId,
    shift_range: bool,
    toggle: bool,
) -> (Vec<TrackId>, Option<TrackId>) {
    let anchor = anchor
        .filter(|anchor| selected.contains(anchor))
        .or_else(|| selected.first().copied())
        .unwrap_or(target);
    let next = if shift_range {
        alignment_range_selection(track_order, anchor, target)
    } else {
        updated_alignment_selection(selected, target, toggle)
    };
    let next_anchor = if shift_range {
        Some(anchor)
    } else if next.contains(&target) {
        Some(target)
    } else {
        next.last().copied()
    };
    (next, next_anchor)
}

fn alignment_track_group(config: &SessionAlignmentTrack, pane: ui::TrackPane) -> Option<u32> {
    match pane {
        ui::TrackPane::Annotation => config.track_group,
        ui::TrackPane::Reads => config.read_track_group,
    }
}

fn alignment_track_group_collapsed(config: &SessionAlignmentTrack, pane: ui::TrackPane) -> bool {
    match pane {
        ui::TrackPane::Annotation => config.track_group_collapsed,
        ui::TrackPane::Reads => config.read_track_group_collapsed,
    }
}

fn set_alignment_track_group(
    config: &mut SessionAlignmentTrack,
    pane: ui::TrackPane,
    group: Option<u32>,
    collapsed: bool,
) {
    match pane {
        ui::TrackPane::Annotation => {
            config.track_group = group;
            config.track_group_collapsed = collapsed;
        }
        ui::TrackPane::Reads => {
            config.read_track_group = group;
            config.read_track_group_collapsed = collapsed;
        }
    }
}

fn normalize_alignment_track_group_configs(
    configs: &mut [&mut SessionAlignmentTrack],
    pane: ui::TrackPane,
) {
    let mut counts = BTreeMap::<u32, usize>::new();
    let mut collapsed = BTreeSet::<u32>::new();
    for config in configs.iter() {
        if let Some(group) = alignment_track_group(config, pane) {
            *counts.entry(group).or_default() += 1;
            if alignment_track_group_collapsed(config, pane) {
                collapsed.insert(group);
            }
        }
    }
    for config in configs {
        let Some(group) = alignment_track_group(config, pane) else {
            set_alignment_track_group(config, pane, None, false);
            continue;
        };
        if counts.get(&group).copied().unwrap_or_default() < 2 {
            set_alignment_track_group(config, pane, None, false);
        } else {
            set_alignment_track_group(config, pane, Some(group), collapsed.contains(&group));
        }
    }
}

fn assign_alignment_track_group(
    configs: &mut [&mut SessionAlignmentTrack],
    selected: &BTreeSet<TrackId>,
    pane: ui::TrackPane,
) -> Option<usize> {
    let member_count = configs
        .iter()
        .filter(|config| selected.contains(&config.id))
        .count();
    if member_count < 2 {
        return None;
    }
    let used = configs
        .iter()
        .filter_map(|config| alignment_track_group(config, pane))
        .collect::<BTreeSet<_>>();
    let mut group = 1_u32;
    while used.contains(&group) {
        group = group.checked_add(1)?;
    }
    for config in configs.iter_mut() {
        if selected.contains(&config.id) {
            set_alignment_track_group(config, pane, Some(group), false);
        }
    }
    normalize_alignment_track_group_configs(configs, pane);
    Some(member_count)
}

impl Viewer {
    pub(crate) fn has_alignment_engine(&self) -> bool {
        !self.alignment_tracks.is_empty()
    }

    pub(crate) fn primary_alignment_track_id(&self) -> TrackId {
        self.alignment_tracks
            .first()
            .map_or(SYNTHETIC_ALIGNMENT_TRACK_ID, |track| track.config.id)
    }

    pub(crate) fn alignment_query_for_track(&self, id: TrackId) -> Option<&Arc<AlignmentQuery>> {
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == id)
            .and_then(|track| track.projection(self.panels.active().id))
            .and_then(|projection| projection.current_query.as_ref())
    }

    pub(crate) fn primary_alignment_query(&self) -> Option<&Arc<AlignmentQuery>> {
        self.alignment_query_for_track(self.primary_alignment_track_id())
    }

    pub(crate) fn alignment_dictionary_for_track(
        &self,
        id: TrackId,
    ) -> Option<&ReferenceDictionary> {
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == id)
            .and_then(|track| track.dictionary.as_ref())
    }

    pub(crate) fn primary_alignment_dictionary(&self) -> Option<&ReferenceDictionary> {
        self.alignment_tracks
            .first()
            .and_then(|track| track.dictionary.as_ref())
    }

    pub(crate) fn alignment_track_name_for_id(&self, id: TrackId) -> String {
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == id)
            .map_or_else(
                || self.primary_alignment_name().to_owned(),
                |track| track.config.name.clone(),
            )
    }

    pub(crate) fn alignment_track_group_chrome(&self) -> Vec<ui::TrackGroupChrome> {
        [ui::TrackPane::Annotation, ui::TrackPane::Reads]
            .into_iter()
            .flat_map(|pane| {
                let mut groups = BTreeMap::<u32, Vec<TrackId>>::new();
                let mut collapsed = BTreeSet::<u32>::new();
                for track in &self.alignment_tracks {
                    if let Some(group) = alignment_track_group(&track.config, pane) {
                        groups.entry(group).or_default().push(track.config.id);
                        if alignment_track_group_collapsed(&track.config, pane) {
                            collapsed.insert(group);
                        }
                    }
                }
                groups.into_iter().filter_map(move |(id, members)| {
                    let anchor = members.first().copied()?;
                    (members.len() > 1).then(|| ui::TrackGroupChrome {
                        id,
                        pane,
                        anchor,
                        members,
                        collapsed: collapsed.contains(&id),
                    })
                })
            })
            .collect()
    }

    pub(crate) fn group_selected_alignment_tracks(&mut self, pane: ui::TrackPane) -> bool {
        let panel = self.panels.active();
        let selected_tracks = match pane {
            ui::TrackPane::Annotation => &panel.selected_alignment_tracks,
            ui::TrackPane::Reads => &panel.selected_read_tracks,
        };
        let selected = selected_tracks.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() < 2 {
            return false;
        }
        let selected_positions = self
            .alignment_tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| selected.contains(&track.config.id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if selected_positions
            .windows(2)
            .any(|pair| pair[1] != pair[0] + 1)
        {
            self.data_status =
                "Track groups must be contiguous; use Shift-click to select a range".to_owned();
            return false;
        }
        let mut configs = self
            .alignment_tracks
            .iter_mut()
            .map(|track| &mut track.config)
            .collect::<Vec<_>>();
        let Some(member_count) = assign_alignment_track_group(&mut configs, &selected, pane) else {
            return false;
        };
        self.data_status = format!(
            "Grouped {member_count} {} tracks",
            match pane {
                ui::TrackPane::Annotation => "annotation",
                ui::TrackPane::Reads => "read-data",
            }
        );
        true
    }

    pub(crate) fn toggle_alignment_track_group(&mut self, pane: ui::TrackPane, group: u32) -> bool {
        let collapsed = self.alignment_tracks.iter().any(|track| {
            alignment_track_group(&track.config, pane) == Some(group)
                && alignment_track_group_collapsed(&track.config, pane)
        });
        let mut changed = false;
        for track in &mut self.alignment_tracks {
            if alignment_track_group(&track.config, pane) == Some(group) {
                set_alignment_track_group(&mut track.config, pane, Some(group), !collapsed);
                changed = true;
            }
        }
        if changed {
            self.data_status = if collapsed {
                format!("Expanded track group {group}")
            } else {
                format!("Minimized track group {group}")
            };
        }
        changed
    }

    pub(crate) fn ungroup_alignment_tracks(&mut self, pane: ui::TrackPane, group: u32) -> bool {
        let mut changed = false;
        for track in &mut self.alignment_tracks {
            if alignment_track_group(&track.config, pane) == Some(group) {
                set_alignment_track_group(&mut track.config, pane, None, false);
                changed = true;
            }
        }
        if changed {
            self.data_status = format!("Ungrouped track group {group}");
        }
        changed
    }

    pub(crate) fn normalize_alignment_track_groups(&mut self) {
        let mut configs = self
            .alignment_tracks
            .iter_mut()
            .map(|track| &mut track.config)
            .collect::<Vec<_>>();
        normalize_alignment_track_group_configs(&mut configs, ui::TrackPane::Annotation);
        normalize_alignment_track_group_configs(&mut configs, ui::TrackPane::Reads);
    }

    pub(crate) fn alignment_filters_for_track(&self, id: TrackId) -> SessionFilters {
        if id == self.primary_alignment_track_id() {
            return self.live_session_filters();
        }
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == id)
            .map_or_else(SessionFilters::default, |track| {
                track.config.filters.clone()
            })
    }

    pub(crate) fn shared_sashimi_maximum(&self, count_mode: SashimiCountMode) -> Option<f64> {
        let selected = &self.panels.active().selected_alignment_tracks;
        let use_selected = selected.len() > 1;
        let mut participant_count = 0_usize;
        let mut maximum = 0.0_f64;
        for track in self.alignment_tracks.iter().filter(|track| {
            track.config.visible && (!use_selected || selected.contains(&track.config.id))
        }) {
            let filters = self.alignment_filters_for_track(track.config.id);
            if filters.sashimi_scale_mode != SashimiScaleMode::Shared
                || filters.sashimi_count_mode != count_mode
            {
                continue;
            }
            let Some(query) = self.alignment_query_for_track(track.config.id) else {
                continue;
            };
            if count_mode == SashimiCountMode::CountsPerMillion
                && query
                    .junctions
                    .provenance
                    .normalization_mapped_records
                    .is_none()
            {
                return None;
            }
            participant_count = participant_count.saturating_add(1);
            for junction in query
                .junctions
                .junctions
                .iter()
                .filter(|junction| junction.depth >= filters.minimum_junction_depth)
            {
                maximum = maximum.max(junction_count_value(&query.junctions, junction, count_mode));
            }
        }
        (participant_count > 0 && maximum > 0.0).then_some(maximum)
    }

    pub(crate) fn sashimi_export_bundle(&self) -> SashimiExportBundle {
        let selected = &self.panels.active().selected_alignment_tracks;
        let use_selected = !selected.is_empty();
        let mut omitted_tracks = 0_usize;
        let mut tracks = Vec::new();
        for track in self.alignment_tracks.iter().filter(|track| {
            track.config.visible && (!use_selected || selected.contains(&track.config.id))
        }) {
            let Some(query) = self.alignment_query_for_track(track.config.id) else {
                continue;
            };
            if tracks.len() == MAX_SASHIMI_EXPORT_TRACKS {
                omitted_tracks = omitted_tracks.saturating_add(1);
                continue;
            }
            tracks.push(SashimiExportTrack {
                id: track.config.id,
                name: self.alignment_track_name_for_id(track.config.id),
                query: Arc::clone(query),
                filters: self.alignment_filters_for_track(track.config.id),
            });
        }
        SashimiExportBundle {
            tracks,
            omitted_tracks,
            viewport: self.panels.active().viewport,
            contig_name: self.panels.active().contig_name.clone(),
            annotations: junction_annotation_context(
                &self.tracks,
                &self.panels.active().track_snapshots,
                self.panels.active().track_snapshot_interval,
                self.panels.active().viewport.interval(),
            ),
        }
    }

    pub(crate) fn select_track_key(&mut self, key: TrackFrameKey, pane: ui::TrackPane) {
        let shift_range = self.modifiers.shift_key();
        let toggle = self.modifiers.control_key() || self.modifiers.super_key();
        match (key, pane) {
            (TrackFrameKey::Alignment(id), ui::TrackPane::Reads) => {
                let panel = self.panels.active();
                let selected = panel.selected_read_tracks.clone();
                let track_order = panel
                    .track_frames
                    .iter()
                    .filter(|frame| frame.frozen_bottom.is_some())
                    .filter_map(|frame| match frame.key {
                        TrackFrameKey::Alignment(track_id) => Some(track_id),
                        TrackFrameKey::Reference
                        | TrackFrameKey::Group(_)
                        | TrackFrameKey::Auxiliary(_) => None,
                    })
                    .collect::<Vec<_>>();
                let (next, next_anchor) = updated_track_pane_selection(
                    &track_order,
                    &selected,
                    panel.read_selection_anchor,
                    id,
                    shift_range,
                    toggle,
                );
                let panel = self.panels.active_mut();
                panel.selected_alignment_tracks.clear();
                panel.alignment_selection_anchor = None;
                panel.selected_read_tracks = next;
                panel.read_selection_anchor = next_anchor;
                panel.selected_track = panel
                    .selected_read_tracks
                    .contains(&id)
                    .then_some(TrackFrameKey::Alignment(id))
                    .or_else(|| {
                        panel
                            .selected_read_tracks
                            .last()
                            .copied()
                            .map(TrackFrameKey::Alignment)
                    });
            }
            (_, ui::TrackPane::Reads) => {}
            (TrackFrameKey::Group(_), ui::TrackPane::Annotation) => {}
            (TrackFrameKey::Alignment(id), ui::TrackPane::Annotation) => {
                let selected = self.panels.active().selected_alignment_tracks.clone();
                let track_order = self
                    .alignment_tracks
                    .iter()
                    .map(|track| track.config.id)
                    .collect::<Vec<_>>();
                let (next, next_anchor) = updated_track_pane_selection(
                    &track_order,
                    &selected,
                    self.panels.active().alignment_selection_anchor,
                    id,
                    shift_range,
                    toggle,
                );
                let panel = self.panels.active_mut();
                panel.selected_read_tracks.clear();
                panel.read_selection_anchor = None;
                panel.selected_alignment_tracks = next;
                panel.alignment_selection_anchor = next_anchor;
                panel.selected_track = panel
                    .selected_alignment_tracks
                    .contains(&id)
                    .then_some(TrackFrameKey::Alignment(id))
                    .or_else(|| {
                        panel
                            .selected_alignment_tracks
                            .last()
                            .copied()
                            .map(TrackFrameKey::Alignment)
                    });
            }
            (TrackFrameKey::Reference | TrackFrameKey::Auxiliary(_), ui::TrackPane::Annotation) => {
                let panel = self.panels.active_mut();
                panel.selected_alignment_tracks.clear();
                panel.alignment_selection_anchor = None;
                panel.selected_read_tracks.clear();
                panel.read_selection_anchor = None;
                panel.selected_track = Some(key);
            }
        }
        self.display_dirty = true;
    }

    pub(crate) fn apply_bulk_alignment_appearance(
        &mut self,
        source_id: TrackId,
        selected_ids: &[TrackId],
    ) -> Result<()> {
        let Some(source) = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == source_id)
            .map(|track| track.config.filters.clone())
        else {
            return Ok(());
        };
        let primary_id = self.primary_alignment_track_id();
        let mut query_requested = false;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| selected_ids.contains(&track.config.id))
        {
            let collect_modifications_changed = track.config.filters.show_modification_frequency
                != source.show_modification_frequency;
            copy_compatible_alignment_appearance(&mut track.config.filters, &source);
            for projection in track.projections.values_mut() {
                projection.alignment_rows = None;
                projection.cached_layout = None;
            }
            if collect_modifications_changed && track.config.visible {
                for projection in track.projections.values_mut() {
                    projection.clock.advance();
                    projection.pending_query = true;
                }
                query_requested = true;
            }
        }
        if selected_ids.contains(&primary_id) {
            self.activate_first_alignment_track()?;
        }
        if query_requested {
            self.panels.active_mut().pending_queries.alignment = true;
            self.dispatch_alignment_query();
        }
        self.rebuild_genomic_display();
        self.display_dirty = true;
        Ok(())
    }

    pub(crate) fn apply_bulk_alignment_state(
        &mut self,
        selected_ids: &[TrackId],
        visibility: Option<bool>,
        collapsed: Option<bool>,
    ) {
        let mut query_membership_changed = Vec::new();
        let mut changed = false;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| selected_ids.contains(&track.config.id))
        {
            if let Some(visible) = visibility {
                if track.config.visible != visible {
                    query_membership_changed.push(track.config.id);
                }
                changed |= track.config.visible != visible;
                track.config.visible = visible;
            }
            if let Some(is_collapsed) = collapsed {
                changed |= track.config.collapsed != is_collapsed;
                track.config.collapsed = is_collapsed;
            }
        }
        if !query_membership_changed.is_empty() {
            self.invalidate_automation_alignment_layouts(Some(&query_membership_changed), true);
            self.request_all_alignment_queries();
        }
        if changed {
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
    }

    pub(crate) fn apply_metadata_bulk_state(
        &mut self,
        selected: &BTreeSet<TrackId>,
        visibility: Option<bool>,
        collapsed: Option<bool>,
        height: Option<f32>,
        rename_to_sample: bool,
    ) {
        let previous_auxiliary_query_membership = auxiliary_query_track_ids(self.tracks.tracks())
            .into_iter()
            .collect::<HashSet<_>>();
        let alignment_ids = self
            .alignment_tracks
            .iter()
            .filter(|track| selected.contains(&track.config.id))
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        if visibility.is_some() || collapsed.is_some() {
            self.apply_bulk_alignment_state(&alignment_ids, visibility, collapsed);
        }
        let mut changed = false;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| selected.contains(&track.config.id))
        {
            if let Some(height) = height {
                let height = height.clamp(40.0, 1_200.0);
                changed |= (track.config.height - height).abs() > f32::EPSILON;
                track.config.height = height;
            }
            if rename_to_sample
                && let Some(sample_key) = &track.config.sample_key
                && track.config.name != *sample_key
            {
                track.config.name.clone_from(sample_key);
                changed = true;
            }
        }
        for track in self
            .tracks
            .tracks_mut()
            .iter_mut()
            .filter(|track| selected.contains(&track.id))
        {
            if let Some(visible) = visibility {
                changed |= track.visible != visible;
                track.visible = visible;
            }
            if let Some(collapsed) = collapsed {
                changed |= track.collapsed != collapsed;
                track.collapsed = collapsed;
            }
            if let Some(height) = height
                && !matches!(track.kind, TrackKind::Mutation | TrackKind::CopyNumber)
            {
                let height = height.clamp(40.0, 600.0);
                changed |= (track.height - height).abs() > f32::EPSILON;
                track.height = height;
            }
            if rename_to_sample
                && let Some(sample_key) = &track.sample_key
                && track.name != *sample_key
            {
                track.name.clone_from(sample_key);
                changed = true;
            }
        }
        let auxiliary_query_membership_changed = previous_auxiliary_query_membership
            != auxiliary_query_track_ids(self.tracks.tracks())
                .into_iter()
                .collect::<HashSet<_>>();
        if auxiliary_query_membership_changed {
            self.invalidate_and_request_all_auxiliary_track_queries();
        }
        if changed {
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
        }
    }

    pub(crate) fn apply_metadata_bulk_alignment_settings(
        &mut self,
        selected: &BTreeSet<TrackId>,
    ) -> Result<()> {
        let Some(source) = self
            .alignment_tracks
            .iter()
            .find(|track| selected.contains(&track.config.id))
            .map(|track| track.config.clone())
        else {
            return Ok(());
        };
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| selected.contains(&track.config.id) && track.config.id != source.id)
        {
            track.config.filters.clone_from(&source.filters);
            track.config.collapsed = source.collapsed;
            track.config.height = source.height;
            track.config.maximum_alignments = source.maximum_alignments;
            for projection in track.projections.values_mut() {
                projection.clock.advance();
                projection.pending_query = track.config.visible;
                projection.alignment_rows = None;
                projection.cached_layout = None;
            }
        }
        if selected.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.request_all_panel_queries();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(())
    }

    pub(crate) fn apply_metadata_quantitative_action(
        &mut self,
        selected: &BTreeSet<TrackId>,
        action: QuantitativeBulkAction,
    ) {
        let changed = apply_quantitative_bulk_action(self.tracks.tracks_mut(), selected, action);
        if changed {
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
        }
    }

    pub(crate) fn remove_auxiliary_tracks(&mut self, ids: &[TrackId]) -> Result<usize> {
        let requested = ids.iter().copied().collect::<HashSet<_>>();
        let actual = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| requested.contains(&track.id))
            .map(|track| track.id)
            .collect::<Vec<_>>();
        if actual.is_empty() {
            return Ok(0);
        }
        if let Some(engine) = &self.track_engine {
            // The actor request is queued before any replacement query. Do not
            // mutate visible state if bounded backpressure rejects the command.
            engine.try_remove_many(actual.clone())?;
        }

        let removed = actual.iter().copied().collect::<HashSet<_>>();
        for id in &actual {
            let _ = self.tracks.remove(*id);
            self.automation_auxiliary_open_state.remove(id);
            self.metadata_selected_tracks.remove(id);
        }
        clear_removed_quantitative_dependencies(self.tracks.tracks_mut(), &removed);
        self.annotation_search_results
            .hits
            .retain(|hit| !removed.contains(&hit.track_id));
        // Results already in flight were computed against the old actor set.
        self.annotation_search_generation = self.annotation_search_generation.saturating_add(1);
        self.annotation_navigation_generation =
            self.annotation_navigation_generation.saturating_add(1);
        self.pending_annotation_navigation = None;
        if matches!(
            self.open_panel,
            Some(UiPanel::TrackSettings(TrackFrameKey::Auxiliary(id))) if removed.contains(&id)
        ) {
            self.open_panel = None;
        }
        if self
            .resizing_track
            .is_some_and(|(id, _)| removed.contains(&id))
        {
            self.resizing_track = None;
        }

        let has_engine = self.track_engine.is_some();
        for panel in self.panels.iter_mut() {
            // Invalidate any unread result produced before the actor processed
            // Remove, while retaining unaffected snapshots for the next frame.
            panel.generation_clock.advance();
            panel.pending_queries.tracks = has_engine;
            clear_removed_auxiliary_panel_state(panel, &removed);
        }
        self.request_all_auxiliary_track_queries();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(actual.len())
    }

    pub(crate) fn remove_metadata_selected_tracks(
        &mut self,
        selected: &BTreeSet<TrackId>,
    ) -> Result<usize> {
        let auxiliary_ids = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| selected.contains(&track.id))
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let alignment_ids = self
            .alignment_tracks
            .iter()
            .rev()
            .filter(|track| selected.contains(&track.config.id))
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        let mut removed = self.remove_auxiliary_tracks(&auxiliary_ids)?;
        for id in alignment_ids {
            removed += usize::from(self.remove_alignment_track(id)?);
        }
        if removed > 0 {
            self.metadata_selected_tracks.clear();
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
        }
        Ok(removed)
    }

    pub(crate) fn advance_alignment_generations(&self, panel_id: PanelId) {
        for track in &self.alignment_tracks {
            if let Some(projection) = track.projection(panel_id) {
                projection.clock.advance();
            }
        }
    }

    pub(crate) fn apply_secondary_alignment_settings(
        &mut self,
        id: TrackId,
        update: AlignmentSettingsDraft,
    ) {
        let Some(track) = self
            .alignment_tracks
            .iter_mut()
            .find(|track| track.config.id == id)
        else {
            return;
        };
        let outcome = commit_alignment_settings_draft(
            &mut track.config,
            &mut track.filter_editor,
            &mut track.active_filters,
            update,
            self.workspace_alignment_limit,
        );
        if outcome.configuration_changed {
            for projection in track.projections.values_mut() {
                projection.alignment_rows = None;
                projection.cached_layout = None;
            }
        }
        if outcome.query_changed && track.config.visible {
            for projection in track.projections.values_mut() {
                projection.clock.advance();
                projection.pending_query = true;
            }
            self.panels.active_mut().pending_queries.alignment = true;
        }
        if outcome.configuration_changed {
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        if outcome.query_changed {
            self.dispatch_alignment_query();
        }
    }
}

#[cfg(test)]
mod track_group_tests {
    use super::*;

    fn config(id: u64) -> SessionAlignmentTrack {
        SessionAlignmentTrack::new(
            TrackId::new(id),
            format!("track-{id}"),
            TrackSource::Local {
                path: format!("track-{id}.bam"),
            },
        )
    }

    #[test]
    fn grouping_selected_tracks_allocates_a_new_saved_group() {
        let mut configs = [config(1), config(2), config(3)];
        configs[0].track_group = Some(1);
        configs[1].track_group = Some(1);
        let selected = [TrackId::new(2), TrackId::new(3)]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut refs = configs.iter_mut().collect::<Vec<_>>();

        assert_eq!(
            assign_alignment_track_group(&mut refs, &selected, ui::TrackPane::Annotation),
            Some(2)
        );
        assert_eq!(configs[0].track_group, None, "orphaned groups are removed");
        assert_eq!(configs[1].track_group, Some(2));
        assert_eq!(configs[2].track_group, Some(2));
        assert!(!configs[1].track_group_collapsed);
    }

    #[test]
    fn normalization_synchronizes_group_minimization_and_clears_singletons() {
        let mut configs = [config(1), config(2), config(3)];
        configs[0].track_group = Some(4);
        configs[0].track_group_collapsed = true;
        configs[1].track_group = Some(4);
        configs[2].track_group = Some(9);
        configs[2].track_group_collapsed = true;
        let mut refs = configs.iter_mut().collect::<Vec<_>>();

        normalize_alignment_track_group_configs(&mut refs, ui::TrackPane::Annotation);

        assert!(
            configs[..2]
                .iter()
                .all(|config| config.track_group == Some(4))
        );
        assert!(
            configs[..2]
                .iter()
                .all(|config| config.track_group_collapsed)
        );
        assert_eq!(configs[2].track_group, None);
        assert!(!configs[2].track_group_collapsed);
    }

    #[test]
    fn assigning_a_read_group_does_not_group_or_rewrite_annotations() {
        let mut configs = [config(1), config(2), config(3)];
        configs[0].track_group = Some(7);
        configs[1].track_group = Some(7);
        let selected = [TrackId::new(2), TrackId::new(3)]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut refs = configs.iter_mut().collect::<Vec<_>>();

        assert_eq!(
            assign_alignment_track_group(&mut refs, &selected, ui::TrackPane::Reads),
            Some(2)
        );

        assert_eq!(configs[0].track_group, Some(7));
        assert_eq!(configs[1].track_group, Some(7));
        assert_eq!(configs[2].track_group, None);
        assert_eq!(configs[0].read_track_group, None);
        assert_eq!(configs[1].read_track_group, Some(1));
        assert_eq!(configs[2].read_track_group, Some(1));
    }

    #[test]
    fn read_pane_selection_supports_range_and_additive_toggle_semantics() {
        let order = (1..=5).map(TrackId::new).collect::<Vec<_>>();
        let (selected, anchor) =
            updated_track_pane_selection(&order, &[], None, TrackId::new(2), false, false);
        assert_eq!(selected, [TrackId::new(2)]);
        assert_eq!(anchor, Some(TrackId::new(2)));

        let (selected, anchor) =
            updated_track_pane_selection(&order, &selected, anchor, TrackId::new(5), true, false);
        assert_eq!(
            selected,
            [
                TrackId::new(2),
                TrackId::new(3),
                TrackId::new(4),
                TrackId::new(5),
            ]
        );
        assert_eq!(anchor, Some(TrackId::new(2)));

        let (selected, anchor) =
            updated_track_pane_selection(&order, &selected, anchor, TrackId::new(3), false, true);
        assert_eq!(
            selected,
            [TrackId::new(2), TrackId::new(4), TrackId::new(5)]
        );
        assert_eq!(anchor, Some(TrackId::new(5)));
    }
}
