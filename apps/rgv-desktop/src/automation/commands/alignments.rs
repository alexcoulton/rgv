use crate::automation::commands::auxiliary_position_snapshot_is_current;
use crate::automation::commands::evidence::automation_scene_supplementary_chain;
use crate::automation::parse_automation_sam_tag;
use crate::display::auxiliary::{alignment_sort_position, quantitative_record_index_at_position};
use crate::inspection::model::{FeatureSelection, QuantitativeSelection, VariantSelection};
use crate::inspection::rna::selected_supplementary_chain;
use crate::ui::TrackFrameKey;
use crate::workspace::alignments::filter_state_from_session;
use crate::workspace::panels::PendingReadSelection;
use crate::workspace::tracks::replace_panel_track_selection;
use crate::{AutomationAlignmentHandleTarget, Viewer, ui};
use anyhow::{Context as _, Result};
use rgv_alignment_scene::{AlignmentComponent, AlignmentHandle, AlignmentHit};
use rgv_automation::{
    AutomationAlignmentRelationship, AutomationAlignmentReveal, AutomationAlignmentSelector,
    AutomationAlignmentState, AutomationNavigationDestination, AutomationSelection,
    AutomationViewportOperation,
};
use rgv_core::{PanelId, TrackId};
use rgv_engine::{AlignmentIndividualReadCapability, TrackSnapshotData};
use rgv_hts::{
    AlignmentRecord, AuxPredicate, SequenceData, TemplateLengthBounds, decode_modified_bases,
    pair_evidence,
};
use rgv_layout::{AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn find_automation_alignments(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        selector: &AutomationAlignmentSelector,
        maximum_results: usize,
    ) -> Result<serde_json::Value> {
        let presentation_committed = self
            .session
            .panels
            .get(panel_id)
            .is_some_and(|panel| panel.navigation_presentation.is_interactive());
        anyhow::ensure!(
            presentation_committed,
            "the target viewport is still being checked"
        );
        let (filters, snapshot, generation) = {
            let track = self
                .session
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)
                .with_context(|| format!("alignment track {} was not found", track_id.get()))?;
            anyhow::ensure!(
                track.scene_engine.is_some(),
                "alignment source is not supported by the unified scene backend"
            );
            let filters = track.config.filters.clone();
            let presented = self
                .presented_alignment_lane(panel_id, track_id)
                .context("the committed alignment scene is unavailable")?;
            anyhow::ensure!(
                presented.lane.summary_current() && presented.source.semantic_target_complete,
                "the committed alignment scene has not settled for the current viewport"
            );
            let snapshot = presented
                .lane
                .snapshot
                .as_ref()
                .context("the committed alignment scene has no semantic snapshot")?;
            anyhow::ensure!(
                matches!(
                    presented.source.individual_reads,
                    AlignmentIndividualReadCapability::Complete { scene_revision }
                        if scene_revision == snapshot.revision()
                ) && presented.read_authority.is_some(),
                "complete reads unavailable: the committed alignment scene refused individual reads"
            );
            (filters, Arc::clone(snapshot), presented.target_revision)
        };
        let search_interval = snapshot.prepared_interval();
        let mut read_name_counts = HashMap::<Vec<u8>, usize>::new();
        let mut supplementary_names = HashSet::<Vec<u8>>::new();
        let requires_complete_chain_index = matches!(
            selector,
            AutomationAlignmentSelector::Semantic {
                has_supplementary_chain: Some(_),
                ..
            }
        );
        if requires_complete_chain_index {
            snapshot
                .reads()
                .for_each_identity(search_interval, |_, read_name, flags| {
                    *read_name_counts.entry(read_name.to_vec()).or_default() += 1;
                    if flags.is_supplementary() {
                        supplementary_names.insert(read_name.to_vec());
                    }
                })?;
        }
        let contig_name = self
            .session
            .panels
            .iter()
            .find(|panel| panel.id == panel_id)
            .map(|panel| panel.contig_name.clone())
            .unwrap_or_default();
        let mut handle_targets = Vec::new();
        let mut hits = Vec::new();
        let mut hit_read_names = Vec::new();
        let mut matched_count = 0usize;
        let mut scan_error = None;
        let mut visit = |alignment_handle: AlignmentHandle, record: AlignmentRecord| -> bool {
            if scan_error.is_some() {
                return false;
            }
            let pair = pair_evidence(
                &record,
                search_interval,
                TemplateLengthBounds {
                    minimum: filters.minimum_template_length,
                    maximum: filters.maximum_template_length,
                },
            );
            let chain = selected_supplementary_chain(&record, "");
            let query_chain_segments = read_name_counts
                .get(&record.read_name)
                .copied()
                .filter(|count| *count > 1 && supplementary_names.contains(&record.read_name))
                .unwrap_or_default();
            let has_navigable_chain = chain.as_ref().is_some_and(|chain| chain.segments.len() > 1)
                || query_chain_segments > 1;
            let modification_result = if matches!(record.sequence, SequenceData::NotLoaded)
                && record.auxiliary_fields.is_empty()
            {
                None
            } else {
                Some(decode_modified_bases(&record))
            };
            let modifications = modification_result
                .as_ref()
                .and_then(|result| result.as_ref().ok());
            let modification_evidence_status = match &modification_result {
                None => "unavailable",
                Some(Ok(calls)) if calls.is_empty() => "absent",
                Some(Ok(_)) => "present",
                Some(Err(_)) => "malformed",
            };
            if matches!(
                selector,
                AutomationAlignmentSelector::Semantic {
                    has_modified_bases: Some(_),
                    ..
                }
            ) && modification_evidence_status == "unavailable"
            {
                scan_error = Some(
                    "modified-base evidence is unavailable at the current query detail; enable modified-base display or zoom to read detail"
                        .to_owned(),
                );
                return false;
            }
            let is_match = match selector {
                AutomationAlignmentSelector::ExactReadName { read_name } => {
                    record.read_name == read_name.as_bytes()
                }
                AutomationAlignmentSelector::Position { position } => {
                    record.start <= *position && *position < record.end
                }
                AutomationAlignmentSelector::Interval { start, end } => {
                    record.start < *end && *start < record.end
                }
                AutomationAlignmentSelector::Semantic {
                    alignment_state,
                    pair_class,
                    has_mapped_mate,
                    has_supplementary_chain,
                    has_modified_bases,
                } => {
                    let state_matches = alignment_state.is_none_or(|state| match state {
                        AutomationAlignmentState::Primary => {
                            !record.flags.is_secondary() && !record.flags.is_supplementary()
                        }
                        AutomationAlignmentState::Secondary => record.flags.is_secondary(),
                        AutomationAlignmentState::Supplementary => record.flags.is_supplementary(),
                    });
                    let mate_available = record.mate_contig.is_some()
                        && record.mate_start.is_some()
                        && !record.flags.is_mate_unmapped();
                    state_matches
                        && pair_class.as_ref().is_none_or(|expected| {
                            format!("{:?}", pair.classification).eq_ignore_ascii_case(expected)
                        })
                        && has_mapped_mate.is_none_or(|expected| expected == mate_available)
                        && has_supplementary_chain
                            .is_none_or(|expected| expected == has_navigable_chain)
                        && has_modified_bases.is_none_or(|expected| {
                            modifications.is_some_and(|calls| expected != calls.is_empty())
                        })
                }
            };
            if !is_match {
                return true;
            }
            matched_count = matched_count.saturating_add(1);
            if hits.len() >= maximum_results {
                return true;
            }
            let handle = uuid::Uuid::new_v4().to_string();
            handle_targets.push((
                handle.clone(),
                AutomationAlignmentHandleTarget {
                    panel_id,
                    track_id,
                    handle: alignment_handle,
                },
            ));
            hit_read_names.push(record.read_name.clone());
            hits.push(serde_json::json!({
                "handle": handle,
                "panel_id": panel_id.get(),
                "track_id": track_id.get(),
                "contig": contig_name.as_str(),
                "start": record.start,
                "end": record.end,
                "flags": record.flags.bits(),
                "mapping_quality": record.mapping_quality,
                "pair_class": format!("{:?}", pair.classification).to_ascii_lowercase(),
                "mapped_mate_available": record.mate_contig.is_some()
                    && record.mate_start.is_some()
                    && !record.flags.is_mate_unmapped(),
                "supplementary_segment_count": chain
                    .as_ref()
                    .map_or(0, |chain| chain.segments.len())
                    .max(query_chain_segments),
                "modification_code_count": modifications.map_or(0, |calls| {
                    calls.iter().map(|call| call.code).collect::<HashSet<_>>().len()
                }),
                "modification_evidence_status": modification_evidence_status,
            }));
            true
        };
        snapshot
            .reads()
            .for_each_full(search_interval, |exported| {
                visit(exported.handle, exported.record)
            })?;
        drop(visit);
        if let Some(error) = scan_error {
            anyhow::bail!(error);
        }
        let mut selected_chain_counts = hit_read_names
            .iter()
            .cloned()
            .map(|read_name| (read_name, (0_usize, false)))
            .collect::<HashMap<_, _>>();
        let mut count_selected_chain = |read_name: &[u8], supplementary: bool| {
            if let Some((count, has_supplementary)) = selected_chain_counts.get_mut(read_name) {
                *count = count.saturating_add(1);
                *has_supplementary |= supplementary;
            }
        };
        snapshot
            .reads()
            .for_each_identity(search_interval, |_, read_name, flags| {
                count_selected_chain(read_name, flags.is_supplementary());
            })?;
        drop(count_selected_chain);
        for (hit, read_name) in hits.iter_mut().zip(&hit_read_names) {
            let Some((count, true)) = selected_chain_counts.get(read_name).copied() else {
                continue;
            };
            let existing = hit
                .get("supplementary_segment_count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_default();
            hit["supplementary_segment_count"] = serde_json::json!(existing.max(count));
        }
        self.automation_alignment_handles.clear();
        self.automation_alignment_handles.extend(handle_targets);
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.session.workspace_revision,
            "query_generation": generation,
            "query_contig": contig_name,
            "query_start": search_interval.start(),
            "query_end": search_interval.end(),
            "hits": hits,
            "truncated": matched_count > maximum_results,
        }))
    }

    pub(crate) fn select_automation_alignment(
        &mut self,
        handle: &str,
        reveal: AutomationAlignmentReveal,
    ) -> Result<serde_json::Value> {
        let target = self
            .automation_alignment_handles
            .get(handle)
            .copied()
            .context("alignment handle is stale or unknown")?;
        let presentation_committed = self
            .session
            .panels
            .get(target.panel_id)
            .is_some_and(|panel| panel.navigation_presentation.is_interactive());
        let track = self
            .session
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == target.track_id)
            .context("alignment handle track is unavailable")?;
        anyhow::ensure!(
            track.scene_engine.is_some(),
            "alignment source is not supported by the unified scene backend"
        );
        let presented = self
            .presented_alignment_lane(target.panel_id, target.track_id)
            .context("the committed alignment scene is unavailable")?;
        let snapshot = presented
            .lane
            .snapshot
            .as_ref()
            .context("the committed alignment scene has no semantic snapshot")?;
        anyhow::ensure!(
            presentation_committed
                && presented.lane.summary_current()
                && presented.source.semantic_target_complete
                && snapshot.reads().contains_handle(target.handle),
            "alignment handle is stale"
        );
        let record = snapshot
            .inspect_retained(target.handle)?
            .context("alignment handle no longer identifies a record")?;
        let scene_revision = snapshot.revision();
        let chain = automation_scene_supplementary_chain(
            snapshot.reads(),
            target.handle,
            &record,
            &self
                .session
                .panels
                .get(target.panel_id)
                .map(|panel| panel.contig_name.as_str())
                .unwrap_or_default(),
        )?;
        // Selecting a read focuses its panel: the inspector shows the focused panel's selection.
        anyhow::ensure!(
            self.activate_view_panel(target.panel_id),
            "alignment panel is unavailable"
        );
        self.session.panels.active_mut().clear_selection();
        self.session.panels.active_mut().selected_alignment_track = Some(target.track_id);
        replace_panel_track_selection(
            self.session.panels.active_mut(),
            TrackFrameKey::Alignment(target.track_id),
            ui::TrackPane::Reads,
        );
        self.session.panels.active_mut().selected_alignment_hit = Some(AlignmentHit {
            track: target.track_id,
            scene_revision,
            handle: target.handle,
            component: AlignmentComponent::ReadSpan,
        });
        self.session.panels.active_mut().selected_chain = chain;
        self.session.panels.active_mut().pending_read_selection = None;
        self.bump_workspace_revision();
        self.retain_active_selection(None);
        self.synchronize_alignment_scene_interactions(target.panel_id);
        self.prepare_alignment_scene_frame();
        self.rebuild_genomic_display(self.session.panels.active().id);
        let mut navigated = false;
        if reveal != AutomationAlignmentReveal::None {
            let operation = match reveal {
                AutomationAlignmentReveal::None => unreachable!(),
                AutomationAlignmentReveal::Fit => AutomationViewportOperation::FitInterval {
                    start: record.start,
                    end: record.end,
                },
                AutomationAlignmentReveal::Center => {
                    let span = self.session.panels.active().viewport.interval().len();
                    let midpoint = record.start.saturating_add(record.end) / 2;
                    let start = midpoint.saturating_sub(span / 2);
                    AutomationViewportOperation::FitInterval {
                        start,
                        end: start.saturating_add(span),
                    }
                }
            };
            self.apply_automation_viewport_operation(target.panel_id, operation)?;
            self.session.panels.active_mut().pending_read_selection =
                PendingReadSelection::for_record(target.track_id, &record);
            navigated = true;
        }
        let interval = self.session.panels.active().viewport.interval();
        Ok(serde_json::json!({
            "selection": self.automation_selection_snapshot(),
            "reveal": format!("{reveal:?}").to_ascii_lowercase(),
            "navigated": navigated,
            "viewport": {
                "contig": self.session.panels.active().contig_name,
                "start": interval.start(),
                "end": interval.end(),
            },
            "workspace_revision": self.session.workspace_revision,
        }))
    }

    pub(crate) fn automation_related_alignments(&self) -> Result<serde_json::Value> {
        let presentation_committed = self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive();
        let track_id = self
            .session
            .panels
            .active()
            .selected_alignment_track
            .context("no alignment is selected")?;
        let track = self
            .session
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .context("selected alignment track is unavailable")?;
        anyhow::ensure!(
            track.scene_engine.is_some(),
            "alignment source is not supported by the unified scene backend"
        );
        let hit = self
            .session
            .panels
            .active()
            .selected_alignment_hit
            .filter(|hit| hit.track == track_id)
            .context("no scene-backed alignment is selected")?;
        let presented = self
            .presented_alignment_lane(self.session.panels.active().id, track_id)
            .context("the committed alignment scene is unavailable")?;
        let snapshot = presented
            .lane
            .snapshot
            .as_ref()
            .context("the committed alignment scene has no semantic snapshot")?;
        anyhow::ensure!(
            presentation_committed && snapshot.reads().contains_handle(hit.handle),
            "the selected alignment is stale"
        );
        let record = snapshot
            .inspect_retained(hit.handle)?
            .context("the selected alignment is unavailable")?;
        let segments = self
            .session
            .panels
            .active()
            .selected_chain
            .as_ref()
            .map_or_else(Vec::new, |chain| {
                chain
                    .segments
                    .iter()
                    .enumerate()
                    .map(|(index, segment)| {
                        serde_json::json!({
                            "segment_index": index,
                            "contig": segment.reference_name,
                            "start": segment.start,
                            "end": segment.end,
                            "current": index == chain.active_segment,
                        })
                    })
                    .collect()
            });
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.session.workspace_revision,
            "mate_available": record.mate_contig.is_some()
                && record.mate_start.is_some()
                && !record.flags.is_mate_unmapped(),
            "supplementary_segments": segments,
        }))
    }

    pub(crate) fn navigate_automation_related_alignment(
        &mut self,
        relationship: AutomationAlignmentRelationship,
        segment_index: Option<usize>,
        destination: AutomationNavigationDestination,
    ) -> Result<serde_json::Value> {
        let panel_id = match (relationship, destination) {
            (AutomationAlignmentRelationship::Mate, AutomationNavigationDestination::NewPanel) => {
                self.open_selected_mate_in_new_panel()?
            }
            (
                AutomationAlignmentRelationship::Mate,
                AutomationNavigationDestination::CurrentPanel,
            ) => self.go_to_selected_mate_in_current_panel()?,
            (
                AutomationAlignmentRelationship::SupplementarySegment,
                AutomationNavigationDestination::NewPanel,
            ) => self.open_chain_segment_in_new_panel(
                segment_index.context("supplementary segment index is required")?,
            )?,
            (
                AutomationAlignmentRelationship::SupplementarySegment,
                AutomationNavigationDestination::CurrentPanel,
            ) => {
                self.navigate_to_chain_segment(
                    segment_index.context("supplementary segment index is required")?,
                )?;
                self.session.panels.active().id
            }
        };
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "workspace_revision": self.session.workspace_revision,
            "settled": self.automation_is_settled(),
        }))
    }

    pub(crate) fn apply_automation_group(
        &mut self,
        track_ids: &[TrackId],
        mode: AlignmentGroupMode,
        direction: AlignmentOrderDirection,
        tag: Option<&str>,
        anchor_position: Option<u64>,
    ) -> Result<()> {
        self.ensure_automation_alignment_tracks(track_ids)?;
        let mode = tag.map_or(Ok(mode), |tag| {
            parse_automation_sam_tag(tag).map(AlignmentGroupMode::Tag)
        })?;
        let captured_anchor = matches!(
            mode,
            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
        )
        .then(|| {
            anchor_position.unwrap_or_else(|| {
                self.session
                    .panels
                    .active()
                    .viewport
                    .interval()
                    .start()
                    .saturating_add(self.session.panels.active().viewport.interval().len() / 2)
            })
        });
        for track in self
            .session
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track.config.filters.alignment_group_mode = mode;
            track.config.filters.alignment_group_order = direction;
            if captured_anchor.is_some() {
                track.config.filters.alignment_sort_anchor = captured_anchor;
            }
        }
        if track_ids.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.invalidate_automation_alignment_layouts(Some(track_ids), false);
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        Ok(())
    }

    pub(super) fn automation_alignment_group_state(
        &self,
        track_ids: &[TrackId],
    ) -> Vec<serde_json::Value> {
        self.session
            .alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .map(|track| {
                let anchor = matches!(
                    track.config.filters.alignment_group_mode,
                    AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
                )
                .then_some(track.config.filters.alignment_sort_anchor)
                .flatten();
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "mode": track.config.filters.alignment_group_mode,
                    "direction": track.config.filters.alignment_group_order,
                    "anchor": anchor,
                })
            })
            .collect()
    }

    pub(super) fn automation_alignment_sort_state(
        &self,
        track_ids: &[TrackId],
    ) -> Vec<serde_json::Value> {
        self.session
            .alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .map(|track| {
                let anchor = (track.config.filters.alignment_sort_mode == AlignmentSortMode::Base)
                    .then_some(track.config.filters.alignment_sort_anchor)
                    .flatten();
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "mode": track.config.filters.alignment_sort_mode,
                    "direction": track.config.filters.alignment_sort_order,
                    "anchor": anchor,
                })
            })
            .collect()
    }

    pub(super) fn automation_alignment_filter_state(
        &self,
        track_ids: &[TrackId],
    ) -> Vec<serde_json::Value> {
        self.session
            .alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .map(|track| {
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "expression": &track.filter_editor.expression,
                })
            })
            .collect()
    }

    pub(crate) fn apply_automation_sort(
        &mut self,
        track_ids: &[TrackId],
        mode: AlignmentSortMode,
        direction: AlignmentOrderDirection,
        tag: Option<&str>,
        anchor_position: Option<u64>,
    ) -> Result<()> {
        self.ensure_automation_alignment_tracks(track_ids)?;
        let mode = tag.map_or(Ok(mode), |tag| {
            parse_automation_sam_tag(tag).map(AlignmentSortMode::Tag)
        })?;
        let captured_anchor = if mode == AlignmentSortMode::Base {
            anchor_position.or_else(|| {
                alignment_sort_position(mode, None, self.session.panels.active().viewport)
            })
        } else {
            None
        };
        for track in self
            .session
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track.config.filters.alignment_sort_mode = mode;
            track.config.filters.alignment_sort_order = direction;
            if captured_anchor.is_some() {
                track.config.filters.alignment_sort_anchor = captured_anchor;
            } else if !matches!(
                track.config.filters.alignment_group_mode,
                AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
            ) {
                track.config.filters.alignment_sort_anchor = None;
            }
        }
        if track_ids.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.invalidate_automation_alignment_layouts(Some(track_ids), false);
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        Ok(())
    }

    pub(crate) fn apply_automation_filter(
        &mut self,
        track_ids: &[TrackId],
        expression: &str,
    ) -> Result<()> {
        self.ensure_automation_alignment_tracks(track_ids)?;
        let predicates = expression
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<AuxPredicate>().map(|_| part.to_owned()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for track in self
            .session
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track
                .config
                .filters
                .auxiliary_predicates
                .clone_from(&predicates);
            let (editor, active) = filter_state_from_session(&track.config.filters)?;
            track.filter_editor = editor;
            track.active_filters = active;
        }
        if track_ids.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.invalidate_automation_alignment_layouts(Some(track_ids), true);
        self.request_all_panel_queries();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        Ok(())
    }

    pub(crate) fn ensure_automation_alignment_tracks(&self, track_ids: &[TrackId]) -> Result<()> {
        for id in track_ids {
            anyhow::ensure!(
                self.session
                    .alignment_tracks
                    .iter()
                    .any(|track| track.config.id == *id),
                "alignment track {} was not found",
                id.get()
            );
        }
        Ok(())
    }

    pub(crate) fn invalidate_automation_alignment_layouts(
        &mut self,
        track_ids: Option<&[TrackId]>,
        query_changed: bool,
    ) {
        for track in
            self.session.alignment_tracks.iter_mut().filter(|track| {
                track_ids.is_none_or(|track_ids| track_ids.contains(&track.config.id))
            })
        {
            for projection in track.projections.values_mut() {
                if query_changed {
                    projection.advance_scene_generation();
                }
            }
        }
        if query_changed {
            self.automation_alignment_handles.clear();
            self.mark_retained_evidence_stale(
                "alignment query settings changed after this evidence was captured",
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_selection(
        &mut self,
        selection: &AutomationSelection,
    ) -> Result<serde_json::Value> {
        match *selection {
            AutomationSelection::Panel { panel_id } => {
                anyhow::ensure!(
                    self.apply_command_translation(&rgv_automation::AutomationCommand::Select {
                        selection: selection.clone(),
                    })?
                    .panel_id
                    .is_some(),
                    "panel {} was not found",
                    panel_id.get()
                );
                let revision = self.bump_workspace_revision();
                Ok(serde_json::json!({"panel_id": panel_id.get(), "workspace_revision": revision}))
            }
            AutomationSelection::Track { track_id } => {
                let key = self.automation_track_key(track_id)?;
                self.session.panels.active_mut().clear_selection();
                self.select_track_key(key, ui::TrackPane::Annotation);
                self.bump_workspace_revision();
                self.retain_active_selection(None);
                Ok(serde_json::json!({
                    "track_id": track_id.get(),
                    "workspace_revision": self.session.workspace_revision,
                    "selection": self.automation_selection_snapshot(),
                }))
            }
            AutomationSelection::Position {
                panel_id,
                track_id,
                position,
                granularity,
            } => {
                anyhow::ensure!(
                    self.activate_view_panel(panel_id),
                    "panel {} was not found",
                    panel_id.get()
                );
                let key = self.automation_track_key(track_id)?;
                anyhow::ensure!(
                    position < self.session.panels.active().contig_length,
                    "position {position} lies beyond contig {} length {}",
                    self.session.panels.active().contig_name,
                    self.session.panels.active().contig_length
                );
                if !self
                    .session
                    .panels
                    .active_mut()
                    .viewport
                    .interval()
                    .contains(position)
                {
                    self.session.panels.active_mut().viewport = self
                        .session
                        .panels
                        .active()
                        .viewport
                        .centered_on(position, self.session.panels.active().contig_length)?;
                    self.session.panels.active().generation_clock.advance();
                    self.request_alignment_query(self.session.panels.active().id);
                    self.request_reference_query(self.session.panels.active().id);
                    self.request_track_query(self.session.panels.active().id);
                }
                let auxiliary_selection = if matches!(key, TrackFrameKey::Auxiliary(_)) {
                    self.session
                        .panels
                        .active_mut()
                        .track_snapshots
                        .iter()
                        .find(|snapshot| snapshot.id == track_id)
                        .and_then(|snapshot| match &snapshot.data {
                            TrackSnapshotData::Features(records) => records
                                .iter()
                                .enumerate()
                                .filter(|(_, record)| {
                                    record.start <= position && position < record.end
                                })
                                .min_by_key(|(index, record)| {
                                    (record.end.saturating_sub(record.start), *index)
                                })
                                .map(|(index, _)| (Some(index), None, None)),
                            TrackSnapshotData::Variants(query) => query
                                .records
                                .iter()
                                .enumerate()
                                .filter(|(_, record)| {
                                    record.start <= position
                                        && position < record.end.max(record.start.saturating_add(1))
                                })
                                .min_by_key(|(index, record)| {
                                    (record.end.saturating_sub(record.start), *index)
                                })
                                .map(|(index, _)| (None, Some(index), None)),
                            TrackSnapshotData::Quantitative(query) => {
                                quantitative_record_index_at_position(&query.records, position)
                                    .map(|index| (None, None, Some(index)))
                            }
                            TrackSnapshotData::CopyNumber(_)
                            | TrackSnapshotData::Mutations(_)
                            | TrackSnapshotData::Interactions(_)
                            | TrackSnapshotData::Gwas(_) => None,
                        })
                } else {
                    None
                };
                self.session.panels.active_mut().clear_selection();
                if matches!(key, TrackFrameKey::Alignment(_)) {
                    self.session.panels.active_mut().selected_alignment_track = Some(track_id);
                    replace_panel_track_selection(
                        self.session.panels.active_mut(),
                        key,
                        ui::TrackPane::Annotation,
                    );
                    self.session.panels.active_mut().selected_pileup_position = Some(position);
                } else if let Some((feature, variant, quantitative)) = auxiliary_selection {
                    replace_panel_track_selection(
                        self.session.panels.active_mut(),
                        key,
                        ui::TrackPane::Annotation,
                    );
                    self.session.panels.active_mut().selected_feature =
                        feature.map(|record_index| FeatureSelection {
                            track_id,
                            record_index,
                        });
                    self.session.panels.active_mut().selected_variant =
                        variant.map(|record_index| VariantSelection {
                            track_id,
                            record_index,
                        });
                    self.session.panels.active_mut().selected_quantitative =
                        quantitative.map(|record_index| QuantitativeSelection {
                            track_id,
                            record_index,
                            position,
                        });
                }
                self.rebuild_genomic_display(self.session.panels.active().id);
                self.mark_display_dirty("explicit display invalidation");
                self.bump_workspace_revision();
                self.retain_active_selection(Some(granularity));
                Ok(serde_json::json!({
                    "panel_id": panel_id.get(),
                    "track_id": track_id.get(),
                    "position": position,
                    "workspace_revision": self.session.workspace_revision,
                    "selection": self.automation_selection_snapshot(),
                }))
            }
        }
    }

    pub(crate) fn automation_exact_base_available(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        position: u64,
    ) -> bool {
        if let Some(available) = self
            .session
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .map(|track| {
                track.scene_engine.is_some()
                    && self
                        .presented_alignment_lane(panel_id, track_id)
                        .and_then(|presented| presented.lane.snapshot.as_ref())
                        .and_then(|snapshot| snapshot.pileup_site_at(position))
                        .is_some_and(|site| site.evidence.allele_observations_available)
            })
        {
            return available;
        }

        self.session
            .panels
            .get(panel_id)
            .is_some_and(|panel| auxiliary_position_snapshot_is_current(panel, track_id, position))
    }

    pub(crate) fn automation_track_key(&self, track_id: TrackId) -> Result<TrackFrameKey> {
        if self
            .session
            .alignment_tracks
            .iter()
            .any(|track| track.config.id == track_id)
        {
            return Ok(TrackFrameKey::Alignment(track_id));
        }
        if self
            .session
            .tracks
            .tracks()
            .iter()
            .any(|track| track.id == track_id)
        {
            return Ok(TrackFrameKey::Auxiliary(track_id));
        }
        anyhow::bail!("track {} was not found", track_id.get())
    }
}
