use crate::inspection::rna::{
    SashimiExportBundle, SashimiExportTrack, junction_annotation_context,
};
use crate::ui::TrackFrameKey;
use crate::workspace::alignments::{
    AlignmentSettingsDraft, AlignmentTrackController, AlignmentTrackLifecycle,
    commit_alignment_settings_draft, copy_compatible_alignment_appearance,
    expanded_workspace_alignment_limit,
};
use crate::workspace::lists::apply_quantitative_bulk_action;
use crate::workspace::panels::ViewPanelController;
use crate::{
    MAX_SASHIMI_EXPORT_TRACKS, QuantitativeBulkAction, SYNTHETIC_ALIGNMENT_TRACK_ID, UiPanel,
    Viewer, auxiliary_query_track_ids, clear_removed_auxiliary_panel_state,
    clear_removed_quantitative_dependencies, ui,
};
use anyhow::Result;
use rgv_core::{
    GenomicInterval, LandmarkAssetRole, PanelId, PresentationNodeId, ReferenceDictionary,
    SessionFilters, TrackChangeSet, TrackConfig, TrackId, TrackIntent, TrackKind,
    WorkspacePresentation,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};

fn updated_track_pane_selection<T: Copy + Eq>(
    track_order: &[T],
    selected: &[T],
    anchor: Option<T>,
    target: T,
    shift_range: bool,
    toggle: bool,
) -> (Vec<T>, Option<T>) {
    let anchor = anchor
        .filter(|anchor| selected.contains(anchor))
        .or_else(|| selected.first().copied())
        .unwrap_or(target);
    let next = if shift_range {
        let Some(anchor_index) = track_order
            .iter()
            .position(|candidate| *candidate == anchor)
        else {
            return (vec![target], Some(target));
        };
        let Some(target_index) = track_order
            .iter()
            .position(|candidate| *candidate == target)
        else {
            return (vec![target], Some(target));
        };
        let (start, end) = if anchor_index <= target_index {
            (anchor_index, target_index)
        } else {
            (target_index, anchor_index)
        };
        track_order[start..=end].to_vec()
    } else {
        if toggle {
            let mut next = selected.to_vec();
            if let Some(index) = next.iter().position(|candidate| *candidate == target) {
                next.remove(index);
            } else {
                next.push(target);
            }
            next
        } else {
            vec![target]
        }
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

pub(crate) fn replace_panel_track_selection(
    panel: &mut ViewPanelController,
    key: TrackFrameKey,
    channel: ui::TrackPane,
) {
    let node = match key {
        TrackFrameKey::Reference => PresentationNodeId::Reference,
        TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
            PresentationNodeId::Track { channel, track_id }
        }
        TrackFrameKey::Group { channel, id } => PresentationNodeId::Group {
            channel,
            group_id: id,
        },
    };
    panel.selected_presentation_nodes = vec![node];
    panel.presentation_selection_anchor = Some(node);
    panel.selected_alignment_tracks = match key {
        TrackFrameKey::Alignment(track_id) => vec![track_id],
        TrackFrameKey::Reference | TrackFrameKey::Group { .. } | TrackFrameKey::Auxiliary(_) => {
            Vec::new()
        }
    };
    panel.selected_read_tracks = match (key, channel) {
        (TrackFrameKey::Alignment(track_id), ui::TrackPane::Reads) => vec![track_id],
        _ => Vec::new(),
    };
    panel.alignment_selection_anchor = match (key, channel) {
        (TrackFrameKey::Alignment(track_id), ui::TrackPane::Annotation) => Some(track_id),
        _ => None,
    };
    panel.read_selection_anchor = match (key, channel) {
        (TrackFrameKey::Alignment(track_id), ui::TrackPane::Reads) => Some(track_id),
        _ => None,
    };
    panel.selected_track = Some(key);
}

fn synchronize_panel_track_selection(
    panel: &mut ViewPanelController,
    alignment_ids: &BTreeSet<TrackId>,
) {
    panel.selected_alignment_tracks = panel
        .selected_presentation_nodes
        .iter()
        .filter_map(|node| match node {
            PresentationNodeId::Track {
                channel: ui::TrackPane::Annotation,
                track_id,
            } if alignment_ids.contains(track_id) => Some(*track_id),
            PresentationNodeId::Reference
            | PresentationNodeId::Track { .. }
            | PresentationNodeId::Group { .. } => None,
        })
        .collect();
    panel.selected_read_tracks = panel
        .selected_presentation_nodes
        .iter()
        .filter_map(|node| match node {
            PresentationNodeId::Track {
                channel: ui::TrackPane::Reads,
                track_id,
            } if alignment_ids.contains(track_id) => Some(*track_id),
            PresentationNodeId::Reference
            | PresentationNodeId::Track { .. }
            | PresentationNodeId::Group { .. } => None,
        })
        .collect();
    panel.alignment_selection_anchor =
        panel
            .presentation_selection_anchor
            .and_then(|node| match node {
                PresentationNodeId::Track {
                    channel: ui::TrackPane::Annotation,
                    track_id,
                } if alignment_ids.contains(&track_id) => Some(track_id),
                PresentationNodeId::Reference
                | PresentationNodeId::Track { .. }
                | PresentationNodeId::Group { .. } => None,
            });
    panel.read_selection_anchor = panel
        .presentation_selection_anchor
        .and_then(|node| match node {
            PresentationNodeId::Track {
                channel: ui::TrackPane::Reads,
                track_id,
            } if alignment_ids.contains(&track_id) => Some(track_id),
            PresentationNodeId::Reference
            | PresentationNodeId::Track { .. }
            | PresentationNodeId::Group { .. } => None,
        });
    panel.selected_track =
        panel
            .selected_presentation_nodes
            .last()
            .copied()
            .map(|node| match node {
                PresentationNodeId::Reference => TrackFrameKey::Reference,
                PresentationNodeId::Track { track_id, .. } => {
                    if alignment_ids.contains(&track_id) {
                        TrackFrameKey::Alignment(track_id)
                    } else {
                        TrackFrameKey::Auxiliary(track_id)
                    }
                }
                PresentationNodeId::Group { channel, group_id } => TrackFrameKey::Group {
                    channel,
                    id: group_id,
                },
            });
}

impl Viewer {
    pub(crate) fn effective_auxiliary_presentation_configs(&self) -> Vec<TrackConfig> {
        let by_id = self
            .tracks
            .tracks()
            .iter()
            .map(|track| (track.id, track))
            .collect::<BTreeMap<_, _>>();
        self.track_presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == ui::TrackPane::Annotation)
            .filter_map(|node| {
                let mut track = (*by_id.get(&node.track_id)?).clone();
                let parent = self
                    .track_presentation
                    .group_for_track(ui::TrackPane::Annotation, node.track_id);
                if parent.is_some_and(|group| !group.expanded) {
                    return None;
                }
                track.visible =
                    node.content_visible && parent.is_none_or(|group| group.content_visible);
                track.collapsed = !track.visible;
                if let Some(height) = node.preferred_height {
                    track.height = height;
                }
                Some(track)
            })
            .collect()
    }

    pub(crate) fn workspace_presentation_snapshot(&self) -> WorkspacePresentation {
        self.track_presentation.clone()
    }

    /// Reconciles source lifecycle only. Presentation choices are never read
    /// back from legacy source/configuration booleans: existing node order,
    /// visibility, preferred sizes, and groups remain authoritative.
    pub(crate) fn reconcile_track_presentation(&mut self) {
        let alignment_defaults = self
            .alignment_tracks
            .iter()
            .map(|track| {
                (
                    track.config.id,
                    track.config.annotation_height,
                    track
                        .config
                        .read_group_height
                        .or(Some(track.config.height.clamp(40.0, 1_200.0))),
                )
            })
            .collect::<Vec<_>>();
        let auxiliary_defaults = self
            .tracks
            .tracks()
            .iter()
            .map(|track| (track.id, Some(track.height.clamp(26.0, 1_200.0))))
            .collect::<Vec<_>>();
        self.track_presentation
            .reconcile_sources(&alignment_defaults, &auxiliary_defaults);
        let overlay_ids = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Quantitative)
            .filter_map(|track| track.quantitative.overlay_group)
            .fold(BTreeMap::<u32, usize>::new(), |mut counts, id| {
                *counts.entry(id).or_default() += 1;
                counts
            })
            .into_iter()
            .filter_map(|(id, count)| (count >= 2).then_some(id))
            .collect::<BTreeSet<_>>();
        self.track_presentation
            .reconcile_composites(ui::TrackPane::Annotation, &overlay_ids);
        self.track_presentation
            .reconcile_composites(ui::TrackPane::Reads, &BTreeSet::new());
        for track in &mut self.alignment_tracks {
            track.config.visible = true;
            track.config.collapsed = false;
            track.config.height = 360.0;
            track.config.annotation_height = None;
            track.config.read_group_height = None;
            track.config.filters.show_reads = true;
        }
        for track in self.tracks.tracks_mut() {
            track.visible = true;
            track.collapsed = false;
            track.height = 120.0;
        }
        self.reconcile_presentation_interaction_state();
    }

    fn reconcile_presentation_interaction_state(&mut self) {
        let alignment_ids = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .collect::<BTreeSet<_>>();
        let mut valid_nodes = self
            .track_presentation
            .pane_interactive_nodes(ui::TrackPane::Annotation)
            .into_iter()
            .chain(
                self.track_presentation
                    .pane_interactive_nodes(ui::TrackPane::Reads),
            )
            .collect::<BTreeSet<_>>();
        if self.reference_engine.is_some() {
            valid_nodes.insert(PresentationNodeId::Reference);
        }
        for panel in self.panels.iter_mut() {
            panel
                .selected_presentation_nodes
                .retain(|node| valid_nodes.contains(node));
            panel.presentation_selection_anchor = panel
                .presentation_selection_anchor
                .filter(|node| valid_nodes.contains(node));
            synchronize_panel_track_selection(panel, &alignment_ids);
        }
        if self.resizing_annotation_track.is_some_and(|(key, _)| {
            let node = match key {
                TrackFrameKey::Reference => PresentationNodeId::Reference,
                TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
                    PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id,
                    }
                }
                TrackFrameKey::Group { channel, id } => PresentationNodeId::Group {
                    channel,
                    group_id: id,
                },
            };
            !valid_nodes.contains(&node)
        }) {
            self.resizing_annotation_track = None;
        }
        if self.resizing_read_track.is_some_and(|(track_id, _)| {
            !valid_nodes.contains(&PresentationNodeId::Track {
                channel: ui::TrackPane::Reads,
                track_id,
            })
        }) {
            self.resizing_read_track = None;
        }
        if self.vertical_overview_drag.is_some_and(|drag| {
            !valid_nodes.contains(&PresentationNodeId::Track {
                channel: ui::TrackPane::Reads,
                track_id: drag.track_id,
            })
        }) {
            self.vertical_overview_drag = None;
        }
    }

    pub(crate) fn apply_track_intents(
        &mut self,
        intents: &[TrackIntent],
    ) -> Result<TrackChangeSet> {
        let mut changes = self.track_presentation.apply_intents(intents)?;
        for intent in intents {
            match intent {
                TrackIntent::SetNodeVisibility {
                    target: PresentationNodeId::Track { channel, track_id },
                    visible: false,
                } => {
                    self.reconcile_suppressed_group_selection(*channel, &[*track_id]);
                    changes.selection_changed = true;
                }
                TrackIntent::SetGroupExpanded {
                    target: PresentationNodeId::Group { channel, group_id },
                    expanded: false,
                }
                | TrackIntent::SetGroupVisibility {
                    target: PresentationNodeId::Group { channel, group_id },
                    visible: false,
                } => {
                    if let Some(members) = self
                        .track_presentation
                        .groups
                        .iter()
                        .find(|group| group.channel == *channel && group.id == *group_id)
                        .map(|group| group.members.clone())
                    {
                        self.reconcile_suppressed_group_selection(*channel, &members);
                        changes.selection_changed = true;
                    }
                }
                TrackIntent::SetCompositeVisibility {
                    channel,
                    composite_id,
                    visible: false,
                } => {
                    let members = self
                        .tracks
                        .tracks()
                        .iter()
                        .filter(|track| {
                            track.kind == TrackKind::Quantitative
                                && track.quantitative.overlay_group == Some(*composite_id)
                        })
                        .map(|track| track.id)
                        .collect::<Vec<_>>();
                    self.reconcile_suppressed_group_selection(*channel, &members);
                    changes.selection_changed = true;
                }
                TrackIntent::SetPaneRowsExpanded {
                    channel,
                    expanded: false,
                } => {
                    let members = self
                        .track_presentation
                        .pane_tracks
                        .iter()
                        .filter(|node| node.channel == *channel)
                        .map(|node| node.track_id)
                        .collect::<Vec<_>>();
                    self.reconcile_suppressed_group_selection(*channel, &members);
                    if *channel == ui::TrackPane::Annotation {
                        for panel in self.panels.iter_mut() {
                            panel
                                .selected_presentation_nodes
                                .retain(|node| *node != PresentationNodeId::Reference);
                            if panel.presentation_selection_anchor
                                == Some(PresentationNodeId::Reference)
                            {
                                panel.presentation_selection_anchor =
                                    panel.selected_presentation_nodes.last().copied();
                            }
                        }
                    }
                    changes.selection_changed = true;
                }
                TrackIntent::SetSectionExpanded { .. }
                | TrackIntent::SetPaneRowsExpanded { .. }
                | TrackIntent::ResetPaneRowHeights { .. }
                | TrackIntent::SetNodeVisibility { .. }
                | TrackIntent::SetGroupExpanded { .. }
                | TrackIntent::SetGroupVisibility { .. }
                | TrackIntent::SetCompositeVisibility { .. }
                | TrackIntent::ResizeNode { .. }
                | TrackIntent::ResizeReadPane { .. }
                | TrackIntent::GroupNodes { .. }
                | TrackIntent::Ungroup { .. }
                | TrackIntent::MoveNode { .. } => {}
            }
        }
        self.reconcile_presentation_interaction_state();
        Ok(changes)
    }

    pub(crate) fn has_alignment_engine(&self) -> bool {
        !self.alignment_tracks.is_empty()
    }

    pub(crate) fn primary_alignment_track_id(&self) -> TrackId {
        self.alignment_tracks
            .first()
            .map_or(SYNTHETIC_ALIGNMENT_TRACK_ID, |track| track.config.id)
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
        self.track_presentation
            .groups
            .iter()
            .filter_map(|group| {
                let anchor = group.members.first().copied()?;
                (group.members.len() > 1).then(|| ui::TrackGroupChrome {
                    id: group.id,
                    pane: group.channel,
                    anchor,
                    members: group.members.clone(),
                    collapsed: !group.expanded,
                    content_visible: group.content_visible,
                })
            })
            .collect()
    }

    pub(crate) fn reconcile_suppressed_group_selection(
        &mut self,
        pane: ui::TrackPane,
        members: &[TrackId],
    ) {
        let alignment_ids = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .collect::<BTreeSet<_>>();
        for panel in self.panels.iter_mut() {
            let suppressed_selected = panel.selected_presentation_nodes.iter().any(|node| {
                matches!(
                    node,
                    PresentationNodeId::Track { channel, track_id }
                        if *channel == pane && members.contains(track_id)
                )
            });
            panel.selected_presentation_nodes.retain(|node| {
                !matches!(
                    node,
                    PresentationNodeId::Track { channel, track_id }
                        if *channel == pane && members.contains(track_id)
                )
            });
            if panel.presentation_selection_anchor.is_some_and(|node| {
                matches!(
                    node,
                    PresentationNodeId::Track { channel, track_id }
                        if channel == pane && members.contains(&track_id)
                )
            }) {
                panel.presentation_selection_anchor =
                    panel.selected_presentation_nodes.last().copied();
            }
            match pane {
                ui::TrackPane::Annotation => {
                    panel
                        .selected_alignment_tracks
                        .retain(|id| !members.contains(id));
                    if panel
                        .alignment_selection_anchor
                        .is_some_and(|id| members.contains(&id))
                    {
                        panel.alignment_selection_anchor =
                            panel.selected_alignment_tracks.last().copied();
                    }
                    if suppressed_selected
                        && panel
                            .selected_alignment_track
                            .is_some_and(|id| members.contains(&id))
                    {
                        panel.selected_alignment_track = None;
                        panel.selected_pileup_position = None;
                        panel.selected_modification_position = None;
                        panel.selected_junction = None;
                        panel.selected_junction_component = None;
                    }
                    panel.selected_feature = panel
                        .selected_feature
                        .filter(|selection| !members.contains(&selection.track_id));
                    panel.selected_variant = panel
                        .selected_variant
                        .filter(|selection| !members.contains(&selection.track_id));
                    panel.selected_gwas = panel
                        .selected_gwas
                        .filter(|selection| !members.contains(&selection.track_id));
                    panel.selected_quantitative = panel
                        .selected_quantitative
                        .filter(|selection| !members.contains(&selection.track_id));
                    panel.selected_copy_number = panel
                        .selected_copy_number
                        .filter(|selection| !members.contains(&selection.track_id));
                    panel.selected_mutation = panel
                        .selected_mutation
                        .filter(|selection| !members.contains(&selection.track_id));
                    panel.selected_interaction = panel
                        .selected_interaction
                        .filter(|selection| !members.contains(&selection.track_id));
                }
                ui::TrackPane::Reads => {
                    panel
                        .selected_read_tracks
                        .retain(|id| !members.contains(id));
                    if panel
                        .read_selection_anchor
                        .is_some_and(|id| members.contains(&id))
                    {
                        panel.read_selection_anchor = panel.selected_read_tracks.last().copied();
                    }
                    if suppressed_selected
                        && panel
                            .selected_alignment_track
                            .is_some_and(|id| members.contains(&id))
                    {
                        panel.selected_alignment_track = None;
                        panel.pending_read_selection = None;
                        panel.pending_alignment_reveal = None;
                        panel.selected_chain = None;
                    }
                }
            }
            synchronize_panel_track_selection(panel, &alignment_ids);
        }
    }

    pub(crate) fn normalize_alignment_track_groups(&mut self) {
        self.reconcile_track_presentation();
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

    fn sashimi_summary_for_track(
        &self,
        track: &AlignmentTrackController,
        interval: GenomicInterval,
    ) -> Option<rgv_hts::JunctionSummary> {
        if track.scene_engine.is_none() {
            return None;
        }
        let presented = self.presented_alignment_lane(self.panels.active().id, track.config.id)?;
        if !presented.lane.summary_current() || !presented.source.semantic_target_complete {
            return None;
        }
        let snapshot = presented.lane.snapshot.as_ref()?;
        let junctions = snapshot.summaries().junctions(interval).ok()?;
        Some(rgv_hts::JunctionSummary {
            junctions,
            provenance: rgv_hts::JunctionProvenance {
                eligible_records: snapshot.reads().evidence().record_count(),
                minimum_flank: snapshot.summaries().contract().minimum_junction_flank,
                normalization_mapped_records: snapshot
                    .summaries()
                    .junction_normalization_mapped_records(),
                all_eligible_records: true,
                ..rgv_hts::JunctionProvenance::default()
            },
        })
    }

    pub(crate) fn sashimi_export_bundle(&self) -> SashimiExportBundle {
        let selected = &self.panels.active().selected_alignment_tracks;
        let use_selected = !selected.is_empty();
        let mut omitted_tracks = 0_usize;
        let mut tracks = Vec::new();
        let interval = self.panels.active().viewport.interval();
        for track in self.alignment_tracks.iter().filter(|track| {
            self.track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                .unwrap_or(false)
                && (!use_selected || selected.contains(&track.config.id))
        }) {
            let Some(junctions) = self.sashimi_summary_for_track(track, interval) else {
                continue;
            };
            if tracks.len() == MAX_SASHIMI_EXPORT_TRACKS {
                omitted_tracks = omitted_tracks.saturating_add(1);
                continue;
            }
            tracks.push(SashimiExportTrack {
                id: track.config.id,
                name: self.alignment_track_name_for_id(track.config.id),
                junctions,
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
                &self.panels.active().track_snapshot_intervals,
                self.panels.active().viewport.interval(),
            ),
        }
    }

    pub(crate) fn select_track_key(&mut self, key: TrackFrameKey, pane: ui::TrackPane) {
        let shift_range = self.modifiers.shift_key();
        let toggle = self.modifiers.control_key() || self.modifiers.super_key();
        let target = match key {
            TrackFrameKey::Reference if pane == ui::TrackPane::Annotation => {
                PresentationNodeId::Reference
            }
            TrackFrameKey::Alignment(track_id) => PresentationNodeId::Track {
                channel: pane,
                track_id,
            },
            TrackFrameKey::Auxiliary(track_id) if pane == ui::TrackPane::Annotation => {
                PresentationNodeId::Track {
                    channel: pane,
                    track_id,
                }
            }
            TrackFrameKey::Group { channel, id } if channel == pane => PresentationNodeId::Group {
                channel,
                group_id: id,
            },
            TrackFrameKey::Reference
            | TrackFrameKey::Auxiliary(_)
            | TrackFrameKey::Group { .. } => return,
        };
        let mut order = self.track_presentation.pane_interactive_nodes(pane);
        if pane == ui::TrackPane::Annotation && self.reference_engine.is_some() {
            order.insert(0, PresentationNodeId::Reference);
        }
        if !order.contains(&target) {
            return;
        }
        let panel = self.panels.active();
        let selected = panel
            .selected_presentation_nodes
            .iter()
            .copied()
            .filter(|node| node.channel() == pane && order.contains(node))
            .collect::<Vec<_>>();
        let anchor = panel
            .presentation_selection_anchor
            .filter(|node| node.channel() == pane);
        let (next, next_anchor) =
            updated_track_pane_selection(&order, &selected, anchor, target, shift_range, toggle);
        let alignment_ids = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .collect::<BTreeSet<_>>();
        let panel = self.panels.active_mut();
        panel.selected_presentation_nodes = next;
        panel.presentation_selection_anchor = next_anchor;
        synchronize_panel_track_selection(panel, &alignment_ids);
        self.mark_display_dirty("explicit display invalidation");
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
            if collect_modifications_changed
                && !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
            {
                for projection in track.projections.values_mut() {
                    projection.advance_scene_generation();
                }
                query_requested = true;
            }
        }
        if selected_ids.contains(&primary_id) {
            self.activate_first_alignment_track()?;
        }
        if query_requested {
            self.request_all_alignment_queries();
        }
        self.rebuild_genomic_display();
        self.mark_display_dirty("explicit display invalidation");
        Ok(())
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
        let requested_visibility = visibility.or_else(|| collapsed.map(|value| !value));
        let mut presentation_intents = Vec::new();
        if let Some(visible) = requested_visibility {
            presentation_intents.extend(alignment_ids.iter().copied().map(|track_id| {
                TrackIntent::SetNodeVisibility {
                    target: PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id,
                    },
                    visible,
                }
            }));
            presentation_intents.extend(
                self.tracks
                    .tracks()
                    .iter()
                    .filter(|track| selected.contains(&track.id))
                    .map(|track| TrackIntent::SetNodeVisibility {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id: track.id,
                        },
                        visible,
                    }),
            );
        }
        if let Some(height) = height {
            presentation_intents.extend(selected.iter().copied().filter_map(|track_id| {
                let is_alignment = self
                    .alignment_tracks
                    .iter()
                    .any(|track| track.config.id == track_id);
                let auxiliary = self
                    .tracks
                    .tracks()
                    .iter()
                    .find(|track| track.id == track_id);
                (is_alignment
                    || auxiliary.is_some_and(|track| {
                        !matches!(track.kind, TrackKind::Mutation | TrackKind::CopyNumber)
                    }))
                .then_some(TrackIntent::ResizeNode {
                    target: PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id,
                    },
                    preferred_height: height.clamp(40.0, 1_200.0),
                })
            }));
        }
        if !presentation_intents.is_empty()
            && let Err(error) = self.apply_track_intents(&presentation_intents)
        {
            self.report_error(error.to_string());
        }
        let mut changed = false;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| selected.contains(&track.config.id))
        {
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
            if rename_to_sample
                && let Some(sample_key) = &track.sample_key
                && track.name != *sample_key
            {
                track.name.clone_from(sample_key);
                changed = true;
            }
        }
        debug_assert_eq!(
            previous_auxiliary_query_membership,
            auxiliary_query_track_ids(self.tracks.tracks())
                .into_iter()
                .collect::<HashSet<_>>(),
            "presentation changes must not alter auxiliary query membership"
        );
        if changed {
            self.rebuild_all_panel_displays();
            self.mark_display_dirty("explicit display invalidation");
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
            track.config.maximum_alignments = source.maximum_alignments;
            for projection in track.projections.values_mut() {
                projection.advance_scene_generation();
            }
        }
        if selected.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.request_all_panel_queries();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
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
            self.mark_display_dirty("explicit display invalidation");
        }
    }

    pub(crate) fn remove_auxiliary_tracks(&mut self, ids: &[TrackId]) -> Result<usize> {
        self.remove_auxiliary_tracks_with_suppression(ids, true)
    }

    pub(crate) fn remove_auxiliary_tracks_with_suppression(
        &mut self,
        ids: &[TrackId],
        suppress_builtin_genes: bool,
    ) -> Result<usize> {
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
        let suppressed_bundles = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| actual.contains(&track.id))
            .filter_map(|track| track.built_in.as_ref())
            .filter(|identity| identity.asset_role == LandmarkAssetRole::RepresentativeGenes)
            .map(|identity| identity.bundle_id.clone())
            .collect::<BTreeSet<_>>();
        if let Some(engine) = &self.track_engine {
            // The actor request is queued before any replacement query. Do not
            // mutate visible state if bounded backpressure rejects the command.
            engine.try_remove_many(actual.clone())?;
        }

        let removed = actual.iter().copied().collect::<HashSet<_>>();
        for id in &actual {
            let _ = self.tracks.remove(*id);
            self.track_presentation.remove_source(*id);
            self.automation_auxiliary_open_state.remove(id);
            self.metadata_selected_tracks.remove(id);
        }
        if suppress_builtin_genes && !suppressed_bundles.is_empty() {
            self.suppressed_builtin_gene_bundles
                .extend(suppressed_bundles);
            self.landmark_search_index = None;
        }
        self.reconcile_track_presentation();
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
        if self.resizing_annotation_track.is_some_and(
            |(key, _)| matches!(key, TrackFrameKey::Auxiliary(id) if removed.contains(&id)),
        ) {
            self.resizing_annotation_track = None;
        }

        let has_engine = self.track_engine.is_some();
        for panel in self.panels.iter_mut() {
            // Invalidate any unread result produced before the actor processed
            // Remove, while retaining unaffected snapshots for the next frame.
            panel.generation_clock.advance();
            panel.pending_queries.tracks = has_engine;
            clear_removed_auxiliary_panel_state(panel, &removed);
        }
        self.invalidate_navigation_contract("auxiliary track removed");
        self.begin_all_panel_refresh_transactions();
        self.request_all_auxiliary_track_queries();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
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
            self.mark_display_dirty("explicit display invalidation");
        }
        Ok(removed)
    }

    pub(crate) fn advance_alignment_generations(&self, panel_id: PanelId) {
        for track in &self.alignment_tracks {
            if let Some(projection) = track.projection(panel_id) {
                projection.advance_scene_generation();
            }
        }
    }

    pub(crate) fn apply_secondary_alignment_settings(
        &mut self,
        id: TrackId,
        mut update: AlignmentSettingsDraft,
    ) {
        let (workspace_limit, requested_limit) = expanded_workspace_alignment_limit(
            self.workspace_alignment_limit,
            update.maximum_alignments,
        );
        self.workspace_alignment_limit = workspace_limit;
        update.maximum_alignments = requested_limit;
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
            workspace_limit,
        );
        if outcome.query_changed && !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_)) {
            for projection in track.projections.values_mut() {
                projection.advance_scene_generation();
            }
            self.panels.active_mut().pending_queries.alignment = true;
        }
        if outcome.configuration_changed {
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
        if outcome.query_changed {
            self.request_all_alignment_queries();
        }
    }
}

#[cfg(test)]
mod track_group_tests {
    use super::*;

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
