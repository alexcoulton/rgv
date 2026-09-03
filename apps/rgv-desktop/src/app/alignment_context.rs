use super::super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentContextEffect {
    Appearance,
    Arrangement,
    ViewFilter,
    ReadAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentContextTarget {
    pub(crate) panel_id: PanelId,
    pub(crate) track_id: TrackId,
    pub(crate) committed_target_revision: u64,
    pub(crate) scene_revision: rgv_alignment_scene::SceneRevision,
    pub(crate) genomic_position: u64,
    pub(crate) hit: Option<AlignmentHit>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlignmentContextMenuState {
    pub(crate) target: AlignmentContextTarget,
    pub(crate) screen_position: egui::Pos2,
}

#[derive(Clone, Debug)]
pub(crate) struct AlignmentContextMenuView {
    pub(crate) filters: SessionFilters,
    pub(crate) contig_name: String,
    pub(crate) read_name: Option<String>,
    pub(crate) mate_available: bool,
    pub(crate) supplementary_segments_available: bool,
    pub(crate) sequence_available: bool,
    pub(crate) eligible_records: Option<usize>,
    pub(crate) matching_records: Option<usize>,
    pub(crate) reference_configured: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentQuickPredicate {
    Substitution,
    Insertion,
    Deletion,
    Indel,
    SoftClip,
    Splice,
    Duplicate,
    Secondary,
    Supplementary,
    MappingQuality30,
    InsertionAtPosition,
    DeletionAtPosition,
    SubstitutionAtPosition,
}

impl AlignmentQuickPredicate {
    #[must_use]
    pub(crate) const fn requires_reference(self) -> bool {
        matches!(self, Self::Substitution | Self::SubstitutionAtPosition)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AlignmentContextAction {
    InspectRead,
    SelectRead,
    CopyReadName,
    CopyReadDetails,
    CopyReadSequence,
    GoToMate,
    OpenMateInNewPanel,
    OpenSupplementarySegments,
    SetColor(AlignmentColorMode),
    SetGroup(AlignmentGroupMode),
    SetSort(AlignmentSortMode),
    ReverseGroupOrder,
    ReverseSortOrder,
    SetStacking(ReadStackingMode),
    TogglePairs,
    ResetArrangement,
    SetBaseDisplay(ReadBaseDisplayMode),
    ToggleBaseQualityShading,
    AddFilter(AlignmentQuickPredicate),
    RemoveFilter(AlignmentQuickPredicate),
    AddHighlight(AlignmentQuickPredicate),
    RemoveHighlight(AlignmentQuickPredicate),
    ClearFilters,
    ClearHighlights,
    OpenTrackSettings,
}

fn alignment_context_component_kind(component: AlignmentComponent) -> &'static str {
    match component {
        AlignmentComponent::ReadSpan => "read_span",
        AlignmentComponent::AlignedSegment { .. } => "aligned_segment",
        AlignmentComponent::Insertion { .. } => "insertion",
        AlignmentComponent::Deletion { .. } => "deletion",
        AlignmentComponent::ReferenceSkip { .. } => "reference_skip",
        AlignmentComponent::SoftClip { .. } => "soft_clip",
        AlignmentComponent::HardClip { .. } => "hard_clip",
        AlignmentComponent::Padding { .. } => "padding",
        AlignmentComponent::DirectionTerminal => "direction_terminal",
        AlignmentComponent::PairConnector { .. } => "pair_connector",
        AlignmentComponent::Label { .. } => "label",
        AlignmentComponent::ModifiedBase { .. } => "modified_base",
        AlignmentComponent::BisulfiteBase { .. } => "bisulfite_base",
        AlignmentComponent::SmrtKinetics { .. } => "smrt_kinetics",
        AlignmentComponent::UltimaIndel { .. } => "ultima_indel",
        AlignmentComponent::SbxInsertion { .. } => "sbx_insertion",
        AlignmentComponent::SbxTailBase { .. } => "sbx_tail_base",
        AlignmentComponent::SpecialistUnavailable { .. } => "specialist_unavailable",
        AlignmentComponent::Base { .. } => "base",
    }
}

impl AlignmentContextAction {
    #[must_use]
    pub(crate) const fn effect(self) -> AlignmentContextEffect {
        match self {
            Self::SetColor(_) | Self::SetBaseDisplay(_) | Self::ToggleBaseQualityShading => {
                AlignmentContextEffect::Appearance
            }
            Self::SetGroup(_)
            | Self::SetSort(_)
            | Self::ReverseGroupOrder
            | Self::ReverseSortOrder
            | Self::SetStacking(_)
            | Self::TogglePairs
            | Self::ResetArrangement => AlignmentContextEffect::Arrangement,
            Self::AddFilter(_)
            | Self::RemoveFilter(_)
            | Self::AddHighlight(_)
            | Self::RemoveHighlight(_)
            | Self::ClearFilters
            | Self::ClearHighlights => AlignmentContextEffect::ViewFilter,
            Self::InspectRead
            | Self::SelectRead
            | Self::CopyReadName
            | Self::CopyReadDetails
            | Self::CopyReadSequence
            | Self::GoToMate
            | Self::OpenMateInNewPanel
            | Self::OpenSupplementarySegments
            | Self::OpenTrackSettings => AlignmentContextEffect::ReadAction,
        }
    }
}

impl Viewer {
    fn reject_alignment_context_menu_open(
        &mut self,
        invocation: &'static str,
        reason: &'static str,
    ) -> bool {
        self.record_manual_diagnostic_event(
            "alignment_context",
            "menu_open_rejected",
            serde_json::json!({
                "invocation": invocation,
                "reason": reason,
                "active_panel_id": self.panels.active().id.get(),
                "cursor_x_pixels": self.cursor.x,
                "cursor_y_pixels": self.cursor.y,
            }),
        );
        false
    }

    fn record_alignment_context_action_result(
        &mut self,
        target: AlignmentContextTarget,
        action: AlignmentContextAction,
        outcome: &'static str,
        reason: Option<&'static str>,
    ) {
        let filter_state = self.alignment_filter_diagnostic_state(target.panel_id, target.track_id);
        self.record_manual_diagnostic_event(
            "alignment_context",
            "action_finished",
            serde_json::json!({
                "panel_id": target.panel_id.get(),
                "track_id": target.track_id.get(),
                "committed_target_revision": target.committed_target_revision,
                "scene_revision": {
                    "high": target.scene_revision.fingerprint().high(),
                    "low": target.scene_revision.fingerprint().low(),
                },
                "genomic_position": target.genomic_position,
                "hit_present": target.hit.is_some(),
                "action": format!("{action:?}"),
                "effect": format!("{:?}", action.effect()),
                "outcome": outcome,
                "reason": reason,
                "filter_state_after": filter_state,
            }),
        );
    }

    pub(crate) fn open_alignment_context_menu_at_cursor(
        &mut self,
        invocation: &'static str,
    ) -> bool {
        if !self.cursor_over_unobstructed_genomic_canvas() {
            return self.reject_alignment_context_menu_open(invocation, "cursor_not_over_canvas");
        }
        let Some(panel_id) = self.panel_at_cursor() else {
            return self.reject_alignment_context_menu_open(invocation, "no_panel_at_cursor");
        };
        self.activate_view_panel(panel_id);
        let scale = self.window.scale_factor().max(f64::EPSILON);
        let point = egui::pos2(
            (self.cursor.x / scale) as f32,
            (self.cursor.y / scale) as f32,
        );
        let track_id = self
            .panels
            .active()
            .live_read_panel_at_y(point.y)
            .or_else(|| {
                self.visible_track_frame_at_screen_y(point.y, |frame| {
                    frame.channel == ui::TrackPane::Annotation
                        && matches!(frame.key, TrackFrameKey::Alignment(_))
                })
                .and_then(|(frame, _)| match frame.key {
                    TrackFrameKey::Alignment(track_id) => Some(track_id),
                    TrackFrameKey::Reference
                    | TrackFrameKey::Group { .. }
                    | TrackFrameKey::Auxiliary(_) => None,
                })
            });
        let Some(track_id) = track_id else {
            return self.reject_alignment_context_menu_open(invocation, "no_alignment_lane");
        };
        let hit = self
            .alignment_scene_hit_at_cursor()
            .filter(|hit| hit.track == track_id);
        let Some(presented) = self.presented_alignment_lane(panel_id, track_id) else {
            return self
                .reject_alignment_context_menu_open(invocation, "no_committed_alignment_lane");
        };
        let Some(snapshot) = presented.lane.snapshot.as_ref() else {
            return self
                .reject_alignment_context_menu_open(invocation, "committed_lane_has_no_snapshot");
        };
        let camera_geometry = presented.lane.read_camera.map_or_else(
            || {
                presented
                    .lane
                    .derived_lanes
                    .iter()
                    .find(|lane| {
                        f64::from(point.y) >= lane.clip.y
                            && f64::from(point.y) < lane.clip.y + lane.clip.height
                    })
                    .or_else(|| presented.lane.derived_lanes.first())
                    .map(|lane| (presented.lane.preparation_camera.viewport(), lane.clip))
            },
            |camera| Some((camera.viewport(), camera.clip())),
        );
        let Some((viewport, clip)) = camera_geometry else {
            return self.reject_alignment_context_menu_open(invocation, "lane_has_no_camera");
        };
        let genomic_position = viewport
            .genomic_at_pixel((f64::from(point.x) - clip.x).clamp(0.0, clip.width))
            .floor() as u64;
        let genomic_position = genomic_position.clamp(
            viewport.interval().start(),
            viewport.interval().end().saturating_sub(1),
        );
        let target = AlignmentContextTarget {
            panel_id,
            track_id,
            committed_target_revision: presented.target_revision,
            scene_revision: snapshot.reads().revision(),
            genomic_position,
            hit,
        };
        self.close_alignment_context_menu("replaced_by_new_invocation");
        self.alignment_context_menu = Some(AlignmentContextMenuState {
            target,
            screen_position: point,
        });
        let filter_state = self.alignment_filter_diagnostic_state(panel_id, track_id);
        self.record_manual_diagnostic_event(
            "alignment_context",
            "menu_opened",
            serde_json::json!({
                "invocation": invocation,
                "panel_id": panel_id.get(),
                "track_id": track_id.get(),
                "committed_target_revision": target.committed_target_revision,
                "scene_revision": {
                    "high": target.scene_revision.fingerprint().high(),
                    "low": target.scene_revision.fingerprint().low(),
                },
                "genomic_position": genomic_position,
                "hit_present": hit.is_some(),
                "hit_component": hit.map(|hit| alignment_context_component_kind(hit.component)),
                "filter_state": filter_state,
            }),
        );
        self.window.request_redraw();
        true
    }

    pub(crate) fn alignment_context_target_is_current(
        &self,
        target: AlignmentContextTarget,
    ) -> bool {
        let Some(presented) = self.presented_alignment_lane(target.panel_id, target.track_id)
        else {
            return false;
        };
        let Some(scene) = presented
            .lane
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.reads())
        else {
            return false;
        };
        presented.target_revision == target.committed_target_revision
            && scene.revision() == target.scene_revision
            && target.hit.is_none_or(|hit| {
                hit.scene_revision == scene.revision() && scene.contains_handle(hit.handle)
            })
    }

    pub(crate) fn close_alignment_context_menu(&mut self, reason: &'static str) -> bool {
        let Some(menu) = self.alignment_context_menu.take() else {
            return false;
        };
        self.record_manual_diagnostic_event(
            "alignment_context",
            "menu_closed",
            serde_json::json!({
                "reason": reason,
                "panel_id": menu.target.panel_id.get(),
                "track_id": menu.target.track_id.get(),
                "committed_target_revision": menu.target.committed_target_revision,
                "scene_revision": {
                    "high": menu.target.scene_revision.fingerprint().high(),
                    "low": menu.target.scene_revision.fingerprint().low(),
                },
                "genomic_position": menu.target.genomic_position,
                "target_current": self.alignment_context_target_is_current(menu.target),
            }),
        );
        true
    }

    fn select_alignment_context_read(&mut self, target: AlignmentContextTarget) -> bool {
        let Some(hit) = target.hit else {
            return false;
        };
        let Some(record) = self.alignment_scene_record_for_hit(target.panel_id, hit) else {
            return false;
        };
        self.activate_view_panel(target.panel_id);
        self.clear_promoted_read();
        let chain = selected_supplementary_chain(&record, &self.panels.active().contig_name);
        let panel = self.panels.active_mut();
        panel.selected_alignment_track = Some(target.track_id);
        panel.selected_alignment_hit = Some(hit);
        panel.selected_pileup_position = None;
        panel.selected_modification_position = None;
        panel.selected_junction = None;
        panel.selected_junction_component = None;
        panel.selected_chain = chain;
        replace_panel_track_selection(
            panel,
            TrackFrameKey::Alignment(target.track_id),
            ui::TrackPane::Reads,
        );
        self.retain_active_read_selection_for_query_refresh();
        self.bump_workspace_revision();
        self.synchronize_alignment_scene_interactions(target.panel_id);
        self.prepare_alignment_scene_frame();
        self.mark_display_dirty("explicit display invalidation");
        true
    }

    fn refresh_alignment_context_track(&mut self, track_id: TrackId, reason: &'static str) {
        self.remember_current_locus();
        self.bump_workspace_revision();
        self.invalidate_navigation_contract(reason);
        let active = self.panels.active().id;
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for panel_id in panel_ids {
            self.panels.activate(panel_id);
            if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            {
                track.projection_mut(panel_id).advance_scene_generation();
            }
            self.request_alignment_scene_camera_update(track_id);
        }
        self.panels.activate(active);
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
    }

    pub(crate) fn apply_alignment_context_action(
        &mut self,
        target: AlignmentContextTarget,
        action: AlignmentContextAction,
    ) {
        let filter_state_before =
            self.alignment_filter_diagnostic_state(target.panel_id, target.track_id);
        self.record_manual_diagnostic_event(
            "alignment_context",
            "action_requested",
            serde_json::json!({
                "panel_id": target.panel_id.get(),
                "track_id": target.track_id.get(),
                "committed_target_revision": target.committed_target_revision,
                "scene_revision": {
                    "high": target.scene_revision.fingerprint().high(),
                    "low": target.scene_revision.fingerprint().low(),
                },
                "genomic_position": target.genomic_position,
                "hit_present": target.hit.is_some(),
                "action": format!("{action:?}"),
                "effect": format!("{:?}", action.effect()),
                "target_current": self.alignment_context_target_is_current(target),
                "filter_state_before": filter_state_before,
            }),
        );
        if !self.alignment_context_target_is_current(target) {
            "That alignment menu referred to an older view; no action was applied"
                .clone_into(&mut self.data_status);
            self.record_alignment_context_action_result(
                target,
                action,
                "rejected",
                Some("stale_committed_target"),
            );
            return;
        }
        let compatible_reference_available = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == target.track_id)
            .and_then(|track| track.dictionary.as_ref())
            .and_then(|dictionary| self.compatible_alignment_reference_revision(dictionary))
            .is_some();
        if matches!(
            action,
            AlignmentContextAction::AddFilter(predicate)
                | AlignmentContextAction::AddHighlight(predicate)
                if predicate.requires_reference() && !compatible_reference_available
        ) {
            "Substitution predicates require a matching reference FASTA"
                .clone_into(&mut self.data_status);
            self.record_alignment_context_action_result(
                target,
                action,
                "rejected",
                Some("reference_unavailable"),
            );
            return;
        }
        let mut action_outcome = "applied";
        let mut action_reason = None;
        match action {
            AlignmentContextAction::InspectRead | AlignmentContextAction::SelectRead => {
                if !self.select_alignment_context_read(target) {
                    "No read was selected at that position".clone_into(&mut self.data_status);
                    action_outcome = "no_op";
                    action_reason = Some("no_read_at_target");
                }
            }
            AlignmentContextAction::CopyReadName => {
                let Some(hit) = target.hit else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("no_read_at_target"),
                    );
                    return;
                };
                let Some(record) = self.alignment_scene_record_for_hit(target.panel_id, hit) else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("read_not_retained"),
                    );
                    return;
                };
                self.egui_context
                    .copy_text(String::from_utf8_lossy(&record.read_name).into_owned());
            }
            AlignmentContextAction::CopyReadDetails => {
                let Some(hit) = target.hit else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("no_read_at_target"),
                    );
                    return;
                };
                let Some(record) = self.alignment_scene_record_for_hit(target.panel_id, hit) else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("read_not_retained"),
                    );
                    return;
                };
                self.egui_context.copy_text(format!(
                    "{}\t{}:{}-{}\tflags=0x{:04x}\tmapq={}\tcigar={}",
                    String::from_utf8_lossy(&record.read_name),
                    self.panels
                        .get(target.panel_id)
                        .map(|panel| panel.contig_name.as_str())
                        .unwrap_or("?"),
                    record.start.saturating_add(1),
                    record.end,
                    record.flags.bits(),
                    record
                        .mapping_quality
                        .map_or(".".to_owned(), |value| value.to_string()),
                    format_cigar(&record.cigar),
                ));
            }
            AlignmentContextAction::CopyReadSequence => {
                let Some(hit) = target.hit else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("no_read_at_target"),
                    );
                    return;
                };
                let Some(record) = self.alignment_scene_record_for_hit(target.panel_id, hit) else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("read_not_retained"),
                    );
                    return;
                };
                if let SequenceData::Present { bases, .. } = record.sequence {
                    self.egui_context
                        .copy_text(String::from_utf8_lossy(&bases).into_owned());
                } else {
                    action_outcome = "no_op";
                    action_reason = Some("sequence_unavailable");
                }
            }
            AlignmentContextAction::GoToMate => {
                if self.select_alignment_context_read(target) {
                    if let Err(error) = self.go_to_selected_mate_in_current_panel() {
                        self.data_status = error.to_string();
                        action_outcome = "failed";
                        action_reason = Some("mate_navigation_failed");
                    }
                } else {
                    action_outcome = "no_op";
                    action_reason = Some("no_read_at_target");
                }
            }
            AlignmentContextAction::OpenMateInNewPanel => {
                if self.select_alignment_context_read(target) {
                    if let Err(error) = self.open_selected_mate_in_new_panel() {
                        self.data_status = error.to_string();
                        action_outcome = "failed";
                        action_reason = Some("mate_panel_open_failed");
                    }
                } else {
                    action_outcome = "no_op";
                    action_reason = Some("no_read_at_target");
                }
            }
            AlignmentContextAction::OpenSupplementarySegments => {
                if self.select_alignment_context_read(target) {
                    match self.open_all_chain_segments_in_panels() {
                        Ok((opened, requested)) => {
                            self.data_status = format!(
                                "Opened {opened} of {requested} supplementary segment panels"
                            );
                        }
                        Err(error) => {
                            self.data_status = error.to_string();
                            action_outcome = "failed";
                            action_reason = Some("supplementary_panel_open_failed");
                        }
                    }
                } else {
                    action_outcome = "no_op";
                    action_reason = Some("no_read_at_target");
                }
            }
            AlignmentContextAction::OpenTrackSettings => {
                self.open_panel = Some(UiPanel::TrackSettings(TrackFrameKey::Alignment(
                    target.track_id,
                )));
            }
            action => {
                let highlight_action = matches!(
                    action,
                    AlignmentContextAction::AddHighlight(_)
                        | AlignmentContextAction::RemoveHighlight(_)
                        | AlignmentContextAction::ClearHighlights
                );
                let target_contig = self
                    .panels
                    .get(target.panel_id)
                    .map(|panel| panel.contig_name.clone())
                    .unwrap_or_default();
                let Some(track) = self
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track.config.id == target.track_id)
                else {
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "no_op",
                        Some("track_no_longer_exists"),
                    );
                    return;
                };
                let filters = &mut track.config.filters;
                let highlight_reference_before = filters
                    .alignment_highlight_filter
                    .all
                    .iter()
                    .any(AlignmentViewPredicate::requires_reference);
                match action {
                    AlignmentContextAction::SetColor(mode) => filters.alignment_color_mode = mode,
                    AlignmentContextAction::SetGroup(mode) => {
                        filters.alignment_group_mode = mode;
                        if matches!(
                            mode,
                            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
                        ) {
                            filters.alignment_sort_anchor = Some(target.genomic_position);
                        }
                    }
                    AlignmentContextAction::SetSort(mode) => {
                        filters.alignment_sort_mode = mode;
                        if mode == AlignmentSortMode::Base {
                            filters.alignment_sort_anchor = Some(target.genomic_position);
                        } else if !matches!(
                            filters.alignment_group_mode,
                            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
                        ) {
                            filters.alignment_sort_anchor = None;
                        }
                    }
                    AlignmentContextAction::ReverseGroupOrder => {
                        filters.alignment_group_order =
                            reverse_order(filters.alignment_group_order);
                    }
                    AlignmentContextAction::ReverseSortOrder => {
                        filters.alignment_sort_order = reverse_order(filters.alignment_sort_order);
                    }
                    AlignmentContextAction::SetStacking(mode) => filters.read_stacking_mode = mode,
                    AlignmentContextAction::TogglePairs => {
                        filters.view_as_pairs = !filters.view_as_pairs
                    }
                    AlignmentContextAction::ResetArrangement => {
                        filters.alignment_group_mode = AlignmentGroupMode::None;
                        filters.alignment_group_order = AlignmentOrderDirection::Natural;
                        filters.alignment_sort_mode = AlignmentSortMode::Start;
                        filters.alignment_sort_order = AlignmentOrderDirection::Natural;
                        filters.alignment_sort_anchor = None;
                        filters.view_as_pairs = false;
                    }
                    AlignmentContextAction::SetBaseDisplay(mode) => {
                        filters.read_base_display_mode = mode;
                    }
                    AlignmentContextAction::ToggleBaseQualityShading => {
                        filters.shade_read_bases_by_quality = !filters.shade_read_bases_by_quality;
                    }
                    AlignmentContextAction::AddFilter(predicate) => {
                        add_quick_alignment_filter(
                            filters,
                            predicate,
                            &target_contig,
                            target.genomic_position,
                        );
                    }
                    AlignmentContextAction::RemoveFilter(predicate) => {
                        remove_quick_alignment_filter(
                            filters,
                            predicate,
                            &target_contig,
                            target.genomic_position,
                        );
                    }
                    AlignmentContextAction::AddHighlight(predicate) => {
                        let predicate = quick_alignment_predicate(
                            predicate,
                            &target_contig,
                            target.genomic_position,
                        );
                        if !filters.alignment_highlight_filter.all.contains(&predicate) {
                            filters.alignment_highlight_filter.all.push(predicate);
                        }
                    }
                    AlignmentContextAction::RemoveHighlight(predicate) => {
                        let predicate = quick_alignment_predicate(
                            predicate,
                            &target_contig,
                            target.genomic_position,
                        );
                        filters
                            .alignment_highlight_filter
                            .all
                            .retain(|existing| existing != &predicate);
                    }
                    AlignmentContextAction::ClearFilters => {
                        filters.alignment_view_filter.all.clear();
                        filters.minimum_mapping_quality = None;
                        filters.include_secondary = true;
                        filters.include_supplementary = true;
                        filters.auxiliary_predicates.clear();
                    }
                    AlignmentContextAction::ClearHighlights => {
                        filters.alignment_highlight_filter.all.clear();
                    }
                    AlignmentContextAction::InspectRead
                    | AlignmentContextAction::SelectRead
                    | AlignmentContextAction::CopyReadName
                    | AlignmentContextAction::CopyReadDetails
                    | AlignmentContextAction::CopyReadSequence
                    | AlignmentContextAction::GoToMate
                    | AlignmentContextAction::OpenMateInNewPanel
                    | AlignmentContextAction::OpenSupplementarySegments
                    | AlignmentContextAction::OpenTrackSettings => unreachable!(),
                }
                if matches!(
                    action,
                    AlignmentContextAction::AddFilter(_)
                        | AlignmentContextAction::RemoveFilter(_)
                        | AlignmentContextAction::ClearFilters
                ) && let Ok((editor, active)) = filter_state_from_session(filters)
                {
                    track.filter_editor = editor;
                    track.active_filters = active;
                }
                let highlight_reference_after = filters
                    .alignment_highlight_filter
                    .all
                    .iter()
                    .any(AlignmentViewPredicate::requires_reference);
                let interaction_only_highlight = matches!(
                    action,
                    AlignmentContextAction::AddHighlight(_)
                        | AlignmentContextAction::RemoveHighlight(_)
                        | AlignmentContextAction::ClearHighlights
                ) && !highlight_reference_before
                    && !highlight_reference_after;
                if highlight_action {
                    self.refresh_alignment_highlight_interactions(&[target.track_id]);
                }
                if interaction_only_highlight {
                    self.remember_current_locus();
                    self.bump_workspace_revision();
                    let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                    for panel_id in panel_ids {
                        self.synchronize_alignment_scene_interactions(panel_id);
                    }
                    self.rebuild_all_panel_displays();
                    self.mark_display_dirty("explicit display invalidation");
                    self.window.request_redraw();
                    self.record_alignment_context_action_result(
                        target,
                        action,
                        "applied",
                        Some("interaction_overlay_only"),
                    );
                    return;
                }
                let reason = match action.effect() {
                    AlignmentContextEffect::Appearance => "alignment appearance changed",
                    AlignmentContextEffect::Arrangement => "alignment arrangement changed",
                    AlignmentContextEffect::ViewFilter => "alignment view filter changed",
                    AlignmentContextEffect::ReadAction => "alignment read action",
                };
                self.refresh_alignment_context_track(target.track_id, reason);
            }
        }
        self.record_alignment_context_action_result(target, action, action_outcome, action_reason);
        self.window.request_redraw();
    }
}

fn add_quick_alignment_filter(
    filters: &mut SessionFilters,
    predicate: AlignmentQuickPredicate,
    contig: &str,
    position: u64,
) {
    // The retained source is deliberately broader than these legacy visibility switches. A quick
    // positive flag predicate must therefore retire the corresponding implicit negative
    // predicate, rather than constructing an impossible `false AND true` expression.
    match predicate {
        AlignmentQuickPredicate::Secondary => {
            filters.include_secondary = true;
            filters.alignment_view_filter.all.retain(|existing| {
                !matches!(
                    existing,
                    AlignmentViewPredicate::Flag {
                        flag: rgv_core::AlignmentFlagPredicate::Secondary,
                        expected: false,
                    }
                )
            });
        }
        AlignmentQuickPredicate::Supplementary => {
            filters.include_supplementary = true;
            filters.alignment_view_filter.all.retain(|existing| {
                !matches!(
                    existing,
                    AlignmentViewPredicate::Flag {
                        flag: rgv_core::AlignmentFlagPredicate::Supplementary,
                        expected: false,
                    }
                )
            });
        }
        AlignmentQuickPredicate::Substitution
        | AlignmentQuickPredicate::Insertion
        | AlignmentQuickPredicate::Deletion
        | AlignmentQuickPredicate::Indel
        | AlignmentQuickPredicate::SoftClip
        | AlignmentQuickPredicate::Splice
        | AlignmentQuickPredicate::Duplicate
        | AlignmentQuickPredicate::MappingQuality30
        | AlignmentQuickPredicate::InsertionAtPosition
        | AlignmentQuickPredicate::DeletionAtPosition
        | AlignmentQuickPredicate::SubstitutionAtPosition => {}
    }
    let predicate = quick_alignment_predicate(predicate, contig, position);
    if !filters.alignment_view_filter.all.contains(&predicate) {
        filters.alignment_view_filter.all.push(predicate);
    }
}

fn remove_quick_alignment_filter(
    filters: &mut SessionFilters,
    predicate: AlignmentQuickPredicate,
    contig: &str,
    position: u64,
) {
    let predicate = quick_alignment_predicate(predicate, contig, position);
    filters
        .alignment_view_filter
        .all
        .retain(|existing| existing != &predicate);
}

fn quick_alignment_predicate_is_active(
    filters: &SessionFilters,
    predicate: AlignmentQuickPredicate,
    highlight: bool,
    contig: &str,
    position: u64,
) -> bool {
    let predicate = quick_alignment_predicate(predicate, contig, position);
    let expression = if highlight {
        &filters.alignment_highlight_filter
    } else {
        &filters.alignment_view_filter
    };
    expression.all.contains(&predicate)
}

fn alignment_active_filter_labels(filters: &SessionFilters) -> Vec<String> {
    let mut labels = Vec::new();
    if let Some(minimum) = filters.minimum_mapping_quality {
        labels.push(format!("MAPQ ≥ {minimum}"));
    }
    if !filters.include_secondary {
        labels.push("Not secondary".to_owned());
    }
    if !filters.include_supplementary {
        labels.push("Not supplementary".to_owned());
    }
    labels.extend(
        filters
            .auxiliary_predicates
            .iter()
            .map(|predicate| format!("SAM tag: {predicate}")),
    );
    labels.extend(
        filters
            .alignment_view_filter
            .all
            .iter()
            .map(alignment_predicate_label),
    );
    labels
}

pub(crate) fn quick_alignment_predicate(
    predicate: AlignmentQuickPredicate,
    contig: &str,
    position: u64,
) -> AlignmentViewPredicate {
    let anywhere = AlignmentPredicateScope::Anywhere;
    let anchored = AlignmentPredicateScope::Position {
        contig: contig.to_owned(),
        position,
    };
    match predicate {
        AlignmentQuickPredicate::Substitution => AlignmentViewPredicate::Substitution {
            scope: anywhere,
            observed_base: None,
            minimum_count: Some(1),
            maximum_count: None,
        },
        AlignmentQuickPredicate::SubstitutionAtPosition => AlignmentViewPredicate::Substitution {
            scope: anchored,
            observed_base: None,
            minimum_count: Some(1),
            maximum_count: None,
        },
        AlignmentQuickPredicate::Insertion
        | AlignmentQuickPredicate::Deletion
        | AlignmentQuickPredicate::Indel
        | AlignmentQuickPredicate::SoftClip
        | AlignmentQuickPredicate::Splice
        | AlignmentQuickPredicate::InsertionAtPosition
        | AlignmentQuickPredicate::DeletionAtPosition => AlignmentViewPredicate::CigarEvent {
            event: match predicate {
                AlignmentQuickPredicate::Insertion
                | AlignmentQuickPredicate::InsertionAtPosition => {
                    rgv_core::AlignmentCigarEventPredicate::Insertion
                }
                AlignmentQuickPredicate::Deletion | AlignmentQuickPredicate::DeletionAtPosition => {
                    rgv_core::AlignmentCigarEventPredicate::Deletion
                }
                AlignmentQuickPredicate::Indel => rgv_core::AlignmentCigarEventPredicate::Indel,
                AlignmentQuickPredicate::SoftClip => {
                    rgv_core::AlignmentCigarEventPredicate::SoftClip
                }
                AlignmentQuickPredicate::Splice => {
                    rgv_core::AlignmentCigarEventPredicate::ReferenceSkip
                }
                AlignmentQuickPredicate::Substitution
                | AlignmentQuickPredicate::SubstitutionAtPosition
                | AlignmentQuickPredicate::Duplicate
                | AlignmentQuickPredicate::Secondary
                | AlignmentQuickPredicate::Supplementary
                | AlignmentQuickPredicate::MappingQuality30 => unreachable!(),
            },
            scope: if matches!(
                predicate,
                AlignmentQuickPredicate::InsertionAtPosition
                    | AlignmentQuickPredicate::DeletionAtPosition
            ) {
                anchored
            } else {
                anywhere
            },
            minimum_length: None,
            maximum_length: None,
        },
        AlignmentQuickPredicate::Duplicate
        | AlignmentQuickPredicate::Secondary
        | AlignmentQuickPredicate::Supplementary => AlignmentViewPredicate::Flag {
            flag: match predicate {
                AlignmentQuickPredicate::Duplicate => rgv_core::AlignmentFlagPredicate::Duplicate,
                AlignmentQuickPredicate::Secondary => rgv_core::AlignmentFlagPredicate::Secondary,
                AlignmentQuickPredicate::Supplementary => {
                    rgv_core::AlignmentFlagPredicate::Supplementary
                }
                _ => unreachable!(),
            },
            expected: true,
        },
        AlignmentQuickPredicate::MappingQuality30 => AlignmentViewPredicate::MappingQuality {
            minimum: Some(30),
            maximum: None,
        },
    }
}

pub(crate) fn alignment_predicate_label(predicate: &AlignmentViewPredicate) -> String {
    match predicate {
        AlignmentViewPredicate::MappingQuality { minimum, maximum } => {
            format!(
                "MAPQ {}–{}",
                minimum.map_or("any".to_owned(), |v| v.to_string()),
                maximum.map_or("any".to_owned(), |v| v.to_string())
            )
        }
        AlignmentViewPredicate::Flag { flag, expected } => {
            format!("{} {flag:?}", if *expected { "Is" } else { "Not" })
        }
        AlignmentViewPredicate::TemplateLength { minimum, maximum } => format!(
            "Template length {}–{}",
            minimum.map_or("any".to_owned(), |value| value.to_string()),
            maximum.map_or("any".to_owned(), |value| value.to_string())
        ),
        AlignmentViewPredicate::MateContig { contig } => format!("Mate on {contig}"),
        AlignmentViewPredicate::Auxiliary {
            tag, comparison, ..
        } => format!("SAM tag {} {comparison:?}", String::from_utf8_lossy(tag)),
        AlignmentViewPredicate::CigarEvent {
            event,
            scope,
            minimum_length,
            maximum_length,
        } => format!(
            "{event:?}{}{}",
            alignment_scope_suffix(scope),
            length_suffix(*minimum_length, *maximum_length)
        ),
        AlignmentViewPredicate::AlignedLength { minimum, maximum } => format!(
            "Aligned length {}–{}",
            minimum.map_or("any".to_owned(), |value| value.to_string()),
            maximum.map_or("any".to_owned(), |value| value.to_string())
        ),
        AlignmentViewPredicate::Substitution {
            scope,
            observed_base,
            minimum_count,
            maximum_count,
        } => format!(
            "Substitution{}{}{}",
            observed_base.map_or(String::new(), |base| format!(" {}", char::from(base))),
            alignment_scope_suffix(scope),
            length_suffix(*minimum_count, *maximum_count)
        ),
    }
}

fn alignment_scope_suffix(scope: &AlignmentPredicateScope) -> String {
    match scope {
        AlignmentPredicateScope::Anywhere => String::new(),
        AlignmentPredicateScope::Position { contig, position } => {
            format!(" at {contig}:{}", position.saturating_add(1))
        }
        AlignmentPredicateScope::Interval { contig, start, end } => {
            format!(" in {contig}:{}-{end}", start.saturating_add(1))
        }
    }
}

fn length_suffix<T: std::fmt::Display>(minimum: Option<T>, maximum: Option<T>) -> String {
    match (minimum, maximum) {
        (None, None) => String::new(),
        (Some(minimum), None) => format!(" ≥ {minimum}"),
        (None, Some(maximum)) => format!(" ≤ {maximum}"),
        (Some(minimum), Some(maximum)) => format!(" {minimum}–{maximum}"),
    }
}

const fn reverse_order(order: AlignmentOrderDirection) -> AlignmentOrderDirection {
    match order {
        AlignmentOrderDirection::Natural => AlignmentOrderDirection::Reverse,
        AlignmentOrderDirection::Reverse => AlignmentOrderDirection::Natural,
    }
}

pub(crate) fn show_alignment_context_menu(
    parent_ui: &egui::Ui,
    state: &mut Option<AlignmentContextMenuState>,
    view: Option<&AlignmentContextMenuView>,
    action: &mut Option<(AlignmentContextTarget, AlignmentContextAction)>,
) {
    let Some(menu) = state.clone() else {
        return;
    };
    let Some(view) = view else {
        *state = None;
        return;
    };
    let context = parent_ui.ctx();
    let popup_id = egui::Id::new("alignment-canvas-context-menu");
    let mut open = true;
    let shown = egui::Popup::new(
        popup_id,
        context.clone(),
        menu.screen_position,
        parent_ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .open_bool(&mut open)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .width(238.0)
    .show(|ui| {
        ui.set_min_width(238.0);
        if let Some(name) = view.read_name.as_deref() {
            ui.label(egui::RichText::new(name).strong().monospace());
            if context_button(
                ui,
                "Inspect read",
                AlignmentContextAction::InspectRead,
                action,
                menu.target,
            ) {
                return;
            }
            if context_button(
                ui,
                "Copy read details",
                AlignmentContextAction::CopyReadDetails,
                action,
                menu.target,
            ) {
                return;
            }
            if ui
                .add_enabled(view.sequence_available, egui::Button::new("Copy sequence"))
                .on_disabled_hover_text("Sequence payload is unavailable for this read")
                .clicked()
            {
                *action = Some((menu.target, AlignmentContextAction::CopyReadSequence));
                return;
            }
            if context_button(
                ui,
                "Select read",
                AlignmentContextAction::SelectRead,
                action,
                menu.target,
            ) {
                return;
            }
            if context_button(
                ui,
                "Copy read name",
                AlignmentContextAction::CopyReadName,
                action,
                menu.target,
            ) {
                return;
            }
            if ui
                .add_enabled(view.mate_available, egui::Button::new("Go to mate"))
                .on_disabled_hover_text("This read does not have a mapped mate")
                .clicked()
            {
                *action = Some((menu.target, AlignmentContextAction::GoToMate));
                return;
            }
            if ui
                .add_enabled(
                    view.mate_available,
                    egui::Button::new("Open mate in new panel"),
                )
                .on_disabled_hover_text("This read does not have a mapped mate")
                .clicked()
            {
                *action = Some((menu.target, AlignmentContextAction::OpenMateInNewPanel));
                return;
            }
            if ui
                .add_enabled(
                    view.supplementary_segments_available,
                    egui::Button::new("Open supplementary segments"),
                )
                .clicked()
            {
                *action = Some((
                    menu.target,
                    AlignmentContextAction::OpenSupplementarySegments,
                ));
                return;
            }
            ui.separator();
        }

        ui.menu_button("Arrange reads", |ui| {
            ui.menu_button("Sort by", |ui| {
                for (label, mode) in [
                    ("Start position", AlignmentSortMode::Start),
                    ("Strand", AlignmentSortMode::Strand),
                    ("First-of-pair strand", AlignmentSortMode::FirstOfPairStrand),
                    ("Mapping quality", AlignmentSortMode::MappingQuality),
                    ("Read name", AlignmentSortMode::ReadName),
                    ("Read group", AlignmentSortMode::ReadGroup),
                    ("Haplotype", AlignmentSortMode::Haplotype),
                    ("Aligned length", AlignmentSortMode::AlignedLength),
                    ("Left clip", AlignmentSortMode::LeftClip),
                    ("Right clip", AlignmentSortMode::RightClip),
                    ("Template length", AlignmentSortMode::TemplateLength),
                    ("Pair orientation", AlignmentSortMode::PairOrientation),
                    ("Mate contig", AlignmentSortMode::MateContig),
                    ("Pair status", AlignmentSortMode::PairStatus),
                    ("Read order", AlignmentSortMode::ReadOrder),
                    ("Base at clicked position", AlignmentSortMode::Base),
                ] {
                    context_radio(
                        ui,
                        label,
                        view.filters.alignment_sort_mode == mode,
                        AlignmentContextAction::SetSort(mode),
                        action,
                        menu.target,
                    );
                }
            });
            context_button(
                ui,
                if view.filters.alignment_sort_order == AlignmentOrderDirection::Natural {
                    "Reverse sort order"
                } else {
                    "Use natural sort order"
                },
                AlignmentContextAction::ReverseSortOrder,
                action,
                menu.target,
            );
            ui.menu_button("Group by", |ui| {
                for (label, mode) in [
                    ("None", AlignmentGroupMode::None),
                    ("Strand", AlignmentGroupMode::Strand),
                    ("Read group", AlignmentGroupMode::ReadGroup),
                    ("Haplotype", AlignmentGroupMode::Haplotype),
                    ("Chimeric", AlignmentGroupMode::Chimeric),
                    ("Supplementary", AlignmentGroupMode::Supplementary),
                    ("Left clip", AlignmentGroupMode::LeftClip),
                    ("Right clip", AlignmentGroupMode::RightClip),
                    ("Pair orientation", AlignmentGroupMode::PairOrientation),
                    ("Unexpected pair", AlignmentGroupMode::UnexpectedPair),
                    ("Mate contig", AlignmentGroupMode::MateContig),
                    ("Read order", AlignmentGroupMode::ReadOrder),
                    (
                        "First-of-pair strand",
                        AlignmentGroupMode::FirstOfPairStrand,
                    ),
                    ("Base at clicked position", AlignmentGroupMode::Base),
                    (
                        "Insertion at clicked position",
                        AlignmentGroupMode::Insertion,
                    ),
                ] {
                    context_radio(
                        ui,
                        label,
                        view.filters.alignment_group_mode == mode,
                        AlignmentContextAction::SetGroup(mode),
                        action,
                        menu.target,
                    );
                }
            });
            context_button(
                ui,
                if view.filters.alignment_group_order == AlignmentOrderDirection::Natural {
                    "Reverse group order"
                } else {
                    "Use natural group order"
                },
                AlignmentContextAction::ReverseGroupOrder,
                action,
                menu.target,
            );
            ui.separator();
            context_radio(
                ui,
                "Stable stacking",
                view.filters.read_stacking_mode == ReadStackingMode::Stable,
                AlignmentContextAction::SetStacking(ReadStackingMode::Stable),
                action,
                menu.target,
            );
            context_radio(
                ui,
                "Compact stacking",
                view.filters.read_stacking_mode == ReadStackingMode::Compact,
                AlignmentContextAction::SetStacking(ReadStackingMode::Compact),
                action,
                menu.target,
            );
            context_check(
                ui,
                "View as pairs",
                view.filters.view_as_pairs,
                AlignmentContextAction::TogglePairs,
                action,
                menu.target,
            );
            ui.separator();
            context_button(
                ui,
                "Reset arrangement / re-pack",
                AlignmentContextAction::ResetArrangement,
                action,
                menu.target,
            );
        });

        ui.menu_button("Colour reads", |ui| {
            for (label, mode) in [
                ("Neutral grey", AlignmentColorMode::None),
                ("Strand", AlignmentColorMode::Strand),
                ("Mapping quality", AlignmentColorMode::MappingQuality),
                ("Read group", AlignmentColorMode::ReadGroup),
                ("Haplotype", AlignmentColorMode::Haplotype),
                ("Template length", AlignmentColorMode::TemplateLength),
                ("Pair orientation", AlignmentColorMode::PairOrientation),
                ("Unexpected pair", AlignmentColorMode::UnexpectedPair),
                ("Mate contig", AlignmentColorMode::MateContig),
                (
                    "First-of-pair strand",
                    AlignmentColorMode::FirstOfPairStrand,
                ),
            ] {
                context_radio(
                    ui,
                    label,
                    view.filters.alignment_color_mode == mode,
                    AlignmentContextAction::SetColor(mode),
                    action,
                    menu.target,
                );
            }
            if matches!(
                view.filters.alignment_color_mode,
                AlignmentColorMode::Tag(_)
            ) {
                ui.label("✓ SAM tag (configured in track settings)");
            }
        });

        ui.menu_button("Display", |ui| {
            for (label, mode) in [
                ("Mismatches only", ReadBaseDisplayMode::Mismatches),
                ("All bases", ReadBaseDisplayMode::All),
                ("Hide bases", ReadBaseDisplayMode::Hidden),
            ] {
                context_radio(
                    ui,
                    label,
                    view.filters.read_base_display_mode == mode,
                    AlignmentContextAction::SetBaseDisplay(mode),
                    action,
                    menu.target,
                );
            }
            context_check(
                ui,
                "Shade bases by quality",
                view.filters.shade_read_bases_by_quality,
                AlignmentContextAction::ToggleBaseQualityShading,
                action,
                menu.target,
            );
        });
        let active_filter_labels = alignment_active_filter_labels(&view.filters);
        let filter_menu_label = if active_filter_labels.is_empty() {
            "Filter reads".to_owned()
        } else {
            format!("Filter reads ({})", active_filter_labels.len())
        };
        ui.menu_button(filter_menu_label, |ui| {
            if active_filter_labels.is_empty() {
                ui.label(egui::RichText::new("No active read filters").weak());
            } else {
                ui.label(egui::RichText::new("Active filters").strong());
                ui.label(
                    egui::RichText::new("All conditions below must match (AND)")
                        .small()
                        .weak(),
                );
                for label in &active_filter_labels {
                    ui.label(format!("✓ {label}"));
                }
            }
            ui.separator();
            for (label, predicate) in quick_predicate_menu_entries() {
                context_predicate_button(ui, label, predicate, false, view, action, menu.target);
            }
            ui.separator();
            for (label, predicate) in [
                (
                    "Insertion at clicked position",
                    AlignmentQuickPredicate::InsertionAtPosition,
                ),
                (
                    "Deletion at clicked position",
                    AlignmentQuickPredicate::DeletionAtPosition,
                ),
                (
                    "Substitution at clicked position",
                    AlignmentQuickPredicate::SubstitutionAtPosition,
                ),
            ] {
                context_predicate_button(ui, label, predicate, false, view, action, menu.target);
            }
            ui.separator();
            if !view.filters.alignment_view_filter.all.is_empty()
                || view.filters.minimum_mapping_quality.is_some()
                || !view.filters.include_secondary
                || !view.filters.include_supplementary
                || !view.filters.auxiliary_predicates.is_empty()
            {
                context_button(
                    ui,
                    "Clear all filters",
                    AlignmentContextAction::ClearFilters,
                    action,
                    menu.target,
                );
            }
            if let (Some(matching), Some(eligible)) = (view.matching_records, view.eligible_records)
            {
                ui.label(
                    egui::RichText::new(format!("{matching} of {eligible} eligible"))
                        .small()
                        .weak(),
                );
            }
        });
        ui.menu_button("Highlight reads", |ui| {
            for (label, predicate) in quick_predicate_menu_entries() {
                context_predicate_button(ui, label, predicate, true, view, action, menu.target);
            }
            if !view.filters.alignment_highlight_filter.all.is_empty() {
                ui.separator();
                context_button(
                    ui,
                    "Clear highlights",
                    AlignmentContextAction::ClearHighlights,
                    action,
                    menu.target,
                );
            }
        });
        ui.separator();
        context_button(
            ui,
            "Track settings…",
            AlignmentContextAction::OpenTrackSettings,
            action,
            menu.target,
        );
        ui.label(
            egui::RichText::new(format!(
                "Position {}",
                menu.target.genomic_position.saturating_add(1)
            ))
            .small()
            .weak(),
        );
    });

    if action.is_some() || !open || shown.is_none() {
        *state = None;
        context.request_repaint();
    }
}

fn quick_predicate_menu_entries() -> [(&'static str, AlignmentQuickPredicate); 10] {
    [
        (
            "Contains substitution",
            AlignmentQuickPredicate::Substitution,
        ),
        ("Contains insertion", AlignmentQuickPredicate::Insertion),
        ("Contains deletion", AlignmentQuickPredicate::Deletion),
        ("Contains any indel", AlignmentQuickPredicate::Indel),
        ("Contains soft clipping", AlignmentQuickPredicate::SoftClip),
        ("Contains splice", AlignmentQuickPredicate::Splice),
        ("Duplicate", AlignmentQuickPredicate::Duplicate),
        ("Secondary", AlignmentQuickPredicate::Secondary),
        ("Supplementary", AlignmentQuickPredicate::Supplementary),
        (
            "MAPQ at least 30",
            AlignmentQuickPredicate::MappingQuality30,
        ),
    ]
}

fn context_button(
    ui: &mut egui::Ui,
    label: &str,
    value: AlignmentContextAction,
    action: &mut Option<(AlignmentContextTarget, AlignmentContextAction)>,
    target: AlignmentContextTarget,
) -> bool {
    if ui.button(label).clicked() {
        *action = Some((target, value));
        ui.close();
        true
    } else {
        false
    }
}

fn context_predicate_button(
    ui: &mut egui::Ui,
    label: &str,
    predicate: AlignmentQuickPredicate,
    highlight: bool,
    view: &AlignmentContextMenuView,
    action: &mut Option<(AlignmentContextTarget, AlignmentContextAction)>,
    target: AlignmentContextTarget,
) {
    let enabled = !predicate.requires_reference() || view.reference_configured;
    let selected = quick_alignment_predicate_is_active(
        &view.filters,
        predicate,
        highlight,
        &view.contig_name,
        target.genomic_position,
    );
    let mut checked = selected;
    let response = ui.add_enabled(enabled, egui::Checkbox::new(&mut checked, label));
    let response = if enabled {
        response
    } else {
        response.on_disabled_hover_text("Requires a matching reference FASTA")
    };
    if response.clicked() {
        *action = Some((
            target,
            match (highlight, checked) {
                (true, true) => AlignmentContextAction::AddHighlight(predicate),
                (true, false) => AlignmentContextAction::RemoveHighlight(predicate),
                (false, true) => AlignmentContextAction::AddFilter(predicate),
                (false, false) => AlignmentContextAction::RemoveFilter(predicate),
            },
        ));
        ui.close();
    }
}

fn context_radio(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    value: AlignmentContextAction,
    action: &mut Option<(AlignmentContextTarget, AlignmentContextAction)>,
    target: AlignmentContextTarget,
) {
    if ui.radio(selected, label).clicked() {
        *action = Some((target, value));
        ui.close();
    }
}

fn context_check(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    value: AlignmentContextAction,
    action: &mut Option<(AlignmentContextTarget, AlignmentContextAction)>,
    target: AlignmentContextTarget,
) {
    let mut checked = selected;
    if ui.checkbox(&mut checked, label).clicked() {
        *action = Some((target, value));
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_context_action_declares_one_effect_class() {
        assert_eq!(
            AlignmentContextAction::SetColor(AlignmentColorMode::None).effect(),
            AlignmentContextEffect::Appearance
        );
        assert_eq!(
            AlignmentContextAction::SetSort(AlignmentSortMode::Start).effect(),
            AlignmentContextEffect::Arrangement
        );
        assert_eq!(
            AlignmentContextAction::InspectRead.effect(),
            AlignmentContextEffect::ReadAction
        );
        assert_eq!(
            AlignmentContextAction::GoToMate.effect(),
            AlignmentContextEffect::ReadAction
        );
        assert_eq!(
            AlignmentContextAction::AddFilter(AlignmentQuickPredicate::Insertion).effect(),
            AlignmentContextEffect::ViewFilter
        );
        assert_eq!(
            AlignmentContextAction::RemoveFilter(AlignmentQuickPredicate::Insertion).effect(),
            AlignmentContextEffect::ViewFilter
        );
        assert_eq!(
            AlignmentContextAction::SetBaseDisplay(ReadBaseDisplayMode::All).effect(),
            AlignmentContextEffect::Appearance
        );
    }

    #[test]
    fn positive_flag_quick_filters_replace_implicit_legacy_exclusions() {
        let dictionary = ReferenceDictionary::from_contigs([("chr7".to_owned(), 100_000)]);
        for (quick, flag) in [
            (
                AlignmentQuickPredicate::Secondary,
                rgv_core::AlignmentFlagPredicate::Secondary,
            ),
            (
                AlignmentQuickPredicate::Supplementary,
                rgv_core::AlignmentFlagPredicate::Supplementary,
            ),
        ] {
            let mut filters = SessionFilters {
                include_secondary: false,
                include_supplementary: false,
                ..SessionFilters::default()
            };
            add_quick_alignment_filter(&mut filters, quick, "chr7", 100);
            let active = active_filters_from_session(&filters).unwrap();
            let contract = alignment_view_filter_contract(&filters, &active, &dictionary);

            assert_eq!(contract.predicates().len(), 2);
            assert!(contract.predicates().iter().any(|predicate| matches!(
                predicate,
                AlignmentFilterPredicate::Flag {
                    flag: candidate,
                    expected: true,
                } if *candidate == flag
            )));
            assert!(!contract.predicates().iter().any(|predicate| matches!(
                predicate,
                AlignmentFilterPredicate::Flag {
                    flag: candidate,
                    expected: false,
                } if *candidate == flag
            )));
        }
    }

    #[test]
    fn quick_filter_state_is_visible_and_individually_removable() {
        let mut filters = SessionFilters {
            include_secondary: true,
            include_supplementary: true,
            ..SessionFilters::default()
        };
        add_quick_alignment_filter(
            &mut filters,
            AlignmentQuickPredicate::Substitution,
            "chr7",
            100,
        );
        add_quick_alignment_filter(&mut filters, AlignmentQuickPredicate::Indel, "chr7", 100);

        assert!(quick_alignment_predicate_is_active(
            &filters,
            AlignmentQuickPredicate::Substitution,
            false,
            "chr7",
            100,
        ));
        assert!(quick_alignment_predicate_is_active(
            &filters,
            AlignmentQuickPredicate::Indel,
            false,
            "chr7",
            100,
        ));
        let labels = alignment_active_filter_labels(&filters);
        assert_eq!(labels.len(), 2);
        assert!(labels.iter().any(|label| label.starts_with("Substitution")));
        assert!(labels.iter().any(|label| label.starts_with("Indel")));

        remove_quick_alignment_filter(&mut filters, AlignmentQuickPredicate::Indel, "chr7", 100);
        assert!(!quick_alignment_predicate_is_active(
            &filters,
            AlignmentQuickPredicate::Indel,
            false,
            "chr7",
            100,
        ));
        assert!(quick_alignment_predicate_is_active(
            &filters,
            AlignmentQuickPredicate::Substitution,
            false,
            "chr7",
            100,
        ));
    }
}
